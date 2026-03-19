//! Tool registry and CLI tool execution.

use crate::agents::capabilities::AgentCapabilities;
use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use std::collections::HashMap;
use std::collections::HashSet;
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
    Python {
        module: String,
        function: String,
    },
    Cli {
        command: String,
        args_template: Vec<String>,
        output_format: OutputFormat,
    },
    /// Execute a compiled `.wasm` module via wasmtime (requires `wasm-tools` feature).
    Wasm {
        module_path: std::path::PathBuf,
    },
    /// Execute a GraphQL query against a Puff schema.
    GraphQL {
        schema_name: Option<String>,
    },
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

#[derive(Clone)]
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
    let (program, mut command_args) = split_command(command)?;
    command_args.extend(args.iter().cloned());
    execute_cli_argv(&program, &command_args, timeout_ms).await
}

pub async fn execute_cli_argv(
    program: &str,
    args: &[String],
    timeout_ms: u64,
) -> Result<String, AgentError> {
    let mut cmd = Command::new(program);
    cmd.args(args);

    let future = cmd.output();

    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), future)
        .await
        .map_err(|_| AgentError::ToolTimeout {
            tool: program.to_string(),
            timeout_ms,
        })?
        .map_err(|e| AgentError::ToolExecutionError {
            tool: program.to_string(),
            message: format!("failed to spawn process: {e}"),
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(AgentError::ToolExecutionError {
            tool: program.to_string(),
            message: stderr,
        })
    }
}

pub async fn execute_registered_tool(
    tool: &RegisteredTool,
    arguments: &serde_json::Value,
    capabilities: &AgentCapabilities,
) -> Result<String, AgentError> {
    match &tool.executor {
        ToolExecutor::Python { module, function } => Err(AgentError::ToolExecutionError {
            tool: tool.definition.name.clone(),
            message: format!(
                "Python tool execution is not implemented for '{}.{}'",
                module, function
            ),
        }),
        ToolExecutor::Cli {
            command,
            args_template,
            ..
        } => {
            let (program, args) =
                render_cli_command(command, args_template, arguments, &tool.definition.name)?;
            crate::agents::sandbox::execute_sandboxed_argv(
                &program,
                &args,
                capabilities,
                tool.timeout_ms,
                &crate::agents::sandbox::SandboxConfig::default(),
            )
            .await
        }
        ToolExecutor::Wasm { module_path } => {
            let input_json =
                serde_json::to_string(arguments).map_err(|e| AgentError::ToolExecutionError {
                    tool: tool.definition.name.clone(),
                    message: format!("failed to serialize tool input: {e}"),
                })?;
            crate::agents::wasm::execute_wasm_tool(module_path, &input_json, tool.timeout_ms)
        }
        ToolExecutor::GraphQL { schema_name } => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let variables = arguments.get("variables");
            crate::agents::graphql_tool::execute_graphql_query(
                query,
                variables,
                schema_name.as_deref(),
            )
            .await
        }
        ToolExecutor::Noop => Ok(format!("Tool '{}' completed (no-op)", tool.definition.name)),
    }
}

pub(crate) fn split_command(command: &str) -> Result<(String, Vec<String>), AgentError> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| AgentError::ConfigError("tool command cannot be empty".to_string()))?;
    Ok((program.to_string(), parts.map(str::to_string).collect()))
}

fn render_cli_command(
    command: &str,
    args_template: &[String],
    arguments: &serde_json::Value,
    tool_name: &str,
) -> Result<(String, Vec<String>), AgentError> {
    let argument_map = match arguments {
        serde_json::Value::Null => None,
        serde_json::Value::Object(map) => Some(map),
        other => {
            return Err(AgentError::ToolExecutionError {
                tool: tool_name.to_string(),
                message: format!("tool arguments must be a JSON object, got {other}"),
            });
        }
    };

    let (program, command_tokens) = split_command(command)?;
    let mut consumed = HashSet::new();
    let mut rendered_args = Vec::with_capacity(command_tokens.len() + args_template.len());

    for token in command_tokens {
        rendered_args.push(render_template_token(
            &token,
            argument_map,
            &mut consumed,
            tool_name,
        )?);
    }

    for token in args_template {
        rendered_args.push(render_template_token(
            token,
            argument_map,
            &mut consumed,
            tool_name,
        )?);
    }

    if let Some(map) = argument_map {
        let mut remaining: Vec<_> = map.iter().collect();
        remaining.sort_by(|(left, _), (right, _)| left.cmp(right));

        for (name, value) in remaining {
            if consumed.contains(name.as_str()) {
                continue;
            }

            match value {
                serde_json::Value::Null => {}
                serde_json::Value::Bool(true) => rendered_args.push(format!("--{name}")),
                serde_json::Value::Bool(false) => {}
                other => {
                    rendered_args.push(format!("--{name}"));
                    rendered_args.push(json_value_to_arg(other, tool_name)?);
                }
            }
        }
    }

    Ok((program, rendered_args))
}

fn render_template_token(
    template: &str,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    consumed: &mut HashSet<String>,
    tool_name: &str,
) -> Result<String, AgentError> {
    let Some(arguments) = arguments else {
        if template.contains('{') {
            return Err(AgentError::ToolExecutionError {
                tool: tool_name.to_string(),
                message: format!("missing tool arguments required by template '{template}'"),
            });
        }
        return Ok(template.to_string());
    };

    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find('{') {
        rendered.push_str(&remaining[..start]);
        let rest = &remaining[start + 1..];
        let Some(end) = rest.find('}') else {
            return Err(AgentError::ToolExecutionError {
                tool: tool_name.to_string(),
                message: format!("unclosed placeholder in argument template '{template}'"),
            });
        };

        let key = &rest[..end];
        let value = arguments
            .get(key)
            .ok_or_else(|| AgentError::ToolExecutionError {
                tool: tool_name.to_string(),
                message: format!("missing required tool argument '{key}'"),
            })?;
        rendered.push_str(&json_value_to_arg(value, tool_name)?);
        consumed.insert(key.to_string());
        remaining = &rest[end + 1..];
    }

    rendered.push_str(remaining);
    Ok(rendered)
}

fn json_value_to_arg(value: &serde_json::Value, tool_name: &str) -> Result<String, AgentError> {
    match value {
        serde_json::Value::Null => Err(AgentError::ToolExecutionError {
            tool: tool_name.to_string(),
            message: "null is not a valid CLI argument value".to_string(),
        }),
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Bool(flag) => Ok(flag.to_string()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        other => serde_json::to_string(other).map_err(|e| AgentError::ToolExecutionError {
            tool: tool_name.to_string(),
            message: format!("failed to serialize tool argument: {e}"),
        }),
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

        assert!(
            registry.get("nonexistent").is_none(),
            "unknown tool should return None"
        );
    }

    #[test]
    fn test_tool_definitions_for_llm() {
        let mut registry = ToolRegistry::new();
        registry.register(make_tool("tool_a"));
        registry.register(make_tool("tool_b"));

        let defs = registry.definitions();
        assert_eq!(
            defs.len(),
            2,
            "should return one definition per registered tool"
        );
    }

    #[test]
    fn test_render_cli_command_replaces_placeholders() {
        let arguments = json!({
            "number": 42,
            "title": "Fix flaky tests",
        });

        let (program, args) = render_cli_command(
            "gh pr view {number}",
            &["--title={title}".to_string()],
            &arguments,
            "gh_tool",
        )
        .unwrap();

        assert_eq!(program, "gh");
        assert_eq!(args, vec!["pr", "view", "42", "--title=Fix flaky tests"]);
    }

    #[test]
    fn test_render_cli_command_appends_unused_arguments_as_flags() {
        let arguments = json!({
            "body": "Details",
            "draft": true,
            "title": "Ship it",
        });

        let (program, args) =
            render_cli_command("gh pr create", &[], &arguments, "gh_tool").unwrap();

        assert_eq!(program, "gh");
        assert_eq!(
            args,
            vec!["pr", "create", "--body", "Details", "--draft", "--title", "Ship it",]
        );
    }
}
