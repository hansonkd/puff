use crate::context::with_puff_context;
use async_trait::async_trait;

use juniper::meta::{Argument, Field, MetaType};
use juniper::{
    Arguments, BoxFuture, ExecutionResult, Executor, FieldError, FromInputValue,
    GraphQLSubscriptionValue, GraphQLType, GraphQLValue, GraphQLValueAsync, InputValue, Registry,
    Value, ValuesStream,
};
use pyo3::types::PyDict;
use pyo3::{Python, ToPyObject};

use crate::errors::{log_puff_error, PuffResult};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;

use crate::graphql::puff_schema::{
    returned_values_into_stream, selection_to_fields, AggroArgument, AggroContext, AggroField,
    AggroObject, AggroTypeInfo, PyContext, SubscriptionSender,
};
use crate::graphql::row_return::ExtractorRootNode;
use crate::graphql::scalar::{AggroScalarValue, AggroValue, Binary, GenericScalar};

use crate::types::text::ToText;
use crate::types::Text;

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
            AggroTypeInfo::Datetime => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<DateTime<Utc>>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<DateTime<Utc>>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<DateTime<Utc>>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<DateTime<Utc>>>(arg_name, &()),
            },
            AggroTypeInfo::Binary => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<Binary>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<Binary>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<Binary>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<Binary>>(arg_name, &()),
            },
            AggroTypeInfo::Uuid => match (arg.param_type.optional, inner.optional) {
                (true, true) => registry.arg::<Option<Vec<Option<Uuid>>>>(arg_name, &()),
                (true, false) => registry.arg::<Option<Vec<Uuid>>>(arg_name, &()),
                (false, true) => registry.arg::<Vec<Option<Uuid>>>(arg_name, &()),
                (false, false) => registry.arg::<Vec<Uuid>>(arg_name, &()),
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
        AggroTypeInfo::Uuid => {
            if arg.param_type.optional {
                registry.arg::<Option<Uuid>>(arg_name, &())
            } else {
                registry.arg::<Uuid>(arg_name, &())
            }
        }
        AggroTypeInfo::Datetime => {
            if arg.param_type.optional {
                registry.arg::<Option<DateTime<Utc>>>(arg_name, &())
            } else {
                registry.arg::<DateTime<Utc>>(arg_name, &())
            }
        }
        AggroTypeInfo::Binary => {
            if arg.param_type.optional {
                registry.arg::<Option<Binary>>(arg_name, &())
            } else {
                registry.arg::<Binary>(arg_name, &())
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
            AggroTypeInfo::Datetime => match (field.return_type.optional, inner.optional) {
                (true, true) => {
                    registry.field::<Option<Vec<Option<DateTime<Utc>>>>>(field_name, &())
                }
                (true, false) => registry.field::<Option<Vec<DateTime<Utc>>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<DateTime<Utc>>>>(field_name, &()),
                (false, false) => registry.field::<Vec<DateTime<Utc>>>(field_name, &()),
            },
            AggroTypeInfo::Binary => match (field.return_type.optional, inner.optional) {
                (true, true) => registry.field::<Option<Vec<Option<Binary>>>>(field_name, &()),
                (true, false) => registry.field::<Option<Vec<Binary>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<Binary>>>(field_name, &()),
                (false, false) => registry.field::<Vec<Binary>>(field_name, &()),
            },
            AggroTypeInfo::Uuid => match (field.return_type.optional, inner.optional) {
                (true, true) => registry.field::<Option<Vec<Option<Uuid>>>>(field_name, &()),
                (true, false) => registry.field::<Option<Vec<Uuid>>>(field_name, &()),
                (false, true) => registry.field::<Vec<Option<Uuid>>>(field_name, &()),
                (false, false) => registry.field::<Vec<Uuid>>(field_name, &()),
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
        AggroTypeInfo::Uuid => {
            if field.return_type.optional {
                registry.field::<Option<Uuid>>(field_name, &())
            } else {
                registry.field::<Uuid>(field_name, &())
            }
        }
        AggroTypeInfo::Datetime => {
            if field.return_type.optional {
                registry.field::<Option<DateTime<Utc>>>(field_name, &())
            } else {
                registry.field::<DateTime<Utc>>(field_name, &())
            }
        }
        AggroTypeInfo::Binary => {
            if field.return_type.optional {
                registry.field::<Option<Binary>>(field_name, &())
            } else {
                registry.field::<Binary>(field_name, &())
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
        let context = executor.context();

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
                let all_objects = info.all_objs.clone();
                let input_objs = &info.input_objs;
                let look_ahead = Python::with_gil(|py| {
                    selection_to_fields(py, aggro_field, look_ahead_slice, input_objs, &all_objects)
                })?;
                // let field_obj = info.all_objs.get(field.return_type.primary_type.as_str()).unwrap();
                let rows = Arc::new(ExtractorRootNode);
                let res = returned_values_into_stream(
                    rows,
                    &look_ahead,
                    aggro_field,
                    all_objects,
                    context,
                )
                .await?;

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
        let context = executor.context();
        let auth = context.auth();

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
                let (look_ahead_fields, args) = Python::with_gil(|py| {
                    let laf =
                        selection_to_fields(py, field, look_ahead_slice, input_objs, &all_objects)?;
                    let args = laf.arguments().to_object(py);
                    PuffResult::Ok((laf, args))
                })?;
                let parents = Arc::new(ExtractorRootNode);
                let ss = SubscriptionSender::new(
                    sender,
                    look_ahead_fields,
                    field.clone(),
                    all_objects,
                    context.auth(),
                    parents.clone(),
                );

                let py_dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
                let acceptor_method = field
                    .acceptor_method
                    .clone()
                    .expect("Subscription field needs an acceptor");

                let python_context = PyContext::new(parents.clone(), auth, None);
                if field.is_async {
                    py_dispatcher.dispatch_asyncio(
                        acceptor_method,
                        (ss, python_context, args),
                        None,
                    )?;
                } else {
                    py_dispatcher.dispatch::<_, &PyDict>(
                        acceptor_method,
                        (ss, python_context, args),
                        None,
                    )?;
                }

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
    fn from_input_value(_v: &InputValue<AggroScalarValue>) -> Result<Self, String> {
        Ok(PuffGqlObject)
    }
}
