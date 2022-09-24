//! async-wormhole allows you to call `.await` async calls across non-async functions, like extern "C" or JIT
//! generated code.
//!
//! ## Motivation
//!
//! Sometimes, when running inside an async environment you need to call into JIT generated code (e.g. wasm)
//! and .await from there. Because the JIT code is not available at compile time, the Rust compiler can't
//! do their "create a state machine" magic. In the end you can't have `.await` statements in non-async
//! functions.
//!
//! This library creates a special stack for executing the JIT code, so it's possible to suspend it at any
//! point of the execution. Once you pass it a closure inside [AsyncWormhole::new](struct.AsyncWormhole.html#method.new)
//! you will get back a future that you can `.await` on. The passed in closure is going to be executed on a
//! new stack.
use std::cell::Cell;
use std::future::Future;
use std::io::Error;

use std::pin::Pin;

use std::task::{Context, Poll, Waker};

use corosensei::{stack, CoroutineResult, ScopedCoroutine, Yielder};

use std::cell::RefCell;

thread_local! {
    pub static FOO: RefCell<u32> = RefCell::new(1);
}

/// AsyncWormhole represents a Future that uses a generator with a separate stack to execute a closure.
///
/// It has the capability to .await on other Futures in the closure using the received
/// [AsyncYielder](struct.AsyncYielder). Once all Futures have been awaited on AsyncWormhole will resolve
/// to the return value of the provided closure.
pub struct AsyncWormhole<'a, Stack, Output>
where
    Stack: stack::Stack + Send,
{
    generator: Option<Cell<ScopedCoroutine<'a, Waker, Option<Output>, (), Stack>>>,
}

impl<'a, Stack, Output> AsyncWormhole<'a, Stack, Output>
where
    Stack: stack::Stack + Send,
{
    /// Returns a new AsyncWormhole, using the passed `stack` to execute the closure `f` on.
    /// The closure will not be executed right away, only if you pass AsyncWormhole to an
    /// async executor (.await on it)
    pub fn new<F>(stack: Stack, f: F) -> Result<Self, Error>
    where
        F: FnOnce(AsyncYielder<Output>) -> Output + 'a + Send,
    {
        let generator = ScopedCoroutine::with_stack(stack, |yielder, waker| {
            let async_yielder = AsyncYielder::new(yielder, waker);
            let finished = Some(f(async_yielder));
            yielder.suspend(finished);
        });

        Ok(Self {
            generator: Some(Cell::new(generator)),
        })
    }
}

impl<'a, Stack, Output> Future for AsyncWormhole<'a, Stack, Output>
where
    Stack: stack::Stack + Unpin + Send,
{
    type Output = Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result = self
            .generator
            .as_mut()
            .unwrap()
            .get_mut()
            .resume(cx.waker().clone());

        match result {
            // If we call the future after it completed it will always return Poll::Pending.
            // But polling a completed future is either way undefined behaviour.
            CoroutineResult::Return(()) | CoroutineResult::Yield(None) => Poll::Pending,
            CoroutineResult::Yield(Some(out)) => {
                // Poll one last time to finish the generator
                self.generator
                    .as_mut()
                    .unwrap()
                    .get_mut()
                    .resume(cx.waker().clone());
                Poll::Ready(out)
            }
        }
    }
}

#[derive(Clone)]
pub struct AsyncYielder<'a, Output> {
    yielder: &'a Yielder<Waker, Option<Output>>,
    waker: Waker,
}

impl<'a, Output> AsyncYielder<'a, Output> {
    pub(crate) fn new(yielder: &'a Yielder<Waker, Option<Output>>, waker: Waker) -> Self {
        Self { yielder, waker }
    }

    /// Takes an `impl Future` and awaits it, returning the value from it once ready.
    pub fn async_suspend<Fut, R>(&mut self, mut future: Fut) -> R
    where
        Fut: Future<Output = R>,
    {
        let mut future = unsafe { Pin::new_unchecked(&mut future) };
        loop {
            let mut cx = Context::from_waker(&mut self.waker);
            self.waker = match future.as_mut().poll(&mut cx) {
                Poll::Pending => self.yielder.suspend(None),
                Poll::Ready(result) => return result,
            };
        }
    }
}
