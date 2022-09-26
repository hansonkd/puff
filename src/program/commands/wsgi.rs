//! Convert a `Router` into a `RunnableCommand`
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::runtime::dispatcher::RuntimeDispatcher;
use crate::web::server::Router;
use crate::python::wsgi::{AsyncFn, create_server_context};
use clap::{ArgMatches, Command};

use std::net::SocketAddr;
use axum::handler::HandlerWithoutStateExt;
use futures_util::future::{BoxFuture, LocalBoxFuture};
use futures_util::{FutureExt, TryFutureExt};
use pyo3::{IntoPy, Py, PyAny, PyErr, PyObject, Python};
use pyo3::exceptions::PyRuntimeError;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Receiver;
use tracing::{error, info};
use crate::python::bootstrap_puff_globals;
use crate::python::greenlet::GreenletDispatcher;
use crate::python::wsgi::handler::WsgiHandler;
use crate::types::Text;


struct WSGIConstructor {
    addr: SocketAddr,
    router: Router,
    dispatcher: RuntimeDispatcher
}

impl AsyncFn for WSGIConstructor {
    fn call(self, handler: WsgiHandler, rx: Receiver<()>) -> LocalBoxFuture<'static, ()> {
        start(self.addr, self.router, self.dispatcher, rx, handler).boxed_local()
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
    global: Option<R>
}

impl<G: IntoPy<Py<PyAny>> + Clone + 'static> WSGIServerCommand<G> {
    pub fn new<M: Into<Text>, A: Into<Text>>(router: Router, module: M, attr: A) -> Self {
        Self {
            router,
            module: module.into(),
            attr: attr.into(),
            global: None
        }
    }
    pub fn with_global<M: Into<Text>, A: Into<Text>>(router: Router, module: M, attr: A, global: G) -> Self {
        Self {
            router,
            module: module.into(),
            attr: attr.into(),
            global: Some(global)
        }
    }
}

impl<R: IntoPy<Py<PyAny>> + Clone + 'static> RunnableCommand for WSGIServerCommand<R> {
    fn cli_parser(&self) -> Command {
        Command::new("wsgi")
    }

    fn runnable_from_args(
        &self,
        _args: &ArgMatches,
        dispatcher: RuntimeDispatcher,
    ) -> Result<Runnable> {
        let this_self = self.clone();
        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
        bootstrap_puff_globals();

        info!("Creating to start server on {:?}", addr);
        let module_str = self.module.clone().into_string();
        let attr_str = self.attr.clone().into_string();
        let g_obj = this_self.global.clone();
        let fut = async move {
            let greenlet = Python::with_gil(|py| {
                        let global_obj =
                match g_obj {
                    Some(v) => v.into_py(py),
                    None => py.None()
                };
                GreenletDispatcher::new(dispatcher.clone(), global_obj)

            })?;
            let wsgi_app: PyObject = dispatcher.dispatch(move || Python::with_gil(|py| {
                Result::Ok(PyObject::from(py.import(module_str.as_str())?.getattr(attr_str)?))
            })).await?;
            info!("Preparing to start server on {:?}", addr);
            let mut ctx = create_server_context(
                wsgi_app,
                WSGIConstructor{addr, dispatcher: dispatcher.clone(), router: this_self.router.clone()},
                dispatcher,
                Some(greenlet)
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

async fn start(addr: SocketAddr, router: Router, dispatcher: RuntimeDispatcher, shutdown_signal: oneshot::Receiver<()>, wsgi: WsgiHandler) {
    let app = router.into_axum_router(dispatcher).fallback(wsgi);
    info!("Starting server on {:?}", addr);
    if let Err(err) = axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
    {
        eprintln!("error running server: {err}");
    };
}

