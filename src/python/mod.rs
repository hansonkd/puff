use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::context::{set_puff_context_waiting, with_puff_context, PuffContext, set_puff_context};
use crate::python::greenlet::GreenletReturn;
use crate::runtime::RuntimeConfig;
use pyo3::types::{PyDict, PyTuple};
use pyo3::{PyResult, Python};

use std::os::raw::c_int;

use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tracing::error;

pub mod greenlet;
pub mod redis;
pub mod wsgi;

fn into_py_result<T>(r: PuffResult<T>) -> PyResult<T> {
    r.map_err(|err| PyRuntimeError::new_err(format!("PuffError: {:?}", err)))
}

#[pyclass]
struct SpawnBlocking;

#[pymethods]
impl SpawnBlocking {
    pub fn __call__(
        &self,
        py: Python,
        function: PyObject,
        args: Py<PyTuple>,
        kwargs: Py<PyDict>,
    ) -> PyResult<()> {
        let ctx = with_puff_context(|ctx| ctx);
        ctx.python_dispatcher().dispatch_blocking(
            py,
            function,
            args.as_ref(py),
            kwargs.as_ref(py),
        )?;
        Ok(())
    }
}

pub fn log_traceback(e: PyErr) {
    Python::with_gil(|py| {
        let t = e.traceback(py);
        t.map(|f| {
            let res = f.format().unwrap_or_else(|e| format!("Error formatting traceback\n: {e}"));
            error!("Python Error: {e}");
            eprintln!("{}", res);
        })
    });
}

pub(crate) fn bootstrap_puff_globals(global_state: PyObject) {
    Python::with_gil(|py| {
        println!("Adding puff....");
        let puff_mod = py.import("puff")?;
        let puff_rust_functions = puff_mod.getattr("rust_objects")?;
        puff_rust_functions.setattr("global_redis_getter", RedisGlobal)?;
        puff_rust_functions.setattr("global_state", global_state)?;
        puff_rust_functions.setattr("blocking_spawner", SpawnBlocking.into_py(py))
    })
    .expect("Could not set python globals");
}

#[inline]
pub fn error_on_minusone(py: Python<'_>, result: c_int) -> PyResult<()> {
    if result != -1 {
        Ok(())
    } else {
        Err(PyErr::fetch(py))
    }
}

#[pyclass]
pub struct PythonPuffContextWaitingSetter(Arc<Mutex<Option<PuffContext>>>);

#[pymethods]
impl PythonPuffContextWaitingSetter {
    pub fn __call__(&self) {
        set_puff_context_waiting(self.0.clone())
    }
}

#[pyclass]
pub struct PythonPuffContextSetter(PuffContext);

#[pymethods]
impl PythonPuffContextSetter {
    pub fn __call__(&self) {
        set_puff_context(self.0.clone())
    }
}

#[derive(Clone)]
pub struct PythonDispatcher {
    thread_obj: PyObject,
    spawn_blocking_fn: PyObject,
    blocking: bool,
}

fn spawn_blocking_fn(py: Python) -> PyResult<PyObject> {
    let puff = py.import("puff")?;
    let res = puff.getattr("spawn_blocking_from_rust")?;
    Ok(res.into_py(py))
}

impl PythonDispatcher {
    pub fn new(context_waiting: Arc<Mutex<Option<PuffContext>>>) -> PyResult<Self> {
        let (thread_obj, spawn_blocking_fn) = Python::with_gil(|py| {
            let puff = py.import("puff")?;
            let ret = puff.call_method1(
                "start_event_loop",
                (PythonPuffContextWaitingSetter(context_waiting.clone()),),
            )?;
            PyResult::Ok((ret.into_py(py), spawn_blocking_fn(py)?))
        })?;

        PyResult::Ok(Self {
            thread_obj,
            spawn_blocking_fn,
            blocking: false,
        })
    }

    pub fn blocking() -> PyResult<Self> {
        let (thread_obj, spawn_blocking_fn) = Python::with_gil(|py| {
            PyResult::Ok((py.None(), spawn_blocking_fn(py)?))
        })?;
        PyResult::Ok(Self {
            thread_obj,
            spawn_blocking_fn,
            blocking: true,
        })
    }

    /// Run the python function a new Tokio blocking working thread.
    pub fn dispatch_blocking<A: IntoPy<Py<PyTuple>>, K: IntoPy<Py<PyDict>>>(
        &self,
        py: Python,
        function: PyObject,
        args: A,
        kwargs: K,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        let (sender, rec) = oneshot::channel();
        let ret = GreenletReturn::new(Some(sender));
        let on_thread_start = with_puff_context(PythonPuffContextSetter);
        self.spawn_blocking_fn.call1(py, (on_thread_start, function, ret, args.into_py(py), kwargs.into_py(py)))?;
        Ok(rec)
    }

    /// Acquires the GIL and Executes the python function on the greenlet thread or a new thread
    /// depending if greenlets were enabled.
    pub fn dispatch<
        A: IntoPy<Py<PyTuple>> + Send + 'static,
        K: IntoPy<Py<PyDict>> + Send + 'static,
    >(
        &self,
        function: PyObject,
        args: A,
        kwargs: K,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        if self.blocking {
            Python::with_gil(|py| self.dispatch_blocking(py, function, args, kwargs))
        } else {
            Python::with_gil(|py| self.dispatch_py(py, function, args, kwargs))
        }
    }

    /// Executes the python function on the greenlet thread.
    pub fn dispatch_py<A: IntoPy<Py<PyTuple>>, K: IntoPy<Py<PyDict>>>(
        &self,
        py: Python,
        function: PyObject,
        args: A,
        kwargs: K,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        let (sender, rec) = oneshot::channel();
        let returner = GreenletReturn::new(Some(sender));
        self.thread_obj.call_method1(
            py,
            "spawn",
            (
                function,
                args.into_py(py).to_object(py),
                kwargs.into_py(py).to_object(py),
                returner,
            ),
        )?;
        Ok(rec)
    }
}

pub fn setup_greenlet(
    config: RuntimeConfig,
    context_waiting: Arc<Mutex<Option<PuffContext>>>,
) -> PyResult<PythonDispatcher> {
    if config.greenlets() {
        PythonDispatcher::new(context_waiting)
    } else {
        PythonDispatcher::blocking()
    }
}
