//! Convert a `Router` into a `RunnableCommand`
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::python::asgi::{create_server_context, AsyncFn};
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use crate::python::asgi::handler::AsgiHandler;
use crate::types::Text;
use futures_util::future::{LocalBoxFuture};
use futures_util::{FutureExt};

use pyo3::{PyObject, Python};
use std::net::SocketAddr;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Receiver;
use tracing::info;

struct ASGIConstructor {
    addr: SocketAddr,
    router: Router,
    dispatcher: PuffContext,
}

impl AsyncFn for ASGIConstructor {
    fn call(self, handler: AsgiHandler, rx: Receiver<()>) -> LocalBoxFuture<'static, ()> {
        start(self.addr, self.router, self.dispatcher, rx, handler).boxed_local()
    }
}

/// The ServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
#[derive(Clone)]
pub struct ASGIServerCommand {
    router: Router,
    module: Text,
    attr: Text,
}

impl ASGIServerCommand {
    pub fn new<M: Into<Text>, A: Into<Text>>(router: Router, module: M, attr: A) -> Self {
        Self {
            router,
            module: module.into(),
            attr: attr.into(),
        }
    }
}

impl RunnableCommand for ASGIServerCommand {
    fn cli_parser(&self) -> Command {
        Command::new("asgi")
    }

    fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let this_self = self.clone();
        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
        let asgi_app: PyObject = Python::with_gil(|py| {
            Result::Ok(
                py.import(self.module.as_str())?
                    .getattr(self.attr.as_str())?
                    .into(),
            )
        })?;
        info!("Creating to start server on {:?}", addr);
        let fut = async move {
            info!("Preparing to start server on {:?}", addr);
            let mut ctx = create_server_context(
                asgi_app,
                ASGIConstructor {
                    addr,
                    dispatcher: context,
                    router: this_self.router,
                },
            );
            let result = ctx.start()?.await;
            result
        };
        Ok(Runnable::new(fut))
    }
}

async fn start(
    addr: SocketAddr,
    router: Router,
    dispatcher: PuffContext,
    shutdown_signal: oneshot::Receiver<()>,
    asgi: AsgiHandler,
) {
    let app = router.into_axum_router(dispatcher).fallback(asgi);
    info!("Starting server on {:?}", addr);
    if let Err(err) = axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(async move {
            if let Err(err) = shutdown_signal.await {
                eprintln!("failed to send shutdown signal: {err}");
            }
        })
        .await
    {
        eprintln!("error running server: {err}");
    };
}
