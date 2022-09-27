use crate::context::{with_puff_context, PuffContext};
use crate::errors::PuffResult;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::{PyObject, PyResult, Python};
use std::future::Future;

use tokio::sync::oneshot;

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
        _py: Python,
        value: PyObject,
        exception: Option<&PyException>,
    ) -> PyResult<()> {
        match self.0.take() {
            Some(sender) => match exception {
                Some(e) => Ok(sender.send(Err(e.into())).unwrap_or(())),
                None => Ok(sender.send(Ok(value)).unwrap_or(())),
            },
            None => Err(PyException::new_err("Already used GreenletReturn")),
        }
    }
}

async fn handle_return<
    F: Future<Output = PuffResult<R>> + Send + 'static,
    R: ToPyObject + 'static,
>(
    return_fun: PyObject,
    f: F,
) {
    let res = f.await;
    Python::with_gil(|py| match res {
        Ok(r) => return_fun
            .call1(py, (r.to_object(py), py.None()))
            .expect("Could not return value"),
        Err(e) => {
            let py_err = PyException::new_err(format!("Greenlet async exception: {e}"));
            return_fun
                .call1(py, (py.None(), py_err))
                .expect("Could not return exception")
        }
    });
}

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
