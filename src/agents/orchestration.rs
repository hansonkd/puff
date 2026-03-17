use std::sync::Arc;

use crate::agents::agent::{Agent, AgentConfig};
use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::llm::*;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Classifies an incoming message and dispatches it to one of the registered
/// specialist agents based on an LLM routing decision.
pub struct Router {
    pub name: String,
    pub router_prompt: String,
    pub router_model: String,
    pub agents: Vec<Agent>,
}

impl Router {
    /// Ask the LLM which agent index (0-based) best fits `message`.
    ///
    /// The LLM is given a compact listing of available agents and asked to
    /// return only the integer index. The response is parsed and bounds-checked
    /// before being returned.
    pub async fn route(
        &self,
        message: &str,
        llm_client: &LlmClient,
    ) -> Result<usize, AgentError> {
        // Build a system prompt that lists all agents.
        let mut agent_listing = String::new();
        for (i, agent) in self.agents.iter().enumerate() {
            let description = agent
                .config
                .system_prompt
                .as_deref()
                .unwrap_or("(no description)");
            agent_listing.push_str(&format!(
                "{i}: {} — {}\n",
                agent.config.name, description
            ));
        }

        let system = format!(
            "{}\n\nAvailable agents:\n{}\nRespond with only the integer index of the best agent.",
            self.router_prompt, agent_listing
        );

        let mut conversation = Conversation::new(&self.name);
        conversation.add_user_message(message);

        let request = LlmRequest::new(self.router_model.clone(), conversation.messages)
            .with_system(system)
            .with_max_tokens(16);

        let response = llm_client.chat(request).await?;

        // Parse the first token-like integer from the response text.
        let index: usize = response
            .text
            .trim()
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                AgentError::OrchestrationError(format!(
                    "Router '{}' returned non-integer response: {:?}",
                    self.name, response.text
                ))
            })?;

        if index >= self.agents.len() {
            return Err(AgentError::OrchestrationError(format!(
                "Router '{}' returned out-of-bounds index {} (have {} agents)",
                self.name,
                index,
                self.agents.len()
            )));
        }

        Ok(index)
    }
}

// ---------------------------------------------------------------------------
// Chain
// ---------------------------------------------------------------------------

/// Runs agents sequentially, feeding each agent's output as the next agent's
/// input (pipeline / chain pattern).
pub struct Chain {
    pub name: String,
    pub agents: Vec<Agent>,
}

impl Chain {
    /// Execute the chain. The first agent receives `input`; every subsequent
    /// agent receives the previous agent's text output. Returns the final
    /// agent's output.
    pub async fn run(
        &self,
        input: &str,
        llm_client: &LlmClient,
    ) -> Result<String, AgentError> {
        if self.agents.is_empty() {
            return Err(AgentError::OrchestrationError(format!(
                "Chain '{}' has no agents",
                self.name
            )));
        }

        let mut current_input = input.to_string();

        for agent in &self.agents {
            let mut conversation = Conversation::new(&agent.config.name);
            conversation.add_user_message(&current_input);

            let response = agent.run_turn(&mut conversation, llm_client).await?;
            current_input = response.text;
        }

        Ok(current_input)
    }
}

// ---------------------------------------------------------------------------
// Parallel
// ---------------------------------------------------------------------------

/// Runs all agents against the same input (sequentially for now; true
/// parallelism is planned for Phase 2) and optionally merges results with an
/// additional LLM call.
pub struct Parallel {
    pub name: String,
    pub agents: Vec<Agent>,
    pub merge_prompt: Option<String>,
    pub merge_model: Option<String>,
}

impl Parallel {
    /// Run all agents against `input` in parallel and collect their responses.
    ///
    /// Each agent is spawned as an independent `tokio` task so all agents run
    /// concurrently. Results are formatted as `"[agent_name]: response"`. If
    /// `merge_prompt` is set, a final LLM call is made to synthesise the
    /// results into a single response; otherwise the formatted results are
    /// concatenated with newlines.
    pub async fn run(
        &self,
        input: &str,
        llm_client: Arc<LlmClient>,
    ) -> Result<String, AgentError> {
        if self.agents.is_empty() {
            return Err(AgentError::OrchestrationError(format!(
                "Parallel '{}' has no agents",
                self.name
            )));
        }

        let mut handles = Vec::with_capacity(self.agents.len());

        for agent in &self.agents {
            let input_owned = input.to_string();
            let agent_name = agent.config.name.clone();
            let agent_model = agent.config.model.clone();
            let system_prompt = agent.build_system_prompt();
            let tools = Arc::clone(&agent.tools);
            let client = Arc::clone(&llm_client);

            let handle = tokio::spawn(async move {
                let config = AgentConfig {
                    name: agent_name.clone(),
                    model: agent_model,
                    system_prompt: Some(system_prompt),
                    skills: vec![],
                    tools_module: None,
                    memory: None,
                    permissions: None,
                };
                let task_agent = Agent::new(config).with_tools_arc(tools);
                let mut conv = Conversation::new(&agent_name);
                conv.add_user_message(&input_owned);

                match task_agent.run_turn(&mut conv, &client).await {
                    Ok(response) => format!("[{}]: {}", agent_name, response.text),
                    Err(e) => format!("[{}]: Error: {}", agent_name, e),
                }
            });

            handles.push(handle);
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(result) => results.push(result),
                Err(e) => results.push(format!("Task panicked: {}", e)),
            }
        }

        let combined = results.join("\n\n");

        // Optionally merge with an LLM call.
        match (&self.merge_prompt, &self.merge_model) {
            (Some(merge_prompt), Some(merge_model)) => {
                let merge_user_message = format!(
                    "Here are the responses from multiple agents:\n\n{}\n\nOriginal input: {}",
                    combined, input
                );

                let mut merge_conversation = Conversation::new(&self.name);
                merge_conversation.add_user_message(&merge_user_message);

                let merge_request =
                    LlmRequest::new(merge_model.clone(), merge_conversation.messages)
                        .with_system(merge_prompt.clone());

                let merge_response = llm_client.chat(merge_request).await?;
                Ok(merge_response.text)
            }
            _ => Ok(combined),
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

/// Runs a supervisor agent that delegates tasks to named worker agents via an
/// auto-generated `delegate` tool.
pub struct Supervisor {
    pub name: String,
    pub supervisor: Agent,
    pub workers: Vec<Agent>,
}

impl Supervisor {
    /// Run the supervisor loop: supervisor decides what to delegate, workers execute.
    ///
    /// The supervisor LLM is given a synthetic `delegate` tool whose description
    /// lists the available workers. When the supervisor calls `delegate`, the
    /// named worker is run with the provided task and its output is returned as
    /// the tool result. The loop continues until the supervisor produces a plain
    /// text response (no tool calls) or the maximum number of rounds is reached.
    pub async fn run(
        &self,
        input: &str,
        llm_client: &LlmClient,
    ) -> Result<String, AgentError> {
        // 1. Build a delegate tool definition for the supervisor.
        let delegate_tool = ToolDefinition {
            name: "delegate".to_string(),
            description: format!(
                "Delegate a task to a worker agent. Available workers: {}",
                self.workers
                    .iter()
                    .map(|w| w.config.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "worker": {"type": "string", "description": "Name of the worker to delegate to"},
                    "task": {"type": "string", "description": "The task to delegate"}
                },
                "required": ["worker", "task"]
            }),
        };

        // 2. Create supervisor conversation with the delegate tool.
        let mut conv = Conversation::new(&self.name);
        conv.add_user_message(input);

        // 3. Build tools list: supervisor's own tools + delegate tool.
        let mut tools = self.supervisor.tools.definitions();
        tools.push(delegate_tool);

        let system_prompt = self.supervisor.build_system_prompt();

        // 4. Loop: call supervisor LLM, handle delegate calls, feed results back.
        let max_rounds = 10;
        for _ in 0..max_rounds {
            let mut request =
                LlmRequest::new(self.supervisor.config.model.clone(), conv.messages.clone());
            if !system_prompt.is_empty() {
                request = request.with_system(system_prompt.clone());
            }
            request = request.with_tools(tools.clone());

            let response = llm_client.chat(request).await?;

            if response.tool_calls.is_empty() {
                // Supervisor is done — return its final text.
                return Ok(response.text);
            }

            // Add the assistant message (with ToolUse blocks) to the conversation.
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
            conv.add_message(Message {
                role: Role::Assistant,
                content: MessageContent::Blocks(blocks),
                tool_call_id: None,
            });

            // Process each tool call.
            for tc in &response.tool_calls {
                let result_content = if tc.name == "delegate" {
                    // Parse worker name and task from arguments.
                    let worker_name = tc
                        .arguments
                        .get("worker")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let task = tc
                        .arguments
                        .get("task")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // Find the requested worker.
                    match self.workers.iter().find(|w| w.config.name == worker_name) {
                        Some(worker) => {
                            let mut worker_conv = Conversation::new(&worker.config.name);
                            worker_conv.add_user_message(task);
                            match worker.run_turn(&mut worker_conv, llm_client).await {
                                Ok(resp) => resp.text,
                                Err(e) => format!("Worker '{}' error: {}", worker_name, e),
                            }
                        }
                        None => format!(
                            "Worker '{}' not found. Available: {}",
                            worker_name,
                            self.workers
                                .iter()
                                .map(|w| w.config.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    }
                } else {
                    format!("Tool '{}' not implemented in supervisor", tc.name)
                };

                conv.add_message(Message {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: result_content,
                        is_error: None,
                    }]),
                    tool_call_id: Some(tc.id.clone()),
                });
            }
        }

        Err(AgentError::OrchestrationError(
            "Supervisor max rounds exceeded".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Handoff
// ---------------------------------------------------------------------------

/// Transfer a conversation from one agent to another.
///
/// The conversation history is preserved (same `Conversation` object). A
/// system-level message is appended to the conversation so that the receiving
/// agent is aware of the context transfer. The conversation's `agent_name` is
/// updated to the new agent.
pub fn handoff(
    conversation: &mut Conversation,
    from_agent: &str,
    to_agent: &str,
    reason: &str,
) {
    conversation.add_message(Message {
        role: Role::User,
        content: MessageContent::Text(format!(
            "[System: Conversation handed off from {} to {}. Reason: {}]",
            from_agent, to_agent, reason
        )),
        tool_call_id: None,
    });
    conversation.agent_name = to_agent.to_string();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent::AgentConfig;

    fn make_agent(name: &str, system_prompt: &str) -> Agent {
        Agent::new(AgentConfig {
            name: name.to_string(),
            model: "claude-sonnet-4-6".to_string(),
            system_prompt: Some(system_prompt.to_string()),
            skills: Vec::new(),
            tools_module: None,
            memory: None,
            permissions: None,
        })
    }

    #[test]
    fn test_router_struct_fields() {
        let router = Router {
            name: "test-router".to_string(),
            router_prompt: "Pick an agent.".to_string(),
            router_model: "claude-sonnet-4-6".to_string(),
            agents: vec![
                make_agent("agent-a", "Handles topic A"),
                make_agent("agent-b", "Handles topic B"),
            ],
        };
        assert_eq!(router.name, "test-router");
        assert_eq!(router.agents.len(), 2);
    }

    #[test]
    fn test_chain_struct_fields() {
        let chain = Chain {
            name: "test-chain".to_string(),
            agents: vec![
                make_agent("step-1", "First step"),
                make_agent("step-2", "Second step"),
            ],
        };
        assert_eq!(chain.name, "test-chain");
        assert_eq!(chain.agents.len(), 2);
    }

    #[test]
    fn test_parallel_struct_fields() {
        let parallel = Parallel {
            name: "test-parallel".to_string(),
            agents: vec![
                make_agent("worker-1", "Does A"),
                make_agent("worker-2", "Does B"),
            ],
            merge_prompt: Some("Merge these.".to_string()),
            merge_model: Some("claude-sonnet-4-6".to_string()),
        };
        assert_eq!(parallel.name, "test-parallel");
        assert_eq!(parallel.agents.len(), 2);
        assert!(parallel.merge_prompt.is_some());
        assert!(parallel.merge_model.is_some());
    }

    #[test]
    fn test_parallel_no_merge() {
        let parallel = Parallel {
            name: "no-merge".to_string(),
            agents: vec![make_agent("worker", "Does stuff")],
            merge_prompt: None,
            merge_model: None,
        };
        assert!(parallel.merge_prompt.is_none());
        assert!(parallel.merge_model.is_none());
    }

    #[test]
    fn test_supervisor_struct_fields() {
        let supervisor = Supervisor {
            name: "test-supervisor".to_string(),
            supervisor: make_agent("boss", "You are the supervisor."),
            workers: vec![
                make_agent("worker-a", "Handles topic A"),
                make_agent("worker-b", "Handles topic B"),
            ],
        };
        assert_eq!(supervisor.name, "test-supervisor");
        assert_eq!(supervisor.workers.len(), 2);
        assert_eq!(supervisor.supervisor.config.name, "boss");
    }

    #[test]
    fn test_handoff_updates_agent_name_and_appends_message() {
        let mut conv = Conversation::new("agent-alpha");
        conv.add_user_message("Hello");
        assert_eq!(conv.agent_name, "agent-alpha");
        assert_eq!(conv.messages.len(), 1);

        handoff(&mut conv, "agent-alpha", "agent-beta", "user requested escalation");

        // agent_name is updated to the receiving agent.
        assert_eq!(conv.agent_name, "agent-beta");
        // A handoff message was appended.
        assert_eq!(conv.messages.len(), 2);

        // Verify content of the handoff message.
        let last = conv.messages.last().unwrap();
        match &last.content {
            MessageContent::Text(text) => {
                assert!(text.contains("agent-alpha"));
                assert!(text.contains("agent-beta"));
                assert!(text.contains("user requested escalation"));
            }
            _ => panic!("expected text message for handoff"),
        }
    }

    #[test]
    fn test_handoff_preserves_history() {
        let mut conv = Conversation::new("first");
        conv.add_user_message("Message one");
        conv.add_assistant_message("Reply one");

        handoff(&mut conv, "first", "second", "reason");

        // Original two messages are still present.
        assert_eq!(conv.messages.len(), 3);
        assert_eq!(conv.agent_name, "second");
    }
}
