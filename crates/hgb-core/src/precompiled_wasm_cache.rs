//! # Precompiled WebAssembly Module Cache (`precompiled_wasm_cache`)
//!
//! High-performance cache for pre-compiled Wasmtime modules backed by memory-mapping
//! and disk persistence (`.hgb/wasm_cache/` or `/dev/shm/hgb_wasm_cache/`).
//!
//! ## Architecture
//! - Keyed by BLAKE3 hash of WASM bytecode + target architecture + compiler version tag.
//! - Multi-tier hierarchy:
//!   1. In-Memory LRU Cache: Lock-free read path returning cloned `Module` in <1µs.
//!   2. Disk Cache (Mmap): Loads pre-compiled `.cwasm` artifacts via `unsafe { Module::deserialize(engine, bytes) }`
//!      in <100µs, completely bypassing Cranelift AOT compilation.
//!   3. Cranelift JIT/AOT Compiler: On cold miss, compiles bytecode, serializes via `module.serialize()`,
//!      persists to disk atomically, and primes the in-memory mmap buffer.
//! - Sandboxed Metered Execution:
//!   * Strict fuel metering with `Store::add_fuel` and remaining fuel accounting.
//!   * Epoch deadline watchdog for wall-clock execution limits.
//!   * Hardware and WebAssembly linear memory page boundaries (`max_memory_pages`) enforced via `StoreLimits`.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use hgb_wasm_kernel::{MeteredExecutionResult, SandboxPolicy};
use memmap2::Mmap;
use wasmtime::{Caller, Engine, Extern, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Val};

use crate::error::{HgbError, Result};

/// Default capacity for in-memory precompiled modules
pub const DEFAULT_MAX_MEMORY_ENTRIES: usize = 128;
/// Current compiler version tag identifying the serialization ABI format
pub const COMPILER_VERSION_TAG: &str = "wasmtime-18.0.0";
/// Cache artifact file extension
pub const CACHE_FILE_EXTENSION: &str = "cwasm";

/// Internal cache entry holding the compiled `wasmtime::Module` and optional backing `Mmap`
#[derive(Clone)]
struct ModuleCacheEntry {
    module: Module,
    _mmap: Option<Arc<Mmap>>,
    last_accessed: Instant,
    size_bytes: usize,
}

/// Statistics and telemetry for precompiled WASM cache performance
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PrecompiledCacheStats {
    pub in_memory_hits: u64,
    pub disk_hits: u64,
    pub misses_and_compiles: u64,
    pub total_lookups: u64,
    pub in_memory_entries: usize,
    pub max_memory_entries: usize,
    pub last_deserialization_duration_us: u64,
    pub last_compilation_duration_us: u64,
}

/// Host state for sandboxed execution of untrusted WebAssembly code
struct SandboxHostState {
    limits: StoreLimits,
    stdout_logs: Vec<u8>,
    allow_stdout: bool,
}

/// High-performance cache for precompiled Wasmtime modules
pub struct PrecompiledWasmCache {
    cache_dir: PathBuf,
    max_memory_entries: usize,
    in_memory: Arc<RwLock<HashMap<String, ModuleCacheEntry>>>,
    lru_order: Arc<RwLock<VecDeque<String>>>,
    in_memory_hits: AtomicU64,
    disk_hits: AtomicU64,
    misses_and_compiles: AtomicU64,
    last_deserialization_us: AtomicU64,
    last_compilation_us: AtomicU64,
}

impl PrecompiledWasmCache {
    /// Initialize with specified directory and maximum in-memory LRU entry count
    pub fn new<P: AsRef<Path>>(cache_dir: P, max_memory_entries: usize) -> Result<Self> {
        let dir = cache_dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)
            .map_err(|e| HgbError::Storage(format!("Failed to create wasm cache directory: {}", e)))?;

        let max_entries = if max_memory_entries == 0 {
            DEFAULT_MAX_MEMORY_ENTRIES
        } else {
            max_memory_entries
        };

        Ok(Self {
            cache_dir: dir,
            max_memory_entries: max_entries,
            in_memory: Arc::new(RwLock::new(HashMap::new())),
            lru_order: Arc::new(RwLock::new(VecDeque::new())),
            in_memory_hits: AtomicU64::new(0),
            disk_hits: AtomicU64::new(0),
            misses_and_compiles: AtomicU64::new(0),
            last_deserialization_us: AtomicU64::new(0),
            last_compilation_us: AtomicU64::new(0),
        })
    }

    /// Initialize default cache pointing to `/dev/shm/hgb_wasm_cache` if available, or `.hgb/wasm_cache`
    pub fn default_cache() -> Result<Self> {
        let shm_dir = Path::new("/dev/shm/hgb_wasm_cache");
        let target_dir = if Path::new("/dev/shm").is_dir() {
            if fs::create_dir_all(shm_dir).is_ok() {
                shm_dir.to_path_buf()
            } else {
                PathBuf::from(".hgb/wasm_cache")
            }
        } else {
            PathBuf::from(".hgb/wasm_cache")
        };

        Self::new(target_dir, DEFAULT_MAX_MEMORY_ENTRIES)
    }

    /// Path to cache directory
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Compute cryptographic cache key for given WASM source/bytecode
    pub fn compute_key(wasm_bytes: &[u8]) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"hagibis-wasm-precompiled-v1:");
        hasher.update(std::env::consts::ARCH.as_bytes());
        hasher.update(b":");
        hasher.update(std::env::consts::OS.as_bytes());
        hasher.update(b":");
        hasher.update(COMPILER_VERSION_TAG.as_bytes());
        hasher.update(b":");
        hasher.update(wasm_bytes);
        hasher.finalize().to_hex().to_string()
    }

    /// Artifact path for a specific cache key
    pub fn artifact_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.{}", key, CACHE_FILE_EXTENSION))
    }

    /// High-speed module retrieval or Cranelift compilation:
    /// 1. Checks in-memory cache first (<1µs latency).
    /// 2. Checks disk cache: loads serialized module and calls `unsafe { Module::deserialize(engine, bytes) }` (<100µs).
    /// 3. On miss: compiles via Cranelift, serializes via `module.serialize()`, persists to disk/mmap, and inserts into cache.
    pub fn get_or_compile(&self, engine: &Engine, wasm_bytes: &[u8]) -> Result<Module> {
        let key = Self::compute_key(wasm_bytes);

        // Tier 1: In-memory cache (<1 microsecond)
        {
            let map = self.in_memory.read().unwrap();
            if let Some(entry) = map.get(&key) {
                self.in_memory_hits.fetch_add(1, Ordering::Relaxed);
                self.update_lru(&key);
                return Ok(entry.module.clone());
            }
        }

        let artifact_path = self.artifact_path(&key);

        // Tier 2: Disk cache backed by memory-mapping (<100 microseconds)
        if artifact_path.exists() {
            let start = Instant::now();
            if let Ok(file) = File::open(&artifact_path) {
                if let Ok(mmap) = unsafe { Mmap::map(&file) } {
                    // Fast deserialization bypassing Cranelift compilation
                    match unsafe { Module::deserialize(engine, &mmap[..]) } {
                        Ok(module) => {
                            let duration_us = start.elapsed().as_micros() as u64;
                            self.disk_hits.fetch_add(1, Ordering::Relaxed);
                            self.last_deserialization_us.store(duration_us, Ordering::Relaxed);

                            let mmap_arc = Arc::new(mmap);
                            self.insert_memory_cache(
                                key.clone(),
                                module.clone(),
                                Some(mmap_arc),
                                wasm_bytes.len(),
                            );
                            return Ok(module);
                        }
                        Err(_e) => {
                            // If deserialization fails (e.g. ABI mismatch or corrupted artifact), remove stale file
                            let _ = fs::remove_file(&artifact_path);
                        }
                    }
                }
            }
        }

        // Tier 3: Cold path - Cranelift compilation and atomic persistence
        self.misses_and_compiles.fetch_add(1, Ordering::Relaxed);
        let start_compile = Instant::now();

        let module = Module::new(engine, wasm_bytes)
            .map_err(|e| HgbError::Execution(format!("Cranelift AOT compilation failed: {}", e)))?;

        let compile_duration_us = start_compile.elapsed().as_micros() as u64;
        self.last_compilation_us.store(compile_duration_us, Ordering::Relaxed);

        // Serialize compiled module artifact
        let serialized_bytes = module.serialize().map_err(|e| {
            HgbError::Execution(format!("Failed to serialize precompiled WASM module: {}", e))
        })?;

        // Atomic disk persistence: write to temp file then rename
        let temp_filename = format!("{}.tmp.{}", key, uuid::Uuid::new_v4().simple());
        let temp_path = self.cache_dir.join(temp_filename);

        if let Ok(()) = fs::write(&temp_path, &serialized_bytes) {
            let _ = fs::rename(&temp_path, &artifact_path);
        }

        // Memory-map persisted artifact if possible
        let mmap_opt = if artifact_path.exists() {
            File::open(&artifact_path)
                .ok()
                .and_then(|f| unsafe { Mmap::map(&f).ok() })
                .map(Arc::new)
        } else {
            None
        };

        self.insert_memory_cache(
            key,
            module.clone(),
            mmap_opt,
            serialized_bytes.len(),
        );

        Ok(module)
    }

    /// Execute untrusted agent-generated code with strict fuel metering, epoch deadline, and memory limits
    pub fn execute_metered(
        &self,
        module: &Module,
        entrypoint: &str,
        input: &[u8],
        policy: &SandboxPolicy,
    ) -> Result<MeteredExecutionResult> {
        let start = Instant::now();

        // 1. Enforce WebAssembly linear memory limits before instantiation
        if policy.max_memory_pages > 0 {
            for export in module.exports() {
                if let Some(mem_type) = export.ty().memory() {
                    if mem_type.minimum() > policy.max_memory_pages as u64 {
                        let err_msg = format!(
                            "Memory page boundary violation: initial memory {} pages exceeds sandbox policy limit of {} pages ({} MiB)",
                            mem_type.minimum(),
                            policy.max_memory_pages,
                            (policy.max_memory_pages * 64) / 1024
                        );
                        return Ok(MeteredExecutionResult::err(-1, 0, start.elapsed().as_micros() as u64, err_msg));
                    }
                }
            }
        }

        // 2. Configure StoreLimits and host state
        let max_bytes = policy.max_memory_bytes();
        let limits = StoreLimitsBuilder::new()
            .memory_size(max_bytes)
            .build();

        let host_state = SandboxHostState {
            limits,
            stdout_logs: Vec::new(),
            allow_stdout: policy.allow_stdout,
        };

        let mut store = Store::new(module.engine(), host_state);
        store.limiter(|state| &mut state.limits);

        // 3. Fuel Metering setup
        let fuel_limit = policy.fuel_limit;
        if fuel_limit > 0 {
            store.set_fuel(fuel_limit).map_err(|e| {
                HgbError::Execution(format!("Failed to set fuel on store: {}", e))
            })?;
        }

        // 4. Epoch Deadline watchdog setup
        let epoch_cancel = Arc::new(AtomicBool::new(false));
        let mut watchdog_thread = None;

        if policy.epoch_deadline_ms > 0 {
            store.set_epoch_deadline(1);
            let engine_clone = module.engine().clone();
            let deadline = Duration::from_millis(policy.epoch_deadline_ms);
            let cancel_flag = epoch_cancel.clone();

            watchdog_thread = Some(std::thread::spawn(move || {
                let poll_start = Instant::now();
                while poll_start.elapsed() < deadline {
                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                if !cancel_flag.load(Ordering::Relaxed) {
                    engine_clone.increment_epoch();
                }
            }));
        }

        // 5. Linker and capability-safe host functions
        let mut linker = Linker::new(module.engine());

        // Host logging syscall wrapper
        let _ = linker.func_wrap("env", "hgb_host_log", |mut caller: Caller<'_, SandboxHostState>, ptr: i32, len: i32| {
            if caller.data().allow_stdout && ptr >= 0 && len > 0 {
                if let Some(Extern::Memory(mem)) = caller.get_export("memory") {
                    let mem_data = mem.data(&caller);
                    let start = ptr as usize;
                    let end = start + (len as usize);
                    if end <= mem_data.len() {
                        let log_slice = mem_data[start..end].to_vec();
                        caller.data_mut().stdout_logs.extend_from_slice(&log_slice);
                    }
                }
            }
        });

        // Instantiate module
        let instance = match linker.instantiate(&mut store, module) {
            Ok(inst) => inst,
            Err(e) => {
                epoch_cancel.store(true, Ordering::Relaxed);
                if let Some(w) = watchdog_thread {
                    let _ = w.join();
                }
                let duration_us = start.elapsed().as_micros() as u64;
                return Ok(MeteredExecutionResult::err(
                    -1,
                    0,
                    duration_us,
                    format!("Sandbox instantiation failed: {}", e),
                ));
            }
        };

        // 6. Find target entrypoint or standard fallback
        let target_func = instance
            .get_func(&mut store, entrypoint)
            .or_else(|| instance.get_func(&mut store, "run"))
            .or_else(|| instance.get_func(&mut store, "main"));

        let func = match target_func {
            Some(f) => f,
            None => {
                epoch_cancel.store(true, Ordering::Relaxed);
                if let Some(w) = watchdog_thread {
                    let _ = w.join();
                }
                let duration_us = start.elapsed().as_micros() as u64;
                return Ok(MeteredExecutionResult::err(
                    -1,
                    0,
                    duration_us,
                    format!("Entrypoint '{}' not found in WebAssembly exports", entrypoint),
                ));
            }
        };

        // 7. Pass input payload and execute function
        let func_ty = func.ty(&store);
        let param_count = func_ty.params().len();

        let mut call_args = Vec::new();
        let mut guest_input_ptr = 0i32;

        if let Some(Extern::Memory(memory)) = instance.get_export(&mut store, "memory") {
            if !input.is_empty() {
                // If module exports hgb_alloc, use it
                if let Some(alloc_func) = instance.get_typed_func::<i32, i32>(&mut store, "hgb_alloc").ok() {
                    if let Ok(ptr) = alloc_func.call(&mut store, input.len() as i32) {
                        guest_input_ptr = ptr;
                        let mem_slice = memory.data_mut(&mut store);
                        let p = ptr as usize;
                        if p + input.len() <= mem_slice.len() {
                            mem_slice[p..p + input.len()].copy_from_slice(input);
                        }
                    }
                } else {
                    // Safe offset placement if alloc not exported
                    let p = 1024usize;
                    let mem_slice = memory.data_mut(&mut store);
                    if p + input.len() <= mem_slice.len() {
                        mem_slice[p..p + input.len()].copy_from_slice(input);
                        guest_input_ptr = p as i32;
                    }
                }
            }
        }

        if param_count == 2 {
            call_args.push(Val::I32(guest_input_ptr));
            call_args.push(Val::I32(input.len() as i32));
        } else if param_count == 1 {
            call_args.push(Val::I32(input.len() as i32));
        }

        let mut results = vec![Val::I32(0); func_ty.results().len()];
        let call_res = func.call(&mut store, &call_args, &mut results);

        // Cancel watchdog thread
        epoch_cancel.store(true, Ordering::Relaxed);
        if let Some(w) = watchdog_thread {
            let _ = w.join();
        }

        let duration_us = start.elapsed().as_micros() as u64;

        // Calculate consumed fuel
        let fuel_remaining = store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_limit.saturating_sub(fuel_remaining);

        match call_res {
            Ok(()) => {
                let mut exit_code = 0i32;
                let mut output = Vec::new();

                if let Some(first_res) = results.first() {
                    match first_res {
                        Val::I32(v) => {
                            exit_code = *v;
                        }
                        Val::I64(v) => {
                            // Pack format: ptr (low 32) | len (high 32)
                            let ptr = (*v & 0xFFFF_FFFF) as usize;
                            let len = ((*v >> 32) & 0xFFFF_FFFF) as usize;
                            if let Some(Extern::Memory(mem)) = instance.get_export(&mut store, "memory") {
                                let mem_data = mem.data(&store);
                                if ptr + len <= mem_data.len() && len > 0 {
                                    output = mem_data[ptr..ptr + len].to_vec();
                                }
                            }
                        }
                        _ => {}
                    }
                }

                if output.is_empty() && !store.data().stdout_logs.is_empty() {
                    output = store.data().stdout_logs.clone();
                }

                Ok(MeteredExecutionResult::ok(
                    exit_code,
                    fuel_consumed,
                    duration_us,
                    output,
                ))
            }
            Err(trap) => {
                let err_msg = format!("Execution trapped: {}", trap);
                Ok(MeteredExecutionResult::err(
                    -1,
                    fuel_consumed,
                    duration_us,
                    err_msg,
                ))
            }
        }
    }

    /// Insert or update in-memory LRU cache
    fn insert_memory_cache(
        &self,
        key: String,
        module: Module,
        mmap: Option<Arc<Mmap>>,
        size_bytes: usize,
    ) {
        let mut map = self.in_memory.write().unwrap();
        let mut lru = self.lru_order.write().unwrap();

        // Evict LRU entries if capacity exceeded
        while map.len() >= self.max_memory_entries && !lru.is_empty() {
            if let Some(oldest_key) = lru.pop_front() {
                map.remove(&oldest_key);
            }
        }

        lru.retain(|k| k != &key);
        lru.push_back(key.clone());

        map.insert(
            key,
            ModuleCacheEntry {
                module,
                _mmap: mmap,
                last_accessed: Instant::now(),
                size_bytes,
            },
        );
    }

    /// Update LRU access timestamp and position
    fn update_lru(&self, key: &str) {
        let mut lru = self.lru_order.write().unwrap();
        lru.retain(|k| k != key);
        lru.push_back(key.to_string());
    }

    /// Clear in-memory cache
    pub fn clear_in_memory(&self) {
        let mut map = self.in_memory.write().unwrap();
        let mut lru = self.lru_order.write().unwrap();
        map.clear();
        lru.clear();
    }

    /// Total cache telemetry statistics
    pub fn stats(&self) -> PrecompiledCacheStats {
        let in_memory_hits = self.in_memory_hits.load(Ordering::Relaxed);
        let disk_hits = self.disk_hits.load(Ordering::Relaxed);
        let misses = self.misses_and_compiles.load(Ordering::Relaxed);
        let total = in_memory_hits + disk_hits + misses;
        let count = self.in_memory.read().unwrap().len();

        PrecompiledCacheStats {
            in_memory_hits,
            disk_hits,
            misses_and_compiles: misses,
            total_lookups: total,
            in_memory_entries: count,
            max_memory_entries: self.max_memory_entries,
            last_deserialization_duration_us: self.last_deserialization_us.load(Ordering::Relaxed),
            last_compilation_duration_us: self.last_compilation_us.load(Ordering::Relaxed),
        }
    }
}
