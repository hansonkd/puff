use pyo3::{IntoPy, PyObject, PyResult, Python};
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use tokio::sync::oneshot;
use crate::runtime::dispatcher::RuntimeDispatcher;

#[pyclass]
pub struct GreenletReturn(Option<oneshot::Sender<PyObject>>);

#[pymethods]
impl GreenletReturn {
    pub fn __call__(&mut self, py: Python, value: PyObject) -> PyResult<()> {
        match self.0.take() {
            Some(sender) => Ok(sender.send(value).unwrap_or(())),
            None => Err(PyException::new_err("Already used GreenletReturn")),
        }
    }
}


#[pyclass]
#[derive(Clone)]
pub struct ThreadDispatcher(RuntimeDispatcher);

impl ThreadDispatcher {
    pub fn dispatcher(&self) -> RuntimeDispatcher {
        self.0.clone()
    }
}

#[derive(Clone)]
pub struct GreenletDispatcher {
    thread_obj: PyObject,
    thread_dispatcher: ThreadDispatcher
}


impl GreenletDispatcher {
    pub fn new(dispatcher: RuntimeDispatcher) -> PyResult<Self> {
        let thread_obj = Python::with_gil(|py| {
            let puff = py.import("puff")?;

            let ret = puff.call_method0("start_event_loop")?;
            PyResult::Ok(ret.into_py(py))
        })?;
        let thread_dispatcher = ThreadDispatcher(dispatcher);
        PyResult::Ok(Self { thread_obj, thread_dispatcher })
    }

    pub fn dispatch(&self, function: PyObject, args: PyObject, kwargs: PyObject) -> PyResult<oneshot::Receiver<PyObject>> {
        Python::with_gil(|py| self.dispatch_py(py, function, args, kwargs))
    }

    pub fn dispatch_py(&self, py: Python, function: PyObject, args: PyObject, kwargs: PyObject) -> PyResult<oneshot::Receiver<PyObject>> {
        let (sender, rec) = oneshot::channel();
        let returner = GreenletReturn(Some(sender));
        self.thread_obj.call_method1(py, "spawn", (function, self.thread_dispatcher.clone(), args, kwargs, returner))?;
        Ok(rec)
    }
}