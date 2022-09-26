//! Convert a `Router` into a `RunnableCommand`
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::python::wsgi::{create_server_context, WsgiServerSpawner};
use crate::web::server::Router;
use clap::{ArgMatches, Command};

use crate::python::greenlet::GreenletDispatcher;
use crate::python::wsgi::handler::WsgiHandler;
use crate::types::Text;
use futures_util::future::{LocalBoxFuture};
use futures_util::{FutureExt};

use pyo3::{IntoPy, Py, PyAny, PyObject, Python};
use std::net::SocketAddr;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Receiver;
use tracing::{error, info};

struct WSGIConstructor {
    addr: SocketAddr,
    router: Router,
    dispatcher: PuffContext,
}

impl WsgiServerSpawner for WSGIConstructor {
    fn call(self, handler: WsgiHandler) -> LocalBoxFuture<'static, ()> {
        start(self.addr, self.router, self.dispatcher, handler).boxed_local()
    }
}

/// The ServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
#[derive(Clone)]
pub struct WSGIServerCommand<R: IntoPy<Py<PyAny>> + Clone + 'static = ()> {
    router: Router,
    module: Text,
    attr: Text,
    global: Option<R>,
}

impl<G: IntoPy<Py<PyAny>> + Clone + 'static> WSGIServerCommand<G> {
    pub fn new<M: Into<Text>, A: Into<Text>>(router: Router, module: M, attr: A) -> Self {
        Self {
            router,
            module: module.into(),
            attr: attr.into(),
            global: None,
        }
    }
    pub fn with_global<M: Into<Text>, A: Into<Text>>(
        router: Router,
        module: M,
        attr: A,
        global: G,
    ) -> Self {
        Self {
            router,
            module: module.into(),
            attr: attr.into(),
            global: Some(global),
        }
    }
}

impl<R: IntoPy<Py<PyAny>> + Clone + 'static> RunnableCommand for WSGIServerCommand<R> {
    fn cli_parser(&self) -> Command {
        Command::new("wsgi")
    }

    fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let this_self = self.clone();
        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

        let module_str = self.module.clone().into_string();
        let attr_str = self.attr.clone().into_string();
        let g_obj = this_self.global.clone();
        let fut = async move {
            let greenlet = Python::with_gil(|py| {
                let global_obj = match g_obj {
                    Some(v) => v.into_py(py),
                    None => py.None(),
                };
                GreenletDispatcher::new(context.clone(), global_obj)
            })?;
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
                    dispatcher: context.clone(),
                    router: this_self.router.clone(),
                },
                Some(greenlet),
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

async fn start(
    addr: SocketAddr,
    router: Router,
    dispatcher: PuffContext,
    wsgi: WsgiHandler,
) {
    let app = router.into_axum_router(dispatcher).fallback(wsgi);
    info!("Starting server on {:?}", addr);
    if let Err(err) = axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
    {
        eprintln!("error running server: {err}");
    };
}
