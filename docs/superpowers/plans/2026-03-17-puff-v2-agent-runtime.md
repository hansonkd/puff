# Puff v2: Deep Stack Agent Runtime — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reimagine Puff as a deep-stack agent runtime — free-threaded Python on Tokio, with built-in LLM gateway, skills/CLI tools, three-tier memory, multi-agent orchestration, and agent server.

**Architecture:** Remove greenlet executor, replace with free-threaded Python threads. Add `src/agents/` module tree with LLM gateway, tool registry, memory system, orchestration patterns, and agent server. All I/O goes through Rust/Tokio; Python threads block on channels. Agent server exposes REST/SSE + WebSocket + GraphQL on Axum.

**Tech Stack:** Rust (Tokio, Axum, reqwest, PyO3 0.24, bb8, serde), Python 3.13+ (free-threaded), Postgres (pgvector), Redis.

**Spec:** `docs/superpowers/specs/2026-03-17-puff-agentic-runtime-design.md`

---

## File Structure

### New Files

| File | Responsibility |
|---|---|
| `src/agents/mod.rs` | Module root, re-exports Agent, Tool, Memory, Router, Supervisor, Chain, Parallel |
| `src/agents/error.rs` | `AgentError` enum — LLM errors, tool errors, memory errors, orchestration errors |
| `src/agents/llm.rs` | `LlmClient` — multi-provider, streaming SSE parsing, token counting, cost tracking |
| `src/agents/provider.rs` | `Provider` trait + `AnthropicProvider`, `OpenAIProvider`, `OllamaProvider` implementations |
| `src/agents/tool.rs` | `ToolRegistry`, JSON schema generation, parallel execution (`ToolDefinition` is defined in `llm.rs` and re-exported) |
| `src/agents/skill.rs` | `Skill` struct, TOML loading, CLI command whitelisting, permission enforcement |
| `src/agents/memory.rs` | `Memory` — conversation tier (Redis), long-term tier (Postgres+pgvector), auto-extraction |
| `src/agents/conversation.rs` | `Conversation` struct — message history, token tracking, context assembly |
| `src/agents/agent.rs` | `Agent` struct — config, execution loop, tool dispatch |
| `src/agents/orchestration.rs` | `Router`, `Supervisor`, `Chain`, `Parallel` — multi-agent patterns |
| `src/agents/streaming.rs` | `SseParser` — parse SSE streams from LLM providers, stream forking |
| `src/agents/trace.rs` | `Trace`, `TraceEvent` — structured observability, Postgres storage |
| `src/agents/eval.rs` | `EvalSuite`, `EvalCase` — evaluation framework with semantic assertions |
| `src/agents/server.rs` | Agent HTTP endpoints — REST/SSE conversations, WebSocket streaming |
| `src/agents/python_bindings.rs` | PyO3 classes exposed to Python — `PyAgent`, `PyTool`, `PyMemory`, etc. |
| `examples/agent_basic.rs` | Minimal agent example (Rust API) |
| `examples/agent_skills/` | Example skill directory with `skill.toml` + `context.md` |
| `examples/python_tests/test_agents.py` | Python integration tests for agent features |

### Modified Files

| File | Changes |
|---|---|
| `Cargo.toml` | Add `pgvector`, `uuid`, `async-stream` dependencies |
| `src/lib.rs` | Add `pub mod agents;` |
| `src/main.rs:24-49` | Add agent config fields (`llm`, `memory`, `agents`, `skills`) to Config struct |
| `src/main.rs:625-705` | Add agent CLI commands, agent server route building |
| `src/runtime/mod.rs:149-165` | Add `LlmConfig`, `MemoryConfig`, `AgentConfig` to RuntimeConfig |
| `src/python/mod.rs:122-134` | Remove `PyDispatchGreenlet` (keep `PyDispatchAsyncIO`, `PyDispatchAsyncCoro`) |
| `src/python/mod.rs:207-253` | Update bootstrap to register agent Python globals instead of greenlet dispatch |
| `src/python/mod.rs:309-353` | Remove greenlet thread creation from `PythonDispatcher::new()` |
| `src/python/mod.rs:409-432` | Remove `dispatch_greenlet()` method |
| `src/databases/postgres.rs` | Add pgvector extension check and memory table creation |
| `src/web/server.rs` | No structural changes — agent routes added via Router in main.rs |
| `src/program/mod.rs` | Add `AgentServeCommand`, `AgentAskCommand`, `AgentListCommand`, `AgentBenchCommand` |

---

## Chunk 1: Foundation — Module Structure & Greenlet Removal

### Task 1.1: Add New Dependencies to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add new dependencies**

Add to `[dependencies]` section of `Cargo.toml`:

```toml
uuid = { version = "1", features = ["v4", "serde"] }
futures-util = "0.3"
```

Also add `"stream"` to the existing `reqwest` features list (needed for `response.bytes_stream()`):

```toml
reqwest = { version = "0.12", features = ["json", "cookies", "multipart", "brotli", "gzip", "stream"] }
```

**Note:** `chrono` is already a dependency — do not re-add it. `pgvector` is a Postgres extension (no Rust crate needed — we use raw SQL with `VECTOR` type).

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles with no errors.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat: add uuid, async-stream, futures-util dependencies for agent runtime"
```

---

### Task 1.2: Create agents Module Skeleton

**Files:**
- Create: `src/agents/mod.rs`
- Create: `src/agents/error.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write agent error types**

Create `src/agents/error.rs`:

```rust
use std::fmt;

#[derive(Debug)]
pub enum AgentError {
    LlmError(String),
    LlmProviderUnavailable(String),
    LlmRateLimited { provider: String, retry_after_ms: Option<u64> },
    LlmStreamInterrupted(String),
    ToolExecutionError { tool: String, message: String },
    ToolNotFound(String),
    ToolPermissionDenied { tool: String, reason: String },
    ToolTimeout { tool: String, timeout_ms: u64 },
    MemoryError(String),
    ConversationNotFound(String),
    OrchestrationError(String),
    SkillLoadError { skill: String, message: String },
    ConfigError(String),
    PythonError(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LlmError(msg) => write!(f, "LLM error: {}", msg),
            Self::LlmProviderUnavailable(p) => write!(f, "LLM provider unavailable: {}", p),
            Self::LlmRateLimited { provider, retry_after_ms } => {
                write!(f, "LLM rate limited by {}", provider)?;
                if let Some(ms) = retry_after_ms {
                    write!(f, " (retry after {}ms)", ms)?;
                }
                Ok(())
            }
            Self::LlmStreamInterrupted(msg) => write!(f, "LLM stream interrupted: {}", msg),
            Self::ToolExecutionError { tool, message } => write!(f, "Tool '{}' failed: {}", tool, message),
            Self::ToolNotFound(t) => write!(f, "Tool not found: {}", t),
            Self::ToolPermissionDenied { tool, reason } => write!(f, "Tool '{}' denied: {}", tool, reason),
            Self::ToolTimeout { tool, timeout_ms } => write!(f, "Tool '{}' timed out after {}ms", tool, timeout_ms),
            Self::MemoryError(msg) => write!(f, "Memory error: {}", msg),
            Self::ConversationNotFound(id) => write!(f, "Conversation not found: {}", id),
            Self::OrchestrationError(msg) => write!(f, "Orchestration error: {}", msg),
            Self::SkillLoadError { skill, message } => write!(f, "Skill '{}' load failed: {}", skill, message),
            Self::ConfigError(msg) => write!(f, "Config error: {}", msg),
            Self::PythonError(msg) => write!(f, "Python error: {}", msg),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<AgentError> for crate::errors::Error {
    fn from(e: AgentError) -> Self {
        anyhow::anyhow!(e.to_string())
    }
}
```

- [ ] **Step 2: Write agents module root**

Create `src/agents/mod.rs`:

```rust
pub mod error;

pub use error::AgentError;

pub type AgentResult<T> = Result<T, AgentError>;
```

- [ ] **Step 3: Register module in lib.rs and fix deny(warnings)**

Add `pub mod agents;` to `src/lib.rs` after the existing module declarations.

Also change `#![deny(warnings)]` to `#![warn(warnings)]` on line 5 of `src/lib.rs`. The existing `deny(warnings)` turns all warnings into hard errors — new modules with in-progress code will have unused imports/variables that would block compilation. Switch to `warn` during development; restore `deny` before release.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add src/agents/ src/lib.rs
git commit -m "feat: add agents module skeleton with error types"
```

---

### Task 1.3: Remove Greenlet Code from Python Module

**Files:**
- Modify: `src/python/mod.rs:122-134` (remove `PyDispatchGreenlet`)
- Modify: `src/python/mod.rs:309-353` (remove greenlet thread creation)
- Modify: `src/python/mod.rs:409-432` (remove `dispatch_greenlet`)
- Modify: `src/python/mod.rs:207-253` (remove greenlet bootstrap)
- Modify: `src/runtime/mod.rs:149-165` (remove `greenlets` field)
- Modify: `src/main.rs:24-49` (remove `greenlets` config field)

**Important:** Do NOT remove `src/python/async_python.rs` — it contains `AsyncReturn` and `run_python_async()` which are the Tokio-to-Python bridge. These are needed regardless of greenlet removal.

- [ ] **Step 1: Read the current files to understand exact line ranges**

Read `src/python/mod.rs`, `src/runtime/mod.rs`, and `src/main.rs` to identify the exact greenlet code locations. The line numbers below are approximate from the initial exploration and may have shifted.

- [ ] **Step 2: Remove `PyDispatchGreenlet` class from `src/python/mod.rs`**

Remove the `#[pyclass]` struct and its `#[pymethods]` impl for `PyDispatchGreenlet` (~lines 122-134). This is the greenlet dispatch callable exposed to Python.

- [ ] **Step 3: Remove greenlet thread creation from `PythonDispatcher::new()`**

In `PythonDispatcher::new()` (~lines 309-353), remove the block that creates the greenlet thread (`puff.start_event_loop()`). Keep the asyncio thread creation block.

- [ ] **Step 4: Remove `dispatch_greenlet()` method from `PythonDispatcher`**

Remove the `dispatch_greenlet()` method (~lines 409-432). Keep `dispatch_asyncio()` and `dispatch_asyncio_coro()`.

- [ ] **Step 5: Update `bootstrap_puff_globals()` to remove greenlet registration**

In `bootstrap_puff_globals()` (~lines 207-253), remove the line that sets `dispatch_greenlet` on the puff module. Keep all other registrations (redis, postgres, pubsub, gql, task_queue, http_client, json functions, etc.).

- [ ] **Step 6: Remove `greenlets` field from `RuntimeConfig`**

In `src/runtime/mod.rs`, remove the `greenlets: bool` field from `RuntimeConfig` struct (~line 155) and the `set_greenlets()` method (~lines 366-370). Update the `Default` impl if it references greenlets.

- [ ] **Step 7: Remove `greenlets` from main.rs Config**

In `src/main.rs`, remove the `greenlets` field from the Config struct (~line 25) and any code that reads it to call `set_greenlets()`.

- [ ] **Step 8: Remove `thread_obj` from `PythonDispatcher` struct**

The `thread_obj` field (~line 285) holds the greenlet thread reference. Remove it. Keep `asyncio_obj`.

- [ ] **Step 9: Verify it compiles**

Run: `cargo check 2>&1 | tail -20`
Expected: May have some warnings about unused imports. Fix those. No errors.

- [ ] **Step 10: Run existing tests to check for regressions**

Run: `cargo test 2>&1 | tail -20`
Expected: All existing tests pass (there are minimal tests in the codebase).

- [ ] **Step 11: Commit**

```bash
git add src/python/mod.rs src/runtime/mod.rs src/main.rs
git commit -m "refactor: remove greenlet executor, prepare for free-threaded Python

Removes PyDispatchGreenlet, greenlet thread creation, dispatch_greenlet()
method, and greenlets config. Preserves async_python.rs (AsyncReturn,
run_python_async) as the Tokio-to-Python bridge mechanism.

BREAKING: Python greenlet-based dispatch is no longer available.
Free-threaded Python (3.13t+) threads replace greenlets for parallelism."
```

---

## Chunk 2: LLM Gateway — Multi-Provider Rust Client

### Task 2.1: SSE Stream Parser

**Files:**
- Create: `src/agents/streaming.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Write tests for SSE parsing**

Add tests at the bottom of `src/agents/streaming.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_data_line() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: {\"type\":\"content\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"type\":\"content\"}");
    }

    #[test]
    fn test_parse_sse_multi_line() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: hello\ndata: world\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello\nworld");
    }

    #[test]
    fn test_parse_sse_with_event_type() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: message_start\ndata: {}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type.as_deref(), Some("message_start"));
    }

    #[test]
    fn test_parse_sse_partial_then_complete() {
        let mut parser = SseParser::new();
        let events1 = parser.feed(b"data: hel");
        assert!(events1.is_empty());
        let events2 = parser.feed(b"lo\n\n");
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].data, "hello");
    }

    #[test]
    fn test_parse_sse_done_signal() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: [DONE]\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "[DONE]");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib agents::streaming 2>&1 | tail -10`
Expected: Compilation error — `SseParser` doesn't exist yet.

- [ ] **Step 3: Implement SSE parser**

Write `src/agents/streaming.rs`:

```rust
/// Server-Sent Events parser for LLM streaming responses.
/// Handles partial chunks, multi-line data, and event types.

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feed raw bytes from the HTTP stream. Returns any complete SSE events.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        loop {
            // Look for double newline (event boundary)
            let boundary = find_event_boundary(&self.buffer);
            if let Some(end) = boundary {
                let event_bytes = &self.buffer[..end];
                if let Some(event) = parse_single_event(event_bytes) {
                    events.push(event);
                }
                // Skip past the double newline
                let skip = if end + 2 <= self.buffer.len() && self.buffer[end..].starts_with(b"\n\n") {
                    end + 2
                } else if end + 4 <= self.buffer.len() && self.buffer[end..].starts_with(b"\r\n\r\n") {
                    end + 4
                } else {
                    end + 2
                };
                self.buffer = self.buffer[skip..].to_vec();
            } else {
                break;
            }
        }

        events
    }
}

fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

fn parse_single_event(bytes: &[u8]) -> Option<SseEvent> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut event_type = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event: ").or_else(|| line.strip_prefix("event:")) {
            event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
            data_lines.push(value);
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    Some(SseEvent {
        event_type,
        data: data_lines.join("\n"),
    })
}
```

- [ ] **Step 4: Add module to agents/mod.rs**

Add `pub mod streaming;` to `src/agents/mod.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib agents::streaming 2>&1 | tail -10`
Expected: All 5 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/agents/streaming.rs src/agents/mod.rs
git commit -m "feat: add SSE stream parser for LLM provider responses"
```

---

### Task 2.2: LLM Types — Messages, Responses, Tool Calls

**Files:**
- Create: `src/agents/llm.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Define core LLM types**

Create `src/agents/llm.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Role in a conversation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Message content — text or structured blocks
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

/// A tool definition sent to the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Parsed tool call from LLM response
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A complete LLM response
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

/// Streaming chunk from LLM
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

/// LLM request configuration
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

/// Configuration for an LLM provider
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
}

/// Top-level LLM configuration
#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub fallback: HashMap<String, Vec<String>>,
    pub cache: Option<LlmCacheConfig>,
    pub rate_limits: Option<HashMap<String, RateLimitConfig>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmCacheConfig {
    #[serde(default)]
    pub enabled: bool,
    pub backend: Option<String>,
    pub ttl: Option<u64>,
    pub strategy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    pub rpm: Option<u32>,
    pub tpm: Option<u32>,
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
```

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod llm;` to `src/agents/mod.rs`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles with no errors.

- [ ] **Step 4: Commit**

```bash
git add src/agents/llm.rs src/agents/mod.rs
git commit -m "feat: add LLM types — messages, tool calls, responses, config"
```

---

### Task 2.3: Provider Trait & Anthropic Implementation

**Files:**
- Create: `src/agents/provider.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Write the Provider trait and Anthropic provider**

Create `src/agents/provider.rs`:

```rust
use crate::agents::error::AgentError;
use crate::agents::llm::*;
use crate::agents::streaming::SseEvent;
use reqwest::Client;
use serde_json::json;
use tokio::sync::mpsc;

/// Trait for LLM providers. Each provider knows how to:
/// - Build HTTP requests for its API format
/// - Parse SSE events into normalized StreamChunks
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Build the HTTP request body for this provider's API format
    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value;

    /// Return the endpoint URL for chat completions
    fn endpoint_url(&self) -> &str;

    /// Return auth headers
    fn auth_headers(&self) -> Vec<(String, String)>;

    /// Parse a single SSE event into stream chunks (may produce 0 or more chunks)
    fn parse_sse_event(&self, event: &SseEvent) -> Vec<StreamChunk>;
}

// ─── Anthropic (Claude) ─────────────────────────────────────────────

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn endpoint_url(&self) -> &str {
        // Constructed per-call; return base
        &self.base_url
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]
    }

    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value {
        let mut body = json!({
            "model": &request.model,
            "max_tokens": request.max_tokens,
            "stream": true,
        });

        // Anthropic uses a top-level "system" field, not a system message
        if let Some(ref system) = request.system {
            body["system"] = json!(system);
        }

        // Convert messages (skip system role — already handled)
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let content = match &m.content {
                    MessageContent::Text(t) => json!(t),
                    MessageContent::Blocks(blocks) => {
                        let block_json: Vec<serde_json::Value> = blocks
                            .iter()
                            .map(|b| serde_json::to_value(b).unwrap_or(json!(null)))
                            .collect();
                        json!(block_json)
                    }
                };
                let mut msg = json!({
                    "role": &m.role,
                    "content": content,
                });
                if let Some(ref id) = m.tool_call_id {
                    msg["tool_use_id"] = json!(id);
                }
                msg
            })
            .collect();
        body["messages"] = json!(messages);

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": &t.name,
                        "description": &t.description,
                        "input_schema": &t.input_schema,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        body
    }

    fn parse_sse_event(&self, event: &SseEvent) -> Vec<StreamChunk> {
        if event.data == "[DONE]" {
            return vec![];
        }

        let parsed: serde_json::Value = match serde_json::from_str(&event.data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let event_type = event
            .event_type
            .as_deref()
            .or_else(|| parsed["type"].as_str())
            .unwrap_or("");

        match event_type {
            "content_block_delta" => {
                let delta = &parsed["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = delta["text"].as_str() {
                            vec![StreamChunk::TextDelta(text.to_string())]
                        } else {
                            vec![]
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(json_str) = delta["partial_json"].as_str() {
                            let index = parsed["index"].as_u64().unwrap_or(0);
                            vec![StreamChunk::ToolCallDelta {
                                id: format!("block_{}", index),
                                input_json: json_str.to_string(),
                            }]
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![],
                }
            }
            "content_block_start" => {
                let block = &parsed["content_block"];
                if block["type"].as_str() == Some("tool_use") {
                    vec![StreamChunk::ToolCallStart {
                        id: block["id"].as_str().unwrap_or("").to_string(),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                    }]
                } else {
                    vec![]
                }
            }
            "content_block_stop" => {
                let index = parsed["index"].as_u64().unwrap_or(0);
                // Only emit ToolCallEnd if this was a tool block (tracked externally)
                vec![]
            }
            "message_delta" => {
                let stop_reason = match parsed["delta"]["stop_reason"].as_str() {
                    Some("end_turn") => StopReason::EndTurn,
                    Some("tool_use") => StopReason::ToolUse,
                    Some("max_tokens") => StopReason::MaxTokens,
                    Some("stop_sequence") => StopReason::StopSequence,
                    Some(other) => StopReason::Unknown(other.to_string()),
                    None => StopReason::EndTurn,
                };
                let usage = &parsed["usage"];
                vec![StreamChunk::Done {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0) as u32,
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or(0) as u32,
                    stop_reason,
                }]
            }
            "message_start" => {
                // Extract input token count from message_start usage
                let usage = &parsed["message"]["usage"];
                if let Some(input_tokens) = usage["input_tokens"].as_u64() {
                    // Store for later — we'll get output tokens in message_delta
                    // For now, no chunk emitted
                }
                vec![]
            }
            "error" => {
                let msg = parsed["error"]["message"]
                    .as_str()
                    .unwrap_or("Unknown error")
                    .to_string();
                vec![StreamChunk::Error(msg)]
            }
            _ => vec![],
        }
    }
}

// ─── OpenAI-compatible (OpenAI, Ollama, etc.) ───────────────────────

pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    provider_name: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
            provider_name: "openai".to_string(),
        }
    }

    pub fn ollama(base_url: String) -> Self {
        Self {
            api_key: String::new(),
            base_url: format!("{}/v1", base_url.trim_end_matches('/')),
            provider_name: "ollama".to_string(),
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn endpoint_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        if self.api_key.is_empty() {
            return vec![];
        }
        vec![("Authorization".to_string(), format!("Bearer {}", self.api_key))]
    }

    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                let content = match &m.content {
                    MessageContent::Text(t) => json!(t),
                    MessageContent::Blocks(_) => json!(m.content),
                };
                let mut msg = json!({
                    "role": &m.role,
                    "content": content,
                });
                if let Some(ref id) = m.tool_call_id {
                    msg["tool_call_id"] = json!(id);
                }
                msg
            })
            .collect();

        let mut body = json!({
            "model": &request.model,
            "messages": messages,
            "max_tokens": request.max_tokens,
            "stream": true,
        });

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": &t.name,
                            "description": &t.description,
                            "parameters": &t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        body
    }

    fn parse_sse_event(&self, event: &SseEvent) -> Vec<StreamChunk> {
        if event.data.trim() == "[DONE]" {
            return vec![StreamChunk::Done {
                input_tokens: 0,
                output_tokens: 0,
                stop_reason: StopReason::EndTurn,
            }];
        }

        let parsed: serde_json::Value = match serde_json::from_str(&event.data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let choice = &parsed["choices"][0];
        let delta = &choice["delta"];

        let mut chunks = Vec::new();

        // Text content
        if let Some(content) = delta["content"].as_str() {
            if !content.is_empty() {
                chunks.push(StreamChunk::TextDelta(content.to_string()));
            }
        }

        // Tool calls
        if let Some(tool_calls) = delta["tool_calls"].as_array() {
            for tc in tool_calls {
                let id = tc["id"].as_str().unwrap_or("").to_string();
                if let Some(func) = tc["function"].as_object() {
                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                        if !name.is_empty() {
                            chunks.push(StreamChunk::ToolCallStart {
                                id: id.clone(),
                                name: name.to_string(),
                            });
                        }
                    }
                    if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
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

        // Finish reason
        if let Some(reason) = choice["finish_reason"].as_str() {
            let stop_reason = match reason {
                "stop" => StopReason::EndTurn,
                "tool_calls" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                other => StopReason::Unknown(other.to_string()),
            };
            let usage = &parsed["usage"];
            chunks.push(StreamChunk::Done {
                input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                stop_reason,
            });
        }

        chunks
    }
}

// ─── Provider factory ───────────────────────────────────────────────

/// Determine which provider to use based on model name
pub fn resolve_provider_for_model(model: &str) -> &str {
    if model.starts_with("claude-") {
        "anthropic"
    } else if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        "openai"
    } else if model.contains('/') {
        // e.g., "ollama/llama3"
        "ollama"
    } else {
        "openai" // default to OpenAI-compatible
    }
}

/// Create a provider from config
pub fn create_provider(
    provider_name: &str,
    config: &crate::agents::llm::ProviderConfig,
) -> Result<Box<dyn Provider>, AgentError> {
    let api_key = config
        .api_key_env
        .as_ref()
        .and_then(|env| std::env::var(env).ok())
        .unwrap_or_default();

    match provider_name {
        "anthropic" => {
            if api_key.is_empty() {
                return Err(AgentError::ConfigError(
                    "ANTHROPIC_API_KEY not set".to_string(),
                ));
            }
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(ref url) = config.base_url {
                provider = provider.with_base_url(url.clone());
            }
            Ok(Box::new(provider))
        }
        "openai" => {
            if api_key.is_empty() {
                return Err(AgentError::ConfigError(
                    "OPENAI_API_KEY not set".to_string(),
                ));
            }
            let mut provider = OpenAIProvider::new(api_key);
            if let Some(ref url) = config.base_url {
                provider = provider.with_base_url(url.clone());
            }
            Ok(Box::new(provider))
        }
        "ollama" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            Ok(Box::new(OpenAIProvider::ollama(base_url)))
        }
        other => Err(AgentError::ConfigError(format!(
            "Unknown provider: {}",
            other
        ))),
    }
}
```

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod provider;` to `src/agents/mod.rs`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles (possibly with unused warnings, which is fine).

- [ ] **Step 4: Commit**

```bash
git add src/agents/provider.rs src/agents/mod.rs
git commit -m "feat: add Provider trait with Anthropic and OpenAI implementations"
```

---

### Task 2.4: LLM Client — Stream and Block

**Files:**
- Modify: `src/agents/llm.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Add the LlmClient to llm.rs**

Append to `src/agents/llm.rs`:

```rust
use crate::agents::error::AgentError;
use crate::agents::provider::{create_provider, resolve_provider_for_model, Provider};
use crate::agents::streaming::SseParser;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::mpsc;

/// The main LLM client. Owns providers and an HTTP client.
/// All LLM calls flow through this.
/// Providers are Arc'd so they can be shared with spawned streaming tasks.
pub struct LlmClient {
    http_client: Client,
    providers: HashMap<String, Arc<dyn Provider>>,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Result<Self, AgentError> {
        let http_client = Client::builder()
            .build()
            .map_err(|e| AgentError::LlmError(format!("Failed to create HTTP client: {}", e)))?;

        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        for (name, provider_config) in &config.providers {
            match create_provider(name, provider_config) {
                Ok(p) => { providers.insert(name.clone(), Arc::from(p)); }
                Err(e) => {
                    tracing::warn!("Failed to initialize provider '{}': {}", name, e);
                }
            }
        }

        Ok(Self {
            http_client,
            providers,
            config,
        })
    }

    /// Make a streaming LLM call. Returns a channel receiver of StreamChunks.
    pub async fn stream(
        &self,
        request: LlmRequest,
    ) -> Result<mpsc::Receiver<StreamChunk>, AgentError> {
        let provider_name = resolve_provider_for_model(&request.model);
        let provider = self
            .providers
            .get(provider_name)
            .ok_or_else(|| AgentError::LlmProviderUnavailable(provider_name.to_string()))?;

        let body = provider.build_request_body(&request);
        let endpoint = match provider_name {
            "anthropic" => format!("{}/v1/messages", provider.endpoint_url()),
            _ => format!("{}/chat/completions", provider.endpoint_url()),
        };

        let mut req_builder = self.http_client.post(&endpoint);
        for (key, value) in provider.auth_headers() {
            req_builder = req_builder.header(&key, &value);
        }
        req_builder = req_builder.header("content-type", "application/json");
        req_builder = req_builder.json(&body);

        let response = req_builder.send().await.map_err(|e| {
            AgentError::LlmError(format!("HTTP request failed: {}", e))
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(AgentError::LlmRateLimited {
                    provider: provider_name.to_string(),
                    retry_after_ms: None,
                });
            }
            return Err(AgentError::LlmError(format!(
                "HTTP {}: {}", status, body
            )));
        }

        let (tx, rx) = mpsc::channel(256);
        let provider_arc = Arc::clone(provider);

        tokio::spawn(async move {
            let mut parser = SseParser::new();
            let mut byte_stream = response.bytes_stream();
            use futures_util::StreamExt;

            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        let events = parser.feed(&bytes);
                        for event in events {
                            let chunks = provider_arc.parse_sse_event(&event);
                            for chunk in chunks {
                                if tx.send(chunk).await.is_err() {
                                    return; // receiver dropped
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                        return;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Blocking LLM call — collects the full streaming response.
    pub async fn chat(&self, request: LlmRequest) -> Result<LlmResponse, AgentError> {
        let mut rx = self.stream(request).await?;

        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut current_tool_id = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_json = String::new();
        let mut input_tokens = 0u32;
        let mut output_tokens = 0u32;
        let mut stop_reason = StopReason::EndTurn;
        let mut model = String::new();

        while let Some(chunk) = rx.recv().await {
            match chunk {
                StreamChunk::TextDelta(t) => text.push_str(&t),
                StreamChunk::ToolCallStart { id, name } => {
                    // Finish previous tool call if any
                    if !current_tool_id.is_empty() {
                        let args: serde_json::Value =
                            serde_json::from_str(&current_tool_json).unwrap_or(serde_json::Value::Null);
                        tool_calls.push(ToolCall {
                            id: current_tool_id.clone(),
                            name: current_tool_name.clone(),
                            arguments: args,
                        });
                    }
                    current_tool_id = id;
                    current_tool_name = name;
                    current_tool_json.clear();
                }
                StreamChunk::ToolCallDelta { input_json, .. } => {
                    current_tool_json.push_str(&input_json);
                }
                StreamChunk::ToolCallEnd { .. } => {
                    if !current_tool_id.is_empty() {
                        let args: serde_json::Value =
                            serde_json::from_str(&current_tool_json).unwrap_or(serde_json::Value::Null);
                        tool_calls.push(ToolCall {
                            id: current_tool_id.clone(),
                            name: current_tool_name.clone(),
                            arguments: args,
                        });
                        current_tool_id.clear();
                        current_tool_name.clear();
                        current_tool_json.clear();
                    }
                }
                StreamChunk::Done {
                    input_tokens: it,
                    output_tokens: ot,
                    stop_reason: sr,
                } => {
                    input_tokens = it;
                    output_tokens = ot;
                    stop_reason = sr;
                }
                StreamChunk::Error(e) => {
                    return Err(AgentError::LlmError(e));
                }
            }
        }

        // Flush any pending tool call
        if !current_tool_id.is_empty() {
            let args: serde_json::Value =
                serde_json::from_str(&current_tool_json).unwrap_or(serde_json::Value::Null);
            tool_calls.push(ToolCall {
                id: current_tool_id,
                name: current_tool_name,
                arguments: args,
            });
        }

        Ok(LlmResponse {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            stop_reason,
            model,
        })
    }
}

```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles. May have warnings about unused `Provider` trait methods — that's fine for now.

- [ ] **Step 3: Commit**

```bash
git add src/agents/llm.rs
git commit -m "feat: add LlmClient with streaming and blocking chat methods"
```

---

## Chunk 3: Tool System — @tool, Skills, CLI Execution

### Task 3.1: Tool Registry and Schema Generation

**Files:**
- Create: `src/agents/tool.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Write tool registry tests**

Add at the bottom of `src/agents/tool.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup_tool() {
        let mut registry = ToolRegistry::new();
        let def = ToolDefinition {
            name: "search".to_string(),
            description: "Search products".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }),
        };
        registry.register(RegisteredTool {
            definition: def,
            executor: ToolExecutor::Noop,
            requires_approval: false,
            timeout_ms: 30000,
        });
        assert!(registry.get("search").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_tool_definitions_for_llm() {
        let mut registry = ToolRegistry::new();
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name: "a".to_string(),
                description: "Tool A".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            executor: ToolExecutor::Noop,
            requires_approval: false,
            timeout_ms: 30000,
        });
        registry.register(RegisteredTool {
            definition: ToolDefinition {
                name: "b".to_string(),
                description: "Tool B".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            executor: ToolExecutor::Noop,
            requires_approval: false,
            timeout_ms: 30000,
        });
        let defs = registry.definitions();
        assert_eq!(defs.len(), 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib agents::tool 2>&1 | tail -10`
Expected: Compilation error.

- [ ] **Step 3: Implement tool registry**

Write `src/agents/tool.rs`:

```rust
use crate::agents::llm::ToolDefinition;
use crate::agents::error::AgentError;
use std::collections::HashMap;

/// How a tool is executed
#[derive(Debug, Clone)]
pub enum ToolExecutor {
    /// Python function (module path + function name)
    Python { module: String, function: String },
    /// CLI command with argument template
    Cli {
        command: String,
        args_template: Vec<String>,
        output_format: OutputFormat,
    },
    /// No-op (for testing)
    Noop,
}

#[derive(Debug, Clone)]
pub enum OutputFormat {
    Json,
    Text,
    Csv,
}

/// A tool registered in the system
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub executor: ToolExecutor,
    pub requires_approval: bool,
    pub timeout_ms: u64,
}

/// Registry of all tools available to agents
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: RegisteredTool) {
        self.tools.insert(tool.definition.name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }

    /// Get all tool definitions in the format LLMs expect
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition.clone()).collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Execute a CLI tool by spawning a process.
/// Arguments are passed via tokio::process::Command::arg() — NEVER shell-interpolated.
pub async fn execute_cli_tool(
    command: &str,
    args: &[String],
    timeout_ms: u64,
) -> Result<String, AgentError> {
    use tokio::process::Command;

    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: "Empty command".to_string(),
        });
    }

    let mut cmd = Command::new(parts[0]);
    for part in &parts[1..] {
        cmd.arg(part);
    }
    for arg in args {
        cmd.arg(arg);
    }

    let output = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        cmd.output(),
    )
    .await
    .map_err(|_| AgentError::ToolTimeout {
        tool: command.to_string(),
        timeout_ms,
    })?
    .map_err(|e| AgentError::ToolExecutionError {
        tool: command.to_string(),
        message: e.to_string(),
    })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: stderr,
        })
    }
}
```

- [ ] **Step 4: Add module to agents/mod.rs**

Add `pub mod tool;` to `src/agents/mod.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib agents::tool 2>&1 | tail -10`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/agents/tool.rs src/agents/mod.rs
git commit -m "feat: add ToolRegistry with CLI execution and schema generation"
```

---

### Task 3.2: Skill Loader

**Files:**
- Create: `src/agents/skill.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Write skill loading tests**

Add at the bottom of `src/agents/skill.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_toml() {
        let toml_str = r#"
[skill]
name = "test-skill"
description = "A test skill"
version = "1.0.0"

[[tools]]
name = "list-items"
description = "List all items"
command = "echo hello"
output = "text"

[permissions]
allow = ["echo *"]
deny = ["rm *"]
"#;
        let skill = Skill::from_toml(toml_str, "/tmp/test-skill").unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.tools.len(), 1);
        assert_eq!(skill.tools[0].name, "list-items");
        assert!(skill.is_command_allowed("echo hello"));
        assert!(!skill.is_command_allowed("rm -rf /"));
    }

    #[test]
    fn test_permission_glob_matching() {
        let skill = Skill {
            name: "test".to_string(),
            description: "test".to_string(),
            version: "1.0.0".to_string(),
            tools: vec![],
            permissions: SkillPermissions {
                allow: vec!["gh pr *".to_string(), "gh issue *".to_string()],
                deny: vec!["gh repo delete *".to_string()],
            },
            context: None,
            source_dir: "/tmp".to_string(),
        };
        assert!(skill.is_command_allowed("gh pr list"));
        assert!(skill.is_command_allowed("gh issue create"));
        assert!(!skill.is_command_allowed("gh repo delete foo"));
        assert!(!skill.is_command_allowed("ls -la"));
    }
}
```

- [ ] **Step 2: Implement skill loader**

Write `src/agents/skill.rs`:

```rust
use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use crate::agents::tool::{RegisteredTool, ToolExecutor, OutputFormat};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct SkillToml {
    skill: SkillMeta,
    #[serde(default)]
    tools: Vec<SkillToolToml>,
    #[serde(default)]
    permissions: SkillPermissionsToml,
}

#[derive(Debug, Deserialize)]
struct SkillMeta {
    name: String,
    description: String,
    #[serde(default = "default_version")]
    version: String,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, Deserialize)]
struct SkillToolToml {
    name: String,
    description: String,
    command: String,
    #[serde(default)]
    args: std::collections::HashMap<String, ArgDef>,
    #[serde(default = "default_output")]
    output: String,
    #[serde(default)]
    requires_approval: bool,
}

#[derive(Debug, Deserialize)]
struct ArgDef {
    #[serde(rename = "type")]
    arg_type: String,
    #[serde(default)]
    description: String,
}

fn default_output() -> String {
    "text".to_string()
}

#[derive(Debug, Deserialize, Default)]
struct SkillPermissionsToml {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

/// A loaded skill with its tools and permissions
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub version: String,
    pub tools: Vec<SkillTool>,
    pub permissions: SkillPermissions,
    pub context: Option<String>,
    pub source_dir: String,
}

#[derive(Debug, Clone)]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: std::collections::HashMap<String, String>,
    pub output_format: OutputFormat,
    pub requires_approval: bool,
}

#[derive(Debug, Clone)]
pub struct SkillPermissions {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl Skill {
    /// Parse a skill from TOML string
    pub fn from_toml(toml_str: &str, source_dir: &str) -> Result<Self, AgentError> {
        let parsed: SkillToml = toml::from_str(toml_str).map_err(|e| {
            AgentError::SkillLoadError {
                skill: "unknown".to_string(),
                message: format!("TOML parse error: {}", e),
            }
        })?;

        let tools = parsed
            .tools
            .into_iter()
            .map(|t| {
                let output_format = match t.output.as_str() {
                    "json" => OutputFormat::Json,
                    "csv" => OutputFormat::Csv,
                    _ => OutputFormat::Text,
                };
                SkillTool {
                    name: t.name,
                    description: t.description,
                    command: t.command,
                    args: t.args.into_iter().map(|(k, v)| (k, v.arg_type)).collect(),
                    output_format,
                    requires_approval: t.requires_approval,
                }
            })
            .collect();

        Ok(Self {
            name: parsed.skill.name,
            description: parsed.skill.description,
            version: parsed.skill.version,
            tools,
            permissions: SkillPermissions {
                allow: parsed.permissions.allow,
                deny: parsed.permissions.deny,
            },
            context: None,
            source_dir: source_dir.to_string(),
        })
    }

    /// Load a skill from a directory (reads skill.toml and optional context.md)
    pub fn load_from_dir(dir: &Path) -> Result<Self, AgentError> {
        let skill_toml_path = dir.join("skill.toml");
        let toml_str = std::fs::read_to_string(&skill_toml_path).map_err(|e| {
            AgentError::SkillLoadError {
                skill: dir.display().to_string(),
                message: format!("Failed to read skill.toml: {}", e),
            }
        })?;

        let mut skill = Self::from_toml(&toml_str, &dir.display().to_string())?;

        // Load optional context.md
        let context_path = dir.join("context.md");
        if context_path.exists() {
            skill.context = std::fs::read_to_string(&context_path).ok();
        }

        Ok(skill)
    }

    /// Check if a command is allowed by this skill's permissions
    pub fn is_command_allowed(&self, command: &str) -> bool {
        // Check deny first
        for pattern in &self.permissions.deny {
            if glob_match(pattern, command) {
                return false;
            }
        }
        // Check allow
        if self.permissions.allow.is_empty() {
            return false; // No allow rules means nothing is allowed
        }
        for pattern in &self.permissions.allow {
            if glob_match(pattern, command) {
                return true;
            }
        }
        false
    }

    /// Convert skill tools into RegisteredTools for the ToolRegistry
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        self.tools
            .into_iter()
            .map(|t| {
                // Build input schema from args
                let mut properties = serde_json::Map::new();
                let mut required = Vec::new();
                for (name, arg_type) in &t.args {
                    let json_type = match arg_type.as_str() {
                        "int" => "integer",
                        "float" | "number" => "number",
                        "bool" => "boolean",
                        _ => "string",
                    };
                    properties.insert(
                        name.clone(),
                        serde_json::json!({"type": json_type}),
                    );
                    required.push(serde_json::Value::String(name.clone()));
                }

                let input_schema = serde_json::json!({
                    "type": "object",
                    "properties": properties,
                    "required": required,
                });

                RegisteredTool {
                    definition: ToolDefinition {
                        name: t.name.clone(),
                        description: t.description,
                        input_schema,
                    },
                    executor: ToolExecutor::Cli {
                        command: t.command,
                        args_template: vec![],
                        output_format: t.output_format,
                    },
                    requires_approval: t.requires_approval,
                    timeout_ms: 30000,
                }
            })
            .collect()
    }
}

/// Simple glob matching for command permission patterns.
/// Supports only trailing `*` wildcards (e.g., "gh pr *" matches "gh pr list").
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern.ends_with(" *") {
        let prefix = &pattern[..pattern.len() - 2];
        text == prefix || text.starts_with(&format!("{} ", prefix))
    } else if pattern == "*" {
        true
    } else {
        pattern == text
    }
}
```

- [ ] **Step 3: Add module to agents/mod.rs**

Add `pub mod skill;` to `src/agents/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib agents::skill 2>&1 | tail -10`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/agents/skill.rs src/agents/mod.rs
git commit -m "feat: add Skill loader with TOML parsing and permission enforcement"
```

---

## Chunk 4: Memory System — Three-Tier Memory

### Task 4.1: Conversation Memory (Redis)

**Files:**
- Create: `src/agents/conversation.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Implement conversation struct**

Create `src/agents/conversation.rs`:

```rust
use crate::agents::llm::{Message, Role, MessageContent};
use uuid::Uuid;

/// A conversation between a user and an agent
#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub agent_name: String,
    pub messages: Vec<Message>,
    pub token_count: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Conversation {
    pub fn new(agent_name: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_name: agent_name.to_string(),
            messages: Vec::new(),
            token_count: 0,
            created_at: chrono::Utc::now(),
        }
    }

    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
        });
    }

    pub fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
        });
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }
}
```

Note: The Redis persistence layer for conversations (save/load to Redis, summarization) will be implemented in Task 4.2 alongside the Memory struct, since it requires the Redis pool from PuffContext.

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod conversation;` to `src/agents/mod.rs`.

- [ ] **Step 3: Verify it compiles**

`chrono` is already a dependency in `Cargo.toml` — no changes needed.

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add src/agents/conversation.rs src/agents/mod.rs
git commit -m "feat: add Conversation struct for message history tracking"
```

---

### Task 4.2: Memory System — Long-Term (Postgres+pgvector) + Redis Persistence

**Files:**
- Create: `src/agents/memory.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Implement memory module**

Create `src/agents/memory.rs`:

```rust
use crate::agents::error::AgentError;
use serde::Deserialize;

/// Memory configuration
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_conversation_backend")]
    pub conversation: String,
    #[serde(default = "default_long_term_backend")]
    pub long_term: String,
    #[serde(default)]
    pub auto_extract: bool,
    #[serde(default = "default_recall_k")]
    pub recall_k: u32,
    #[serde(default = "default_summarize_after")]
    pub summarize_after: u32,
    pub embedding_model: Option<String>,
    pub embedding_dimensions: Option<u32>,
}

fn default_conversation_backend() -> String { "redis".to_string() }
fn default_long_term_backend() -> String { "postgres".to_string() }
fn default_recall_k() -> u32 { 10 }
fn default_summarize_after() -> u32 { 50 }

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            conversation: default_conversation_backend(),
            long_term: default_long_term_backend(),
            auto_extract: false,
            recall_k: default_recall_k(),
            summarize_after: default_summarize_after(),
            embedding_model: None,
            embedding_dimensions: None,
        }
    }
}

/// SQL for creating the puff_memories table (run at startup if pgvector is enabled)
pub const CREATE_MEMORIES_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS puff_memories (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent       TEXT NOT NULL,
    scope       TEXT NOT NULL,
    scope_id    TEXT,
    content     TEXT NOT NULL,
    embedding   VECTOR,
    importance  FLOAT DEFAULT 0.5,
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    accessed_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_puff_memories_agent ON puff_memories(agent);
CREATE INDEX IF NOT EXISTS idx_puff_memories_scope ON puff_memories(scope, scope_id);
"#;

/// SQL for creating the puff_llm_usage table (cost tracking)
pub const CREATE_USAGE_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS puff_llm_usage (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent          TEXT NOT NULL,
    conversation   UUID,
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    input_tokens   INT NOT NULL,
    output_tokens  INT NOT NULL,
    cost_usd       NUMERIC(10, 6),
    latency_ms     INT,
    cached         BOOLEAN DEFAULT FALSE,
    created_at     TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_puff_llm_usage_agent ON puff_llm_usage(agent);
CREATE INDEX IF NOT EXISTS idx_puff_llm_usage_created ON puff_llm_usage(created_at);
"#;

/// SQL for creating the puff_traces table (observability)
pub const CREATE_TRACES_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS puff_traces (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent           TEXT NOT NULL,
    conversation    UUID,
    trace_data      JSONB NOT NULL,
    total_latency_ms INT,
    total_cost_usd  NUMERIC(10, 6),
    created_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_puff_traces_agent ON puff_traces(agent);
CREATE INDEX IF NOT EXISTS idx_puff_traces_created ON puff_traces(created_at);
"#;

/// Save a memory to Postgres (long-term tier)
pub async fn save_memory(
    client: &tokio_postgres::Client,
    agent: &str,
    scope: &str,
    scope_id: Option<&str>,
    content: &str,
    embedding: Option<&[f32]>,
) -> Result<(), AgentError> {
    let embedding_str = embedding.map(|e| {
        format!(
            "[{}]",
            e.iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    });

    client
        .execute(
            "INSERT INTO puff_memories (agent, scope, scope_id, content, embedding) VALUES ($1, $2, $3, $4, $5::vector)",
            &[
                &agent,
                &scope,
                &scope_id,
                &content,
                &embedding_str.as_deref(),
            ],
        )
        .await
        .map_err(|e| AgentError::MemoryError(format!("Failed to save memory: {}", e)))?;

    Ok(())
}

/// Recall relevant memories via vector similarity search
pub async fn recall_memories(
    client: &tokio_postgres::Client,
    agent: &str,
    query_embedding: &[f32],
    k: u32,
    scope: Option<&str>,
    scope_id: Option<&str>,
) -> Result<Vec<MemoryRecord>, AgentError> {
    let embedding_str = format!(
        "[{}]",
        query_embedding
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );

    let query = if let (Some(scope), Some(scope_id)) = (scope, scope_id) {
        format!(
            "SELECT id, content, 1 - (embedding <=> $1::vector) as similarity \
             FROM puff_memories \
             WHERE agent = $2 AND scope = $3 AND scope_id = $4 \
             ORDER BY embedding <=> $1::vector \
             LIMIT $5"
        )
    } else {
        format!(
            "SELECT id, content, 1 - (embedding <=> $1::vector) as similarity \
             FROM puff_memories \
             WHERE agent = $2 \
             ORDER BY embedding <=> $1::vector \
             LIMIT $3"
        )
    };

    // Execute with appropriate params based on scope
    let rows = if let (Some(scope), Some(scope_id)) = (scope, scope_id) {
        client
            .query(
                &query,
                &[&embedding_str, &agent, &scope, &scope_id, &(k as i64)],
            )
            .await
    } else {
        client
            .query(&query, &[&embedding_str, &agent, &(k as i64)])
            .await
    };

    let rows = rows.map_err(|e| AgentError::MemoryError(format!("Failed to recall memories: {}", e)))?;

    Ok(rows
        .iter()
        .map(|row| MemoryRecord {
            id: row.get::<_, uuid::Uuid>("id").to_string(),
            content: row.get("content"),
            similarity: row.get("similarity"),
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub similarity: f64,
}
```

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod memory;` to `src/agents/mod.rs`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles. May need to add `tokio-postgres` to direct dependencies if not already re-exported.

- [ ] **Step 4: Commit**

```bash
git add src/agents/memory.rs src/agents/mod.rs
git commit -m "feat: add Memory system with pgvector long-term storage and schema DDL"
```

---

## Chunk 5: Agent Core — Definition, Execution Loop, Configuration

### Task 5.1: Agent Struct and Execution Loop

**Files:**
- Create: `src/agents/agent.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Implement agent struct**

Create `src/agents/agent.rs`:

```rust
use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::llm::*;
use crate::agents::memory::MemoryConfig;
use crate::agents::tool::ToolRegistry;
use serde::Deserialize;
use std::sync::Arc;

/// Agent configuration — can be built from Python API or puff.toml
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentPermissions {
    pub sql: Option<String>,
    pub http: Option<Vec<String>>,
    pub filesystem: Option<String>,
}

/// A configured agent ready to handle conversations
pub struct Agent {
    pub config: AgentConfig,
    pub tools: Arc<ToolRegistry>,
    pub context_snippets: Vec<String>, // from skill context.md files
}

impl Agent {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            tools: Arc::new(ToolRegistry::new()),
            context_snippets: Vec::new(),
        }
    }

    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = Arc::new(tools);
        self
    }

    pub fn with_context(mut self, context: String) -> Self {
        self.context_snippets.push(context);
        self
    }

    /// Build the system prompt from base prompt + skill context
    pub fn build_system_prompt(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref prompt) = self.config.system_prompt {
            parts.push(prompt.clone());
        }

        for snippet in &self.context_snippets {
            parts.push(snippet.clone());
        }

        parts.join("\n\n")
    }

    /// Build the full LLM request for a conversation turn
    pub fn build_request(&self, conversation: &Conversation) -> LlmRequest {
        let mut request = LlmRequest::new(
            self.config.model.clone(),
            conversation.messages.clone(),
        );

        let system = self.build_system_prompt();
        if !system.is_empty() {
            request = request.with_system(system);
        }

        let tool_defs = self.tools.definitions();
        if !tool_defs.is_empty() {
            request = request.with_tools(tool_defs);
        }

        request
    }

    /// Run one turn of the agent loop:
    /// 1. Build context
    /// 2. Call LLM
    /// 3. If tool calls, execute them and call LLM again
    /// 4. Return final response
    ///
    /// This is the core loop that runs on a Python/agent thread.
    /// LLM calls and tool execution go through Rust/Tokio.
    pub async fn run_turn(
        &self,
        conversation: &mut Conversation,
        llm_client: &LlmClient,
    ) -> Result<LlmResponse, AgentError> {
        let max_tool_rounds = 10; // prevent infinite loops

        for _round in 0..max_tool_rounds {
            let request = self.build_request(conversation);
            let response = llm_client.chat(request).await?;

            if response.tool_calls.is_empty() {
                // No tool calls — we're done
                conversation.add_assistant_message(&response.text);
                return Ok(response);
            }

            // Add assistant message with tool calls to conversation
            let tool_use_blocks: Vec<ContentBlock> = response
                .tool_calls
                .iter()
                .map(|tc| ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                })
                .collect();

            let mut blocks = Vec::new();
            if !response.text.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: response.text.clone(),
                });
            }
            blocks.extend(tool_use_blocks);

            conversation.add_message(Message {
                role: Role::Assistant,
                content: MessageContent::Blocks(blocks),
                tool_call_id: None,
            });

            // Execute tools (TODO: parallel execution, Python tools, permission checks)
            // For now, this is a placeholder that returns tool not found errors
            for tc in &response.tool_calls {
                let result = match self.tools.get(&tc.name) {
                    Some(_tool) => {
                        // Tool execution will be wired up when Python bindings are added
                        format!("Tool '{}' executed (stub)", tc.name)
                    }
                    None => {
                        format!("Error: tool '{}' not found", tc.name)
                    }
                };

                conversation.add_message(Message {
                    role: Role::User,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: result,
                        is_error: None,
                    }]),
                    tool_call_id: Some(tc.id.clone()),
                });
            }
        }

        Err(AgentError::OrchestrationError(
            "Max tool rounds exceeded".to_string(),
        ))
    }
}
```

- [ ] **Step 2: Add module to agents/mod.rs and update re-exports**

Update `src/agents/mod.rs`:

```rust
pub mod agent;
pub mod conversation;
pub mod error;
pub mod llm;
pub mod memory;
pub mod provider;
pub mod skill;
pub mod streaming;
pub mod tool;

pub use agent::{Agent, AgentConfig};
pub use conversation::Conversation;
pub use error::AgentError;
pub use llm::{LlmClient, LlmConfig, LlmRequest, LlmResponse, Message, Role};
pub use memory::MemoryConfig;
pub use skill::Skill;
pub use tool::ToolRegistry;

pub type AgentResult<T> = Result<T, AgentError>;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add src/agents/agent.rs src/agents/mod.rs
git commit -m "feat: add Agent struct with execution loop (LLM + tool dispatch)"
```

---

## Chunk 6: Orchestration — Multi-Agent Patterns

### Task 6.1: Router, Supervisor, Chain, Parallel

**Files:**
- Create: `src/agents/orchestration.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Implement orchestration patterns**

Create `src/agents/orchestration.rs`:

```rust
use crate::agents::agent::Agent;
use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::llm::*;

/// Router — classifies messages and dispatches to specialist agents
pub struct Router {
    pub name: String,
    pub router_prompt: String,
    pub router_model: String,
    pub agents: Vec<Agent>,
}

impl Router {
    /// Determine which agent should handle a message
    pub async fn route(
        &self,
        message: &str,
        llm_client: &LlmClient,
    ) -> Result<usize, AgentError> {
        let agent_list: String = self
            .agents
            .iter()
            .enumerate()
            .map(|(i, a)| format!("{}: {} - {}", i, a.config.name, a.config.system_prompt.as_deref().unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\n");

        let system = format!(
            "{}\n\nAvailable agents:\n{}\n\nRespond with ONLY the agent number (0-indexed).",
            self.router_prompt, agent_list
        );

        let messages = vec![Message {
            role: Role::User,
            content: MessageContent::Text(message.to_string()),
            tool_call_id: None,
        }];

        let request = LlmRequest::new(self.router_model.clone(), messages)
            .with_system(system)
            .with_max_tokens(16);

        let response = llm_client.chat(request).await?;

        let index: usize = response
            .text
            .trim()
            .parse()
            .map_err(|_| AgentError::OrchestrationError(
                format!("Router returned non-numeric response: '{}'", response.text.trim())
            ))?;

        if index >= self.agents.len() {
            return Err(AgentError::OrchestrationError(
                format!("Router returned index {} but only {} agents available", index, self.agents.len())
            ));
        }

        Ok(index)
    }
}

/// Chain — sequential pipeline, each agent's output feeds the next
pub struct Chain {
    pub name: String,
    pub agents: Vec<Agent>,
}

impl Chain {
    /// Run the chain: first agent gets the input, each subsequent agent gets the previous output
    pub async fn run(
        &self,
        input: &str,
        llm_client: &LlmClient,
    ) -> Result<String, AgentError> {
        let mut current_input = input.to_string();

        for agent in &self.agents {
            let mut conv = Conversation::new(&agent.config.name);
            conv.add_user_message(&current_input);

            let response = agent.run_turn(&mut conv, llm_client).await?;
            current_input = response.text;
        }

        Ok(current_input)
    }
}

/// Parallel — multiple agents work simultaneously, results merged
pub struct Parallel {
    pub name: String,
    pub agents: Vec<Agent>,
    pub merge_prompt: Option<String>,
    pub merge_model: Option<String>,
}

impl Parallel {
    /// Run all agents in parallel and optionally merge results
    pub async fn run(
        &self,
        input: &str,
        llm_client: &LlmClient,
    ) -> Result<String, AgentError> {
        let mut handles = Vec::new();

        // Spawn each agent on a separate Tokio task
        // In production with free-threaded Python, these would be real threads
        for agent_config in &self.agents {
            let input_owned = input.to_string();
            let agent_name = agent_config.config.name.clone();
            let model = agent_config.config.model.clone();
            let system_prompt = agent_config.build_system_prompt();

            // For now, we just collect promises. Full implementation requires
            // cloning or Arc-ing the agent and llm_client.
            // This is a structural placeholder showing the pattern.
            handles.push((agent_name, input_owned));
        }

        // TODO: Execute in parallel with tokio::spawn or std::thread::spawn
        // For now, execute sequentially as a correct baseline
        let mut results = Vec::new();
        for agent in &self.agents {
            let mut conv = Conversation::new(&agent.config.name);
            conv.add_user_message(input);

            match agent.run_turn(&mut conv, llm_client).await {
                Ok(response) => results.push(format!("[{}]: {}", agent.config.name, response.text)),
                Err(e) => results.push(format!("[{}]: Error: {}", agent.config.name, e)),
            }
        }

        let combined = results.join("\n\n");

        // Optionally merge with an LLM call
        if let Some(ref merge_prompt) = self.merge_prompt {
            let model = self.merge_model.as_deref().unwrap_or("claude-haiku-4-5");
            let messages = vec![Message {
                role: Role::User,
                content: MessageContent::Text(combined),
                tool_call_id: None,
            }];

            let request = LlmRequest::new(model, messages)
                .with_system(merge_prompt.clone());

            let response = llm_client.chat(request).await?;
            Ok(response.text)
        } else {
            Ok(combined)
        }
    }
}
```

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod orchestration;` and re-exports for `Router`, `Chain`, `Parallel`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add src/agents/orchestration.rs src/agents/mod.rs
git commit -m "feat: add orchestration patterns — Router, Chain, Parallel"
```

---

## Chunk 7: Agent Server & Observability

### Task 7.1: Trace Module

**Files:**
- Create: `src/agents/trace.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Implement trace types**

Create `src/agents/trace.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub trace_id: String,
    pub agent: String,
    pub conversation: Option<String>,
    pub events: Vec<TraceEvent>,
    pub total_latency_ms: u64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceEvent {
    #[serde(rename = "router")]
    Router {
        decision: String,
        latency_ms: u64,
    },
    #[serde(rename = "llm_call")]
    LlmCall {
        model: String,
        input_tokens: u32,
        output_tokens: u32,
        latency_ms: u64,
        cost_usd: f64,
    },
    #[serde(rename = "tool_call")]
    ToolCall {
        tool: String,
        latency_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "memory_recall")]
    MemoryRecall {
        memories_found: u32,
        latency_ms: u64,
    },
    #[serde(rename = "handoff")]
    Handoff {
        from: String,
        to: String,
    },
}

impl Trace {
    pub fn new(agent: &str) -> Self {
        Self {
            trace_id: Uuid::new_v4().to_string(),
            agent: agent.to_string(),
            conversation: None,
            events: Vec::new(),
            total_latency_ms: 0,
            total_cost_usd: 0.0,
        }
    }

    pub fn with_conversation(mut self, conv_id: &str) -> Self {
        self.conversation = Some(conv_id.to_string());
        self
    }

    pub fn add_event(&mut self, event: TraceEvent) {
        match &event {
            TraceEvent::LlmCall { latency_ms, cost_usd, .. } => {
                self.total_latency_ms += latency_ms;
                self.total_cost_usd += cost_usd;
            }
            TraceEvent::ToolCall { latency_ms, .. } => {
                self.total_latency_ms += latency_ms;
            }
            TraceEvent::Router { latency_ms, .. } => {
                self.total_latency_ms += latency_ms;
            }
            TraceEvent::MemoryRecall { latency_ms, .. } => {
                self.total_latency_ms += latency_ms;
            }
            _ => {}
        }
        self.events.push(event);
    }
}
```

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod trace;` to `src/agents/mod.rs`.

- [ ] **Step 3: Commit**

```bash
git add src/agents/trace.rs src/agents/mod.rs
git commit -m "feat: add Trace and TraceEvent for agent observability"
```

---

### Task 7.2: Agent Server HTTP Endpoints

**Files:**
- Create: `src/agents/server.rs`
- Modify: `src/agents/mod.rs`

- [ ] **Step 1: Implement agent API handlers**

Create `src/agents/server.rs`:

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse, IntoResponse, Sse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::agents::agent::Agent;
use crate::agents::conversation::Conversation;

/// Shared state for the agent server
pub struct AgentServerState {
    pub agents: HashMap<String, Agent>,
    pub conversations: RwLock<HashMap<String, Conversation>>,
    pub llm_client: crate::agents::llm::LlmClient,
}

// Request/Response types
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

/// Build the agent server Axum router
pub fn agent_router(state: Arc<AgentServerState>) -> Router {
    Router::new()
        .route("/api/conversations", post(start_conversation))
        .route("/api/conversations/{id}", post(continue_conversation))
        .route("/api/conversations/{id}", get(get_conversation))
        .route("/api/agents", get(list_agents))
        .route("/health", get(health_check))
        .with_state(state)
}

async fn start_conversation(
    State(state): State<Arc<AgentServerState>>,
    Json(req): Json<StartConversationRequest>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let agent = state
        .agents
        .get(&req.agent)
        .ok_or((StatusCode::NOT_FOUND, format!("Agent '{}' not found", req.agent)))?;

    let mut conversation = Conversation::new(&req.agent);
    conversation.add_user_message(&req.message);

    let response = agent
        .run_turn(&mut conversation, &state.llm_client)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let conv_id = conversation.id.clone();
    let agent_name = conversation.agent_name.clone();

    state
        .conversations
        .write()
        .await
        .insert(conv_id.clone(), conversation);

    Ok(Json(ConversationResponse {
        conversation_id: conv_id,
        agent: agent_name,
        response: response.text,
    }))
}

async fn continue_conversation(
    State(state): State<Arc<AgentServerState>>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<ConversationResponse>, (StatusCode, String)> {
    let message = req["message"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "Missing 'message' field".to_string()))?;

    let mut conversations = state.conversations.write().await;
    let conversation = conversations
        .get_mut(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("Conversation '{}' not found", id)))?;

    conversation.add_user_message(message);

    let agent = state
        .agents
        .get(&conversation.agent_name)
        .ok_or((StatusCode::NOT_FOUND, "Agent not found".to_string()))?;

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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let conversations = state.conversations.read().await;
    let conversation = conversations
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, format!("Conversation '{}' not found", id)))?;

    Ok(Json(serde_json::json!({
        "id": conversation.id,
        "agent": conversation.agent_name,
        "message_count": conversation.messages.len(),
        "messages": conversation.messages,
    })))
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
            tools: agent.tools.names().iter().map(|s| s.to_string()).collect(),
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
```

- [ ] **Step 2: Add module to agents/mod.rs**

Add `pub mod server;` to `src/agents/mod.rs`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add src/agents/server.rs src/agents/mod.rs
git commit -m "feat: add agent server with REST API endpoints"
```

---

### Task 7.3: Wire Agent Server into main.rs

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add agent config to main Config struct**

The `Config` struct at `src/main.rs:23` has `#[serde(deny_unknown_fields)]`. This means new TOML fields would cause parse errors for existing users. Remove `#[serde(deny_unknown_fields)]` from the top-level `Config` struct to allow forward compatibility (new fields are ignored by old versions). Keep `deny_unknown_fields` on the sub-structs.

Then add these fields to the `Config` struct:

```rust
#[serde(default)]
llm: Option<puff::agents::llm::LlmConfig>,
#[serde(default)]
memory: Option<puff::agents::memory::MemoryConfig>,
#[serde(default)]
agents: Option<Vec<puff::agents::agent::AgentConfig>>,
#[serde(default)]
agent_server: Option<AgentServerConfig>,
```

Add the config struct:

```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentServerConfig {
    #[serde(default = "default_agent_port")]
    pub port: u16,
    #[serde(default)]
    pub websockets: bool,
    #[serde(default)]
    pub graphql: bool,
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

fn default_agent_port() -> u16 { 8080 }
```

- [ ] **Step 2: Create AgentServeCommand**

Create `src/program/commands/agent.rs`:

```rust
use crate::program::{RunnableCommand, Runnable};
use crate::context::PuffContext;
use crate::agents::server::{AgentServerState, agent_router};
use crate::agents::llm::{LlmClient, LlmConfig};
use crate::agents::agent::{Agent, AgentConfig};
use crate::agents::skill::Skill;
use clap::{Command, ArgMatches};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AgentServeCommand {
    pub agent_configs: Vec<AgentConfig>,
    pub llm_config: LlmConfig,
    pub port: u16,
}

impl RunnableCommand for AgentServeCommand {
    fn cli_parser(&self) -> Command {
        Command::new("agent")
            .about("Serve agents via HTTP API")
            .arg(clap::arg!(--port <PORT> "Port to listen on").default_value("8080"))
    }

    fn make_runnable(
        &mut self,
        args: &ArgMatches,
        _dispatcher: PuffContext,
    ) -> crate::errors::Result<Runnable> {
        let port: u16 = args
            .get_one::<String>("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.port);

        let llm_client = LlmClient::new(self.llm_config.clone())?;

        let mut agents = HashMap::new();
        for config in &self.agent_configs {
            let mut agent = Agent::new(config.clone());

            // Load skills
            for skill_path in &config.skills {
                let path = std::path::Path::new(skill_path);
                if path.exists() {
                    match Skill::load_from_dir(path) {
                        Ok(skill) => {
                            if let Some(ref ctx) = skill.context {
                                agent = agent.with_context(ctx.clone());
                            }
                            let mut registry = crate::agents::tool::ToolRegistry::new();
                            for tool in skill.into_registered_tools() {
                                registry.register(tool);
                            }
                            agent = agent.with_tools(registry);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load skill '{}': {}", skill_path, e);
                        }
                    }
                }
            }

            agents.insert(config.name.clone(), agent);
        }

        let state = Arc::new(AgentServerState {
            agents,
            conversations: RwLock::new(HashMap::new()),
            llm_client,
        });

        let router = agent_router(state);

        Ok(Runnable::new(async move {
            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to bind to port {}: {}", port, e))?;

            tracing::info!("Agent server listening on port {}", port);
            axum::serve(listener, router)
                .await
                .map_err(|e| anyhow::anyhow!("Server error: {}", e))?;

            Ok(std::process::ExitCode::SUCCESS)
        }))
    }
}
```

- [ ] **Step 3: Register AgentServeCommand in main.rs**

In the command registration section (~line 625), after the existing commands:

```rust
if let Some(ref agent_configs) = config.agents {
    let llm_config = config.llm.clone().unwrap_or_default();
    let port = config.agent_server.as_ref().map(|s| s.port).unwrap_or(8080);
    program = program.command(AgentServeCommand {
        agent_configs: agent_configs.clone(),
        llm_config,
        port,
    });
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles.

- [ ] **Step 5: Write a basic integration test**

Create `examples/agent_basic.rs`:

```rust
use puff::agents::*;
use puff::agents::llm::*;
use puff::agents::tool::*;
use puff::agents::agent::*;

fn main() {
    // This example shows how to configure an agent from Rust
    let config = AgentConfig {
        name: "demo".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        system_prompt: Some("You are a helpful assistant.".to_string()),
        skills: vec![],
        tools_module: None,
        memory: None,
        permissions: None,
    };

    let agent = Agent::new(config);
    println!("Agent '{}' configured with model '{}'", agent.config.name, agent.config.model);
    println!("System prompt: {}", agent.build_system_prompt());
}
```

- [ ] **Step 6: Run the example**

Run: `cargo run --example agent_basic 2>&1 | tail -5`
Expected: Prints agent configuration.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/program/commands/ examples/agent_basic.rs
git commit -m "feat: wire agent server into main.rs and add basic agent example"
```

---

### Task 7.4: Example Skill Directory

**Files:**
- Create: `examples/agent_skills/github/skill.toml`
- Create: `examples/agent_skills/github/context.md`

- [ ] **Step 1: Create example GitHub skill**

Create `examples/agent_skills/github/skill.toml`:

```toml
[skill]
name = "github"
description = "Interact with GitHub repositories via the gh CLI"
version = "1.0.0"

[[tools]]
name = "list-prs"
description = "List open pull requests"
command = "gh pr list --json number,title,author,url"
output = "json"

[[tools]]
name = "view-pr"
description = "View a specific pull request"
command = "gh pr view"
args = { number = { type = "int", description = "PR number" } }
output = "json"

[[tools]]
name = "create-pr"
description = "Create a new pull request"
command = "gh pr create"
args = { title = { type = "str", description = "PR title" }, body = { type = "str", description = "PR body" } }
requires_approval = true

[permissions]
allow = [
    "gh pr *",
    "gh issue *",
    "gh repo view *",
]
deny = [
    "gh repo delete *",
]
```

Create `examples/agent_skills/github/context.md`:

```markdown
# GitHub Skill Context

When working with pull requests:
- Always check CI status before suggesting a merge
- Link related issues using "Fixes #N" in PR bodies
- PRs with more than 500 lines changed should be flagged for splitting

When triaging issues:
- Label bugs with priority based on user impact
- Check for duplicates before creating new issues
```

- [ ] **Step 2: Write a test that loads the skill**

Add to `examples/agent_basic.rs` or a new test file:

```rust
// Test skill loading
let skill_path = std::path::Path::new("examples/agent_skills/github");
let skill = puff::agents::skill::Skill::load_from_dir(skill_path).unwrap();
println!("Loaded skill: {} v{}", skill.name, skill.version);
println!("Tools: {:?}", skill.tools.iter().map(|t| &t.name).collect::<Vec<_>>());
println!("Context: {:?}", skill.context.is_some());
```

- [ ] **Step 3: Commit**

```bash
git add examples/agent_skills/
git commit -m "feat: add example GitHub skill with skill.toml and context.md"
```

---

## Chunk 8: Python Bindings & Integration

### Task 8.1: Python Agent Bindings

**Files:**
- Create: `src/agents/python_bindings.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/python/mod.rs`

- [ ] **Step 1: Create Python-facing agent classes**

Create `src/agents/python_bindings.rs`:

```rust
use pyo3::prelude::*;

/// Python-facing Agent class
#[pyclass(name = "Agent")]
pub struct PyAgent {
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
}

#[pymethods]
impl PyAgent {
    #[new]
    #[pyo3(signature = (name, model="claude-sonnet-4-6", system_prompt=None, skills=None, tools=None, memory=None, permissions=None))]
    fn new(
        name: String,
        model: &str,
        system_prompt: Option<String>,
        skills: Option<Vec<String>>,
        tools: Option<Vec<PyObject>>,
        memory: Option<PyObject>,
        permissions: Option<PyObject>,
    ) -> Self {
        Self {
            name,
            model: model.to_string(),
            system_prompt,
        }
    }
}

/// Python-facing tool decorator result
#[pyclass(name = "ToolDef")]
pub struct PyToolDef {
    pub name: String,
    pub description: String,
    pub function: PyObject,
}

/// Register agent-related Python classes with the puff module
pub fn register_python_classes(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyAgent>()?;
    m.add_class::<PyToolDef>()?;
    Ok(())
}
```

- [ ] **Step 2: Wire into bootstrap_puff_globals in src/python/mod.rs**

In the `bootstrap_puff_globals` function, after the existing registrations, add:

```rust
crate::agents::python_bindings::register_python_classes(&puff_module)?;
```

- [ ] **Step 3: Add module to agents/mod.rs**

Add `pub mod python_bindings;` to `src/agents/mod.rs`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add src/agents/python_bindings.rs src/agents/mod.rs src/python/mod.rs
git commit -m "feat: add Python bindings for Agent and Tool classes"
```

---

### Task 8.2: Integration Test — Full Agent Conversation

**Files:**
- Create: `examples/python_tests/test_agents.py`

- [ ] **Step 1: Create Python integration test**

Create `examples/python_tests/test_agents.py`:

```python
"""Integration tests for Puff agent features."""
import puff


def test_agent_creation():
    """Test that we can create an Agent from Python."""
    agent = puff.Agent(
        name="test-agent",
        model="claude-sonnet-4-6",
        system_prompt="You are a test agent.",
    )
    assert agent.name == "test-agent"
    assert agent.model == "claude-sonnet-4-6"


def test_agent_no_args():
    """Test agent with minimal args."""
    agent = puff.Agent(name="minimal")
    assert agent.name == "minimal"
    assert agent.model == "claude-sonnet-4-6"  # default
```

- [ ] **Step 2: Verify tests pass**

Run: `cargo run --example pytest 2>&1 | tail -10`
Expected: Tests pass (this depends on the pytest example runner working).

- [ ] **Step 3: Commit**

```bash
git add examples/python_tests/test_agents.py
git commit -m "test: add Python integration tests for agent creation"
```

---

## Phase 2: Deferred Spec Items

The following spec sections are **intentionally deferred** to Phase 2. Phase 1 (above) delivers a working agent runtime with the core loop, tool system, memory, LLM gateway, orchestration, and server. Phase 2 adds production hardening and advanced features.

| Spec Section | What's Missing | Dependency |
|---|---|---|
| **Embeddings** (`puff.embed()`, `puff.embed_batch()`) | No embedding generation method on LlmClient | Needed for memory recall to work end-to-end |
| **LLM Caching** (Redis-backed response cache) | Config defined but no cache check/write logic | Requires Redis pool access in LlmClient |
| **Cost Tracking** (Postgres persistence) | DDL defined but no write logic after LLM calls | Requires Postgres pool access in LlmClient |
| **Rate Limiting & Resilience** (backoff, fallback chains) | 429 detected but no retry/fallback | Needs token bucket or leaky bucket in Rust |
| **Supervisor Pattern** | Not implemented | Requires auto-generated `delegate` tool |
| **Parallel Execution** | Currently sequential placeholder | Requires `Arc<Agent>` + `Arc<LlmClient>` for thread spawning |
| **Handoff** | Not implemented | Requires conversation transfer protocol |
| **Human-in-the-Loop** | Not implemented | Requires WebSocket event protocol |
| **WebSocket Streaming** | Not implemented | Requires stream forking in Rust |
| **GraphQL Analytics** | Not implemented | Requires trace persistence + GraphQL schema |
| **Evaluation Framework** (`EvalSuite`, `EvalCase`) | Types listed but no implementation | Requires working agent + assertions |
| **CLI Commands** (`puff skill install`, `puff memory search`, etc.) | Only `puff agent serve` implemented | Requires clap subcommand tree |
| **Migration / Backward Compat** | `deny_unknown_fields` removed from top-level Config | May need deprecation warnings for old fields |

**Recommended Phase 2 priority order:**
1. Embeddings (unblocks memory system)
2. Parallel execution (unblocks Parallel orchestration)
3. WebSocket streaming (unblocks real-time UIs)
4. Rate limiting + fallback (production readiness)
5. Supervisor + Handoff (advanced orchestration)
6. Evaluation framework (testing infrastructure)
7. Cost tracking + analytics (observability)
8. CLI commands (developer experience)

---

## Post-Implementation Checklist

After all chunks are complete:

- [ ] Run `cargo test` — all Rust tests pass
- [ ] Run `cargo clippy` — no warnings
- [ ] Run `cargo fmt --check` — code is formatted
- [ ] Run the agent basic example: `cargo run --example agent_basic`
- [ ] Verify skill loading works with the example skill
- [ ] Test with a real API key: set `ANTHROPIC_API_KEY` and test a conversation
- [ ] Update `readme.md` with agent runtime features
- [ ] Update `CHANGELOG.md` with v2.0 changes
- [ ] Bump version in `Cargo.toml` to `0.2.0`
