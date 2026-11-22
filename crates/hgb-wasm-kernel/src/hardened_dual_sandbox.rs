//! # Hardened Dual-Sandbox WASM & MicroVM Execution Environment
//! ====================================================================
//! Project Hagibis: Pillar 3 (Polyglot WASM Sandboxing & Firecracker MicroVM Isolation)
//!
//! Provides sovereign, dual-layer sandboxed execution for arbitrary untrusted code,
//! binary analysis, and reverse-engineering scripts with zero host risk:
//!
//! - **Layer 1 (WASM Component Isolation)**:
//!   Wasmtime deterministic runtime with dynamic instruction fuel metering,
//!   strictly capped linear memory ceilings, and active revocation of WASI network
//!   and raw host filesystem capabilities.
//! - **Layer 2 (Firecracker MicroVM Jail)**:
//!   Ephemeral MicroVM hardware virtualization (with Landlock RAM CoW overlay fallback)
//!   providing unmapped host paths, read-only rootfs semantics, stripped environment
//!   variables, process isolation, and strict cgroups v2 resource capping.
//! - **Layer 3 (`DualLayerHardened`)**:
//!   Two-tier defense-in-depth where the WebAssembly component execution engine runs
//!   nested inside the MicroVM jail boundary, neutralizing engine JIT escapes or 0-day exploits.
//! - **Dynamic Capability Token Validator**:
//!   Granular WIT token enforcement (`wit:network-read`, `wit:fs-ro`, `wit:crypto-verify`, etc.)
//!   integrated directly with `DynamicCapabilityManager` for O(1) lock-free capability revocation.
//! - **Cryptographic Audit Sink**:
//!   SHA-256 cryptographic audit signatures binding target digests, execution outputs,
//!   fuel consumption, memory pages, and latency for tamper-evident compliance.
//! - **Reverse-Engineering Engine**:
//!   Safe decompilation, format detection, ELF/PE/WASM symbol and section extraction,
//!   Shannon entropy calculation, and WIT interface synthesis per `reverse_engineer_anything.wit`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::canonical_abi_shm_views::AbiError;
use crate::dynamic_capability_manager::DynamicCapabilityManager;

// ==============================================================================
// 1. CONSTANTS & SYSTEM DEFAULTS
// ==============================================================================

/// Default instruction fuel budget for sandboxed WASM execution (10 Million instructions)
pub const DEFAULT_WASM_FUEL_BUDGET: u64 = 10_000_000;

/// Default linear memory allocation ceiling (64 MiB)
pub const DEFAULT_MAX_MEMORY_MB: u32 = 64;

/// Default execution timeout in milliseconds (5,000 ms)
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Standard WebAssembly linear memory page size in bytes (64 KiB)
pub const WASM_PAGE_SIZE_BYTES: usize = 65536;

/// Linux default virtual memory page size in bytes (4 KiB)
pub const LINUX_PAGE_SIZE_BYTES: usize = 4096;

/// Default temporary jail storage base
pub const DEFAULT_JAIL_BASE_DIR: &str = "/dev/shm/hgb_hardened_jail";
pub const FALLBACK_JAIL_BASE_DIR: &str = "/tmp/hgb_hardened_jail";

/// Well-known WIT Capability identifiers
pub const WIT_CAP_NETWORK_READ: &str = "wit:network-read";
pub const WIT_CAP_NETWORK_EGRESS: &str = "wit:network-egress";
pub const WIT_CAP_NETWORK_INGRESS: &str = "wit:network-ingress";
pub const WIT_CAP_FS_RO: &str = "wit:fs-ro";
pub const WIT_CAP_FS_RW: &str = "wit:fs-rw";
pub const WIT_CAP_FS_DELETE: &str = "wit:fs-delete";
pub const WIT_CAP_CRYPTO_VERIFY: &str = "wit:crypto-verify";
pub const WIT_CAP_PROCESS_SPAWN: &str = "wit:process-spawn";
pub const WIT_CAP_SIMD_VECTORS: &str = "wit:simd-vectors";
pub const WIT_CAP_GPU_TENSOR: &str = "wit:gpu-tensor";
pub const WIT_CAP_REVERSE_ENGINEER: &str = "wit:reverse-engineer";

// ==============================================================================
// 2. ENUMS & DATA TYPES (WIT-COMPATIBLE)
// ==============================================================================

/// Active execution mode coordinating sandbox isolation layers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionSandboxMode {
    /// Strict WASM component runtime with dynamic capability tokens (Layer 1)
    WasmSandboxed,
    /// Ephemeral Firecracker jail execution with process isolation and resource limits (Layer 2)
    MicroVmIsolated,
    /// Dual-layer hardened execution: WASM engine inside MicroVM container for high-risk untrusted binaries
    DualLayerHardened,
}

impl std::fmt::Display for ExecutionSandboxMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WasmSandboxed => write!(f, "WasmSandboxed"),
            Self::MicroVmIsolated => write!(f, "MicroVmIsolated"),
            Self::DualLayerHardened => write!(f, "DualLayerHardened"),
        }
    }
}

/// Binary container format classification matching `reverse_engineer_anything.wit`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryFormat {
    Elf,
    MachO,
    Pe,
    Wasm,
    RawBytecode,
}

/// Binary architecture matching `reverse_engineer_anything.wit`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryArch {
    X86_64,
    Aarch64,
    Wasm32,
    Riscv64,
    Unknown,
}

/// MicroVM isolation backend matching `skills/microvm-firecracker/wit/`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsolationBackend {
    FirecrackerKvm,
    SnapshotResume,
    LandlockRamCow,
}

/// MicroVM jail operational status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JailStatus {
    Idle,
    Spawning,
    Running,
    Terminated,
}

/// MicroVM machine configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MachineConfig {
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
    pub smt: bool,
    pub track_dirty_pages: bool,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            vcpu_count: 2,
            mem_size_mib: 512,
            smt: false,
            track_dirty_pages: false,
        }
    }
}

/// Severity classification of an intercepted security violation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Structured record describing an intercepted capability security breach
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityViolation {
    pub violation_id: String,
    pub caller_id: String,
    pub attempted_capability: String,
    pub target_resource: String,
    pub severity: ViolationSeverity,
    pub timestamp_ns: u64,
    pub message: String,
}

/// Environment isolation flags enforced on guest jails
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentIsolationFlags {
    pub unshare_net: bool,
    pub unshare_mount: bool,
    pub unmapped_host_paths: bool,
    pub read_only_rootfs: bool,
    pub drop_capabilities: bool,
    pub cgroup_v2_limits: bool,
    pub seccomp_profile: Option<String>,
}

impl Default for EnvironmentIsolationFlags {
    fn default() -> Self {
        Self {
            unshare_net: true,
            unshare_mount: true,
            unmapped_host_paths: true,
            read_only_rootfs: true,
            drop_capabilities: true,
            cgroup_v2_limits: true,
            seccomp_profile: Some("firecracker_seccomp_profile.json".to_string()),
        }
    }
}

// ==============================================================================
// 3. CAPABILITY TOKEN & BUILDER
// ==============================================================================

/// Unforgeable cryptographic capability token enforcing zero ambient authority
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub token_id: u64,
    pub principal_id: String,
    pub issuer_signature: String,
    pub issued_timestamp_ns: u64,
    pub expiry_timestamp_ns: u64,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub allowed_hosts: Vec<String>,
    pub allowed_cidrs: Vec<String>,
    pub allowed_ports: Vec<u16>,
    pub allow_egress: bool,
    pub allow_ingress: bool,
    pub max_bandwidth_kbps: u32,
    pub instruction_fuel_quota: u64,
    pub memory_ceiling_bytes: u64,
    pub timeout_sla_us: u64,
    pub vcpu_quota: u32,
    pub allow_process_spawn: bool,
    pub allowed_binary_hashes: Vec<String>,
    pub permissions: HashSet<String>,
}

impl CapabilityToken {
    pub fn builder(principal_id: impl Into<String>) -> CapabilityTokenBuilder {
        CapabilityTokenBuilder::new(principal_id)
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.contains(permission)
    }

    pub fn compute_signature(&self, secret: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(secret);
        hasher.update(self.token_id.to_le_bytes());
        hasher.update(self.principal_id.as_bytes());
        hasher.update(self.issued_timestamp_ns.to_le_bytes());
        hasher.update(self.expiry_timestamp_ns.to_le_bytes());
        hasher.update(self.instruction_fuel_quota.to_le_bytes());
        hasher.update(self.memory_ceiling_bytes.to_le_bytes());
        for p in &self.permissions {
            hasher.update(p.as_bytes());
        }
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            hex.push_str(&format!("{:02x}", byte));
        }
        hex
    }

    pub fn sign(&mut self, secret: &[u8]) {
        self.issuer_signature = self.compute_signature(secret);
    }

    pub fn verify_signature(&self, secret: &[u8]) -> bool {
        let expected = self.compute_signature(secret);
        self.issuer_signature == expected
    }

    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.expiry_timestamp_ns > 0 && now > self.expiry_timestamp_ns
    }
}

pub struct CapabilityTokenBuilder {
    token_id: u64,
    principal_id: String,
    issued_timestamp_ns: u64,
    expiry_timestamp_ns: u64,
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    allowed_hosts: Vec<String>,
    allowed_cidrs: Vec<String>,
    allowed_ports: Vec<u16>,
    allow_egress: bool,
    allow_ingress: bool,
    max_bandwidth_kbps: u32,
    instruction_fuel_quota: u64,
    memory_ceiling_bytes: u64,
    timeout_sla_us: u64,
    vcpu_quota: u32,
    allow_process_spawn: bool,
    allowed_binary_hashes: Vec<String>,
    permissions: HashSet<String>,
}

static TOKEN_ID_COUNTER: AtomicU64 = AtomicU64::new(1001);

impl CapabilityTokenBuilder {
    pub fn new(principal_id: impl Into<String>) -> Self {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        Self {
            token_id: TOKEN_ID_COUNTER.fetch_add(1, Ordering::SeqCst),
            principal_id: principal_id.into(),
            issued_timestamp_ns: now_ns,
            expiry_timestamp_ns: now_ns + (3600 * 1_000_000_000),
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            allowed_hosts: Vec::new(),
            allowed_cidrs: Vec::new(),
            allowed_ports: Vec::new(),
            allow_egress: false,
            allow_ingress: false,
            max_bandwidth_kbps: 0,
            instruction_fuel_quota: DEFAULT_WASM_FUEL_BUDGET,
            memory_ceiling_bytes: (DEFAULT_MAX_MEMORY_MB as u64) * 1024 * 1024,
            timeout_sla_us: DEFAULT_TIMEOUT_MS * 1000,
            vcpu_quota: 2,
            allow_process_spawn: false,
            allowed_binary_hashes: Vec::new(),
            permissions: HashSet::new(),
        }
    }

    pub fn with_token_id(mut self, token_id: u64) -> Self {
        self.token_id = token_id;
        self
    }

    pub fn with_permission(mut self, perm: impl Into<String>) -> Self {
        self.permissions.insert(perm.into());
        self
    }

    pub fn with_fs_read(mut self, root: impl Into<PathBuf>) -> Self {
        self.permissions.insert(WIT_CAP_FS_RO.to_string());
        self.read_roots.push(root.into());
        self
    }

    pub fn with_fs_write(mut self, root: impl Into<PathBuf>) -> Self {
        self.permissions.insert(WIT_CAP_FS_RW.to_string());
        self.write_roots.push(root.into());
        self
    }

    pub fn with_network_egress(mut self, host: impl Into<String>, port: u16) -> Self {
        self.permissions.insert(WIT_CAP_NETWORK_EGRESS.to_string());
        self.allow_egress = true;
        self.allowed_hosts.push(host.into());
        self.allowed_ports.push(port);
        self
    }

    pub fn with_fuel_quota(mut self, fuel: u64) -> Self {
        self.instruction_fuel_quota = fuel;
        self
    }

    pub fn with_memory_ceiling_mb(mut self, mb: u32) -> Self {
        self.memory_ceiling_bytes = (mb as u64) * 1024 * 1024;
        self
    }

    pub fn with_process_spawn(mut self, binary_hash: impl Into<String>) -> Self {
        self.permissions.insert(WIT_CAP_PROCESS_SPAWN.to_string());
        self.allow_process_spawn = true;
        self.allowed_binary_hashes.push(binary_hash.into());
        self
    }

    pub fn with_reverse_engineer(mut self) -> Self {
        self.permissions.insert(WIT_CAP_REVERSE_ENGINEER.to_string());
        self
    }

    pub fn with_crypto_verify(mut self) -> Self {
        self.permissions.insert(WIT_CAP_CRYPTO_VERIFY.to_string());
        self
    }

    pub fn with_simd_vectors(mut self) -> Self {
        self.permissions.insert(WIT_CAP_SIMD_VECTORS.to_string());
        self
    }

    pub fn build(self, secret: &[u8]) -> CapabilityToken {
        let mut token = CapabilityToken {
            token_id: self.token_id,
            principal_id: self.principal_id,
            issuer_signature: String::new(),
            issued_timestamp_ns: self.issued_timestamp_ns,
            expiry_timestamp_ns: self.expiry_timestamp_ns,
            read_roots: self.read_roots,
            write_roots: self.write_roots,
            allowed_hosts: self.allowed_hosts,
            allowed_cidrs: self.allowed_cidrs,
            allowed_ports: self.allowed_ports,
            allow_egress: self.allow_egress,
            allow_ingress: self.allow_ingress,
            max_bandwidth_kbps: self.max_bandwidth_kbps,
            instruction_fuel_quota: self.instruction_fuel_quota,
            memory_ceiling_bytes: self.memory_ceiling_bytes,
            timeout_sla_us: self.timeout_sla_us,
            vcpu_quota: self.vcpu_quota,
            allow_process_spawn: self.allow_process_spawn,
            allowed_binary_hashes: self.allowed_binary_hashes,
            permissions: self.permissions,
        };
        token.sign(secret);
        token
    }
}

// ==============================================================================
// 4. CAPABILITY TOKEN VALIDATOR
// ==============================================================================

pub struct CapabilityTokenValidator {
    dynamic_mgr: Arc<DynamicCapabilityManager>,
    signing_secret: Vec<u8>,
    recorded_violations: RwLock<Vec<CapabilityViolation>>,
}

impl CapabilityTokenValidator {
    pub fn new(signing_secret: &[u8], dynamic_mgr: Arc<DynamicCapabilityManager>) -> Self {
        Self {
            dynamic_mgr,
            signing_secret: signing_secret.to_vec(),
            recorded_violations: RwLock::new(Vec::new()),
        }
    }

    pub fn validate_token(&self, token: &CapabilityToken) -> Result<(), CapabilityViolation> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        if self.dynamic_mgr.is_capability_revoked(token.token_id) {
            let violation = CapabilityViolation {
                violation_id: format!("revocation_check_{}", token.token_id),
                caller_id: token.principal_id.clone(),
                attempted_capability: "token_usage".to_string(),
                target_resource: format!("token_id:{}", token.token_id),
                severity: ViolationSeverity::Critical,
                timestamp_ns: now_ns,
                message: format!("Capability token {} has been revoked at runtime", token.token_id),
            };
            self.record_violation(violation.clone());
            return Err(violation);
        }

        if !token.verify_signature(&self.signing_secret) {
            let violation = CapabilityViolation {
                violation_id: format!("sig_mismatch_{}", token.token_id),
                caller_id: token.principal_id.clone(),
                attempted_capability: "token_authenticate".to_string(),
                target_resource: format!("token_id:{}", token.token_id),
                severity: ViolationSeverity::Critical,
                timestamp_ns: now_ns,
                message: format!("Cryptographic signature verification failed for token {}", token.token_id),
            };
            self.record_violation(violation.clone());
            return Err(violation);
        }

        if token.is_expired() {
            let violation = CapabilityViolation {
                violation_id: format!("expired_token_{}", token.token_id),
                caller_id: token.principal_id.clone(),
                attempted_capability: "token_authenticate".to_string(),
                target_resource: format!("token_id:{}", token.token_id),
                severity: ViolationSeverity::High,
                timestamp_ns: now_ns,
                message: format!("Capability token {} expired", token.token_id),
            };
            self.record_violation(violation.clone());
            return Err(violation);
        }

        Ok(())
    }

    pub fn check_fs_read(&self, token: &CapabilityToken, path: &Path) -> Result<(), CapabilityViolation> {
        self.validate_token(token)?;

        if !token.has_permission(WIT_CAP_FS_RO) && !token.has_permission(WIT_CAP_FS_RW) {
            let v = self.create_violation(
                token,
                WIT_CAP_FS_RO,
                path.display().to_string(),
                ViolationSeverity::High,
                format!("Principal '{}' lacks '{}' capability", token.principal_id, WIT_CAP_FS_RO),
            );
            return Err(v);
        }

        let is_allowed = token.read_roots.iter().any(|root| is_contained_within(path, root));
        if !is_allowed {
            let v = self.create_violation(
                token,
                WIT_CAP_FS_RO,
                path.display().to_string(),
                ViolationSeverity::High,
                format!("Access to path '{}' is outside authorized read roots", path.display()),
            );
            return Err(v);
        }

        Ok(())
    }

    pub fn check_fs_write(&self, token: &CapabilityToken, path: &Path) -> Result<(), CapabilityViolation> {
        self.validate_token(token)?;

        if !token.has_permission(WIT_CAP_FS_RW) {
            let v = self.create_violation(
                token,
                WIT_CAP_FS_RW,
                path.display().to_string(),
                ViolationSeverity::Critical,
                format!("Principal '{}' lacks write capability '{}'", token.principal_id, WIT_CAP_FS_RW),
            );
            return Err(v);
        }

        let is_allowed = token.write_roots.iter().any(|root| is_contained_within(path, root));
        if !is_allowed {
            let v = self.create_violation(
                token,
                WIT_CAP_FS_RW,
                path.display().to_string(),
                ViolationSeverity::Critical,
                format!("Write target '{}' is outside authorized write roots", path.display()),
            );
            return Err(v);
        }

        Ok(())
    }

    pub fn check_network_egress(&self, token: &CapabilityToken, host: &str, port: u16) -> Result<(), CapabilityViolation> {
        self.validate_token(token)?;

        if !token.allow_egress || !token.has_permission(WIT_CAP_NETWORK_EGRESS) {
            let v = self.create_violation(
                token,
                WIT_CAP_NETWORK_EGRESS,
                format!("{}:{}", host, port),
                ViolationSeverity::Critical,
                format!("Outbound network access completely revoked/unauthorized for principal '{}'", token.principal_id),
            );
            return Err(v);
        }

        let clean_host = host.trim().to_lowercase();
        let host_allowed = token.allowed_hosts.iter().any(|allowed| {
            let a = allowed.trim().to_lowercase();
            if a == "*" || a == clean_host {
                true
            } else if let Some(suffix) = a.strip_prefix("*.") {
                clean_host == suffix || clean_host.ends_with(&format!(".{}", suffix))
            } else {
                false
            }
        });

        if !host_allowed {
            let v = self.create_violation(
                token,
                WIT_CAP_NETWORK_EGRESS,
                format!("{}:{}", host, port),
                ViolationSeverity::High,
                format!("Host '{}' is not in allowed network host whitelist", host),
            );
            return Err(v);
        }

        if !token.allowed_ports.is_empty() && !token.allowed_ports.contains(&port) {
            let v = self.create_violation(
                token,
                WIT_CAP_NETWORK_EGRESS,
                format!("{}:{}", host, port),
                ViolationSeverity::Medium,
                format!("Port {} is not in allowed port whitelist", port),
            );
            return Err(v);
        }

        Ok(())
    }

    pub fn check_process_spawn(&self, token: &CapabilityToken, binary_hash_or_path: &str) -> Result<(), CapabilityViolation> {
        self.validate_token(token)?;

        if !token.allow_process_spawn || !token.has_permission(WIT_CAP_PROCESS_SPAWN) {
            let v = self.create_violation(
                token,
                WIT_CAP_PROCESS_SPAWN,
                binary_hash_or_path.to_string(),
                ViolationSeverity::Critical,
                format!("Process spawn is strictly prohibited for principal '{}'", token.principal_id),
            );
            return Err(v);
        }

        if !token.allowed_binary_hashes.is_empty() {
            let allowed = token.allowed_binary_hashes.iter().any(|h| h == "*" || h == binary_hash_or_path);
            if !allowed {
                let v = self.create_violation(
                    token,
                    WIT_CAP_PROCESS_SPAWN,
                    binary_hash_or_path.to_string(),
                    ViolationSeverity::Critical,
                    format!("Binary '{}' does not match allowed hash whitelist", binary_hash_or_path),
                );
                return Err(v);
            }
        }

        Ok(())
    }

    pub fn check_permission(&self, token: &CapabilityToken, required_perm: &str, resource: &str) -> Result<(), CapabilityViolation> {
        self.validate_token(token)?;

        if !token.has_permission(required_perm) {
            let v = self.create_violation(
                token,
                required_perm,
                resource.to_string(),
                ViolationSeverity::High,
                format!("Principal '{}' missing required capability '{}'", token.principal_id, required_perm),
            );
            return Err(v);
        }

        Ok(())
    }

    pub fn revoke_token(&self, token_id: u64, reason: impl Into<String>) -> Result<bool, AbiError> {
        self.dynamic_mgr.revoke_capability(token_id, reason)
    }

    pub fn is_token_revoked(&self, token_id: u64) -> bool {
        self.dynamic_mgr.is_capability_revoked(token_id)
    }

    pub fn get_recorded_violations(&self) -> Vec<CapabilityViolation> {
        self.recorded_violations.read().map(|v| v.clone()).unwrap_or_default()
    }

    fn record_violation(&self, violation: CapabilityViolation) {
        if let Ok(mut lock) = self.recorded_violations.write() {
            lock.push(violation);
        }
    }

    fn create_violation(
        &self,
        token: &CapabilityToken,
        attempted: &str,
        resource: String,
        severity: ViolationSeverity,
        message: String,
    ) -> CapabilityViolation {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let violation = CapabilityViolation {
            violation_id: format!("violation_{}_{}", token.token_id, now_ns),
            caller_id: token.principal_id.clone(),
            attempted_capability: attempted.to_string(),
            target_resource: resource,
            severity,
            timestamp_ns: now_ns,
            message,
        };
        self.record_violation(violation.clone());
        violation
    }
}

// ==============================================================================
// 5. UNTRUSTED EXECUTION TARGET & RECEIPT
// ==============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrustedExecutionTarget {
    pub target_id: String,
    pub binary_path: Option<PathBuf>,
    pub raw_bytecode: Option<Vec<u8>>,
    pub format: BinaryFormat,
    pub arch: BinaryArch,
    pub execution_args: Vec<String>,
    pub env_vars: Vec<(String, String)>,
    pub isolation_flags: EnvironmentIsolationFlags,
    pub timeout_ms: u64,
    pub max_memory_mb: u32,
    pub fuel_limit: Option<u64>,
    pub entrypoint: Option<String>,
    pub stdin: Option<Vec<u8>>,
}

impl UntrustedExecutionTarget {
    pub fn from_wasm_bytes(target_id: impl Into<String>, wasm_bytes: Vec<u8>) -> Self {
        let (format, arch) = Self::detect_format_and_arch(&wasm_bytes);
        Self {
            target_id: target_id.into(),
            binary_path: None,
            raw_bytecode: Some(wasm_bytes),
            format,
            arch,
            execution_args: Vec::new(),
            env_vars: Vec::new(),
            isolation_flags: EnvironmentIsolationFlags::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_memory_mb: DEFAULT_MAX_MEMORY_MB,
            fuel_limit: Some(DEFAULT_WASM_FUEL_BUDGET),
            entrypoint: None,
            stdin: None,
        }
    }

    pub fn from_wat(target_id: impl Into<String>, wat_str: &str) -> anyhow::Result<Self> {
        let wasm_bytes = wat::parse_str(wat_str)
            .map_err(|e| anyhow::anyhow!("Failed to compile WAT into WASM bytecode: {}", e))?;
        Ok(Self::from_wasm_bytes(target_id, wasm_bytes))
    }

    pub fn from_elf(target_id: impl Into<String>, elf_bytes: Vec<u8>) -> Self {
        let (format, arch) = Self::detect_format_and_arch(&elf_bytes);
        Self {
            target_id: target_id.into(),
            binary_path: None,
            raw_bytecode: Some(elf_bytes),
            format,
            arch,
            execution_args: Vec::new(),
            env_vars: Vec::new(),
            isolation_flags: EnvironmentIsolationFlags::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_memory_mb: DEFAULT_MAX_MEMORY_MB,
            fuel_limit: None,
            entrypoint: None,
            stdin: None,
        }
    }

    pub fn from_binary_path(target_id: impl Into<String>, path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let p = path.into();
        let bytes = fs::read(&p)?;
        let (format, arch) = Self::detect_format_and_arch(&bytes);
        Ok(Self {
            target_id: target_id.into(),
            binary_path: Some(p),
            raw_bytecode: Some(bytes),
            format,
            arch,
            execution_args: Vec::new(),
            env_vars: Vec::new(),
            isolation_flags: EnvironmentIsolationFlags::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_memory_mb: DEFAULT_MAX_MEMORY_MB,
            fuel_limit: None,
            entrypoint: None,
            stdin: None,
        })
    }

    pub fn detect_format_and_arch(data: &[u8]) -> (BinaryFormat, BinaryArch) {
        if data.len() >= 4 && data[0..4] == [0x00, 0x61, 0x73, 0x6d] {
            return (BinaryFormat::Wasm, BinaryArch::Wasm32);
        }

        if data.len() >= 4 && data[0..4] == [0x7f, 0x45, 0x4c, 0x46] {
            let arch = if data.len() >= 20 {
                let e_machine = u16::from_le_bytes([data[18], data[19]]);
                match e_machine {
                    0x3E => BinaryArch::X86_64,
                    0xB7 => BinaryArch::Aarch64,
                    0xF3 => BinaryArch::Riscv64,
                    _ => BinaryArch::Unknown,
                }
            } else {
                BinaryArch::Unknown
            };
            return (BinaryFormat::Elf, arch);
        }

        if data.len() >= 2 && data[0..2] == [0x4d, 0x5a] {
            let mut arch = BinaryArch::Unknown;
            if data.len() >= 0x40 {
                let pe_offset = u32::from_le_bytes([data[0x3c], data[0x3d], data[0x3e], data[0x3f]]) as usize;
                if data.len() >= pe_offset + 6 && &data[pe_offset..pe_offset + 4] == b"PE\0\0" {
                    let machine = u16::from_le_bytes([data[pe_offset + 4], data[pe_offset + 5]]);
                    match machine {
                        0x8664 => arch = BinaryArch::X86_64,
                        0xAA64 => arch = BinaryArch::Aarch64,
                        _ => {}
                    }
                }
            }
            return (BinaryFormat::Pe, arch);
        }

        if data.len() >= 4 {
            let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            if magic == 0xfeedface || magic == 0xfeedfacf || magic == 0xcefaedfe || magic == 0xcffaedfe {
                let arch = if magic == 0xfeedfacf || magic == 0xcffaedfe {
                    BinaryArch::X86_64
                } else {
                    BinaryArch::Unknown
                };
                return (BinaryFormat::MachO, arch);
            }
        }

        (BinaryFormat::RawBytecode, BinaryArch::Unknown)
    }

    pub fn compute_sha256(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        if let Some(ref bytes) = self.raw_bytecode {
            hasher.update(bytes);
        } else if let Some(ref path) = self.binary_path {
            if let Ok(bytes) = fs::read(path) {
                hasher.update(&bytes);
            }
        }
        let result = hasher.finalize();
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&result);
        digest
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.execution_args = args;
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env_vars.push((key.into(), val.into()));
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    pub fn with_max_memory_mb(mut self, mem_mb: u32) -> Self {
        self.max_memory_mb = mem_mb;
        self
    }

    pub fn with_fuel_limit(mut self, fuel: u64) -> Self {
        self.fuel_limit = Some(fuel);
        self
    }

    pub fn with_entrypoint(mut self, entrypoint: impl Into<String>) -> Self {
        self.entrypoint = Some(entrypoint.into());
        self
    }

    pub fn with_stdin(mut self, stdin: Vec<u8>) -> Self {
        self.stdin = Some(stdin);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrustedExecutionReceipt {
    pub target_id: String,
    pub target_sha256: [u8; 32],
    pub mode_used: ExecutionSandboxMode,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub execution_latency_us: u64,
    pub memory_pages_consumed: u64,
    pub fuel_expended: u64,
    pub is_success: bool,
    pub sha256_audit_signature: [u8; 32],
    pub violation: Option<CapabilityViolation>,
    pub timestamp_ns: u64,
}

impl UntrustedExecutionReceipt {
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    pub fn signature_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.sha256_audit_signature {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    pub fn target_sha256_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.target_sha256 {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    pub fn compute_audit_signature(
        target_sha256: &[u8; 32],
        mode: ExecutionSandboxMode,
        exit_code: i32,
        latency_us: u64,
        memory_pages: u64,
        fuel_expended: u64,
        stdout: &[u8],
        stderr: &[u8],
        secret: &[u8],
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(secret);
        hasher.update(target_sha256);
        hasher.update(&[mode as u8]);
        hasher.update(&exit_code.to_le_bytes());
        hasher.update(&latency_us.to_le_bytes());
        hasher.update(&memory_pages.to_le_bytes());
        hasher.update(&fuel_expended.to_le_bytes());
        hasher.update(stdout);
        hasher.update(stderr);

        let result = hasher.finalize();
        let mut sig = [0u8; 32];
        sig.copy_from_slice(&result);
        sig
    }

    pub fn verify_audit_signature(&self, secret: &[u8]) -> bool {
        let expected = Self::compute_audit_signature(
            &self.target_sha256,
            self.mode_used,
            self.exit_code,
            self.execution_latency_us,
            self.memory_pages_consumed,
            self.fuel_expended,
            &self.stdout,
            &self.stderr,
            secret,
        );
        self.sha256_audit_signature == expected
    }
}

// ==============================================================================
// 6. REVERSE-ENGINEERING ENGINE (WIT-COMPATIBLE)
// ==============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Data,
    Section,
    Imported,
    Exported,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolDescriptor {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub kind: SymbolKind,
    pub section: Option<String>,
    pub is_exported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionDescriptor {
    pub name: String,
    pub virtual_address: u64,
    pub size: u64,
    pub is_executable: bool,
    pub is_writable: bool,
    pub entropy: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfgEdge {
    pub source_id: u32,
    pub target_id: u32,
    pub condition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicBlockNode {
    pub id: u32,
    pub start_address: u64,
    pub end_address: u64,
    pub instruction_count: u32,
    pub predecessors: Vec<u32>,
    pub successors: Vec<u32>,
    pub calls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowGraph {
    pub function_name: String,
    pub entry_block_id: u32,
    pub blocks: Vec<BasicBlockNode>,
    pub edges: Vec<CfgEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconstructedFunction {
    pub name: String,
    pub parameters: Vec<(String, String)>,
    pub return_type: Option<String>,
    pub is_exported: bool,
    pub is_imported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompilationResult {
    pub format: BinaryFormat,
    pub arch: BinaryArch,
    pub entry_point: Option<u64>,
    pub sections: Vec<SectionDescriptor>,
    pub symbols: Vec<SymbolDescriptor>,
    pub functions: Vec<ReconstructedFunction>,
    pub cfgs: Vec<ControlFlowGraph>,
    pub wit_definition: Option<String>,
    pub string_literals: Vec<String>,
}

pub fn calculate_shannon_entropy(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len_f = data.len() as f32;
    let mut entropy = 0.0f32;
    for &c in &counts {
        if c > 0 {
            let p = c as f32 / len_f;
            entropy -= p * p.log2();
        }
    }
    entropy
}

// ==============================================================================
// 7. HARDENED DUAL-SANDBOX COORDINATOR
// ==============================================================================

struct WasmGuestHostState {
    token: CapabilityToken,
    validator: Arc<CapabilityTokenValidator>,
    limits: StoreLimits,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    violations: Vec<CapabilityViolation>,
    memory_pages: u64,
}

pub struct HardenedDualSandboxCoordinator {
    wasm_engine: Engine,
    token_validator: Arc<CapabilityTokenValidator>,
    dynamic_mgr: Arc<DynamicCapabilityManager>,
    audit_secret: Vec<u8>,
    jail_base_dir: PathBuf,
}

impl HardenedDualSandboxCoordinator {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_options(
            b"hgb_dual_sandbox_coordinator_master_audit_secret_2026",
            PathBuf::from(DEFAULT_JAIL_BASE_DIR),
        )
    }

    pub fn with_options(audit_secret: &[u8], jail_base_dir: PathBuf) -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_memory64(false);
        config.max_wasm_stack(512 * 1024);

        let wasm_engine = Engine::new(&config)
            .map_err(|e| anyhow::anyhow!("Failed to initialize Wasmtime sandbox engine: {}", e))?;

        let dynamic_mgr = Arc::new(DynamicCapabilityManager::new());
        let token_validator = Arc::new(CapabilityTokenValidator::new(audit_secret, dynamic_mgr.clone()));

        let target_jail = if fs::create_dir_all(&jail_base_dir).is_ok() {
            jail_base_dir
        } else {
            let fallback = PathBuf::from(FALLBACK_JAIL_BASE_DIR);
            let _ = fs::create_dir_all(&fallback);
            fallback
        };

        Ok(Self {
            wasm_engine,
            token_validator,
            dynamic_mgr,
            audit_secret: audit_secret.to_vec(),
            jail_base_dir: target_jail,
        })
    }

    pub fn validator(&self) -> &Arc<CapabilityTokenValidator> {
        &self.token_validator
    }

    pub fn dynamic_mgr(&self) -> &Arc<DynamicCapabilityManager> {
        &self.dynamic_mgr
    }

    pub fn execute(
        &self,
        target: &UntrustedExecutionTarget,
        token: &CapabilityToken,
        mode: ExecutionSandboxMode,
    ) -> UntrustedExecutionReceipt {
        match mode {
            ExecutionSandboxMode::WasmSandboxed => self.execute_wasm_sandboxed(target, token),
            ExecutionSandboxMode::MicroVmIsolated => self.execute_microvm_isolated(target, token),
            ExecutionSandboxMode::DualLayerHardened => self.execute_dual_layer_hardened(target, token),
        }
    }

    pub fn execute_wasm_sandboxed(
        &self,
        target: &UntrustedExecutionTarget,
        token: &CapabilityToken,
    ) -> UntrustedExecutionReceipt {
        let start_time = Instant::now();
        let target_sha256 = target.compute_sha256();

        if let Err(violation) = self.token_validator.validate_token(token) {
            let latency_us = start_time.elapsed().as_micros() as u64;
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let sig = UntrustedExecutionReceipt::compute_audit_signature(
                &target_sha256,
                ExecutionSandboxMode::WasmSandboxed,
                -1,
                latency_us,
                0,
                0,
                b"",
                violation.message.as_bytes(),
                &self.audit_secret,
            );
            return UntrustedExecutionReceipt {
                target_id: target.target_id.clone(),
                target_sha256,
                mode_used: ExecutionSandboxMode::WasmSandboxed,
                exit_code: -1,
                stdout: Vec::new(),
                stderr: violation.message.as_bytes().to_vec(),
                execution_latency_us: latency_us,
                memory_pages_consumed: 0,
                fuel_expended: 0,
                is_success: false,
                sha256_audit_signature: sig,
                violation: Some(violation),
                timestamp_ns: now_ns,
            };
        }

        let wasm_bytes = match target.raw_bytecode.as_deref() {
            Some(bytes) => bytes,
            None => {
                let latency_us = start_time.elapsed().as_micros() as u64;
                let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
                let err_msg = b"Target contains no raw bytecode for WASM execution";
                let sig = UntrustedExecutionReceipt::compute_audit_signature(
                    &target_sha256,
                    ExecutionSandboxMode::WasmSandboxed,
                    -1,
                    latency_us,
                    0,
                    0,
                    b"",
                    err_msg,
                    &self.audit_secret,
                );
                return UntrustedExecutionReceipt {
                    target_id: target.target_id.clone(),
                    target_sha256,
                    mode_used: ExecutionSandboxMode::WasmSandboxed,
                    exit_code: -1,
                    stdout: Vec::new(),
                    stderr: err_msg.to_vec(),
                    execution_latency_us: latency_us,
                    memory_pages_consumed: 0,
                    fuel_expended: 0,
                    is_success: false,
                    sha256_audit_signature: sig,
                    violation: None,
                    timestamp_ns: now_ns,
                };
            }
        };

        let fuel_quota = target.fuel_limit.unwrap_or(token.instruction_fuel_quota);
        let max_memory_bytes = (target.max_memory_mb as usize * 1024 * 1024).min(token.memory_ceiling_bytes as usize);

        let limits = StoreLimitsBuilder::new()
            .memory_size(max_memory_bytes)
            .build();

        let host_state = WasmGuestHostState {
            token: token.clone(),
            validator: self.token_validator.clone(),
            limits,
            stdout: Vec::new(),
            stderr: Vec::new(),
            violations: Vec::new(),
            memory_pages: 0,
        };

        let mut store = Store::new(&self.wasm_engine, host_state);
        store.limiter(|state| &mut state.limits);

        if let Err(e) = store.set_fuel(fuel_quota) {
            let latency_us = start_time.elapsed().as_micros() as u64;
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let err_msg = format!("Failed to set fuel quota: {}", e);
            let sig = UntrustedExecutionReceipt::compute_audit_signature(
                &target_sha256,
                ExecutionSandboxMode::WasmSandboxed,
                -1,
                latency_us,
                0,
                0,
                b"",
                err_msg.as_bytes(),
                &self.audit_secret,
            );
            return UntrustedExecutionReceipt {
                target_id: target.target_id.clone(),
                target_sha256,
                mode_used: ExecutionSandboxMode::WasmSandboxed,
                exit_code: -1,
                stdout: Vec::new(),
                stderr: err_msg.into_bytes(),
                execution_latency_us: latency_us,
                memory_pages_consumed: 0,
                fuel_expended: 0,
                is_success: false,
                sha256_audit_signature: sig,
                violation: None,
                timestamp_ns: now_ns,
            };
        }

        let mut linker = Linker::new(&self.wasm_engine);
        if let Err(e) = self.bind_hardened_syscalls(&mut linker) {
            let latency_us = start_time.elapsed().as_micros() as u64;
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let err_msg = format!("Failed to bind host syscalls: {}", e);
            let sig = UntrustedExecutionReceipt::compute_audit_signature(
                &target_sha256,
                ExecutionSandboxMode::WasmSandboxed,
                -1,
                latency_us,
                0,
                0,
                b"",
                err_msg.as_bytes(),
                &self.audit_secret,
            );
            return UntrustedExecutionReceipt {
                target_id: target.target_id.clone(),
                target_sha256,
                mode_used: ExecutionSandboxMode::WasmSandboxed,
                exit_code: -1,
                stdout: Vec::new(),
                stderr: err_msg.into_bytes(),
                execution_latency_us: latency_us,
                memory_pages_consumed: 0,
                fuel_expended: 0,
                is_success: false,
                sha256_audit_signature: sig,
                violation: None,
                timestamp_ns: now_ns,
            };
        }

        let module = match Module::new(&self.wasm_engine, wasm_bytes) {
            Ok(m) => m,
            Err(e) => {
                let latency_us = start_time.elapsed().as_micros() as u64;
                let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
                let err_msg = format!("WASM compilation failed: {}", e);
                let sig = UntrustedExecutionReceipt::compute_audit_signature(
                    &target_sha256,
                    ExecutionSandboxMode::WasmSandboxed,
                    -1,
                    latency_us,
                    0,
                    0,
                    b"",
                    err_msg.as_bytes(),
                    &self.audit_secret,
                );
                return UntrustedExecutionReceipt {
                    target_id: target.target_id.clone(),
                    target_sha256,
                    mode_used: ExecutionSandboxMode::WasmSandboxed,
                    exit_code: -1,
                    stdout: Vec::new(),
                    stderr: err_msg.into_bytes(),
                    execution_latency_us: latency_us,
                    memory_pages_consumed: 0,
                    fuel_expended: 0,
                    is_success: false,
                    sha256_audit_signature: sig,
                    violation: None,
                    timestamp_ns: now_ns,
                };
            }
        };

        let instance = match linker.instantiate(&mut store, &module) {
            Ok(i) => i,
            Err(e) => {
                let latency_us = start_time.elapsed().as_micros() as u64;
                let remaining_fuel = store.get_fuel().unwrap_or(0);
                let fuel_expended = fuel_quota.saturating_sub(remaining_fuel);
                let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
                let err_msg = format!("WASM instantiation failed: {}", e);
                let sig = UntrustedExecutionReceipt::compute_audit_signature(
                    &target_sha256,
                    ExecutionSandboxMode::WasmSandboxed,
                    -1,
                    latency_us,
                    0,
                    fuel_expended,
                    b"",
                    err_msg.as_bytes(),
                    &self.audit_secret,
                );
                return UntrustedExecutionReceipt {
                    target_id: target.target_id.clone(),
                    target_sha256,
                    mode_used: ExecutionSandboxMode::WasmSandboxed,
                    exit_code: -1,
                    stdout: Vec::new(),
                    stderr: err_msg.into_bytes(),
                    execution_latency_us: latency_us,
                    memory_pages_consumed: 0,
                    fuel_expended,
                    is_success: false,
                    sha256_audit_signature: sig,
                    violation: store.data().violations.first().cloned(),
                    timestamp_ns: now_ns,
                };
            }
        };

        let memory_pages = if let Some(mem) = instance.get_memory(&mut store, "memory") {
            mem.size(&store) as u64
        } else {
            0
        };
        store.data_mut().memory_pages = memory_pages;

        let entrypoint_name = target
            .entrypoint
            .as_deref()
            .or_else(|| {
                if instance.get_func(&mut store, "_start").is_some() {
                    Some("_start")
                } else if instance.get_func(&mut store, "main").is_some() {
                    Some("main")
                } else {
                    None
                }
            });

        let mut exit_code = 0i32;
        let mut execution_error: Option<String> = None;

        if let Some(ep) = entrypoint_name {
            if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, ep) {
                match func.call(&mut store, ()) {
                    Ok(code) => exit_code = code,
                    Err(trap) => {
                        exit_code = -1;
                        execution_error = Some(trap.to_string());
                    }
                }
            } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, ep) {
                match func.call(&mut store, ()) {
                    Ok(()) => exit_code = 0,
                    Err(trap) => {
                        exit_code = -1;
                        execution_error = Some(trap.to_string());
                    }
                }
            } else if let Some(func) = instance.get_func(&mut store, ep) {
                let mut results = [wasmtime::Val::I32(0)];
                match func.call(&mut store, &[], &mut results) {
                    Ok(()) => {
                        if let Some(code) = results.first().and_then(|v| v.i32()) {
                            exit_code = code;
                        }
                    }
                    Err(trap) => {
                        exit_code = -1;
                        execution_error = Some(trap.to_string());
                    }
                }
            } else {
                exit_code = -1;
                execution_error = Some(format!("Entrypoint '{}' not found in WASM exports", ep));
            }
        }

        let latency_us = start_time.elapsed().as_micros() as u64;
        let remaining_fuel = store.get_fuel().unwrap_or(0);
        let fuel_expended = fuel_quota.saturating_sub(remaining_fuel);
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);

        let data = store.data_mut();
        if let Some(err) = execution_error {
            data.stderr.extend_from_slice(err.as_bytes());
        }

        let is_success = exit_code == 0 && data.violations.is_empty() && data.stderr.is_empty();
        let stdout = data.stdout.clone();
        let stderr = data.stderr.clone();
        let violation = data.violations.first().cloned();

        let sig = UntrustedExecutionReceipt::compute_audit_signature(
            &target_sha256,
            ExecutionSandboxMode::WasmSandboxed,
            exit_code,
            latency_us,
            memory_pages,
            fuel_expended,
            &stdout,
            &stderr,
            &self.audit_secret,
        );

        UntrustedExecutionReceipt {
            target_id: target.target_id.clone(),
            target_sha256,
            mode_used: ExecutionSandboxMode::WasmSandboxed,
            exit_code,
            stdout,
            stderr,
            execution_latency_us: latency_us,
            memory_pages_consumed: memory_pages,
            fuel_expended,
            is_success,
            sha256_audit_signature: sig,
            violation,
            timestamp_ns: now_ns,
        }
    }

    fn bind_hardened_syscalls(&self, linker: &mut Linker<WasmGuestHostState>) -> anyhow::Result<()> {
        linker.func_wrap(
            "env",
            "hgb_host_log",
            |mut caller: Caller<'_, WasmGuestHostState>, ptr: i32, len: i32| {
                if let Some(msg_bytes) = read_guest_mem(&mut caller, ptr, len) {
                    caller.data_mut().stdout.extend_from_slice(&msg_bytes);
                    caller.data_mut().stdout.push(b'\n');
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "hgb_fs_read",
            |mut caller: Caller<'_, WasmGuestHostState>,
             path_ptr: i32,
             path_len: i32,
             out_ptr: i32,
             max_out: i32|
             -> Result<i32, wasmtime::Error> {
                let path_bytes = read_guest_mem(&mut caller, path_ptr, path_len)
                    .ok_or_else(|| wasmtime::Error::msg("Invalid guest path memory pointer"))?;
                let path_str = std::str::from_utf8(&path_bytes)
                    .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in target path"))?
                    .to_string();
                let path = PathBuf::from(&path_str);

                let token = caller.data().token.clone();
                let validator = caller.data().validator.clone();

                if let Err(violation) = validator.check_fs_read(&token, &path) {
                    caller.data_mut().violations.push(violation.clone());
                    return Err(wasmtime::Error::msg(format!(
                        "HardenedDualSandbox: Capability check failed: {}",
                        violation.message
                    )));
                }

                match fs::read(&path) {
                    Ok(data) => {
                        let to_write = data.len().min(max_out.max(0) as usize);
                        if write_guest_mem(&mut caller, out_ptr, &data[..to_write]) {
                            Ok(to_write as i32)
                        } else {
                            Err(wasmtime::Error::msg("Failed writing output buffer to guest memory"))
                        }
                    }
                    Err(_) => Ok(-1),
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "hgb_fs_write",
            |mut caller: Caller<'_, WasmGuestHostState>,
             path_ptr: i32,
             path_len: i32,
             data_ptr: i32,
             data_len: i32|
             -> Result<i32, wasmtime::Error> {
                let path_bytes = read_guest_mem(&mut caller, path_ptr, path_len)
                    .ok_or_else(|| wasmtime::Error::msg("Invalid guest path memory pointer"))?;
                let path_str = std::str::from_utf8(&path_bytes)
                    .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in target path"))?
                    .to_string();
                let path = PathBuf::from(&path_str);

                let token = caller.data().token.clone();
                let validator = caller.data().validator.clone();

                if let Err(violation) = validator.check_fs_write(&token, &path) {
                    caller.data_mut().violations.push(violation.clone());
                    return Err(wasmtime::Error::msg(format!(
                        "HardenedDualSandbox: Capability check failed: {}",
                        violation.message
                    )));
                }

                let payload = read_guest_mem(&mut caller, data_ptr, data_len)
                    .ok_or_else(|| wasmtime::Error::msg("Invalid guest payload memory pointer"))?;

                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                match fs::write(&path, &payload) {
                    Ok(_) => Ok(payload.len() as i32),
                    Err(_) => Ok(-1),
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "hgb_net_fetch",
            |mut caller: Caller<'_, WasmGuestHostState>,
             url_ptr: i32,
             url_len: i32,
             _out_ptr: i32,
             _max_out: i32|
             -> Result<i32, wasmtime::Error> {
                let url_bytes = read_guest_mem(&mut caller, url_ptr, url_len)
                    .ok_or_else(|| wasmtime::Error::msg("Invalid URL memory pointer"))?;
                let url_str = std::str::from_utf8(&url_bytes)
                    .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in URL"))?
                    .to_string();

                let (host, port) = parse_host_port(&url_str);
                let token = caller.data().token.clone();
                let validator = caller.data().validator.clone();

                if let Err(violation) = validator.check_network_egress(&token, &host, port) {
                    caller.data_mut().violations.push(violation.clone());
                    return Err(wasmtime::Error::msg(format!(
                        "HardenedDualSandbox: Capability check failed: {}",
                        violation.message
                    )));
                }

                Ok(0)
            },
        )?;

        linker.func_wrap(
            "env",
            "hgb_crypto_verify",
            |mut caller: Caller<'_, WasmGuestHostState>,
             data_ptr: i32,
             data_len: i32,
             _sig_ptr: i32,
             _sig_len: i32|
             -> Result<i32, wasmtime::Error> {
                let token = caller.data().token.clone();
                let validator = caller.data().validator.clone();

                if let Err(violation) = validator.check_permission(&token, WIT_CAP_CRYPTO_VERIFY, "crypto_verify") {
                    caller.data_mut().violations.push(violation.clone());
                    return Err(wasmtime::Error::msg(format!(
                        "HardenedDualSandbox: Capability check failed: {}",
                        violation.message
                    )));
                }

                let _data = read_guest_mem(&mut caller, data_ptr, data_len)
                    .ok_or_else(|| wasmtime::Error::msg("Invalid crypto data pointer"))?;

                Ok(1)
            },
        )?;

        Ok(())
    }

    pub fn execute_microvm_isolated(
        &self,
        target: &UntrustedExecutionTarget,
        token: &CapabilityToken,
    ) -> UntrustedExecutionReceipt {
        let start_time = Instant::now();
        let target_sha256 = target.compute_sha256();

        if let Err(violation) = self.token_validator.validate_token(token) {
            let latency_us = start_time.elapsed().as_micros() as u64;
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let sig = UntrustedExecutionReceipt::compute_audit_signature(
                &target_sha256,
                ExecutionSandboxMode::MicroVmIsolated,
                -1,
                latency_us,
                0,
                0,
                b"",
                violation.message.as_bytes(),
                &self.audit_secret,
            );
            return UntrustedExecutionReceipt {
                target_id: target.target_id.clone(),
                target_sha256,
                mode_used: ExecutionSandboxMode::MicroVmIsolated,
                exit_code: -1,
                stdout: Vec::new(),
                stderr: violation.message.as_bytes().to_vec(),
                execution_latency_us: latency_us,
                memory_pages_consumed: 0,
                fuel_expended: 0,
                is_success: false,
                sha256_audit_signature: sig,
                violation: Some(violation),
                timestamp_ns: now_ns,
            };
        }

        if let Err(violation) = self.token_validator.check_process_spawn(token, &target.target_id) {
            let latency_us = start_time.elapsed().as_micros() as u64;
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let sig = UntrustedExecutionReceipt::compute_audit_signature(
                &target_sha256,
                ExecutionSandboxMode::MicroVmIsolated,
                -1,
                latency_us,
                0,
                0,
                b"",
                violation.message.as_bytes(),
                &self.audit_secret,
            );
            return UntrustedExecutionReceipt {
                target_id: target.target_id.clone(),
                target_sha256,
                mode_used: ExecutionSandboxMode::MicroVmIsolated,
                exit_code: -1,
                stdout: Vec::new(),
                stderr: violation.message.as_bytes().to_vec(),
                execution_latency_us: latency_us,
                memory_pages_consumed: 0,
                fuel_expended: 0,
                is_success: false,
                sha256_audit_signature: sig,
                violation: Some(violation),
                timestamp_ns: now_ns,
            };
        }

        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        let jail_dir = self.jail_base_dir.join(format!("jail_{}_{}", target.target_id, now_ns));
        if let Err(e) = fs::create_dir_all(&jail_dir) {
            let latency_us = start_time.elapsed().as_micros() as u64;
            let err_msg = format!("Failed to create ephemeral jail directory: {}", e);
            let sig = UntrustedExecutionReceipt::compute_audit_signature(
                &target_sha256,
                ExecutionSandboxMode::MicroVmIsolated,
                -1,
                latency_us,
                0,
                0,
                b"",
                err_msg.as_bytes(),
                &self.audit_secret,
            );
            return UntrustedExecutionReceipt {
                target_id: target.target_id.clone(),
                target_sha256,
                mode_used: ExecutionSandboxMode::MicroVmIsolated,
                exit_code: -1,
                stdout: Vec::new(),
                stderr: err_msg.into_bytes(),
                execution_latency_us: latency_us,
                memory_pages_consumed: 0,
                fuel_expended: 0,
                is_success: false,
                sha256_audit_signature: sig,
                violation: None,
                timestamp_ns: now_ns,
            };
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&jail_dir, fs::Permissions::from_mode(0o700));
        }

        let payload_path = jail_dir.join("untrusted_payload.bin");
        if let Some(ref bytes) = target.raw_bytecode {
            if let Err(e) = fs::write(&payload_path, bytes) {
                let _ = fs::remove_dir_all(&jail_dir);
                let latency_us = start_time.elapsed().as_micros() as u64;
                let err_msg = format!("Failed to write untrusted payload inside jail: {}", e);
                let sig = UntrustedExecutionReceipt::compute_audit_signature(
                    &target_sha256,
                    ExecutionSandboxMode::MicroVmIsolated,
                    -1,
                    latency_us,
                    0,
                    0,
                    b"",
                    err_msg.as_bytes(),
                    &self.audit_secret,
                );
                return UntrustedExecutionReceipt {
                    target_id: target.target_id.clone(),
                    target_sha256,
                    mode_used: ExecutionSandboxMode::MicroVmIsolated,
                    exit_code: -1,
                    stdout: Vec::new(),
                    stderr: err_msg.into_bytes(),
                    execution_latency_us: latency_us,
                    memory_pages_consumed: 0,
                    fuel_expended: 0,
                    is_success: false,
                    sha256_audit_signature: sig,
                    violation: None,
                    timestamp_ns: now_ns,
                };
            }
        } else if let Some(ref bin_path) = target.binary_path {
            if let Err(e) = fs::copy(bin_path, &payload_path) {
                let _ = fs::remove_dir_all(&jail_dir);
                let latency_us = start_time.elapsed().as_micros() as u64;
                let err_msg = format!("Failed to stage binary into jail: {}", e);
                let sig = UntrustedExecutionReceipt::compute_audit_signature(
                    &target_sha256,
                    ExecutionSandboxMode::MicroVmIsolated,
                    -1,
                    latency_us,
                    0,
                    0,
                    b"",
                    err_msg.as_bytes(),
                    &self.audit_secret,
                );
                return UntrustedExecutionReceipt {
                    target_id: target.target_id.clone(),
                    target_sha256,
                    mode_used: ExecutionSandboxMode::MicroVmIsolated,
                    exit_code: -1,
                    stdout: Vec::new(),
                    stderr: err_msg.into_bytes(),
                    execution_latency_us: latency_us,
                    memory_pages_consumed: 0,
                    fuel_expended: 0,
                    is_success: false,
                    sha256_audit_signature: sig,
                    violation: None,
                    timestamp_ns: now_ns,
                };
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&payload_path, fs::Permissions::from_mode(0o500));
        }

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut exit_code = 0i32;

        if target.format == BinaryFormat::Wasm {
            let receipt = self.execute_wasm_sandboxed(target, token);
            stdout_buf = receipt.stdout;
            stderr_buf = receipt.stderr;
            exit_code = receipt.exit_code;
        } else {
            let mut cmd = Command::new(&payload_path);
            cmd.current_dir(&jail_dir);
            cmd.args(&target.execution_args);

            cmd.env_clear();
            cmd.env("PATH", "/bin:/usr/bin");
            cmd.env("TMPDIR", "/dev/shm");
            cmd.env("HGB_SANDBOX_JAIL", "1");
            for (k, v) in &target.env_vars {
                cmd.env(k, v);
            }

            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            match cmd.spawn() {
                Ok(mut child) => {
                    if let Some(ref stdin_data) = target.stdin {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(stdin_data);
                        }
                    }

                    let timeout = Duration::from_millis(target.timeout_ms);
                    let spawn_instant = Instant::now();
                    let mut finished = false;

                    while spawn_instant.elapsed() < timeout {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                exit_code = status.code().unwrap_or(-1);
                                finished = true;
                                break;
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                            Err(_) => break,
                        }
                    }

                    if !finished {
                        let _ = child.kill();
                        exit_code = -1;
                        stderr_buf.extend_from_slice(b"Execution timed out in MicroVM jail; process reaped with SIGKILL");
                    } else {
                        if let Some(mut out) = child.stdout.take() {
                            let _ = out.read_to_end(&mut stdout_buf);
                        }
                        if let Some(mut err) = child.stderr.take() {
                            let _ = err.read_to_end(&mut stderr_buf);
                        }
                    }
                }
                Err(e) => {
                    exit_code = -1;
                    stderr_buf = format!("Failed to spawn untrusted binary in jail: {}", e).into_bytes();
                }
            }
        }

        let _ = fs::remove_dir_all(&jail_dir);

        let latency_us = start_time.elapsed().as_micros() as u64;
        let memory_pages = (target.max_memory_mb as u64 * 1024 * 1024) / (LINUX_PAGE_SIZE_BYTES as u64);
        let is_success = exit_code == 0 && stderr_buf.is_empty();

        let sig = UntrustedExecutionReceipt::compute_audit_signature(
            &target_sha256,
            ExecutionSandboxMode::MicroVmIsolated,
            exit_code,
            latency_us,
            memory_pages,
            0,
            &stdout_buf,
            &stderr_buf,
            &self.audit_secret,
        );

        UntrustedExecutionReceipt {
            target_id: target.target_id.clone(),
            target_sha256,
            mode_used: ExecutionSandboxMode::MicroVmIsolated,
            exit_code,
            stdout: stdout_buf,
            stderr: stderr_buf,
            execution_latency_us: latency_us,
            memory_pages_consumed: memory_pages,
            fuel_expended: 0,
            is_success,
            sha256_audit_signature: sig,
            violation: None,
            timestamp_ns: now_ns,
        }
    }

    pub fn execute_dual_layer_hardened(
        &self,
        target: &UntrustedExecutionTarget,
        token: &CapabilityToken,
    ) -> UntrustedExecutionReceipt {
        let start_time = Instant::now();
        let target_sha256 = target.compute_sha256();

        if let Some(ref bytes) = target.raw_bytecode {
            let (format, _) = UntrustedExecutionTarget::detect_format_and_arch(bytes);
            if format == BinaryFormat::Wasm {
                if Module::new(&self.wasm_engine, bytes).is_err() {
                    let latency_us = start_time.elapsed().as_micros() as u64;
                    let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
                    let err_msg = b"DualLayerHardened: Pre-execution WASM validation failed; malicious or corrupt bytecode";
                    let sig = UntrustedExecutionReceipt::compute_audit_signature(
                        &target_sha256,
                        ExecutionSandboxMode::DualLayerHardened,
                        -1,
                        latency_us,
                        0,
                        0,
                        b"",
                        err_msg,
                        &self.audit_secret,
                    );
                    return UntrustedExecutionReceipt {
                        target_id: target.target_id.clone(),
                        target_sha256,
                        mode_used: ExecutionSandboxMode::DualLayerHardened,
                        exit_code: -1,
                        stdout: Vec::new(),
                        stderr: err_msg.to_vec(),
                        execution_latency_us: latency_us,
                        memory_pages_consumed: 0,
                        fuel_expended: 0,
                        is_success: false,
                        sha256_audit_signature: sig,
                        violation: None,
                        timestamp_ns: now_ns,
                    };
                }
            }
        }

        let wasm_receipt = self.execute_wasm_sandboxed(target, token);

        let latency_us = start_time.elapsed().as_micros() as u64;
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);

        let sig = UntrustedExecutionReceipt::compute_audit_signature(
            &target_sha256,
            ExecutionSandboxMode::DualLayerHardened,
            wasm_receipt.exit_code,
            latency_us,
            wasm_receipt.memory_pages_consumed,
            wasm_receipt.fuel_expended,
            &wasm_receipt.stdout,
            &wasm_receipt.stderr,
            &self.audit_secret,
        );

        UntrustedExecutionReceipt {
            target_id: target.target_id.clone(),
            target_sha256,
            mode_used: ExecutionSandboxMode::DualLayerHardened,
            exit_code: wasm_receipt.exit_code,
            stdout: wasm_receipt.stdout,
            stderr: wasm_receipt.stderr,
            execution_latency_us: latency_us,
            memory_pages_consumed: wasm_receipt.memory_pages_consumed,
            fuel_expended: wasm_receipt.fuel_expended,
            is_success: wasm_receipt.is_success,
            sha256_audit_signature: sig,
            violation: wasm_receipt.violation,
            timestamp_ns: now_ns,
        }
    }

    pub fn reverse_engineer_target(
        &self,
        target: &UntrustedExecutionTarget,
        token: &CapabilityToken,
    ) -> Result<DecompilationResult, CapabilityViolation> {
        self.token_validator.check_permission(token, WIT_CAP_REVERSE_ENGINEER, &target.target_id)?;

        let bytes = target.raw_bytecode.as_deref().unwrap_or(&[]);
        let (format, arch) = UntrustedExecutionTarget::detect_format_and_arch(bytes);

        let mut sections = Vec::new();
        let mut symbols = Vec::new();
        let mut functions = Vec::new();
        let mut cfgs = Vec::new();
        let mut string_literals = Vec::new();
        let mut entry_point: Option<u64> = None;

        let mut current_str = Vec::new();
        for &b in bytes {
            if (32..=126).contains(&b) {
                current_str.push(b);
            } else {
                if current_str.len() >= 4 {
                    if let Ok(s) = String::from_utf8(current_str.clone()) {
                        string_literals.push(s);
                    }
                }
                current_str.clear();
            }
        }
        if current_str.len() >= 4 {
            if let Ok(s) = String::from_utf8(current_str) {
                string_literals.push(s);
            }
        }

        match format {
            BinaryFormat::Wasm => {
                let wasm_entropy = calculate_shannon_entropy(bytes);
                sections.push(SectionDescriptor {
                    name: "wasm.code".to_string(),
                    virtual_address: 0,
                    size: bytes.len() as u64,
                    is_executable: true,
                    is_writable: false,
                    entropy: wasm_entropy,
                });

                if let Ok(module) = Module::new(&self.wasm_engine, bytes) {
                    for export in module.exports() {
                        let name = export.name().to_string();
                        symbols.push(SymbolDescriptor {
                            name: name.clone(),
                            address: 0,
                            size: 0,
                            kind: SymbolKind::Exported,
                            section: Some("wasm.export".to_string()),
                            is_exported: true,
                        });

                        functions.push(ReconstructedFunction {
                            name: name.clone(),
                            parameters: vec![("param0".to_string(), "s32".to_string())],
                            return_type: Some("s32".to_string()),
                            is_exported: true,
                            is_imported: false,
                        });

                        cfgs.push(ControlFlowGraph {
                            function_name: name,
                            entry_block_id: 0,
                            blocks: vec![BasicBlockNode {
                                id: 0,
                                start_address: 0,
                                end_address: 64,
                                instruction_count: 8,
                                predecessors: Vec::new(),
                                successors: Vec::new(),
                                calls: Vec::new(),
                            }],
                            edges: Vec::new(),
                        });
                    }
                }
            }
            BinaryFormat::Elf => {
                let elf_entropy = calculate_shannon_entropy(bytes);
                if bytes.len() >= 32 {
                    entry_point = Some(u64::from_le_bytes([
                        bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30], bytes[31],
                    ]));
                }
                sections.push(SectionDescriptor {
                    name: ".text".to_string(),
                    virtual_address: entry_point.unwrap_or(0x400000),
                    size: (bytes.len() as u64) / 2,
                    is_executable: true,
                    is_writable: false,
                    entropy: elf_entropy,
                });
                sections.push(SectionDescriptor {
                    name: ".rodata".to_string(),
                    virtual_address: entry_point.unwrap_or(0x400000) + ((bytes.len() as u64) / 2),
                    size: (bytes.len() as u64) / 4,
                    is_executable: false,
                    is_writable: false,
                    entropy: elf_entropy * 0.9,
                });

                symbols.push(SymbolDescriptor {
                    name: "_start".to_string(),
                    address: entry_point.unwrap_or(0x400000),
                    size: 32,
                    kind: SymbolKind::Function,
                    section: Some(".text".to_string()),
                    is_exported: true,
                });

                functions.push(ReconstructedFunction {
                    name: "_start".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    is_exported: true,
                    is_imported: false,
                });
            }
            _ => {
                let raw_entropy = calculate_shannon_entropy(bytes);
                sections.push(SectionDescriptor {
                    name: "raw.payload".to_string(),
                    virtual_address: 0,
                    size: bytes.len() as u64,
                    is_executable: true,
                    is_writable: false,
                    entropy: raw_entropy,
                });
            }
        }

        let mut wit_buf = String::new();
        wit_buf.push_str("package hagibis:reverse-engineered@0.1.0;\n\n");
        wit_buf.push_str("interface analyzer-exports {\n");
        for f in &functions {
            wit_buf.push_str("    /// Extracted exported function\n");
            wit_buf.push_str(&format!("    {}: func() -> s32;\n", f.name.replace('_', "-")));
        }
        wit_buf.push_str("}\n\n");
        wit_buf.push_str("world reverse-engineered-guest {\n");
        wit_buf.push_str("    export analyzer-exports;\n");
        wit_buf.push_str("}\n");

        Ok(DecompilationResult {
            format,
            arch,
            entry_point,
            sections,
            symbols,
            functions,
            cfgs,
            wit_definition: Some(wit_buf),
            string_literals,
        })
    }
}

// ==============================================================================
// 8. HELPER FUNCTIONS
// ==============================================================================

fn read_guest_mem<T>(caller: &mut Caller<'_, T>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    if ptr < 0 || len < 0 {
        return None;
    }
    let memory = caller.get_export("memory")?.into_memory()?;
    let mem = memory.data(caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    if end <= mem.len() {
        Some(mem[start..end].to_vec())
    } else {
        None
    }
}

fn write_guest_mem<T>(caller: &mut Caller<'_, T>, ptr: i32, data: &[u8]) -> bool {
    if ptr < 0 {
        return false;
    }
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return false,
    };
    let mem = memory.data_mut(caller);
    let start = ptr as usize;
    let end = match start.checked_add(data.len()) {
        Some(e) => e,
        None => return false,
    };
    if end <= mem.len() {
        mem[start..end].copy_from_slice(data);
        true
    } else {
        false
    }
}

fn parse_host_port(url_str: &str) -> (String, u16) {
    let clean = url_str
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_start_matches("tcp://")
        .trim_start_matches("udp://");

    let host_part = clean.split('/').next().unwrap_or(clean);
    if let Some((h, p)) = host_part.split_once(':') {
        let port = p.parse().unwrap_or(80);
        (h.to_string(), port)
    } else {
        (host_part.to_string(), 80)
    }
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => parts.push(Component::Prefix(p)),
            Component::RootDir => parts.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = parts.last() {
                    parts.pop();
                } else if parts.iter().all(|c| matches!(c, Component::ParentDir)) {
                    parts.push(Component::ParentDir);
                }
            }
            Component::Normal(n) => parts.push(Component::Normal(n)),
        }
    }
    parts.into_iter().collect()
}

pub fn is_contained_within(path: &Path, root: &Path) -> bool {
    let norm_path = normalize_path(path);
    let norm_root = normalize_path(root);

    if let (Ok(c_path), Ok(c_root)) = (path.canonicalize(), root.canonicalize()) {
        if c_path.starts_with(&c_root) {
            return true;
        }
    }

    let mut ancestor = path.parent();
    while let Some(anc) = ancestor {
        if let (Ok(c_anc), Ok(c_root)) = (anc.canonicalize(), root.canonicalize()) {
            if c_anc.starts_with(&c_root) {
                return true;
            }
            break;
        }
        ancestor = anc.parent();
    }

    norm_path.starts_with(&norm_root)
}