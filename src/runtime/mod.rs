//! Types used to interact with the Puff Runtime
//!

use pyo3::prelude::PyObject;
use pyo3::{PyResult, Python};
use std::collections::HashMap;

use reqwest::header::HeaderValue;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::types::text::ToText;
use crate::types::Text;

#[derive(Clone)]
pub struct PostgresOpts {
    pub pool_size: u32,
}

impl PostgresOpts {
    pub fn set_pool_size(mut self, size: u32) -> Self {
        self.pool_size = size;
        self
    }
}

impl Default for PostgresOpts {
    fn default() -> Self {
        Self { pool_size: 10 }
    }
}

#[derive(Clone)]
pub struct RedisOpts {
    pub pool_size: u32,
}

impl RedisOpts {
    pub fn set_pool_size(mut self, size: u32) -> Self {
        self.pool_size = size;
        self
    }
}

impl Default for RedisOpts {
    fn default() -> Self {
        Self { pool_size: 10 }
    }
}

#[derive(Clone)]
pub struct PubSubOpts {
    pub pool_size: u32,
}

impl PubSubOpts {
    pub fn set_pool_size(mut self, size: u32) -> Self {
        self.pool_size = size;
        self
    }
}

impl Default for PubSubOpts {
    fn default() -> Self {
        Self { pool_size: 10 }
    }
}

#[derive(Clone)]
pub struct TaskQueueOpts {
    pub pool_size: u32,
    pub max_concurrent_tasks: u32,
}

impl TaskQueueOpts {
    pub fn set_pool_size(mut self, size: u32) -> Self {
        self.pool_size = size;
        self
    }

    pub fn set_max_concurrent_tasks(mut self, max_streams: u32) -> Self {
        self.max_concurrent_tasks = max_streams;
        self
    }
}

impl Default for TaskQueueOpts {
    fn default() -> Self {
        Self {
            pool_size: 10,
            max_concurrent_tasks: (num_cpus::get() * 4) as u32,
        }
    }
}

#[derive(Clone)]
pub struct GqlOpts {
    pub schema_import: Text,
    pub db: Option<Text>,
}

impl GqlOpts {
    pub fn new<T: Into<Text>>(schema_import: T, db: Option<Text>) -> Self {
        Self {
            schema_import: schema_import.into(),
            db,
        }
    }
}

#[derive(Clone, Default)]
pub struct HttpClientOpts {
    pub http2_prior_knowledge: Option<bool>,
    pub max_idle_connections: Option<u32>,
    pub user_agent: Option<HeaderValue>,
}

impl HttpClientOpts {
    pub fn set_http2_prior_knowledge(mut self, val: bool) -> Self {
        self.http2_prior_knowledge = Some(val);
        self
    }

    pub fn set_max_idle_connections(mut self, val: u32) -> Self {
        self.max_idle_connections = Some(val);
        self
    }

    pub fn set_user_agent(mut self, val: HeaderValue) -> Self {
        self.user_agent = Some(val);
        self
    }
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
/// use puff_rs::runtime::RuntimeConfig;
/// let config = RuntimeConfig::default();
/// ```
#[derive(Clone)]
pub struct RuntimeConfig {
    max_blocking_threads: usize,
    tokio_worker_threads: usize,
    python: bool,
    redis: HashMap<Text, RedisOpts>,
    postgres: HashMap<Text, PostgresOpts>,
    pubsub: HashMap<Text, PubSubOpts>,
    http_client_builder: HashMap<Text, HttpClientOpts>,
    task_queue: HashMap<Text, TaskQueueOpts>,
    greenlets: bool,
    asyncio: bool,
    env_vars: Vec<(Text, Text)>,
    python_paths: Vec<Text>,
    gql_modules: HashMap<Text, GqlOpts>,
    global_state_fn: Option<Arc<dyn Fn(Python) -> PyResult<PyObject> + Send + Sync + 'static>>,
    blocking_task_keep_alive: Duration,
}

impl RuntimeConfig {
    /// Create a new default config.
    pub fn new() -> Self {
        Self::default()
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
    pub fn redis(&self) -> &HashMap<Text, RedisOpts> {
        &self.redis
    }
    /// Get if a global postgres will be enabled.
    pub fn postgres(&self) -> &HashMap<Text, PostgresOpts> {
        &self.postgres
    }

    /// Get if a global pubsub will be enabled.
    pub fn pubsub(&self) -> &HashMap<Text, PubSubOpts> {
        &self.pubsub
    }

    /// Get if a global pubsub will be enabled.
    pub fn http_client_builder(&self) -> &HashMap<Text, HttpClientOpts> {
        &self.http_client_builder
    }

    /// Get if a global task_queue will be enabled.
    pub fn task_queue(&self) -> &HashMap<Text, TaskQueueOpts> {
        &self.task_queue
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
    pub fn gql_modules(&self) -> &HashMap<Text, GqlOpts> {
        &self.gql_modules
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

    /// Sets whether to start with python.
    ///
    /// Default: true
    pub fn set_python(self, python: bool) -> Self {
        let mut new = self;
        new.python = python;
        new
    }

    /// Sets whether to start with a global Redis pool.
    pub fn add_default_redis(self) -> Self {
        let mut new = self;
        new.redis.insert("default".into(), RedisOpts::default());
        new
    }

    /// Configure an additional named redis pool.
    pub fn add_named_redis<N: Into<Text>>(self, name: N, opts: RedisOpts) -> Self {
        let mut new = self;
        new.redis.insert(name.into(), opts);
        new
    }

    /// Sets whether to start with a global Postgres pool.
    pub fn add_default_postgres(self) -> Self {
        let mut new = self;
        new.postgres
            .insert("default".into(), PostgresOpts::default());
        new
    }

    /// Configure an additional named Postgres pool.
    pub fn add_named_postgres<N: Into<Text>>(self, name: N, opts: PostgresOpts) -> Self {
        let mut new = self;
        new.postgres.insert(name.into(), opts);
        new
    }

    /// Sets whether to start with a global PubSubClient.
    pub fn add_default_pubsub(self) -> Self {
        let mut new = self;
        new.pubsub.insert("default".into(), PubSubOpts::default());
        new
    }

    /// Sets whether to start with a global PubSubClient.
    pub fn add_named_pubsub<N: Into<Text>>(self, name: N, config: PubSubOpts) -> Self {
        let mut new = self;
        new.pubsub.insert(name.into(), config);
        new
    }

    /// Add global HTTP Client
    pub fn add_http_client(self) -> Self {
        let mut new = self;
        new.http_client_builder
            .insert("default".into(), HttpClientOpts::default());
        new
    }

    /// Add named HTTP Client
    pub fn add_named_http_client<N: Into<Text>>(self, name: N, opts: HttpClientOpts) -> Self {
        let mut new = self;
        new.http_client_builder.insert(name.into(), opts);
        new
    }

    /// Sets whether to start with a global TaskQueue.
    pub fn add_default_task_queue(self) -> Self {
        let mut new = self;
        new.task_queue
            .insert("default".into(), TaskQueueOpts::default());
        new
    }

    /// Sets whether to start with a global TaskQueue.
    pub fn add_named_task_queue<N: Into<Text>>(self, name: N, opts: TaskQueueOpts) -> Self {
        let mut new = self;
        new.task_queue.insert(name.into(), opts);
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
    pub fn add_gql_schema<T: Into<Text>>(self, schema_module_path: T) -> Self {
        let mut new = self;
        new.gql_modules.insert(
            "default".into(),
            GqlOpts {
                schema_import: schema_module_path.into(),
                db: None,
            },
        );
        new
    }

    /// If provided, will load an additional GraphQl configuration from the path to the Schema.
    ///
    /// Default: None
    pub fn add_gql_schema_named<N: Into<Text>>(self, name: N, opts: GqlOpts) -> Self {
        let mut new = self;
        new.gql_modules.insert(name.into(), opts);
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
            redis: HashMap::new(),
            postgres: HashMap::new(),
            pubsub: HashMap::new(),
            task_queue: HashMap::new(),
            blocking_task_keep_alive: Duration::from_secs(30),
            python_paths: Vec::new(),
            env_vars: Vec::new(),
            gql_modules: HashMap::new(),
            http_client_builder: HashMap::new(),
        }
        .add_http_client()
    }
}
