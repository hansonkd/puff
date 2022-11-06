use std::num::NonZeroUsize;

use crate::databases::redis::{with_redis, RedisClient};

use crate::python::async_python::run_python_async;
use bb8_redis::redis::{Cmd, ErrorKind, FromRedisValue, RedisError, RedisResult, Value};

use crate::python;
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};

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

struct PyRedisBytes(Py<PyBytes>);

impl FromRedisValue for PyRedisBytes {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        match v {
            Value::Data(v) => Ok(Python::with_gil(|py| {
                PyRedisBytes(PyBytes::new(py, v.as_slice()).into_py(py))
            })),
            val => Err(RedisError::from((
                ErrorKind::TypeError,
                "Response was of incompatible type",
                format!(
                    "Response type not bytes compatible. (response was {:?})",
                    val
                ),
            ))),
        }
    }
}

impl ToPyObject for PyRedisBytes {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.0.to_object(py)
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
        run_python_async(return_fun, async move {
            let mut conn = client.get().await?;
            let res: T = command.query_async(&mut *conn).await?;
            Ok(res)
        });
        Ok(py.None())
    }
}

#[pymethods]
impl PythonRedis {
    fn get(&self, py: Python, return_fun: PyObject, key: &PyAny) -> PyResult<PyObject> {
        self.run_command::<PyRedisBytes>(py, return_fun, Cmd::get(python::py_obj_to_bytes(key)?))
    }

    fn set(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        val: &PyAny,
        ex: Option<usize>,
        nx: Option<bool>,
    ) -> PyResult<PyObject> {
        let nx = nx.unwrap_or_default();
        if nx {
            match ex {
                Some(seconds) => {
                    let mut command =
                        Cmd::set(python::py_obj_to_bytes(key)?, python::py_obj_to_bytes(val)?);
                    command.arg("NX").arg("EX").arg(seconds);
                    self.run_command::<()>(py, return_fun, command)
                }
                None => self.run_command::<()>(
                    py,
                    return_fun,
                    Cmd::set_nx(python::py_obj_to_bytes(key)?, python::py_obj_to_bytes(val)?),
                ),
            }
        } else {
            match ex {
                Some(seconds) => self.run_command::<()>(
                    py,
                    return_fun,
                    Cmd::set_ex(
                        python::py_obj_to_bytes(key)?,
                        python::py_obj_to_bytes(val)?,
                        seconds,
                    ),
                ),
                None => self.run_command::<()>(
                    py,
                    return_fun,
                    Cmd::set(python::py_obj_to_bytes(key)?, python::py_obj_to_bytes(val)?),
                ),
            }
        }
    }

    fn mget(&self, py: Python, return_fun: PyObject, keys: &PyList) -> PyResult<PyObject> {
        let mut vec = Vec::with_capacity(keys.len());
        for key in keys {
            vec.push(python::py_obj_to_bytes(key)?);
        }
        self.run_command::<Vec<Option<PyRedisBytes>>>(py, return_fun, Cmd::get(vec))
    }

    fn mset(
        &self,
        py: Python,
        return_fun: PyObject,
        vals: &PyAny,
        nx: Option<bool>,
    ) -> PyResult<PyObject> {
        let mut vec = Vec::new();
        for row in vals.iter()? {
            let mut it = row?.iter()?;
            let key = it.next().ok_or(PyTypeError::new_err(
                "Expected two elements in mset element",
            ))??;
            let value = it.next().ok_or(PyTypeError::new_err(
                "Expected two elements in mset element",
            ))??;
            vec.push((
                python::py_obj_to_bytes(key)?,
                python::py_obj_to_bytes(value)?,
            ));
        }
        let nx = nx.unwrap_or_default();
        if nx {
            let mut command = Cmd::set_multiple(vec.as_slice());
            command.arg("NX");
            self.run_command::<()>(py, return_fun, command)
        } else {
            self.run_command::<()>(py, return_fun, Cmd::set_multiple(vec.as_slice()))
        }
    }

    fn persist(&self, py: Python, return_fun: PyObject, key: &PyAny) -> PyResult<PyObject> {
        self.run_command::<bool>(py, return_fun, Cmd::persist(python::py_obj_to_bytes(key)?))
    }

    fn expire(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        seconds: usize,
    ) -> PyResult<PyObject> {
        self.run_command::<bool>(
            py,
            return_fun,
            Cmd::expire(python::py_obj_to_bytes(key)?, seconds),
        )
    }

    fn delete(&self, py: Python, return_fun: PyObject, key: &PyAny) -> PyResult<PyObject> {
        self.run_command::<bool>(py, return_fun, Cmd::del(python::py_obj_to_bytes(key)?))
    }

    fn incr(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        delta: i64,
    ) -> PyResult<PyObject> {
        self.run_command::<i64>(
            py,
            return_fun,
            Cmd::incr(python::py_obj_to_bytes(key)?, delta),
        )
    }

    fn decr(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        delta: i64,
    ) -> PyResult<PyObject> {
        self.run_command::<i64>(
            py,
            return_fun,
            Cmd::decr(python::py_obj_to_bytes(key)?, delta),
        )
    }

    fn blpop(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        timeout: usize,
    ) -> PyResult<PyObject> {
        self.run_command::<Option<(PyRedisBytes, PyRedisBytes)>>(
            py,
            return_fun,
            Cmd::blpop(python::py_obj_to_bytes(key)?, timeout),
        )
    }

    fn brpop(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        timeout: usize,
    ) -> PyResult<PyObject> {
        self.run_command::<Option<(PyRedisBytes, PyRedisBytes)>>(
            py,
            return_fun,
            Cmd::brpop(python::py_obj_to_bytes(key)?, timeout),
        )
    }

    fn rpop(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        count: Option<usize>,
    ) -> PyResult<PyObject> {
        self.run_command::<Vec<PyRedisBytes>>(
            py,
            return_fun,
            Cmd::rpop(
                python::py_obj_to_bytes(key)?,
                count.map(NonZeroUsize::new).unwrap_or_default(),
            ),
        )
    }

    fn lpop(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        count: Option<usize>,
    ) -> PyResult<PyObject> {
        self.run_command::<Vec<PyRedisBytes>>(
            py,
            return_fun,
            Cmd::lpop(
                python::py_obj_to_bytes(key)?,
                count.map(NonZeroUsize::new).unwrap_or_default(),
            ),
        )
    }

    fn rpush(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        value: &PyAny,
    ) -> PyResult<PyObject> {
        self.run_command::<i64>(
            py,
            return_fun,
            Cmd::rpush(
                python::py_obj_to_bytes(key)?,
                python::py_obj_to_bytes(value)?,
            ),
        )
    }

    fn lpush(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        value: &PyAny,
    ) -> PyResult<PyObject> {
        self.run_command::<i64>(
            py,
            return_fun,
            Cmd::lpush(
                python::py_obj_to_bytes(key)?,
                python::py_obj_to_bytes(value)?,
            ),
        )
    }

    fn rpoplpush(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &PyAny,
        destination: &PyAny,
    ) -> PyResult<PyObject> {
        self.run_command::<PyRedisBytes>(
            py,
            return_fun,
            Cmd::rpoplpush(
                python::py_obj_to_bytes(key)?,
                python::py_obj_to_bytes(destination)?,
            ),
        )
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
