use pyo3::exceptions::PyValueError;
use crate::databases::redis::{Cmd, RedisClient, with_redis};
use pyo3::prelude::*;
use crate::python::into_py_result;


#[pyclass]
pub struct RedisGlobal;

impl ToPyObject for RedisGlobal {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        RedisGlobal.into_py(py)
    }
}

#[pymethods]
impl RedisGlobal {
    fn __call__(&self) -> PythonRedis {
        PythonRedis(with_redis(|r| r))
    }
}

#[pyclass]
struct PythonRedis(RedisClient);


#[pymethods]
impl PythonRedis {
    fn get(&self, py: Python, val: &str) -> PyResult<String> {
        py.allow_threads(|| into_py_result(self.0.query(Cmd::get(val))))
    }

    fn set(&self, py: Python, key: &str, val: &str) -> PyResult<()> {
       py.allow_threads(|| into_py_result(self.0.query(Cmd::set(key, val))))
    }
}