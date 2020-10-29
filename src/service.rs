use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::marker::PhantomData;
use async_trait::async_trait;
use futures::lock::Mutex;
use crate::{Actor, Addr, Container};

#[derive(Clone, Debug)]
pub struct Registry {
  map: Arc<Mutex<HashMap<TypeId, Box<dyn Any + Send>>>>,
}

impl Registry {
  pub fn new() -> Self {
    Registry {
      map: Arc::new(Mutex::new(HashMap::new()))
    }
  }
}

#[async_trait]
pub trait Service: Actor {
  type Error;
  async fn create(registry: &Registry) -> Result<Self, Self::Error>;
}

impl Registry {
  pub fn deferred(&self) -> Deferred<Self> {
    Deferred {
      registry: self.clone(),
      _t: PhantomData
    }
  }

  pub async fn resolve<S>(&self) -> Result<Addr<S>, S::Error>
  where S: Service
  {
    use std::collections::hash_map::Entry;
    let mut guard = self.map.lock().await;
    let addr = match guard.entry(TypeId::of::<S>()) {
      Entry::Vacant(e) => {
        let container = S::create(self).await?.start();
        let addr = container.addr();
        e.insert(Box::new(container));
        addr
      },
      Entry::Occupied(e) => e.get().downcast_ref::<Container<S>>().unwrap().addr(),
    };
    Ok(addr)
  }
}

#[derive(Debug)]
pub struct Deferred<S> {
  registry: Registry,
  _t: PhantomData<S>,
}

impl<S> Deferred<S>
where S: Service
{
  pub async fn resolve(&self) -> Result<Addr<S>, S::Error> {
    self.registry.resolve().await
  }
}

#[cfg(test)]
mod test {
  use crate::*;

  #[tokio::test]
  async fn test_service() {
    struct Number(i32);
    impl Actor for Number {}
    struct Increase;
    impl Message for Increase {
      type Result = ();
    }
    #[async_trait]
    impl Handler<Increase> for Number {
      async fn handle(&mut self, _: &mut Context<Self>, _: Increase) {
        self.0 += 1;
      }
    }
    struct GetValue;
    impl Message for GetValue {
      type Result = i32;
    }
    #[async_trait]
    impl Handler<GetValue> for Number {
      async fn handle(&mut self, _: &mut Context<Self>, _: GetValue) -> i32 {
        self.0
      }
    }
    #[async_trait]
    impl Service for Number {
      type Error = ();

      async fn create(_registry: &Registry) -> Result<Self, Self::Error> {
        Ok(Number(0))
      }
    }

    let registry = Registry::new();
    registry.resolve::<Number>().await.unwrap().send(Increase).await.unwrap();
    registry.resolve::<Number>().await.unwrap().send(Increase).await.unwrap();
    registry.resolve::<Number>().await.unwrap().send(Increase).await.unwrap();

    let value = registry.resolve::<Number>().await.unwrap().send(GetValue).await.unwrap();
    assert_eq!(value, 0 + 1 + 1 + 1);
  }
}