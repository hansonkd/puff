//! Convert Postgres rows and Python objects to async_graphql::Value.

use crate::errors::PuffResult;
use crate::graphql::scalar::*;
use crate::python::postgres::column_to_python;
use crate::types::Text;
use anyhow::{anyhow, bail, Result};
use async_graphql::Value as GqlValue;
use indexmap::IndexMap;
use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use std::collections::HashMap;
use tokio_postgres::{Column, Row, Statement};

/// Convert a Python object to async_graphql::Value.
pub fn convert_pyany_to_value(attribute_val: &Bound<'_, PyAny>) -> Result<GqlValue> {
    if attribute_val.is_none() {
        return Ok(GqlValue::Null);
    }
    if let Ok(s) = attribute_val.extract::<String>() {
        return Ok(GqlValue::String(s));
    }
    if let Ok(s) = attribute_val.extract::<bool>() {
        return Ok(GqlValue::Boolean(s));
    }
    if let Ok(r) = attribute_val.extract::<i64>() {
        return Ok(GqlValue::Number(r.into()));
    }
    if let Ok(s) = attribute_val.extract::<f64>() {
        return Ok(match serde_json::Number::from_f64(s) {
            Some(n) => GqlValue::Number(n),
            None => GqlValue::Null,
        });
    }
    if let Ok(l) = attribute_val.downcast::<PyList>() {
        let items: Result<Vec<_>> = l.iter().map(|s| convert_pyany_to_value(&s)).collect();
        return Ok(GqlValue::List(items?));
    }
    if let Ok(l) = attribute_val.downcast::<PyDict>() {
        let mut obj = IndexMap::with_capacity(l.len());
        for (k, s) in l.iter() {
            obj.insert(
                async_graphql::Name::new(k.to_string()),
                convert_pyany_to_value(&s)?,
            );
        }
        return Ok(GqlValue::Object(obj));
    }
    if let Ok(dict_obj) = attribute_val.getattr("__dict__") {
        if let Ok(dict) = dict_obj.downcast::<PyDict>() {
            if !dict.is_empty() {
                let mut obj = IndexMap::with_capacity(dict.len());
                for (key, value) in dict.iter() {
                    let key = key.to_string();
                    if !key.starts_with("__") {
                        obj.insert(
                            async_graphql::Name::new(&key),
                            convert_pyany_to_value(&value)?,
                        );
                    }
                }
                return Ok(GqlValue::Object(obj));
            }
        }
    }
    let dir = attribute_val
        .dir()
        .map_err(|e| anyhow!("Failed to call dir(): {}", e))?;
    let size = dir.len();
    let mut obj = IndexMap::with_capacity(size);

    for v in dir.iter() {
        if let Ok(s) = v.downcast::<PyString>() {
            let key = s
                .to_str()
                .map_err(|e| anyhow!("Python dir could not unwrap key to str: {}", e))?;
            if !key.starts_with("__") {
                let py_val = attribute_val
                    .getattr(key)
                    .map_err(|e| anyhow!("Could not get attribute '{}': {}", key, e))?;
                if !py_val.is_callable() {
                    let gql_val = convert_pyany_to_value(&py_val)?;
                    obj.insert(async_graphql::Name::new(key), gql_val);
                }
            }
        }
    }
    Ok(GqlValue::Object(obj))
}

/// Convert a Postgres column value to async_graphql::Value.
pub fn convert_postgres_to_value(
    r: &Row,
    column_index: usize,
    t: &tokio_postgres::types::Type,
) -> Result<GqlValue> {
    match t {
        &tokio_postgres::types::Type::BOOL => {
            if let Some(r) = r.get::<_, Option<bool>>(column_index) {
                Ok(gql_bool(r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::INT2 | &tokio_postgres::types::Type::INT4 => {
            if let Some(r) = r.get::<_, Option<i32>>(column_index) {
                Ok(gql_int(r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::INT8 => {
            if let Some(r) = r.get::<_, Option<i64>>(column_index) {
                Ok(gql_long(r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::BYTEA => {
            if let Some(r) = r.get::<_, Option<&[u8]>>(column_index) {
                Ok(gql_binary(r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::TIMESTAMP => {
            if let Some(r) = r.get::<_, Option<chrono::NaiveDateTime>>(column_index) {
                let dt: chrono::DateTime<chrono::Utc> =
                    chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(r, chrono::Utc);
                Ok(gql_datetime(&dt))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::UUID => {
            if let Some(r) = r.get::<_, Option<uuid::Uuid>>(column_index) {
                Ok(gql_uuid(&r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::FLOAT4 | &tokio_postgres::types::Type::FLOAT8 => {
            if let Some(r) = r.get::<_, Option<f64>>(column_index) {
                Ok(gql_float(r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        &tokio_postgres::types::Type::TEXT | &tokio_postgres::types::Type::VARCHAR => {
            if let Some(r) = r.get::<_, Option<&str>>(column_index) {
                Ok(gql_string(r))
            } else {
                Ok(GqlValue::Null)
            }
        }
        t => {
            bail!("Unsupported postgres type {}", t)
        }
    }
}

pub trait ExtractValues {
    fn len(&self) -> usize;
    fn row_presence(&self) -> Vec<bool>;
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Option<Vec<GqlValue>>>>;
    fn extract_py_values(&self, py: Python, names: &[Bound<'_, PyString>]) -> PuffResult<PyObject>;
    fn extract_first(&self) -> Result<Vec<GqlValue>>;
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

    fn extract_values(&self, names: &[Text]) -> Result<Vec<Option<Vec<GqlValue>>>> {
        if names.is_empty() {
            return Ok(vec![Some(Vec::new()); self.rows.len()]);
        }

        let ordered_columns = self.ordered_columns(names)?;

        let mut ret_vec = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut row_vec = Vec::with_capacity(ordered_columns.len());
            for (column_ix, column) in &ordered_columns {
                let field_val = convert_postgres_to_value(row, *column_ix, column.type_())?;
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

    fn extract_first(&self) -> Result<Vec<GqlValue>> {
        let mut ret_vec = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let columns = row.columns();
            let c = columns
                .first()
                .ok_or(anyhow!("Expected at least one column in query."))?;
            let field_val = convert_postgres_to_value(row, 0, c.type_())?;
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
    fn extract_values(&self, _names: &[Text]) -> Result<Vec<Option<Vec<GqlValue>>>> {
        bail!("Cannot extract values from the Root")
    }
    fn extract_py_values(&self, _py: Python, _names: &[Bound<'_, PyString>]) -> Result<PyObject> {
        bail!("Cannot extract values from the Root")
    }
    fn extract_first(&self) -> Result<Vec<GqlValue>> {
        Ok(vec![GqlValue::Null])
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
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Option<Vec<GqlValue>>>> {
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
                    let gql_val = convert_pyany_to_value(&val)?;
                    row_vec.push(gql_val)
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

    fn extract_first(&self) -> Result<Vec<GqlValue>> {
        Python::with_gil(|py| {
            let l = self.py_list.bind(py);
            let mut ret_vec = Vec::with_capacity(l.len());
            for row in l.iter() {
                if row.is_none() {
                    ret_vec.push(GqlValue::Null);
                    continue;
                }
                let val = convert_pyany_to_value(&row)?;
                ret_vec.push(val);
            }
            Ok(ret_vec)
        })
    }
}
