use crate::databases::redis::RedisClient;
use crate::errors::{Error, PuffResult};
use crate::runtime::dispatcher::Dispatcher;

use crate::runtime::{run_with_config_on_local, RuntimeConfig};
use crate::types::{Puff, Text};
use futures_util::future::BoxFuture;
use futures_util::FutureExt;

use std::cell::RefCell;

use crate::databases::postgres::PostgresClient;
use crate::databases::pubsub::PubSubClient;
use crate::python::PythonDispatcher;
use futures_util::task::SpawnExt;
use std::sync::{Arc, Mutex};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{broadcast, oneshot};
use tokio::task::LocalSet;
use tracing::{error, info};

/// The central control structure for dispatching tasks onto coroutine workers.
/// All tasks in the same runtime will have access to the same dispatcher. The dispatcher contains
/// a reference to the parent Tokio Runtime as well as references to all coroutine workers.
#[derive(Clone)]
pub struct PuffContext {
    dispatcher: Arc<Dispatcher>,
    handle: Handle,
    redis: Option<RedisClient>,
    postgres: Option<PostgresClient>,
    python_dispatcher: Option<PythonDispatcher>,
    pubsub_client: Option<PubSubClient>,
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
        let (notify_shutdown, _) = broadcast::channel(1);
        Self {
            handle,
            dispatcher: Arc::new(Dispatcher::empty(notify_shutdown)),
            redis: None,
            postgres: None,
            python_dispatcher: None,
            pubsub_client: None,
        }
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config.
    pub fn new(dispatcher: Arc<Dispatcher>, handle: Handle) -> PuffContext {
        Self::new_with_options(handle, dispatcher, None, None, None, None)
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config. Includes options.
    pub fn new_with_options(
        handle: Handle,
        dispatcher: Arc<Dispatcher>,
        redis: Option<RedisClient>,
        postgres: Option<PostgresClient>,
        python_dispatcher: Option<PythonDispatcher>,
        pubsub_client: Option<PubSubClient>,
    ) -> PuffContext {
        let arc_dispatcher = Self {
            dispatcher,
            handle,
            redis,
            postgres,
            python_dispatcher,
            pubsub_client,
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

    /// The coroutine dispatcher.
    pub fn dispatcher(&self) -> Arc<Dispatcher> {
        self.dispatcher.clone()
    }

    /// Dispatch a task onto a blocking thread. This must be used if you use any type of non-async
    /// blocking functions (like from the std-lib). This is also useful if you have a task that takes
    /// a lot of computational power.
    ///
    /// Tokio will create up to `max_blocking_threads` as specified in [crate::runtime::RuntimeConfig], after
    /// which it will start to queue tasks until previous tasks finish. Each task will execute up
    /// to the duration specified in `blocking_task_keep_alive`.
    pub fn dispatch_blocking<F, R>(&self, f: F) -> BoxFuture<'static, Result<R, Error>>
    where
        F: FnOnce() -> Result<R, Error> + Send + 'static,
        R: Send + 'static,
    {
        let (new_sender, rec) = oneshot::channel();
        let dispatcher = self.clone();
        let future = self.handle.spawn_blocking(|| {
            let handle = Handle::current();
            let local = LocalSet::new();
            let h = local.spawn_local(run_with_config_on_local(dispatcher, new_sender, f));
            handle.block_on(local);
            handle.block_on(h)
        });
        let f = async {
            future.await???;
            Ok(rec.await??)
        };
        f.boxed()
    }
}

impl Default for PuffContext {
    fn default() -> Self {
        let rt = Runtime::new().unwrap();
        let (notify_shutdown, _) = broadcast::channel(1);
        let (dispatcher, waiting) = Dispatcher::new(notify_shutdown, RuntimeConfig::default());
        let ctx = PuffContext::new(Arc::new(dispatcher), rt.handle().clone());
        for w in waiting {
            w.send(ctx.clone()).unwrap_or(());
        }
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
                Ok(r) => match r {
                    Ok(_r) => {
                        info!("Task completed.")
                    }
                    Err(_e) => {
                        error!("Task {_task_name} error {_task_name}: {_e}")
                    }
                },
                Err(_e) => {
                    error!("Task {_task_name} unexpected error : {_e}")
                }
            }
        }
    });
}
