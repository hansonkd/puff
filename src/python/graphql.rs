use std::collections::HashMap;
use juniper::execute;
use pyo3::{PyObject, PyResult, Python};
use pyo3::types::{PyDict, PyList};
use pyo3::prelude::*;
use crate::context::with_puff_context;
use crate::errors::{PuffResult, to_py_error};
use crate::graphql::{AggroContext, convert_pyany_to_input, juniper_value_to_python};
use crate::python::greenlet::greenlet_async;

#[pyclass]
#[derive(Clone)]
pub struct PythonGraphql;

impl ToPyObject for PythonGraphql {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}

#[pymethods]
impl PythonGraphql {
    fn query(&self, py: Python, return_fun: PyObject, query: String, variables: &PyDict) -> PyResult<()> {
        let ctx = with_puff_context(|ctx| ctx);
        let mut hm = HashMap::with_capacity(variables.len());
        for (k, v) in variables {
            let variables = to_py_error("GQL Inputs", convert_pyany_to_input(v))?;
            hm.insert(k.to_string(), variables);
        }
        greenlet_async(ctx.clone(), return_fun, async move {
            let gql = ctx.gql();
            let (value, errors) = execute(query.as_str(), None, &gql, &hm, &AggroContext::new()).await?;
            Python::with_gil(|py| {
                let pydict = PyDict::new(py);
                let data = juniper_value_to_python(py, &value)?;
                if errors.is_empty() {
                    let py_errors = PyList::empty(py);
                    for error in errors {
                        let pydict = PyDict::new(py);
                        pydict.set_item("path", error.path())?;
                        pydict.set_item("error", format!("{:?}", error.error()))?;
                        pydict.set_item("location", format!("{:?}", error.location()))?;
                        py_errors.append(pydict)?;
                    }
                    pydict.set_item("errors", py_errors)?;
                }

                pydict.set_item("data", data)?;
                let r: PyObject = pydict.into_py(py);
                Ok(r)
            })
        });
        Ok(())
    }
}