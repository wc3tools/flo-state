pub mod error;
pub mod mock;
pub mod registry;
pub mod reply;

pub use async_trait::async_trait;
pub use registry::{Deferred, Registry, RegistryError, RegistryRef, Service};

use crate::mock::MockMessage;
use error::{Error, Result};
use flo_task::{SpawnScope, SpawnScopeHandle};
use std::future::Future;
use std::time::Duration;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender;
use tokio::sync::mpsc::error::SendTimeoutError;

#[async_trait]
pub trait Actor: Send + Sized + 'static {
  async fn started(&mut self, _ctx: &mut Context<Self>) {}
  async fn stopped(self) {}
  fn start(self) -> Container<Self> {
    Container::new(self)
  }
}

pub trait Message: Send + 'static {
  type Result: Send + 'static;
}

#[async_trait]
pub trait Handler<M>: Send + Sized
where
  M: Message,
{
  async fn handle(&mut self, ctx: &mut Context<Self>, message: M) -> M::Result;
}

pub struct Recipient<M> {
  tx: Arc<dyn MessageSender<M>>,
}

impl<M> Clone for Recipient<M> {
  fn clone(&self) -> Self {
    Self {
      tx: self.tx.clone(),
    }
  }
}

impl<M> Recipient<M>
where
  M: Message,
{
  pub async fn send(&self, message: M) -> Result<M::Result> {
    self.tx.send(message).await
  }
  pub async fn send_timeout(&self, timeout: Duration, message: M) -> Result<M::Result> {
    self.tx.send_timeout(timeout, message).await
  }
}

#[async_trait]
trait MessageSender<M>
where
  M: Message,
  Self: Send + Sync,
{
  async fn send(&self, message: M) -> Result<M::Result>;
  async fn send_timeout(&self, timeout: Duration, message: M) -> Result<M::Result>;
}

#[async_trait]
impl<S, M> MessageSender<M> for Addr<S>
where
  S: Handler<M>,
  M: Message,
{
  async fn send(&self, message: M) -> Result<M::Result> {
    Addr::send(self, message).await
  }
  async fn send_timeout(&self, timeout: Duration, message: M) -> Result<M::Result> {
    Addr::send_timeout(self, timeout, message).await
  }
}

type ItemReplySender<T> = Sender<T>;

#[async_trait]
trait ItemObj<S>: Send + 'static {
  fn as_mock_message(&mut self) -> MockMessage;
  async fn handle(&mut self, state: &mut S, ctx: &mut Context<S>);
}

struct MsgItem<M>
  where
    M: Message,
{
  message: M,
  tx: ItemReplySender<M::Result>,
}

impl<M> MsgItem<M>
  where
    M: Message,
{
  async fn handle<S>(self, state: &mut S, ctx: &mut Context<S>)
    where
      S: Handler<M>,
  {
    self.tx.send(state.handle(ctx, self.message).await).ok();
  }
}

#[async_trait]
impl<S, M> ItemObj<S> for Option<MsgItem<M>>
  where
    S: Handler<M>,
    M: Message,
{
  fn as_mock_message(&mut self) -> MockMessage {
    let MsgItem { message, tx } = self.take().expect("item already consumed");
    MockMessage {
      message: Box::new(Some(message)),
      tx: Some(Box::new(Some(tx))),
    }
  }

  async fn handle(&mut self, state: &mut S, ctx: &mut Context<S>) {
    if let Some(item) = self.take() {
      item.handle(state, ctx).await;
    }
  }
}

struct NotifyItem<M>
  where M: Message
{
  message: M,
}

#[async_trait]
impl<S, M> ItemObj<S> for Option<NotifyItem<M>>
  where
    S: Handler<M>,
    M: Message<Result = ()>,
{
  fn as_mock_message(&mut self) -> MockMessage {
    let NotifyItem { message} = self.take().expect("item already consumed");
    MockMessage {
      message: Box::new(Some(message)),
      tx: None,
    }
  }

  async fn handle(&mut self, state: &mut S, ctx: &mut Context<S>) {
    if let Some(item) = self.take() {
      state.handle(ctx, item.message).await;
    }
  }
}

#[derive(Debug)]
pub struct Addr<S> {
  tx: mpsc::Sender<ContainerMessage<S>>,
}

impl<S> Clone for Addr<S> {
  fn clone(&self) -> Self {
    Addr {
      tx: self.tx.clone(),
    }
  }
}

impl<S> Addr<S> {
  pub fn recipient<M>(self) -> Recipient<M>
  where
    S: Handler<M> + 'static,
    M: Message,
  {
    Recipient { tx: Arc::new(self) }
  }

  /// Sends a message to the actor and wait for the result
  pub async fn send<M>(&self, message: M) -> Result<M::Result>
  where
    M: Message + Send + 'static,
    S: Handler<M>,
  {
    send(&self.tx, message, None).await
  }

  /// Sends a message to the actor and wait for the result, but only for a limited time.
  pub async fn send_timeout<M>(&self, timeout: Duration, message: M) -> Result<M::Result>
    where
      M: Message + Send + 'static,
      S: Handler<M>,
  {
    send(&self.tx, message, Some(timeout)).await
  }

  /// Sends a message to the actor without waiting for the result
  pub async fn notify<M>(&self, message: M) -> Result<()>
    where
      M: Message<Result = ()> + Send + 'static,
      S: Handler<M>,
  {
    notify(&self.tx, message, None).await
  }

  /// Sends a message to the actor without waiting for the result, but only for a limited time.
  pub async fn notify_timeout<M>(&self, timeout: Duration, message: M) -> Result<()>
    where
      M: Message<Result = ()> + Send + 'static,
      S: Handler<M>,
  {
    notify(&self.tx, message, Some(timeout)).await
  }
}

pub struct Context<S> {
  tx: mpsc::Sender<ContainerMessage<S>>,
  scope: SpawnScopeHandle,
}

impl<S> Context<S> {
  pub fn addr(&self) -> Addr<S> {
    Addr {
      tx: self.tx.clone(),
    }
  }

  /// Spawns a future into the container.
  /// All futures spawned into the container will be cancelled if the container dropped.
  pub fn spawn<F>(&self, f: F)
  where
    F: Future<Output = ()> + Send + 'static,
  {
    self.scope.spawn(f);
  }
}

#[derive(Debug)]
pub struct Container<S> {
  tx: mpsc::Sender<ContainerMessage<S>>,
  scope: SpawnScope,
}

impl<S> Container<S>
where
  S: Actor,
{
  pub fn new(initial_state: S) -> Self {
    let scope = SpawnScope::new();
    let (tx, mut rx) = mpsc::channel(8);

    tokio::spawn({
      let mut ctx = Context {
        tx: tx.clone(),
        scope: scope.handle(),
      };
      async move {
        let mut state = initial_state;

        state.started(&mut ctx).await;

        loop {
          tokio::select! {
            _ = ctx.scope.left() => {
              state.stopped().await;
              break;
            }
            Some(item) = rx.recv() => {
              match item {
                ContainerMessage::Item(mut item) => {
                  item.handle(&mut state, &mut ctx).await;
                }
                ContainerMessage::Terminate(tx) => {
                  rx.close();

                  // drain messages
                  while let Ok(item) = rx.try_recv() {
                    match item {
                      ContainerMessage::Item(mut item) => {
                        item.handle(&mut state, &mut ctx).await;
                      }
                      ContainerMessage::Terminate(_) => unreachable!(),
                    }
                  }

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

  pub fn addr(&self) -> Addr<S> {
    Addr {
      tx: self.tx.clone(),
    }
  }

  /// Sends a message to the actor and wait for the result
  pub async fn send<M>(&self, message: M) -> Result<M::Result>
  where
    M: Message + Send + 'static,
    S: Handler<M>,
  {
    send(&self.tx, message, None).await
  }

  /// Sends a message to the actor and wait for the result, but only for a limited time.
  pub async fn send_timeout<M>(&self, timeout: Duration, message: M) -> Result<M::Result>
    where
      M: Message + Send + 'static,
      S: Handler<M>,
  {
    send(&self.tx, message, Some(timeout)).await
  }

  /// Sends a message to the actor without waiting for the result
  pub async fn notify<M>(&self, message: M) -> Result<()>
  where
    M: Message<Result = ()> + Send + 'static,
    S: Handler<M>,
  {
    notify(&self.tx, message, None).await
  }

  /// Sends a message to the actor without waiting for the result, but only for a limited time.
  pub async fn notify_timeout<M>(&self, timeout: Duration, message: M) -> Result<()>
    where
      M: Message<Result = ()> + Send + 'static,
      S: Handler<M>,
  {
    notify(&self.tx, message, Some(timeout)).await
  }


  /// Spawns a future into the container.
  /// All futures spawned into the container will be cancelled if the container dropped.
  pub fn spawn<F>(&self, f: F)
  where
    F: Future<Output = ()> + Send + 'static,
  {
    self.scope.spawn(f);
  }

  pub async fn shutdown(mut self) -> Result<S> {
    let (tx, rx) = oneshot::channel();
    self
      .tx
      .send(ContainerMessage::Terminate(tx))
      .await
      .map_err(|_| Error::WorkerGone)?;
    rx.await.map_err(|_| Error::WorkerGone)
  }
}

async fn send<S, M>(tx: &mpsc::Sender<ContainerMessage<S>>, message: M, timeout: Option<Duration>) -> Result<M::Result>
where
  S: Handler<M>,
  M: Message + Send + 'static,
{
  let (reply_tx, reply_rx) = oneshot::channel();
  let boxed = Box::new(Some(MsgItem {
    message,
    tx: reply_tx,
  }));

  if let Some(timeout) = timeout {
    tx.clone()
      .send_timeout(ContainerMessage::Item(boxed), timeout)
      .await
      .map_err(|err| match err {
        SendTimeoutError::Timeout(_) => {
          Error::SendTimeout
        }
        SendTimeoutError::Closed(_) => {
          Error::WorkerGone
        }
      })?;
  } else {
    tx.clone()
      .send(ContainerMessage::Item(boxed))
      .await
      .map_err(|_| Error::WorkerGone)?;
  }
  let res = reply_rx.await.map_err(|_| Error::WorkerGone)?;
  Ok(res)
}

async fn notify<S, M>(tx: &mpsc::Sender<ContainerMessage<S>>, message: M, timeout: Option<Duration>) -> Result<()>
  where
    S: Handler<M>,
    M: Message<Result = ()> + Send + 'static,
{
  let boxed = Box::new(Some(NotifyItem {
    message,
  }));
  if let Some(timeout) = timeout {
    tx.clone()
      .send_timeout(ContainerMessage::Item(boxed), timeout)
      .await
      .map_err(|err| match err {
        SendTimeoutError::Timeout(_) => {
          Error::SendTimeout
        }
        SendTimeoutError::Closed(_) => {
          Error::WorkerGone
        }
      })?;
  } else {
    tx.clone()
      .send(ContainerMessage::Item(boxed))
      .await
      .map_err(|_| Error::WorkerGone)?;
  }
  Ok(())
}

enum ContainerMessage<S> {
  Item(Box<dyn ItemObj<S>>),
  Terminate(oneshot::Sender<S>),
}

impl Actor for () {}