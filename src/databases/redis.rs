use crate::errors::PuffResult;
use crate::runtime::yield_to_future_io;
use bb8_redis::bb8::Pool;
use bb8_redis::redis::{FromRedisValue, IntoConnectionInfo};
use bb8_redis::RedisConnectionManager;
use std::time::Duration;

use crate::context::with_puff_context;
use crate::types::Puff;
pub use bb8_redis::redis::Cmd;
use clap::{Arg, Command};
use tracing::info;

#[derive(Clone)]
pub struct RedisClient {
    client: Pool<RedisConnectionManager>,
}

impl Puff for RedisClient {}

impl RedisClient {
    pub fn new<T: IntoConnectionInfo + Send + 'static>(conn: T) -> PuffResult<Self> {
        yield_to_future_io(new_redis_async(conn, false))?
    }

    pub fn query<T: FromRedisValue + Send + 'static>(&self, command: Cmd) -> PuffResult<T> {
        let client = self.client.clone();
        Ok(yield_to_future_io(async move {
            let mut conn = client.get().await?;
            PuffResult::Ok(command.query_async(&mut *conn).await?)
        })??)
    }

    pub fn pool(&self) -> Pool<RedisConnectionManager> {
        return self.client.clone();
    }
}

pub async fn new_redis_async<T: IntoConnectionInfo>(
    conn: T,
    check: bool,
) -> PuffResult<RedisClient> {
    let manager = RedisConnectionManager::new(conn)?;
    let pool = Pool::builder().build(manager).await?;
    let local_pool = pool.clone();
    if check {
        info!("Checking redis connectivity...");
        let check_fut = async {
            let mut conn = local_pool.get().await?;
            PuffResult::Ok(Cmd::new().arg("PING").query_async(&mut *conn).await?)
        };

        tokio::time::timeout(Duration::from_secs(5), check_fut).await??;
        info!("Redis looks good.");
    }

    Ok(RedisClient { client: pool })
}

pub fn query_redis<T: FromRedisValue + Send + 'static>(command: Cmd) -> PuffResult<T> {
    with_puff_context(move |d| d.redis().query(command))
}

pub fn with_redis<F: FnOnce(RedisClient) -> T, T>(f: F) -> T {
    with_puff_context(move |d| f(d.redis()))
}

pub(crate) fn add_redis_command_arguments(command: Command) -> Command {
    command.arg(
        Arg::new("redis_url")
            .long("redis-url")
            .takes_value(true)
            .value_name("REDIS_URL")
            .env("PUFF_REDIS_URL")
            .default_value("redis://localhost:6379")
            .help("Global Redis pool configuration."),
    )
}
