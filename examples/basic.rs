use puff::databases::redis::{Cmd, RedisClient};
use puff::errors::Result;
use puff::program::commands::BasicCommand;
use puff::program::Program;
use puff::runtime::RuntimeConfig;
use puff::tasks::{join_all, Task};
use puff::tracing::info;
use puff::types::{Puff, Text};
use puff::web::client::{Client, PuffClientResponse, PuffRequestBuilder};

fn main() {
    let config = RuntimeConfig::default().set_redis(true);
    Program::new("my_first_app")
        .runtime_config(config)
        .about("This is my first app")
        .command(BasicCommand::new("my_command", my_command))
        .run()
}

// Basic handler that responds with a static string
fn my_command() -> Result<()> {
    let redis = RedisClient::new("redis://localhost")?;

    let client = Client::new();
    let _client2 = client.puff();
    // let task1 = Task::spawn(move || {
    //     background_task_1(client2)
    // });

    let task2 = Task::spawn(|| background_task_2(redis));

    let texts = join_all(vec![task2]);
    for text in texts {
        info!("Received {}", text?.len());
    }
    Ok(())
}

fn background_task_1(client: Client) -> Result<Text> {
    client
        .get("http://www.google.com/")
        .puff_response()?
        .puff_text()
}

fn background_task_2(redis: RedisClient) -> Result<Text> {
    redis.query(Cmd::set("hello", "world"))?;
    redis.query(Cmd::get("hello"))
}
