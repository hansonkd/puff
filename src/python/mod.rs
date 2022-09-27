use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::context::{set_puff_context_waiting, with_puff_context, PuffContext};
use crate::python::greenlet::GreenletReturn;
use crate::runtime::{RuntimeConfig, Strategy};
use pyo3::types::{PyDict, PyTuple};
use pyo3::{PyResult, Python};
use rand::prelude::SliceRandom;
use rand::thread_rng;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

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

pub(crate) fn bootstrap_puff_globals(global_state: PyObject) {
    Python::with_gil(|py| {
        println!("Adding puff....");
        let puff_mod = py.import("puff")?;
        let puff_rust_functions = puff_mod.getattr("rust_functions")?;
        puff_rust_functions.setattr("get_redis", RedisGlobal)?;
        puff_rust_functions.setattr("global_state", global_state)?;
        puff_rust_functions.setattr("spawn_blocking_internal", SpawnBlocking.into_py(py))
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
pub struct PythonPuffContextSetter(Arc<Mutex<Option<PuffContext>>>);

#[pymethods]
impl PythonPuffContextSetter {
    pub fn __call__(&self) {
        set_puff_context_waiting(self.0.clone())
    }
}

#[derive(Clone)]
pub struct PythonDispatcher {
    thread_obj: PyObject,
    blocking: bool,
}

impl PythonDispatcher {
    pub fn new(context_waiting: Arc<Mutex<Option<PuffContext>>>) -> PyResult<Self> {
        let thread_obj = Python::with_gil(|py| {
            let puff = py.import("puff")?;
            let ret = puff.call_method1(
                "start_event_loop",
                (PythonPuffContextSetter(context_waiting.clone()),),
            )?;
            PyResult::Ok(ret.into_py(py))
        })?;

        PyResult::Ok(Self {
            thread_obj,
            blocking: false,
        })
    }

    pub fn blocking() -> PyResult<Self> {
        PyResult::Ok(Self {
            thread_obj: Python::with_gil(|py| py.None()),
            blocking: true,
        })
    }

    pub fn dispatch_blocking<A: IntoPy<Py<PyTuple>>, K: IntoPy<Py<PyDict>>>(
        &self,
        py: Python,
        function: PyObject,
        args: A,
        kwargs: K,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        let (sender, rec) = oneshot::channel();
        let args_py = args.into_py(py);
        let kwargs_py = kwargs.into_py(py);
        let ctx = with_puff_context(|ctx| ctx);
        ctx.handle().spawn_blocking(move || {
            Python::with_gil(|py| {
                sender
                    .send(function.call(py, args_py.as_ref(py), Some(kwargs_py.as_ref(py))))
                    .unwrap_or(())
            })
        });
        Ok(rec)
    }

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
