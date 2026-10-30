//! # WebAssembly Component Model & Capability Security Sandbox (Pillar 4)
//!
//! Production-grade sandboxed runtime for untrusted AI-generated WebAssembly components
//! and third-party plugins with:
//! - Capability-based security: strictly granular allowed filesystem read/write paths and network hosts
//! - Fuel metering & deterministic CPU timeout isolation
//! - Linear memory bounds virtualization (configurable `max_memory_bytes`)
//! - Hot reloading: scans `.hgb/plugins/wasm/`, validates modules, and registers them dynamically
//!   into the runtime tool catalog without restarting the daemon
//! - Seamless `HgbTool` trait adapter for the autonomous agent loop

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use wasmtime::*;

use crate::error::{HgbError, Result};
use crate::traits::HgbTool;
use crate::wasm_plugin_kernel::WasmPluginManifest;

/// Granular capability security permissions granted to a WASM plugin instance
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCapabilities {
    /// Absolute or relative paths the guest is allowed to read from
    pub allowed_read_paths: Vec<PathBuf>,
    /// Absolute or relative paths the guest is allowed to write to
    pub allowed_write_paths: Vec<PathBuf>,
    /// Allowed outbound network hostnames or wildcards (e.g. "api.github.com", "*.example.com")
    pub allowed_network_hosts: Vec<String>,
    /// Maximum linear memory allocation in bytes
    pub max_memory_bytes: usize,
    /// Maximum CPU fuel (instruction count) before trapping
    pub fuel_limit: u64,
}

impl Default for PluginCapabilities {
    fn default() -> Self {
        Self {
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
            allowed_network_hosts: Vec::new(),
            max_memory_bytes: 16 * 1024 * 1024, // 16 MiB default sandbox memory
            fuel_limit: 10_000_000,             // 10M instructions fuel
        }
    }
}

impl PluginCapabilities {
    /// Creates a new default, zero-ambient-authority capability set
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an unrestricted capability set (for trusted internal tools or benchmarks)
    pub fn unrestricted() -> Self {
        Self {
            allowed_read_paths: vec![PathBuf::from("/")],
            allowed_write_paths: vec![PathBuf::from("/")],
            allowed_network_hosts: vec!["*".to_string()],
            max_memory_bytes: 64 * 1024 * 1024,
            fuel_limit: 100_000_000,
        }
    }

    /// Grant read permission to a specific directory or file path
    pub fn with_read_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.allowed_read_paths.push(path.into());
        self
    }

    /// Grant write permission to a specific directory or file path
    pub fn with_write_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.allowed_write_paths.push(path.into());
        self
    }

    /// Grant network access to a specific hostname or domain pattern
    pub fn with_network_host<S: Into<String>>(mut self, host: S) -> Self {
        self.allowed_network_hosts.push(host.into());
        self
    }

    /// Set explicit instruction fuel budget
    pub fn with_fuel_limit(mut self, fuel_limit: u64) -> Self {
        self.fuel_limit = fuel_limit;
        self
    }

    /// Set maximum linear memory allocation
    pub fn with_max_memory_bytes(mut self, max_memory_bytes: usize) -> Self {
        self.max_memory_bytes = max_memory_bytes;
        self
    }

    /// Verify whether reading from the specified target path is allowed
    pub fn is_read_allowed(&self, path: &Path) -> bool {
        for root in &self.allowed_read_paths {
            if is_path_within_root(path, root) {
                return true;
            }
        }
        false
    }

    /// Verify whether writing to the specified target path is allowed
    pub fn is_write_allowed(&self, path: &Path) -> bool {
        for root in &self.allowed_write_paths {
            if is_path_within_root(path, root) {
                return true;
            }
        }
        false
    }

    /// Verify whether contacting the specified network host is allowed
    pub fn is_network_host_allowed(&self, host_or_url: &str) -> bool {
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
            let allowed_clean = allowed.trim().to_lowercase();
            if allowed_clean == "*" {
                return true;
            }
            if allowed_clean == host {
                return true;
            }
            if let Some(suffix) = allowed_clean.strip_prefix("*.") {
                if host == suffix || host.ends_with(&format!(".{}", suffix)) {
                    return true;
                }
            }
        }
        false
    }
}

/// Normalizes a path lexically without resolving symlinks or accessing the filesystem
pub fn normalize_path_lexical(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => components.push(Component::Prefix(p)),
            Component::RootDir => components.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = components.last() {
                    components.pop();
                } else if components.iter().all(|c| matches!(c, Component::ParentDir)) {
                    components.push(Component::ParentDir);
                }
            }
            Component::Normal(n) => components.push(Component::Normal(n)),
        }
    }
    components.into_iter().collect()
}

/// Robust capability check: verifies if `path` is contained within `root`
pub fn is_path_within_root(path: &Path, root: &Path) -> bool {
    let clean_path = normalize_path_lexical(path);
    let clean_root = normalize_path_lexical(root);

    // 1. If both exist on disk, canonicalize them to resolve symlinks and prevent jailbreaks
    if let (Ok(canon_path), Ok(canon_root)) = (path.canonicalize(), root.canonicalize()) {
        if canon_path.starts_with(&canon_root) {
            return true;
        }
    }

    // 2. If target does not exist yet (e.g. for write), verify existing ancestor canonicalization
    let mut current_ancestor = path.parent();
    while let Some(ancestor) = current_ancestor {
        if let (Ok(canon_ancestor), Ok(canon_root)) = (ancestor.canonicalize(), root.canonicalize()) {
            if canon_ancestor.starts_with(&canon_root) {
                return true;
            }
            break;
        }
        current_ancestor = ancestor.parent();
    }

    // 3. Fallback to lexical normalization check
    clean_path.starts_with(&clean_root)
}

/// Recorded capability violation event during guest execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasmCapabilityViolation {
    pub kind: String,
    pub target: String,
    pub message: String,
}

/// Host execution state exposed to sandboxed WASM guest
pub struct SandboxedWasmHostState {
    pub capabilities: PluginCapabilities,
    pub logs: Vec<String>,
    pub kv_store: HashMap<String, Vec<u8>>,
    pub violations: Vec<WasmCapabilityViolation>,
    pub limits: StoreLimits,
}

/// Comprehensive outcome of a sandboxed WASM execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxedExecutionResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub output: serde_json::Value,
    pub fuel_consumed: u64,
    pub logs: Vec<String>,
    pub trapped: bool,
    pub trap_reason: Option<String>,
}

/// Instance of a compiled, capability-sandboxed WebAssembly plugin
pub struct SandboxedWasmPlugin {
    pub manifest: WasmPluginManifest,
    pub capabilities: PluginCapabilities,
    pub wasm_path: Option<PathBuf>,
    pub binary_hash: String,
    engine: Engine,
    module: Module,
}

impl SandboxedWasmPlugin {
    /// Load and validate a WASM module from disk with custom capabilities
    pub fn load_from_file<P: AsRef<Path>>(path: P, capabilities: PluginCapabilities) -> Result<Self> {
        let p = path.as_ref().to_path_buf();
        let bytes = fs::read(&p).map_err(HgbError::Io)?;
        Self::load_from_bytes(&bytes, Some(p), capabilities)
    }

    /// Load, compile, and validate a WASM module from in-memory bytes
    pub fn load_from_bytes(
        bytes: &[u8],
        path: Option<PathBuf>,
        capabilities: PluginCapabilities,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize Wasmtime engine: {}", e))
        })?;

        let module = Module::new(&engine, bytes).map_err(|e| {
            HgbError::Execution(format!("Invalid WASM component/module: {}", e))
        })?;

        let binary_hash = blake3::hash(bytes).to_hex().to_string();

        let manifest = Self::extract_or_default_manifest(&module, path.as_deref());

        Ok(Self {
            manifest,
            capabilities,
            wasm_path: path,
            binary_hash,
            engine,
            module,
        })
    }

    fn extract_or_default_manifest(_module: &Module, path: Option<&Path>) -> WasmPluginManifest {
        let name = path
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("wasm_sandboxed_plugin");

        WasmPluginManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: format!("Sandboxed WebAssembly agent tool '{}'", name),
            author: Some("AI Sandbox Autonomous Engine".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "JSON or string argument passed to plugin" }
                }
            }),
        }
    }

    /// Execute the sandboxed guest with fuel consumption and capability enforcement
    pub fn execute_sandbox(&self, arguments: &serde_json::Value) -> Result<SandboxedExecutionResult> {
        let fuel_limit = self.capabilities.fuel_limit;
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.capabilities.max_memory_bytes)
            .build();

        let host_state = SandboxedWasmHostState {
            capabilities: self.capabilities.clone(),
            logs: Vec::new(),
            kv_store: HashMap::new(),
            violations: Vec::new(),
            limits,
        };

        let mut store = Store::new(&self.engine, host_state);
        store.limiter(|state| &mut state.limits);

        store.set_fuel(fuel_limit).map_err(|e| {
            HgbError::Execution(format!("Failed to configure WASM fuel limit: {}", e))
        })?;

        let mut linker = Linker::new(&self.engine);
        self.bind_capabilities(&mut linker)?;

        // Instantiate guest inside the capability sandbox
        let instance = match linker.instantiate(&mut store, &self.module) {
            Ok(inst) => inst,
            Err(e) => {
                let err_str = e.to_string();
                return Ok(SandboxedExecutionResult {
                    success: false,
                    exit_code: None,
                    output: serde_json::json!({ "error": err_str }),
                    fuel_consumed: fuel_limit.saturating_sub(store.get_fuel().unwrap_or(0)),
                    logs: store.data().logs.clone(),
                    trapped: true,
                    trap_reason: Some(format!("Instantiation error: {}", err_str)),
                });
            }
        };

        // Try invoking exported entrypoints
        let result = if let Ok(run_func) = instance.get_typed_func::<(), i32>(&mut store, "run") {
            match run_func.call(&mut store, ()) {
                Ok(exit_code) => {
                    let fuel_left = store.get_fuel().unwrap_or(0);
                    let fuel_consumed = fuel_limit.saturating_sub(fuel_left);
                    SandboxedExecutionResult {
                        success: true,
                        exit_code: Some(exit_code),
                        output: serde_json::json!({
                            "status": "success",
                            "exit_code": exit_code,
                            "plugin": self.manifest.name,
                            "message": format!("WASM function run() returned {}", exit_code)
                        }),
                        fuel_consumed,
                        logs: store.data().logs.clone(),
                        trapped: false,
                        trap_reason: None,
                    }
                }
                Err(trap) => {
                    let err_msg = trap.to_string();
                    let fuel_left = store.get_fuel().unwrap_or(1);
                    let is_fuel = fuel_left == 0
                        || trap.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel)
                        || err_msg.contains("fuel")
                        || trap.root_cause().to_string().contains("fuel");
                    let is_unreachable = trap.downcast_ref::<Trap>() == Some(&Trap::UnreachableCodeReached)
                        || err_msg.contains("unreachable");
                    let trap_reason = if is_fuel {
                        "fuel_exhausted".to_string()
                    } else if is_unreachable {
                        "unreachable".to_string()
                    } else {
                        err_msg
                    };
                    SandboxedExecutionResult {
                        success: false,
                        exit_code: None,
                        output: serde_json::json!({
                            "status": "trapped",
                            "error": trap_reason,
                            "fuel_limit": fuel_limit,
                        }),
                        fuel_consumed: fuel_limit,
                        logs: store.data().logs.clone(),
                        trapped: true,
                        trap_reason: Some(trap_reason),
                    }
                }
            }
        } else {
            // Default response when module doesn't export run()
            let fuel_left = store.get_fuel().unwrap_or(0);
            SandboxedExecutionResult {
                success: true,
                exit_code: Some(0),
                output: serde_json::json!({
                    "status": "success",
                    "plugin": self.manifest.name,
                    "version": self.manifest.version,
                    "input_received": arguments,
                    "capabilities": {
                        "allowed_reads": self.capabilities.allowed_read_paths.len(),
                        "allowed_writes": self.capabilities.allowed_write_paths.len(),
                        "allowed_hosts": self.capabilities.allowed_network_hosts.len(),
                        "fuel_limit": self.capabilities.fuel_limit,
                    }
                }),
                fuel_consumed: fuel_limit.saturating_sub(fuel_left),
                logs: store.data().logs.clone(),
                trapped: false,
                trap_reason: None,
            }
        };

        Ok(result)
    }

    /// Directly execute tool returning standard JSON Result (implements HgbTool semantics)
    pub fn execute(&self, arguments: &serde_json::Value) -> Result<serde_json::Value> {
        let res = self.execute_sandbox(arguments)?;
        if res.trapped {
            if let Some(ref reason) = res.trap_reason {
                if reason.contains("fuel") || reason == "fuel_exhausted" {
                    return Err(HgbError::Execution(format!(
                        "WASM runtime trapped: CPU fuel exhaustion (limit: {} units)",
                        self.capabilities.fuel_limit
                    )));
                }
                if reason.contains("Capability violation") {
                    return Err(HgbError::Security(format!("Sandboxed security violation: {}", reason)));
                }
            }
            return Err(HgbError::Execution(format!(
                "WASM runtime trapped: {}",
                res.trap_reason.unwrap_or_else(|| "Unknown trap".to_string())
            )));
        }
        Ok(res.output)
    }

    /// Bind host syscalls enforcing capability security
    fn bind_capabilities(&self, linker: &mut Linker<SandboxedWasmHostState>) -> Result<()> {
        // Safe logging
        linker
            .func_wrap("env", "hgb_host_log", |mut caller: Caller<'_, SandboxedWasmHostState>, ptr: i32, len: i32| {
                if let Some(msg_bytes) = read_guest_memory(&mut caller, ptr, len) {
                    if let Ok(msg) = std::str::from_utf8(&msg_bytes) {
                        caller.data_mut().logs.push(msg.to_string());
                    }
                }
            })
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // Filesystem read: trapped if unauthorized
        linker
            .func_wrap(
                "env",
                "hgb_fs_read",
                |mut caller: Caller<'_, SandboxedWasmHostState>,
                 path_ptr: i32,
                 path_len: i32,
                 out_ptr: i32,
                 max_out: i32|
                 -> std::result::Result<i32, wasmtime::Error> {
                    let path_bytes = read_guest_memory(&mut caller, path_ptr, path_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid path memory pointer"))?;
                    let path_str = std::str::from_utf8(&path_bytes)
                        .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in path"))?
                        .to_string();
                    let target_path = PathBuf::from(&path_str);

                    if !caller.data().capabilities.is_read_allowed(&target_path) {
                        let violation = WasmCapabilityViolation {
                            kind: "unauthorized_read".to_string(),
                            target: path_str.clone(),
                            message: format!("Read access to '{}' denied by capability sandbox", path_str),
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
                            if write_guest_memory(&mut caller, out_ptr, &data[..write_len]) {
                                Ok(write_len as i32)
                            } else {
                                Err(wasmtime::Error::msg("Failed writing output buffer to guest memory"))
                            }
                        }
                        Err(_) => Ok(-1), // File read error (e.g. not found)
                    }
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // Filesystem write: trapped if unauthorized
        linker
            .func_wrap(
                "env",
                "hgb_fs_write",
                |mut caller: Caller<'_, SandboxedWasmHostState>,
                 path_ptr: i32,
                 path_len: i32,
                 data_ptr: i32,
                 data_len: i32|
                 -> std::result::Result<i32, wasmtime::Error> {
                    let path_bytes = read_guest_memory(&mut caller, path_ptr, path_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid path memory pointer"))?;
                    let path_str = std::str::from_utf8(&path_bytes)
                        .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in path"))?
                        .to_string();
                    let target_path = PathBuf::from(&path_str);

                    if !caller.data().capabilities.is_write_allowed(&target_path) {
                        let violation = WasmCapabilityViolation {
                            kind: "unauthorized_write".to_string(),
                            target: path_str.clone(),
                            message: format!("Write access to '{}' denied by capability sandbox", path_str),
                        };
                        caller.data_mut().violations.push(violation);
                        return Err(wasmtime::Error::msg(format!(
                            "Capability violation: write access to '{}' denied",
                            path_str
                        )));
                    }

                    let payload = read_guest_memory(&mut caller, data_ptr, data_len)
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

        // Network fetch: trapped if unauthorized host
        linker
            .func_wrap(
                "env",
                "hgb_net_fetch",
                |mut caller: Caller<'_, SandboxedWasmHostState>,
                 url_ptr: i32,
                 url_len: i32,
                 _out_ptr: i32,
                 _max_out: i32|
                 -> std::result::Result<i32, wasmtime::Error> {
                    let url_bytes = read_guest_memory(&mut caller, url_ptr, url_len)
                        .ok_or_else(|| wasmtime::Error::msg("Invalid URL memory pointer"))?;
                    let url_str = std::str::from_utf8(&url_bytes)
                        .map_err(|_| wasmtime::Error::msg("Invalid UTF-8 in URL"))?
                        .to_string();

                    if !caller.data().capabilities.is_network_host_allowed(&url_str) {
                        let violation = WasmCapabilityViolation {
                            kind: "unauthorized_network".to_string(),
                            target: url_str.clone(),
                            message: format!("Network access to '{}' denied by capability sandbox", url_str),
                        };
                        caller.data_mut().violations.push(violation);
                        return Err(wasmtime::Error::msg(format!(
                            "Capability violation: network access to '{}' denied",
                            url_str
                        )));
                    }

                    // Network host permitted
                    Ok(0)
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        Ok(())
    }

    /// Direct capability gate check for file read
    pub fn check_and_read_file(&self, path: &Path) -> Result<Vec<u8>> {
        if !self.capabilities.is_read_allowed(path) {
            return Err(HgbError::Security(format!(
                "Capability violation: read access to '{}' denied by sandbox policy",
                path.display()
            )));
        }
        fs::read(path).map_err(HgbError::Io)
    }

    /// Direct capability gate check for file write
    pub fn check_and_write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        if !self.capabilities.is_write_allowed(path) {
            return Err(HgbError::Security(format!(
                "Capability violation: write access to '{}' denied by sandbox policy",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, data).map_err(HgbError::Io)
    }

    /// Direct capability gate check for network fetch
    pub fn check_and_fetch_url(&self, url: &str) -> Result<String> {
        if !self.capabilities.is_network_host_allowed(url) {
            return Err(HgbError::Security(format!(
                "Capability violation: network access to '{}' denied by sandbox policy",
                url
            )));
        }
        Ok(format!("Authorized network access to {}", url))
    }
}

/// Helper function to read guest memory slice
fn read_guest_memory<T>(caller: &mut Caller<'_, T>, ptr: i32, len: i32) -> Option<Vec<u8>> {
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

/// Helper function to write guest memory slice
fn write_guest_memory<T>(caller: &mut Caller<'_, T>, ptr: i32, data: &[u8]) -> bool {
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

/// Adapter converting a `SandboxedWasmPlugin` into an agent tool (`HgbTool`)
pub struct SandboxedWasmToolAdapter {
    plugin: Arc<SandboxedWasmPlugin>,
}

impl SandboxedWasmToolAdapter {
    pub fn new(plugin: Arc<SandboxedWasmPlugin>) -> Self {
        Self { plugin }
    }
}

#[async_trait]
impl HgbTool for SandboxedWasmToolAdapter {
    fn name(&self) -> &str {
        &self.plugin.manifest.name
    }

    fn description(&self) -> &str {
        &self.plugin.manifest.description
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        self.plugin.execute(&arguments)
    }
}

/// Summary report emitted after a dynamic scan and hot-reload cycle
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReloadReport {
    pub loaded: Vec<String>,
    pub reloaded: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: Vec<String>,
    pub errors: Vec<String>,
}

/// Dynamic WebAssembly Plugin Catalog & Hot-Reloading Registry
pub struct WasmPluginRegistry {
    pub plugin_dirs: Vec<PathBuf>,
    pub catalog: Arc<RwLock<HashMap<String, Arc<SandboxedWasmPlugin>>>>,
    fingerprints: Arc<RwLock<HashMap<String, (u64, String)>>>,
    default_capabilities: PluginCapabilities,
}

impl WasmPluginRegistry {
    /// Initialize registry watching workspace `.hgb/plugins/wasm/`
    pub fn new<P: AsRef<Path>>(workspace_root: P) -> Self {
        let ws = workspace_root.as_ref();
        let mut dirs = Vec::new();
        dirs.push(ws.join(".hgb").join("plugins").join("wasm"));
        dirs.push(ws.join(".hgb").join("plugins"));
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(home).join(".hgb").join("plugins").join("wasm"));
        }

        Self {
            plugin_dirs: dirs,
            catalog: Arc::new(RwLock::new(HashMap::new())),
            fingerprints: Arc::new(RwLock::new(HashMap::new())),
            default_capabilities: PluginCapabilities::default(),
        }
    }

    /// Initialize with a single explicit plugin directory
    pub fn with_plugin_dir<P: AsRef<Path>>(plugin_dir: P) -> Self {
        Self {
            plugin_dirs: vec![plugin_dir.as_ref().to_path_buf()],
            catalog: Arc::new(RwLock::new(HashMap::new())),
            fingerprints: Arc::new(RwLock::new(HashMap::new())),
            default_capabilities: PluginCapabilities::default(),
        }
    }

    /// Set default capabilities applied to newly discovered modules
    pub fn set_default_capabilities(&mut self, caps: PluginCapabilities) {
        self.default_capabilities = caps;
    }

    /// Scan all watched plugin directories, hot load new or modified plugins, and deregister removed ones
    pub fn scan_and_reload(&self) -> Result<ReloadReport> {
        let mut report = ReloadReport::default();
        let mut discovered_names = std::collections::HashSet::new();

        for dir in &self.plugin_dirs {
            if !dir.is_dir() {
                continue;
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("wasm") {
                    continue;
                }

                let file_name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("wasm_plugin")
                    .to_string();

                discovered_names.insert(file_name.clone());

                let metadata = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(e) => {
                        report.errors.push(format!("{}: metadata error: {}", file_name, e));
                        continue;
                    }
                };

                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let bytes = match fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        report.errors.push(format!("{}: read error: {}", file_name, e));
                        continue;
                    }
                };
                let hash = blake3::hash(&bytes).to_hex().to_string();

                // Check existing fingerprint
                let mut needs_reload = false;
                let mut is_new = false;
                {
                    let fps = self.fingerprints.read().unwrap();
                    if let Some((old_mtime, old_hash)) = fps.get(&file_name) {
                        if *old_hash != hash || *old_mtime != mtime {
                            needs_reload = true;
                        } else {
                            report.unchanged.push(file_name.clone());
                            continue;
                        }
                    } else {
                        is_new = true;
                    }
                }

                // Load sidecar capabilities if present (<plugin>.caps.json)
                let caps = self.resolve_sidecar_capabilities(&path);

                match SandboxedWasmPlugin::load_from_bytes(&bytes, Some(path.clone()), caps) {
                    Ok(plugin) => {
                        let plugin_arc = Arc::new(plugin);
                        self.catalog
                            .write()
                            .unwrap()
                            .insert(file_name.clone(), plugin_arc);
                        self.fingerprints
                            .write()
                            .unwrap()
                            .insert(file_name.clone(), (mtime, hash));

                        if is_new {
                            report.loaded.push(file_name);
                        } else if needs_reload {
                            report.reloaded.push(file_name);
                        }
                    }
                    Err(e) => {
                        report.errors.push(format!("{}: load error: {}", file_name, e));
                    }
                }
            }
        }

        // Deregister plugins that were removed from disk
        let mut catalog = self.catalog.write().unwrap();
        let mut fingerprints = self.fingerprints.write().unwrap();
        let existing_names: Vec<String> = catalog.keys().cloned().collect();

        for name in existing_names {
            if !discovered_names.contains(&name) {
                // If it was loaded from a file (has fingerprint), remove it
                if fingerprints.remove(&name).is_some() {
                    catalog.remove(&name);
                    report.removed.push(name);
                }
            }
        }

        Ok(report)
    }

    /// Read sidecar `.caps.json` if it exists next to the wasm binary
    fn resolve_sidecar_capabilities(&self, wasm_path: &Path) -> PluginCapabilities {
        let sidecar = wasm_path.with_extension("caps.json");
        if sidecar.is_file() {
            if let Ok(content) = fs::read_to_string(&sidecar) {
                if let Ok(caps) = serde_json::from_str::<PluginCapabilities>(&content) {
                    return caps;
                }
            }
        }
        self.default_capabilities.clone()
    }

    /// Hot-register untrusted AI-generated code or plugin bytes directly in-memory
    pub fn register_plugin_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        capabilities: Option<PluginCapabilities>,
    ) -> Result<Arc<SandboxedWasmPlugin>> {
        let caps = capabilities.unwrap_or_else(|| self.default_capabilities.clone());
        let plugin = SandboxedWasmPlugin::load_from_bytes(bytes, None, caps)?;
        let plugin_arc = Arc::new(plugin);
        self.catalog
            .write()
            .unwrap()
            .insert(name.to_string(), plugin_arc.clone());
        Ok(plugin_arc)
    }

    /// Retrieve loaded plugin instance by name
    pub fn get_plugin(&self, name: &str) -> Option<Arc<SandboxedWasmPlugin>> {
        self.catalog.read().unwrap().get(name).cloned()
    }

    /// Export currently loaded plugins as `HgbTool` objects for the ReAct engine
    pub fn export_agent_tools(&self) -> Vec<Arc<dyn HgbTool>> {
        let catalog = self.catalog.read().unwrap();
        catalog
            .values()
            .map(|plugin| Arc::new(SandboxedWasmToolAdapter::new(plugin.clone())) as Arc<dyn HgbTool>)
            .collect()
    }

    /// Returns list of all currently registered plugin manifests
    pub fn list_manifests(&self) -> Vec<WasmPluginManifest> {
        let catalog = self.catalog.read().unwrap();
        catalog.values().map(|p| p.manifest.clone()).collect()
    }
}

/// Type alias for backward compatibility and semantic clarity
pub type WasmPluginSandboxEngine = WasmPluginRegistry;
pub type WasmPluginCatalog = WasmPluginRegistry;

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_sleb128(mut value: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut more = true;
        while more {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if (value == 0 && (byte & 0x40) == 0) || (value == -1 && (byte & 0x40) != 0) {
                more = false;
            } else {
                byte |= 0x80;
            }
            bytes.push(byte);
        }
        bytes
    }

    /// Generate minimal valid WASM binary with an exported `run` function returning any i32
    fn generate_valid_wasm(return_val: i32) -> Vec<u8> {
        let leb = encode_sleb128(return_val);
        let mut body = vec![0x00, 0x41];
        body.extend_from_slice(&leb);
        body.push(0x0b);

        let body_len = body.len() as u8;
        let sec_len = (1 + 1 + body.len()) as u8;

        let mut wasm = vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00,
            0x0a, sec_len, 0x01, body_len,
        ];
        wasm.extend_from_slice(&body);
        wasm
    }

    /// Generate an infinite loop WASM module that exhausts CPU fuel
    fn generate_infinite_loop_wasm() -> Vec<u8> {
        // (module
        //   (func (export "run") (result i32)
        //     (loop (br 0))
        //     i32.const 0
        //   )
        // )
        vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00,
            0x0a, 0x0b, 0x01, 0x09, 0x00, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x41, 0x00, 0x0b,
        ]
    }

    #[test]
    fn test_capability_sandbox_enforcement() {
        let tmp = std::env::temp_dir().join(format!("hgb_cap_test_{}", std::process::id()));
        let allowed_dir = tmp.join("allowed");
        let forbidden_dir = tmp.join("forbidden");
        let _ = fs::create_dir_all(&allowed_dir);
        let _ = fs::create_dir_all(&forbidden_dir);

        let allowed_file = allowed_dir.join("sample.txt");
        let forbidden_file = forbidden_dir.join("secret.txt");
        fs::write(&allowed_file, "clean content").expect("write allowed");
        fs::write(&forbidden_file, "sensitive secret").expect("write forbidden");

        let caps = PluginCapabilities::new()
            .with_read_path(&allowed_dir)
            .with_write_path(&allowed_dir)
            .with_network_host("api.github.com");

        let wasm_bytes = generate_valid_wasm(42);
        let plugin = SandboxedWasmPlugin::load_from_bytes(&wasm_bytes, None, caps)
            .expect("plugin load");

        // 1. Allowed path read succeeds
        let read_res = plugin.check_and_read_file(&allowed_file);
        assert!(read_res.is_ok());
        assert_eq!(String::from_utf8(read_res.unwrap()).unwrap(), "clean content");

        // 2. Forbidden path read rejected by security sandbox
        let forbidden_res = plugin.check_and_read_file(&forbidden_file);
        assert!(forbidden_res.is_err());
        match forbidden_res {
            Err(HgbError::Security(msg)) => {
                assert!(msg.contains("Capability violation"));
            }
            _ => panic!("Expected HgbError::Security for unauthorized read"),
        }

        // 3. Allowed path write succeeds
        let new_file = allowed_dir.join("out.txt");
        let write_res = plugin.check_and_write_file(&new_file, b"agent output");
        assert!(write_res.is_ok());
        assert_eq!(fs::read_to_string(&new_file).unwrap(), "agent output");

        // 4. Forbidden path write rejected
        let bad_write = forbidden_dir.join("malicious.sh");
        let write_forbidden = plugin.check_and_write_file(&bad_write, b"rm -rf /");
        assert!(write_forbidden.is_err());

        // 5. Network host enforcement
        assert!(plugin.check_and_fetch_url("https://api.github.com/v1").is_ok());
        assert!(plugin.check_and_fetch_url("https://evil-exfiltration.com").is_err());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fuel_exhaustion_handling() {
        let infinite_wasm = generate_infinite_loop_wasm();
        let caps = PluginCapabilities::new().with_fuel_limit(50_000); // 50k instructions limit

        let plugin = SandboxedWasmPlugin::load_from_bytes(&infinite_wasm, None, caps)
            .expect("load infinite wasm");

        // Execute sandbox report
        let res = plugin.execute_sandbox(&serde_json::json!({})).expect("execute sandbox");
        assert!(res.trapped);
        assert_eq!(res.trap_reason.as_deref(), Some("fuel_exhausted"));
        assert_eq!(res.fuel_consumed, 50_000);

        // Execute via standard execute() traps gracefully into HgbError::Execution
        let exec_err = plugin.execute(&serde_json::json!({}));
        assert!(exec_err.is_err());
        match exec_err {
            Err(HgbError::Execution(msg)) => {
                assert!(msg.contains("fuel"));
            }
            _ => panic!("Expected HgbError::Execution for fuel exhaustion"),
        }
    }

    #[test]
    fn test_hot_loading_and_dynamic_tool_registration() {
        let tmp = std::env::temp_dir().join(format!("hgb_hot_test_{}", std::process::id()));
        let wasm_dir = tmp.join("plugins").join("wasm");
        let _ = fs::create_dir_all(&wasm_dir);

        let registry = WasmPluginRegistry::with_plugin_dir(&wasm_dir);

        // 1. Initial scan is empty
        let initial_report = registry.scan_and_reload().expect("scan");
        assert!(initial_report.loaded.is_empty());
        assert_eq!(registry.export_agent_tools().len(), 0);

        // 2. Drop a new WASM module into the directory
        let plugin_path = wasm_dir.join("calc_tool.wasm");
        fs::write(&plugin_path, generate_valid_wasm(10)).expect("write wasm");

        let report1 = registry.scan_and_reload().expect("scan after add");
        assert_eq!(report1.loaded, vec!["calc_tool"]);
        assert_eq!(registry.export_agent_tools().len(), 1);

        let tool = registry.get_plugin("calc_tool").expect("find plugin");
        let res = tool.execute(&serde_json::json!({})).expect("exec tool");
        assert_eq!(res.get("exit_code").and_then(|v| v.as_i64()), Some(10));

        // 3. Hot update the module with new behavior (return 99)
        fs::write(&plugin_path, generate_valid_wasm(99)).expect("update wasm");
        let report2 = registry.scan_and_reload().expect("scan after update");
        assert_eq!(report2.reloaded, vec!["calc_tool"]);

        let tool_updated = registry.get_plugin("calc_tool").expect("find updated plugin");
        let res2 = tool_updated.execute(&serde_json::json!({})).expect("exec tool updated");
        assert_eq!(res2.get("exit_code").and_then(|v| v.as_i64()), Some(99));

        // 4. In-memory dynamic registration of AI-generated code
        let dynamic_bytes = generate_valid_wasm(77);
        let dynamic_plugin = registry
            .register_plugin_bytes("ai_generated_summarizer", &dynamic_bytes, None)
            .expect("register bytes");
        assert_eq!(dynamic_plugin.manifest.name, "wasm_sandboxed_plugin");
        assert!(registry.get_plugin("ai_generated_summarizer").is_some());

        // 5. Remove plugin file and verify deregistration
        let _ = fs::remove_file(&plugin_path);
        let report3 = registry.scan_and_reload().expect("scan after remove");
        assert_eq!(report3.removed, vec!["calc_tool"]);
        assert!(registry.get_plugin("calc_tool").is_none());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_pgo_representative_workload_execution() {
        use crate::shm_ring_buffer::{ShmRingConsumer, ShmRingProducer};
        use crate::token_shredder::{TokenPolicyConfig, TokenPolicyEngine, TokenShredder};
        use rusqlite::Connection;

        // 1. TokenShredder credential redaction and JSON/base64 compaction
        let policy_config = TokenPolicyConfig {
            redact_credentials: true,
            shred_json_blobs: true,
            shred_base64: true,
            json_blob_min_bytes: 32,
            base64_min_bytes: 32,
            ..Default::default()
        };
        let mut shredder = TokenShredder::new();
        shredder.policy = Some(TokenPolicyEngine::new(policy_config));

        let raw_input = r#"
            sk-proj-1234567890abcdefABCDEF1234567890
            {"user":"pgo_trainer","flags":[1,2,3,4,5],"status":"active","metrics":{"cpu":45}}
            [PROGRESS] [==========>] 100% 12.0 MB / 12.0 MB
        "#;
        let shred_res = shredder.shred(raw_input);
        assert!(!shred_res.content.contains("sk-proj-1234567890"));
        assert!(shred_res.shredded_bytes < shred_res.original_bytes);

        // 2. Zero-copy shared memory ring buffer IPC roundtrips
        let tmp = std::env::temp_dir().join(format!("hgb_pgo_unit_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let ring_path = tmp.join("unit_ring.shm");
        let capacity = 64 * 1024;
        let mut producer = ShmRingProducer::create_with_path(&ring_path, "unit_prod", capacity).expect("create producer");
        let mut consumer = ShmRingConsumer::open_with_path(&ring_path, "unit_cons").expect("open consumer");

        let test_payload = b"HAGIBIS_ZERO_COPY_IPC_PGO_FRAME";
        assert!(producer.try_write(test_payload).expect("write frame"));
        let received = consumer.try_read().expect("read frame");
        assert_eq!(received.as_deref(), Some(&test_payload[..]));
        let _ = fs::remove_dir_all(&tmp);

        // 3. Dynamic SIMD / vector distance calculations (dot product, cosine similarity, normalization)
        let vec_a = vec![0.6f32; 128];
        let vec_b = vec![0.8f32; 128];
        let dot: f32 = vec_a.iter().zip(vec_b.iter()).map(|(a, b)| a * b).sum();
        let norm_a: f32 = vec_a.iter().map(|a| a * a).sum::<f32>().sqrt();
        let norm_b: f32 = vec_b.iter().map(|b| b * b).sum::<f32>().sqrt();
        let cos = dot / (norm_a * norm_b);
        assert!(dot > 0.0);
        assert!((cos - 1.0).abs() < 1e-4);

        // 4. WASM kernel execution
        let wasm_bytes = generate_valid_wasm(42);
        let caps = PluginCapabilities::new().with_fuel_limit(100_000);
        let plugin = SandboxedWasmPlugin::load_from_bytes(&wasm_bytes, None, caps).expect("wasm load");
        let exec_res = plugin.execute_sandbox(&serde_json::json!({"action": "pgo_verify"})).expect("exec wasm");
        assert!(exec_res.success);
        assert_eq!(exec_res.exit_code, Some(42));

        // 5. SQLite FTS5 + vector KNN index execution
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE VIRTUAL TABLE fts_code USING fts5(filepath, content);
             INSERT INTO fts_code (filepath, content) VALUES ('src/lib.rs', 'pub fn hagibis_pgo_core_dispatch() -> bool { true }');"
        ).expect("fts5 setup");

        let mut stmt = conn.prepare("SELECT filepath FROM fts_code WHERE fts_code MATCH ?1").expect("prepare fts5");
        let mut rows = stmt.query(["hagibis_pgo_core_dispatch"]).expect("query fts5");
        let row = rows.next().expect("read row").expect("has match");
        let path: String = row.get(0).expect("get path");
        assert_eq!(path, "src/lib.rs");
    }
}
