//! Agent error types.

use std::fmt;

/// Errors that can occur within the agent runtime.
#[derive(Debug)]
pub enum AgentError {
    /// An error from the LLM provider.
    LlmError(String),
    /// The requested LLM provider is not available.
    LlmProviderUnavailable(String),
    /// The LLM provider rate-limited the request.
    LlmRateLimited {
        provider: String,
        retry_after_ms: Option<u64>,
    },
    /// The LLM stream was interrupted before completion.
    LlmStreamInterrupted(String),
    /// A tool failed during execution.
    ToolExecutionError { tool: String, message: String },
    /// The requested tool was not found.
    ToolNotFound(String),
    /// Permission denied for tool execution.
    ToolPermissionDenied { tool: String, reason: String },
    /// A tool exceeded its execution timeout.
    ToolTimeout { tool: String, timeout_ms: u64 },
    /// An error related to memory/context management.
    MemoryError(String),
    /// The requested conversation was not found.
    ConversationNotFound(String),
    /// An error in the orchestration layer.
    OrchestrationError(String),
    /// Failed to load a skill.
    SkillLoadError { skill: String, message: String },
    /// A configuration error.
    ConfigError(String),
    /// An error originating from Python interop.
    PythonError(String),
    /// A budget limit was exceeded.
    BudgetExceeded {
        resource: String,
        limit: u64,
        used: u64,
    },
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentError::LlmError(msg) => write!(f, "LLM error: {msg}"),
            AgentError::LlmProviderUnavailable(provider) => {
                write!(f, "LLM provider unavailable: {provider}")
            }
            AgentError::LlmRateLimited {
                provider,
                retry_after_ms,
            } => {
                write!(f, "LLM rate limited by {provider}")?;
                if let Some(ms) = retry_after_ms {
                    write!(f, " (retry after {ms}ms)")?;
                }
                Ok(())
            }
            AgentError::LlmStreamInterrupted(msg) => {
                write!(f, "LLM stream interrupted: {msg}")
            }
            AgentError::ToolExecutionError { tool, message } => {
                write!(f, "Tool execution error [{tool}]: {message}")
            }
            AgentError::ToolNotFound(tool) => write!(f, "Tool not found: {tool}"),
            AgentError::ToolPermissionDenied { tool, reason } => {
                write!(f, "Tool permission denied [{tool}]: {reason}")
            }
            AgentError::ToolTimeout { tool, timeout_ms } => {
                write!(f, "Tool timeout [{tool}]: exceeded {timeout_ms}ms")
            }
            AgentError::MemoryError(msg) => write!(f, "Memory error: {msg}"),
            AgentError::ConversationNotFound(id) => {
                write!(f, "Conversation not found: {id}")
            }
            AgentError::OrchestrationError(msg) => {
                write!(f, "Orchestration error: {msg}")
            }
            AgentError::SkillLoadError { skill, message } => {
                write!(f, "Skill load error [{skill}]: {message}")
            }
            AgentError::ConfigError(msg) => write!(f, "Config error: {msg}"),
            AgentError::PythonError(msg) => write!(f, "Python error: {msg}"),
            AgentError::BudgetExceeded {
                resource,
                limit,
                used,
            } => {
                write!(
                    f,
                    "Budget exceeded for {resource}: limit={limit}, used={used}"
                )
            }
        }
    }
}

impl std::error::Error for AgentError {}

// Conversion to crate::errors::Error (anyhow::Error) is provided automatically
// by anyhow's blanket `impl<E: std::error::Error + Send + Sync + 'static> From<E> for anyhow::Error`.
