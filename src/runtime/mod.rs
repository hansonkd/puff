//! Types used to interact with the Puff Runtime
//!

use std::collections::HashMap;
use pyo3::prelude::PyObject;
use pyo3::{PyResult, Python};
use reqwest::ClientBuilder;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::types::text::ToText;
use crate::types::Text;

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
    redis_pool_size: u32,
    postgres_pool_size: u32,
    postgres: bool,
    pubsub: bool,
    http_client_builder: Arc<dyn Fn() -> ClientBuilder>,
    task_queue: bool,
    task_queue_max_concurrent_tasks: usize,
    greenlets: bool,
    asyncio: bool,
    env_vars: Vec<(Text, Text)>,
    python_paths: Vec<Text>,
    gql_modules: HashMap<Text, Text>,
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
    /// Get if a global redis will be enabled.
    pub fn redis_pool_size(&self) -> u32 {
        self.redis_pool_size
    }
    /// Get if a global postgres will be enabled.
    pub fn postgres_pool_size(&self) -> u32 {
        self.postgres_pool_size
    }
    /// Get if a global pubsub will be enabled.
    pub fn pubsub(&self) -> bool {
        self.pubsub
    }

    /// Get if a global pubsub will be enabled.
    pub fn http_client_builder(&self) -> ClientBuilder {
        (self.http_client_builder)()
    }

    /// Get if a global task_queue will be enabled.
    pub fn task_queue(&self) -> bool {
        self.task_queue
    }

    /// Get maximum number of concurrent tasks to pull from queue.
    pub fn task_queue_max_concurrent_tasks(&self) -> usize {
        self.task_queue_max_concurrent_tasks
    }
    /// Get if greenlets will be enabled.
    pub fn greenlets(&self) -> bool {
        self.greenlets
    }
    /// Get if asyncio will be enabled.
    pub fn asyncio(&self) -> bool {
        self.asyncio
    }
    /// Get the gql modules that will be enabled.
    pub fn gql_modules(&self) -> HashMap<Text, Text> {
        self.gql_modules.clone()
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

    /// Sets the max size of postgres pool. Also enables postgres if not enabled already.
    ///
    /// Default: 10
    pub fn set_postgres_pool_size(self, pool_size: u32) -> Self {
        let mut new = self;
        new.postgres = true;
        new.postgres_pool_size = pool_size;
        new
    }

    /// Sets the max size of redis pool. Also enables redis if not enabled already.
    ///
    /// Default: 10
    pub fn set_redis_pool_size(self, pool_size: u32) -> Self {
        let mut new = self;
        new.redis = true;
        new.redis_pool_size = pool_size;
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

    /// Set a constructor function for the HTTP ClientBuilder
    ///
    /// Default: ClientBuilder::new
    pub fn set_http_client_builder_fn<F: Fn() -> ClientBuilder + 'static>(
        self,
        builder: F,
    ) -> Self {
        let mut new = self;
        new.http_client_builder = Arc::new(builder);
        new
    }

    /// Sets whether to start with a global TaskQueue.
    ///
    /// Default: false
    pub fn set_task_queue(self, task_queue: bool) -> Self {
        let mut new = self;
        new.task_queue = task_queue;
        new
    }

    /// Sets the number of tasks to run concurrently from the queue. Sets `task_queue` to true.
    ///
    /// Default: num_cpu * 4
    pub fn set_task_queue_concurrent_tasks(self, max_workers: usize) -> Self {
        let mut new = self;
        new = new.set_task_queue(true);
        new.task_queue_max_concurrent_tasks = max_workers;
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

    /// Sets whether to use greenlets when executing python.
    ///
    /// Default: false
    pub fn set_asyncio(self, asyncio: bool) -> Self {
        let mut new = self;
        new.asyncio = asyncio;
        new
    }

    /// If provided, will load the GraphQl configuration from the module path to the Schema.
    ///
    /// Default: None
    pub fn set_gql_schema<T: Into<Text>>(self, schema_module_path: T) -> Self {
        let mut new = self;
        new.gql_modules.insert("default".into(), schema_module_path.into());
        new
    }

    /// If provided, will load an additional GraphQl configuration from the path to the Schema.
    ///
    /// Default: None
    pub fn set_gql_internal_schema_named<N: Into<Text>, T: Into<Text>>(self, name: N, schema_module_path: T) -> Self {
        let mut new = self;
        new.gql_modules.insert(name.into(), schema_module_path.into());
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
            asyncio: false,
            redis: false,
            redis_pool_size: 10,
            postgres_pool_size: 10,
            postgres: false,
            pubsub: false,
            task_queue: false,
            task_queue_max_concurrent_tasks: num_cpus::get() * 4,
            blocking_task_keep_alive: Duration::from_secs(30),
            strategy: Strategy::RoundRobin,
            python_paths: Vec::new(),
            env_vars: Vec::new(),
            gql_modules: HashMap::new(),
            http_client_builder: Arc::new(ClientBuilder::new),
        }
    }
}
