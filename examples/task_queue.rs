use puff_rs::prelude::*;
use puff_rs::program::commands::{PythonCommand, WaitForever};

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .add_default_task_queue();

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
