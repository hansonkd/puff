//! Call Graphql from Python
use crate::context::with_puff_context;
use crate::errors::PuffResult;
use crate::graphql::PuffGraphqlConfig;
use crate::prelude::ToText;
use crate::python::async_python::run_python_async;
use crate::python::postgres::Connection;
use crate::types::Text;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::{PyObject, PyResult, Python};
use std::sync::Arc;
use tokio::sync::mpsc::{channel, Receiver};
use tokio::sync::Mutex;

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
        with_puff_context(|ctx| PythonGraphql(ctx.gql())).to_object(py)
    }

    fn by_name(&self, py: Python, name: &str) -> PyObject {
        with_puff_context(|ctx| PythonGraphql(ctx.gql_named(name))).to_object(py)
    }
}

#[pyclass]
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct StreamReceiver(Arc<Mutex<Receiver<PuffResult<(Option<Text>, PyObject)>>>>);

impl ToPyObject for StreamReceiver {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}

#[pymethods]
impl StreamReceiver {
    fn __call__(&self, ret_func: PyObject) {
        let rec = self.0.clone();
        run_python_async(ret_func, async move {
            if let Some(r) = rec.lock().await.recv().await {
                r.map(|f| Python::with_gil(|py| f.into_py(py)))
            } else {
                Python::with_gil(|py| Ok(py.None()))
            }
        })
    }
}

/// Query a graphql schema from Python
#[pyclass]
pub struct PythonGraphql(PuffGraphqlConfig);

impl Clone for PythonGraphql {
    fn clone(&self) -> Self {
        PythonGraphql(self.0.clone())
    }
}

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
        variables: Bound<'_, PyDict>,
        conn: Option<&Connection>,
        auth: Option<PyObject>,
    ) -> PyResult<()> {
        // Convert Python dict to serde_json::Value for variables
        let vars_json = Python::with_gil(|_py| -> PyResult<serde_json::Value> {
            let mut map = serde_json::Map::new();
            for (k, v) in variables.iter() {
                let key = k.to_string();
                let json_val = pythonize::depythonize(&v).unwrap_or(serde_json::Value::Null);
                map.insert(key, json_val);
            }
            Ok(serde_json::Value::Object(map))
        })?;

        let schema = self.0.schema();
        let context = if let Some(c) = conn {
            self.0.new_context_with_connection(auth, Some(c.clone()))
        } else {
            self.0.new_context(auth)
        };
        let ctx_arc = Arc::new(context);

        run_python_async(return_fun, async move {
            let mut request = async_graphql::Request::new(&query);
            let vars = async_graphql::Variables::from_json(vars_json);
            request = request.variables(vars);
            let request = request.data(ctx_arc);
            let response = schema.execute(request).await;

            convert_response_to_python(&response)
        });
        Ok(())
    }

    /// Subscribe to GraphQL (streaming)
    pub fn subscribe(
        &self,
        return_fun: PyObject,
        query: String,
        variables: Bound<'_, PyDict>,
        conn: Option<&Connection>,
        auth: Option<PyObject>,
    ) -> PyResult<()> {
        let vars_json = Python::with_gil(|_py| -> PyResult<serde_json::Value> {
            let mut map = serde_json::Map::new();
            for (k, v) in variables.iter() {
                let key = k.to_string();
                let json_val = pythonize::depythonize(&v).unwrap_or(serde_json::Value::Null);
                map.insert(key, json_val);
            }
            Ok(serde_json::Value::Object(map))
        })?;

        let schema = self.0.schema();
        let context = if let Some(c) = conn {
            self.0.new_context_with_connection(auth, Some(c.clone()))
        } else {
            self.0.new_context(auth)
        };
        let ctx_arc = Arc::new(context);

        run_python_async(return_fun, async move {
            let (sender, rec) = channel(1);
            let this_sender = sender.clone();
            let fut = async move {
                let mut request = async_graphql::Request::new(&query);
                let vars = async_graphql::Variables::from_json(vars_json);
                request = request.variables(vars);
                let request = request.data(ctx_arc);

                use futures_util::StreamExt;
                let mut stream = schema.execute_stream(request);

                while let Some(response) = stream.next().await {
                    let py_val = convert_response_to_python(&response)?;

                    // Extract field name from response data
                    let field_name = if let serde_json::Value::Object(ref data) =
                        serde_json::to_value(&response.data).unwrap_or(serde_json::Value::Null)
                    {
                        data.keys().next().map(|k| k.to_text())
                    } else {
                        None
                    };

                    if sender.send(Ok((field_name, py_val))).await.is_err() {
                        break;
                    }
                }

                Ok(())
            };

            let handle = with_puff_context(|ctx| ctx.handle());
            handle.spawn(async move {
                if let Err(e) = fut.await {
                    this_sender.send(Err(e)).await.unwrap_or_default();
                }
            });

            PuffResult::Ok(StreamReceiver(Arc::new(Mutex::new(rec))))
        });
        Ok(())
    }
}

fn convert_response_to_python(response: &async_graphql::Response) -> PuffResult<PyObject> {
    Python::with_gil(|py| {
        let pydict = PyDict::new(py);

        // Convert data
        let data_json = serde_json::to_value(&response.data).unwrap_or(serde_json::Value::Null);
        let data_py = json_to_python(py, &data_json)?;
        pydict.set_item("data", data_py)?;

        // Convert errors
        if !response.errors.is_empty() {
            let py_errors = PyList::empty(py);
            for error in &response.errors {
                let error_dict = PyDict::new(py);
                error_dict.set_item("error", error.message.clone())?;
                let path: Vec<String> = error.path.iter().map(|p| format!("{:?}", p)).collect();
                error_dict.set_item("path", path)?;
                let locations: Vec<String> = error
                    .locations
                    .iter()
                    .map(|l| format!("{}:{}", l.line, l.column))
                    .collect();
                error_dict.set_item("location", format!("{:?}", locations))?;
                py_errors.append(error_dict)?;
            }
            pydict.set_item("errors", py_errors)?;
        }

        let r: PyObject = pydict.unbind().into();
        Ok(r)
    })
}

fn json_to_python(py: Python, v: &serde_json::Value) -> PuffResult<PyObject> {
    match v {
        serde_json::Value::Null => Ok(py.None()),
        serde_json::Value::Bool(b) => Ok(b.into_py(py)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_py(py))
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_py(py))
            } else {
                Ok(py.None())
            }
        }
        serde_json::Value::String(s) => Ok(s.clone().into_py(py)),
        serde_json::Value::Array(arr) => {
            let mut vec = Vec::with_capacity(arr.len());
            for item in arr {
                vec.push(json_to_python(py, item)?);
            }
            Ok(PyList::new(py, vec)?.into_py(py))
        }
        serde_json::Value::Object(obj) => {
            let dict = PyDict::new(py);
            for (k, v) in obj {
                dict.set_item(k, json_to_python(py, v)?)?;
            }
            Ok(dict.into_py(py))
        }
    }
}
