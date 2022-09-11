use crate::runtime::async_suspend;
use tokio::sync::oneshot;

pub struct Sender<T>(oneshot::Sender<T>);

impl<T> Sender<T> {
    pub fn send(mut self, val: T) -> Result<(), T> {
        self.0.send(val)
    }
}

pub struct Receiver<T>(oneshot::Receiver<T>);

impl<T> Receiver<T> {
    pub fn recv(self) -> Result<T, oneshot::error::RecvError> {
        async_suspend(self.0)
    }
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (send, rec) = oneshot::channel();
    (Sender(send), Receiver(rec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::start_runtime_and_run;
    use crate::tasks::dispatcher::Dispatcher;

    #[test]
    fn check_oneshot() {
        let dispatcher = Dispatcher::default();
        let (sender, recv) = channel();
        dispatcher.dispatch(|| Ok(sender.send(42).unwrap_or(())));
        let r = start_runtime_and_run(|| Ok(recv.recv().unwrap_or(0))).unwrap();
        assert_eq!(r, 42)
    }
}
