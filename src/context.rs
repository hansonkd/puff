use std::sync::{Arc, Mutex};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{broadcast, oneshot};
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use pyo3::{Py, PyAny};
use futures_util::future::BoxFuture;
use tokio::task::LocalSet;
use futures_util::FutureExt;
use std::cell::RefCell;
use crate::databases::redis::RedisClient;
use crate::errors::Error;
use crate::runtime::dispatcher::{Dispatcher, Stats};
use crate::runtime::{run_with_config_on_local, RuntimeConfig, Strategy};
use crate::runtime::runner::LocalSpawner;
use crate::runtime::shutdown::Shutdown;
use crate::types::Puff;

/// The central control structure for dispatching tasks onto coroutine workers.
/// All tasks in the same runtime will have access to the same dispatcher. The dispatcher contains
/// a reference to the parent Tokio Runtime as well as references to all coroutine workers.
#[derive(Clone)]
pub struct PuffContext {
    dispatcher: Arc<Dispatcher>,
    handle: Handle,
    redis: Option<RedisClient>
}

thread_local! {
    pub static PUFF_CONTEXT_WAITING: RefCell<Option<Arc<Mutex<Option<PuffContext>>>>> = RefCell::new(None);
    pub static PUFF_CONTEXT: RefCell<Option<PuffContext >> = RefCell::new(None);
}

pub fn set_puff_context(context: PuffContext) {
    PUFF_CONTEXT.with(|mut d| {
        *d.borrow_mut() = Some(context.puff())
    });
}

pub fn set_puff_context_waiting(context: Arc<Mutex<Option<PuffContext>>>) {
    PUFF_CONTEXT_WAITING.with(|mut d| {
        *d.borrow_mut() = Some(context.clone())
    });
}

pub fn with_puff_context<F: FnOnce(PuffContext) -> R, R>(f: F) -> R {
    let dispatcher = PUFF_CONTEXT.with(|d| {
        match d.borrow().clone() {
            Some(v) => v,
            None => PUFF_CONTEXT_WAITING.with(|w| {
                match w.borrow().clone() {
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
                    },
                    None => {
                        panic!("Dispatcher can only be used from a puff context.")
                    }
                }
            })
        }
    });
    f(dispatcher)
}

pub fn with_context(context: PuffContext) {
    PUFF_CONTEXT.with(|mut d| {
        *d.borrow_mut() = Some(context.puff())
    });
}

impl PuffContext {
    /// Creates an empty RuntimeDispatcher with no active threads for testing.
    pub fn empty(handle: Handle) -> PuffContext {
        let (notify_shutdown, _) = broadcast::channel(1);
        Self{
            dispatcher: Arc::new(Dispatcher::empty(notify_shutdown)),
            handle: handle,
            redis: None
        }
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config.
    pub fn new(dispatcher: Arc<Dispatcher>, handle: Handle) -> PuffContext {
        Self::new_with_options(handle, dispatcher,None)
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config. Includes options.
    pub fn new_with_options(handle: Handle, dispatcher: Arc<Dispatcher>, redis: Option<RedisClient>) -> PuffContext {
        let arc_dispatcher = Self{dispatcher, handle, redis};

        arc_dispatcher
    }

    // #[inline]
    // pub(crate) fn clone_for_new_task(&self) -> Self { RuntimeDispatcher(Arc::new(self.0.clone_for_new_task())) }

    /// A Handle into the multi-threaded async runtime
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// The configured redis client. Panics if not enabled.
    pub fn redis(&self) -> RedisClient {
        self.redis.clone().expect("Redis is not configured for this runtime.")
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
        PuffContext::empty(rt.handle().clone())
    }
}

impl Puff for PuffContext {}


