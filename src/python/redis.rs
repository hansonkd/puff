use crate::databases::redis::{with_redis, RedisClient};

use crate::python::greenlet::greenlet_async;
use crate::types::Bytes;
use bb8_redis::redis::{Cmd, FromRedisValue, RedisResult, Value};



use pyo3::prelude::*;
use pyo3::types::PyBytes;

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
struct PyRedisValue(PyObject);

impl ToPyObject for PyRedisValue {
    fn to_object(&self, _py: Python<'_>) -> PyObject {
        self.0.clone()
    }
}

impl FromRedisValue for PyRedisValue {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        Python::with_gil(|py| Ok(PyRedisValue(extract_redis_to_python(py, v))))
    }
}

#[pyclass]
pub struct PythonRedis(RedisClient);

fn extract_redis_to_python(py: Python, val: &Value) -> PyObject {
    match val {
        Value::Int(i) => i.into_py(py),
        Value::Data(v) => PyBytes::new(py, v.as_slice()).into_py(py),
        Value::Nil => py.None(),
        Value::Bulk(vec) => vec
            .iter()
            .map(|v| extract_redis_to_python(py, v))
            .collect::<Vec<PyObject>>()
            .into_py(py),
        Value::Status(s) => s.into_py(py),
        Value::Okay => RedisOk.into_py(py),
    }
}

impl PythonRedis {
    fn run_command<T: ToPyObject + FromRedisValue + 'static>(
        &self,
        py: Python,
        return_fun: PyObject,
        command: Cmd,
    ) -> PyResult<PyObject> {
        let client = self.0.pool();
        greenlet_async(return_fun, async move {
            let mut conn = client.get().await?;
            let res: T = command.query_async(&mut *conn).await?;
            Ok(res)
        });
        Ok(py.None())
    }
}


#[pymethods]
impl PythonRedis {
    fn get(&self, py: Python, return_fun: PyObject, val: &[u8]) -> PyResult<PyObject> {
        self.run_command::<Bytes>(py, return_fun, Cmd::get(val))
    }

    fn set(&self, py: Python, return_fun: PyObject, key: &[u8], val: &[u8], ex: Option<usize>, nx: Option<bool>) -> PyResult<PyObject> {
        let nx = nx.unwrap_or_default();
        if nx {
            match ex {
                Some(seconds) => {
                    let mut command = Cmd::set(key, val);
                    command.arg("NX").arg("EX").arg(seconds);
                    self.run_command::<()>(py, return_fun, command)
                },
                None => self.run_command::<()>(py, return_fun, Cmd::set_nx(key, val))
            }
        } else {
            match ex {
                Some(seconds) => self.run_command::<()>(py, return_fun, Cmd::set_ex(key, val, seconds)),
                None => self.run_command::<()>(py, return_fun, Cmd::set(key, val))
            }
        }

    }

    fn mget(&self, py: Python, return_fun: PyObject, keys: Vec<&[u8]>) -> PyResult<PyObject> {
        self.run_command::<Vec<Option<Bytes>>>(py, return_fun, Cmd::get(keys))
    }

    fn mset(&self, py: Python, return_fun: PyObject, vals: Vec<(&[u8], &[u8])>, nx: Option<bool>) -> PyResult<PyObject> {
        let nx = nx.unwrap_or_default();
        if nx {
            let mut command  = Cmd::set_multiple(vals.as_slice());
            command.arg("NX");
            self.run_command::<()>(py, return_fun, command)
        } else {
            self.run_command::<()>(py, return_fun, Cmd::set_multiple(vals.as_slice()))
        }
    }

    fn persist(&self, py: Python, return_fun: PyObject, key: &[u8]) -> PyResult<PyObject> {
        self.run_command::<bool>(py, return_fun, Cmd::persist(key))
    }

    fn expire(&self, py: Python, return_fun: PyObject, key: &[u8], seconds: usize) -> PyResult<PyObject> {
        self.run_command::<bool>(py, return_fun, Cmd::expire(key, seconds))
    }

    fn delete(&self, py: Python, return_fun: PyObject, key: &[u8]) -> PyResult<PyObject> {
        self.run_command::<bool>(py, return_fun, Cmd::del(key))
    }

    fn incr(&self, py: Python, return_fun: PyObject, key: &[u8], delta: i64) -> PyResult<PyObject> {
        self.run_command::<i64>(py, return_fun, Cmd::incr(key, delta))
    }

    fn decr(&self, py: Python, return_fun: PyObject, key: &[u8], delta: i64) -> PyResult<PyObject> {
        self.run_command::<i64>(py, return_fun, Cmd::decr(key, delta))
    }

    fn flushdb(&self, py: Python, return_fun: PyObject) -> PyResult<PyObject> {
        let mut command = Cmd::new();
        command.arg("FLUSHDB");
        self.run_command::<bool>(py, return_fun, command)
    }

    fn command(
        &self,
        py: Python,
        return_fun: PyObject,
        command: Vec<&PyBytes>,
    ) -> PyResult<PyObject> {
        let mut cmd = Cmd::new();
        for part in command {
            cmd.arg(part.as_bytes());
        }
        self.run_command::<PyRedisValue>(py, return_fun, cmd)
    }
}
