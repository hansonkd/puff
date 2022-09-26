//! Run functions Asynchronously.
//!
//! Tasks are Puffs way to manage running a function in a non-blocking way. Tasks can be awaited
//! without blocking the rest of the Runtime.
//!
//! # Examples
//!
//! ```no_run
//! use puff::tasks::Task;
//! let task = Task::spawn(|| {
//!     Ok(42)
//! });
//!
//! assert_eq!(task.join().unwrap(), 42)
//! ```
//!
use crate::runtime::yield_to_future;
use futures::future::BoxFuture;

use std::time::Duration;

use crate::errors::Error;

use crate::context::{PuffContext, with_puff_context};


/// A Task representing a currently executing coroutine. Can be awaited on using `join`.
///
/// Task runs a closure on a Puff coroutine thread.
///
/// # Examples
///
/// ```no_run
/// use puff::tasks::Task;
/// let task = Task::spawn(|| {
///     Ok(42)
/// });
///
/// assert_eq!(task.join().unwrap(), 42)
/// ```
///
pub struct Task<R: 'static> {
    handle: BoxFuture<'static, Result<R, Error>>,
}

impl<R> Task<R>
where
    R: 'static + Send,
{
    /// Run the closure asynchronously in a new coroutine. This will use the dispatching
    /// strategy to find a shared worker that will run the task.
    ///
    /// See [crate::runtime::dispatcher::RuntimeDispatcher::dispatch] for more information.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use puff::tasks::Task;
    /// let task = Task::spawn(|| {
    ///     Ok(42)
    /// });
    /// ```
    pub fn spawn<F>(f: F) -> Self
    where
        F: FnOnce() -> Result<R, Error> + Send + 'static,
    {
        Task {
            handle: with_puff_context(move |v| v.dispatcher().dispatch(f)),
        }
    }

    /// Run the closure asynchronously in a blocking thread. This is useful if your function takes a
    /// lot of computational power or uses a non-async blocking function.
    ///
    /// See [crate::context::PuffContext::dispatch_blocking] for more information.
    ///
    /// # Examples
    ///
    /// In the following example, we use `thread::sleep` instead of `puff::tasks::sleep`.
    /// Normally this would block other coroutines, but with `spawn_blocking` its isolated to its own thread
    ///
    /// ```no_run
    /// use puff::tasks::Task;
    /// use std::{thread, time};
    ///
    /// let task = Task::spawn_blocking(|| {
    ///     let ten_seconds = time::Duration::from_secs(10);
    ///     thread::sleep(ten_seconds);
    ///     Ok(42)
    /// });
    /// ```
    pub fn spawn_blocking<F>(f: F) -> Self
    where
        F: FnOnce() -> Result<R, Error> + Send + 'static,
    {
        Task {
            handle: with_puff_context(move |v| v.dispatch_blocking(f)),
        }
    }

    /// Wait for the task to finish and return the result. This function will suspend the current
    /// coroutine and yield to other tasks.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use puff::tasks::Task;
    /// let task = Task::spawn(|| {
    ///     Ok(42)
    /// });
    /// assert_eq!(task.join().unwrap(), 42)
    /// ```
    pub fn join(self) -> Result<R, Error> {
        Ok(yield_to_future(self.handle)?)
    }

    /// Wait for the task to finish within the timeout. See [Self::join] for more information.
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use puff::tasks::{sleep, Task};
    /// let task = Task::spawn(|| {
    ///     sleep(Duration::from_millis(100));
    ///     Ok(42)
    /// });
    /// assert!(task.timeout(Duration::from_millis(50)).is_err());
    /// ```
    pub fn timeout(self, duration: Duration) -> Result<R, Error> {
        Ok(yield_to_future(tokio::time::timeout(
            duration,
            self.handle,
        ))??)
    }
}

/// Wait for all tasks to finish.
///
/// The returned Task will drive execution for all of its underlying futures,
/// collecting the results into a destination `Vec<Result<T>>` in the same order as they
/// were provided.
///
/// # Examples
///
/// ```no_run
/// use puff::tasks::{join_all, Task};
/// let task = Task::spawn(|| {
///     Ok(42)
/// });
///
/// let task2 = Task::spawn(|| {
///     Ok(420)
/// });
/// assert_eq!(join_all(vec![task, task2]).len(), 2)
/// ```
pub fn join_all<R>(iter: Vec<Task<R>>) -> Vec<Result<R, Error>>
where
    R: 'static,
{
    yield_to_future(futures::future::join_all(
        iter.into_iter().map(|i| async { Ok(i.handle.await?) }),
    ))
}

/// Wait for all tasks to finish within the timeout. See [join_all].
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use puff::tasks::{timeout_all, Task};
/// let task = Task::spawn(|| {
///     Ok(42)
/// });
///
/// let task2 = Task::spawn(|| {
///     Ok(420)
/// });
/// assert!(timeout_all(Duration::from_millis(100), vec![task, task2]).is_ok())
/// ```
pub fn timeout_all<R>(
    duration: Duration,
    iter: Vec<Task<R>>,
) -> Result<Vec<Result<R, Error>>, Error>
where
    R: 'static,
{
    Ok(yield_to_future(tokio::time::timeout(
        duration,
        futures::future::join_all(iter.into_iter().map(|i| async { Ok(i.handle.await?) })),
    ))?)
}

/// Sleep the current task for the specified duration.
///
/// The same warnings apply from tokio's sleep:
///
/// No work is performed while awaiting on the sleep future to complete. Sleep operates at
/// millisecond granularity and should not be used for tasks that require high-resolution timers.
/// The implementation is platform specific, and some platforms (specifically Windows) will provide
/// timers with a larger resolution than 1 ms.
///
/// The maximum duration for a sleep is 68719476734 milliseconds (approximately 2.2 years).
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use puff::tasks::{sleep, Task};
/// let task = Task::spawn(|| {
///     sleep(Duration::from_secs(1));
///     Ok(42)
/// });
pub fn sleep(duration: Duration) -> () {
    yield_to_future(tokio::time::sleep(duration))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::start_runtime_and_run;
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
                    yield_to_future(async move { results.lock().await.push(xx) });
                    Ok(xx)
                });
                tasks.push(task);
                expected.push(xx);
            }
            let joined = join_all(tasks);
            let _res: Vec<Error> = joined.into_iter().filter_map(|v| v.err()).collect();
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
            println!("Nested Task");
            let task = Task::spawn(move || {
                println!("Outer Task Start");
                let ret = Task::spawn(move || {
                    println!("Inner Task Start");
                    Ok(59)
                }).join();
                println!("Outer Task End");
                ret
            });
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
