use thiserror::Error;

#[derive(Error, Debug)]
pub enum HgbError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Daemon IPC error: {0}")]
    Ipc(String),

    #[error("Security violation: {0}")]
    Security(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl HgbError {
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Execution(msg.into())
    }

    pub fn syntax(msg: impl Into<String>) -> Self {
        Self::Execution(msg.into())
    }

    pub fn serialization(msg: impl Into<String>) -> Self {
        Self::Serialization(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, HgbError>;

