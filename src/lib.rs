mod error;
use error::{Result, Error};

use std::future::Future;
use futures::{FutureExt, StreamExt};
use tokio::sync::oneshot;
use tokio::sync::mpsc;
use tokio::sync::oneshot::Sender;
use async_trait::async_trait;
use flo_task::SpawnScope;

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;

pub trait Message: Send + 'static {
  type Result: Send + 'static;
}

#[async_trait]
pub trait Handler<M>: Send
  where M: Message,
{
  async fn handle(&mut self, message: M) -> M::Result;
}

struct Item<M>
where M: Message
{
  message: M,
  tx: Sender<M::Result>
}

impl<M> Item<M>
  where
    M: Message
{
  async fn handle<S>(self, state: &mut S)
    where S: Handler<M>
  {
    self.tx.send(state.handle(self.message).await).ok();
  }
}

#[async_trait]
trait ItemObj<S>: Send + 'static {
  async fn handle(&mut self, state: &mut S);
}

#[async_trait]
impl<S, M> ItemObj<S> for Option<Item<M>>
  where
    S: Handler<M>,
    M: Message,
{
  async fn handle(&mut self, state: &mut S) {
    if let Some(item) = self.take() {
      item.handle(state).await;
    }
  }
}

#[derive(Debug, Clone)]
pub struct Addr<S> {
  tx: mpsc::Sender<ContainerMessage<S>>,
  spawn_tx: mpsc::UnboundedSender<BoxFuture<'static, ()>>,
}

impl<S> Addr<S> {
  pub async fn send<M>(&self, message: M) -> Result<M::Result>
    where M: Message + Send + 'static,
          S: Handler<M>
  {
    send(&self.tx, message).await
  }

  /// Spawns a future into the container.
  /// All futures spawned into the container will be cancelled if the container dropped.
  pub fn spawn<F>(&self, f: F)
    where F: Future<Output = ()> + Send + 'static
  {
    self.spawn_tx.send(f.boxed()).ok();
  }
}

#[derive(Debug)]
pub struct Container<S> {
  tx: mpsc::Sender<ContainerMessage<S>>,
  scope: SpawnScope,
  spawn_tx: mpsc::UnboundedSender<BoxFuture<'static, ()>>,
}

impl<S> Container<S>
  where S: Send + 'static
{
  pub fn new(initial: S) -> Self
  {
    let scope = SpawnScope::new();
    let (tx, mut rx) = mpsc::channel(8);
    let (spawn_tx, mut spawn_rx) = mpsc::unbounded_channel();

    tokio::spawn({
      let mut scope = scope.handle();
      async move {
        let mut state = initial;
        let mut tasks = FuturesUnordered::new();
        tasks.push(futures::future::pending().boxed());
        loop {
          tokio::select! {
            _ = scope.left() => {
              break;
            }
            _ = tasks.next() => {}
            Some(item) = rx.recv() => {
              match item {
                ContainerMessage::Item(mut item) => {
                  item.handle(&mut state).await;
                }
                ContainerMessage::Terminate(tx) => {
                  tx.send(state).ok();
                  break;
                }
              }
            }
            Some(f) = spawn_rx.recv() => {
              tasks.push(f);
            }
          }
        }
      }
    });

    Self { tx, scope, spawn_tx }
  }

  pub fn addr(&self) -> Addr<S> {
    Addr {
      tx: self.tx.clone(),
      spawn_tx: self.spawn_tx.clone(),
    }
  }

  pub async fn send<M>(&self, message: M) -> Result<M::Result>
    where M: Message + Send + 'static,
          S: Handler<M>
  {
    send(&self.tx, message).await
  }

  /// Spawns a future into the container.
  /// All futures spawned into the container will be cancelled if the container dropped.
  pub fn spawn<F>(&self, f: F)
  where F: Future<Output = ()> + Send + 'static
  {
    self.spawn_tx.send(f.boxed()).ok();
  }

  pub async fn into_state(mut self) -> Result<S> {
    let (tx, rx) = oneshot::channel();
    self
      .tx
      .send(ContainerMessage::Terminate(tx))
      .await
      .map_err(|_| Error::WorkerGone)?;
    rx.await.map_err(|_| Error::WorkerGone)
  }
}

async fn send<S, M>(tx: &mpsc::Sender<ContainerMessage<S>>, message: M) -> Result<M::Result>
  where S: Handler<M>,
        M: Message + Send + 'static,
{
  let (reply_tx, reply_rx) = oneshot::channel();
  let boxed = Box::new(Some(Item {
    message,
    tx: reply_tx
  }));
  tx.clone().send(ContainerMessage::Item(boxed)).await.map_err(|_| Error::WorkerGone)?;
  let res = reply_rx.await.map_err(|_| Error::WorkerGone)?;
  Ok(res)
}

enum ContainerMessage<S> {
  Item(Box<dyn ItemObj<S>>),
  Terminate(oneshot::Sender<S>),
}