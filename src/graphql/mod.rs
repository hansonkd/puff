//! Use Python Dataclasses to Define a GQL Schema (async-graphql dynamic)
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use pyo3::{Py, PyAny, PyObject, PyResult, Python};
use std::sync::Arc;

pub use handlers::{handle_graphql, handle_graphql_named, handle_subscriptions, handle_subscriptions_named, playground};
pub mod handlers;
pub(crate) mod puff_schema;
mod row_return;
pub(crate) mod scalar;
mod schema;

use crate::context::with_puff_context;
use crate::errors::PuffResult;
pub use puff_schema::AggroContext;

use crate::python::postgres::Connection;
use crate::python::PythonDispatcher;
use crate::types::Text;

pub struct PuffGraphqlConfig {
    schema: async_graphql::dynamic::Schema,
    db: Option<Text>,
    pub(crate) auth: Option<PyObject>,
    pub(crate) auth_async: bool,
    shared_connection: Option<Connection>,
}

impl Clone for PuffGraphqlConfig {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            schema: self.schema.clone(),
            db: self.db.clone(),
            auth: self.auth.as_ref().map(|o| o.clone_ref(py)),
            auth_async: self.auth_async,
            shared_connection: self.shared_connection.clone(),
        })
    }
}

impl PuffGraphqlConfig {
    pub fn schema(&self) -> async_graphql::dynamic::Schema {
        self.schema.clone()
    }

    pub fn new_context(&self, auth: Option<PyObject>) -> AggroContext {
        AggroContext::new_with_connection(auth, self.shared_connection.clone(), self.clone())
    }

    pub fn new_context_with_connection(
        &self,
        auth: Option<PyObject>,
        conn: Option<Connection>,
    ) -> AggroContext {
        let conn = conn.or_else(|| self.shared_connection.clone());
        AggroContext::new_with_connection(auth, conn, self.clone())
    }

    pub fn is_shared_connection(&self, conn: &Connection) -> bool {
        match &self.shared_connection {
            Some(shared) => shared.identity() == conn.identity(),
            None => false,
        }
    }
}

pub(crate) async fn load_schema(
    module: Text,
    db: Option<Text>,
    _py_dispatcher: PythonDispatcher,
) -> PyResult<PuffGraphqlConfig> {
    let import_string_fn = Python::with_gil(|py| -> PyResult<_> {
        let puff = py.import("puff")?;
        let import_string_fn = puff.getattr("import_string")?.to_object(py);
        Ok(import_string_fn)
    })?;

    let schema_py = Python::with_gil(|py| import_string_fn.call1(py, (module.as_str(),)))?;
    let (auth, auth_async, converted_objs, input_objs) = Python::with_gil(|py| -> PyResult<_> {
        let puff_gql = py.import("puff.graphql")?;
        let t2d = puff_gql.getattr("type_to_description")?;
        let ret = puff_schema::convert(py, schema_py.bind(py), &t2d)?;
        Ok(ret)
    })?;

    let all_objs = Arc::new(converted_objs);
    let input_objs_arc = Arc::new(input_objs);

    // Create a shared connection whose background task (and its prepared-statement
    // cache) persists across requests.
    let shared_connection = if let Some(ref db_name) = db {
        let pg = with_puff_context(|ctx| ctx.postgres_named(db_name.as_str()));
        let conn = Connection::new(pg.pool());
        crate::python::postgres::set_autocommit(&conn, true).await;
        Some(conn)
    } else {
        None
    };

    // Build a temporary config (without schema) to pass to build_schema for subscription support
    let temp_schema = {
        use async_graphql::dynamic::*;
        Schema::build("_TempQ", None, None)
            .register(Object::new("_TempQ").field(
                Field::new("_e", TypeRef::named(TypeRef::BOOLEAN), |_| {
                    FieldFuture::new(async { Ok(None::<FieldValue<'_>>) })
                }),
            ))
            .finish()
            .expect("temp schema")
    };
    let temp_config = PuffGraphqlConfig {
        schema: temp_schema,
        db: db.clone(),
        auth: Python::with_gil(|py| auth.as_ref().map(|o| o.clone_ref(py))),
        auth_async,
        shared_connection: shared_connection.clone(),
    };

    let dynamic_schema = schema::build_schema(&all_objs, &input_objs_arc, &temp_config)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Schema build failed: {}", e)))?;

    Ok(PuffGraphqlConfig {
        schema: dynamic_schema,
        auth,
        auth_async,
        db,
        shared_connection,
    })
}

/// Convert async_graphql::Value to Python object.
#[allow(dead_code)]
pub(crate) fn gql_value_to_python(py: Python, v: &async_graphql::Value) -> PuffResult<Py<PyAny>> {
    match v {
        async_graphql::Value::Null => Ok(Python::None(py)),
        async_graphql::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_py(py))
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_py(py))
            } else {
                Ok(Python::None(py))
            }
        }
        async_graphql::Value::String(s) => Ok(s.clone().into_py(py)),
        async_graphql::Value::Boolean(b) => Ok(b.into_py(py)),
        async_graphql::Value::List(inner) => {
            let mut val_vec: Vec<PyObject> = Vec::with_capacity(inner.len());
            for iv in inner {
                val_vec.push(gql_value_to_python(py, iv)?);
            }
            Ok(PyList::new(py, val_vec)?.into())
        }
        async_graphql::Value::Object(inner) => {
            let mut val_vec: Vec<(PyObject, PyObject)> = Vec::with_capacity(inner.len());
            for (k, iv) in inner {
                val_vec.push((
                    PyString::new(py, k.as_str()).into(),
                    gql_value_to_python(py, iv)?,
                ));
            }
            let seq = PyList::new(py, val_vec)?;
            Ok(PyDict::from_sequence(&seq.into_any())?.into())
        }
        _ => Ok(Python::None(py)),
    }
}
