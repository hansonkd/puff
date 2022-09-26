use pyo3::{AsPyPointer, PyResult, Python};
use pyo3::exceptions::PyRuntimeError;
use pyo3::types::PyType;
use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::prelude::*;
use crate::context::with_puff_context;
use std::os::raw::c_int;

pub mod asgi;
pub mod wsgi;
pub mod redis;
pub mod greenlet;


fn into_py_result<T>(r: PuffResult<T>) -> PyResult<T> {
    r.map_err(|err| PyRuntimeError::new_err(format!("PuffError: {:?}", err)))
}


pub(crate) fn bootstrap_puff_globals() {
    Python::with_gil(|py| {
        println!("Adding contextvars....");
        // let contextvars = py.import("contextvars")?;
        // let t = PyType::new::<ContextVar>(py);
        // contextvars.add_class::<ContextVard>()?;
        // contextvars.setattr("ContextVar", t)?;
        println!("Adding puff....");
        // contextvars.setattr("ContextVar", context_var_class)?;
        let puff_mod = py.import("puff")?;
        puff_mod.setattr("get_redis", RedisGlobal)
    }).expect("Could not set python globals");
}

pub(crate) struct PythonContext(PyObject);

#[inline]
pub fn error_on_minusone(py: Python<'_>, result: c_int) -> PyResult<()> {
    if result != -1 {
        Ok(())
    } else {
        Err(PyErr::fetch(py))
    }
}
