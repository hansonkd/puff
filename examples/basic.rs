use bb8_redis::redis::AsyncCommands;
use futures_util::future::join_all;
use puff_rs::databases::redis::RedisClient;
use puff_rs::prelude::*;
use puff_rs::program::commands::BasicCommand;

fn main() -> ExitCode {
    let config = RuntimeConfig::default().add_default_redis();
    Program::new("my_first_app")
        .runtime_config(config)
        .about("This is my first app")
        .command(BasicCommand::new("my_command", |_| my_command()))
        .run()
}

// Basic handler that responds with a static string
async fn my_command() -> PuffResult<ExitCode> {
    let redis = with_puff_context(|ctx| ctx.redis());
    let task1 = background_task(redis.puff());
    let task2 = background_task(redis);
    let results: usize = join_all(vec![task1, task2])
        .await
        .iter()
        .map(|v| v.as_ref().unwrap().len())
        .sum();
    println!("Retrieved {} bytes from redis.", results);
    Ok(ExitCode::SUCCESS)
}

async fn background_task(redis: RedisClient) -> PuffResult<Text> {
    let pool = redis.pool();
    let mut conn = pool.get().await?;
    conn.set("hello", "world").await?;
    Ok(conn.get("hello").await?)
}
