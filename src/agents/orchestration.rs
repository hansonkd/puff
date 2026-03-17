use crate::agents::agent::Agent;
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
    /// Run all agents against `input` and collect their responses.
    ///
    /// Results are formatted as `"[agent_name]: response"`. If `merge_prompt`
    /// is set, a final LLM call is made to synthesise the results into a
    /// single response; otherwise the formatted results are concatenated with
    /// newlines.
    pub async fn run(
        &self,
        input: &str,
        llm_client: &LlmClient,
    ) -> Result<String, AgentError> {
        if self.agents.is_empty() {
            return Err(AgentError::OrchestrationError(format!(
                "Parallel '{}' has no agents",
                self.name
            )));
        }

        // Run each agent sequentially (Phase 2: true async parallel).
        let mut results: Vec<String> = Vec::with_capacity(self.agents.len());

        for agent in &self.agents {
            let mut conversation = Conversation::new(&agent.config.name);
            conversation.add_user_message(input);

            let response = agent.run_turn(&mut conversation, llm_client).await?;
            results.push(format!("[{}]: {}", agent.config.name, response.text));
        }

        let combined = results.join("\n");

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
}
