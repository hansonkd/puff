//! Convert a `Router` into a `RunnableCommand`
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use crate::program::commands::HttpServerConfig;
use axum::ServiceExt;

use std::process::ExitCode;
use std::sync::Arc;

/// The ServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
#[derive(Clone)]
pub struct ServerCommand<F: Fn() -> Router + 'static>(Arc<F>);

impl<F: Fn() -> Router + 'static> ServerCommand<F> {
    pub fn new(s: F) -> Self {
        Self(Arc::new(s))
    }
}

impl<F: Fn() -> Router + 'static> RunnableCommand for ServerCommand<F> {
    fn cli_parser(&self) -> Command {
        HttpServerConfig::add_command_options(Command::new("runserver"))
    }

    fn runnable_from_args(&self, args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let config = HttpServerConfig::new_from_args(args);
        let router_fn = self.0.clone();
        let fut = async move {
            let app = router_fn().into_axum_router(context);
            let server = config.server_builder().serve(app.into_make_service());
            server.await?;
            Ok(ExitCode::SUCCESS)
        };
        Ok(Runnable::new(fut))
    }
}
