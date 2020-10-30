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

type Map = HashMap<TypeId, Box<dyn Any + Send + Sync>>;

#[derive(Debug)]
pub struct Registry {
  map: Arc<Mutex<Map>>,
}

impl Registry {
  pub fn new() -> Self {
    Registry {
      map: Arc::new(Mutex::new(HashMap::new())),
    }
  }
}

pub struct RegistryRef {
  r: Weak<Mutex<Map>>,
  map: OwnedMutexGuard<Map>,
}

impl RegistryRef {
  pub fn deferred<S>(&self) -> Deferred<S>
  where
    S: Service,
  {
    Deferred::new(self.r.clone())
  }

  pub async fn resolve<S>(&mut self) -> Result<Addr<S>, S::Error>
  where
    S: Service,
  {
    let type_id = TypeId::of::<S>();
    if let Some(addr) = self
      .map
      .get(&type_id)
      .map(|v| v.downcast_ref::<Container<S>>().unwrap().addr())
    {
      return Ok(addr);
    }
    let container = S::create(self).await?.start();
    let addr = container.addr();
    self.map.insert(type_id, Box::new(container));
    Ok(addr)
  }
}

#[async_trait]
pub trait Service: Actor {
  type Error: From<RegistryError>;
  async fn create(registry: &mut RegistryRef) -> Result<Self, Self::Error>;
}

impl Registry {
  pub fn deferred<S>(&self) -> Deferred<S>
  where
    S: Service,
  {
    Deferred::new(Arc::downgrade(&self.map))
  }

  pub async fn resolve<S>(&self) -> Result<Addr<S>, S::Error>
  where
    S: Service,
  {
    let guard = self.map.clone().lock_owned().await;
    let mut r = RegistryRef {
      r: Arc::downgrade(&self.map),
      map: guard,
    };
    r.resolve().await
  }
}

#[derive(Debug)]
pub struct Deferred<S> {
  r: Weak<Mutex<Map>>,
  resolved: Option<Addr<S>>,
  _phantom: PhantomData<S>,
}

impl<S> Deferred<S>
where
  S: Service,
{
  fn new(r: Weak<Mutex<Map>>) -> Self {
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
        map: guard,
      };
      let addr = registry.resolve::<S>().await?;
      self.resolved = Some(addr.clone());
      Ok(addr)
    }
  }
}

impl<S> Clone for Deferred<S> {
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

    struct Dep;
    impl Actor for Dep {}
    #[async_trait]
    impl Service for Dep {
      type Error = registry::RegistryError;

      async fn create(_: &mut RegistryRef) -> Result<Self, Self::Error> {
        static CREATED: OnceCell<bool> = OnceCell::new();

        CREATED.set(true).unwrap();

        Ok(Dep)
      }
    }

    struct Number {
      dep_deferred: Deferred<Dep>,
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
    impl Service for Number {
      type Error = registry::RegistryError;

      async fn create(registry: &mut RegistryRef) -> Result<Self, Self::Error> {
        let _dep = registry.resolve::<Dep>().await?;
        Ok(Number {
          dep_deferred: registry.deferred(),
          value: 0,
        })
      }
    }

    let registry = Registry::new();
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
