use puff::program::commands::wsgi::WSGIServerCommand;
use puff::program::Program;
use puff::types::text::{Text, ToText};
use puff::errors::Result;
use puff::web::server::Router;
use puff::web::client::{Client, PuffClientResponse, PuffRequestBuilder};
use std::time::Duration;
use pyo3::{PyObject, Python};
use puff::runtime::RuntimeConfig;


fn main() {
    let app = Router::new().get("/rust/", || Ok("Cool"));
    let rc = RuntimeConfig::default().set_redis(true);

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new(app, "flask_example", "app"))
        .run()
}
