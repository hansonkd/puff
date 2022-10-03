use crate::context::PuffContext;
use crate::errors::PuffResult;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::{PyObject, PyResult, Python};
use std::future::Future;

use crate::python::log_traceback;
use tokio::sync::oneshot;

/// Python return
#[pyclass]
pub struct GreenletReturn(Option<oneshot::Sender<PyResult<PyObject>>>);

impl GreenletReturn {
    pub fn new(rec: Option<oneshot::Sender<PyResult<PyObject>>>) -> Self {
        Self(rec)
    }
}

#[pymethods]
impl GreenletReturn {
    pub fn __call__(
        &mut self,
        py: Python,
        value: PyObject,
        exception: Option<&PyAny>,
    ) -> PyResult<()> {
        match self.0.take() {
            Some(sender) => match exception {
                Some(e) => Ok(sender.send(Err(PyErr::from_value(e))).unwrap_or(())),
                None => Ok(sender.send(Ok(value)).unwrap_or(())),
            },
            None => Err(PyException::new_err("Already used GreenletReturn")),
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
                .map_err(|e| log_traceback(e)),
            Err(e) => {
                match e.downcast::<PyErr>() {
                    Ok(py_err) => {
                        return_fun
                        .call1(py, (py.None(), py_err))
                        .map_err(|e| log_traceback(e))
                    }
                    Err(e) => {
                        let py_err = PyException::new_err(format!("Rust Error: {e}"));
                        return_fun
                        .call1(py, (py.None(), py_err))
                        .map_err(|e| log_traceback(e))
                    }
                }


            }
        }
        .unwrap_or(py.None())
    });
}

/// The the future in the Tokio and execute the return function when finished. Does not block.
pub fn greenlet_async<
    F: Future<Output = PuffResult<R>> + Send + 'static,
    R: ToPyObject + 'static,
>(
    ctx: PuffContext,
    return_fun: PyObject,
    f: F,
) {
    let h = ctx.handle();
    h.spawn(handle_return(return_fun, f));
}
