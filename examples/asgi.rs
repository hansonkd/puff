use puff::program::commands::asgi::ASGIServerCommand;
use puff::program::Program;
use puff::types::text::{Text, ToText};
use puff::errors::Result;
use puff::web::server::Router;
use puff::web::client::{Client, PuffClientResponse, PuffRequestBuilder};
use std::time::Duration;
use pyo3::{PyObject, Python};
use puff::runtime::RuntimeConfig;


fn main() {
    let app = Router::new().get("/", root);
    let rc = RuntimeConfig::default().set_asyncio(true);

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(ASGIServerCommand::new(app, "fastapi_example", "app"))
        .run()
}

// Basic handler that responds with a static string
fn root() -> Result<Text> {
    let client = Client::new();
    client.get("http://google.com/").puff_response()?.puff_text()
}
