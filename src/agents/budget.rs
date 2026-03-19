use std::sync::atomic::{AtomicU64, Ordering};

use crate::agents::capabilities::BudgetCapability;
use crate::agents::error::AgentError;

/// Tracks resource usage for an agent and enforces budget limits.
///
/// All counters use atomic operations so the tracker can be shared across
/// concurrent tasks via `Arc<AgentBudgetTracker>`.
pub struct AgentBudgetTracker {
    limits: BudgetCapability,
    input_tokens_used: AtomicU64,
    output_tokens_used: AtomicU64,
    /// Cost stored as micro-dollars (USD * 1_000_000) for atomic integer ops.
    cost_microdollars: AtomicU64,
    tool_calls_used: AtomicU64,
}

impl AgentBudgetTracker {
    pub fn new(limits: BudgetCapability) -> Self {
        Self {
            limits,
            input_tokens_used: AtomicU64::new(0),
            output_tokens_used: AtomicU64::new(0),
            cost_microdollars: AtomicU64::new(0),
            tool_calls_used: AtomicU64::new(0),
        }
    }

    /// Check whether the agent is still within its LLM token/cost budget.
    /// Call this **before** making an LLM request.
    pub fn check_llm_budget(&self) -> Result<(), AgentError> {
        if let Some(max_input) = self.limits.max_input_tokens {
            let used = self.input_tokens_used.load(Ordering::Relaxed);
            if used >= max_input {
                return Err(AgentError::BudgetExceeded {
                    resource: "input_tokens".into(),
                    limit: max_input,
                    used,
                });
            }
        }
        if let Some(max_output) = self.limits.max_output_tokens {
            let used = self.output_tokens_used.load(Ordering::Relaxed);
            if used >= max_output {
                return Err(AgentError::BudgetExceeded {
                    resource: "output_tokens".into(),
                    limit: max_output,
                    used,
                });
            }
        }
        if let Some(max_cost) = self.limits.max_cost_usd {
            let used_micro = self.cost_microdollars.load(Ordering::Relaxed);
            let limit_micro = (max_cost * 1_000_000.0) as u64;
            if used_micro >= limit_micro {
                return Err(AgentError::BudgetExceeded {
                    resource: "cost_usd".into(),
                    limit: limit_micro,
                    used: used_micro,
                });
            }
        }
        Ok(())
    }

    /// Record token and cost usage after an LLM response is received.
    pub fn record_llm_usage(&self, input_tokens: u64, output_tokens: u64, cost_usd: f64) {
        self.input_tokens_used
            .fetch_add(input_tokens, Ordering::Relaxed);
        self.output_tokens_used
            .fetch_add(output_tokens, Ordering::Relaxed);
        let cost_micro = (cost_usd * 1_000_000.0) as u64;
        self.cost_microdollars
            .fetch_add(cost_micro, Ordering::Relaxed);
    }

    /// Check whether the agent is still within its tool-call budget.
    /// Call this **before** executing a tool.
    pub fn check_tool_budget(&self) -> Result<(), AgentError> {
        if let Some(max_calls) = self.limits.max_tool_calls {
            let used = self.tool_calls_used.load(Ordering::Relaxed);
            if used >= max_calls as u64 {
                return Err(AgentError::BudgetExceeded {
                    resource: "tool_calls".into(),
                    limit: max_calls as u64,
                    used,
                });
            }
        }
        Ok(())
    }

    /// Record that a tool call was executed.
    pub fn record_tool_call(&self) {
        self.tool_calls_used.fetch_add(1, Ordering::Relaxed);
    }

    // -----------------------------------------------------------------------
    // Read-only accessors
    // -----------------------------------------------------------------------

    pub fn input_tokens_used(&self) -> u64 {
        self.input_tokens_used.load(Ordering::Relaxed)
    }

    pub fn output_tokens_used(&self) -> u64 {
        self.output_tokens_used.load(Ordering::Relaxed)
    }

    pub fn cost_usd_used(&self) -> f64 {
        self.cost_microdollars.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    pub fn tool_calls_used(&self) -> u64 {
        self.tool_calls_used.load(Ordering::Relaxed)
    }

    pub fn limits(&self) -> &BudgetCapability {
        &self.limits
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn unlimited() -> BudgetCapability {
        BudgetCapability::default()
    }

    fn limited() -> BudgetCapability {
        BudgetCapability {
            max_input_tokens: Some(1000),
            max_output_tokens: Some(500),
            max_cost_usd: Some(0.10),
            max_tool_calls: Some(5),
            max_tool_cpu_seconds: None,
        }
    }

    #[test]
    fn test_unlimited_budget_never_fails() {
        let tracker = AgentBudgetTracker::new(unlimited());
        assert!(tracker.check_llm_budget().is_ok());
        assert!(tracker.check_tool_budget().is_ok());

        tracker.record_llm_usage(999_999, 999_999, 999.0);
        tracker.record_tool_call();

        // Still ok because limits are None
        assert!(tracker.check_llm_budget().is_ok());
        assert!(tracker.check_tool_budget().is_ok());
    }

    #[test]
    fn test_llm_budget_input_tokens_exceeded() {
        let tracker = AgentBudgetTracker::new(limited());
        tracker.record_llm_usage(1001, 0, 0.0);
        let err = tracker.check_llm_budget().unwrap_err();
        assert!(matches!(err, AgentError::BudgetExceeded { .. }));
        assert!(err.to_string().contains("input_tokens"));
    }

    #[test]
    fn test_llm_budget_output_tokens_exceeded() {
        let tracker = AgentBudgetTracker::new(limited());
        tracker.record_llm_usage(0, 501, 0.0);
        let err = tracker.check_llm_budget().unwrap_err();
        assert!(matches!(err, AgentError::BudgetExceeded { .. }));
        assert!(err.to_string().contains("output_tokens"));
    }

    #[test]
    fn test_llm_budget_cost_exceeded() {
        let tracker = AgentBudgetTracker::new(limited());
        tracker.record_llm_usage(0, 0, 0.11);
        let err = tracker.check_llm_budget().unwrap_err();
        assert!(matches!(err, AgentError::BudgetExceeded { .. }));
        assert!(err.to_string().contains("cost_usd"));
    }

    #[test]
    fn test_tool_budget_exceeded() {
        let tracker = AgentBudgetTracker::new(limited());
        for _ in 0..5 {
            tracker.record_tool_call();
        }
        let err = tracker.check_tool_budget().unwrap_err();
        assert!(matches!(err, AgentError::BudgetExceeded { .. }));
        assert!(err.to_string().contains("tool_calls"));
    }

    #[test]
    fn test_within_budget_passes() {
        let tracker = AgentBudgetTracker::new(limited());
        tracker.record_llm_usage(500, 200, 0.05);
        assert!(tracker.check_llm_budget().is_ok());

        for _ in 0..4 {
            tracker.record_tool_call();
        }
        assert!(tracker.check_tool_budget().is_ok());
    }

    #[test]
    fn test_accessors() {
        let tracker = AgentBudgetTracker::new(limited());
        tracker.record_llm_usage(100, 50, 0.01);
        tracker.record_tool_call();
        tracker.record_tool_call();

        assert_eq!(tracker.input_tokens_used(), 100);
        assert_eq!(tracker.output_tokens_used(), 50);
        assert!((tracker.cost_usd_used() - 0.01).abs() < 1e-6);
        assert_eq!(tracker.tool_calls_used(), 2);
    }
}
