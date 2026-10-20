use thiserror::Error;

#[derive(Error, Debug)]
pub enum NerveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Platform error: {0}")]
    Platform(String),
}

pub type Result<T> = std::result::Result<T, NerveError>;
