//! Built-in GraphQL query tool for agents.
//!
//! Lets agents query the application's GraphQL schema directly
//! with zero HTTP overhead -- a direct call to the async-graphql engine.

use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use crate::agents::tool::{RegisteredTool, ToolExecutor};
use crate::context::with_puff_context;
use crate::graphql::PuffGraphqlConfig;
use std::sync::Arc;

/// Create a `RegisteredTool` for GraphQL querying.
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
}
