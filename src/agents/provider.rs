//! LLM provider implementations (Anthropic, OpenAI, Ollama).

use crate::agents::error::AgentError;
use crate::agents::llm::{
    ContentBlock, LlmRequestRef, Message, MessageContent, Role, StopReason, StreamChunk,
    ToolDefinition,
};
use crate::agents::streaming::SseEvent;
use serde_json::json;

use crate::agents::llm::ProviderConfig;

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// Normalizes differences between LLM provider APIs (Anthropic, OpenAI, etc.).
pub trait Provider: Send + Sync {
    /// Human-readable provider name (e.g. "anthropic", "openai").
    fn name(&self) -> &str;

    /// Build the JSON request body for this provider's API.
    fn build_request_body(&self, request: LlmRequestRef<'_>) -> serde_json::Value;

    /// The full URL to POST streaming chat requests to.
    fn endpoint_url(&self) -> &str;

    /// The full URL to POST embedding requests to.
    /// Returns `None` if this provider does not support embeddings.
    fn embeddings_url(&self) -> Option<String> {
        None
    }

    /// Returns headers required for authentication / versioning.
    fn auth_headers(&self) -> Vec<(String, String)>;

    /// Convert a single SSE event into zero or more `StreamChunk`s.
    fn parse_sse_event(&self, event: &SseEvent) -> Vec<StreamChunk>;
}

// ---------------------------------------------------------------------------
// Anthropic provider
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    api_key: String,
    #[allow(dead_code)]
    base_url: String,
    endpoint: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string());
        let endpoint = format!("{}/v1/messages", base);
        Self {
            api_key,
            base_url: base,
            endpoint,
        }
    }

    // -- helpers for building Anthropic-specific message bodies ---------------

    fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let role = match m.role {
                    Role::User | Role::Tool => "user",
                    Role::Assistant => "assistant",
                    // System messages are handled via the top-level `system` field.
                    Role::System => unreachable!(),
                };

                let content = match &m.content {
                    MessageContent::Text(t) => {
                        // Tool-result messages need special wrapping for Anthropic.
                        if m.role == Role::Tool {
                            if let Some(ref tool_call_id) = m.tool_call_id {
                                json!([{
                                    "type": "tool_result",
                                    "tool_use_id": tool_call_id,
                                    "content": t,
                                }])
                            } else {
                                json!(t)
                            }
                        } else {
                            json!(t)
                        }
                    }
                    MessageContent::Blocks(blocks) => {
                        let converted: Vec<serde_json::Value> = blocks
                            .iter()
                            .map(|b| match b {
                                ContentBlock::Text { text } => {
                                    json!({"type": "text", "text": text})
                                }
                                ContentBlock::ToolUse { id, name, input } => {
                                    json!({"type": "tool_use", "id": id, "name": name, "input": input})
                                }
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => {
                                    let mut v = json!({
                                        "type": "tool_result",
                                        "tool_use_id": tool_use_id,
                                        "content": content,
                                    });
                                    if let Some(err) = is_error {
                                        v["is_error"] = json!(err);
                                    }
                                    v
                                }
                            })
                            .collect();
                        json!(converted)
                    }
                };

                json!({
                    "role": role,
                    "content": content,
                })
            })
            .collect()
    }

    fn convert_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect()
    }
}

impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn build_request_body(&self, request: LlmRequestRef<'_>) -> serde_json::Value {
        let mut body = json!({
            "model": request.model.as_ref(),
            "max_tokens": request.max_tokens,
            "stream": true,
            "messages": Self::convert_messages(request.messages),
        });

        if let Some(system) = request.system.as_deref() {
            body["system"] = json!(system);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        if !request.tools.is_empty() {
            body["tools"] = json!(Self::convert_tools(request.tools));
        }

        body
    }

    fn endpoint_url(&self) -> &str {
        &self.endpoint
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]
    }

    fn parse_sse_event(&self, event: &SseEvent) -> Vec<StreamChunk> {
        let event_type = match &event.event_type {
            Some(t) => t.as_str(),
            None => return vec![],
        };

        match event_type {
            "content_block_start" => {
                let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                    return vec![];
                };
                if let Some(cb) = val.get("content_block") {
                    let block_type = cb.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if block_type == "tool_use" {
                        let id = cb
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = cb
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return vec![StreamChunk::ToolCallStart { id, name }];
                    }
                }
                vec![]
            }
            "content_block_delta" => {
                let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                    return vec![];
                };
                if let Some(delta) = val.get("delta") {
                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                return vec![StreamChunk::TextDelta(text.to_string())];
                            }
                        }
                        "input_json_delta" => {
                            if let Some(json_str) =
                                delta.get("partial_json").and_then(|v| v.as_str())
                            {
                                // Anthropic doesn't include the tool-call id in the delta,
                                // but callers track it from the preceding
                                // content_block_start. We pass an empty string here;
                                // the accumulator in `chat()` will reconcile.
                                return vec![StreamChunk::ToolCallDelta {
                                    id: String::new(),
                                    input_json: json_str.to_string(),
                                }];
                            }
                        }
                        _ => {}
                    }
                }
                vec![]
            }
            "content_block_stop" => {
                // Signal that the current content block (possibly a tool call) has ended.
                // id is not provided in this event; the caller tracks it.
                vec![StreamChunk::ToolCallEnd { id: String::new() }]
            }
            "message_delta" => {
                let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                    return vec![];
                };
                let stop_reason = val
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|v| v.as_str())
                    .map(anthropic_stop_reason)
                    .unwrap_or(StopReason::Unknown("none".to_string()));

                let usage = val.get("usage");
                let input_tokens = usage
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output_tokens = usage
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                vec![StreamChunk::Done {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                }]
            }
            "message_start" => {
                // Extract input token count from message_start if present.
                let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                    return vec![];
                };
                // We don't emit a chunk here; input_tokens will come with message_delta.
                let _ = val;
                vec![]
            }
            "ping" | "message_stop" => vec![],
            "error" => {
                vec![StreamChunk::Error(event.data.clone())]
            }
            _ => vec![],
        }
    }
}

fn anthropic_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Unknown(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible provider (also used for Ollama)
// ---------------------------------------------------------------------------

pub struct OpenAIProvider {
    api_key: Option<String>,
    #[allow(dead_code)]
    base_url: String,
    endpoint: String,
    provider_name: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| "https://api.openai.com".to_string());
        let endpoint = format!("{}/chat/completions", base.trim_end_matches('/'));
        Self {
            api_key: Some(api_key),
            base_url: base,
            endpoint,
            provider_name: "openai".to_string(),
        }
    }

    /// Create a provider that talks to a local Ollama instance (no auth).
    pub fn ollama(base_url: String) -> Self {
        let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        Self {
            api_key: None,
            base_url,
            endpoint,
            provider_name: "ollama".to_string(),
        }
    }

    fn convert_messages(request: LlmRequestRef<'_>) -> Vec<serde_json::Value> {
        let mut msgs: Vec<serde_json::Value> = Vec::new();

        // Inject system prompt as the first message.
        if let Some(system) = request.system.as_deref() {
            msgs.push(json!({"role": "system", "content": system}));
        }

        for m in request.messages {
            match m.role {
                Role::System => {
                    if let MessageContent::Text(ref t) = m.content {
                        msgs.push(json!({"role": "system", "content": t}));
                    }
                }
                Role::User => {
                    let content = Self::message_content_to_json(&m.content);
                    msgs.push(json!({"role": "user", "content": content}));
                }
                Role::Assistant => {
                    let mut msg = json!({"role": "assistant"});
                    match &m.content {
                        MessageContent::Text(t) => {
                            msg["content"] = json!(t);
                        }
                        MessageContent::Blocks(blocks) => {
                            let mut text_parts: Vec<String> = Vec::new();
                            let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                            for block in blocks {
                                match block {
                                    ContentBlock::Text { text } => {
                                        text_parts.push(text.clone());
                                    }
                                    ContentBlock::ToolUse { id, name, input } => {
                                        tool_calls.push(json!({
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": input.to_string(),
                                            }
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                            if !text_parts.is_empty() {
                                msg["content"] = json!(text_parts.join("\n"));
                            }
                            if !tool_calls.is_empty() {
                                msg["tool_calls"] = json!(tool_calls);
                            }
                        }
                    }
                    msgs.push(msg);
                }
                Role::Tool => {
                    let content = match &m.content {
                        MessageContent::Text(t) => t.clone(),
                        MessageContent::Blocks(blocks) => {
                            let mut parts = Vec::new();
                            for block in blocks {
                                if let ContentBlock::ToolResult { content, .. } = block {
                                    parts.push(content.clone());
                                }
                            }
                            parts.join("\n")
                        }
                    };
                    let mut msg = json!({
                        "role": "tool",
                        "content": content,
                    });
                    if let Some(ref id) = m.tool_call_id {
                        msg["tool_call_id"] = json!(id);
                    }
                    msgs.push(msg);
                }
            }
        }

        msgs
    }

    fn message_content_to_json(content: &MessageContent) -> serde_json::Value {
        match content {
            MessageContent::Text(t) => json!(t),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<String> = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
                json!(parts.join("\n"))
            }
        }
    }

    fn convert_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect()
    }
}

impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn embeddings_url(&self) -> Option<String> {
        Some(format!(
            "{}/embeddings",
            self.base_url.trim_end_matches('/')
        ))
    }

    fn build_request_body(&self, request: LlmRequestRef<'_>) -> serde_json::Value {
        let mut body = json!({
            "model": request.model.as_ref(),
            "stream": true,
            "messages": Self::convert_messages(request.clone()),
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        // OpenAI uses max_completion_tokens for newer models, but max_tokens
        // still works for most. Use the broader field name here.
        body["max_tokens"] = json!(request.max_tokens);

        if !request.tools.is_empty() {
            body["tools"] = json!(Self::convert_tools(request.tools));
        }

        // Request streaming usage info when supported.
        body["stream_options"] = json!({"include_usage": true});

        body
    }

    fn endpoint_url(&self) -> &str {
        &self.endpoint
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        match &self.api_key {
            Some(key) => vec![("Authorization".to_string(), format!("Bearer {}", key))],
            None => vec![],
        }
    }

    fn parse_sse_event(&self, event: &SseEvent) -> Vec<StreamChunk> {
        // OpenAI streaming uses `data:` lines without an `event:` field.
        if event.data == "[DONE]" {
            return vec![];
        }

        let Ok(val) = serde_json::from_str::<serde_json::Value>(&event.data) else {
            return vec![];
        };

        // Check for error objects.
        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return vec![StreamChunk::Error(msg.to_string())];
        }

        let mut chunks = Vec::new();

        // Handle usage-only chunk (OpenAI sends a final chunk with usage and
        // empty choices when stream_options.include_usage is set).
        if let Some(usage) = val.get("usage") {
            if usage.is_object() {
                let input_tokens = usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output_tokens = usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                // The stop reason may have arrived in an earlier chunk. We
                // still emit Done here with whatever we can glean.
                let stop_reason = val
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|v| v.as_str())
                    .map(openai_stop_reason)
                    .unwrap_or(StopReason::EndTurn);

                chunks.push(StreamChunk::Done {
                    input_tokens,
                    output_tokens,
                    stop_reason,
                });
            }
        }

        // Process choices.
        let Some(choices) = val.get("choices").and_then(|v| v.as_array()) else {
            return chunks;
        };

        for choice in choices {
            // finish_reason
            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                // If we haven't already emitted Done via usage, emit one now
                // with zero token counts (they'll be filled later via
                // accumulation in `chat()`).
                if !chunks.iter().any(|c| matches!(c, StreamChunk::Done { .. })) {
                    chunks.push(StreamChunk::Done {
                        input_tokens: 0,
                        output_tokens: 0,
                        stop_reason: openai_stop_reason(reason),
                    });
                }
            }

            let Some(delta) = choice.get("delta") else {
                continue;
            };

            // Text content
            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    chunks.push(StreamChunk::TextDelta(content.to_string()));
                }
            }

            // Tool calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    let id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    if let Some(func) = tc.get("function") {
                        // If there's a name, this is a tool call start.
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            chunks.push(StreamChunk::ToolCallStart {
                                id: id.clone(),
                                name: name.to_string(),
                            });
                        }
                        // If there are arguments, this is a delta.
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            if !args.is_empty() {
                                chunks.push(StreamChunk::ToolCallDelta {
                                    id: id.clone(),
                                    input_json: args.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        chunks
    }
}

fn openai_stop_reason(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "tool_calls" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        other => StopReason::Unknown(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Return the provider name for a given model string.
///
/// - `claude-*` -> `"anthropic"`
/// - `gpt-*`, `o1`, `o3` -> `"openai"`
/// - models containing `/` -> `"ollama"` (e.g. `llama3/latest`)
pub fn resolve_provider_for_model(model: &str) -> &str {
    if model.starts_with("claude") {
        "anthropic"
    } else if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        "openai"
    } else if model.contains('/') {
        "ollama"
    } else {
        // Default to openai for unknown models (compatible API).
        "openai"
    }
}

/// Factory: create a boxed `Provider` from a provider name and its config.
pub fn create_provider(
    name: &str,
    config: &ProviderConfig,
) -> Result<Box<dyn Provider>, AgentError> {
    match name {
        "anthropic" => {
            let api_key = resolve_api_key(config, "ANTHROPIC_API_KEY")?;
            Ok(Box::new(AnthropicProvider::new(
                api_key,
                config.base_url.clone(),
            )))
        }
        "openai" => {
            let api_key = resolve_api_key(config, "OPENAI_API_KEY")?;
            Ok(Box::new(OpenAIProvider::new(
                api_key,
                config.base_url.clone(),
            )))
        }
        "ollama" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
            Ok(Box::new(OpenAIProvider::ollama(base_url)))
        }
        other => Err(AgentError::LlmProviderUnavailable(other.to_string())),
    }
}

/// Resolve an API key from the config or from an environment variable.
fn resolve_api_key(config: &ProviderConfig, default_env: &str) -> Result<String, AgentError> {
    let env_var = config.api_key_env.as_deref().unwrap_or(default_env);

    std::env::var(env_var).map_err(|_| {
        AgentError::ConfigError(format!(
            "API key environment variable '{}' is not set",
            env_var
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm::LlmRequest;

    #[test]
    fn test_resolve_provider_for_model() {
        assert_eq!(resolve_provider_for_model("claude-sonnet-4-6"), "anthropic");
        assert_eq!(resolve_provider_for_model("claude-3-opus"), "anthropic");
        assert_eq!(resolve_provider_for_model("gpt-4"), "openai");
        assert_eq!(resolve_provider_for_model("gpt-4o"), "openai");
        assert_eq!(resolve_provider_for_model("o1"), "openai");
        assert_eq!(resolve_provider_for_model("o3-mini"), "openai");
        assert_eq!(resolve_provider_for_model("llama3/latest"), "ollama");
        assert_eq!(resolve_provider_for_model("mistral"), "openai"); // default
    }

    #[test]
    fn test_anthropic_build_request_body() {
        let provider = AnthropicProvider::new("test-key".to_string(), None);
        let request = LlmRequest::new(
            "claude-sonnet-4-6",
            vec![Message {
                role: Role::User,
                content: MessageContent::Text("hello".to_string()),
                tool_call_id: None,
            }],
        )
        .with_system("You are helpful.");

        let body = provider.build_request_body(request.borrowed());
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["stream"], true);
        assert!(body["messages"].is_array());
    }

    #[test]
    fn test_openai_build_request_body() {
        let provider = OpenAIProvider::new("test-key".to_string(), None);
        let request = LlmRequest::new(
            "gpt-4",
            vec![Message {
                role: Role::User,
                content: MessageContent::Text("hello".to_string()),
                tool_call_id: None,
            }],
        )
        .with_system("You are helpful.");

        let body = provider.build_request_body(request.borrowed());
        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["stream"], true);
        // System should be first message
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn test_anthropic_auth_headers() {
        let provider = AnthropicProvider::new("sk-test-123".to_string(), None);
        let headers = provider.auth_headers();
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers[0],
            ("x-api-key".to_string(), "sk-test-123".to_string())
        );
        assert_eq!(
            headers[1],
            ("anthropic-version".to_string(), "2023-06-01".to_string())
        );
    }

    #[test]
    fn test_openai_auth_headers() {
        let provider = OpenAIProvider::new("sk-test-456".to_string(), None);
        let headers = provider.auth_headers();
        assert_eq!(headers.len(), 1);
        assert_eq!(
            headers[0],
            (
                "Authorization".to_string(),
                "Bearer sk-test-456".to_string()
            )
        );
    }

    #[test]
    fn test_ollama_no_auth() {
        let provider = OpenAIProvider::ollama("http://localhost:11434/v1".to_string());
        assert_eq!(provider.name(), "ollama");
        assert!(provider.auth_headers().is_empty());
    }

    #[test]
    fn test_anthropic_parse_text_delta() {
        let provider = AnthropicProvider::new("key".to_string(), None);
        let event = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#.to_string(),
        };
        let chunks = provider.parse_sse_event(&event);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], StreamChunk::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn test_anthropic_parse_tool_use_start() {
        let provider = AnthropicProvider::new("key".to_string(), None);
        let event = SseEvent {
            event_type: Some("content_block_start".to_string()),
            data: r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_123","name":"get_weather"}}"#.to_string(),
        };
        let chunks = provider.parse_sse_event(&event);
        assert_eq!(chunks.len(), 1);
        assert!(
            matches!(&chunks[0], StreamChunk::ToolCallStart { id, name } if id == "toolu_123" && name == "get_weather")
        );
    }

    #[test]
    fn test_openai_parse_text_delta() {
        let provider = OpenAIProvider::new("key".to_string(), None);
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"index":0,"delta":{"content":"Hi"}}]}"#.to_string(),
        };
        let chunks = provider.parse_sse_event(&event);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], StreamChunk::TextDelta(t) if t == "Hi"));
    }

    #[test]
    fn test_openai_parse_done_signal() {
        let provider = OpenAIProvider::new("key".to_string(), None);
        let event = SseEvent {
            event_type: None,
            data: "[DONE]".to_string(),
        };
        let chunks = provider.parse_sse_event(&event);
        assert!(chunks.is_empty());
    }
}
