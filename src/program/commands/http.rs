//! Convert a `Router` into a `RunnableCommand`
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::runtime::dispatcher::RuntimeDispatcher;
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use std::net::SocketAddr;

/// The ServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
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
        _args: &ArgMatches,
        dispatcher: RuntimeDispatcher,
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
