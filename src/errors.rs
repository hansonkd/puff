//! Easy Error Handling.
//!
//! Puff uses [anyhow::Error] for transparently catching and converting almost all error types. See
//! Anyhow's documentation for more information.
use axum::response::{IntoResponse, Response};

pub type Result<T> = anyhow::Result<T>;
pub type Error = anyhow::Error;
pub type PuffResult<T> = Result<T>;


pub struct RequestError(Error);

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        todo!()
    }
}