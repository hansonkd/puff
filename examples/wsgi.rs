use bb8_redis::redis::Cmd;
use futures_util::future::join_all;
use puff::databases::redis::with_redis;
use puff::errors::PuffResult;
use puff::program::commands::wsgi::WSGIServerCommand;
use puff::program::Program;
use puff::python::greenlet::greenlet_async;
use puff::runtime::RuntimeConfig;

use puff::context::with_puff_context;
use puff::types::{Bytes, BytesBuilder};
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

fn main() {
    let router = Router::new().get("/rust/", || {
        let r = with_redis(|r| {
            let mut builder = BytesBuilder::new();
            for _ in 0..1000 {
                let res = r.query::<Bytes>(Cmd::get("blam"))?;
                builder.put(res)
            }
            PuffResult::Ok(builder.into_bytes())
        })?;
        Ok(r)
    });
    let rc = RuntimeConfig::default()
        .set_redis(true)
        .set_global_state_fn(|py| Ok(MyState.into_py(py)));

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new(router, "flask_example", "app"))
        .run()
}
