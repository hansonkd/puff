//! Multiple Producer, Single Consumer Channel
//!
//! Useful for having many tasks send multiple values to one task. Receivers keep awaiting new messages
//! until all senders have been dropped.
//!
//! If a `Receiver` is dropped, Senders can no longer send messages.
//!
//! See [tokio::sync::mpsc] for more information about unbounded mpsc channels.
//!
//! # Example
//!
//! ```no_run
//! use puff::channels::mpsc::channel;
//! use puff::tasks::Task;
//! use puff::types::Puff;
//!
//! let (sender, mut recv) = channel();
//! let sender2 = sender.puff(); // Create a new reference to `Sender`
//!
//! let task1 = Task::spawn(move || {
//!     sender.send(42)?;
//!     Ok(())
//! });
//!
//! Task::spawn(move || {
//!     task1.join()?; // Wait for first task to complete.
//!     sender2.send(420)?;
//!     Ok(())
//! });
//!
//! assert_eq!(recv.recv(), Some(42));
//! assert_eq!(recv.recv(), Some(420));
//! assert_eq!(recv.recv(), None);
//! ```
use crate::runtime::yield_to_future;
use crate::types::Puff;
use tokio::sync::mpsc;

/// Create a new mpsc Sender. Can send multiple values and is a `Puff` type for cheap clones.
#[derive(Clone)]
pub struct Sender<T>(mpsc::UnboundedSender<T>);

impl<T> Sender<T> {
    /// Send a message across the channel. An error will be returned if the Receiver was dropped.
    pub fn send(&self, val: T) -> Result<(), mpsc::error::SendError<T>> {
        self.0.send(val)
    }
}

/// Create a new mpsc Receiver. Expects zero or more values.
pub struct Receiver<T>(mpsc::UnboundedReceiver<T>);

impl<T> Receiver<T> {
    /// Yield the current coroutine until a message is ready.
    pub fn recv(&mut self) -> Option<T> {
        let fut = self.0.recv();
        yield_to_future(fut)
    }
}

/// Create a new mpsc channel with a Sender and a Receiver.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (send, rec) = mpsc::unbounded_channel();
    (Sender(send), Receiver(rec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::PuffContext;
    use crate::runtime::start_runtime_and_run;

    #[test]
    fn check_mpsc() {
        let dispatcher = PuffContext::default();
        let (sender, mut recv) = channel();
        dispatcher.dispatch(move || {
            sender.send(42).unwrap_or(());
            Ok(sender.send(43).unwrap_or(()))
        });
        let r = start_runtime_and_run(move || {
            let mut res = Vec::new();
            res.push(recv.recv().unwrap_or(0));
            res.push(recv.recv().unwrap_or(0));
            res.push(recv.recv().unwrap_or(0));
            Ok(res)
        })
        .unwrap();
        assert_eq!(r, vec![42, 43, 0])
    }
}

impl<T: Puff> Puff for Sender<T> {}
