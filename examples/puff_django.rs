use puff_rs::program::commands::django_management::DjangoManagementCommand;
use puff_rs::program::commands::pytest::PytestCommand;
use puff_rs::program::commands::wsgi::WSGIServerCommand;
use puff_rs::program::Program;
use puff_rs::runtime::RuntimeConfig;

use std::process::ExitCode;

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_env("DJANGO_SETTINGS_MODULE", "puff_django_example.settings")
        .add_python_path("examples/puff_django_example")
        .add_python_path("examples/")
        .add_default_postgres()
        .add_default_redis()
        .add_default_pubsub();

    Program::new("puff_django_app_example")
        .about("This is my first django app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new(
            "puff_django_example.wsgi.application",
        ))
        .command(DjangoManagementCommand::new())
        .command(PytestCommand::new("examples/puff_django_example"))
        .run()
}
