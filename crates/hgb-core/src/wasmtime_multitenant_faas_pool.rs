//! # Wasmtime Multi-Tenant JIT Pooling & Snapshot FaaS Runner
//!
//! Production-grade edge Function-as-a-Service (FaaS) runtime engine:
//! 1. **Instance Pooling Allocator**: Configures `wasmtime::InstanceAllocationStrategy::Pooling`
//!    to eliminate `mmap`/`munmap` system call overhead via pre-allocated instance slots, memories,
//!    and tables with microsecond-level resets.
//! 2. **Pre-Compiled Module Caching (`InstancePre`)**: Maintains an in-memory cache of pre-linked
//!    `InstancePre<FaasTenantHostState>` structures and AOT-serialized Cranelift artifacts on disk/shm,
//!    enabling sub-millisecond instance spin-up (<500 microseconds).
//! 3. **Memory Snapshotting & Instant-Restore (`FaasMemorySnapshot`)**: Captures non-volatile linear
//!    memory states, exported globals, and FaaS host buffers to allow instant-restore execution
//!    without cold initialization penalties.
//! 4. **Strict Multi-Tenant Sandboxing**: Per-tenant memory page quotas, CPU fuel caps, table limits,
//!    concurrent invocation limits, isolated `/dev/shm` virtual filesystems, and Ed25519 attestations.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use wasmtime::{
    Caller, Config, Engine, Instance, InstanceAllocationStrategy, InstancePre, Linker, Module,
    OptLevel, PoolingAllocationConfig, Store, StoreLimits, StoreLimitsBuilder, Trap, Val,
};

use crate::error::{HgbError, Result};
pub use crate::multitenant_wasm_sandbox::{
    resolve_default_shm_root, resolve_tenant_path, sanitize_tenant_id, LanguageKind,
    PolyglotSnippetRunner, SandboxSecurityAttestation, StreamChunk, StreamKind,
    GAS_LIMIT_EXHAUSTED_DIAGNOSTIC,
};

/// Default instances allocated in the Wasmtime pooling allocator
pub const DEFAULT_POOL_CORE_INSTANCES: u32 = 256;
/// Default memories in the pooling allocator
pub const DEFAULT_POOL_MEMORIES: u32 = 256;
/// Default tables in the pooling allocator
pub const DEFAULT_POOL_TABLES: u32 = 256;
/// Default maximum memories allowed per module
pub const DEFAULT_MAX_MEMORIES_PER_MODULE: u32 = 2;
/// Default maximum tables allowed per module
pub const DEFAULT_MAX_TABLES_PER_MODULE: u32 = 10;
/// Default table elements ceiling per table
pub const DEFAULT_MAX_TABLE_ELEMENTS: u32 = 10_000;
/// Hard ceiling on allowable linear memory pages per module in the pool (512 pages * 64 KiB = 32 MiB)
pub const DEFAULT_POOL_MEMORY_PAGES: u64 = 512;
/// Default fuel allocated per tenant FaaS invocation (15 million instructions)
pub const DEFAULT_TENANT_GAS_BUDGET: u64 = 15_000_000;
/// Default maximum memory pages for a tenant (256 pages * 64 KiB = 16 MiB)
pub const DEFAULT_TENANT_MEMORY_PAGES: u32 = 256;
/// Default execution deadline for a tenant snippet in milliseconds
pub const DEFAULT_TENANT_DEADLINE_MS: u64 = 5_000;
/// Hard ceiling on tenant memory pages (1024 pages = 64 MiB)
pub const MAX_TENANT_MEMORY_PAGES_CEILING: u32 = 1024;
/// Minimum gas limit
pub const MIN_TENANT_GAS_LIMIT: u64 = 10_000;
/// Hard ceiling on gas limit
pub const MAX_TENANT_GAS_CEILING: u64 = 1_000_000_000;

/// Cache tier descriptor demonstrating how an instance was initialized
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaasCacheTier {
    /// Instant instantiation directly from cached pre-linked `InstancePre` (<500 us)
    InstancePreHit,
    /// Instantiation from AOT disk/shm cache
    AotDiskHit,
    /// Cold JIT compilation using Cranelift
    ColdJitCompilation,
    /// Instant execution restored from a `FaasMemorySnapshot` (<500 us)
    SnapshotRestored,
}

impl std::fmt::Display for FaasCacheTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InstancePreHit => write!(f, "instance_pre_hit"),
            Self::AotDiskHit => write!(f, "aot_disk_hit"),
            Self::ColdJitCompilation => write!(f, "cold_jit_compilation"),
            Self::SnapshotRestored => write!(f, "snapshot_restored"),
        }
    }
}

/// Global variable state saved in a memory snapshot
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FaasGlobalVal {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl FaasGlobalVal {
    pub fn from_val(val: Val) -> Option<Self> {
        match val {
            Val::I32(v) => Some(Self::I32(v)),
            Val::I64(v) => Some(Self::I64(v)),
            Val::F32(v) => Some(Self::F32(f32::from_bits(v))),
            Val::F64(v) => Some(Self::F64(f64::from_bits(v))),
            _ => None,
        }
    }

    pub fn to_val(&self) -> Val {
        match self {
            Self::I32(v) => Val::I32(*v),
            Self::I64(v) => Val::I64(*v),
            Self::F32(v) => Val::F32(v.to_bits()),
            Self::F64(v) => Val::F64(v.to_bits()),
        }
    }
}

/// Memory snapshot capturing linear memory bytes, exported globals, and FaaS state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaasMemorySnapshot {
    pub snapshot_id: String,
    pub tenant_id: String,
    pub module_hash: String,
    pub created_at_epoch_ms: u64,
    pub memory_pages: u32,
    pub memory_bytes: Vec<u8>,
    pub globals: Vec<(String, FaasGlobalVal)>,
    pub fuel_at_snapshot: u64,
    pub custom_kv_state: HashMap<String, Vec<u8>>,
    pub blake3_digest: String,
}

impl FaasMemorySnapshot {
    /// Compute a BLAKE3 integrity checksum over snapshot contents
    pub fn calculate_digest(
        tenant_id: &str,
        module_hash: &str,
        memory_bytes: &[u8],
        globals: &[(String, FaasGlobalVal)],
        kv: &HashMap<String, Vec<u8>>,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"hgb-faas-snapshot-v1:");
        hasher.update(tenant_id.as_bytes());
        hasher.update(b":");
        hasher.update(module_hash.as_bytes());
        hasher.update(b":mem_len=");
        hasher.update(&(memory_bytes.len() as u64).to_le_bytes());
        hasher.update(memory_bytes);

        for (name, val) in globals {
            hasher.update(name.as_bytes());
            match val {
                FaasGlobalVal::I32(v) => hasher.update(&v.to_le_bytes()),
                FaasGlobalVal::I64(v) => hasher.update(&v.to_le_bytes()),
                FaasGlobalVal::F32(v) => hasher.update(&v.to_bits().to_le_bytes()),
                FaasGlobalVal::F64(v) => hasher.update(&v.to_bits().to_le_bytes()),
            };
        }

        let mut sorted_keys: Vec<&String> = kv.keys().collect();
        sorted_keys.sort();
        for k in sorted_keys {
            hasher.update(k.as_bytes());
            if let Some(v) = kv.get(k) {
                hasher.update(v);
            }
        }

        hasher.finalize().to_hex().to_string()
    }

    /// Verify cryptographic integrity of the snapshot bytes
    pub fn verify_integrity(&self) -> bool {
        let expected = Self::calculate_digest(
            &self.tenant_id,
            &self.module_hash,
            &self.memory_bytes,
            &self.globals,
            &self.custom_kv_state,
        );
        self.blake3_digest == expected
    }

    /// Byte size of the in-memory snapshot
    pub fn size_bytes(&self) -> usize {
        self.memory_bytes.len()
            + self.globals.iter().map(|(k, _)| k.len() + 8).sum::<usize>()
            + self.custom_kv_state.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
    }
}

/// Configuration settings for the Wasmtime FaaS Pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaasPoolConfig {
    pub total_core_instances: u32,
    pub total_memories: u32,
    pub total_tables: u32,
    pub max_memories_per_module: u32,
    pub max_tables_per_module: u32,
    pub table_elements: u32,
    pub memory_pages: u64,
    pub enable_pooling: bool,
    pub linear_memory_keep_resident_bytes: usize,
    pub aot_cache_dir: Option<PathBuf>,
    pub default_gas_budget: u64,
    pub epoch_interruption: bool,
}

impl Default for FaasPoolConfig {
    fn default() -> Self {
        Self {
            total_core_instances: DEFAULT_POOL_CORE_INSTANCES,
            total_memories: DEFAULT_POOL_MEMORIES,
            total_tables: DEFAULT_POOL_TABLES,
            max_memories_per_module: DEFAULT_MAX_MEMORIES_PER_MODULE,
            max_tables_per_module: DEFAULT_MAX_TABLES_PER_MODULE,
            table_elements: DEFAULT_MAX_TABLE_ELEMENTS,
            memory_pages: DEFAULT_POOL_MEMORY_PAGES,
            enable_pooling: true,
            linear_memory_keep_resident_bytes: 0,
            aot_cache_dir: Some(PathBuf::from("/dev/shm/hgb_faas_aot_cache")),
            default_gas_budget: DEFAULT_TENANT_GAS_BUDGET,
            epoch_interruption: true,
        }
    }
}

impl FaasPoolConfig {
    pub fn with_instances(mut self, instances: u32) -> Self {
        self.total_core_instances = instances.max(1);
        self.total_memories = instances.max(1);
        self.total_tables = instances.max(1);
        self
    }

    pub fn with_memory_pages(mut self, pages: u64) -> Self {
        self.memory_pages = pages.clamp(1, 4096);
        self
    }

    pub fn with_pooling(mut self, enabled: bool) -> Self {
        self.enable_pooling = enabled;
        self
    }

    pub fn with_aot_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.aot_cache_dir = Some(dir.into());
        self
    }
}

/// Tenant isolation and resource bounding policy for FaaS invocations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantFaasPolicy {
    pub tenant_id: String,
    pub max_memory_pages: u32,
    pub gas_budget: u64,
    pub execution_deadline_ms: u64,
    pub table_limit: u32,
    pub max_concurrent_invocations: usize,
    pub isolated_virtual_root: PathBuf,
    pub allow_network: bool,
    pub allow_stdout_capture: bool,
}

impl TenantFaasPolicy {
    pub fn new(tenant_id: impl Into<String>) -> Self {
        let t_id = tenant_id.into();
        let default_root = resolve_default_shm_root(&t_id);

        Self {
            tenant_id: t_id,
            max_memory_pages: DEFAULT_TENANT_MEMORY_PAGES,
            gas_budget: DEFAULT_TENANT_GAS_BUDGET,
            execution_deadline_ms: DEFAULT_TENANT_DEADLINE_MS,
            table_limit: 5_000,
            max_concurrent_invocations: 16,
            isolated_virtual_root: default_root,
            allow_network: false,
            allow_stdout_capture: true,
        }
    }

    pub fn with_max_memory_pages(mut self, pages: u32) -> Self {
        self.max_memory_pages = pages.clamp(1, MAX_TENANT_MEMORY_PAGES_CEILING);
        self
    }

    pub fn with_gas_budget(mut self, budget: u64) -> Self {
        self.gas_budget = budget.clamp(MIN_TENANT_GAS_LIMIT, MAX_TENANT_GAS_CEILING);
        self
    }

    pub fn with_deadline_ms(mut self, ms: u64) -> Self {
        self.execution_deadline_ms = ms;
        self
    }

    pub fn with_virtual_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.isolated_virtual_root = root.into();
        self
    }

    pub fn with_max_concurrent_invocations(mut self, limit: usize) -> Self {
        self.max_concurrent_invocations = limit.max(1);
        self
    }

    pub fn max_memory_bytes(&self) -> usize {
        (self.max_memory_pages as usize) * 64 * 1024
    }

    pub fn ensure_virtual_root(&self) -> Result<PathBuf> {
        let root = &self.isolated_virtual_root;
        fs::create_dir_all(root).map_err(HgbError::Io)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(root) {
                let mut perms = meta.permissions();
                perms.set_mode(0o700);
                let _ = fs::set_permissions(root, perms);
            }
        }

        Ok(root.clone())
    }
}

/// Host execution state provided to each tenant's Store
pub struct FaasTenantHostState {
    pub tenant_id: String,
    pub virtual_root: PathBuf,
    pub stdout_buf: Vec<u8>,
    pub stderr_buf: Vec<u8>,
    pub stream_tx: Option<mpsc::Sender<StreamChunk>>,
    pub limits: StoreLimits,
    pub input_payload: Vec<u8>,
    pub output_payload: Vec<u8>,
    pub kv_store: HashMap<String, Vec<u8>>,
    pub security_violations: Vec<String>,
}

/// Cached module entry holding compiled Module and pre-linked `InstancePre`
pub struct CachedFaasModule {
    pub code_hash: String,
    pub module: Arc<Module>,
    pub instance_pre: InstancePre<FaasTenantHostState>,
    pub compile_duration_us: u64,
    pub hit_count: AtomicU64,
    pub created_at_epoch_ms: u64,
    pub serialized_aot_bytes: Option<Vec<u8>>,
}

/// Tenant context maintained inside the FaaS Pool
pub struct TenantFaasContext {
    pub tenant_id: String,
    pub policy: TenantFaasPolicy,
    pub created_at_epoch_ms: u64,
    pub active_invocations: AtomicUsize,
    pub total_invocations: AtomicU64,
    pub total_fuel_consumed: AtomicU64,
    pub virtual_root: PathBuf,
    pub security_violations: Arc<std::sync::Mutex<Vec<String>>>,
}

/// Structured outcome of a tenant FaaS invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaasExecutionReport {
    pub execution_id: String,
    pub tenant_id: String,
    pub module_hash: String,
    pub snapshot_id: Option<String>,
    pub cache_tier: FaasCacheTier,
    /// Time taken to checkout/instantiate the instance (<500 microseconds target)
    pub spinup_duration_us: u64,
    /// Time spent running the entrypoint
    pub execution_duration_us: u64,
    /// Total wall-clock latency (spinup + restore + exec)
    pub total_latency_us: u64,
    pub fuel_consumed: u64,
    pub fuel_remaining: u64,
    pub memory_pages_used: u32,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub output_bytes: Vec<u8>,
    pub trapped: bool,
    pub error_diagnostic: Option<String>,
    pub security_attestation: SandboxSecurityAttestation,
}

impl FaasExecutionReport {
    pub fn is_success(&self) -> bool {
        !self.trapped && self.error_diagnostic.is_none() && self.exit_code >= 0
    }

    pub fn is_gas_exhausted(&self) -> bool {
        self.error_diagnostic.as_deref() == Some(GAS_LIMIT_EXHAUSTED_DIAGNOSTIC)
            || (self.trapped && self.fuel_remaining == 0)
    }

    pub fn output_as_string(&self) -> Option<String> {
        if self.output_bytes.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&self.output_bytes).to_string())
        }
    }

    pub fn output_as_json<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        if self.output_bytes.is_empty() {
            None
        } else {
            serde_json::from_slice(&self.output_bytes).ok()
        }
    }
}

/// Global telemetry snapshot of the FaaS Pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaasPoolTelemetry {
    pub total_invocations: u64,
    pub total_execution_time_us: u64,
    pub avg_latency_us: f64,
    pub pre_hits: u64,
    pub aot_hits: u64,
    pub cold_compilations: u64,
    pub snapshots_created: u64,
    pub snapshots_restored: u64,
    pub traps_gas_exhausted: u64,
    pub traps_total: u64,
    pub active_tenants: usize,
    pub cached_modules_count: usize,
    pub registered_snapshots_count: usize,
    pub pooling_enabled: bool,
}

/// Tenant session summary information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantFaasSessionInfo {
    pub tenant_id: String,
    pub created_at_epoch_ms: u64,
    pub active_invocations: usize,
    pub total_invocations: u64,
    pub total_fuel_consumed: u64,
    pub max_memory_pages: u32,
    pub virtual_root: String,
    pub violations_count: usize,
}

/// # Production Wasmtime Multi-Tenant JIT Pooling & Snapshot FaaS Runner
pub struct WasmtimeMultitenantFaasPool {
    engine: Engine,
    config: FaasPoolConfig,
    module_cache: Arc<RwLock<HashMap<String, Arc<CachedFaasModule>>>>,
    snapshot_store: Arc<RwLock<HashMap<String, Arc<FaasMemorySnapshot>>>>,
    tenants: Arc<RwLock<HashMap<String, Arc<TenantFaasContext>>>>,
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    base_virtual_root: PathBuf,
    pooling_active: bool,
    // Global telemetry counters
    total_invocations: AtomicU64,
    total_execution_time_us: AtomicU64,
    pre_hits: AtomicU64,
    aot_hits: AtomicU64,
    cold_compilations: AtomicU64,
    snapshots_created: AtomicU64,
    snapshots_restored: AtomicU64,
    traps_gas_exhausted: AtomicU64,
    traps_total: AtomicU64,
}

impl Default for WasmtimeMultitenantFaasPool {
    fn default() -> Self {
        Self::new().expect("Failed to initialize default WasmtimeMultitenantFaasPool")
    }
}

impl WasmtimeMultitenantFaasPool {
    /// Initialize with default pooling configuration and `/dev/shm` virtual storage
    pub fn new() -> Result<Self> {
        Self::with_config(FaasPoolConfig::default())
    }

    /// Initialize with custom configuration
    pub fn with_config(config: FaasPoolConfig) -> Result<Self> {
        let base_shm = resolve_default_shm_root("faas_shared_base");
        let parent = base_shm.parent().unwrap_or_else(|| Path::new("/tmp")).to_path_buf();
        Self::with_config_and_base(config, parent)
    }

    /// Initialize with custom configuration and explicit base storage root
    pub fn with_config_and_base(config: FaasPoolConfig, base_root: impl Into<PathBuf>) -> Result<Self> {
        let base = base_root.into();
        fs::create_dir_all(&base).map_err(HgbError::Io)?;

        // Prepare AOT cache directory if configured
        if let Some(ref aot_dir) = config.aot_cache_dir {
            let _ = fs::create_dir_all(aot_dir);
        }

        // Configure Wasmtime engine with pooling allocator strategy
        let mut wt_config = Config::new();
        wt_config.consume_fuel(true);
        if config.epoch_interruption {
            wt_config.epoch_interruption(true);
        }
        wt_config.cranelift_opt_level(OptLevel::Speed);
        wt_config.parallel_compilation(true);

        let mut pooling_active = false;
        if config.enable_pooling {
            let mut pool_cfg = PoolingAllocationConfig::default();
            pool_cfg.total_core_instances(config.total_core_instances);
            pool_cfg.total_memories(config.total_memories);
            pool_cfg.total_tables(config.total_tables);
            pool_cfg.max_memories_per_module(config.max_memories_per_module);
            pool_cfg.max_tables_per_module(config.max_tables_per_module);
            pool_cfg.table_elements(config.table_elements);
            pool_cfg.memory_pages(config.memory_pages);

            if config.linear_memory_keep_resident_bytes > 0 {
                pool_cfg.linear_memory_keep_resident(config.linear_memory_keep_resident_bytes);
            }

            wt_config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool_cfg));
            pooling_active = true;
        }

        // Resilient Engine construction: fall back to OnDemand if system virtual address space is constrained
        let engine = match Engine::new(&wt_config) {
            Ok(eng) => eng,
            Err(e) => {
                if pooling_active {
                    // Fall back to OnDemand allocation strategy
                    let mut fallback_config = Config::new();
                    fallback_config.consume_fuel(true);
                    if config.epoch_interruption {
                        fallback_config.epoch_interruption(true);
                    }
                    fallback_config.cranelift_opt_level(OptLevel::Speed);
                    fallback_config.parallel_compilation(true);
                    fallback_config.allocation_strategy(InstanceAllocationStrategy::OnDemand);
                    pooling_active = false;

                    Engine::new(&fallback_config).map_err(|e2| {
                        HgbError::Execution(format!("Failed to initialize fallback Wasmtime Engine: {}", e2))
                    })?
                } else {
                    return Err(HgbError::Execution(format!("Failed to initialize Wasmtime Engine: {}", e)));
                }
            }
        };

        // Seed cryptographic attestation keypair
        let mut seed = [0u8; 32];
        let uuid_bytes = *uuid::Uuid::new_v4().as_bytes();
        let time_bytes = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_le_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&uuid_bytes);
        hasher.update(&time_bytes);
        hasher.update(b"hagibis-wasmtime-multitenant-faas-pool-v1");
        seed.copy_from_slice(hasher.finalize().as_bytes());

        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();

        Ok(Self {
            engine,
            config,
            module_cache: Arc::new(RwLock::new(HashMap::new())),
            snapshot_store: Arc::new(RwLock::new(HashMap::new())),
            tenants: Arc::new(RwLock::new(HashMap::new())),
            signing_key,
            verifying_key,
            base_virtual_root: base,
            pooling_active,
            total_invocations: AtomicU64::new(0),
            total_execution_time_us: AtomicU64::new(0),
            pre_hits: AtomicU64::new(0),
            aot_hits: AtomicU64::new(0),
            cold_compilations: AtomicU64::new(0),
            snapshots_created: AtomicU64::new(0),
            snapshots_restored: AtomicU64::new(0),
            traps_gas_exhausted: AtomicU64::new(0),
            traps_total: AtomicU64::new(0),
        })
    }

    /// Access reference to the internal Wasmtime Engine
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Check whether instance pooling is actively enabled
    pub fn is_pooling_enabled(&self) -> bool {
        self.pooling_active
    }

    /// Access verifying key hex for attestation verification
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.to_bytes())
    }

    /// Get or create tenant context with isolation policy
    pub async fn get_or_create_tenant(
        &self,
        tenant_id: &str,
        policy_override: Option<TenantFaasPolicy>,
    ) -> Result<Arc<TenantFaasContext>> {
        let mut lock = self.tenants.write().await;
        if let Some(ctx) = lock.get(tenant_id) {
            return Ok(Arc::clone(ctx));
        }

        let policy = match policy_override {
            Some(mut p) => {
                p.tenant_id = tenant_id.to_string();
                if p.isolated_virtual_root.as_os_str().is_empty() {
                    p.isolated_virtual_root = self.base_virtual_root.join(format!("tenant_{}", sanitize_tenant_id(tenant_id)));
                }
                p
            }
            None => {
                let root = self.base_virtual_root.join(format!("tenant_{}", sanitize_tenant_id(tenant_id)));
                TenantFaasPolicy::new(tenant_id).with_virtual_root(root)
            }
        };

        policy.ensure_virtual_root()?;
        let v_root = policy.isolated_virtual_root.clone();

        let ctx = Arc::new(TenantFaasContext {
            tenant_id: tenant_id.to_string(),
            policy,
            created_at_epoch_ms: Utc::now().timestamp_millis() as u64,
            active_invocations: AtomicUsize::new(0),
            total_invocations: AtomicU64::new(0),
            total_fuel_consumed: AtomicU64::new(0),
            virtual_root: v_root,
            security_violations: Arc::new(std::sync::Mutex::new(Vec::new())),
        });

        lock.insert(tenant_id.to_string(), Arc::clone(&ctx));
        Ok(ctx)
    }

    /// Bind WASI preview1 and Hagibis edge FaaS host functions to a Linker
    pub fn create_faas_linker(engine: &Engine) -> Result<Linker<FaasTenantHostState>> {
        let mut linker = Linker::new(engine);

        // 1. WASI preview1 fd_write: Real-time streamed stdout/stderr
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<'_, FaasTenantHostState>, fd: i32, iovs_ptr: i32, iovs_len: i32, nwritten_ptr: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 28, // EBADF
                };

                let mut total_written: u32 = 0;
                for i in 0..iovs_len {
                    let iov_offset = (iovs_ptr as usize) + (i as usize) * 8;
                    let mem_data = memory.data(&caller);
                    if iov_offset + 8 > mem_data.len() {
                        return 21; // EFAULT
                    }

                    let buf_ptr = u32::from_le_bytes(mem_data[iov_offset..iov_offset + 4].try_into().unwrap()) as usize;
                    let buf_len = u32::from_le_bytes(mem_data[iov_offset + 4..iov_offset + 8].try_into().unwrap()) as usize;

                    if buf_ptr + buf_len > mem_data.len() {
                        return 21; // EFAULT
                    }

                    let bytes = mem_data[buf_ptr..buf_ptr + buf_len].to_vec();
                    let stream = if fd == 1 { StreamKind::Stdout } else { StreamKind::Stderr };
                    let chunk_str = String::from_utf8_lossy(&bytes).to_string();

                    if let Some(ref tx) = caller.data().stream_tx {
                        let chunk = StreamChunk {
                            tenant_id: caller.data().tenant_id.clone(),
                            stream,
                            chunk: chunk_str,
                            timestamp_ms: Utc::now().timestamp_millis() as u64,
                        };
                        let _ = tx.try_send(chunk);
                    }

                    let state = caller.data_mut();
                    if fd == 1 {
                        state.stdout_buf.extend_from_slice(&bytes);
                    } else if fd == 2 {
                        state.stderr_buf.extend_from_slice(&bytes);
                    }
                    total_written += buf_len as u32;
                }

                let mem_mut = memory.data_mut(&mut caller);
                let nw_offset = nwritten_ptr as usize;
                if nw_offset + 4 <= mem_mut.len() {
                    mem_mut[nw_offset..nw_offset + 4].copy_from_slice(&total_written.to_le_bytes());
                }

                0 // SUCCESS
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi fd_write: {}", e)))?;

        // 2. WASI proc_exit
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            |_caller: Caller<'_, FaasTenantHostState>, _code: i32| {},
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi proc_exit: {}", e)))?;

        // 3. WASI sched_yield
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "sched_yield",
            |_caller: Caller<'_, FaasTenantHostState>| -> i32 { 0 },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi sched_yield: {}", e)))?;

        // 4. WASI clock_time_get
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<'_, FaasTenantHostState>, _id: i32, _precision: i64, time_ptr: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 28,
                };
                let now_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
                let mem_mut = memory.data_mut(&mut caller);
                let offset = time_ptr as usize;
                if offset + 8 <= mem_mut.len() {
                    mem_mut[offset..offset + 8].copy_from_slice(&now_ns.to_le_bytes());
                    0
                } else {
                    21
                }
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi clock_time_get: {}", e)))?;

        // 5. WASI random_get
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<'_, FaasTenantHostState>, buf_ptr: i32, buf_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 28,
                };
                let offset = buf_ptr as usize;
                let len = buf_len as usize;
                let mem_mut = memory.data_mut(&mut caller);
                if offset + len <= mem_mut.len() {
                    for i in 0..len {
                        mem_mut[offset + i] = ((offset + i * 37) & 0xFF) as u8;
                    }
                    0
                } else {
                    21
                }
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi random_get: {}", e)))?;

        // 6. WASI environ_get & environ_sizes_get
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "environ_sizes_get",
            |mut caller: Caller<'_, FaasTenantHostState>, count_ptr: i32, buf_size_ptr: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 28,
                };
                let mem = memory.data_mut(&mut caller);
                let c_off = count_ptr as usize;
                let b_off = buf_size_ptr as usize;
                if c_off + 4 <= mem.len() && b_off + 4 <= mem.len() {
                    mem[c_off..c_off + 4].copy_from_slice(&0u32.to_le_bytes());
                    mem[b_off..b_off + 4].copy_from_slice(&0u32.to_le_bytes());
                    0
                } else {
                    21
                }
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi environ_sizes_get: {}", e)))?;

        linker.func_wrap(
            "wasi_snapshot_preview1",
            "environ_get",
            |_caller: Caller<'_, FaasTenantHostState>, _environ_ptr: i32, _buf_ptr: i32| -> i32 { 0 },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind wasi environ_get: {}", e)))?;

        // 7. Hagibis Edge FaaS host function: hgb_faas_get_input
        linker.func_wrap(
            "hgb_faas",
            "get_input",
            |mut caller: Caller<'_, FaasTenantHostState>, out_ptr: i32, max_len: i32| -> i32 {
                let input = caller.data().input_payload.clone();
                let len = input.len().min(max_len as usize);
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let mem_mut = memory.data_mut(&mut caller);
                let off = out_ptr as usize;
                if off + len <= mem_mut.len() {
                    mem_mut[off..off + len].copy_from_slice(&input[..len]);
                    len as i32
                } else {
                    -1
                }
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind hgb_faas get_input: {}", e)))?;

        // 8. Hagibis Edge FaaS host function: hgb_faas_set_output
        linker.func_wrap(
            "hgb_faas",
            "set_output",
            |mut caller: Caller<'_, FaasTenantHostState>, in_ptr: i32, in_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let off = in_ptr as usize;
                let len = in_len as usize;
                let mem = memory.data(&caller);
                if off + len <= mem.len() {
                    let out = mem[off..off + len].to_vec();
                    caller.data_mut().output_payload = out;
                    0
                } else {
                    -1
                }
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind hgb_faas set_output: {}", e)))?;

        // 9. Hagibis Edge FaaS host function: hgb_faas_kv_get & hgb_faas_kv_set
        linker.func_wrap(
            "hgb_faas",
            "kv_get",
            |mut caller: Caller<'_, FaasTenantHostState>, k_ptr: i32, k_len: i32, v_ptr: i32, v_max: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let k_off = k_ptr as usize;
                let k_l = k_len as usize;
                let mem = memory.data(&caller);
                if k_off + k_l > mem.len() {
                    return -1;
                }
                let key = String::from_utf8_lossy(&mem[k_off..k_off + k_l]).to_string();

                let val = match caller.data().kv_store.get(&key) {
                    Some(v) => v.clone(),
                    None => return -2, // NOT_FOUND
                };

                let copy_len = val.len().min(v_max as usize);
                let mem_mut = memory.data_mut(&mut caller);
                let v_off = v_ptr as usize;
                if v_off + copy_len <= mem_mut.len() {
                    mem_mut[v_off..v_off + copy_len].copy_from_slice(&val[..copy_len]);
                    copy_len as i32
                } else {
                    -1
                }
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind hgb_faas kv_get: {}", e)))?;

        linker.func_wrap(
            "hgb_faas",
            "kv_set",
            |mut caller: Caller<'_, FaasTenantHostState>, k_ptr: i32, k_len: i32, v_ptr: i32, v_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };
                let k_off = k_ptr as usize;
                let k_l = k_len as usize;
                let v_off = v_ptr as usize;
                let v_l = v_len as usize;
                let mem = memory.data(&caller);
                if k_off + k_l > mem.len() || v_off + v_l > mem.len() {
                    return -1;
                }
                let key = String::from_utf8_lossy(&mem[k_off..k_off + k_l]).to_string();
                let val = mem[v_off..v_off + v_l].to_vec();

                caller.data_mut().kv_store.insert(key, val);
                0
            },
        ).map_err(|e| HgbError::Execution(format!("Failed to bind hgb_faas kv_set: {}", e)))?;

        Ok(linker)
    }

    /// Retrieve or compile a pre-linked `InstancePre` cached entry
    pub async fn get_or_compile_precompiled(
        &self,
        wasm_bytes: &[u8],
    ) -> Result<(Arc<CachedFaasModule>, FaasCacheTier)> {
        let code_hash = blake3::hash(wasm_bytes).to_hex().to_string();

        // 1. Fast-path: In-memory `InstancePre` cache check
        {
            let lock = self.module_cache.read().await;
            if let Some(entry) = lock.get(&code_hash) {
                entry.hit_count.fetch_add(1, Ordering::Relaxed);
                self.pre_hits.fetch_add(1, Ordering::Relaxed);
                return Ok((Arc::clone(entry), FaasCacheTier::InstancePreHit));
            }
        }

        // 2. Secondary-path: AOT disk/shm cache check
        if let Some(ref aot_dir) = self.config.aot_cache_dir {
            let aot_file = aot_dir.join(format!("{}.aot", &code_hash));
            if aot_file.is_file() {
                if let Ok(serialized) = fs::read(&aot_file) {
                    let compile_start = Instant::now();
                    // SAFETY: The AOT file is stored in an isolated root, named by BLAKE3 hash,
                    // generated previously by this exact Wasmtime version on this machine.
                    if let Ok(module) = unsafe { Module::deserialize(&self.engine, &serialized) } {
                        let linker = Self::create_faas_linker(&self.engine)?;
                        if let Ok(instance_pre) = linker.instantiate_pre(&module) {
                            let dur_us = compile_start.elapsed().as_micros() as u64;
                            let entry = Arc::new(CachedFaasModule {
                                code_hash: code_hash.clone(),
                                module: Arc::new(module),
                                instance_pre,
                                compile_duration_us: dur_us,
                                hit_count: AtomicU64::new(1),
                                created_at_epoch_ms: Utc::now().timestamp_millis() as u64,
                                serialized_aot_bytes: Some(serialized),
                            });

                            let mut lock = self.module_cache.write().await;
                            lock.insert(code_hash, Arc::clone(&entry));
                            self.aot_hits.fetch_add(1, Ordering::Relaxed);
                            return Ok((entry, FaasCacheTier::AotDiskHit));
                        }
                    }
                }
            }
        }

        // 3. Cold-path: Cranelift JIT compilation
        let compile_start = Instant::now();
        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            HgbError::Execution(format!("Cranelift JIT compilation failed: {}", e))
        })?;

        let linker = Self::create_faas_linker(&self.engine)?;
        let instance_pre = linker.instantiate_pre(&module).map_err(|e| {
            HgbError::Execution(format!("Failed to pre-link InstancePre: {}", e))
        })?;

        let dur_us = compile_start.elapsed().as_micros() as u64;

        // Serialize AOT artifact to disk/shm
        let serialized_bytes = match module.serialize() {
            Ok(bytes) => {
                if let Some(ref aot_dir) = self.config.aot_cache_dir {
                    let aot_file = aot_dir.join(format!("{}.aot", &code_hash));
                    let _ = fs::write(&aot_file, &bytes);
                }
                Some(bytes)
            }
            Err(_) => None,
        };

        let entry = Arc::new(CachedFaasModule {
            code_hash: code_hash.clone(),
            module: Arc::new(module),
            instance_pre,
            compile_duration_us: dur_us,
            hit_count: AtomicU64::new(1),
            created_at_epoch_ms: Utc::now().timestamp_millis() as u64,
            serialized_aot_bytes: serialized_bytes,
        });

        let mut lock = self.module_cache.write().await;
        lock.insert(code_hash, Arc::clone(&entry));
        self.cold_compilations.fetch_add(1, Ordering::Relaxed);

        Ok((entry, FaasCacheTier::ColdJitCompilation))
    }

    /// Pre-compile and pre-warm a module into the `InstancePre` cache
    pub async fn precompile_and_cache(&self, lang: LanguageKind, source: &str) -> Result<String> {
        let wasm_bytes = PolyglotSnippetRunner::prepare_bytecode(lang, source)?;
        let (cached, _) = self.get_or_compile_precompiled(&wasm_bytes).await?;
        Ok(cached.code_hash.clone())
    }

    /// Retrieve an existing cached module by hash
    pub async fn get_cached_module(&self, hash: &str) -> Option<Arc<CachedFaasModule>> {
        self.module_cache.read().await.get(hash).cloned()
    }

    /// Capture a linear memory snapshot of an active instance
    pub fn create_snapshot(
        &self,
        tenant_id: &str,
        snapshot_id: &str,
        instance: &Instance,
        store: &mut Store<FaasTenantHostState>,
        module_hash: &str,
    ) -> Result<Arc<FaasMemorySnapshot>> {
        let mem = instance.get_memory(&mut *store, "memory").ok_or_else(|| {
            HgbError::Execution("Module does not export a 'memory' section for snapshotting".to_string())
        })?;

        let memory_pages = mem.size(&*store) as u32;
        let memory_bytes = mem.data(&*store).to_vec();

        // Capture exported globals
        let mut globals = Vec::new();
        let mut global_handles = Vec::new();
        for export in instance.exports(&mut *store) {
            let name = export.name().to_string();
            if let Some(global) = export.into_global() {
                global_handles.push((name, global));
            }
        }
        for (name, global) in global_handles {
            let val = global.get(&mut *store);
            if let Some(g_val) = FaasGlobalVal::from_val(val) {
                globals.push((name, g_val));
            }
        }

        let fuel_at_snapshot = store.get_fuel().unwrap_or(0);
        let custom_kv = store.data().kv_store.clone();

        let digest = FaasMemorySnapshot::calculate_digest(
            tenant_id,
            module_hash,
            &memory_bytes,
            &globals,
            &custom_kv,
        );

        let snapshot = Arc::new(FaasMemorySnapshot {
            snapshot_id: snapshot_id.to_string(),
            tenant_id: tenant_id.to_string(),
            module_hash: module_hash.to_string(),
            created_at_epoch_ms: Utc::now().timestamp_millis() as u64,
            memory_pages,
            memory_bytes,
            globals,
            fuel_at_snapshot,
            custom_kv_state: custom_kv,
            blake3_digest: digest,
        });

        self.snapshots_created.fetch_add(1, Ordering::Relaxed);
        Ok(snapshot)
    }

    /// Register a snapshot into the thread-safe registry
    pub async fn register_snapshot(&self, snapshot: Arc<FaasMemorySnapshot>) {
        let mut lock = self.snapshot_store.write().await;
        lock.insert(snapshot.snapshot_id.clone(), snapshot);
    }

    /// Retrieve a registered snapshot
    pub async fn get_snapshot(&self, snapshot_id: &str) -> Option<Arc<FaasMemorySnapshot>> {
        self.snapshot_store.read().await.get(snapshot_id).cloned()
    }

    /// Delete a registered snapshot
    pub async fn delete_snapshot(&self, snapshot_id: &str) -> bool {
        self.snapshot_store.write().await.remove(snapshot_id).is_some()
    }

    /// Instant-restore linear memory, globals, and KV state into an instantiated slot
    pub fn restore_snapshot(
        &self,
        snapshot: &FaasMemorySnapshot,
        instance: &Instance,
        store: &mut Store<FaasTenantHostState>,
    ) -> Result<u64> {
        let start = Instant::now();

        if !snapshot.verify_integrity() {
            return Err(HgbError::Security(
                "MemorySnapshot integrity verification failed (BLAKE3 digest mismatch)".to_string(),
            ));
        }

        let mem = instance.get_memory(&mut *store, "memory").ok_or_else(|| {
            HgbError::Execution("Target instance missing 'memory' export for snapshot restore".to_string())
        })?;

        let current_pages = mem.size(&*store) as u32;
        if current_pages < snapshot.memory_pages {
            let needed = (snapshot.memory_pages - current_pages) as u64;
            mem.grow(&mut *store, needed).map_err(|e| {
                HgbError::Execution(format!("Failed to grow memory to snapshot size: {}", e))
            })?;
        }

        // Restore linear memory bytes
        mem.write(&mut *store, 0, &snapshot.memory_bytes).map_err(|e| {
            HgbError::Execution(format!("Failed to write snapshot memory slice: {}", e))
        })?;

        // Restore exported globals
        for (name, val) in &snapshot.globals {
            if let Some(global) = instance.get_global(&mut *store, name) {
                let _ = global.set(&mut *store, val.to_val());
            }
        }

        // Restore KV state
        store.data_mut().kv_store = snapshot.custom_kv_state.clone();

        // Restore remaining fuel
        if snapshot.fuel_at_snapshot > 0 {
            let _ = store.set_fuel(snapshot.fuel_at_snapshot);
        }

        self.snapshots_restored.fetch_add(1, Ordering::Relaxed);
        Ok(start.elapsed().as_micros() as u64)
    }

    /// Execute a FaaS invocation using pre-linked `InstancePre` pooling
    pub async fn execute_faas(
        &self,
        tenant_id: &str,
        lang: LanguageKind,
        code_or_bytes: &str,
        entrypoint: Option<&str>,
        input_payload: Option<&[u8]>,
        gas_override: Option<u64>,
        stream_tx: Option<mpsc::Sender<StreamChunk>>,
    ) -> Result<FaasExecutionReport> {
        let tenant = self.get_or_create_tenant(tenant_id, None).await?;

        // Enforce per-tenant concurrency quota
        let current_active = tenant.active_invocations.fetch_add(1, Ordering::SeqCst);
        if current_active >= tenant.policy.max_concurrent_invocations {
            tenant.active_invocations.fetch_sub(1, Ordering::SeqCst);
            return Err(HgbError::Execution(format!(
                "Tenant '{}' reached maximum concurrency limit ({})",
                tenant_id, tenant.policy.max_concurrent_invocations
            )));
        }

        let wasm_bytes = match PolyglotSnippetRunner::prepare_bytecode(lang, code_or_bytes) {
            Ok(b) => b,
            Err(e) => {
                tenant.active_invocations.fetch_sub(1, Ordering::SeqCst);
                return Err(e);
            }
        };

        let (cached_mod, cache_tier) = match self.get_or_compile_precompiled(&wasm_bytes).await {
            Ok(c) => c,
            Err(e) => {
                tenant.active_invocations.fetch_sub(1, Ordering::SeqCst);
                return Err(e);
            }
        };

        let engine = self.engine.clone();
        let signing_key = self.signing_key.clone();
        let verifying_key = self.verifying_key.clone();
        let tenant_id_str = tenant_id.to_string();
        let policy = tenant.policy.clone();
        let virtual_root = tenant.virtual_root.clone();
        let code_hash = cached_mod.code_hash.clone();
        let entrypoint_str = entrypoint.map(|s| s.to_string());
        let input_vec = input_payload.map(|p| p.to_vec()).unwrap_or_default();
        let instance_pre = cached_mod.instance_pre.clone();

        // Execute in worker threadpool to preserve async executor throughput
        let report = tokio::task::spawn_blocking(move || {
            Self::execute_instance_pre_blocking(
                &engine,
                &instance_pre,
                &tenant_id_str,
                &policy,
                &virtual_root,
                &code_hash,
                cache_tier,
                None, // No snapshot
                entrypoint_str.as_deref(),
                &input_vec,
                gas_override,
                stream_tx,
                &signing_key,
                &verifying_key,
            )
        })
        .await
        .map_err(|e| HgbError::Execution(format!("Blocking task join error: {}", e)))??;

        tenant.active_invocations.fetch_sub(1, Ordering::SeqCst);
        tenant.total_invocations.fetch_add(1, Ordering::Relaxed);
        tenant.total_fuel_consumed.fetch_add(report.fuel_consumed, Ordering::Relaxed);

        self.total_invocations.fetch_add(1, Ordering::Relaxed);
        self.total_execution_time_us.fetch_add(report.total_latency_us, Ordering::Relaxed);

        if report.trapped {
            self.traps_total.fetch_add(1, Ordering::Relaxed);
            if report.is_gas_exhausted() {
                self.traps_gas_exhausted.fetch_add(1, Ordering::Relaxed);
            }
        }

        Ok(report)
    }

    /// Execute a FaaS invocation by instant-restoring from a `FaasMemorySnapshot`
    pub async fn execute_faas_from_snapshot(
        &self,
        tenant_id: &str,
        snapshot_id: &str,
        entrypoint: Option<&str>,
        input_payload: Option<&[u8]>,
        gas_override: Option<u64>,
        stream_tx: Option<mpsc::Sender<StreamChunk>>,
    ) -> Result<FaasExecutionReport> {
        let snapshot = self.get_snapshot(snapshot_id).await.ok_or_else(|| {
            HgbError::NotFound(format!("MemorySnapshot '{}' not found in registry", snapshot_id))
        })?;

        if snapshot.tenant_id != tenant_id {
            return Err(HgbError::Security(format!(
                "Tenant '{}' unauthorized to restore snapshot belonging to '{}'",
                tenant_id, snapshot.tenant_id
            )));
        }

        let tenant = self.get_or_create_tenant(tenant_id, None).await?;
        let current_active = tenant.active_invocations.fetch_add(1, Ordering::SeqCst);
        if current_active >= tenant.policy.max_concurrent_invocations {
            tenant.active_invocations.fetch_sub(1, Ordering::SeqCst);
            return Err(HgbError::Execution(format!(
                "Tenant '{}' reached maximum concurrency limit ({})",
                tenant_id, tenant.policy.max_concurrent_invocations
            )));
        }

        // Retrieve cached precompiled module
        let cached_mod = self.get_cached_module(&snapshot.module_hash).await.ok_or_else(|| {
            HgbError::NotFound(format!(
                "Precompiled module '{}' associated with snapshot not in cache",
                snapshot.module_hash
            ))
        })?;

        let engine = self.engine.clone();
        let signing_key = self.signing_key.clone();
        let verifying_key = self.verifying_key.clone();
        let tenant_id_str = tenant_id.to_string();
        let policy = tenant.policy.clone();
        let virtual_root = tenant.virtual_root.clone();
        let code_hash = snapshot.module_hash.clone();
        let entrypoint_str = entrypoint.map(|s| s.to_string());
        let input_vec = input_payload.map(|p| p.to_vec()).unwrap_or_default();
        let instance_pre = cached_mod.instance_pre.clone();
        let snap_clone = Arc::clone(&snapshot);

        let report = tokio::task::spawn_blocking(move || {
            Self::execute_instance_pre_blocking(
                &engine,
                &instance_pre,
                &tenant_id_str,
                &policy,
                &virtual_root,
                &code_hash,
                FaasCacheTier::SnapshotRestored,
                Some(&snap_clone),
                entrypoint_str.as_deref(),
                &input_vec,
                gas_override,
                stream_tx,
                &signing_key,
                &verifying_key,
            )
        })
        .await
        .map_err(|e| HgbError::Execution(format!("Blocking task join error: {}", e)))??;

        tenant.active_invocations.fetch_sub(1, Ordering::SeqCst);
        tenant.total_invocations.fetch_add(1, Ordering::Relaxed);
        tenant.total_fuel_consumed.fetch_add(report.fuel_consumed, Ordering::Relaxed);

        self.total_invocations.fetch_add(1, Ordering::Relaxed);
        self.total_execution_time_us.fetch_add(report.total_latency_us, Ordering::Relaxed);

        if report.trapped {
            self.traps_total.fetch_add(1, Ordering::Relaxed);
            if report.is_gas_exhausted() {
                self.traps_gas_exhausted.fetch_add(1, Ordering::Relaxed);
            }
        }

        Ok(report)
    }

    /// Execute an initialization routine and save a snapshot in one coordinated workflow
    pub async fn execute_and_snapshot(
        &self,
        tenant_id: &str,
        lang: LanguageKind,
        init_code: &str,
        init_entrypoint: &str,
        snapshot_id: &str,
    ) -> Result<(FaasExecutionReport, Arc<FaasMemorySnapshot>)> {
        let tenant = self.get_or_create_tenant(tenant_id, None).await?;
        let wasm_bytes = PolyglotSnippetRunner::prepare_bytecode(lang, init_code)?;
        let (cached_mod, _) = self.get_or_compile_precompiled(&wasm_bytes).await?;

        let engine = self.engine.clone();
        let signing_key = self.signing_key.clone();
        let verifying_key = self.verifying_key.clone();
        let tenant_id_str = tenant_id.to_string();
        let policy = tenant.policy.clone();
        let virtual_root = tenant.virtual_root.clone();
        let code_hash = cached_mod.code_hash.clone();
        let entrypoint_str = init_entrypoint.to_string();
        let instance_pre = cached_mod.instance_pre.clone();
        let snap_id = snapshot_id.to_string();

        let (report, snapshot) = tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let gas_budget = policy.gas_budget;

            let limits = StoreLimitsBuilder::new()
                .memory_size(policy.max_memory_bytes())
                .instances(1)
                .tables(policy.table_limit as usize)
                .build();

            let host_state = FaasTenantHostState {
                tenant_id: tenant_id_str.clone(),
                virtual_root,
                stdout_buf: Vec::new(),
                stderr_buf: Vec::new(),
                stream_tx: None,
                limits,
                input_payload: Vec::new(),
                output_payload: Vec::new(),
                kv_store: HashMap::new(),
                security_violations: Vec::new(),
            };

            let mut store = Store::new(&engine, host_state);
            store.limiter(|state| &mut state.limits);
            store.set_fuel(gas_budget).map_err(|e| {
                HgbError::Execution(format!("Failed to set fuel budget: {}", e))
            })?;
            store.set_epoch_deadline(1);

            let spinup_start = Instant::now();
            let instance = instance_pre.instantiate(&mut store).map_err(|e| {
                HgbError::Execution(format!("InstancePre instantiation failed: {}", e))
            })?;
            let spinup_us = spinup_start.elapsed().as_micros() as u64;

            // Invoke initialization entrypoint
            let mut exit_code = 0;
            let mut trap_error = None;

            let exec_start = Instant::now();
            if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, &entrypoint_str) {
                match func.call(&mut store, ()) {
                    Ok(c) => exit_code = c,
                    Err(e) => trap_error = Some(e),
                }
            } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, &entrypoint_str) {
                if let Err(e) = func.call(&mut store, ()) {
                    trap_error = Some(e);
                }
            } else {
                return Err(HgbError::NotFound(format!("Initialization entrypoint '{}' not found", entrypoint_str)));
            }
            let exec_us = exec_start.elapsed().as_micros() as u64;

            // Capture snapshot of memory and globals
            let mem = instance.get_memory(&mut store, "memory").ok_or_else(|| {
                HgbError::Execution("Missing 'memory' export for snapshotting".to_string())
            })?;
            let memory_pages = mem.size(&store) as u32;
            let memory_bytes = mem.data(&store).to_vec();

            let mut globals = Vec::new();
            let mut global_handles = Vec::new();
            for export in instance.exports(&mut store) {
                let name = export.name().to_string();
                if let Some(global) = export.into_global() {
                    global_handles.push((name, global));
                }
            }
            for (name, global) in global_handles {
                let val = global.get(&mut store);
                if let Some(g_val) = FaasGlobalVal::from_val(val) {
                    globals.push((name, g_val));
                }
            }

            let fuel_at_snap = store.get_fuel().unwrap_or(0);
            let kv_snap = store.data().kv_store.clone();

            let digest = FaasMemorySnapshot::calculate_digest(
                &tenant_id_str,
                &code_hash,
                &memory_bytes,
                &globals,
                &kv_snap,
            );

            let snapshot = Arc::new(FaasMemorySnapshot {
                snapshot_id: snap_id,
                tenant_id: tenant_id_str.clone(),
                module_hash: code_hash.clone(),
                created_at_epoch_ms: Utc::now().timestamp_millis() as u64,
                memory_pages,
                memory_bytes,
                globals,
                fuel_at_snapshot: fuel_at_snap,
                custom_kv_state: kv_snap,
                blake3_digest: digest,
            });

            let total_us = start.elapsed().as_micros() as u64;
            let remaining_fuel = store.get_fuel().unwrap_or(0);
            let gas_consumed = gas_budget.saturating_sub(remaining_fuel);
            let trapped = trap_error.is_some();
            let error_diagnostic = trap_error.map(|e| e.to_string());

            let attestation = Self::generate_attestation_sync(
                &signing_key,
                &verifying_key,
                &tenant_id_str,
                &code_hash,
                gas_consumed,
                memory_pages,
                store.data().security_violations.is_empty(),
            );

            let report = FaasExecutionReport {
                execution_id: uuid::Uuid::new_v4().to_string(),
                tenant_id: tenant_id_str,
                module_hash: code_hash,
                snapshot_id: None,
                cache_tier: FaasCacheTier::ColdJitCompilation,
                spinup_duration_us: spinup_us,
                execution_duration_us: exec_us,
                total_latency_us: total_us,
                fuel_consumed: gas_consumed,
                fuel_remaining: remaining_fuel,
                memory_pages_used: memory_pages,
                exit_code,
                stdout: String::from_utf8_lossy(&store.data().stdout_buf).to_string(),
                stderr: String::from_utf8_lossy(&store.data().stderr_buf).to_string(),
                output_bytes: store.data().output_payload.clone(),
                trapped,
                error_diagnostic,
                security_attestation: attestation,
            };

            Ok((report, snapshot))
        })
        .await
        .map_err(|e| HgbError::Execution(format!("Task execution error: {}", e)))??;

        self.register_snapshot(Arc::clone(&snapshot)).await;
        self.snapshots_created.fetch_add(1, Ordering::Relaxed);
        tenant.total_invocations.fetch_add(1, Ordering::Relaxed);
        tenant.total_fuel_consumed.fetch_add(report.fuel_consumed, Ordering::Relaxed);
        self.total_invocations.fetch_add(1, Ordering::Relaxed);
        self.total_execution_time_us.fetch_add(report.total_latency_us, Ordering::Relaxed);
        Ok((report, snapshot))
    }

    /// Internal synchronous blocking execution engine leveraging `InstancePre`
    fn execute_instance_pre_blocking(
        engine: &Engine,
        instance_pre: &InstancePre<FaasTenantHostState>,
        tenant_id: &str,
        policy: &TenantFaasPolicy,
        virtual_root: &Path,
        code_hash: &str,
        cache_tier: FaasCacheTier,
        snapshot_opt: Option<&Arc<FaasMemorySnapshot>>,
        entrypoint: Option<&str>,
        input_payload: &[u8],
        gas_override: Option<u64>,
        stream_tx: Option<mpsc::Sender<StreamChunk>>,
        signing_key: &SigningKey,
        verifying_key: &VerifyingKey,
    ) -> Result<FaasExecutionReport> {
        let total_start = Instant::now();
        let gas_budget = gas_override
            .unwrap_or(policy.gas_budget)
            .clamp(MIN_TENANT_GAS_LIMIT, MAX_TENANT_GAS_CEILING);

        let limits = StoreLimitsBuilder::new()
            .memory_size(policy.max_memory_bytes())
            .instances(1)
            .tables(policy.table_limit as usize)
            .build();

        let host_state = FaasTenantHostState {
            tenant_id: tenant_id.to_string(),
            virtual_root: virtual_root.to_path_buf(),
            stdout_buf: Vec::new(),
            stderr_buf: Vec::new(),
            stream_tx,
            limits,
            input_payload: input_payload.to_vec(),
            output_payload: Vec::new(),
            kv_store: HashMap::new(),
            security_violations: Vec::new(),
        };

        let mut store = Store::new(engine, host_state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(gas_budget).map_err(|e| {
            HgbError::Execution(format!("Failed to set tenant fuel budget: {}", e))
        })?;
        store.set_epoch_deadline(1);

        // 1. Ultra-fast instantiation from pre-linked `InstancePre` slot (<500 us)
        let spinup_start = Instant::now();
        let instance = match instance_pre.instantiate(&mut store) {
            Ok(inst) => inst,
            Err(e) => {
                let spinup_us = spinup_start.elapsed().as_micros() as u64;
                let attestation = Self::generate_attestation_sync(
                    signing_key,
                    verifying_key,
                    tenant_id,
                    code_hash,
                    0,
                    0,
                    false,
                );
                return Ok(FaasExecutionReport {
                    execution_id: uuid::Uuid::new_v4().to_string(),
                    tenant_id: tenant_id.to_string(),
                    module_hash: code_hash.to_string(),
                    snapshot_id: snapshot_opt.map(|s| s.snapshot_id.clone()),
                    cache_tier,
                    spinup_duration_us: spinup_us,
                    execution_duration_us: 0,
                    total_latency_us: spinup_us,
                    fuel_consumed: 0,
                    fuel_remaining: gas_budget,
                    memory_pages_used: 0,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Instance checkout trap: {}", e),
                    output_bytes: Vec::new(),
                    trapped: true,
                    error_diagnostic: Some(format!("Instantiation trap: {}", e)),
                    security_attestation: attestation,
                });
            }
        };
        let spinup_duration_us = spinup_start.elapsed().as_micros() as u64;

        // 2. Instant memory restore if executing from snapshot
        if let Some(snapshot) = snapshot_opt {
            if let Err(e) = Self::restore_snapshot_sync(snapshot, &instance, &mut store) {
                let attestation = Self::generate_attestation_sync(
                    signing_key,
                    verifying_key,
                    tenant_id,
                    code_hash,
                    0,
                    0,
                    false,
                );
                return Ok(FaasExecutionReport {
                    execution_id: uuid::Uuid::new_v4().to_string(),
                    tenant_id: tenant_id.to_string(),
                    module_hash: code_hash.to_string(),
                    snapshot_id: Some(snapshot.snapshot_id.clone()),
                    cache_tier,
                    spinup_duration_us,
                    execution_duration_us: 0,
                    total_latency_us: total_start.elapsed().as_micros() as u64,
                    fuel_consumed: 0,
                    fuel_remaining: gas_budget,
                    memory_pages_used: 0,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Snapshot restore error: {}", e),
                    output_bytes: Vec::new(),
                    trapped: true,
                    error_diagnostic: Some(format!("Snapshot restore failed: {}", e)),
                    security_attestation: attestation,
                });
            }
        }

        // 3. Invoke entrypoint
        let exec_start = Instant::now();
        let mut exit_code = 0;
        let mut trap_error = None;

        let target_entry = entrypoint.unwrap_or("run");
        if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, target_entry) {
            match func.call(&mut store, ()) {
                Ok(code) => exit_code = code,
                Err(e) => trap_error = Some(e),
            }
        } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, target_entry) {
            if let Err(e) = func.call(&mut store, ()) {
                trap_error = Some(e);
            }
        } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "_start") {
            if let Err(e) = func.call(&mut store, ()) {
                trap_error = Some(e);
            }
        } else if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, "main") {
            match func.call(&mut store, ()) {
                Ok(code) => exit_code = code,
                Err(e) => trap_error = Some(e),
            }
        } else if let Ok(func) = instance.get_typed_func::<(i32, i32), u64>(&mut store, "hgb_tool_invoke") {
            match func.call(&mut store, (0, 0)) {
                Ok(packed) => {
                    let len = (packed >> 32) as usize;
                    let ptr = (packed & 0xFFFFFFFF) as usize;
                    if len > 0 {
                        if let Some(mem) = instance.get_memory(&mut store, "memory") {
                            let slice = mem.data(&store);
                            if ptr + len <= slice.len() {
                                let out = slice[ptr..ptr + len].to_vec();
                                store.data_mut().output_payload = out;
                            }
                        }
                    }
                }
                Err(e) => trap_error = Some(e),
            }
        } else {
            trap_error = Some(wasmtime::Error::msg(format!(
                "No valid entrypoint ('{}', '_start', 'main', or 'hgb_tool_invoke') found",
                target_entry
            )));
        }

        let execution_duration_us = exec_start.elapsed().as_micros() as u64;
        let total_latency_us = total_start.elapsed().as_micros() as u64;

        let mut remaining_fuel = store.get_fuel().unwrap_or(0);
        let mut fuel_consumed = gas_budget.saturating_sub(remaining_fuel);
        let mut trapped = false;
        let mut error_diagnostic = None;

        if let Some(trap) = trap_error {
            trapped = true;
            let trap_str = trap.to_string();
            let is_fuel_exhausted = remaining_fuel == 0
                || trap_str.to_lowercase().contains("fuel")
                || trap_str.to_lowercase().contains("gas")
                || trap.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel);

            if is_fuel_exhausted {
                exit_code = -1;
                fuel_consumed = gas_budget;
                remaining_fuel = 0;
                error_diagnostic = Some(GAS_LIMIT_EXHAUSTED_DIAGNOSTIC.to_string());
                store
                    .data_mut()
                    .stderr_buf
                    .extend_from_slice(b"Execution trap: CPU fuel limit exhausted\n");
            } else if trap_str.contains("interrupt") || trap_str.contains("epoch") {
                exit_code = -1;
                error_diagnostic = Some("DeadlineExceeded: Execution deadline timed out".to_string());
                store
                    .data_mut()
                    .stderr_buf
                    .extend_from_slice(b"Execution trap: Execution deadline timed out\n");
            } else {
                exit_code = -1;
                error_diagnostic = Some(format!("Execution trap: {}", trap_str));
                let msg = format!("Execution trap: {}\n", trap_str);
                store.data_mut().stderr_buf.extend_from_slice(msg.as_bytes());
            }
        }

        let memory_pages = instance
            .get_memory(&mut store, "memory")
            .map(|m| m.size(&store) as u32)
            .unwrap_or(1);

        let data = store.data();
        let stdout = String::from_utf8_lossy(&data.stdout_buf).to_string();
        let stderr = String::from_utf8_lossy(&data.stderr_buf).to_string();
        let output_bytes = data.output_payload.clone();
        let isolation_clean = data.security_violations.is_empty();

        let attestation = Self::generate_attestation_sync(
            signing_key,
            verifying_key,
            tenant_id,
            code_hash,
            fuel_consumed,
            memory_pages,
            isolation_clean,
        );

        Ok(FaasExecutionReport {
            execution_id: uuid::Uuid::new_v4().to_string(),
            tenant_id: tenant_id.to_string(),
            module_hash: code_hash.to_string(),
            snapshot_id: snapshot_opt.map(|s| s.snapshot_id.clone()),
            cache_tier,
            spinup_duration_us,
            execution_duration_us,
            total_latency_us,
            fuel_consumed,
            fuel_remaining: remaining_fuel,
            memory_pages_used: memory_pages,
            exit_code,
            stdout,
            stderr,
            output_bytes,
            trapped,
            error_diagnostic,
            security_attestation: attestation,
        })
    }

    fn restore_snapshot_sync(
        snapshot: &FaasMemorySnapshot,
        instance: &Instance,
        store: &mut Store<FaasTenantHostState>,
    ) -> Result<()> {
        if !snapshot.verify_integrity() {
            return Err(HgbError::Security(
                "MemorySnapshot integrity verification failed (BLAKE3 mismatch)".to_string(),
            ));
        }

        let mem = instance.get_memory(&mut *store, "memory").ok_or_else(|| {
            HgbError::Execution("Missing 'memory' export for snapshot restore".to_string())
        })?;

        let current_pages = mem.size(&*store) as u32;
        if current_pages < snapshot.memory_pages {
            let needed = (snapshot.memory_pages - current_pages) as u64;
            let _ = mem.grow(&mut *store, needed);
        }

        mem.write(&mut *store, 0, &snapshot.memory_bytes).map_err(|e| {
            HgbError::Execution(format!("Memory slice write failed: {}", e))
        })?;

        for (name, val) in &snapshot.globals {
            if let Some(global) = instance.get_global(&mut *store, name) {
                let _ = global.set(&mut *store, val.to_val());
            }
        }

        store.data_mut().kv_store = snapshot.custom_kv_state.clone();
        if snapshot.fuel_at_snapshot > 0 {
            let _ = store.set_fuel(snapshot.fuel_at_snapshot);
        }

        Ok(())
    }

    fn generate_attestation_sync(
        signing_key: &SigningKey,
        verifying_key: &VerifyingKey,
        tenant_id: &str,
        code_hash_blake3: &str,
        gas_consumed: u64,
        memory_pages: u32,
        isolation_clean: bool,
    ) -> SandboxSecurityAttestation {
        let timestamp_epoch_ms = Utc::now().timestamp_millis() as u64;
        let canonical = format!(
            "hgb-wasm-attest:tenant={}:hash={}:gas={}:pages={}:time={}:clean={}",
            tenant_id, code_hash_blake3, gas_consumed, memory_pages, timestamp_epoch_ms, isolation_clean
        );

        let signature = signing_key.sign(canonical.as_bytes());
        let ed25519_signature = hex::encode(signature.to_bytes());
        let public_key_hex = hex::encode(verifying_key.to_bytes());

        SandboxSecurityAttestation {
            tenant_id: tenant_id.to_string(),
            code_hash_blake3: code_hash_blake3.to_string(),
            ed25519_signature,
            public_key_hex,
            timestamp_epoch_ms,
            memory_pages_allocated: memory_pages,
            gas_consumed,
            isolation_clean,
        }
    }

    /// Retrieve aggregate runtime telemetry
    pub async fn telemetry(&self) -> FaasPoolTelemetry {
        let invocations = self.total_invocations.load(Ordering::Relaxed);
        let exec_us = self.total_execution_time_us.load(Ordering::Relaxed);
        let avg_latency = if invocations > 0 {
            exec_us as f64 / invocations as f64
        } else {
            0.0
        };

        let active_tenants = self.tenants.read().await.len();
        let cached_modules = self.module_cache.read().await.len();
        let registered_snapshots = self.snapshot_store.read().await.len();

        FaasPoolTelemetry {
            total_invocations: invocations,
            total_execution_time_us: exec_us,
            avg_latency_us: avg_latency,
            pre_hits: self.pre_hits.load(Ordering::Relaxed),
            aot_hits: self.aot_hits.load(Ordering::Relaxed),
            cold_compilations: self.cold_compilations.load(Ordering::Relaxed),
            snapshots_created: self.snapshots_created.load(Ordering::Relaxed),
            snapshots_restored: self.snapshots_restored.load(Ordering::Relaxed),
            traps_gas_exhausted: self.traps_gas_exhausted.load(Ordering::Relaxed),
            traps_total: self.traps_total.load(Ordering::Relaxed),
            active_tenants,
            cached_modules_count: cached_modules,
            registered_snapshots_count: registered_snapshots,
            pooling_enabled: self.pooling_active,
        }
    }

    /// List active tenant metadata
    pub async fn list_active_tenants(&self) -> Vec<TenantFaasSessionInfo> {
        let lock = self.tenants.read().await;
        lock.values()
            .map(|ctx| TenantFaasSessionInfo {
                tenant_id: ctx.tenant_id.clone(),
                created_at_epoch_ms: ctx.created_at_epoch_ms,
                active_invocations: ctx.active_invocations.load(Ordering::Relaxed),
                total_invocations: ctx.total_invocations.load(Ordering::Relaxed),
                total_fuel_consumed: ctx.total_fuel_consumed.load(Ordering::Relaxed),
                max_memory_pages: ctx.policy.max_memory_pages,
                virtual_root: ctx.virtual_root.to_string_lossy().to_string(),
                violations_count: ctx.security_violations.lock().map(|v| v.len()).unwrap_or(0),
            })
            .collect()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_faas_pool_initialization_and_pre_compilation() {
        let temp_dir = tempdir().expect("temp dir");
        let config = FaasPoolConfig::default()
            .with_instances(64)
            .with_memory_pages(256)
            .with_pooling(true)
            .with_aot_cache_dir(temp_dir.path().join("aot_cache"));

        let pool = WasmtimeMultitenantFaasPool::with_config_and_base(config, temp_dir.path().join("tenants"))
            .expect("Failed to initialize FaaS pool");

        assert!(pool.is_pooling_enabled(), "Instance pooling allocator should be active");

        let wat_code = r#"
            (module
                (memory (export "memory") 1)
                (func (export "run") (result i32)
                    i32.const 42
                )
            )
        "#;

        // First invocation: Cold JIT compilation + InstancePre caching
        let report1 = pool
            .execute_faas("tenant_alpha", LanguageKind::Wat, wat_code, Some("run"), None, None, None)
            .await
            .expect("Execution 1 failed");

        assert!(report1.is_success());
        assert_eq!(report1.exit_code, 42);
        assert_eq!(report1.cache_tier, FaasCacheTier::ColdJitCompilation);
        assert!(report1.security_attestation.verify(), "Attestation signature must be valid");

        // Second invocation: Instant InstancePre cache hit
        let report2 = pool
            .execute_faas("tenant_alpha", LanguageKind::Wat, wat_code, Some("run"), None, None, None)
            .await
            .expect("Execution 2 failed");

        assert!(report2.is_success());
        assert_eq!(report2.exit_code, 42);
        assert_eq!(report2.cache_tier, FaasCacheTier::InstancePreHit);
        println!(
            "Sub-millisecond spinup latency: {} us (target < 500 us)",
            report2.spinup_duration_us
        );
        assert!(
            report2.spinup_duration_us < 2000,
            "Spin-up duration {} us exceeds ceiling",
            report2.spinup_duration_us
        );
    }

    #[tokio::test]
    async fn test_sub_millisecond_spinup_latency() {
        let temp_dir = tempdir().expect("temp dir");
        let config = FaasPoolConfig::default().with_instances(32);
        let pool = WasmtimeMultitenantFaasPool::with_config_and_base(config, temp_dir.path().join("tenants"))
            .expect("pool init");

        let wat_code = r#"
            (module
                (memory (export "memory") 1)
                (func (export "run") (result i32)
                    i32.const 100
                )
            )
        "#;

        // Warm up cache
        let _ = pool
            .execute_faas("bench_tenant", LanguageKind::Wat, wat_code, Some("run"), None, None, None)
            .await
            .expect("warmup");

        // Execute 5 warm runs and measure spin-up duration
        let mut total_spinup_us = 0u64;
        for i in 0..5 {
            let rep = pool
                .execute_faas("bench_tenant", LanguageKind::Wat, wat_code, Some("run"), None, None, None)
                .await
                .expect("bench run");
            assert_eq!(rep.cache_tier, FaasCacheTier::InstancePreHit);
            total_spinup_us += rep.spinup_duration_us;
            println!("Run {}: spinup = {} us, total latency = {} us", i + 1, rep.spinup_duration_us, rep.total_latency_us);
        }

        let avg_spinup = total_spinup_us as f64 / 5.0;
        println!("Average warm spin-up latency: {:.2} us", avg_spinup);
        assert!(avg_spinup < 1000.0, "Average spin-up must be sub-millisecond");
    }

    #[tokio::test]
    async fn test_memory_snapshot_create_and_instant_restore() {
        let temp_dir = tempdir().expect("temp dir");
        let pool = WasmtimeMultitenantFaasPool::with_config_and_base(
            FaasPoolConfig::default().with_instances(32),
            temp_dir.path().join("tenants"),
        )
        .expect("pool init");

        let wat_code = r#"
            (module
                (memory (export "memory") 1)
                (global $counter (mut i32) (i32.const 10))
                (export "counter" (global $counter))

                ;; Write byte 0xAA at offset 100 and increment counter by 5
                (func (export "init_state") (result i32)
                    (i32.store8 (i32.const 100) (i32.const 170))
                    (global.set $counter (i32.add (global.get $counter) (i32.const 5)))
                    global.get $counter
                )

                ;; Verify offset 100 is still 0xAA and add another 1 to counter
                (func (export "verify_and_step") (result i32)
                    (i32.store8 (i32.const 101) (i32.load8_u (i32.const 100)))
                    (global.set $counter (i32.add (global.get $counter) (i32.const 1)))
                    global.get $counter
                )
            )
        "#;

        // 1. Initialize and capture snapshot
        let (init_rep, snapshot) = pool
            .execute_and_snapshot(
                "faas_workflow_tenant",
                LanguageKind::Wat,
                wat_code,
                "init_state",
                "workflow_step_1",
            )
            .await
            .expect("Init and snapshot failed");

        assert_eq!(init_rep.exit_code, 15); // 10 + 5
        assert!(snapshot.verify_integrity());
        assert_eq!(snapshot.memory_bytes[100], 170); // 0xAA

        // 2. Instant-restore from snapshot and execute next step
        let restore_start = Instant::now();
        let step_rep = pool
            .execute_faas_from_snapshot(
                "faas_workflow_tenant",
                "workflow_step_1",
                Some("verify_and_step"),
                None,
                None,
                None,
            )
            .await
            .expect("Execution from snapshot failed");
        let restore_elapsed_us = restore_start.elapsed().as_micros();

        println!("Restore & execution duration: {} us", restore_elapsed_us);
        assert_eq!(step_rep.cache_tier, FaasCacheTier::SnapshotRestored);
        assert_eq!(step_rep.exit_code, 16); // 15 + 1
        assert!(step_rep.is_success());
        assert!(step_rep.security_attestation.verify());

        // 3. Repeat restore from the same snapshot to verify idempotent instant-restore
        let step_rep2 = pool
            .execute_faas_from_snapshot(
                "faas_workflow_tenant",
                "workflow_step_1",
                Some("verify_and_step"),
                None,
                None,
                None,
            )
            .await
            .expect("Repeat restore failed");

        assert_eq!(step_rep2.exit_code, 16); // Fresh instance from snapshot returns 15 + 1 = 16 again
    }

    #[tokio::test]
    async fn test_gas_exhaustion_trapping() {
        let temp_dir = tempdir().expect("temp dir");
        let pool = WasmtimeMultitenantFaasPool::with_config_and_base(
            FaasPoolConfig::default().with_instances(16),
            temp_dir.path().join("tenants"),
        )
        .expect("pool init");

        // Infinite loop in WebAssembly
        let wat_infinite = r#"
            (module
                (memory (export "memory") 1)
                (func (export "run") (result i32)
                    (loop $spin
                        (br $spin)
                    )
                    i32.const 0
                )
            )
        "#;

        let low_gas_limit = 20_000;
        let report = pool
            .execute_faas(
                "runaway_tenant",
                LanguageKind::Wat,
                wat_infinite,
                Some("run"),
                None,
                Some(low_gas_limit),
                None,
            )
            .await
            .expect("Execution join failed");

        assert!(report.trapped, "Execution must trap on infinite loop");
        assert!(report.is_gas_exhausted(), "Trap reason must be gas exhaustion");
        assert_eq!(report.fuel_remaining, 0);
        assert!(
            report
                .error_diagnostic
                .as_deref()
                .unwrap()
                .contains("GasLimitExceeded"),
            "Diagnostic must mention GasLimitExceeded"
        );
    }

    #[tokio::test]
    async fn test_multi_tenant_isolation_and_quotas() {
        let temp_dir = tempdir().expect("temp dir");
        let pool = WasmtimeMultitenantFaasPool::with_config_and_base(
            FaasPoolConfig::default().with_instances(32),
            temp_dir.path().join("tenants"),
        )
        .expect("pool init");

        let policy_a = TenantFaasPolicy::new("tenant_alice")
            .with_max_memory_pages(64)
            .with_gas_budget(1_000_000);
        let policy_b = TenantFaasPolicy::new("tenant_bob")
            .with_max_memory_pages(32)
            .with_gas_budget(2_000_000);

        let _ctx_a = pool.get_or_create_tenant("tenant_alice", Some(policy_a)).await.expect("ctx a");
        let _ctx_b = pool.get_or_create_tenant("tenant_bob", Some(policy_b)).await.expect("ctx b");

        // Alice creates a snapshot
        let wat_alice = r#"
            (module
                (memory (export "memory") 1)
                (func (export "init") (result i32)
                    (i32.store (i32.const 0) (i32.const 777))
                    i32.const 777
                )
            )
        "#;

        let (_rep, _snap) = pool
            .execute_and_snapshot("tenant_alice", LanguageKind::Wat, wat_alice, "init", "alice_secret_snap")
            .await
            .expect("alice snap");

        // Bob attempts to restore Alice's snapshot -> Security violation
        let bob_attempt = pool
            .execute_faas_from_snapshot("tenant_bob", "alice_secret_snap", Some("init"), None, None, None)
            .await;

        assert!(bob_attempt.is_err(), "Bob must NOT be permitted to restore Alice's snapshot");
        match bob_attempt {
            Err(HgbError::Security(msg)) => {
                assert!(msg.contains("unauthorized to restore snapshot"));
            }
            other => panic!("Expected HgbError::Security, got {:?}", other),
        }

        // Bob runs a legitimate invocation in his own isolated sandbox
        let bob_rep = pool
            .execute_faas("tenant_bob", LanguageKind::Wat, wat_alice, Some("init"), None, None, None)
            .await
            .expect("bob task");
        assert_eq!(bob_rep.exit_code, 777);
        assert!(bob_rep.is_success());

        // Verify Telemetry
        let tele = pool.telemetry().await;
        assert!(tele.total_invocations >= 2);
        assert!(tele.snapshots_created >= 1);
        assert_eq!(tele.active_tenants, 2);
        assert!(tele.pooling_enabled);
    }

    #[tokio::test]
    async fn test_polyglot_and_streaming_capture() {
        let temp_dir = tempdir().expect("temp dir");
        let pool = WasmtimeMultitenantFaasPool::with_config_and_base(
            FaasPoolConfig::default().with_instances(16),
            temp_dir.path().join("tenants"),
        )
        .expect("pool init");

        let js_code = r#"
            console.log("Hello from Hagibis Edge FaaS!");
            console.error("Diagnostic log stream");
        "#;

        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(16);

        let report = pool
            .execute_faas(
                "polyglot_tenant",
                LanguageKind::JavaScript,
                js_code,
                Some("run"),
                None,
                None,
                Some(stream_tx),
            )
            .await
            .expect("JS execution failed");

        assert!(report.is_success());
        assert!(report.stdout.contains("Hello from Hagibis Edge FaaS!"));
        assert!(report.stderr.contains("Diagnostic log stream"));

        let chunk1 = stream_rx.try_recv().expect("chunk 1");
        assert!(chunk1.chunk.contains("Hello from Hagibis Edge FaaS!"));
    }
}
