use crate::agents::agent::Agent;
use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::llm::LlmClient;
use crate::agents::persistence::{load_conversation_snapshot, save_conversation_snapshot};
use crate::databases::postgres::PostgresClient;
use crate::supervision::{
    AgentInstanceSpec, SupervisorCommand, SupervisorNodeSnapshot, SupervisorNodeState,
    SupervisorRuntime,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

const MAILBOX_BUFFER: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessageResponse {
    pub agent: String,
    pub conversation_id: String,
    pub response: String,
    pub message_count: usize,
}

pub(crate) enum AgentMailboxCommand {
    SendUserMessage {
        message: String,
        respond_to: oneshot::Sender<Result<AgentMessageResponse, AgentError>>,
    },
    GetConversation {
        respond_to: oneshot::Sender<Conversation>,
    },
}

#[derive(Clone)]
pub struct AgentMailbox {
    key: String,
    agent_name: String,
    node_id: String,
    sender: mpsc::Sender<AgentMailboxCommand>,
}

impl AgentMailbox {
    pub(crate) fn new(
        key: impl Into<String>,
        agent_name: impl Into<String>,
        node_id: impl Into<String>,
        sender: mpsc::Sender<AgentMailboxCommand>,
    ) -> Self {
        Self {
            key: key.into(),
            agent_name: agent_name.into(),
            node_id: node_id.into(),
            sender,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub async fn send_message(
        &self,
        message: impl Into<String>,
    ) -> Result<AgentMessageResponse, AgentError> {
        let (respond_to, response_rx) = oneshot::channel();
        self.sender
            .send(AgentMailboxCommand::SendUserMessage {
                message: message.into(),
                respond_to,
            })
            .await
            .map_err(|_| {
                AgentError::OrchestrationError(format!(
                    "agent mailbox '{}' is no longer available",
                    self.key
                ))
            })?;

        response_rx.await.map_err(|_| {
            AgentError::OrchestrationError(format!(
                "agent mailbox '{}' dropped its response channel",
                self.key
            ))
        })?
    }

    pub async fn conversation(&self) -> Result<Conversation, AgentError> {
        let (respond_to, response_rx) = oneshot::channel();
        self.sender
            .send(AgentMailboxCommand::GetConversation { respond_to })
            .await
            .map_err(|_| AgentError::ConversationNotFound(self.key.clone()))?;

        response_rx
            .await
            .map_err(|_| AgentError::ConversationNotFound(self.key.clone()))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_supervised_agent_mailbox(
    supervisor: &SupervisorRuntime,
    parent_id: &str,
    key: impl Into<String>,
    node_id: Option<String>,
    agent: Agent,
    llm_client: LlmClient,
    postgres: Option<PostgresClient>,
    metadata: serde_json::Value,
) -> anyhow::Result<(SupervisorNodeSnapshot, AgentMailbox)> {
    let agent_name = agent.config.name.clone();
    let spec = AgentInstanceSpec {
        agent_name: agent_name.clone(),
        display_name: agent_name.clone(),
        metadata,
    };

    let (snapshot, mut supervisor_commands) = match node_id {
        Some(node_id) => supervisor.spawn_named_managed_agent_instance(node_id, parent_id, spec)?,
        None => supervisor.spawn_managed_agent_instance(parent_id, spec)?,
    };

    let (mailbox_tx, mut mailbox_rx) = mpsc::channel(MAILBOX_BUFFER);
    let mailbox = AgentMailbox::new(
        key,
        agent_name.clone(),
        snapshot.node_id.clone(),
        mailbox_tx,
    );

    let runtime = supervisor.clone();
    let node_id = snapshot.node_id.clone();
    let mailbox_key = mailbox.key().to_string();
    tokio::spawn(async move {
        let mut conversation = match postgres.as_ref() {
            Some(postgres) => match load_conversation_snapshot(postgres, &mailbox_key).await {
                Ok(Some(saved)) => saved,
                Ok(None) => Conversation::new(&agent_name).with_id(node_id.clone()),
                Err(error) => {
                    tracing::warn!(
                        "Failed to load persisted conversation '{}' for '{}': {}",
                        mailbox_key,
                        agent_name,
                        error
                    );
                    Conversation::new(&agent_name).with_id(node_id.clone())
                }
            },
            None => Conversation::new(&agent_name).with_id(node_id.clone()),
        };
        runtime.set_node_state(&node_id, SupervisorNodeState::Running, None);

        loop {
            tokio::select! {
                command = supervisor_commands.recv() => {
                    match command {
                        Some(SupervisorCommand::Stop) | None => {
                            runtime.set_node_state(&node_id, SupervisorNodeState::Stopping, None);
                            runtime.set_node_state(&node_id, SupervisorNodeState::Stopped, None);
                            break;
                        }
                        Some(SupervisorCommand::Restart) => {
                            runtime.set_node_state(
                                &node_id,
                                SupervisorNodeState::Starting,
                                Some("explicit restart requested".to_string()),
                            );
                            runtime.bump_restart_count(&node_id);
                            conversation = Conversation::new(&agent_name).with_id(node_id.clone());
                            if let Some(postgres) = postgres.as_ref() {
                                if let Err(error) = save_conversation_snapshot(
                                    postgres,
                                    &mailbox_key,
                                    &node_id,
                                    &agent_name,
                                    &conversation,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        "Failed to persist reset conversation '{}' for '{}': {}",
                                        mailbox_key,
                                        agent_name,
                                        error
                                    );
                                }
                            }
                            runtime.set_node_state(&node_id, SupervisorNodeState::Running, None);
                        }
                    }
                }
                command = mailbox_rx.recv() => {
                    let Some(command) = command else {
                        runtime.set_node_state(&node_id, SupervisorNodeState::Stopped, None);
                        break;
                    };

                    match command {
                        AgentMailboxCommand::SendUserMessage { message, respond_to } => {
                            conversation.add_user_message(&message);
                            let result = match agent.run_turn(&mut conversation, &llm_client).await {
                                Ok(response) => Ok(AgentMessageResponse {
                                    agent: agent_name.clone(),
                                    conversation_id: conversation.id.clone(),
                                    response: response.text,
                                    message_count: conversation.messages.len(),
                                }),
                                Err(error) => {
                                    runtime.record_failure(&node_id, error.to_string());
                                    Err(error)
                                }
                            };
                            if let Some(postgres) = postgres.as_ref() {
                                if let Err(error) = save_conversation_snapshot(
                                    postgres,
                                    &mailbox_key,
                                    &node_id,
                                    &agent_name,
                                    &conversation,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        "Failed to persist conversation '{}' for '{}': {}",
                                        mailbox_key,
                                        agent_name,
                                        error
                                    );
                                }
                            }
                            let _ = respond_to.send(result);
                            if runtime
                                .node_snapshot(&node_id)
                                .map(|snapshot| snapshot.state == SupervisorNodeState::Failed)
                                .unwrap_or(false)
                            {
                                runtime.set_node_state(&node_id, SupervisorNodeState::Running, None);
                            }
                        }
                        AgentMailboxCommand::GetConversation { respond_to } => {
                            let _ = respond_to.send(conversation.clone());
                        }
                    }
                }
            }
        }
    });

    Ok((snapshot, mailbox))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent::{Agent, AgentConfig};
    use crate::agents::llm::{LlmClient, LlmConfig};
    use crate::supervision::{SupervisionConfig, ROOT_NODE_ID};
    use tokio::runtime::Handle;

    fn make_agent(name: &str) -> Agent {
        Agent::new(AgentConfig {
            name: name.to_string(),
            model: "claude-sonnet-4-6".to_string(),
            system_prompt: Some("You are helpful.".to_string()),
            skills: Vec::new(),
            tools_module: None,
            memory: None,
            permissions: None,
            capabilities: None,
            scope: None,
            visible_scopes: Vec::new(),
        })
    }

    #[tokio::test]
    async fn mailbox_conversation_starts_empty() {
        let runtime = SupervisorRuntime::new(Handle::current(), SupervisionConfig::default());
        let llm = LlmClient::new(LlmConfig::default()).expect("llm client");
        let (_snapshot, mailbox) = spawn_supervised_agent_mailbox(
            &runtime,
            ROOT_NODE_ID,
            "agent/tester",
            Some("agent-instance:tester".to_string()),
            make_agent("tester"),
            llm,
            None,
            serde_json::Value::Null,
        )
        .expect("spawn mailbox");

        let conversation = mailbox.conversation().await.expect("conversation");
        assert_eq!(conversation.agent_name, "tester");
        assert!(conversation.messages.is_empty());
    }
}
