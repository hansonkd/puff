use crate::context::with_puff_context;
use async_trait::async_trait;
use axum::headers::Te;
use juniper::executor::LookAheadMethods;
use juniper::meta::{Argument, Field, MetaType};
use juniper::{
    Arguments, BoxFuture, DefaultScalarValue, ExecutionResult, Executor, FieldError,
    FromInputValue, GraphQLSubscriptionValue, GraphQLType, GraphQLValue, GraphQLValueAsync,
    InputValue, Registry, Value, ValuesStream,
};
use pyo3::types::PyDict;
use pyo3::Python;
use serde_json::{Map, Value as JsonValue};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use crate::errors::log_puff_error;

use crate::graphql::puff_schema::{
    returned_values_into_stream, selection_to_fields, AggroArgument, AggroContext, AggroField,
    AggroObject, AggroTypeInfo, SubscriptionSender,
};
use crate::graphql::row_return::{ExtractorRootNode, RootNode};
use crate::graphql::scalar::{AggroScalarValue, AggroValue, GenericScalar};
use crate::types::text::ToText;
use crate::types::Text;

pub enum SchemaComponent {
    Mutation,
    Query,
    Subscription,
}

#[derive(Debug, Clone)]
pub struct SchemaInfo {
    pub all_objs: Arc<BTreeMap<Text, AggroObject>>,
    pub input_objs: Arc<BTreeMap<Text, AggroObject>>,
    pub name: Text,
    pub commit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PuffGqlObject;

impl PuffGqlObject {
    pub fn new() -> Self {
        Self
    }
}

impl<'s> GraphQLType<AggroScalarValue> for PuffGqlObject {
    fn name(info: &Self::TypeInfo) -> Option<&str> {
        if let Some(obj) = info.all_objs.get(&info.name) {
            if obj.fields.is_empty() {
                let n = match info.name.as_str() {
                    "Mutation" => "_Mutation",
                    "Query" => "_Query",
                    "Subscription" => "_Subscription",
                    s => s,
                };
                Some(n)
            } else {
                Some(info.name.as_str())
            }
        } else {
            Some(info.name.as_str())
        }
    }

    fn meta<'r>(
        info: &Self::TypeInfo,
        registry: &mut Registry<'r, AggroScalarValue>,
    ) -> MetaType<'r, AggroScalarValue>
    where
        AggroScalarValue: 'r,
    {
        if let Some(obj) = info.all_objs.get(&info.name) {
            if obj.fields.is_empty() {
                return registry.build_object_type::<Self>(&info, &[]).into_meta();
            }
            let mut fields = Vec::new();
            for (k, v) in obj.fields.iter() {
                let f = build_field(info, registry, k, v);
                fields.push(f);
            }

            registry
                .build_object_type::<PuffGqlObject>(info, fields.as_slice())
                .into_meta()
        } else if let Some(obj) = info.input_objs.get(&info.name) {
            let mut args = Vec::new();

            for (k, v) in obj.fields.iter() {
                let f = build_argument(info, registry, k, &v.as_argument());
                args.push(f);
            }

            registry
                .build_input_object_type::<PuffGqlObject>(info, args.as_slice())
                .into_meta()
        } else {
            panic!("Did not find object, {}, in all types", info.name.as_str())
        }
    }
}

fn build_argument<'r>(
    info: &SchemaInfo,
    registry: &mut Registry<'r, AggroScalarValue>,
    arg_name: &str,
    arg: &AggroArgument,
) -> Argument<'r, AggroScalarValue> {
    match &arg.param_type.type_info {
        AggroTypeInfo::List(inner) => match &inner.type_info {
            AggroTypeInfo::Any => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<GenericScalar>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<GenericScalar>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<GenericScalar>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<GenericScalar>>(arg_name, &()),
            },
            AggroTypeInfo::String => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<String>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<String>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<String>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<String>>(arg_name, &()),
            },
            AggroTypeInfo::Int => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<i32>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<i32>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<i32>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<i32>>(arg_name, &()),
            },
            AggroTypeInfo::Float => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<f64>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<f64>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<f64>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<f64>>(arg_name, &()),
            },
            AggroTypeInfo::Boolean => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<bool>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<bool>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<bool>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<bool>>(arg_name, &()),
            },
            AggroTypeInfo::List(_) => {
                panic!("Cannot have nested list. Found one in {}", arg_name)
            }
            AggroTypeInfo::Object(tn) => {
                let field_node_type_info = &SchemaInfo {
                    all_objs: info.all_objs.clone(),
                    input_objs: info.input_objs.clone(),
                    name: tn.clone(),
                    commit: info.commit,
                };

                match (arg.param_type.optional, inner.optional) {
                    (true, true) => registry
                        .arg::<Option<Vec<Option<PuffGqlObject>>>>(arg_name, field_node_type_info),
                    (true, false) => {
                        registry.arg::<Option<Vec<PuffGqlObject>>>(arg_name, field_node_type_info)
                    }
                    (false, true) => {
                        registry.arg::<Vec<Option<PuffGqlObject>>>(arg_name, field_node_type_info)
                    }
                    (false, false) => {
                        registry.arg::<Vec<PuffGqlObject>>(arg_name, field_node_type_info)
                    }
                }
            }
        },
        AggroTypeInfo::Any => {
            if arg.param_type.optional {
                registry.arg::<Option<GenericScalar>>(arg_name, &())
            } else {
                registry.arg::<GenericScalar>(arg_name, &())
            }
        }
        AggroTypeInfo::String => {
            if arg.param_type.optional {
                registry.arg::<Option<String>>(arg_name, &())
            } else {
                registry.arg::<String>(arg_name, &())
            }
        }
        AggroTypeInfo::Int => {
            if arg.param_type.optional {
                registry.arg::<Option<i32>>(arg_name, &())
            } else {
                registry.arg::<i32>(arg_name, &())
            }
        }
        AggroTypeInfo::Float => {
            if arg.param_type.optional {
                registry.arg::<Option<f64>>(arg_name, &())
            } else {
                registry.arg::<f64>(arg_name, &())
            }
        }
        AggroTypeInfo::Boolean => {
            if arg.param_type.optional {
                registry.arg::<Option<bool>>(arg_name, &())
            } else {
                registry.arg::<bool>(arg_name, &())
            }
        }
        AggroTypeInfo::Object(tn) => {
            let field_node_type_info = &SchemaInfo {
                all_objs: info.all_objs.clone(),
                input_objs: info.input_objs.clone(),
                name: tn.clone(),
                commit: info.commit,
            };
            if arg.param_type.optional {
                registry.arg::<Option<PuffGqlObject>>(arg_name, field_node_type_info)
            } else {
                registry.arg::<PuffGqlObject>(arg_name, field_node_type_info)
            }
        }
    }
}

fn build_field<'r>(
    info: &SchemaInfo,
    registry: &mut Registry<'r, AggroScalarValue>,
    field_name: &str,
    field: &AggroField,
) -> Field<'r, AggroScalarValue> {
    let juniper_field = match &field.return_type.type_info {
        AggroTypeInfo::List(inner) => match &inner.type_info {
            AggroTypeInfo::List(_) => {
                panic!("Cannot have a nested list but found one for {}", field_name)
            }
            AggroTypeInfo::Any => match (field.return_type.optional, inner.optional) {
                (true, true) => {
                    registry.field::<Option<Vec<Option<GenericScalar>>>>(field_name, &())
                }
                (true, false) => registry.field::<Option<Vec<GenericScalar>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<GenericScalar>>>(field_name, &()),
                (false, false) => registry.field::<Vec<GenericScalar>>(field_name, &()),
            },
            AggroTypeInfo::String => match (field.return_type.optional, inner.optional) {
                (true, true) => registry.field::<Option<Vec<Option<String>>>>(field_name, &()),
                (true, false) => registry.field::<Option<Vec<String>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<String>>>(field_name, &()),
                (false, false) => registry.field::<Vec<String>>(field_name, &()),
            },
            AggroTypeInfo::Int => match (field.return_type.optional, inner.optional) {
                (true, true) => registry.field::<Option<Vec<Option<i32>>>>(field_name, &()),
                (true, false) => registry.field::<Option<Vec<i32>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<i32>>>(field_name, &()),
                (false, false) => registry.field::<Vec<i32>>(field_name, &()),
            },
            AggroTypeInfo::Float => match (field.return_type.optional, inner.optional) {
                (true, true) => registry.field::<Option<Vec<Option<f64>>>>(field_name, &()),
                (true, false) => registry.field::<Option<Vec<f64>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<f64>>>(field_name, &()),
                (false, false) => registry.field::<Vec<f64>>(field_name, &()),
            },
            AggroTypeInfo::Boolean => match (field.return_type.optional, inner.optional) {
                (true, true) => registry.field::<Option<Vec<Option<bool>>>>(field_name, &()),
                (true, false) => registry.field::<Option<Vec<bool>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<bool>>>(field_name, &()),
                (false, false) => registry.field::<Vec<bool>>(field_name, &()),
            },
            AggroTypeInfo::Object(s) => {
                let field_node_type_info = &SchemaInfo {
                    all_objs: info.all_objs.clone(),
                    input_objs: info.input_objs.clone(),
                    name: s.clone(),
                    commit: info.commit,
                };
                match (field.return_type.optional, inner.optional) {
                    (true, true) => registry.field::<Option<Vec<Option<PuffGqlObject>>>>(
                        field_name,
                        field_node_type_info,
                    ),
                    (true, false) => registry
                        .field::<Option<Vec<PuffGqlObject>>>(field_name, field_node_type_info),
                    (false, true) => registry
                        .field::<Vec<Option<PuffGqlObject>>>(field_name, field_node_type_info),
                    (false, false) => {
                        registry.field::<Vec<PuffGqlObject>>(field_name, field_node_type_info)
                    }
                }
            }
        },
        AggroTypeInfo::Any => {
            if field.return_type.optional {
                registry.field::<Option<GenericScalar>>(field_name, &())
            } else {
                registry.field::<GenericScalar>(field_name, &())
            }
        }
        AggroTypeInfo::String => {
            if field.return_type.optional {
                registry.field::<Option<String>>(field_name, &())
            } else {
                registry.field::<String>(field_name, &())
            }
        }
        AggroTypeInfo::Int => {
            if field.return_type.optional {
                registry.field::<Option<i32>>(field_name, &())
            } else {
                registry.field::<i32>(field_name, &())
            }
        }
        AggroTypeInfo::Float => {
            if field.return_type.optional {
                registry.field::<Option<f64>>(field_name, &())
            } else {
                registry.field::<f64>(field_name, &())
            }
        }
        AggroTypeInfo::Boolean => {
            if field.return_type.optional {
                registry.field::<Option<bool>>(field_name, &())
            } else {
                registry.field::<bool>(field_name, &())
            }
        }
        AggroTypeInfo::Object(tn) => {
            let field_node_type_info = &SchemaInfo {
                all_objs: info.all_objs.clone(),
                input_objs: info.input_objs.clone(),
                name: tn.clone(),
                commit: info.commit,
            };
            if field.return_type.optional {
                registry.field::<Option<PuffGqlObject>>(field_name, field_node_type_info)
            } else {
                registry.field::<PuffGqlObject>(field_name, field_node_type_info)
            }
        }
    };

    field
        .arguments
        .iter()
        .fold(juniper_field, |jp, (arg_name, arg)| {
            jp.argument(build_argument(info, registry, arg_name.as_str(), &arg))
        })
}

// impl<S> GraphQLValue<S> for Object
//     where
//         S: ScalarValue
impl GraphQLValue<AggroScalarValue> for PuffGqlObject {
    type Context = AggroContext;
    type TypeInfo = SchemaInfo;

    fn type_name<'i>(&self, info: &'i Self::TypeInfo) -> Option<&'i str> {
        <Self as GraphQLType<AggroScalarValue>>::name(info)
    }
}

#[async_trait]
impl GraphQLValueAsync<AggroScalarValue> for PuffGqlObject {
    fn resolve_field_async<'a>(
        &'a self,
        info: &'a Self::TypeInfo,
        field_name: &'a str,
        _args: &'a Arguments<'a, AggroScalarValue>,
        executor: &'a Executor<'_, '_, Self::Context, AggroScalarValue>,
    ) -> BoxFuture<'a, ExecutionResult<AggroScalarValue>> {
        Box::pin(async move {
            let fut = async move {
                let look_ahead = executor.look_ahead();
                let look_ahead_slice = &look_ahead;

                let this_obj = info
                    .all_objs
                    .get(&info.name)
                    .expect(format!("Could not find object {}", info.name).as_str());
                let aggro_field = this_obj
                    .fields
                    .get(&field_name.to_text())
                    .expect(format!("Could not find field {}", field_name).as_str());
                // let ctx = executor.context();
                let pool = with_puff_context(|ctx| ctx.postgres().pool());
                let all_objects = info.all_objs.clone();
                let input_objs = &info.input_objs;
                let mut client = pool.get().await?;
                let look_ahead = Python::with_gil(|py| {
                    selection_to_fields(py, aggro_field, look_ahead_slice, input_objs, &all_objects)
                })?;
                // let field_obj = info.all_objs.get(field.return_type.primary_type.as_str()).unwrap();
                let txn = client.transaction().await?;
                let rows = Arc::new(ExtractorRootNode);

                let res =
                    returned_values_into_stream(&txn, rows, &look_ahead, aggro_field, all_objects)
                        .await?;

                if info.commit {
                    txn.commit().await?;
                } else {
                    txn.rollback().await?;
                }

                for s in res {
                    return Ok(s);
                }
                return Ok(AggroValue::Null);
            };
            let result = log_puff_error("GQL", fut.await);

            return Ok(result?);
        })
    }
}

impl GraphQLSubscriptionValue<AggroScalarValue> for PuffGqlObject {
    fn resolve_field_into_stream<'s, 'i, 'ft, 'args, 'e, 'ref_e, 'res, 'f>(
        &'s self,
        info: &'i Self::TypeInfo,
        field_name: &'ft str,
        _: Arguments<'args, AggroScalarValue>,
        executor: &'ref_e Executor<'ref_e, 'e, Self::Context, AggroScalarValue>,
    ) -> BoxFuture<
        'f,
        Result<Value<ValuesStream<'res, AggroScalarValue>>, FieldError<AggroScalarValue>>,
    >
    where
        's: 'f,
        'i: 'res,
        'ft: 'f,
        'args: 'f,
        'ref_e: 'f,
        'res: 'f,
        'e: 'res,
    {
        Box::pin(async move {
            let fut = async move {
                let look_ahead = executor.look_ahead();
                let look_ahead_slice = &look_ahead;

                let this_obj = info
                    .all_objs
                    .get(&info.name)
                    .expect(format!("Could not find object {}", info.name).as_str());
                let field = this_obj
                    .fields
                    .get(&field_name.to_text())
                    .expect(format!("Could not find subscription field {}", field_name).as_str());
                // let ctx = executor.context();
                let (sender, ret) = unbounded_channel();
                let all_objects = info.all_objs.clone();
                let input_objs = &info.input_objs;
                let look_ahead = Python::with_gil(|py| {
                    selection_to_fields(py, field, look_ahead_slice, input_objs, &all_objects)
                })?;

                let ss = SubscriptionSender::new(sender, look_ahead, field.clone(), all_objects);

                let py_dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
                let acceptor_method = field
                    .acceptor_method
                    .clone()
                    .expect("Subscription field needs an acceptor");
                py_dispatcher.dispatch::<_, &PyDict>(acceptor_method, (ss, ), None)?;
                let stream: ValuesStream<AggroScalarValue> =
                    Box::pin(UnboundedReceiverStream::new(ret));
                Ok(Value::Scalar(stream))
            };
            let result = log_puff_error("GQL Subscription", fut.await);

            return Ok(result?);
        })
    }
}

impl FromInputValue<AggroScalarValue> for PuffGqlObject {
    type Error = String;
    fn from_input_value(v: &InputValue<AggroScalarValue>) -> Result<Self, String> {
        Ok(PuffGqlObject)
    }
}
