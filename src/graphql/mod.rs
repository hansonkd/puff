//! Use Python Dataclasses to Define a GQL Schema
use anyhow::bail;
use juniper::{InputValue, RootNode, Spanning};
use pyo3::prelude::*;
use pyo3::exceptions::PyRuntimeError;
use pyo3::{Bound, IntoPy, Py, PyAny, PyObject, PyResult, Python, ToPyObject};
use std::sync::Arc;

pub use handlers::{handle_graphql, handle_subscriptions};
pub mod handlers;
mod puff_schema;
mod row_return;
pub(crate) mod scalar;
mod schema;
use crate::context::with_puff_context;
use crate::errors::PuffResult;
pub use puff_schema::AggroContext;
use pyo3::types::{PyDict, PyList, PyString};

use crate::graphql::scalar::{AggroScalarValue, AggroValue};
use crate::graphql::schema::PuffGqlObject;
use crate::python::postgres::Connection;
use crate::python::PythonDispatcher;
use crate::types::text::ToText;
use crate::types::Text;

pub(crate) type PuffGraphqlRoot =
    Arc<RootNode<'static, PuffGqlObject, PuffGqlObject, PuffGqlObject, AggroScalarValue>>;

pub struct PuffGraphqlConfig {
    root: PuffGraphqlRoot,
    db: Option<Text>,
    auth: Option<PyObject>,
    auth_async: bool,
    /// A long-lived connection whose background task retains the prepared-statement
    /// cache across GraphQL requests. Created once at startup and reused for every
    /// request that does **not** supply its own connection (e.g. Django's
    /// `borrow_db_context`).
    shared_connection: Option<Connection>,
}

impl Clone for PuffGraphqlConfig {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            root: self.root.clone(),
            db: self.db.clone(),
            auth: self.auth.as_ref().map(|o| o.clone_ref(py)),
            auth_async: self.auth_async,
            shared_connection: self.shared_connection.clone(),
        })
    }
}

impl PuffGraphqlConfig {
    pub fn root(&self) -> PuffGraphqlRoot {
        self.root.clone()
    }
    pub fn new_context(&self, auth: Option<PyObject>) -> AggroContext {
        // Reuse the shared connection (and its prepared-statement cache) when
        // no explicit connection is provided.  The shared connection runs in
        // autocommit mode so requests are isolated.
        AggroContext::new_with_connection(auth, self.shared_connection.clone(), self.clone())
    }

    pub fn new_context_with_connection(
        &self,
        auth: Option<PyObject>,
        conn: Option<Connection>,
    ) -> AggroContext {
        // Use the caller-supplied connection (e.g. Django's borrow_db_context)
        // or fall back to the shared one.
        let conn = conn.or_else(|| self.shared_connection.clone());
        AggroContext::new_with_connection(auth, conn, self.clone())
    }

    /// Returns `true` if the given connection is the shared, long-lived one
    /// and must **not** be closed after a single request.
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
    py_dispatcher: PythonDispatcher,
) -> PyResult<PuffGraphqlConfig> {
    let import_string_fn = Python::with_gil(|py| -> PyResult<_> {
        let puff = py.import("puff")?;
        let import_string_fn = puff.getattr("import_string")?.to_object(py);
        Ok(import_string_fn)
    })?;

    let schema = py_dispatcher
        .dispatch1(import_string_fn, (module,))?
        .await
        .map_err(|e| PyRuntimeError::new_err(
            format!("Schema loading task failed: {}", e)
        ))??;
    let (auth, auth_async, converted_objs, input_objs) = Python::with_gil(|py| -> PyResult<_> {
        let puff_gql = py.import("puff.graphql")?;
        let t2d = puff_gql.getattr("type_to_description")?;

        let ret = puff_schema::convert(py, schema.bind(py), &t2d)?;
        Ok(ret)
    })?;

    let info = schema::SchemaInfo {
        name: "Query".to_text(),
        all_objs: Arc::new(converted_objs),
        input_objs: Arc::new(input_objs),
        commit: false,
    };
    let mutation_info = schema::SchemaInfo {
        name: "Mutation".to_text(),
        all_objs: info.all_objs.clone(),
        input_objs: info.input_objs.clone(),
        commit: true,
    };
    let subscription_info = schema::SchemaInfo {
        name: "Subscription".to_text(),
        all_objs: info.all_objs.clone(),
        input_objs: info.input_objs.clone(),
        commit: false,
    };
    let object = schema::PuffGqlObject::new();

    let schema: RootNode<_, _, _, AggroScalarValue> = RootNode::new_with_info(
        object.clone(),
        object.clone(),
        object.clone(),
        info,
        mutation_info,
        subscription_info,
    );

    // Create a shared connection whose background task (and its prepared-statement
    // cache) persists across requests.  Autocommit is enabled so that each request's
    // queries are independent — no transaction bleeds across requests.
    let shared_connection = if let Some(ref db_name) = db {
        let pg = with_puff_context(|ctx| ctx.postgres_named(db_name.as_str()));
        let conn = Connection::new(pg.pool());
        crate::python::postgres::set_autocommit(&conn, true).await;
        Some(conn)
    } else {
        None
    };

    Ok(PuffGraphqlConfig {
        root: Arc::new(schema),
        auth,
        auth_async,
        db,
        shared_connection,
    })
}

pub(crate) fn juniper_value_to_python(py: Python, v: &AggroValue) -> PuffResult<Py<PyAny>> {
    match v {
        AggroValue::List(inner) => {
            let mut val_vec: Vec<PyObject> = Vec::with_capacity(inner.len());
            for iv in inner {
                val_vec.push(juniper_value_to_python(py, iv)?);
            }
            Ok(PyList::new(py, val_vec)?.into())
        }
        AggroValue::Object(inner) => {
            let mut val_vec: Vec<(PyObject, PyObject)> = Vec::with_capacity(inner.field_count());
            for (k, iv) in inner.iter() {
                val_vec.push((
                    PyString::new(py, k).into(),
                    juniper_value_to_python(py, iv)?,
                ));
            }
            let seq = PyList::new(py, val_vec)?;
            Ok(PyDict::from_sequence(&seq.into_any())?.into())
        }
        AggroValue::Scalar(s) => scalar_to_python(py, s),
        AggroValue::Null => Ok(Python::None(py)),
    }
}

fn scalar_to_python(py: Python, v: &AggroScalarValue) -> PuffResult<Py<PyAny>> {
    match v {
        AggroScalarValue::String(s) => Ok(s.clone().into_py(py)),
        AggroScalarValue::Int(s) => Ok(s.into_py(py)),
        AggroScalarValue::Long(s) => Ok(s.into_py(py)),
        AggroScalarValue::Binary(s) => Ok(s.clone().into_py(py)),
        AggroScalarValue::Uuid(s) => Ok(s.to_string().into_py(py)),
        AggroScalarValue::Datetime(s) => Ok(s.clone().into_py(py)),
        AggroScalarValue::Float(s) => Ok(s.into_py(py)),
        AggroScalarValue::Boolean(s) => Ok(s.into_py(py)),
        AggroScalarValue::Generic(s) => juniper_value_to_python(py, s),
    }
}

pub(crate) fn convert_pyany_to_input(
    attribute_val: &Bound<'_, PyAny>,
) -> PuffResult<InputValue<AggroScalarValue>> {
    if let Ok(s) = attribute_val.extract() {
        return Ok(InputValue::Scalar(AggroScalarValue::String(s)));
    }
    if let Ok(s) = attribute_val.extract() {
        return Ok(InputValue::Scalar(AggroScalarValue::Boolean(s)));
    }
    if let Ok(s) = attribute_val.extract() {
        return Ok(InputValue::Scalar(AggroScalarValue::Int(s)));
    }
    if let Ok(s) = attribute_val.extract() {
        return Ok(InputValue::Scalar(AggroScalarValue::Long(s)));
    }
    if let Ok(s) = attribute_val.extract() {
        return Ok(InputValue::Scalar(AggroScalarValue::Float(s)));
    }
    if let Ok(l) = attribute_val.downcast::<PyList>() {
        let mut vec = Vec::with_capacity(l.len());
        for item in l.iter() {
            vec.push(Spanning::unlocated(convert_pyany_to_input(&item)?));
        }
        return Ok(InputValue::List(vec));
    }
    if let Ok(l) = attribute_val.downcast::<PyDict>() {
        let mut vec = Vec::with_capacity(l.len());
        for (k, s) in l.iter() {
            vec.push((
                Spanning::unlocated(k.to_string()),
                Spanning::unlocated(convert_pyany_to_input(&s)?),
            ));
        }

        return Ok(InputValue::Object(vec));
    }
    bail!("Expected a simple python type, list or dict.")
}
