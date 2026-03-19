//! Built-in GraphQL query tool for agents.
//!
//! Lets agents query the application's GraphQL schema directly
//! with zero HTTP overhead — a direct call to the juniper engine.

use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use crate::agents::tool::{RegisteredTool, ToolExecutor};
use crate::context::with_puff_context;
use crate::graphql::scalar::AggroScalarValue;
use crate::graphql::PuffGraphqlConfig;
use base64::Engine;
use juniper::{execute, Spanning, InputValue, Value};
use std::collections::HashMap;

/// Create a `RegisteredTool` for GraphQL querying.
///
/// The tool accepts a `query` string and optional `variables` JSON object.
/// If `schema_name` is `Some`, the tool name is `graphql_query_{name}`;
/// otherwise it is simply `graphql_query`.
pub fn graphql_query_tool(schema_name: Option<&str>) -> RegisteredTool {
    let name = match schema_name {
        Some(n) => format!("graphql_query_{}", n),
        None => "graphql_query".to_string(),
    };

    RegisteredTool {
        definition: ToolDefinition {
            name,
            description: "Query the application's GraphQL API. Returns JSON results.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The GraphQL query string"
                    },
                    "variables": {
                        "type": "object",
                        "description": "Optional variables for the query"
                    }
                },
                "required": ["query"]
            }),
        },
        executor: ToolExecutor::GraphQL {
            schema_name: schema_name.map(|s| s.to_string()),
        },
        requires_approval: false,
        timeout_ms: 30000,
    }
}

/// Execute a GraphQL query against Puff's built-in engine.
///
/// Returns the result as a JSON string containing `data` and optional
/// `errors` fields, mirroring the standard GraphQL response format.
pub async fn execute_graphql_query(
    query: &str,
    variables: Option<&serde_json::Value>,
    schema_name: Option<&str>,
) -> Result<String, AgentError> {
    // Get the GraphQL config from PuffContext.
    let gql_config: PuffGraphqlConfig = with_puff_context(|ctx| match schema_name {
        Some(name) => ctx.gql_named(name),
        None => ctx.gql(),
    });

    let root = gql_config.root();
    let context = gql_config.new_context(None); // No auth for agent queries

    // Convert variables from serde_json::Value to juniper InputValue.
    let juniper_vars: HashMap<String, InputValue<AggroScalarValue>> = match variables {
        Some(serde_json::Value::Object(map)) => map
            .iter()
            .map(|(k, v)| (k.clone(), json_to_input_value(v)))
            .collect(),
        _ => HashMap::new(),
    };

    // Execute the query.
    let (value, errors) = execute(query, None, &root, &juniper_vars, &context)
        .await
        .map_err(|e| AgentError::ToolExecutionError {
            tool: "graphql_query".into(),
            message: format!("GraphQL execution error: {:?}", e),
        })?;

    // Convert result to JSON.
    let mut result = serde_json::Map::new();
    result.insert("data".to_string(), juniper_value_to_json(&value));

    if !errors.is_empty() {
        let error_list: Vec<serde_json::Value> = errors
            .iter()
            .map(|e| {
                serde_json::json!({
                    "message": format!("{:?}", e.error()),
                    "path": e.path(),
                })
            })
            .collect();
        result.insert("errors".to_string(), serde_json::Value::Array(error_list));
    }

    serde_json::to_string(&result).map_err(|e| AgentError::ToolExecutionError {
        tool: "graphql_query".into(),
        message: format!("Failed to serialize result: {}", e),
    })
}

/// Convert `serde_json::Value` to `juniper::InputValue<AggroScalarValue>`.
fn json_to_input_value(v: &serde_json::Value) -> InputValue<AggroScalarValue> {
    match v {
        serde_json::Value::Null => InputValue::null(),
        serde_json::Value::Bool(b) => InputValue::Scalar(AggroScalarValue::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i64::from(i32::MIN) && i <= i64::from(i32::MAX) {
                    InputValue::Scalar(AggroScalarValue::Int(i as i32))
                } else {
                    InputValue::Scalar(AggroScalarValue::Long(i))
                }
            } else if let Some(f) = n.as_f64() {
                InputValue::Scalar(AggroScalarValue::Float(f))
            } else {
                InputValue::null()
            }
        }
        serde_json::Value::String(s) => {
            InputValue::Scalar(AggroScalarValue::String(s.as_str().into()))
        }
        serde_json::Value::Array(arr) => InputValue::List(
            arr.iter()
                .map(|item| Spanning::unlocated(json_to_input_value(item)))
                .collect(),
        ),
        serde_json::Value::Object(map) => InputValue::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        Spanning::unlocated(k.clone()),
                        Spanning::unlocated(json_to_input_value(v)),
                    )
                })
                .collect(),
        ),
    }
}

/// Convert `juniper::Value<AggroScalarValue>` to `serde_json::Value`.
fn juniper_value_to_json(v: &Value<AggroScalarValue>) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Scalar(s) => scalar_to_json(s),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(juniper_value_to_json).collect())
        }
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj.iter() {
                map.insert(k.to_string(), juniper_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

/// Convert a single `AggroScalarValue` to `serde_json::Value`.
fn scalar_to_json(s: &AggroScalarValue) -> serde_json::Value {
    match s {
        AggroScalarValue::Int(i) => serde_json::json!(i),
        AggroScalarValue::Long(i) => serde_json::json!(i),
        AggroScalarValue::Float(f) => serde_json::json!(f),
        AggroScalarValue::String(s) => serde_json::json!(s.as_str()),
        AggroScalarValue::Boolean(b) => serde_json::json!(b),
        AggroScalarValue::Binary(b) => {
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(b.as_slice()))
        }
        AggroScalarValue::Datetime(d) => serde_json::json!(d.to_string()),
        AggroScalarValue::Uuid(u) => serde_json::json!(u.to_string()),
        AggroScalarValue::Generic(inner) => juniper_value_to_json(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graphql_query_tool_default_name() {
        let tool = graphql_query_tool(None);
        assert_eq!(tool.definition.name, "graphql_query");
        assert!(!tool.requires_approval);
        assert_eq!(tool.timeout_ms, 30000);
    }

    #[test]
    fn test_graphql_query_tool_named() {
        let tool = graphql_query_tool(Some("analytics"));
        assert_eq!(tool.definition.name, "graphql_query_analytics");
    }

    #[test]
    fn test_json_to_input_value_null() {
        let iv = json_to_input_value(&serde_json::Value::Null);
        assert!(matches!(iv, InputValue::Null));
    }

    #[test]
    fn test_json_to_input_value_bool() {
        let iv = json_to_input_value(&serde_json::json!(true));
        match iv {
            InputValue::Scalar(AggroScalarValue::Boolean(b)) => assert!(b),
            other => panic!("expected Boolean, got {:?}", other),
        }
    }

    #[test]
    fn test_json_to_input_value_int() {
        let iv = json_to_input_value(&serde_json::json!(42));
        match iv {
            InputValue::Scalar(AggroScalarValue::Int(i)) => assert_eq!(i, 42),
            other => panic!("expected Int, got {:?}", other),
        }
    }

    #[test]
    fn test_json_to_input_value_float() {
        let iv = json_to_input_value(&serde_json::json!(3.14));
        match iv {
            InputValue::Scalar(AggroScalarValue::Float(f)) => {
                assert!((f - 3.14).abs() < f64::EPSILON)
            }
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn test_json_to_input_value_string() {
        let iv = json_to_input_value(&serde_json::json!("hello"));
        match iv {
            InputValue::Scalar(AggroScalarValue::String(s)) => assert_eq!(s.as_str(), "hello"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_json_to_input_value_array() {
        let iv = json_to_input_value(&serde_json::json!([1, "two"]));
        match iv {
            InputValue::List(items) => assert_eq!(items.len(), 2),
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn test_json_to_input_value_object() {
        let iv = json_to_input_value(&serde_json::json!({"key": "val"}));
        match iv {
            InputValue::Object(fields) => assert_eq!(fields.len(), 1),
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn test_juniper_value_to_json_null() {
        let result = juniper_value_to_json(&Value::Null);
        assert_eq!(result, serde_json::Value::Null);
    }

    #[test]
    fn test_juniper_value_to_json_scalar_int() {
        let result = juniper_value_to_json(&Value::Scalar(AggroScalarValue::Int(7)));
        assert_eq!(result, serde_json::json!(7));
    }

    #[test]
    fn test_juniper_value_to_json_scalar_string() {
        let result =
            juniper_value_to_json(&Value::Scalar(AggroScalarValue::String("abc".into())));
        assert_eq!(result, serde_json::json!("abc"));
    }

    #[test]
    fn test_juniper_value_to_json_list() {
        let list = Value::List(vec![
            Value::Scalar(AggroScalarValue::Int(1)),
            Value::Scalar(AggroScalarValue::Int(2)),
        ]);
        let result = juniper_value_to_json(&list);
        assert_eq!(result, serde_json::json!([1, 2]));
    }
}
