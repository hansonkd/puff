use puff::errors::Result;
use puff::graphql::handlers::{graphql_execute, graphql_subscriptions, playground};
use puff::graphql::{load_schema, AggroContext};
use puff::program::commands::http::ServerCommand;
use puff::program::Program;
use puff::runtime::RuntimeConfig;
use puff::types::text::Text;
use puff::web::client::{Client, PuffClientResponse, PuffRequestBuilder};
use puff::web::server::Router;
use std::process::ExitCode;

fn main() -> ExitCode {
    // build our application with a route
    // .post("/graphql/", make_graphql_python_service("my_module.Query"))
    // .fallback(make_wsgi_service("my_module.app"));
    let config = RuntimeConfig::default()
        .set_postgres(true)
        .add_python_path("examples/");
    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(config)
        .command(ServerCommand::new(|| {
            let ctx = AggroContext::new();
            let schema = load_schema("graphql_python.schema").expect("Expected Schema");
            Router::new()
                .get("/", playground("/graphql", "/subscriptions"))
                .post("/graphql", graphql_execute(schema.clone(), ctx))
                .get("/subscriptions", graphql_subscriptions(schema.clone(), ctx))
        }))
        .run()
}
