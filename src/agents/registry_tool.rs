use crate::agents::error::AgentError;
use crate::agents::tool::{RegisteredTool, ToolExecutor};
use crate::context::with_puff_context;

pub fn registry_message_tool() -> RegisteredTool {
    RegisteredTool {
        definition: crate::agents::llm::ToolDefinition {
            name: "message_agent".to_string(),
            description: "Send a message to another registered agent or agent instance".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Agent name like 'planner' or registry key like 'agent/planner' or 'instance/<node_id>'"
                    },
                    "message": {
                        "type": "string",
                        "description": "Message to send to the target agent"
                    }
                },
                "required": ["target", "message"],
                "additionalProperties": false
            }),
        },
        executor: ToolExecutor::RegistryMessage,
        requires_approval: false,
        timeout_ms: 60_000,
    }
}

pub fn registry_scan_tool() -> RegisteredTool {
    RegisteredTool {
        definition: crate::agents::llm::ToolDefinition {
            name: "find_agents".to_string(),
            description: "List registered agents by registry prefix or agent-name prefix"
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "prefix": {
                        "type": "string",
                        "description": "Registry key prefix like 'agent/' or agent-name prefix like 'plan'"
                    }
                },
                "additionalProperties": false
            }),
        },
        executor: ToolExecutor::RegistryScan,
        requires_approval: false,
        timeout_ms: 5_000,
    }
}

pub async fn execute_registry_message_tool(
    target: &str,
    message: &str,
) -> Result<String, AgentError> {
    if target.trim().is_empty() {
        return Err(AgentError::ToolExecutionError {
            tool: "message_agent".to_string(),
            message: "target is required".to_string(),
        });
    }
    if message.trim().is_empty() {
        return Err(AgentError::ToolExecutionError {
            tool: "message_agent".to_string(),
            message: "message is required".to_string(),
        });
    }

    let registry = with_puff_context(|ctx| ctx.registry());
    let response = registry
        .send_message(target, message)
        .await
        .map_err(|error| AgentError::ToolExecutionError {
            tool: "message_agent".to_string(),
            message: error.to_string(),
        })?;

    serde_json::to_string(&response).map_err(|error| AgentError::ToolExecutionError {
        tool: "message_agent".to_string(),
        message: format!("failed to serialize registry response: {error}"),
    })
}

pub async fn execute_registry_scan_tool(prefix: Option<&str>) -> Result<String, AgentError> {
    let registry = with_puff_context(|ctx| ctx.registry());
    let prefix = prefix.unwrap_or("agent/");
    let entries = if prefix.contains('/') {
        registry.scan_prefix_async(prefix).await
    } else {
        registry
            .list_entries()
            .await
            .into_iter()
            .filter(|entry| entry.agent_name.starts_with(prefix))
            .collect()
    };

    serde_json::to_string(&entries).map_err(|error| AgentError::ToolExecutionError {
        tool: "find_agents".to_string(),
        message: format!("failed to serialize registry scan: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_tools_have_stable_names() {
        assert_eq!(registry_message_tool().definition.name, "message_agent");
        assert_eq!(registry_scan_tool().definition.name, "find_agents");
    }
}
