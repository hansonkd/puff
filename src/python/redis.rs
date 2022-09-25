use std::future::Future;
use bb8_redis::redis::{FromRedisValue, Value};
use futures_util::future::join_all;
use pyo3::exceptions::{PyException, PyValueError};
use crate::databases::redis::{Cmd, RedisClient, with_redis};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use crate::errors::PuffResult;
use crate::python::greenlet::{greenlet_async, ThreadDispatcher};
use crate::python::into_py_result;
use crate::types::{Bytes, BytesBuilder};


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

async fn handle_it<F: Future<Output=PyResult<PyObject>>>(return_fun: PyObject, f: F) {
    let res = f.await;
    Python::with_gil(|py| match res {
        Ok(r) => return_fun.call1(py, (r, py.None())).expect("Could not return value"),
        Err(e) => {
            return_fun.call1(py, (py.None(), e)).expect("Could not return exception")
        }
    });
}


impl PythonRedis {
    fn run_command<T: ToPyObject + FromRedisValue + 'static>(&self, py: Python, ctx: &ThreadDispatcher, return_fun: PyObject, command: Cmd) -> PyResult<PyObject> {
        let client = self.0.client();
        greenlet_async(ctx, return_fun, async move {
            let mut conn = client.get().await?;
            let res: T = command.query_async(&mut *conn).await?;
            Ok(res)
        });
        Ok(py.None())
    }

    fn run_command_concat(&self, py: Python, ctx: &ThreadDispatcher, return_fun: PyObject, commands: Vec<Cmd>) -> PyResult<PyObject> {
        let client = self.0.client();
        greenlet_async(ctx, return_fun, async move {
            let mut builder = BytesBuilder::new();
            let mut queries = Vec::with_capacity(commands.len());

            for cmd in commands {
                let client = client.clone();
                queries.push(async move {
                    let mut conn = client.get().await?;
                    PuffResult::Ok(cmd.query_async::<_, Vec<u8>>(&mut *conn).await?)
                })
            }

            let results = join_all(queries).await;

            for result in results {
                let res: Vec<u8> = result?;
                builder.put_slice(res.as_slice());
            }

            let res = builder.into_bytes();
            Ok(res)
        });
        Ok(py.None())
    }
}

#[pymethods]
impl PythonRedis {
    fn get(&self, py: Python, ctx: &ThreadDispatcher, return_fun: PyObject, val: &str) -> PyResult<PyObject> {
        self.run_command::<Bytes>(py, ctx, return_fun, Cmd::get(val))
    }

    fn get_ten(&self, py: Python, ctx: &ThreadDispatcher, return_fun: PyObject, val: &str) -> PyResult<PyObject> {
        let mut vec = Vec::with_capacity(10);
        for _ in 0..10 {
            vec.push(Cmd::get(val))
        }
        self.run_command_concat(py, ctx, return_fun, vec)
    }

    fn set(&self, py: Python, ctx: &ThreadDispatcher, return_fun: PyObject, key: &str, val: &str) -> PyResult<PyObject> {
        self.run_command::<()>(py, ctx, return_fun, Cmd::set(key, val))
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