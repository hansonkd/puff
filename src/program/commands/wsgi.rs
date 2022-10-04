//! Convert a `Router` into a `RunnableCommand`
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::python::wsgi::{create_server_context, WsgiServerSpawner};
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use crate::python::wsgi::handler::WsgiHandler;
use crate::types::Text;
use futures_util::future::LocalBoxFuture;
use futures_util::FutureExt;

use pyo3::prelude::*;

use crate::program::commands::HttpServerConfig;
use crate::types::text::ToText;
use tracing::info;

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
#[derive(Clone)]
pub struct WSGIServerCommand {
    router: Router,
    app_path: Text,
}

impl WSGIServerCommand {
    pub fn new<M: Into<Text>>(router: Router, app_path: M) -> Self {
        Self {
            router,
            app_path: app_path.into(),
        }
    }
}

impl RunnableCommand for WSGIServerCommand {
    fn cli_parser(&self) -> Command {
        HttpServerConfig::add_command_options(Command::new("runserver"))
    }

    fn runnable_from_args(&self, args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let this_self = self.clone();

        let wsgi_app = Python::with_gil(|py| {
            let puff_mod = py.import("puff")?;
            PyResult::Ok(
                puff_mod
                    .call_method1("import_string", (self.app_path.clone().into_py(py),))?
                    .into_py(py),
            )
        })?;

        let config = HttpServerConfig::new_from_args(args);

        let fut = async move {
            let server_name = config.socket_addr.ip().to_text();
            let server_port = config.socket_addr.port();
            let mut ctx = create_server_context(
                wsgi_app,
                WSGIConstructor {
                    config,
                    puff_context: context.clone(),
                    router: this_self.router.clone(),
                },
                context.clone(),
                server_name,
                server_port,
            );
            ctx.start()?.await?;
            Ok(())
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
    info!("Starting server on {:?}", http_configuration.socket_addr);
    if let Err(err) = axum::Server::bind(&http_configuration.socket_addr)
        .serve(app.into_make_service())
        .await
    {
        eprintln!("error running server: {err}");
    };
}
