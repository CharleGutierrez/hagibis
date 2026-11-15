//! # Encrypted Swarm Transport Channels & Cryptographic Primitives
//!
//! Production-grade security layer for Hagibis P2P mesh:
//! - Ed25519 cryptographic node identities using `libp2p::identity`.
//! - Authenticated RPC envelope with RFC 2104 HMAC-SHA256 and Ed25519 digital signatures.
//! - Monotonic nonces with strict replay attack protection.
//! - Timestamp skew tolerance verification against replay windows.
//! - Multiplexed channels: Task Gossip, Telemetry Broadcast, MicroVM Work Stealing, Raft Consensus.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::identity::{Keypair, PublicKey};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{MeshError, Result};

/// Maximum allowable clock skew in milliseconds (30 seconds)
pub const MAX_TIMESTAMP_SKEW_MS: i64 = 30_000;

/// Multiplexed swarm communication channels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwarmChannelType {
    /// P2P distributed agent task gossip and scheduling
    TaskGossip,
    /// Live cluster telemetry, node capacity, and thermal headroom broadcast
    TelemetryBroadcast,
    /// MicroVM distributed work-stealing and load balancing requests
    MicroVmWorkStealing,
    /// High-priority Raft consensus log replication and leader election
    RaftConsensus,
    /// Direct node-to-node encrypted RPC conduit
    DirectRpc,
}

impl SwarmChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TaskGossip => "task-gossip",
            Self::TelemetryBroadcast => "telemetry-broadcast",
            Self::MicroVmWorkStealing => "microvm-work-stealing",
            Self::RaftConsensus => "raft-consensus",
            Self::DirectRpc => "direct-rpc",
        }
    }
}

/// Cryptographic Node Identity wrapping a libp2p Ed25519 Keypair
#[derive(Clone)]
pub struct NodeIdentity {
    keypair: Keypair,
}

impl NodeIdentity {
    /// Generate a fresh cryptographically secure Ed25519 identity
    pub fn generate() -> Self {
        Self {
            keypair: Keypair::generate_ed25519(),
        }
    }

    /// Access the underlying libp2p keypair reference
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }

    /// Get the libp2p PeerId derived from the public key
    pub fn peer_id(&self) -> PeerId {
        self.keypair.public().to_peer_id()
    }

    /// String representation of the node's PeerId
    pub fn peer_id_string(&self) -> String {
        self.peer_id().to_string()
    }

    /// Public key for this node
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public()
    }

    /// Export keypair as protobuf-encoded bytes
    pub fn to_protobuf_encoding(&self) -> Result<Vec<u8>> {
        self.keypair
            .to_protobuf_encoding()
            .map_err(|e| MeshError::TransportSecurity(format!("Keypair serialization failed: {}", e)))
    }

    /// Restore keypair from protobuf-encoded bytes
    pub fn from_protobuf_encoding(bytes: &[u8]) -> Result<Self> {
        let keypair = Keypair::from_protobuf_encoding(bytes)
            .map_err(|e| MeshError::TransportSecurity(format!("Keypair deserialization failed: {}", e)))?;
        Ok(Self { keypair })
    }

    /// Digitally sign arbitrary bytes using the node's Ed25519 private key
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        self.keypair
            .sign(data)
            .map_err(|e| MeshError::TransportSecurity(format!("Ed25519 signing failed: {}", e)))
    }

    /// Verify an Ed25519 signature against an arbitrary public key
    pub fn verify_signature(public_key: &PublicKey, data: &[u8], signature: &[u8]) -> Result<()> {
        if public_key.verify(data, signature) {
            Ok(())
        } else {
            Err(MeshError::SignatureVerificationFailed(
                "Invalid Ed25519 digital signature".to_string(),
            ))
        }
    }
}

/// Computes SHA-256 hash digest of a byte slice
pub fn compute_sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Computes hex-encoded SHA-256 hash string
pub fn compute_sha256_hex(data: &[u8]) -> String {
    let digest = compute_sha256(data);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Pure Rust implementation of RFC 2104 HMAC-SHA256 with constant-time equality
pub fn compute_hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let hashed_key = compute_sha256(key);
        k[..32].copy_from_slice(&hashed_key);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    // Inner hash: H(K ^ ipad || data)
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    // Outer hash: H(K ^ opad || inner_hash)
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    outer.finalize().into()
}

/// Constant-time verification of HMAC-SHA256 authentication tag
pub fn verify_hmac_sha256_constant_time(key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
    let computed = compute_hmac_sha256(key, data);
    let mut diff = 0u8;
    for (a, b) in computed.iter().zip(expected.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Security header attached to every inter-node swarm message
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HgbSecurityHeader {
    /// Originating node PeerId
    pub sender_id: String,
    /// Destination node PeerId
    pub recipient_id: String,
    /// Channel multiplexing identifier
    pub channel: SwarmChannelType,
    /// Strictly monotonic sequence nonce per peer channel
    pub nonce: u64,
    /// Milliseconds since UNIX epoch
    pub timestamp_ms: u64,
    /// RFC 2104 HMAC-SHA256 authentication tag
    pub hmac_sha256: [u8; 32],
    /// Ed25519 signature over canonical header contents + payload
    pub source_signature: Vec<u8>,
}

impl HgbSecurityHeader {
    /// Builds canonical byte representation for HMAC and signature computation
    pub fn canonical_bytes(
        sender_id: &str,
        recipient_id: &str,
        channel: SwarmChannelType,
        nonce: u64,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(sender_id.len() + recipient_id.len() + payload.len() + 64);
        bytes.extend_from_slice(sender_id.as_bytes());
        bytes.push(0x3A); // ':'
        bytes.extend_from_slice(recipient_id.as_bytes());
        bytes.push(0x3A);
        bytes.extend_from_slice(channel.as_str().as_bytes());
        bytes.push(0x3A);
        bytes.extend_from_slice(&nonce.to_be_bytes());
        bytes.push(0x3A);
        bytes.extend_from_slice(&timestamp_ms.to_be_bytes());
        bytes.push(0x3A);
        bytes.extend_from_slice(payload);
        bytes
    }
}

/// Authenticated and signed envelope transmitted over libp2p swarm streams
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecureEnvelope {
    pub header: HgbSecurityHeader,
    pub payload: Vec<u8>,
}

impl SecureEnvelope {
    /// Seal and cryptographically sign an envelope for a recipient node
    pub fn seal(
        identity: &NodeIdentity,
        shared_hmac_secret: &[u8],
        recipient_id: &str,
        channel: SwarmChannelType,
        nonce: u64,
        payload: &[u8],
    ) -> Result<Self> {
        let sender_id = identity.peer_id_string();
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let canonical = HgbSecurityHeader::canonical_bytes(
            &sender_id,
            recipient_id,
            channel,
            nonce,
            timestamp_ms,
            payload,
        );

        // 1. Calculate HMAC-SHA256 over canonical data
        let hmac = compute_hmac_sha256(shared_hmac_secret, &canonical);

        // 2. Digitally sign canonical bytes + HMAC with Ed25519 identity key
        let mut signable = Vec::with_capacity(canonical.len() + 32);
        signable.extend_from_slice(&canonical);
        signable.extend_from_slice(&hmac);
        let signature = identity.sign(&signable)?;

        Ok(Self {
            header: HgbSecurityHeader {
                sender_id,
                recipient_id: recipient_id.to_string(),
                channel,
                nonce,
                timestamp_ms,
                hmac_sha256: hmac,
                source_signature: signature,
            },
            payload: payload.to_vec(),
        })
    }

    /// Verify cryptographic envelope authenticity, reject replays, and unwrap payload
    pub fn verify_and_open<'a>(
        &'a self,
        shared_hmac_secret: &[u8],
        sender_public_key: &PublicKey,
        nonce_validator: &mut NonceValidator,
    ) -> Result<&'a [u8]> {
        // 1. Check timestamp skew tolerance
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let delta = (now_ms as i64) - (self.header.timestamp_ms as i64);
        if delta.abs() > MAX_TIMESTAMP_SKEW_MS {
            return Err(MeshError::TimestampSkewed { delta_ms: delta });
        }

        // 2. Validate monotonic nonce to prevent replay attacks
        nonce_validator.validate_and_record(&self.header.sender_id, self.header.nonce)?;

        // 3. Reconstruct canonical bytes
        let canonical = HgbSecurityHeader::canonical_bytes(
            &self.header.sender_id,
            &self.header.recipient_id,
            self.header.channel,
            self.header.nonce,
            self.header.timestamp_ms,
            &self.payload,
        );

        // 4. Verify HMAC-SHA256 tag
        if !verify_hmac_sha256_constant_time(
            shared_hmac_secret,
            &canonical,
            &self.header.hmac_sha256,
        ) {
            return Err(MeshError::HmacMismatch);
        }

        // 5. Verify Ed25519 digital signature
        let mut signable = Vec::with_capacity(canonical.len() + 32);
        signable.extend_from_slice(&canonical);
        signable.extend_from_slice(&self.header.hmac_sha256);

        NodeIdentity::verify_signature(sender_public_key, &signable, &self.header.source_signature)?;

        Ok(&self.payload)
    }
}

/// Tracks per-peer monotonic nonce sequences to prevent replay attacks
#[derive(Debug, Default, Clone)]
pub struct NonceValidator {
    /// Highest observed nonce per peer
    peer_nonces: HashMap<String, u64>,
}

impl NonceValidator {
    pub fn new() -> Self {
        Self {
            peer_nonces: HashMap::new(),
        }
    }

    /// Validate that nonce is strictly monotonic for the given peer, and record it
    pub fn validate_and_record(&mut self, peer_id: &str, nonce: u64) -> Result<()> {
        if let Some(&last_nonce) = self.peer_nonces.get(peer_id) {
            if nonce <= last_nonce {
                return Err(MeshError::ReplayDetected {
                    peer_id: peer_id.to_string(),
                    nonce,
                });
            }
        }

        self.peer_nonces.insert(peer_id.to_string(), nonce);
        Ok(())
    }

    /// Reset or remove tracked nonce for disconnected peer
    pub fn remove_peer(&mut self, peer_id: &str) {
        self.peer_nonces.remove(peer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_identity_generation_and_ed25519_signatures() {
        let identity = NodeIdentity::generate();
        let peer_id = identity.peer_id();
        assert!(!peer_id.to_string().is_empty());

        let message = b"Hagibis MicroVM State Migration Proof";
        let signature = identity.sign(message).expect("Sign should succeed");
        assert_eq!(signature.len(), 64);

        let pubkey = identity.public_key();
        NodeIdentity::verify_signature(&pubkey, message, &signature)
            .expect("Valid signature must verify");

        let corrupted_message = b"Hagibis MicroVM Tampered";
        assert!(NodeIdentity::verify_signature(&pubkey, corrupted_message, &signature).is_err());
    }

    #[test]
    fn test_node_identity_protobuf_roundtrip() {
        let identity1 = NodeIdentity::generate();
        let proto_bytes = identity1.to_protobuf_encoding().expect("Serialize keypair");
        let identity2 = NodeIdentity::from_protobuf_encoding(&proto_bytes).expect("Deserialize keypair");

        assert_eq!(identity1.peer_id(), identity2.peer_id());
    }

    #[test]
    fn test_hmac_sha256_computation_and_constant_time_verification() {
        let key = b"super-secret-cluster-mesh-hmac-key";
        let message = b"Raft consensus state payload";

        let tag = compute_hmac_sha256(key, message);
        assert!(verify_hmac_sha256_constant_time(key, message, &tag));

        let wrong_key = b"wrong-key";
        assert!(!verify_hmac_sha256_constant_time(wrong_key, message, &tag));

        let mut corrupted_tag = tag;
        corrupted_tag[0] ^= 0xFF;
        assert!(!verify_hmac_sha256_constant_time(key, message, &corrupted_tag));
    }

    #[test]
    fn test_secure_envelope_sealing_and_verification() {
        let sender = NodeIdentity::generate();
        let recipient = NodeIdentity::generate();
        let shared_secret = b"hagibis-secure-mesh-shared-secret";
        let mut validator = NonceValidator::new();

        let payload = b"{\"action\":\"spawn_microvm\",\"vcpus\":4,\"ram_mb\":2048}";
        let envelope = SecureEnvelope::seal(
            &sender,
            shared_secret,
            &recipient.peer_id_string(),
            SwarmChannelType::TaskGossip,
            1,
            payload,
        )
        .expect("Envelope sealing must succeed");

        assert_eq!(envelope.header.sender_id, sender.peer_id_string());
        assert_eq!(envelope.header.recipient_id, recipient.peer_id_string());
        assert_eq!(envelope.header.nonce, 1);

        let unwrapped = envelope
            .verify_and_open(shared_secret, &sender.public_key(), &mut validator)
            .expect("Unwrap must succeed");

        assert_eq!(unwrapped, payload);
    }

    #[test]
    fn test_secure_envelope_tamper_rejection() {
        let sender = NodeIdentity::generate();
        let recipient = NodeIdentity::generate();
        let shared_secret = b"cluster-secret-key-12345";
        let mut validator = NonceValidator::new();

        let payload = b"Original uncorrupted microvm payload";
        let mut envelope = SecureEnvelope::seal(
            &sender,
            shared_secret,
            &recipient.peer_id_string(),
            SwarmChannelType::MicroVmWorkStealing,
            1,
            payload,
        )
        .unwrap();

        // 1. Tamper with payload
        envelope.payload[0] ^= 0xAA;
        let res = envelope.verify_and_open(shared_secret, &sender.public_key(), &mut validator);
        assert!(res.is_err(), "Tampered payload must be rejected by HMAC");

        // Restore payload and tamper with nonce
        envelope.payload[0] ^= 0xAA;
        envelope.header.nonce = 42;
        let res2 = envelope.verify_and_open(shared_secret, &sender.public_key(), &mut validator);
        assert!(res2.is_err(), "Tampered nonce must be rejected");
    }

    #[test]
    fn test_replay_attack_prevention() {
        let sender = NodeIdentity::generate();
        let recipient = NodeIdentity::generate();
        let shared_secret = b"cluster-secret-key-replay-test";
        let mut validator = NonceValidator::new();

        let payload = b"Transfer microvm #42 to target node";

        // First message with nonce 1
        let env1 = SecureEnvelope::seal(
            &sender,
            shared_secret,
            &recipient.peer_id_string(),
            SwarmChannelType::RaftConsensus,
            1,
            payload,
        )
        .unwrap();

        env1.verify_and_open(shared_secret, &sender.public_key(), &mut validator)
            .expect("First envelope must succeed");

        // Replay attack: try to send env1 again
        let replay_res = env1.verify_and_open(shared_secret, &sender.public_key(), &mut validator);
        assert!(
            matches!(replay_res, Err(MeshError::ReplayDetected { .. })),
            "Replaying same envelope must trigger ReplayDetected error"
        );

        // Regression attack: send message with lower nonce (e.g., 0)
        let env_regress = SecureEnvelope::seal(
            &sender,
            shared_secret,
            &recipient.peer_id_string(),
            SwarmChannelType::RaftConsensus,
            0,
            payload,
        )
        .unwrap();

        let regress_res = env_regress.verify_and_open(shared_secret, &sender.public_key(), &mut validator);
        assert!(matches!(regress_res, Err(MeshError::ReplayDetected { .. })));

        // Monotonic next nonce 2 must succeed
        let env2 = SecureEnvelope::seal(
            &sender,
            shared_secret,
            &recipient.peer_id_string(),
            SwarmChannelType::RaftConsensus,
            2,
            payload,
        )
        .unwrap();

        env2.verify_and_open(shared_secret, &sender.public_key(), &mut validator)
            .expect("Monotonic next nonce 2 must succeed");
    }

    #[test]
    fn test_timestamp_skew_rejection() {
        let sender = NodeIdentity::generate();
        let recipient = NodeIdentity::generate();
        let shared_secret = b"cluster-secret-key-skew-test";
        let mut validator = NonceValidator::new();

        let payload = b"MicroVM work stealing ping";
        let mut envelope = SecureEnvelope::seal(
            &sender,
            shared_secret,
            &recipient.peer_id_string(),
            SwarmChannelType::TelemetryBroadcast,
            1,
            payload,
        )
        .unwrap();

        // Artificially make timestamp 2 minutes in the past
        envelope.header.timestamp_ms -= 120_000;

        let res = envelope.verify_and_open(shared_secret, &sender.public_key(), &mut validator);
        assert!(
            matches!(res, Err(MeshError::TimestampSkewed { .. })),
            "Skewed timestamp must be rejected"
        );
    }
}
