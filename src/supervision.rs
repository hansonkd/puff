use crate::errors::{PuffResult, Result};
use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc};

pub const ROOT_NODE_ID: &str = "runtime-root";
const INITIAL_BACKOFF_MS: u64 = 250;
const MAX_BACKOFF_SECS: u64 = 30;
const EVENT_BUFFER: usize = 256;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorNodeKind {
    SystemRoot,
    Service,
    AgentSupervisor,
    AgentInstance,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorNodeState {
    Starting,
    Running,
    Degraded,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Permanent,
    Transient,
    Temporary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisionConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub admin_api: bool,
    #[serde(default)]
    pub event_stream: bool,
    #[serde(default = "default_max_dynamic_children")]
    pub max_dynamic_children: usize,
}

impl Default for SupervisionConfig {
    fn default() -> Self {
        Self {
            enable: false,
            admin_api: false,
            event_stream: false,
            max_dynamic_children: default_max_dynamic_children(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSupervisorSpec {
    pub name: String,
    pub supervisor_agent: String,
    #[serde(default)]
    pub workers: Vec<String>,
    #[serde(default = "default_true")]
    pub boot: bool,
    #[serde(default = "default_max_children")]
    pub max_children: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorNodeSnapshot {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub kind: SupervisorNodeKind,
    pub display_name: String,
    pub state: SupervisorNodeState,
    pub restart_policy: RestartPolicy,
    pub restart_count: u64,
    pub dynamic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_children: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SupervisorTreeSnapshot {
    pub root_id: String,
    pub config: SupervisionConfig,
    pub nodes: Vec<SupervisorNodeSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorEventKind {
    NodeAdded,
    NodeUpdated,
    RestartScheduled,
}

#[derive(Debug, Clone, Serialize)]
pub struct SupervisorEvent {
    pub kind: SupervisorEventKind,
    pub node: SupervisorNodeSnapshot,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentInstanceSpec {
    pub agent_name: String,
    pub display_name: String,
    pub metadata: serde_json::Value,
}

type ServiceTaskFactory =
    Arc<dyn Fn() -> BoxFuture<'static, PuffResult<()>> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct SupervisorRuntime {
    inner: Arc<SupervisorRuntimeInner>,
}

struct SupervisorRuntimeInner {
    handle: Handle,
    config: SupervisionConfig,
    nodes: RwLock<HashMap<String, NodeRecord>>,
    events: broadcast::Sender<SupervisorEvent>,
}

struct NodeRecord {
    snapshot: SupervisorNodeSnapshot,
    command_tx: Option<mpsc::Sender<NodeCommand>>,
}

#[derive(Debug)]
pub enum SupervisorCommand {
    Stop,
    Restart,
}

type NodeCommand = SupervisorCommand;

impl SupervisorRuntime {
    pub fn new(handle: Handle, config: SupervisionConfig) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let now = Utc::now();
        let mut nodes = HashMap::new();
        nodes.insert(
            ROOT_NODE_ID.to_string(),
            NodeRecord {
                snapshot: SupervisorNodeSnapshot {
                    node_id: ROOT_NODE_ID.to_string(),
                    parent_id: None,
                    kind: SupervisorNodeKind::SystemRoot,
                    display_name: "Runtime Root".to_string(),
                    state: SupervisorNodeState::Running,
                    restart_policy: RestartPolicy::Permanent,
                    restart_count: 0,
                    dynamic: false,
                    configured_agent: None,
                    supervisor_agent: None,
                    workers: Vec::new(),
                    max_children: None,
                    last_error: None,
                    last_failure_at: None,
                    started_at: Some(now),
                    updated_at: now,
                    children: Vec::new(),
                    metadata: serde_json::Value::Null,
                },
                command_tx: None,
            },
        );
        Self {
            inner: Arc::new(SupervisorRuntimeInner {
                handle,
                config,
                nodes: RwLock::new(nodes),
                events,
            }),
        }
    }

    pub fn config(&self) -> SupervisionConfig {
        self.inner.config.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.inner.events.subscribe()
    }

    pub fn set_node_state(
        &self,
        node_id: &str,
        state: SupervisorNodeState,
        message: Option<String>,
    ) {
        self.transition_node(node_id, state, message);
    }

    pub fn tree_snapshot(&self) -> SupervisorTreeSnapshot {
        let mut nodes = self
            .inner
            .nodes
            .read()
            .unwrap()
            .values()
            .map(|record| record.snapshot.clone())
            .collect::<Vec<_>>();
        nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        SupervisorTreeSnapshot {
            root_id: ROOT_NODE_ID.to_string(),
            config: self.inner.config.clone(),
            nodes,
        }
    }

    pub fn node_snapshot(&self, node_id: &str) -> Option<SupervisorNodeSnapshot> {
        self.inner
            .nodes
            .read()
            .unwrap()
            .get(node_id)
            .map(|record| record.snapshot.clone())
    }

    pub fn register_service_task<F>(
        &self,
        task_name: &str,
        display_name: &str,
        factory: F,
    ) -> Result<SupervisorNodeSnapshot>
    where
        F: Fn() -> BoxFuture<'static, PuffResult<()>> + Send + Sync + 'static,
    {
        self.spawn_service_task(
            format!("service:{}", sanitize_node_fragment(task_name)),
            display_name.to_string(),
            RestartPolicy::Permanent,
            Arc::new(factory),
        )
    }

    pub fn ensure_static_agent_supervisor(
        &self,
        spec: AgentSupervisorSpec,
    ) -> Result<SupervisorNodeSnapshot> {
        let node_id = format!("agent-supervisor:{}", sanitize_node_fragment(&spec.name));
        if let Some(snapshot) = self.node_snapshot(&node_id) {
            return Ok(snapshot);
        }

        self.insert_node(
            ROOT_NODE_ID,
            SupervisorNodeSnapshot {
                node_id,
                parent_id: Some(ROOT_NODE_ID.to_string()),
                kind: SupervisorNodeKind::AgentSupervisor,
                display_name: spec.name.clone(),
                state: SupervisorNodeState::Running,
                restart_policy: RestartPolicy::Permanent,
                restart_count: 0,
                dynamic: false,
                configured_agent: None,
                supervisor_agent: Some(spec.supervisor_agent),
                workers: spec.workers,
                max_children: Some(spec.max_children),
                last_error: None,
                last_failure_at: None,
                started_at: Some(Utc::now()),
                updated_at: Utc::now(),
                children: Vec::new(),
                metadata: serde_json::Value::Null,
            },
            None,
        )
    }

    pub fn spawn_agent_supervisor(
        &self,
        parent_id: &str,
        spec: AgentSupervisorSpec,
        metadata: serde_json::Value,
    ) -> Result<SupervisorNodeSnapshot> {
        self.ensure_parent_allows_child(parent_id, SupervisorNodeKind::AgentSupervisor)?;
        self.enforce_dynamic_limit()?;

        self.insert_node(
            parent_id,
            SupervisorNodeSnapshot {
                node_id: format!("agent-supervisor:{}", uuid::Uuid::new_v4()),
                parent_id: Some(parent_id.to_string()),
                kind: SupervisorNodeKind::AgentSupervisor,
                display_name: spec.name,
                state: SupervisorNodeState::Running,
                restart_policy: RestartPolicy::Transient,
                restart_count: 0,
                dynamic: true,
                configured_agent: None,
                supervisor_agent: Some(spec.supervisor_agent),
                workers: spec.workers,
                max_children: Some(spec.max_children),
                last_error: None,
                last_failure_at: None,
                started_at: Some(Utc::now()),
                updated_at: Utc::now(),
                children: Vec::new(),
                metadata,
            },
            None,
        )
    }

    pub fn spawn_agent_instance(
        &self,
        parent_id: &str,
        spec: AgentInstanceSpec,
    ) -> Result<SupervisorNodeSnapshot> {
        self.spawn_agent_instance_with_node_id(
            parent_id,
            format!("agent-instance:{}", uuid::Uuid::new_v4()),
            spec,
            None,
        )
    }

    pub fn spawn_managed_agent_instance(
        &self,
        parent_id: &str,
        spec: AgentInstanceSpec,
    ) -> Result<(SupervisorNodeSnapshot, mpsc::Receiver<SupervisorCommand>)> {
        let (command_tx, command_rx) = mpsc::channel(8);
        let snapshot = self.spawn_agent_instance_with_node_id(
            parent_id,
            format!("agent-instance:{}", uuid::Uuid::new_v4()),
            spec,
            Some(command_tx),
        )?;
        Ok((snapshot, command_rx))
    }

    pub fn spawn_named_managed_agent_instance(
        &self,
        node_id: String,
        parent_id: &str,
        spec: AgentInstanceSpec,
    ) -> Result<(SupervisorNodeSnapshot, mpsc::Receiver<SupervisorCommand>)> {
        let (command_tx, command_rx) = mpsc::channel(8);
        let snapshot =
            self.spawn_agent_instance_with_node_id(parent_id, node_id, spec, Some(command_tx))?;
        Ok((snapshot, command_rx))
    }

    fn spawn_agent_instance_with_node_id(
        &self,
        parent_id: &str,
        node_id: String,
        spec: AgentInstanceSpec,
        command_tx: Option<mpsc::Sender<NodeCommand>>,
    ) -> Result<SupervisorNodeSnapshot> {
        self.ensure_parent_allows_child(parent_id, SupervisorNodeKind::AgentInstance)?;
        self.enforce_dynamic_limit()?;

        self.insert_node(
            parent_id,
            SupervisorNodeSnapshot {
                node_id,
                parent_id: Some(parent_id.to_string()),
                kind: SupervisorNodeKind::AgentInstance,
                display_name: spec.display_name,
                state: SupervisorNodeState::Running,
                restart_policy: RestartPolicy::Transient,
                restart_count: 0,
                dynamic: true,
                configured_agent: Some(spec.agent_name),
                supervisor_agent: None,
                workers: Vec::new(),
                max_children: None,
                last_error: None,
                last_failure_at: None,
                started_at: Some(Utc::now()),
                updated_at: Utc::now(),
                children: Vec::new(),
                metadata: spec.metadata,
            },
            command_tx,
        )
    }

    pub async fn stop_node(&self, node_id: &str) -> Result<SupervisorNodeSnapshot> {
        if node_id == ROOT_NODE_ID {
            anyhow::bail!("cannot stop the runtime root node");
        }

        let descendants = self.collect_descendants(node_id)?;
        for descendant in descendants.into_iter().rev() {
            let command = self.command_sender(&descendant);
            if let Some(sender) = command {
                sender
                    .send(NodeCommand::Stop)
                    .await
                    .map_err(|_| anyhow::anyhow!("node '{}' is no longer running", descendant))?;
            } else {
                self.transition_node(&descendant, SupervisorNodeState::Stopping, None);
                self.transition_node(&descendant, SupervisorNodeState::Stopped, None);
            }
        }

        self.node_snapshot(node_id)
            .ok_or_else(|| anyhow::anyhow!("node '{}' not found", node_id))
    }

    pub async fn restart_node(&self, node_id: &str) -> Result<SupervisorNodeSnapshot> {
        if node_id == ROOT_NODE_ID {
            anyhow::bail!("cannot restart the runtime root node");
        }

        let command = self.command_sender(node_id);
        if let Some(sender) = command {
            sender
                .send(NodeCommand::Restart)
                .await
                .map_err(|_| anyhow::anyhow!("node '{}' is no longer running", node_id))?;
        } else {
            self.transition_node(node_id, SupervisorNodeState::Starting, None);
            self.bump_restart_count(node_id);
            self.transition_node(node_id, SupervisorNodeState::Running, None);
        }

        self.node_snapshot(node_id)
            .ok_or_else(|| anyhow::anyhow!("node '{}' not found", node_id))
    }

    fn spawn_service_task(
        &self,
        node_id: String,
        display_name: String,
        restart_policy: RestartPolicy,
        factory: ServiceTaskFactory,
    ) -> Result<SupervisorNodeSnapshot> {
        if let Some(snapshot) = self.node_snapshot(&node_id) {
            return Ok(snapshot);
        }

        let now = Utc::now();
        let (command_tx, mut command_rx) = mpsc::channel(8);
        let snapshot = self.insert_node(
            ROOT_NODE_ID,
            SupervisorNodeSnapshot {
                node_id: node_id.clone(),
                parent_id: Some(ROOT_NODE_ID.to_string()),
                kind: SupervisorNodeKind::Service,
                display_name,
                state: SupervisorNodeState::Starting,
                restart_policy,
                restart_count: 0,
                dynamic: false,
                configured_agent: None,
                supervisor_agent: None,
                workers: Vec::new(),
                max_children: None,
                last_error: None,
                last_failure_at: None,
                started_at: Some(now),
                updated_at: now,
                children: Vec::new(),
                metadata: serde_json::Value::Null,
            },
            Some(command_tx),
        )?;

        let runtime = self.clone();
        let handle = self.inner.handle.clone();
        self.inner.handle.spawn(async move {
            let mut backoff = Duration::from_millis(INITIAL_BACKOFF_MS);
            loop {
                runtime.transition_node(&node_id, SupervisorNodeState::Starting, None);
                let mut task = handle.spawn(factory());
                runtime.transition_node(&node_id, SupervisorNodeState::Running, None);

                enum ServiceOutcome {
                    Command(Option<NodeCommand>),
                    Exit(std::result::Result<PuffResult<()>, tokio::task::JoinError>),
                }

                let outcome = tokio::select! {
                    command = command_rx.recv() => ServiceOutcome::Command(command),
                    result = &mut task => ServiceOutcome::Exit(result),
                };

                match outcome {
                    ServiceOutcome::Command(Some(NodeCommand::Stop))
                    | ServiceOutcome::Command(None) => {
                        runtime.transition_node(&node_id, SupervisorNodeState::Stopping, None);
                        task.abort();
                        let _ = task.await;
                        runtime.transition_node(&node_id, SupervisorNodeState::Stopped, None);
                        break;
                    }
                    ServiceOutcome::Command(Some(NodeCommand::Restart)) => {
                        runtime.transition_node(&node_id, SupervisorNodeState::Stopping, None);
                        task.abort();
                        let _ = task.await;
                        runtime.bump_restart_count(&node_id);
                        runtime.emit_event(
                            SupervisorEventKind::RestartScheduled,
                            &node_id,
                            Some("explicit restart requested".to_string()),
                        );
                        backoff = Duration::from_millis(INITIAL_BACKOFF_MS);
                        continue;
                    }
                    ServiceOutcome::Exit(Ok(Ok(()))) => {
                        let should_restart = matches!(restart_policy, RestartPolicy::Permanent);
                        if should_restart {
                            runtime.bump_restart_count(&node_id);
                            runtime.transition_node(
                                &node_id,
                                SupervisorNodeState::Degraded,
                                Some("service exited cleanly; restarting".to_string()),
                            );
                            runtime.emit_event(
                                SupervisorEventKind::RestartScheduled,
                                &node_id,
                                Some("service exited cleanly".to_string()),
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = next_backoff(backoff);
                            continue;
                        }

                        runtime.transition_node(&node_id, SupervisorNodeState::Stopped, None);
                        break;
                    }
                    ServiceOutcome::Exit(Ok(Err(error))) => {
                        let error_message = error.to_string();
                        runtime.record_failure(&node_id, error_message.clone());
                        let should_restart = !matches!(restart_policy, RestartPolicy::Temporary);
                        if should_restart {
                            runtime.bump_restart_count(&node_id);
                            runtime.emit_event(
                                SupervisorEventKind::RestartScheduled,
                                &node_id,
                                Some(error_message),
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = next_backoff(backoff);
                            continue;
                        }
                        break;
                    }
                    ServiceOutcome::Exit(Err(join_error)) => {
                        let error_message = join_error.to_string();
                        runtime.record_failure(&node_id, error_message.clone());
                        let should_restart = !matches!(restart_policy, RestartPolicy::Temporary);
                        if should_restart {
                            runtime.bump_restart_count(&node_id);
                            runtime.emit_event(
                                SupervisorEventKind::RestartScheduled,
                                &node_id,
                                Some(error_message),
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = next_backoff(backoff);
                            continue;
                        }
                        break;
                    }
                }
            }
        });

        Ok(snapshot)
    }

    fn insert_node(
        &self,
        parent_id: &str,
        snapshot: SupervisorNodeSnapshot,
        command_tx: Option<mpsc::Sender<NodeCommand>>,
    ) -> Result<SupervisorNodeSnapshot> {
        let mut nodes = self.inner.nodes.write().unwrap();
        if !nodes.contains_key(parent_id) {
            anyhow::bail!("parent node '{}' not found", parent_id);
        }
        if nodes.contains_key(&snapshot.node_id) {
            anyhow::bail!("node '{}' already exists", snapshot.node_id);
        }

        if let Some(parent) = nodes.get_mut(parent_id) {
            parent.snapshot.children.push(snapshot.node_id.clone());
            parent.snapshot.updated_at = Utc::now();
        }

        nodes.insert(
            snapshot.node_id.clone(),
            NodeRecord {
                snapshot: snapshot.clone(),
                command_tx,
            },
        );
        drop(nodes);

        self.emit_event(SupervisorEventKind::NodeAdded, &snapshot.node_id, None);
        Ok(snapshot)
    }

    fn command_sender(&self, node_id: &str) -> Option<mpsc::Sender<NodeCommand>> {
        self.inner
            .nodes
            .read()
            .unwrap()
            .get(node_id)
            .and_then(|record| record.command_tx.clone())
    }

    fn transition_node(&self, node_id: &str, state: SupervisorNodeState, message: Option<String>) {
        let now = Utc::now();
        let mut nodes = self.inner.nodes.write().unwrap();
        if let Some(record) = nodes.get_mut(node_id) {
            if matches!(state, SupervisorNodeState::Running) && record.snapshot.started_at.is_none()
            {
                record.snapshot.started_at = Some(now);
            }
            record.snapshot.state = state;
            record.snapshot.updated_at = now;
            if let Some(ref msg) = message {
                record.snapshot.last_error = Some(msg.clone());
            }
        }
        drop(nodes);
        self.emit_event(SupervisorEventKind::NodeUpdated, node_id, message);
    }

    pub(crate) fn bump_restart_count(&self, node_id: &str) {
        let mut nodes = self.inner.nodes.write().unwrap();
        if let Some(record) = nodes.get_mut(node_id) {
            record.snapshot.restart_count += 1;
            record.snapshot.updated_at = Utc::now();
        }
    }

    pub(crate) fn record_failure(&self, node_id: &str, error: String) {
        let now = Utc::now();
        let mut nodes = self.inner.nodes.write().unwrap();
        if let Some(record) = nodes.get_mut(node_id) {
            record.snapshot.state = SupervisorNodeState::Failed;
            record.snapshot.last_error = Some(error.clone());
            record.snapshot.last_failure_at = Some(now);
            record.snapshot.updated_at = now;
        }
        drop(nodes);
        self.emit_event(SupervisorEventKind::NodeUpdated, node_id, Some(error));
    }

    fn emit_event(&self, kind: SupervisorEventKind, node_id: &str, message: Option<String>) {
        let Some(node) = self.node_snapshot(node_id) else {
            return;
        };
        let _ = self.inner.events.send(SupervisorEvent {
            kind,
            node,
            timestamp: Utc::now(),
            message,
        });
    }

    fn collect_descendants(&self, node_id: &str) -> Result<Vec<String>> {
        let nodes = self.inner.nodes.read().unwrap();
        if !nodes.contains_key(node_id) {
            anyhow::bail!("node '{}' not found", node_id);
        }

        let mut stack = vec![node_id.to_string()];
        let mut descendants = Vec::new();
        while let Some(current) = stack.pop() {
            descendants.push(current.clone());
            if let Some(record) = nodes.get(&current) {
                for child in &record.snapshot.children {
                    stack.push(child.clone());
                }
            }
        }
        Ok(descendants)
    }

    fn ensure_parent_allows_child(
        &self,
        parent_id: &str,
        child_kind: SupervisorNodeKind,
    ) -> Result<()> {
        let nodes = self.inner.nodes.read().unwrap();
        let parent = nodes
            .get(parent_id)
            .ok_or_else(|| anyhow::anyhow!("parent node '{}' not found", parent_id))?;

        if let Some(limit) = parent.snapshot.max_children {
            if parent.snapshot.children.len() >= limit {
                anyhow::bail!("parent node '{}' reached max_children={}", parent_id, limit);
            }
        }

        match (&parent.snapshot.kind, child_kind) {
            (SupervisorNodeKind::SystemRoot, _)
            | (SupervisorNodeKind::AgentSupervisor, SupervisorNodeKind::AgentSupervisor)
            | (SupervisorNodeKind::AgentSupervisor, SupervisorNodeKind::AgentInstance) => Ok(()),
            _ => anyhow::bail!(
                "node '{}' of kind '{:?}' cannot spawn child kind '{:?}'",
                parent_id,
                parent.snapshot.kind,
                child_kind
            ),
        }
    }

    fn enforce_dynamic_limit(&self) -> Result<()> {
        let nodes = self.inner.nodes.read().unwrap();
        let current_dynamic = nodes
            .values()
            .filter(|record| record.snapshot.dynamic)
            .count();
        if current_dynamic >= self.inner.config.max_dynamic_children {
            anyhow::bail!(
                "dynamic child limit reached ({})",
                self.inner.config.max_dynamic_children
            );
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_max_children() -> usize {
    64
}

fn default_max_dynamic_children() -> usize {
    256
}

fn next_backoff(current: Duration) -> Duration {
    let max = Duration::from_secs(MAX_BACKOFF_SECS);
    let doubled = current.saturating_mul(2);
    if doubled > max {
        max
    } else {
        doubled
    }
}

fn sanitize_node_fragment(input: &str) -> String {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn service_restarts_after_failure() {
        let runtime = SupervisorRuntime::new(Handle::current(), SupervisionConfig::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_inner = attempts.clone();

        runtime
            .register_service_task("test-service", "Test Service", move || {
                let attempts = attempts_inner.clone();
                Box::pin(async move {
                    let current = attempts.fetch_add(1, Ordering::SeqCst);
                    if current == 0 {
                        anyhow::bail!("boom");
                    }
                    std::future::pending::<()>().await;
                    #[allow(unreachable_code)]
                    Ok(())
                })
            })
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = runtime
                    .node_snapshot("service:test-service")
                    .expect("snapshot");
                if snapshot.restart_count >= 1 && snapshot.state == SupervisorNodeState::Running {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("service should restart");

        assert!(attempts.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn dynamic_children_respect_parent_limits() {
        let runtime = SupervisorRuntime::new(
            Handle::current(),
            SupervisionConfig {
                enable: true,
                admin_api: true,
                event_stream: true,
                max_dynamic_children: 8,
            },
        );

        let supervisor = runtime
            .spawn_agent_supervisor(
                ROOT_NODE_ID,
                AgentSupervisorSpec {
                    name: "dynamic-root".to_string(),
                    supervisor_agent: "planner".to_string(),
                    workers: vec!["worker".to_string()],
                    boot: true,
                    max_children: 1,
                },
                serde_json::Value::Null,
            )
            .unwrap();

        runtime
            .spawn_agent_instance(
                &supervisor.node_id,
                AgentInstanceSpec {
                    agent_name: "worker".to_string(),
                    display_name: "worker-1".to_string(),
                    metadata: serde_json::Value::Null,
                },
            )
            .unwrap();

        let err = runtime
            .spawn_agent_instance(
                &supervisor.node_id,
                AgentInstanceSpec {
                    agent_name: "worker".to_string(),
                    display_name: "worker-2".to_string(),
                    metadata: serde_json::Value::Null,
                },
            )
            .unwrap_err();

        assert!(err.to_string().contains("max_children"));
    }
}
