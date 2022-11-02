use puff_rs::prelude::*;
use puff_rs::program::commands::PytestCommand;


fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .set_task_queue(true)
        .set_redis(true)
        .set_pubsub(true);

    Program::new("my_first_app")
    .about("This is my first app")
    .runtime_config(rc)
    .command(PytestCommand::new(
        "./examples/python_tests",
    ))
    .run()
}
