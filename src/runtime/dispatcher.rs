//! Distribute Tasks onto Coroutine workers and bootstrap them with the Puff Context.
//!
//! Every Puff Runtime has a `RuntimeDispatcher`. Calling the dispatcher produces a future that you
//! can then await on in an async context.
//!
//! The primary way to interact with the dispatcher is calling [crate::tasks::Task] or [crate::program::RunnableCommand]
//!
use crate::context::PuffContext;

use futures::future::BoxFuture;
use rand::seq::SliceRandom;
use rand::thread_rng;


use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc};
use std::time::Duration;

use tokio::sync::{broadcast, oneshot};

use tracing::info;

use crate::errors::Error;
use crate::runtime::runner::LocalSpawner;
use crate::runtime::shutdown::Shutdown;
use crate::runtime::{RuntimeConfig, Strategy};


/// Simple statistics about each worker.
#[derive(Clone, Debug)]
pub struct Stats {
    pub worker_id: usize,
    pub total_tasks: usize,
    pub total_tasks_completed: usize,
}

/// An abstraction to schedule coroutines onto threads (LocalSpawners).
#[derive(Clone)]
pub struct Dispatcher {
    spawners: Vec<LocalSpawner>,
    strategy: Strategy,
    config: RuntimeConfig,
    next: Arc<AtomicUsize>,
    notify_shutdown: broadcast::Sender<()>,
}

impl Dispatcher {
    pub fn empty(notify_shutdown: broadcast::Sender<()>) -> Self {
        Dispatcher {
            notify_shutdown,
            config: RuntimeConfig::default(),
            spawners: Vec::new(),
            strategy: Strategy::Random,
            next: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn new(
        notify_shutdown: broadcast::Sender<()>,
        config: RuntimeConfig,
    ) -> (Self, Vec<oneshot::Sender<PuffContext>>) {
        let size = config.coroutine_threads();
        let mut spawners = Vec::with_capacity(size);
        let mut waiting = Vec::with_capacity(size);
        let strategy = config.strategy();
        for _ in 0..size {
            let (send, rec) = oneshot::channel();
            spawners.push(LocalSpawner::new(
                rec,
                Shutdown::new(notify_shutdown.subscribe()),
            ));
            waiting.push(send);
        }
        let disptcher = Self {
            config,
            strategy,
            spawners,
            notify_shutdown,
            next: Arc::new(AtomicUsize::new(0)),
        };
        return (disptcher, waiting);
    }

    #[inline]
    pub fn stack_size(&self) -> usize {
        self.config.stack_size()
    }

    /// Start a debugging monitor that prints stats if configured.
    pub fn start_monitor(&self) -> () {
        if let Some(duration) = self.config.monitor() {
            let dispatcher = self.clone();
            std::thread::spawn(move || loop {
                let stats = dispatcher.stats();
                for stat in stats {
                    info!(
                        "Worker: {} | Total: {} | Completed: {}",
                        stat.worker_id, stat.total_tasks, stat.total_tasks_completed
                    );
                }
                std::thread::sleep(duration);
            });
        }
    }

    /// Information about the current threads controlled by this Dispatcher.
    pub fn stats(&self) -> Vec<Stats> {
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

    /// Dispatch a task onto a coroutine worker thread and return a future to await on. Tasks are
    /// dispatched according to the `strategy` in [crate::runtime::RuntimeConfig].
    pub fn dispatch<F, R>(&self, f: F) -> BoxFuture<'static, Result<R, Error>>
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
