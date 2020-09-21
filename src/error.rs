use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
  #[error("worker gone")]
  WorkerGone,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;