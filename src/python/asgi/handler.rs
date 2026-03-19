use crate::context::with_puff_context;
use crate::python::{get_cached_string, PythonDispatcher};
use axum::body::{Body, Bytes as AxumBytes};
use axum::handler::Handler;
use axum::http::{HeaderValue, Request, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use http::header::HeaderName;
use http_body_util::BodyExt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyInt, PyList, PyString, PyTuple};
use pyo3::DowncastError;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;

pub struct AsgiHandler {
    app: PyObject,
    dispatcher: PythonDispatcher,
    asgi_spec: Py<PyDict>,
    handle: Handle,
}

impl Clone for AsgiHandler {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            app: self.app.clone_ref(py),
            dispatcher: self.dispatcher.clone(),
            asgi_spec: self.asgi_spec.clone_ref(py),
            handle: self.handle.clone(),
        })
    }
}

impl AsgiHandler {
    pub fn new(app: PyObject, dispatcher: PythonDispatcher) -> AsgiHandler {
        let asgi_spec = Python::with_gil(|py| {
            let asgi = PyDict::new(py);
            asgi.set_item(
                get_cached_string(py, "spec_version"),
                get_cached_string(py, "2.0"),
            )
            .unwrap();
            asgi.set_item(
                get_cached_string(py, "version"),
                get_cached_string(py, "2.0"),
            )
            .unwrap();
            asgi.unbind()
        });
        let handle = with_puff_context(|ctx| ctx.handle());
        AsgiHandler {
            app,
            dispatcher,
            asgi_spec,
            handle,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
enum AsgiError {
    PyErr(PyErr),
    InvalidHttpVersion,
    ExpectedResponseStart,
    MissingResponse,
    ExpectedResponseBody,
    FailedToCreateResponse,
    InvalidHeader,
    InvalidUtf8InPath,
}

impl From<PyErr> for AsgiError {
    fn from(e: PyErr) -> Self {
        AsgiError::PyErr(e)
    }
}

impl<'a, 'py> From<DowncastError<'a, 'py>> for AsgiError {
    fn from(e: DowncastError<'a, 'py>) -> Self {
        AsgiError::PyErr(e.into())
    }
}

impl IntoResponse for AsgiError {
    fn into_response(self) -> Response {
        match self {
            AsgiError::InvalidHttpVersion => (StatusCode::BAD_REQUEST, "Unsupported HTTP version"),
            AsgiError::InvalidUtf8InPath => (StatusCode::BAD_REQUEST, "Invalid Utf8 in path"),
            AsgiError::PyErr(_)
            | AsgiError::ExpectedResponseStart
            | AsgiError::MissingResponse
            | AsgiError::ExpectedResponseBody
            | AsgiError::FailedToCreateResponse
            | AsgiError::InvalidHeader => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
        }
        .into_response()
    }
}

/// Used to set the HttpReceiver's disconnected flag when the connection is closed
struct SetTrueOnDrop(Arc<AtomicBool>);

impl Drop for SetTrueOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Synchronous receive/send for thread-per-request ASGI
// ---------------------------------------------------------------------------

/// SyncReceiver: Python calls `await receive()`. Under the hood this is a
/// synchronous blocking read from a Tokio channel, wrapped in a Python
/// coroutine via asyncio.
#[pyclass]
struct SyncReceiver {
    rx: Arc<Mutex<UnboundedReceiver<AxumBytes>>>,
    disconnected: Arc<AtomicBool>,
}

#[pymethods]
impl SyncReceiver {
    /// Called as `await receive()` from the ASGI app.
    /// Returns a Python dict: {"type": "http.request", "body": b"..."}
    fn __call__(&self) -> PyResult<PyObject> {
        let rx = self.rx.clone();
        let disconnected = self.disconnected.clone();

        // Spawn the async recv on Tokio, block this thread on oneshot.
        // This avoids calling handle.block_on() inside the Tokio runtime,
        // which would panic with "Cannot start a runtime from within a runtime".
        let (tx, orx) = tokio::sync::oneshot::channel();
        let handle = with_puff_context(|ctx| ctx.handle());
        handle.spawn(async move {
            let next = rx.lock().await.recv().await;
            let _ = tx.send((next, disconnected.load(Ordering::SeqCst)));
        });

        let result: (Option<AxumBytes>, bool) = Python::with_gil(|py| {
            py.allow_threads(|| {
                orx.blocking_recv().map_err(|_|
                    pyo3::exceptions::PyRuntimeError::new_err("Receive cancelled"))
            })
        })?;

        Python::with_gil(|py| {
            let (next, is_disconnected) = result;
            let scope = PyDict::new(py);
            if is_disconnected || next.is_none() {
                scope.set_item(
                    get_cached_string(py, "type"),
                    get_cached_string(py, "http.disconnect"),
                )?;
            } else if let Some(bytes) = next {
                scope.set_item(
                    get_cached_string(py, "type"),
                    get_cached_string(py, "http.request"),
                )?;
                scope.set_item(get_cached_string(py, "body"), PyBytes::new(py, &bytes[..]))?;
            }
            Ok(scope.unbind().into_any())
        })
    }
}

/// SyncSender: Python calls `await send(msg)`. This directly processes the
/// message and sends it through a Rust channel — no async needed.
#[pyclass]
struct SyncSender {
    tx: mpsc::UnboundedSender<Py<PyDict>>,
}

#[pymethods]
impl SyncSender {
    fn __call__<'a>(&'a mut self, py: Python<'a>, args: Py<PyDict>) -> PyResult<PyObject> {
        match self.tx.send(args) {
            Ok(_) => Ok(py.None()),
            Err(_) => Err(PyErr::new::<PyRuntimeError, _>("connection closed")),
        }
    }
}

impl<S> Handler<AsgiHandler, S> for AsgiHandler {
    type Future = Pin<Box<dyn Future<Output = Response> + Send>>;

    fn call(self, req: Request<Body>, _state: S) -> Self::Future {
        let (http_sender, mut http_sender_rx) = {
            let (tx, rx) = mpsc::unbounded_channel::<Py<PyDict>>();
            (SyncSender { tx }, rx)
        };
        let disconnected = Arc::new(AtomicBool::new(false));
        let (receiver_tx, receiver_rx) = mpsc::unbounded_channel();
        let receiver = SyncReceiver {
            rx: Arc::new(Mutex::new(receiver_rx)),
            disconnected: disconnected.clone(),
        };
        let (req, body): (_, Body) = req.into_parts();
        let handle = self.handle.clone();

        // Stream the request body into the receiver channel
        handle.spawn(async move {
            let mut body = body;
            while let Some(frame_result) = body.frame().await {
                if let Ok(frame) = frame_result {
                    if let Ok(data) = frame.into_data() {
                        let _ = receiver_tx.send(data);
                    }
                }
            }
        });

        Box::pin(async move {
            let _disconnected = SetTrueOnDrop(disconnected);

            // Build the ASGI scope and call the app on a blocking thread.
            // Using spawn_blocking avoids blocking Tokio's async worker pool —
            // Python's asyncio.run() drives a per-request event loop on the
            // blocking thread pool instead.
            let call_result = tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    let (app, asgi) = (self.app.clone_ref(py), self.asgi_spec.clone_ref(py));

                    let scope = PyDict::new(py);
                    scope.set_item(get_cached_string(py, "type"), get_cached_string(py, "http"))?;
                    scope.set_item(get_cached_string(py, "asgi"), asgi)?;
                    scope.set_item(
                        get_cached_string(py, "http_version"),
                        match req.version {
                            Version::HTTP_10 => get_cached_string(py, "1.0"),
                            Version::HTTP_11 => get_cached_string(py, "1.1"),
                            Version::HTTP_2 => get_cached_string(py, "2"),
                            _ => return Err(AsgiError::InvalidHttpVersion),
                        },
                    )?;
                    scope.set_item(get_cached_string(py, "method"), req.method.as_str())?;
                    scope.set_item(
                        get_cached_string(py, "scheme"),
                        req.uri.scheme_str().unwrap_or("http"),
                    )?;
                    if let Some(path_and_query) = req.uri.path_and_query() {
                        let path = path_and_query.path();
                        let raw_path = path.as_bytes();
                        let path = percent_encoding::percent_decode(raw_path)
                            .decode_utf8()
                            .map_err(|_| AsgiError::InvalidUtf8InPath)?;
                        scope.set_item(get_cached_string(py, "path"), path)?;
                        let raw_path_bytes = PyBytes::new(py, path_and_query.path().as_bytes());
                        scope.set_item(get_cached_string(py, "raw_path"), raw_path_bytes)?;
                        if let Some(query) = path_and_query.query() {
                            let qs_bytes = PyBytes::new(py, query.as_bytes());
                            scope.set_item(get_cached_string(py, "query_string"), qs_bytes)?;
                        } else {
                            let qs_bytes = PyBytes::new(py, "".as_bytes());
                            scope.set_item(get_cached_string(py, "query_string"), qs_bytes)?;
                        }
                    } else {
                        scope.set_item("path", "")?;
                        let raw_path_bytes = PyBytes::new(py, "".as_bytes());
                        scope.set_item("raw_path", raw_path_bytes)?;
                        let qs_bytes = PyBytes::new(py, "".as_bytes());
                        scope.set_item("query_string", qs_bytes)?;
                    }
                    scope.set_item(
                        get_cached_string(py, "root_path"),
                        get_cached_string(py, ""),
                    )?;

                    let headers_vec: Vec<Bound<'_, PyList>> = req
                        .headers
                        .iter()
                        .map(|(name, value)| {
                            let name_bytes = PyBytes::new(py, name.as_str().as_bytes());
                            let value_bytes = PyBytes::new(py, value.as_bytes());
                            let pair: [Bound<'_, PyAny>; 2] =
                                [name_bytes.into_any(), value_bytes.into_any()];
                            PyList::new(py, pair).unwrap()
                        })
                        .collect::<Vec<_>>();
                    let headers = PyList::new(py, headers_vec)?;
                    scope.set_item(get_cached_string(py, "headers"), headers)?;

                    let sender = Py::new(py, http_sender)?;
                    let receiver = Py::new(py, receiver)?;

                    // Call the ASGI app — returns a coroutine
                    let coro = app.call1(py, (scope, receiver, sender))?;

                    // Run the coroutine using asyncio.run() on THIS thread.
                    // Each request gets its own thread (from Tokio's blocking pool)
                    // and drives its own event loop.
                    let asyncio = py.import("asyncio")?;
                    let result = asyncio.call_method1("run", (coro,));
                    match result {
                        Ok(_) => Ok(()),
                        Err(e) => {
                            tracing::error!("ASGI app error: {e}");
                            Err(AsgiError::PyErr(e))
                        }
                    }
                })
            }).await;

            // Flatten: JoinError (panic/cancel) -> AsgiError, or inner error
            let call_result: Result<(), AsgiError> = match call_result {
                Ok(inner) => inner,
                Err(e) => Err(AsgiError::PyErr(
                    pyo3::exceptions::PyRuntimeError::new_err(format!("ASGI task panicked: {}", e))
                )),
            };

            if let Err(e) = call_result {
                return e.into_response();
            }

            // Now read the response from the sender channel
            let response = if let Some(resp) = http_sender_rx.recv().await {
                let res = Python::with_gil(|py| {
                    let dict = resp.into_bound(py);
                    if let Some(value) = dict.get_item("type")? {
                        let value: Bound<'_, PyString> = value
                            .downcast()
                            .map_err(|_e| anyhow::anyhow!("Invalid asgi type value"))?
                            .clone();
                        let value = value.to_str()?;
                        if value == "http.response.start" {
                            let status_item = dict
                                .get_item(get_cached_string(py, "status"))?
                                .ok_or_else(|| {
                                    PyErr::new::<PyRuntimeError, _>(
                                        "Missing status in http.response.start",
                                    )
                                })?;
                            let status_val: Bound<'_, PyInt> = status_item
                                .downcast()
                                .map_err(|_e| anyhow::anyhow!("Invalid asgi status value"))?
                                .clone();
                            let status: u16 = status_val.extract()?;
                            let mut this_response = Response::builder();
                            this_response = this_response.status(status);
                            let headers = this_response.headers_mut().unwrap();

                            if let Some(raw) = dict.get_item(get_cached_string(py, "headers"))? {
                                for item in raw.iter()? {
                                    let item = item?;
                                    let (key_obj, val_obj) =
                                        if let Ok(t) = item.downcast::<PyTuple>() {
                                            (t.get_item(0)?, t.get_item(1)?)
                                        } else if let Ok(l) = item.downcast::<PyList>() {
                                            (l.get_item(0)?, l.get_item(1)?)
                                        } else {
                                            continue;
                                        };
                                    let k: &Bound<'_, PyBytes> = key_obj
                                        .downcast::<PyBytes>()
                                        .map_err(|_e| anyhow::anyhow!("Invalid asgi header key"))?;
                                    let v: &Bound<'_, PyBytes> =
                                        val_obj.downcast::<PyBytes>().map_err(|_e| {
                                            anyhow::anyhow!("Invalid asgi header value")
                                        })?;
                                    headers.insert(
                                        HeaderName::from_bytes(k.as_bytes())?,
                                        HeaderValue::from_bytes(v.as_bytes())?,
                                    );
                                }
                            };

                            crate::errors::PuffResult::Ok(Ok(this_response))
                        } else {
                            Ok(Err(AsgiError::ExpectedResponseStart))
                        }
                    } else {
                        Ok(Err(AsgiError::ExpectedResponseStart))
                    }
                });

                match res {
                    Ok(Ok(new_response)) => new_response,
                    Ok(Err(e)) => return e.into_response(),
                    Err(_e) => {
                        tracing::error!("Failed to create response: {:?}", _e);
                        return AsgiError::InvalidHeader.into_response();
                    }
                }
            } else {
                return AsgiError::MissingResponse.into_response();
            };

            // Stream response body
            let (sender, body_rec) = mpsc::channel(2);

            handle.spawn(async move {
                while let Some(resp) = http_sender_rx.recv().await {
                    let more_body = match Python::with_gil(|py| {
                        let dict = resp.into_bound(py);
                        if let Some(value) = dict.get_item(get_cached_string(py, "type"))? {
                            let value: Bound<'_, PyString> = value.downcast()?.clone();
                            let value = value.to_str()?;
                            if value == "http.response.body" {
                                let more_body = if let Some(raw) =
                                    dict.get_item(get_cached_string(py, "more_body"))?
                                {
                                    raw.extract::<bool>()?
                                } else {
                                    false
                                };
                                if let Some(raw) = dict.get_item(get_cached_string(py, "body"))? {
                                    if let Ok(s) = raw.downcast::<PyBytes>() {
                                        Ok((AxumBytes::copy_from_slice(s.as_bytes()), more_body))
                                    } else {
                                        Err(AsgiError::ExpectedResponseBody)
                                    }
                                } else {
                                    Err(AsgiError::ExpectedResponseBody)
                                }
                            } else {
                                Err(AsgiError::ExpectedResponseBody)
                            }
                        } else {
                            Err(AsgiError::ExpectedResponseBody)
                        }
                    }) {
                        Ok((body_piece, more_body)) => {
                            sender
                                .send(Result::<_, Infallible>::Ok(body_piece))
                                .await
                                .unwrap();
                            more_body
                        }
                        Err(_e) => {
                            tracing::error!("Failed to create response: {:?}", _e);
                            return ;
                        }
                    };
                    if !more_body {
                        break;
                    }
                }
            });

            let body = Body::from_stream(ReceiverStream::new(body_rec));
            match response.body(body) {
                Ok(response) => response.into_response(),
                Err(_e) => {
                    tracing::error!("Failed to create response: {_e}");
                    AsgiError::FailedToCreateResponse.into_response()
                }
            }
        })
    }
}
