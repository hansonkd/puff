use anyhow::bail;
use std::time::{Duration, SystemTime};

use crate::context::is_puff_context_ready;
use crate::errors::{handle_puff_result, log_puff_error, to_py_error, PuffResult};
use crate::prelude::with_puff_context;
use crate::python::async_python::run_python_async;
use crate::python::{get_cached_object, PythonDispatcher};
use crate::types::text::{Text, ToText};
use bb8_postgres::bb8::Pool;
use bb8_redis::redis::{Cmd, IntoConnectionInfo, Pipeline};
use bb8_redis::RedisConnectionManager;
use clap::{Arg, Command};
use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::{error, info};
use uuid::Uuid;

/// Access the Global graphql context
#[pyclass]
#[derive(Clone)]
pub struct GlobalTaskQueue;

impl ToPyObject for GlobalTaskQueue {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py)
    }
}

#[pymethods]
impl GlobalTaskQueue {
    fn __call__(&self, py: Python) -> PyObject {
        with_puff_context(|ctx| ctx.task_queue()).into_py(py)
    }
    fn by_name(&self, name: &str) -> TaskQueue {
        with_puff_context(|ctx| ctx.task_queue_named(name))
    }
}

#[derive(Clone)]
#[pyclass]
pub struct TaskQueue {
    pool: Pool<RedisConnectionManager>,
    queue_name: Text,
}

#[pymethods]
impl TaskQueue {
    fn add_task(
        &self,
        py: Python,
        ret_func: PyObject,
        func_path: Text,
        py_params: PyObject,
        unix_time_ms: usize,
        timeout_ms: usize,
        keep_results_for_ms: usize,
        async_fn: bool,
        trigger: bool,
    ) -> PyResult<()> {
        let params = pythonize::depythonize(py_params.as_ref(py))
            .map_err(|_| PyException::new_err("Could not convert python value to json"))?;
        let this_self = self.clone();
        run_python_async(
            ret_func,
            add_task(
                this_self,
                func_path,
                params,
                unix_time_ms,
                timeout_ms,
                keep_results_for_ms,
                async_fn,
                trigger,
            ),
        );
        Ok(())
    }

    fn task_result(&self, ret_func: PyObject, item_key: Vec<u8>) -> PyResult<()> {
        let this_self = self.clone();
        run_python_async(ret_func, task_result(this_self, item_key));
        Ok(())
    }

    fn wait_for_task_result(
        &self,
        ret_func: PyObject,
        item_key: Vec<u8>,
        poll_interval_ms: u64,
        timeout_ms: u128,
    ) -> PyResult<()> {
        let this_self = self.clone();
        run_python_async(
            ret_func,
            wait_for_result(this_self, item_key, poll_interval_ms, timeout_ms),
        );
        Ok(())
    }
}

impl TaskQueue {
    pub fn new<T: Into<Text>>(queue_name: T, pool: Pool<RedisConnectionManager>) -> Self {
        Self {
            queue_name: queue_name.into(),
            pool,
        }
    }
}

/// A task sent to the TaskQueue
#[derive(Clone, Serialize, Deserialize)]
#[pyclass]
pub struct Task {
    id: Uuid,
    func_import_path: Text,
    params: serde_json::Value,
    scheduled_at: usize,
    timeout_ms: usize,
    keep_results_for_ms: usize,
    async_fn: bool,
}

#[pymethods]
impl Task {
    fn task_id(&self, py: Python) -> Py<PyBytes> {
        PyBytes::new(py, self.id.as_bytes()).into_py(py)
    }

    fn params(&self, py: Python) -> PyResult<PyObject> {
        pythonize::pythonize(py, &self.params)
            .map_err(|_| PyException::new_err("Could not convert json value to to python"))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[pyclass]
pub struct TaskResult {
    result: serde_json::Value,
    exception: Option<Text>,
    finished_at_unix_ms: u128,
}

pub async fn add_task<T: Into<Text>>(
    task_queue: TaskQueue,
    func_path: T,
    params: serde_json::Value,
    unix_time_ms: usize,
    timeout_ms: usize,
    keep_results_for_ms: usize,
    async_fn: bool,
    trigger: bool,
) -> PuffResult<Py<PyBytes>> {
    let id = uuid::Uuid::new_v4();
    let item_key = id.into_bytes();
    let t = Task {
        id,
        func_import_path: func_path.into(),
        params,
        scheduled_at: unix_time_ms,
        timeout_ms,
        keep_results_for_ms,
        async_fn,
    };
    let to_send = serde_json::ser::to_vec(&t)?;
    let mut r = task_queue.pool.get().await?;
    let task_hmap = task_hmap_key(&task_queue);
    let task_queue_key = task_queue_key(&task_queue);
    let queue_wait_key = queue_wait_key(&task_queue);

    let cmd2 = Cmd::hset(task_hmap.as_slice(), &item_key, to_send);
    let cmd = Cmd::zadd(task_queue_key.as_slice(), &item_key, unix_time_ms);
    let mut pipe = Pipeline::new();

    pipe.add_command(cmd2).ignore().add_command(cmd).ignore();

    if trigger {
        let cmd3 = Cmd::zadd(queue_wait_key.as_slice(), b"1", 0);
        pipe.add_command(cmd3).ignore();
    }

    pipe.query_async(&mut *r).await?;

    Python::with_gil(|py| Ok(PyBytes::new(py, &item_key).into_py(py)))
}

fn item_key_lock(task_queue: &TaskQueue, key: &[u8]) -> Vec<u8> {
    [task_queue.queue_name.as_bytes(), b"-lock-", key].concat()
}

fn item_key_complete(task_queue: &TaskQueue, key: &[u8]) -> Vec<u8> {
    [task_queue.queue_name.as_bytes(), b"-complete-", key].concat()
}

fn task_hmap_key(task_queue: &TaskQueue) -> Vec<u8> {
    [task_queue.queue_name.as_bytes(), b"-tasks"].concat()
}

fn task_queue_key(task_queue: &TaskQueue) -> Vec<u8> {
    [task_queue.queue_name.as_bytes(), b"-queue"].concat()
}

fn queue_wait_key(task_queue: &TaskQueue) -> Vec<u8> {
    [task_queue.queue_name.as_bytes(), b"-wait"].concat()
}

pub async fn finish_task(
    task_queue: &TaskQueue,
    item: &[u8],
    result: Result<serde_json::Value, Text>,
    keep_results_for_ms: usize,
) -> PuffResult<()> {
    let item_key_complete = item_key_complete(task_queue, item);
    let task_hmap_key = task_hmap_key(task_queue);
    let task_queue_key = task_queue_key(&task_queue);

    let mut r = task_queue.pool.get().await?;
    let finished_at_unix_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_millis();

    let task_result = match result {
        Err(e) => TaskResult {
            result: serde_json::Value::Null,
            exception: Some(e),
            finished_at_unix_ms,
        },
        Ok(r) => TaskResult {
            result: r,
            exception: None,
            finished_at_unix_ms,
        },
    };

    let result_vec = serde_json::to_vec(&task_result)?;
    let cmd = Cmd::new()
        .arg("SET")
        .arg(item_key_complete)
        .arg(&result_vec)
        .arg("NX")
        .arg("PX")
        .arg(keep_results_for_ms)
        .clone();
    let res = Pipeline::new()
        .atomic()
        .hdel(task_hmap_key.as_slice(), item)
        .ignore()
        .zrem(task_queue_key.as_slice(), &[item])
        .ignore()
        .add_command(cmd)
        .ignore()
        .query_async(&mut *r)
        .await?;
    Ok(res)
}

pub async fn task_result(task_queue: TaskQueue, item: Vec<u8>) -> PuffResult<Option<PyObject>> {
    let item_key_complete = item_key_complete(&task_queue, item.as_slice());

    let mut r = task_queue.pool.get().await?;

    let result: Option<Vec<u8>> = Cmd::get(item_key_complete).query_async(&mut *r).await?;

    let res = match result {
        Some(v) => {
            let task = serde_json::de::from_slice::<TaskResult>(&v)?;
            if let Some(e) = task.exception {
                error!(
                    "Reading task result, but task resulted in the following error: {}",
                    &e
                );
                Err(PyRuntimeError::new_err(
                    "Task resulted in an error, see logs for details.",
                ))?
            } else {
                Some(Python::with_gil(|py| {
                    pythonize::pythonize(py, &task.result)
                })?)
            }
        }
        None => None,
    };

    Ok(res)
}

pub async fn wait_for_result(
    task_queue: TaskQueue,
    item: Vec<u8>,
    poll_interval_ms: u64,
    timeout_ms: u128,
) -> PuffResult<Option<PyObject>> {
    let item_key_complete = item_key_complete(&task_queue, item.as_slice());
    let sleep_duration = Duration::from_millis(poll_interval_ms);
    let start_time = SystemTime::now();

    loop {
        let mut r = task_queue.pool.get().await?;
        let result: Option<Vec<u8>> = Cmd::get(&item_key_complete).query_async(&mut *r).await?;
        match result {
            Some(v) => {
                let task = serde_json::de::from_slice::<TaskResult>(&v)?;
                if let Some(e) = task.exception {
                    error!("Reading task result, but task resulted in an error {}", &e);
                    Err(PyRuntimeError::new_err(
                        "Task resulted in an error, see logs for details.",
                    ))?
                } else {
                    return Ok(Some(Python::with_gil(|py| {
                        pythonize::pythonize(py, &task.result)
                    })?));
                }
            }
            None => (),
        };

        if SystemTime::now().duration_since(start_time)?.as_millis() < timeout_ms {
            tokio::time::sleep(sleep_duration).await
        } else {
            return Ok(None);
        }
    }
}

pub async fn next_task(
    task_queue: &TaskQueue,
    tasks_per_loop: isize,
    wait_duration: Duration,
) -> PuffResult<Task> {
    let mut r = task_queue.pool.get().await?;
    let task_hmap_key = task_hmap_key(task_queue);
    let task_queue_key = task_queue_key(task_queue);
    let queue_wait_key = queue_wait_key(&task_queue);
    let wait_duration_secs =
        wait_duration.as_secs() as f64 + wait_duration.subsec_nanos() as f64 * 1e-9;

    loop {
        let unix_time_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis();
        let mut offset = 0;

        loop {
            let next_items: Vec<Vec<u8>> = Cmd::zrangebyscore_limit(
                task_queue_key.as_slice(),
                "-inf",
                unix_time_ms as f64,
                offset,
                tasks_per_loop,
            )
            .query_async(&mut *r)
            .await?;
            let next_items_len = next_items.len();
            if next_items_len == 0 {
                break;
            }
            let mut pipeline = Pipeline::new();
            for item in &next_items {
                let item_key = item_key_lock(task_queue, item.as_slice());
                let item_key_complete = item_key_complete(task_queue, item.as_slice());
                pipeline.exists(&item_key);
                pipeline.exists(&item_key_complete);
            }
            let existing: Vec<bool> = pipeline.query_async(&mut *r).await?;
            let mut exiting_iter = existing.iter();

            for item in next_items {
                let locked = exiting_iter.next().unwrap();
                let completed = exiting_iter.next().unwrap();

                if !locked && !completed {
                    let maybe_task: Option<Vec<u8>> = Cmd::hget(&task_hmap_key, &item)
                        .query_async(&mut *r)
                        .await?;

                    let t: Task = if let Some(task) = maybe_task {
                        match serde_json::de::from_slice(task.as_slice()) {
                            Ok(t) => t,
                            Err(e) => {
                                finish_task(
                                    &task_queue,
                                    item.as_slice(),
                                    Err(format!("Could not deserialize {}", e).into()),
                                    30 * 1000,
                                )
                                .await?;
                                continue;
                            }
                        }
                    } else {
                        continue;
                    };

                    let item_key = item_key_lock(task_queue, item.as_slice());
                    let claimed: bool = Cmd::new()
                        .arg("SET")
                        .arg(item_key)
                        .arg("1")
                        .arg("NX")
                        .arg("PX")
                        .arg(t.timeout_ms)
                        .query_async(&mut *r)
                        .await?;
                    if claimed {
                        return Ok(t);
                    }
                }
            }

            if next_items_len < tasks_per_loop as usize {
                break;
            } else {
                offset += tasks_per_loop;
            }
        }
        let mut blocking_cmd = Cmd::new();
        blocking_cmd
            .arg("BZPOPMIN")
            .arg(queue_wait_key.as_slice())
            .arg(wait_duration_secs);
        let _fut = blocking_cmd.query_async::<_, i64>(&mut *r).await;
    }
}

pub async fn do_loop_iteration(
    task_queue: &TaskQueue,
    task: Task,
    dispatcher: PythonDispatcher,
) -> PuffResult<()> {
    let func_and_payload_result: PyResult<_> = Python::with_gil(|py| {
        let cached_obj = get_cached_object(py, task.func_import_path.clone())?;
        let py_params = pythonize::pythonize(py, &task.params)?;
        Ok((cached_obj, py_params))
    });

    let json_text_res = async {
        let (func, payload) = func_and_payload_result?;
        if !Python::with_gil(|py| func.as_ref(py).hasattr("__is_puff_task"))? {
            bail!("Task does not have __is_puff_task set.");
        }
        let task_result = if task.async_fn {
            dispatcher
                .dispatch_asyncio(func, (payload,), None)?
                .await??
        } else {
            dispatcher.dispatch1(func, (payload,))?.await??
        };
        let json_value_result =
            Python::with_gil(|py| pythonize::depythonize(task_result.into_ref(py)))?;
        Ok(json_value_result)
    };
    let job_result = log_puff_error("task-queue-job", json_text_res.await);
    let json_plain_text_result = match to_py_error("task-loop", job_result) {
        Ok(r) => Ok(r),
        Err(e) => Err(Python::with_gil(|py| {
            let tb = e.traceback(py).map(|tb| tb.format().ok());
            format!("{}\n{:?}", e, tb.flatten()).to_text()
        })),
    };

    finish_task(
        task_queue,
        task.id.as_bytes(),
        json_plain_text_result,
        task.keep_results_for_ms,
    )
    .await?;
    Ok(())
}

pub fn loop_tasks(
    task_queue: TaskQueue,
    num_workers: usize,
    handle: Handle,
    dispatcher: PythonDispatcher,
) -> () {
    let tasks_per_loop = num_workers as isize;
    let startup_wait_duration = Duration::from_millis(10);
    let wait_duration = Duration::from_millis(1000);
    let (sender, mut rec) = mpsc::unbounded_channel();
    let first_sender = sender.clone();
    let inner_handle = handle.clone();
    handle.spawn(async move {
        while !is_puff_context_ready() {
            tokio::time::sleep(startup_wait_duration).await;
        }

        if num_workers == 0 {
            tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
        }

        while let Some(()) = rec.recv().await {
            let new_sender = sender.clone();
            let new_tq = task_queue.clone();
            let new_dispatcher = dispatcher.clone();
            let task = match next_task(&new_tq, tasks_per_loop, wait_duration).await {
                Ok(t) => t,
                Err(e) => {
                    handle_puff_result("task-queue-next-task", Err(e));
                    new_sender.send(()).expect("Could not trigger next task.");
                    continue;
                }
            };

            inner_handle.spawn(async move {
                let loop_result = do_loop_iteration(&new_tq, task, new_dispatcher).await;
                handle_puff_result("task-queue-loop", loop_result);
                new_sender.send(()).expect("Could not trigger next task.");
            });
        }
    });
    handle.spawn(async move {
        for _ in 0..num_workers {
            first_sender
                .send(())
                .expect("Could not trigger task worker.");
        }
    });
}

/// Build a new TaskQueue with the provided connection information.
pub async fn new_task_queue_async<T: IntoConnectionInfo>(
    conn: T,
    check: bool,
    pool_size: u32,
) -> PuffResult<TaskQueue> {
    let conn_info = conn.into_connection_info()?;
    let manager = RedisConnectionManager::new(conn_info.clone())?;
    let pool = Pool::builder().max_size(pool_size).build(manager).await?;
    let local_pool = pool.clone();
    if check {
        info!("Checking TaskQueue connectivity...");
        let check_fut = async {
            let mut conn = local_pool.get().await?;
            PuffResult::Ok(Cmd::new().arg("PING").query_async(&mut *conn).await?)
        };

        tokio::time::timeout(Duration::from_secs(5), check_fut).await??;
        info!("TaskQueue looks good.");
    }
    let client = TaskQueue {
        queue_name: "_tq".into(),
        pool,
    };
    Ok(client)
}

pub(crate) fn add_task_queue_command_arguments(name: &str, command: Command) -> Command {
    let name_lower = name.to_lowercase();
    let name_upper = name.to_uppercase();
    command.arg(
        Arg::new(format!("{}_task_queue_url", name_lower))
            .long(format!("{}-task-queue-url", name_lower))
            .num_args(1)
            .value_name(format!("{}_TASK_QUEUE_URL", name_upper))
            .env(format!("PUFF_{}_TASK_QUEUE_URL", name_upper))
            .default_value("redis://localhost:6379")
            .help(format!("{} Redis TaskQueue configuration.", name)),
    )
}
