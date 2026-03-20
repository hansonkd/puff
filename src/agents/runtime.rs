use crate::agents::agent::Agent;
use crate::agents::llm::LlmClient;
use crate::agents::mailbox::spawn_supervised_agent_mailbox;
use crate::agents::registry::{AgentRegistry, AgentRegistryEntry, AgentRegistryKind};
use crate::databases::postgres::PostgresClient;
use crate::supervision::{AgentSupervisorSpec, SupervisorNodeSnapshot, SupervisorRuntime};
use anyhow::Result;
use std::time::Duration;

const NAMED_AGENT_SUPERVISOR_NAME: &str = "named-agents";
const NAMED_AGENT_SUPERVISOR_AGENT: &str = "__runtime__";
const NAMED_AGENT_REGISTRATION_POLL_MS: u64 = 100;
const NAMED_AGENT_REGISTRATION_TIMEOUT_SECS: u64 = 5;

pub fn ensure_named_agent_supervisor(
    supervisor: &SupervisorRuntime,
) -> Result<SupervisorNodeSnapshot> {
    supervisor.ensure_static_agent_supervisor(AgentSupervisorSpec {
        name: NAMED_AGENT_SUPERVISOR_NAME.to_string(),
        supervisor_agent: NAMED_AGENT_SUPERVISOR_AGENT.to_string(),
        workers: Vec::new(),
        boot: true,
        max_children: 1024,
    })
}

pub async fn register_named_agent(
    supervisor: &SupervisorRuntime,
    registry: &AgentRegistry,
    agent: Agent,
    llm_client: LlmClient,
    postgres: Option<PostgresClient>,
) -> Result<AgentRegistryEntry> {
    let supervisor_snapshot = ensure_named_agent_supervisor(supervisor)?;
    registry.register_supervisor_entry(
        AgentRegistryEntry::new(
            format!("supervisor/{}", supervisor_snapshot.node_id),
            AgentRegistryKind::AgentSupervisor,
            supervisor_snapshot
                .supervisor_agent
                .clone()
                .unwrap_or_else(|| supervisor_snapshot.display_name.clone()),
        )
        .with_supervisor_node_id(supervisor_snapshot.node_id.clone())
        .with_metadata(serde_json::json!({
            "display_name": supervisor_snapshot.display_name,
            "workers": supervisor_snapshot.workers,
            "max_children": supervisor_snapshot.max_children,
            "internal": true,
            "owner_id": registry.owner_id(),
        })),
        supervisor,
    );
    let key = format!("agent/{}", agent.config.name);

    if let Some(existing) = registry.get(&key) {
        return Ok(existing);
    }

    if !registry.try_claim_named_agent(&key).await? {
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(NAMED_AGENT_REGISTRATION_TIMEOUT_SECS);
        loop {
            if let Some(existing) = registry.resolve_async(&key).await {
                return Ok(existing);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "named agent '{}' is leased by another runtime but did not register in time",
                    agent.config.name
                );
            }
            tokio::time::sleep(Duration::from_millis(NAMED_AGENT_REGISTRATION_POLL_MS)).await;
        }
    }

    let node_id = named_agent_node_id(&agent.config.name);
    let (snapshot, mailbox) = spawn_supervised_agent_mailbox(
        supervisor,
        &supervisor_snapshot.node_id,
        key.clone(),
        Some(node_id),
        agent.clone(),
        llm_client,
        postgres,
        serde_json::json!({
            "display_name": agent.config.name,
            "configured": true,
            "model": agent.config.model,
            "owner_id": registry.owner_id(),
        }),
    )?;

    Ok(registry.register_mailbox(
        AgentRegistryEntry::new(
            key,
            AgentRegistryKind::AgentInstance,
            agent.config.name.clone(),
        )
        .with_supervisor_node_id(snapshot.node_id.clone())
        .with_metadata(serde_json::json!({
            "display_name": snapshot.display_name,
            "state": snapshot.state,
            "parent_id": snapshot.parent_id,
            "configured": true,
            "model": agent.config.model,
            "owner_id": registry.owner_id(),
        })),
        mailbox,
        supervisor,
    ))
}

fn named_agent_node_id(agent_name: &str) -> String {
    format!("agent-instance:named-{}", sanitize_fragment(agent_name))
}

fn sanitize_fragment(input: &str) -> String {
    let mut sanitized = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_agent_node_id_is_sanitized() {
        assert_eq!(
            named_agent_node_id("planner/main"),
            "agent-instance:named-planner_main"
        );
    }
}
