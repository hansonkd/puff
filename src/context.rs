use crate::databases::redis::RedisClient;
use crate::errors::{handle_puff_result, PuffResult};

use crate::tasks::TaskQueue;
use crate::types::{Puff, Text};
use crate::web::client::PyHttpClient;
use futures_util::future::BoxFuture;

use std::cell::RefCell;

use crate::databases::postgres::PostgresClient;
use crate::databases::pubsub::PubSubClient;
use crate::graphql::PuffGraphqlRoot;
use crate::python::PythonDispatcher;
use std::sync::{Arc, Mutex};
use tokio::runtime::{Handle, Runtime};

use tracing::{error, info};

/// The central control structure for dispatching tasks onto coroutine workers.
/// All tasks in the same runtime will have access to the same dispatcher. The dispatcher contains
/// a reference to the parent Tokio Runtime as well as references to all coroutine workers.
#[derive(Clone)]
pub struct PuffContext {
    handle: Handle,
    http_client: Option<PyHttpClient>,
    redis: Option<RedisClient>,
    postgres: Option<PostgresClient>,
    python_dispatcher: Option<PythonDispatcher>,
    pubsub_client: Option<PubSubClient>,
    task_queue_client: Option<TaskQueue>,
    gql_root: Option<PuffGraphqlRoot>,
}

// Context consists of a hierarchy of Contexts. PUFF_CONTEXT is the primary thread local that holds
// the current context. PUFF_CONTEXT_WAITING is a Mutex used to bootstrap the variable into a tokio
// thread. Since certain parts of the context require the tokio runtime, we have to start threads
// with no PUFF_CONTEXT set, build the PuffContext, and then set the PUFF_CONTEXT_WAITING mutex.
// If PUFF_CONTEXT is not set when trying to access it, it will look at the PUFF_CONTEXT_WAITING
// and set PUFF_CONTEXT for the thread. This allows us to lazily set the context while avoiding
// a mutex lock every time trying to access it.
thread_local! {
    pub static PUFF_CONTEXT_WAITING: RefCell<Option<Arc<Mutex<Option<PuffContext>>>>> = RefCell::new(None);
    pub static PUFF_CONTEXT: RefCell<Option<PuffContext >> = RefCell::new(None);
}

pub fn set_puff_context(context: PuffContext) {
    PUFF_CONTEXT.with(|d| *d.borrow_mut() = Some(context.puff()));
}

pub fn set_puff_context_waiting(context: Arc<Mutex<Option<PuffContext>>>) {
    PUFF_CONTEXT_WAITING.with(|d| *d.borrow_mut() = Some(context.clone()));
}

pub fn with_puff_context<F: FnOnce(PuffContext) -> R, R>(f: F) -> R {
    let maybe_context = PUFF_CONTEXT.with(|d| d.borrow().clone());

    let context = match maybe_context {
        Some(v) => v,
        None => PUFF_CONTEXT_WAITING.with(|w| match w.borrow().clone() {
            Some(r) => {
                let locked = r.lock().unwrap();
                match &*locked {
                    Some(v) => {
                        set_puff_context(v.puff());
                        v.puff()
                    }
                    None => {
                        panic!("Accessed puff context before context was set.")
                    }
                }
            }
            None => {
                panic!("Context can only be used from a puff context.")
            }
        }),
    };
    f(context)
}

pub fn with_context(context: PuffContext) {
    PUFF_CONTEXT.with(|d| *d.borrow_mut() = Some(context.puff()));
}

impl PuffContext {
    /// Creates an empty RuntimeDispatcher with no active threads for testing.
    pub fn empty(handle: Handle) -> PuffContext {
        Self {
            handle,
            redis: None,
            postgres: None,
            python_dispatcher: None,
            pubsub_client: None,
            task_queue_client: None,
            gql_root: None,
            http_client: None,
        }
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`.
    pub fn new(handle: Handle) -> PuffContext {
        Self::new_with_options(handle, None, None, None, None, None, None, None)
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config. Includes options.
    pub fn new_with_options(
        handle: Handle,
        redis: Option<RedisClient>,
        postgres: Option<PostgresClient>,
        python_dispatcher: Option<PythonDispatcher>,
        pubsub_client: Option<PubSubClient>,
        task_queue_client: Option<TaskQueue>,
        gql_root: Option<PuffGraphqlRoot>,
        http_client: Option<PyHttpClient>,
    ) -> PuffContext {
        let arc_dispatcher = Self {
            handle,
            redis,
            postgres,
            python_dispatcher,
            pubsub_client,
            task_queue_client,
            gql_root,
            http_client,
        };

        arc_dispatcher
    }

    /// A Handle into the multi-threaded async runtime
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// The global configured PubSubClient
    pub fn pubsub(&self) -> PubSubClient {
        self.pubsub_client
            .clone()
            .expect("PubSub is not configured for this runtime.")
    }

    /// The global configured reqwest::Client
    pub fn http_client(&self) -> PyHttpClient {
        self.http_client
            .clone()
            .expect("HttpClient is not configured for this runtime.")
    }

    /// The global configured PubSubClient
    pub fn task_queue(&self) -> TaskQueue {
        self.task_queue_client
            .clone()
            .expect("TaskQueue is not configured for this runtime.")
    }

    /// A Handle into the multi-threaded async runtime
    pub fn python_dispatcher(&self) -> PythonDispatcher {
        self.python_dispatcher
            .clone()
            .expect("Python is not configured for this runtime.")
    }

    /// The configured redis client. Panics if not enabled.
    pub fn redis(&self) -> RedisClient {
        self.redis
            .clone()
            .expect("Redis is not configured for this runtime.")
    }

    /// The configured postgres client. Panics if not enabled.
    pub fn postgres(&self) -> PostgresClient {
        self.postgres
            .clone()
            .expect("Postgres is not configured for this runtime.")
    }

    /// The configured graphql root node. Panics if not enabled.
    pub fn gql(&self) -> PuffGraphqlRoot {
        self.gql_root
            .clone()
            .expect("Postgres is not configured for this runtime.")
    }
}

impl Default for PuffContext {
    fn default() -> Self {
        let rt = Runtime::new().unwrap();
        let ctx = PuffContext::new(rt.handle().clone());
        ctx
    }
}

impl Puff for PuffContext {}

pub fn supervised_task<F: Fn() -> BoxFuture<'static, PuffResult<()>> + Send + Sync + 'static>(
    context: PuffContext,
    _task_name: Text,
    f: F,
) {
    let handle = context.handle();
    let inner_handle = handle.clone();
    handle.spawn(async move {
        loop {
            info!("Starting task {_task_name}");
            let result = inner_handle.spawn(f()).await;
            match result {
                Ok(r) => {
                    let label = format!("Task {}", _task_name);
                    handle_puff_result(label.as_str(), r)
                }
                Err(_e) => {
                    error!("Task {_task_name} unexpected error : {_e}")
                }
            }
        }
    });
}
