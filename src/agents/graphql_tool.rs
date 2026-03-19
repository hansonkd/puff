//! Built-in GraphQL query tool for agents.
//!
//! Lets agents query the application's GraphQL schema directly
//! with zero HTTP overhead -- a direct call to the async-graphql engine.
//!
//! Also provides schema introspection and query validation tools
//! for agent-friendly GraphQL interactions.

use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use crate::agents::tool::{RegisteredTool, ToolExecutor};
use crate::context::with_puff_context;
use crate::graphql::PuffGraphqlConfig;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Feature 2: Helper to fetch the schema SDL (best-effort, never panics)
// ---------------------------------------------------------------------------

/// Try to fetch the SDL for a given schema name.  Returns `None` when the
/// Puff context is not yet available (e.g. tool created at config time).
fn try_get_schema_sdl(schema_name: Option<&str>) -> Option<String> {
    let schema_name_owned = schema_name.map(|s| s.to_string());
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        with_puff_context(|ctx| {
            let config = match schema_name_owned.as_deref() {
                Some(name) => ctx.gql_named(name),
                None => ctx.gql(),
            };
            config.schema().sdl()
        })
    }))
    .ok()
}

// ---------------------------------------------------------------------------
// GraphQL query tool  (Feature 2: dynamic description with SDL)
// ---------------------------------------------------------------------------

/// Create a `RegisteredTool` for GraphQL querying.
///
/// When the Puff context is available the tool description embeds the
/// schema SDL so the LLM already knows what fields exist.
pub fn graphql_query_tool(schema_name: Option<&str>) -> RegisteredTool {
    let name = match schema_name {
        Some(n) => format!("graphql_query_{}", n),
        None => "graphql_query".to_string(),
    };

    // Feature 2: embed the SDL in the description when available
    let description = match try_get_schema_sdl(schema_name) {
        Some(sdl) => {
            let schema_desc = if sdl.len() > 2000 {
                format!("{}...\n(schema truncated)", &sdl[..2000])
            } else {
                sdl
            };
            format!(
                "Query the application's GraphQL API. Returns JSON with 'data' and optional 'errors'.\n\nSchema:\n{}",
                schema_desc
            )
        }
        None => "Query the application's GraphQL API. Returns JSON results.".to_string(),
    };

    RegisteredTool {
        definition: ToolDefinition {
            name,
            description,
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

// ---------------------------------------------------------------------------
// Feature 1: Schema introspection tool
// ---------------------------------------------------------------------------

/// Create a tool that returns a concise schema summary (SDL format).
pub fn graphql_schema_tool(schema_name: Option<&str>) -> RegisteredTool {
    let name = match schema_name {
        Some(n) => format!("graphql_schema_{}", n),
        None => "graphql_schema".to_string(),
    };
    RegisteredTool {
        definition: ToolDefinition {
            name,
            description:
                "Get a concise summary of the GraphQL schema -- types, fields, and arguments."
                    .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        executor: ToolExecutor::GraphQLSchema {
            schema_name: schema_name.map(|s| s.to_string()),
        },
        requires_approval: false,
        timeout_ms: 5000,
    }
}

/// Generate a concise schema summary string (SDL format).
pub fn generate_schema_summary(schema_name: Option<&str>) -> Result<String, AgentError> {
    let sdl = with_puff_context(|ctx| {
        let config = match schema_name {
            Some(name) => ctx.gql_named(name),
            None => ctx.gql(),
        };
        config.schema().sdl()
    });
    Ok(sdl)
}

// ---------------------------------------------------------------------------
// Feature 4: Query validation tool
// ---------------------------------------------------------------------------

/// Create a tool that validates a GraphQL query against the schema without
/// executing it.
pub fn graphql_validate_tool(schema_name: Option<&str>) -> RegisteredTool {
    let name = match schema_name {
        Some(n) => format!("graphql_validate_{}", n),
        None => "graphql_validate".to_string(),
    };
    RegisteredTool {
        definition: ToolDefinition {
            name,
            description:
                "Validate a GraphQL query for syntax errors without executing it.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The GraphQL query string to validate"
                    }
                },
                "required": ["query"]
            }),
        },
        executor: ToolExecutor::GraphQLValidate {
            schema_name: schema_name.map(|s| s.to_string()),
        },
        requires_approval: false,
        timeout_ms: 5000,
    }
}

/// Validate a GraphQL query string for syntax errors.
///
/// Uses `async_graphql::parser::parse_query` to parse the document without
/// executing it.  This catches syntax errors cheaply -- no database access
/// or resolver execution is involved.
pub fn validate_graphql_query(
    query: &str,
    _schema_name: Option<&str>,
) -> Result<String, AgentError> {
    match async_graphql::parser::parse_query(query) {
        Ok(_doc) => Ok("Query parsed successfully".to_string()),
        Err(e) => Ok(format!("Parse error: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// GraphQL query execution (unchanged)
// ---------------------------------------------------------------------------

/// Execute a GraphQL query against Puff's built-in engine.
pub async fn execute_graphql_query(
    query: &str,
    variables: Option<&serde_json::Value>,
    schema_name: Option<&str>,
) -> Result<String, AgentError> {
    let gql_config: PuffGraphqlConfig = with_puff_context(|ctx| match schema_name {
        Some(name) => ctx.gql_named(name),
        None => ctx.gql(),
    });

    let schema = gql_config.schema();
    let context = gql_config.new_context(None);
    let ctx_arc = Arc::new(context);

    let mut request = async_graphql::Request::new(query);

    if let Some(serde_json::Value::Object(map)) = variables {
        let vars =
            async_graphql::Variables::from_json(serde_json::Value::Object(map.clone()));
        request = request.variables(vars);
    }

    let request = request.data(ctx_arc);
    let response = schema.execute(request).await;

    let mut result = serde_json::Map::new();
    let data_json = serde_json::to_value(&response.data).unwrap_or(serde_json::Value::Null);
    result.insert("data".to_string(), data_json);

    if !response.errors.is_empty() {
        let error_list: Vec<serde_json::Value> = response
            .errors
            .iter()
            .map(|e| {
                serde_json::json!({
                    "message": e.message,
                    "path": e.path.iter().map(|p| format!("{:?}", p)).collect::<Vec<_>>(),
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

    // Feature 1: schema tool tests
    #[test]
    fn test_graphql_schema_tool_default_name() {
        let tool = graphql_schema_tool(None);
        assert_eq!(tool.definition.name, "graphql_schema");
        assert!(!tool.requires_approval);
        assert_eq!(tool.timeout_ms, 5000);
    }

    #[test]
    fn test_graphql_schema_tool_named() {
        let tool = graphql_schema_tool(Some("analytics"));
        assert_eq!(tool.definition.name, "graphql_schema_analytics");
    }

    // Feature 4: validate tool tests
    #[test]
    fn test_graphql_validate_tool_default_name() {
        let tool = graphql_validate_tool(None);
        assert_eq!(tool.definition.name, "graphql_validate");
        assert!(!tool.requires_approval);
        assert_eq!(tool.timeout_ms, 5000);
    }

    #[test]
    fn test_graphql_validate_tool_named() {
        let tool = graphql_validate_tool(Some("analytics"));
        assert_eq!(tool.definition.name, "graphql_validate_analytics");
    }

    #[test]
    fn test_validate_valid_query() {
        let result = validate_graphql_query("{ users { id name } }", None).unwrap();
        assert_eq!(result, "Query parsed successfully");
    }

    #[test]
    fn test_validate_invalid_query() {
        let result = validate_graphql_query("{ users { id name }", None).unwrap();
        assert!(result.starts_with("Parse error:"), "got: {}", result);
    }

    #[test]
    fn test_validate_mutation_syntax() {
        let result = validate_graphql_query(
            r#"mutation { createUser(name: "Alice", email: "a@b.com") { id } }"#,
            None,
        )
        .unwrap();
        assert_eq!(result, "Query parsed successfully");
    }

    // Feature 2: dynamic description falls back gracefully without context
    #[test]
    fn test_graphql_query_tool_fallback_description() {
        // Without a Puff context, the tool should still create successfully
        // with a generic description.
        let tool = graphql_query_tool(None);
        assert!(
            tool.definition.description.contains("GraphQL API"),
            "description should mention GraphQL API"
        );
    }
}
