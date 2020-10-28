use std::future::Future;
use std::task;
use futures::task::Context;
use std::pin::Pin;
use tokio::sync::oneshot;
use futures::FutureExt;

pub struct FutureReply<T>(oneshot::Receiver<T>);

impl<T> FutureReply<T> {
  pub fn channel() -> (FutureReplySender<T>, FutureReply<T>) {
    let (tx, rx) = oneshot::channel();
    (FutureReplySender(tx), FutureReply(rx))
  }
}

pub struct FutureReplySender<T>(oneshot::Sender<T>);
impl<T> FutureReplySender<T> {
  pub fn send(self, value: T) -> Result<(), T> {
    self.0.send(value)
  }
}

impl<T> Future for FutureReply<T> {
  type Output = Option<T>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> task::Poll<Self::Output> {
    self.0.poll_unpin(cx).map(|res| res.ok())
  }
}