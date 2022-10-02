extern crate core;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::WebSocketUpgrade;

use axum::response::Response;
use bb8_redis::redis::Cmd;
use futures_util::future::join_all;

use puff::databases::redis::with_redis;
use puff::errors::PuffResult;
use puff::program::commands::wsgi::WSGIServerCommand;
use puff::program::Program;
use puff::python::greenlet::greenlet_async;
use puff::runtime::{yield_to_future, RuntimeConfig};

use puff::context::with_puff_context;
use puff::tasks::Task;
use puff::types::{Bytes, BytesBuilder, Puff, Text};
use puff::web::server::Router;
use pyo3::prelude::*;

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
        let ctx = with_puff_context(|ctx| ctx);
        greenlet_async(ctx, return_fun, get_many(key, num));
        Ok(py.None())
    }
}

async fn on_upgrade(mut socket: WebSocket) {
    let pubsub = with_puff_context(|ctx| ctx.pubsub());
    let (conn, mut rec) = pubsub.connection().expect("No connection");
    conn.subscribe("hello").await.unwrap().unwrap();

    loop {
        tokio::select! {
            Some(v) = rec.recv() => {
                let text = v.text().unwrap_or("invalid utf8".into());
                let msg = format!("{} said {}", v.from(), text);
                if socket.send(Message::Text(msg)).await.is_err() {
                    // client disconnected
                    return;
                }
            },
            Some(msg) = socket.recv() => {
                if let Ok(msg) = msg {
                    conn.publish("hello", msg.into_data()).await.unwrap().unwrap();
                } else {
                    // client disconnected
                    return;
                };
            },
            else => {
                // client disconnected
                break
            }
        }
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

fn main() {
    let router = Router::new()
        .get("/deepest/", || {
            yield_to_future(get_many("blam".into(), 1000))
        })
        .get::<_, _, _, Response>("/ws/", ws_handler);

    let rc = RuntimeConfig::default()
        .set_postgres(true)
        .set_redis(true)
        .set_pubsub(true)
        .set_global_state_fn(|py| Ok(MyState.into_py(py)));

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new(router, "flask_example.app"))
        .run()
}

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(on_upgrade)
}
