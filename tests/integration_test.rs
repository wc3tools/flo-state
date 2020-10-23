use flo_state::*;
use futures::FutureExt;

#[tokio::test]
async fn test_state() {
  struct State {
    value: i64,
  }

  struct Add;
  impl Message for Add {
    type Result = ();
  }

  impl Handler<Add> for State {
    type Result = ();

    fn handle(&mut self, _: Add) -> Self::Result {
      self.value += 1
    }
  }

  struct Sub;
  impl Message for Sub {
    type Result = ();
  }

  impl Handler<Sub> for State {
    type Result = ();

    fn handle(&mut self, _: Sub) -> Self::Result {
      self.value -= 1
    }
  }

  struct GetValue;
  impl Message for GetValue {
    type Result = i64;
  }

  impl Handler<GetValue> for State {
    type Result = i64;

    fn handle(&mut self, _: GetValue) -> Self::Result {
      self.value
    }
  }

  struct GetValueAsync;
  impl Message for GetValueAsync {
    type Result = i64;
  }

  impl Handler<GetValueAsync> for State {
    type Result = FutureResponse<i64>;

    fn handle(&mut self, _: GetValueAsync) -> Self::Result {
      let value = self.value;
      async move {
        value
      }.boxed()
    }
  }

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

  let value = addr.send(GetValueAsync).await.unwrap();
  assert_eq!(value, -10);

  let state = container.into_state().await.unwrap();
  assert_eq!(state.value, -10)
}
