use bb8_redis::bb8::Pool;
use bb8_redis::RedisConnectionManager;
use bb8_redis::redis::{FromRedisValue, IntoConnectionInfo};
use crate::errors::PuffResult;
use crate::runtime::{yield_to_future, yield_to_future_io};

pub use bb8_redis::redis::Cmd;
use clap::{Arg, Command};
use crate::tasks::DISPATCHER;
use crate::types::Puff;


#[derive(Clone)]
pub struct RedisClient {
    client: Pool<RedisConnectionManager>
}

impl Puff for RedisClient {}

impl RedisClient {
    pub fn new<T: IntoConnectionInfo + Send + 'static>(conn: T) -> PuffResult<Self> {
        yield_to_future_io(new_client_async(conn))?
    }

    pub fn query<T: FromRedisValue + Send + 'static>(&self, command: Cmd) -> PuffResult<T> {
        let client = self.client.clone();
        Ok(yield_to_future(async move {
            let mut conn = client.get().await?;
            PuffResult::Ok(command.query_async(&mut *conn).await?)
        })?)
    }
}

pub async fn new_client_async<T: IntoConnectionInfo>(conn: T) -> PuffResult<RedisClient> {
    let manager = RedisConnectionManager::new(conn)?;
    let pool = Pool::builder().build(manager).await?;
    Ok(RedisClient {
        client: pool
    })
}


pub fn query_redis<T: FromRedisValue + Send + 'static>(command: Cmd) -> PuffResult<T> {
    DISPATCHER.with(move |d| d.redis().query(command))
}

pub fn with_redis<F: FnOnce(RedisClient) -> T, T>(f: F) -> T {
    DISPATCHER.with(move |d| f(d.redis()))
}

pub(crate) fn add_redis_command_arguments(command: Command) -> Command {
    command.arg(
        Arg::new("redis_url")
            .long("redis_url")
            .takes_value(true)
            .value_name("REDIS_URL")
            .env("PUFF_REDIS_URL")
            .default_value("redis://localhost:6379")
            .help("Specify the global Redis pool configuration.")
    )
}