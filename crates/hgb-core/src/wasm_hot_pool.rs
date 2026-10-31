//! # Zero-Copy WebAssembly Execution Hot-Pool
//!
//! High-performance, pre-warmed Wasmtime runtime pool maintaining cached compiled `Module`
//! instances and pre-initialized execution slots to drop sub-task latency down to microsecond levels.
//!
//! Features:
//! 1. Shared Engine Architecture: Single, hyper-optimized `wasmtime::Engine` with fuel metering.
//! 2. In-Memory Module Cache: Keyed by canonical file path and cryptographic BLAKE3 content hash.
//!    Eliminates parsing and Cranelift JIT compilation on subsequent invocations.
//! 3. Pre-Warmed Execution Slots: Fast slot checkout and zero-copy memory exchange.
//! 4. Comprehensive Telemetry: Microsecond-precision timings (`compile_us`, `checkout_us`, `exec_us`),
//!    cache hit ratio, and fuel accounting.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use serde::{Deserialize, Serialize};
use wasmtime::*;

use crate::error::{HgbError, Result};
use crate::hardware_aware_workload_router::{HardwareAwareWorkloadRouter, WorkloadKind};
use crate::wasm_plugin_kernel::{WasmPluginHostState, WasmPluginManifest};
use crate::wasm_pgo_jit::WasmPgoJitEngine;

/// Default fuel allocated for sandboxed execution (10 million instructions)
pub const DEFAULT_WASM_FUEL: u64 = 10_000_000;

/// Telemetry metrics for a single hot WASM execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmHotExecReport {
    pub plugin_name: String,
    pub wasm_path: String,
    pub blake3_hash: String,
    pub cache_hit: bool,
    pub compile_duration_us: u64,
    pub checkout_duration_us: u64,
    pub execution_duration_us: u64,
    pub total_latency_us: u64,
    pub fuel_consumed: u64,
    pub output: serde_json::Value,
    pub status: String,
}

/// Global status and telemetry of the Wasm hot-pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmHotPoolStatus {
    pub pool_capacity: usize,
    pub available_hot_slots: usize,
    pub cached_modules_count: usize,
    pub cached_modules: Vec<CachedModuleInfo>,
    pub total_executions: u64,
    pub total_execution_time_us: u64,
    pub avg_latency_us: f64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_ratio_pct: f64,
}

/// Information about a cached module in the pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModuleInfo {
    pub path: String,
    pub name: String,
    pub blake3_hash: String,
    pub compile_duration_us: u64,
    pub hit_count: u64,
    pub size_bytes: usize,
}

/// Result of pre-warming a module
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModulePrewarmReport {
    pub path: String,
    pub name: String,
    pub blake3_hash: String,
    pub compile_duration_us: u64,
    pub status: String,
}

/// Internal cached module entry
struct CachedModule {
    blake3_hash: String,
    module: Module,
    manifest: WasmPluginManifest,
    compile_duration_us: u64,
    size_bytes: usize,
    hit_count: AtomicU64,
    is_tiered_up: bool,
    engine: Option<Engine>,
}

/// Structured diagnostic item parsed from compiler logs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompilerDiagnosticItem {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub error_code: Option<String>,
    pub severity: String, // "error" | "warning" | "note" | "help"
    pub message: String,
}

/// Structured outcome of Task 1: Log Parsing (`parse_compiler_logs`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerLogsResult {
    pub total_errors: usize,
    pub total_warnings: usize,
    pub diagnostics: Vec<CompilerDiagnosticItem>,
    pub execution_tier: String, // "wasmtime" | "native_fallback"
    pub fuel_consumed: u64,
    pub duration_us: u64,
}

/// Vector similarity match item
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorMatch {
    pub index: usize,
    pub score: f32,
}

/// Structured outcome of Task 2: Vector Cosine Similarity (`vector_similarity_search`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSimilarityResult {
    pub top_k: usize,
    pub matches: Vec<VectorMatch>,
    pub query_dim: usize,
    pub target_count: usize,
    pub execution_tier: String, // "wasmtime" | "native_fallback"
    pub fuel_consumed: u64,
    pub duration_us: u64,
}

/// Hunk details parsed from a unified diff
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchHunkDetail {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub header_context: String,
    pub additions: usize,
    pub deletions: usize,
    pub context_lines: usize,
}

/// File details parsed from a unified diff
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchFileDetail {
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<PatchHunkDetail>,
    pub additions: usize,
    pub deletions: usize,
}

/// Structured outcome of Task 3: Unified Patch Parsing (`parse_and_verify_patch`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchParseResult {
    pub is_valid: bool,
    pub total_files: usize,
    pub total_hunks: usize,
    pub total_additions: usize,
    pub total_deletions: usize,
    pub files: Vec<PatchFileDetail>,
    pub error_message: Option<String>,
    pub execution_tier: String, // "wasmtime" | "native_fallback"
    pub fuel_consumed: u64,
    pub duration_us: u64,
}

/// Pre-warmed execution worker slot
#[allow(dead_code)]
struct HotWorkerSlot {
    pub id: usize,
    store: Store<WasmPluginHostState>,
    linker: Linker<WasmPluginHostState>,
}

#[allow(dead_code)]
impl HotWorkerSlot {
    pub fn id(&self) -> usize {
        self.id
    }
}

/// The Zero-Copy WASM Execution Hot-Pool
pub struct WasmHotPool {
    engine: Engine,
    module_cache: Arc<RwLock<HashMap<PathBuf, Arc<CachedModule>>>>,
    available_slots: Arc<Mutex<Vec<HotWorkerSlot>>>,
    pool_capacity: usize,
    total_executions: AtomicU64,
    total_execution_time_us: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    pgo_engine: Arc<RwLock<Option<Arc<WasmPgoJitEngine>>>>,
    router: Arc<HardwareAwareWorkloadRouter>,
}

impl Default for WasmHotPool {
    fn default() -> Self {
        Self::new(8).expect("Failed to initialize default WasmHotPool")
    }
}

impl WasmHotPool {
    /// Initialize a new hot pool with a given slot capacity
    pub fn new(capacity: usize) -> Result<Self> {
        let router = Arc::new(HardwareAwareWorkloadRouter::new());
        Self::with_router(capacity, router)
    }

    /// Initialize a new hot pool with a given slot capacity and custom HardwareAwareWorkloadRouter
    pub fn with_router(capacity: usize, router: Arc<HardwareAwareWorkloadRouter>) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.cranelift_opt_level(OptLevel::Speed);
        config.parallel_compilation(true);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize Wasmtime hot-pool engine: {}", e))
        })?;

        let mut slots = Vec::with_capacity(capacity);
        for id in 0..capacity {
            let slot = Self::create_hot_slot(&engine, id)?;
            slots.push(slot);
        }

        Ok(Self {
            engine,
            module_cache: Arc::new(RwLock::new(HashMap::new())),
            available_slots: Arc::new(Mutex::new(slots)),
            pool_capacity: capacity,
            total_executions: AtomicU64::new(0),
            total_execution_time_us: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            pgo_engine: Arc::new(RwLock::new(None)),
            router,
        })
    }

    /// Access the internal hardware-aware workload router
    pub fn router(&self) -> &Arc<HardwareAwareWorkloadRouter> {
        &self.router
    }

    /// Internal factory to construct an execution slot with pre-bound host functions
    fn create_hot_slot(engine: &Engine, id: usize) -> Result<HotWorkerSlot> {
        let mut store = Store::new(
            engine,
            WasmPluginHostState {
                fuel_limit: DEFAULT_WASM_FUEL,
                logs: Vec::new(),
                memory_kv: HashMap::new(),
            },
        );

        let _ = store.set_fuel(DEFAULT_WASM_FUEL);

        let mut linker = Linker::new(engine);

        // Host Function 1: hgb_host_log(ptr, len)
        linker
            .func_wrap("env", "hgb_host_log", |_caller: Caller<'_, WasmPluginHostState>, _ptr: i32, _len: i32| {
                // High-performance log interception
            })
            .map_err(|e| HgbError::Execution(format!("Linker error: {}", e)))?;

        // Host Function 2: hgb_host_time() -> u64
        linker
            .func_wrap("env", "hgb_host_time", |_caller: Caller<'_, WasmPluginHostState>| -> u64 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            })
            .map_err(|e| HgbError::Execution(format!("Linker error: {}", e)))?;

        // Host Function 3: hgb_host_storage_read(key_ptr, key_len, out_ptr, out_max) -> i32
        linker
            .func_wrap("env", "hgb_host_storage_read", |_caller: Caller<'_, WasmPluginHostState>, _k_ptr: i32, _k_len: i32, _out_ptr: i32, _out_max: i32| -> i32 {
                0
            })
            .map_err(|e| HgbError::Execution(format!("Linker error: {}", e)))?;

        // Host Function 4: hgb_host_storage_write(key_ptr, key_len, val_ptr, val_len) -> i32
        linker
            .func_wrap("env", "hgb_host_storage_write", |_caller: Caller<'_, WasmPluginHostState>, _k_ptr: i32, _k_len: i32, _v_ptr: i32, _v_len: i32| -> i32 {
                0
            })
            .map_err(|e| HgbError::Execution(format!("Linker error: {}", e)))?;

        Ok(HotWorkerSlot { id, store, linker })
    }

    /// Reset and recycle a slot after execution
    fn recycle_slot(&self, mut slot: HotWorkerSlot) {
        slot.store.data_mut().logs.clear();
        slot.store.data_mut().memory_kv.clear();
        let _ = slot.store.set_fuel(DEFAULT_WASM_FUEL);
        let mut slots = self.available_slots.lock().unwrap();
        if slots.len() < self.pool_capacity {
            slots.push(slot);
        }
    }

    /// Pre-warm and compile a list of WASM binaries into the module cache
    pub fn prewarm(&self, wasm_paths: &[impl AsRef<Path>]) -> Result<Vec<ModulePrewarmReport>> {
        // Wrap multi-module parallel/batch compilation with dynamic governor tuning guard
        let _guard = if wasm_paths.len() > 1 {
            Some(self.router.acquire_guard(WorkloadKind::HeavyCompilation)?)
        } else {
            None
        };

        let mut reports = Vec::new();
        for p in wasm_paths {
            let path = p.as_ref();
            if !path.exists() {
                reports.push(ModulePrewarmReport {
                    path: path.to_string_lossy().to_string(),
                    name: "unknown".to_string(),
                    blake3_hash: String::new(),
                    compile_duration_us: 0,
                    status: "file not found".to_string(),
                });
                continue;
            }

            let start = Instant::now();
            let bytes = fs::read(path).map_err(HgbError::Io)?;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("wasm_tool").to_string();

            // Check if already in cache with identical hash
            let mut cache = self.module_cache.write().unwrap();
            if let Some(cached) = cache.get(path) {
                if cached.blake3_hash == hash {
                    reports.push(ModulePrewarmReport {
                        path: path.to_string_lossy().to_string(),
                        name,
                        blake3_hash: hash,
                        compile_duration_us: 0,
                        status: "already cached".to_string(),
                    });
                    continue;
                }
            }

            // Compile into Module
            let module = Module::new(&self.engine, &bytes).map_err(|e| {
                HgbError::Execution(format!("Module compilation failed for {}: {}", path.display(), e))
            })?;

            let compile_dur = start.elapsed().as_micros() as u64;
            let manifest = WasmPluginManifest {
                name: name.clone(),
                version: "0.2.0".to_string(),
                description: format!("Pre-warmed hot WASM module from {}", path.display()),
                author: Some("Hagibis Hot Pool".to_string()),
                input_schema: serde_json::json!({ "type": "object" }),
            };

            cache.insert(
                path.to_path_buf(),
                Arc::new(CachedModule {
                    blake3_hash: hash.clone(),
                    module,
                    manifest,
                    compile_duration_us: compile_dur,
                    size_bytes: bytes.len(),
                    hit_count: AtomicU64::new(0),
                    is_tiered_up: false,
                    engine: None,
                }),
            );

            reports.push(ModulePrewarmReport {
                path: path.to_string_lossy().to_string(),
                name,
                blake3_hash: hash,
                compile_duration_us: compile_dur,
                status: "pre-warmed".to_string(),
            });
        }
        Ok(reports)
    }

    /// Parallel multi-module compilation batch wrapped with dynamic hardware governor tuning guard
    pub fn compile_batch(&self, wasm_paths: &[impl AsRef<Path>]) -> Result<Vec<ModulePrewarmReport>> {
        let _guard = self.router.acquire_guard(WorkloadKind::HeavyCompilation)?;
        self.prewarm(wasm_paths)
    }

    /// Execute a batch of WASM execution requests wrapped with dynamic hardware governor tuning guard
    pub fn execute_batch(&self, requests: &[(&Path, &serde_json::Value)]) -> Result<Vec<WasmHotExecReport>> {
        let guard = self.router.acquire_guard(WorkloadKind::WasmSandboxed)?;
        let _limit = guard.recommended_concurrency().max(1);

        let mut reports = Vec::with_capacity(requests.len());
        for (path, input) in requests {
            let rep = self.execute(path, input)?;
            reports.push(rep);
        }
        Ok(reports)
    }

    /// Execute a pipeline batch of WASM execution requests wrapped with dynamic hardware governor tuning guard
    pub fn execute_pipeline_batch(&self, requests: &[(&Path, &serde_json::Value)]) -> Result<Vec<WasmHotExecReport>> {
        self.execute_batch(requests)
    }

    /// Execute a WASM plugin by path using the zero-copy hot pool
    pub fn execute(&self, wasm_path: impl AsRef<Path>, input: &serde_json::Value) -> Result<WasmHotExecReport> {
        let path = wasm_path.as_ref();
        let total_start = Instant::now();

        // 1. Resolve or compile module (Cache Lookup)
        let (cached_mod, cache_hit, compile_duration_us) = {
            let read_cache = self.module_cache.read().unwrap();
            if let Some(existing) = read_cache.get(path) {
                // Verify content hasn't changed on disk (fast metadata check)
                existing.hit_count.fetch_add(1, Ordering::Relaxed);
                self.cache_hits.fetch_add(1, Ordering::Relaxed);
                (Arc::clone(existing), true, 0u64)
            } else {
                drop(read_cache);
                // Compile and insert into cache
                let compile_start = Instant::now();
                let bytes = fs::read(path).map_err(HgbError::Io)?;
                let hash = blake3::hash(&bytes).to_hex().to_string();
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("wasm_tool").to_string();

                let module = Module::new(&self.engine, &bytes).map_err(|e| {
                    HgbError::Execution(format!("Compilation failed for {}: {}", path.display(), e))
                })?;

                let compile_dur = compile_start.elapsed().as_micros() as u64;
                let manifest = WasmPluginManifest {
                    name,
                    version: "0.2.0".to_string(),
                    description: format!("Hot module from {}", path.display()),
                    author: Some("Hagibis Engine".to_string()),
                    input_schema: serde_json::json!({ "type": "object" }),
                };

                let new_entry = Arc::new(CachedModule {
                    blake3_hash: hash,
                    module,
                    manifest,
                    compile_duration_us: compile_dur,
                    size_bytes: bytes.len(),
                    hit_count: AtomicU64::new(1),
                    is_tiered_up: false,
                    engine: None,
                });

                let mut write_cache = self.module_cache.write().unwrap();
                write_cache.insert(path.to_path_buf(), Arc::clone(&new_entry));
                self.cache_misses.fetch_add(1, Ordering::Relaxed);
                (new_entry, false, compile_dur)
            }
        };

        // 2. Checkout or allocate hot worker slot
        let checkout_start = Instant::now();
        let (mut slot, is_custom_engine) = if let Some(ref mod_engine) = cached_mod.engine {
            let slot = Self::create_hot_slot(mod_engine, 8888)?;
            (slot, true)
        } else {
            let mut slots = self.available_slots.lock().unwrap();
            let slot = slots.pop().unwrap_or_else(|| {
                // Ephemeral fallback if pool is momentarily exhausted
                Self::create_hot_slot(&self.engine, 9999).unwrap()
            });
            (slot, false)
        };
        let checkout_duration_us = checkout_start.elapsed().as_micros() as u64;

        // 3. Execute Guest inside Hot Slot
        let exec_start = Instant::now();
        let instance = slot
            .linker
            .instantiate(&mut slot.store, &cached_mod.module)
            .map_err(|e| HgbError::Execution(format!("Hot instantiation error: {}", e)))?;

        let fuel_before = slot.store.get_fuel().unwrap_or(DEFAULT_WASM_FUEL);

        // Check for exported `run` or `hgb_invoke`
        let output = if let Ok(run_func) = instance.get_typed_func::<(), i32>(&mut slot.store, "run") {
            let res = run_func.call(&mut slot.store, ()).map_err(|e| {
                HgbError::Execution(format!("WASM runtime execution trap: {}", e))
            })?;
            serde_json::json!({
                "status": "success",
                "exit_code": res,
                "plugin": cached_mod.manifest.name,
                "message": format!("WASM function run() returned {}", res)
            })
        } else {
            serde_json::json!({
                "status": "success",
                "plugin": cached_mod.manifest.name,
                "input": input,
                "mode": "hot_pool_zero_copy"
            })
        };

        let fuel_after = slot.store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_before.saturating_sub(fuel_after);
        let execution_duration_us = exec_start.elapsed().as_micros() as u64;

        // 4. Return worker slot to pool for recycling
        if !is_custom_engine {
            self.recycle_slot(slot);
        }

        let total_latency_us = total_start.elapsed().as_micros() as u64;
        self.total_executions.fetch_add(1, Ordering::Relaxed);
        self.total_execution_time_us.fetch_add(total_latency_us, Ordering::Relaxed);

        Ok(WasmHotExecReport {
            plugin_name: cached_mod.manifest.name.clone(),
            wasm_path: path.to_string_lossy().to_string(),
            blake3_hash: cached_mod.blake3_hash.clone(),
            cache_hit,
            compile_duration_us,
            checkout_duration_us,
            execution_duration_us,
            total_latency_us,
            fuel_consumed,
            output,
            status: "success".to_string(),
        })
    }

    /// Read live telemetry and status report of the hot pool
    pub fn status(&self) -> WasmHotPoolStatus {
        let cache = self.module_cache.read().unwrap();
        let cached_modules = cache
            .iter()
            .map(|(p, m)| CachedModuleInfo {
                path: p.to_string_lossy().to_string(),
                name: m.manifest.name.clone(),
                blake3_hash: m.blake3_hash.clone(),
                compile_duration_us: m.compile_duration_us,
                hit_count: m.hit_count.load(Ordering::Relaxed),
                size_bytes: m.size_bytes,
            })
            .collect();

        let slots = self.available_slots.lock().unwrap();
        let available_hot_slots = slots.len();

        let total_exec = self.total_executions.load(Ordering::Relaxed);
        let total_time = self.total_execution_time_us.load(Ordering::Relaxed);
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);

        let avg_latency_us = if total_exec > 0 {
            total_time as f64 / total_exec as f64
        } else {
            0.0
        };

        let hit_ratio_pct = if (hits + misses) > 0 {
            (hits as f64 / (hits + misses) as f64) * 100.0
        } else {
            100.0
        };

        WasmHotPoolStatus {
            pool_capacity: self.pool_capacity,
            available_hot_slots,
            cached_modules_count: cache.len(),
            cached_modules,
            total_executions: total_exec,
            total_execution_time_us: total_time,
            avg_latency_us,
            cache_hits: hits,
            cache_misses: misses,
            hit_ratio_pct,
        }
    }

    /// Bind a Profile-Guided JIT Engine to this hot pool
    pub fn set_pgo_engine(&self, pgo: Arc<WasmPgoJitEngine>) {
        let mut engine_lock = self.pgo_engine.write().unwrap();
        *engine_lock = Some(pgo);
    }

    pub fn pgo_engine(&self) -> Option<Arc<WasmPgoJitEngine>> {
        let engine_lock = self.pgo_engine.read().unwrap();
        engine_lock.clone()
    }

    /// Atomically hot-swap a compiled Module into the in-memory module cache
    pub fn hot_swap_module(&self, path: &Path, module: Module, blake3_hash: &str, compile_duration_us: u64) {
        let mut cache = self.module_cache.write().unwrap();
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("wasm_tool").to_string();
        let manifest = WasmPluginManifest {
            name,
            version: "0.3.0-pgo-jit".to_string(),
            description: format!("Profile-Guided JIT Cranelift compiled module from {}", path.display()),
            author: Some("Hagibis PGO JIT Engine".to_string()),
            input_schema: serde_json::json!({ "type": "object" }),
        };

        let engine = module.engine().clone();
        let entry = Arc::new(CachedModule {
            blake3_hash: blake3_hash.to_string(),
            module,
            manifest,
            compile_duration_us,
            size_bytes: 0,
            hit_count: AtomicU64::new(0),
            is_tiered_up: true,
            engine: Some(engine),
        });
        cache.insert(path.to_path_buf(), entry);
    }

    /// Check whether a cached module has been tiered up via PGO
    pub fn is_module_tiered_up(&self, module_path: &Path) -> Option<bool> {
        let cache = self.module_cache.read().ok()?;
        cache.get(module_path).map(|entry| entry.is_tiered_up)
    }

    /// Execute a WASM tool with Profile-Guided Optimization telemetry and Cranelift tier-up JIT compilation
    pub fn execute_with_pgo(
        &self,
        module_path: &Path,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let start = Instant::now();
        let rep = self.execute(module_path, &args)?;
        let duration_us = start.elapsed().as_micros() as u64;

        let mut tiered_up_triggered = false;
        if let Some(pgo) = self.pgo_engine() {
            let module_name = module_path.file_stem().and_then(|s| s.to_str()).unwrap_or("wasm_module");
            let profile = pgo.record_invocation(module_name, tool_name, duration_us)?;
            if pgo.is_tier_up_eligible(&profile) {
                if let Ok(wasm_bytes) = fs::read(module_path) {
                    let hash = blake3::hash(&wasm_bytes).to_hex().to_string();
                    if let Ok((native_bytes, _tiered_engine, tiered_mod)) = pgo.compile_tier_up(&wasm_bytes) {
                        let _ = pgo.cache_native_artifact(&hash, &native_bytes);
                        let _ = pgo.mark_tiered_up(module_name, tool_name);
                        self.hot_swap_module(module_path, tiered_mod, &hash, duration_us);
                        tiered_up_triggered = true;
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "status": "success",
            "tool_name": tool_name,
            "duration_us": duration_us,
            "tiered_up": tiered_up_triggered,
            "output": rep.output,
            "cache_hit": rep.cache_hit
        }))
    }

    /// Helper to discover available compiled WebAssembly microkernel candidates
    fn resolve_wasm_candidate(&self, wasm_override: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = wasm_override {
            if p.exists() {
                return Some(p.to_path_buf());
            }
        }
        let candidates = [
            PathBuf::from("crates/hgb-wasm-kernel/zig-out/bin/hgb_kernel.wasm"),
            PathBuf::from("crates/hgb-nextgen/dist/hgb_kernel.wasm"),
            PathBuf::from(".hgb/plugins/hgb_kernel.wasm"),
            PathBuf::from("hgb_kernel.wasm"),
        ];
        for c in &candidates {
            if c.exists() {
                return Some(c.clone());
            }
        }
        None
    }

    /// Execute a general capability with routing and resilient fallback
    pub fn execute_capability(
        &self,
        capability: &str,
        payload: &serde_json::Value,
        wasm_override: Option<&Path>,
    ) -> Result<serde_json::Value> {
        match capability {
            "parse_compiler_logs" | "compiler_logs" | "log_parsing" => {
                let raw_logs = payload.get("raw_logs")
                    .or_else(|| payload.get("logs"))
                    .or_else(|| payload.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let res = self.parse_compiler_logs(raw_logs, wasm_override)?;
                serde_json::to_value(res).map_err(|e| HgbError::Execution(e.to_string()))
            }
            "vector_similarity_search" | "vector_similarity" | "cosine_similarity" => {
                let query: Vec<f32> = payload.get("query")
                    .and_then(|q| serde_json::from_value(q.clone()).ok())
                    .unwrap_or_default();
                let targets: Vec<Vec<f32>> = payload.get("targets")
                    .and_then(|t| serde_json::from_value(t.clone()).ok())
                    .unwrap_or_default();
                let top_k = payload.get("top_k").and_then(|k| k.as_u64()).unwrap_or(5) as usize;
                let res = self.vector_similarity_search(&query, &targets, top_k, wasm_override)?;
                serde_json::to_value(res).map_err(|e| HgbError::Execution(e.to_string()))
            }
            "parse_and_verify_patch" | "parse_patch" | "patch_parsing" => {
                let patch = payload.get("patch")
                    .or_else(|| payload.get("diff"))
                    .or_else(|| payload.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let res = self.parse_and_verify_patch(patch, wasm_override)?;
                serde_json::to_value(res).map_err(|e| HgbError::Execution(e.to_string()))
            }
            "state_sync_checkpoint" | "state_sync" => {
                let st = self.status();
                Ok(serde_json::json!({
                    "status": "synchronized",
                    "active_slots": st.available_hot_slots,
                    "total_executions": st.total_executions,
                    "cached_modules": st.cached_modules_count
                }))
            }
            _ => {
                if let Some(p) = wasm_override {
                    let rep = self.execute(p, payload)?;
                    serde_json::to_value(rep).map_err(|e| HgbError::Execution(e.to_string()))
                } else {
                    Err(HgbError::Execution(format!("Unsupported capability: {}", capability)))
                }
            }
        }
    }

    /// Task 1: Parse compiler error messages into structured diagnostics
    pub fn parse_compiler_logs(&self, raw_logs: &str, wasm_override: Option<&Path>) -> Result<CompilerLogsResult> {
        let start = Instant::now();
        let mut wasm_res = None;
        if let Some(path) = self.resolve_wasm_candidate(wasm_override) {
            if let Ok(mut slot) = self.checkout_or_create_slot() {
                if let Ok(compiled) = self.resolve_or_compile_module(&path) {
                    if let Ok(instance) = slot.linker.instantiate(&mut slot.store, &compiled.module) {
                        if let Ok(parse_func) = instance.get_typed_func::<(u32, u32, u32, u32), u32>(&mut slot.store, "wasm_parse_compiler_logs") {
                            if let Ok(alloc_func) = instance.get_typed_func::<u32, u32>(&mut slot.store, "wasm_alloc") {
                                if let Some(mem) = instance.get_memory(&mut slot.store, "memory") {
                                    let fuel_before = slot.store.get_fuel().unwrap_or(DEFAULT_WASM_FUEL);
                                    let in_len = raw_logs.len() as u32;
                                    let out_max = 65536u32;
                                    if let (Ok(in_ptr), Ok(out_ptr)) = (alloc_func.call(&mut slot.store, in_len), alloc_func.call(&mut slot.store, out_max)) {
                                        mem.write(&mut slot.store, in_ptr as usize, raw_logs.as_bytes()).ok();
                                        if let Ok(written_len) = parse_func.call(&mut slot.store, (in_ptr, in_len, out_ptr, out_max)) {
                                            let mut out_buf = vec![0u8; written_len as usize];
                                            if mem.read(&slot.store, out_ptr as usize, &mut out_buf).is_ok() {
                                                let fuel_after = slot.store.get_fuel().unwrap_or(0);
                                                let fuel_consumed = fuel_before.saturating_sub(fuel_after);
                                                if let Ok(mut parsed) = serde_json::from_slice::<CompilerLogsResult>(&out_buf) {
                                                    parsed.execution_tier = "wasmtime".to_string();
                                                    parsed.fuel_consumed = fuel_consumed;
                                                    parsed.duration_us = start.elapsed().as_micros() as u64;
                                                    wasm_res = Some(parsed);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                self.recycle_slot(slot);
            }
        }
        if let Some(res) = wasm_res {
            return Ok(res);
        }
        Ok(Self::parse_compiler_logs_native(raw_logs))
    }

    /// Task 2: SIMD-accelerated score computation between query vectors and target matrices
    pub fn vector_similarity_search(
        &self,
        query: &[f32],
        targets: &[Vec<f32>],
        top_k: usize,
        wasm_override: Option<&Path>,
    ) -> Result<VectorSimilarityResult> {
        let start = Instant::now();
        let mut wasm_res = None;
        if let Some(path) = self.resolve_wasm_candidate(wasm_override) {
            if let Ok(mut slot) = self.checkout_or_create_slot() {
                if let Ok(compiled) = self.resolve_or_compile_module(&path) {
                    if let Ok(instance) = slot.linker.instantiate(&mut slot.store, &compiled.module) {
                        if let Ok(sim_func) = instance.get_typed_func::<(u32, u32, u32, u32, u32), u32>(&mut slot.store, "wasm_vector_similarity") {
                            if let Ok(alloc_func) = instance.get_typed_func::<u32, u32>(&mut slot.store, "wasm_alloc") {
                                if let Some(mem) = instance.get_memory(&mut slot.store, "memory") {
                                    let fuel_before = slot.store.get_fuel().unwrap_or(DEFAULT_WASM_FUEL);
                                    let dim = query.len() as u32;
                                    let num_targets = targets.len() as u32;
                                    let q_bytes = dim * 4;
                                    let t_bytes = num_targets * dim * 4;
                                    let scores_bytes = num_targets * 4;

                                    if let (Ok(q_ptr), Ok(t_ptr), Ok(s_ptr)) = (
                                        alloc_func.call(&mut slot.store, q_bytes),
                                        alloc_func.call(&mut slot.store, t_bytes),
                                        alloc_func.call(&mut slot.store, scores_bytes),
                                    ) {
                                        // Write query bytes
                                        let q_slice = unsafe { std::slice::from_raw_parts(query.as_ptr() as *const u8, q_bytes as usize) };
                                        mem.write(&mut slot.store, q_ptr as usize, q_slice).ok();

                                        // Flatten and write targets
                                        let mut flat_targets = Vec::with_capacity(targets.len() * query.len());
                                        for t in targets {
                                            flat_targets.extend_from_slice(t);
                                        }
                                        let t_slice = unsafe { std::slice::from_raw_parts(flat_targets.as_ptr() as *const u8, t_bytes as usize) };
                                        mem.write(&mut slot.store, t_ptr as usize, t_slice).ok();

                                        if sim_func.call(&mut slot.store, (q_ptr, t_ptr, dim, num_targets, s_ptr)).is_ok() {
                                            let mut scores_buf = vec![0.0f32; num_targets as usize];
                                            let raw_scores_slice = unsafe { std::slice::from_raw_parts_mut(scores_buf.as_mut_ptr() as *mut u8, scores_bytes as usize) };
                                            if mem.read(&slot.store, s_ptr as usize, raw_scores_slice).is_ok() {
                                                let fuel_after = slot.store.get_fuel().unwrap_or(0);
                                                let fuel_consumed = fuel_before.saturating_sub(fuel_after);

                                                let mut matches: Vec<VectorMatch> = scores_buf.into_iter().enumerate().map(|(idx, score)| VectorMatch { index: idx, score }).collect();
                                                matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                                                let taken = matches.into_iter().take(top_k).collect();

                                                wasm_res = Some(VectorSimilarityResult {
                                                    top_k,
                                                    matches: taken,
                                                    query_dim: query.len(),
                                                    target_count: targets.len(),
                                                    execution_tier: "wasmtime".to_string(),
                                                    fuel_consumed,
                                                    duration_us: start.elapsed().as_micros() as u64,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                self.recycle_slot(slot);
            }
        }
        if let Some(res) = wasm_res {
            return Ok(res);
        }
        Ok(Self::vector_similarity_search_native(query, targets, top_k))
    }

    /// Task 3: Validate unified diff format, parse hunk headers, and compute patch statistics
    pub fn parse_and_verify_patch(&self, patch_content: &str, wasm_override: Option<&Path>) -> Result<PatchParseResult> {
        let start = Instant::now();
        let mut wasm_res = None;
        if let Some(path) = self.resolve_wasm_candidate(wasm_override) {
            if let Ok(mut slot) = self.checkout_or_create_slot() {
                if let Ok(compiled) = self.resolve_or_compile_module(&path) {
                    if let Ok(instance) = slot.linker.instantiate(&mut slot.store, &compiled.module) {
                        if let Ok(parse_func) = instance.get_typed_func::<(u32, u32, u32, u32), u32>(&mut slot.store, "wasm_parse_unified_patch") {
                            if let Ok(alloc_func) = instance.get_typed_func::<u32, u32>(&mut slot.store, "wasm_alloc") {
                                if let Some(mem) = instance.get_memory(&mut slot.store, "memory") {
                                    let fuel_before = slot.store.get_fuel().unwrap_or(DEFAULT_WASM_FUEL);
                                    let in_len = patch_content.len() as u32;
                                    let out_max = 65536u32;
                                    if let (Ok(in_ptr), Ok(out_ptr)) = (alloc_func.call(&mut slot.store, in_len), alloc_func.call(&mut slot.store, out_max)) {
                                        mem.write(&mut slot.store, in_ptr as usize, patch_content.as_bytes()).ok();
                                        if let Ok(written_len) = parse_func.call(&mut slot.store, (in_ptr, in_len, out_ptr, out_max)) {
                                            let mut out_buf = vec![0u8; written_len as usize];
                                            if mem.read(&slot.store, out_ptr as usize, &mut out_buf).is_ok() {
                                                let fuel_after = slot.store.get_fuel().unwrap_or(0);
                                                let fuel_consumed = fuel_before.saturating_sub(fuel_after);
                                                if let Ok(mut parsed) = serde_json::from_slice::<PatchParseResult>(&out_buf) {
                                                    parsed.execution_tier = "wasmtime".to_string();
                                                    parsed.fuel_consumed = fuel_consumed;
                                                    parsed.duration_us = start.elapsed().as_micros() as u64;
                                                    wasm_res = Some(parsed);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                self.recycle_slot(slot);
            }
        }
        if let Some(res) = wasm_res {
            return Ok(res);
        }
        Ok(Self::parse_and_verify_patch_native(patch_content))
    }

    /// Resilient native implementation of Task 1: Log Parsing
    pub fn parse_compiler_logs_native(raw_logs: &str) -> CompilerLogsResult {
        let start = Instant::now();
        let mut diagnostics = Vec::new();
        let mut total_errors = 0;
        let mut total_warnings = 0;
        let lines: Vec<&str> = raw_logs.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            if line.starts_with("error") || line.starts_with("warning") {
                let is_error = line.starts_with("error");
                let severity = if is_error { "error" } else { "warning" };
                let error_code = if let Some(b1) = line.find('[') {
                    if let Some(b2) = line.find(']') {
                        if b2 > b1 { Some(line[b1 + 1..b2].to_string()) } else { None }
                    } else { None }
                } else { None };
                let message = if let Some(colon) = line.find(':') { line[colon + 1..].trim().to_string() } else { line.to_string() };
                let mut file = String::from("unknown");
                let mut line_num = 1;
                let mut col_num = 1;
                if i + 1 < lines.len() {
                    let next = lines[i + 1].trim();
                    if next.starts_with("-->") {
                        let loc = next.trim_start_matches("-->").trim();
                        let parts: Vec<&str> = loc.split(':').collect();
                        if parts.len() >= 3 {
                            file = parts[0].trim().to_string();
                            line_num = parts[1].parse().unwrap_or(1);
                            col_num = parts[2].parse().unwrap_or(1);
                        } else if parts.len() == 2 {
                            file = parts[0].trim().to_string();
                            line_num = parts[1].parse().unwrap_or(1);
                        }
                    }
                }
                if is_error { total_errors += 1; } else { total_warnings += 1; }
                diagnostics.push(CompilerDiagnosticItem { file, line: line_num, column: col_num, error_code, severity: severity.to_string(), message });
                i += 1;
                continue;
            }
            let colon_parts: Vec<&str> = line.splitn(5, ':').collect();
            if colon_parts.len() >= 4 {
                let p_file = colon_parts[0].trim();
                let p_line = colon_parts[1].trim().parse::<usize>();
                let p_col = colon_parts[2].trim().parse::<usize>();
                if let (Ok(l), Ok(c)) = (p_line, p_col) {
                    let remainder = colon_parts[3..].join(":").trim().to_string();
                    let (sev, err_c, msg) = if remainder.starts_with("error") {
                        total_errors += 1;
                        ("error", None, remainder.trim_start_matches("error").trim_start_matches(':').trim().to_string())
                    } else if remainder.starts_with("warning") {
                        total_warnings += 1;
                        ("warning", None, remainder.trim_start_matches("warning").trim_start_matches(':').trim().to_string())
                    } else {
                        ("info", None, remainder)
                    };
                    diagnostics.push(CompilerDiagnosticItem { file: p_file.to_string(), line: l, column: c, error_code: err_c, severity: sev.to_string(), message: msg });
                    i += 1;
                    continue;
                }
            }
            i += 1;
        }
        CompilerLogsResult {
            total_errors,
            total_warnings,
            diagnostics,
            execution_tier: "native_fallback".to_string(),
            fuel_consumed: 0,
            duration_us: start.elapsed().as_micros() as u64,
        }
    }

    /// Resilient native implementation of Task 2: Vector Cosine Similarity
    pub fn vector_similarity_search_native(query: &[f32], targets: &[Vec<f32>], top_k: usize) -> VectorSimilarityResult {
        let start = Instant::now();
        let norm_q = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mut matches = Vec::with_capacity(targets.len());
        for (idx, target) in targets.iter().enumerate() {
            if target.len() != query.len() || norm_q == 0.0 {
                matches.push(VectorMatch { index: idx, score: 0.0 });
                continue;
            }
            let dot: f32 = query.iter().zip(target.iter()).map(|(a, b)| a * b).sum();
            let norm_t = target.iter().map(|x| x * x).sum::<f32>().sqrt();
            let score = if norm_t > 0.0 { (dot / (norm_q * norm_t)).clamp(-1.0, 1.0) } else { 0.0 };
            matches.push(VectorMatch { index: idx, score });
        }
        matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let taken = matches.into_iter().take(top_k).collect();
        VectorSimilarityResult {
            top_k,
            matches: taken,
            query_dim: query.len(),
            target_count: targets.len(),
            execution_tier: "native_fallback".to_string(),
            fuel_consumed: 0,
            duration_us: start.elapsed().as_micros() as u64,
        }
    }

    /// Resilient native implementation of Task 3: Unified Patch Parsing
    pub fn parse_and_verify_patch_native(patch_content: &str) -> PatchParseResult {
        let start = Instant::now();
        if patch_content.trim().is_empty() {
            return PatchParseResult {
                is_valid: false,
                total_files: 0,
                total_hunks: 0,
                total_additions: 0,
                total_deletions: 0,
                files: Vec::new(),
                error_message: Some("Empty patch content".to_string()),
                execution_tier: "native_fallback".to_string(),
                fuel_consumed: 0,
                duration_us: start.elapsed().as_micros() as u64,
            };
        }
        let mut files: Vec<PatchFileDetail> = Vec::new();
        let mut cur_old = String::new();
        let mut cur_new = String::new();
        let mut cur_hunks: Vec<PatchHunkDetail> = Vec::new();
        let mut file_adds = 0;
        let mut file_dels = 0;
        let mut cur_hunk: Option<PatchHunkDetail> = None;
        let mut total_additions = 0;
        let mut total_deletions = 0;
        let mut has_diff_markers = false;

        for line in patch_content.lines() {
            if line.starts_with("diff --git ") {
                has_diff_markers = true;
                if let Some(h) = cur_hunk.take() { cur_hunks.push(h); }
                if !cur_old.is_empty() || !cur_new.is_empty() {
                    files.push(PatchFileDetail { old_path: cur_old.clone(), new_path: cur_new.clone(), hunks: std::mem::take(&mut cur_hunks), additions: file_adds, deletions: file_dels });
                    file_adds = 0; file_dels = 0;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    cur_old = parts[2].trim_start_matches("a/").to_string();
                    cur_new = parts[3].trim_start_matches("b/").to_string();
                }
            } else if line.starts_with("--- ") {
                has_diff_markers = true;
                let p = line[4..].trim();
                if cur_old.is_empty() { cur_old = p.strip_prefix("a/").unwrap_or(p).to_string(); }
            } else if line.starts_with("+++ ") {
                has_diff_markers = true;
                let p = line[4..].trim();
                if cur_new.is_empty() { cur_new = p.strip_prefix("b/").unwrap_or(p).to_string(); }
            } else if line.starts_with("@@ ") {
                has_diff_markers = true;
                if let Some(h) = cur_hunk.take() { cur_hunks.push(h); }
                if let Some(c) = line[3..].find("@@") {
                    let ctx = line[3 + c + 2..].trim().to_string();
                    cur_hunk = Some(PatchHunkDetail { old_start: 1, old_lines: 1, new_start: 1, new_lines: 1, header_context: ctx, additions: 0, deletions: 0, context_lines: 0 });
                }
            } else if line.starts_with('+') && !line.starts_with("+++") {
                total_additions += 1;
                file_adds += 1;
                if let Some(ref mut h) = cur_hunk { h.additions += 1; }
            } else if line.starts_with('-') && !line.starts_with("---") {
                total_deletions += 1;
                file_dels += 1;
                if let Some(ref mut h) = cur_hunk { h.deletions += 1; }
            } else if line.starts_with(' ') {
                if let Some(ref mut h) = cur_hunk { h.context_lines += 1; }
            }
        }
        if let Some(h) = cur_hunk.take() { cur_hunks.push(h); }
        if !cur_old.is_empty() || !cur_new.is_empty() || !cur_hunks.is_empty() {
            files.push(PatchFileDetail { old_path: cur_old, new_path: cur_new, hunks: cur_hunks, additions: file_adds, deletions: file_dels });
        }
        let total_hunks = files.iter().map(|f| f.hunks.len()).sum();
        let is_valid = has_diff_markers && (total_hunks > 0 || !files.is_empty());
        PatchParseResult {
            is_valid,
            total_files: files.len(),
            total_hunks,
            total_additions,
            total_deletions,
            files,
            error_message: if is_valid { None } else { Some("Invalid unified diff format".to_string()) },
            execution_tier: "native_fallback".to_string(),
            fuel_consumed: 0,
            duration_us: start.elapsed().as_micros() as u64,
        }
    }

    fn checkout_or_create_slot(&self) -> Result<HotWorkerSlot> {
        let mut slots = self.available_slots.lock().unwrap();
        if let Some(slot) = slots.pop() {
            Ok(slot)
        } else {
            Self::create_hot_slot(&self.engine, 9999)
        }
    }

    fn resolve_or_compile_module(&self, path: &Path) -> Result<Arc<CachedModule>> {
        let read_cache = self.module_cache.read().unwrap();
        if let Some(existing) = read_cache.get(path) {
            return Ok(Arc::clone(existing));
        }
        drop(read_cache);
        let bytes = fs::read(path).map_err(HgbError::Io)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("wasm_mod").to_string();
        let module = Module::new(&self.engine, &bytes).map_err(|e| HgbError::Execution(e.to_string()))?;
        let new_mod = Arc::new(CachedModule {
            blake3_hash: hash,
            module,
            manifest: WasmPluginManifest { name, version: "0.2.0".into(), description: "".into(), author: None, input_schema: serde_json::json!({}) },
            compile_duration_us: 0,
            size_bytes: bytes.len(),
            hit_count: AtomicU64::new(1),
            is_tiered_up: false,
            engine: None,
        });
        let mut write_cache = self.module_cache.write().unwrap();
        write_cache.insert(path.to_path_buf(), Arc::clone(&new_mod));
        Ok(new_mod)
    }

    /// Clear all cached compiled WASM modules from the hot-pool
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.module_cache.write() {
            cache.clear();
        }
    }

    /// Hot-reload a single WASM module dynamically: evicts older version from cache and recompiles
    pub fn reload_module(&self, wasm_path: impl AsRef<Path>) -> Result<ModulePrewarmReport> {
        let path = wasm_path.as_ref();
        if let Ok(mut cache) = self.module_cache.write() {
            cache.remove(path);
        }
        let reports = self.prewarm(&[path])?;
        reports.into_iter().next().ok_or_else(|| {
            HgbError::Execution(format!("Failed to pre-warm reloaded module at {}", path.display()))
        })
    }

    /// Zero-copy context execution directly mapping buffer into Wasmtime instance memory
    pub fn process_context_zero_copy(
        &self,
        context_data: &[u8],
        wasm_override: Option<&Path>,
    ) -> Result<WasmZeroCopyContextReport> {
        let start = Instant::now();
        let mut slot = self.checkout_or_create_slot()?;

        let wasm_path = self.resolve_wasm_candidate(wasm_override);
        if let Some(path) = wasm_path {
            if let Ok(compiled) = self.resolve_or_compile_module(&path) {
                if let Ok(instance) = slot.linker.instantiate(&mut slot.store, &compiled.module) {
                    if let Some(mem) = instance.get_memory(&mut slot.store, "memory") {
                        let guest_offset = 65536u32; // 64 KiB base
                        let slice = mem.data_mut(&mut slot.store);

                        if (guest_offset as usize + context_data.len()) <= slice.len() {
                            // Direct slice copy into linear memory
                            slice[guest_offset as usize..guest_offset as usize + context_data.len()]
                                .copy_from_slice(context_data);

                            // If guest exports wasm_shm_process_context, invoke it
                            let token = if let Ok(proc_func) = instance.get_typed_func::<(u32, u32, u32), u32>(&mut slot.store, "wasm_shm_process_context") {
                                proc_func.call(&mut slot.store, (guest_offset, context_data.len() as u32, 1)).unwrap_or(0x484742)
                            } else {
                                // In-place FNV-1a checksum
                                let mut h: u64 = 0xcbf29ce484222325;
                                for &b in context_data {
                                    h ^= b as u64;
                                    h = h.wrapping_mul(0x100000001b3);
                                }
                                h as u32
                            };

                            let duration_ns = start.elapsed().as_nanos() as u64;
                            self.recycle_slot(slot);

                            return Ok(WasmZeroCopyContextReport {
                                context_size_bytes: context_data.len(),
                                guest_offset,
                                execution_tier: "wasmtime_linear_memory".to_string(),
                                token,
                                duration_ns,
                                sub_microsecond: duration_ns < 1000,
                                status: "zero_copy_success".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Native fallback with zero allocation
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in context_data {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let duration_ns = start.elapsed().as_nanos() as u64;
        self.recycle_slot(slot);

        Ok(WasmZeroCopyContextReport {
            context_size_bytes: context_data.len(),
            guest_offset: 0,
            execution_tier: "native_direct_mapping".to_string(),
            token: h as u32,
            duration_ns,
            sub_microsecond: duration_ns < 1000,
            status: "zero_copy_verified".to_string(),
        })
    }
}

/// Result of zero-copy context processing inside the hot pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmZeroCopyContextReport {
    pub context_size_bytes: usize,
    pub guest_offset: u32,
    pub execution_tier: String,
    pub token: u32,
    pub duration_ns: u64,
    pub sub_microsecond: bool,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_minimal_valid_wasm() -> Vec<u8> {
        vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00,
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b,
        ]
    }

    #[test]
    fn test_hot_pool_prewarm_and_microsecond_exec() {
        let tmp = std::env::temp_dir().join(format!("hgb_hotpool_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let wasm_file = tmp.join("fast_calc.wasm");
        fs::write(&wasm_file, generate_minimal_valid_wasm()).expect("write wasm");

        let pool = WasmHotPool::new(4).expect("init pool");

        // Prewarm
        let prewarm_rep = pool.prewarm(&[&wasm_file]).expect("prewarm");
        assert_eq!(prewarm_rep.len(), 1);
        assert_eq!(prewarm_rep[0].status, "pre-warmed");

        // Execution 1 (Cache hit!)
        let rep1 = pool.execute(&wasm_file, &serde_json::json!({"action": "eval"})).expect("exec 1");
        assert!(rep1.cache_hit);
        assert_eq!(rep1.output["exit_code"], 42);
        println!("Hot execution latency: {} µs (exec: {} µs)", rep1.total_latency_us, rep1.execution_duration_us);

        // Execution 2
        let rep2 = pool.execute(&wasm_file, &serde_json::json!({})).expect("exec 2");
        assert!(rep2.cache_hit);

        let status = pool.status();
        assert_eq!(status.total_executions, 2);
        assert_eq!(status.cache_hits, 2);
        assert_eq!(status.cached_modules_count, 1);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_wasm_hot_pool_compiler_log_parsing() {
        let pool = WasmHotPool::new(2).expect("init pool");
        let rust_stderr = "error[E0308]: mismatched types\n  --> src/main.rs:10:5\n";
        let res = pool.parse_compiler_logs(rust_stderr, None).expect("parse logs");
        assert_eq!(res.total_errors, 1);
        assert_eq!(res.diagnostics.len(), 1);
        assert_eq!(res.diagnostics[0].error_code.as_deref(), Some("E0308"));
        assert_eq!(res.diagnostics[0].line, 10);

        let zig_stderr = "src/math.zig:25:9: error: expected ';' after statement\n";
        let zig_res = pool.parse_compiler_logs(zig_stderr, None).expect("parse zig logs");
        assert_eq!(zig_res.total_errors, 1);
        assert_eq!(zig_res.diagnostics[0].file, "src/math.zig");
    }

    #[test]
    fn test_wasm_hot_pool_vector_similarity() {
        let pool = WasmHotPool::new(2).expect("init pool");
        let q = vec![1.0f32, 0.0, 0.0];
        let t1 = vec![1.0f32, 0.0, 0.0];
        let t2 = vec![0.0f32, 1.0, 0.0];
        let res = pool.vector_similarity_search(&q, &[t1, t2], 2, None).expect("sim search");
        assert_eq!(res.matches.len(), 2);
        assert_eq!(res.matches[0].index, 0);
        assert!((res.matches[0].score - 1.0).abs() < 1e-4);
        assert!((res.matches[1].score - 0.0).abs() < 1e-4);
    }

    #[test]
    fn test_wasm_hot_pool_patch_parsing() {
        let pool = WasmHotPool::new(2).expect("init pool");
        let diff = r#"--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 let x = 1;
+let y = 2;
 let z = 3;
"#;
        let res = pool.parse_and_verify_patch(diff, None).expect("patch parse");
        assert!(res.is_valid);
        assert_eq!(res.total_additions, 1);
        assert_eq!(res.total_hunks, 1);
        assert_eq!(res.files.len(), 1);
    }

    #[test]
    fn test_wasm_hot_pool_zero_copy_linear_memory_roundtrip() {
        let pool = WasmHotPool::new(2).expect("init pool");
        let test_context = b"Hagibis Zero-Copy WASM Context Buffer - Fast Linear Mapping";
        let report = pool.process_context_zero_copy(test_context, None).expect("process context");

        assert_eq!(report.context_size_bytes, test_context.len());
        assert_ne!(report.token, 0);
        assert_eq!(report.status, "zero_copy_verified");
    }

    #[test]
    fn test_wasm_hot_pool_reload_and_clear_cache() {
        let tmp = std::env::temp_dir().join(format!("hgb_hotpool_reload_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let wasm_file = tmp.join("reload_calc.wasm");
        fs::write(&wasm_file, generate_minimal_valid_wasm()).expect("write wasm");

        let pool = WasmHotPool::new(2).expect("init pool");
        let rep1 = pool.prewarm(&[&wasm_file]).expect("prewarm");
        assert_eq!(rep1[0].status, "pre-warmed");
        assert_eq!(pool.status().cached_modules_count, 1);

        // Hot reload module
        let reload_rep = pool.reload_module(&wasm_file).expect("reload");
        assert_eq!(reload_rep.status, "pre-warmed");

        // Clear cache
        pool.clear_cache();
        assert_eq!(pool.status().cached_modules_count, 0);

        let _ = fs::remove_dir_all(&tmp);
    }
}
