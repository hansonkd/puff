use wsgi::Sender;
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
    str,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use std::io::Stderr;
use std::task::{Context, Poll};
use anyhow::{anyhow, Error};
use axum::body::{Full, HttpBody};
use axum::headers::HeaderMap;
use axum::http::header::ToStrError;
use futures_util::TryFutureExt;
use hyper::body::{Buf, SizeHint};
use tokio::sync::{mpsc::{self, UnboundedReceiver}, Mutex, oneshot};
use tracing::{error, info};
use crate::python::greenlet::GreenletDispatcher;
use crate::python::wsgi;
use crate::runtime::dispatcher::RuntimeDispatcher;
use crate::runtime::yield_to_future;


const MAX_LIST_BODY_INLINE_CONCAT: u64 = 1024 * 4;

#[derive(Clone)]
pub struct WsgiHandler {
    app: PyObject,
    dispatcher: RuntimeDispatcher,
    greenlet: Option<GreenletDispatcher>
}

impl WsgiHandler {
    pub fn new(app: PyObject, dispatcher: RuntimeDispatcher, greenlet: GreenletDispatcher) -> WsgiHandler {
        WsgiHandler { app, dispatcher, greenlet: Some(greenlet) }
    }

    pub fn blocking(app: PyObject, dispatcher: RuntimeDispatcher) -> WsgiHandler {
        WsgiHandler { app, dispatcher, greenlet: None }
    }
}

#[derive(Debug)]
enum WsgiError {
    PyErr(PyErr),
    InvalidHttpVersion,
    ExpectedResponseStart,
    MissingResponse,
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
            WsgiError::PyErr(_)
            | WsgiError::ExpectedResponseStart
            | WsgiError::MissingResponse
            | WsgiError::ExpectedResponseBody
            | WsgiError::FailedToCreateResponse
            | WsgiError::InvalidHeader => {
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
    rx: Arc<Mutex<UnboundedReceiver<Option<Body>>>>
}


pub struct HttpResponseBody(PyObject, Option<u64>);


impl HttpBody for HttpResponseBody {
    // type Data = PyBytesBuf;
    type Data = Bytes;
    type Error = Error;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Self::Error>>> {
        let b = Python::with_gil(|py| {
            let obj = &self.0;
            let r = obj.as_ref(py);
            if let Ok(next_bytes) = r.call_method0("__next__") {
                let extracted = {
                    if let Ok(bytes) = next_bytes.downcast::<PyBytes>() {
                        Ok(Bytes::copy_from_slice(bytes.as_bytes()))
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

    fn poll_trailers(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
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
        let (http_sender, mut http_sender_rx) = Sender::new();
        let disconnected = Arc::new(AtomicBool::new(false));
        let (req, body): (_, Body) = req.into_parts();

        let body_fut = move || {
            let body_bytes = if let Ok(body_bytes) = yield_to_future(hyper::body::to_bytes(body)) {
                body_bytes
            } else {
                 error!("Could not extract request body.");
                return Response::builder().status(StatusCode::BAD_REQUEST).body(Body::empty()).unwrap().into_response();
            };
            let mut content_length: Option<u64> = None;

            // receiver_tx.send(Some(body)).unwrap();
            let _disconnected = SetTrueOnDrop(disconnected);
            let iterator_res: Result<Result<(PyObject, PyObject), Response<Body>>, Error> = Python::with_gil(|py| {
                let environ = PyDict::new(py);
                environ.set_item("wsgi.version", (1, 0))?;
                environ.set_item("wsgi.url_scheme", req.uri.scheme_str().unwrap_or("http"))?;
                environ.set_item("wsgi.input", PyBytes::new(py, &body_bytes[..]))?;
                environ.set_item("wsgi.errors", "")?;
                environ.set_item("wsgi.multithread", true)?;
                environ.set_item("wsgi.run_once", false)?;

                let server_protocol = match req.version {
                    Version::HTTP_10 => "HTTP/1.0",
                    Version::HTTP_11 => "HTTP/1.1",
                    Version::HTTP_2 => "HTTP/2",
                    _ => {
                        error!("Invalid HTTP version");
                        return Ok(Err(Response::builder().status(StatusCode::BAD_REQUEST).body(Body::empty()).unwrap()));
                    },
                };

                environ.set_item("SERVER_PROTOCOL", server_protocol)?;
                environ.set_item("REQUEST_METHOD", req.method.as_str())?;
                if let Some(path_and_query) = req.uri.path_and_query() {
                    let path = path_and_query.path();
                    let raw_path = path.as_bytes();
                    // the spec requires this to be percent decoded at this point
                    // https://asgi.readthedocs.io/en/latest/specs/www.html#http-connection-scope
                    let path = if let Ok(r) = percent_encoding::percent_decode(raw_path).decode_utf8() {
                        r
                    } else {
                        error!("Invalid path encoding");
                        return Ok(Err(Response::builder().status(StatusCode::BAD_REQUEST).body(Body::empty())).unwrap());
                    };

                    environ.set_item("PATH_INFO", path)?;
                    if let Some(query) = path_and_query.query() {
                        environ.set_item("QUERY_STRING", query)?;
                    } else {
                        environ.set_item("QUERY_STRING", "")?;
                    }
                } else {
                    // TODO: is it even possible to get here?
                    // we have to set these to something as they're not optional in the spec
                    environ.set_item("PATH_INFO", "")?;
                    environ.set_item("QUERY_STRING", "")?;
                }
                environ.set_item("SCRIPT_NAME", "")?;

                for (name, value) in req.headers.iter() {
                    let corrected_name = match name.as_str() {
                        "content-length" => {
                            let the_string = str::from_utf8(value.as_bytes())?;
                            content_length = Some(the_string.parse()?);
                            "CONTENT_LENGTH".to_owned()
                        },
                        "content-type" => "CONTENT_TYPE".to_owned(),
                        s => s.to_uppercase().replace("-", "_"),
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

            match iterator_res {
                Ok(Err(res)) => res.into_response(),
                Ok(Ok(args)) => {
                    let iterator_res = Python::with_gil(|py| app.call1(py, args));
                    let iterator = match iterator_res {
                        Ok(r) => r,
                        Err(e) => {
                            error!("Couldn't call wsgi app function, {e}");
                            return WsgiError::PyErr(e).into_response()
                        }
                    };

                    let mut response = Response::builder();
                    let responded = if let Ok(r) = yield_to_future(http_sender_rx) {
                        r
                    } else {
                        error!("Did not receive start_response");
                        return WsgiError::ExpectedResponseStart.into_response()
                    };

                    if let (status_code_str, pyheaders) = responded {
                        let status = status_code_str.split(" ").next().expect("Invalid wsgi status format");
                        let status_code: u16 = status.parse().expect("Invalid wsgi status code format");
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
                    }
                    Python::with_gil(|py| {
                        let iter_py = iterator.as_ref(py);
                        if content_length.unwrap_or(u64::MAX) < MAX_LIST_BODY_INLINE_CONCAT {
                                if let Ok(s)  = iter_py.extract::<Vec<&[u8]>>() {
                                    let merged = s.concat();
                                    let body = Full::from(merged);
                                    return response.body(body).map(|f| f.into_response()).unwrap_or(WsgiError::FailedToCreateResponse.into_response())
                                }
                        }
                        let body = HttpResponseBody(PyObject::from(iter_py.iter().unwrap()), content_length);
                        response.body(body).map(|f| f.into_response()).unwrap_or(WsgiError::FailedToCreateResponse.into_response())
                    })
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Error preparing request scope: {e:?}");
                    Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Body::empty()).unwrap().into_response()
                }
            }
        };

        let real_fut = async move {
            let ret =
                if self.blocking {
                    self.dispatcher.dispatch_blocking(|| Ok(body_fut())).await
                } else {
                    self.dispatcher.dispatch(|| Ok(body_fut())).await
                };
            match ret {
                Ok(r) => {
                    r
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Error preparing request scope: {e:?}");
                    Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Body::empty()).unwrap().into_response()
                }
            }
        };
        Box::pin(real_fut)
    }
}