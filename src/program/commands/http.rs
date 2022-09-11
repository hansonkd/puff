use crate::errors::Result;
use crate::program::{CommandError, Runnable, RunnableCommand};
use crate::tasks::dispatcher::Dispatcher;
use crate::web::http::Router;
use clap::{ArgMatches, Command};
use futures_util::FutureExt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone)]
pub struct ServerCommand(Router);

impl ServerCommand {
    pub fn new(s: Router) -> Self {
        Self(s)
    }
}

impl RunnableCommand for ServerCommand {
    fn cli_parser(&self) -> Command {
        Command::new("server")
    }

    fn runnable_from_args(
        &self,
        args: &ArgMatches,
        dispatcher: Arc<Dispatcher>,
    ) -> Result<Runnable> {
        let this_self = self.clone();
        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
        let fut = async move {
            Ok(this_self
                .0
                .clone()
                .into_hyper_server(&addr, dispatcher)
                .await?)
        };
        Ok(Runnable::new(fut))
    }
}
