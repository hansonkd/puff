//! Call Graphql from Python
use crate::context::with_puff_context;
use crate::errors::{to_py_error, PuffResult};
use crate::graphql::scalar::AggroScalarValue;
use crate::graphql::{
    convert_pyany_to_input, juniper_value_to_python, AggroContext, PuffGraphqlRoot,
};
use crate::prelude::ToText;
use crate::python::async_python::run_python_async;
use crate::python::postgres::Connection;
use crate::types::Text;
use anyhow::bail;
use juniper::executor::{get_operation, resolve_validated_subscription};
use juniper::parser::parse_document_source;
use juniper::validation::{validate_input_values, visit_all_rules, ValidatorContext};
use juniper::{execute, ExecutionError, GraphQLError, Value};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::{PyObject, PyResult, Python};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::{channel, Receiver};
use tokio::sync::Mutex;
use tokio_stream::{StreamExt, StreamMap};

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

    fn by_name(&self, py: Python, name: &str) -> PyObject {
        with_puff_context(|ctx| PythonGraphql(ctx.gql_named(name).root())).to_object(py)
    }
}

#[pyclass]
#[derive(Clone)]
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

    /// Query the GraphQL result Asynchronously
    pub fn subscribe(
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
            let (sender, rec) = channel(1);
            let this_sender = sender.clone();
            let fut = async move {
                let document = parse_document_source(query.as_str(), &this_root.schema)?;

                {
                    let mut ctx = ValidatorContext::new(&this_root.schema, &document);
                    visit_all_rules(&mut ctx, &document);

                    let errors = ctx.into_errors();
                    if !errors.is_empty() {
                        Err(GraphQLError::ValidationError(errors))?;
                    }
                }

                let operation = get_operation(&document, None)?;

                {
                    let errors = validate_input_values(&hm, operation, &this_root.schema);

                    if !errors.is_empty() {
                        Err(GraphQLError::ValidationError(errors))?;
                    }
                }

                let ctx = AggroContext::new_with_connection(auth, this_conn);
                let (value, errors) =
                    resolve_validated_subscription(&document, operation, &this_root, &hm, &ctx)
                        .await?;

                if !errors.is_empty() {
                    let py_val = convert_execution_response(&Value::null(), errors)?;
                    if let Err(_) = sender.send(Ok((None, py_val))).await {
                        return Ok(());
                    }
                }

                let response_returned_object = match value {
                    Value::Object(o) => o,
                    _ => bail!("Expected object from subscription."),
                };

                let fields = response_returned_object.into_iter();

                let mut stream_map = StreamMap::new();

                for (name, stream_val) in fields {
                    // since macro returns Value::Scalar(iterator) every time,
                    // other variants may be skipped
                    match stream_val {
                        Value::Scalar(stream) => {
                            stream_map.insert(name.to_text(), stream);
                        }
                        _ => unreachable!(),
                    }
                }

                while let Some((name, nv)) = stream_map.next().await {
                    let py_val = match nv {
                        Ok(v) => convert_execution_response(&v, vec![])?,
                        Err(e) => convert_execution_response(&Value::null(), vec![e])?,
                    };
                    if let Err(_) = sender.send(Ok((Some(name), py_val))).await {
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

fn convert_execution_response(
    value: &Value<AggroScalarValue>,
    errors: Vec<ExecutionError<AggroScalarValue>>,
) -> PuffResult<PyObject> {
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
