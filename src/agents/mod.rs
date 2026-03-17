pub mod error;
pub mod llm;
pub mod provider;
pub mod streaming;

pub use error::AgentError;

pub type AgentResult<T> = Result<T, AgentError>;
