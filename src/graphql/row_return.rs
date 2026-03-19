use crate::graphql::scalar::{AggroScalarValue, AggroValue};

use crate::errors::PuffResult;
use crate::python::postgres::column_to_python;
use crate::types::{Bytes, Text, UtcDateTime};
use anyhow::{anyhow, bail, Result};
use juniper::Object;
use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use std::collections::HashMap;
use tokio_postgres::{Column, Row, Statement};

pub fn convert_pyany_to_jupiter(attribute_val: &Bound<'_, PyAny>) -> AggroValue {
    if attribute_val.is_none() {
        return AggroValue::Null;
    }
    if let Ok(s) = attribute_val.extract() {
        return AggroValue::Scalar(AggroScalarValue::String(s));
    }
    if let Ok(s) = attribute_val.extract() {
        return AggroValue::Scalar(AggroScalarValue::Boolean(s));
    }
    if let Ok(r) = attribute_val.extract::<i64>() {
        return if let Ok(s) = r.try_into() {
            AggroValue::Scalar(AggroScalarValue::Int(s))
        } else {
            AggroValue::Scalar(AggroScalarValue::Long(r))
        };
    }
    if let Ok(s) = attribute_val.extract() {
        return AggroValue::Scalar(AggroScalarValue::Float(s));
    }
    if let Ok(l) = attribute_val.downcast::<PyList>() {
        return AggroValue::List(l.iter().map(|s| convert_pyany_to_jupiter(&s)).collect());
    }
    if let Ok(l) = attribute_val.downcast::<PyDict>() {
        return AggroValue::Object(
            l.iter()
                .map(|(k, s)| (k.to_string(), convert_pyany_to_jupiter(&s)))
                .collect(),
        );
    }
    if let Ok(dict_obj) = attribute_val.getattr("__dict__") {
        if let Ok(dict) = dict_obj.downcast::<PyDict>() {
            if !dict.is_empty() {
                let mut obj = Object::with_capacity(dict.len());
                for (key, value) in dict.iter() {
                    let key = key.to_string();
                    if !key.starts_with("__") {
                        obj.add_field(&key, convert_pyany_to_jupiter(&value));
                    }
                }
                return AggroValue::Object(obj);
            }
        }
    }
    let dir = attribute_val.dir().expect("Failed to call dir()");
    let size = dir.len();
    let mut obj = Object::with_capacity(size);

    for v in dir.iter() {
        if let Ok(s) = v.downcast::<PyString>() {
            let key = s.to_str().expect("Python dir could not unwrap key to str.");
            if !key.starts_with("__") {
                let py_val = attribute_val
                    .getattr(key)
                    .unwrap_or_else(|_| panic!("Could not get {}", key));
                if !py_val.is_callable() {
                    let juniper_val = convert_pyany_to_jupiter(&py_val);
                    obj.add_field(key, juniper_val);
                }
            }
        }
    }
    AggroValue::Object(obj)
}

pub fn convert_postgres_to_juniper(
    r: &Row,
    column_index: usize,
    t: &tokio_postgres::types::Type,
) -> Result<AggroValue> {
    match t {
        &tokio_postgres::types::Type::BOOL => {
            if let Some(r) = r.get::<_, Option<bool>>(column_index) {
                Ok(AggroValue::scalar(r))
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::INT2 | &tokio_postgres::types::Type::INT4 => {
            if let Some(r) = r.get::<_, Option<i32>>(column_index) {
                Ok(AggroValue::scalar(r))
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::INT8 => {
            if let Some(r) = r.get::<_, Option<i64>>(column_index) {
                Ok(if let Ok(s) = r.try_into() {
                    AggroValue::Scalar(AggroScalarValue::Int(s))
                } else {
                    AggroValue::Scalar(AggroScalarValue::Long(r))
                })
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::BYTEA => {
            if let Some(r) = r.get::<_, Option<&[u8]>>(column_index) {
                Ok(AggroValue::Scalar(AggroScalarValue::Binary(
                    Bytes::copy_from_slice(r),
                )))
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::TIMESTAMP => {
            if let Some(r) = r.get::<_, Option<_>>(column_index) {
                Ok(AggroValue::Scalar(AggroScalarValue::Datetime(
                    UtcDateTime::new(r),
                )))
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::UUID => {
            if let Some(r) = r.get::<_, Option<_>>(column_index) {
                Ok(AggroValue::Scalar(AggroScalarValue::Uuid(r)))
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::FLOAT4 | &tokio_postgres::types::Type::FLOAT8 => {
            if let Some(r) = r.get::<_, Option<f64>>(column_index) {
                Ok(AggroValue::scalar(r))
            } else {
                Ok(AggroValue::null())
            }
        }
        &tokio_postgres::types::Type::TEXT | &tokio_postgres::types::Type::VARCHAR => {
            if let Some(r) = r.get::<_, Option<&str>>(column_index) {
                Ok(AggroValue::scalar(r))
            } else {
                Ok(AggroValue::null())
            }
        }
        t => {
            panic!("Unsupported postgres type {}", t)
        }
    }
}

pub trait ExtractValues {
    fn len(&self) -> usize;
    fn row_presence(&self) -> Vec<bool>;
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Option<Vec<AggroValue>>>>;
    fn extract_py_values(&self, py: Python, names: &[Bound<'_, PyString>]) -> PuffResult<PyObject>;
    fn extract_first(&self) -> Result<Vec<AggroValue>>;
}

pub struct PostgresResultRows {
    pub statement: Statement,
    pub rows: Vec<Row>,
    column_indices: HashMap<String, usize>,
}

impl PostgresResultRows {
    pub fn new(statement: Statement, rows: Vec<Row>) -> Self {
        let column_indices = statement
            .columns()
            .iter()
            .enumerate()
            .map(|(index, column)| (column.name().to_string(), index))
            .collect();

        Self {
            statement,
            rows,
            column_indices,
        }
    }

    fn ordered_columns(&self, names: &[Text]) -> Result<Vec<(usize, &Column)>> {
        let columns = self.statement.columns();
        let mut ordered = Vec::with_capacity(names.len());

        for name in names {
            let index = *self
                .column_indices
                .get(name.as_str())
                .ok_or_else(|| anyhow!("Could not find {} in Postgres row", name))?;
            ordered.push((index, &columns[index]));
        }

        Ok(ordered)
    }

    fn ordered_py_columns<'a>(
        &'a self,
        names: &[Bound<'_, PyString>],
    ) -> PuffResult<Vec<(usize, &'a Column)>> {
        let columns = self.statement.columns();
        let mut ordered = Vec::with_capacity(names.len());

        for name in names {
            let name = name.to_str()?;
            let index = *self
                .column_indices
                .get(name)
                .ok_or_else(|| anyhow!("Could not find {} in Postgres row", name))?;
            ordered.push((index, &columns[index]));
        }

        Ok(ordered)
    }
}

impl ExtractValues for PostgresResultRows {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn row_presence(&self) -> Vec<bool> {
        vec![true; self.rows.len()]
    }

    fn extract_values(&self, names: &[Text]) -> Result<Vec<Option<Vec<AggroValue>>>> {
        if names.is_empty() {
            return Ok(vec![Some(Vec::new()); self.rows.len()]);
        }

        let ordered_columns = self.ordered_columns(names)?;

        let mut ret_vec = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut row_vec = Vec::with_capacity(ordered_columns.len());
            for (column_ix, column) in &ordered_columns {
                let field_val = convert_postgres_to_juniper(row, *column_ix, column.type_())?;
                row_vec.push(field_val);
            }
            ret_vec.push(Some(row_vec));
        }
        Ok(ret_vec)
    }

    fn extract_py_values(&self, py: Python, names: &[Bound<'_, PyString>]) -> PuffResult<PyObject> {
        let ordered_columns = self.ordered_py_columns(names)?;

        let ret_vec = PyList::empty(py);
        for row in &self.rows {
            let row_vec = PyList::empty(py);
            for (column_ix, column) in &ordered_columns {
                let field_val = column_to_python(py, *column_ix, column, row)?;
                row_vec.append(field_val)?;
            }
            ret_vec.append(row_vec)?;
        }
        Ok(ret_vec.into_py(py))
    }

    fn extract_first(&self) -> Result<Vec<AggroValue>> {
        let mut ret_vec = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let columns = row.columns();
            let c = columns.first()
                .ok_or(anyhow!("Expected at least one column in query."))?;
            let field_val = convert_postgres_to_juniper(row, 0, c.type_())?;
            ret_vec.push(field_val);
        }
        Ok(ret_vec)
    }
}

pub struct ExtractorRootNode;

impl ExtractValues for ExtractorRootNode {
    fn len(&self) -> usize {
        1
    }
    fn row_presence(&self) -> Vec<bool> {
        vec![true]
    }
    fn extract_values(&self, _names: &[Text]) -> Result<Vec<Option<Vec<AggroValue>>>> {
        bail!("Cannot extract values from the Root")
    }
    fn extract_py_values(&self, _py: Python, _names: &[Bound<'_, PyString>]) -> Result<PyObject> {
        bail!("Cannot extract values from the Root")
    }
    fn extract_first(&self) -> Result<Vec<AggroValue>> {
        Ok(vec![AggroValue::Null])
    }
}

pub struct PythonResultRows {
    pub py_list: Py<PyList>,
}

impl ExtractValues for PythonResultRows {
    fn len(&self) -> usize {
        Python::with_gil(|py| self.py_list.bind(py).len())
    }
    fn row_presence(&self) -> Vec<bool> {
        Python::with_gil(|py| {
            self.py_list
                .bind(py)
                .iter()
                .map(|row| !row.is_none())
                .collect()
        })
    }
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Option<Vec<AggroValue>>>> {
        if names.is_empty() {
            return Ok(self
                .row_presence()
                .into_iter()
                .map(|present| present.then(Vec::new))
                .collect());
        }

        Python::with_gil(|py| {
            let l = self.py_list.bind(py);
            let mut ret_vec = Vec::with_capacity(l.len());

            for row in l.iter() {
                if row.is_none() {
                    ret_vec.push(None);
                    continue;
                }
                let mut row_vec = Vec::with_capacity(names.len());
                for name in names {
                    let val = if let Ok(d) = row.downcast::<PyDict>() {
                        d.get_item(name.as_str())?
                            .ok_or(PyKeyError::new_err(format!(
                                "Could not find {} in parent",
                                name
                            )))?
                    } else {
                        row.getattr(name.as_str())?
                    };
                    let jupiter_val = convert_pyany_to_jupiter(&val);
                    row_vec.push(jupiter_val)
                }
                ret_vec.push(Some(row_vec));
            }
            Ok(ret_vec)
        })
    }

    fn extract_py_values(&self, py: Python, names: &[Bound<'_, PyString>]) -> PuffResult<PyObject> {
        let l = self.py_list.bind(py);
        let final_list = PyList::empty(py);
        for row in l.iter() {
            let row_vec = PyList::empty(py);
            let none = row.is_none();
            for name in names {
                if none {
                    row_vec.append(py.None())?;
                    continue;
                }
                let val = if let Ok(d) = row.downcast::<PyDict>() {
                    d.get_item(name.to_str()?)?
                        .ok_or(PyKeyError::new_err(format!(
                            "Could not find {} in parent",
                            name
                        )))?
                } else {
                    row.getattr(name.to_str()?)?
                };
                row_vec.append(val)?
            }
            final_list.append(row_vec)?;
        }
        Ok(final_list.into_py(py))
    }

    fn extract_first(&self) -> Result<Vec<AggroValue>> {
        Python::with_gil(|py| {
            let l = self.py_list.bind(py);
            let mut ret_vec = Vec::with_capacity(l.len());
            for row in l.iter() {
                if row.is_none() {
                    ret_vec.push(AggroValue::null());
                    continue;
                }
                let val = convert_pyany_to_jupiter(&row);
                ret_vec.push(val);
            }
            Ok(ret_vec)
        })
    }
}
