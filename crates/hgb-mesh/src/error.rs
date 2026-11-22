//! Error types for Hagibis Mesh networking, transport security, and consensus.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, MeshError>;

#[derive(Error, Debug)]
pub enum MeshError {
    #[error("Cryptographic transport security failure: {0}")]
    TransportSecurity(String),

    #[error("Ed25519 signature verification failed: {0}")]
    SignatureVerificationFailed(String),

    #[error("Replay attack detected: nonce {nonce} has already been observed or regressed for peer {peer_id}")]
    ReplayDetected { peer_id: String, nonce: u64 },

    #[error("Message timestamp skewed beyond acceptable tolerance: delta {delta_ms}ms")]
    TimestampSkewed { delta_ms: i64 },

    #[error("HMAC authentication mismatch")]
    HmacMismatch,

    #[error("Consensus violation: {0}")]
    Consensus(String),

    #[error("Quorum not reached: required {required}, got {achieved}")]
    QuorumNotReached { required: usize, achieved: usize },

    #[error("MicroVM migration error: {0}")]
    MigrationError(String),

    #[error("State checkpoint integrity hash mismatch: expected {expected}, actual {actual}")]
    CheckpointHashMismatch { expected: String, actual: String },

    #[error("P2P network error: {0}")]
    Network(String),

    #[error("Swarm dial or listen error: {0}")]
    Swarm(String),

    #[error("Node not in leader state")]
    NotLeader,

    #[error("Serialization / Deserialization error: {0}")]
    Serialization(String),

    #[error("Base64 decoding error: {0}")]
    Base64(String),

    #[error("Internal I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Cryptographic failure: {0}")]
    CryptographicFailure(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Lock acquisition failed: {0}")]
    LockError(String),
}
