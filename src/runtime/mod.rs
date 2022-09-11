use crate::runtime::wormhole::{AsyncWormhole, AsyncYielder};
use anyhow::anyhow;
use corosensei::stack::{DefaultStack, Stack};
use futures::FutureExt;
use std::any::Any;
use std::borrow::{Borrow, BorrowMut};
use std::future::Future;
use std::io::ErrorKind;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::errors::Error;
use crate::tasks::dispatcher::Dispatcher;
use crate::tasks::DISPATCHER;

pub mod wormhole;

type PuffWormhole = AsyncWormhole<'static, DefaultStack, (), fn()>;

tokio::task_local! {
    static YIELDER: *mut AsyncYielder<'static, ()>;
    static STACK_SIZE:  usize;
}

pub fn async_suspend<Fut, R>(mut future: Fut) -> R
where
    Fut: Future<Output = R>,
{
    let m: *mut AsyncYielder<'static, ()> = YIELDER.get();
    match unsafe { m.as_mut() } {
        Some(l) => l.async_suspend(future),
        None => panic!("async_suspend must be called from a Puff context"),
    }
}

pub async fn run_local_wormhole_with_stack<F, R>(
    s: DefaultStack,
    sender: oneshot::Sender<Result<R, Error>>,
    f: F,
) -> Result<(), Error>
where
    F: FnOnce() -> Result<R, Error> + 'static + Send,
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

#[derive(Clone, Copy)]
pub enum Strategy {
    LeastBusy,
    Random,
    RoundRobin,
}

#[derive(Clone, Copy)]
pub struct RuntimeConfig {
    pub(crate) stack_size: usize,
    pub(crate) num_workers: usize,
    pub(crate) strategy: Strategy,
}

impl RuntimeConfig {
    pub fn set_strategy(&self, strategy: Strategy) -> Self {
        let mut new = self.clone();
        new.strategy = strategy;
        new
    }

    pub fn set_stack_size(&self, stack_size: usize) -> Self {
        let mut new = self.clone();
        new.stack_size = stack_size;
        new
    }

    pub fn set_num_workers(&self, num_workers: usize) -> Self {
        let mut new = self.clone();
        new.num_workers = num_workers;
        new
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            stack_size: 1024 * 1024,
            num_workers: 1,
            strategy: Strategy::LeastBusy,
        }
    }
}

pub async fn run_new_stack<F, R>(
    sender: oneshot::Sender<Result<R, Error>>,
    f: F,
) -> Result<(), Error>
where
    F: FnOnce() -> Result<R, Error> + 'static + Send,
    R: 'static + Send,
{
    let stack_size = STACK_SIZE.get();
    let stack = DefaultStack::new(stack_size)?;
    run_local_wormhole_with_stack(stack, sender, f).await
}

pub async fn run<F, R>(f: F) -> Result<R, Error>
where
    F: FnOnce() -> Result<R, Error> + 'static + Send,
    R: 'static + Send,
{
    let dispatcher = DISPATCHER.with(|v| v.clone());
    let local = tokio::task::LocalSet::new();
    let (sender, recv) = oneshot::channel();
    local
        .run_until(DISPATCHER.scope(dispatcher, run_new_stack(sender, f)))
        .await?;
    recv.await
        .map_err(|_| anyhow!("Could not receive result for closure"))?
}

pub async fn run_with_config<F, R>(config: RuntimeConfig, f: F) -> Result<R, Error>
where
    F: FnOnce() -> Result<R, Error> + Send + 'static,
    R: 'static + Send,
{
    STACK_SIZE.scope(config.stack_size, run(f)).await
}

pub async fn run_with_config_on_local<F, R>(
    config: RuntimeConfig,
    dispatcher: Arc<Dispatcher>,
    sender: oneshot::Sender<Result<R, Error>>,
    f: F,
) -> Result<(), Error>
where
    F: FnOnce() -> Result<R, Error> + Send + 'static,
    R: 'static + Send,
{
    DISPATCHER
        .scope(
            dispatcher,
            STACK_SIZE.scope(config.stack_size, run_new_stack(sender, f)),
        )
        .await
}

pub fn start_runtime_and_run<F, R>(f: F) -> Result<R, Error>
where
    F: FnOnce() -> Result<R, Error> + Send + 'static,
    R: 'static + Send,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let config = RuntimeConfig::default();
    let dispatcher = Arc::new(Dispatcher::new(config));
    // rt.block_on(DISPATCHER.scope(dispatcher, run_with_config(config, f)))
    rt.block_on(DISPATCHER.scope(dispatcher.clone(), dispatcher.dispatch(f)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::join_all;
    use std::time::Duration;
    use tokio::sync::Mutex;

    #[test]
    fn check_wormhole() {
        start_runtime_and_run(|| {
            let r = async_suspend(async { 42 });
            assert_eq!(r, 42);
            Ok(())
        })
        .unwrap();
    }

    fn recurse(xx: usize) {
        let r = async_suspend(async move {
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

    // fn recurse_wormhole(xx: usize, dispatcher: Dispatcher) {
    //     let r = async_suspend(run(move || {
    //         // tokio::time::sleep(Duration::from_millis(xx as u64)).await;
    //         if xx > 0 {
    //             Ok(recurse_wormhole(xx - 1, dispatcher))
    //         } else {
    //             Ok(())
    //         }
    //     }))
    //     .unwrap();
    // }
    //
    // #[test]
    // fn check_recurse_wormhole() {
    //     start_runtime_and_run(|| Ok(recurse_wormhole(10000))).unwrap()
    // }

    #[test]
    fn check_wormhole_tasks_many_wormholes_dont_block() {
        // Run a sequence of futures. The futures should resolve in reverse order.

        use tokio::runtime::Runtime;
        const NUMBER_TASKS: usize = 50;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let config = RuntimeConfig::default();
        let dispatcher = Dispatcher::new(config);

        rt.block_on(DISPATCHER.scope(Arc::new(dispatcher), async {
            let mut tasks = Vec::new();
            let mut expected = Vec::new();
            let results = Arc::new(Mutex::new(Vec::new()));
            for xx in 0..NUMBER_TASKS {
                let results = results.clone();
                let task = run_with_config(config, move || {
                    let r = async_suspend(async move {
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
