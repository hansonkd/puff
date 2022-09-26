use std::future::Future;
use pyo3::{IntoPy, PyObject, PyResult, Python};
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use tokio::sync::oneshot;
use crate::errors::PuffResult;
use crate::runtime::dispatcher::RuntimeDispatcher;

#[pyclass]
pub struct GreenletReturn(Option<oneshot::Sender<PyResult<PyObject>>>);

#[pymethods]
impl GreenletReturn {
    pub fn __call__(&mut self, py: Python, value: PyObject, exception: Option<&PyException>) -> PyResult<()> {
        match self.0.take() {
            Some(sender) => {
                match exception {
                    Some(e) => Ok(sender.send(Err(e.into())).unwrap_or(())),
                    None => Ok(sender.send(Ok(value)).unwrap_or(()))
                }
            },
            None => Err(PyException::new_err("Already used GreenletReturn")),
        }
    }
}


#[pyclass]
#[derive(Clone)]
pub struct GreenletContext(RuntimeDispatcher, PyObject);

impl GreenletContext {
    pub fn dispatcher(&self) -> RuntimeDispatcher {
        self.0.clone()
    }
}

#[pymethods]
impl GreenletContext {
    pub fn global_state(&self) -> PyObject {
        self.1.clone()
    }
}

#[derive(Clone)]
pub struct GreenletDispatcher {
    thread_obj: PyObject,
    thread_dispatcher: GreenletContext
}


impl GreenletDispatcher {
    pub fn new(dispatcher: RuntimeDispatcher, global_state: PyObject) -> PyResult<Self> {
        let thread_obj = Python::with_gil(|py| {
            let puff = py.import("puff")?;

            let ret = puff.call_method0("start_event_loop")?;
            PyResult::Ok(ret.into_py(py))
        })?;
        let thread_dispatcher = GreenletContext(dispatcher, global_state);
        PyResult::Ok(Self { thread_obj, thread_dispatcher })
    }

    pub fn dispatch(&self, function: PyObject, args: PyObject, kwargs: PyObject) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        Python::with_gil(|py| self.dispatch_py(py, function, args, kwargs))
    }

    pub fn dispatch_py(&self, py: Python, function: PyObject, args: PyObject, kwargs: PyObject) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        let (sender, rec) = oneshot::channel();
        let returner = GreenletReturn(Some(sender));
        self.thread_obj.call_method1(py, "spawn", (function, self.thread_dispatcher.clone(), args, kwargs, returner))?;
        Ok(rec)
    }
}

async fn handle_return<F: Future<Output=PuffResult<R>> + Send + 'static, R: ToPyObject + 'static>(return_fun: PyObject, f: F) {
    let res = f.await;
    Python::with_gil(|py| match res {
        Ok(r) => return_fun.call1(py, (r.to_object(py), py.None())).expect("Could not return value"),
        Err(e) => {
            let py_err = PyException::new_err(format!("Greenlet async exception: {e}"));
            return_fun.call1(py, (py.None(), py_err)).expect("Could not return exception")
        }
    });
}

pub fn greenlet_async<F: Future<Output=PuffResult<R>> + Send + 'static, R: ToPyObject + 'static>(ctx: &GreenletContext, return_fun: PyObject, f: F) {
    let h = ctx.dispatcher().handle();
    h.spawn(handle_return(return_fun, f));
}