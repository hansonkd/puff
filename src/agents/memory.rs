//! Three-tier memory system with pgvector support.

use serde::{Deserialize, Serialize};
use std::fmt::Write;

use crate::agents::error::AgentError;

// ---------------------------------------------------------------------------
// MemoryConfig
// ---------------------------------------------------------------------------

fn default_conversation_backend() -> String {
    "redis".to_string()
}

fn default_long_term_backend() -> String {
    "postgres".to_string()
}

fn default_recall_k() -> u32 {
    10
}

fn default_summarize_after() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ---------------------------------------------------------------------------
// SQL DDL constants
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Memory record
// ---------------------------------------------------------------------------

pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub similarity: f64,
}

fn vector_to_pgvector(values: &[f32]) -> String {
    let mut out = String::with_capacity(values.len().saturating_mul(12).saturating_add(2));
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(&mut out, "{value}");
    }
    out.push(']');
    out
}

// ---------------------------------------------------------------------------
// Memory functions
// ---------------------------------------------------------------------------

/// Insert a memory into puff_memories.
pub async fn save_memory(
    client: &tokio_postgres::Client,
    agent: &str,
    scope: &str,
    scope_id: Option<&str>,
    content: &str,
    embedding: Option<&[f32]>,
) -> Result<(), AgentError> {
    let embedding_str: Option<String> = embedding.map(vector_to_pgvector);

    let sql = if embedding_str.is_some() {
        "INSERT INTO puff_memories (agent, scope, scope_id, content, embedding) \
         VALUES ($1, $2, $3, $4, $5::vector)"
    } else {
        "INSERT INTO puff_memories (agent, scope, scope_id, content) \
         VALUES ($1, $2, $3, $4)"
    };

    let result = if let Some(ref emb) = embedding_str {
        client
            .execute(sql, &[&agent, &scope, &scope_id, &content, emb])
            .await
    } else {
        client
            .execute(sql, &[&agent, &scope, &scope_id, &content])
            .await
    };

    result
        .map(|_| ())
        .map_err(|e| AgentError::MemoryError(format!("save_memory failed: {e}")))
}

/// Record LLM usage to Postgres.
#[allow(clippy::too_many_arguments)]
pub async fn record_llm_usage(
    client: &tokio_postgres::Client,
    agent: &str,
    conversation_id: Option<&str>,
    provider: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
    latency_ms: u32,
    cached: bool,
) -> Result<(), AgentError> {
    client
        .execute(
            "INSERT INTO puff_llm_usage (agent, conversation, provider, model, input_tokens, output_tokens, cost_usd, latency_ms, cached) \
             VALUES ($1, $2::uuid, $3, $4, $5, $6, $7, $8, $9)",
            &[
                &agent,
                &conversation_id
                    .and_then(|c| uuid::Uuid::parse_str(c).ok()),
                &provider,
                &model,
                &(input_tokens as i32),
                &(output_tokens as i32),
                &cost_usd,
                &(latency_ms as i32),
                &cached,
            ],
        )
        .await
        .map_err(|e| AgentError::MemoryError(format!("Failed to record usage: {}", e)))?;

    Ok(())
}

/// Estimate cost in USD based on model and token counts.
/// Prices are approximate and should be updated periodically.
pub fn estimate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (input_price, output_price) = match model {
        m if m.starts_with("claude-opus") => (15.0, 75.0), // per 1M tokens
        m if m.starts_with("claude-sonnet") => (3.0, 15.0),
        m if m.starts_with("claude-haiku") => (0.25, 1.25),
        m if m.starts_with("gpt-4o") => (2.5, 10.0),
        m if m.starts_with("gpt-4") => (30.0, 60.0),
        _ => (1.0, 2.0), // default conservative estimate
    };
    (input_tokens as f64 * input_price + output_tokens as f64 * output_price) / 1_000_000.0
}

/// Recall memories using cosine similarity via pgvector.
pub async fn recall_memories(
    client: &tokio_postgres::Client,
    agent: &str,
    query_embedding: &[f32],
    k: u32,
    scope: Option<&str>,
    scope_id: Option<&str>,
) -> Result<Vec<MemoryRecord>, AgentError> {
    let embedding_str = vector_to_pgvector(query_embedding);

    let rows = match (scope, scope_id) {
        (Some(sc), Some(sid)) => {
            let sql = "SELECT id::text, content, 1 - (embedding <=> $1::vector) AS similarity \
                       FROM puff_memories \
                       WHERE agent = $2 AND scope = $3 AND scope_id = $4 \
                       ORDER BY similarity DESC \
                       LIMIT $5";
            client
                .query(sql, &[&embedding_str, &agent, &sc, &sid, &(k as i64)])
                .await
        }
        (Some(sc), None) => {
            let sql = "SELECT id::text, content, 1 - (embedding <=> $1::vector) AS similarity \
                       FROM puff_memories \
                       WHERE agent = $2 AND scope = $3 \
                       ORDER BY similarity DESC \
                       LIMIT $4";
            client
                .query(sql, &[&embedding_str, &agent, &sc, &(k as i64)])
                .await
        }
        _ => {
            let sql = "SELECT id::text, content, 1 - (embedding <=> $1::vector) AS similarity \
                       FROM puff_memories \
                       WHERE agent = $2 \
                       ORDER BY similarity DESC \
                       LIMIT $3";
            client
                .query(sql, &[&embedding_str, &agent, &(k as i64)])
                .await
        }
    };

    let rows = rows.map_err(|e| AgentError::MemoryError(format!("recall_memories failed: {e}")))?;

    let records = rows
        .into_iter()
        .map(|row| MemoryRecord {
            id: row.get(0),
            content: row.get(1),
            similarity: row.get(2),
        })
        .collect();

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::estimate_cost;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn test_estimate_cost_claude_opus() {
        // 1M input + 1M output at $15/$75 per 1M → $90
        let cost = estimate_cost("claude-opus-4", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 90.0), "expected 90.0, got {cost}");
    }

    #[test]
    fn test_estimate_cost_claude_sonnet() {
        // 500k input + 500k output at $3/$15 per 1M → $9
        let cost = estimate_cost("claude-sonnet-3-5", 500_000, 500_000);
        assert!(approx_eq(cost, 9.0), "expected 9.0, got {cost}");
    }

    #[test]
    fn test_estimate_cost_claude_haiku() {
        // 1M input + 1M output at $0.25/$1.25 per 1M → $1.50
        let cost = estimate_cost("claude-haiku-3", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 1.5), "expected 1.5, got {cost}");
    }

    #[test]
    fn test_estimate_cost_gpt4o() {
        // 1M input + 1M output at $2.5/$10 per 1M → $12.5
        let cost = estimate_cost("gpt-4o-mini", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 12.5), "expected 12.5, got {cost}");
    }

    #[test]
    fn test_estimate_cost_gpt4() {
        // 1M input + 1M output at $30/$60 per 1M → $90
        let cost = estimate_cost("gpt-4-turbo", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 90.0), "expected 90.0, got {cost}");
    }

    #[test]
    fn test_estimate_cost_unknown_model() {
        // 1M input + 1M output at $1/$2 per 1M → $3
        let cost = estimate_cost("unknown-model-x", 1_000_000, 1_000_000);
        assert!(approx_eq(cost, 3.0), "expected 3.0, got {cost}");
    }

    #[test]
    fn test_estimate_cost_zero_tokens() {
        let cost = estimate_cost("claude-sonnet-4", 0, 0);
        assert!(approx_eq(cost, 0.0), "expected 0.0, got {cost}");
    }

    #[test]
    fn test_estimate_cost_only_input_tokens() {
        // 1M input, 0 output at $3/$15 per 1M → $3
        let cost = estimate_cost("claude-sonnet-4-5", 1_000_000, 0);
        assert!(approx_eq(cost, 3.0), "expected 3.0, got {cost}");
    }
}
