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

use pyo3::{PyObject, Python};
use std::net::SocketAddr;

use tracing::{error, info};

struct WSGIConstructor {
    addr: SocketAddr,
    router: Router,
    puff_context: PuffContext,
}

impl WsgiServerSpawner for WSGIConstructor {
    fn call(self, handler: WsgiHandler) -> LocalBoxFuture<'static, ()> {
        start(self.addr, self.router, self.puff_context, handler).boxed_local()
    }
}

/// The WSGIServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
#[derive(Clone)]
pub struct WSGIServerCommand {
    router: Router,
    module: Text,
    attr: Text,
}

impl WSGIServerCommand {
    pub fn new<M: Into<Text>, A: Into<Text>>(router: Router, module: M, attr: A) -> Self {
        Self {
            router,
            module: module.into(),
            attr: attr.into(),
        }
    }
}

impl RunnableCommand for WSGIServerCommand {
    fn cli_parser(&self) -> Command {
        Command::new("wsgi")
    }

    fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let this_self = self.clone();
        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

        let module_str = self.module.clone().into_string();
        let attr_str = self.attr.clone().into_string();
        let fut = async move {
            let wsgi_app = tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    Result::Ok(PyObject::from(
                        py.import(module_str.as_str())?.getattr(attr_str)?,
                    ))
                })
            })
            .await??;
            info!("Preparing to start server on {:?}", addr);
            let mut ctx = create_server_context(
                wsgi_app,
                WSGIConstructor {
                    addr,
                    puff_context: context.clone(),
                    router: this_self.router.clone(),
                },
                context.clone(),
            );
            let shutdown = tokio::signal::ctrl_c();
            // let result = ctx.start()?.await;
            tokio::select! {
                res = ctx.start()? => {
                    // If an error is received here, accepting connections from the TCP
                    // listener failed multiple times and the server is giving up and
                    // shutting down.
                    //
                    // Errors encountered when handling individual connections do not
                    // bubble up to this point.
                    if let Err(err) = res {
                        error!(cause = %err, "failed to start server");
                    }
                }
                _ = shutdown => {
                    // The shutdown signal has been received.
                    info!("shutting down");
                }
            }

            Ok(())
        };
        Ok(Runnable::new(fut))
    }
}

async fn start(addr: SocketAddr, router: Router, puff_context: PuffContext, wsgi: WsgiHandler) {
    let app = router.into_axum_router(puff_context).fallback(wsgi);
    info!("Starting server on {:?}", addr);
    if let Err(err) = axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
    {
        eprintln!("error running server: {err}");
    };
}
