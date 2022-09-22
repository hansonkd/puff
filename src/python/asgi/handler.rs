use asgi::Sender;
use axum::{
    body::{boxed, Body, BoxBody, Bytes},
    handler::Handler,
    headers::HeaderName,
    http::{HeaderValue, Request, StatusCode, Version},
    response::{IntoResponse, Response},
};
use pyo3::types::{PyBytes, PyDict, PyLong, PyString};
use pyo3::{
    exceptions::PyRuntimeError,
    prelude::*,
    types::{PyList, PyMapping},
    PyDowncastError,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver},
    Mutex,
};
use crate::python::asgi;

#[derive(Clone)]
pub struct AsgiHandler {
    app: PyObject,
    locals: pyo3_asyncio::TaskLocals,
}

impl AsgiHandler {
    pub fn new_with_locals(app: PyObject, locals: pyo3_asyncio::TaskLocals) -> AsgiHandler {
        AsgiHandler { app, locals }
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
    rx: Arc<Mutex<UnboundedReceiver<Option<Body>>>>,
    locals: pyo3_asyncio::TaskLocals,
}

#[pymethods]
impl HttpReceiver {
    fn __call__<'a>(&'a self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let rx = self.rx.clone();
        let disconnected = self.disconnected.clone();
        pyo3_asyncio::tokio::future_into_py_with_locals(py, self.locals.clone(), async move {
            let next = rx.lock().await.recv().await;

            if matches!(next, None) || disconnected.load(Ordering::SeqCst) {
                Python::with_gil(|py| {
                    let scope = PyDict::new(py);
                    scope.set_item("type", "http.disconnect")?;
                    Ok::<_, PyErr>(scope.into())
                })
            } else if let Some(Some(body)) = next {
                let bytes = hyper::body::to_bytes(body)
                    .await
                    .map_err(|_e| PyErr::new::<PyRuntimeError, _>("failed to fetch data"))?;
                Python::with_gil(|py| {
                    let bytes = PyBytes::new(py, &bytes[..]);
                    let scope = PyDict::new(py);
                    scope.set_item("type", "http.request")?;
                    scope.set_item("body", bytes)?;
                    let scope: Py<PyDict> = scope.into();
                    Ok::<_, PyErr>(scope)
                })
            } else {
                Python::with_gil(|py| {
                    let scope = PyDict::new(py);
                    scope.set_item("type", "http.request")?;
                    Ok::<_, PyErr>(scope.into())
                })
            }
        })
    }
}

impl<S> Handler<AsgiHandler, S> for AsgiHandler {
    type Future = Pin<Box<dyn Future<Output = Response<BoxBody>> + Send>>;

    fn call(self, req: Request<Body>, _state: Arc<S>) -> Self::Future {
        let app = self.app.clone();
        let locals = self.locals;
        let (http_sender, mut http_sender_rx) = Sender::new(locals.clone());
        let disconnected = Arc::new(AtomicBool::new(false));
        let (receiver_tx, receiver_rx) = mpsc::unbounded_channel();
        let receiver = HttpReceiver {
            rx: Arc::new(Mutex::new(receiver_rx)),
            disconnected: disconnected.clone(),
            locals: locals.clone(),
        };
        let (req, body): (_, Body) = req.into_parts();
        Box::pin(async move {
            receiver_tx.send(Some(body)).unwrap();
            let _disconnected = SetTrueOnDrop(disconnected);
            match Python::with_gil(|py| {
                let asgi = PyDict::new(py);
                asgi.set_item("spec_version", "2.0")?;
                asgi.set_item("version", "2.0")?;
                let scope = PyDict::new(py);
                scope.set_item("type", "http")?;
                scope.set_item("asgi", asgi)?;
                scope.set_item(
                    "http_version",
                    match req.version {
                        Version::HTTP_10 => "1.0",
                        Version::HTTP_11 => "1.1",
                        Version::HTTP_2 => "2",
                        _ => return Err(AsgiError::InvalidHttpVersion),
                    },
                )?;
                scope.set_item("method", req.method.as_str())?;
                scope.set_item("scheme", req.uri.scheme_str().unwrap_or("http"))?;
                if let Some(path_and_query) = req.uri.path_and_query() {
                    let path = path_and_query.path();
                    let raw_path = path.as_bytes();
                    // the spec requires this to be percent decoded at this point
                    // https://asgi.readthedocs.io/en/latest/specs/www.html#http-connection-scope
                    let path = percent_encoding::percent_decode(raw_path)
                        .decode_utf8()
                        .map_err(|_| AsgiError::InvalidUtf8InPath)?;
                    scope.set_item("path", path)?;
                    let raw_path_bytes = PyBytes::new(py, path_and_query.path().as_bytes());
                    scope.set_item("raw_path", raw_path_bytes)?;
                    if let Some(query) = path_and_query.query() {
                        let qs_bytes = PyBytes::new(py, query.as_bytes());
                        scope.set_item("query_string", qs_bytes)?;
                    } else {
                        let qs_bytes = PyBytes::new(py, "".as_bytes());
                        scope.set_item("query_string", qs_bytes)?;
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
                scope.set_item("root_path", "")?;

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
                scope.set_item("headers", headers)?;
                // TODO: client/server args
                let sender = Py::new(py, http_sender)?;
                let receiver = Py::new(py, receiver)?;
                let args = (scope, receiver, sender);
                let res = app.call1(py, args)?;
                let fut = res.extract(py)?;
                let coro = pyo3_asyncio::into_future_with_locals(&locals, fut)?;
                Ok::<_, AsgiError>(coro)
            }) {
                Ok(http_coro) => {
                    tokio::spawn(async move {
                        if let Err(_e) = http_coro.await {
                            #[cfg(feature = "tracing")]
                            tracing::error!("error handling request: {_e}");
                        }
                    });

                    let mut response = Response::builder();

                    if let Some(resp) = http_sender_rx.recv().await {
                        let (status, headers) = match Python::with_gil(|py| {
                            let dict: &PyDict = resp.into_ref(py);
                            if let Some(value) = dict.get_item("type") {
                                let value: &PyString = value.downcast()?;
                                let value = value.to_str()?;
                                if value == "http.response.start" {
                                    let value: &PyLong = dict
                                        .get_item("status")
                                        .ok_or_else(|| {
                                            PyErr::new::<PyRuntimeError, _>(
                                                "Missing status in http.response.start",
                                            )
                                        })?
                                        .downcast()?;
                                    let status: u16 = value.extract()?;

                                    let headers = if let Some(raw) = dict.get_item("headers") {
                                        let value: &PyMapping = raw.downcast()?;
                                        Some(
                                            value
                                                .iter()?
                                                .map(|item| {
                                                    item.and_then(
                                                        PyAny::extract::<(Vec<u8>, Vec<u8>)>,
                                                    )
                                                })
                                                .collect::<PyResult<Vec<_>>>()?,
                                        )
                                    } else {
                                        None
                                    };
                                    Ok((status, headers))
                                } else {
                                    Err(AsgiError::ExpectedResponseStart)
                                }
                            } else {
                                Err(AsgiError::ExpectedResponseStart)
                            }
                        }) {
                            Ok((status, headers)) => (status, headers),
                            Err(e) => {
                                return e.into_response();
                            }
                        };
                        response = response.status(status);
                        if let Some(pyheaders) = headers {
                            let headers = response.headers_mut().unwrap();
                            for (name, value) in pyheaders {
                                let name = match HeaderName::from_bytes(&name) {
                                    Ok(name) => name,
                                    Err(_e) => {
                                        return AsgiError::InvalidHeader.into_response();
                                    }
                                };
                                let value = match HeaderValue::from_bytes(&value) {
                                    Ok(value) => value,
                                    Err(_e) => {
                                        return AsgiError::InvalidHeader.into_response();
                                    }
                                };
                                headers.append(name, value);
                            }
                        }
                    } else {
                        return AsgiError::MissingResponse.into_response();
                    }

                    let mut body = Vec::new();
                    while let Some(resp) = http_sender_rx.recv().await {
                        let (bytes, more_body) = match Python::with_gil(|py| {
                            let dict: &PyDict = resp.into_ref(py);
                            if let Some(value) = dict.get_item("type") {
                                let value: &PyString = value.downcast()?;
                                let value = value.to_str()?;
                                if value == "http.response.body" {
                                    let more_body = if let Some(raw) = dict.get_item("more_body") {
                                        raw.extract::<bool>()?
                                    } else {
                                        false
                                    };
                                    if let Some(raw) = dict.get_item("body") {
                                        Ok((raw.extract::<Vec<u8>>()?, more_body))
                                    } else {
                                        Ok((Vec::new(), more_body))
                                    }
                                } else {
                                    Err(AsgiError::ExpectedResponseBody)
                                }
                            } else {
                                Err(AsgiError::ExpectedResponseBody)
                            }
                        }) {
                            Ok((bytes, more_body)) => (bytes, more_body),
                            Err(e) => {
                                return e.into_response();
                            }
                        };
                        body.extend(bytes);
                        if !more_body {
                            break;
                        }
                    }

                    let body = boxed(Body::from(Bytes::from(body)));
                    match response.body(body) {
                        Ok(response) => response.into_response(),
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!("Failed to create response: {_e}");
                            AsgiError::FailedToCreateResponse.into_response()
                        }
                    }
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Error preparing request scope: {e:?}");
                    e.into_response()
                }
            }
        })
    }
}