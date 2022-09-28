extern crate core;

use axum::extract::WebSocketUpgrade;
use axum::extract::ws::{Message, WebSocket};
use axum::http::Request;
use axum::response::Response;
use bb8_redis::redis::Cmd;
use futures_util::future::join_all;
use hyper::Body;
use puff::databases::redis::with_redis;
use puff::errors::PuffResult;
use puff::program::commands::wsgi::WSGIServerCommand;
use puff::program::Program;
use puff::python::greenlet::greenlet_async;
use puff::runtime::RuntimeConfig;

use puff::context::with_puff_context;
use puff::tasks::Task;
use puff::types::{Bytes, BytesBuilder};
use puff::web::server::Router;
use pyo3::prelude::*;
use tracing::info;

#[pyclass]
#[derive(Clone)]
struct MyState;

#[pymethods]
impl MyState {
    fn concat_redis_gets(
        &self,
        py: Python,
        return_fun: PyObject,
        key: &str,
        num: usize,
    ) -> PyResult<PyObject> {
        let mut vec = Vec::with_capacity(num);
        for _ in 0..num {
            vec.push(Cmd::get(key))
        }
        self.run_command_concat(py, return_fun, vec)
    }
}

impl MyState {
    fn run_command_concat(
        &self,
        py: Python,
        return_fun: PyObject,
        commands: Vec<Cmd>,
    ) -> PyResult<PyObject> {
        let ctx = with_puff_context(|ctx| ctx);
        let pool = ctx.redis().pool();

        greenlet_async(ctx, return_fun, async move {
            let mut builder = BytesBuilder::new();
            let mut queries = Vec::with_capacity(commands.len());

            for cmd in commands {
                let pool = pool.clone();
                queries.push(async move {
                    let mut conn = pool.get().await?;
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

async fn on_upgrade(mut socket: WebSocket) {
    let pubsub = with_puff_context(|ctx| ctx.pubsub());
    let (conn, mut rec) = pubsub.connection().expect("No connection");
    conn.subscribe("hello").await.unwrap().unwrap();

    loop {
        tokio::select! {
            Some(v) = rec.recv() => {
                info!("Sending pubsub");
                let text = v.text().unwrap_or("invalid utf8".into());
                let msg = format!("{} said {}", v.from(), text);
                if socket.send(Message::Text(msg)).await.is_err() {
                    // client disconnected
                    return;
                }
            },
            Some(msg) = socket.recv() => {
                if let Ok(msg) = msg {
                    info!("Got Message");
                    conn.publish("hello", msg.into_data()).await.unwrap().unwrap();
                } else {
                    // client disconnected
                    return;
                };
            },
            else => {
                break
            }
        }
    }
    // while let Some(msg) = socket.recv().await {
    //     let msg = if let Ok(msg) = msg {
    //         msg
    //     } else {
    //         // client disconnected
    //         return;
    //     };
    //
    //     if socket.send(msg).await.is_err() {
    //         // client disconnected
    //         return;
    //     }
    // }
}

fn main() {
    let router = Router::new().get("/rust/", || {
        let r = with_redis(|r| {
            let mut builder = BytesBuilder::new();
            let mut tasks = Vec::new();
            for _ in 0..1000 {
                let r = r.clone();
                tasks.push(Task::spawn(move || r.query::<Bytes>(Cmd::get("blam"))))
            }

            let results = puff::tasks::join_all(tasks);

            for res in results {
                builder.put_slice(res?.as_ref())
            }

            PuffResult::Ok(builder.into_bytes())
        })?;
        Ok(r)
    }).get::<_, _, _, Response>("/ws/", ws_handler);

    let rc = RuntimeConfig::default()
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