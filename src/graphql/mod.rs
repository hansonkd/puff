use juniper::{EmptyMutation, EmptySubscription, RootNode};
use pyo3::{PyAny, PyResult, Python};
use std::sync::Arc;

pub mod handlers;
mod puff_schema;
mod row_return;
mod scalar;
mod schema;
pub use puff_schema::AggroContext;

use crate::graphql::scalar::AggroScalarValue;
use crate::graphql::schema::Object;
use crate::types::text::ToText;
use serde_json;

pub fn load_schema(
    module: &str,
) -> PyResult<Arc<RootNode<Object, Object, Object, AggroScalarValue>>> {
    let (converted_objs, input_objs) = Python::with_gil(|py| -> PyResult<_> {
        let puff = py.import("puff")?;
        let puff_gql = py.import("puff.graphql")?;
        let service_description_function = puff.call_method1("import_string", (module,))?;
        let t2d = puff_gql.getattr("type_to_description")?;

        let schema: &PyAny = service_description_function.call0()?;

        let (converted_objs, input_objs) = puff_schema::convert(schema, t2d)?;

        Ok((converted_objs, input_objs))
    })?;

    let data = serde_json::from_str(r#"{}"#).unwrap();

    let info = schema::SchemaInfo {
        name: "Query".to_text(),
        all_objs: Arc::new(converted_objs),
        input_objs: Arc::new(input_objs),
    };
    let mutation_info = schema::SchemaInfo {
        name: "Mutation".to_text(),
        all_objs: info.all_objs.clone(),
        input_objs: info.input_objs.clone(),
    };
    let subscription_info = schema::SchemaInfo {
        name: "Subscription".to_text(),
        all_objs: info.all_objs.clone(),
        input_objs: info.input_objs.clone(),
    };
    let object = schema::Object { fields: data };

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
