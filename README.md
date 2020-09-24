# flo-state

Async state management library.

```rust
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

  let mut container = Container::new(State { value: 0 }, TestHandler);
  for _ in 0..10_usize {
    container.send(Msg::Add).await.unwrap();
  }

  for _ in 0..20_usize {
    container.send(Msg::Sub).await.unwrap();
  }

  let (tx, rx) = ReplyChannel::new();
  container.send(Msg::GetValue { tx }).await.unwrap();
  let value = rx.await;
  assert_eq!(value, Some(-10));

  let state = container.into_state().await.unwrap();
  assert_eq!(state.value, -10)
}
```
