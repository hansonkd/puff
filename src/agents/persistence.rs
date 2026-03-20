use crate::agents::conversation::Conversation;
use crate::databases::postgres::PostgresClient;
use anyhow::Result;
use tokio::sync::OnceCell;

static CONVERSATION_TABLE_READY: OnceCell<()> = OnceCell::const_new();

const CONVERSATION_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS puff_agent_conversations (
    registry_key TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    conversation_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
)
"#;

pub async fn load_conversation_snapshot(
    postgres: &PostgresClient,
    registry_key: &str,
) -> Result<Option<Conversation>> {
    ensure_conversation_table(postgres).await?;
    let pool = postgres.pool();
    let conn = pool.get().await?;
    let row = conn
        .query_opt(
            "SELECT conversation_json FROM puff_agent_conversations WHERE registry_key = $1",
            &[&registry_key],
        )
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let value: serde_json::Value = row.get(0);
    Ok(Some(serde_json::from_value(value)?))
}

pub async fn save_conversation_snapshot(
    postgres: &PostgresClient,
    registry_key: &str,
    node_id: &str,
    agent_name: &str,
    conversation: &Conversation,
) -> Result<()> {
    ensure_conversation_table(postgres).await?;
    let pool = postgres.pool();
    let conn = pool.get().await?;
    let conversation_json = serde_json::to_value(conversation)?;
    conn.execute(
        r#"
        INSERT INTO puff_agent_conversations (
            registry_key,
            node_id,
            agent_name,
            conversation_json,
            created_at,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (registry_key) DO UPDATE
        SET node_id = EXCLUDED.node_id,
            agent_name = EXCLUDED.agent_name,
            conversation_json = EXCLUDED.conversation_json,
            updated_at = EXCLUDED.updated_at
        "#,
        &[
            &registry_key,
            &node_id,
            &agent_name,
            &conversation_json,
            &conversation.created_at,
            &conversation.updated_at,
        ],
    )
    .await?;
    Ok(())
}

async fn ensure_conversation_table(postgres: &PostgresClient) -> Result<()> {
    CONVERSATION_TABLE_READY
        .get_or_try_init(|| async {
            let pool = postgres.pool();
            let conn = pool.get().await?;
            conn.batch_execute(CONVERSATION_TABLE_SQL).await?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map(|_| ())
}
