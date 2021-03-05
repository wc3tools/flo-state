use async_trait::async_trait;
use flo_state::*;

#[tokio::test]
async fn test_send() {
  let container = Owner::new(State { value: 0 });
  let addr = container.addr();

  for _ in 0..10_usize {
    addr.send(Add).await.unwrap();
  }

  for _ in 0..20_usize {
    addr.send(Sub).await.unwrap();
  }

  let value = addr.send(GetValue).await.unwrap();
  assert_eq!(value, -10);

  let state = container.shutdown().await.unwrap();
  assert_eq!(state.value, -10)
}

#[tokio::test]
async fn test_spawn() {
  use tokio::sync::mpsc::*;
  use tokio::sync::oneshot;
  let (tx, mut rx) = channel::<oneshot::Sender<()>>(1);

  let task = async move {
    while let Some(tx) = rx.recv().await {
      tx.send(()).unwrap();
    }
  };

  let container = Owner::new(());
  container.spawn(task);

  for _ in 0..3 {
    let (t, r) = oneshot::channel();
    tx.send(t).await.unwrap();
    r.await.unwrap();
  }

  drop(container);
  tokio::time::sleep(std::time::Duration::from_secs(1)).await;

  let (t, _r) = oneshot::channel();
  tx.send(t).await.err().unwrap();
}

#[tokio::test]
async fn test_callbacks() {
  use tokio::sync::oneshot;
  struct Callbacks {
    started: Option<oneshot::Sender<()>>,
    stopped: oneshot::Sender<()>,
  }

  #[async_trait]
  impl Actor for Callbacks {
    async fn started(&mut self, _: &mut Context<Self>) {
      self.started.take().unwrap().send(()).ok();
    }

    async fn stopped(self) {
      self.stopped.send(()).ok();
    }
  }

  let (started, rx_started) = oneshot::channel();
  let (stopped, rx_stopped) = oneshot::channel();

  let state = Callbacks {
    started: Some(started),
    stopped,
  };

  let owner = state.start();
  drop(owner);

  rx_started.await.unwrap();
  rx_stopped.await.unwrap();
}

#[tokio::test]
async fn test_notify() {
  use std::sync::Arc;
  use tokio::sync::Notify;

  struct A;
  impl Actor for A {}

  struct M(usize, Addr<A>, Arc<Notify>);
  impl Message for M {
    type Result = ();
  }

  #[async_trait]
  impl Handler<M> for A {
    async fn handle(
      &mut self,
      ctx: &mut Context<Self>,
      M(n, reply_addr, done): M,
    ) -> <M as Message>::Result {
      if n < 10 {
        reply_addr.notify(M(n + 1, ctx.addr(), done)).await.unwrap();
      } else {
        done.notify_one();
      }
    }
  }

  let done = Arc::new(Notify::new());

  let a1 = A.start();
  let a2 = A.start();

  let m = M(0, a2.addr(), done.clone());
  a1.notify(m).await.unwrap();

  done.notified().await;

  a1.shutdown().await.unwrap();
  a2.shutdown().await.unwrap();
}

struct State {
  value: i64,
}

impl Actor for State {}

struct Add;
impl Message for Add {
  type Result = ();
}

#[async_trait]
impl Handler<Add> for State {
  async fn handle(&mut self, _: &mut Context<Self>, _: Add) {
    self.value += 1
  }
}

struct Sub;
impl Message for Sub {
  type Result = ();
}

#[async_trait]
impl Handler<Sub> for State {
  async fn handle(&mut self, _: &mut Context<Self>, _: Sub) {
    self.value -= 1
  }
}

struct GetValue;
impl Message for GetValue {
  type Result = i64;
}

#[async_trait]
impl Handler<GetValue> for State {
  async fn handle(&mut self, _: &mut Context<Self>, _: GetValue) -> i64 {
    self.value
  }
}
