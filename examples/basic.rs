use puff::program::Program;
use puff::errors::Result;
use puff::program::commands::BasicCommand;
use puff::tasks::{join_all, Task};
use puff::web::client::{Client, PuffClientResponse, PuffRequestBuilder};
use puff::types::{Puff, Text};
use puff::tracing::info;

fn main() {
    Program::new("my_first_app")
        .about("This is my first app")
        .command(BasicCommand::new("my_command", my_command))
        .run()
}

// Basic handler that responds with a static string
fn my_command() -> Result<()> {
    let client = Client::new();
    let client2 = client.puff();
    let task1 = Task::spawn(move || {
        background_task_1(client2)
    });

    let task2 = Task::spawn(|| {
        background_task_1(client)
    });

    let texts = join_all(vec![task1, task2]);
    for text in texts {
        info!("Received {}", text?.len());
    }
    Ok(())
}

fn background_task_1(client: Client) -> Result<Text> {
    client.get("http://www.google.com/").puff_response()?.puff_text()
}

fn background_task_2(client: Client) -> Result<Text> {
    client.get("http://www.github.com/").puff_response()?.puff_text()
}