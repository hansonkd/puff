use std::collections::HashMap;
use std::time::Duration;

use crate::json::{dump_string, run_loads};
use crate::prelude::{run_python_async, with_puff_context, PuffResult};
use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple};
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, ClientBuilder, Method, Response};

/// Access the Global graphql context
#[pyclass]
#[derive(Clone)]
pub struct GlobalHttpClient;

impl ToPyObject for GlobalHttpClient {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}

#[pymethods]
impl GlobalHttpClient {
    fn __call__(&self, py: Python) -> PyObject {
        with_puff_context(|ctx| ctx.http_client()).into_py(py)
    }
}

#[pyclass]
#[derive(Clone)]
pub struct PyHttpClient {
    client: Client,
}

#[pymethods]
impl PyHttpClient {
    fn request(
        &self,
        ret_func: PyObject,
        method: &str,
        url: &str,
        headers: Option<&PyDict>,
        body: Option<Vec<u8>>,
        data: Option<HashMap<String, String>>,
        files: Option<&PyDict>,
        timeout_ms: u64,
    ) -> PyResult<()> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|_| PyTypeError::new_err("Invalid method"))?;
        let mut rb = self.client.request(method, url);

        if let Some(h) = headers {
            for (k, v) in h.iter() {
                if let Ok(key) = k.downcast::<PyString>() {
                    if let Ok(value) = v.downcast::<PyBytes>() {
                        rb = rb.header(
                            key.to_str()?,
                            HeaderValue::from_bytes(value.as_bytes())
                                .map_err(|_| PyTypeError::new_err("Invalid HeaderName"))?,
                        );
                    } else {
                        Err(PyTypeError::new_err("Header value should be bytes"))?
                    }
                } else {
                    Err(PyTypeError::new_err("Header name should be string"))?
                }
            }
        }

        if let Some(b) = body {
            rb = rb.body(b)
        } else if let Some(f) = files {
            let mut mp = Form::new();

            if let Some(d) = data {
                for (k, v) in d {
                    mp = mp.text(k, v);
                }
            }
            for (k, v) in f.iter() {
                let part_name = k.str()?.to_str()?.to_owned();
                if let Ok(text_value) = v.downcast::<PyString>() {
                    let part = Part::text(text_value.to_str()?.to_owned());
                    mp = mp.part(part_name, part);
                } else if let Ok(text_value) = v.downcast::<PyBytes>() {
                    let part = Part::bytes(text_value.as_bytes().to_vec());
                    mp = mp.part(part_name, part);
                } else if let Ok(v) = v.call_method0("read") {
                    if let Ok(text_value) = v.downcast::<PyString>() {
                        let part = Part::text(text_value.to_str()?.to_owned());
                        mp = mp.part(part_name, part);
                    } else if let Ok(text_value) = v.downcast::<PyBytes>() {
                        let part = Part::bytes(text_value.as_bytes().to_vec());
                        mp = mp.part(part_name, part);
                    }
                } else if let Ok(tuple_value) = v.downcast::<PyTuple>() {
                    let file_name = tuple_value.get_item(0)?.str()?.to_str()?.to_owned();
                    let v = tuple_value.get_item(1)?;

                    if let Ok(text_value) = v.downcast::<PyString>() {
                        let part = Part::text(text_value.to_str()?.to_owned()).file_name(file_name);
                        mp = mp.part(part_name, part);
                    } else if let Ok(text_value) = v.downcast::<PyBytes>() {
                        let part = Part::bytes(text_value.as_bytes().to_vec()).file_name(file_name);
                        mp = mp.part(part_name, part);
                    } else if let Ok(v) = v.call_method0("read") {
                        if let Ok(text_value) = v.downcast::<PyString>() {
                            let part =
                                Part::text(text_value.to_str()?.to_owned()).file_name(file_name);
                            mp = mp.part(part_name, part);
                        } else if let Ok(text_value) = v.downcast::<PyBytes>() {
                            let part =
                                Part::bytes(text_value.as_bytes().to_vec()).file_name(file_name);
                            mp = mp.part(part_name, part);
                        }
                    }
                }
            }
            rb = rb.multipart(mp)
        } else {
            if let Some(d) = data {
                rb = rb.form(&d);
            }
        }

        let request = rb
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|_| PyRuntimeError::new_err("Error building HTTP Request"))?;

        let this_client = self.client.clone();

        run_python_async(ret_func, async move {
            let response = this_client.execute(request).await?;
            let status = response.status().as_u16();
            Python::with_gil(|py| {
                Ok(PyHttpResponse {
                    response: Some(response),
                    status,
                }
                .into_py(py))
            })
        });

        Ok(())
    }

    fn request_json(
        &self,
        py: Python,
        ret_func: PyObject,
        method: &str,
        url: &str,
        headers: Option<&PyDict>,
        body: &PyAny,
        timeout_ms: u64,
    ) -> PyResult<()> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|_| PyTypeError::new_err("Invalid method"))?;
        let mut rb = self.client.request(method, url);

        if let Some(h) = headers {
            for (k, v) in h.iter() {
                if let Ok(key) = k.downcast::<PyString>() {
                    if let Ok(value) = v.downcast::<PyBytes>() {
                        rb = rb.header(
                            key.to_str()?,
                            HeaderValue::from_bytes(value.as_bytes())
                                .map_err(|_| PyTypeError::new_err("Invalid HeaderName"))?,
                        );
                    } else {
                        Err(PyTypeError::new_err("Header value should be bytes"))?
                    }
                } else {
                    Err(PyTypeError::new_err("Header name should be string"))?
                }
            }
        }

        rb = rb.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let obj = dump_string(py, body.into_py(py), None, None, None)?;
        rb = rb.body(obj);

        let request = rb
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|_| PyRuntimeError::new_err("Error building HTTP Request"))?;

        let this_client = self.client.clone();

        run_python_async(ret_func, async move {
            let response = this_client.execute(request).await?;
            let status = response.status().as_u16();
            Python::with_gil(|py| {
                Ok(PyHttpResponse {
                    response: Some(response),
                    status,
                }
                .into_py(py))
            })
        });

        Ok(())
    }
}

#[pyclass]
pub struct PyHttpResponse {
    response: Option<Response>,
    status: u16,
}

#[pymethods]
impl PyHttpResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn cookies(&mut self, py: Python) -> PyResult<PyObject> {
        let d = PyDict::new(py);
        if let Some(r) = &self.response {
            for c in r.cookies() {
                d.set_item(c.name(), c.value())?
            }
        }
        Ok(d.into_py(py))
    }

    pub fn headers(&mut self, py: Python) -> PyResult<PyObject> {
        let d = PyDict::new(py);
        if let Some(r) = &self.response {
            for (hn, hv) in r.headers() {
                d.set_item(hn.as_str(), hv.as_bytes())?
            }
        }
        Ok(d.into_py(py))
    }

    pub fn header(&mut self, py: Python, hn: &PyString) -> PyResult<Option<PyObject>> {
        if let Some(r) = &self.response {
            if let Some(v) = r.headers().get(hn.to_str()?) {
                return Ok(Some(PyBytes::new(py, v.as_bytes()).into_py(py)));
            }
        }
        Ok(None)
    }

    pub fn json(&mut self, return_func: PyObject) -> PyResult<()> {
        let r = self
            .response
            .take()
            .ok_or(PyRuntimeError::new_err("Already consumed response"))?;
        run_python_async(return_func, async move {
            let bs = r.text().await?;
            let r = Python::with_gil(|py| run_loads(py, bs, None, None))?;
            Ok(r)
        });
        Ok(())
    }

    pub fn body(&mut self, return_func: PyObject) -> PyResult<()> {
        let r = self
            .response
            .take()
            .ok_or(PyRuntimeError::new_err("Already consumed response"))?;
        run_python_async(return_func, async move {
            let bs = r.bytes().await?;
            let r: PyObject = Python::with_gil(|py| PyBytes::new(py, &bs).into_py(py));
            Ok(r)
        });
        Ok(())
    }

    pub fn body_text(&mut self, return_func: PyObject) -> PyResult<()> {
        let r = self
            .response
            .take()
            .ok_or(PyRuntimeError::new_err("Already consumed response"))?;
        run_python_async(return_func, async move {
            let bs = r.text().await?;
            let r: PyObject = Python::with_gil(|py| PyString::new(py, bs.as_str()).into_py(py));
            Ok(r)
        });
        Ok(())
    }
}

/// Build a new TaskQueue with the provided connection information.
pub fn new_http_client(client_builder: ClientBuilder) -> PuffResult<PyHttpClient> {
    let client = client_builder.build()?;
    let http_client = PyHttpClient { client };
    Ok(http_client)
}
