use pyo3::{PyResult, Python};
use pyo3::exceptions::PyRuntimeError;
use pyo3::ffi::PyObject;
use pyo3::types::PyType;
use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::prelude::*;
use crate::runtime::with_dispatcher;

pub mod asgi;
pub mod wsgi;
pub mod redis;


fn into_py_result<T>(r: PuffResult<T>) -> PyResult<T> {
    r.map_err(|err| PyRuntimeError::new_err(format!("PuffError: {:?}", err)))
}

#[pyclass]
struct Token {
    old_value: Option<Py<PyAny>>
}

#[pyclass]
struct ContextVar {
    name: String,
    default: Option<Py<PyAny>>
}

#[pymethods]
impl ContextVar {
    #[new]
    fn new(name: String, default: Option<Py<PyAny>>) -> Self {
        Self {
            name,
            default
        }
    }

    fn set(&mut self, value: Py<PyAny>) -> Token {
        let old_value = with_dispatcher(|d| {
            d.with_mutable_context_vars(|vars| {
                vars.insert(self.name.clone(), value.clone())
            })
        });
        Token{ old_value }
    }

    fn reset(&mut self, token: &Token) -> () {
        match &token.old_value {
            Some(t) => {
                self.set(t.clone());
            }
            None => {
                ()
            }
        }
    }

    fn get(&self, py: Python, default: Option<Py<PyAny>>) -> Option<Py<PyAny>> {
        with_dispatcher(|d| {
            d.with_context_vars(|vars| {
                vars.get(&self.name).map(|v| v.into_py(py)).or(default).or(self.default.clone())
            })
        })
    }
}

pub(crate) fn bootstrap_puff_globals() {
    Python::with_gil(|py| {
        println!("Adding contextvars....");
        let contextvars = py.import("contextvars")?;
        let t = PyType::new::<ContextVar>(py);
        // contextvars.add_class::<ContextVard>()?;
        contextvars.setattr("ContextVar", t)?;
        println!("Adding puff....");
        // contextvars.setattr("ContextVar", context_var_class)?;
        let puff_mod = py.import("puff")?;
        puff_mod.setattr("get_redis", RedisGlobal)
    }).unwrap();
}