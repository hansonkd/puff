use puff_rs::prelude::*;
use puff_rs::program::commands::PytestCommand;

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .add_default_task_queue()
        .add_default_redis()
        .add_default_postgres()
        .add_default_pubsub()
        .add_gql_schema("graphql_python.Schema")
        .add_gql_schema_named("alt", "graphql_python.AltSchema");

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(PytestCommand::new("./examples/python_tests"))
        .run()
}
