use crate::{
  Addr, ContainerMessage, Handler, ItemReplySender,
  Message,
};
use futures::future::{abortable, AbortHandle, BoxFuture};
use futures::FutureExt;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::future::Future;
use tokio::sync::mpsc;

type BoxedHandler = Box<dyn FnMut(MockMessage) -> BoxFuture<'static, ()> + Send>;

pub struct Mock<S> {
  tx: mpsc::Sender<ContainerMessage<S>>,
  abort_handle: AbortHandle,
  _t: PhantomData<S>,
}

impl<S> Mock<S> {
  pub fn builder() -> MockBuilder<S> {
    MockBuilder::new()
  }

  pub fn addr(&self) -> Addr<S> {
    Addr {
      tx: self.tx.clone(),
    }
  }
}

impl<S> Drop for Mock<S> {
  fn drop(&mut self) {
    self.abort_handle.abort()
  }
}

pub(crate) struct MockMessage {
  pub(crate) message: Box<dyn Any + Send>,
  pub(crate) tx: Box<dyn Any + Send>,
}

pub struct MockBuilder<S> {
  handler_map: HashMap<TypeId, BoxedHandler>,
  _t: PhantomData<S>,
}

impl<S> MockBuilder<S> {
  fn new() -> Self {
    Self {
      handler_map: HashMap::new(),
      _t: PhantomData,
    }
  }

  pub fn handle<M, F, R>(mut self, mut f: F) -> Self
  where
    M: Message + Sync,
    S: Handler<M>,
    F: FnMut(&M) -> R + Send + 'static,
    R: Future<Output = M::Result> + Send + 'static
  {
    self.handler_map.insert(
      TypeId::of::<M>(),
      Box::new(move |MockMessage { message, mut tx }| {
        let message = message.downcast_ref::<M>().unwrap();
        let tx = tx.downcast_mut::<Option<ItemReplySender<M::Result>>>().unwrap().take().unwrap();
        let task = f(message);
        async move {
          tx.send(task.await).ok();
        }.boxed()
      }),
    );
    self
  }

  pub fn build(self) -> Mock<S>
  where
    S: Send + 'static,
  {
    let (tx, mut rx) = mpsc::channel(1);

    let mut handler_map = self.handler_map;

    let (task, abort_handle) = abortable(async move {
      while let Some(msg) = rx.recv().await {
        match msg {
          ContainerMessage::Item(mut boxed) => {
            let mock_message = boxed.as_mock_message();
            let type_id = mock_message.message.as_ref().type_id();
            match handler_map.get_mut(&type_id) {
              Some(handler) => handler(mock_message).await,
              None => panic!("message mock handler not provided: {:?}", type_id),
            }
          }
          ContainerMessage::Terminate(_) => unreachable!(),
        }
      }
    });

    tokio::spawn(task);

    Mock {
      tx,
      abort_handle,
      _t: PhantomData,
    }
  }
}

#[cfg(test)]
mod test {
  use super::*;
  use async_trait::async_trait;
  use crate::{Actor, Context};

  #[tokio::test]
  async fn test_mock() {
    struct TestActor;
    impl Actor for TestActor {}
    struct TestMessage;
    impl Message for TestMessage {
      type Result = i32;
    }
    #[async_trait]
    impl Handler<TestMessage> for TestActor {
      async fn handle(&mut self, _ctx: &mut Context<Self>, _message: TestMessage) -> <TestMessage as Message>::Result {
        0
      }
    }

    let actor = TestActor;
    let actor = actor.start();

    assert_eq!(actor.send(TestMessage).await.unwrap(), 0);

    let mut value = 41;
    let mock = Mock::<TestActor>::builder().handle(move |_: &TestMessage| {
      value += 1;
      async move {
        value
      }
    }).build();

    assert_eq!(mock.addr().send(TestMessage).await.unwrap(), 42);
    assert_eq!(mock.addr().send(TestMessage).await.unwrap(), 43);
    assert_eq!(mock.addr().send(TestMessage).await.unwrap(), 44);
    assert_eq!(mock.addr().send(TestMessage).await.unwrap(), 45);
  }
}