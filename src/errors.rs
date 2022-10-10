//! Easy Error Handling.
//!
//! Puff uses [anyhow::Error] for transparently catching and converting almost all error types. See
//! Anyhow's documentation for more information.
use crate::python::log_traceback_with_label;
use axum::response::{IntoResponse, Response};
use pyo3::exceptions::PyException;
use pyo3::{PyErr, PyResult};
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
        Ok(pye) => log_traceback_with_label(label, &pye),
        Err(e) => {
            error!("Encountered {label} Error: {e}")
        }
    }
}

pub fn log_puff_error<T>(label: &str, r: PuffResult<T>) -> PuffResult<T> {
    match r {
        Ok(pye) => Ok(pye),
        Err(e) => {
            match e.downcast::<PyErr>() {
                Ok(pye) => {
                    log_traceback_with_label(label, &pye);
                    Err(pye.into())
                }
                Err(e) => {
                    error!("Encountered {label} Error: {}", &e);
                    Err(e)
                }
            }

        }
    }
}

pub fn to_py_error<T>(_label: &str, r: PuffResult<T>) -> PyResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => match e.downcast::<PyErr>() {
            Ok(pye) => Err(pye),
            Err(e) => Err(PyException::new_err(format!("Puff Error: {}", e))),
        },
    }
}

pub fn handle_puff_result(label: &str, r: PuffResult<()>) {
    match r {
        Ok(()) => (),
        Err(e) => handle_puff_error(label, e),
    }
}
