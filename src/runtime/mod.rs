//! Types used to interact with the Puff Runtime
//!
use crate::runtime::wormhole::{AsyncWormhole, YIELDER};
use corosensei::stack::DefaultStack;

use crate::context;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

use crate::context::{PuffContext};
use crate::errors::Result;
use crate::runtime::dispatcher::Dispatcher;
use crate::types::Puff;

pub mod dispatcher;
pub mod runner;
pub mod shutdown;
mod wormhole;

type PuffWormhole = AsyncWormhole<'static, DefaultStack>;

/// Suspend execution of the current coroutine and run the future. This function MUST be called from
/// a Puff task. By default this function will run the future on the same single-threaded Tokio
/// runtime as the task's coroutine is executing on. To schedule futures on the more robust
/// multi-threaded runtime use [yield_to_future_io]
///
/// *Calling this function outside of a Puff task will cause a panic*
pub fn yield_to_future<Fut, R>(future: Fut) -> R
where
    Fut: Future<Output = R>,
{
    let m = YIELDER
        .try_with(|m| m.clone())
        .expect("async_suspend must be called from a Puff context");

    match unsafe {
        let x = m.borrow().as_mut();
        x
    } {
        Some(l) => l.async_suspend(future),
        None => panic!("async_suspend must be called from a Puff context"),
    }
}

/// Suspend execution of the current coroutine and run the future. This function MUST be called from
/// a Puff task. This function will run on the parent multi-threaded runtime.
///
/// *Calling this function outside of a Puff task will cause a panic*
pub fn yield_to_future_io<Fut, R>(future: Fut) -> Result<R>
where
    Fut: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let handle = context::with_puff_context(|v| v.handle());
    Ok(yield_to_future(handle.spawn(future))?)
}

pub(crate) async fn run_local_wormhole_with_stack<F, R>(
    s: DefaultStack,
    sender: oneshot::Sender<Result<R>>,
    f: F,
) -> Result<()>
where
    F: FnOnce() -> Result<R> + 'static + Send,
    R: 'static + Send,
{
    Ok(
        tokio::task::spawn_local(PuffWormhole::new(s, move || {
            // let r = DISPATCHER.sync_scope(dispatcher, || YIELDER.sync_scope(inner, f));
            let r = f();
            // If we can't send, the future wasn't awaited
            sender.send(r).unwrap_or(())
        })?)
        .await?,
    )
}

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
/// use puff::runtime::{RuntimeConfig, Strategy};
/// let config = RuntimeConfig::default().set_strategy(Strategy::Random);
/// ```
#[derive(Clone)]
pub struct RuntimeConfig {
    stack_size: usize,
    max_blocking_threads: usize,
    tokio_worker_threads: usize,
    coroutine_threads: usize,
    python: bool,
    asyncio: bool,
    redis: bool,
    blocking_task_keep_alive: Duration,
    strategy: Strategy,
}

impl RuntimeConfig {
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
    /// Get the current coroutine_threads
    ///
    /// The number of workers which will execute coroutines. One a coroutine is on a thread, it will
    /// not move. Each thread is a single-threaded Tokio runtime.
    pub fn coroutine_threads(&self) -> usize {
        self.coroutine_threads
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
    /// Get the current stack_size for coroutines.
    pub fn stack_size(&self) -> usize {
        self.stack_size
    }
    /// Get if python will be enabled.
    pub fn python(&self) -> bool {
        self.python
    }
    /// Get if asyncio will be enabled.
    pub fn asyncio(&self) -> bool {
        self.asyncio
    }
    /// Get if a global redis will be enabled.
    pub fn redis(&self) -> bool {
        self.redis
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

    /// Sets the stack_size for coroutines.
    ///
    /// Default: 1Mb
    pub fn set_stack_size(self, stack_size: usize) -> Self {
        let mut new = self;
        new.stack_size = stack_size;
        new
    }

    /// Sets coroutine_threads.
    ///
    /// Default: num_cpus
    pub fn set_coroutine_threads(self, coroutine_threads: usize) -> Self {
        let mut new = self;
        new.coroutine_threads = coroutine_threads;
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
    /// Default: true
    pub fn set_redis(self, redis: bool) -> Self {
        let mut new = self;
        new.redis = redis;
        new
    }

    // /// Sets whether to start with asyncio.
    // ///
    // /// Default: false
    // pub fn set_asyncio(self, async_io: bool) -> Self {
    //     let mut new = self;
    //     new.asyncio = async_io;
    //     new
    // }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            stack_size: 1024 * 1024,
            max_blocking_threads: 1024,
            tokio_worker_threads: num_cpus::get(),
            coroutine_threads: num_cpus::get(),
            python: true,
            asyncio: false,
            redis: false,
            blocking_task_keep_alive: Duration::from_secs(30),
            strategy: Strategy::RoundRobin,
        }
    }
}

async fn run_new_stack<F, R>(
    stack_size: usize,
    sender: oneshot::Sender<Result<R>>,
    f: F,
) -> Result<()>
where
    F: FnOnce() -> Result<R> + 'static + Send,
    R: 'static + Send,
{
    let stack = DefaultStack::new(stack_size)?;
    run_local_wormhole_with_stack( stack, sender, f).await
}

pub(crate) async fn run_with_config_on_local<F, R>(
    dispatcher: PuffContext,
    sender: oneshot::Sender<Result<R>>,
    f: F,
) -> Result<()>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: 'static + Send,
{
    let stack_size = dispatcher.dispatcher().stack_size();
    run_new_stack(stack_size, sender, f).await
}

/// Start a default single-threaded runtime and run a closure in a Puff context.
///
/// This should only be run outside of a puff context (for example in a test). The primary entrypoint
/// for Puff Tasks is [crate::program::Program].
pub fn start_runtime_and_run<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: 'static + Send,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let config = RuntimeConfig::default().set_coroutine_threads(1);
    let (notify_shutdown, _) = broadcast::channel(1);
    let (dispatcher, waiting) = Dispatcher::new(notify_shutdown, config);
    let context = PuffContext::new(Arc::new(dispatcher), rt.handle().clone());
    for wait in waiting {
        wait.send(context.puff()).unwrap_or(());
    }
    rt.block_on(context.dispatcher().dispatch(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    
    
    
    

    #[test]
    fn check_wormhole() {
        start_runtime_and_run(|| {
            let r = yield_to_future(async { 42 });
            assert_eq!(r, 42);
            Ok(())
        })
        .unwrap();
    }

    fn recurse(xx: usize) {
        let r = yield_to_future(async move {
            // tokio::time::sleep(Duration::from_millis(xx as u64)).await;
            xx - 1
        });

        if r > 0 {
            recurse(xx - 1)
        } else {
            ()
        }
    }

    #[test]
    fn check_recurse() {
        start_runtime_and_run(|| Ok(recurse(10000))).unwrap();
    }
}
