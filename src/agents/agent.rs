use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::llm::{
    ContentBlock, LlmClient, LlmRequest, LlmResponse, Message, MessageContent, Role,
};
use crate::agents::memory::MemoryConfig;
use crate::agents::tool::ToolRegistry;

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub tools_module: Option<String>,
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
    #[serde(default)]
    pub permissions: Option<AgentPermissions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentPermissions {
    pub sql: Option<String>,
    pub http: Option<Vec<String>>,
    pub filesystem: Option<String>,
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

const MAX_TOOL_ROUNDS: usize = 10;

pub struct Agent {
    pub config: AgentConfig,
    pub tools: Arc<ToolRegistry>,
    pub context_snippets: Vec<String>,
}

impl Agent {
    /// Create a new agent with empty tools and no context snippets.
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            tools: Arc::new(ToolRegistry::new()),
            context_snippets: Vec::new(),
        }
    }

    /// Set the tool registry for this agent.
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = Arc::new(tools);
        self
    }

    /// Add a context snippet (e.g. from a skill's `context.md`).
    pub fn with_context(mut self, context: String) -> Self {
        self.context_snippets.push(context);
        self
    }

    /// Build the full system prompt by joining the configured system prompt
    /// with any context snippets, separated by double newlines.
    pub fn build_system_prompt(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();

        if let Some(ref prompt) = self.config.system_prompt {
            parts.push(prompt.as_str());
        }

        for snippet in &self.context_snippets {
            parts.push(snippet.as_str());
        }

        parts.join("\n\n")
    }

    /// Build a complete `LlmRequest` from the current conversation state.
    pub fn build_request(&self, conversation: &Conversation) -> LlmRequest {
        let system_prompt = self.build_system_prompt();

        let mut request = LlmRequest::new(
            self.config.model.clone(),
            conversation.messages.clone(),
        );

        if !system_prompt.is_empty() {
            request = request.with_system(system_prompt);
        }

        let tool_defs = self.tools.definitions();
        if !tool_defs.is_empty() {
            request = request.with_tools(tool_defs);
        }

        request
    }

    /// Run a single agent turn: call the LLM, execute any tool calls, and
    /// loop until the LLM produces a final response (no tool calls) or the
    /// maximum number of rounds is exceeded.
    pub async fn run_turn(
        &self,
        conversation: &mut Conversation,
        llm_client: &LlmClient,
    ) -> Result<LlmResponse, AgentError> {
        for _round in 0..MAX_TOOL_ROUNDS {
            let request = self.build_request(conversation);
            let response = llm_client.chat(request).await?;

            if response.tool_calls.is_empty() {
                // No tool calls — final response. Add assistant message and return.
                if !response.text.is_empty() {
                    conversation.add_assistant_message(&response.text);
                }
                return Ok(response);
            }

            // The response contains tool calls. Add the assistant message with
            // ToolUse content blocks, then process each tool call.
            let mut blocks: Vec<ContentBlock> = Vec::new();

            if !response.text.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: response.text.clone(),
                });
            }

            for tc in &response.tool_calls {
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }

            conversation.add_message(Message {
                role: Role::Assistant,
                content: MessageContent::Blocks(blocks),
                tool_call_id: None,
            });

            // Execute each tool call and add tool result messages.
            for tc in &response.tool_calls {
                let (content, is_error) = if self.tools.get(&tc.name).is_some() {
                    (
                        format!("Tool '{}' executed (stub)", tc.name),
                        None,
                    )
                } else {
                    (
                        format!("Tool '{}' not found in registry", tc.name),
                        Some(true),
                    )
                };

                conversation.add_message(Message {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content,
                        is_error,
                    }]),
                    tool_call_id: Some(tc.id.clone()),
                });
            }

            // Loop back to call the LLM again with the tool results.
        }

        Err(AgentError::OrchestrationError(format!(
            "Agent '{}' exceeded maximum tool rounds ({})",
            self.config.name, MAX_TOOL_ROUNDS
        )))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(name: &str) -> AgentConfig {
        AgentConfig {
            name: name.to_string(),
            model: "claude-sonnet-4-6".to_string(),
            system_prompt: Some("You are a helpful assistant.".to_string()),
            skills: Vec::new(),
            tools_module: None,
            memory: None,
            permissions: None,
        }
    }

    #[test]
    fn test_new_agent_has_empty_tools_and_context() {
        let agent = Agent::new(make_config("test"));
        assert!(agent.tools.is_empty());
        assert!(agent.context_snippets.is_empty());
    }

    #[test]
    fn test_with_tools() {
        let mut registry = ToolRegistry::new();
        use crate::agents::llm::ToolDefinition;
        use crate::agents::tool::{RegisteredTool, ToolExecutor};
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            executor: ToolExecutor::Noop,
            requires_approval: false,
            timeout_ms: 5000,
        });

        let agent = Agent::new(make_config("test")).with_tools(registry);
        assert_eq!(agent.tools.len(), 1);
    }

    #[test]
    fn test_with_context() {
        let agent = Agent::new(make_config("test"))
            .with_context("Context A".to_string())
            .with_context("Context B".to_string());
        assert_eq!(agent.context_snippets.len(), 2);
    }

    #[test]
    fn test_build_system_prompt() {
        let agent = Agent::new(make_config("test"))
            .with_context("Skill context here.".to_string());
        let prompt = agent.build_system_prompt();
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("Skill context here."));
        assert!(prompt.contains("\n\n"));
    }

    #[test]
    fn test_build_system_prompt_no_system() {
        let mut config = make_config("test");
        config.system_prompt = None;
        let agent = Agent::new(config)
            .with_context("Only context.".to_string());
        let prompt = agent.build_system_prompt();
        assert_eq!(prompt, "Only context.");
    }

    #[test]
    fn test_build_system_prompt_empty() {
        let mut config = make_config("test");
        config.system_prompt = None;
        let agent = Agent::new(config);
        let prompt = agent.build_system_prompt();
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_build_request() {
        let agent = Agent::new(make_config("test"));
        let conversation = Conversation::new("test");
        let request = agent.build_request(&conversation);
        assert_eq!(request.model, "claude-sonnet-4-6");
        assert!(request.system.is_some());
        assert!(request.tools.is_empty());
    }

    #[test]
    fn test_build_request_with_tools() {
        let mut registry = ToolRegistry::new();
        use crate::agents::llm::ToolDefinition;
        use crate::agents::tool::{RegisteredTool, ToolExecutor};
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name: "my_tool".to_string(),
                description: "desc".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            executor: ToolExecutor::Noop,
            requires_approval: false,
            timeout_ms: 5000,
        });

        let agent = Agent::new(make_config("test")).with_tools(registry);
        let conversation = Conversation::new("test");
        let request = agent.build_request(&conversation);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "my_tool");
    }
}
