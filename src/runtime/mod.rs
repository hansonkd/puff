//! Types used to interact with the Puff Runtime
//!
use crate::runtime::wormhole::{AsyncWormhole, AsyncYielder};
use corosensei::stack::DefaultStack;

use std::future::Future;
use std::time::Duration;
use tokio::sync::oneshot;

use crate::errors::Result;
use crate::tasks::DISPATCHER;
use crate::types::Puff;
use dispatcher::RuntimeDispatcher;

pub mod dispatcher;
mod runner;
mod wormhole;

type PuffWormhole = AsyncWormhole<'static, DefaultStack, ()>;

tokio::task_local! {
    static YIELDER: *mut AsyncYielder<'static, ()>;
}

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
    let m: *mut AsyncYielder<'static, ()> = YIELDER.get();
    match unsafe { m.as_mut() } {
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
    let handle = DISPATCHER.with(|v| v.handle());
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
    let dispatcher = DISPATCHER.with(|v| v.clone());

    Ok(
        tokio::task::spawn_local(PuffWormhole::new(s, move |mut yielder| {
            let inner =
                (&yielder as *const AsyncYielder<()>) as usize as *mut AsyncYielder<'static, ()>;
            let r = yielder
                .async_suspend(DISPATCHER.scope(dispatcher, YIELDER.scope(inner, async { f() })));

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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            stack_size: 1024 * 1024,
            max_blocking_threads: 1024,
            tokio_worker_threads: num_cpus::get(),
            coroutine_threads: num_cpus::get(),
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
    run_local_wormhole_with_stack(stack, sender, f).await
}

pub(crate) async fn run_with_config_on_local<F, R>(
    dispatcher: RuntimeDispatcher,
    sender: oneshot::Sender<Result<R>>,
    f: F,
) -> Result<()>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: 'static + Send,
{
    let stack_size = dispatcher.stack_size();
    DISPATCHER
        .scope(dispatcher, run_new_stack(stack_size, sender, f))
        .await
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
    let dispatcher = RuntimeDispatcher::new(config, rt.handle().clone());
    rt.block_on(DISPATCHER.scope(dispatcher.puff(), dispatcher.dispatch(f)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use futures::future::join_all;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    async fn run<F, R>(f: F) -> Result<R>
    where
        F: FnOnce() -> Result<R> + 'static + Send,
        R: 'static + Send,
    {
        let dispatcher = DISPATCHER.with(|v| v.clone());
        let local = tokio::task::LocalSet::new();
        let (sender, recv) = oneshot::channel();
        local
            .run_until(DISPATCHER.scope(dispatcher, run_new_stack(1024 * 1024, sender, f)))
            .await?;
        recv.await
            .map_err(|_| anyhow!("Could not receive result for closure"))?
    }

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

    #[test]
    fn check_wormhole_tasks_many_wormholes_dont_block() {
        // Run a sequence of futures. The futures should resolve in reverse order.
        const NUMBER_TASKS: usize = 50;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let config = RuntimeConfig::default();
        let dispatcher = RuntimeDispatcher::new(config, rt.handle().clone());

        rt.block_on(DISPATCHER.scope(dispatcher, async {
            let mut tasks = Vec::new();
            let mut expected = Vec::new();
            let results = Arc::new(Mutex::new(Vec::new()));
            for xx in 0..NUMBER_TASKS {
                let results = results.clone();
                let task = run(move || {
                    let r = yield_to_future(async move {
                        tokio::time::sleep(Duration::from_millis(10 * (NUMBER_TASKS - xx) as u64))
                            .await;
                        results.lock().await.push(xx);
                        xx
                    });
                    assert_eq!(r, xx);
                    Ok(())
                });
                tasks.push(task);
                expected.push(xx);
            }

            expected.reverse();

            join_all(tasks).await;

            assert_eq!(*results.lock().await, expected);
        }));
    }
}
