use futures_util::future::join_all;

use puff_rs::databases::redis::bb8_redis::redis::Cmd;
use puff_rs::databases::redis::with_redis;
use puff_rs::prelude::*;
use puff_rs::program::commands::wsgi::WSGIServerCommand;
use puff_rs::python::async_python::run_python_async;

#[pyclass]
#[derive(Clone)]
struct MyState;

#[pymethods]
impl MyState {
    fn concat_redis_gets(
        &self,
        py: Python,
        return_fun: PyObject,
        key: Text,
        num: usize,
    ) -> PyResult<PyObject> {
        run_python_async(return_fun, get_many(key, num));
        Ok(py.None())
    }
}

async fn get_many(key: Text, num: usize) -> PuffResult<PyObject> {
    let pool = with_redis(|r| r.pool());
    let mut builder = BytesBuilder::new();
    let mut queries = Vec::with_capacity(num);

    for _ in 0..num {
        let key = key.puff();
        let pool = pool.clone();
        queries.push(async move {
            let mut conn = pool.get().await?;
            PuffResult::Ok(Cmd::get(key).query_async::<Vec<u8>>(&mut *conn).await?)
        })
    }

    let results = join_all(queries).await;

    for result in results {
        let res: Vec<u8> = result?;
        builder.put_slice(res.as_slice());
    }

    let bytes = builder.into_bytes();
    Ok(Python::with_gil(|py| {
        pyo3::types::PyBytes::new(py, bytes.as_ref())
            .into_any()
            .unbind()
    }))
}

async fn handle_root() -> RequestResult<Bytes> {
    let pool = with_redis(|r| r.pool());
    let mut conn = pool.get().await.map_err(|e| anyhow::anyhow!("{}", e))?;
    let result: Vec<u8> = Cmd::get("mykey")
        .query_async(&mut *conn)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(Bytes::copy_from_slice(&result))
}

fn main() -> ExitCode {
    let router = Router::new().get("/", handle_root);

    let rc = RuntimeConfig::default()
        .add_default_postgres()
        .add_default_redis()
        .add_default_pubsub()
        .set_global_state_fn(|py| pyo3::Py::new(py, MyState).map(Into::into));

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new_with_router(
            "flask_example.app",
            router,
        ))
        .run()
}
