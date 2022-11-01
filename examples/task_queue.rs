use puff_rs::prelude::*;
use puff_rs::program::commands::{PythonCommand, WaitForever};
use reqwest::ClientBuilder;

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .set_http_client_builder_fn(|| ClientBuilder::new().http2_prior_knowledge())
        .set_task_queue(true);

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(PythonCommand::new(
            "run_main",
            "task_queue_example.run_main",
        ))
        .command(WaitForever)
        .run()
}