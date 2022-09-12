//! OneShot Channel
//!
//! Useful for sending one value to another Task. Once the value is sent, the channel is closed
//! and no more values can be sent.
//!
//! See [tokio::sync::oneshot] for more information about oneshot channels.
//!
//! # Example
//!
//! ```no_run
//! use puff::channels::oneshot::channel;
//! use puff::tasks::Task;
//! let (sender, mut recv) = channel();
//! Task::spawn(|| {
//!     sender.send(42).unwrap_or(());
//!     Ok(())
//! });
//!
//! assert_eq!(recv.recv().unwrap(), 42);
//! ```
//!
use crate::errors::Error;
use crate::runtime::yield_to_future;
use tokio::sync::oneshot;

/// Create a new oneshot Sender. Sends exactly one value.
pub struct Sender<T>(oneshot::Sender<T>);

impl<T> Sender<T> {
    /// Send a message across the channel.
    ///
    /// If a Receiver was dropped or a value was already sent, the original value will be returned
    /// in an `Err`.
    pub fn send(self, val: T) -> Result<(), T> {
        self.0.send(val)
    }
}

/// A oneshot Receiver. Expects exactly one value.
pub struct Receiver<T>(oneshot::Receiver<T>);

impl<T> Receiver<T> {
    /// Yield the coroutine until a value is ready.
    pub fn recv(self) -> Result<T, Error> {
        yield_to_future(self.0).map_err(|e| e.into())
    }
}

/// Create a new oneshot channel with a `Sender` and a `Receiver`.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (send, rec) = oneshot::channel();
    (Sender(send), Receiver(rec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::dispatcher::RuntimeDispatcher;
    use crate::runtime::start_runtime_and_run;

    #[test]
    fn check_oneshot() {
        let dispatcher = RuntimeDispatcher::default();
        let (sender, recv) = channel();
        dispatcher.dispatch(|| Ok(sender.send(42).unwrap_or(())));
        let r = start_runtime_and_run(|| Ok(recv.recv().unwrap_or(0))).unwrap();
        assert_eq!(r, 42)
    }
}
