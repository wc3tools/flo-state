use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum Error {
  #[error("worker gone")]
  WorkerGone,
  #[error("send timeout")]
  SendTimeout,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
