//! # Architectural Power-Up I: Close the Loop with Native Linux Kernel Integration
//!
//! Direct, production-grade Linux kernel and hardware optimization loop interfacing
//! `hgb-daemon` with `dynatune` and `kernel-optimizer`:
//! - Reliable sibling path discovery (`dynatune`, `kernel-optimizer`, `dynabook-kernel-ctl`, plugins)
//! - Dynamic CPU governor & Energy Performance Preference (EPP) scaling
//! - Multi-Gen LRU (MGLRU) proactive memory compaction and aging
//! - Thermal zone watchdog with automatic clamp & hysteresis recovery
//! - Workload burst governor with RAII lease guard and auto-sleep idle decay
//! - Bidirectional Unix domain socket IPC & async in-process channel communication

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::UnixListener as TokioUnixListener;
use tokio::sync::{broadcast, mpsc};

use hgb_core::error::{HgbError, Result};

// ==============================================================================
// 1. Constants & Tuning Invariants
// ==============================================================================

/// Default Unix domain socket path for dynatune IPC
pub const DEFAULT_IPC_SOCKET_PATH: &str = "/run/dynatune.sock";

/// Fallback temporary IPC socket path if `/run` is unprivileged
pub const FALLBACK_IPC_SOCKET_PATH: &str = "/tmp/hgb_dynatune_ipc.sock";

/// Emergency thermal throttle threshold in Celsius (78.0°C)
pub const THERMAL_CLAMP_THRESHOLD_C: f64 = 78.0;

/// Thermal recovery threshold with hysteresis in Celsius (70.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 70.0;

/// Critical battery capacity percentage threshold on discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;

/// Battery recovery capacity percentage with hysteresis (22%)
pub const BATTERY_RECOVERY_DISCHARGE_PCT: u32 = 22;

/// MGLRU burst protection TTL in milliseconds (1000ms) for low-latency memory compaction
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;

/// MGLRU balanced / default idle TTL in milliseconds (0ms)
pub const BALANCED_MGLRU_MIN_TTL_MS: u64 = 0;

/// MGLRU battery conservation TTL in milliseconds (5000ms) to reduce timer interrupts
pub const BATTERY_MGLRU_MIN_TTL_MS: u64 = 5000;

/// Default auto-sleep idle grace period before dropping from burst to balanced/powersave
pub const DEFAULT_IDLE_GRACE_PERIOD_SECS: u64 = 3;

/// Default nominal concurrency limit under healthy thermal conditions
pub const DEFAULT_NOMINAL_CONCURRENCY: usize = 4;

/// Clamped concurrency limit under thermal or critical battery stress
pub const CLAMPED_CONCURRENCY_LIMIT: usize = 1;

// ==============================================================================
// 2. Sibling Path Discovery
// ==============================================================================

/// Discovered filesystem paths for dynatune and kernel-optimizer suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneKernelPaths {
    pub dynatune_dir: Option<PathBuf>,
    pub dynatune_bin: Option<PathBuf>,
    pub kernel_optimizer_dir: Option<PathBuf>,
    pub dynabook_kernel_ctl: Option<PathBuf>,
    pub cpu_governor_sh: Option<PathBuf>,
    pub mglru_monitor_sh: Option<PathBuf>,
    pub battery_care_sh: Option<PathBuf>,
}

impl DynatuneKernelPaths {
    /// Discover paths across environment overrides, relative sibling paths, and standard locations
    pub fn discover(workspace_hint: Option<&Path>) -> Self {
        let mut paths = Self {
            dynatune_dir: None,
            dynatune_bin: None,
            kernel_optimizer_dir: None,
            dynabook_kernel_ctl: None,
            cpu_governor_sh: None,
            mglru_monitor_sh: None,
            battery_care_sh: None,
        };

        // 1. Candidate search directories for dynatune
        let mut dynatune_candidates = Vec::new();
        if let Ok(env_path) = std::env::var("DYNATUNE_PATH") {
            dynatune_candidates.push(PathBuf::from(env_path));
        }
        if let Some(hint) = workspace_hint {
            dynatune_candidates.push(hint.join("../dynatune"));
            dynatune_candidates.push(hint.join("../../dynatune"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            dynatune_candidates.push(cwd.join("../dynatune"));
            dynatune_candidates.push(cwd.join("dynatune"));
        }
        dynatune_candidates.push(PathBuf::from("/home/toshiba/My AI Project/dynatune"));

        for cand in dynatune_candidates {
            if cand.is_dir() {
                paths.dynatune_dir = Some(cand.clone());
                // Look for binary in target/release or target/debug
                let release_bin = cand.join("target/release/dynatune");
                let debug_bin = cand.join("target/debug/dynatune");
                if release_bin.is_file() {
                    paths.dynatune_bin = Some(release_bin);
                } else if debug_bin.is_file() {
                    paths.dynatune_bin = Some(debug_bin);
                }
                break;
            } else if cand.is_file() {
                paths.dynatune_bin = Some(cand.clone());
                if let Some(parent) = cand.parent() {
                    paths.dynatune_dir = Some(parent.to_path_buf());
                }
                break;
            }
        }

        // 2. Candidate search directories for kernel-optimizer
        let mut ko_candidates = Vec::new();
        if let Ok(env_path) = std::env::var("KERNEL_OPTIMIZER_PATH") {
            ko_candidates.push(PathBuf::from(env_path));
        }
        if let Some(hint) = workspace_hint {
            ko_candidates.push(hint.join("../kernel-optimizer"));
            ko_candidates.push(hint.join("../../kernel-optimizer"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            ko_candidates.push(cwd.join("../kernel-optimizer"));
            ko_candidates.push(cwd.join("kernel-optimizer"));
        }
        ko_candidates.push(PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer"));

        for cand in ko_candidates {
            if cand.is_dir() {
                paths.kernel_optimizer_dir = Some(cand.clone());

                let ctl_bin = cand.join("dynabook-kernel-ctl");
                if ctl_bin.is_file() {
                    paths.dynabook_kernel_ctl = Some(ctl_bin);
                }

                let cpu_gov = cand.join("plugins/cpu-governor.sh");
                if cpu_gov.is_file() {
                    paths.cpu_governor_sh = Some(cpu_gov);
                }

                let mglru = cand.join("plugins/mglru-monitor.sh");
                if mglru.is_file() {
                    paths.mglru_monitor_sh = Some(mglru);
                }

                let bat = cand.join("plugins/battery-care.sh");
                if bat.is_file() {
                    paths.battery_care_sh = Some(bat);
                }
                break;
            } else if cand.is_file() && cand.file_name().map(|n| n == "dynabook-kernel-ctl").unwrap_or(false) {
                paths.dynabook_kernel_ctl = Some(cand.clone());
                if let Some(parent) = cand.parent() {
                    paths.kernel_optimizer_dir = Some(parent.to_path_buf());
                    let cpu_gov = parent.join("plugins/cpu-governor.sh");
                    if cpu_gov.is_file() {
                        paths.cpu_governor_sh = Some(cpu_gov);
                    }
                    let mglru = parent.join("plugins/mglru-monitor.sh");
                    if mglru.is_file() {
                        paths.mglru_monitor_sh = Some(mglru);
                    }
                }
                break;
            }
        }

        // 3. Fallbacks to system installations
        if paths.dynabook_kernel_ctl.is_none() {
            let sys_ctl = PathBuf::from("/usr/local/bin/dynabook-kernel-ctl");
            if sys_ctl.is_file() {
                paths.dynabook_kernel_ctl = Some(sys_ctl);
            }
        }
        if paths.cpu_governor_sh.is_none() {
            let sys_gov = PathBuf::from("/usr/lib/dynabook-kernel/plugins/cpu-governor.sh");
            if sys_gov.is_file() {
                paths.cpu_governor_sh = Some(sys_gov);
            }
        }
        if paths.mglru_monitor_sh.is_none() {
            let sys_mglru = PathBuf::from("/usr/lib/dynabook-kernel/plugins/mglru-monitor.sh");
            if sys_mglru.is_file() {
                paths.mglru_monitor_sh = Some(sys_mglru);
            }
        }

        paths
    }

    /// Check if dynatune workspace or binary is available
    pub fn is_dynatune_available(&self) -> bool {
        self.dynatune_dir.is_some() || self.dynatune_bin.is_some()
    }

    /// Check if kernel-optimizer suite is available
    pub fn is_kernel_optimizer_available(&self) -> bool {
        self.kernel_optimizer_dir.is_some() || self.dynabook_kernel_ctl.is_some()
    }

    /// Return a human-readable summary of discovered paths
    pub fn summary(&self) -> String {
        format!(
            "dynatune: {}, ctl: {}, cpu-gov: {}, mglru: {}",
            self.dynatune_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()),
            self.dynabook_kernel_ctl.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()),
            self.cpu_governor_sh.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()),
            self.mglru_monitor_sh.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into())
        )
    }
}

// ==============================================================================
// 3. Kernel Profiles & Hardware Data Models
// ==============================================================================

/// Target hardware profile mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KernelProfile {
    /// High-performance burst mode: governor `performance`, EPP `performance`, turbo on, MGLRU 1000ms
    Performance,
    /// Balanced day-to-day mode: governor `powersave`, EPP `balance_performance`, turbo on, MGLRU 0ms
    BalancePerformance,
    /// Energy efficiency / auto-sleep mode: governor `powersave`, EPP `balance_power`, turbo on, MGLRU 0ms
    PowerSave,
    /// Maximum power conservation: governor `powersave`, EPP `power`, turbo off, MGLRU 5000ms
    Power,
    /// Emergency thermal or battery clamp: governor `powersave`, EPP `power`, turbo off, MGLRU 0ms, concurrency: 1
    ThermalThrottled,
}

impl KernelProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::BalancePerformance => "balance_performance",
            Self::PowerSave => "powersave",
            Self::Power => "power",
            Self::ThermalThrottled => "thermal_throttled",
        }
    }

    pub fn governor_name(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::BalancePerformance | Self::PowerSave | Self::Power | Self::ThermalThrottled => "powersave",
        }
    }

    pub fn epp_name(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::BalancePerformance => "balance_performance",
            Self::PowerSave => "balance_power",
            Self::Power | Self::ThermalThrottled => "power",
        }
    }

    pub fn turbo_enabled(&self) -> bool {
        match self {
            Self::Performance | Self::BalancePerformance | Self::PowerSave => true,
            Self::Power | Self::ThermalThrottled => false,
        }
    }

    pub fn mglru_min_ttl_ms(&self) -> u64 {
        match self {
            Self::Performance => BURST_MGLRU_MIN_TTL_MS,
            Self::BalancePerformance | Self::PowerSave | Self::ThermalThrottled => BALANCED_MGLRU_MIN_TTL_MS,
            Self::Power => BATTERY_MGLRU_MIN_TTL_MS,
        }
    }

    pub fn script_profile_arg(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::BalancePerformance => "balanced",
            Self::PowerSave | Self::Power | Self::ThermalThrottled => "battery",
        }
    }

    pub fn concurrency_limit(&self) -> usize {
        match self {
            Self::ThermalThrottled => CLAMPED_CONCURRENCY_LIMIT,
            _ => DEFAULT_NOMINAL_CONCURRENCY,
        }
    }
}

impl std::fmt::Display for KernelProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Workloads that trigger automatic burst acceleration
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BurstWorkload {
    /// Heavy compilation (`cargo build`, Wasm compilation, gcc/clang)
    HeavyCompilation,
    /// Vector database indexing and embedding batches
    VectorIndexing,
    /// Local LLM inference, speculative decoding, and prompt ingestion
    LlmInference,
    /// Autonomous agent self-healing loops and AST transformations
    AutonomousLoop,
    /// Firecracker microVM spin-up and sandbox creation
    MicroVmSpinup,
    /// Custom workload identifier
    Custom(String),
}

impl BurstWorkload {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().replace('-', "_").as_str() {
            "compilation" | "build" | "cargo_build" | "wasm_compilation" | "heavy_compilation" => {
                Self::HeavyCompilation
            }
            "vector" | "vector_indexing" | "embedding" | "rag" => Self::VectorIndexing,
            "llm" | "inference" | "llm_inference" | "local_llm" => Self::LlmInference,
            "autonomous" | "autonomous_loop" | "self_healing" => Self::AutonomousLoop,
            "microvm" | "microvm_spinup" | "firecracker" => Self::MicroVmSpinup,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::HeavyCompilation => "heavy_compilation",
            Self::VectorIndexing => "vector_indexing",
            Self::LlmInference => "llm_inference",
            Self::AutonomousLoop => "autonomous_loop",
            Self::MicroVmSpinup => "microvm_spinup",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for BurstWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Linux Multi-Gen LRU (MGLRU) Subsystem telemetry and decoded flags
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MglruStatus {
    pub available: bool,
    pub enabled_raw: String,
    pub enabled_flags: u32,
    pub eviction_active: bool,
    pub file_page_protection: bool,
    pub page_age_pulling: bool,
    pub min_ttl_ms: u64,
    pub reclaim_efficiency: Option<String>,
}

/// Thermal zone metric query result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalZone {
    pub name: String,
    pub zone_type: String,
    pub temp_c: f64,
    pub temp_milli_c: i64,
}

/// Comprehensive thermal watchdog snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalSnapshot {
    pub zones: Vec<ThermalZone>,
    pub cpu_pkg_temp_c: f64,
    pub max_temp_c: f64,
    pub is_clamped: bool,
    pub status_description: String,
}

/// Host power supply and battery telemetry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatterySnapshot {
    pub ac_connected: bool,
    pub battery_present: bool,
    pub capacity_pct: u32,
    pub status: String,
    pub is_discharging: bool,
}

/// CPU hardware scaling frequency and governor status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuTuningState {
    pub scaling_driver: String,
    pub active_governor: String,
    pub active_epp: String,
    pub turbo_enabled: bool,
    pub core_frequencies_mhz: Vec<f64>,
}

/// Complete status report of the Kernel-Dynatune IPC Loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelLoopStatus {
    pub current_profile: KernelProfile,
    pub target_profile: KernelProfile,
    pub active_burst_tasks: usize,
    pub is_thermally_clamped: bool,
    pub clamp_reason: Option<String>,
    pub cpu: CpuTuningState,
    pub mglru: MglruStatus,
    pub thermal: ThermalSnapshot,
    pub battery: BatterySnapshot,
    pub paths: DynatuneKernelPaths,
    pub total_burst_ramp_ups: u64,
    pub total_idle_drops: u64,
    pub total_thermal_clamps: u64,
    pub uptime_secs: u64,
    pub timestamp_ms: u64,
}

// ==============================================================================
// 4. IPC Protocol Messages
// ==============================================================================

/// High-level IPC commands dispatched over the Unix domain socket or internal channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload")]
pub enum KernelIpcCommand {
    /// Retrieve full status and telemetry
    GetStatus,
    /// Apply an explicit profile
    SetProfile { profile: KernelProfile },
    /// Apply CPU Energy Performance Preference
    SetEpp { epp: String },
    /// Apply CPU scaling governor
    SetGovernor { governor: String },
    /// Set MGLRU min_ttl_ms proactive compaction window
    SetMglruTtl { min_ttl_ms: u64 },
    /// Acquire a workload burst lease (locks high frequency)
    AcquireBurst { workload: BurstWorkload },
    /// Release a workload burst lease
    ReleaseBurst { lease_id: String },
    /// Trigger script profile via mglru-monitor.sh or cpu-governor.sh
    TriggerScriptProfile { profile: String },
    /// Ping daemon for health and latency
    Ping,
}

/// Response returned from IPC command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelIpcResponse {
    pub success: bool,
    pub message: String,
    pub applied_profile: Option<KernelProfile>,
    pub lease_id: Option<String>,
    pub status: Option<KernelLoopStatus>,
    pub timestamp_ms: u64,
}

impl KernelIpcResponse {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            applied_profile: None,
            lease_id: None,
            status: None,
            timestamp_ms: current_epoch_ms(),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            applied_profile: None,
            lease_id: None,
            status: None,
            timestamp_ms: current_epoch_ms(),
        }
    }
}

// ==============================================================================
// 5. Configuration & Telemetry Overrides
// ==============================================================================

/// Configuration parameters for KernelDynatuneLoop
#[derive(Debug, Clone)]
pub struct KernelDynatuneConfig {
    pub sysfs_root: PathBuf,
    pub procfs_root: PathBuf,
    pub socket_path: PathBuf,
    pub idle_grace_period: Duration,
    pub thermal_clamp_threshold_c: f64,
    pub thermal_recovery_threshold_c: f64,
    pub battery_critical_threshold_pct: u32,
    pub auto_throttle_enabled: bool,
}

impl Default for KernelDynatuneConfig {
    fn default() -> Self {
        let socket_path = PathBuf::from(DEFAULT_IPC_SOCKET_PATH);
        Self {
            sysfs_root: PathBuf::from("/"),
            procfs_root: PathBuf::from("/"),
            socket_path,
            idle_grace_period: Duration::from_secs(DEFAULT_IDLE_GRACE_PERIOD_SECS),
            thermal_clamp_threshold_c: THERMAL_CLAMP_THRESHOLD_C,
            thermal_recovery_threshold_c: THERMAL_RECOVERY_THRESHOLD_C,
            battery_critical_threshold_pct: BATTERY_CRITICAL_DISCHARGE_PCT,
            auto_throttle_enabled: true,
        }
    }
}

/// Telemetry override hook for deterministic testing
#[derive(Debug, Clone, Default)]
pub struct TelemetryOverride {
    pub cpu_pkg_temp_c: Option<f64>,
    pub battery_pct: Option<u32>,
    pub is_discharging: Option<bool>,
    pub ac_connected: Option<bool>,
}

// ==============================================================================
// 6. KernelDynatuneLoop Implementation
// ==============================================================================

/// Primary Controller bridging `hgb-daemon` with native Linux kernel & dynatune
pub struct KernelDynatuneLoop {
    config: KernelDynatuneConfig,
    paths: DynatuneKernelPaths,
    current_profile: Arc<RwLock<KernelProfile>>,
    target_profile: Arc<RwLock<KernelProfile>>,
    active_burst_tasks: Arc<AtomicUsize>,
    burst_registry: Arc<RwLock<HashMap<String, BurstWorkload>>>,
    is_thermally_clamped: Arc<AtomicBool>,
    thermal_clamp_reason: Arc<RwLock<Option<String>>>,
    idle_generation: Arc<AtomicU64>,
    total_burst_ramp_ups: Arc<AtomicU64>,
    total_idle_drops: Arc<AtomicU64>,
    total_thermal_clamps: Arc<AtomicU64>,
    started_at: Instant,
    telemetry_override: Arc<RwLock<Option<TelemetryOverride>>>,
    active_governor_cache: Arc<RwLock<String>>,
    active_epp_cache: Arc<RwLock<String>>,
    active_turbo_cache: Arc<AtomicBool>,
    active_mglru_min_ttl_cache: Arc<AtomicU64>,
}

impl KernelDynatuneLoop {
    /// Create a new KernelDynatuneLoop with default auto-discovered paths
    pub fn new() -> Arc<Self> {
        let paths = DynatuneKernelPaths::discover(None);
        Self::with_config(KernelDynatuneConfig::default(), paths)
    }

    /// Create with explicit configuration and discovered paths
    pub fn with_config(config: KernelDynatuneConfig, paths: DynatuneKernelPaths) -> Arc<Self> {
        let loop_inst = Arc::new(Self {
            config,
            paths,
            current_profile: Arc::new(RwLock::new(KernelProfile::BalancePerformance)),
            target_profile: Arc::new(RwLock::new(KernelProfile::BalancePerformance)),
            active_burst_tasks: Arc::new(AtomicUsize::new(0)),
            burst_registry: Arc::new(RwLock::new(HashMap::new())),
            is_thermally_clamped: Arc::new(AtomicBool::new(false)),
            thermal_clamp_reason: Arc::new(RwLock::new(None)),
            idle_generation: Arc::new(AtomicU64::new(0)),
            total_burst_ramp_ups: Arc::new(AtomicU64::new(0)),
            total_idle_drops: Arc::new(AtomicU64::new(0)),
            total_thermal_clamps: Arc::new(AtomicU64::new(0)),
            started_at: Instant::now(),
            telemetry_override: Arc::new(RwLock::new(None)),
            active_governor_cache: Arc::new(RwLock::new("powersave".to_string())),
            active_epp_cache: Arc::new(RwLock::new("balance_performance".to_string())),
            active_turbo_cache: Arc::new(AtomicBool::new(true)),
            active_mglru_min_ttl_cache: Arc::new(AtomicU64::new(BALANCED_MGLRU_MIN_TTL_MS)),
        });

        // Initialize state from sysfs
        loop_inst.sync_initial_state();

        loop_inst
    }

    /// Create an isolated loop for tests with mock sysfs root and test socket
    pub fn for_test(sysfs_root: impl AsRef<Path>, socket_path: impl AsRef<Path>) -> Arc<Self> {
        let mut config = KernelDynatuneConfig::default();
        config.sysfs_root = sysfs_root.as_ref().to_path_buf();
        config.procfs_root = sysfs_root.as_ref().to_path_buf();
        config.socket_path = socket_path.as_ref().to_path_buf();
        config.idle_grace_period = Duration::from_millis(50);

        let paths = DynatuneKernelPaths {
            dynatune_dir: None,
            dynatune_bin: None,
            kernel_optimizer_dir: None,
            dynabook_kernel_ctl: None,
            cpu_governor_sh: None,
            mglru_monitor_sh: None,
            battery_care_sh: None,
        };

        Self::with_config(config, paths)
    }

    /// Read discovered paths
    pub fn paths(&self) -> &DynatuneKernelPaths {
        &self.paths
    }

    /// Read active configuration
    pub fn config(&self) -> &KernelDynatuneConfig {
        &self.config
    }

    /// Override telemetry for deterministic testing
    pub fn set_telemetry_override(&self, ov: Option<TelemetryOverride>) {
        let mut guard = self.telemetry_override.write().unwrap();
        *guard = ov;
    }

    /// Read current active profile
    pub fn current_profile(&self) -> KernelProfile {
        *self.current_profile.read().unwrap()
    }

    /// Synchronize initial cache with live sysfs values
    fn sync_initial_state(&self) {
        let cpu = self.read_cpu_state();
        if !cpu.active_governor.is_empty() && cpu.active_governor != "unknown" {
            *self.active_governor_cache.write().unwrap() = cpu.active_governor;
        }
        if !cpu.active_epp.is_empty() && cpu.active_epp != "unknown" {
            *self.active_epp_cache.write().unwrap() = cpu.active_epp;
        }
        self.active_turbo_cache.store(cpu.turbo_enabled, Ordering::Relaxed);

        let mglru = self.read_mglru_status();
        if mglru.available {
            self.active_mglru_min_ttl_cache.store(mglru.min_ttl_ms, Ordering::Relaxed);
        }
    }

    // ==========================================================================
    // Hardware Integration: CPU Governor & EPP
    // ==========================================================================

    /// Apply dynamic CPU governor and EPP across all available cores via sysfs and script fallback
    pub fn apply_profile(&self, profile: KernelProfile) -> Result<()> {
        *self.target_profile.write().unwrap() = profile;

        let gov = profile.governor_name();
        let epp = profile.epp_name();
        let turbo = profile.turbo_enabled();
        let ttl = profile.mglru_min_ttl_ms();

        // 1. Sysfs write: Governor & EPP
        let mut sysfs_written = false;
        let cpu_dir = self.config.sysfs_root.join("sys/devices/system/cpu");
        if cpu_dir.is_dir() {
            // cpufreq policies or individual cores
            let cpufreq_policies = cpu_dir.join("cpufreq");
            if cpufreq_policies.is_dir() {
                if let Ok(entries) = fs::read_dir(&cpufreq_policies) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                            let _ = fs::write(p.join("scaling_governor"), gov);
                            let _ = fs::write(p.join("energy_performance_preference"), epp);
                            sysfs_written = true;
                        }
                    }
                }
            }

            // Fallback: iterate cpu0, cpu1...
            if let Ok(entries) = fs::read_dir(&cpu_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                        let cpufreq = entry.path().join("cpufreq");
                        if cpufreq.is_dir() {
                            let _ = fs::write(cpufreq.join("scaling_governor"), gov);
                            let _ = fs::write(cpufreq.join("energy_performance_preference"), epp);
                            sysfs_written = true;
                        }
                    }
                }
            }

            // Intel Turbo Boost
            let no_turbo_path = cpu_dir.join("intel_pstate/no_turbo");
            let turbo_val = if turbo { "0\n" } else { "1\n" };
            let _ = fs::write(no_turbo_path, turbo_val);
        }

        // 2. Sysfs write: MGLRU min_ttl_ms
        let mglru_ttl_path = self.config.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        if mglru_ttl_path.exists() {
            let _ = fs::write(&mglru_ttl_path, format!("{}\n", ttl));
        }

        // 3. Fallback to scripts if sysfs direct writes are not accessible (e.g. running unprivileged)
        if !sysfs_written {
            let script_arg = profile.script_profile_arg();
            if let Some(ref cpu_gov) = self.paths.cpu_governor_sh {
                let _ = Command::new("bash")
                    .arg(cpu_gov)
                    .arg("profile")
                    .arg(script_arg)
                    .output();
            } else if let Some(ref ctl) = self.paths.dynabook_kernel_ctl {
                let _ = Command::new(ctl)
                    .arg("profile")
                    .arg(script_arg)
                    .output();
            }

            if let Some(ref mglru_sh) = self.paths.mglru_monitor_sh {
                let _ = Command::new("bash")
                    .arg(mglru_sh)
                    .arg("profile")
                    .arg(script_arg)
                    .output();
            }
        }

        // Update in-memory state
        *self.active_governor_cache.write().unwrap() = gov.to_string();
        *self.active_epp_cache.write().unwrap() = epp.to_string();
        self.active_turbo_cache.store(turbo, Ordering::Relaxed);
        self.active_mglru_min_ttl_cache.store(ttl, Ordering::Relaxed);
        *self.current_profile.write().unwrap() = profile;

        Ok(())
    }

    /// Set CPU scaling governor explicitly
    pub fn set_cpu_governor(&self, governor: &str) -> Result<()> {
        let cpu_dir = self.config.sysfs_root.join("sys/devices/system/cpu");
        let mut wrote = false;

        let cpufreq_policies = cpu_dir.join("cpufreq");
        if cpufreq_policies.is_dir() {
            if let Ok(entries) = fs::read_dir(&cpufreq_policies) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        if fs::write(p.join("scaling_governor"), governor).is_ok() {
                            wrote = true;
                        }
                    }
                }
            }
        }

        if !wrote {
            if let Some(ref cpu_gov) = self.paths.cpu_governor_sh {
                let _ = Command::new("bash")
                    .arg(cpu_gov)
                    .arg("set-governor")
                    .arg(governor)
                    .output();
            }
        }

        *self.active_governor_cache.write().unwrap() = governor.to_string();
        Ok(())
    }

    /// Set CPU Energy Performance Preference (EPP) explicitly
    pub fn set_cpu_epp(&self, epp: &str) -> Result<()> {
        let cpu_dir = self.config.sysfs_root.join("sys/devices/system/cpu");
        let mut wrote = false;

        let cpufreq_policies = cpu_dir.join("cpufreq");
        if cpufreq_policies.is_dir() {
            if let Ok(entries) = fs::read_dir(&cpufreq_policies) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        if fs::write(p.join("energy_performance_preference"), epp).is_ok() {
                            wrote = true;
                        }
                    }
                }
            }
        }

        if !wrote {
            if let Some(ref cpu_gov) = self.paths.cpu_governor_sh {
                let _ = Command::new("bash")
                    .arg(cpu_gov)
                    .arg("set-epp")
                    .arg(epp)
                    .output();
            }
        }

        *self.active_epp_cache.write().unwrap() = epp.to_string();
        Ok(())
    }

    /// Set Intel Turbo Boost state
    pub fn set_turbo(&self, enabled: bool) -> Result<()> {
        let no_turbo_path = self.config.sysfs_root.join("sys/devices/system/cpu/intel_pstate/no_turbo");
        let val = if enabled { "0\n" } else { "1\n" };
        let _ = fs::write(no_turbo_path, val);
        self.active_turbo_cache.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    /// Read live CPU tuning state from sysfs
    pub fn read_cpu_state(&self) -> CpuTuningState {
        let cpu_dir = self.config.sysfs_root.join("sys/devices/system/cpu");
        let mut driver = "unknown".to_string();
        let mut governor = self.active_governor_cache.read().unwrap().clone();
        let mut epp = self.active_epp_cache.read().unwrap().clone();
        let mut freqs = Vec::new();

        // 1. Try policy0
        let policy0 = cpu_dir.join("cpufreq/policy0");
        if policy0.is_dir() {
            if let Ok(d) = fs::read_to_string(policy0.join("scaling_driver")) {
                driver = d.trim().to_string();
            }
            if let Ok(g) = fs::read_to_string(policy0.join("scaling_governor")) {
                governor = g.trim().to_string();
            }
            if let Ok(e) = fs::read_to_string(policy0.join("energy_performance_preference")) {
                epp = e.trim().to_string();
            }
        } else {
            // Try cpu0/cpufreq
            let cpu0 = cpu_dir.join("cpu0/cpufreq");
            if cpu0.is_dir() {
                if let Ok(d) = fs::read_to_string(cpu0.join("scaling_driver")) {
                    driver = d.trim().to_string();
                }
                if let Ok(g) = fs::read_to_string(cpu0.join("scaling_governor")) {
                    governor = g.trim().to_string();
                }
                if let Ok(e) = fs::read_to_string(cpu0.join("energy_performance_preference")) {
                    epp = e.trim().to_string();
                }
            }
        }

        // Frequencies across cores
        if let Ok(entries) = fs::read_dir(&cpu_dir) {
            let mut cpu_nodes: Vec<_> = entries
                .flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with("cpu") && n[3..].chars().all(|c| c.is_ascii_digit())
                })
                .collect();
            cpu_nodes.sort_by_key(|e| e.file_name());

            for node in cpu_nodes {
                let freq_file = node.path().join("cpufreq/scaling_cur_freq");
                if let Ok(content) = fs::read_to_string(freq_file) {
                    if let Ok(khz) = content.trim().parse::<f64>() {
                        freqs.push(khz / 1000.0);
                    }
                }
            }
        }

        let turbo = fs::read_to_string(cpu_dir.join("intel_pstate/no_turbo"))
            .map(|s| s.trim() == "0")
            .unwrap_or_else(|_| self.active_turbo_cache.load(Ordering::Relaxed));

        CpuTuningState {
            scaling_driver: driver,
            active_governor: governor,
            active_epp: epp,
            turbo_enabled: turbo,
            core_frequencies_mhz: freqs,
        }
    }

    // ==========================================================================
    // Hardware Integration: Multi-Gen LRU (MGLRU)
    // ==========================================================================

    /// Read MGLRU flags and proactive compaction status from `/sys/kernel/mm/lru_gen`
    pub fn read_mglru_status(&self) -> MglruStatus {
        let mglru_dir = self.config.sysfs_root.join("sys/kernel/mm/lru_gen");
        let enabled_path = mglru_dir.join("enabled");

        if !enabled_path.exists() {
            return MglruStatus {
                available: false,
                enabled_raw: "Not compiled / Disabled".to_string(),
                enabled_flags: 0,
                eviction_active: false,
                file_page_protection: false,
                page_age_pulling: false,
                min_ttl_ms: 0,
                reclaim_efficiency: None,
            };
        }

        let enabled_raw = fs::read_to_string(&enabled_path)
            .unwrap_or_else(|_| "0".into())
            .trim()
            .to_string();

        let flags = if enabled_raw.starts_with("0x") || enabled_raw.starts_with("0X") {
            u32::from_str_radix(&enabled_raw[2..], 16).unwrap_or(0)
        } else {
            enabled_raw.parse::<u32>().unwrap_or(0)
        };

        let eviction_active = (flags & 1) != 0;
        let file_page_protection = (flags & 2) != 0;
        let page_age_pulling = (flags & 4) != 0;

        let min_ttl_ms = fs::read_to_string(mglru_dir.join("min_ttl_ms"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or_else(|| self.active_mglru_min_ttl_cache.load(Ordering::Relaxed));

        // Read vmstat reclaim efficiency if available
        let mut reclaim_efficiency = None;
        let vmstat_path = self.config.procfs_root.join("proc/vmstat");
        if let Ok(vmstat) = fs::read_to_string(vmstat_path) {
            let mut pgscan: u64 = 0;
            let mut pgsteal: u64 = 0;
            for line in vmstat.lines() {
                if line.starts_with("pgscan_") {
                    if let Some(val) = line.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
                        pgscan += val;
                    }
                } else if line.starts_with("pgsteal_") {
                    if let Some(val) = line.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
                        pgsteal += val;
                    }
                }
            }
            if pgscan > 0 {
                let eff = (pgsteal as f64 / pgscan as f64) * 100.0;
                reclaim_efficiency = Some(format!("{:.2}% ({} stolen / {} scanned)", eff, pgsteal, pgscan));
            } else {
                reclaim_efficiency = Some("Optimal (No direct thrashing)".to_string());
            }
        }

        MglruStatus {
            available: true,
            enabled_raw,
            enabled_flags: flags,
            eviction_active,
            file_page_protection,
            page_age_pulling,
            min_ttl_ms,
            reclaim_efficiency,
        }
    }

    /// Set MGLRU `min_ttl_ms` proactive memory compaction window
    pub fn set_mglru_min_ttl(&self, min_ttl_ms: u64) -> Result<()> {
        let ttl_path = self.config.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        if fs::write(&ttl_path, format!("{}\n", min_ttl_ms)).is_err() {
            if let Some(ref mglru_sh) = self.paths.mglru_monitor_sh {
                let _ = Command::new("bash")
                    .arg(mglru_sh)
                    .arg("set-min-ttl")
                    .arg(min_ttl_ms.to_string())
                    .output();
            }
        }
        self.active_mglru_min_ttl_cache.store(min_ttl_ms, Ordering::Relaxed);
        Ok(())
    }

    /// Trigger `mglru-monitor.sh profile [performance|balanced|battery]`
    pub fn apply_mglru_script_profile(&self, profile_name: &str) -> Result<String> {
        if let Some(ref script) = self.paths.mglru_monitor_sh {
            let output = Command::new("bash")
                .arg(script)
                .arg("profile")
                .arg(profile_name)
                .output()
                .map_err(|e| HgbError::Execution(format!("Failed to execute mglru-monitor.sh: {}", e)))?;

            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if !output.status.success() && !stderr.is_empty() {
                return Err(HgbError::Execution(format!("mglru-monitor.sh failed: {}", stderr)));
            }
            Ok(stdout)
        } else {
            // Apply via direct sysfs min_ttl_ms
            let ttl = match profile_name {
                "performance" => BURST_MGLRU_MIN_TTL_MS,
                "battery" => BATTERY_MGLRU_MIN_TTL_MS,
                _ => BALANCED_MGLRU_MIN_TTL_MS,
            };
            self.set_mglru_min_ttl(ttl)?;
            Ok(format!("Direct sysfs set min_ttl_ms={}", ttl))
        }
    }

    // ==========================================================================
    // Hardware Integration: Thermal Watchdog & Safety Clamp
    // ==========================================================================

    /// Read live thermal telemetry across all thermal zones
    pub fn read_thermal_telemetry(&self) -> ThermalSnapshot {
        // Check test override first
        if let Some(ref ov) = *self.telemetry_override.read().unwrap() {
            if let Some(cpu_pkg_temp_c) = ov.cpu_pkg_temp_c {
                let is_clamped = cpu_pkg_temp_c >= self.config.thermal_clamp_threshold_c;
                return ThermalSnapshot {
                    zones: vec![ThermalZone {
                        name: "thermal_zone_override".to_string(),
                        zone_type: "x86_pkg_temp".to_string(),
                        temp_c: cpu_pkg_temp_c,
                        temp_milli_c: (cpu_pkg_temp_c * 1000.0) as i64,
                    }],
                    cpu_pkg_temp_c,
                    max_temp_c: cpu_pkg_temp_c,
                    is_clamped,
                    status_description: if is_clamped {
                        "Clamped (Override Thermal Triggered)".to_string()
                    } else {
                        "Normal (Override)".to_string()
                    },
                };
            }
        }

        let thermal_dir = self.config.sysfs_root.join("sys/class/thermal");
        let mut zones = Vec::new();
        let mut cpu_temp_milli = None;
        let mut max_milli: i64 = 0;

        if let Ok(entries) = fs::read_dir(thermal_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let type_path = entry.path().join("type");
                    let temp_path = entry.path().join("temp");

                    let z_type = fs::read_to_string(&type_path)
                        .unwrap_or_else(|_| "unknown".to_string())
                        .trim()
                        .to_string();

                    if let Ok(raw_temp) = fs::read_to_string(&temp_path) {
                        if let Ok(milli) = raw_temp.trim().parse::<i64>() {
                            let temp_c = milli as f64 / 1000.0;
                            if milli > max_milli {
                                max_milli = milli;
                            }

                            if z_type == "x86_pkg_temp" {
                                cpu_temp_milli = Some(milli);
                            } else if cpu_temp_milli.is_none() && (z_type == "acpitz" || z_type == "cpu-thermal") {
                                cpu_temp_milli = Some(milli);
                            }

                            zones.push(ThermalZone {
                                name,
                                zone_type: z_type,
                                temp_c,
                                temp_milli_c: milli,
                            });
                        }
                    }
                }
            }
        }

        let effective_milli = cpu_temp_milli.unwrap_or(max_milli);
        let cpu_pkg_temp_c = effective_milli as f64 / 1000.0;
        let max_temp_c = max_milli as f64 / 1000.0;

        let is_clamped = self.is_thermally_clamped.load(Ordering::SeqCst);
        let status_desc = if is_clamped {
            "Critical (Thermal Clamp Active - Throttled)".to_string()
        } else if cpu_pkg_temp_c >= self.config.thermal_clamp_threshold_c {
            "Critical (Thermal Limit Exceeded)".to_string()
        } else if cpu_pkg_temp_c >= 70.0 {
            "Warm (Heavy Workload)".to_string()
        } else {
            "Optimal (Cool)".to_string()
        };

        ThermalSnapshot {
            zones,
            cpu_pkg_temp_c,
            max_temp_c,
            is_clamped,
            status_description: status_desc,
        }
    }

    /// Read host battery and AC power supply telemetry
    pub fn read_battery_telemetry(&self) -> BatterySnapshot {
        if let Some(ref ov) = *self.telemetry_override.read().unwrap() {
            let ac = ov.ac_connected.unwrap_or(true);
            let pct = ov.battery_pct.unwrap_or(80);
            let disch = ov.is_discharging.unwrap_or(false);
            return BatterySnapshot {
                ac_connected: ac,
                battery_present: true,
                capacity_pct: pct,
                status: if disch { "Discharging".into() } else { "Charging".into() },
                is_discharging: disch,
            };
        }

        let ps_dir = self.config.sysfs_root.join("sys/class/power_supply");
        let mut ac_connected = false;
        let mut battery_present = false;
        let mut capacity_pct: u32 = 100;
        let mut status = "Full".to_string();
        let mut is_discharging = false;

        if let Ok(entries) = fs::read_dir(ps_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(c) = fs::read_to_string(entry.path().join("online")) {
                        if c.trim() == "1" {
                            ac_connected = true;
                        }
                    }
                } else if name.starts_with("BAT") {
                    battery_present = true;
                    if let Ok(c) = fs::read_to_string(entry.path().join("capacity")) {
                        if let Ok(pct) = c.trim().parse::<u32>() {
                            capacity_pct = pct;
                        }
                    }
                    if let Ok(s) = fs::read_to_string(entry.path().join("status")) {
                        status = s.trim().to_string();
                        if status.eq_ignore_ascii_case("Discharging") {
                            is_discharging = true;
                        }
                    }
                }
            }
        }

        BatterySnapshot {
            ac_connected,
            battery_present,
            capacity_pct,
            status,
            is_discharging,
        }
    }

    /// Thermal & battery watchdog evaluation with hysteresis recovery
    /// Returns true if currently clamped under thermal or battery distress
    pub fn evaluate_watchdog(&self) -> bool {
        let thermal = self.read_thermal_telemetry();
        let battery = self.read_battery_telemetry();
        let currently_clamped = self.is_thermally_clamped.load(Ordering::SeqCst);

        // 1. Thermal Emergency Trigger
        let thermal_trigger = thermal.cpu_pkg_temp_c >= self.config.thermal_clamp_threshold_c;
        // 2. Battery Emergency Trigger (discharging with < 20% capacity)
        let battery_trigger = battery.is_discharging && battery.capacity_pct < self.config.battery_critical_threshold_pct;

        if currently_clamped {
            // Hysteresis Recovery:
            // Release clamp ONLY when temperature is <= recovery threshold (70°C) AND battery >= 22% (or AC plugged in)
            let thermal_recovered = thermal.cpu_pkg_temp_c <= self.config.thermal_recovery_threshold_c;
            let battery_recovered = !battery.is_discharging || battery.capacity_pct >= BATTERY_RECOVERY_DISCHARGE_PCT || battery.ac_connected;

            if thermal_recovered && battery_recovered {
                // Lift clamp
                self.is_thermally_clamped.store(false, Ordering::SeqCst);
                *self.thermal_clamp_reason.write().unwrap() = None;

                // Restore appropriate profile based on active burst tasks
                let tasks = self.active_burst_tasks.load(Ordering::SeqCst);
                let restored = if tasks > 0 {
                    KernelProfile::Performance
                } else {
                    KernelProfile::BalancePerformance
                };
                let _ = self.apply_profile(restored);
                false
            } else {
                true // Still clamped
            }
        } else if thermal_trigger || battery_trigger {
            // Engage Emergency Clamp
            self.is_thermally_clamped.store(true, Ordering::SeqCst);
            self.total_thermal_clamps.fetch_add(1, Ordering::Relaxed);

            let reason = if thermal_trigger {
                format!(
                    "Thermal emergency clamp engaged: CPU temperature {:.1}°C >= {:.1}°C threshold",
                    thermal.cpu_pkg_temp_c, self.config.thermal_clamp_threshold_c
                )
            } else {
                format!(
                    "Battery critical clamp engaged: capacity {}% on active discharge",
                    battery.capacity_pct
                )
            };

            *self.thermal_clamp_reason.write().unwrap() = Some(reason);
            let _ = self.apply_profile(KernelProfile::ThermalThrottled);
            true
        } else {
            false
        }
    }

    // ==========================================================================
    // Workload Burst Governor & RAII Leases
    // ==========================================================================

    /// Acquire an RAII workload burst lease
    ///
    /// Signals high-performance mode:
    /// - CPU governor: `performance`
    /// - EPP: `performance`
    /// - Turbo Boost: enabled
    /// - MGLRU: `min_ttl_ms 1000`
    ///
    /// If thermal or battery clamp is active, returns clamped lease with concurrency=1.
    pub fn acquire_burst(self: &Arc<Self>, workload: BurstWorkload) -> Result<WorkloadBurstGuard> {
        let lease_id = format!("lease_{}_{}", workload, current_epoch_ms());

        // Check safety clamp
        let is_clamped = self.evaluate_watchdog();

        let prev = self.active_burst_tasks.fetch_add(1, Ordering::SeqCst);
        self.idle_generation.fetch_add(1, Ordering::SeqCst);

        {
            let mut reg = self.burst_registry.write().unwrap();
            reg.insert(lease_id.clone(), workload.clone());
        }

        if is_clamped {
            // Clamped: ensure throttled profile
            let _ = self.apply_profile(KernelProfile::ThermalThrottled);
            Ok(WorkloadBurstGuard {
                loop_ref: Arc::clone(self),
                lease_id,
                workload,
                clamped: true,
                active: true,
            })
        } else {
            // Ramp-up to performance mode if this is the first task or not yet in performance
            if prev == 0 || self.current_profile() != KernelProfile::Performance {
                self.total_burst_ramp_ups.fetch_add(1, Ordering::Relaxed);
                let _ = self.apply_profile(KernelProfile::Performance);
            }

            Ok(WorkloadBurstGuard {
                loop_ref: Arc::clone(self),
                lease_id,
                workload,
                clamped: false,
                active: true,
            })
        }
    }

    /// Internal release of a burst lease by ID
    pub fn release_burst(&self, lease_id: &str) {
        {
            let mut reg = self.burst_registry.write().unwrap();
            reg.remove(lease_id);
        }

        let prev = self.active_burst_tasks.fetch_sub(1, Ordering::SeqCst);
        let current_gen = self.idle_generation.fetch_add(1, Ordering::SeqCst) + 1;

        if prev == 1 {
            // Task queue drained to 0: schedule smooth auto-sleep idle drop
            let loop_clone = self.shallow_clone();
            let grace = self.config.idle_grace_period;

            tokio::spawn(async move {
                tokio::time::sleep(grace).await;
                // Verify generation and count haven't changed
                if loop_clone.active_burst_tasks.load(Ordering::SeqCst) == 0
                    && loop_clone.idle_generation.load(Ordering::SeqCst) == current_gen
                {
                    if !loop_clone.is_thermally_clamped.load(Ordering::SeqCst) {
                        loop_clone.total_idle_drops.fetch_add(1, Ordering::Relaxed);
                        let _ = loop_clone.apply_profile(KernelProfile::BalancePerformance);
                    }
                }
            });
        }
    }

    fn shallow_clone(&self) -> Arc<Self> {
        Arc::new(Self {
            config: self.config.clone(),
            paths: self.paths.clone(),
            current_profile: Arc::clone(&self.current_profile),
            target_profile: Arc::clone(&self.target_profile),
            active_burst_tasks: Arc::clone(&self.active_burst_tasks),
            burst_registry: Arc::clone(&self.burst_registry),
            is_thermally_clamped: Arc::clone(&self.is_thermally_clamped),
            thermal_clamp_reason: Arc::clone(&self.thermal_clamp_reason),
            idle_generation: Arc::clone(&self.idle_generation),
            total_burst_ramp_ups: Arc::clone(&self.total_burst_ramp_ups),
            total_idle_drops: Arc::clone(&self.total_idle_drops),
            total_thermal_clamps: Arc::clone(&self.total_thermal_clamps),
            started_at: self.started_at,
            telemetry_override: Arc::clone(&self.telemetry_override),
            active_governor_cache: Arc::clone(&self.active_governor_cache),
            active_epp_cache: Arc::clone(&self.active_epp_cache),
            active_turbo_cache: Arc::clone(&self.active_turbo_cache),
            active_mglru_min_ttl_cache: Arc::clone(&self.active_mglru_min_ttl_cache),
        })
    }

    // ==========================================================================
    // Status & IPC Handling
    // ==========================================================================

    /// Collect comprehensive hardware and governor state
    pub fn get_status(&self) -> KernelLoopStatus {
        let cpu = self.read_cpu_state();
        let mglru = self.read_mglru_status();
        let thermal = self.read_thermal_telemetry();
        let battery = self.read_battery_telemetry();

        KernelLoopStatus {
            current_profile: self.current_profile(),
            target_profile: *self.target_profile.read().unwrap(),
            active_burst_tasks: self.active_burst_tasks.load(Ordering::SeqCst),
            is_thermally_clamped: self.is_thermally_clamped.load(Ordering::SeqCst),
            clamp_reason: self.thermal_clamp_reason.read().unwrap().clone(),
            cpu,
            mglru,
            thermal,
            battery,
            paths: self.paths.clone(),
            total_burst_ramp_ups: self.total_burst_ramp_ups.load(Ordering::Relaxed),
            total_idle_drops: self.total_idle_drops.load(Ordering::Relaxed),
            total_thermal_clamps: self.total_thermal_clamps.load(Ordering::Relaxed),
            uptime_secs: self.started_at.elapsed().as_secs(),
            timestamp_ms: current_epoch_ms(),
        }
    }

    /// Process a parsed IPC command and return response
    pub fn handle_ipc_command(self: &Arc<Self>, cmd: KernelIpcCommand) -> KernelIpcResponse {
        match cmd {
            KernelIpcCommand::GetStatus => {
                let status = self.get_status();
                KernelIpcResponse {
                    success: true,
                    message: "Status queried successfully".into(),
                    applied_profile: Some(status.current_profile),
                    lease_id: None,
                    status: Some(status),
                    timestamp_ms: current_epoch_ms(),
                }
            }
            KernelIpcCommand::SetProfile { profile } => {
                match self.apply_profile(profile) {
                    Ok(_) => KernelIpcResponse {
                        success: true,
                        message: format!("Applied profile '{}'", profile),
                        applied_profile: Some(profile),
                        lease_id: None,
                        status: Some(self.get_status()),
                        timestamp_ms: current_epoch_ms(),
                    },
                    Err(e) => KernelIpcResponse::err(format!("Failed to apply profile: {}", e)),
                }
            }
            KernelIpcCommand::SetEpp { epp } => {
                match self.set_cpu_epp(&epp) {
                    Ok(_) => KernelIpcResponse::ok(format!("Set EPP to '{}'", epp)),
                    Err(e) => KernelIpcResponse::err(format!("Failed to set EPP: {}", e)),
                }
            }
            KernelIpcCommand::SetGovernor { governor } => {
                match self.set_cpu_governor(&governor) {
                    Ok(_) => KernelIpcResponse::ok(format!("Set scaling governor to '{}'", governor)),
                    Err(e) => KernelIpcResponse::err(format!("Failed to set governor: {}", e)),
                }
            }
            KernelIpcCommand::SetMglruTtl { min_ttl_ms } => {
                match self.set_mglru_min_ttl(min_ttl_ms) {
                    Ok(_) => KernelIpcResponse::ok(format!("Set MGLRU min_ttl_ms to {}", min_ttl_ms)),
                    Err(e) => KernelIpcResponse::err(format!("Failed to set MGLRU min_ttl_ms: {}", e)),
                }
            }
            KernelIpcCommand::AcquireBurst { workload } => {
                match self.acquire_burst(workload) {
                    Ok(guard) => {
                        let lease_id = guard.lease_id().to_string();
                        // Forget guard in IPC server so it remains held until ReleaseBurst
                        std::mem::forget(guard);
                        KernelIpcResponse {
                            success: true,
                            message: "Acquired burst lease".into(),
                            applied_profile: Some(self.current_profile()),
                            lease_id: Some(lease_id),
                            status: Some(self.get_status()),
                            timestamp_ms: current_epoch_ms(),
                        }
                    }
                    Err(e) => KernelIpcResponse::err(format!("Failed to acquire burst: {}", e)),
                }
            }
            KernelIpcCommand::ReleaseBurst { lease_id } => {
                self.release_burst(&lease_id);
                KernelIpcResponse::ok(format!("Released burst lease '{}'", lease_id))
            }
            KernelIpcCommand::TriggerScriptProfile { profile } => {
                let mglru_res = self.apply_mglru_script_profile(&profile);
                match mglru_res {
                    Ok(msg) => KernelIpcResponse::ok(format!("Script profile triggered: {}", msg)),
                    Err(e) => KernelIpcResponse::err(format!("Script profile failed: {}", e)),
                }
            }
            KernelIpcCommand::Ping => KernelIpcResponse::ok("pong"),
        }
    }

    // ==========================================================================
    // IPC Server (Unix Domain Socket)
    // ==========================================================================

    /// Start the Unix Domain Socket IPC server in a background task
    pub async fn start_ipc_server(
        self: Arc<Self>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<()> {
        let socket_path = &self.config.socket_path;

        // Clean up pre-existing socket file if present
        if socket_path.exists() {
            let _ = fs::remove_file(socket_path);
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let listener = TokioUnixListener::bind(socket_path)
            .map_err(|e| HgbError::Ipc(format!("Failed to bind IPC socket {:?}: {}", socket_path, e)))?;

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    break;
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((stream, _)) => {
                            let loop_ref = Arc::clone(&self);
                            tokio::spawn(async move {
                                let (reader, mut writer) = stream.into_split();
                                let mut lines = TokioBufReader::new(reader).lines();

                                while let Ok(Some(line)) = lines.next_line().await {
                                    let line_trimmed = line.trim();
                                    if line_trimmed.is_empty() {
                                        continue;
                                    }

                                    let resp = match serde_json::from_str::<KernelIpcCommand>(line_trimmed) {
                                        Ok(cmd) => loop_ref.handle_ipc_command(cmd),
                                        Err(e) => KernelIpcResponse::err(format!("Invalid JSON command: {}", e)),
                                    };

                                    if let Ok(mut resp_bytes) = serde_json::to_vec(&resp) {
                                        resp_bytes.push(b'\n');
                                        if writer.write_all(&resp_bytes).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        let _ = fs::remove_file(socket_path);
        Ok(())
    }

    /// Dispatch an IPC command over a Unix domain socket
    pub async fn send_ipc_command(socket_path: &Path, cmd: &KernelIpcCommand) -> Result<KernelIpcResponse> {
        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|e| HgbError::Ipc(format!("Failed to connect to IPC socket {:?}: {}", socket_path, e)))?;

        let (reader, mut writer) = stream.into_split();
        let mut cmd_bytes = serde_json::to_vec(cmd)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize IPC command: {}", e)))?;
        cmd_bytes.push(b'\n');

        writer.write_all(&cmd_bytes).await
            .map_err(|e| HgbError::Ipc(format!("Failed to write to IPC socket: {}", e)))?;

        let mut lines = TokioBufReader::new(reader).lines();
        let next_line = lines.next_line().await
            .map_err(|e| HgbError::Ipc(format!("Failed to read response from IPC socket: {}", e)))?;

        if let Some(line) = next_line {
            let resp = serde_json::from_str::<KernelIpcResponse>(&line)
                .map_err(|e| HgbError::Serialization(format!("Failed to parse IPC response: {}", e)))?;
            Ok(resp)
        } else {
            Err(HgbError::Ipc("Empty response from IPC server".to_string()))
        }
    }

    /// Create an in-process protocol channel for zero-overhead task communication
    pub fn create_ipc_channel(
        self: Arc<Self>,
    ) -> (mpsc::Sender<(KernelIpcCommand, tokio::sync::oneshot::Sender<KernelIpcResponse>)>, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<(KernelIpcCommand, tokio::sync::oneshot::Sender<KernelIpcResponse>)>(64);
        let loop_ref = Arc::clone(&self);

        let handle = tokio::spawn(async move {
            while let Some((cmd, reply_tx)) = rx.recv().await {
                let resp = loop_ref.handle_ipc_command(cmd);
                let _ = reply_tx.send(resp);
            }
        });

        (tx, handle)
    }
}

// ==============================================================================
// 7. RAII Workload Burst Guard
// ==============================================================================

/// RAII Guard that locks high-performance burst mode while held
pub struct WorkloadBurstGuard {
    loop_ref: Arc<KernelDynatuneLoop>,
    lease_id: String,
    workload: BurstWorkload,
    clamped: bool,
    active: bool,
}

impl WorkloadBurstGuard {
    pub fn lease_id(&self) -> &str {
        &self.lease_id
    }

    pub fn workload(&self) -> &BurstWorkload {
        &self.workload
    }

    pub fn is_clamped(&self) -> bool {
        self.clamped
    }

    pub fn concurrency_limit(&self) -> usize {
        if self.clamped {
            CLAMPED_CONCURRENCY_LIMIT
        } else {
            DEFAULT_NOMINAL_CONCURRENCY
        }
    }
}

impl Drop for WorkloadBurstGuard {
    fn drop(&mut self) {
        if self.active {
            self.loop_ref.release_burst(&self.lease_id);
            self.active = false;
        }
    }
}

// ==============================================================================
// 8. Utility Functions
// ==============================================================================

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ==============================================================================
// 9. Comprehensive Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_mock_sysfs(root: &Path) {
        // cpufreq policy0
        let policy0 = root.join("sys/devices/system/cpu/cpufreq/policy0");
        fs::create_dir_all(&policy0).unwrap();
        fs::write(policy0.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(policy0.join("energy_performance_preference"), "balance_performance\n").unwrap();
        fs::write(policy0.join("scaling_driver"), "intel_pstate\n").unwrap();
        fs::write(policy0.join("scaling_cur_freq"), "1800000\n").unwrap();

        // cpufreq cpu0
        let cpu0 = root.join("sys/devices/system/cpu/cpu0/cpufreq");
        fs::create_dir_all(&cpu0).unwrap();
        fs::write(cpu0.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu0.join("energy_performance_preference"), "balance_performance\n").unwrap();
        fs::write(cpu0.join("scaling_driver"), "intel_pstate\n").unwrap();
        fs::write(cpu0.join("scaling_cur_freq"), "1800000\n").unwrap();

        // intel_pstate
        let pstate = root.join("sys/devices/system/cpu/intel_pstate");
        fs::create_dir_all(&pstate).unwrap();
        fs::write(pstate.join("no_turbo"), "0\n").unwrap();

        // MGLRU
        let mglru = root.join("sys/kernel/mm/lru_gen");
        fs::create_dir_all(&mglru).unwrap();
        fs::write(mglru.join("enabled"), "0x0007\n").unwrap();
        fs::write(mglru.join("min_ttl_ms"), "0\n").unwrap();

        // proc/vmstat
        let proc = root.join("proc");
        fs::create_dir_all(&proc).unwrap();
        fs::write(
            proc.join("vmstat"),
            "pgscan_kswapd 1000\npgscan_direct 500\npgsteal_kswapd 800\npgsteal_direct 400\n",
        )
        .unwrap();

        // Thermal zones
        let thermal = root.join("sys/class/thermal/thermal_zone0");
        fs::create_dir_all(&thermal).unwrap();
        fs::write(thermal.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(thermal.join("temp"), "52000\n").unwrap();

        // Power supply
        let ps_ac = root.join("sys/class/power_supply/ADP1");
        fs::create_dir_all(&ps_ac).unwrap();
        fs::write(ps_ac.join("online"), "1\n").unwrap();

        let ps_bat = root.join("sys/class/power_supply/BAT1");
        fs::create_dir_all(&ps_bat).unwrap();
        fs::write(ps_bat.join("status"), "Full\n").unwrap();
        fs::write(ps_bat.join("capacity"), "95\n").unwrap();
    }

    #[test]
    fn test_path_discovery_overrides_and_defaults() {
        let temp_dir = tempdir().unwrap();
        let fake_dynatune = temp_dir.path().join("dynatune");
        let fake_ko = temp_dir.path().join("kernel-optimizer");
        fs::create_dir_all(&fake_dynatune).unwrap();
        fs::create_dir_all(fake_ko.join("plugins")).unwrap();
        fs::write(fake_ko.join("dynabook-kernel-ctl"), "#!/bin/sh\n").unwrap();
        fs::write(fake_ko.join("plugins/cpu-governor.sh"), "#!/bin/sh\n").unwrap();
        fs::write(fake_ko.join("plugins/mglru-monitor.sh"), "#!/bin/sh\n").unwrap();

        std::env::set_var("DYNATUNE_PATH", fake_dynatune.to_str().unwrap());
        std::env::set_var("KERNEL_OPTIMIZER_PATH", fake_ko.to_str().unwrap());

        let paths = DynatuneKernelPaths::discover(None);
        assert_eq!(paths.dynatune_dir, Some(fake_dynatune));
        assert_eq!(paths.kernel_optimizer_dir, Some(fake_ko.clone()));
        assert_eq!(paths.dynabook_kernel_ctl, Some(fake_ko.join("dynabook-kernel-ctl")));
        assert_eq!(paths.cpu_governor_sh, Some(fake_ko.join("plugins/cpu-governor.sh")));
        assert_eq!(paths.mglru_monitor_sh, Some(fake_ko.join("plugins/mglru-monitor.sh")));
        assert!(paths.is_dynatune_available());
        assert!(paths.is_kernel_optimizer_available());
        assert!(paths.summary().contains("cpu-gov"));

        // Clean up env
        std::env::remove_var("DYNATUNE_PATH");
        std::env::remove_var("KERNEL_OPTIMIZER_PATH");
    }

    #[test]
    fn test_cpu_governor_and_epp_profiles() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("test.sock");
        setup_mock_sysfs(&sysfs_root);

        let kloop = KernelDynatuneLoop::for_test(&sysfs_root, &sock_path);

        // 1. Performance mode
        kloop.apply_profile(KernelProfile::Performance).unwrap();
        assert_eq!(kloop.current_profile(), KernelProfile::Performance);
        let gov = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov.trim(), "performance");
        let epp = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp.trim(), "performance");
        let ttl = fs::read_to_string(sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(ttl.trim(), "1000");

        // 2. BalancePerformance mode
        kloop.apply_profile(KernelProfile::BalancePerformance).unwrap();
        assert_eq!(kloop.current_profile(), KernelProfile::BalancePerformance);
        let gov = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov.trim(), "powersave");
        let epp = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp.trim(), "balance_performance");
        let ttl = fs::read_to_string(sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(ttl.trim(), "0");

        // 3. Power mode
        kloop.apply_profile(KernelProfile::Power).unwrap();
        assert_eq!(kloop.current_profile(), KernelProfile::Power);
        let epp = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp.trim(), "power");
        let ttl = fs::read_to_string(sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(ttl.trim(), "5000");
    }

    #[test]
    fn test_mglru_controls_and_decoding() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("test.sock");
        setup_mock_sysfs(&sysfs_root);

        let kloop = KernelDynatuneLoop::for_test(&sysfs_root, &sock_path);
        let mglru = kloop.read_mglru_status();

        assert!(mglru.available);
        assert_eq!(mglru.enabled_flags, 7);
        assert!(mglru.eviction_active);
        assert!(mglru.file_page_protection);
        assert!(mglru.page_age_pulling);
        assert_eq!(mglru.min_ttl_ms, 0);
        assert!(mglru.reclaim_efficiency.unwrap().contains("80.00%"));

        // Set min_ttl_ms
        kloop.set_mglru_min_ttl(1000).unwrap();
        let mglru_updated = kloop.read_mglru_status();
        assert_eq!(mglru_updated.min_ttl_ms, 1000);
    }

    #[test]
    fn test_thermal_watchdog_threshold_and_hysteresis() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("test.sock");
        setup_mock_sysfs(&sysfs_root);

        let kloop = KernelDynatuneLoop::for_test(&sysfs_root, &sock_path);

        // Healthy state at 52°C
        assert!(!kloop.evaluate_watchdog());
        assert_eq!(kloop.current_profile(), KernelProfile::BalancePerformance);

        // 1. Thermal spike: 82.5°C >= 78.0°C
        kloop.set_telemetry_override(Some(TelemetryOverride {
            cpu_pkg_temp_c: Some(82.5),
            battery_pct: Some(85),
            is_discharging: Some(false),
            ac_connected: Some(true),
        }));

        let clamped = kloop.evaluate_watchdog();
        assert!(clamped, "Must engage thermal clamp at 82.5°C");
        assert_eq!(kloop.current_profile(), KernelProfile::ThermalThrottled);
        assert_eq!(kloop.get_status().cpu.active_governor, "powersave");
        assert_eq!(kloop.get_status().cpu.active_epp, "power");

        // 2. Intermediate temp: 74.0°C (between 70.0°C recovery and 78.0°C clamp)
        // Must remain clamped due to hysteresis!
        kloop.set_telemetry_override(Some(TelemetryOverride {
            cpu_pkg_temp_c: Some(74.0),
            battery_pct: Some(85),
            is_discharging: Some(false),
            ac_connected: Some(true),
        }));

        let still_clamped = kloop.evaluate_watchdog();
        assert!(still_clamped, "Must stay clamped due to hysteresis at 74.0°C");
        assert_eq!(kloop.current_profile(), KernelProfile::ThermalThrottled);

        // 3. Recovery temp: 68.0°C <= 70.0°C
        kloop.set_telemetry_override(Some(TelemetryOverride {
            cpu_pkg_temp_c: Some(68.0),
            battery_pct: Some(85),
            is_discharging: Some(false),
            ac_connected: Some(true),
        }));

        let recovered = kloop.evaluate_watchdog();
        assert!(!recovered, "Must recover below 70.0°C");
        assert_eq!(kloop.current_profile(), KernelProfile::BalancePerformance);
    }

    #[tokio::test]
    async fn test_workload_burst_governor_and_guard() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("test.sock");
        setup_mock_sysfs(&sysfs_root);

        let kloop = KernelDynatuneLoop::for_test(&sysfs_root, &sock_path);

        // 1. Initial state
        assert_eq!(kloop.get_status().active_burst_tasks, 0);
        assert_eq!(kloop.current_profile(), KernelProfile::BalancePerformance);

        // 2. Acquire burst guard for HeavyCompilation
        let guard1 = kloop.acquire_burst(BurstWorkload::HeavyCompilation).unwrap();
        assert_eq!(guard1.workload(), &BurstWorkload::HeavyCompilation);
        assert!(!guard1.is_clamped());
        assert_eq!(guard1.concurrency_limit(), DEFAULT_NOMINAL_CONCURRENCY);
        assert_eq!(kloop.current_profile(), KernelProfile::Performance);
        assert_eq!(kloop.get_status().active_burst_tasks, 1);

        // Verify sysfs locked to performance
        let gov = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov.trim(), "performance");

        // 3. Acquire second burst guard for LlmInference
        {
            let guard2 = kloop.acquire_burst(BurstWorkload::LlmInference).unwrap();
            assert_eq!(kloop.get_status().active_burst_tasks, 2);
            assert_eq!(kloop.current_profile(), KernelProfile::Performance);
            drop(guard2);
        }

        // guard2 dropped, tasks back to 1, still in performance
        assert_eq!(kloop.get_status().active_burst_tasks, 1);
        assert_eq!(kloop.current_profile(), KernelProfile::Performance);

        // 4. Drop guard1 -> triggers idle grace timer (50ms)
        drop(guard1);
        assert_eq!(kloop.get_status().active_burst_tasks, 0);

        // Wait for idle grace timer to elapse
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Governor smoothly dropped back to BalancePerformance
        assert_eq!(kloop.current_profile(), KernelProfile::BalancePerformance);
        let gov_dropped = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov_dropped.trim(), "powersave");
    }

    #[tokio::test]
    async fn test_ipc_unix_socket_messaging() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("ipc.sock");
        setup_mock_sysfs(&sysfs_root);

        let kloop = KernelDynatuneLoop::for_test(&sysfs_root, &sock_path);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let server_loop = Arc::clone(&kloop);
        let server_handle = tokio::spawn(async move {
            let _ = server_loop.start_ipc_server(shutdown_rx).await;
        });

        // Small yield to let listener bind
        tokio::time::sleep(Duration::from_millis(20)).await;

        // 1. Send Ping
        let resp = KernelDynatuneLoop::send_ipc_command(&sock_path, &KernelIpcCommand::Ping).await.unwrap();
        assert!(resp.success);
        assert_eq!(resp.message, "pong");

        // 2. Send GetStatus
        let resp = KernelDynatuneLoop::send_ipc_command(&sock_path, &KernelIpcCommand::GetStatus).await.unwrap();
        assert!(resp.success);
        let status = resp.status.unwrap();
        assert_eq!(status.current_profile, KernelProfile::BalancePerformance);

        // 3. Send SetProfile Performance
        let resp = KernelDynatuneLoop::send_ipc_command(
            &sock_path,
            &KernelIpcCommand::SetProfile {
                profile: KernelProfile::Performance,
            },
        )
        .await
        .unwrap();
        assert!(resp.success);
        assert_eq!(kloop.current_profile(), KernelProfile::Performance);

        // 4. Send AcquireBurst and ReleaseBurst
        let resp = KernelDynatuneLoop::send_ipc_command(
            &sock_path,
            &KernelIpcCommand::AcquireBurst {
                workload: BurstWorkload::VectorIndexing,
            },
        )
        .await
        .unwrap();
        assert!(resp.success);
        let lease_id = resp.lease_id.unwrap();
        assert_eq!(kloop.get_status().active_burst_tasks, 1);

        let resp = KernelDynatuneLoop::send_ipc_command(
            &sock_path,
            &KernelIpcCommand::ReleaseBurst {
                lease_id,
            },
        )
        .await
        .unwrap();
        assert!(resp.success);
        assert_eq!(kloop.get_status().active_burst_tasks, 0);

        // Shutdown IPC server
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn test_in_process_channel_messaging() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("test.sock");
        setup_mock_sysfs(&sysfs_root);

        let kloop = KernelDynatuneLoop::for_test(&sysfs_root, &sock_path);
        let (tx, _handle) = kloop.shallow_clone().create_ipc_channel();

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send((KernelIpcCommand::Ping, reply_tx)).await.unwrap();
        let resp = reply_rx.await.unwrap();
        assert!(resp.success);
        assert_eq!(resp.message, "pong");
    }

    #[test]
    fn test_live_host_sysfs_read_live() {
        // Real non-mocked execution against the host
        let kloop = KernelDynatuneLoop::new();
        let status = kloop.get_status();

        // Host has real MGLRU enabled
        assert!(status.mglru.available, "Host must have MGLRU available");
        assert!(!status.cpu.core_frequencies_mhz.is_empty(), "Host must have logical cores");
        assert!(!status.thermal.zones.is_empty(), "Host must have thermal zones");
        assert!(status.thermal.cpu_pkg_temp_c > 0.0, "CPU temperature should be positive");
    }
}
