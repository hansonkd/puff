//! Types used to interact with the Puff Runtime
//!

use pyo3::prelude::PyObject;
use pyo3::{PyResult, Python};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::types::text::ToText;
use crate::types::{Puff, Text};

/// Strategies for distributing tasks onto a coroutine worker thread.
/// In general the strategy shouldn't matter that much if your tasks
/// are roughly uniform in terms of computational requirements. However,
/// some strategies might be better depending on the context of your application. For example,
/// [Strategy::LeastBusy] can be useful if you have certain tasks that might be heavier than others.
/// All counting and distributions are done with atomic counters for speed, however there may be
/// some differences in practice from the described as atomic counters can be concurrently read.
#[derive(Clone, Copy)]
pub enum Strategy {
    ///  Tracks the number of executing tasks on a coroutine, picks worker with fewest tasks.
    LeastBusy,
    /// Randomly selects worker for each task.
    Random,
    /// Keeps a queue of workers. On each task, takes a worker from the queue, runs the
    /// task, then puts the worker at the back of the queue.
    RoundRobin,
}

/// Controls options for running puff tasks.
///
/// See the [Readme's Architecture Section](https://github.com/hansonkd/puff/blob/main/README.md#architecture)
/// for more information about the multi-threaded tokio runtime, coroutine threads, and blocking threads to
/// get a better understanding of these options. The default options should be sane for most applications.
///
/// # Examples
///
/// ```
/// use puff_rs::runtime::{RuntimeConfig, Strategy};
/// let config = RuntimeConfig::default().set_strategy(Strategy::Random);
/// ```
#[derive(Clone)]
pub struct RuntimeConfig {
    max_blocking_threads: usize,
    tokio_worker_threads: usize,
    python: bool,
    redis: bool,
    postgres: bool,
    pubsub: bool,
    greenlets: bool,
    env_vars: Vec<(Text, Text)>,
    python_paths: Vec<Text>,
    gql_module: Option<Text>,
    global_state_fn: Option<Arc<dyn Fn(Python) -> PyResult<PyObject> + Send + Sync + 'static>>,
    blocking_task_keep_alive: Duration,
    strategy: Strategy,
}

impl RuntimeConfig {
    /// Create a new default config.
    pub fn new() -> Self {
        Self::default()
    }
    /// Get the current strategy.
    ///
    /// See [Strategy] for more information about choosing a strategy.
    pub fn strategy(&self) -> Strategy {
        self.strategy
    }
    /// Get the current max_blocking_threads
    ///
    /// This is the number of threads that can be spawned for blocking tasks.
    pub fn max_blocking_threads(&self) -> usize {
        self.max_blocking_threads
    }
    /// Get the current tokio_worker_threads
    ///
    /// This is the number of threads that Tokio will use to execute things like DB queries,
    /// HTTP Servers, and IO. These threads can share and distribute tasks amongst themselves.
    pub fn tokio_worker_threads(&self) -> usize {
        self.tokio_worker_threads
    }
    /// Get the current blocking_task_keep_alive
    ///
    /// The maximum time a blocking thread can keep executing before being killed.
    pub fn blocking_task_keep_alive(&self) -> Duration {
        self.blocking_task_keep_alive
    }
    pub fn python(&self) -> bool {
        self.python
    }
    /// Get if a global redis will be enabled.
    pub fn redis(&self) -> bool {
        self.redis
    }
    /// Get if a global postgres will be enabled.
    pub fn postgres(&self) -> bool {
        self.postgres
    }
    /// Get if a global pubsub will be enabled.
    pub fn pubsub(&self) -> bool {
        self.pubsub
    }
    /// Get if greenlets will be enabled.
    pub fn greenlets(&self) -> bool {
        self.greenlets
    }
    /// Get the gql module that will be enabled.
    pub fn gql_module(&self) -> Option<Text> {
        self.gql_module.puff()
    }
    /// Get all python paths to add to environment.
    pub fn python_paths(&self) -> Vec<Text> {
        self.python_paths.clone()
    }
    /// Get all env vars to add to environment.
    pub fn env_vars(&self) -> Vec<(Text, Text)> {
        self.env_vars.clone()
    }
    /// Apply env vars to add to environment.
    pub fn apply_env_vars(&self) -> () {
        for (k, v) in &self.env_vars {
            std::env::set_var(k.as_str(), v.as_str())
        }
    }

    /// Get the global Python object
    pub fn global_state(&self) -> PyResult<PyObject> {
        Python::with_gil(|py| match self.global_state_fn.clone() {
            Some(r) => r(py),
            None => Ok(py.None()),
        })
    }

    /// Set the tokio_worker_threads for coroutines.
    ///
    /// Default: num_cpus
    pub fn set_tokio_worker_threads(self, tokio_worker_threads: usize) -> Self {
        let mut new = self;
        new.tokio_worker_threads = tokio_worker_threads;
        new
    }

    /// Set the blocking_task_keep_alive.
    ///
    /// Default: 30 seconds
    pub fn set_blocking_task_keep_alive(self, blocking_task_keep_alive: Duration) -> Self {
        let mut new = self;
        new.blocking_task_keep_alive = blocking_task_keep_alive;
        new
    }

    /// Set the max_blocking_threads.
    ///
    /// Default: 1024
    pub fn set_max_blocking_threads(self, max_blocking_threads: usize) -> Self {
        let mut new = self;
        new.max_blocking_threads = max_blocking_threads;
        new
    }

    /// Sets the strategy for distributing tasks onto a coroutine thread.
    ///
    /// Default: [Strategy::RoundRobin]
    pub fn set_strategy(self, strategy: Strategy) -> Self {
        let mut new = self;
        new.strategy = strategy;
        new
    }

    /// Sets whether to start with python.
    ///
    /// Default: true
    pub fn set_python(self, python: bool) -> Self {
        let mut new = self;
        new.python = python;
        new
    }

    /// Sets whether to start with a global Redis pool.
    ///
    /// Default: false
    pub fn set_redis(self, redis: bool) -> Self {
        let mut new = self;
        new.redis = redis;
        new
    }

    /// Sets whether to start with a global Postgres pool.
    ///
    /// Default: false
    pub fn set_postgres(self, postgres: bool) -> Self {
        let mut new = self;
        new.postgres = postgres;
        new
    }

    /// Sets whether to start with a global PubSubClient.
    ///
    /// Default: false
    pub fn set_pubsub(self, pubsub: bool) -> Self {
        let mut new = self;
        new.pubsub = pubsub;
        new
    }

    /// Sets whether to use greenlets when executing python.
    ///
    /// Default: true
    pub fn set_greenlets(self, greenlets: bool) -> Self {
        let mut new = self;
        new.greenlets = greenlets;
        new
    }

    /// If provided, will load the GraphQl configuration from the module path to the Schema.
    ///
    /// Default: None
    pub fn set_gql_schema_class<T: Into<Text>>(self, schema_module_path: T) -> Self {
        let mut new = self;
        new.gql_module = Some(schema_module_path.into());
        new
    }

    /// Run a function in a python context and set the result as the global state for Python.
    ///
    /// Default: None
    pub fn set_global_state_fn<F: Fn(Python) -> PyResult<PyObject> + Send + Sync + 'static>(
        self,
        f: F,
    ) -> Self {
        let mut new = self;
        new.global_state_fn = Some(Arc::new(f));
        new
    }
    /// Add the current directory to the PYTHONPATH
    pub fn add_env<K: Into<Text>, V: Into<Text>>(self, k: K, v: V) -> Self {
        let mut new = self;
        new.env_vars.push((k.into(), v.into()));
        new
    }

    /// Add the current directory to the PYTHONPATH
    pub fn add_cwd_to_python_path(self) -> Self {
        let cwd = std::env::current_dir().expect("Could not read Current Working Directory");
        let mut new = self;
        new.python_paths.push(
            cwd.to_str()
                .expect("Could not convert path to string")
                .to_text(),
        );
        new
    }

    /// Add the relative directory to the PYTHONPATH
    pub fn add_python_path<T: Into<Text>>(self, path: T) -> Self {
        // let cwd_str = cwd.to_str().expect("Could not convert cwd path to string");
        let p: PathBuf = path
            .into()
            .parse()
            .expect("Could not convert string to path");
        let path_to_add = if p.is_relative() {
            let cwd = std::env::current_dir()
                .expect("Could not read Current Working Directory and path is relative.");
            cwd.join(p)
        } else {
            p
        };
        let path_text = path_to_add
            .to_str()
            .expect("Could not convert path into a string.");
        let mut new = self;
        new.python_paths.push(path_text.into());
        new
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            max_blocking_threads: 1024,
            tokio_worker_threads: num_cpus::get(),
            python: true,
            global_state_fn: None,
            greenlets: true,
            redis: false,
            postgres: false,
            pubsub: false,
            blocking_task_keep_alive: Duration::from_secs(30),
            strategy: Strategy::RoundRobin,
            python_paths: Vec::new(),
            env_vars: Vec::new(),
            gql_module: None,
        }
    }
}
