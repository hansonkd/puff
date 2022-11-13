use puff_rs::prelude::*;
use puff_rs::program::commands::PytestCommand;
use puff_rs::runtime::{GqlOpts, PostgresOpts, PubSubOpts, RedisOpts, TaskQueueOpts};

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .add_default_task_queue()
        .add_named_task_queue("alttaskqueue", TaskQueueOpts::default())
        .add_default_redis()
        .add_named_redis("altredis", RedisOpts::default())
        .add_default_postgres()
        .add_named_postgres("altpostgres", PostgresOpts::default())
        .add_default_pubsub()
        .add_named_pubsub("altpubsub", PubSubOpts::default())
        .add_gql_schema_named(
            "default",
            GqlOpts::new("graphql_python.Schema", Some("default".into())),
        )
        .add_gql_schema_named("alt", GqlOpts::new("graphql_python.AltSchema", None));

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(PytestCommand::new("./examples/python_tests"))
        .run()
}
