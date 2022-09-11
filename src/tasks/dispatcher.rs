use futures::future::BoxFuture;
use rand::seq::SliceRandom;
use rand::thread_rng;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;

use crate::errors::Error;
use crate::runtime::{RuntimeConfig, Strategy};
use crate::tasks::runner::LocalSpawner;

#[derive(Clone, Debug)]
pub struct Stats {
    pub worker_id: usize,
    pub total_tasks: usize,
    pub total_tasks_completed: usize,
}

#[derive(Clone)]
pub struct Dispatcher {
    spawners: Vec<LocalSpawner>,
    strategy: Strategy,
    next: Arc<AtomicUsize>,
}

impl Dispatcher {
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            spawners: Vec::new(),
            strategy: Strategy::Random,
            next: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn new(config: RuntimeConfig) -> Self {
        let size = config.num_workers;
        let mut vec = Vec::with_capacity(size);
        let mut waiting = Vec::with_capacity(size);
        for _ in 0..size {
            let (send, rec) = oneshot::channel();
            vec.push(LocalSpawner::new(config, rec));
            waiting.push(send);
        }

        let dispatcher = Self {
            spawners: vec,
            strategy: config.strategy.clone(),
            next: Arc::new(AtomicUsize::new(0)),
        };
        let arc_dispatcher = Arc::new(dispatcher.clone());

        for i in waiting {
            i.send(arc_dispatcher.clone()).unwrap_or(());
        }

        dispatcher
    }

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

impl Default for Dispatcher {
    fn default() -> Self {
        Dispatcher::new(RuntimeConfig::default())
    }
}

// #[cfg(test)]
// mod tests {
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
//             .set_num_workers(62);
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
//         let num_workers = 5;
//         let rt = RuntimeConfig::default()
//             .set_strategy(Strategy::RoundRobin)
//             .set_num_workers(num_workers);
//         let dispatcher = Dispatcher::new(rt);
//
//         for _ in 0..num_workers {
//             dispatcher.dispatch(move || {
//                 Ok(async_suspend(async move {
//                     tokio::time::sleep(Duration::from_millis(100)).await
//                 }))
//             });
//         }
//
//         for _ in 0..num_workers {
//             dispatcher.dispatch(move || {
//                 Ok(async_suspend(async move {
//                     tokio::time::sleep(Duration::from_millis(200)).await
//                 }))
//             });
//         }
//         sleep(Duration::from_millis(50));
//
//         let stats = dispatcher.stats();
//         assert_eq!(stats.len(), num_workers);
//         for stat in stats {
//             assert_eq!(stat.total_tasks, 2);
//             assert_eq!(stat.total_tasks_completed, 0);
//         }
//     }
//
//     #[test]
//     fn check_dispatcher_count_least_busy() {
//         let num_workers = 5;
//         let rounds = 3;
//         let rt = RuntimeConfig::default()
//             .set_strategy(Strategy::LeastBusy)
//             .set_num_workers(num_workers);
//         let dispatcher = Dispatcher::new(rt);
//
//         for _ in 0..(num_workers * rounds) {
//             // Pause a bit to allow AtomicUsize to sync
//             sleep(Duration::from_millis(10));
//             dispatcher.dispatch(move || {
//                 Ok(async_suspend(async move {
//                     tokio::time::sleep(Duration::from_millis((num_workers * rounds * 15) as u64))
//                         .await
//                 }))
//             });
//         }
//
//         sleep(Duration::from_millis(10));
//
//         let stats = dispatcher.stats();
//         assert_eq!(stats.len(), num_workers);
//         for stat in stats {
//             assert_eq!(stat.total_tasks, rounds);
//         }
//     }
//
//     #[test]
//     fn check_dispatcher_count_random() {
//         let num_workers = 5;
//         let rounds = 5;
//         let rt = RuntimeConfig::default()
//             .set_strategy(Strategy::Random)
//             .set_num_workers(num_workers);
//         let dispatcher = Dispatcher::new(rt);
//         let mut to_await = Vec::new();
//         for _ in 0..(num_workers * rounds) {
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
//         assert_eq!(stats.len(), num_workers);
//         // Not sure what else to do here to check distribution
//         for stat in stats {
//             assert_ne!(stat.total_tasks, 0);
//         }
//     }
// }
