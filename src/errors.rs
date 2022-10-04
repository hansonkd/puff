//! Easy Error Handling.
//!
//! Puff uses [anyhow::Error] for transparently catching and converting almost all error types. See
//! Anyhow's documentation for more information.
use crate::python::log_traceback_with_label;
use axum::response::{IntoResponse, Response};
use pyo3::PyErr;
use tracing::error;

pub type Result<T> = anyhow::Result<T>;
pub type Error = anyhow::Error;
pub type PuffResult<T> = Result<T>;

pub struct RequestError(Error);

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        todo!()
    }
}

pub fn handle_puff_error(label: &str, e: Error) {
    match e.downcast::<PyErr>() {
        Ok(pye) => log_traceback_with_label(label, pye),
        Err(e) => {
            error!("Encountered {label} Error: {e}")
        }
    }
}

pub fn handle_puff_result(label: &str, r: PuffResult<()>) {
    match r {
        Ok(()) => (),
        Err(e) => handle_puff_error(label, e),
    }
}
