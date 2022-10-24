use crate::context::with_puff_context;
use crate::databases::pubsub::{ConnectionId, PubSubClient, PubSubConnection, PubSubMessage};
use crate::errors::to_py_error;
use crate::prelude::run_python_async;
use crate::python::async_python::handle_python_return;
use crate::types::{Bytes, Text};

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use pyo3::{IntoPy, PyObject, Python, ToPyObject};
use std::str::FromStr;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[pyclass]
pub struct GlobalPubSub;

impl ToPyObject for GlobalPubSub {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        GlobalPubSub.into_py(py)
    }
}

#[pymethods]
impl GlobalPubSub {
    fn __call__(&self) -> PythonPubSubClient {
        let client = with_puff_context(|ctx| ctx.pubsub());
        PythonPubSubClient { client }
    }
}

#[pyclass]
struct PythonPubSubClient {
    client: PubSubClient,
}

#[pymethods]
impl PythonPubSubClient {
    fn new_connection_id(&self, _py: Python) -> String {
        self.client.new_connection_id().to_string()
    }

    fn publish_as(
        &self,
        ret_fun: PyObject,
        connection_id: &PyString,
        channel: Text,
        message: Text,
    ) -> PyResult<()> {
        let connection_id_bytes = ConnectionId::from_str(connection_id.to_str()?)
            .map_err(|_e| PyTypeError::new_err("Invalid Connection ID"))?;
        let client = self.client.clone();
        let bytes = Bytes::copy_from_slice(message.as_bytes());
        run_python_async(ret_fun, async move {
            client
                .publish_as(connection_id_bytes, channel, bytes)
                .await?;
            Ok(true)
        });
        Ok(())
    }

    fn publish_bytes_as(
        &self,
        ret_fun: PyObject,
        connection_id: &PyString,
        channel: Text,
        message: &PyBytes,
    ) -> PyResult<()> {
        let connection_id_bytes = ConnectionId::from_str(connection_id.to_str()?)
            .map_err(|_e| PyTypeError::new_err("Invalid Connection ID"))?;
        let bytes = Bytes::copy_from_slice(message.as_bytes());
        let client = self.client.clone();
        run_python_async(ret_fun, async move {
            client
                .publish_as(connection_id_bytes, channel, bytes)
                .await?;
            Ok(true)
        });
        Ok(())
    }

    fn connection_with_id(&self, py: Python, connection_id: &PyString) -> PyResult<PyObject> {
        let connection_id_bytes = ConnectionId::from_str(connection_id.to_str()?)
            .map_err(|_e| PyTypeError::new_err("Invalid Connection ID"))?;
        let (connection, rec) = to_py_error(
            "pubsub",
            self.client.connection_with_id(connection_id_bytes),
        )?;
        start_pubsub_listener_loop(py, connection, rec)
    }

    fn connection(&self, py: Python) -> PyResult<PyObject> {
        let (connection, rec) = to_py_error("pubsub", self.client.connection())?;
        start_pubsub_listener_loop(py, connection, rec)
    }
}

fn start_pubsub_listener_loop(
    py: Python,
    connection: PubSubConnection,
    mut rec: UnboundedReceiver<PubSubMessage>,
) -> PyResult<PyObject> {
    let (sender, mut job_rec) = mpsc::unbounded_channel();
    let handle = with_puff_context(|ctx| ctx.handle());
    handle.spawn(async move {
        while let Some(job) = job_rec.recv().await {
            handle_python_return(job, async {
                let next_msg = rec.recv().await;
                Ok(next_msg.map(|m| PyPubSubMessage { message: m }))
            })
            .await
        }
    });
    Ok(PythonPubSubConnection { connection, sender }.into_py(py))
}

#[pyclass]
struct PythonPubSubConnection {
    connection: PubSubConnection,
    sender: UnboundedSender<PyObject>,
}

#[derive(Clone)]
#[pyclass]
struct PyPubSubMessage {
    message: PubSubMessage,
}

#[pymethods]
impl PyPubSubMessage {
    #[getter]
    fn get_text(&self, py: Python) -> PyObject {
        self.message.text().into_py(py)
    }

    #[getter]
    fn get_body(&self, py: Python) -> PyObject {
        self.message.body().to_object(py)
    }

    #[getter]
    fn get_from_connection_id(&self) -> String {
        self.message.from().to_string()
    }
}

impl ToPyObject for PyPubSubMessage {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}

#[pymethods]
impl PythonPubSubConnection {
    fn who_am_i(&self, _py: Python) -> String {
        self.connection.who_am_i().to_string()
    }

    fn subscribe(&self, ret_fun: PyObject, channel: Text) {
        let conn = self.connection.clone();
        run_python_async(ret_fun, async move { Ok(conn.subscribe(channel).await) })
    }

    fn publish(&self, ret_fun: PyObject, channel: Text, message: Text) {
        let conn = self.connection.clone();
        let bytes = Bytes::copy_from_slice(message.as_bytes());
        run_python_async(ret_fun, async move {
            conn.publish(channel, bytes).await?;
            Ok(true)
        })
    }

    fn publish_bytes(&self, ret_fun: PyObject, channel: Text, message: &PyBytes) {
        let bytes = Bytes::copy_from_slice(message.as_bytes());
        let conn = self.connection.clone();
        run_python_async(ret_fun, async move {
            conn.publish(channel, bytes).await?;
            Ok(true)
        })
    }

    fn receive(&mut self, ret_fun: PyObject) -> PyResult<()> {
        if let Err(r) = self.sender.send(ret_fun) {
            Python::with_gil(|py| r.0.call1(py, (py.None(), py.None())))?;
        }
        Ok(())
    }
}
