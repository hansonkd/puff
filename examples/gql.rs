use puff_rs::graphql::handlers::{handle_graphql, handle_subscriptions, playground};

use puff_rs::program::commands::http::ServerCommand;
use puff_rs::program::Program;
use puff_rs::runtime::RuntimeConfig;

use puff_rs::web::server::Router;
use std::process::ExitCode;

fn main() -> ExitCode {
    let config = RuntimeConfig::default()
        .add_default_postgres()
        .add_gql_schema("graphql_python.Schema")
        .add_python_path("examples/");

    let router = Router::new()
        .get("/", playground("/graphql", Some("/subscriptions")))
        .post("/graphql", handle_graphql())
        .get("/subscriptions", handle_subscriptions());

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(config)
        .command(ServerCommand::new(router))
        .run()
}
