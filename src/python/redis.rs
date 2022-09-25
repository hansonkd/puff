use bb8_redis::redis::Value;
use pyo3::exceptions::{PyException, PyValueError};
use crate::databases::redis::{Cmd, RedisClient, with_redis};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use crate::python::greenlet::ThreadDispatcher;
use crate::python::into_py_result;
use crate::types::Bytes;


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
    fn get(&self, py: Python, ctx: &ThreadDispatcher, return_fun: PyObject, val: &str) -> PyResult<PyObject> {
        let h = ctx.dispatcher().handle();
        let command = Cmd::get(val);
        let client = self.0.client();
        h.spawn(async move {
            let conn_res = client.get().await.map_err(|r| PyException::new_err(format!("Pool exception: {r}")));
            let mut conn = match conn_res {
                Ok(r) => r,
                Err(e) => {
                    Python::with_gil(|py| return_fun.call1(py, (py.None(), e))).expect("Could not return");
                    return ();
                }
            };
            let res: PyResult<Bytes> = command.query_async(&mut *conn).await.map_err(|r| PyException::new_err(format!("Redis exception: {r}")));
            match res {
                Ok(r) => Python::with_gil(|py| {
                    Python::with_gil(|py| return_fun.call1(py, (&r.into_py(py), py.None())))
                }),
                Err(e) => {
                    Python::with_gil(|py| return_fun.call1(py, (py.None(), e)))
                }
            }.expect("Could not return");
        });

        Ok(py.None())
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