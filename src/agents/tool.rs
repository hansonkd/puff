use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use std::collections::HashMap;
use std::time::Duration;
use tokio::process::Command;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum OutputFormat {
    Json,
    Text,
    Csv,
}

#[derive(Debug, Clone)]
pub enum ToolExecutor {
    Python { module: String, function: String },
    Cli { command: String, args_template: Vec<String>, output_format: OutputFormat },
    /// Execute a compiled `.wasm` module via wasmtime (requires `wasm-tools` feature).
    Wasm { module_path: std::path::PathBuf },
    Noop,
}

#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub executor: ToolExecutor,
    pub requires_approval: bool,
    pub timeout_ms: u64,
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. The key is `tool.definition.name`.
    pub fn register(&mut self, tool: RegisteredTool) {
        self.tools.insert(tool.definition.name.clone(), tool);
    }

    /// Look up a registered tool by name.
    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }

    /// Return all tool definitions (for passing to the LLM).
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition.clone()).collect()
    }

    /// Return all registered tool names.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CLI tool execution
// ---------------------------------------------------------------------------

/// Execute a CLI command with the given arguments and an optional timeout.
///
/// `command` may contain whitespace (e.g. `"git status"`); the first token
/// is used as the program, the rest become leading arguments. `args` are
/// appended afterwards.
pub async fn execute_cli_tool(
    command: &str,
    args: &[String],
    timeout_ms: u64,
) -> Result<String, AgentError> {
    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or(command);
    let base_args: Vec<&str> = parts.collect();

    let mut cmd = Command::new(program);
    cmd.args(&base_args);
    cmd.args(args);

    let future = cmd.output();

    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), future)
        .await
        .map_err(|_| AgentError::ToolTimeout {
            tool: command.to_string(),
            timeout_ms,
        })?
        .map_err(|e| AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: format!("failed to spawn process: {e}"),
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: stderr,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool(name: &str) -> RegisteredTool {
        RegisteredTool {
            definition: ToolDefinition {
                name: name.to_string(),
                description: format!("Tool {name}"),
                input_schema: json!({ "type": "object", "properties": {} }),
            },
            executor: ToolExecutor::Noop,
            requires_approval: false,
            timeout_ms: 5000,
        }
    }

    #[test]
    fn test_register_and_lookup_tool() {
        let mut registry = ToolRegistry::new();
        let tool = make_tool("my_tool");
        registry.register(tool);

        let found = registry.get("my_tool");
        assert!(found.is_some(), "registered tool should be findable");
        assert_eq!(found.unwrap().definition.name, "my_tool");

        assert!(registry.get("nonexistent").is_none(), "unknown tool should return None");
    }

    #[test]
    fn test_tool_definitions_for_llm() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("tool_a"));
        registry.register(make_tool("tool_b"));

        let defs = registry.definitions();
        assert_eq!(defs.len(), 2, "should return one definition per registered tool");
    }
}
