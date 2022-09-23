//! Distribute Tasks onto Coroutine workers and bootstrap them with the Puff Context.
//!
//! Every Puff Runtime has a `RuntimeDispatcher`. Calling the dispatcher produces a future that you
//! can then await on in an async context.
//!
//! The primary way to interact with the dispatcher is calling [crate::tasks::Task] or [crate::program::RunnableCommand]
//!
use std::cell::RefCell;
use std::collections::HashMap;
use futures::future::BoxFuture;
use futures_util::FutureExt;
use rand::seq::SliceRandom;
use rand::thread_rng;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use pyo3::{Py, PyAny};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::oneshot;
use tokio::task::LocalSet;
use tracing::info;
use crate::databases::redis::RedisClient;

use crate::errors::Error;
use crate::runtime::runner::LocalSpawner;
use crate::runtime::{run_with_config_on_local, RuntimeConfig, Strategy};
use crate::types::Puff;

/// Simple statistics about each worker.
#[derive(Clone, Debug)]
pub struct Stats {
    pub worker_id: usize,
    pub total_tasks: usize,
    pub total_tasks_completed: usize,
}

/// The central control structure for dispatching tasks onto coroutine workers.
/// All tasks in the same runtime will have access to the same dispatcher. The dispatcher contains
/// a reference to the parent Tokio Runtime as well as references to all coroutine workers.
#[derive(Clone)]
pub struct RuntimeDispatcher(Arc<Dispatcher>);

impl RuntimeDispatcher {
    /// Creates an empty RuntimeDispatcher with no active threads for testing.
    pub fn empty(handle: Handle) -> RuntimeDispatcher {
        Self(Arc::new(Dispatcher {
            handle,
            config: RuntimeConfig::default(),
            spawners: Vec::new(),
            strategy: Strategy::Random,
            redis: None,
            context_vars: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicUsize::new(0)),
        }))
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config.
    pub fn new(config: RuntimeConfig, handle: Handle) -> RuntimeDispatcher {
        Self::new_with_options(config, handle, None)
    }

    /// Creates a new RuntimeDispatcher using the supplied `RuntimeConfig`. This function will start
    /// the number of `coroutine_threads` specified in your config. Includes options.
    pub fn new_with_options(config: RuntimeConfig, handle: Handle, redis: Option<RedisClient>) -> RuntimeDispatcher {
        let size = config.coroutine_threads();
        let mut spawners = Vec::with_capacity(size);
        let mut waiting = Vec::with_capacity(size);
        for _ in 0..size {
            let (send, rec) = oneshot::channel();
            spawners.push(LocalSpawner::new(rec));
            waiting.push(send);
        }

        let strategy = config.strategy().clone();
        let dispatcher = Dispatcher {
            handle,
            config,
            strategy,
            spawners,
            redis,
            context_vars: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicUsize::new(0)),
        };
        let arc_dispatcher = Self(Arc::new(dispatcher));

        for i in waiting {
            i.send(arc_dispatcher.puff()).unwrap_or(());
        }

        arc_dispatcher
    }

    pub(crate) fn with_mutable_context_vars<F: FnOnce(&mut HashMap<String, Py<PyAny>>) -> R, R>(&self, f: F) -> R {
        let mut vars = self.0.context_vars.lock().expect("Could not lock context vars.");
        f(&mut vars)
    }

    pub(crate) fn with_context_vars<F: FnOnce(&HashMap<String, Py<PyAny>>) -> R, R>(&self, f: F) -> R {
        let mut vars = self.0.context_vars.lock().expect("Could not lock context vars.");
        f(&mut vars)
    }

    #[inline]
    pub(crate) fn monitor(&self) -> () {
        self.0.monitor()
    }

    #[inline]
    pub(crate) fn clone_for_new_task(&self) -> Self { RuntimeDispatcher(Arc::new(self.0.clone_for_new_task())) }

    #[inline]
    pub(crate) fn stack_size(&self) -> usize {
        self.0.stack_size()
    }

    #[inline]
    pub(crate) fn handle(&self) -> Handle {
        self.0.handle()
    }

    pub(crate) fn redis(&self) -> RedisClient {
        self.0.redis().expect("Redis is not configured for this runtime.")
    }

    /// Information about the current threads controlled by this Dispatcher.
    pub fn stats(&self) -> Vec<Stats> {
        self.0.stats()
    }

    /// Dispatch a task onto a coroutine worker thread and return a future to await on. Tasks are
    /// dispatched according to the `strategy` in [crate::runtime::RuntimeConfig].
    #[inline]
    pub fn dispatch<F, R>(&self, f: F) -> BoxFuture<'static, Result<R, Error>>
    where
        F: FnOnce() -> Result<R, Error> + Send + 'static,
        R: Send + 'static,
    {
        self.0.dispatch(f)
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
        let future = self.0.handle.spawn_blocking(|| {
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

impl Default for RuntimeDispatcher {
    fn default() -> Self {
        let rt = Runtime::new().unwrap();
        RuntimeDispatcher::new(RuntimeConfig::default(), rt.handle().clone())
    }
}

impl Puff for RuntimeDispatcher {}

#[derive(Clone)]
struct Dispatcher {
    spawners: Vec<LocalSpawner>,
    strategy: Strategy,
    handle: Handle,
    config: RuntimeConfig,
    redis: Option<RedisClient>,
    next: Arc<AtomicUsize>,
    context_vars: Arc<Mutex<HashMap<String, Py<PyAny>>>>
}

impl Dispatcher {
    fn clone_for_new_task(&self) -> Self {
        Dispatcher {
            spawners: self.spawners.clone(),
            strategy: self.strategy.clone(),
            handle: self.handle.clone(),
            config: self.config.clone(),
            redis: self.redis.clone(),
            next: self.next.clone(),
            context_vars: Arc::new(Mutex::new(HashMap::new()))
        }
    }

    #[inline]
    fn stack_size(&self) -> usize {
        self.config.stack_size()
    }

    #[inline]
    fn handle(&self) -> Handle {
        self.handle.clone()
    }

    #[inline]
    fn redis(&self) -> Option<RedisClient> {
        self.redis.clone()
    }

    fn monitor(&self) -> () {
        let dispatcher = self.clone();
        std::thread::spawn(move || {
            loop {
                let stats = dispatcher.stats();
                for stat in stats {
                    info!("Dispatcher: {} {} {}", stat.worker_id, stat.total_tasks, stat.total_tasks_completed);
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    fn stats(&self) -> Vec<Stats> {
        self.spawners
            .iter()
            .enumerate()
            .map(|(worker_id, v)| Stats {
                worker_id,
                total_tasks: v.total_tasks(),
                total_tasks_completed: v.total_tasks_completed(),
            })
            .collect()
    }

    fn dispatch<F, R>(&self, f: F) -> BoxFuture<'static, Result<R, Error>>
    where
        F: FnOnce() -> Result<R, Error> + Send + 'static,
        R: Send + 'static,
    {
        let spawner = self.find_spawner();
        spawner.spawn(f)
    }

    fn find_spawner(&self) -> &LocalSpawner {
        if self.spawners.len() == 1 {
            return self
                .spawners
                .first()
                .expect("Expected at least one Spawner");
        }
        match self.strategy {
            Strategy::LeastBusy => self.find_spawner_least_busy(),
            Strategy::RoundRobin => self.find_spawner_round_robin(),
            Strategy::Random => self.find_spawner_random(),
        }
    }

    fn find_spawner_round_robin(&self) -> &LocalSpawner {
        let next_ix = self.next.fetch_add(1, Ordering::SeqCst);
        unsafe { self.spawners.get_unchecked(next_ix % self.spawners.len()) }
    }

    fn find_spawner_random(&self) -> &LocalSpawner {
        let mut rng = thread_rng();
        self.spawners
            .as_slice()
            .choose(&mut rng)
            .expect("Expected at least one Spawner")
    }

    fn find_spawner_least_busy(&self) -> &LocalSpawner {
        let mut results = Vec::with_capacity(1);
        let mut least = usize::MAX;
        for spawner in &self.spawners {
            let threads = spawner.active_tasks();
            if threads < least {
                least = threads;
                results.truncate(0);
            }

            if threads == least {
                results.push(spawner)
            }
        }

        let mut rng = thread_rng();
        results
            .as_slice()
            .choose(&mut rng)
            .expect("Expected at least one Spawner")
    }
}

#[cfg(test)]
mod tests {
    // These tests should check distribution of strategies, but haven't had time to make it consistent.

    //     use super::*;
    //     use crate::runtime::{async_suspend, run_with_config, start_runtime_and_run, RuntimeConfig};
    //     use futures::executor::block_on;
    //     use futures::future::join_all;
    //     use std::sync::Arc;
    //     use std::thread::sleep;
    //     use std::time::Duration;
    //     use tokio::sync::{oneshot, Mutex};
    //
    //     #[test]
    //     fn check_dispatcher_empty_stats() {
    //         let rt = RuntimeConfig::default()
    //             .set_strategy(Strategy::RoundRobin)
    //             .set_num_coroutine_threads(62);
    //         let dispatcher = Dispatcher::new(rt);
    //         let stats = dispatcher.stats();
    //         assert_eq!(stats.len(), 62);
    //         for stat in stats {
    //             assert_eq!(stat.total_tasks, 0);
    //             assert_eq!(stat.total_tasks_completed, 0);
    //         }
    //     }
    //
    //     #[test]
    //     fn check_dispatcher_count_round_robin() {
    //         let num_coroutine_threads = 5;
    //         let rt = RuntimeConfig::default()
    //             .set_strategy(Strategy::RoundRobin)
    //             .set_num_coroutine_threads(num_coroutine_threads);
    //         let dispatcher = Dispatcher::new(rt);
    //
    //         for _ in 0..num_coroutine_threads {
    //             dispatcher.dispatch(move || {
    //                 Ok(async_suspend(async move {
    //                     tokio::time::sleep(Duration::from_millis(100)).await
    //                 }))
    //             });
    //         }
    //
    //         for _ in 0..num_coroutine_threads {
    //             dispatcher.dispatch(move || {
    //                 Ok(async_suspend(async move {
    //                     tokio::time::sleep(Duration::from_millis(200)).await
    //                 }))
    //             });
    //         }
    //         sleep(Duration::from_millis(50));
    //
    //         let stats = dispatcher.stats();
    //         assert_eq!(stats.len(), num_coroutine_threads);
    //         for stat in stats {
    //             assert_eq!(stat.total_tasks, 2);
    //             assert_eq!(stat.total_tasks_completed, 0);
    //         }
    //     }
    //
    //     #[test]
    //     fn check_dispatcher_count_least_busy() {
    //         let num_coroutine_threads = 5;
    //         let rounds = 3;
    //         let rt = RuntimeConfig::default()
    //             .set_strategy(Strategy::LeastBusy)
    //             .set_num_coroutine_threads(num_coroutine_threads);
    //         let dispatcher = Dispatcher::new(rt);
    //
    //         for _ in 0..(num_coroutine_threads * rounds) {
    //             // Pause a bit to allow AtomicUsize to sync
    //             sleep(Duration::from_millis(10));
    //             dispatcher.dispatch(move || {
    //                 Ok(async_suspend(async move {
    //                     tokio::time::sleep(Duration::from_millis((num_coroutine_threads * rounds * 15) as u64))
    //                         .await
    //                 }))
    //             });
    //         }
    //
    //         sleep(Duration::from_millis(10));
    //
    //         let stats = dispatcher.stats();
    //         assert_eq!(stats.len(), num_coroutine_threads);
    //         for stat in stats {
    //             assert_eq!(stat.total_tasks, rounds);
    //         }
    //     }
    //
    //     #[test]
    //     fn check_dispatcher_count_random() {
    //         let num_coroutine_threads = 5;
    //         let rounds = 5;
    //         let rt = RuntimeConfig::default()
    //             .set_strategy(Strategy::Random)
    //             .set_num_coroutine_threads(num_coroutine_threads);
    //         let dispatcher = Dispatcher::new(rt);
    //         let mut to_await = Vec::new();
    //         for _ in 0..(num_coroutine_threads * rounds) {
    //             let handle = dispatcher.dispatch(move || {
    //                 Ok(async_suspend(async move {
    //                     tokio::time::sleep(Duration::from_millis(15)).await
    //                 }))
    //             });
    //             to_await.push(handle);
    //         }
    //
    //         block_on(join_all(to_await));
    //         sleep(Duration::from_millis(10));
    //         let stats = dispatcher.stats();
    //         assert_eq!(stats.len(), num_coroutine_threads);
    //         // Not sure what else to do here to check distribution
    //         for stat in stats {
    //             assert_ne!(stat.total_tasks, 0);
    //         }
    //     }
}
