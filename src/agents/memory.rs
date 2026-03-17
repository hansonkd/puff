use serde::{Deserialize, Serialize};

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
    let embedding_str: Option<String> = embedding.map(|e| {
        let inner: Vec<String> = e.iter().map(|v| v.to_string()).collect();
        format!("[{}]", inner.join(","))
    });

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

/// Recall memories using cosine similarity via pgvector.
pub async fn recall_memories(
    client: &tokio_postgres::Client,
    agent: &str,
    query_embedding: &[f32],
    k: u32,
    scope: Option<&str>,
    scope_id: Option<&str>,
) -> Result<Vec<MemoryRecord>, AgentError> {
    let inner: Vec<String> = query_embedding.iter().map(|v| v.to_string()).collect();
    let embedding_str = format!("[{}]", inner.join(","));

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
