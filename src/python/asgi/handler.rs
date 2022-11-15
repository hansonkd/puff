use crate::context::with_puff_context;
use crate::errors::PuffResult;
use crate::prelude::run_python_async;
use crate::python::{asgi, get_cached_string, PythonDispatcher};
use anyhow::anyhow;
use asgi::Sender;
use axum::body::{boxed, Body, BoxBody, Bytes as AxumBytes, HttpBody};
use axum::handler::Handler;
use axum::headers::HeaderName;
use axum::http::{HeaderValue, Request, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use futures::pin_mut;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyLong, PyMapping, PyString, PyTuple};
use pyo3::PyDowncastError;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Clone)]
pub struct AsgiHandler {
    app: PyObject,
    dispatcher: PythonDispatcher,
    asgi_spec: Py<PyDict>,
    handle: Handle,
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
            asgi.into_py(py)
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

impl From<PyDowncastError<'_>> for AsgiError {
    fn from(e: PyDowncastError<'_>) -> Self {
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

#[pyclass]
struct HttpReceiver {
    disconnected: Arc<AtomicBool>,
    rx: Arc<Mutex<UnboundedReceiver<AxumBytes>>>,
}

#[pymethods]
impl HttpReceiver {
    fn __call__(&self, return_func: PyObject) -> () {
        let rx = self.rx.clone();
        let disconnected = self.disconnected.clone();
        run_python_async(return_func, async move {
            let next = rx.lock().await.recv().await;

            if matches!(next, None) || disconnected.load(Ordering::SeqCst) {
                Python::with_gil(|py| {
                    let scope = PyDict::new(py);
                    scope.set_item(
                        get_cached_string(py, "type"),
                        get_cached_string(py, "http.disconnect"),
                    )?;
                    Ok(scope.into())
                })
            } else if let Some(bytes) = next {
                Python::with_gil(|py| {
                    let bytes = PyBytes::new(py, &bytes[..]);
                    let scope = PyDict::new(py);
                    scope.set_item(
                        get_cached_string(py, "type"),
                        get_cached_string(py, "http.request"),
                    )?;
                    scope.set_item(get_cached_string(py, "body"), bytes)?;
                    let scope: Py<PyDict> = scope.into();
                    Ok(scope)
                })
            } else {
                Python::with_gil(|py| {
                    let scope = PyDict::new(py);
                    scope.set_item("type", "http.request")?;
                    Ok(scope.into())
                })
            }
        })
    }
}

impl<S> Handler<AsgiHandler, S> for AsgiHandler {
    type Future = Pin<Box<dyn Future<Output = Response<BoxBody>> + Send>>;

    fn call(self, req: Request<Body>, _state: S) -> Self::Future {
        let app = self.app.clone();
        let (http_sender, mut http_sender_rx) = Sender::new();
        let disconnected = Arc::new(AtomicBool::new(false));
        let (receiver_tx, receiver_rx) = mpsc::unbounded_channel();
        let receiver = HttpReceiver {
            rx: Arc::new(Mutex::new(receiver_rx)),
            disconnected: disconnected.clone(),
        };
        let (req, body): (_, Body) = req.into_parts();
        let handle = self.handle.clone();
        handle.spawn(async move {
            pin_mut!(body);

            while let Some(s) = body.data().await {
                receiver_tx.send(s.unwrap()).unwrap();
            }
        });
        let dispatcher = self.dispatcher.clone();
        let asgi = self.asgi_spec.clone();
        Box::pin(async move {
            let _disconnected = SetTrueOnDrop(disconnected);
            match Python::with_gil(|py| {
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
                    // the spec requires this to be percent decoded at this point
                    // https://asgi.readthedocs.io/en/latest/specs/www.html#http-connection-scope
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
                    // TODO: is it even possible to get here?
                    // we have to set these to something as they're not optional in the spec
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

                let headers = req
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        let name_bytes = PyBytes::new(py, name.as_str().as_bytes());
                        let value_bytes = PyBytes::new(py, value.as_bytes());
                        PyList::new(py, [name_bytes, value_bytes])
                    })
                    .collect::<Vec<_>>();
                let headers = PyList::new(py, headers);
                scope.set_item(get_cached_string(py, "headers"), headers)?;
                // TODO: client/server args
                let sender = Py::new(py, http_sender)?;
                let receiver = Py::new(py, receiver)?;
                let args = (scope, receiver, sender);
                let res = app.call1(py, args)?;
                let fut = res.extract(py)?;
                let coro = dispatcher.dispatch_asyncio_coro(py, fut)?;
                Ok::<_, AsgiError>(coro)
            }) {
                Ok(http_coro) => {
                    handle.spawn(async move {
                        if let Err(_e) = http_coro.await {
                            tracing::error!("error handling request: {_e}");
                        }
                    });

                    let response = if let Some(resp) = http_sender_rx.recv().await {
                        let res = Python::with_gil(|py| {
                            let dict: &PyDict = resp.into_ref(py);
                            if let Some(value) = dict.get_item("type") {
                                let value: &PyString = value
                                    .downcast()
                                    .map_err(|_e| anyhow!("Invalid asgi type value"))?;
                                let value = value.to_str()?;
                                if value == "http.response.start" {
                                    let value: &PyLong = dict
                                        .get_item(get_cached_string(py, "status"))
                                        .ok_or_else(|| {
                                            PyErr::new::<PyRuntimeError, _>(
                                                "Missing status in http.response.start",
                                            )
                                        })?
                                        .downcast()
                                        .map_err(|_e| anyhow!("Invalid asgi status value"))?;
                                    let status: u16 = value.extract()?;
                                    let mut this_response = Response::builder();
                                    this_response = this_response.status(status);
                                    let headers = this_response.headers_mut().unwrap();

                                    if let Some(raw) =
                                        dict.get_item(get_cached_string(py, "headers"))
                                    {
                                        let value: &PyMapping = raw
                                            .downcast()
                                            .map_err(|_e| anyhow!("Invalid asgi headers value"))?;
                                        for item in value.iter()? {
                                            if let Ok(t) = item?.downcast::<PyTuple>() {
                                                let k = t
                                                    .get_item(0)?
                                                    .downcast::<PyBytes>()
                                                    .map_err(|_e| {
                                                        anyhow!("Invalid asgi header key value")
                                                    })?;
                                                let v = t
                                                    .get_item(1)?
                                                    .downcast::<PyBytes>()
                                                    .map_err(|_e| {
                                                        anyhow!("Invalid asgi header value value")
                                                    })?;
                                                headers.insert(
                                                    HeaderName::from_bytes(k.as_bytes())?,
                                                    HeaderValue::from_bytes(v.as_bytes())?,
                                                );
                                            }
                                        }
                                    };

                                    PuffResult::Ok(Ok(this_response))
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

                    let (sender, body_rec) = mpsc::channel(2);

                    handle.spawn(async move {
                        while let Some(resp) = http_sender_rx.recv().await {
                            let more_body = match Python::with_gil(|py| {
                                let dict: &PyDict = resp.into_ref(py);
                                if let Some(value) = dict.get_item(get_cached_string(py, "type")) {
                                    let value: &PyString = value.downcast()?;
                                    let value = value.to_str()?;
                                    if value == "http.response.body" {
                                        let more_body = if let Some(raw) =
                                            dict.get_item(get_cached_string(py, "more_body"))
                                        {
                                            raw.extract::<bool>()?
                                        } else {
                                            false
                                        };
                                        if let Some(raw) =
                                            dict.get_item(get_cached_string(py, "body"))
                                        {
                                            if let Ok(s) = raw.downcast::<PyBytes>() {
                                                Ok((
                                                    AxumBytes::copy_from_slice(s.as_bytes()),
                                                    more_body,
                                                ))
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
                                    return ();
                                }
                            };
                            if !more_body {
                                break;
                            }
                        }
                    });

                    let body = boxed(Body::wrap_stream(ReceiverStream::new(body_rec)));
                    match response.body(body) {
                        Ok(response) => response.into_response(),
                        Err(_e) => {
                            tracing::error!("Failed to create response: {_e}");
                            AsgiError::FailedToCreateResponse.into_response()
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error preparing request scope: {e:?}");
                    e.into_response()
                }
            }
        })
    }
}
