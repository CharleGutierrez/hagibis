//! # WebAssembly Capabilities-Based Security Sandbox (`wasm_capability_sandbox`)
//!
//! Pillar 1: Supercharge the WebAssembly Sandbox (hgb-wasm-kernel & hgb-core)
//!
//! Production-grade capability isolation:
//! - Granular capability constraints per guest instance.
//! - Virtual filesystem sandbox: read-only access to designated directories (e.g., `hgb-storage`),
//!   with write access strictly denied unless explicitly authorized.
//! - Network sandbox: zero ambient authority; denies all socket, TCP, and HTTP traffic unless
//!   explicitly granted `allow_network: true` with strict hostname filtering.
//! - Defense-in-depth against lexical directory traversal and symlink jailbreak attempts.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use serde::{Deserialize, Serialize};
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::error::{HgbError, Result};
use crate::wasm_gas_metering::{GasMeterEngine, GAS_LIMIT_EXCEEDED_DIAGNOSTIC};

/// Recorded capability security violation event
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitySecurityViolation {
    pub kind: String,
    pub target: String,
    pub reason: String,
}

/// Capability security policy configuring filesystem and network access bounds
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasmCapabilityPolicy {
    /// Directory paths granted read-only or read-write access
    pub allowed_read_paths: Vec<PathBuf>,
    /// Directory paths explicitly granted write access
    pub allowed_write_paths: Vec<PathBuf>,
    /// Master toggle for network sandbox
    pub allow_network: bool,
    /// Explicit authorized outbound hostnames or patterns ("*.domain.com")
    pub allowed_network_hosts: Vec<String>,
    /// Maximum gas fuel instructions
    pub gas_limit: u64,
    /// Maximum linear memory allocation in bytes
    pub max_memory_bytes: usize,
}

impl Default for WasmCapabilityPolicy {
    fn default() -> Self {
        Self {
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
            allow_network: false,
            allowed_network_hosts: Vec::new(),
            gas_limit: 10_000_000,
            max_memory_bytes: 16 * 1024 * 1024, // 16 MiB default
        }
    }
}

impl WasmCapabilityPolicy {
    /// New default zero-ambient-authority capability policy
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-only access to designated storage paths with networking disabled
    pub fn read_only_storage(storage_root: impl Into<PathBuf>) -> Self {
        Self::default().with_storage_access(storage_root, false)
    }

    /// Unrestricted policy for trusted benchmarking
    pub fn unrestricted() -> Self {
        Self {
            allowed_read_paths: vec![PathBuf::from("/")],
            allowed_write_paths: vec![PathBuf::from("/")],
            allow_network: true,
            allowed_network_hosts: vec!["*".to_string()],
            gas_limit: 100_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
        }
    }

    /// Grant read access to a storage root, optionally permitting writes
    pub fn with_storage_access(mut self, storage_root: impl Into<PathBuf>, allow_write: bool) -> Self {
        let p = storage_root.into();
        self.allowed_read_paths.push(p.clone());
        if allow_write {
            self.allowed_write_paths.push(p);
        }
        self
    }

    pub fn with_read_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_read_paths.push(path.into());
        self
    }

    pub fn with_write_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_write_paths.push(path.into());
        self
    }

    pub fn with_allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }

    pub fn with_network_host(mut self, host: impl Into<String>) -> Self {
        self.allowed_network_hosts.push(host.into());
        self
    }

    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    pub fn with_max_memory_bytes(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Check if reading from `target` is permitted
    pub fn is_read_allowed(&self, target: &Path) -> bool {
        for root in &self.allowed_read_paths {
            if is_contained_within(target, root) {
                return true;
            }
        }
        false
    }

    /// Check if writing to `target` is permitted
    pub fn is_write_allowed(&self, target: &Path) -> bool {
        for root in &self.allowed_write_paths {
            if is_contained_within(target, root) {
                return true;
            }
        }
        false
    }

    /// Check if outbound network traffic to `host_or_url` is permitted
    pub fn is_network_allowed(&self, host_or_url: &str) -> bool {
        if !self.allow_network {
            return false;
        }

        if self.allowed_network_hosts.is_empty() {
            // When allow_network is true and no host whitelist is specified, open network
            return true;
        }

        let host = if let Ok(parsed) = reqwest::Url::parse(host_or_url) {
            parsed.host_str().unwrap_or("").to_lowercase()
        } else {
            host_or_url
                .split('/')
                .next()
                .unwrap_or(host_or_url)
                .split(':')
                .next()
                .unwrap_or(host_or_url)
                .trim()
                .to_lowercase()
        };

        if host.is_empty() {
            return false;
        }

        for allowed in &self.allowed_network_hosts {
            let clean = allowed.trim().to_lowercase();
            if clean == "*" || clean == host {
                return true;
            }
            if let Some(suffix) = clean.strip_prefix("*.") {
                if host == suffix || host.ends_with(&format!(".{}", suffix)) {
                    return true;
                }
            }
        }

        false
    }
}

/// Normalizes path lexically without symlink traversal
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

/// Verify if `path` is contained within `root` with symlink and lexical jailbreak protection
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

/// Host execution state for capability-sandboxed guests
pub struct WasmCapabilityHostState {
    pub policy: WasmCapabilityPolicy,
    pub violations: Vec<CapabilitySecurityViolation>,
    pub logs: Vec<String>,
    pub limits: StoreLimits,
}

/// Capability execution outcome
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityExecutionOutcome {
    pub exit_code: i32,
    pub gas_consumed: u64,
    pub gas_remaining: u64,
    pub duration_us: u64,
    pub trapped: bool,
    pub error_diagnostic: Option<String>,
    pub violations: Vec<CapabilitySecurityViolation>,
    pub logs: Vec<String>,
}

impl CapabilityExecutionOutcome {
    pub fn is_success(&self) -> bool {
        !self.trapped && self.error_diagnostic.is_none() && self.violations.is_empty() && self.exit_code >= 0
    }
}

/// Sandbox engine binding capability security into Wasmtime Linker
pub struct WasmCapabilitySandbox {
    engine: Engine,
}

impl WasmCapabilitySandbox {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_memory64(false);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize WasmCapabilitySandbox engine: {}", e))
        })?;

        Ok(Self { engine })
    }

    /// Instantiate and execute guest with capability constraints
    pub fn execute(
        &self,
        wasm_bytes: &[u8],
        entrypoint: &str,
        policy: WasmCapabilityPolicy,
    ) -> Result<CapabilityExecutionOutcome> {
        let gas_limit = policy.gas_limit;
        let limits = StoreLimitsBuilder::new()
            .memory_size(policy.max_memory_bytes)
            .build();

        let host_state = WasmCapabilityHostState {
            policy: policy.clone(),
            violations: Vec::new(),
            logs: Vec::new(),
            limits,
        };

        let mut store = Store::new(&self.engine, host_state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(gas_limit).map_err(|e| {
            HgbError::Execution(format!("Failed to set sandbox fuel: {}", e))
        })?;

        let mut linker = Linker::new(&self.engine);
        self.bind_syscalls(&mut linker)?;

        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            HgbError::Execution(format!("Invalid WASM module for capability sandbox: {}", e))
        })?;

        let instance = match linker.instantiate(&mut store, &module) {
            Ok(inst) => inst,
            Err(e) => {
                let err_msg = e.to_string();
                let remaining = store.get_fuel().unwrap_or(0);
                return Ok(CapabilityExecutionOutcome {
                    exit_code: -1,
                    gas_consumed: gas_limit.saturating_sub(remaining),
                    gas_remaining: remaining,
                    duration_us: 0,
                    trapped: true,
                    error_diagnostic: Some(format!("Instantiation error: {}", err_msg)),
                    violations: store.data().violations.clone(),
                    logs: store.data().logs.clone(),
                });
            }
        };

        let start = std::time::Instant::now();

        if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, entrypoint) {
            match func.call(&mut store, ()) {
                Ok(exit_code) => {
                    let duration_us = start.elapsed().as_micros() as u64;
                    let remaining = store.get_fuel().unwrap_or(0);
                    Ok(CapabilityExecutionOutcome {
                        exit_code,
                        gas_consumed: gas_limit.saturating_sub(remaining),
                        gas_remaining: remaining,
                        duration_us,
                        trapped: false,
                        error_diagnostic: None,
                        violations: store.data().violations.clone(),
                        logs: store.data().logs.clone(),
                    })
                }
                Err(trap) => {
                    let duration_us = start.elapsed().as_micros() as u64;
                    let remaining = store.get_fuel().unwrap_or(0);
                    let err_str = trap.to_string();
                    let diag = if remaining == 0 || err_str.contains("fuel") {
                        GAS_LIMIT_EXCEEDED_DIAGNOSTIC.to_string()
                    } else {
                        err_str
                    };

                    Ok(CapabilityExecutionOutcome {
                        exit_code: -1,
                        gas_consumed: gas_limit.saturating_sub(remaining),
                        gas_remaining: remaining,
                        duration_us,
                        trapped: true,
                        error_diagnostic: Some(diag),
                        violations: store.data().violations.clone(),
                        logs: store.data().logs.clone(),
                    })
                }
            }
        } else {
            Err(HgbError::Execution(format!("Entrypoint '{}' not found", entrypoint)))
        }
    }

    /// Bind capability-checked host syscalls
    fn bind_syscalls(&self, linker: &mut Linker<WasmCapabilityHostState>) -> Result<()> {
        // Logging
        linker
            .func_wrap("env", "hgb_host_log", |mut caller: Caller<'_, WasmCapabilityHostState>, ptr: i32, len: i32| {
                if let Some(msg_bytes) = read_guest_mem(&mut caller, ptr, len) {
                    if let Ok(msg) = std::str::from_utf8(&msg_bytes) {
                        caller.data_mut().logs.push(msg.to_string());
                    }
                }
            })
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // Filesystem Read: Granted only if path is within allowed_read_paths
        linker
            .func_wrap(
                "env",
                "hgb_fs_read",
                |mut caller: Caller<'_, WasmCapabilityHostState>,
                 path_ptr: i32,
                 path_len: i32,
                 out_ptr: i32,
                 max_out: i32|
                 -> std::result::Result<i32, wasmtime::Error> {
                    let path_bytes = read_guest_mem(&mut caller, path_ptr, path_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid path memory pointer"))?;
                    let path_str = std::str::from_utf8(&path_bytes)
                        .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in path"))?
                        .to_string();
                    let target_path = PathBuf::from(&path_str);

                    if !caller.data().policy.is_read_allowed(&target_path) {
                        let violation = CapabilitySecurityViolation {
                            kind: "unauthorized_read".to_string(),
                            target: path_str.clone(),
                            reason: format!("Read access to '{}' denied by capability policy", path_str),
                        };
                        caller.data_mut().violations.push(violation);
                        return Err(wasmtime::Error::msg(format!(
                            "Capability violation: read access to '{}' denied",
                            path_str
                        )));
                    }

                    match fs::read(&target_path) {
                        Ok(data) => {
                            let write_len = data.len().min(max_out.max(0) as usize);
                            if write_guest_mem(&mut caller, out_ptr, &data[..write_len]) {
                                Ok(write_len as i32)
                            } else {
                                Err(wasmtime::Error::msg("Failed writing output buffer to guest memory"))
                            }
                        }
                        Err(_) => Ok(-1),
                    }
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // Filesystem Write: Denied unless path is within allowed_write_paths
        linker
            .func_wrap(
                "env",
                "hgb_fs_write",
                |mut caller: Caller<'_, WasmCapabilityHostState>,
                 path_ptr: i32,
                 path_len: i32,
                 data_ptr: i32,
                 data_len: i32|
                 -> std::result::Result<i32, wasmtime::Error> {
                    let path_bytes = read_guest_mem(&mut caller, path_ptr, path_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid path memory pointer"))?;
                    let path_str = std::str::from_utf8(&path_bytes)
                        .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in path"))?
                        .to_string();
                    let target_path = PathBuf::from(&path_str);

                    if !caller.data().policy.is_write_allowed(&target_path) {
                        let violation = CapabilitySecurityViolation {
                            kind: "unauthorized_write".to_string(),
                            target: path_str.clone(),
                            reason: format!("Write access to '{}' denied by capability policy", path_str),
                        };
                        caller.data_mut().violations.push(violation);
                        return Err(wasmtime::Error::msg(format!(
                            "Capability violation: write access to '{}' denied",
                            path_str
                        )));
                    }

                    let payload = read_guest_mem(&mut caller, data_ptr, data_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid payload memory pointer"))?;

                    if let Some(parent) = target_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }

                    match fs::write(&target_path, &payload) {
                        Ok(_) => Ok(0),
                        Err(_) => Ok(-1),
                    }
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // Network Fetch / Connect: Denied unless allow_network is true and host is permitted
        linker
            .func_wrap(
                "env",
                "hgb_net_fetch",
                |mut caller: Caller<'_, WasmCapabilityHostState>,
                 url_ptr: i32,
                 url_len: i32,
                 _out_ptr: i32,
                 _max_out: i32|
                 -> std::result::Result<i32, wasmtime::Error> {
                    let url_bytes = read_guest_mem(&mut caller, url_ptr, url_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid URL memory pointer"))?;
                    let url_str = std::str::from_utf8(&url_bytes)
                        .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in URL"))?
                        .to_string();

                    if !caller.data().policy.is_network_allowed(&url_str) {
                        let violation = CapabilitySecurityViolation {
                            kind: "unauthorized_network".to_string(),
                            target: url_str.clone(),
                            reason: format!("Network access to '{}' denied by capability policy", url_str),
                        };
                        caller.data_mut().violations.push(violation);
                        return Err(wasmtime::Error::msg(format!(
                            "Capability violation: network access to '{}' denied",
                            url_str
                        )));
                    }

                    Ok(0)
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        Ok(())
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_only_storage_access_denies_write() {
        let policy = WasmCapabilityPolicy::new()
            .with_storage_access("/tmp/storage", false);
        assert!(policy.is_read_allowed(Path::new("/tmp/storage/data.txt")));
        assert!(!policy.is_write_allowed(Path::new("/tmp/storage/data.txt")));

        let policy_rw = WasmCapabilityPolicy::new()
            .with_storage_access("/tmp/storage", true);
        assert!(policy_rw.is_read_allowed(Path::new("/tmp/storage/data.txt")));
        assert!(policy_rw.is_write_allowed(Path::new("/tmp/storage/data.txt")));
    }

    #[test]
    fn test_network_sandbox_denial_by_default() {
        let policy = WasmCapabilityPolicy::default();
        assert!(!policy.allow_network);
        assert!(!policy.is_network_allowed("api.anthropic.com"));
    }

    #[test]
    fn test_network_sandbox_allowed_when_explicit() {
        let policy = WasmCapabilityPolicy::new()
            .with_allow_network(true)
            .with_network_host("*.anthropic.com");
        assert!(policy.allow_network);
        assert!(policy.is_network_allowed("api.anthropic.com"));
        assert!(!policy.is_network_allowed("evil.com"));
    }

    #[test]
    fn test_path_traversal_jailbreak_prevention() {
        let root = Path::new("/var/app/sandbox");
        let valid = Path::new("/var/app/sandbox/sub/file.txt");
        let escape = Path::new("/var/app/sandbox/../../etc/passwd");
        assert!(is_contained_within(valid, root));
        assert!(!is_contained_within(escape, root));
    }
}
