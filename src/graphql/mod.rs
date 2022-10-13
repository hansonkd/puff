//! Use Python Dataclasses to Define a GQL Schema
use anyhow::bail;
use juniper::{InputValue, RootNode, Spanning};
use pyo3::{IntoPy, Py, PyAny, PyObject, PyResult, Python, ToPyObject};
use std::sync::Arc;

pub use handlers::{handle_graphql, handle_subscriptions};
pub mod handlers;
mod puff_schema;
mod row_return;
mod scalar;
mod schema;
use crate::errors::PuffResult;
pub use puff_schema::AggroContext;
use pyo3::types::{PyDict, PyList, PyString};

use crate::graphql::scalar::{AggroScalarValue, AggroValue};
use crate::graphql::schema::PuffGqlObject;
use crate::python::PythonDispatcher;
use crate::types::text::ToText;
use crate::types::Text;

pub(crate) type PuffGraphqlRoot =
    Arc<RootNode<'static, PuffGqlObject, PuffGqlObject, PuffGqlObject, AggroScalarValue>>;

pub(crate) async fn load_schema(
    module: Text,
    py_dispatcher: PythonDispatcher,
) -> PyResult<PuffGraphqlRoot> {
    let import_string_fn = Python::with_gil(|py| -> PyResult<_> {
        let puff = py.import("puff")?;
        let import_string_fn = puff.getattr("import_string")?.to_object(py);
        Ok(import_string_fn)
    })?;

    let schema = py_dispatcher
        .dispatch1(import_string_fn, (module,))?
        .await
        .unwrap()?;
    let (converted_objs, input_objs) = Python::with_gil(|py| -> PyResult<_> {
        let puff_gql = py.import("puff.graphql")?;
        let t2d = puff_gql.getattr("type_to_description")?;

        let (converted_objs, input_objs) = puff_schema::convert(schema.as_ref(py), t2d)?;
        Ok((converted_objs, input_objs))
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

    Ok(Arc::new(schema))
}

pub(crate) fn juniper_value_to_python(py: Python, v: &AggroValue) -> PuffResult<Py<PyAny>> {
    match v {
        AggroValue::List(inner) => {
            let mut val_vec: Vec<PyObject> = Vec::with_capacity(inner.len());
            for iv in inner {
                val_vec.push(juniper_value_to_python(py, iv)?);
            }
            Ok(PyList::new(py, val_vec).into())
        }
        AggroValue::Object(inner) => {
            let mut val_vec: Vec<(PyObject, PyObject)> = Vec::with_capacity(inner.field_count());
            for (k, iv) in inner.iter() {
                val_vec.push((
                    PyString::new(py, k).into(),
                    juniper_value_to_python(py, iv)?,
                ));
            }
            Ok(PyDict::from_sequence(py, PyList::new(py, val_vec).into())?.into())
        }
        AggroValue::Scalar(s) => scalar_to_python(py, s),
        AggroValue::Null => Ok(Python::None(py)),
    }
}

fn scalar_to_python(py: Python, v: &AggroScalarValue) -> PuffResult<Py<PyAny>> {
    match v {
        AggroScalarValue::String(s) => Ok(s.into_py(py)),
        AggroScalarValue::Int(s) => Ok(s.into_py(py)),
        AggroScalarValue::Float(s) => Ok(s.into_py(py)),
        AggroScalarValue::Boolean(s) => Ok(s.into_py(py)),
        AggroScalarValue::Generic(s) => juniper_value_to_python(py, s),
    }
}

pub(crate) fn convert_pyany_to_input(
    attribute_val: &PyAny,
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
        return Ok(InputValue::Scalar(AggroScalarValue::Float(s)));
    }
    if let Ok(l) = attribute_val.extract::<&PyList>() {
        let mut vec = Vec::with_capacity(l.len());
        for item in l.into_iter() {
            vec.push(Spanning::unlocated(convert_pyany_to_input(item)?));
        }
        return Ok(InputValue::List(vec));
    }
    if let Ok(l) = attribute_val.extract::<&PyDict>() {
        let mut vec = Vec::with_capacity(l.len());
        for (k, s) in l.into_iter() {
            vec.push((
                Spanning::unlocated(k.to_string()),
                Spanning::unlocated(convert_pyany_to_input(s)?),
            ));
        }

        return Ok(InputValue::Object(vec));
    }
    bail!("Expected a simple python type, list or dict.")
}
