use crate::errors::Error;
use crate::runtime::dispatcher::RuntimeDispatcher;
use crate::runtime::run_with_config_on_local;

use anyhow::anyhow;
use futures::future::BoxFuture;
use futures::FutureExt;
use std::any::Any;

use crate::types::Puff;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{spawn_local, LocalSet};
use tracing::warn;

type PuffReturn = Box<dyn Any + Send + 'static>;

struct SpawnerJob(
    oneshot::Sender<Result<PuffReturn, Error>>,
    Box<dyn FnOnce() -> Result<PuffReturn, Error> + Send + 'static>,
);

trait Runnable: Send + Any + 'static
where
    Self: Sized,
{
}

impl<T: Sized + Send + Any + 'static> Runnable for T {}

#[derive(Clone)]
pub(crate) struct LocalSpawner {
    send: UnboundedSender<SpawnerJob>,
    num_tasks_completed: Arc<AtomicUsize>,
    num_tasks: Arc<AtomicUsize>,
}

impl LocalSpawner {
    fn without_dispatcher() -> Self {
        let rt = Runtime::new().unwrap();
        let (send, rec) = oneshot::channel();
        let runner = LocalSpawner::new(rec);
        send.send(RuntimeDispatcher::empty(rt.handle().clone()))
            .unwrap_or(());
        runner
    }

    pub(crate) fn new(dispatcher_lazy: oneshot::Receiver<RuntimeDispatcher>) -> Self {
        let (send, mut recv) = mpsc::unbounded_channel();
        let num_tasks = Arc::new(AtomicUsize::new(0));
        let num_tasks_completed = Arc::new(AtomicUsize::new(0));
        let num_tasks_loop = num_tasks.clone();
        let num_tasks_completed_loop = num_tasks_completed.clone();
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        std::thread::spawn(move || {

            let dispatcher = rt.block_on(dispatcher_lazy).unwrap();

            let local = LocalSet::new();
            let dispatcher = dispatcher.puff();
            local.spawn_local(async move {
                while let Some(SpawnerJob(new_sender, new_task)) = recv.recv().await {
                    let dispatcher = dispatcher.puff();
                    let num_tasks_loop = num_tasks_loop.clone();
                    let num_tasks_completed_loop = num_tasks_completed_loop.clone();
                    let _ = num_tasks_loop.fetch_add(1, Ordering::SeqCst);
                    let fut = async move {
                        let res = run_with_config_on_local(dispatcher, new_sender, new_task).await;
                        let _ = num_tasks_completed_loop.fetch_add(1, Ordering::SeqCst);
                        res
                    };

                    spawn_local(fut);
                }
                // If the while loop returns, then all the LocalSpawner
                // objects have have been dropped.
            });

            // This will return once all senders are dropped and all
            // spawned tasks have returned.
            rt.block_on(local);
        });

        Self {
            send,
            num_tasks,
            num_tasks_completed,
        }
    }

    #[inline]
    pub(crate) fn total_tasks(&self) -> usize {
        self.num_tasks.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn total_tasks_completed(&self) -> usize {
        self.num_tasks_completed.load(Ordering::Relaxed)
    }

    pub(crate) fn active_tasks(&self) -> usize {
        let total_tasks = self.total_tasks();
        let completed = self.total_tasks_completed();
        // This should never be negative, but just in case...
        if total_tasks >= completed {
            total_tasks - completed
        } else {
            warn!("Total tasks ({}) is less than completed tasks ({}). This can lead to the thread not being scheduled.", total_tasks, completed);
            0
        }
    }

    pub(crate) fn spawn<F, R>(&self, f: F) -> BoxFuture<'static, Result<R, Error>>
    where
        F: FnOnce() -> Result<R, Error> + Sized + Send + 'static,
        R: Send + Sized + 'static,
    {
        let (sender, recv) = oneshot::channel::<Result<PuffReturn, Error>>();
        let new_f = || {
            let res = f()?;
            let a: Box<dyn Any + Send> = Box::new(res);
            Ok(a)
        };
        let task = async {
            let r = recv.await;
            match r {
                Ok(v) => Ok(v?.downcast().map(|v| (*v)).unwrap()),
                Err(r) => Err(anyhow!("Could nto receive dispatch result {:?}", r)),
            }
        }
        .boxed();

        match self.send.send(SpawnerJob(sender, Box::new(new_f))) {
            Ok(()) => task,
            Err(_err) => panic!("Error sending to task executor."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::yield_to_future;
    use futures::executor::block_on;
    use std::sync::Arc;
    use std::thread::sleep;
    use std::time::Duration;
    use tokio::sync::{oneshot, Mutex};

    #[test]
    fn check_runner() {
        let runner = LocalSpawner::without_dispatcher();
        let counter = Arc::new(Mutex::new(0));
        let counter2 = counter.clone();
        let (sender, rec) = oneshot::channel();
        let runner_ref = runner.clone();
        runner.spawn(move || {
            assert_eq!(runner_ref.active_tasks(), 1);
            yield_to_future(async move {
                let mut c = counter2.lock().await;
                *c += 1;
                sender.send(()).unwrap();
            });
            Ok(())
        });

        let res = futures::executor::block_on(async {
            rec.await.unwrap();
            let c = counter.lock().await;
            *c
        });
        assert_eq!(res, 1);

        // Sleep a bit to let it mark the task as complete
        sleep(Duration::from_millis(100));
        assert_eq!(runner.active_tasks(), 0);
    }

    // #[test]
    fn check_thread_count() {
        let runner = LocalSpawner::without_dispatcher();

        let task = runner.spawn(move || {
            Ok(yield_to_future(async move {
                tokio::time::sleep(Duration::from_millis(100)).await
            }))
        });

        let task2 = runner.spawn(move || {
            Ok(yield_to_future(async move {
                tokio::time::sleep(Duration::from_millis(200)).await
            }))
        });

        sleep(Duration::from_millis(20));
        assert_eq!(runner.active_tasks(), 2);
        block_on(task).unwrap();
        sleep(Duration::from_millis(10));
        assert_eq!(runner.active_tasks(), 1);
        block_on(task2).unwrap();
        sleep(Duration::from_millis(10));
        assert_eq!(runner.active_tasks(), 0);
    }

    // #[test]
    fn check_thread_count_panic() {
        let runner = LocalSpawner::without_dispatcher();

        let task1 = runner.spawn(move || -> Result<(), Error> {
            yield_to_future(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
            });

            panic!("uh oh");
        });

        let task2 = runner.spawn(move || {
            Ok(yield_to_future(async move {
                tokio::time::sleep(Duration::from_millis(150)).await
            }))
        });

        sleep(Duration::from_millis(20));
        assert_eq!(runner.active_tasks(), 2);
        assert!(block_on(task1).is_err());
        sleep(Duration::from_millis(10));
        assert_eq!(runner.active_tasks(), 1);
        block_on(task2).unwrap();
        sleep(Duration::from_millis(10));
        assert_eq!(runner.active_tasks(), 0);
    }
}
