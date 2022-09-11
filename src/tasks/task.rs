use crate::runtime::{async_suspend, run, run_new_stack};
use futures::future::BoxFuture;
use std::future;
use std::future::Future;
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::errors::Error;
use crate::tasks::DISPATCHER;

pub struct Task<R: 'static> {
    handle: BoxFuture<'static, Result<R, Error>>,
}

impl<R> Task<R>
where
    R: 'static + Send,
{
    pub fn spawn<F>(f: F) -> Self
    where
        F: FnOnce() -> Result<R, Error> + Send + 'static,
    {
        Task {
            handle: DISPATCHER.with(move |v| v.dispatch(f)),
        }
    }

    pub fn join(self) -> Result<R, Error> {
        Ok(async_suspend(self.handle)?)
    }

    pub fn timeout(self, duration: Duration) -> Result<R, Error> {
        Ok(async_suspend(tokio::time::timeout(duration, self.handle))??)
    }
}

pub fn join_all<R>(iter: Vec<Task<R>>) -> Vec<Result<R, Error>>
where
    R: 'static,
{
    async_suspend(futures::future::join_all(
        iter.into_iter().map(|i| async { Ok(i.handle.await?) }),
    ))
}

pub fn timeout_all<R>(
    duration: Duration,
    iter: Vec<Task<R>>,
) -> Result<Vec<Result<R, Error>>, Error>
where
    R: 'static,
{
    Ok(async_suspend(tokio::time::timeout(
        duration,
        futures::future::join_all(iter.into_iter().map(|i| async { Ok(i.handle.await?) })),
    ))?)
}

pub fn sleep(duration: Duration) -> () {
    async_suspend(tokio::time::sleep(duration))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{run_with_config, start_runtime_and_run, RuntimeConfig};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    #[test]
    fn check_task() {
        start_runtime_and_run(|| {
            let task = Task::spawn(|| {
                sleep(Duration::from_millis(100));
                Ok(1)
            });

            let task2 = Task::spawn(|| {
                sleep(Duration::from_millis(200));
                Ok(2)
            });

            let res = task.join().unwrap();
            let res2 = task2.join().unwrap();

            assert_eq!(res, 1);
            assert_eq!(res2, 2);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn check_task_dont_block() {
        // Run a sequence of tasks. The tasks should resolve in reverse order.
        const NUMBER_TASKS: usize = 50;
        start_runtime_and_run(|| {
            let mut tasks = Vec::new();
            let mut expected = Vec::new();
            let results = Arc::new(Mutex::new(Vec::new()));
            for xx in 0..NUMBER_TASKS {
                let results = results.clone();

                let task = Task::spawn(move || {
                    sleep(Duration::from_millis(10 * (NUMBER_TASKS - xx) as u64));
                    async_suspend(async move { results.lock().await.push(xx) });
                    Ok(xx)
                });
                tasks.push(task);
                expected.push(xx);
            }
            let joined = join_all(tasks);
            let res: Vec<Error> = joined.into_iter().filter_map(|v| v.err()).collect();
            // assert_eq!(res, Vec::<Error>::new());

            // let res: Vec<usize> = joined.into_iter().filter_map(|v| v.ok()).collect();
            //
            // assert_eq!(res, expected);
            //
            // expected.reverse();
            //
            // let m = async_suspend(async move { results.lock().await.clone() });
            //
            // assert_eq!(m, expected);

            Ok(())
        })
        .unwrap();
    }

    fn recursive(xx: usize) -> Task<usize> {
        Task::spawn(move || {
            if xx > 0 {
                recursive(xx - 1).join()
            } else {
                Ok(xx)
            }
        })
    }

    #[test]
    fn check_recursive_tasks_dont_block() {
        start_runtime_and_run(|| recursive(1000).join()).unwrap();
    }

    #[test]
    fn check_nested_task() {
        let val = start_runtime_and_run(|| {
            let task = Task::spawn(move || Task::spawn(move || Ok(59)).join());
            task.join()
        })
        .unwrap();
        assert_eq!(val, 59)
    }

    #[test]
    fn check_timeout_fails() {
        start_runtime_and_run(|| {
            let task = Task::spawn(|| {
                sleep(Duration::from_millis(100));
                Ok(())
            });
            let res = task.timeout(Duration::from_millis(50));
            assert!(res.is_err());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn check_timeout_passes() {
        start_runtime_and_run(|| {
            let task = Task::spawn(|| {
                sleep(Duration::from_millis(50));
                Ok(())
            });
            let res = task.timeout(Duration::from_millis(100));
            assert!(res.is_ok());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn check_timeout_all_passes() {
        start_runtime_and_run(|| {
            let task = Task::spawn(|| {
                sleep(Duration::from_millis(10));
                Ok(())
            });
            let task2 = Task::spawn(|| {
                sleep(Duration::from_millis(50));
                Ok(())
            });
            let res = timeout_all(Duration::from_millis(100), vec![task, task2]);
            assert!(res.is_ok());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn check_timeout_all_fail() {
        start_runtime_and_run(|| {
            let task = Task::spawn(|| {
                sleep(Duration::from_millis(10));
                Ok(())
            });
            let task2 = Task::spawn(|| {
                sleep(Duration::from_millis(50));
                Ok(())
            });
            let res = timeout_all(Duration::from_millis(30), vec![task, task2]);
            assert!(res.is_err());
            Ok(())
        })
        .unwrap();
    }
}
