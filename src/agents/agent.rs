//! Core agent struct and execution loop.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;

use crate::agents::budget::AgentBudgetTracker;
use crate::agents::capabilities::AgentCapabilities;
use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::llm::{
    ContentBlock, LlmClient, LlmRequest, LlmRequestRef, LlmResponse, Message, MessageContent, Role,
    ToolDefinition,
};
use crate::agents::memory::{self, MemoryConfig};
use crate::agents::tool::{RegisteredTool, ToolRegistry};

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
    #[serde(default)]
    pub capabilities: Option<AgentCapabilities>,
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
const MAX_ARCHIVED_CONTEXT_CHARS: usize = 32 * 1024;

pub struct ToolExecutionOutcome {
    pub content: String,
    pub is_error: Option<bool>,
}

impl ToolExecutionOutcome {
    fn success(content: String) -> Self {
        Self {
            content,
            is_error: None,
        }
    }

    fn error(error: AgentError) -> Self {
        Self {
            content: error.to_string(),
            is_error: Some(true),
        }
    }
}

pub struct Agent {
    pub config: AgentConfig,
    pub tools: Arc<ToolRegistry>,
    pub context_snippets: Vec<String>,
    system_prompt: String,
    tool_definitions: Arc<[ToolDefinition]>,
    pub capabilities: AgentCapabilities,
    pub budget: Arc<AgentBudgetTracker>,
}

impl Agent {
    /// Create a new agent with empty tools and no context snippets.
    pub fn new(config: AgentConfig) -> Self {
        let capabilities = config.capabilities.clone().unwrap_or_default();
        let budget = Arc::new(AgentBudgetTracker::new(capabilities.budget.clone()));
        Self {
            config,
            tools: Arc::new(ToolRegistry::new()),
            context_snippets: Vec::new(),
            system_prompt: String::new(),
            tool_definitions: Arc::<[ToolDefinition]>::from(Vec::new()),
            capabilities,
            budget,
        }
        .rebuild_system_prompt()
    }

    /// Set the tool registry for this agent.
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = Arc::new(tools);
        self.rebuild_tool_definitions()
    }

    /// Attach a pre-existing `Arc<ToolRegistry>` without double-wrapping.
    /// Useful when sharing a registry across parallel tasks.
    pub fn with_tools_arc(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = tools;
        self.rebuild_tool_definitions()
    }

    /// Add a context snippet (e.g. from a skill's `context.md`).
    pub fn with_context(mut self, context: String) -> Self {
        self.context_snippets.push(context);
        self.rebuild_system_prompt()
    }

    fn rebuild_system_prompt(mut self) -> Self {
        let mut parts: Vec<&str> = Vec::new();

        if let Some(ref prompt) = self.config.system_prompt {
            parts.push(prompt.as_str());
        }

        for snippet in &self.context_snippets {
            parts.push(snippet.as_str());
        }

        self.system_prompt = parts.join("\n\n");
        self
    }

    fn rebuild_tool_definitions(mut self) -> Self {
        self.tool_definitions = Arc::from(self.tools.definitions());
        self
    }

    /// Build the full system prompt by joining the configured system prompt
    /// with any context snippets, separated by double newlines.
    pub fn build_system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.tool_definitions
    }

    pub fn prepare_tool_call(&self, tool_name: &str) -> Result<RegisteredTool, AgentError> {
        self.capabilities.check_tool(tool_name)?;
        self.budget.check_tool_budget()?;
        self.tools
            .get(tool_name)
            .cloned()
            .ok_or_else(|| AgentError::ToolNotFound(tool_name.to_string()))
    }

    pub async fn execute_prepared_tool(
        &self,
        tool: &RegisteredTool,
        tool_call: &crate::agents::llm::ToolCall,
    ) -> Result<String, AgentError> {
        self.budget.record_tool_call();
        crate::agents::tool::execute_registered_tool(tool, &tool_call.arguments, &self.capabilities)
            .await
    }

    pub async fn execute_tool_call(
        &self,
        tool_call: &crate::agents::llm::ToolCall,
    ) -> ToolExecutionOutcome {
        let tool = match self.prepare_tool_call(&tool_call.name) {
            Ok(tool) => tool,
            Err(error) => return ToolExecutionOutcome::error(error),
        };

        match self.execute_prepared_tool(&tool, tool_call).await {
            Ok(content) => ToolExecutionOutcome::success(content),
            Err(error) => ToolExecutionOutcome::error(error),
        }
    }

    fn effective_system_prompt<'a>(
        &'a self,
        conversation: &'a Conversation,
    ) -> Option<Cow<'a, str>> {
        match (
            self.system_prompt.is_empty(),
            conversation.archived_context(),
        ) {
            (true, None) => None,
            (false, None) => Some(Cow::Borrowed(self.system_prompt.as_str())),
            (true, Some(archived)) => Some(Cow::Owned(format!(
                "Earlier conversation context:\n{}",
                archived
            ))),
            (false, Some(archived)) => Some(Cow::Owned(format!(
                "{}\n\nEarlier conversation context:\n{}",
                self.system_prompt, archived
            ))),
        }
    }

    fn conversation_window(&self) -> Option<usize> {
        self.config
            .memory
            .as_ref()
            .map(|memory| memory.summarize_after as usize)
            .filter(|window| *window > 0)
    }

    pub fn compact_conversation(&self, conversation: &mut Conversation) {
        if let Some(window) = self.conversation_window() {
            conversation.compact(window, MAX_ARCHIVED_CONTEXT_CHARS);
        }
    }

    pub fn build_request_ref<'a>(&'a self, conversation: &'a Conversation) -> LlmRequestRef<'a> {
        let mut request =
            LlmRequestRef::new(self.config.model.as_str(), conversation.messages.as_slice())
                .with_tools(self.tool_definitions());

        if let Some(system_prompt) = self.effective_system_prompt(conversation) {
            request = request.with_system(system_prompt);
        }

        request
    }

    /// Build a complete `LlmRequest` from the current conversation state.
    pub fn build_request(&self, conversation: &Conversation) -> LlmRequest {
        self.build_request_ref(conversation).to_owned()
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
            self.compact_conversation(conversation);

            // Check LLM budget before making the call.
            self.budget.check_llm_budget()?;

            let response = llm_client
                .chat_ref(self.build_request_ref(conversation))
                .await?;

            // Record token/cost usage from the response.
            self.budget.record_llm_usage(
                response.input_tokens as u64,
                response.output_tokens as u64,
                memory::estimate_cost(
                    &self.config.model,
                    response.input_tokens,
                    response.output_tokens,
                ),
            );

            if response.tool_calls.is_empty() {
                // No tool calls — final response. Add assistant message and return.
                if !response.text.is_empty() {
                    conversation.add_assistant_message(&response.text);
                    self.compact_conversation(conversation);
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
            self.compact_conversation(conversation);

            // Execute each tool call and add tool result messages.
            for tc in &response.tool_calls {
                let outcome = self.execute_tool_call(tc).await;

                conversation.add_message(Message {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: outcome.content,
                        is_error: outcome.is_error,
                    }]),
                    tool_call_id: Some(tc.id.clone()),
                });
                self.compact_conversation(conversation);
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
            capabilities: None,
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
        let agent = Agent::new(make_config("test")).with_context("Skill context here.".to_string());
        let prompt = agent.build_system_prompt();
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("Skill context here."));
        assert!(prompt.contains("\n\n"));
    }

    #[test]
    fn test_build_system_prompt_no_system() {
        let mut config = make_config("test");
        config.system_prompt = None;
        let agent = Agent::new(config).with_context("Only context.".to_string());
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

    #[test]
    fn test_build_request_uses_archived_context() {
        let mut config = make_config("test");
        config.memory = Some(MemoryConfig {
            summarize_after: 1,
            ..MemoryConfig::default()
        });
        let agent = Agent::new(config);
        let mut conversation = Conversation::new("test");
        conversation.add_user_message("first");
        conversation.add_assistant_message("second");
        agent.compact_conversation(&mut conversation);

        let request = agent.build_request(&conversation);
        assert_eq!(conversation.messages.len(), 1);
        assert!(request
            .system
            .unwrap()
            .contains("Earlier conversation context"));
    }
}
