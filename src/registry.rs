use async_trait::async_trait;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Weak};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{Actor, Addr, Container};

#[derive(Error, Debug)]
pub enum RegistryError {
  #[error("registry gone")]
  RegistryGone,
}

#[derive(Debug)]
struct State<D> {
  data: D,
  map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

#[derive(Debug)]
pub struct Registry<D = ()> {
  state: Arc<Mutex<State<D>>>,
}

impl<D> Registry<D> {
  pub fn new() -> Self
  where
    D: Default,
  {
    Self::with_data(Default::default())
  }

  pub fn with_data(data: D) -> Self {
    Self {
      state: Arc::new(Mutex::new(State {
        data,
        map: HashMap::new(),
      })),
    }
  }
}

pub struct RegistryRef<D = ()> {
  r: Weak<Mutex<State<D>>>,
  guard: OwnedMutexGuard<State<D>>,
}

impl<D> RegistryRef<D> {
  pub fn deferred<S>(&self) -> Deferred<S, D>
  where
    S: Service<D>,
  {
    Deferred::new(self.r.clone())
  }

  pub async fn resolve<S>(&mut self) -> Result<Addr<S>, S::Error>
  where
    S: Service<D>,
  {
    let type_id = TypeId::of::<S>();
    if let Some(addr) = self
      .guard
      .map
      .get(&type_id)
      .map(|v| v.downcast_ref::<Container<S>>().unwrap().addr())
    {
      return Ok(addr);
    }
    let container = S::create(self).await?.start();
    let addr = container.addr();
    self.guard.map.insert(type_id, Box::new(container));
    Ok(addr)
  }

  pub fn data(&self) -> &D {
    &self.guard.data
  }
}

#[async_trait]
pub trait Service<D = ()>: Actor {
  type Error: From<RegistryError>;
  async fn create(registry: &mut RegistryRef<D>) -> Result<Self, Self::Error>;
}

impl<D> Registry<D> {
  pub fn deferred<S>(&self) -> Deferred<S, D>
  where
    S: Service<D>,
  {
    Deferred::new(Arc::downgrade(&self.state))
  }

  pub async fn resolve<S>(&self) -> Result<Addr<S>, S::Error>
  where
    S: Service<D>,
  {
    let guard = self.state.clone().lock_owned().await;
    let mut r = RegistryRef {
      r: Arc::downgrade(&self.state),
      guard,
    };
    r.resolve().await
  }
}

#[derive(Debug)]
pub struct Deferred<S, D = ()> {
  r: Weak<Mutex<State<D>>>,
  resolved: Option<Addr<S>>,
  _phantom: PhantomData<S>,
}

impl<S, D> Deferred<S, D>
where
  S: Service<D>,
{
  fn new(r: Weak<Mutex<State<D>>>) -> Self {
    Self {
      r,
      resolved: None,
      _phantom: PhantomData,
    }
  }

  pub async fn resolve(&mut self) -> Result<Addr<S>, S::Error> {
    if let Some(addr) = self.resolved.clone() {
      Ok(addr)
    } else {
      let map = self
        .r
        .upgrade()
        .ok_or_else(|| S::Error::from(RegistryError::RegistryGone))?;
      let guard = map.lock_owned().await;
      let mut registry = RegistryRef {
        r: self.r.clone(),
        guard,
      };
      let addr = registry.resolve::<S>().await?;
      self.resolved = Some(addr.clone());
      Ok(addr)
    }
  }
}

impl<S, D> Clone for Deferred<S, D> {
  fn clone(&self) -> Self {
    Self {
      r: self.r.clone(),
      resolved: self.resolved.clone(),
      _phantom: PhantomData,
    }
  }
}

#[cfg(test)]
mod test {
  use crate::registry::RegistryRef;
  use crate::*;

  #[tokio::test]
  async fn test_registry() {
    use once_cell::sync::OnceCell;

    struct Data {
      id: i32,
    }

    struct Dep;
    impl Actor for Dep {}
    #[async_trait]
    impl Service<Data> for Dep {
      type Error = registry::RegistryError;

      async fn create(_: &mut RegistryRef<Data>) -> Result<Self, Self::Error> {
        static CREATED: OnceCell<bool> = OnceCell::new();

        CREATED.set(true).unwrap();

        Ok(Dep)
      }
    }

    struct Number {
      dep_deferred: Deferred<Dep, Data>,
      value: i32,
    }
    impl Actor for Number {}
    struct Increase;
    impl Message for Increase {
      type Result = ();
    }
    #[async_trait]
    impl Handler<Increase> for Number {
      async fn handle(&mut self, _: &mut Context<Self>, _: Increase) {
        self.value += 1;
      }
    }
    struct GetValue;
    impl Message for GetValue {
      type Result = i32;
    }
    #[async_trait]
    impl Handler<GetValue> for Number {
      async fn handle(&mut self, _: &mut Context<Self>, _: GetValue) -> i32 {
        self.value
      }
    }

    struct Resolve;
    impl Message for Resolve {
      type Result = Addr<Dep>;
    }

    #[async_trait]
    impl Handler<Resolve> for Number {
      async fn handle(&mut self, _ctx: &mut Context<Self>, _message: Resolve) -> Addr<Dep> {
        self.dep_deferred.resolve().await.unwrap()
      }
    }

    #[async_trait]
    impl Service<Data> for Number {
      type Error = registry::RegistryError;

      async fn create(registry: &mut RegistryRef<Data>) -> Result<Self, Self::Error> {
        let _dep = registry.resolve::<Dep>().await?;

        assert_eq!(registry.data().id, 42);

        Ok(Number {
          dep_deferred: registry.deferred(),
          value: 0,
        })
      }
    }

    let registry = Registry::with_data(Data { id: 42 });
    registry
      .resolve::<Number>()
      .await
      .unwrap()
      .send(Increase)
      .await
      .unwrap();
    registry
      .resolve::<Number>()
      .await
      .unwrap()
      .send(Increase)
      .await
      .unwrap();
    registry
      .resolve::<Number>()
      .await
      .unwrap()
      .send(Increase)
      .await
      .unwrap();

    let value = registry
      .resolve::<Number>()
      .await
      .unwrap()
      .send(GetValue)
      .await
      .unwrap();

    let number = registry.resolve::<Number>().await.unwrap();

    let _dep = number.send(Resolve).await.unwrap();

    assert_eq!(value, 0 + 1 + 1 + 1);
  }
}
