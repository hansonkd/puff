use crate::python::{get_cached_string, log_traceback_with_label, wsgi, PythonDispatcher};
use anyhow::{anyhow, Error};
use axum::body::{Body, BoxBody, Bytes, Full, HttpBody};
use axum::handler::Handler;
use axum::headers::{HeaderMap, HeaderName};

use axum::http::{HeaderValue, Request, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use hyper::body::SizeHint;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyString};
use pyo3::PyDowncastError;
use std::future::Future;

use std::pin::Pin;
use std::str;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::errors::handle_puff_error;
use crate::types::Text;
use tracing::error;
use wsgi::Sender;

const MAX_LIST_BODY_INLINE_CONCAT: u64 = 1024 * 32;

#[derive(Clone)]
pub struct WsgiHandler {
    app: PyObject,
    server_name: Text,
    server_port: u16,
    python_dispatcher: PythonDispatcher,
    std_err: PyObject,
    bytesio: PyObject,
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

impl From<PyDowncastError<'_>> for WsgiError {
    fn from(e: PyDowncastError<'_>) -> Self {
        WsgiError::PyErr(e.into())
    }
}

impl Into<Error> for WsgiError {
    fn into(self) -> Error {
        anyhow!("Error with response {:?}", self)
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

pub struct HttpResponseBody(PyObject, Option<u64>);

impl HttpBody for HttpResponseBody {
    // type Data = PyBytesBuf;
    type Data = Bytes;
    type Error = Error;

    fn poll_data(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Self::Error>>> {
        let b = Python::with_gil(|py| {
            let obj = &self.0;
            let r = obj.as_ref(py);
            if let Ok(next_bytes) = r.call_method0("__next__") {
                let extracted = {
                    if let Ok(bytes) = next_bytes.downcast::<PyBytes>() {
                        Ok(Bytes::copy_from_slice(bytes.as_bytes()))
                        // Ok(Bytes::copy_from_slice(bytes.as_bytes()))
                    } else if let Ok(str) = next_bytes.downcast::<PyString>() {
                        Ok(Bytes::copy_from_slice(str.to_str().unwrap().as_bytes()))
                    } else {
                        Err(anyhow!("Invalid type returned in wsgi stream"))
                    }
                };
                Some(extracted)
            } else {
                None
            }
        });
        Poll::Ready(b)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
        Poll::Ready(Ok(None))
    }

    fn size_hint(&self) -> SizeHint {
        if let Some(hint) = self.1 {
            SizeHint::with_exact(hint)
        } else {
            SizeHint::default()
        }
    }
}

impl<S> Handler<WsgiHandler, S> for WsgiHandler {
    type Future = Pin<Box<dyn Future<Output = Response<BoxBody>> + Send>>;

    fn call(self, req: Request<Body>, _state: Arc<S>) -> Self::Future {
        let app = self.app.clone();
        let (http_sender, http_sender_rx) = Sender::new();
        let disconnected = Arc::new(AtomicBool::new(false));
        let (req, body): (_, Body) = req.into_parts();

        let body_fut = async move {
            let body_bytes = if let Ok(body_bytes) = hyper::body::to_bytes(body).await {
                body_bytes
            } else {
                error!("Could not extract request body.");
                return WsgiError::ExpectedResponseBody.into_response();
            };
            let mut content_length: Option<u64> = None;

            // receiver_tx.send(Some(body)).unwrap();
            let _disconnected = SetTrueOnDrop(disconnected);
            let args_to_send: Result<Result<(PyObject, PyObject), Response>, Error> =
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
                    environ.set_item(get_cached_string(py, "wsgi.errors"), self.std_err.clone())?;
                    environ.set_item(get_cached_string(py, "wsgi.multithread"), false)?;
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
                        // the spec requires this to be percent decoded at this point
                        // https://asgi.readthedocs.io/en/latest/specs/www.html#http-connection-scope
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
                        // TODO: is it even possible to get here?
                        // we have to set these to something as they're not optional in the spec
                        environ.set_item("PATH_INFO", "")?;
                        environ.set_item("QUERY_STRING", "")?;
                    }
                    environ.set_item(get_cached_string(py, "SCRIPT_NAME"), "")?;

                    for (name, value) in req.headers.iter() {
                        let corrected_name = match name.to_string().to_uppercase().as_str() {
                            "CONTENT-LENGTH" => {
                                let the_string = str::from_utf8(value.as_bytes())?;
                                content_length = Some(the_string.parse()?);
                                "CONTENT_LENGTH".to_owned()
                            }
                            "CONTENT-TYPE" => "CONTENT_TYPE".to_owned(),
                            s => format!("HTTP_{}", s.replace("-", "_")),
                        };
                        if let Some(val) = environ.get_item(corrected_name.as_str()) {
                            let s = val.downcast::<PyString>().unwrap();
                            let new_value = [s.to_str()?, value.to_str()?].join(",");
                            environ.set_item(corrected_name.as_str(), new_value)?;
                        } else {
                            environ.set_item(corrected_name.as_str(), value.to_str()?)?;
                        }
                    }

                    // TODO: client/server args
                    let sender = Py::new(py, http_sender)?;
                    let args = (PyObject::from(environ), sender.into_py(py));

                    Ok::<_, Error>(Ok(args))
                });

            match args_to_send {
                Ok(Err(res)) => res.into_response(),
                Ok(Ok(args)) => {
                    let calculate_value = async {
                        let r = {
                            let rec = self.python_dispatcher.dispatch1(app, args)?;
                            rec.await.map_err(|_e| {
                                PyException::new_err("Could not await greenlet result in wsgi.")
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
                    let status = status_code_str
                        .split(" ")
                        .next()
                        .expect("Invalid wsgi status format");
                    let status_code: u16 = status.parse().expect("Invalid wsgi status code format");
                    let headers = response.headers_mut().unwrap();
                    let mut resp_content_len: Option<u64> = None;
                    for (name, value) in pyheaders {
                        if name.to_uppercase() == "CONTENT-LENGTH" {
                            resp_content_len = Some(value.parse().expect("Invalid content-length"));
                        }
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
                    let res = Python::with_gil(|py| {
                        let iter_py = iterator.as_ref(py);

                        if resp_content_len.unwrap_or(u64::MAX) < MAX_LIST_BODY_INLINE_CONCAT {
                            let mut combined =
                                Vec::with_capacity(resp_content_len.unwrap_or(0) as usize);
                            for x in iter_py.iter()? {
                                let bytes = x?.downcast::<PyBytes>()?;
                                combined.extend_from_slice(bytes.as_bytes());
                            }
                            let body = Full::from(combined);
                            Ok(response
                                .body(body)
                                .map(|f| f.into_response())
                                .unwrap_or(WsgiError::FailedToCreateResponse.into_response()))
                        } else {
                            let body = HttpResponseBody(
                                PyObject::from(iter_py.iter().unwrap()),
                                resp_content_len,
                            );
                            Ok(response
                                .body(body)
                                .map(|f| f.into_response())
                                .unwrap_or(WsgiError::FailedToCreateResponse.into_response()))
                        }
                    });

                    match res {
                        Ok(r) => r,
                        Err(e) => WsgiError::PyErr(e).into_response(),
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
