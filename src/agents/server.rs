//! Agent HTTP server — REST and WebSocket endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::agents::agent::Agent;
use crate::agents::conversation::Conversation;
use crate::agents::llm::{
    ContentBlock, LlmClient, Message, MessageContent, Role, StreamChunk, ToolCall,
};
use crate::agents::memory;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

pub(crate) struct SharedConversation {
    turn_lock: Mutex<()>,
    state: Mutex<Conversation>,
}

impl SharedConversation {
    fn new(conversation: Conversation) -> Self {
        Self {
            turn_lock: Mutex::new(()),
            state: Mutex::new(conversation),
        }
    }
}

pub struct AgentServerState {
    pub(crate) agents: HashMap<String, Agent>,
    pub(crate) conversations: RwLock<HashMap<String, Arc<SharedConversation>>>,
    pub(crate) llm_client: LlmClient,
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
        .route("/ws/conversations/{id}", get(ws_conversation))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn start_conversation(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<StartConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let agent = state.agents.get(&req.agent).ok_or_else(|| {
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

    prune_stale_conversations(&state).await;
    state.conversations.write().await.insert(
        conversation_id.clone(),
        Arc::new(SharedConversation::new(conversation)),
    );

    Ok(Json(ConversationResponse {
        conversation_id,
        agent: agent_name,
        response: response.text,
    }))
}

async fn continue_conversation(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
    Json(req): Json<ContinueConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let conversation = state
        .conversations
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Conversation '{}' not found", id),
            )
        })?;

    let _turn_guard = conversation.turn_lock.lock().await;
    let mut conversation = conversation.state.lock().await;

    let agent = state.agents.get(&conversation.agent_name).ok_or_else(|| {
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
        .run_turn(&mut conversation, &state.llm_client)
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
    let conversation = state
        .conversations
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Conversation '{}' not found", id),
            )
        })?;
    let conversation = conversation.state.lock().await;

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

/// Human-in-the-loop approval timeout (seconds).
const APPROVAL_TIMEOUT_SECS: u64 = 3600;
const CONVERSATION_IDLE_TTL_HOURS: i64 = 24;

/// Helper: send a JSON value over the WebSocket, ignoring send errors (the
/// outer loop will notice the disconnect on the next recv).
async fn ws_send_json(socket: &mut WebSocket, value: serde_json::Value) {
    let _ = socket.send(WsMessage::Text(value.to_string().into())).await;
}

async fn prune_stale_conversations(state: &AgentServerState) {
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(CONVERSATION_IDLE_TTL_HOURS);
    let mut conversations = state.conversations.write().await;
    conversations.retain(|_, conversation| {
        conversation
            .state
            .try_lock()
            .map(|guard| guard.updated_at >= cutoff)
            .unwrap_or(true)
    });
}

/// State for accumulating an in-flight tool call from streaming chunks.
struct PendingToolCall {
    id: String,
    name: String,
    json_buf: String,
}

/// Core WebSocket loop.  For each text message received from the client the
/// server:
///
/// 1. Parses the JSON payload for `message` and (optional) `agent` fields.
/// 2. Looks up or creates the conversation identified by `conv_id`.
/// 3. Builds an LLM request and calls `llm_client.stream()`.
/// 4. Forwards every `StreamChunk` to the client immediately as a JSON
///    WebSocket message.
/// 5. When the LLM requests tool calls, executes them and loops back
///    to call the LLM again with the results.
/// 6. When a tool has `requires_approval = true`, sends a
///    `human_approval_needed` event and waits (up to 1 h) for the client's
///    `human_approval_response` before executing or skipping the tool.
async fn handle_ws(mut socket: WebSocket, conv_id: String, state: Arc<AgentServerState>) {
    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Close(_) => return,
            // Ignore binary / ping / pong frames.
            _ => continue,
        };

        // --- Parse client message -------------------------------------------
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

        // Handle human_approval_response messages (unexpected outside the
        // approval flow — just ignore them here).
        if parsed.get("type").and_then(|v| v.as_str()) == Some("human_approval_response") {
            continue;
        }

        let user_message = parsed["message"].as_str().unwrap_or("").to_string();
        let agent_name_opt = parsed["agent"].as_str().map(String::from);

        // --- Look up or create the conversation ------------------------------
        let existing_conversation = {
            let convs = state.conversations.read().await;
            convs.get(&conv_id).cloned()
        };

        let (conversation, agent_name) = if let Some(conversation) = existing_conversation {
            let agent_name = conversation.state.lock().await.agent_name.clone();
            (conversation, agent_name)
        } else {
            let agent_name = match agent_name_opt {
                Some(name) => name,
                None => {
                    ws_send_json(
                        &mut socket,
                        serde_json::json!({
                            "type": "error",
                            "message": "No conversation found and no 'agent' field provided"
                        }),
                    )
                    .await;
                    continue;
                }
            };

            if !state.agents.contains_key(&agent_name) {
                ws_send_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "error",
                        "message": format!("Agent '{}' not found", agent_name)
                    }),
                )
                .await;
                continue;
            }

            let conversation = Arc::new(SharedConversation::new(
                Conversation::new(&agent_name).with_id(conv_id.clone()),
            ));
            prune_stale_conversations(&state).await;
            state
                .conversations
                .write()
                .await
                .insert(conv_id.clone(), Arc::clone(&conversation));
            (conversation, agent_name)
        };

        if !state.agents.contains_key(&agent_name) {
            ws_send_json(
                &mut socket,
                serde_json::json!({
                    "type": "error",
                    "message": format!("Agent '{}' not found", agent_name)
                }),
            )
            .await;
            continue;
        }

        let agent = state.agents.get(&agent_name).unwrap();
        let _turn_guard = conversation.turn_lock.lock().await;
        {
            let mut conv = conversation.state.lock().await;
            conv.add_user_message(&user_message);
            agent.compact_conversation(&mut conv);
        }

        // --- Streaming agent turn -------------------------------------------
        // Run the full agentic loop (LLM call → tool execution → repeat) while
        // streaming every token/event back to the client in real-time.
        let max_rounds: usize = 10;

        'agent_loop: for _round in 0..max_rounds {
            if let Err(e) = agent.budget.check_llm_budget() {
                ws_send_json(
                    &mut socket,
                    serde_json::json!({"type": "error", "message": e.to_string()}),
                )
                .await;
                break 'agent_loop;
            }

            // Call the streaming API.
            let llm_request = {
                let mut conv = conversation.state.lock().await;
                agent.compact_conversation(&mut conv);
                agent.build_request(&conv)
            };
            let mut rx = match state.llm_client.stream(llm_request).await {
                Ok(r) => r,
                Err(e) => {
                    ws_send_json(
                        &mut socket,
                        serde_json::json!({"type": "error", "message": e.to_string()}),
                    )
                    .await;
                    break 'agent_loop;
                }
            };

            // Accumulate the full response text and tool calls so we can
            // update the conversation history after the stream ends.
            let mut response_text = String::new();
            let mut input_tokens: u32 = 0;
            let mut output_tokens: u32 = 0;
            let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();
            let mut completed_tool_calls: Vec<ToolCall> = Vec::new();
            let mut current_tool_idx: Option<usize> = None;
            let mut got_tool_use = false;

            // Forward each chunk to the WebSocket client.
            while let Some(chunk) = rx.recv().await {
                match chunk {
                    StreamChunk::TextDelta(ref delta) => {
                        response_text.push_str(delta);
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({"type": "text", "content": delta}),
                        )
                        .await;
                    }
                    StreamChunk::ToolCallStart { ref id, ref name } => {
                        got_tool_use = true;
                        let idx = pending_tool_calls.len();
                        pending_tool_calls.push(PendingToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            json_buf: String::new(),
                        });
                        current_tool_idx = Some(idx);
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "tool_call",
                                "name": name,
                                "status": "running"
                            }),
                        )
                        .await;
                    }
                    StreamChunk::ToolCallDelta {
                        ref id,
                        ref input_json,
                    } => {
                        let target = if !id.is_empty() {
                            pending_tool_calls.iter_mut().find(|tc| tc.id == *id)
                        } else {
                            current_tool_idx.and_then(|i| pending_tool_calls.get_mut(i))
                        };
                        if let Some(tc) = target {
                            tc.json_buf.push_str(input_json);
                        }
                    }
                    StreamChunk::ToolCallEnd { ref id } => {
                        let target_idx = if !id.is_empty() {
                            pending_tool_calls.iter().position(|tc| tc.id == *id)
                        } else {
                            current_tool_idx
                        };
                        if let Some(idx) = target_idx {
                            let tc = pending_tool_calls.remove(idx);
                            let arguments: serde_json::Value = serde_json::from_str(&tc.json_buf)
                                .unwrap_or(serde_json::Value::Null);
                            completed_tool_calls.push(ToolCall {
                                id: tc.id,
                                name: tc.name,
                                arguments,
                            });
                            current_tool_idx = None;
                        }
                    }
                    StreamChunk::Done {
                        input_tokens: it,
                        output_tokens: ot,
                        ..
                    } => {
                        input_tokens = it;
                        output_tokens = ot;

                        // Flush any remaining pending tool calls.
                        for tc in pending_tool_calls.drain(..) {
                            let arguments: serde_json::Value = serde_json::from_str(&tc.json_buf)
                                .unwrap_or(serde_json::Value::Null);
                            completed_tool_calls.push(ToolCall {
                                id: tc.id,
                                name: tc.name,
                                arguments,
                            });
                        }
                    }
                    StreamChunk::Error(ref e) => {
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({"type": "error", "message": e}),
                        )
                        .await;
                        break 'agent_loop;
                    }
                }
            }

            agent.budget.record_llm_usage(
                input_tokens as u64,
                output_tokens as u64,
                memory::estimate_cost(&agent.config.model, input_tokens, output_tokens),
            );

            // --- Update conversation history ---------------------------------
            if !got_tool_use {
                // Final text response — add assistant message and finish.
                if !response_text.is_empty() {
                    let mut conv = conversation.state.lock().await;
                    conv.add_assistant_message(&response_text);
                    agent.compact_conversation(&mut conv);
                }
                ws_send_json(
                    &mut socket,
                    serde_json::json!({"type": "done", "conversation_id": conv_id}),
                )
                .await;
                break 'agent_loop;
            }

            // The LLM made tool calls.  Add the assistant message with
            // ToolUse blocks and then process each tool call.
            {
                let mut conv = conversation.state.lock().await;
                let mut blocks: Vec<ContentBlock> = Vec::new();
                if !response_text.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: response_text.clone(),
                    });
                }
                for tc in &completed_tool_calls {
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
                agent.compact_conversation(&mut conv);
            }

            // Execute each tool call, handling human-in-the-loop when needed.
            for tc in &completed_tool_calls {
                let prepared_tool = match agent.prepare_tool_call(&tc.name) {
                    Ok(tool) => tool,
                    Err(error) => {
                        let result_content = error.to_string();
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "tool_result",
                                "name": tc.name,
                                "result": result_content.clone()
                            }),
                        )
                        .await;

                        let mut conv = conversation.state.lock().await;
                        conv.add_message(Message {
                            role: Role::User,
                            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: result_content,
                                is_error: Some(true),
                            }]),
                            tool_call_id: Some(tc.id.clone()),
                        });
                        agent.compact_conversation(&mut conv);
                        continue;
                    }
                };

                if prepared_tool.requires_approval {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    ws_send_json(
                        &mut socket,
                        serde_json::json!({
                            "type": "human_approval_needed",
                            "action": tc.name,
                            "details": tc.arguments.to_string(),
                            "request_id": request_id
                        }),
                    )
                    .await;

                    // Wait for the client's approval response with a timeout.
                    let approved = 'approval: loop {
                        let timeout = tokio::time::timeout(
                            std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS),
                            socket.recv(),
                        )
                        .await;

                        match timeout {
                            Err(_elapsed) => {
                                // Timed out — treat as rejection.
                                ws_send_json(
                                    &mut socket,
                                    serde_json::json!({
                                        "type": "error",
                                        "message": "Human approval timed out"
                                    }),
                                )
                                .await;
                                break 'approval false;
                            }
                            Ok(None) | Ok(Some(Err(_))) => {
                                // Socket closed.
                                return;
                            }
                            Ok(Some(Ok(WsMessage::Close(_)))) => {
                                return;
                            }
                            Ok(Some(Ok(WsMessage::Text(t)))) => {
                                let resp: serde_json::Value =
                                    serde_json::from_str(&t.to_string()).unwrap_or_default();
                                if resp.get("type").and_then(|v| v.as_str())
                                    == Some("human_approval_response")
                                    && resp.get("request_id").and_then(|v| v.as_str())
                                        == Some(request_id.as_str())
                                {
                                    break 'approval resp
                                        .get("approved")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                }
                                // Wrong message type — keep waiting.
                            }
                            Ok(Some(Ok(_))) => {
                                // Binary/ping/pong — keep waiting.
                            }
                        }
                    };

                    if !approved {
                        // Record a "skipped" tool result and continue with
                        // the next tool.
                        {
                            let mut conv = conversation.state.lock().await;
                            conv.add_message(Message {
                                role: Role::User,
                                content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: "Tool execution rejected by user".to_string(),
                                    is_error: Some(true),
                                }]),
                                tool_call_id: Some(tc.id.clone()),
                            });
                            agent.compact_conversation(&mut conv);
                        }
                        ws_send_json(
                            &mut socket,
                            serde_json::json!({
                                "type": "tool_result",
                                "name": tc.name,
                                "result": "rejected"
                            }),
                        )
                        .await;
                        continue;
                    }
                }

                let (result_content, is_error) =
                    match agent.execute_prepared_tool(&prepared_tool, tc).await {
                        Ok(content) => (content, None),
                        Err(error) => (error.to_string(), Some(true)),
                    };

                ws_send_json(
                    &mut socket,
                    serde_json::json!({
                        "type": "tool_result",
                        "name": tc.name,
                        "result": result_content.clone()
                    }),
                )
                .await;

                // Add the tool result to the conversation.
                {
                    let mut conv = conversation.state.lock().await;
                    conv.add_message(Message {
                        role: Role::User,
                        content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: result_content,
                            is_error,
                        }]),
                        tool_call_id: Some(tc.id.clone()),
                    });
                    agent.compact_conversation(&mut conv);
                }
            }

            // Loop back to call the LLM again with the tool results.
        } // end agent_loop
    } // end socket recv loop
}
