use crate::errors::PuffResult;
use bb8_redis::bb8::Pool;
use bb8_redis::redis::IntoConnectionInfo;
use bb8_redis::RedisConnectionManager;
use std::time::Duration;

use crate::context::with_puff_context;
use crate::types::Puff;
pub use bb8_redis;
use bb8_redis::redis::Cmd;
use clap::{Arg, Command};
use tracing::info;

#[derive(Clone)]
pub struct RedisClient {
    client: Pool<RedisConnectionManager>,
}

impl Puff for RedisClient {}

impl RedisClient {
    pub fn pool(&self) -> Pool<RedisConnectionManager> {
        return self.client.clone();
    }
}

pub async fn new_redis_async<T: IntoConnectionInfo>(
    conn: T,
    check: bool,
    pool_size: u32,
) -> PuffResult<RedisClient> {
    let manager = RedisConnectionManager::new(conn)?;
    let pool = Pool::builder().max_size(pool_size).build(manager).await?;
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

pub fn with_redis<F: FnOnce(RedisClient) -> T, T>(f: F) -> T {
    with_puff_context(move |d| f(d.redis()))
}

pub(crate) fn add_redis_command_arguments(name: &str, command: Command) -> Command {
    let name_lower = name.to_lowercase();
    let name_upper = name.to_uppercase();

    command.arg(
        Arg::new(format!("{}_redis_url", name_lower))
            .long(format!("{}-redis-url", name_lower))
            .num_args(1)
            .value_name(format!("{}_REDIS_URL", name_upper))
            .env(format!("PUFF_{}_REDIS_URL", name_upper))
            .default_value("redis://localhost:6379")
            .help(format!("Redis pool configuration for '{}'.", name)),
    )
}
