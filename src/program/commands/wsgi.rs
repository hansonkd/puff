//! Convert a `Router` into a `RunnableCommand` and run Python WSGI as a fallback.
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::python::wsgi::{create_server_context, WsgiServerSpawner};
use crate::web::server::Router;
use clap::{ArgMatches, Command};
use std::process::ExitCode;
use std::sync::Arc;

use crate::python::wsgi::handler::WsgiHandler;
use crate::types::Text;
use futures_util::future::LocalBoxFuture;
use futures_util::FutureExt;

use pyo3::prelude::*;

use crate::program::commands::HttpServerConfig;
use crate::types::text::ToText;

struct WSGIConstructor {
    config: HttpServerConfig,
    router: Router,
    puff_context: PuffContext,
}

impl WsgiServerSpawner for WSGIConstructor {
    fn call(self, handler: WsgiHandler) -> LocalBoxFuture<'static, ()> {
        start(self.config, self.router, self.puff_context, handler).boxed_local()
    }
}

/// The WSGIServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
pub struct WSGIServerCommand {
    router_fn: Option<Box<dyn FnOnce() -> Router + 'static>>,
    app_path: Text,
}

impl WSGIServerCommand {
    pub fn new<M: Into<Text>>(app_path: M) -> Self {
        Self {
            router_fn: Some(Box::new(|| Router::new())),
            app_path: app_path.into(),
        }
    }
    pub fn new_with_router<M: Into<Text>>(app_path: M, r: Router) -> Self {
        Self {
            router_fn: Some(Box::new(|| r)),
            app_path: app_path.into(),
        }
    }
    pub fn new_with_router_init<M: Into<Text>, F: FnOnce() -> Router + 'static>(
        app_path: M,
        f: F,
    ) -> Self {
        Self {
            router_fn: Some(Box::new(f)),
            app_path: app_path.into(),
        }
    }
}

impl RunnableCommand for WSGIServerCommand {
    fn cli_parser(&self) -> Command {
        HttpServerConfig::add_command_options(Command::new("runserver"))
    }

    fn make_runnable(&mut self, args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let wsgi_app = Python::with_gil(|py| {
            let puff_mod = py.import("puff")?;
            PyResult::Ok(
                puff_mod
                    .call_method1("import_string", (self.app_path.clone().into_py(py),))?
                    .into_py(py),
            )
        })?;

        let config = HttpServerConfig::new_from_args(args);
        let router_fn = self.router_fn.take().expect("Already ran.");
        let fut = async move {
            let server_name = config.socket_addr.ip().to_text();
            let server_port = config.socket_addr.port();
            let router = router_fn();
            let mut ctx = create_server_context(
                wsgi_app,
                WSGIConstructor {
                    config,
                    router,
                    puff_context: context.clone(),
                },
                context.clone(),
                server_name,
                server_port,
            );
            ctx.start()?.await?;

            Ok(ExitCode::SUCCESS)
        };
        Ok(Runnable::new(fut))
    }
}

async fn start(
    http_configuration: HttpServerConfig,
    router: Router,
    puff_context: PuffContext,
    wsgi: WsgiHandler,
) {
    let app = router.into_axum_router(puff_context).fallback(wsgi);
    if let Err(err) = http_configuration
        .server_builder()
        .serve(app.into_make_service())
        .await
    {
        eprintln!("error running server: {err}");
    };
}
