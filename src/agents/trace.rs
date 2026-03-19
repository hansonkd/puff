//! Structured observability traces for agent execution.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::error::AgentError;

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
    Router { decision: String, latency_ms: u64 },
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
    Handoff { from: String, to: String },
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
            TraceEvent::Router { latency_ms, .. } => {
                self.total_latency_ms += latency_ms;
            }
            TraceEvent::LlmCall {
                latency_ms,
                cost_usd,
                ..
            } => {
                self.total_latency_ms += latency_ms;
                self.total_cost_usd += cost_usd;
            }
            TraceEvent::ToolCall { latency_ms, .. } => {
                self.total_latency_ms += latency_ms;
            }
            TraceEvent::MemoryRecall { latency_ms, .. } => {
                self.total_latency_ms += latency_ms;
            }
            TraceEvent::Handoff { .. } => {}
        }
        self.events.push(event);
    }
}

/// Save a trace to Postgres.
pub async fn save_trace(client: &tokio_postgres::Client, trace: &Trace) -> Result<(), AgentError> {
    let trace_json = serde_json::to_value(&trace.events)
        .map_err(|e| AgentError::MemoryError(format!("Failed to serialize trace: {}", e)))?;

    client
        .execute(
            "INSERT INTO puff_traces (id, agent, conversation, trace_data, total_latency_ms, total_cost_usd) \
             VALUES ($1::uuid, $2, $3::uuid, $4, $5, $6)",
            &[
                &Uuid::parse_str(&trace.trace_id).unwrap_or_else(|_| Uuid::new_v4()),
                &trace.agent,
                &trace
                    .conversation
                    .as_ref()
                    .and_then(|c| Uuid::parse_str(c).ok()),
                &trace_json,
                &(trace.total_latency_ms as i32),
                &(trace.total_cost_usd as f64),
            ],
        )
        .await
        .map_err(|e| AgentError::MemoryError(format!("Failed to save trace: {}", e)))?;

    Ok(())
}
