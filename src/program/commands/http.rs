//! Convert a `Router` into a `RunnableCommand`
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use std::net::SocketAddr;
use std::process::ExitCode;

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
        Command::new("runserver")
    }

    fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let this_self = self.clone();
        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
        let fut = async move {
            this_self
                .0
                .clone()
                .into_hyper_server(&addr, context)
                .await?;
            Ok(ExitCode::SUCCESS)
        };
        Ok(Runnable::new(fut))
    }
}
