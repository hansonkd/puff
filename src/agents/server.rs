//! Agent HTTP server — REST and WebSocket endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::agents::agent::Agent;
use crate::agents::llm::LlmClient;
use crate::agents::mailbox::{spawn_supervised_agent_mailbox, AgentMessageResponse};
use crate::agents::persistence::load_conversation_snapshot;
use crate::agents::registry::{AgentRegistry, AgentRegistryEntry, AgentRegistryKind};
use crate::agents::runtime::ensure_named_agent_supervisor;
use crate::databases::postgres::PostgresClient;
use crate::supervision::{
    AgentSupervisorSpec, SupervisorNodeKind, SupervisorNodeSnapshot, SupervisorTreeSnapshot,
};

pub struct AgentServerState {
    pub(crate) agents: HashMap<String, Agent>,
    pub(crate) llm_client: LlmClient,
    pub(crate) registry: AgentRegistry,
    pub(crate) supervisor: crate::supervision::SupervisorRuntime,
    pub(crate) postgres: Option<PostgresClient>,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StartConversationRequest {
    pub agent: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct ContinueConversationRequest {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_context: Option<String>,
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

#[derive(Deserialize)]
pub struct RegistryQuery {
    pub prefix: Option<String>,
}

#[derive(Deserialize)]
pub struct RegistryMessageRequest {
    pub target: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct RegistryTargetQuery {
    pub target: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpawnRuntimeChildRequest {
    AgentSupervisor {
        name: String,
        supervisor_agent: String,
        #[serde(default)]
        workers: Vec<String>,
        #[serde(default)]
        max_children: Option<usize>,
        #[serde(default)]
        metadata: Option<serde_json::Value>,
    },
    AgentInstance {
        agent: String,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        metadata: Option<serde_json::Value>,
    },
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
        .route("/api/runtime/registry", get(list_registry_entries))
        .route("/api/runtime/registry/message", post(send_registry_message))
        .route(
            "/api/runtime/registry/conversation",
            get(get_registry_conversation),
        )
        .route("/api/runtime/registry/stop", post(stop_registry_target))
        .route(
            "/api/runtime/registry/restart",
            post(restart_registry_target),
        )
        .route("/api/runtime/tree", get(get_runtime_tree))
        .route("/api/runtime/nodes/{id}", get(get_runtime_node))
        .route(
            "/api/runtime/nodes/{id}/children",
            post(spawn_runtime_child),
        )
        .route("/api/runtime/nodes/{id}/stop", post(stop_runtime_node))
        .route(
            "/api/runtime/nodes/{id}/restart",
            post(restart_runtime_node),
        )
        .route("/health", get(health_check))
        .route("/ws/conversations/{id}", get(ws_conversation))
        .route("/ws/runtime/events", get(ws_runtime_events))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn start_conversation(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<StartConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let conversation_id = format!("agent-instance:{}", uuid::Uuid::new_v4());
    let target =
        ensure_conversation_instance(&state, &conversation_id, Some(req.agent.as_str()), "http")
            .await?;
    let response = state
        .registry
        .send_message(&target, req.message)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ConversationResponse {
        conversation_id,
        agent: response.agent,
        response: response.response,
    }))
}

async fn continue_conversation(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ContinueConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let target = ensure_conversation_instance(&state, &id, None, "http").await?;
    let response = state
        .registry
        .send_message(&target, req.message)
        .await
        .map_err(|e| {
            let status = if matches!(
                e,
                crate::agents::registry::AgentRegistryError::EntryNotFound(_)
            ) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, e.to_string())
        })?;

    Ok(Json(ConversationResponse {
        conversation_id: id,
        agent: response.agent,
        response: response.response,
    }))
}

async fn get_conversation(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
) -> Result<Json<ConversationDetail>, (StatusCode, String)> {
    let target = resolve_conversation_target(&state, &id).await?;
    let conversation = state.registry.conversation(&target).await.map_err(|e| {
        let status = if matches!(
            e,
            crate::agents::registry::AgentRegistryError::EntryNotFound(_)
        ) {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, e.to_string())
    })?;

    let messages = serde_json::to_value(&conversation.messages)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ConversationDetail {
        conversation_id: conversation.id.clone(),
        agent: conversation.agent_name.clone(),
        message_count: conversation.messages.len(),
        archived_context: conversation.archived_context().map(str::to_owned),
        messages,
    }))
}

async fn list_agents(State(state): State<Arc<AgentServerState>>) -> Json<Vec<AgentInfo>> {
    let agents: Vec<AgentInfo> = state
        .agents
        .iter()
        .map(|(name, agent)| AgentInfo {
            name: name.clone(),
            model: agent.config.model.clone(),
            tools: agent
                .tool_definitions()
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
        })
        .collect();

    Json(agents)
}

async fn health_check(State(state): State<Arc<AgentServerState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        agents: state.agents.len(),
    })
}

fn ensure_runtime_admin_enabled(state: &AgentServerState) -> Result<(), (StatusCode, String)> {
    if !state.supervisor.config().admin_api {
        return Err((
            StatusCode::NOT_FOUND,
            "Runtime admin API is disabled".to_string(),
        ));
    }
    Ok(())
}

fn ensure_runtime_event_stream_enabled(
    state: &AgentServerState,
) -> Result<(), (StatusCode, String)> {
    let config = state.supervisor.config();
    if !(config.admin_api && config.event_stream) {
        return Err((
            StatusCode::NOT_FOUND,
            "Runtime event stream is disabled".to_string(),
        ));
    }
    Ok(())
}

fn validate_agent_exists(
    state: &AgentServerState,
    agent_name: &str,
) -> Result<(), (StatusCode, String)> {
    if !state.agents.contains_key(agent_name) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Agent '{}' is not configured", agent_name),
        ));
    }
    Ok(())
}

fn validate_supervisor_spawn_target(
    parent: &SupervisorNodeSnapshot,
    requested_agent: &str,
) -> Result<(), (StatusCode, String)> {
    if parent.kind == SupervisorNodeKind::AgentSupervisor
        && !parent.workers.is_empty()
        && !parent
            .workers
            .iter()
            .any(|worker| worker == requested_agent)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Agent '{}' is not permitted under supervisor '{}'",
                requested_agent, parent.display_name
            ),
        ));
    }
    Ok(())
}

fn registry_key_for_snapshot(snapshot: &SupervisorNodeSnapshot) -> Option<String> {
    match snapshot.kind {
        SupervisorNodeKind::AgentSupervisor => Some(format!("supervisor/{}", snapshot.node_id)),
        SupervisorNodeKind::AgentInstance => Some(format!("instance/{}", snapshot.node_id)),
        _ => None,
    }
}

fn registry_entry_for_snapshot(snapshot: &SupervisorNodeSnapshot) -> Option<AgentRegistryEntry> {
    match snapshot.kind {
        SupervisorNodeKind::AgentSupervisor => Some(
            AgentRegistryEntry::new(
                format!("supervisor/{}", snapshot.node_id),
                AgentRegistryKind::AgentSupervisor,
                snapshot
                    .supervisor_agent
                    .clone()
                    .unwrap_or_else(|| snapshot.display_name.clone()),
            )
            .with_supervisor_node_id(snapshot.node_id.clone())
            .with_metadata(serde_json::json!({
                "display_name": snapshot.display_name,
                "workers": snapshot.workers,
                "max_children": snapshot.max_children,
            })),
        ),
        SupervisorNodeKind::AgentInstance => Some(
            AgentRegistryEntry::new(
                format!("instance/{}", snapshot.node_id),
                AgentRegistryKind::AgentInstance,
                snapshot
                    .configured_agent
                    .clone()
                    .unwrap_or_else(|| snapshot.display_name.clone()),
            )
            .with_supervisor_node_id(snapshot.node_id.clone())
            .with_metadata(serde_json::json!({
                "display_name": snapshot.display_name,
                "state": snapshot.state,
                "parent_id": snapshot.parent_id,
            })),
        ),
        _ => None,
    }
}

async fn get_runtime_tree(
    State(state): State<Arc<AgentServerState>>,
) -> Result<Json<SupervisorTreeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    Ok(Json(state.supervisor.tree_snapshot()))
}

async fn list_registry_entries(
    State(state): State<Arc<AgentServerState>>,
    Query(query): Query<RegistryQuery>,
) -> Result<Json<Vec<AgentRegistryEntry>>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let entries = match query.prefix {
        Some(prefix) => state.registry.scan_prefix_async(&prefix).await,
        None => state.registry.list_entries().await,
    };
    Ok(Json(entries))
}

async fn send_registry_message(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<RegistryMessageRequest>,
) -> Result<Json<AgentMessageResponse>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let response = state
        .registry
        .send_message(&req.target, req.message)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(response))
}

async fn get_registry_conversation(
    State(state): State<Arc<AgentServerState>>,
    Query(query): Query<RegistryTargetQuery>,
) -> Result<Json<ConversationDetail>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let conversation = state
        .registry
        .conversation(&query.target)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let messages = serde_json::to_value(&conversation.messages)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(ConversationDetail {
        conversation_id: conversation.id.clone(),
        agent: conversation.agent_name.clone(),
        message_count: conversation.messages.len(),
        archived_context: conversation.archived_context().map(str::to_owned),
        messages,
    }))
}

async fn stop_registry_target(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<RegistryTargetQuery>,
) -> Result<Json<SupervisorNodeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let snapshot = state
        .registry
        .stop_target(&req.target)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(snapshot))
}

async fn restart_registry_target(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<RegistryTargetQuery>,
) -> Result<Json<SupervisorNodeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let snapshot = state
        .registry
        .restart_target(&req.target)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(snapshot))
}

async fn resolve_conversation_target(
    state: &AgentServerState,
    conversation_id: &str,
) -> Result<String, (StatusCode, String)> {
    let key = format!("instance/{conversation_id}");
    if state.registry.resolve_async(&key).await.is_some()
        || (state.postgres.is_some()
            && state.registry.conversation(&key).await.map(|_| ()).is_ok())
    {
        Ok(key)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("Conversation '{}' not found", conversation_id),
        ))
    }
}

async fn ensure_conversation_instance(
    state: &AgentServerState,
    conversation_id: &str,
    requested_agent: Option<&str>,
    transport: &str,
) -> Result<String, (StatusCode, String)> {
    let key = format!("instance/{conversation_id}");
    if state.registry.resolve_async(&key).await.is_some() {
        return Ok(key);
    }

    let agent_name = match requested_agent {
        Some(agent_name) => agent_name.to_string(),
        None => {
            let Some(postgres) = state.postgres.as_ref() else {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "No conversation found and no 'agent' field provided".to_string(),
                ));
            };
            let saved = load_conversation_snapshot(postgres, &key)
                .await
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                .ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        "No conversation found and no 'agent' field provided".to_string(),
                    )
                })?;
            saved.agent_name
        }
    };
    let agent = state.agents.get(&agent_name).cloned().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Agent '{}' not found", agent_name),
        )
    })?;
    let parent = ensure_named_agent_supervisor(&state.supervisor)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (snapshot, mailbox) = spawn_supervised_agent_mailbox(
        &state.supervisor,
        &parent.node_id,
        key.clone(),
        Some(conversation_id.to_string()),
        agent,
        state.llm_client.clone(),
        state.postgres.clone(),
        serde_json::json!({
            "display_name": agent_name,
            "ephemeral": true,
            "transport": transport,
            "owner_id": state.registry.owner_id(),
        }),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let entry = registry_entry_for_snapshot(&snapshot).ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to register conversation instance".to_string(),
        )
    })?;
    state
        .registry
        .register_mailbox(entry, mailbox, &state.supervisor);
    Ok(key)
}

async fn get_runtime_node(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SupervisorNodeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let node = state.supervisor.node_snapshot(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Runtime node '{}' not found", id),
        )
    })?;
    Ok(Json(node))
}

async fn spawn_runtime_child(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
    Json(req): Json<SpawnRuntimeChildRequest>,
) -> Result<Json<SupervisorNodeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let parent = state.supervisor.node_snapshot(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Runtime node '{}' not found", id),
        )
    })?;

    let snapshot = match req {
        SpawnRuntimeChildRequest::AgentSupervisor {
            name,
            supervisor_agent,
            workers,
            max_children,
            metadata,
        } => {
            validate_agent_exists(&state, &supervisor_agent)?;
            for worker in &workers {
                validate_agent_exists(&state, worker)?;
            }

            state
                .supervisor
                .spawn_agent_supervisor(
                    &id,
                    AgentSupervisorSpec {
                        name,
                        supervisor_agent,
                        workers,
                        boot: true,
                        max_children: max_children.unwrap_or(64),
                    },
                    metadata.unwrap_or(serde_json::Value::Null),
                )
                .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?
        }
        SpawnRuntimeChildRequest::AgentInstance {
            agent,
            display_name: _,
            metadata,
        } => {
            validate_agent_exists(&state, &agent)?;
            validate_supervisor_spawn_target(&parent, &agent)?;
            let agent_runtime = state.agents.get(&agent).cloned().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Agent '{}' is not configured", agent),
                )
            })?;
            let node_id = format!("agent-instance:{}", uuid::Uuid::new_v4());
            let registry_key = format!("instance/{node_id}");
            let (snapshot, mailbox) = spawn_supervised_agent_mailbox(
                &state.supervisor,
                &id,
                registry_key.clone(),
                Some(node_id),
                agent_runtime,
                state.llm_client.clone(),
                state.postgres.clone(),
                metadata.unwrap_or(serde_json::Value::Null),
            )
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
            let entry = registry_entry_for_snapshot(&snapshot).ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "agent instance snapshot could not be registered".to_string(),
                )
            })?;
            state
                .registry
                .register_mailbox(entry, mailbox, &state.supervisor);
            return Ok(Json(snapshot));
        }
    };

    if let Some(entry) = registry_entry_for_snapshot(&snapshot) {
        state
            .registry
            .register_supervisor_entry(entry, &state.supervisor);
    }

    Ok(Json(snapshot))
}

async fn stop_runtime_node(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SupervisorNodeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let prior = state.supervisor.node_snapshot(&id);
    let snapshot = state
        .supervisor
        .stop_node(&id)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    if let Some(prior) = prior {
        if let Some(key) = registry_key_for_snapshot(&prior) {
            state.registry.unregister(&key);
        }
    }
    Ok(Json(snapshot))
}

async fn restart_runtime_node(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SupervisorNodeSnapshot>, (StatusCode, String)> {
    ensure_runtime_admin_enabled(&state)?;
    let snapshot = state
        .supervisor
        .restart_node(&id)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    if let Some(entry) = registry_entry_for_snapshot(&snapshot) {
        match snapshot.kind {
            SupervisorNodeKind::AgentSupervisor => {
                state
                    .registry
                    .register_supervisor_entry(entry, &state.supervisor);
            }
            SupervisorNodeKind::AgentInstance => {
                if let Some(existing) = state
                    .registry
                    .get(&format!("instance/{}", snapshot.node_id))
                {
                    state.registry.register(existing);
                } else {
                    state.registry.register(entry);
                }
            }
            _ => {
                state.registry.register(entry);
            }
        };
    }
    Ok(Json(snapshot))
}

// ---------------------------------------------------------------------------
// WebSocket streaming handler
// ---------------------------------------------------------------------------

/// Upgrade handler: accepts the WebSocket upgrade request and hands off to
/// `handle_ws` once the connection is established.
async fn ws_conversation(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<AgentServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, id, state))
}

async fn ws_runtime_events(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AgentServerState>>,
) -> impl IntoResponse {
    match ensure_runtime_event_stream_enabled(&state) {
        Ok(()) => ws.on_upgrade(move |socket| handle_runtime_events_ws(socket, state)),
        Err((status, message)) => (status, message).into_response(),
    }
}

/// Helper: send a JSON value over the WebSocket, ignoring send errors (the
/// outer loop will notice the disconnect on the next recv).
async fn ws_send_json(socket: &mut WebSocket, value: serde_json::Value) {
    let _ = socket.send(WsMessage::Text(value.to_string().into())).await;
}

async fn handle_runtime_events_ws(mut socket: WebSocket, state: Arc<AgentServerState>) {
    let mut events = state.supervisor.subscribe();
    ws_send_json(
        &mut socket,
        serde_json::json!({
            "type": "snapshot",
            "tree": state.supervisor.tree_snapshot()
        }),
    )
    .await;

    loop {
        tokio::select! {
            socket_msg = socket.recv() => {
                match socket_msg {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "event",
                                "event": event
                            }),
                        ).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "warning",
                                "message": format!("runtime event stream lagged by {} messages", skipped)
                            }),
                        ).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}
async fn handle_ws(mut socket: WebSocket, conv_id: String, state: Arc<AgentServerState>) {
    ws_send_json(
        &mut socket,
        serde_json::json!({
            "type": "ready",
            "conversation_id": conv_id
        }),
    )
    .await;

    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Close(_) => return,
            _ => continue,
        };

        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                ws_send_json(
                    &mut socket,
                    serde_json::json!({"type": "error", "message": e.to_string()}),
                )
                .await;
                continue;
            }
        };

        match parsed.get("type").and_then(|value| value.as_str()) {
            Some("conversation_snapshot") => {
                match state
                    .registry
                    .conversation(&format!("instance/{}", conv_id))
                    .await
                {
                    Ok(conversation) => {
                        let messages = match serde_json::to_value(&conversation.messages) {
                            Ok(messages) => messages,
                            Err(error) => {
                                ws_send_json(
                                    &mut socket,
                                    serde_json::json!({"type": "error", "message": error.to_string()}),
                                )
                                .await;
                                continue;
                            }
                        };
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "conversation",
                                "conversation_id": conversation.id,
                                "agent": conversation.agent_name,
                                "message_count": conversation.messages.len(),
                                "archived_context": conversation.archived_context(),
                                "messages": messages
                            }),
                        )
                        .await;
                    }
                    Err(error) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({"type": "error", "message": error.to_string()}),
                        )
                        .await;
                    }
                }
            }
            _ => {
                let user_message = parsed
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if user_message.is_empty() {
                    ws_send_json(
                        &mut socket,
                        serde_json::json!({"type": "error", "message": "message is required"}),
                    )
                    .await;
                    continue;
                }

                let target = match ensure_conversation_instance(
                    &state,
                    &conv_id,
                    parsed.get("agent").and_then(|value| value.as_str()),
                    "websocket",
                )
                .await
                {
                    Ok(target) => target,
                    Err((status, message)) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "error",
                                "status": status.as_u16(),
                                "message": message
                            }),
                        )
                        .await;
                        continue;
                    }
                };

                match state.registry.send_message(&target, user_message).await {
                    Ok(response) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "response",
                                "conversation_id": conv_id,
                                "agent": response.agent,
                                "response": response.response,
                                "message_count": response.message_count
                            }),
                        )
                        .await;
                    }
                    Err(error) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({"type": "error", "message": error.to_string()}),
                        )
                        .await;
                    }
                }
            }
        }
    }
}
