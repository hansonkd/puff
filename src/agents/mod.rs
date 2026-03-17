pub mod error;

pub use error::AgentError;

pub type AgentResult<T> = Result<T, AgentError>;
