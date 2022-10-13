//! Easy Error Handling.
//!
//! Puff uses [anyhow::Error] for transparently catching and converting almost all error types. See
//! Anyhow's documentation for more information.
use crate::python::log_traceback_with_label;
use axum::response::{IntoResponse, Response};
use pyo3::exceptions::PyException;
use pyo3::{PyErr, PyResult};
use tracing::error;

use crate::web::server::StatusCode;
pub use anyhow;

pub type Result<T> = anyhow::Result<T>;
pub type Error = anyhow::Error;
pub type PuffResult<T> = Result<T>;
pub type RequestResult<T> = std::result::Result<T, RequestError>;

/// An error structure to use in Axum requests.
pub struct RequestError(Error);

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        handle_puff_error("Axum Request", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
    }
}

impl From<Error> for RequestError {
    fn from(e: Error) -> Self {
        RequestError(e)
    }
}

/// Consume an error and log it.
pub fn handle_puff_error(label: &str, e: Error) {
    match e.downcast::<PyErr>() {
        Ok(pye) => log_traceback_with_label(label, &pye),
        Err(e) => {
            error!("Encountered {label} Error: {e}")
        }
    }
}

/// Consume a PuffResult and log it if it has an error.
pub fn handle_puff_result(label: &str, r: PuffResult<()>) {
    let _ = log_puff_error(label, r);
}

/// Log a puff error if it exists for the result, and return the result back.
pub fn log_puff_error<T>(label: &str, r: PuffResult<T>) -> PuffResult<T> {
    match r {
        Ok(pye) => Ok(pye),
        Err(e) => match e.downcast::<PyErr>() {
            Ok(pye) => {
                log_traceback_with_label(label, &pye);
                Err(pye.into())
            }
            Err(e) => {
                error!("Encountered {label} Error: {}", &e);
                Err(e)
            }
        },
    }
}

/// Convert a PuffError to a PyError
pub fn to_py_error<T>(_label: &str, r: PuffResult<T>) -> PyResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => match e.downcast::<PyErr>() {
            Ok(pye) => Err(pye),
            Err(e) => Err(PyException::new_err(format!("Puff Error: {}", e))),
        },
    }
}
