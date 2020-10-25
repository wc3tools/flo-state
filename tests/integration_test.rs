use flo_state::*;
use async_trait::async_trait;

#[tokio::test]
async fn test_send() {
  let container = Container::new(State { value: 0 });
  let addr = container.addr();

  for _ in 0..10_usize {
    addr.send(Add).await.unwrap();
  }

  for _ in 0..20_usize {
    addr.send(Sub).await.unwrap();
  }

  let value = addr.send(GetValue).await.unwrap();
  assert_eq!(value, -10);

  let state = container.into_state().await.unwrap();
  assert_eq!(state.value, -10)
}

#[tokio::test]
async fn test_spawn() {
  use tokio::sync::mpsc::*;
  use tokio::sync::oneshot;
  let (mut tx, mut rx) = channel::<oneshot::Sender<()>>(1);

  let task = async move {
    while let Some(tx) = rx.recv().await {
      tx.send(()).unwrap();
    }
  };

  let container = Container::new(());
  container.spawn(task);

  for _ in 0..3 {
    let (t, r) = oneshot::channel();
    tx.send(t).await.unwrap();
    r.await.unwrap();
  }

  drop(container);
  tokio::time::delay_for(std::time::Duration::from_secs(1)).await;

  let (t, _r) = oneshot::channel();
  tx.send(t).await.err().unwrap();
}

struct State {
  value: i64,
}

struct Add;
impl Message for Add {
  type Result = ();
}

#[async_trait]
impl Handler<Add> for State {
  async fn handle(&mut self, _: Add) {
    self.value += 1
  }
}

struct Sub;
impl Message for Sub {
  type Result = ();
}

#[async_trait]
impl Handler<Sub> for State {
  async fn handle(&mut self, _: Sub) {
    self.value -= 1
  }
}

struct GetValue;
impl Message for GetValue {
  type Result = i64;
}

#[async_trait]
impl Handler<GetValue> for State {
  async fn handle(&mut self, _: GetValue) -> i64 {
    self.value
  }
}