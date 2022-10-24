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

pub fn convert_pyany_to_jupiter(attribute_val: &PyAny) -> AggroValue {
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
    if let Ok(l) = attribute_val.extract::<&PyList>() {
        return AggroValue::List(l.into_iter().map(|s| convert_pyany_to_jupiter(s)).collect());
    }
    if let Ok(l) = attribute_val.extract::<&PyDict>() {
        return AggroValue::Object(
            l.into_iter()
                .map(|(k, s)| (k.to_string(), convert_pyany_to_jupiter(s)))
                .collect(),
        );
    }
    let size = attribute_val.dir().len();
    let mut obj = Object::with_capacity(size);

    for v in attribute_val.dir().iter() {
        if let Ok(s) = v.downcast::<PyString>() {
            let key = s.to_str().expect("Python dir could not unwrap key to str.");
            if !key.starts_with("__") {
                let py_val = attribute_val
                    .getattr(key)
                    .expect(&format!("Could not get {}", key));
                if !py_val.is_callable() {
                    let juniper_val = convert_pyany_to_jupiter(py_val);
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
            let r: bool = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        &tokio_postgres::types::Type::INT2 | &tokio_postgres::types::Type::INT4 => {
            let r: i32 = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        &tokio_postgres::types::Type::INT8 => {
            let r: i64 = r.get(column_index);
            Ok(if let Ok(s) = r.try_into() {
                AggroValue::Scalar(AggroScalarValue::Int(s))
            } else {
                AggroValue::Scalar(AggroScalarValue::Long(r))
            })
        }
        &tokio_postgres::types::Type::BYTEA => {
            let r = r.get(column_index);
            Ok(AggroValue::Scalar(AggroScalarValue::Binary(
                Bytes::copy_from_slice(r),
            )))
        }
        &tokio_postgres::types::Type::TIMESTAMP => {
            let r = r.get(column_index);
            Ok(AggroValue::Scalar(AggroScalarValue::Datetime(
                UtcDateTime::new(r),
            )))
        }
        &tokio_postgres::types::Type::UUID => {
            let r = r.get(column_index);
            Ok(AggroValue::Scalar(AggroScalarValue::Uuid(r)))
        }
        &tokio_postgres::types::Type::FLOAT4 | &tokio_postgres::types::Type::FLOAT8 => {
            let r: f64 = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        &tokio_postgres::types::Type::TEXT | &tokio_postgres::types::Type::VARCHAR => {
            let r: &str = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        t => {
            panic!("Unsupported postgres type {}", t)
        }
    }
}

pub trait ExtractValues {
    fn len(&self) -> usize;
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Vec<AggroValue>>>;
    fn extract_py_values(&self, py: Python, names: &[&PyString]) -> PuffResult<Py<PyList>>;
    fn extract_first(&self) -> Result<Vec<AggroValue>>;
}

pub struct PostgresResultRows {
    pub statement: Statement,
    pub rows: Vec<Row>,
}

impl ExtractValues for PostgresResultRows {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn extract_values(&self, names: &[Text]) -> Result<Vec<Vec<AggroValue>>> {
        let field_mapping: HashMap<&str, (usize, &Column)> = names
            .iter()
            .flat_map(|field_name| {
                self.statement
                    .columns()
                    .iter()
                    .enumerate()
                    .find_map(|(ix, c)| {
                        if c.name() == field_name.as_str() {
                            Some((c.name(), (ix, c)))
                        } else {
                            None
                        }
                    })
            })
            .collect();

        let mut ret_vec = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut row_vec = Vec::with_capacity(names.len());
            for name in names {
                let (column_ix, c) = if let Some((column_ix, c)) = field_mapping.get(name.as_str())
                {
                    (column_ix, *c)
                } else {
                    bail!("Could not find {} in Postgres row", name)
                };
                let field_val = convert_postgres_to_juniper(row, *column_ix, c.type_())?;
                row_vec.push(field_val);
            }
            ret_vec.push(row_vec);
        }
        Ok(ret_vec)
    }

    fn extract_py_values(&self, py: Python, names: &[&PyString]) -> PuffResult<Py<PyList>> {
        let field_mapping: HashMap<&str, (usize, &Column)> = names
            .iter()
            .flat_map(|field_name| {
                self.statement
                    .columns()
                    .iter()
                    .enumerate()
                    .find_map(|(ix, c)| {
                        if c.name() == field_name.to_str().expect("Expected string") {
                            Some((c.name(), (ix, c)))
                        } else {
                            None
                        }
                    })
            })
            .collect();

        let ret_vec = PyList::empty(py);
        for row in &self.rows {
            let row_vec = PyList::empty(py);
            for name in names {
                let (column_ix, c) = if let Some((column_ix, c)) = field_mapping.get(name.to_str()?)
                {
                    (column_ix, *c)
                } else {
                    bail!("Could not find {} in Postgres row", name)
                };
                let field_val = column_to_python(py, *column_ix, c, row)?;
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
            let c = columns
                .get(0)
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
    fn extract_values(&self, _names: &[Text]) -> Result<Vec<Vec<AggroValue>>> {
        bail!("Cannot extract values from the Root")
    }
    fn extract_py_values(&self, _py: Python, _names: &[&PyString]) -> Result<Py<PyList>> {
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
        Python::with_gil(|py| self.py_list.as_ref(py).len())
    }
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Vec<AggroValue>>> {
        Python::with_gil(|py| {
            let l = self.py_list.as_ref(py);
            let mut ret_vec = Vec::with_capacity(l.len());

            for row in l {
                let mut row_vec = Vec::with_capacity(names.len());
                for name in names {
                    let val = if let Ok(d) = row.downcast::<PyDict>() {
                        d.get_item(name.as_str())
                            .ok_or(PyKeyError::new_err(format!(
                                "Could not find {} in parent",
                                name
                            )))?
                    } else {
                        row.getattr(name.as_str())?
                    };
                    let jupiter_val = convert_pyany_to_jupiter(val);
                    row_vec.push(jupiter_val)
                }
                ret_vec.push(row_vec);
            }
            Ok(ret_vec)
        })
    }

    fn extract_py_values(&self, py: Python, names: &[&PyString]) -> PuffResult<Py<PyList>> {
        let l = self.py_list.as_ref(py);
        let final_list = PyList::empty(py);
        for row in l {
            let row_vec = PyList::empty(py);
            for name in names {
                let val = if let Ok(d) = row.downcast::<PyDict>() {
                    d.get_item(name.to_str()?)
                        .ok_or(PyKeyError::new_err(format!(
                            "Could not find {} in parent",
                            name.to_string()
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
            let l = self.py_list.as_ref(py);
            let mut ret_vec = Vec::with_capacity(l.len());

            for row in l {
                let val = convert_pyany_to_jupiter(row);
                ret_vec.push(val);
            }
            Ok(ret_vec)
        })
    }
}
