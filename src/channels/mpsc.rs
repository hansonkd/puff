use crate::runtime::async_suspend;
use tokio::sync::mpsc;

pub struct Sender<T>(mpsc::UnboundedSender<T>);

impl<T> Sender<T> {
    pub fn send(&self, val: T) -> Result<(), mpsc::error::SendError<T>> {
        self.0.send(val)
    }
}

pub struct Receiver<T>(mpsc::UnboundedReceiver<T>);

impl<T> Receiver<T> {
    pub fn recv(&mut self) -> Option<T> {
        let fut = self.0.recv();
        async_suspend(fut)
    }
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (send, rec) = mpsc::unbounded_channel();
    (Sender(send), Receiver(rec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::start_runtime_and_run;
    use crate::tasks::dispatcher::Dispatcher;

    #[test]
    fn check_mpsc() {
        let dispatcher = Dispatcher::default();
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
