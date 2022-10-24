use crate::context::with_puff_context;
use crate::errors::PuffResult;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::{PyObject, PyResult, Python};
use std::future::Future;

use crate::python::log_traceback;
use tokio::sync::oneshot;

/// Python return
#[pyclass]
pub struct AsyncReturn(Option<oneshot::Sender<PyResult<PyObject>>>);

impl AsyncReturn {
    pub fn new(rec: Option<oneshot::Sender<PyResult<PyObject>>>) -> Self {
        Self(rec)
    }
}

#[pymethods]
impl AsyncReturn {
    pub fn __call__(&mut self, value: PyObject, exception: Option<&PyAny>) -> PyResult<()> {
        match self.0.take() {
            Some(sender) => match exception {
                Some(e) => Ok(sender.send(Err(PyErr::from_value(e))).unwrap_or(())),
                None => Ok(sender.send(Ok(value)).unwrap_or(())),
            },
            None => Err(PyException::new_err("Already used AsyncReturn")),
        }
    }
}

pub async fn handle_return<F: Future<Output = PuffResult<R>>, R: ToPyObject>(
    return_fun: PyObject,
    f: F,
) {
    let res = f.await;
    Python::with_gil(|py| {
        match res {
            Ok(r) => return_fun
                .call1(py, (r.to_object(py), py.None()))
                .map_err(|e| log_traceback(&e)),
            Err(e) => match e.downcast::<PyErr>() {
                Ok(py_err) => return_fun
                    .call1(py, (py.None(), py_err))
                    .map_err(|e| log_traceback(&e)),
                Err(e) => {
                    let py_err = PyException::new_err(format!("Rust Error: {e}"));
                    return_fun
                        .call1(py, (py.None(), py_err))
                        .map_err(|e| log_traceback(&e))
                }
            },
        }
        .unwrap_or(py.None())
    });
}

pub async fn handle_python_return<F: Future<Output = PyResult<R>>, R: ToPyObject>(
    return_fun: PyObject,
    f: F,
) {
    let res = f.await;
    Python::with_gil(|py| {
        match res {
            Ok(r) => return_fun
                .call1(py, (r.to_object(py), py.None()))
                .map_err(|e| log_traceback(&e)),
            Err(py_err) => return_fun
                .call1(py, (py.None(), py_err))
                .map_err(|e| log_traceback(&e)),
        }
        .unwrap_or(py.None())
    });
}

/// The the future in the Tokio and execute the return function when finished. Does not block.
pub fn run_python_async<
    F: Future<Output = PuffResult<R>> + Send + 'static,
    R: ToPyObject + 'static,
>(
    return_fun: PyObject,
    f: F,
) {
    let h = with_puff_context(|ctx| ctx.handle());
    h.spawn(handle_return(return_fun, f));
}
