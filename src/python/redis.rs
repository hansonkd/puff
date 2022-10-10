use crate::databases::redis::{with_redis, Cmd, RedisClient};

use crate::python::greenlet::greenlet_async;
use crate::types::Bytes;
use bb8_redis::redis::{FromRedisValue, Value};

use crate::context::with_puff_context;

use pyo3::prelude::*;


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
        Value::Bulk(vec) => vec
            .into_iter()
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
        let ctx = with_puff_context(|ctx| ctx);
        greenlet_async(ctx, return_fun, async move {
            let mut conn = client.get().await?;
            let res: T = command.query_async(&mut *conn).await?;
            Ok(res)
        });
        Ok(py.None())
    }
}

#[pymethods]
impl PythonRedis {
    fn get(&self, py: Python, return_fun: PyObject, val: &str) -> PyResult<PyObject> {
        self.run_command::<Bytes>(py, return_fun, Cmd::get(val))
    }

    fn set(&self, py: Python, return_fun: PyObject, key: &str, val: &str) -> PyResult<PyObject> {
        self.run_command::<()>(py, return_fun, Cmd::set(key, val))
    }
}
