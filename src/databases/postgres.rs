use crate::errors::PuffResult;
use crate::runtime::yield_to_future_io;
use bb8_postgres::PostgresConnectionManager;
use bb8_redis::bb8::Pool;
use bb8_redis::redis::{FromRedisValue, IntoConnectionInfo};
use bb8_redis::RedisConnectionManager;
use std::time::Duration;

use crate::context::with_puff_context;
use crate::types::{Puff, Text};
pub use bb8_redis::redis::Cmd;
use clap::{Arg, Command};
use tokio_postgres::tls::NoTls;
use tokio_postgres::Config;
use tracing::info;

#[derive(Clone)]
pub struct PostgresClient {
    client: Pool<PostgresConnectionManager<NoTls>>,
}

impl Puff for PostgresClient {}

impl PostgresClient {
    pub fn pool(&self) -> Pool<PostgresConnectionManager<NoTls>> {
        return self.client.clone();
    }
}

pub async fn new_postgres_async<T: Into<Text>>(
    config: T,
    check: bool,
) -> PuffResult<PostgresClient> {
    let manager = PostgresConnectionManager::new_from_stringlike(config.into(), NoTls)?;
    let pool = Pool::builder().build(manager).await?;
    let local_pool = pool.clone();
    if check {
        info!("Checking Postgres connectivity...");
        let check_fut = async {
            let mut conn = local_pool.get().await?;
            let my_conn = &mut *conn;
            let _r = my_conn.query_one("SELECT 1", &[]).await?;
            PuffResult::Ok(())
        };

        tokio::time::timeout(Duration::from_secs(5), check_fut).await??;
        info!("Postgres looks good.");
    }

    Ok(PostgresClient { client: pool })
}

pub fn with_postgres<F: FnOnce(PostgresClient) -> T, T>(f: F) -> T {
    with_puff_context(move |d| f(d.postgres()))
}

pub(crate) fn add_postgres_command_arguments(command: Command) -> Command {
    command.arg(
        Arg::new("postgres_url")
            .long("postgres-url")
            .num_args(1)
            .value_name("POSTGRES_URL")
            .env("PUFF_POSTGRES_URL")
            .default_value("postgres://postgres:password@localhost:5432/postgres")
            .help("Global Postgres pool configuration."),
    )
}