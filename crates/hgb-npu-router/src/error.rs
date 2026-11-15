//! # Error Definitions for NPU Router

use thiserror::Error;

#[derive(Error, Debug)]
pub enum NpuRouterError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Hagibis Core error: {0}")]
    Core(#[from] hgb_core::HgbError),

    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Execution error: {0}")]
    ExecutionFailed(String),

    #[error("Governor script failure: {0}")]
    GovernorFailure(String),

    #[error("Cryptographic signature mismatch: {0}")]
    InvalidSignature(String),

    #[error("Thermal safeguard triggered: {0}")]
    ThermalSafeguardEngaged(String),

    #[error("Buffer dimension mismatch: expected {expected}, found {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
}

pub type Result<T> = std::result::Result<T, NpuRouterError>;