//! Schema builder: builds an async_graphql::dynamic::Schema from AggroObjects.

use crate::errors::PuffResult;
use crate::graphql::puff_schema::*;
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

fn build_object_type(
    obj: &AggroObject,
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    input_objs: &Arc<BTreeMap<Text, AggroObject>>,
    _is_root: bool,
    _commit: bool,
) -> Object {
    let mut gql_obj = Object::new(obj.name.as_str());

    for (field_name, field) in obj.fields.iter() {
        let type_ref = decoded_type_to_type_ref(&field.return_type);
        let field_arc = Arc::new(field.clone());
        let objects = all_objects.clone();
        let inputs = input_objs.clone();

        let mut gql_field = Field::new(field_name.as_str(), type_ref, move |ctx| {
            let field = field_arc.clone();
            let objects = objects.clone();
            let inputs = inputs.clone();
            FieldFuture::new(async move {
                let aggro_ctx = ctx
                    .ctx
                    .data::<Arc<AggroContext>>()
                    .map_err(|e| async_graphql::Error::new(format!("Missing AggroContext: {:?}", e)))?;

                let (look_ahead, layer_cache) = Python::with_gil(|py| {
                    let look_ahead =
                        build_look_ahead_from_ctx(py, &field, ctx.ctx, &inputs, &objects, &ctx.args)?;
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
                    Ok(val) => {
                        if val == GqlValue::Null {
                            Ok(None)
                        } else {
                            Ok(Some(FieldValue::value(val)))
                        }
                    }
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

        let mut sub_field =
            SubscriptionField::new(field_name.as_str(), type_ref, move |ctx| {
                let field = field_arc.clone();
                let objects = objects.clone();
                let inputs = inputs.clone();
                let config = config.clone();
                SubscriptionFieldFuture::new(async move {
                    let aggro_ctx = ctx
                        .ctx
                        .data::<Arc<AggroContext>>()
                        .map_err(|e| {
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

                    let acceptor_method = Python::with_gil(|py| {
                        field.acceptor_method.as_ref().map(|o| o.clone_ref(py))
                    })
                    .ok_or_else(|| {
                        async_graphql::Error::new(format!(
                            "Subscription field '{}' needs an acceptor",
                            field.name
                        ))
                    })?;

                    if field.is_async {
                        py_dispatcher
                            .dispatch_asyncio(
                                acceptor_method,
                                (ss, python_context, py_args),
                                None,
                            )
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
