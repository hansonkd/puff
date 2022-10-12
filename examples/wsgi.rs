use futures_util::future::join_all;

use puff_rs::databases::redis::with_redis;
use puff_rs::prelude::*;
use puff_rs::databases::redis::bb8_redis::redis::Cmd;
use puff_rs::program::commands::wsgi::WSGIServerCommand;
use puff_rs::python::greenlet::greenlet_async;
use puff_rs::axum::extract::ws::{Message, WebSocket};
use puff_rs::axum::extract::WebSocketUpgrade;
use puff_rs::axum::response::Response;


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
        greenlet_async(return_fun, get_many(key, num));
        Ok(py.None())
    }
}


async fn get_many(key: Text, num: usize) -> PuffResult<Bytes> {
    let pool = with_redis(|r| r.pool());
    let mut builder = BytesBuilder::new();
    let mut queries = Vec::with_capacity(num);

    for _ in 0..num {
        let key = key.puff();
        let pool = pool.clone();
        queries.push(async move {
            let mut conn = pool.get().await?;
            PuffResult::Ok(Cmd::get(key).query_async::<_, Vec<u8>>(&mut *conn).await?)
        })
    }

    let results = join_all(queries).await;

    for result in results {
        let res: Vec<u8> = result?;
        builder.put_slice(res.as_slice());
    }

    Ok(builder.into_bytes())
}

async fn handle_root() -> RequestResult<Bytes> {
    Ok(get_many("mykey".to_text(), 10).await?)
}

fn main() -> ExitCode {
    let router = Router::new()
        .get("/", handle_root);

    let rc = RuntimeConfig::default()
        .set_postgres(true)
        .set_redis(true)
        .set_pubsub(true)
        .set_global_state_fn(|py| Ok(MyState.into_py(py)));

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new_with_router("flask_example.app", router))
        .run()
}
