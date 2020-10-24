mod error;
use error::{Result, Error};

use std::future::Future;
use futures::{FutureExt, StreamExt};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::mpsc;
use tokio::sync::oneshot::Sender;
use flo_task::SpawnScope;

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;

pub type FutureResponse<T> = BoxFuture<'static, T>;

pub trait Message {
  type Result: Send + 'static;
}

pub trait MessageResponse<M>
  where M: Message
{
  fn handle(self, tx: Option<oneshot::Sender<M::Result>>);
}

macro_rules! impl_sync_message_response {
  ($($t:ty),*) => {
    $(
      impl<M> MessageResponse<M> for $t
      where M: Message<Result = Self>,
      {
        fn handle(self, tx: Option<Sender<<M as Message>::Result>>) {
          if let Some(tx) = tx {
            tx.send(self).ok();
          }
        }
      }
    )*
  };
}

impl_sync_message_response!((), u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64, String, bool);

impl<M> MessageResponse<M> for FutureResponse<M::Result>
  where M: Message,
{
  fn handle(self, tx: Option<Sender<<M as Message>::Result>>) {
    tokio::spawn(async move {
      let res = self.await;
      if let Some(tx) = tx {
        tx.send(res).ok();
      }
    });
  }
}

pub trait Handler<M>
  where M: Message,
{
  type Result: MessageResponse<M>;

  fn handle(&mut self, message: M) -> Self::Result;
}

struct Item<M>
  where M: Message
{
  message: M,
  tx: oneshot::Sender<Option<M::Result>>,
}

trait ItemObj<S>: Send + 'static {
  fn handle(&mut self, state: &mut S) -> HandleResult;
}

impl<S, M> ItemObj<S> for Option<Item<M>>
  where
    S: Handler<M>,
    M: Message + Send + 'static,
{
  fn handle(&mut self, state: &mut S) -> HandleResult {
    if let Some(item) = self.take() {
      item.handle(state)
    } else {
      HandleResult::Sync
    }
  }
}

impl<M> Item<M>
  where
    M: Message
{
  fn handle<S>(self, state: &mut S) -> HandleResult
    where S: Handler<M>
  {
    let r = state.handle(self.message);
    let (tx, mut rx) = oneshot::channel();
    r.handle(Some(tx));
    match rx.try_recv() {
      Ok(res) => {
        self.tx.send(Some(res)).ok();
        HandleResult::Sync
      },
      Err(TryRecvError::Empty) => {
        let tx = self.tx;
        HandleResult::Async(async move {
          tx.send(rx.await.ok()).ok();
        }.boxed())
      },
      Err(TryRecvError::Closed) => {
        self.tx.send(None).ok();
        HandleResult::Sync
      },
    }
  }
}

pub enum HandleResult {
  Sync,
  Async(FutureResponse<()>)
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
                  match item.handle(&mut state) {
                    HandleResult::Sync => {},
                    HandleResult::Async(f) => { f.await },
                  }
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
  let res = reply_rx.await.map_err(|_| Error::WorkerGone)?.ok_or_else(|| Error::WorkerGone)?;
  Ok(res)
}

enum ContainerMessage<S> {
  Item(Box<dyn ItemObj<S>>),
  Terminate(oneshot::Sender<S>),
}