//! # Cryptographic Provenance Ledger for Autopoietic Patches (`provenance_merkle_ledger`)
//!
//! Production-grade immutable local Merkle-tree provenance ledger that signs every
//! autonomous code modification using Ed25519 keypairs and produces BLAKE3 binary
//! Merkle trees with cryptographic inclusion proofs.
//!
//! ## Core Architecture:
//! 1. **Ed25519 Digital Patch Signing**:
//!    - Binds agent identity, target file, pre/post hashes, AST diff, and audit context.
//!    - Guarantees non-repudiation and detects code tampering.
//! 2. **BLAKE3 Binary Merkle Tree**:
//!    - Canonical serialization and leaf hashing via BLAKE3.
//!    - Full binary Merkle tree with child pairs hashed as `BLAKE3(left || right)`.
//!    - High-performance inclusion proofs (`MerkleProof`) with sibling path traversal.
//! 3. **Append-Only Ledger Persistence**:
//!    - Atomic disk persistence in `.hgb/provenance_ledger.jsonl`.
//!    - Cryptographic signature validation on ledger reload and scheduled audit sweeps.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use blake3::Hasher as Blake3Hasher;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PROVENANCE_LEDGER_PATH: &str = ".hgb/provenance_ledger.jsonl";

// ==============================================================================
// 1. Error Definitions
// ==============================================================================

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceLedgerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Ed25519 signature error: {0}")]
    Ed25519(String),

    #[error("Invalid public key: {0}")]
    InvalidPublicKey(String),

    #[error("Invalid signature format: {0}")]
    InvalidSignature(String),

    #[error("Signature missing on provenance record {patch_id}")]
    SignatureMissing { patch_id: String },

    #[error("Cryptographic signature verification failed for patch {patch_id}: {reason}")]
    SignatureVerificationFailed {
        patch_id: String,
        reason: String,
    },

    #[error("Merkle leaf index {index} is out of bounds (total leaves: {total})")]
    LeafIndexOutOfBounds { index: usize, total: usize },

    #[error("Merkle proof verification failed: expected root {expected}, computed {computed}")]
    MerkleProofMismatch {
        expected: String,
        computed: String,
    },

    #[error("Tampered record detected in ledger at index {index} (patch {patch_id}): {reason}")]
    TamperedRecord {
        index: usize,
        patch_id: String,
        reason: String,
    },

    #[error("Hex decoding error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("Signing key missing: ledger initialized in read-only verification mode")]
    MissingSigningKey,
}

// ==============================================================================
// 2. Author Identity & Patch Provenance Record
// ==============================================================================

/// Cryptographic Author Identity specifying who or what generated the autopoietic patch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorIdentity {
    pub agent_id: String,
    pub model_name: String,
    pub public_key_hex: String,
    pub timestamp_secs: u64,
}

impl AuthorIdentity {
    pub fn new(
        agent_id: impl Into<String>,
        model_name: impl Into<String>,
        public_key_hex: impl Into<String>,
        timestamp_secs: u64,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            model_name: model_name.into(),
            public_key_hex: public_key_hex.into(),
            timestamp_secs,
        }
    }
}

/// Immutable provenance record for an autonomous code modification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchProvenanceRecord {
    pub patch_id: String,
    pub author_identity: AuthorIdentity,
    pub target_crate: String,
    pub target_file: String,
    pub pre_patch_hash: String,
    pub post_patch_hash: String,
    pub patch_diff: String,
    pub patch_ast_fingerprint: String,
    pub audit_context: String,
    pub ed25519_signature: String,
}

impl PatchProvenanceRecord {
    /// Canonical payload bytes for Ed25519 digital signature signing and verification.
    /// Covers all invariant fields of the record excluding the signature itself.
    pub fn canonical_payload(&self) -> Vec<u8> {
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"HGB_AUTONOMOUS_PATCH_PROVENANCE_V1\n");
        hasher.update(self.patch_id.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.author_identity.agent_id.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.author_identity.model_name.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.author_identity.public_key_hex.as_bytes());
        hasher.update(b"\n");
        hasher.update(&self.author_identity.timestamp_secs.to_le_bytes());
        hasher.update(b"\n");
        hasher.update(self.target_crate.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.target_file.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.pre_patch_hash.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.post_patch_hash.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.patch_diff.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.patch_ast_fingerprint.as_bytes());
        hasher.update(b"\n");
        hasher.update(self.audit_context.as_bytes());
        hasher.finalize().as_bytes().to_vec()
    }

    /// Signs this record using an Ed25519 SigningKey, populating `ed25519_signature`.
    pub fn sign(&mut self, signing_key: &SigningKey) {
        let payload = self.canonical_payload();
        let sig = signing_key.sign(&payload);
        self.ed25519_signature = hex::encode(sig.to_bytes());
    }

    /// Verifies the digital signature of this record against its public key.
    pub fn verify_signature(&self) -> Result<(), ProvenanceLedgerError> {
        if self.ed25519_signature.is_empty() {
            return Err(ProvenanceLedgerError::SignatureMissing {
                patch_id: self.patch_id.clone(),
            });
        }

        let pubkey_bytes = hex::decode(&self.author_identity.public_key_hex)?;
        let pubkey_arr: [u8; 32] = pubkey_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProvenanceLedgerError::InvalidPublicKey("Public key length must be 32 bytes".into()))?;
        let verifying_key = VerifyingKey::from_bytes(&pubkey_arr)
            .map_err(|e| ProvenanceLedgerError::InvalidPublicKey(e.to_string()))?;

        let sig_bytes = hex::decode(&self.ed25519_signature)?;
        let sig_arr: [u8; 64] = sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProvenanceLedgerError::InvalidSignature("Signature length must be 64 bytes".into()))?;
        let signature = Signature::from_bytes(&sig_arr);

        let payload = self.canonical_payload();
        verifying_key
            .verify(&payload, &signature)
            .map_err(|e| ProvenanceLedgerError::SignatureVerificationFailed {
                patch_id: self.patch_id.clone(),
                reason: e.to_string(),
            })?;

        Ok(())
    }

    /// Computes the leaf hash from canonical serialized record using BLAKE3.
    pub fn compute_leaf_hash(&self) -> String {
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"HGB_PROVENANCE_LEAF_V1:");
        let json_bytes = serde_json::to_vec(self).unwrap_or_default();
        hasher.update(&json_bytes);
        hasher.finalize().to_hex().to_string()
    }

    /// Computes raw 32-byte leaf hash.
    pub fn compute_leaf_hash_bytes(&self) -> [u8; 32] {
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"HGB_PROVENANCE_LEAF_V1:");
        let json_bytes = serde_json::to_vec(self).unwrap_or_default();
        hasher.update(&json_bytes);
        *hasher.finalize().as_bytes()
    }
}

// ==============================================================================
// 3. Merkle Tree & Inclusion Proofs
// ==============================================================================

/// Cryptographic inclusion proof demonstrating a leaf's membership in a Merkle tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerkleProof {
    pub leaf_index: usize,
    pub leaf_hash: String,
    pub siblings: Vec<String>,
    pub root: String,
}

impl MerkleProof {
    /// Validates this inclusion proof against a specified Merkle root.
    pub fn verify(&self, expected_root: &str) -> bool {
        verify_merkle_proof(&self.leaf_hash, self, expected_root)
    }
}

/// Standalone cryptographic verification for Merkle inclusion proofs.
pub fn verify_merkle_proof(leaf_hash: &str, proof: &MerkleProof, root: &str) -> bool {
    if proof.leaf_hash != leaf_hash || proof.root != root || root.is_empty() {
        return false;
    }

    let leaf_bytes = match hex::decode(leaf_hash) {
        Ok(b) if b.len() == 32 => b,
        _ => return false,
    };
    let mut current = [0u8; 32];
    current.copy_from_slice(&leaf_bytes);

    if proof.siblings.is_empty() {
        return proof.leaf_index == 0 && hex::encode(current) == root;
    }

    let mut idx = proof.leaf_index;
    for sibling_hex in &proof.siblings {
        let sibling_bytes = match hex::decode(sibling_hex) {
            Ok(b) if b.len() == 32 => b,
            _ => return false,
        };
        let mut sibling = [0u8; 32];
        sibling.copy_from_slice(&sibling_bytes);

        let mut concat = [0u8; 64];
        if idx % 2 == 0 {
            // Current node is left child; sibling is right child
            concat[..32].copy_from_slice(&current);
            concat[32..].copy_from_slice(&sibling);
        } else {
            // Current node is right child; sibling is left child
            concat[..32].copy_from_slice(&sibling);
            concat[32..].copy_from_slice(&current);
        }

        current = *blake3::hash(&concat).as_bytes();
        idx /= 2;
    }

    hex::encode(current) == root
}

/// Full binary Merkle Tree over provenance records with BLAKE3 child hashing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvenanceMerkleTree {
    pub leaf_hashes: Vec<String>,
    pub root: String,
}

impl ProvenanceMerkleTree {
    /// Constructs a Merkle tree from a collection of provenance records.
    pub fn from_records(records: &[PatchProvenanceRecord]) -> Self {
        let leaf_hashes: Vec<String> = records.iter().map(|r| r.compute_leaf_hash()).collect();
        Self::from_leaf_hashes(leaf_hashes)
    }

    /// Constructs a Merkle tree from pre-computed leaf hash hex strings.
    pub fn from_leaf_hashes(leaf_hashes: Vec<String>) -> Self {
        if leaf_hashes.is_empty() {
            let empty_root = blake3::hash(b"").to_hex().to_string();
            return Self {
                leaf_hashes,
                root: empty_root,
            };
        }

        if leaf_hashes.len() == 1 {
            return Self {
                root: leaf_hashes[0].clone(),
                leaf_hashes,
            };
        }

        let root = Self::compute_root_from_hashes(&leaf_hashes);
        Self { leaf_hashes, root }
    }

    #[inline]
    fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(left);
        concat[32..].copy_from_slice(right);
        *blake3::hash(&concat).as_bytes()
    }

    fn compute_root_from_hashes(leaf_hashes: &[String]) -> String {
        let mut current_level: Vec<[u8; 32]> = leaf_hashes
            .iter()
            .map(|h| {
                let bytes = hex::decode(h).unwrap_or_default();
                let mut arr = [0u8; 32];
                if bytes.len() == 32 {
                    arr.copy_from_slice(&bytes);
                }
                arr
            })
            .collect();

        while current_level.len() > 1 {
            let mut next_level = Vec::with_capacity((current_level.len() + 1) / 2);
            for i in (0..current_level.len()).step_by(2) {
                let left = &current_level[i];
                let right = if i + 1 < current_level.len() {
                    &current_level[i + 1]
                } else {
                    // Duplicate last node for odd count
                    &current_level[i]
                };
                next_level.push(Self::hash_pair(left, right));
            }
            current_level = next_level;
        }

        hex::encode(current_level[0])
    }

    /// Returns the top-level Merkle root hex string.
    pub fn merkle_root(&self) -> String {
        self.root.clone()
    }

    pub fn root_hex(&self) -> String {
        self.root.clone()
    }

    /// Generates a cryptographic inclusion proof for the leaf at `leaf_index`.
    pub fn generate_merkle_proof(&self, leaf_index: usize) -> Result<MerkleProof, ProvenanceLedgerError> {
        if leaf_index >= self.leaf_hashes.len() {
            return Err(ProvenanceLedgerError::LeafIndexOutOfBounds {
                index: leaf_index,
                total: self.leaf_hashes.len(),
            });
        }

        if self.leaf_hashes.len() == 1 {
            return Ok(MerkleProof {
                leaf_index: 0,
                leaf_hash: self.leaf_hashes[0].clone(),
                siblings: Vec::new(),
                root: self.root.clone(),
            });
        }

        let mut current_level: Vec<[u8; 32]> = self
            .leaf_hashes
            .iter()
            .map(|h| {
                let bytes = hex::decode(h).unwrap_or_default();
                let mut arr = [0u8; 32];
                if bytes.len() == 32 {
                    arr.copy_from_slice(&bytes);
                }
                arr
            })
            .collect();

        let mut siblings = Vec::new();
        let mut idx = leaf_index;

        while current_level.len() > 1 {
            let mut next_level = Vec::with_capacity((current_level.len() + 1) / 2);
            for i in (0..current_level.len()).step_by(2) {
                let right = if i + 1 < current_level.len() {
                    current_level[i + 1]
                } else {
                    current_level[i]
                };

                if i == idx || i + 1 == idx {
                    if idx % 2 == 0 {
                        // Current is left; sibling is right
                        siblings.push(hex::encode(right));
                    } else {
                        // Current is right; sibling is left
                        siblings.push(hex::encode(current_level[i]));
                    }
                }

                next_level.push(Self::hash_pair(&current_level[i], &right));
            }
            idx /= 2;
            current_level = next_level;
        }

        Ok(MerkleProof {
            leaf_index,
            leaf_hash: self.leaf_hashes[leaf_index].clone(),
            siblings,
            root: self.root.clone(),
        })
    }

    /// Verifies an inclusion proof using this tree's root.
    pub fn verify_merkle_proof(&self, leaf_hash: &str, proof: &MerkleProof, root: &str) -> bool {
        verify_merkle_proof(leaf_hash, proof, root)
    }
}

// ==============================================================================
// 4. Append-Only Provenance Ledger
// ==============================================================================

/// Immutable local Merkle-tree provenance ledger managing Ed25519 signing and disk persistence.
pub struct ProvenanceMerkleLedger {
    file_path: PathBuf,
    signing_key: Arc<RwLock<Option<SigningKey>>>,
    verifying_key: Arc<RwLock<Option<VerifyingKey>>>,
    records: Arc<RwLock<Vec<PatchProvenanceRecord>>>,
    default_agent_id: String,
    default_model_name: String,
}

impl ProvenanceMerkleLedger {
    /// Creates a new provenance ledger with a newly generated Ed25519 keypair.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let (signing_key, verifying_key) = Self::generate_keypair();
        Self {
            file_path: path.into(),
            signing_key: Arc::new(RwLock::new(Some(signing_key))),
            verifying_key: Arc::new(RwLock::new(Some(verifying_key))),
            records: Arc::new(RwLock::new(Vec::new())),
            default_agent_id: "hagibis-autopoietic-supervisor".to_string(),
            default_model_name: "hagibis-slm-deepseek-coder".to_string(),
        }
    }

    /// Initializes a ledger with specific signing or verifying keys.
    pub fn with_keys(
        path: impl Into<PathBuf>,
        signing_key: Option<SigningKey>,
        verifying_key: Option<VerifyingKey>,
    ) -> Self {
        let vk = verifying_key.or_else(|| signing_key.as_ref().map(|sk| sk.verifying_key()));
        Self {
            file_path: path.into(),
            signing_key: Arc::new(RwLock::new(signing_key)),
            verifying_key: Arc::new(RwLock::new(vk)),
            records: Arc::new(RwLock::new(Vec::new())),
            default_agent_id: "hagibis-autopoietic-supervisor".to_string(),
            default_model_name: "hagibis-slm-deepseek-coder".to_string(),
        }
    }

    /// Generates a cryptographically secure 32-byte Ed25519 keypair from system entropy.
    pub fn generate_keypair() -> (SigningKey, VerifyingKey) {
        let mut secret = [0u8; 32];
        let mut f = File::open("/dev/urandom").expect("access /dev/urandom for crypto keys");
        f.read_exact(&mut secret).expect("read 32 random bytes for ed25519");
        let signing_key = SigningKey::from_bytes(&secret);
        let verifying_key = signing_key.verifying_key();
        (signing_key, verifying_key)
    }

    /// Loads or creates a ledger from disk, optionally verifying all signatures on load.
    pub fn load_from_file(
        path: impl Into<PathBuf>,
        signing_key: Option<SigningKey>,
        verify_signatures_on_load: bool,
    ) -> Result<Self, ProvenanceLedgerError> {
        let file_path = path.into();
        let ledger = Self::with_keys(file_path.clone(), signing_key, None);

        if file_path.exists() {
            ledger.reload_from_disk(verify_signatures_on_load)?;
        }

        Ok(ledger)
    }

    /// Reloads all records from disk and optionally verifies signatures.
    pub fn reload_from_disk(&self, verify_signatures: bool) -> Result<usize, ProvenanceLedgerError> {
        if !self.file_path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&self.file_path)?;
        let mut loaded = Vec::new();

        for (idx, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let record: PatchProvenanceRecord = serde_json::from_str(line)?;
            if verify_signatures {
                if let Err(e) = record.verify_signature() {
                    return Err(ProvenanceLedgerError::TamperedRecord {
                        index: idx,
                        patch_id: record.patch_id,
                        reason: e.to_string(),
                    });
                }
            }
            loaded.push(record);
        }

        let count = loaded.len();
        let mut records = self.records.write().unwrap();
        *records = loaded;
        Ok(count)
    }

    /// Saves the active Ed25519 signing key to a specified file.
    pub fn save_signing_key(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let sk_guard = self.signing_key.read().unwrap();
        if let Some(sk) = sk_guard.as_ref() {
            if let Some(parent) = path.as_ref().parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(path, sk.to_bytes())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "No signing key present"))
        }
    }

    /// Loads an Ed25519 signing key from a specified file.
    pub fn load_signing_key(path: impl AsRef<Path>) -> io::Result<SigningKey> {
        let bytes = fs::read(path)?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Key file must be 32 bytes"))?;
        Ok(SigningKey::from_bytes(&arr))
    }

    /// Returns the public key hex string of the active keypair.
    pub fn public_key_hex(&self) -> Option<String> {
        self.verifying_key
            .read()
            .unwrap()
            .as_ref()
            .map(|vk| hex::encode(vk.as_bytes()))
    }

    /// Returns the default AuthorIdentity template used for signing new entries.
    pub fn default_author_identity(&self) -> AuthorIdentity {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        AuthorIdentity {
            agent_id: self.default_agent_id.clone(),
            model_name: self.default_model_name.clone(),
            public_key_hex: self.public_key_hex().unwrap_or_default(),
            timestamp_secs: now_secs,
        }
    }

    /// Sets the default agent identifier and model name.
    pub fn set_author_identity(&mut self, agent_id: impl Into<String>, model_name: impl Into<String>) {
        self.default_agent_id = agent_id.into();
        self.default_model_name = model_name.into();
    }

    /// Atomically signs and persists a patch provenance record to the ledger.
    pub fn append_record(
        &self,
        mut record: PatchProvenanceRecord,
    ) -> Result<PatchProvenanceRecord, ProvenanceLedgerError> {
        // Sign if not already signed
        if record.ed25519_signature.is_empty() {
            let sk_guard = self.signing_key.read().unwrap();
            if let Some(sk) = sk_guard.as_ref() {
                record.sign(sk);
            } else {
                return Err(ProvenanceLedgerError::MissingSigningKey);
            }
        } else {
            // Verify existing signature
            record.verify_signature()?;
        }

        // Persist to disk with atomic append
        if let Some(parent) = self.file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let line = serde_json::to_string(&record)?;
        {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file_path)?;
            writeln!(file, "{}", line)?;
            file.flush()?;
        }

        let mut records = self.records.write().unwrap();
        records.push(record.clone());
        Ok(record)
    }

    /// Appends a record and returns its BLAKE3 leaf hash.
    pub fn append(&self, record: PatchProvenanceRecord) -> Result<String, ProvenanceLedgerError> {
        let saved = self.append_record(record)?;
        Ok(saved.compute_leaf_hash())
    }

    /// Convenient helper to create, sign, and record a patch in one atomic operation.
    pub fn record_patch(
        &self,
        target_crate: impl Into<String>,
        target_file: impl Into<String>,
        pre_patch_hash: impl Into<String>,
        post_patch_hash: impl Into<String>,
        patch_diff: impl Into<String>,
        patch_ast_fingerprint: impl Into<String>,
        audit_context: impl Into<String>,
    ) -> Result<PatchProvenanceRecord, ProvenanceLedgerError> {
        let author = self.default_author_identity();
        let record = PatchProvenanceRecord {
            patch_id: uuid::Uuid::new_v4().to_string(),
            author_identity: author,
            target_crate: target_crate.into(),
            target_file: target_file.into(),
            pre_patch_hash: pre_patch_hash.into(),
            post_patch_hash: post_patch_hash.into(),
            patch_diff: patch_diff.into(),
            patch_ast_fingerprint: patch_ast_fingerprint.into(),
            audit_context: audit_context.into(),
            ed25519_signature: String::new(),
        };

        self.append_record(record)
    }

    /// Integrates with `EvolutionReceipt` to record provenance receipts.
    pub fn record_evolution_receipt(
        &self,
        receipt: &crate::evolution_ledger::EvolutionReceipt,
    ) -> Result<PatchProvenanceRecord, ProvenanceLedgerError> {
        let audit_ctx = format!(
            "anomaly_kind={}, strategy={}, metric_before={:.2}, metric_after={:.2}, speedup={:.1}%, sandbox_exit={}",
            receipt.anomaly_kind,
            receipt.strategy,
            receipt.observed_metric_before,
            receipt.observed_metric_after,
            receipt.speedup_percentage,
            receipt.sandbox_exit_code,
        );

        self.record_patch(
            &receipt.target_crate,
            &receipt.target_file,
            &receipt.pre_patch_ast_hash,
            &receipt.post_patch_ast_hash,
            &receipt.patch_diff,
            &receipt.blake3_provenance_hash,
            audit_ctx,
        )
    }

    /// Integrates with `ClosedLoopCompilerHealer` to record remediation certificates.
    pub fn record_healer_remediation(
        &self,
        target_crate: &str,
        target_file: &Path,
        diag: &crate::live_closed_loop_compiler_healer::CompilerDiagnostic,
        patch: &crate::live_closed_loop_compiler_healer::PatchSynthesisResult,
        cert: &crate::live_closed_loop_compiler_healer::RemediationCertificate,
    ) -> Result<PatchProvenanceRecord, ProvenanceLedgerError> {
        let audit_ctx = format!(
            "compiler_diagnostic={}, message='{}', first_green_verified={}, duration_ms={}",
            diag.code,
            diag.message,
            cert.first_green_verified,
            cert.verification_duration_ms,
        );

        self.record_patch(
            target_crate,
            target_file.to_string_lossy(),
            &patch.ast_pre_hash,
            &patch.ast_post_hash,
            &patch.diff,
            &cert.consensus_receipt_hash,
            audit_ctx,
        )
    }

    /// Verifies Ed25519 signatures of all records currently in the ledger.
    pub fn audit_all_signatures(&self) -> Result<usize, ProvenanceLedgerError> {
        let records = self.records.read().unwrap();
        for (idx, record) in records.iter().enumerate() {
            record.verify_signature().map_err(|e| ProvenanceLedgerError::TamperedRecord {
                index: idx,
                patch_id: record.patch_id.clone(),
                reason: e.to_string(),
            })?;
        }
        Ok(records.len())
    }

    /// Builds the Merkle tree over all records in the ledger.
    pub fn merkle_tree(&self) -> ProvenanceMerkleTree {
        let records = self.records.read().unwrap();
        ProvenanceMerkleTree::from_records(&records)
    }

    /// Computes the top-level Merkle root across all recorded patches.
    pub fn merkle_root(&self) -> String {
        self.merkle_tree().merkle_root()
    }

    /// Generates a cryptographic inclusion proof for a leaf index.
    pub fn generate_merkle_proof(&self, leaf_index: usize) -> Result<MerkleProof, ProvenanceLedgerError> {
        self.merkle_tree().generate_merkle_proof(leaf_index)
    }

    /// Verifies a Merkle inclusion proof.
    pub fn verify_merkle_proof(&self, leaf_hash: &str, proof: &MerkleProof, root: &str) -> bool {
        verify_merkle_proof(leaf_hash, proof, root)
    }

    /// Returns the total number of records currently loaded in the ledger.
    pub fn total_records(&self) -> usize {
        self.records.read().unwrap().len()
    }

    /// Returns a specific record by index.
    pub fn get_record(&self, index: usize) -> Option<PatchProvenanceRecord> {
        self.records.read().unwrap().get(index).cloned()
    }

    /// Returns a clone of all records currently in memory.
    pub fn records(&self) -> Vec<PatchProvenanceRecord> {
        self.records.read().unwrap().clone()
    }
}

// ==============================================================================
// 5. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_author(public_key_hex: &str) -> AuthorIdentity {
        AuthorIdentity {
            agent_id: "agent-test-01".to_string(),
            model_name: "test-model-v1".to_string(),
            public_key_hex: public_key_hex.to_string(),
            timestamp_secs: 1700000000,
        }
    }

    fn create_test_record(patch_id: &str, author: AuthorIdentity) -> PatchProvenanceRecord {
        PatchProvenanceRecord {
            patch_id: patch_id.to_string(),
            author_identity: author,
            target_crate: "hgb-mesh".to_string(),
            target_file: "src/transport.rs".to_string(),
            pre_patch_hash: "a1b2c3d4e5f60718293041526374859607182930415263748596071829304152".to_string(),
            post_patch_hash: "f6e5d4c3b2a10987654321fedcba0987654321fedcba0987654321fedcba0987".to_string(),
            patch_diff: "+ socket.set_nodelay(true);".to_string(),
            patch_ast_fingerprint: "ast_fingerprint_01".to_string(),
            audit_context: "compiler_diagnostic=E0308, speedup=25.4%".to_string(),
            ed25519_signature: String::new(),
        }
    }

    #[test]
    fn test_record_creation_and_ed25519_signing() {
        let (signing_key, verifying_key) = ProvenanceMerkleLedger::generate_keypair();
        let pubkey_hex = hex::encode(verifying_key.as_bytes());

        let author = create_test_author(&pubkey_hex);
        let mut record = create_test_record("test-patch-001", author);

        assert!(record.ed25519_signature.is_empty());
        assert!(record.verify_signature().is_err());

        // Sign the record
        record.sign(&signing_key);
        assert!(!record.ed25519_signature.is_empty());
        assert_eq!(record.ed25519_signature.len(), 128); // 64 bytes in hex

        // Verification must succeed
        assert!(record.verify_signature().is_ok());

        // Leaf hash must be 32 bytes (64 hex characters)
        let leaf_hash = record.compute_leaf_hash();
        assert_eq!(leaf_hash.len(), 64);
        assert_eq!(record.compute_leaf_hash_bytes().len(), 32);
    }

    #[test]
    fn test_signature_verification_and_tamper_detection() {
        let (signing_key, verifying_key) = ProvenanceMerkleLedger::generate_keypair();
        let pubkey_hex = hex::encode(verifying_key.as_bytes());

        let author = create_test_author(&pubkey_hex);
        let mut pristine = create_test_record("test-patch-tamper", author);
        pristine.sign(&signing_key);
        assert!(pristine.verify_signature().is_ok());

        // 1. Tamper with patch_diff
        let mut tampered_diff = pristine.clone();
        tampered_diff.patch_diff.push_str(" malicious payload injected;");
        assert!(tampered_diff.verify_signature().is_err(), "Diff tampering must fail verification");

        // 2. Tamper with pre_patch_hash
        let mut tampered_pre = pristine.clone();
        tampered_pre.pre_patch_hash = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert!(tampered_pre.verify_signature().is_err(), "Pre-patch hash tampering must fail verification");

        // 3. Tamper with post_patch_hash
        let mut tampered_post = pristine.clone();
        tampered_post.post_patch_hash = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string();
        assert!(tampered_post.verify_signature().is_err(), "Post-patch hash tampering must fail verification");

        // 4. Tamper with author identity (agent_id)
        let mut tampered_agent = pristine.clone();
        tampered_agent.author_identity.agent_id = "malicious-rogue-agent".to_string();
        assert!(tampered_agent.verify_signature().is_err(), "Author identity tampering must fail verification");

        // 5. Tamper with target crate or file
        let mut tampered_crate = pristine.clone();
        tampered_crate.target_crate = "rogue-crate".to_string();
        assert!(tampered_crate.verify_signature().is_err(), "Target crate tampering must fail verification");

        // 6. Tamper with audit context
        let mut tampered_audit = pristine.clone();
        tampered_audit.audit_context = "forged audit pass".to_string();
        assert!(tampered_audit.verify_signature().is_err(), "Audit context tampering must fail verification");

        // 7. Tamper with signature bytes directly
        let mut tampered_sig = pristine.clone();
        let mut sig_bytes = hex::decode(&tampered_sig.ed25519_signature).unwrap();
        sig_bytes[0] ^= 0xff;
        tampered_sig.ed25519_signature = hex::encode(sig_bytes);
        assert!(tampered_sig.verify_signature().is_err(), "Corrupted signature bytes must fail verification");
    }

    #[test]
    fn test_merkle_root_computation_multiple_records() {
        let (signing_key, verifying_key) = ProvenanceMerkleLedger::generate_keypair();
        let pubkey_hex = hex::encode(verifying_key.as_bytes());

        // Case 1: Empty records
        let empty_tree = ProvenanceMerkleTree::from_records(&[]);
        assert_eq!(empty_tree.root, blake3::hash(b"").to_hex().to_string());

        // Case 2: 1 record
        let author = create_test_author(&pubkey_hex);
        let mut rec1 = create_test_record("patch-1", author.clone());
        rec1.sign(&signing_key);
        let leaf1 = rec1.compute_leaf_hash();

        let single_tree = ProvenanceMerkleTree::from_records(&[rec1.clone()]);
        assert_eq!(single_tree.root, leaf1, "Single leaf tree root must equal the leaf hash itself");

        // Case 3: 2 records
        let mut rec2 = create_test_record("patch-2", author.clone());
        rec2.patch_diff = "+ println!(\"log 2\");".to_string();
        rec2.sign(&signing_key);
        let leaf2 = rec2.compute_leaf_hash();

        let tree2 = ProvenanceMerkleTree::from_records(&[rec1.clone(), rec2.clone()]);
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&hex::decode(&leaf1).unwrap());
        concat[32..].copy_from_slice(&hex::decode(&leaf2).unwrap());
        let expected_root2 = blake3::hash(&concat).to_hex().to_string();
        assert_eq!(tree2.root, expected_root2, "Two-leaf root must equal BLAKE3(leaf1 || leaf2)");

        // Case 4: 3 records (odd count duplicates last leaf)
        let mut rec3 = create_test_record("patch-3", author.clone());
        rec3.patch_diff = "+ vec.reserve(128);".to_string();
        rec3.sign(&signing_key);
        let leaf3 = rec3.compute_leaf_hash();

        let tree3 = ProvenanceMerkleTree::from_records(&[rec1.clone(), rec2.clone(), rec3.clone()]);
        // Level 0: pair (leaf1, leaf2) -> P0, pair (leaf3, leaf3) -> P1
        let mut concat_p1 = [0u8; 64];
        concat_p1[..32].copy_from_slice(&hex::decode(&leaf3).unwrap());
        concat_p1[32..].copy_from_slice(&hex::decode(&leaf3).unwrap());
        let p1 = blake3::hash(&concat_p1);

        let mut concat_root3 = [0u8; 64];
        concat_root3[..32].copy_from_slice(&hex::decode(&expected_root2).unwrap());
        concat_root3[32..].copy_from_slice(p1.as_bytes());
        let expected_root3 = blake3::hash(&concat_root3).to_hex().to_string();
        assert_eq!(tree3.root, expected_root3, "Three-leaf root must duplicate leaf3");

        // Case 5: 8 records (power of 2)
        let mut records = Vec::new();
        for i in 0..8 {
            let mut r = create_test_record(&format!("patch-{}", i), author.clone());
            r.patch_diff = format!("+ diff_{}", i);
            r.sign(&signing_key);
            records.push(r);
        }
        let tree8 = ProvenanceMerkleTree::from_records(&records);
        assert_eq!(tree8.root.len(), 64);

        // Determinism check: same records must yield identical Merkle root
        let tree8_bis = ProvenanceMerkleTree::from_records(&records);
        assert_eq!(tree8.root, tree8_bis.root);

        // Sensitivity check: altering 1 bit in 1 record must change root
        records[3].patch_diff.push_str("X");
        let tree8_altered = ProvenanceMerkleTree::from_records(&records);
        assert_ne!(tree8.root, tree8_altered.root, "Altered record must change top-level Merkle root");
    }

    #[test]
    fn test_merkle_inclusion_proof_generation_and_verification() {
        let (signing_key, verifying_key) = ProvenanceMerkleLedger::generate_keypair();
        let pubkey_hex = hex::encode(verifying_key.as_bytes());
        let author = create_test_author(&pubkey_hex);

        for num_records in [1, 2, 3, 5, 7, 8, 15, 16] {
            let mut records = Vec::new();
            for i in 0..num_records {
                let mut r = create_test_record(&format!("patch-{}-of-{}", i, num_records), author.clone());
                r.patch_diff = format!("+ diff_index_{}", i);
                r.sign(&signing_key);
                records.push(r);
            }

            let tree = ProvenanceMerkleTree::from_records(&records);
            let root = tree.merkle_root();

            // Verify inclusion proof for every single leaf
            for (idx, record) in records.iter().enumerate() {
                let leaf_hash = record.compute_leaf_hash();
                let proof = tree
                    .generate_merkle_proof(idx)
                    .unwrap_or_else(|e| panic!("Failed to generate proof for leaf {}/{}: {:?}", idx, num_records, e));

                assert_eq!(proof.leaf_index, idx);
                assert_eq!(proof.leaf_hash, leaf_hash);
                assert_eq!(proof.root, root);

                // Standalone verification
                let verified = verify_merkle_proof(&leaf_hash, &proof, &root);
                assert!(
                    verified,
                    "Inclusion proof verification failed for leaf {} in tree of size {}",
                    idx, num_records
                );

                // Proof struct helper verification
                assert!(proof.verify(&root));

                // Tree method verification
                assert!(tree.verify_merkle_proof(&leaf_hash, &proof, &root));
            }
        }
    }

    #[test]
    fn test_rejection_of_forged_merkle_proofs() {
        let (signing_key, verifying_key) = ProvenanceMerkleLedger::generate_keypair();
        let pubkey_hex = hex::encode(verifying_key.as_bytes());
        let author = create_test_author(&pubkey_hex);

        let mut records = Vec::new();
        for i in 0..6 {
            let mut r = create_test_record(&format!("patch-forgery-{}", i), author.clone());
            r.patch_diff = format!("+ code line {}", i);
            r.sign(&signing_key);
            records.push(r);
        }

        let tree = ProvenanceMerkleTree::from_records(&records);
        let root = tree.merkle_root();
        let valid_proof = tree.generate_merkle_proof(2).unwrap();
        assert!(verify_merkle_proof(&valid_proof.leaf_hash, &valid_proof, &root));

        // 1. Forged leaf hash
        let forged_leaf_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(
            !verify_merkle_proof(forged_leaf_hash, &valid_proof, &root),
            "Forged leaf hash must be rejected"
        );

        // 2. Tampered sibling in audit path
        let mut tampered_sibling_proof = valid_proof.clone();
        if !tampered_sibling_proof.siblings.is_empty() {
            tampered_sibling_proof.siblings[0] =
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string();
            assert!(
                !verify_merkle_proof(&tampered_sibling_proof.leaf_hash, &tampered_sibling_proof, &root),
                "Tampered sibling in proof must be rejected"
            );
        }

        // 3. Forged Merkle root
        let forged_root = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert!(
            !verify_merkle_proof(&valid_proof.leaf_hash, &valid_proof, forged_root),
            "Forged root must be rejected"
        );

        // 4. Altered leaf index (mismatched position)
        let mut tampered_index_proof = valid_proof.clone();
        tampered_index_proof.leaf_index = 3;
        assert!(
            !verify_merkle_proof(&tampered_index_proof.leaf_hash, &tampered_index_proof, &root),
            "Mismatched leaf index must be rejected"
        );

        // 5. Foreign leaf proof: using proof for leaf 1 to claim leaf 2
        let proof_leaf_1 = tree.generate_merkle_proof(1).unwrap();
        let leaf_2_hash = records[2].compute_leaf_hash();
        assert!(
            !verify_merkle_proof(&leaf_2_hash, &proof_leaf_1, &root),
            "Foreign leaf proof must be rejected"
        );

        // 6. Out of bounds leaf index request
        assert!(tree.generate_merkle_proof(999).is_err());
    }

    #[test]
    fn test_append_only_ledger_disk_persistence_and_reload() {
        let temp = tempdir().unwrap();
        let ledger_path = temp.path().join("provenance_ledger.jsonl");

        let (signing_key, verifying_key) = ProvenanceMerkleLedger::generate_keypair();
        let ledger = ProvenanceMerkleLedger::with_keys(
            &ledger_path,
            Some(signing_key),
            Some(verifying_key),
        );

        assert_eq!(ledger.total_records(), 0);

        // Append 3 patches
        let r1 = ledger
            .record_patch(
                "hgb-core",
                "src/lib.rs",
                "pre_hash_1",
                "post_hash_1",
                "+ pub mod new_feature;",
                "ast_fp_1",
                "diagnostic=none",
            )
            .unwrap();

        let r2 = ledger
            .record_patch(
                "hgb-autopoietic",
                "src/evolution_ledger.rs",
                "pre_hash_2",
                "post_hash_2",
                "+ use provenance_ledger;",
                "ast_fp_2",
                "diagnostic=E0432",
            )
            .unwrap();

        let r3 = ledger
            .record_patch(
                "hgb-wasm-kernel",
                "src/sandbox.rs",
                "pre_hash_3",
                "post_hash_3",
                "+ ring.flush();",
                "ast_fp_3",
                "metric=p99_latency_spike",
            )
            .unwrap();

        assert_eq!(ledger.total_records(), 3);
        assert!(ledger_path.exists());

        // Verify audit on current records
        let audited_count = ledger.audit_all_signatures().unwrap();
        assert_eq!(audited_count, 3);

        let root_before = ledger.merkle_root();
        assert_eq!(root_before.len(), 64);

        // Reload ledger from disk and verify signatures on load
        let reloaded_ledger = ProvenanceMerkleLedger::load_from_file(&ledger_path, None, true).unwrap();
        assert_eq!(reloaded_ledger.total_records(), 3);
        assert_eq!(reloaded_ledger.merkle_root(), root_before);

        let reloaded_r1 = reloaded_ledger.get_record(0).unwrap();
        assert_eq!(reloaded_r1.patch_id, r1.patch_id);
        assert_eq!(reloaded_r1.patch_diff, r1.patch_diff);
        assert_eq!(reloaded_r1.ed25519_signature, r1.ed25519_signature);

        let reloaded_r2 = reloaded_ledger.get_record(1).unwrap();
        assert_eq!(reloaded_r2.patch_id, r2.patch_id);
        assert_eq!(reloaded_r2.patch_diff, r2.patch_diff);

        let reloaded_r3 = reloaded_ledger.get_record(2).unwrap();
        assert_eq!(reloaded_r3.patch_id, r3.patch_id);
        assert_eq!(reloaded_r3.patch_diff, r3.patch_diff);

        // Test tamper detection upon disk reload
        let disk_content = fs::read_to_string(&ledger_path).unwrap();
        let tampered_disk = disk_content.replacen("pub mod new_feature;", "MALICIOUS_INJECTION;", 1);
        fs::write(&ledger_path, tampered_disk).unwrap();

        let tamper_result = ProvenanceMerkleLedger::load_from_file(&ledger_path, None, true);
        assert!(
            tamper_result.is_err(),
            "Disk tamper detection must reject tampered JSONL lines"
        );
    }

    #[tokio::test]
    async fn test_evolution_ledger_provenance_integration() {
        use crate::evolution_ledger::{EvolutionLedger, EvolutionReceipt, GENESIS_PARENT_HASH};

        let temp = tempdir().unwrap();
        let evo_path = temp.path().join("evo_ledger.jsonl");
        let prov_path = temp.path().join("prov_ledger.jsonl");

        let prov_ledger = Arc::new(ProvenanceMerkleLedger::new(&prov_path));
        let evo_ledger = EvolutionLedger::new(&evo_path).with_provenance_ledger(prov_ledger.clone());

        assert_eq!(prov_ledger.total_records(), 0);

        let receipt = EvolutionReceipt {
            receipt_id: "rcpt-integration-001".to_string(),
            prev_receipt_hash: GENESIS_PARENT_HASH.to_string(),
            cycle_timestamp_ms: 12345678,
            target_crate: "hgb-mesh".to_string(),
            target_file: "src/session.rs".to_string(),
            target_function: "handle_stream".to_string(),
            strategy: "PreallocateVectors".to_string(),
            anomaly_kind: "LatencyDegradation".to_string(),
            pre_patch_ast_hash: "pre_hash_123".to_string(),
            post_patch_ast_hash: "post_hash_456".to_string(),
            patch_diff: "+ buffer.reserve(4096);".to_string(),
            observed_metric_before: 100.0,
            observed_metric_after: 20.0,
            speedup_percentage: 80.0,
            sandbox_run_id: "sb-run-001".to_string(),
            sandbox_exit_code: 0,
            sandbox_logs_hash: "logs_hash_1".to_string(),
            pgo_profile_applied: true,
            hot_reloaded: true,
            blake3_provenance_hash: String::new(),
        };

        let hash = evo_ledger.append_receipt(receipt).await.unwrap();
        assert!(!hash.is_empty());

        // Provenance ledger must have recorded 1 patch receipt
        assert_eq!(prov_ledger.total_records(), 1);
        let prov_record = prov_ledger.get_record(0).unwrap();
        assert_eq!(prov_record.target_crate, "hgb-mesh");
        assert_eq!(prov_record.target_file, "src/session.rs");
        assert_eq!(prov_record.patch_diff, "+ buffer.reserve(4096);");
        assert!(prov_record.verify_signature().is_ok());

        // Merkle root and proof must verify
        let root = prov_ledger.merkle_root();
        let proof = prov_ledger.generate_merkle_proof(0).unwrap();
        assert!(verify_merkle_proof(&proof.leaf_hash, &proof, &root));
    }

    #[test]
    fn test_compiler_healer_provenance_integration() {
        use crate::live_closed_loop_compiler_healer::{
            CompilerDiagnostic, DiagnosticSeverity,
            PatchStrategy, PatchSynthesisResult, RemediationCertificate, VerificationReport,
        };

        let temp = tempdir().unwrap();
        let prov_path = temp.path().join("prov_ledger.jsonl");
        let prov_ledger = Arc::new(ProvenanceMerkleLedger::new(&prov_path));

        let diag = CompilerDiagnostic {
            file_path: temp.path().join("src/lib.rs"),
            line: 10,
            column: 5,
            end_line: 10,
            end_column: 20,
            code: "E0308".to_string(),
            severity: DiagnosticSeverity::Error,
            message: "mismatched types: expected String, found &str".to_string(),
            suggested_replacement: Some(".to_string()".to_string()),
            rendered: None,
            raw_json: None,
        };

        let patch = PatchSynthesisResult {
            strategy: PatchStrategy::TypeMismatchStrToString,
            repaired_source: "let s = \"hello\".to_string();".to_string(),
            diff: "+ .to_string()".to_string(),
            ast_pre_hash: "pre_hash_e0308".to_string(),
            ast_post_hash: "post_hash_e0308".to_string(),
        };

        let report = VerificationReport {
            first_green: true,
            exit_code: 0,
            stdout: "".to_string(),
            stderr: "".to_string(),
            verification_ms: 45,
        };

        let cert = RemediationCertificate::mint(
            "hgb-core",
            &diag.file_path,
            &diag,
            &patch,
            &report,
        );

        let rec = prov_ledger.record_healer_remediation(
            "hgb-core",
            &diag.file_path,
            &diag,
            &patch,
            &cert,
        ).unwrap();

        assert_eq!(rec.target_crate, "hgb-core");
        assert_eq!(rec.pre_patch_hash, "pre_hash_e0308");
        assert_eq!(rec.post_patch_hash, "post_hash_e0308");
        assert!(rec.verify_signature().is_ok());
        assert_eq!(prov_ledger.total_records(), 1);

        let root = prov_ledger.merkle_root();
        let proof = prov_ledger.generate_merkle_proof(0).unwrap();
        assert!(verify_merkle_proof(&proof.leaf_hash, &proof, &root));
    }
}
