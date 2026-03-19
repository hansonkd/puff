//! Per-agent connection quotas.

use crate::agents::error::AgentError;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Per-agent connection quota manager.
/// Limits how many simultaneous connections each agent can hold.
pub struct ConnectionQuota {
    semaphores: dashmap::DashMap<String, Arc<Semaphore>>,
    max_per_agent: usize,
}

impl ConnectionQuota {
    pub fn new(max_per_agent: usize) -> Self {
        Self {
            semaphores: dashmap::DashMap::new(),
            max_per_agent,
        }
    }

    /// Acquire a connection permit for the given agent.
    /// Returns a permit that must be held while the connection is in use.
    /// When the permit is dropped, the slot is released.
    pub async fn acquire(&self, agent_name: &str) -> Result<OwnedSemaphorePermit, AgentError> {
        let semaphore = self
            .semaphores
            .entry(agent_name.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_agent)))
            .clone();

        semaphore.acquire_owned().await.map_err(|_| {
            AgentError::ConfigError(format!(
                "Connection quota for agent '{}' closed",
                agent_name
            ))
        })
    }

    /// Try to acquire without waiting. Returns None if quota is full.
    pub fn try_acquire(&self, agent_name: &str) -> Option<OwnedSemaphorePermit> {
        let semaphore = self
            .semaphores
            .entry(agent_name.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_agent)))
            .clone();

        semaphore.try_acquire_owned().ok()
    }

    /// Get the number of available permits for an agent.
    pub fn available(&self, agent_name: &str) -> usize {
        match self.semaphores.get(agent_name) {
            Some(sem) => sem.available_permits(),
            None => self.max_per_agent,
        }
    }

    pub fn max_per_agent(&self) -> usize {
        self.max_per_agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_quota_acquire_and_release() {
        let quota = ConnectionQuota::new(2);
        let p1 = quota.acquire("agent-a").await.unwrap();
        let p2 = quota.acquire("agent-a").await.unwrap();
        assert_eq!(quota.available("agent-a"), 0);
        drop(p1);
        assert_eq!(quota.available("agent-a"), 1);
        drop(p2);
        assert_eq!(quota.available("agent-a"), 2);
    }

    #[tokio::test]
    async fn test_quota_independent_agents() {
        let quota = ConnectionQuota::new(1);
        let _p1 = quota.acquire("agent-a").await.unwrap();
        // agent-b should still be able to acquire
        let _p2 = quota.acquire("agent-b").await.unwrap();
        assert_eq!(quota.available("agent-a"), 0);
        assert_eq!(quota.available("agent-b"), 0);
    }

    #[test]
    fn test_try_acquire_when_full() {
        let quota = ConnectionQuota::new(1);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _p1 = rt.block_on(quota.acquire("agent-a")).unwrap();
        assert!(quota.try_acquire("agent-a").is_none());
    }

    #[test]
    fn test_defaults() {
        let quota = ConnectionQuota::new(5);
        assert_eq!(quota.max_per_agent(), 5);
        assert_eq!(quota.available("unknown-agent"), 5);
    }
}
