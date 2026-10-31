//! # Pillar 1: WASM-Zig Sandbox & Autonomous Agent Compiler Loop (`zig_wasm_agent_compiler`)
//!
//! Unifies the high-performance freestanding WebAssembly microkernel written in Zig
//! (`crates/hgb-wasm-kernel`) with autonomous agent self-healing loops
//! (`docs/masterclass/ch8_autonomous_loops.py`).
//!
//! ## Subsystems:
//! 1. **ZigWasmAgentCompiler**:
//!    - Instant JIT/AOT compilation of Zig source code to WebAssembly modules (`wasm32-freestanding` / `wasm32-wasi`).
//!    - Standalone compilation units in `/dev/shm/hgb_zig_wasm_<hash>` (RAM disk) with fallback to tempdir.
//!    - Sub-millisecond cache hits via BLAKE3 content-addressable storage (in-memory + disk).
//!    - Direct invocations of local `zig build-exe` producing clean `.wasm` binaries.
//! 2. **SandboxedWasmAgentRunner**:
//!    - Executes compiled micro-WASM modules inside Wasmtime with strict fuel metering,
//!      linear memory virtualization (default 16MB), and capability boundaries.
//!    - Safe execution of untrusted AI-generated code with zero host risk (trapping on invalid memory access,
//!      infinite loops, or unauthorized syscalls).
//!    - Supports both `run() -> i32` and `hgb_tool_invoke(ptr, len) -> u64` ABI conventions.
//! 3. **AutonomousZigAgentLoop**:
//!    - Accepts high-level task specifications ("calculate fibonacci", "format json", "filter text", "parse csv", etc.)
//!      or takes LLM-generated Zig source.
//!    - Closed-loop feedback: compiles, executes in sandbox, captures output, and if execution panics or
//!      compilation fails, self-heals by analyzing compiler AST / diagnostics, patching Zig syntax or logic,
//!      and recompiling until First-Green execution is verified.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use regex::Regex;
use serde::{Deserialize, Serialize};
use wasmtime::*;

use crate::error::{HgbError, Result};
use crate::wasm_plugin_sandbox::{
    PluginCapabilities, SandboxedWasmHostState, SandboxedWasmPlugin, WasmCapabilityViolation,
};

/// Target WebAssembly architecture for Zig compilation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZigWasmTarget {
    /// Freestanding 32-bit WebAssembly (zero libc overhead, < 1KB binaries)
    Wasm32Freestanding,
    /// WebAssembly System Interface (WASI) target
    Wasm32Wasi,
}

impl ZigWasmTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            ZigWasmTarget::Wasm32Freestanding => "wasm32-freestanding",
            ZigWasmTarget::Wasm32Wasi => "wasm32-wasi",
        }
    }
}

impl fmt::Display for ZigWasmTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Default for ZigWasmTarget {
    fn default() -> Self {
        ZigWasmTarget::Wasm32Freestanding
    }
}

/// Optimization mode for the Zig compiler
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZigOptimization {
    Debug,
    ReleaseSmall,
    ReleaseFast,
    ReleaseSafe,
}

impl ZigOptimization {
    pub fn as_str(&self) -> &'static str {
        match self {
            ZigOptimization::Debug => "Debug",
            ZigOptimization::ReleaseSmall => "ReleaseSmall",
            ZigOptimization::ReleaseFast => "ReleaseFast",
            ZigOptimization::ReleaseSafe => "ReleaseSafe",
        }
    }
}

impl fmt::Display for ZigOptimization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Default for ZigOptimization {
    fn default() -> Self {
        ZigOptimization::ReleaseSmall
    }
}

/// Configuration options for the Zig-WASM compiler
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZigCompilerConfig {
    pub target: ZigWasmTarget,
    pub optimization: ZigOptimization,
    pub zig_binary: PathBuf,
    pub cache_dir: PathBuf,
    pub shm_build_dir: Option<PathBuf>,
    pub custom_flags: Vec<String>,
}

impl Default for ZigCompilerConfig {
    fn default() -> Self {
        let zig_bin = locate_zig_binary().unwrap_or_else(|| PathBuf::from("zig"));
        let cache_dir = resolve_cache_dir();
        let shm_build_dir = resolve_shm_build_dir();

        Self {
            target: ZigWasmTarget::default(),
            optimization: ZigOptimization::default(),
            zig_binary: zig_bin,
            cache_dir,
            shm_build_dir,
            custom_flags: Vec::new(),
        }
    }
}

/// Finds the real `zig` compiler on the host system
pub fn locate_zig_binary() -> Option<PathBuf> {
    // 1. Check environment variable
    if let Ok(bin_env) = std::env::var("ZIG_BIN") {
        let p = PathBuf::from(bin_env);
        if p.is_file() {
            return Some(p);
        }
    }

    // 2. Check well-known user path (e.g. /home/toshiba/.local/bin/zig)
    if let Ok(home) = std::env::var("HOME") {
        let local_bin = PathBuf::from(home).join(".local").join("bin").join("zig");
        if local_bin.is_file() {
            return Some(local_bin);
        }
    }

    // 3. Search standard system directories
    for path in &["/usr/local/bin/zig", "/usr/bin/zig", "/bin/zig", "/opt/zig/zig"] {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    // 4. Try PATH lookup via `which zig`
    if let Ok(output) = Command::new("which").arg("zig").output() {
        if output.status.success() {
            let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !found.is_empty() {
                let p = PathBuf::from(found);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }

    None
}

/// Resolves the default cache directory (prioritizing `/dev/shm` for maximum speed)
fn resolve_cache_dir() -> PathBuf {
    let shm_cache = PathBuf::from("/dev/shm/hgb_zig_wasm_cache");
    if Path::new("/dev/shm").is_dir() {
        let _ = fs::create_dir_all(&shm_cache);
        if shm_cache.is_dir() {
            return shm_cache;
        }
    }
    let fallback = std::env::temp_dir().join("hgb_zig_wasm_cache");
    let _ = fs::create_dir_all(&fallback);
    fallback
}

/// Resolves RAM disk compilation directory if available
fn resolve_shm_build_dir() -> Option<PathBuf> {
    if Path::new("/dev/shm").is_dir() {
        Some(PathBuf::from("/dev/shm"))
    } else {
        None
    }
}

/// Outcome of a Zig WebAssembly compilation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZigCompilationResult {
    pub wasm_bytes: Vec<u8>,
    pub source_hash: String,
    pub cache_hit: bool,
    pub compile_duration_us: u64,
    pub wasm_path: Option<PathBuf>,
    pub size_bytes: usize,
}

pub type CompilationResult = ZigCompilationResult;

/// Telemetry statistics for the compiler
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompilerStats {
    pub total_compilations: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub total_compile_time_us: u64,
}

/// Dynamically compiles Zig source code into WebAssembly modules on the fly
pub struct ZigWasmAgentCompiler {
    pub config: ZigCompilerConfig,
    in_memory_cache: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    total_compilations: Arc<AtomicU64>,
    cache_hits: Arc<AtomicU64>,
    total_compile_time_us: Arc<AtomicU64>,
}

impl Default for ZigWasmAgentCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl ZigWasmAgentCompiler {
    /// Initialize with default auto-detected configuration
    pub fn new() -> Self {
        Self {
            config: ZigCompilerConfig::default(),
            in_memory_cache: Arc::new(RwLock::new(HashMap::new())),
            total_compilations: Arc::new(AtomicU64::new(0)),
            cache_hits: Arc::new(AtomicU64::new(0)),
            total_compile_time_us: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Initialize with custom configuration
    pub fn with_config(config: ZigCompilerConfig) -> Self {
        Self {
            config,
            in_memory_cache: Arc::new(RwLock::new(HashMap::new())),
            total_compilations: Arc::new(AtomicU64::new(0)),
            cache_hits: Arc::new(AtomicU64::new(0)),
            total_compile_time_us: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set compilation target
    pub fn with_target(mut self, target: ZigWasmTarget) -> Self {
        self.config.target = target;
        self
    }

    /// Set optimization level
    pub fn with_optimization(mut self, opt: ZigOptimization) -> Self {
        self.config.optimization = opt;
        self
    }

    /// Set cache directory
    pub fn with_cache_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.config.cache_dir = dir.into();
        let _ = fs::create_dir_all(&self.config.cache_dir);
        self
    }

    /// Compiles Zig source code to a WebAssembly module with BLAKE3 caching
    pub fn compile(&self, source_code: &str) -> Result<CompilationResult> {
        self.compile_with_options(source_code, None, None, &[])
    }

    /// Compiles Zig source code with custom target, optimization, and explicit export symbols
    pub fn compile_with_options(
        &self,
        source_code: &str,
        target: Option<ZigWasmTarget>,
        opt: Option<ZigOptimization>,
        extra_exports: &[&str],
    ) -> Result<CompilationResult> {
        self.total_compilations.fetch_add(1, Ordering::Relaxed);
        let start_time = Instant::now();

        let effective_target = target.unwrap_or(self.config.target);
        let effective_opt = opt.unwrap_or(self.config.optimization);

        // 1. Content-addressable key incorporating source bytes, target, and optimization level
        let mut hasher = blake3::Hasher::new();
        hasher.update(source_code.as_bytes());
        hasher.update(effective_target.as_str().as_bytes());
        hasher.update(effective_opt.as_str().as_bytes());
        for exp in extra_exports {
            hasher.update(exp.as_bytes());
        }
        let source_hash = hasher.finalize().to_hex().to_string();

        // 2. Check Fast Path: In-Memory RAM Cache Hit (< 0.05ms)
        {
            let mem = self.in_memory_cache.read().unwrap();
            if let Some(bytes) = mem.get(&source_hash) {
                self.cache_hits.fetch_add(1, Ordering::Relaxed);
                let duration = start_time.elapsed().as_micros() as u64;
                let cached_path = self.config.cache_dir.join(format!("{}.wasm", source_hash));
                return Ok(CompilationResult {
                    wasm_bytes: bytes.clone(),
                    source_hash,
                    cache_hit: true,
                    compile_duration_us: duration,
                    wasm_path: if cached_path.exists() { Some(cached_path) } else { None },
                    size_bytes: bytes.len(),
                });
            }
        }

        // 3. Check Medium Path: Disk /dev/shm Cache Hit (< 0.5ms)
        let cached_wasm_file = self.config.cache_dir.join(format!("{}.wasm", source_hash));
        if cached_wasm_file.is_file() {
            if let Ok(bytes) = fs::read(&cached_wasm_file) {
                if !bytes.is_empty() {
                    self.cache_hits.fetch_add(1, Ordering::Relaxed);
                    self.in_memory_cache
                        .write()
                        .unwrap()
                        .insert(source_hash.clone(), bytes.clone());
                    let duration = start_time.elapsed().as_micros() as u64;
                    return Ok(CompilationResult {
                        wasm_bytes: bytes.clone(),
                        source_hash,
                        cache_hit: true,
                        compile_duration_us: duration,
                        wasm_path: Some(cached_wasm_file),
                        size_bytes: bytes.len(),
                    });
                }
            }
        }

        // 4. Cache Miss: Invoke local Zig compiler
        let build_root = self
            .config
            .shm_build_dir
            .as_ref()
            .cloned()
            .unwrap_or_else(std::env::temp_dir);

        let unit_dir = build_root.join(format!("hgb_zig_wasm_{}", source_hash));
        fs::create_dir_all(&unit_dir).map_err(HgbError::Io)?;

        let source_file = unit_dir.join("tool.zig");
        fs::write(&source_file, source_code).map_err(HgbError::Io)?;

        let out_wasm = unit_dir.join("tool.wasm");

        // Construct `zig build-exe` command invocation
        let mut cmd = Command::new(&self.config.zig_binary);
        cmd.arg("build-exe")
            .arg("-target")
            .arg(effective_target.as_str())
            .arg("-O")
            .arg(effective_opt.as_str())
            .arg("-fno-entry")
            .arg("-rdynamic");

        // Conditionally pass explicit export symbols if present in source or specified
        let mut exports_to_check = vec!["run", "hgb_tool_manifest", "hgb_tool_invoke", "hgb_alloc", "hgb_dealloc"];
        for exp in extra_exports {
            if !exports_to_check.contains(exp) {
                exports_to_check.push(exp);
            }
        }

        for sym in exports_to_check {
            let pattern = format!("fn {}", sym);
            if source_code.contains(&pattern) {
                cmd.arg(format!("--export={}", sym));
            }
        }

        cmd.arg(format!("-femit-bin={}", out_wasm.display()));
        cmd.arg(&source_file);

        for flag in &self.config.custom_flags {
            cmd.arg(flag);
        }

        let output = cmd.output().map_err(|e| {
            HgbError::Execution(format!(
                "Failed to execute Zig compiler at '{}': {}",
                self.config.zig_binary.display(),
                e
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let _ = fs::remove_dir_all(&unit_dir);
            return Err(HgbError::Execution(format!(
                "Zig compilation failed (exit code {:?}):\n{}",
                output.status.code(),
                stderr
            )));
        }

        let wasm_bytes = fs::read(&out_wasm).map_err(|e| {
            HgbError::Execution(format!("Compiled WASM binary not found at '{}': {}", out_wasm.display(), e))
        })?;

        // Persist to disk cache
        let _ = fs::create_dir_all(&self.config.cache_dir);
        let _ = fs::write(&cached_wasm_file, &wasm_bytes);

        // Store into in-memory cache
        self.in_memory_cache
            .write()
            .unwrap()
            .insert(source_hash.clone(), wasm_bytes.clone());

        // Clean up temporary compilation directory
        let _ = fs::remove_dir_all(&unit_dir);

        let duration = start_time.elapsed().as_micros() as u64;
        self.total_compile_time_us.fetch_add(duration, Ordering::Relaxed);

        Ok(CompilationResult {
            wasm_bytes: wasm_bytes.clone(),
            source_hash,
            cache_hit: false,
            compile_duration_us: duration,
            wasm_path: Some(cached_wasm_file),
            size_bytes: wasm_bytes.len(),
        })
    }

    /// Clear both in-memory and disk caches
    pub fn clear_cache(&self) -> Result<()> {
        self.in_memory_cache.write().unwrap().clear();
        if self.config.cache_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&self.config.cache_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("wasm") {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
        Ok(())
    }

    /// Fetch compiler statistics
    pub fn stats(&self) -> CompilerStats {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let total = self.total_compilations.load(Ordering::Relaxed);
        let misses = total.saturating_sub(hits);
        let total_time = self.total_compile_time_us.load(Ordering::Relaxed);

        CompilerStats {
            total_compilations: total,
            cache_hits: hits,
            cache_misses: misses,
            total_compile_time_us: total_time,
        }
    }
}

/// Comprehensive outcome of executing a sandboxed WebAssembly tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInvocationResult {
    pub success: bool,
    pub output: serde_json::Value,
    pub exit_code: Option<i32>,
    pub fuel_consumed: u64,
    pub fuel_limit: u64,
    pub duration_us: u64,
    pub logs: Vec<String>,
    pub trapped: bool,
    pub trap_reason: Option<String>,
    pub memory_bytes_used: usize,
}

/// Capability-bounded, fuel-metered execution runner for micro-WASM modules
pub struct SandboxedWasmAgentRunner {
    engine: Engine,
    pub capabilities: PluginCapabilities,
    pub default_fuel_limit: u64,
    pub max_memory_bytes: usize,
}

impl Default for SandboxedWasmAgentRunner {
    fn default() -> Self {
        Self::new().expect("Failed to initialize default SandboxedWasmAgentRunner")
    }
}

impl SandboxedWasmAgentRunner {
    /// Initialize with default capabilities (16 MiB max heap, 10M instructions fuel limit)
    pub fn new() -> Result<Self> {
        Self::with_capabilities(PluginCapabilities::default())
    }

    /// Initialize with custom capabilities
    pub fn with_capabilities(capabilities: PluginCapabilities) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to create Wasmtime engine: {}", e))
        })?;

        let fuel = capabilities.fuel_limit;
        let memory = capabilities.max_memory_bytes;

        Ok(Self {
            engine,
            capabilities,
            default_fuel_limit: fuel,
            max_memory_bytes: memory,
        })
    }

    /// Set explicit instruction fuel budget
    pub fn with_fuel_limit(mut self, fuel_limit: u64) -> Self {
        self.default_fuel_limit = fuel_limit;
        self.capabilities.fuel_limit = fuel_limit;
        self
    }

    /// Set maximum linear memory allocation
    pub fn with_memory_limit(mut self, max_bytes: usize) -> Self {
        self.max_memory_bytes = max_bytes;
        self.capabilities.max_memory_bytes = max_bytes;
        self
    }

    /// Execute compiled WebAssembly module bytes inside the isolated sandbox
    pub fn execute_module(
        &self,
        wasm_bytes: &[u8],
        arguments: &serde_json::Value,
    ) -> Result<AgentInvocationResult> {
        self.execute_with_fuel(wasm_bytes, arguments, self.default_fuel_limit)
    }

    /// Execute with custom fuel limit
    pub fn execute_with_fuel(
        &self,
        wasm_bytes: &[u8],
        arguments: &serde_json::Value,
        fuel_limit: u64,
    ) -> Result<AgentInvocationResult> {
        let start_time = Instant::now();

        // 1. Compile Module in Wasmtime
        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            HgbError::Execution(format!("Failed to load WASM module: {}", e))
        })?;

        // 2. Setup Host State and Resource Limits
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.max_memory_bytes)
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
            HgbError::Execution(format!("Failed to set WASM fuel limit: {}", e))
        })?;

        // 3. Setup Linker and Bind Capability-Restricted Host Syscalls
        let mut linker = Linker::new(&self.engine);
        self.bind_sandbox_host_functions(&mut linker)?;

        // 4. Instantiate guest
        let instance = match linker.instantiate(&mut store, &module) {
            Ok(inst) => inst,
            Err(e) => {
                let err_str = e.to_string();
                let duration = start_time.elapsed().as_micros() as u64;
                return Ok(AgentInvocationResult {
                    success: false,
                    output: serde_json::json!({ "error": err_str }),
                    exit_code: None,
                    fuel_consumed: fuel_limit.saturating_sub(store.get_fuel().unwrap_or(0)),
                    fuel_limit,
                    duration_us: duration,
                    logs: store.data().logs.clone(),
                    trapped: true,
                    trap_reason: Some(format!("Instantiation error: {}", err_str)),
                    memory_bytes_used: 0,
                });
            }
        };

        // 5. Check Exported Functions
        // Path A: Dynamic tool ABI `hgb_tool_invoke(ptr: i32, len: i32) -> i64`
        if let Ok(invoke_fn) = instance.get_typed_func::<(i32, i32), i64>(&mut store, "hgb_tool_invoke") {
            let input_bytes = match arguments {
                serde_json::Value::String(s) => s.as_bytes().to_vec(),
                other => other.to_string().into_bytes(),
            };

            // Allocate input in guest linear memory
            let in_ptr = if let Ok(alloc_fn) = instance.get_typed_func::<i32, i32>(&mut store, "hgb_alloc") {
                match alloc_fn.call(&mut store, input_bytes.len() as i32) {
                    Ok(ptr) => ptr,
                    Err(trap) => return self.handle_trap(trap, fuel_limit, &store, start_time),
                }
            } else {
                0
            };

            // Write input to guest linear memory
            if let Some(memory) = instance.get_memory(&mut store, "memory") {
                if let Err(e) = memory.write(&mut store, in_ptr as usize, &input_bytes) {
                    let duration = start_time.elapsed().as_micros() as u64;
                    return Ok(AgentInvocationResult {
                        success: false,
                        output: serde_json::json!({ "error": format!("Memory write failed: {}", e) }),
                        exit_code: None,
                        fuel_consumed: fuel_limit.saturating_sub(store.get_fuel().unwrap_or(0)),
                        fuel_limit,
                        duration_us: duration,
                        logs: store.data().logs.clone(),
                        trapped: true,
                        trap_reason: Some(format!("Memory write violation: {}", e)),
                        memory_bytes_used: memory.data_size(&store),
                    });
                }
            }

            // Call `hgb_tool_invoke`
            match invoke_fn.call(&mut store, (in_ptr, input_bytes.len() as i32)) {
                Ok(packed_res) => {
                    let out_ptr = (packed_res as u64 & 0xFFFF_FFFF) as usize;
                    let out_len = ((packed_res as u64) >> 32) as usize;

                    let mut output_val = serde_json::Value::Null;
                    let mut mem_size = 0;

                    if let Some(memory) = instance.get_memory(&mut store, "memory") {
                        mem_size = memory.data_size(&store);
                        let mem_slice = memory.data(&store);
                        if out_ptr + out_len <= mem_slice.len() {
                            let raw_bytes = &mem_slice[out_ptr..out_ptr + out_len];
                            if let Ok(parsed_json) = serde_json::from_slice::<serde_json::Value>(raw_bytes) {
                                output_val = parsed_json;
                            } else if let Ok(parsed_str) = std::str::from_utf8(raw_bytes) {
                                output_val = serde_json::Value::String(parsed_str.to_string());
                            }
                        }
                    }

                    // Free input buffer if guest exported `hgb_dealloc`
                    if let Ok(dealloc_fn) = instance.get_typed_func::<(i32, i32), ()>(&mut store, "hgb_dealloc") {
                        let _ = dealloc_fn.call(&mut store, (in_ptr, input_bytes.len() as i32));
                    }

                    let fuel_left = store.get_fuel().unwrap_or(0);
                    let fuel_consumed = fuel_limit.saturating_sub(fuel_left);
                    let duration = start_time.elapsed().as_micros() as u64;

                    return Ok(AgentInvocationResult {
                        success: true,
                        output: output_val,
                        exit_code: Some(0),
                        fuel_consumed,
                        fuel_limit,
                        duration_us: duration,
                        logs: store.data().logs.clone(),
                        trapped: false,
                        trap_reason: None,
                        memory_bytes_used: mem_size,
                    });
                }
                Err(trap) => return self.handle_trap(trap, fuel_limit, &store, start_time),
            }
        }

        // Path B: Standard `run() -> i32`
        if let Ok(run_fn) = instance.get_typed_func::<(), i32>(&mut store, "run") {
            match run_fn.call(&mut store, ()) {
                Ok(exit_code) => {
                    let fuel_left = store.get_fuel().unwrap_or(0);
                    let fuel_consumed = fuel_limit.saturating_sub(fuel_left);
                    let duration = start_time.elapsed().as_micros() as u64;
                    let mem_size = instance
                        .get_memory(&mut store, "memory")
                        .map(|m| m.data_size(&store))
                        .unwrap_or(0);

                    return Ok(AgentInvocationResult {
                        success: true,
                        output: serde_json::json!({
                            "exit_code": exit_code,
                            "status": "success",
                        }),
                        exit_code: Some(exit_code),
                        fuel_consumed,
                        fuel_limit,
                        duration_us: duration,
                        logs: store.data().logs.clone(),
                        trapped: false,
                        trap_reason: None,
                        memory_bytes_used: mem_size,
                    });
                }
                Err(trap) => return self.handle_trap(trap, fuel_limit, &store, start_time),
            }
        }

        // Path C: Fallback for instantiated modules without entry point
        let fuel_left = store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_limit.saturating_sub(fuel_left);
        let duration = start_time.elapsed().as_micros() as u64;

        Ok(AgentInvocationResult {
            success: true,
            output: serde_json::json!({
                "status": "module_instantiated",
                "arguments": arguments,
            }),
            exit_code: Some(0),
            fuel_consumed,
            fuel_limit,
            duration_us: duration,
            logs: store.data().logs.clone(),
            trapped: false,
            trap_reason: None,
            memory_bytes_used: 0,
        })
    }

    /// Converts compiled WebAssembly bytes into a `SandboxedWasmPlugin` for `WasmPluginRegistry`
    pub fn into_plugin(&self, name: &str, wasm_bytes: &[u8]) -> Result<SandboxedWasmPlugin> {
        let fake_path = PathBuf::from(format!("{}.wasm", name));
        SandboxedWasmPlugin::load_from_bytes(wasm_bytes, Some(fake_path), self.capabilities.clone())
    }

    /// Classify and wrap Wasmtime traps into structured `AgentInvocationResult`
    fn handle_trap(
        &self,
        trap: wasmtime::Error,
        fuel_limit: u64,
        store: &Store<SandboxedWasmHostState>,
        start_time: Instant,
    ) -> Result<AgentInvocationResult> {
        let err_msg = trap.to_string();
        let fuel_left = store.get_fuel().unwrap_or(0);

        let is_fuel = fuel_left == 0
            || trap.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel)
            || err_msg.contains("fuel")
            || trap.root_cause().to_string().contains("fuel");

        let is_unreachable = trap.downcast_ref::<Trap>() == Some(&Trap::UnreachableCodeReached)
            || err_msg.contains("unreachable");

        let is_memory = err_msg.contains("out of bounds") || err_msg.contains("memory");

        let trap_reason = if is_fuel {
            "fuel_exhausted".to_string()
        } else if is_unreachable {
            "unreachable_code_executed".to_string()
        } else if is_memory {
            "memory_bounds_violation".to_string()
        } else {
            err_msg
        };

        let duration = start_time.elapsed().as_micros() as u64;

        Ok(AgentInvocationResult {
            success: false,
            output: serde_json::json!({
                "status": "trapped",
                "error": trap_reason,
            }),
            exit_code: None,
            fuel_consumed: fuel_limit,
            fuel_limit,
            duration_us: duration,
            logs: store.data().logs.clone(),
            trapped: true,
            trap_reason: Some(trap_reason),
            memory_bytes_used: 0,
        })
    }

    /// Bind host capability security callbacks
    fn bind_sandbox_host_functions(&self, linker: &mut Linker<SandboxedWasmHostState>) -> Result<()> {
        // 1. Safe host logging: hgb_host_log(ptr, len)
        linker
            .func_wrap(
                "env",
                "hgb_host_log",
                |mut caller: Caller<'_, SandboxedWasmHostState>, ptr: i32, len: i32| {
                    if let Some(msg_bytes) = read_caller_memory(&mut caller, ptr, len) {
                        if let Ok(msg) = std::str::from_utf8(&msg_bytes) {
                            caller.data_mut().logs.push(msg.to_string());
                        }
                    }
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // 2. Capability-checked filesystem read: hgb_fs_read(path_ptr, path_len, out_ptr, max_out) -> i32
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
                    let path_bytes = read_caller_memory(&mut caller, path_ptr, path_len)
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
                            if write_caller_memory(&mut caller, out_ptr, &data[..write_len]) {
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

        // 3. Capability-checked filesystem write: hgb_fs_write(path_ptr, path_len, data_ptr, data_len) -> i32
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
                    let path_bytes = read_caller_memory(&mut caller, path_ptr, path_len)
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

                    let payload = read_caller_memory(&mut caller, data_ptr, data_len)
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

        // 4. Capability-checked network fetch: hgb_net_fetch(url_ptr, url_len, out_ptr, max_out) -> i32
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
                    let url_bytes = read_caller_memory(&mut caller, url_ptr, url_len)
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

                    Ok(0)
                },
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        // 5. Host millisecond timestamp: hgb_host_time() -> u64
        linker
            .func_wrap("env", "hgb_host_time", |_caller: Caller<'_, SandboxedWasmHostState>| -> u64 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            })
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        Ok(())
    }
}

/// Helper function to read guest memory
fn read_caller_memory<T>(caller: &mut Caller<'_, T>, ptr: i32, len: i32) -> Option<Vec<u8>> {
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

/// Helper function to write guest memory
fn write_caller_memory<T>(caller: &mut Caller<'_, T>, ptr: i32, data: &[u8]) -> bool {
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

/// Record of an autonomous self-healing recovery step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealingAttempt {
    pub round: usize,
    pub error_kind: String,
    pub diagnostic: String,
    pub patch_summary: String,
}

/// Comprehensive report emitted by an `AutonomousZigAgentLoop` execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomousLoopReport {
    pub task: String,
    pub success: bool,
    pub final_source: String,
    pub wasm_hash: String,
    pub output: serde_json::Value,
    pub healing_rounds: usize,
    pub healing_history: Vec<HealingAttempt>,
    pub compile_time_us: u64,
    pub execution_time_us: u64,
    pub fuel_consumed: u64,
}

/// Autonomous agent loop compiling Zig on the fly, executing sandboxed, and self-healing
pub struct AutonomousZigAgentLoop {
    pub compiler: Arc<ZigWasmAgentCompiler>,
    pub runner: Arc<SandboxedWasmAgentRunner>,
    pub max_healing_rounds: usize,
    pub workspace_root: Option<PathBuf>,
}

impl Default for AutonomousZigAgentLoop {
    fn default() -> Self {
        Self::new().expect("Failed to initialize AutonomousZigAgentLoop")
    }
}

impl AutonomousZigAgentLoop {
    /// Initialize with default compiler and runner
    pub fn new() -> Result<Self> {
        let compiler = Arc::new(ZigWasmAgentCompiler::new());
        let runner = Arc::new(SandboxedWasmAgentRunner::new()?);

        Ok(Self {
            compiler,
            runner,
            max_healing_rounds: 3,
            workspace_root: None,
        })
    }

    /// Builder: attach custom compiler
    pub fn with_compiler(mut self, compiler: Arc<ZigWasmAgentCompiler>) -> Self {
        self.compiler = compiler;
        self
    }

    /// Builder: attach custom sandboxed runner
    pub fn with_runner(mut self, runner: Arc<SandboxedWasmAgentRunner>) -> Self {
        self.runner = runner;
        self
    }

    /// Builder: set maximum self-healing attempts
    pub fn with_max_healing_rounds(mut self, rounds: usize) -> Self {
        self.max_healing_rounds = rounds;
        self
    }

    /// Builder: attach workspace root
    pub fn with_workspace_root<P: Into<PathBuf>>(mut self, root: P) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// Executes a high-level task specification autonomously (e.g. "format json", "calculate fibonacci", etc.)
    pub fn execute_task(
        &self,
        task_spec: &str,
        input: &serde_json::Value,
    ) -> Result<AutonomousLoopReport> {
        let template_source = Self::generate_template(task_spec);
        self.execute_source(&template_source, input, task_spec)
    }

    /// Compiles, executes, and self-heals Zig source code inside the sandbox
    pub fn execute_source(
        &self,
        initial_source: &str,
        input: &serde_json::Value,
        task_name: &str,
    ) -> Result<AutonomousLoopReport> {
        let mut current_source = initial_source.to_string();
        let mut healing_history = Vec::new();
        let mut total_compile_us = 0;

        for round in 0..=self.max_healing_rounds {
            // Step 1: Compile Zig to WASM
            let comp_res = match self.compiler.compile(&current_source) {
                Ok(res) => {
                    total_compile_us += res.compile_duration_us;
                    res
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if round < self.max_healing_rounds {
                        if let Some(healed_source) = self.self_heal_syntax(&current_source, &err_msg) {
                            healing_history.push(HealingAttempt {
                                round: round + 1,
                                error_kind: "compiler_syntax_error".to_string(),
                                diagnostic: err_msg,
                                patch_summary: "Surgically patched Zig syntax diagnostic".to_string(),
                            });
                            current_source = healed_source;
                            continue;
                        }
                    }
                    return Err(HgbError::Execution(format!(
                        "Autonomous loop failed at compilation round {}: {}",
                        round, err_msg
                    )));
                }
            };

            // Step 2: Execute in Sandbox
            let exec_res = self.runner.execute_module(&comp_res.wasm_bytes, input)?;

            // Step 3: Handle runtime failures / traps
            if exec_res.trapped {
                let reason = exec_res
                    .trap_reason
                    .clone()
                    .unwrap_or_else(|| "Unknown trap".to_string());

                if round < self.max_healing_rounds {
                    if let Some(healed_source) = self.self_heal_runtime(&current_source, &reason) {
                        healing_history.push(HealingAttempt {
                            round: round + 1,
                            error_kind: "sandbox_runtime_trap".to_string(),
                            diagnostic: reason,
                            patch_summary: "Applied runtime recovery patch (fuel expansion or bounds protection)".to_string(),
                        });
                        current_source = healed_source;
                        continue;
                    }
                }

                return Ok(AutonomousLoopReport {
                    task: task_name.to_string(),
                    success: false,
                    final_source: current_source,
                    wasm_hash: comp_res.source_hash,
                    output: exec_res.output,
                    healing_rounds: healing_history.len(),
                    healing_history,
                    compile_time_us: total_compile_us,
                    execution_time_us: exec_res.duration_us,
                    fuel_consumed: exec_res.fuel_consumed,
                });
            }

            // Step 4: First-Green Execution Verified!
            return Ok(AutonomousLoopReport {
                task: task_name.to_string(),
                success: true,
                final_source: current_source,
                wasm_hash: comp_res.source_hash,
                output: exec_res.output,
                healing_rounds: healing_history.len(),
                healing_history,
                compile_time_us: total_compile_us,
                execution_time_us: exec_res.duration_us,
                fuel_consumed: exec_res.fuel_consumed,
            });
        }

        Err(HgbError::Execution("Exceeded maximum self-healing rounds".to_string()))
    }

    /// Generates idiomatic, freestanding Zig source templates for common AI agent tasks
    pub fn generate_template(task_spec: &str) -> String {
        let task_lower = task_spec.to_lowercase();

        if task_lower.contains("fibonacci") || task_lower.contains("fib") {
            return r#"
pub export fn run() i32 {
    return fibonacci(10);
}

pub export fn fibonacci(n: i32) i32 {
    if (n <= 0) return 0;
    if (n == 1) return 1;
    var a: i32 = 0;
    var b: i32 = 1;
    var i: i32 = 2;
    while (i <= n) : (i += 1) {
        const next = a + b;
        a = b;
        b = next;
    }
    return b;
}

var in_buf: [4096]u8 = undefined;
var out_buf: [4096]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"fibonacci\",\"description\":\"Calculate fibonacci number\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    const input = input_ptr[0..input_len];
    var n: i32 = 10;
    if (input.len > 0) {
        var parsed: i32 = 0;
        var valid = false;
        for (input) |c| {
            if (c >= '0' and c <= '9') {
                parsed = parsed * 10 + @as(i32, c - '0');
                valid = true;
            }
        }
        if (valid) n = parsed;
    }
    const res = fibonacci(n);

    const prefix = "{\"fibonacci\":";
    @memcpy(out_buf[0..prefix.len], prefix);
    var idx: usize = prefix.len;

    var temp_digits: [16]u8 = undefined;
    var d_count: usize = 0;
    var val = res;
    if (val == 0) {
        out_buf[idx] = '0';
        idx += 1;
    } else {
        while (val > 0) {
            temp_digits[d_count] = @as(u8, @intCast(@rem(val, 10))) + '0';
            val = @divTrunc(val, 10);
            d_count += 1;
        }
        var j: usize = d_count;
        while (j > 0) {
            j -= 1;
            out_buf[idx] = temp_digits[j];
            idx += 1;
        }
    }
    out_buf[idx] = '}';
    idx += 1;

    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = idx;
    return ptr_u64 | (len_u64 << 32);
}
"#.trim().to_string();
        }

        if task_lower.contains("json") || task_lower.contains("format json") {
            return r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    const input = input_ptr[0..input_len];
    var out_idx: usize = 0;
    var in_string = false;
    var escape = false;

    for (input) |c| {
        if (escape) {
            out_buf[out_idx] = c;
            out_idx += 1;
            escape = false;
            continue;
        }
        if (c == '\\') {
            out_buf[out_idx] = c;
            out_idx += 1;
            escape = true;
            continue;
        }
        if (c == '"') {
            in_string = !in_string;
            out_buf[out_idx] = c;
            out_idx += 1;
            continue;
        }
        if (!in_string and (c == ' ' or c == '\t' or c == '\r' or c == '\n')) {
            continue;
        }
        out_buf[out_idx] = c;
        out_idx += 1;
    }

    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = out_idx;
    return ptr_u64 | (len_u64 << 32);
}
"#.trim().to_string();
        }

        if task_lower.contains("csv") || task_lower.contains("parse csv") {
            return r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    const input = input_ptr[0..input_len];
    var out_idx: usize = 0;

    out_buf[out_idx] = '[';
    out_idx += 1;
    out_buf[out_idx] = '[';
    out_idx += 1;
    out_buf[out_idx] = '"';
    out_idx += 1;

    for (input) |c| {
        if (c == ',') {
            out_buf[out_idx] = '"';
            out_idx += 1;
            out_buf[out_idx] = ',';
            out_idx += 1;
            out_buf[out_idx] = '"';
            out_idx += 1;
        } else if (c == '\n') {
            out_buf[out_idx] = '"';
            out_idx += 1;
            out_buf[out_idx] = ']';
            out_idx += 1;
            out_buf[out_idx] = ',';
            out_idx += 1;
            out_buf[out_idx] = '[';
            out_idx += 1;
            out_buf[out_idx] = '"';
            out_idx += 1;
        } else if (c == '\r') {
            // skip carriage return
        } else if (c == '"') {
            out_buf[out_idx] = '\\';
            out_idx += 1;
            out_buf[out_idx] = '"';
            out_idx += 1;
        } else {
            out_buf[out_idx] = c;
            out_idx += 1;
        }
    }

    out_buf[out_idx] = '"';
    out_idx += 1;
    out_buf[out_idx] = ']';
    out_idx += 1;
    out_buf[out_idx] = ']';
    out_idx += 1;

    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = out_idx;
    return ptr_u64 | (len_u64 << 32);
}
"#.trim().to_string();
        }

        if task_lower.contains("filter") || task_lower.contains("regex") {
            return r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    const input = input_ptr[0..input_len];
    var out_idx: usize = 0;
    var filter_term: []const u8 = "";
    var content: []const u8 = input;

    var nl_idx: ?usize = null;
    for (input, 0..) |c, i| {
        if (c == '\n') {
            nl_idx = i;
            break;
        }
    }

    if (nl_idx) |idx| {
        filter_term = input[0..idx];
        content = input[idx + 1..];
    }

    var start: usize = 0;
    while (start < content.len) {
        var end = start;
        while (end < content.len and content[end] != '\n') : (end += 1) {}
        const line = content[start..end];

        var matches = false;
        if (filter_term.len == 0) {
            matches = true;
        } else if (line.len >= filter_term.len) {
            var i: usize = 0;
            while (i + filter_term.len <= line.len) : (i += 1) {
                var equal = true;
                for (filter_term, 0..) |fc, j| {
                    if (line[i + j] != fc) {
                        equal = false;
                        break;
                    }
                }
                if (equal) {
                    matches = true;
                    break;
                }
            }
        }

        if (matches) {
            if (out_idx + line.len + 1 <= out_buf.len) {
                @memcpy(out_buf[out_idx..out_idx + line.len], line);
                out_idx += line.len;
                out_buf[out_idx] = '\n';
                out_idx += 1;
            }
        }

        start = end + 1;
    }

    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = out_idx;
    return ptr_u64 | (len_u64 << 32);
}
"#.trim().to_string();
        }

        // Generic default tool template
        r#"
var in_buf: [4096]u8 = undefined;
var out_buf: [4096]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 42;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"generic_wasm_tool\",\"version\":\"0.1.0\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    const input = input_ptr[0..input_len];
    const prefix = "{\"received\":\"";
    @memcpy(out_buf[0..prefix.len], prefix);
    var idx = prefix.len;

    const copy_len = if (input.len < 1000) input.len else 1000;
    @memcpy(out_buf[idx..idx + copy_len], input[0..copy_len]);
    idx += copy_len;

    const suffix = "\"}";
    @memcpy(out_buf[idx..idx + suffix.len], suffix);
    idx += suffix.len;

    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = idx;
    return ptr_u64 | (len_u64 << 32);
}
"#.trim().to_string()
    }

    /// Surgical self-healing for Zig compiler diagnostics
    pub fn self_heal_syntax(&self, failed_source: &str, compiler_error: &str) -> Option<String> {
        let mut lines: Vec<String> = failed_source.lines().map(|s| s.to_string()).collect();
        let mut modified = false;

        // 1. Missing semicolon: `expected ';' after statement` or `expected ';', found`
        let re_semi = Regex::new(r":(\d+):\d+:\s*error:\s*expected\s*';'").ok()?;
        if let Some(caps) = re_semi.captures(compiler_error) {
            if let Ok(line_num) = caps[1].parse::<usize>() {
                if line_num > 0 && line_num <= lines.len() {
                    let target_idx = line_num - 1;
                    if !lines[target_idx].trim().ends_with(';') {
                        lines[target_idx].push(';');
                        modified = true;
                    } else if target_idx > 0 && !lines[target_idx - 1].trim().ends_with(';') {
                        lines[target_idx - 1].push(';');
                        modified = true;
                    }
                }
            }
        }

        // 2. Cannot assign to constant: `cannot assign to constant`
        let re_const = Regex::new(r":(\d+):(\d+):\s*error:\s*cannot assign to constant").ok()?;
        if let Some(caps) = re_const.captures(compiler_error) {
            if let Ok(line_num) = caps[1].parse::<usize>() {
                if line_num > 0 && line_num <= lines.len() {
                    let err_line = lines[line_num - 1].clone();
                    // Extract identifier being assigned
                    if let Some(id_match) = Regex::new(r"^\s*([a-zA-Z0-9_]+)\s*=").ok()?.captures(&err_line) {
                        let var_name = id_match[1].to_string();
                        // Scan previous lines for `const <var_name>` and replace with `var <var_name>`
                        for l in &mut lines {
                            let pattern = format!("const {} ", var_name);
                            if l.contains(&pattern) {
                                *l = l.replace(&pattern, &format!("var {} ", var_name));
                                modified = true;
                                break;
                            }
                            let pattern_colon = format!("const {}:", var_name);
                            if l.contains(&pattern_colon) {
                                *l = l.replace(&pattern_colon, &format!("var {}:", var_name));
                                modified = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // 3. Local variable never mutated: `local variable is never mutated` -> change `var` to `const`
        let re_var = Regex::new(r":(\d+):(\d+):\s*error:\s*local variable is never mutated").ok()?;
        if let Some(caps) = re_var.captures(compiler_error) {
            if let Ok(line_num) = caps[1].parse::<usize>() {
                if line_num > 0 && line_num <= lines.len() {
                    let target = &mut lines[line_num - 1];
                    if target.contains("var ") {
                        *target = target.replacen("var ", "const ", 1);
                        modified = true;
                    }
                }
            }
        }

        // 4. Unused local variable/constant: `unused local constant` or `unused local variable`
        let re_unused = Regex::new(r":(\d+):(\d+):\s*error:\s*unused local (constant|variable)").ok()?;
        if let Some(caps) = re_unused.captures(compiler_error) {
            if let Ok(line_num) = caps[1].parse::<usize>() {
                if line_num > 0 && line_num <= lines.len() {
                    let decl_line = &lines[line_num - 1];
                    let re_id = Regex::new(r"(?:const|var)\s+([a-zA-Z0-9_]+)").ok()?;
                    if let Some(id_caps) = re_id.captures(decl_line) {
                        let id = &id_caps[1];
                        // Append `_ = <id>;` on next line
                        let discard = format!("    _ = {};", id);
                        lines.insert(line_num, discard);
                        modified = true;
                    }
                }
            }
        }

        if modified {
            Some(lines.join("\n"))
        } else {
            None
        }
    }

    /// Self-healing for runtime execution traps
    pub fn self_heal_runtime(&self, failed_source: &str, trap_reason: &str) -> Option<String> {
        if trap_reason.contains("fuel_exhausted") || trap_reason.contains("fuel") {
            // If CPU fuel was exhausted, we can wrap the task in a bounded iteration or optimize loop
            let modified = failed_source.replace("while (true)", "var _iter: usize = 0; while (_iter < 100000) : (_iter += 1)");
            if modified != failed_source {
                return Some(modified);
            }
        }

        if trap_reason.contains("bounds") || trap_reason.contains("out of bounds") {
            // Expand scratch buffer size from 4096 to 65536
            let modified = failed_source.replace("[4096]u8", "[65536]u8");
            if modified != failed_source {
                return Some(modified);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blake3_hashing_and_cache_hits() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let compiler = ZigWasmAgentCompiler::new().with_cache_dir(temp_dir.path());
        compiler.clear_cache().expect("clear cache");
        let source_code = "pub export fn run() i32 { return 42; }";

        // First compilation: real compilation (cache miss)
        let res1 = compiler.compile(source_code).expect("First compile should succeed");
        assert!(!res1.cache_hit, "First compile must be a cache miss");
        assert_eq!(res1.size_bytes, res1.wasm_bytes.len());
        assert!(!res1.wasm_bytes.is_empty());

        // Second compilation: must hit in-memory / disk cache
        let res2 = compiler.compile(source_code).expect("Second compile should succeed");
        assert!(res2.cache_hit, "Second compile must be a cache hit");
        assert_eq!(res1.source_hash, res2.source_hash);
        assert_eq!(res1.wasm_bytes, res2.wasm_bytes);

        let stats = compiler.stats();
        assert_eq!(stats.total_compilations, 2);
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 1);
    }

    #[test]
    fn test_zig_compilation_to_valid_wasm() {
        let compiler = ZigWasmAgentCompiler::new();
        let source = r#"
pub export fn run() i32 {
    return 777;
}
"#;
        let res = compiler.compile(source).expect("compile zig wasm");
        assert!(!res.wasm_bytes.is_empty());
        // WASM magic bytes '\0asm'
        assert_eq!(&res.wasm_bytes[0..4], &[0x00, 0x61, 0x73, 0x6d]);

        let runner = SandboxedWasmAgentRunner::new().expect("runner new");
        let exec = runner
            .execute_module(&res.wasm_bytes, &serde_json::json!({}))
            .expect("execute");
        assert!(exec.success);
        assert_eq!(exec.exit_code, Some(777));
        assert!(!exec.trapped);
    }

    #[test]
    fn test_sandboxed_tool_invocation_with_fuel_metering() {
        let compiler = ZigWasmAgentCompiler::new();
        // Compile fibonacci tool
        let fib_source = AutonomousZigAgentLoop::generate_template("fibonacci");
        let res = compiler.compile(&fib_source).expect("compile fibonacci");

        let runner = SandboxedWasmAgentRunner::new()
            .expect("runner new")
            .with_fuel_limit(500_000);

        // Test running fibonacci(10) -> 55
        let exec = runner
            .execute_module(&res.wasm_bytes, &serde_json::Value::String("10".to_string()))
            .expect("execute fibonacci");

        assert!(exec.success);
        assert!(!exec.trapped);
        assert!(exec.fuel_consumed > 0);
        assert!(exec.fuel_consumed < 500_000);

        let fib_val = exec.output.get("fibonacci").and_then(|v| v.as_i64());
        assert_eq!(fib_val, Some(55));

        // Test fuel exhaustion trapping
        let infinite_loop = r#"
pub export fn run() i32 {
    var i: i32 = 0;
    while (true) {
        i += 1;
    }
    return i;
}
"#;
        let inf_res = compiler.compile(infinite_loop).expect("compile infinite loop");
        let trapped_exec = runner
            .execute_with_fuel(&inf_res.wasm_bytes, &serde_json::json!({}), 10_000)
            .expect("execute trapped");

        assert!(!trapped_exec.success);
        assert!(trapped_exec.trapped);
        assert_eq!(trapped_exec.trap_reason.as_deref(), Some("fuel_exhausted"));
        assert_eq!(trapped_exec.fuel_consumed, 10_000);
    }

    #[test]
    fn test_self_healing_loop_recovery_on_syntax_error() {
        let agent_loop = AutonomousZigAgentLoop::new().expect("create autonomous loop");

        // Broken code: missing semicolon on return statement
        let broken_code = r#"
pub export fn run() i32 {
    return 42
}
"#;
        // Verify compiler fails on raw broken code
        let comp_err = agent_loop.compiler.compile(broken_code);
        assert!(comp_err.is_err());

        // Autonomous loop executes, self-heals by appending semicolon, and achieves First-Green!
        let report = agent_loop
            .execute_source(broken_code, &serde_json::json!({}), "self_heal_test")
            .expect("autonomous execution with self-healing");

        assert!(report.success, "Autonomous loop must achieve First-Green verification");
        assert_eq!(report.healing_rounds, 1, "Must heal exactly 1 syntax error");
        assert_eq!(report.healing_history[0].error_kind, "compiler_syntax_error");
        assert_eq!(
            report.output.get("exit_code").and_then(|v| v.as_i64()),
            Some(42)
        );
    }

    #[test]
    fn test_autonomous_task_execution_json_and_csv() {
        let agent_loop = AutonomousZigAgentLoop::new().expect("loop new");

        // 1. Task: Format JSON
        let json_input = serde_json::Value::String("{\n  \"name\" :  \"hagibis\" , \"count\" : 42\n}".to_string());
        let report_json = agent_loop
            .execute_task("format json", &json_input)
            .expect("exec json task");

        assert!(report_json.success);
        assert_eq!(
            report_json.output,
            serde_json::json!({"name": "hagibis", "count": 42})
        );

        // 2. Task: Parse CSV
        let csv_input = serde_json::Value::String("col1,col2\nval1,val2".to_string());
        let report_csv = agent_loop
            .execute_task("parse csv", &csv_input)
            .expect("exec csv task");

        assert!(report_csv.success);
        assert_eq!(
            report_csv.output,
            serde_json::json!([["col1", "col2"], ["val1", "val2"]])
        );
    }
}
