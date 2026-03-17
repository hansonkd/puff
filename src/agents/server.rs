use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::agents::agent::Agent;
use crate::agents::conversation::Conversation;
use crate::agents::llm::LlmClient;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

pub struct AgentServerState {
    pub agents: HashMap<String, Agent>,
    pub conversations: RwLock<HashMap<String, Conversation>>,
    pub llm_client: LlmClient,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StartConversationRequest {
    pub agent: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct ConversationResponse {
    pub conversation_id: String,
    pub agent: String,
    pub response: String,
}

#[derive(Serialize)]
pub struct ConversationDetail {
    pub conversation_id: String,
    pub agent: String,
    pub message_count: usize,
    pub messages: serde_json::Value,
}

#[derive(Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub model: String,
    pub tools: Vec<String>,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub agents: usize,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn agent_router(state: Arc<AgentServerState>) -> axum::Router {
    axum::Router::new()
        .route("/api/conversations", post(start_conversation))
        .route(
            "/api/conversations/{id}",
            post(continue_conversation).get(get_conversation),
        )
        .route("/api/agents", get(list_agents))
        .route("/health", get(health_check))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn start_conversation(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<StartConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let agent = state
        .agents
        .get(&req.agent)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Agent '{}' not found", req.agent),
            )
        })?;

    let mut conversation = Conversation::new(&req.agent);
    conversation.add_user_message(&req.message);

    let response = agent
        .run_turn(&mut conversation, &state.llm_client)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let conversation_id = conversation.id.clone();
    let agent_name = conversation.agent_name.clone();

    state
        .conversations
        .write()
        .await
        .insert(conversation_id.clone(), conversation);

    Ok(Json(ConversationResponse {
        conversation_id,
        agent: agent_name,
        response: response.text,
    }))
}

async fn continue_conversation(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
    Json(req): Json<StartConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let mut conversations = state.conversations.write().await;

    let conversation = conversations
        .get_mut(&id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Conversation '{}' not found", id),
            )
        })?;

    let agent = state
        .agents
        .get(&conversation.agent_name)
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Agent '{}' not found for conversation",
                    conversation.agent_name
                ),
            )
        })?;

    conversation.add_user_message(&req.message);

    let response = agent
        .run_turn(conversation, &state.llm_client)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ConversationResponse {
        conversation_id: id,
        agent: conversation.agent_name.clone(),
        response: response.text,
    }))
}

async fn get_conversation(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
) -> Result<Json<ConversationDetail>, (StatusCode, String)> {
    let conversations = state.conversations.read().await;

    let conversation = conversations
        .get(&id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Conversation '{}' not found", id),
            )
        })?;

    let messages = serde_json::to_value(&conversation.messages)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ConversationDetail {
        conversation_id: conversation.id.clone(),
        agent: conversation.agent_name.clone(),
        message_count: conversation.messages.len(),
        messages,
    }))
}

async fn list_agents(
    State(state): State<Arc<AgentServerState>>,
) -> Json<Vec<AgentInfo>> {
    let agents: Vec<AgentInfo> = state
        .agents
        .iter()
        .map(|(name, agent)| AgentInfo {
            name: name.clone(),
            model: agent.config.model.clone(),
            tools: agent.tools.names().into_iter().map(|s| s.to_string()).collect(),
        })
        .collect();

    Json(agents)
}

async fn health_check(
    State(state): State<Arc<AgentServerState>>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        agents: state.agents.len(),
    })
}
