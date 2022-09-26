pub mod handler;

use anyhow::{anyhow, Result};

use std::future::Future;




use crate::python::greenlet::GreenletDispatcher;
use crate::python::wsgi::handler::WsgiHandler;


use futures_util::future::LocalBoxFuture;
use futures_util::FutureExt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use tokio::sync::{oneshot};

#[pyclass]
pub struct Sender {
    tx: Option<oneshot::Sender<(String, Vec<(String, String)>)>>,
}

impl Sender {
    pub fn new() -> (Sender, oneshot::Receiver<(String, Vec<(String, String)>)>) {
        let (tx, rx) = oneshot::channel();
        (Sender { tx: Some(tx) }, rx)
    }
}

#[pymethods]
impl Sender {
    fn __call__<'a>(
        &'a mut self,
        py: Python<'a>,
        status: String,
        list: Vec<(String, String)>,
    ) -> PyResult<PyObject> {
        match self.tx.take() {
            Some(sender) => match sender.send((status, list)) {
                Ok(_) => Ok(py.None()),
                Err(_) => Err(PyErr::new::<PyRuntimeError, _>("response closed")),
            },
            None => Err(PyErr::new::<PyRuntimeError, _>(
                "already sent start response",
            )),
        }
    }
}

pub trait WsgiServerSpawner {
    fn call(self, handler: WsgiHandler) -> LocalBoxFuture<'static, ()>;
}

impl<T, F> WsgiServerSpawner for T
where
    T: FnOnce(WsgiHandler) -> F,
    F: Future<Output = ()> + 'static,
{
    fn call(self, handler: WsgiHandler) -> LocalBoxFuture<'static, ()> {
        Box::pin(self(handler))
    }
}

pub struct ServerContext<T: WsgiServerSpawner> {
    app: Option<PyObject>,
    server: Option<T>,
    greenlet: Option<GreenletDispatcher>,
}

impl<T: WsgiServerSpawner> ServerContext<T> {
    pub fn start(&mut self) -> Result<LocalBoxFuture<Result<()>>> {
        match (
            self.app.take(),
            self.server.take(),
        ) {
            (Some(app), Some(server)) => {
                let fut = async move {
                    // create wsgi service
                    let wsgi_handler = WsgiHandler::new(
                        app.clone(),
                        self.greenlet.clone(),
                    );

                    server.call(wsgi_handler).await;

                    Ok(())
                };

                Ok(fut.boxed_local())
            }
            _ => Err(anyhow!("Already Started!")),
        }
    }
}

pub fn create_server_context<T: WsgiServerSpawner>(
    app: PyObject,
    server: T,
    greenlet: Option<GreenletDispatcher>,
) -> ServerContext<T> {
    ServerContext {
        app: Some(app),
        server: Some(server),
        greenlet
    }
}
