
use puff::graphql::handlers::{
    handle_graphql, handle_subscriptions, playground,
};

use puff::program::commands::http::ServerCommand;
use puff::program::Program;
use puff::runtime::RuntimeConfig;


use puff::web::server::Router;
use std::process::ExitCode;

fn main() -> ExitCode {
    let config = RuntimeConfig::default()
        .set_postgres(true)
        .set_gql_module("graphql_python.schema")
        .add_python_path("examples/");

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(config)
        .command(ServerCommand::new(|| {
            Router::new()
                .get("/", playground("/graphql", "/subscriptions"))
                .post("/graphql", handle_graphql())
                .get("/subscriptions", handle_subscriptions())
        }))
        .run()
}
