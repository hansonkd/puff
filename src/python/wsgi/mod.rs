pub mod handler;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use anyhow::anyhow;
use anyhow::Result;
use axum::handler::HandlerWithoutStateExt;

use futures::future::BoxFuture;
use futures_util::future::LocalBoxFuture;
use futures_util::FutureExt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use tokio::sync::{mpsc, oneshot, Mutex};
use crate::python::greenlet::GreenletDispatcher;
use crate::python::wsgi::handler::WsgiHandler;
use crate::runtime::dispatcher::RuntimeDispatcher;
use crate::web::server::Router;


#[pyclass]
pub struct Sender {
    tx: Option<oneshot::Sender<(String, Vec<(String, String)>)>>
}

impl Sender {
    pub fn new() -> (Sender, oneshot::Receiver<(String, Vec<(String, String)>)>) {
        let (tx, rx) = oneshot::channel();
        (Sender { tx: Some(tx) }, rx)
    }
}

#[pymethods]
impl Sender {
    fn __call__<'a>(&'a mut self, py: Python<'a>, status: String, list: Vec<(String, String)>) -> PyResult<PyObject> {
        match self.tx.take() {
            Some(sender) => {
                match sender.send((status, list)) {
                    Ok(_) => Ok(py.None()),
                    Err(_) => Err(PyErr::new::<PyRuntimeError, _>("response closed")),
                }
            }
            None => Err(PyErr::new::<PyRuntimeError, _>("already sent start response"))
        }

    }
}

pub trait AsyncFn {
    fn call(self, handler: WsgiHandler, rx: oneshot::Receiver<()>) -> LocalBoxFuture<'static, ()>;
}

impl<T, F> AsyncFn for T
where
    T: FnOnce(WsgiHandler, oneshot::Receiver<()>) -> F,
    F: Future<Output = ()>  + 'static,
{
    fn call(self, handler: WsgiHandler, rx: oneshot::Receiver<()>) -> LocalBoxFuture<'static, ()> {
        Box::pin(self(handler, rx))
    }
}

pub struct ServerContext<T: AsyncFn> {
    trigger_shutdown_tx: Option<oneshot::Sender<()>>,
    trigger_shutdown_rx: Option<oneshot::Receiver<()>>,
    wait_shutdown_tx: Option<oneshot::Sender<()>>,
    wait_shutdown_rx: Option<oneshot::Receiver<()>>,
    app: Option<PyObject>,
    server: Option<T>,
    dispatcher: RuntimeDispatcher,
    greenlet: Option<GreenletDispatcher>
}


impl<T: AsyncFn> ServerContext<T> {
    fn shutdown<'a>(&'a mut self, py: Python<'a>) -> PyResult<&'a PyAny> {
        if let (Some(tx), Some(rx)) = (
            self.trigger_shutdown_tx.take(),
            self.wait_shutdown_rx.take(),
        ) {
            if let Err(_e) = tx.send(()) {
                tracing::warn!("failed to send shutdown notification: {:?}", _e);
            }
            pyo3_asyncio::tokio::future_into_py(py, async move {
                if let Err(_e) = rx.await {
                    tracing::warn!("failed waiting for shutdown: {:?}", _e);
                }
                Ok::<_, PyErr>(Python::with_gil(|py| py.None()))
            })
        } else {
            pyo3_asyncio::tokio::future_into_py(py, async move {
                Ok::<_, PyErr>(Python::with_gil(|py| py.None()))
            })
        }
    }

    pub fn start(&mut self) -> Result<LocalBoxFuture<Result<()>>> {
        match (
            self.trigger_shutdown_rx.take(),
            self.app.take(),
            self.server.take(),
            self.wait_shutdown_tx.take(),
        ) {
            (Some(rx), Some(app), Some(server), Some(tx)) => {
                let fut = async move {
                    // create wsgi service
                    let wsgi_handler = WsgiHandler::new(app.clone(), self.dispatcher.clone(), self.greenlet.clone());

                    server.call(wsgi_handler, rx).await;
                    
                    Ok(())
                };

                Ok(fut.boxed_local())
            }
            _ => Err(anyhow!("Already Started!")),
        }
    }
}

pub fn create_server_context<T: AsyncFn>(
    app: PyObject,
    server: T,
    dispatcher: RuntimeDispatcher,
    greenlet: Option<GreenletDispatcher>
) -> ServerContext<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (wait_shutdown_tx, wait_shutdown_rx) = tokio::sync::oneshot::channel();
    ServerContext {
        trigger_shutdown_tx: Some(tx),
        trigger_shutdown_rx: Some(rx),
        wait_shutdown_tx: Some(wait_shutdown_tx),
        wait_shutdown_rx: Some(wait_shutdown_rx),
        app: Some(app),
        server: Some(server),
        greenlet,
        dispatcher
    }
}
