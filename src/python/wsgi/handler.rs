use crate::python::{get_cached_string, log_traceback_with_label, wsgi, PythonDispatcher};
use anyhow::{anyhow, Error};
use axum::body::Body;
use axum::handler::Handler;
use http::header::HeaderName;

use axum::http::{HeaderValue, Request, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyString};
use pyo3::DowncastError;
use std::convert::Infallible;
use std::future::Future;

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::errors::handle_puff_error;
use crate::types::Text;
use tokio_stream::wrappers::ReceiverStream;
use tracing::error;
use wsgi::Sender;

pub struct WsgiHandler {
    app: PyObject,
    server_name: Text,
    server_port: u16,
    python_dispatcher: PythonDispatcher,
    std_err: PyObject,
    bytesio: PyObject,
}

impl Clone for WsgiHandler {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            app: self.app.clone_ref(py),
            server_name: self.server_name.clone(),
            server_port: self.server_port,
            python_dispatcher: self.python_dispatcher.clone(),
            std_err: self.std_err.clone_ref(py),
            bytesio: self.bytesio.clone_ref(py),
        })
    }
}

impl WsgiHandler {
    pub fn new(
        app: PyObject,
        python_dispatcher: PythonDispatcher,
        server_name: Text,
        server_port: u16,
        std_err: PyObject,
        bytesio: PyObject,
    ) -> WsgiHandler {
        WsgiHandler {
            app,
            python_dispatcher,
            server_name,
            server_port,
            std_err,
            bytesio,
        }
    }
}

#[derive(Debug)]
enum WsgiError {
    PyErr(PyErr),
    InvalidHttpVersion,
    ExpectedResponseStart,
    ExpectedResponseBody,
    FailedToCreateResponse,
    InvalidHeader,
    InvalidUtf8InPath,
}

impl From<PyErr> for WsgiError {
    fn from(e: PyErr) -> Self {
        WsgiError::PyErr(e)
    }
}

impl<'a, 'py> From<DowncastError<'a, 'py>> for WsgiError {
    fn from(e: DowncastError<'a, 'py>) -> Self {
        WsgiError::PyErr(e.into())
    }
}

impl From<WsgiError> for Error {
    fn from(val: WsgiError) -> Self {
        anyhow!("Error with response {:?}", val)
    }
}

impl IntoResponse for WsgiError {
    fn into_response(self) -> Response {
        match self {
            WsgiError::InvalidHttpVersion => (StatusCode::BAD_REQUEST, "Unsupported HTTP version"),
            WsgiError::InvalidUtf8InPath => (StatusCode::BAD_REQUEST, "Invalid Utf8 in path"),
            WsgiError::ExpectedResponseStart
            | WsgiError::ExpectedResponseBody
            | WsgiError::FailedToCreateResponse
            | WsgiError::InvalidHeader => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
            WsgiError::PyErr(e) => {
                log_traceback_with_label("Wsgi", &e);
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

impl<S> Handler<WsgiHandler, S> for WsgiHandler {
    type Future = Pin<Box<dyn Future<Output = Response> + Send>>;

    fn call(self, req: Request<Body>, _state: S) -> Self::Future {
        // No with_gil here — we move self into the async block and access
        // PyObjects only inside with_gil blocks below. This eliminates one
        // GIL acquisition per request.
        let (http_sender, http_sender_rx) = Sender::new();
        let disconnected = Arc::new(AtomicBool::new(false));
        let (req, body): (_, Body) = req.into_parts();

        let body_fut = async move {
            // Cap the request body at 10 MiB to prevent unbounded memory usage.
            const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;
            let body_bytes = if let Ok(body_bytes) = axum::body::to_bytes(body, MAX_BODY_SIZE).await
            {
                body_bytes
            } else {
                error!("Could not extract request body (exceeds {} byte limit).", MAX_BODY_SIZE);
                return WsgiError::ExpectedResponseBody.into_response();
            };

            let _disconnected = SetTrueOnDrop(disconnected);

            // Single with_gil block: build environ + clone app + create sender args
            // This merges what was previously 2 separate with_gil calls.
            let args_to_send: Result<Result<(PyObject, PyObject, PyObject), Response>, Error> =
                Python::with_gil(|py| {
                    let environ = PyDict::new(py);
                    environ.set_item(get_cached_string(py, "wsgi.version"), (1, 0))?;
                    environ.set_item(
                        get_cached_string(py, "wsgi.url_scheme"),
                        req.uri.scheme_str().unwrap_or("http"),
                    )?;
                    environ.set_item(
                        get_cached_string(py, "wsgi.input"),
                        self.bytesio
                            .call1(py, (PyByteArray::new(py, &body_bytes[..]),))?,
                    )?;
                    environ.set_item(
                        get_cached_string(py, "wsgi.errors"),
                        self.std_err.clone_ref(py),
                    )?;
                    environ.set_item(get_cached_string(py, "wsgi.multithread"), true)?;
                    environ.set_item(get_cached_string(py, "wsgi.run_once"), false)?;

                    let server_protocol = match req.version {
                        Version::HTTP_10 => get_cached_string(py, "HTTP/1.0"),
                        Version::HTTP_11 => get_cached_string(py, "HTTP/1.1"),
                        Version::HTTP_2 => get_cached_string(py, "HTTP/2"),
                        _ => {
                            error!("Invalid HTTP version");
                            return Ok(Err(WsgiError::InvalidHttpVersion.into_response()));
                        }
                    };

                    environ.set_item(
                        get_cached_string(py, "SERVER_NAME"),
                        self.server_name.as_str(),
                    )?;
                    environ.set_item(get_cached_string(py, "SERVER_PORT"), self.server_port)?;
                    environ.set_item(get_cached_string(py, "SERVER_PROTOCOL"), server_protocol)?;
                    environ
                        .set_item(get_cached_string(py, "REQUEST_METHOD"), req.method.as_str())?;
                    if let Some(path_and_query) = req.uri.path_and_query() {
                        let path = path_and_query.path();
                        let raw_path = path.as_bytes();
                        let path = if let Ok(r) =
                            percent_encoding::percent_decode(raw_path).decode_utf8()
                        {
                            r
                        } else {
                            error!("Invalid path encoding");
                            return Ok(Err(WsgiError::InvalidUtf8InPath.into_response()));
                        };

                        environ.set_item(get_cached_string(py, "PATH_INFO"), path)?;
                        if let Some(query) = path_and_query.query() {
                            environ.set_item(get_cached_string(py, "QUERY_STRING"), query)?;
                        } else {
                            environ.set_item(
                                get_cached_string(py, "QUERY_STRING"),
                                get_cached_string(py, ""),
                            )?;
                        }
                    } else {
                        environ.set_item("PATH_INFO", "")?;
                        environ.set_item("QUERY_STRING", "")?;
                    }
                    environ.set_item(get_cached_string(py, "SCRIPT_NAME"), "")?;

                    for (name, value) in req.headers.iter() {
                        let corrected_name = match name.to_string().to_uppercase().as_str() {
                            "CONTENT-LENGTH" => "CONTENT_LENGTH".to_owned(),
                            "CONTENT-TYPE" => "CONTENT_TYPE".to_owned(),
                            s => format!("HTTP_{}", s.replace("-", "_")),
                        };
                        if let Some(val) = environ.get_item(corrected_name.as_str())? {
                            let s: Bound<'_, PyString> = val
                                .downcast()
                                .map_err(|e| {
                                    pyo3::PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                                        e.to_string(),
                                    )
                                })?
                                .clone();
                            let new_value = [s.to_str()?, value.to_str()?].join(",");
                            environ.set_item(corrected_name.as_str(), new_value)?;
                        } else {
                            environ.set_item(corrected_name.as_str(), value.to_str()?)?;
                        }
                    }

                    let sender = Py::new(py, http_sender)?;
                    let args = (environ.into_any().unbind(), sender.into_py(py));
                    // Clone app inside this same with_gil — no extra GIL acquisition
                    let app_for_dispatch = self.app.clone_ref(py);

                    Ok::<_, Error>(Ok((args.0, args.1, app_for_dispatch)))
                });

            match args_to_send {
                Ok(Err(res)) => res.into_response(),
                Ok(Ok((arg0, arg1, app))) => {
                    let calculate_value = async {
                        let r = {
                            let rec = self.python_dispatcher.dispatch1(app, (arg0, arg1))?;
                            rec.await.map_err(|_e| {
                                PyException::new_err("Could not await result in wsgi.")
                            })??
                        };
                        Ok(r)
                    };
                    let iterator_res = calculate_value.await;
                    let iterator = match iterator_res {
                        Ok(r) => r,
                        Err(e) => return WsgiError::PyErr(e).into_response(),
                    };
                    let mut response = Response::builder();
                    let responded = if let Ok(r) = http_sender_rx.await {
                        r
                    } else {
                        error!("Did not receive start_response");
                        return WsgiError::ExpectedResponseStart.into_response();
                    };
                    let (status_code_str, pyheaders) = responded;
                    let status = match status_code_str.split(' ').next() {
                        Some(s) => s,
                        None => return WsgiError::FailedToCreateResponse.into_response(),
                    };
                    let status_code: u16 = match status.parse() {
                        Ok(c) => c,
                        Err(_) => return WsgiError::FailedToCreateResponse.into_response(),
                    };
                    let headers = response.headers_mut().unwrap();
                    for (name, value) in pyheaders {
                        let name = match HeaderName::from_bytes(name.as_bytes()) {
                            Ok(name) => name,
                            Err(_e) => {
                                return WsgiError::InvalidHeader.into_response();
                            }
                        };
                        let value = match HeaderValue::from_bytes(value.as_bytes()) {
                            Ok(value) => value,
                            Err(_e) => {
                                return WsgiError::InvalidHeader.into_response();
                            }
                        };
                        headers.append(name, value);
                    }
                    response = response.status(status_code);

                    // Stream the response body through a channel instead of
                    // buffering the entire iterator in memory.
                    let (body_tx, body_rx) =
                        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, Infallible>>(4);

                    tokio::task::spawn_blocking(move || {
                        Python::with_gil(|py| {
                            let iter_py = iterator.bind(py);
                            if let Ok(iter) = iter_py.iter() {
                                for x in iter {
                                    if let Ok(item) = x {
                                        let chunk =
                                            if let Ok(bytes) = item.downcast::<PyBytes>() {
                                                Some(axum::body::Bytes::copy_from_slice(
                                                    bytes.as_bytes(),
                                                ))
                                            } else if let Ok(s) = item.downcast::<PyString>() {
                                                Some(axum::body::Bytes::copy_from_slice(
                                                    s.to_str().unwrap_or("").as_bytes(),
                                                ))
                                            } else {
                                                None
                                            };
                                        if let Some(chunk) = chunk {
                                            if body_tx.blocking_send(Ok(chunk)).is_err() {
                                                break; // client disconnected
                                            }
                                        }
                                    }
                                }
                            }
                            // body_tx is dropped here, signalling end-of-stream
                        });
                    });

                    let body = Body::from_stream(ReceiverStream::new(body_rx));
                    match response.body(body) {
                        Ok(r) => r.into_response(),
                        Err(_e) => WsgiError::FailedToCreateResponse.into_response(),
                    }
                }
                Err(e) => {
                    handle_puff_error("Wsgi Request Scope", e);
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .unwrap()
                        .into_response()
                }
            }
        };

        Box::pin(body_fut)
    }
}
