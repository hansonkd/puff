use bb8_redis::redis::Value;
use pyo3::exceptions::PyValueError;
use crate::databases::redis::{Cmd, RedisClient, with_redis};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
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
struct RedisOk;

#[pyclass]
pub struct PythonRedis(RedisClient);


fn extract_redis_to_python(py: Python, val: Value) -> PyObject {
    match val {
        Value::Int(i) => i.into_py(py),
        Value::Data(v) => v.into_py(py),
        Value::Nil => py.None().into_py(py),
        Value::Bulk(vec) => vec.into_iter().map(|v| extract_redis_to_python(py, v)).collect::<Vec<PyObject>>().into_py(py),
        Value::Status(s) => s.into_py(py),
        Value::Okay => RedisOk.into_py(py),
    }
}

#[pymethods]
impl PythonRedis {
    fn get(&self, py: Python, val: &str) -> PyResult<String> {
        py.allow_threads(|| into_py_result(self.0.query(Cmd::get(val))))
    }

    fn set(&self, py: Python, key: &str, val: &str) -> PyResult<()> {
       py.allow_threads(|| into_py_result(self.0.query(Cmd::set(key, val))))
    }

    fn command(&self, py: Python, command: &PyList) -> PyResult<PyObject> {
        let mut cmd = Cmd::new();
        for arg in command {
            if let Ok(v) = arg.downcast::<PyBytes>() {
                cmd.arg(v.as_bytes());
            } else {
                return Err(PyValueError::new_err("Redis command expected a list of bytes."));
            }
        }
       py.allow_threads(|| into_py_result(self.0.query(cmd).map(|v|
           Python::with_gil(|inner_py| extract_redis_to_python(inner_py, v) ))))
    }
}