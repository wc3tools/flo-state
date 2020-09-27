//! Async state management by message passing

pub mod error;
use error::{Error, Result};

use flo_task::SpawnScope;
use futures::FutureExt;
use std::future::Future;
use tokio::sync::mpsc::{channel, Sender};
use tokio::sync::oneshot;

#[derive(Debug)]
pub struct Container<M, S> {
  tx: Sender<InternalMessage<M, S>>,
  scope: SpawnScope,
}

impl<M, S> Container<M, S> {
  pub fn new<H>(initial: S, handler: H) -> Self
  where
    M: Send + 'static,
    S: Send + 'static,
    H: Handler<S, Message = M> + Send + 'static,
  {
    let scope = SpawnScope::new();
    let (tx, mut rx) = channel(8);

    tokio::spawn({
      let mut scope = scope.handle();
      async move {
        let mut state = initial;
        let mut handler = handler;
        loop {
          tokio::select! {
            _ = scope.left() => {
              break;
            }
            Some(msg) = rx.recv() => {
              match msg {
                InternalMessage::Message(msg, tx) => {
                  handler.handle(&mut state, msg);
                  if let Some(tx) = tx {
                    tx.send(());
                  }
                }
                InternalMessage::Batch(msgs, tx) => {
                  for msg in msgs {
                    handler.handle(&mut state, msg);
                  }
                  if let Some(tx) = tx {
                    tx.send(());
                  }
                }
                InternalMessage::Terminate(tx) => {
                  tx.send(state).ok();
                  break;
                }
              }
            }
          }
        }
      }
    });

    Self { tx, scope }
  }

  pub fn handle(&self) -> Handle<M, S> {
    Handle(self.tx.clone())
  }

  pub async fn into_state(mut self) -> Result<S> {
    let (tx, rx) = oneshot::channel();
    self
      .tx
      .send(InternalMessage::Terminate(tx))
      .await
      .map_err(|_| Error::WorkerGone)?;
    rx.await.map_err(|_| Error::WorkerGone)
  }
}

#[derive(Debug)]
pub struct Handle<M, S>(Sender<InternalMessage<M, S>>);
impl<M, S> Handle<M, S> {
  pub async fn send(&mut self, msg: M) -> Result<(), Error> {
    self
      .0
      .send(InternalMessage::Message(msg, None))
      .await
      .map_err(|_| Error::WorkerGone)?;
    Ok(())
  }

  pub async fn send_and_wait(&mut self, msg: M) -> Result<(), Error> {
    let (tx, rx) = ReplyChannel::new();
    self
      .0
      .send(InternalMessage::Message(msg, Some(tx)))
      .await
      .map_err(|_| Error::WorkerGone)?;
    rx.await;
    Ok(())
  }

  pub async fn send_batch(&mut self, batch: Vec<M>) -> Result<(), Error> {
    self
      .0
      .send(InternalMessage::Batch(batch, None))
      .await
      .map_err(|_| Error::WorkerGone)?;
    Ok(())
  }

  pub async fn send_batch_and_wait(&mut self, batch: Vec<M>) -> Result<(), Error> {
    let (tx, rx) = ReplyChannel::new();
    self
      .0
      .send(InternalMessage::Batch(batch, Some(tx)))
      .await
      .map_err(|_| Error::WorkerGone)?;
    rx.await;
    Ok(())
  }
}

impl<M, S> Clone for Handle<M, S> {
  fn clone(&self) -> Self {
    Handle(self.0.clone())
  }
}

pub trait Handler<S> {
  type Message;

  fn handle(&mut self, state: &mut S, msg: Self::Message);
}

enum InternalMessage<M, S> {
  Message(M, Option<ReplyChannel<()>>),
  Batch(Vec<M>, Option<ReplyChannel<()>>),
  Terminate(oneshot::Sender<S>),
}

/// One-shot reply channel
/// Reply will be discarded if the underlying is broken
#[derive(Debug)]
pub struct ReplyChannel<M>(oneshot::Sender<M>);
impl<M> ReplyChannel<M> {
  pub fn new() -> (Self, impl Future<Output = Option<M>>) {
    let (tx, rx) = oneshot::channel();
    (Self(tx), rx.map(Result::ok))
  }

  pub fn send(self, msg: M) {
    self.0.send(msg).ok();
  }
}

#[tokio::test]
async fn test_state() {
  enum Msg {
    Add,
    Sub,
    GetValue { tx: ReplyChannel<i64> },
  }
  struct State {
    value: i64,
  }
  struct TestHandler;
  impl Handler<State> for TestHandler {
    type Message = Msg;

    fn handle(&mut self, state: &mut State, msg: Self::Message) {
      match msg {
        Msg::Add => {
          state.value += 1;
        }
        Msg::Sub => {
          state.value -= 1;
        }
        Msg::GetValue { tx } => tx.send(state.value),
      }
    }
  }

  let container = Container::new(State { value: 0 }, TestHandler);
  let mut handle = container.handle();

  for _ in 0..10_usize {
    handle.send(Msg::Add).await.unwrap();
  }

  for _ in 0..20_usize {
    handle.send(Msg::Sub).await.unwrap();
  }

  let (tx, rx) = ReplyChannel::new();
  handle.send(Msg::GetValue { tx }).await.unwrap();
  let value = rx.await;
  assert_eq!(value, Some(-10));

  let state = container.into_state().await.unwrap();
  assert_eq!(state.value, -10)
}
