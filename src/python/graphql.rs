//! Call Graphql from Python
use crate::context::with_puff_context;
use crate::errors::{PuffResult, to_py_error};
use crate::graphql::{convert_pyany_to_input, juniper_value_to_python, AggroContext, PuffGraphqlRoot};
use crate::python::async_python::run_python_async;
use crate::python::postgres::Connection;
use juniper::{execute, ExecutionError, Value};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::{PyObject, PyResult, Python};
use std::collections::HashMap;
use crate::graphql::scalar::AggroScalarValue;

/// Access the Global graphql context
#[pyclass]
#[derive(Clone)]
pub struct GlobalGraphQL;

impl ToPyObject for GlobalGraphQL {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}
#[pymethods]
impl GlobalGraphQL {
    fn __call__(&self, py: Python) -> PyObject {
        with_puff_context(|ctx| PythonGraphql(ctx.gql().root())).to_object(py)
    }
}


/// Query a graphql schema from Python
#[pyclass]
#[derive(Clone)]
pub struct PythonGraphql(PuffGraphqlRoot);

impl ToPyObject for PythonGraphql {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}

#[pymethods]
impl PythonGraphql {
    /// Query the GraphQL result Asynchronously
    pub fn query(
        &self,
        return_fun: PyObject,
        query: String,
        variables: &PyDict,
        conn: Option<&Connection>,
        auth: Option<PyObject>,
    ) -> PyResult<()> {
        let mut hm = HashMap::with_capacity(variables.len());
        for (k, v) in variables {
            let variables = to_py_error("GQL Inputs", convert_pyany_to_input(v))?;
            hm.insert(k.to_string(), variables);
        }
        let this_root = self.0.clone();
        let this_conn = conn.map(|f| f.clone()).or_else(|| {
            let pool = with_puff_context(|ctx| ctx.postgres_safe());
            pool.map(|p| Connection::new(p.pool()))
        });
        run_python_async(return_fun, async move {
            let (value, errors) = execute(
                query.as_str(),
                None,
                &this_root,
                &hm,
                &AggroContext::new_with_connection(auth, this_conn),
            )
            .await?;

            convert_execution_response(&value, errors)
        });
        Ok(())
    }
}


fn convert_execution_response(value: &Value<AggroScalarValue>, errors: Vec<ExecutionError<AggroScalarValue>>) -> PuffResult<PyObject> {
    Python::with_gil(|py| {
        let pydict = PyDict::new(py);
        let data = juniper_value_to_python(py, &value)?;
        if !errors.is_empty() {
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
}