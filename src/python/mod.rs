
use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use pyo3::{PyResult, Python};
use std::os::raw::c_int;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use pyo3::types::{PyDict, PyTuple};
use rand::prelude::SliceRandom;
use rand::thread_rng;
use tokio::sync::oneshot;
use crate::context::{PuffContext, set_puff_context, set_puff_context_waiting};
use crate::python::greenlet::{GreenletContext, GreenletReturn};
use crate::runtime::{RuntimeConfig, Strategy};

pub mod greenlet;
pub mod redis;
pub mod wsgi;

fn into_py_result<T>(r: PuffResult<T>) -> PyResult<T> {
    r.map_err(|err| PyRuntimeError::new_err(format!("PuffError: {:?}", err)))
}

pub(crate) fn bootstrap_puff_globals() {
    Python::with_gil(|py| {
        println!("Adding puff....");
        let puff_mod = py.import("puff")?;
        puff_mod.setattr("get_redis", RedisGlobal)
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
    thread_objs: Vec<PyObject>,
    greenlet_context: GreenletContext,
    next: Arc<AtomicUsize>,
    blocking: bool,
    strategy: Strategy
}

impl PythonDispatcher {
    pub fn new(global_state: PyObject, threads: usize, context_waiting: Arc<Mutex<Option<PuffContext>>>) -> PyResult<Self> {
        let thread_objs = Python::with_gil(|py| {
            let puff = py.import("puff")?;
            let mut vec = Vec::with_capacity(threads);
            for _ in 0..threads {
                let ret = puff.call_method1("start_event_loop", (PythonPuffContextSetter(context_waiting.clone()), ))?;
                vec.push(ret.into_py(py))
            }
            PyResult::Ok(vec)
        })?;
        let greenlet_context = GreenletContext::new(global_state);
        let strategy = Strategy::RoundRobin;

        PyResult::Ok(Self {
            thread_objs,
            greenlet_context,
            strategy,
            next: Arc::new(AtomicUsize::new(0)),
            blocking: false
        })
    }

    pub fn blocking(global_state: PyObject) -> PyResult<Self> {
        let greenlet_context = GreenletContext::new(global_state);
        let strategy = Strategy::Random;
        PyResult::Ok(Self {
            thread_objs: Vec::new(),
            greenlet_context,
            strategy,
            next: Arc::new(AtomicUsize::new(0)),
            blocking: true
        })
    }

    pub fn dispatch<A: IntoPy<Py<PyTuple>> + Send + 'static, K: IntoPy<Py<PyDict>> + Send + 'static>(
        &self,
        function: PyObject,
        args: A,
        kwargs: K,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        if self.blocking {
            let (sender, rec) = oneshot::channel();
            self.greenlet_context.puff_context().handle().spawn_blocking(move || {
                Python::with_gil(|py| sender.send(function.call(py, args, Some(kwargs.into_py(py).as_ref(py)))).unwrap_or(()))
            });
            Ok(rec)
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
        self.find_thread_obj().call_method1(
            py,
            "spawn",
            (
                function,
                self.greenlet_context.clone(),
                args.into_py(py).to_object(py),
                kwargs.into_py(py).to_object(py),
                returner,
            ),
        )?;
        Ok(rec)
    }

    fn find_thread_obj(&self) -> PyObject {
        if self.thread_objs.len() == 1 {
            return self
                .thread_objs
                .first()
                .expect("Expected at least one Spawner")
                .clone();
        }
        match self.strategy {
            Strategy::LeastBusy => panic!("Least busy not supported in python dispatcher."),
            Strategy::RoundRobin => self.find_thread_obj_round_robin(),
            Strategy::Random => self.find_thread_obj_random(),
        }
    }

    fn find_thread_obj_round_robin(&self) -> PyObject {
        let next_count = self.next.fetch_add(1, Ordering::SeqCst);
        let l = self.thread_objs.len();
        let next_ix = next_count % l;
        let r: &PyObject = unsafe { self.thread_objs.get_unchecked(next_ix) };
        r.clone()
    }

    fn find_thread_obj_random(&self) -> PyObject {
        let mut rng = thread_rng();
        self.thread_objs
            .as_slice()
            .choose(&mut rng)
            .expect("Expected at least one Spawner")
            .clone()
    }
}


pub fn setup_greenlet(config: RuntimeConfig, context_waiting: Arc<Mutex<Option<PuffContext>>>) -> PyResult<PythonDispatcher> {
    let global_obj = config.global_state()?;
    if config.greenlets() {
        PythonDispatcher::new(global_obj, 1, context_waiting)
    } else {
        PythonDispatcher::blocking(global_obj)
    }
}
