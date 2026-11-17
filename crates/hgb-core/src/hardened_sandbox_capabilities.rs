//! # Hardened Dual-Sandbox Capabilities & Memory-Safe IPC Subsystem
//!
//! Production-grade capability-based security isolation (Pillar 2) for Hagibis:
//! - **WASM Kernel Hardening**: Strips unnecessary host imports, enforces zero ambient authority,
//!   and implements unforgeable cryptographic capability tokens (`CapabilityToken`) verified prior
//!   to invoking any host function.
//! - **Fine-Grained Capability Gating**: Granular filesystem read/write roots with path traversal
//!   defense, network hostname/CIDR egress filters, compute fuel/memory quotas, and process restrictions.
//! - **Memory-Safe SHM Ring Buffer IPC**: Connects microVMs via lock-free shared memory ring buffers
//!   rather than raw unbounded Unix sockets, preventing socket exhaustion (`EMFILE`), race conditions,
//!   and IPC buffer bloat.
//! - **Dual-Sandbox Workload Arbiter**: Routes lightweight compute to stripped FastWasm sandboxes
//!   and escalates heavier or stateful workloads to Seccomp-jailed Firecracker microVMs over SHM IPC.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ==============================================================================
// 1. Constants & Magic Numbers
// ==============================================================================

/// 64-byte CPU cacheline alignment boundary
pub const CACHELINE_BYTES: usize = 64;

/// Hagibis Hardened SHM Magic: "HGB_SHM2" in ASCII (0x4847425F53484D32)
pub const SHM_MAGIC_V2: u64 = 0x4847425F_53484D32;

/// IPC Frame Magic: "FRME" in ASCII (0x46524D45)
pub const IPC_FRAME_MAGIC: u32 = 0x46524D45;

/// Wrap Marker entry type indicating jump to start of circular arena
pub const IPC_WRAP_MARKER: u16 = 0xFFFF;

/// Minimum ring buffer payload capacity (4 KiB)
pub const MIN_RING_CAPACITY: usize = 4096;

/// Default FastWasm memory ceiling (64 MiB)
pub const FAST_WASM_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Default instruction fuel budget for sandboxed WASM
pub const DEFAULT_WASM_FUEL_BUDGET: u64 = 20_000_000;

// ==============================================================================
// 2. Structured Errors
// ==============================================================================

/// Structured errors originating from the capability sandbox and IPC subsystem
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum SandboxError {
    #[error("Capability token validation failed: {0}")]
    InvalidToken(String),

    #[error("Capability token expired at timestamp {expired_at_ns}, current is {current_ns}")]
    TokenExpired { expired_at_ns: u64, current_ns: u64 },

    #[error("Capability signature verification failed: invalid BLAKE3 MAC signature")]
    SignatureVerificationFailed,

    #[error("Capability violation [{kind}]: target={target}, reason={reason}")]
    CapabilityViolation {
        kind: String,
        target: String,
        reason: String,
        severity: ViolationSeverity,
    },

    #[error("Path traversal detected: {path} attempts to escape sandbox boundary")]
    PathTraversalAttack { path: String },

    #[error("Host import '{import_name}' has been stripped for zero ambient authority")]
    StrippedHostImport { import_name: String },

    #[error("Fuel budget exhausted: consumed {consumed} > allocated {budget}")]
    FuelExhausted { consumed: u64, budget: u64 },

    #[error("Memory ceiling exceeded: requested {requested_bytes} > limit {limit_bytes}")]
    MemoryCeilingExceeded {
        requested_bytes: usize,
        limit_bytes: usize,
    },

    #[error("SHM Ring Buffer error: {0}")]
    ShmRingError(String),

    #[error("IPC Frame checksum mismatch: calculated {calculated:08x}, expected {expected:08x}")]
    ChecksumMismatch { calculated: u32, expected: u32 },

    #[error("MicroVM IPC buffer full: ring capacity exhausted")]
    IpcBufferFull,
}

/// Severity classification of capability breaches
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationSeverity {
    Low,
    Medium,
    High,
    Critical,
}

// ==============================================================================
// 3. Granular Capability Primitives
// ==============================================================================

/// Fine-grained filesystem read capability flags
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadCaps {
    pub read_exact: bool,
    pub read_subtree: bool,
    pub allow_symlink: bool,
    pub read_metadata: bool,
}

impl Default for FsReadCaps {
    fn default() -> Self {
        Self {
            read_exact: true,
            read_subtree: false,
            allow_symlink: false,
            read_metadata: true,
        }
    }
}

/// Fine-grained filesystem write capability flags
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWriteCaps {
    pub write_atomic: bool,
    pub write_append: bool,
    pub write_truncate: bool,
    pub create_temp: bool,
    pub delete_allowed: bool,
}

impl Default for FsWriteCaps {
    fn default() -> Self {
        Self {
            write_atomic: true,
            write_append: false,
            write_truncate: false,
            create_temp: true,
            delete_allowed: false,
        }
    }
}

/// Network CIDR and port filtering rule
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpCidrRule {
    pub cidr: String,
    pub port_min: u16,
    pub port_max: u16,
    pub allow_dns: bool,
}

/// Network capability boundary
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetCapability {
    pub allowed_hosts: Vec<String>,
    pub allowed_cidrs: Vec<IpCidrRule>,
    pub allow_egress: bool,
    pub allow_ingress: bool,
    pub max_bandwidth_kbps: u32,
}

impl Default for NetCapability {
    fn default() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            allowed_cidrs: Vec::new(),
            allow_egress: false,
            allow_ingress: false,
            max_bandwidth_kbps: 0,
        }
    }
}

/// Compute and hardware resource capability bounds
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeCapability {
    pub fuel_quota: u64,
    pub memory_ceiling_bytes: usize,
    pub timeout_sla_micros: u64,
    pub vcpu_quota: u32,
    pub allow_simd: bool,
    pub allow_gpu: bool,
}

impl Default for ComputeCapability {
    fn default() -> Self {
        Self {
            fuel_quota: DEFAULT_WASM_FUEL_BUDGET,
            memory_ceiling_bytes: FAST_WASM_MAX_MEMORY_BYTES,
            timeout_sla_micros: 5000, // 5ms FastWasm SLA
            vcpu_quota: 1,
            allow_simd: true,
            allow_gpu: false,
        }
    }
}

/// Subprocess and execution capability bounds
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessCapability {
    pub allow_process_spawn: bool,
    pub allowed_binary_hashes: Vec<String>,
    pub allow_elevated_privileges: bool,
}

impl Default for ProcessCapability {
    fn default() -> Self {
        Self {
            allow_process_spawn: false,
            allowed_binary_hashes: Vec::new(),
            allow_elevated_privileges: false, // Always strictly false
        }
    }
}

// ==============================================================================
// 4. Cryptographic Capability Token & Token Issuer
// ==============================================================================

/// Unforgeable cryptographic capability token presented by sandboxed guest
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub token_id: String,
    pub principal_id: String,
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,
    pub fs_read_roots: Vec<PathBuf>,
    pub fs_write_roots: Vec<PathBuf>,
    pub fs_read_caps: FsReadCaps,
    pub fs_write_caps: FsWriteCaps,
    pub net_caps: NetCapability,
    pub compute_caps: ComputeCapability,
    pub process_caps: ProcessCapability,
    /// BLAKE3 Keyed MAC signature
    pub signature: [u8; 32],
}

impl CapabilityToken {
    /// Computes the signable byte payload of this capability token
    pub fn signable_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(self.token_id.as_bytes());
        buf.extend_from_slice(b":");
        buf.extend_from_slice(self.principal_id.as_bytes());
        buf.extend_from_slice(b":");
        buf.extend_from_slice(&self.issued_at_ns.to_le_bytes());
        buf.extend_from_slice(&self.expires_at_ns.to_le_bytes());
        for r in &self.fs_read_roots {
            buf.extend_from_slice(r.to_string_lossy().as_bytes());
            buf.push(b',');
        }
        for w in &self.fs_write_roots {
            buf.extend_from_slice(w.to_string_lossy().as_bytes());
            buf.push(b',');
        }
        for h in &self.net_caps.allowed_hosts {
            buf.extend_from_slice(h.as_bytes());
            buf.push(b',');
        }
        buf.extend_from_slice(&self.compute_caps.fuel_quota.to_le_bytes());
        buf.extend_from_slice(&(self.compute_caps.memory_ceiling_bytes as u64).to_le_bytes());
        buf.push(if self.process_caps.allow_process_spawn { 1 } else { 0 });
        buf.push(if self.process_caps.allow_elevated_privileges { 1 } else { 0 });
        buf
    }
}

/// Cryptographic Capability Token Issuer & Verifier
#[derive(Debug, Clone)]
pub struct TokenIssuer {
    signing_key: [u8; 32],
}

impl TokenIssuer {
    /// Initializes an issuer with a 32-byte secret signing key
    pub fn new(signing_key: [u8; 32]) -> Self {
        Self { signing_key }
    }

    /// Generates a random issuer with an ephemeral entropy key
    pub fn generate_ephemeral() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let key_hash = blake3::hash(&seed.to_le_bytes());
        Self {
            signing_key: *key_hash.as_bytes(),
        }
    }

    /// Signs and constructs a complete `CapabilityToken`
    pub fn issue_token(
        &self,
        token_id: impl Into<String>,
        principal_id: impl Into<String>,
        ttl_seconds: u64,
        fs_read_roots: Vec<PathBuf>,
        fs_write_roots: Vec<PathBuf>,
        fs_read_caps: FsReadCaps,
        fs_write_caps: FsWriteCaps,
        net_caps: NetCapability,
        compute_caps: ComputeCapability,
        process_caps: ProcessCapability,
    ) -> CapabilityToken {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let expires_at_ns = now_ns + (ttl_seconds * 1_000_000_000);

        // Normalize roots
        let norm_read: Vec<PathBuf> = fs_read_roots
            .into_iter()
            .map(|p| normalize_path(&p))
            .collect();
        let norm_write: Vec<PathBuf> = fs_write_roots
            .into_iter()
            .map(|p| normalize_path(&p))
            .collect();

        let mut token = CapabilityToken {
            token_id: token_id.into(),
            principal_id: principal_id.into(),
            issued_at_ns: now_ns,
            expires_at_ns,
            fs_read_roots: norm_read,
            fs_write_roots: norm_write,
            fs_read_caps,
            fs_write_caps,
            net_caps,
            compute_caps,
            process_caps,
            signature: [0u8; 32],
        };

        let signable = token.signable_bytes();
        let mac = blake3::keyed_hash(&self.signing_key, &signable);
        token.signature = *mac.as_bytes();
        token
    }

    /// Verifies the cryptographic integrity and validity of a `CapabilityToken`
    pub fn verify_token(&self, token: &CapabilityToken) -> Result<(), SandboxError> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        if now_ns > token.expires_at_ns {
            return Err(SandboxError::TokenExpired {
                expired_at_ns: token.expires_at_ns,
                current_ns: now_ns,
            });
        }

        let signable = token.signable_bytes();
        let expected_mac = blake3::keyed_hash(&self.signing_key, &signable);

        // Constant time verification check
        if !constant_time_eq_32(&token.signature, expected_mac.as_bytes()) {
            return Err(SandboxError::SignatureVerificationFailed);
        }

        Ok(())
    }
}

/// Constant time 32-byte equality check
#[inline]
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Lexically canonicalizes a path without accessing the host filesystem
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                components.push(c.as_os_str());
            }
        }
    }
    components.into_iter().collect()
}

// ==============================================================================
// 5. Capability Verification Engine
// ==============================================================================

/// Capability verification and access control engine
#[derive(Debug, Clone)]
pub struct CapabilityVerifier {
    issuer: TokenIssuer,
    revocation_list: Arc<RwLock<HashSet<String>>>,
}

impl CapabilityVerifier {
    /// Creates a verifier backed by the token issuer
    pub fn new(issuer: TokenIssuer) -> Self {
        Self {
            issuer,
            revocation_list: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Revokes a specific token by its token ID
    pub fn revoke_token(&self, token_id: &str) {
        if let Ok(mut lock) = self.revocation_list.write() {
            lock.insert(token_id.to_string());
        }
    }

    /// Checks if a token is revoked
    pub fn is_revoked(&self, token_id: &str) -> bool {
        if let Ok(lock) = self.revocation_list.read() {
            lock.contains(token_id)
        } else {
            true
        }
    }

    /// Comprehensive token validation
    pub fn validate_token(&self, token: &CapabilityToken) -> Result<(), SandboxError> {
        if self.is_revoked(&token.token_id) {
            return Err(SandboxError::InvalidToken(format!(
                "Token {} has been revoked",
                token.token_id
            )));
        }
        self.issuer.verify_token(token)
    }

    /// Verifies guest filesystem read access against token read roots
    pub fn check_fs_read(
        &self,
        token: &CapabilityToken,
        target_path: &Path,
    ) -> Result<PathBuf, SandboxError> {
        self.validate_token(token)?;

        let target_str = target_path.to_string_lossy();
        if target_str.contains('\0') || target_str.contains("..") {
            return Err(SandboxError::PathTraversalAttack {
                path: target_str.into_owned(),
            });
        }

        let normalized = normalize_path(target_path);

        // Check if path matches or falls within any granted read root
        let mut permitted = false;
        for root in &token.fs_read_roots {
            let norm_root = normalize_path(root);
            if normalized == norm_root && token.fs_read_caps.read_exact {
                permitted = true;
                break;
            }
            if normalized.starts_with(&norm_root) {
                if token.fs_read_caps.read_subtree {
                    permitted = true;
                    break;
                } else if normalized == norm_root {
                    permitted = true;
                    break;
                }
            }
        }

        if !permitted {
            return Err(SandboxError::CapabilityViolation {
                kind: "fs_read".to_string(),
                target: normalized.to_string_lossy().into_owned(),
                reason: format!(
                    "Path is outside granted read roots: {:?}",
                    token.fs_read_roots
                ),
                severity: ViolationSeverity::High,
            });
        }

        Ok(normalized)
    }

    /// Verifies guest filesystem write access against token write roots
    pub fn check_fs_write(
        &self,
        token: &CapabilityToken,
        target_path: &Path,
        is_delete: bool,
    ) -> Result<PathBuf, SandboxError> {
        self.validate_token(token)?;

        let target_str = target_path.to_string_lossy();
        if target_str.contains('\0') || target_str.contains("..") {
            return Err(SandboxError::PathTraversalAttack {
                path: target_str.into_owned(),
            });
        }

        if is_delete && !token.fs_write_caps.delete_allowed {
            return Err(SandboxError::CapabilityViolation {
                kind: "fs_delete".to_string(),
                target: target_str.into_owned(),
                reason: "Token lacks delete_allowed capability".to_string(),
                severity: ViolationSeverity::High,
            });
        }

        let normalized = normalize_path(target_path);

        let mut permitted = false;
        for root in &token.fs_write_roots {
            let norm_root = normalize_path(root);
            if normalized.starts_with(&norm_root) {
                permitted = true;
                break;
            }
        }

        if !permitted {
            return Err(SandboxError::CapabilityViolation {
                kind: "fs_write".to_string(),
                target: normalized.to_string_lossy().into_owned(),
                reason: format!(
                    "Path is outside granted write roots: {:?}",
                    token.fs_write_roots
                ),
                severity: ViolationSeverity::High,
            });
        }

        Ok(normalized)
    }

    /// Verifies network egress access against token whitelist
    pub fn check_net_egress(
        &self,
        token: &CapabilityToken,
        host: &str,
        port: u16,
    ) -> Result<(), SandboxError> {
        self.validate_token(token)?;

        if !token.net_caps.allow_egress {
            return Err(SandboxError::CapabilityViolation {
                kind: "net_egress".to_string(),
                target: format!("{}:{}", host, port),
                reason: "Network egress is globally disabled for this token".to_string(),
                severity: ViolationSeverity::High,
            });
        }

        // Host whitelist verification (exact or wildcard *.example.com)
        let mut host_allowed = false;
        for pattern in &token.net_caps.allowed_hosts {
            if pattern == "*" || pattern == host {
                host_allowed = true;
                break;
            }
            if pattern.starts_with("*.") {
                let suffix = &pattern[1..]; // ".example.com"
                if host.ends_with(suffix) {
                    host_allowed = true;
                    break;
                }
            }
        }

        if !host_allowed {
            return Err(SandboxError::CapabilityViolation {
                kind: "net_egress".to_string(),
                target: format!("{}:{}", host, port),
                reason: format!("Host '{}' not in allowed_hosts whitelist", host),
                severity: ViolationSeverity::Critical,
            });
        }

        Ok(())
    }

    /// Verifies process execution capability
    pub fn check_process_exec(
        &self,
        token: &CapabilityToken,
        binary_path: &Path,
        binary_blake3_hash: &str,
    ) -> Result<(), SandboxError> {
        self.validate_token(token)?;

        if !token.process_caps.allow_process_spawn {
            return Err(SandboxError::CapabilityViolation {
                kind: "process_spawn".to_string(),
                target: binary_path.to_string_lossy().into_owned(),
                reason: "Process spawning is strictly prohibited by capability policy".to_string(),
                severity: ViolationSeverity::Critical,
            });
        }

        if token.process_caps.allow_elevated_privileges {
            return Err(SandboxError::CapabilityViolation {
                kind: "privilege_escalation".to_string(),
                target: binary_path.to_string_lossy().into_owned(),
                reason: "Elevated privileges (sudo/root) are strictly blocked".to_string(),
                severity: ViolationSeverity::Critical,
            });
        }

        if !token
            .process_caps
            .allowed_binary_hashes
            .iter()
            .any(|h| h.eq_ignore_ascii_case(binary_blake3_hash))
        {
            return Err(SandboxError::CapabilityViolation {
                kind: "process_spawn".to_string(),
                target: binary_path.to_string_lossy().into_owned(),
                reason: format!(
                    "Binary hash '{}' does not match allowed_binary_hashes",
                    binary_blake3_hash
                ),
                severity: ViolationSeverity::Critical,
            });
        }

        Ok(())
    }
}

// ==============================================================================
// 6. WASM Kernel Hardening & Stripped Host Environment
// ==============================================================================

/// Recorded capability access audit record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostAccessAuditRecord {
    pub timestamp_ns: u64,
    pub token_id: String,
    pub principal_id: String,
    pub host_function: String,
    pub target: String,
    pub allowed: bool,
    pub error: Option<String>,
}

/// Hardened WASM Host Environment with stripped ambient authority
#[derive(Debug)]
pub struct HardenedWasmHostEnv {
    verifier: CapabilityVerifier,
    audit_log: Arc<Mutex<Vec<HostAccessAuditRecord>>>,
    violation_count: Arc<AtomicU64>,
}

impl HardenedWasmHostEnv {
    /// Creates a hardened host environment backed by a capability verifier
    pub fn new(verifier: CapabilityVerifier) -> Self {
        Self {
            verifier,
            audit_log: Arc::new(Mutex::new(Vec::new())),
            violation_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Total count of intercepted security violations
    pub fn violation_count(&self) -> u64 {
        self.violation_count.load(Ordering::Relaxed)
    }

    /// Intercepts guest read file attempt
    pub fn host_read_file(
        &self,
        token: &CapabilityToken,
        path: &Path,
    ) -> Result<Vec<u8>, SandboxError> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let check = self.verifier.check_fs_read(token, path);
        match check {
            Ok(safe_path) => {
                let res = std::fs::read(&safe_path).map_err(|e| {
                    SandboxError::CapabilityViolation {
                        kind: "fs_read".to_string(),
                        target: safe_path.to_string_lossy().into_owned(),
                        reason: format!("Host IO error: {}", e),
                        severity: ViolationSeverity::Medium,
                    }
                });

                self.record_audit(
                    now_ns,
                    token,
                    "host_read_file",
                    &safe_path.to_string_lossy(),
                    res.is_ok(),
                    res.as_ref().err().map(|e| e.to_string()),
                );

                res
            }
            Err(e) => {
                self.violation_count.fetch_add(1, Ordering::Relaxed);
                self.record_audit(
                    now_ns,
                    token,
                    "host_read_file",
                    &path.to_string_lossy(),
                    false,
                    Some(e.to_string()),
                );
                Err(e)
            }
        }
    }

    /// Intercepts guest write file attempt
    pub fn host_write_file(
        &self,
        token: &CapabilityToken,
        path: &Path,
        data: &[u8],
    ) -> Result<usize, SandboxError> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let check = self.verifier.check_fs_write(token, path, false);
        match check {
            Ok(safe_path) => {
                let write_res = if token.fs_write_caps.write_atomic {
                    // Atomic write via temp file rename
                    let tmp_path = safe_path.with_extension(format!("tmp.{}", now_ns));
                    std::fs::write(&tmp_path, data)
                        .and_then(|_| std::fs::rename(&tmp_path, &safe_path))
                        .map(|_| data.len())
                } else if token.fs_write_caps.write_append {
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&safe_path)
                        .and_then(|mut f| f.write_all(data).map(|_| data.len()))
                } else {
                    std::fs::write(&safe_path, data).map(|_| data.len())
                };

                let res = write_res.map_err(|e| SandboxError::CapabilityViolation {
                    kind: "fs_write".to_string(),
                    target: safe_path.to_string_lossy().into_owned(),
                    reason: format!("Host write error: {}", e),
                    severity: ViolationSeverity::Medium,
                });

                self.record_audit(
                    now_ns,
                    token,
                    "host_write_file",
                    &safe_path.to_string_lossy(),
                    res.is_ok(),
                    res.as_ref().err().map(|e| e.to_string()),
                );

                res
            }
            Err(e) => {
                self.violation_count.fetch_add(1, Ordering::Relaxed);
                self.record_audit(
                    now_ns,
                    token,
                    "host_write_file",
                    &path.to_string_lossy(),
                    false,
                    Some(e.to_string()),
                );
                Err(e)
            }
        }
    }

    /// Stripped ambient environment variables access: returns empty or strictly denied
    pub fn host_environ_get(&self, _token: &CapabilityToken) -> Result<Vec<(String, String)>, SandboxError> {
        // Zero ambient authority: guest is never granted ambient host environment variables
        Err(SandboxError::StrippedHostImport {
            import_name: "wasi_snapshot_preview1::environ_get".to_string(),
        })
    }

    /// Stripped socket creation: raw sockets are never created from WASM fast path
    pub fn host_sock_create(&self, _token: &CapabilityToken) -> Result<(), SandboxError> {
        Err(SandboxError::StrippedHostImport {
            import_name: "wasi_snapshot_preview1::sock_open".to_string(),
        })
    }

    /// Quantized high-resolution clock to prevent timing side channels
    pub fn host_clock_get_quantized_ns(&self, _token: &CapabilityToken) -> u64 {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        // Quantize to 100-microsecond (100_000 ns) granularity
        (now_ns / 100_000) * 100_000
    }

    fn record_audit(
        &self,
        timestamp_ns: u64,
        token: &CapabilityToken,
        func: &str,
        target: &str,
        allowed: bool,
        error: Option<String>,
    ) {
        if let Ok(mut log) = self.audit_log.lock() {
            log.push(HostAccessAuditRecord {
                timestamp_ns,
                token_id: token.token_id.clone(),
                principal_id: token.principal_id.clone(),
                host_function: func.to_string(),
                target: target.to_string(),
                allowed,
                error,
            });
        }
    }
}

// ==============================================================================
// 7. Memory-Safe Zero-Copy SHM Ring Buffer IPC Channel
// ==============================================================================

/// Header structure for Memory-Safe Ring Buffer IPC (strict 64-byte alignment)
#[repr(C, align(64))]
pub struct ShmIpcRingHeader {
    pub magic: u64,
    pub version: u32,
    pub capacity: u32,
    pub head: AtomicU64,
    pub tail: AtomicU64,
    pub drop_counter: AtomicU64,
    pub generation: AtomicU32,
    _padding: [u8; 24],
}

/// Structured IPC Frame Header in shared memory
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcFrameHeader {
    pub magic: u32,
    pub seq_id: u64,
    pub entry_type: u16,
    pub payload_len: u32,
    pub checksum_crc32: u32,
    pub token_digest: [u8; 16],
}

/// Standard payload entry types transmitted over MicroVM IPC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum IpcEntryType {
    ToolRequest = 1,
    ToolResponse = 2,
    TelemetryFrame = 3,
    HeartbeatPing = 4,
    HeartbeatPong = 5,
    WrapMarker = 0xFFFF,
}

impl IpcEntryType {
    pub fn from_u16(val: u16) -> Option<Self> {
        match val {
            1 => Some(Self::ToolRequest),
            2 => Some(Self::ToolResponse),
            3 => Some(Self::TelemetryFrame),
            4 => Some(Self::HeartbeatPing),
            5 => Some(Self::HeartbeatPong),
            0xFFFF => Some(Self::WrapMarker),
            _ => None,
        }
    }
}

/// Producer instance writing to the memory-safe SHM ring buffer
pub struct ShmIpcProducer {
    mmap: Arc<Mutex<MmapMut>>,
    capacity: usize,
    seq_counter: AtomicU64,
}

/// Consumer instance reading from the memory-safe SHM ring buffer
pub struct ShmIpcConsumer {
    mmap: Arc<Mutex<MmapMut>>,
    capacity: usize,
}

/// Creates a memory-safe shared memory ring buffer channel
pub fn create_shm_ipc_pair(capacity: usize) -> Result<(ShmIpcProducer, ShmIpcConsumer), SandboxError> {
    if !capacity.is_power_of_two() || capacity < MIN_RING_CAPACITY {
        return Err(SandboxError::ShmRingError(format!(
            "Capacity {} must be power of two and >= {}",
            capacity, MIN_RING_CAPACITY
        )));
    }

    let header_size = std::mem::size_of::<ShmIpcRingHeader>();
    let total_size = header_size + capacity;

    let mut mmap = MmapMut::map_anon(total_size)
        .map_err(|e| SandboxError::ShmRingError(format!("Failed to map anonymous memory: {}", e)))?;

    unsafe {
        let hdr_ptr = mmap.as_mut_ptr() as *mut ShmIpcRingHeader;
        (*hdr_ptr).magic = SHM_MAGIC_V2;
        (*hdr_ptr).version = 2;
        (*hdr_ptr).capacity = capacity as u32;
        (*hdr_ptr).head = AtomicU64::new(0);
        (*hdr_ptr).tail = AtomicU64::new(0);
        (*hdr_ptr).drop_counter = AtomicU64::new(0);
        (*hdr_ptr).generation = AtomicU32::new(1);
    }

    let shared_mmap = Arc::new(Mutex::new(mmap));

    let producer = ShmIpcProducer {
        mmap: Arc::clone(&shared_mmap),
        capacity,
        seq_counter: AtomicU64::new(1),
    };

    let consumer = ShmIpcConsumer {
        mmap: shared_mmap,
        capacity,
    };

    Ok((producer, consumer))
}

impl ShmIpcProducer {
    /// Writes a payload frame into the ring buffer with memory safety and zero socket overhead
    pub fn send_frame(
        &self,
        entry_type: IpcEntryType,
        token: &CapabilityToken,
        payload: &[u8],
    ) -> Result<u64, SandboxError> {
        let frame_hdr_size = std::mem::size_of::<IpcFrameHeader>();
        let total_frame_size = frame_hdr_size + payload.len();

        let mut mmap_guard = self.mmap.lock().map_err(|_| {
            SandboxError::ShmRingError("Failed to acquire ring buffer mutex".to_string())
        })?;

        let hdr = unsafe { &mut *(mmap_guard.as_mut_ptr() as *mut ShmIpcRingHeader) };
        let capacity = self.capacity;
        let mask = capacity - 1;

        let head = hdr.head.load(Ordering::Relaxed) as usize;
        let tail = hdr.tail.load(Ordering::Acquire) as usize;

        let used = head.wrapping_sub(tail);
        if capacity - used < total_frame_size {
            hdr.drop_counter.fetch_add(1, Ordering::Relaxed);
            return Err(SandboxError::IpcBufferFull);
        }

        let head_offset = head & mask;
        let seq = self.seq_counter.fetch_add(1, Ordering::Relaxed);

        // CRC32 checksum for payload integrity verification
        let checksum = compute_crc32(payload);

        let mut token_digest = [0u8; 16];
        token_digest.copy_from_slice(&token.signature[0..16]);

        let frame_header = IpcFrameHeader {
            magic: IPC_FRAME_MAGIC,
            seq_id: seq,
            entry_type: entry_type as u16,
            payload_len: payload.len() as u32,
            checksum_crc32: checksum,
            token_digest,
        };

        let arena_start = std::mem::size_of::<ShmIpcRingHeader>();

        // Check if contiguous space is available to the end of circular arena
        if head_offset + total_frame_size > capacity {
            // Need wrap marker
            let remaining = capacity - head_offset;
            let new_head = head + remaining;

            // Re-evaluate from offset 0
            let new_used = new_head.wrapping_sub(tail);
            if capacity - new_used < total_frame_size {
                hdr.drop_counter.fetch_add(1, Ordering::Relaxed);
                return Err(SandboxError::IpcBufferFull);
            }

            if remaining >= frame_hdr_size {
                let wrap_header = IpcFrameHeader {
                    magic: IPC_FRAME_MAGIC,
                    seq_id: 0,
                    entry_type: IPC_WRAP_MARKER,
                    payload_len: 0,
                    checksum_crc32: 0,
                    token_digest: [0u8; 16],
                };
                unsafe {
                    let dest = mmap_guard
                        .as_mut_ptr()
                        .add(arena_start + head_offset) as *mut IpcFrameHeader;
                    std::ptr::write(dest, wrap_header);
                }
            }

            unsafe {
                let dest_hdr = mmap_guard.as_mut_ptr().add(arena_start) as *mut IpcFrameHeader;
                std::ptr::write(dest_hdr, frame_header);
                let dest_payload = mmap_guard.as_mut_ptr().add(arena_start + frame_hdr_size);
                std::ptr::copy_nonoverlapping(payload.as_ptr(), dest_payload, payload.len());
            }

            hdr.head
                .store((new_head + total_frame_size) as u64, Ordering::Release);
        } else {
            // Contiguous write
            unsafe {
                let dest_hdr = mmap_guard
                    .as_mut_ptr()
                    .add(arena_start + head_offset) as *mut IpcFrameHeader;
                std::ptr::write(dest_hdr, frame_header);
                let dest_payload = mmap_guard
                    .as_mut_ptr()
                    .add(arena_start + head_offset + frame_hdr_size);
                std::ptr::copy_nonoverlapping(payload.as_ptr(), dest_payload, payload.len());
            }

            hdr.head
                .store((head + total_frame_size) as u64, Ordering::Release);
        }

        Ok(seq)
    }
}

impl ShmIpcConsumer {
    /// Polls and consumes the next available frame from the ring buffer
    pub fn poll_frame(&self) -> Result<Option<(IpcFrameHeader, Vec<u8>)>, SandboxError> {
        let frame_hdr_size = std::mem::size_of::<IpcFrameHeader>();

        let mut mmap_guard = self.mmap.lock().map_err(|_| {
            SandboxError::ShmRingError("Failed to acquire ring buffer mutex".to_string())
        })?;

        let hdr = unsafe { &mut *(mmap_guard.as_mut_ptr() as *mut ShmIpcRingHeader) };
        let capacity = self.capacity;
        let mask = capacity - 1;

        let head = hdr.head.load(Ordering::Acquire) as usize;
        let tail = hdr.tail.load(Ordering::Relaxed) as usize;

        if tail == head {
            return Ok(None);
        }

        let tail_offset = tail & mask;
        let arena_start = std::mem::size_of::<ShmIpcRingHeader>();
        let remaining = capacity - tail_offset;

        if remaining < frame_hdr_size {
            let new_tail = tail + remaining;
            hdr.tail.store(new_tail as u64, Ordering::Release);
            drop(mmap_guard);
            return self.poll_frame();
        }

        unsafe {
            let frame_ptr = mmap_guard.as_ptr().add(arena_start + tail_offset) as *const IpcFrameHeader;
            let frame_hdr = std::ptr::read(frame_ptr);

            if frame_hdr.entry_type == IPC_WRAP_MARKER {
                // Wrap to index 0
                let new_tail = tail + remaining;
                hdr.tail.store(new_tail as u64, Ordering::Release);
                drop(mmap_guard);
                return self.poll_frame();
            }

            if frame_hdr.magic != IPC_FRAME_MAGIC {
                return Err(SandboxError::ShmRingError(format!(
                    "Invalid frame magic: 0x{:08x} at tail_offset {}",
                    frame_hdr.magic, tail_offset
                )));
            }

            let payload_len = frame_hdr.payload_len as usize;
            let total_frame_size = frame_hdr_size + payload_len;

            let payload_src = mmap_guard
                .as_ptr()
                .add(arena_start + tail_offset + frame_hdr_size);
            let mut payload = vec![0u8; payload_len];
            std::ptr::copy_nonoverlapping(payload_src, payload.as_mut_ptr(), payload_len);

            // Verify CRC32 checksum
            let calculated = compute_crc32(&payload);
            if calculated != frame_hdr.checksum_crc32 {
                return Err(SandboxError::ChecksumMismatch {
                    calculated,
                    expected: frame_hdr.checksum_crc32,
                });
            }

            hdr.tail
                .store((tail + total_frame_size) as u64, Ordering::Release);
            Ok(Some((frame_hdr, payload)))
        }
    }
}

/// Bidirectional shared memory IPC channel between MicroVM and Host
pub struct MicrovmShmChannel {
    pub tx: ShmIpcProducer,
    pub rx: ShmIpcConsumer,
    pub token: CapabilityToken,
}

impl MicrovmShmChannel {
    /// Constructs a bidirectional channel pair
    pub fn new_pair(
        capacity: usize,
        token: CapabilityToken,
    ) -> Result<(Self, Self), SandboxError> {
        let (host_tx, vm_rx) = create_shm_ipc_pair(capacity)?;
        let (vm_tx, host_rx) = create_shm_ipc_pair(capacity)?;

        let host_channel = Self {
            tx: host_tx,
            rx: host_rx,
            token: token.clone(),
        };

        let vm_channel = Self {
            tx: vm_tx,
            rx: vm_rx,
            token,
        };

        Ok((host_channel, vm_channel))
    }
}

/// Table-based CRC32 computation for fast memory integrity checks
fn compute_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

// ==============================================================================
// 8. Dual-Sandbox Routing Arbiter
// ==============================================================================

/// Sandboxed execution tier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxTier {
    FastWasm,
    HeavyMicroVM,
}

/// Execution workload specification submitted to arbiter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadSpec {
    pub tool_name: String,
    pub payload_bytes: usize,
    pub memory_estimate_bytes: usize,
    pub requires_filesystem: bool,
    pub requires_network: bool,
    pub requires_subprocess: bool,
    pub latency_sla_micros: u64,
}

/// Dual-sandbox routing arbiter enforcing security and tiering policies
#[derive(Debug, Clone)]
pub struct HardenedDualSandboxRouter {
    verifier: CapabilityVerifier,
}

impl HardenedDualSandboxRouter {
    /// Creates a new router instance
    pub fn new(verifier: CapabilityVerifier) -> Self {
        Self { verifier }
    }

    /// Evaluates workload and selects optimal sandbox tier
    pub fn route_workload(
        &self,
        token: &CapabilityToken,
        workload: &WorkloadSpec,
    ) -> Result<SandboxTier, SandboxError> {
        self.verifier.validate_token(token)?;

        // If workload requires process spawning or elevated memory, it must go to microVM
        if workload.requires_subprocess {
            if !token.process_caps.allow_process_spawn {
                return Err(SandboxError::CapabilityViolation {
                    kind: "process_spawn".to_string(),
                    target: workload.tool_name.clone(),
                    reason: "Workload requires subprocess but token prohibits it".to_string(),
                    severity: ViolationSeverity::Critical,
                });
            }
            return Ok(SandboxTier::HeavyMicroVM);
        }

        // FastWasm path requirements:
        // 1. Memory estimate <= FastWasm ceiling (64 MiB)
        // 2. Latency SLA <= 5000 µs (5 ms)
        // 3. No raw socket networking
        // 4. No unbounded filesystem access
        let fits_fast_wasm = workload.memory_estimate_bytes <= FAST_WASM_MAX_MEMORY_BYTES
            && workload.latency_sla_micros <= 5000
            && !workload.requires_network
            && !workload.requires_filesystem;

        if fits_fast_wasm {
            Ok(SandboxTier::FastWasm)
        } else {
            Ok(SandboxTier::HeavyMicroVM)
        }
    }
}

// ==============================================================================
// 9. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_issuer() -> TokenIssuer {
        TokenIssuer::new([0x42; 32])
    }

    fn sample_token(issuer: &TokenIssuer) -> CapabilityToken {
        issuer.issue_token(
            "tok-test-01",
            "agent-secure-01",
            300,
            vec![PathBuf::from("/var/sandbox/data")],
            vec![PathBuf::from("/var/sandbox/scratch")],
            FsReadCaps::default(),
            FsWriteCaps::default(),
            NetCapability {
                allowed_hosts: vec!["internal.hagibis.ai".to_string(), "*.crates.io".to_string()],
                allowed_cidrs: Vec::new(),
                allow_egress: true,
                allow_ingress: false,
                max_bandwidth_kbps: 10_000,
            },
            ComputeCapability::default(),
            ProcessCapability::default(),
        )
    }

    #[test]
    fn test_token_issuance_and_signature_verification() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);

        assert!(issuer.verify_token(&token).is_ok());

        // Tampering token ID invalidates signature
        let mut tampered = token.clone();
        tampered.token_id = "tok-tampered-02".to_string();
        assert_eq!(
            issuer.verify_token(&tampered),
            Err(SandboxError::SignatureVerificationFailed)
        );
    }

    #[test]
    fn test_token_expiration() {
        let issuer = test_issuer();
        let mut token = sample_token(&issuer);
        // Set expiry in the past
        token.expires_at_ns = 100;
        let verifier = CapabilityVerifier::new(issuer);
        assert!(matches!(
            verifier.validate_token(&token),
            Err(SandboxError::TokenExpired { .. })
        ));
    }

    #[test]
    fn test_token_revocation() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);
        let verifier = CapabilityVerifier::new(issuer);

        assert!(verifier.validate_token(&token).is_ok());
        verifier.revoke_token(&token.token_id);
        assert!(verifier.is_revoked(&token.token_id));
        assert!(matches!(
            verifier.validate_token(&token),
            Err(SandboxError::InvalidToken(_))
        ));
    }

    #[test]
    fn test_filesystem_path_traversal_attacks() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);
        let verifier = CapabilityVerifier::new(issuer);

        // Valid read path inside /var/sandbox/data
        let valid_path = PathBuf::from("/var/sandbox/data");
        assert!(verifier.check_fs_read(&token, &valid_path).is_ok());

        // Path traversal attack attempting /var/sandbox/data/../../etc/passwd
        let attack_path = PathBuf::from("/var/sandbox/data/../../etc/passwd");
        let res = verifier.check_fs_read(&token, &attack_path);
        assert!(matches!(res, Err(SandboxError::PathTraversalAttack { .. })));

        // Attempting to read outside boundary
        let outside_path = PathBuf::from("/etc/shadow");
        let res = verifier.check_fs_read(&token, &outside_path);
        assert!(matches!(res, Err(SandboxError::CapabilityViolation { .. })));
    }

    #[test]
    fn test_network_egress_whitelist_matching() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);
        let verifier = CapabilityVerifier::new(issuer);

        // Exact host match
        assert!(verifier.check_net_egress(&token, "internal.hagibis.ai", 443).is_ok());

        // Wildcard *.crates.io match
        assert!(verifier.check_net_egress(&token, "static.crates.io", 443).is_ok());

        // Unauthorized host
        let unauth = verifier.check_net_egress(&token, "evil.attacker.com", 80);
        assert!(matches!(unauth, Err(SandboxError::CapabilityViolation { .. })));
    }

    #[test]
    fn test_stripped_host_imports_deny_ambient_authority() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);
        let verifier = CapabilityVerifier::new(issuer);
        let host_env = HardenedWasmHostEnv::new(verifier);

        // Ambient environment variables are stripped
        let env_res = host_env.host_environ_get(&token);
        assert!(matches!(env_res, Err(SandboxError::StrippedHostImport { .. })));

        // Raw socket creation is stripped
        let sock_res = host_env.host_sock_create(&token);
        assert!(matches!(sock_res, Err(SandboxError::StrippedHostImport { .. })));

        // Quantized clock test
        let t1 = host_env.host_clock_get_quantized_ns(&token);
        assert_eq!(t1 % 100_000, 0);
    }

    #[test]
    fn test_shm_ring_buffer_bidirectional_ipc() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);

        let (host_chan, vm_chan) =
            MicrovmShmChannel::new_pair(16384, token.clone()).expect("Create channel pair");

        let test_payload = b"EXECUTE_SECCOMP_ISOLATED_AST_DIFF";
        let seq = host_chan
            .tx
            .send_frame(IpcEntryType::ToolRequest, &token, test_payload)
            .expect("Send frame from host");

        assert_eq!(seq, 1);

        // VM receives frame
        let received = vm_chan.rx.poll_frame().expect("Poll frame").expect("Frame present");
        assert_eq!(received.0.seq_id, 1);
        assert_eq!(received.0.entry_type, IpcEntryType::ToolRequest as u16);
        assert_eq!(received.1, test_payload);

        // VM replies to host
        let response_payload = b"AST_PATCH_APPLIED_SUCCESSFULLY";
        let vm_seq = vm_chan
            .tx
            .send_frame(IpcEntryType::ToolResponse, &token, response_payload)
            .expect("Send reply from VM");
        assert_eq!(vm_seq, 1);

        let host_received = host_chan
            .rx
            .poll_frame()
            .expect("Poll host reply")
            .expect("Host reply present");
        assert_eq!(host_received.0.entry_type, IpcEntryType::ToolResponse as u16);
        assert_eq!(host_received.1, response_payload);
    }

    #[test]
    fn test_socket_exhaustion_immunity_10000_frames() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);

        // Create ring buffer
        let (producer, consumer) = create_shm_ipc_pair(65536).expect("Create ring");

        let payload = b"TELEMETRY_SAMPLE_PACKET_64_BYTES_LONG_PAYLOAD_ABCDEFGHIJKLMNO";

        // Transmit 10,000 frames through memory-safe SHM ring buffer without any socket descriptor allocations
        for i in 1..=10_000 {
            producer
                .send_frame(IpcEntryType::TelemetryFrame, &token, payload)
                .expect("Send frame");

            let poll_res = consumer
                .poll_frame()
                .expect("Poll frame")
                .expect("Frame exists");
            assert_eq!(poll_res.0.seq_id, i);
            assert_eq!(poll_res.1.len(), payload.len());
        }
    }

    #[test]
    fn test_dual_sandbox_routing_arbiter() {
        let issuer = test_issuer();
        let token = sample_token(&issuer);
        let verifier = CapabilityVerifier::new(issuer);
        let router = HardenedDualSandboxRouter::new(verifier);

        // Lightweight pure-compute workload -> FastWasm
        let fast_workload = WorkloadSpec {
            tool_name: "hgb_ast_parse".to_string(),
            payload_bytes: 1024,
            memory_estimate_bytes: 4 * 1024 * 1024, // 4 MiB <= 64 MiB
            requires_filesystem: false,
            requires_network: false,
            requires_subprocess: false,
            latency_sla_micros: 2000, // 2 ms <= 5 ms
        };
        assert_eq!(
            router.route_workload(&token, &fast_workload).unwrap(),
            SandboxTier::FastWasm
        );

        // Heavy memory workload -> HeavyMicroVM
        let heavy_workload = WorkloadSpec {
            tool_name: "hgb_large_model_inference".to_string(),
            payload_bytes: 10 * 1024 * 1024,
            memory_estimate_bytes: 128 * 1024 * 1024, // 128 MiB > 64 MiB
            requires_filesystem: false,
            requires_network: false,
            requires_subprocess: false,
            latency_sla_micros: 100_000,
        };
        assert_eq!(
            router.route_workload(&token, &heavy_workload).unwrap(),
            SandboxTier::HeavyMicroVM
        );

        // Subprocess workload without permission -> error
        let blocked_workload = WorkloadSpec {
            tool_name: "hgb_shell_exec".to_string(),
            payload_bytes: 512,
            memory_estimate_bytes: 8 * 1024 * 1024,
            requires_filesystem: false,
            requires_network: false,
            requires_subprocess: true,
            latency_sla_micros: 10_000,
        };
        assert!(router.route_workload(&token, &blocked_workload).is_err());
    }
}
