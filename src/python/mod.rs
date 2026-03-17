use crate::errors::PuffResult;
use crate::python::redis::RedisGlobal;

use crate::context::{set_puff_context, set_puff_context_waiting, with_puff_context, PuffContext};
use crate::python::async_python::{run_python_async, AsyncReturn};
use crate::runtime::RuntimeConfig;
use crate::tasks::GlobalTaskQueue;
use crate::types::text::Text;
use crate::web::client::GlobalHttpClient;
use pyo3::types::{IntoPyDict, PyBytes, PyDict, PyString, PyTuple};
use std::os::raw::c_int;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::error;

use crate::python::graphql::GlobalGraphQL;
use crate::python::postgres::{add_pg_puff_exceptions, PostgresGlobal};
use tracing::log::info;

use crate::json::{dump, dumpb, dumps, load, loadb, loads};
use crate::python::pubsub::GlobalPubSub;
use pyo3::exceptions::PyTypeError;
pub use pyo3::prelude::*;
use std::str;

pub mod asgi;
pub mod async_python;
pub mod graphql;
pub mod postgres;
pub mod pubsub;
pub mod queue;
pub mod redis;
pub mod wsgi;

thread_local! {
    static CACHED_STRINGS: RefCell<HashMap<&'static str, Py<PyString>>> = RefCell::new(HashMap::new());
    static CACHED_IMPORTED_OBJS: RefCell<HashMap<Text, PyObject>> = RefCell::new(HashMap::new());
    static CACHED_PATH_IMPORTER: RefCell<Option<PyObject>> = RefCell::new(None);
}

pub fn get_cached_string<'a>(py: Python<'a>, k: &'static str) -> Py<PyString> {
    CACHED_STRINGS.with(|d| {
        let mut s = d.borrow_mut();
        if let Some(existing) = s.get(k) {
            existing.clone_ref(py)
        } else {
            let new_str: Py<PyString> = PyString::new(py, k).unbind();
            s.insert(k, new_str.clone_ref(py));
            new_str
        }
    })
}

pub fn get_cached_path_importer<'a>(py: Python<'a>) -> PyObject {
    CACHED_PATH_IMPORTER.with(|d| {
        let mut s = d.borrow_mut();
        if let Some(existing) = s.as_ref() {
            existing.clone_ref(py)
        } else {
            let puff_mod = py.import("puff").expect("Could not import puff");
            let string_import_fn = puff_mod
                .getattr("import_string")
                .expect("Could not import puff.import_string")
                .unbind();
            *s = Some(string_import_fn.clone_ref(py));
            string_import_fn
        }
    })
}

pub fn get_cached_object<'a>(py: Python<'a>, k: Text) -> PyResult<PyObject> {
    CACHED_IMPORTED_OBJS.with(|d| {
        let mut s = d.borrow_mut();
        if let Some(existing) = s.get(&k) {
            Ok(existing.clone_ref(py))
        } else {
            let cache_importer = get_cached_path_importer(py);
            let result = cache_importer.call1(py, (k.as_str(),))?;
            s.insert(k, result.clone_ref(py));
            Ok(result)
        }
    })
}

#[pyclass]
struct PyDispatchAsyncIO;

#[pymethods]
impl PyDispatchAsyncIO {
    pub fn __call__(&self, return_func: PyObject, f: PyObject) -> PyResult<()> {
        let dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
        run_python_async(return_func, async move {
            Ok(dispatcher.dispatch_asyncio(f, (), None)?.await??)
        });
        Ok(())
    }
}

#[pyclass]
struct ReadFileBytes;

#[pymethods]
impl ReadFileBytes {
    pub fn __call__(&self, return_func: PyObject, file_name: String) {
        run_python_async(return_func, async move {
            let contents = tokio::fs::read(&file_name).await?;
            Ok(Python::with_gil(|py| {
                PyBytes::new(py, contents.as_slice()).to_object(py)
            }))
        })
    }
}

#[pyclass]
struct PySleepMs;

#[pymethods]
impl PySleepMs {
    pub fn __call__(&self, return_func: PyObject, sleep_time_ms: u64) {
        run_python_async(return_func, async move {
            tokio::time::sleep(Duration::from_millis(sleep_time_ms)).await;
            Ok(Python::with_gil(|py| py.None()))
        })
    }
}

#[pyclass]
struct PyDispatchAsyncCoro;

#[pymethods]
impl PyDispatchAsyncCoro {
    pub fn __call__(&self, return_func: PyObject, awaitable: PyObject) -> PyResult<()> {
        let dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
        run_python_async(return_func, async move {
            Ok(Python::with_gil(|py| dispatcher.dispatch_asyncio_coro(py, awaitable))?.await??)
        });
        Ok(())
    }
}

pub fn log_traceback(e: &PyErr) {
    log_traceback_with_label("Unexpected", e)
}

pub fn log_traceback_with_label(label: &str, e: &PyErr) {
    Python::with_gil(|py| {
        let t = e.traceback_bound(py);
        let tb = t
            .map(|f| {
                let tb = f
                    .format()
                    .unwrap_or_else(|e| format!("Error formatting traceback\n: {e}"));
                format!("\n{}", tb)
            })
            .unwrap_or_default();
        error!("Encountered Python {label} Error:\n{e}{tb}");
    });
}

const ACTIVATE_THIS: &'static std::ffi::CStr = c"
import sys
import os

old_os_path = os.environ.get('PATH', '')
os.environ['PATH'] = os.path.dirname(os.path.abspath(abs_file)) + os.pathsep + old_os_path
base = os.path.dirname(os.path.dirname(os.path.abspath(abs_file)))
if sys.platform == 'win32':
    site_packages = os.path.join(base, 'Lib', 'site-packages')
else:
    site_packages = os.path.join(base, 'lib', 'python%s' % sys.version.split(' ', 1)[0].rsplit('.', 1)[0], 'site-packages')
prev_sys_path = list(sys.path)
import site
site.addsitedir(site_packages)
sys.real_prefix = sys.prefix
sys.prefix = base
# Move the added items to the front of the path:
new_sys_path = []
for item in list(sys.path):
    if item not in prev_sys_path:
        new_sys_path.append(item)
        sys.path.remove(item)
sys.path[:0] = new_sys_path
";

pub(crate) fn bootstrap_puff_globals(config: RuntimeConfig) -> PuffResult<()> {
    let global_state = config.global_state()?;
    Python::with_gil(|py| {
        if let Ok(v) = std::env::var("VIRTUAL_ENV") {
            let locals = [("abs_file", format!("{}/bin/activate", v))].into_py_dict(py)?;
            py.run(ACTIVATE_THIS, None, Some(&locals))?;
        }

        let sys_path = py.import("sys")?.getattr("path")?;
        let mut paths = config.python_paths();
        paths.reverse();
        for t in paths {
            info!("Adding {} to beginning of PYTHONPATH", t);
            sys_path.call_method1("insert", (0, t.as_str()))?;
        }

        info!("Adding puff to python....");
        let puff_mod = py.import("puff")?;
        let puff_json_mod = py.import("puff.json_impl")?;

        let puff_rust_functions = puff_mod.getattr("rust_objects")?;
        puff_rust_functions.setattr("is_puff", true)?;
        puff_rust_functions.setattr("global_redis_getter", RedisGlobal)?;
        puff_rust_functions.setattr("global_postgres_getter", PostgresGlobal)?;
        puff_rust_functions.setattr("global_pubsub_getter", GlobalPubSub)?;
        puff_rust_functions.setattr("global_gql_getter", GlobalGraphQL)?;
        puff_rust_functions.setattr("global_task_queue_getter", GlobalTaskQueue)?;
        puff_rust_functions.setattr("global_http_client_getter", GlobalHttpClient)?;
        puff_rust_functions.setattr("dispatch_asyncio", PyDispatchAsyncIO.into_py(py))?;
        puff_rust_functions.setattr("dispatch_asyncio_coro", PyDispatchAsyncCoro.into_py(py))?;
        puff_json_mod.add_function(wrap_pyfunction!(load, &puff_json_mod)?)?;
        puff_json_mod.add_function(wrap_pyfunction!(loads, &puff_json_mod)?)?;
        puff_json_mod.add_function(wrap_pyfunction!(loadb, &puff_json_mod)?)?;
        puff_json_mod.add_function(wrap_pyfunction!(dump, &puff_json_mod)?)?;
        puff_json_mod.add_function(wrap_pyfunction!(dumps, &puff_json_mod)?)?;
        puff_json_mod.add_function(wrap_pyfunction!(dumpb, &puff_json_mod)?)?;
        add_pg_puff_exceptions(py)?;
        puff_mod.setattr("global_state", global_state)?;
        puff_rust_functions.setattr("read_file_bytes", ReadFileBytes.into_py(py))?;
        puff_rust_functions.setattr("sleep_ms", PySleepMs.into_py(py))?;
        crate::agents::python_bindings::register_python_classes(&puff_mod)?;
        puff_mod.call_method0("patch_libs")?;
        info!("Finished adding puff to python.");
        PyResult::Ok(())
    })?;
    Ok(())
}

#[inline]
pub fn error_on_minusone(py: Python<'_>, result: c_int) -> PyResult<()> {
    if result != -1 {
        Ok(())
    } else {
        Err(PyErr::take(py).unwrap_or_else(|| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Unknown error")))
    }
}

#[pyclass]
pub struct PythonPuffContextWaitingSetter(Arc<Mutex<Option<PuffContext>>>);

#[pymethods]
impl PythonPuffContextWaitingSetter {
    pub fn __call__(&self) {
        set_puff_context_waiting(self.0.clone())
    }
}

#[pyclass]
pub struct PythonPuffContextSetter(PuffContext);

#[pymethods]
impl PythonPuffContextSetter {
    pub fn __call__(&self) {
        set_puff_context(self.0.clone())
    }
}

pub struct PythonDispatcher {
    asyncio_obj: Option<PyObject>,
    spawn_blocking_fn: PyObject,
}

impl Clone for PythonDispatcher {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            asyncio_obj: self.asyncio_obj.as_ref().map(|o| o.clone_ref(py)),
            spawn_blocking_fn: self.spawn_blocking_fn.clone_ref(py),
        })
    }
}

fn spawn_blocking_fn(py: Python) -> PyResult<PyObject> {
    let puff = py.import("puff")?;
    let res = puff.getattr("spawn_blocking_from_rust")?;
    Ok(res.into_py(py))
}

impl PythonDispatcher {
    pub fn new(
        config: RuntimeConfig,
        context_waiting: Arc<Mutex<Option<PuffContext>>>,
    ) -> PyResult<Self> {
        let (asyncio_obj, spawn_blocking_fn) = Python::with_gil(|py| {
            let asyncio_thread = if config.asyncio() {
                let puff_asyncio = py.import("puff.asyncio_support")?;
                Some(
                    puff_asyncio
                        .call_method1(
                            "start_event_loop",
                            (PythonPuffContextWaitingSetter(context_waiting.clone()),),
                        )?
                        .into_py(py),
                )
            } else {
                None
            };
            PyResult::Ok((asyncio_thread, spawn_blocking_fn(py)?))
        })?;

        PyResult::Ok(Self {
            asyncio_obj,
            spawn_blocking_fn,
        })
    }

    /// Run the python function a new Tokio blocking working thread.
    pub fn dispatch_blocking<A: IntoPy<Py<PyTuple>>>(
        &self,
        py: Python,
        function: PyObject,
        args: A,
        kwargs: Py<PyDict>,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        let (sender, rec) = oneshot::channel();
        let ret = AsyncReturn::new(Some(sender));
        let on_thread_start = with_puff_context(PythonPuffContextSetter);
        self.spawn_blocking_fn.call1(
            py,
            (
                on_thread_start,
                function,
                args.into_py(py),
                kwargs.into_any(),
                ret,
            ),
        )?;
        Ok(rec)
    }

    /// Acquires the GIL and executes the python function on a blocking thread.
    /// Takes no keyword arguments.
    pub fn dispatch1<A: IntoPy<Py<PyTuple>> + Send + 'static>(
        &self,
        function: PyObject,
        args: A,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        self.dispatch(function, args, None)
    }

    /// Acquires the GIL and executes the python function on a blocking thread.
    pub fn dispatch<A: IntoPy<Py<PyTuple>>>(
        &self,
        function: PyObject,
        args: A,
        kwargs: Option<Py<PyDict>>,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        Python::with_gil(|py| {
            let kwargs: Py<PyDict> = kwargs
                .unwrap_or_else(|| PyDict::new(py).unbind());
            self.dispatch_blocking(py, function, args, kwargs)
        })
    }

    /// Executes the python function on the asyncio thread to create an awaitable to add.
    pub fn dispatch_asyncio<A: IntoPy<Py<PyTuple>>>(
        &self,
        function: PyObject,
        args: A,
        kwargs: Option<Py<PyDict>>,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        Python::with_gil(|py| {
            let (sender, rec) = oneshot::channel();
            let returner = AsyncReturn::new(Some(sender));
            self.asyncio_obj
                .as_ref()
                .expect("AsyncIO not enabled")
                .call_method1(
                    py,
                    "spawn",
                    (
                        function,
                        args.into_py(py).into_any(),
                        kwargs.unwrap_or_else(|| PyDict::new(py).unbind()).into_any(),
                        returner,
                    ),
                )?;
            Ok(rec)
        })
    }

    /// Dispatch an awaitable coroutine onto the event loop.
    pub fn dispatch_asyncio_coro(
        &self,
        py: Python,
        function: PyObject,
    ) -> PyResult<oneshot::Receiver<PyResult<PyObject>>> {
        let (sender, rec) = oneshot::channel();
        let returner = AsyncReturn::new(Some(sender));
        self.asyncio_obj
            .as_ref()
            .expect("AsyncIO not enabled")
            .call_method1(py, "spawn_coro", (function, returner))?;
        Ok(rec)
    }
}

pub fn setup_python_executors(
    config: RuntimeConfig,
    context_waiting: Arc<Mutex<Option<PuffContext>>>,
) -> PyResult<PythonDispatcher> {
    PythonDispatcher::new(config, context_waiting)
}

pub fn py_obj_to_bytes<'py>(val: &Bound<'py, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(r) = val.downcast::<PyString>() {
        Ok(r.to_str()?.as_bytes().to_vec())
    } else if let Ok(r) = val.downcast::<PyBytes>() {
        Ok(r.as_bytes().to_vec())
    } else {
        Err(PyTypeError::new_err(format!(
            "Expected str or bytes, got {}",
            val.to_string()
        )))
    }
}
