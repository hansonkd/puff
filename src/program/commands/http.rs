//! Convert a `Router` into a `RunnableCommand`
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use crate::program::commands::HttpServerConfig;
use std::process::ExitCode;

/// The ServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
pub struct ServerCommand(Option<Box<dyn FnOnce() -> Router + 'static>>);

impl ServerCommand {
    pub fn new(r: Router) -> Self {
        Self(Some(Box::new(|| r)))
    }
    pub fn new_with_router_init<F: FnOnce() -> Router + 'static>(f: F) -> Self {
        Self(Some(Box::new(f)))
    }
}

impl RunnableCommand for ServerCommand {
    fn cli_parser(&self) -> Command {
        HttpServerConfig::add_command_options(Command::new("runserver"))
    }

    fn make_runnable(&mut self, args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let config = HttpServerConfig::new_from_args(args);
        let router_fn = self.0.take().expect("Already ran.");
        let fut = async move {
            let app = router_fn().into_axum_router(context);
            let server = config.server_builder().serve(app.into_make_service());
            server.await?;
            Ok(ExitCode::SUCCESS)
        };
        Ok(Runnable::new(fut))
    }
}
