//! Schema builder: builds an async_graphql::dynamic::Schema from AggroObjects.

use anyhow::anyhow;

use crate::errors::PuffResult;
use crate::graphql::puff_schema::*;
use crate::graphql::row_return::convert_pyany_to_value;
use crate::graphql::row_return::ExtractorRootNode;
use crate::graphql::scalar::register_scalars;
use crate::types::text::ToText;
use crate::types::Text;

use async_graphql::dynamic::*;
use async_graphql::Value as GqlValue;

use std::collections::BTreeMap;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::errors::log_puff_error;

/// Extract the parent GqlValue from a resolver context's parent_value.
///
/// async-graphql stores the parent's FieldValue in `ctx.parent_value`.
/// When the parent resolver returned `FieldValue::value(GqlValue::Object({...}))`
/// we retrieve it via `try_to_value()`.  This is the standard representation
/// used by the layer engine for object types.
fn extract_parent_gql_value<'a>(ctx: &'a ResolverContext<'_>) -> Option<&'a GqlValue> {
    ctx.parent_value.try_to_value().ok()
}

fn decoded_type_to_type_ref(dt: &DecodedType) -> TypeRef {
    let inner = type_info_to_type_ref(&dt.type_info);
    if dt.optional {
        inner
    } else {
        TypeRef::NonNull(Box::new(inner))
    }
}

fn type_info_to_type_ref(ti: &AggroTypeInfo) -> TypeRef {
    match ti {
        AggroTypeInfo::String => TypeRef::named(TypeRef::STRING),
        AggroTypeInfo::Int => TypeRef::named(TypeRef::INT),
        AggroTypeInfo::Boolean => TypeRef::named(TypeRef::BOOLEAN),
        AggroTypeInfo::Float => TypeRef::named(TypeRef::FLOAT),
        AggroTypeInfo::Datetime => TypeRef::named("DateTime"),
        AggroTypeInfo::Uuid => TypeRef::named(TypeRef::STRING),
        AggroTypeInfo::Binary => TypeRef::named("Binary"),
        AggroTypeInfo::Any => TypeRef::named("GenericScalar"),
        AggroTypeInfo::List(inner) => {
            let inner_ref = decoded_type_to_type_ref(inner);
            TypeRef::List(Box::new(inner_ref))
        }
        AggroTypeInfo::Object(name) => TypeRef::named(name.as_str()),
    }
}

fn decoded_type_to_type_ref_input(dt: &DecodedType) -> TypeRef {
    let inner = type_info_to_type_ref_input(&dt.type_info);
    if dt.optional {
        inner
    } else {
        TypeRef::NonNull(Box::new(inner))
    }
}

fn type_info_to_type_ref_input(ti: &AggroTypeInfo) -> TypeRef {
    match ti {
        AggroTypeInfo::String => TypeRef::named(TypeRef::STRING),
        AggroTypeInfo::Int => TypeRef::named(TypeRef::INT),
        AggroTypeInfo::Boolean => TypeRef::named(TypeRef::BOOLEAN),
        AggroTypeInfo::Float => TypeRef::named(TypeRef::FLOAT),
        AggroTypeInfo::Datetime => TypeRef::named("DateTime"),
        AggroTypeInfo::Uuid => TypeRef::named(TypeRef::STRING),
        AggroTypeInfo::Binary => TypeRef::named("Binary"),
        AggroTypeInfo::Any => TypeRef::named("GenericScalar"),
        AggroTypeInfo::List(inner) => {
            let inner_ref = decoded_type_to_type_ref_input(inner);
            TypeRef::List(Box::new(inner_ref))
        }
        AggroTypeInfo::Object(name) => TypeRef::named(name.as_str()),
    }
}

fn can_use_root_scalar_fast_path(
    field: &AggroField,
    is_root: bool,
    ctx: &ResolverContext<'_>,
) -> bool {
    is_root
        && !field.is_async
        && field.safe_without_context
        && field.producer_method.is_some()
        && field.value_from_column.is_none()
        && ctx.ctx.field().selection_set().count() == 0
        && supports_direct_value_fast_path(&field.return_type.type_info)
}

fn resolve_root_scalar_fast_path(
    field: &AggroField,
    inputs: &Arc<BTreeMap<Text, AggroObject>>,
    aggro_ctx: &AggroContext,
    args: &ObjectAccessor<'_>,
) -> PuffResult<GqlValue> {
    let auth = aggro_ctx.auth();
    let producer = Python::with_gil(|py| field.producer_method.as_ref().map(|o| o.clone_ref(py)))
        .ok_or_else(|| anyhow!("Field {} is missing a producer", field.name))?;

    Python::with_gil(|py| {
        let py_args = collect_arguments_for_python(py, inputs, field, args)?;
        let arg_dict = args_to_py_dict(py, &py_args)?;
        let py_extractor = PyContext::new(
            Arc::new(ExtractorRootNode),
            auth,
            None,
            PyDict::new(py).unbind(),
            Vec::new(),
        );
        let result = producer.call_bound(py, (py_extractor,), Some(&arg_dict))?;
        convert_pyany_to_value(result.bind(py))
    })
}

/// Convert a resolved GqlValue into a FieldValue suitable for async-graphql.
///
/// Returns `None` for null values (which async-graphql treats as absent/null).
/// For all other values, wraps them with `FieldValue::value()`. async-graphql
/// handles object and list-of-object resolution automatically: when the schema
/// type is an Object, it calls `resolve_container` which invokes child field
/// resolvers with the parent FieldValue. Child resolvers then use
/// `extract_parent_gql_value` to read from the parent object's data.
fn gql_to_field_value(val: GqlValue) -> Option<FieldValue<'static>> {
    if val == GqlValue::Null {
        None
    } else {
        Some(FieldValue::value(val))
    }
}

fn build_object_type(
    obj: &AggroObject,
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    input_objs: &Arc<BTreeMap<Text, AggroObject>>,
    is_root: bool,
    _commit: bool,
) -> Object {
    let mut gql_obj = Object::new(obj.name.as_str());

    for (field_name, field) in obj.fields.iter() {
        let type_ref = decoded_type_to_type_ref(&field.return_type);
        let field_arc = Arc::new(field.clone());
        let objects = all_objects.clone();
        let inputs = input_objs.clone();
        let field_name_owned = field_name.clone();

        let mut gql_field = Field::new(field_name.as_str(), type_ref, move |ctx| {
            let field = field_arc.clone();
            let objects = objects.clone();
            let inputs = inputs.clone();
            let field_name_owned = field_name_owned.clone();
            FieldFuture::new(async move {
                let aggro_ctx = ctx.ctx.data::<Arc<AggroContext>>().map_err(|e| {
                    async_graphql::Error::new(format!("Missing AggroContext: {:?}", e))
                })?;

                // ---------------------------------------------------------
                // CHILD FIELD path: extract value from parent object data
                // ---------------------------------------------------------
                if !is_root {
                    // The parent resolver returned FieldValue::value(GqlValue::Object({...})).
                    // async-graphql passes it as parent_value. Extract the underlying
                    // GqlValue::Object and read this field's column from it.
                    if let Some(GqlValue::Object(parent_obj)) = extract_parent_gql_value(&ctx) {
                        let col = field
                            .value_from_column
                            .as_ref()
                            .unwrap_or(&field_name_owned);

                        let val = parent_obj
                            .get(col.as_str())
                            .cloned()
                            .unwrap_or(GqlValue::Null);

                        return Ok(gql_to_field_value(val));
                    }
                    // If we could not extract from parent, fall through to
                    // layer engine (should not normally happen for well-formed
                    // schemas, but keeps backward compat).
                }

                if can_use_root_scalar_fast_path(&field, is_root, &ctx) {
                    let result = log_puff_error(
                        "GQL",
                        resolve_root_scalar_fast_path(&field, &inputs, aggro_ctx, &ctx.args),
                    );
                    return match result {
                        Ok(val) => Ok(gql_to_field_value(val)),
                        Err(e) => Err(async_graphql::Error::new(format!("{}", e))),
                    };
                }

                let (look_ahead, layer_cache) = Python::with_gil(|py| {
                    let look_ahead = build_look_ahead_from_ctx(
                        py, &field, ctx.ctx, &inputs, &objects, &ctx.args,
                    )?;
                    let d = PyDict::new(py).unbind();
                    PuffResult::Ok((look_ahead, d))
                })
                .map_err(|e| async_graphql::Error::new(format!("LookAhead error: {}", e)))?;

                let rows = Arc::new(ExtractorRootNode);
                let fut = async {
                    let res = returned_values_into_stream(
                        rows,
                        &look_ahead,
                        &field,
                        objects,
                        aggro_ctx,
                        layer_cache,
                    )
                    .await?;

                    if let Some(value) = res.into_iter().next() {
                        Ok(value)
                    } else {
                        Ok(GqlValue::Null)
                    }
                };

                let result = log_puff_error("GQL", fut.await);
                match result {
                    Ok(val) => Ok(gql_to_field_value(val)),
                    Err(e) => Err(async_graphql::Error::new(format!("{}", e))),
                }
            })
        });

        for (arg_name, arg) in &field.arguments {
            let arg_type = decoded_type_to_type_ref_input(&arg.param_type);
            gql_field = gql_field.argument(InputValue::new(arg_name.as_str(), arg_type));
        }

        gql_obj = gql_obj.field(gql_field);
    }

    gql_obj
}

fn build_input_object_type(obj: &AggroObject) -> InputObject {
    let mut input_obj = InputObject::new(obj.name.as_str());

    for (field_name, field) in obj.fields.iter() {
        let type_ref = decoded_type_to_type_ref_input(&field.return_type);
        input_obj = input_obj.field(InputValue::new(field_name.as_str(), type_ref));
    }

    input_obj
}

fn build_subscription_type(
    obj: &AggroObject,
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    input_objs: &Arc<BTreeMap<Text, AggroObject>>,
    config_for_sub: &crate::graphql::PuffGraphqlConfig,
) -> Subscription {
    let mut sub = Subscription::new("Subscription");

    for (field_name, field) in obj.fields.iter() {
        let type_ref = decoded_type_to_type_ref(&field.return_type);
        let field_arc = Arc::new(field.clone());
        let objects = all_objects.clone();
        let inputs = input_objs.clone();
        let config = config_for_sub.clone();

        let mut sub_field = SubscriptionField::new(field_name.as_str(), type_ref, move |ctx| {
            let field = field_arc.clone();
            let objects = objects.clone();
            let inputs = inputs.clone();
            let config = config.clone();
            SubscriptionFieldFuture::new(async move {
                let aggro_ctx = ctx.ctx.data::<Arc<AggroContext>>().map_err(|e| {
                    async_graphql::Error::new(format!("Missing AggroContext: {:?}", e))
                })?;

                let (look_ahead_fields, py_args, layer_cache) = Python::with_gil(|py| {
                    let laf = build_look_ahead_from_ctx(
                        py, &field, ctx.ctx, &inputs, &objects, &ctx.args,
                    )?;
                    let args_dict = PyDict::new(py);
                    for (k, v) in laf.arguments().iter() {
                        args_dict.set_item(k.as_str(), v)?;
                    }
                    let args: PyObject = args_dict.into_py(py);
                    PuffResult::Ok((laf, args, PyDict::new(py).unbind()))
                })
                .map_err(|e| async_graphql::Error::new(format!("{}", e)))?;

                let parents = Arc::new(ExtractorRootNode);

                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let ss = SubscriptionSender::new(
                    sender,
                    look_ahead_fields,
                    (*field).clone(),
                    objects,
                    aggro_ctx.auth(),
                    parents.clone(),
                    config,
                );

                let py_dispatcher =
                    crate::context::with_puff_context(|ctx| ctx.python_dispatcher());

                let python_context = PyContext::new(
                    parents,
                    aggro_ctx.auth(),
                    None,
                    layer_cache,
                    field.depends_on.clone(),
                );

                let acceptor_method =
                    Python::with_gil(|py| field.acceptor_method.as_ref().map(|o| o.clone_ref(py)))
                        .ok_or_else(|| {
                            async_graphql::Error::new(format!(
                                "Subscription field '{}' needs an acceptor",
                                field.name
                            ))
                        })?;

                if field.is_async {
                    py_dispatcher
                        .dispatch_asyncio(acceptor_method, (ss, python_context, py_args), None)
                        .map_err(|e| async_graphql::Error::new(format!("{}", e)))?;
                } else {
                    py_dispatcher
                        .dispatch(acceptor_method, (ss, python_context, py_args), None)
                        .map_err(|e| async_graphql::Error::new(format!("{}", e)))?;
                }

                let stream = async_stream::stream! {
                    while let Some(val) = receiver.recv().await {
                        yield Ok(FieldValue::value(val));
                    }
                };

                Ok(stream)
            })
        });

        for (arg_name, arg) in &field.arguments {
            let arg_type = decoded_type_to_type_ref_input(&arg.param_type);
            sub_field = sub_field.argument(InputValue::new(arg_name.as_str(), arg_type));
        }

        sub = sub.field(sub_field);
    }

    sub
}

/// Build the full dynamic schema from the parsed Python schema.
pub fn build_schema(
    all_objs: &Arc<BTreeMap<Text, AggroObject>>,
    input_objs: &Arc<BTreeMap<Text, AggroObject>>,
    config: &crate::graphql::PuffGraphqlConfig,
) -> PuffResult<async_graphql::dynamic::Schema> {
    let has_query = all_objs
        .get(&"Query".to_text())
        .is_some_and(|o| !o.fields.is_empty());
    let has_mutation = all_objs
        .get(&"Mutation".to_text())
        .is_some_and(|o| !o.fields.is_empty());
    let has_subscription = all_objs
        .get(&"Subscription".to_text())
        .is_some_and(|o| !o.fields.is_empty());

    let mutation_name = if has_mutation { Some("Mutation") } else { None };
    let subscription_name = if has_subscription {
        Some("Subscription")
    } else {
        None
    };

    let query_type_name = if has_query { "Query" } else { "_Query" };

    let mut builder = Schema::build(query_type_name, mutation_name, subscription_name);

    builder = register_scalars(builder);

    for (name, obj) in all_objs.iter() {
        match name.as_str() {
            "Query" => {
                if has_query {
                    let gql_obj = build_object_type(obj, all_objs, input_objs, true, false);
                    builder = builder.register(gql_obj);
                }
            }
            "Mutation" => {
                if has_mutation {
                    let gql_obj = build_object_type(obj, all_objs, input_objs, true, true);
                    builder = builder.register(gql_obj);
                }
            }
            "Subscription" => {
                if has_subscription {
                    let sub = build_subscription_type(obj, all_objs, input_objs, config);
                    builder = builder.register(sub);
                }
            }
            _ => {
                let gql_obj = build_object_type(obj, all_objs, input_objs, false, false);
                builder = builder.register(gql_obj);
            }
        }
    }

    for (_name, obj) in input_objs.iter() {
        let input_obj = build_input_object_type(obj);
        builder = builder.register(input_obj);
    }

    if !has_query {
        let empty_query = Object::new("_Query").field(Field::new(
            "_empty",
            TypeRef::named(TypeRef::BOOLEAN),
            |_| FieldFuture::new(async { Ok(FieldValue::NONE) }),
        ));
        builder = builder.register(empty_query);
    }

    let schema = builder
        .finish()
        .map_err(|e| anyhow::anyhow!("Schema build error: {}", e))?;
    Ok(schema)
}
