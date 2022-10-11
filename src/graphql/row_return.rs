use crate::graphql::scalar::{AggroScalarValue, AggroValue};

use crate::types::Text;
use anyhow::{anyhow, bail, Result};
use juniper::{InputValue, Object};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use std::collections::HashMap;
use tokio_postgres::{Column, Row, Statement};
use crate::errors::PuffResult;

pub fn convert_pyany_to_jupiter(attribute_val: &PyAny) -> AggroValue {
    if let Ok(s) = attribute_val.extract() {
        return AggroValue::Scalar(AggroScalarValue::String(s));
    }
    if let Ok(s) = attribute_val.extract() {
        return AggroValue::Scalar(AggroScalarValue::Boolean(s));
    }
    if let Ok(s) = attribute_val.extract() {
        return AggroValue::Scalar(AggroScalarValue::Int(s));
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
    match *t {
        tokio_postgres::types::Type::BOOL => {
            let r: bool = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        tokio_postgres::types::Type::INT2 | tokio_postgres::types::Type::INT4 => {
            let r: i32 = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        tokio_postgres::types::Type::FLOAT4 | tokio_postgres::types::Type::FLOAT8 => {
            let r: f64 = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        tokio_postgres::types::Type::TEXT | tokio_postgres::types::Type::VARCHAR => {
            let r: &str = r.get(column_index);
            Ok(AggroValue::scalar(r))
        }
        _ => {
            panic!("Unsupported postgres type")
        }
    }
}

pub trait ExtractValues {
    fn len(&self) -> usize;
    fn extract_values(&self, names: &[Text]) -> Result<Vec<Vec<AggroValue>>>;
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
            let py_none = py.None();
            let none = py_none.as_ref(py);

            for row in l {
                let mut row_vec = Vec::with_capacity(names.len());
                for name in names {
                    let val = if let Ok(d) = row.downcast::<PyDict>() {
                        d.getattr(name.as_str()).unwrap_or(none)
                    } else {
                        row.getattr(name.as_str()).unwrap_or(none)
                    };
                    let jupiter_val = convert_pyany_to_jupiter(val);
                    row_vec.push(jupiter_val)
                }
                ret_vec.push(row_vec);
            }
            Ok(ret_vec)
        })
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
