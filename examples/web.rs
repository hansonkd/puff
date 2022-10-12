use puff_rs::prelude::*;
use puff_rs::program::commands::ServerCommand;

fn main() -> ExitCode {
    let app = Router::new().get("/", root);

    Program::new("my_first_app")
        .about("This is my first app")
        .command(ServerCommand::new(app))
        .run()
}

// Basic handler that responds with a static string
async fn root() -> Text {
    "Ok".to_text()
}
