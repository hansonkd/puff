pub mod handler;

use anyhow::{anyhow, Result};

use std::future::Future;

use std::sync::Arc;


use crate::python::asgi::handler::AsgiHandler;


use futures_util::future::LocalBoxFuture;
use futures_util::FutureExt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use tokio::sync::{mpsc, oneshot, Mutex};

#[pyclass]
struct Receiver {
    rx: Arc<Mutex<mpsc::UnboundedReceiver<Py<PyDict>>>>,
}

impl Receiver {
    pub fn new() -> (Receiver, mpsc::UnboundedSender<Py<PyDict>>) {
        let (tx, rx) = mpsc::unbounded_channel::<Py<PyDict>>();
        (
            Receiver {
                rx: Arc::new(Mutex::new(rx)),
            },
            tx,
        )
    }
}

#[pymethods]
impl Receiver {
    fn __call__<'a>(&'a mut self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let rx = self.rx.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let next = rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| PyErr::new::<PyRuntimeError, _>("connection closed"))?;
            Ok(next)
        })
    }
}

#[pyclass]
pub struct Sender {
    tx: mpsc::UnboundedSender<Py<PyDict>>,
    locals: pyo3_asyncio::TaskLocals,
}

impl Sender {
    pub fn new(locals: pyo3_asyncio::TaskLocals) -> (Sender, mpsc::UnboundedReceiver<Py<PyDict>>) {
        let (tx, rx) = mpsc::unbounded_channel::<Py<PyDict>>();
        (Sender { tx, locals }, rx)
    }
}

#[pymethods]
impl Sender {
    fn __call__<'a>(&'a mut self, py: Python<'a>, args: Py<PyDict>) -> PyResult<&'a PyAny> {
        let fut = self.locals.event_loop(py).call_method0("create_future")?;
        match self.tx.send(args) {
            Ok(_) => fut.call_method1("set_result", (py.None(),))?,
            Err(_) => fut.call_method1(
                "set_exception",
                (PyErr::new::<PyRuntimeError, _>("connection closed"),),
            )?,
        };
        Ok(fut)
    }
}

pub trait AsyncFn {
    fn call(self, handler: AsgiHandler, rx: oneshot::Receiver<()>) -> LocalBoxFuture<'static, ()>;
}

impl<T, F> AsyncFn for T
where
    T: FnOnce(AsgiHandler, oneshot::Receiver<()>) -> F,
    F: Future<Output = ()> + 'static,
{
    fn call(self, handler: AsgiHandler, rx: oneshot::Receiver<()>) -> LocalBoxFuture<'static, ()> {
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
                let locals =
                    Python::with_gil(|py| pyo3_asyncio::TaskLocals::with_running_loop(py))?;
                let (lifespan_receiver, lifespan_receiver_tx) = Receiver::new();
                let (lifespan_sender, mut lifespan_sender_rx) = Sender::new(locals.clone());
                //let (ready_tx, ready_rx) = oneshot::channel::<()>();

                let fut = async move {
                    // https://asgi.readthedocs.io/en/latest/specs/lifespan.html
                    let lifespan = Python::with_gil(|py| {
                        let asgi = PyDict::new(py);
                        asgi.set_item("spec_version", "2.0")?;
                        asgi.set_item("version", "2.0")?;
                        let scope = PyDict::new(py);
                        scope.set_item("type", "lifespan")?;
                        scope.set_item("asgi", asgi)?;

                        let sender = Py::new(py, lifespan_sender)?;
                        let receiver = Py::new(py, lifespan_receiver)?;
                        let args = (scope, receiver, sender);
                        let res = app.call_method1(py, "__call__", args)?;
                        let fut = res.extract(py)?;
                        pyo3_asyncio::into_future_with_locals(&locals, fut)
                    })?;

                    let lifespan_startup = Python::with_gil(|py| {
                        let scope = PyDict::new(py);
                        scope.set_item("type", "lifespan.startup")?;
                        let scope: Py<PyDict> = scope.into();
                        Ok::<Py<PyDict>, PyErr>(scope)
                    })?;
                    if lifespan_receiver_tx.send(lifespan_startup).is_err() {
                        return Err(anyhow!("Failed to send lifespan startup",));
                    }

                    // will continue running until the server sends lifespan.shutdown
                    tokio::spawn(async move {
                        if let Err(_e) = lifespan.await {
                            tracing::error!("Error processing lifespan: {_e}");
                        }
                    });

                    if let Some(resp) = lifespan_sender_rx.recv().await {
                        Python::with_gil(|py| {
                            let dict: &PyDict = resp.into_ref(py);
                            if let Some(value) = dict.get_item("type") {
                                let value: &PyString = value.downcast().unwrap();
                                let value = value.to_str()?;
                                if value == "lifespan.startup.complete" {
                                    return Ok(());
                                }
                            }
                            Err(anyhow!("Failed during asgi startup",))
                        })?;
                    }

                    // create asgi service
                    let asgi_handler = AsgiHandler::new_with_locals(app.clone(), locals.clone());

                    server.call(asgi_handler, rx).await;

                    // shutdown
                    let lifespan_shutdown = Python::with_gil(|py| {
                        let scope = PyDict::new(py);
                        scope.set_item("type", "lifespan.shutdown")?;
                        let scope: Py<PyDict> = scope.into();
                        Ok::<Py<PyDict>, PyErr>(scope)
                    })?;
                    if lifespan_receiver_tx.send(lifespan_shutdown).is_err() {
                        return Err(anyhow!("Failed to send lifespan shutdown",));
                    }

                    // receive the shutdown success, event though we don't care about it. without this the sender_rx gets dropped too early and the shutdown fails.
                    lifespan_sender_rx.recv().await;

                    if let Err(_e) = tx.send(()) {
                        tracing::error!("Failed to send shutdown completion");
                    }

                    Ok(())
                };

                Ok(fut.boxed_local())
            }
            (_, _, _, _) => Err(anyhow!("Already Started!")),
        }
    }
}

pub fn create_server_context<T: AsyncFn>(app: PyObject, server: T) -> ServerContext<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (wait_shutdown_tx, wait_shutdown_rx) = tokio::sync::oneshot::channel();
    ServerContext {
        trigger_shutdown_tx: Some(tx),
        trigger_shutdown_rx: Some(rx),
        wait_shutdown_tx: Some(wait_shutdown_tx),
        wait_shutdown_rx: Some(wait_shutdown_rx),
        app: Some(app),
        server: Some(server),
    }
}
