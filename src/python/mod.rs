
use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use pyo3::{PyResult, Python};
use std::os::raw::c_int;

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
