use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;

#[pyclass]
pub struct PyEventLoopQueue {
    handle: Handle,
    sender: UnboundedSender<PyObject>,
    receiver: Arc<Mutex<UnboundedReceiver<PyObject>>>,
}

impl PyEventLoopQueue {
    pub fn new(handle: Handle) -> Self {
        let (sender, receiver) = unbounded_channel();

        PyEventLoopQueue {
            handle,
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }
}

#[pymethods]
impl PyEventLoopQueue {
    pub fn put(&self, item: PyObject) -> PyResult<()> {
        self.sender
            .send(item)
            .map_err(|_| PyRuntimeError::new_err("Could not add item to Queue"))
    }

    pub fn get(&self, py: Python) -> Option<PyObject> {
        let r = self.receiver.clone();

        py.allow_threads(|| {
            self.handle
                .block_on(async move { r.lock().await.recv().await })
        })
    }
}
