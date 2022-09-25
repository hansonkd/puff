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
use std::borrow::BorrowMut;
use std::cell::Cell;
use std::future::Future;
use std::io::Error;

use std::pin::Pin;

use std::task::{Context, Poll, Waker};

use corosensei::{stack, CoroutineResult, ScopedCoroutine, Yielder};

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use pyo3::{Py, Python};
use crate::python::PythonContext;
use crate::runtime::dispatcher::RuntimeDispatcher;
use crate::types::Puff;

thread_local! {
    pub static YIELDER: RefCell<*mut AsyncYielder<'static>> = RefCell::new(std::ptr::null_mut());
    pub static DISPATCHER: RefCell<Option<RuntimeDispatcher>> = RefCell::new(None);
}

/// AsyncWormhole represents a Future that uses a generator with a separate stack to execute a closure.
///
/// It has the capability to .await on other Futures in the closure using the received
/// [AsyncYielder](struct.AsyncYielder). Once all Futures have been awaited on AsyncWormhole will resolve
/// to the return value of the provided closure.
pub struct AsyncWormhole<'a, Stack>
where
    Stack: stack::Stack + Send,
{
    generator: Option<Cell<ScopedCoroutine<'a, Waker, Option<()>, (), Stack>>>,
    ref_yielder: Rc<Mutex<*mut AsyncYielder<'static>>>,
    dispatcher: RuntimeDispatcher,
    context: PythonContext,
}

impl<'a, Stack> AsyncWormhole<'a, Stack>
where
    Stack: stack::Stack + Send,
{
    /// Returns a new AsyncWormhole, using the passed `stack` to execute the closure `f` on.
    /// The closure will not be executed right away, only if you pass AsyncWormhole to an
    /// async executor (.await on it)
    pub fn new<F>(stack: Stack, dispatcher: RuntimeDispatcher, f: F) -> Result<Self, Error>
    where
        F: FnOnce() -> () + 'a + Send,
    {
        let ref_yielder = Rc::new(Mutex::new(std::ptr::null_mut()));
        let mut coroutine_yielder = ref_yielder.clone();
        let coroutine_dispatcher = dispatcher.clone();
        let context = PythonContext::copy_context();
        let generator = ScopedCoroutine::with_stack(stack, move |yielder, waker| {
            let async_yielder = AsyncYielder::new(yielder, waker, coroutine_dispatcher.puff());
            // coroutine_yielder.replace(async_yielder.as_pointer());
            let mut mutex_changer = coroutine_yielder.lock().unwrap();
            *mutex_changer = async_yielder.as_pointer();
            drop(mutex_changer);
            YIELDER.with(|y| {
                *y.borrow_mut() = async_yielder.as_pointer();
            });
            let _ = async_yielder;
            let finished = Some(f());
            yielder.suspend(finished);

        });

        Ok(Self {
            ref_yielder,
            dispatcher,
            context,
            generator: Some(Cell::new(generator)),
        })
    }

    fn pre_post_poll(&self) {
        YIELDER.with(|y| {
            *y.borrow_mut() = self.ref_yielder.lock().unwrap().clone();
        });

        DISPATCHER.with(|d| {
            *d.borrow_mut() = Some(self.dispatcher.puff())
        });
    }
}

impl<'a, Stack> Future for AsyncWormhole<'a, Stack>
where
    Stack: stack::Stack + Unpin + Send,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.pre_post_poll();
        // self.context.enter_context().unwrap_or(());

        let result = self
            .generator
            .as_mut()
            .unwrap()
            .get_mut()
            .resume(cx.waker().clone());



        match result {
            // If we call the future after it completed it will always return Poll::Pending.
            // But polling a completed future is either way undefined behaviour.
            CoroutineResult::Return(()) | CoroutineResult::Yield(None) => {
                // self.pre_post_poll();
                // self.context.exit_context().unwrap_or(());
                // println!("Exiting pycontext pending");
                Poll::Pending
            },
            CoroutineResult::Yield(Some(out)) => {
                // Poll one last time to finish the generator
                self.generator
                    .as_mut()
                    .unwrap()
                    .get_mut()
                    .resume(cx.waker().clone());
                // self.context.exit_context().unwrap_or(());
                // println!("Exiting pycontext ready");
                Poll::Ready(out)
            }
        }
    }
}


impl<'a, Stack> Drop for AsyncWormhole<'a, Stack>
where
    Stack: stack::Stack + Send
{
    fn drop(&mut self) {
        // Dropping a generator can cause an unwind and execute code inside of the separate context.
        // In this regard it's similar to a `poll` call and we need to fire pre and post poll hooks.
        // Note, that we **don't** do a last `post_poll` call once the generator is dropped.
        if let Some(generator) = self.generator.as_mut() {
            if generator.get_mut().started() && !generator.get_mut().done() {
                self.pre_post_poll()
            }
        }
    }
}

#[derive(Clone)]
pub struct AsyncYielder<'a> {
    yielder: &'a Yielder<Waker, Option<()>>,
    waker: Waker,
    dispatcher: RuntimeDispatcher
}

impl<'a> AsyncYielder<'a> {
    pub(crate) fn new(yielder: &'a Yielder<Waker, Option<()>>, waker: Waker, dispatcher: RuntimeDispatcher) -> Self {
        Self { yielder, waker, dispatcher }
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
                Poll::Ready(result) => {
                    return result
                },
            };
        }
    }

    pub fn as_pointer(&self) -> *mut AsyncYielder<'static> {
        (self as *const AsyncYielder) as usize as *mut AsyncYielder<'static>
    }
}
