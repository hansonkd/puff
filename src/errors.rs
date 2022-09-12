//! Easy Error Handling.
//!
//! Puff uses [anyhow::Error] for transparently catching and converting almost all error types. See
//! Anyhow's documentation for more information.
pub type Result<T> = anyhow::Result<T>;
pub type Error = anyhow::Error;
