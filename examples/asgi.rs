use puff_rs::prelude::*;
use puff_rs::program::commands::ASGIServerCommand;

fn main() -> ExitCode {
    let app = Router::new().get("/", root);
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true);

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(ASGIServerCommand::new_with_router(
            "fast_api_example.app",
            app,
        ))
        .run()
}

// Basic handler that responds with a static string
async fn root() -> Text {
    "Ok".to_text()
}
