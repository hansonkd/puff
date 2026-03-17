use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::agents::error::AgentError;
use crate::agents::provider::{create_provider, resolve_provider_for_model, Provider};
use crate::agents::streaming::SseParser;
use futures_util::StreamExt;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub stop_reason: StopReason,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Unknown(String),
}

#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, input_json: String },
    ToolCallEnd { id: String },
    Done {
        input_tokens: u32,
        output_tokens: u32,
        stop_reason: StopReason,
    },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub system: Option<String>,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            max_tokens: 4096,
            temperature: None,
            system: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub fallback: HashMap<String, Vec<String>>,
    pub cache: Option<LlmCacheConfig>,
    pub rate_limits: Option<HashMap<String, RateLimitConfig>>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_model: Some("claude-sonnet-4-6".to_string()),
            providers: HashMap::new(),
            fallback: HashMap::new(),
            cache: None,
            rate_limits: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCacheConfig {
    #[serde(default)]
    pub enabled: bool,
    pub backend: Option<String>,
    pub ttl: Option<u64>,
    pub strategy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub rpm: Option<u32>,
    pub tpm: Option<u32>,
}

// ---------------------------------------------------------------------------
// LlmClient
// ---------------------------------------------------------------------------

/// The central LLM client that manages providers and makes streaming calls
/// to LLM APIs.
pub struct LlmClient {
    http_client: reqwest::Client,
    providers: HashMap<String, Arc<dyn Provider>>,
    config: LlmConfig,
}

impl LlmClient {
    /// Create a new `LlmClient` from the given configuration.
    ///
    /// Initialises providers listed in `config.providers`. If the config has
    /// no explicit providers, sensible defaults (anthropic, openai) are
    /// created on-demand during requests.
    pub fn new(config: LlmConfig) -> Result<Self, AgentError> {
        let http_client = reqwest::Client::builder()
            .build()
            .map_err(|e| AgentError::LlmError(format!("failed to build HTTP client: {e}")))?;

        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();

        for (name, prov_config) in &config.providers {
            let provider = create_provider(name, prov_config)?;
            providers.insert(name.clone(), Arc::from(provider));
        }

        Ok(Self {
            http_client,
            providers,
            config,
        })
    }

    /// Resolve (or lazily create) the provider for the given model.
    fn resolve_provider(&self, model: &str) -> Result<Arc<dyn Provider>, AgentError> {
        let provider_name = resolve_provider_for_model(model);

        if let Some(p) = self.providers.get(provider_name) {
            return Ok(Arc::clone(p));
        }

        // Attempt to create the provider on the fly using defaults.
        let default_config = self
            .config
            .providers
            .get(provider_name)
            .cloned()
            .unwrap_or(ProviderConfig {
                api_key_env: None,
                base_url: None,
            });

        let provider = create_provider(provider_name, &default_config)?;
        Ok(Arc::from(provider))
    }

    /// Make a streaming LLM call. Returns an `mpsc::Receiver` that yields
    /// `StreamChunk` values as they arrive from the provider.
    ///
    /// A background `tokio` task is spawned to drive the HTTP request and
    /// parse the SSE stream; the caller consumes chunks from the channel.
    pub async fn stream(
        &self,
        request: LlmRequest,
    ) -> Result<mpsc::Receiver<StreamChunk>, AgentError> {
        let provider = self.resolve_provider(&request.model)?;
        let body = provider.build_request_body(&request);
        let url = provider.endpoint_url().to_string();
        let auth = provider.auth_headers();

        let mut req_builder = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json");

        for (key, value) in &auth {
            req_builder = req_builder.header(key, value);
        }

        let response = req_builder
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::LlmError(format!("HTTP request failed: {e}")))?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|secs| secs * 1000);

            return Err(AgentError::LlmRateLimited {
                provider: provider.name().to_string(),
                retry_after_ms: retry_after,
            });
        }

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "failed to read error body".to_string());
            return Err(AgentError::LlmError(format!(
                "HTTP {}: {}",
                status, error_body
            )));
        }

        let (tx, rx) = mpsc::channel::<StreamChunk>(256);

        // Clone the Arc'd provider into the spawned task.
        let provider = Arc::clone(&provider);

        tokio::spawn(async move {
            let mut parser = SseParser::new();
            let mut byte_stream = response.bytes_stream();

            while let Some(result) = byte_stream.next().await {
                match result {
                    Ok(bytes) => {
                        let events = parser.feed(&bytes);
                        for event in events {
                            let chunks = provider.parse_sse_event(&event);
                            for chunk in chunks {
                                if tx.send(chunk).await.is_err() {
                                    // Receiver dropped; stop processing.
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(StreamChunk::Error(format!("stream read error: {e}")))
                            .await;
                        return;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Make a blocking (non-streaming) LLM call that collects the full
    /// response. Internally uses `stream()` and accumulates all chunks.
    pub async fn chat(&self, request: LlmRequest) -> Result<LlmResponse, AgentError> {
        let model = request.model.clone();
        let mut rx = self.stream(request).await?;

        let mut text = String::new();
        let mut input_tokens: u32 = 0;
        let mut output_tokens: u32 = 0;
        let mut stop_reason = StopReason::Unknown("no_done_event".to_string());

        // Track in-flight tool calls.
        struct PendingToolCall {
            id: String,
            name: String,
            json_buf: String,
        }

        let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();
        let mut completed_tool_calls: Vec<ToolCall> = Vec::new();

        // Track the "current" tool-call index for providers (like Anthropic)
        // that don't include the id in delta/end events.
        let mut current_tool_idx: Option<usize> = None;

        while let Some(chunk) = rx.recv().await {
            match chunk {
                StreamChunk::TextDelta(delta) => {
                    text.push_str(&delta);
                }
                StreamChunk::ToolCallStart { id, name } => {
                    let idx = pending_tool_calls.len();
                    pending_tool_calls.push(PendingToolCall {
                        id: id.clone(),
                        name,
                        json_buf: String::new(),
                    });
                    current_tool_idx = Some(idx);
                }
                StreamChunk::ToolCallDelta { id, input_json } => {
                    // Find the tool call to append to. Prefer matching by id,
                    // fall back to current_tool_idx for Anthropic (empty id).
                    let target = if !id.is_empty() {
                        pending_tool_calls.iter_mut().find(|tc| tc.id == id)
                    } else {
                        current_tool_idx.and_then(|i| pending_tool_calls.get_mut(i))
                    };

                    if let Some(tc) = target {
                        tc.json_buf.push_str(&input_json);
                    }
                }
                StreamChunk::ToolCallEnd { id } => {
                    // Finalise the tool call. Match by id or use
                    // current_tool_idx.
                    let target_idx = if !id.is_empty() {
                        pending_tool_calls.iter().position(|tc| tc.id == id)
                    } else {
                        current_tool_idx
                    };

                    if let Some(idx) = target_idx {
                        let tc = pending_tool_calls.remove(idx);
                        let arguments: serde_json::Value =
                            serde_json::from_str(&tc.json_buf).unwrap_or(serde_json::Value::Null);
                        completed_tool_calls.push(ToolCall {
                            id: tc.id,
                            name: tc.name,
                            arguments,
                        });
                        // Reset current index.
                        current_tool_idx = None;
                    }
                }
                StreamChunk::Done {
                    input_tokens: it,
                    output_tokens: ot,
                    stop_reason: sr,
                } => {
                    if it > 0 {
                        input_tokens = it;
                    }
                    if ot > 0 {
                        output_tokens = ot;
                    }
                    stop_reason = sr;
                }
                StreamChunk::Error(e) => {
                    return Err(AgentError::LlmError(e));
                }
            }
        }

        // Flush any remaining pending tool calls (some providers may not
        // send explicit end events).
        for tc in pending_tool_calls {
            let arguments: serde_json::Value =
                serde_json::from_str(&tc.json_buf).unwrap_or(serde_json::Value::Null);
            completed_tool_calls.push(ToolCall {
                id: tc.id,
                name: tc.name,
                arguments,
            });
        }

        Ok(LlmResponse {
            text,
            tool_calls: completed_tool_calls,
            input_tokens,
            output_tokens,
            stop_reason,
            model,
        })
    }
}
