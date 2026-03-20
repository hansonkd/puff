use crate::agents::conversation::Conversation;
use crate::agents::error::AgentError;
use crate::agents::mailbox::{AgentMailbox, AgentMessageResponse};
use crate::agents::persistence::load_conversation_snapshot;
use crate::databases::postgres::PostgresClient;
use crate::databases::redis::RedisClient;
use crate::supervision::SupervisorRuntime;
use bb8_redis::redis::{AsyncCommands, Cmd};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const REGISTRY_HASH_KEY: &str = "puff:agents:registry:v1";
const NAMED_AGENT_LEASE_TTL_SECS: u64 = 30;
const NAMED_AGENT_LEASE_RENEW_SECS: u64 = 10;
const REPLY_TTL_SECS: u64 = 60;
const REMOTE_REPLY_POLL_MS: u64 = 50;
const REMOTE_REPLY_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisMailboxRequest {
    request_id: String,
    target: String,
    kind: RedisMailboxRequestKind,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RedisMailboxRequestKind {
    SendUserMessage { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisMailboxReply {
    response: Option<AgentMessageResponse>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisControlRequest {
    request_id: String,
    target: String,
    kind: RedisControlRequestKind,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RedisControlRequestKind {
    Stop,
    Restart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedisControlReply {
    snapshot: Option<crate::supervision::SupervisorNodeSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRegistryKind {
    ConfiguredAgent,
    AgentSupervisor,
    AgentInstance,
    Service,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistryEntry {
    pub key: String,
    pub kind: AgentRegistryKind,
    pub agent_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor_node_id: Option<String>,
    pub registered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

impl AgentRegistryEntry {
    pub fn new(
        key: impl Into<String>,
        kind: AgentRegistryKind,
        agent_name: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            key: key.into(),
            kind,
            agent_name: agent_name.into(),
            supervisor_node_id: None,
            registered_at: now,
            updated_at: now,
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_supervisor_node_id(mut self, supervisor_node_id: impl Into<String>) -> Self {
        self.supervisor_node_id = Some(supervisor_node_id.into());
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[derive(Clone, Default)]
pub struct AgentRegistry {
    inner: Arc<RwLock<BTreeMap<String, AgentRegistryEntry>>>,
    controls: Arc<RwLock<HashMap<String, RegistryControl>>>,
    redis: Option<RedisClient>,
    postgres: Option<PostgresClient>,
    owner_id: Arc<String>,
    leased_keys: Arc<RwLock<HashSet<String>>>,
}

#[derive(Clone, Default)]
struct RegistryControl {
    mailbox: Option<AgentMailbox>,
    supervisor: Option<LocalSupervisorControl>,
    mailbox_bridge_started: bool,
    control_bridge_started: bool,
}

#[derive(Clone)]
struct LocalSupervisorControl {
    node_id: String,
    supervisor: SupervisorRuntime,
}

#[derive(Debug)]
pub enum AgentRegistryError {
    EntryNotFound(String),
    NotMessageable(String),
    MailboxUnavailable(String),
    RedisUnavailable(String),
    Redis(String),
    Agent(AgentError),
}

impl fmt::Display for AgentRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentRegistryError::EntryNotFound(target) => {
                write!(f, "registry entry '{}' not found", target)
            }
            AgentRegistryError::NotMessageable(target) => {
                write!(f, "registry entry '{}' does not accept messages", target)
            }
            AgentRegistryError::MailboxUnavailable(target) => {
                write!(f, "registry mailbox '{}' is unavailable", target)
            }
            AgentRegistryError::RedisUnavailable(detail) => write!(f, "{detail}"),
            AgentRegistryError::Redis(detail) => write!(f, "{detail}"),
            AgentRegistryError::Agent(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for AgentRegistryError {}

impl From<AgentError> for AgentRegistryError {
    fn from(value: AgentError) -> Self {
        Self::Agent(value)
    }
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::with_backends(None, None)
    }

    pub fn with_redis(redis: Option<RedisClient>) -> Self {
        Self::with_backends(redis, None)
    }

    pub fn with_backends(redis: Option<RedisClient>, postgres: Option<PostgresClient>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
            controls: Arc::new(RwLock::new(HashMap::new())),
            redis,
            postgres,
            owner_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            leased_keys: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub fn owner_id(&self) -> &str {
        self.owner_id.as_str()
    }

    pub fn register(&self, mut entry: AgentRegistryEntry) -> AgentRegistryEntry {
        let mut inner = self.inner.write().unwrap();
        let now = Utc::now();
        if let Some(existing) = inner.get(&entry.key) {
            entry.registered_at = existing.registered_at;
        }
        entry.updated_at = now;
        inner.insert(entry.key.clone(), entry.clone());
        drop(inner);

        if let Some(redis) = self.redis.clone() {
            let entry_key = entry.key.clone();
            let entry_json = serde_json::to_string(&entry);
            tokio::spawn(async move {
                match entry_json {
                    Ok(payload) => {
                        let pool = redis.pool();
                        let conn_result = pool.get().await;
                        if let Ok(mut conn) = conn_result {
                            let _ = conn
                                .hset::<_, _, _, ()>(REGISTRY_HASH_KEY, &entry_key, payload)
                                .await;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            "Failed to serialize registry entry '{}': {}",
                            entry_key,
                            error
                        );
                    }
                }
            });
        }
        entry
    }

    pub fn unregister(&self, key: &str) -> Option<AgentRegistryEntry> {
        self.controls.write().unwrap().remove(key);
        self.leased_keys.write().unwrap().remove(key);
        let removed = self.inner.write().unwrap().remove(key);
        if removed.is_some() {
            if let Some(redis) = self.redis.clone() {
                let key = key.to_string();
                let owner_id = self.owner_id.to_string();
                tokio::spawn(async move {
                    {
                        let pool = redis.pool();
                        let conn_result = pool.get().await;
                        if let Ok(mut conn) = conn_result {
                            let _ = conn.hdel::<_, _, ()>(REGISTRY_HASH_KEY, &key).await;
                            let _ = conn
                                .del::<_, ()>(&[
                                    mailbox_conversation_key(&key),
                                    mailbox_pending_key(&key),
                                    mailbox_processing_key(&key),
                                    control_pending_key(&key),
                                    control_processing_key(&key),
                                ])
                                .await;
                            let lease_key = named_agent_lease_key(&key);
                            match conn.get::<_, Option<String>>(&lease_key).await {
                                Ok(Some(current_owner)) if current_owner == owner_id => {
                                    let _ = conn.del::<_, ()>(lease_key).await;
                                }
                                _ => {}
                            }
                        }
                    }
                });
            }
        }
        removed
    }

    pub fn get(&self, key: &str) -> Option<AgentRegistryEntry> {
        self.inner.read().unwrap().get(key).cloned()
    }

    pub fn resolve(&self, target: &str) -> Option<AgentRegistryEntry> {
        if let Some(entry) = self.get(target) {
            return Some(entry);
        }

        let normalized = format!("agent/{target}");
        if let Some(entry) = self.get(&normalized) {
            return Some(entry);
        }

        self.find_by_agent_name(target)
    }

    pub fn list(&self) -> Vec<AgentRegistryEntry> {
        self.inner.read().unwrap().values().cloned().collect()
    }

    pub fn scan_prefix(&self, prefix: &str) -> Vec<AgentRegistryEntry> {
        let inner = self.inner.read().unwrap();
        let mut matches = Vec::new();
        for (key, entry) in inner.range(prefix.to_string()..) {
            if !key.starts_with(prefix) {
                break;
            }
            matches.push(entry.clone());
        }
        matches
    }

    pub fn register_mailbox(
        &self,
        entry: AgentRegistryEntry,
        mailbox: AgentMailbox,
        supervisor: &SupervisorRuntime,
    ) -> AgentRegistryEntry {
        let key = entry.key.clone();
        let node_id = entry.supervisor_node_id.clone();
        let registered = self.register(entry);
        let mut start_mailbox_bridge = false;
        let mut start_control_bridge = false;
        {
            let mut controls = self.controls.write().unwrap();
            let control = controls.entry(key.clone()).or_default();
            control.mailbox = Some(mailbox);
            if let Some(node_id) = node_id {
                control.supervisor = Some(LocalSupervisorControl {
                    node_id,
                    supervisor: supervisor.clone(),
                });
            }
            if self.redis.is_some() && !control.mailbox_bridge_started {
                control.mailbox_bridge_started = true;
                start_mailbox_bridge = true;
            }
            if self.redis.is_some()
                && control.supervisor.is_some()
                && !control.control_bridge_started
            {
                control.control_bridge_started = true;
                start_control_bridge = true;
            }
        }
        if start_mailbox_bridge {
            self.spawn_remote_mailbox_bridge(key.clone());
        }
        if start_control_bridge {
            self.spawn_remote_control_bridge(key.clone());
        }
        registered
    }

    pub fn register_supervisor_entry(
        &self,
        entry: AgentRegistryEntry,
        supervisor: &SupervisorRuntime,
    ) -> AgentRegistryEntry {
        let key = entry.key.clone();
        let node_id = entry.supervisor_node_id.clone();
        let registered = self.register(entry);
        let mut start_control_bridge = false;
        {
            let mut controls = self.controls.write().unwrap();
            let control = controls.entry(key.clone()).or_default();
            if let Some(node_id) = node_id {
                control.supervisor = Some(LocalSupervisorControl {
                    node_id,
                    supervisor: supervisor.clone(),
                });
            }
            if self.redis.is_some()
                && control.supervisor.is_some()
                && !control.control_bridge_started
            {
                control.control_bridge_started = true;
                start_control_bridge = true;
            }
        }
        if start_control_bridge {
            self.spawn_remote_control_bridge(key);
        }
        registered
    }

    pub async fn try_claim_named_agent(&self, key: &str) -> Result<bool, AgentRegistryError> {
        let Some(redis) = self.redis.clone() else {
            return Ok(true);
        };
        let lease_key = named_agent_lease_key(key);
        let owner_id = self.owner_id.to_string();
        let pool = redis.pool();
        let mut conn = pool
            .get()
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;

        let claimed: Option<String> = Cmd::new()
            .arg("SET")
            .arg(&lease_key)
            .arg(&owner_id)
            .arg("NX")
            .arg("EX")
            .arg(NAMED_AGENT_LEASE_TTL_SECS)
            .query_async(&mut *conn)
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;

        if claimed.is_some() {
            self.start_named_agent_lease(key.to_string());
            return Ok(true);
        }

        let current_owner = conn
            .get::<_, Option<String>>(&lease_key)
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
        if current_owner.as_deref() == Some(owner_id.as_str()) {
            self.start_named_agent_lease(key.to_string());
            return Ok(true);
        }

        Ok(false)
    }

    pub fn find_by_agent_name(&self, agent_name: &str) -> Option<AgentRegistryEntry> {
        if let Some(entry) = self.get(&format!("agent/{agent_name}")) {
            return Some(entry);
        }

        self.inner
            .read()
            .unwrap()
            .values()
            .find(|entry| entry.agent_name == agent_name)
            .cloned()
    }

    pub fn scan_agent_names(&self, prefix: &str) -> Vec<AgentRegistryEntry> {
        self.inner
            .read()
            .unwrap()
            .values()
            .filter(|entry| entry.agent_name.starts_with(prefix))
            .cloned()
            .collect()
    }

    pub async fn send_message(
        &self,
        target: &str,
        message: impl Into<String>,
    ) -> Result<AgentMessageResponse, AgentRegistryError> {
        let key = self.resolve_message_target_async(target).await?;
        let message = message.into();
        match self.lookup_mailbox(&key) {
            Ok(mailbox) => mailbox.send_message(message).await.map_err(Into::into),
            Err(AgentRegistryError::NotMessageable(_))
            | Err(AgentRegistryError::MailboxUnavailable(_)) => {
                if self.redis.is_some() {
                    self.send_remote_message(&key, message).await
                } else {
                    Err(AgentRegistryError::NotMessageable(key))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub async fn conversation(&self, target: &str) -> Result<Conversation, AgentRegistryError> {
        let key = match self.resolve_message_target_async(target).await {
            Ok(key) => key,
            Err(AgentRegistryError::EntryNotFound(_)) if target.contains('/') => {
                return self.remote_conversation(target).await;
            }
            Err(error) => return Err(error),
        };
        match self.lookup_mailbox(&key) {
            Ok(mailbox) => mailbox.conversation().await.map_err(Into::into),
            Err(AgentRegistryError::NotMessageable(_))
            | Err(AgentRegistryError::MailboxUnavailable(_)) => {
                if self.redis.is_some() {
                    self.remote_conversation(&key).await
                } else {
                    Err(AgentRegistryError::NotMessageable(key))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub async fn stop_target(
        &self,
        target: &str,
    ) -> Result<crate::supervision::SupervisorNodeSnapshot, AgentRegistryError> {
        self.control_target(target, RedisControlRequestKind::Stop)
            .await
    }

    pub async fn restart_target(
        &self,
        target: &str,
    ) -> Result<crate::supervision::SupervisorNodeSnapshot, AgentRegistryError> {
        self.control_target(target, RedisControlRequestKind::Restart)
            .await
    }

    pub async fn list_entries(&self) -> Vec<AgentRegistryEntry> {
        match self.remote_scan("*").await {
            Ok(entries) if !entries.is_empty() => entries,
            _ => self.list(),
        }
    }

    pub async fn scan_prefix_async(&self, prefix: &str) -> Vec<AgentRegistryEntry> {
        let pattern = format!("{prefix}*");
        match self.remote_scan(&pattern).await {
            Ok(entries) if !entries.is_empty() => entries,
            _ => self.scan_prefix(prefix),
        }
    }

    pub async fn resolve_async(&self, target: &str) -> Option<AgentRegistryEntry> {
        if let Some(entry) = self.resolve(target) {
            return Some(entry);
        }

        if let Some(entry) = self.remote_get(target).await {
            return Some(entry);
        }

        let normalized = format!("agent/{target}");
        if let Some(entry) = self.remote_get(&normalized).await {
            return Some(entry);
        }

        self.remote_find_by_agent_name(target).await
    }

    async fn resolve_message_target_async(
        &self,
        target: &str,
    ) -> Result<String, AgentRegistryError> {
        self.resolve_async(target)
            .await
            .map(|entry| entry.key)
            .ok_or_else(|| AgentRegistryError::EntryNotFound(target.to_string()))
    }

    fn lookup_mailbox(&self, key: &str) -> Result<AgentMailbox, AgentRegistryError> {
        let controls = self.controls.read().unwrap();
        match controls
            .get(key)
            .and_then(|control| control.mailbox.clone())
        {
            Some(mailbox) => Ok(mailbox),
            None if self.inner.read().unwrap().contains_key(key) => {
                Err(AgentRegistryError::NotMessageable(key.to_string()))
            }
            None => Err(AgentRegistryError::MailboxUnavailable(key.to_string())),
        }
    }

    fn spawn_remote_mailbox_bridge(&self, key: String) {
        let Some(redis) = self.redis.clone() else {
            return;
        };
        let Some(mailbox) = self
            .controls
            .read()
            .unwrap()
            .get(&key)
            .and_then(|control| control.mailbox.clone())
        else {
            return;
        };

        tokio::spawn(async move {
            if let Err(error) = recover_processing_queue(
                &redis,
                &mailbox_processing_key(&key),
                &mailbox_pending_key(&key),
            )
            .await
            {
                tracing::warn!("Failed to recover Redis mailbox queue '{}': {}", key, error);
            }

            if let Ok(conversation) = mailbox.conversation().await {
                let _ = store_remote_conversation(&redis, &key, &conversation).await;
            }

            loop {
                let payload = match pop_remote_request(&redis, &key).await {
                    Ok(payload) => payload,
                    Err(error) => {
                        tracing::warn!("Redis mailbox bridge '{}' pop failed: {}", key, error);
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                };

                let Some(payload) = payload else {
                    continue;
                };

                let request: RedisMailboxRequest = match serde_json::from_str(&payload) {
                    Ok(request) => request,
                    Err(error) => {
                        tracing::warn!(
                            "Redis mailbox bridge '{}' got invalid payload: {}",
                            key,
                            error
                        );
                        let _ = ack_remote_request(&redis, &key, &payload).await;
                        continue;
                    }
                };

                let reply = match request.kind {
                    RedisMailboxRequestKind::SendUserMessage { message } => {
                        match mailbox.send_message(message).await {
                            Ok(response) => {
                                if let Ok(conversation) = mailbox.conversation().await {
                                    let _ = store_remote_conversation(&redis, &key, &conversation)
                                        .await;
                                }
                                RedisMailboxReply {
                                    response: Some(response),
                                    error: None,
                                }
                            }
                            Err(error) => RedisMailboxReply {
                                response: None,
                                error: Some(error.to_string()),
                            },
                        }
                    }
                };

                let _ = write_remote_reply(&redis, &request.request_id, &reply).await;
                let _ = ack_remote_request(&redis, &key, &payload).await;
            }
        });
    }

    fn spawn_remote_control_bridge(&self, key: String) {
        let Some(redis) = self.redis.clone() else {
            return;
        };
        let registry = self.clone();
        let Some(control) = self
            .controls
            .read()
            .unwrap()
            .get(&key)
            .and_then(|control| control.supervisor.clone())
        else {
            return;
        };

        tokio::spawn(async move {
            if let Err(error) = recover_processing_queue(
                &redis,
                &control_processing_key(&key),
                &control_pending_key(&key),
            )
            .await
            {
                tracing::warn!("Failed to recover Redis control queue '{}': {}", key, error);
            }

            loop {
                let payload = match pop_remote_payload(
                    &redis,
                    &control_pending_key(&key),
                    &control_processing_key(&key),
                )
                .await
                {
                    Ok(payload) => payload,
                    Err(error) => {
                        tracing::warn!("Redis control bridge '{}' pop failed: {}", key, error);
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                };

                let Some(payload) = payload else {
                    continue;
                };

                let request: RedisControlRequest = match serde_json::from_str(&payload) {
                    Ok(request) => request,
                    Err(error) => {
                        tracing::warn!(
                            "Redis control bridge '{}' got invalid payload: {}",
                            key,
                            error
                        );
                        let _ = ack_remote_payload(&redis, &control_processing_key(&key), &payload)
                            .await;
                        continue;
                    }
                };

                let reply = match request.kind {
                    RedisControlRequestKind::Stop => {
                        match control.supervisor.stop_node(&control.node_id).await {
                            Ok(snapshot) => RedisControlReply {
                                snapshot: Some(snapshot),
                                error: None,
                            },
                            Err(error) => RedisControlReply {
                                snapshot: None,
                                error: Some(error.to_string()),
                            },
                        }
                    }
                    RedisControlRequestKind::Restart => {
                        match control.supervisor.restart_node(&control.node_id).await {
                            Ok(snapshot) => RedisControlReply {
                                snapshot: Some(snapshot),
                                error: None,
                            },
                            Err(error) => RedisControlReply {
                                snapshot: None,
                                error: Some(error.to_string()),
                            },
                        }
                    }
                };

                if reply.error.is_none() && matches!(request.kind, RedisControlRequestKind::Stop) {
                    registry.unregister(&key);
                }

                let _ = write_remote_control_reply(&redis, &request.request_id, &reply).await;
                let _ = ack_remote_payload(&redis, &control_processing_key(&key), &payload).await;
            }
        });
    }

    async fn send_remote_message(
        &self,
        key: &str,
        message: String,
    ) -> Result<AgentMessageResponse, AgentRegistryError> {
        let redis = self.redis.clone().ok_or_else(|| {
            AgentRegistryError::RedisUnavailable(format!(
                "registry target '{}' is not local and Redis transport is not configured",
                key
            ))
        })?;
        let request = RedisMailboxRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            target: key.to_string(),
            kind: RedisMailboxRequestKind::SendUserMessage { message },
            created_at: Utc::now(),
        };
        push_remote_request(&redis, key, &request).await?;
        let reply = wait_for_remote_reply(&redis, &request.request_id).await?;
        match (reply.response, reply.error) {
            (Some(response), None) => Ok(response),
            (_, Some(error)) => Err(AgentRegistryError::Redis(error)),
            _ => Err(AgentRegistryError::Redis(format!(
                "remote mailbox '{}' returned an empty reply",
                key
            ))),
        }
    }

    async fn remote_conversation(&self, key: &str) -> Result<Conversation, AgentRegistryError> {
        if let Some(redis) = self.redis.clone() {
            match load_remote_conversation(&redis, key).await {
                Ok(conversation) => return Ok(conversation),
                Err(AgentRegistryError::EntryNotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(postgres) = self.postgres.clone() {
            return load_conversation_snapshot(&postgres, key)
                .await
                .map_err(|error| AgentRegistryError::Redis(error.to_string()))?
                .ok_or_else(|| AgentRegistryError::EntryNotFound(key.to_string()));
        }
        Err(AgentRegistryError::RedisUnavailable(format!(
            "conversation '{}' is not local and no durable backend is configured",
            key
        )))
    }

    async fn remote_get(&self, key: &str) -> Option<AgentRegistryEntry> {
        let redis = self.redis.clone()?;
        let pool = redis.pool();
        let mut conn = pool.get().await.ok()?;
        let raw = conn
            .hget::<_, _, Option<String>>(REGISTRY_HASH_KEY, key)
            .await
            .ok()??;
        serde_json::from_str(&raw).ok()
    }

    async fn remote_find_by_agent_name(&self, agent_name: &str) -> Option<AgentRegistryEntry> {
        let entries = self.remote_scan("*").await.ok()?;
        entries
            .into_iter()
            .find(|entry| entry.agent_name == agent_name)
    }

    async fn remote_scan(
        &self,
        pattern: &str,
    ) -> Result<Vec<AgentRegistryEntry>, AgentRegistryError> {
        let redis = self.redis.clone().ok_or_else(|| {
            AgentRegistryError::RedisUnavailable(
                "Redis registry backend is not configured".to_string(),
            )
        })?;
        let pool = redis.pool();
        let mut conn = pool
            .get()
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
        let mut cursor: u64 = 0;
        let mut entries = Vec::new();

        loop {
            let (next_cursor, chunk): (u64, Vec<(String, String)>) = Cmd::new()
                .arg("HSCAN")
                .arg(REGISTRY_HASH_KEY)
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .query_async(&mut *conn)
                .await
                .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;

            for (_field, value) in chunk {
                if let Ok(entry) = serde_json::from_str::<AgentRegistryEntry>(&value) {
                    entries.push(entry);
                }
            }

            if next_cursor == 0 {
                break;
            }
            cursor = next_cursor;
        }

        Ok(entries)
    }

    async fn control_target(
        &self,
        target: &str,
        kind: RedisControlRequestKind,
    ) -> Result<crate::supervision::SupervisorNodeSnapshot, AgentRegistryError> {
        let entry = self
            .resolve_async(target)
            .await
            .ok_or_else(|| AgentRegistryError::EntryNotFound(target.to_string()))?;

        let local = {
            self.controls
                .read()
                .unwrap()
                .get(&entry.key)
                .and_then(|control| control.supervisor.clone())
        };
        if let Some(local) = local {
            let snapshot =
                match kind {
                    RedisControlRequestKind::Stop => local
                        .supervisor
                        .stop_node(&local.node_id)
                        .await
                        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?,
                    RedisControlRequestKind::Restart => local
                        .supervisor
                        .restart_node(&local.node_id)
                        .await
                        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?,
                };
            if matches!(kind, RedisControlRequestKind::Stop) {
                self.unregister(&entry.key);
            }
            return Ok(snapshot);
        }

        self.send_remote_control(&entry.key, kind).await
    }

    async fn send_remote_control(
        &self,
        key: &str,
        kind: RedisControlRequestKind,
    ) -> Result<crate::supervision::SupervisorNodeSnapshot, AgentRegistryError> {
        let redis = self.redis.clone().ok_or_else(|| {
            AgentRegistryError::RedisUnavailable(format!(
                "registry target '{}' is not local and Redis control transport is not configured",
                key
            ))
        })?;
        let request = RedisControlRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            target: key.to_string(),
            kind,
            created_at: Utc::now(),
        };
        push_remote_control_request(&redis, key, &request).await?;
        let reply = wait_for_remote_control_reply(&redis, &request.request_id).await?;
        match (reply.snapshot, reply.error) {
            (Some(snapshot), None) => Ok(snapshot),
            (_, Some(error)) => Err(AgentRegistryError::Redis(error)),
            _ => Err(AgentRegistryError::Redis(format!(
                "remote control '{}' returned an empty reply",
                key
            ))),
        }
    }

    fn start_named_agent_lease(&self, key: String) {
        let Some(redis) = self.redis.clone() else {
            return;
        };
        let owner_id = self.owner_id.to_string();
        let mut leased_keys = self.leased_keys.write().unwrap();
        if !leased_keys.insert(key.clone()) {
            return;
        }
        drop(leased_keys);

        let leased_keys = self.leased_keys.clone();
        tokio::spawn(async move {
            let lease_key = named_agent_lease_key(&key);
            loop {
                tokio::time::sleep(Duration::from_secs(NAMED_AGENT_LEASE_RENEW_SECS)).await;
                if !leased_keys.read().unwrap().contains(&key) {
                    break;
                }
                let pool = redis.pool();
                let mut conn = match pool.get().await {
                    Ok(conn) => conn,
                    Err(error) => {
                        tracing::warn!("Failed to renew named-agent lease '{}': {}", key, error);
                        continue;
                    }
                };
                match conn.get::<_, Option<String>>(&lease_key).await {
                    Ok(Some(current_owner)) if current_owner == owner_id => {
                        let _ = conn
                            .expire::<_, ()>(&lease_key, NAMED_AGENT_LEASE_TTL_SECS as i64)
                            .await;
                    }
                    Ok(Some(_)) | Ok(None) => {
                        leased_keys.write().unwrap().remove(&key);
                        break;
                    }
                    Err(error) => {
                        tracing::warn!("Failed to read named-agent lease '{}': {}", key, error);
                    }
                }
            }
        });
    }
}

fn mailbox_pending_key(key: &str) -> String {
    format!("puff:agents:mailbox:{key}:pending")
}

fn mailbox_processing_key(key: &str) -> String {
    format!("puff:agents:mailbox:{key}:processing")
}

fn mailbox_reply_key(request_id: &str) -> String {
    format!("puff:agents:reply:{request_id}")
}

fn mailbox_conversation_key(key: &str) -> String {
    format!("puff:agents:conversation:{key}")
}

fn named_agent_lease_key(key: &str) -> String {
    format!("puff:agents:lease:{key}")
}

fn control_pending_key(key: &str) -> String {
    format!("puff:agents:control:{key}:pending")
}

fn control_processing_key(key: &str) -> String {
    format!("puff:agents:control:{key}:processing")
}

fn control_reply_key(request_id: &str) -> String {
    format!("puff:agents:control-reply:{request_id}")
}

async fn push_remote_request(
    redis: &RedisClient,
    key: &str,
    request: &RedisMailboxRequest,
) -> Result<(), AgentRegistryError> {
    let payload = serde_json::to_string(request)
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    conn.lpush::<_, _, ()>(mailbox_pending_key(key), payload)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn pop_remote_request(
    redis: &RedisClient,
    key: &str,
) -> Result<Option<String>, AgentRegistryError> {
    pop_remote_payload(
        redis,
        &mailbox_pending_key(key),
        &mailbox_processing_key(key),
    )
    .await
}

async fn pop_remote_payload(
    redis: &RedisClient,
    pending_key: &str,
    processing_key: &str,
) -> Result<Option<String>, AgentRegistryError> {
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    Cmd::new()
        .arg("BRPOPLPUSH")
        .arg(pending_key)
        .arg(processing_key)
        .arg(1)
        .query_async(&mut *conn)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn ack_remote_request(
    redis: &RedisClient,
    key: &str,
    payload: &str,
) -> Result<(), AgentRegistryError> {
    ack_remote_payload(redis, &mailbox_processing_key(key), payload).await
}

async fn ack_remote_payload(
    redis: &RedisClient,
    processing_key: &str,
    payload: &str,
) -> Result<(), AgentRegistryError> {
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    conn.lrem::<_, _, ()>(processing_key, 1, payload)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn recover_processing_queue(
    redis: &RedisClient,
    processing_key: &str,
    pending_key: &str,
) -> Result<(), AgentRegistryError> {
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    loop {
        let moved: Option<String> = Cmd::new()
            .arg("RPOPLPUSH")
            .arg(processing_key)
            .arg(pending_key)
            .query_async(&mut *conn)
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
        if moved.is_none() {
            break;
        }
    }
    Ok(())
}

async fn write_remote_reply(
    redis: &RedisClient,
    request_id: &str,
    reply: &RedisMailboxReply,
) -> Result<(), AgentRegistryError> {
    let payload = serde_json::to_string(reply)
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let reply_key = mailbox_reply_key(request_id);
    conn.set_ex::<_, _, ()>(reply_key, payload, REPLY_TTL_SECS)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn push_remote_control_request(
    redis: &RedisClient,
    key: &str,
    request: &RedisControlRequest,
) -> Result<(), AgentRegistryError> {
    let payload = serde_json::to_string(request)
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    conn.lpush::<_, _, ()>(control_pending_key(key), payload)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn write_remote_control_reply(
    redis: &RedisClient,
    request_id: &str,
    reply: &RedisControlReply,
) -> Result<(), AgentRegistryError> {
    let payload = serde_json::to_string(reply)
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    conn.set_ex::<_, _, ()>(control_reply_key(request_id), payload, REPLY_TTL_SECS)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn wait_for_remote_control_reply(
    redis: &RedisClient,
    request_id: &str,
) -> Result<RedisControlReply, AgentRegistryError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(REMOTE_REPLY_TIMEOUT_SECS);
    let reply_key = control_reply_key(request_id);

    loop {
        let pool = redis.pool();
        let mut conn = pool
            .get()
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
        if let Some(payload) = conn
            .get::<_, Option<String>>(&reply_key)
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?
        {
            let _ = conn.del::<_, ()>(&reply_key).await;
            return serde_json::from_str(&payload)
                .map_err(|error| AgentRegistryError::Redis(error.to_string()));
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(AgentRegistryError::Redis(format!(
                "timed out waiting for remote control reply '{}'",
                request_id
            )));
        }

        tokio::time::sleep(Duration::from_millis(REMOTE_REPLY_POLL_MS)).await;
    }
}

async fn wait_for_remote_reply(
    redis: &RedisClient,
    request_id: &str,
) -> Result<RedisMailboxReply, AgentRegistryError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(REMOTE_REPLY_TIMEOUT_SECS);
    let reply_key = mailbox_reply_key(request_id);

    loop {
        let pool = redis.pool();
        let mut conn = pool
            .get()
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
        if let Some(payload) = conn
            .get::<_, Option<String>>(&reply_key)
            .await
            .map_err(|error| AgentRegistryError::Redis(error.to_string()))?
        {
            let _ = conn.del::<_, ()>(&reply_key).await;
            return serde_json::from_str(&payload)
                .map_err(|error| AgentRegistryError::Redis(error.to_string()));
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(AgentRegistryError::Redis(format!(
                "timed out waiting for remote reply '{}'",
                request_id
            )));
        }

        tokio::time::sleep(Duration::from_millis(REMOTE_REPLY_POLL_MS)).await;
    }
}

async fn store_remote_conversation(
    redis: &RedisClient,
    key: &str,
    conversation: &Conversation,
) -> Result<(), AgentRegistryError> {
    let payload = serde_json::to_string(conversation)
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    conn.set::<_, _, ()>(mailbox_conversation_key(key), payload)
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

async fn load_remote_conversation(
    redis: &RedisClient,
    key: &str,
) -> Result<Conversation, AgentRegistryError> {
    let pool = redis.pool();
    let mut conn = pool
        .get()
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?;
    let payload = conn
        .get::<_, Option<String>>(mailbox_conversation_key(key))
        .await
        .map_err(|error| AgentRegistryError::Redis(error.to_string()))?
        .ok_or_else(|| AgentRegistryError::EntryNotFound(key.to_string()))?;
    serde_json::from_str(&payload).map_err(|error| AgentRegistryError::Redis(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::mailbox::AgentMailbox;
    use crate::supervision::{SupervisionConfig, SupervisorRuntime};
    use tokio::sync::mpsc;

    #[test]
    fn register_and_get_entry() {
        let registry = AgentRegistry::new();
        registry.register(AgentRegistryEntry::new(
            "agent/planner",
            AgentRegistryKind::ConfiguredAgent,
            "planner",
        ));

        let entry = registry.get("agent/planner").expect("entry");
        assert_eq!(entry.agent_name, "planner");
        assert_eq!(entry.kind, AgentRegistryKind::ConfiguredAgent);
    }

    #[test]
    fn scan_prefix_returns_sorted_matches() {
        let registry = AgentRegistry::new();
        registry.register(AgentRegistryEntry::new(
            "agent/a",
            AgentRegistryKind::ConfiguredAgent,
            "a",
        ));
        registry.register(AgentRegistryEntry::new(
            "agent/ab",
            AgentRegistryKind::ConfiguredAgent,
            "ab",
        ));
        registry.register(AgentRegistryEntry::new(
            "service/pubsub",
            AgentRegistryKind::Service,
            "pubsub",
        ));

        let matches = registry.scan_prefix("agent/");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].key, "agent/a");
        assert_eq!(matches[1].key, "agent/ab");
    }

    #[test]
    fn find_by_agent_name_prefers_named_agent_key() {
        let registry = AgentRegistry::new();
        registry.register(AgentRegistryEntry::new(
            "instance/abc",
            AgentRegistryKind::AgentInstance,
            "planner",
        ));
        registry.register(AgentRegistryEntry::new(
            "agent/planner",
            AgentRegistryKind::AgentInstance,
            "planner",
        ));

        let entry = registry.find_by_agent_name("planner").expect("planner");
        assert_eq!(entry.key, "agent/planner");
    }

    #[tokio::test]
    async fn send_message_returns_not_messageable_for_plain_entry() {
        let registry = AgentRegistry::new();
        registry.register(AgentRegistryEntry::new(
            "service/planner",
            AgentRegistryKind::Service,
            "planner",
        ));

        let error = registry
            .send_message("service/planner", "hello")
            .await
            .expect_err("not messageable");
        assert!(matches!(error, AgentRegistryError::NotMessageable(_)));
    }

    #[tokio::test]
    async fn register_mailbox_reuses_entry_record() {
        let registry = AgentRegistry::new();
        let supervisor = SupervisorRuntime::new(
            tokio::runtime::Handle::current(),
            SupervisionConfig::default(),
        );
        let (tx, _rx) = mpsc::channel(1);
        registry.register_mailbox(
            AgentRegistryEntry::new("agent/planner", AgentRegistryKind::AgentInstance, "planner"),
            AgentMailbox::new("agent/planner", "planner", "agent-instance:planner", tx),
            &supervisor,
        );

        let entry = registry.get("agent/planner").expect("entry");
        assert_eq!(entry.agent_name, "planner");
    }
}
