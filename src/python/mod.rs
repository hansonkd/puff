use pyo3::{AsPyPointer, PyResult, Python};
use pyo3::exceptions::PyRuntimeError;
use pyo3::types::PyType;
use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;
use pyo3::prelude::*;
use crate::runtime::with_dispatcher;
use std::os::raw::c_int;

pub mod asgi;
pub mod wsgi;
pub mod redis;
pub mod greenlet;


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

impl PythonContext {
    pub(crate) fn copy_context() -> Self {
        Python::with_gil(|py| {
            let ctx = unsafe{
                let r = pyo3::ffi::PyContext_New();
                py.from_owned_ptr::<PyAny>(r)
            };
            PythonContext(ctx.into_py(py))
        })
    }

    pub(crate) fn enter_context(&self) -> PyResult<()> {
        Python::with_gil(|py| {
            println!("Entering pycontext");
            unsafe{
                // let new_ctx = pyo3::ffi::PyContext_Copy(self.0.as_ref(py).as_ptr());
                let ptr = self.0.as_ref(py).as_ptr();
                error_on_minusone(
                    py,
                    pyo3::ffi::PyContext_Enter(ptr),
                )?;
                pyo3::ffi::Py_DECREF(ptr);
                Ok(())
            }
        })

    }

    pub(crate) fn exit_context(&self) -> PyResult<()> {
        Python::with_gil(|py| {
            println!("Exiting pycontext");
            unsafe { error_on_minusone(py, pyo3::ffi::PyContext_Exit(self.0.as_ref(py).as_ptr())) }
        })
    }
}
