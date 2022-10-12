use puff_rs::graphql::handlers::{
    handle_graphql, handle_subscriptions, playground,
};

use puff_rs::program::commands::http::ServerCommand;
use puff_rs::program::Program;
use puff_rs::runtime::RuntimeConfig;


use puff_rs::web::server::Router;
use std::process::ExitCode;

fn main() -> ExitCode {
    let config = RuntimeConfig::default()
        .set_postgres(true)
        .set_gql_module("graphql_python.schema")
        .add_python_path("examples/");

    let router = Router::new()
                .get("/", playground("/graphql", "/subscriptions"))
                .post("/graphql", handle_graphql())
                .get("/subscriptions", handle_subscriptions());

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(config)
        .command(ServerCommand::new(router))
        .run()
}
