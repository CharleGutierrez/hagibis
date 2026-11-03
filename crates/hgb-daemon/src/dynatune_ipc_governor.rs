//! # Pillar 1: Direct IPC & FFI Bridge with Dynatune & Kernel-Optimizer
//!
//! Production-grade direct IPC and kernel governor orchestration bridge connecting
//! `hgb-daemon` to host optimization systems:
//! - Unix domain socket `/run/dynatune.sock` (with bidirectional JSON-RPC IPC)
//! - Direct Linux sysfs control (`/sys/devices/system/cpu/cpufreq`, `intel_pstate`, MGLRU)
//! - Direct execution fallback to `dynabook-kernel-ctl` and `powerprofilesctl`
//!
//! ## Key Capabilities:
//! 1. **Proactive Workload Ramp-Up**:
//!    Triggered when heavy LLM inference batches, massive Wasm tool compilation,
//!    or Firecracker microVM spin-up starts. Signals kernel governor to switch scaling
//!    governor to `performance`, sets EPP to `performance`, enables Intel turbo boost,
//!    and adjusts Linux MGLRU memory compaction (`/sys/kernel/mm/lru_gen/min_ttl_ms 1000`).
//! 2. **Auto-Sleep & Ultra-Low-Power Idle Drop**:
//!    Maintains active task reference counter (`Arc<AtomicUsize>`). When queue becomes idle
//!    (active heavy tasks = 0 for configurable idle grace period, e.g. 5 seconds), smoothly
//!    signals the governor to drop into ultra-low-power mode: governor `powersave`, EPP `power`
//!    or `balance_power`, and reset MGLRU `min_ttl_ms 0`.
//! 3. **Thermal & Battery Safeguard**:
//!    If CPU package temp >= 78.0°C or battery < 20% on discharge, clamps to `powersave` and
//!    restricts concurrency to 1 worker until cooled (< 70.0°C).
//! 4. **RAII Guard `DynatuneWorkloadLease`**:
//!    Tasks acquire the boost lease on start and automatically drop when finished or dropped.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use hgb_core::error::Result;

// ==============================================================================
// 1. Constants & Tuning Thresholds
// ==============================================================================

/// Default Unix domain socket path for dynatune daemon
pub const DEFAULT_DYNATUNE_SOCK: &str = "/run/dynatune.sock";

/// System-wide install path for dynabook-kernel-ctl
pub const SYSTEM_KERNEL_CTL_BIN: &str = "/usr/local/bin/dynabook-kernel-ctl";

/// Sibling workspace path for dynabook-kernel-ctl
pub const SIBLING_KERNEL_CTL_BIN: &str = "/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl";

/// Emergency thermal throttle threshold in Celsius (78.0°C)
pub const THERMAL_CLAMP_THRESHOLD_C: f64 = 78.0;

/// Thermal recovery threshold in Celsius with hysteresis (70.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 70.0;

/// Critical battery capacity percentage threshold on discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;

/// Linux Multi-Gen LRU (MGLRU) burst target (1000ms) to prioritize memory compaction
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;

/// Linux Multi-Gen LRU (MGLRU) baseline / idle target (0ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Default auto-sleep idle grace period before dropping into ultra-low-power mode
pub const DEFAULT_IDLE_GRACE_PERIOD_SECS: u64 = 5;

/// Clamped concurrency limit under thermal or critical battery stress
pub const CLAMPED_CONCURRENCY_LIMIT: usize = 1;

/// Default nominal concurrency limit when operating under healthy thermal conditions
pub const DEFAULT_NOMINAL_CONCURRENCY_LIMIT: usize = 4;

// ==============================================================================
// 2. Data Types & Models
// ==============================================================================

/// Classification of heavy workloads triggering proactive kernel ramp-up
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HeavyWorkloadType {
    /// Heavy local LLM inference batches and speculative draft generation
    LlmInference,
    /// Massive Wasm tool compilation, linking, and JIT verification
    WasmCompilation,
    /// Firecracker microVM spin-up, kernel boot, and container jail creation
    MicroVmSpinup,
    /// Multi-step autonomous agent self-healing loops
    AutonomousLoop,
    /// Heavy vector embedding batches and RAG indexing
    VectorBatch,
    /// Custom heavy workload identified by string key
    Custom(String),
}

impl HeavyWorkloadType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().replace('-', "_").as_str() {
            "llm" | "inference" | "llm_inference" | "localllminference" => Self::LlmInference,
            "wasm" | "compile" | "wasm_compilation" | "heavy_compile" | "build" => Self::WasmCompilation,
            "microvm" | "firecracker" | "microvm_spinup" | "microvm_spawn" => Self::MicroVmSpinup,
            "autonomous" | "autonomous_loop" | "self_healing" => Self::AutonomousLoop,
            "vector" | "vector_batch" | "embedding" | "rag" => Self::VectorBatch,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::LlmInference => "llm_inference",
            Self::WasmCompilation => "wasm_compilation",
            Self::MicroVmSpinup => "microvm_spinup",
            Self::AutonomousLoop => "autonomous_loop",
            Self::VectorBatch => "vector_batch",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for HeavyWorkloadType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Target Governor & Power states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernorMode {
    /// High-performance mode (governor: performance, EPP: performance, turbo: on, MGLRU: 1000)
    Performance,
    /// Ultra-low-power idle mode (governor: powersave, EPP: power, turbo: off, MGLRU: 0)
    PowerSave,
    /// Balanced day-to-day mode (governor: powersave, EPP: balance_power / balance_performance)
    Balanced,
    /// Clamped under thermal or battery distress (forced powersave, concurrency: 1)
    Clamped,
}

impl std::fmt::Display for GovernorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Performance => write!(f, "performance"),
            Self::PowerSave => write!(f, "powersave"),
            Self::Balanced => write!(f, "balanced"),
            Self::Clamped => write!(f, "clamped_powersave"),
        }
    }
}

/// IPC Command Message sent over `/run/dynatune.sock`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneIpcMessage {
    pub action: String,
    pub governor: Option<String>,
    pub epp: Option<String>,
    pub turbo: Option<bool>,
    pub mglru_min_ttl_ms: Option<u64>,
    pub workload: Option<String>,
    pub concurrency_limit: Option<usize>,
    pub timestamp_ms: u64,
}

/// IPC Response Message received from `/run/dynatune.sock`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DynatuneIpcResponse {
    pub success: bool,
    pub applied_governor: Option<String>,
    pub applied_epp: Option<String>,
    pub applied_turbo: Option<bool>,
    pub applied_mglru_min_ttl_ms: Option<u64>,
    pub message: Option<String>,
    pub host_temp_c: Option<f64>,
    pub battery_pct: Option<u32>,
}

/// Comprehensive real-time status of Dynatune IPC Governor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneGovernorStatus {
    pub active_heavy_tasks: usize,
    pub current_mode: GovernorMode,
    pub target_governor: String,
    pub target_epp: String,
    pub turbo_enabled: bool,
    pub mglru_min_ttl_ms: u64,
    pub cpu_pkg_temp_c: f64,
    pub battery_pct: u32,
    pub ac_connected: bool,
    pub is_discharging: bool,
    pub thermal_clamped: bool,
    pub safeguard_reason: Option<String>,
    pub concurrency_limit: usize,
    pub ipc_socket_active: bool,
    pub ipc_socket_path: String,
    pub last_transition: String,
    pub total_ramp_ups: u64,
    pub total_idle_drops: u64,
    pub total_thermal_clamps: u64,
    pub active_workloads: HashMap<String, usize>,
    pub timestamp_epoch_ms: u64,
}

/// Telemetry override hook for simulated testing
#[derive(Debug, Clone, Default)]
pub struct TelemetryOverride {
    pub cpu_temp_c: Option<f64>,
    pub battery_pct: Option<u32>,
    pub is_discharging: Option<bool>,
    pub ac_connected: Option<bool>,
}

/// Configuration settings for DynatuneIpcGovernor
#[derive(Debug, Clone)]
pub struct DynatuneIpcConfig {
    pub socket_path: PathBuf,
    pub sysfs_root: PathBuf,
    pub kernel_ctl_bin: Option<PathBuf>,
    pub idle_grace_period: Duration,
    pub thermal_clamp_threshold_c: f64,
    pub thermal_recovery_threshold_c: f64,
    pub battery_critical_pct: u32,
    pub nominal_concurrency: usize,
}

impl Default for DynatuneIpcConfig {
    fn default() -> Self {
        let kernel_ctl_bin = if Path::new(SYSTEM_KERNEL_CTL_BIN).exists() {
            Some(PathBuf::from(SYSTEM_KERNEL_CTL_BIN))
        } else if Path::new(SIBLING_KERNEL_CTL_BIN).exists() {
            Some(PathBuf::from(SIBLING_KERNEL_CTL_BIN))
        } else {
            None
        };

        Self {
            socket_path: PathBuf::from(DEFAULT_DYNATUNE_SOCK),
            sysfs_root: PathBuf::from("/"),
            kernel_ctl_bin,
            idle_grace_period: Duration::from_secs(DEFAULT_IDLE_GRACE_PERIOD_SECS),
            thermal_clamp_threshold_c: THERMAL_CLAMP_THRESHOLD_C,
            thermal_recovery_threshold_c: THERMAL_RECOVERY_THRESHOLD_C,
            battery_critical_pct: BATTERY_CRITICAL_DISCHARGE_PCT,
            nominal_concurrency: DEFAULT_NOMINAL_CONCURRENCY_LIMIT,
        }
    }
}

// ==============================================================================
// 3. DynatuneIpcGovernor Implementation
// ==============================================================================

/// High-Performance Governor & Subsystem Bridge for Hagibis Daemon
pub struct DynatuneIpcGovernor {
    config: DynatuneIpcConfig,
    active_heavy_tasks: Arc<AtomicUsize>,
    workload_registry: Arc<RwLock<HashMap<String, usize>>>,
    current_mode: Arc<RwLock<GovernorMode>>,
    is_thermal_clamped: Arc<AtomicBool>,
    idle_generation: Arc<AtomicU64>,
    ipc_socket_active: Arc<AtomicBool>,
    total_ramp_ups: Arc<AtomicU64>,
    total_idle_drops: Arc<AtomicU64>,
    total_thermal_clamps: Arc<AtomicU64>,
    last_transition_reason: Arc<RwLock<String>>,
    telemetry_override: Arc<RwLock<Option<TelemetryOverride>>>,
    idle_notify: Arc<Notify>,
    active_sysfs_governor: Arc<RwLock<String>>,
    active_sysfs_epp: Arc<RwLock<String>>,
    active_turbo: Arc<AtomicBool>,
    active_mglru_min_ttl_ms: Arc<AtomicU64>,
}

impl DynatuneIpcGovernor {
    /// Create a new Dynatune IPC Governor with default production configuration
    pub fn new() -> Self {
        Self::with_config(DynatuneIpcConfig::default())
    }

    /// Create with explicit custom configuration
    pub fn with_config(config: DynatuneIpcConfig) -> Self {
        let active_heavy_tasks = Arc::new(AtomicUsize::new(0));
        let workload_registry = Arc::new(RwLock::new(HashMap::new()));
        let current_mode = Arc::new(RwLock::new(GovernorMode::PowerSave));
        let is_thermal_clamped = Arc::new(AtomicBool::new(false));
        let idle_generation = Arc::new(AtomicU64::new(0));
        let ipc_socket_active = Arc::new(AtomicBool::new(false));
        let total_ramp_ups = Arc::new(AtomicU64::new(0));
        let total_idle_drops = Arc::new(AtomicU64::new(0));
        let total_thermal_clamps = Arc::new(AtomicU64::new(0));
        let last_transition_reason = Arc::new(RwLock::new("Initialized in ultra-low-power idle mode".to_string()));
        let telemetry_override = Arc::new(RwLock::new(None));
        let idle_notify = Arc::new(Notify::new());
        let active_sysfs_governor = Arc::new(RwLock::new("powersave".to_string()));
        let active_sysfs_epp = Arc::new(RwLock::new("power".to_string()));
        let active_turbo = Arc::new(AtomicBool::new(false));
        let active_mglru_min_ttl_ms = Arc::new(AtomicU64::new(0));

        let governor = Self {
            config,
            active_heavy_tasks,
            workload_registry,
            current_mode,
            is_thermal_clamped,
            idle_generation,
            ipc_socket_active,
            total_ramp_ups,
            total_idle_drops,
            total_thermal_clamps,
            last_transition_reason,
            telemetry_override,
            idle_notify,
            active_sysfs_governor,
            active_sysfs_epp,
            active_turbo,
            active_mglru_min_ttl_ms,
        };

        // Query initial socket state
        governor.check_ipc_socket();

        governor
    }

    /// Create an isolated governor instance with temporary sysfs root for unit tests
    pub fn for_test(sysfs_root: impl AsRef<Path>, socket_path: impl AsRef<Path>) -> Self {
        let mut config = DynatuneIpcConfig::default();
        config.sysfs_root = sysfs_root.as_ref().to_path_buf();
        config.socket_path = socket_path.as_ref().to_path_buf();
        config.idle_grace_period = Duration::from_millis(50);
        Self::with_config(config)
    }

    /// Read active configuration
    pub fn config(&self) -> &DynatuneIpcConfig {
        &self.config
    }

    /// Override telemetry for deterministic unit testing
    pub fn set_telemetry_override(&self, ov: Option<TelemetryOverride>) {
        let mut guard = self.telemetry_override.write().unwrap();
        *guard = ov;
    }

    /// Acquire RAII Workload Lease for a heavy task
    ///
    /// Automatically performs Proactive Workload Ramp-Up:
    /// - Switches CPU scaling governor to `performance`
    /// - Sets EPP to `performance`
    /// - Enables Intel Turbo Boost
    /// - Adjusts Linux MGLRU memory compaction (`min_ttl_ms 1000`)
    ///
    /// If thermal or battery safeguard engages, forces clamped powersave with concurrency = 1.
    pub fn acquire_lease(self: &Arc<Self>, workload: impl AsRef<str>) -> DynatuneWorkloadLease {
        let workload_str = workload.as_ref().to_string();
        let workload_type = HeavyWorkloadType::from_str(&workload_str);

        // Track active tasks reference counter
        let prev_count = self.active_heavy_tasks.fetch_add(1, Ordering::SeqCst);
        self.idle_generation.fetch_add(1, Ordering::SeqCst);

        // Update workload breakdown registry
        {
            let mut reg = self.workload_registry.write().unwrap();
            *reg.entry(workload_type.to_string()).or_insert(0) += 1;
        }

        // Evaluate thermal and battery safeguard
        let safeguard_engaged = self.evaluate_safeguard();

        if safeguard_engaged {
            // Under thermal or critical battery stress, override and clamp to powersave
            self.total_thermal_clamps.fetch_add(1, Ordering::Relaxed);
            *self.current_mode.write().unwrap() = GovernorMode::Clamped;
            *self.last_transition_reason.write().unwrap() = format!(
                "Safeguard override: clamped to powersave for workload '{}'",
                workload_type
            );

            // Enforce powersave hardware state
            let _ = self.apply_hardware_profile("powersave", "power", false, IDLE_MGLRU_MIN_TTL_MS);

            DynatuneWorkloadLease {
                governor: Arc::clone(self),
                workload: workload_type.to_string(),
                clamped: true,
                active: true,
            }
        } else {
            // Proactive Workload Ramp-Up
            if prev_count == 0 || *self.current_mode.read().unwrap() != GovernorMode::Performance {
                self.proactive_ramp_up(&workload_type);
            }

            DynatuneWorkloadLease {
                governor: Arc::clone(self),
                workload: workload_type.to_string(),
                clamped: false,
                active: true,
            }
        }
    }

    /// Proactive Workload Ramp-Up implementation
    fn proactive_ramp_up(&self, workload: &HeavyWorkloadType) {
        self.total_ramp_ups.fetch_add(1, Ordering::Relaxed);
        *self.current_mode.write().unwrap() = GovernorMode::Performance;
        *self.last_transition_reason.write().unwrap() = format!(
            "Proactive workload ramp-up triggered by '{}'",
            workload
        );

        // 1. Primary: IPC over /run/dynatune.sock
        let ipc_payload = DynatuneIpcMessage {
            action: "ramp_up".to_string(),
            governor: Some("performance".to_string()),
            epp: Some("performance".to_string()),
            turbo: Some(true),
            mglru_min_ttl_ms: Some(BURST_MGLRU_MIN_TTL_MS),
            workload: Some(workload.to_string()),
            concurrency_limit: Some(self.config.nominal_concurrency),
            timestamp_ms: current_epoch_ms(),
        };

        let ipc_ok = self.send_ipc_command(&ipc_payload);

        // 2. Direct sysfs control (always execute or fallback)
        let _ = self.apply_hardware_profile("performance", "performance", true, BURST_MGLRU_MIN_TTL_MS);

        // 3. Fallback execution of dynabook-kernel-ctl if available
        if !ipc_ok {
            let _ = self.invoke_kernel_ctl_profile("performance");
        }
    }

    /// Smooth Ultra-Low-Power Idle Drop implementation
    fn ultra_low_power_idle_drop(&self) {
        self.total_idle_drops.fetch_add(1, Ordering::Relaxed);
        *self.current_mode.write().unwrap() = GovernorMode::PowerSave;
        *self.last_transition_reason.write().unwrap() =
            "Auto-sleep: Task queue drained for grace period, dropped to ultra-low-power mode".to_string();

        // 1. IPC over /run/dynatune.sock
        let ipc_payload = DynatuneIpcMessage {
            action: "idle_drop".to_string(),
            governor: Some("powersave".to_string()),
            epp: Some("power".to_string()),
            turbo: Some(false),
            mglru_min_ttl_ms: Some(IDLE_MGLRU_MIN_TTL_MS),
            workload: None,
            concurrency_limit: Some(self.config.nominal_concurrency),
            timestamp_ms: current_epoch_ms(),
        };

        let ipc_ok = self.send_ipc_command(&ipc_payload);

        // 2. Direct sysfs control: governor powersave, EPP power, turbo off, MGLRU 0
        let _ = self.apply_hardware_profile("powersave", "power", false, IDLE_MGLRU_MIN_TTL_MS);

        // 3. Fallback execution of dynabook-kernel-ctl battery profile
        if !ipc_ok {
            let _ = self.invoke_kernel_ctl_profile("battery");
        }

        // 4. Notify any listeners awaiting idle state
        self.idle_notify.notify_waiters();
    }

    /// Notification handle fired whenever governor transitions into ultra-low-power idle drop
    pub fn idle_notify(&self) -> &Arc<Notify> {
        &self.idle_notify
    }

    /// Internal hook invoked automatically when a DynatuneWorkloadLease drops
    pub(crate) fn release_lease_internal(self: &Arc<Self>, workload: &str) {
        // Decrement workload registry
        {
            let mut reg = self.workload_registry.write().unwrap();
            if let Some(count) = reg.get_mut(workload) {
                if *count > 1 {
                    *count -= 1;
                } else {
                    reg.remove(workload);
                }
            }
        }

        // Decrement active tasks count
        let prev = self.active_heavy_tasks.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            // Active heavy tasks dropped to zero -> schedule smooth idle drop with grace period
            let drop_generation = self.idle_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let governor_clone = Arc::clone(self);
            let grace_period = self.config.idle_grace_period;

            // Spawn async/timer watcher to enforce the grace period
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    tokio::time::sleep(grace_period).await;
                    if governor_clone.idle_generation.load(Ordering::SeqCst) == drop_generation
                        && governor_clone.active_heavy_tasks.load(Ordering::SeqCst) == 0
                    {
                        governor_clone.ultra_low_power_idle_drop();
                    }
                });
            } else {
                // Synchronous fallback thread for non-tokio contexts
                std::thread::spawn(move || {
                    std::thread::sleep(grace_period);
                    if governor_clone.idle_generation.load(Ordering::SeqCst) == drop_generation
                        && governor_clone.active_heavy_tasks.load(Ordering::SeqCst) == 0
                    {
                        governor_clone.ultra_low_power_idle_drop();
                    }
                });
            }
        }
    }

    /// Force immediate idle drop without waiting for grace period (used in shutdown/testing)
    pub fn force_idle_drop(&self) {
        self.idle_generation.fetch_add(1, Ordering::SeqCst);
        self.ultra_low_power_idle_drop();
    }

    // ==============================================================================
    // 4. Thermal & Battery Safeguards
    // ==============================================================================

    /// Evaluate thermal and battery telemetry; returns true if safeguard must clamp
    pub fn evaluate_safeguard(&self) -> bool {
        let (cpu_temp_c, battery_pct, is_discharging, _ac_connected) = self.query_hardware_telemetry();

        let was_clamped = self.is_thermal_clamped.load(Ordering::Relaxed);

        if was_clamped {
            // Hysteresis recovery check: must cool below thermal_recovery_threshold_c (< 70.0°C)
            // AND battery must not be critically discharging
            if cpu_temp_c < self.config.thermal_recovery_threshold_c
                && (!is_discharging || battery_pct >= self.config.battery_critical_pct)
            {
                self.is_thermal_clamped.store(false, Ordering::SeqCst);
                false
            } else {
                true
            }
        } else {
            // Clamp trigger check: temp >= 78.0°C or battery < 20% on discharge
            let thermal_breach = cpu_temp_c >= self.config.thermal_clamp_threshold_c;
            let battery_breach = is_discharging && (battery_pct < self.config.battery_critical_pct);

            if thermal_breach || battery_breach {
                self.is_thermal_clamped.store(true, Ordering::SeqCst);
                true
            } else {
                false
            }
        }
    }

    /// Read live hardware telemetry from sysfs or test overrides
    pub fn query_hardware_telemetry(&self) -> (f64, u32, bool, bool) {
        if let Some(ref ov) = *self.telemetry_override.read().unwrap() {
            let temp = ov.cpu_temp_c.unwrap_or(55.0);
            let bat = ov.battery_pct.unwrap_or(85);
            let dis = ov.is_discharging.unwrap_or(false);
            let ac = ov.ac_connected.unwrap_or(true);
            return (temp, bat, dis, ac);
        }

        let temp = self.read_cpu_package_temp();
        let (bat, dis, ac) = self.read_battery_telemetry();
        (temp, bat, dis, ac)
    }

    /// Read CPU package temperature from sysfs / hwmon
    fn read_cpu_package_temp(&self) -> f64 {
        let sysfs = &self.config.sysfs_root;

        // 1. Try hwmon coretemp
        let hwmon_dir = sysfs.join("sys/class/hwmon");
        if let Ok(entries) = fs::read_dir(hwmon_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = fs::read_to_string(p.join("name")).unwrap_or_default();
                if name.trim() == "coretemp" {
                    // Check package temp or highest core temp
                    for input_entry in fs::read_dir(&p).into_iter().flatten().flatten() {
                        let name_str = input_entry.file_name().to_string_lossy().to_string();
                        if name_str.starts_with("temp") && name_str.ends_with("_input") {
                            if let Ok(raw) = fs::read_to_string(input_entry.path()) {
                                if let Ok(milli) = raw.trim().parse::<f64>() {
                                    let temp_c = milli / 1000.0;
                                    if temp_c > 10.0 && temp_c < 120.0 {
                                        return temp_c;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Fallback to thermal zones
        let thermal_dir = sysfs.join("sys/class/thermal");
        let mut max_zone_temp = 50.0;
        if let Ok(entries) = fs::read_dir(thermal_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let fname = p.file_name().unwrap_or_default().to_string_lossy();
                if fname.starts_with("thermal_zone") {
                    if let Ok(raw) = fs::read_to_string(p.join("temp")) {
                        if let Ok(milli) = raw.trim().parse::<f64>() {
                            let temp_c = milli / 1000.0;
                            if temp_c > max_zone_temp && temp_c < 120.0 {
                                max_zone_temp = temp_c;
                            }
                        }
                    }
                }
            }
        }

        max_zone_temp
    }

    /// Read battery capacity and charging status
    fn read_battery_telemetry(&self) -> (u32, bool, bool) {
        let sysfs = &self.config.sysfs_root;
        let ps_dir = sysfs.join("sys/class/power_supply");

        let mut battery_pct = 100u32;
        let mut is_discharging = false;
        let mut ac_connected = true;

        if let Ok(entries) = fs::read_dir(ps_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();

                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(online) = fs::read_to_string(p.join("online")) {
                        ac_connected = online.trim() == "1";
                    }
                } else if name.starts_with("BAT") {
                    if let Ok(cap_str) = fs::read_to_string(p.join("capacity")) {
                        if let Ok(pct) = cap_str.trim().parse::<u32>() {
                            battery_pct = pct;
                        }
                    }
                    if let Ok(stat_str) = fs::read_to_string(p.join("status")) {
                        let s = stat_str.trim();
                        is_discharging = s.eq_ignore_ascii_case("Discharging");
                        if s.eq_ignore_ascii_case("Charging") || s.eq_ignore_ascii_case("Full") {
                            ac_connected = true;
                        }
                    }
                }
            }
        }

        (battery_pct, is_discharging, ac_connected)
    }

    // ==============================================================================
    // 5. IPC Socket & Fallback Subsystems
    // ==============================================================================

    /// Probe whether `/run/dynatune.sock` is responsive
    pub fn check_ipc_socket(&self) -> bool {
        let sock_path = &self.config.socket_path;
        if sock_path.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::net::UnixStream;
                if let Ok(mut stream) = UnixStream::connect(sock_path) {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                    let ping = serde_json::json!({ "action": "ping" }).to_string() + "\n";
                    if stream.write_all(ping.as_bytes()).is_ok() {
                        self.ipc_socket_active.store(true, Ordering::SeqCst);
                        return true;
                    }
                }
            }
        }
        self.ipc_socket_active.store(false, Ordering::SeqCst);
        false
    }

    /// Send structured JSON command message over `/run/dynatune.sock`
    fn send_ipc_command(&self, msg: &DynatuneIpcMessage) -> bool {
        let sock_path = &self.config.socket_path;
        if !sock_path.exists() {
            self.ipc_socket_active.store(false, Ordering::SeqCst);
            return false;
        }

        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            if let Ok(mut stream) = UnixStream::connect(sock_path) {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));

                if let Ok(payload) = serde_json::to_string(msg) {
                    let full_cmd = format!("{}\n", payload);
                    if stream.write_all(full_cmd.as_bytes()).is_ok() {
                        self.ipc_socket_active.store(true, Ordering::SeqCst);
                        let mut buf = [0u8; 1024];
                        let _ = stream.read(&mut buf);
                        return true;
                    }
                }
            }
        }

        self.ipc_socket_active.store(false, Ordering::SeqCst);
        false
    }

    /// Apply direct sysfs configurations to host hardware
    fn apply_hardware_profile(
        &self,
        governor: &str,
        epp: &str,
        turbo: bool,
        mglru_min_ttl_ms: u64,
    ) -> Result<()> {
        let sysfs = &self.config.sysfs_root;

        // 1. CPU Scaling Governor
        let cpufreq_dir = sysfs.join("sys/devices/system/cpu/cpufreq");
        if cpufreq_dir.exists() {
            if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if name.starts_with("policy") {
                        let _ = fs::write(path.join("scaling_governor"), format!("{}\n", governor));
                        let _ = fs::write(path.join("energy_performance_preference"), format!("{}\n", epp));
                    }
                }
            }
        } else {
            // Alternate policy path under cpu*
            let cpu_dir = sysfs.join("sys/devices/system/cpu");
            if let Ok(entries) = fs::read_dir(cpu_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                        let _ = fs::write(p.join("cpufreq/scaling_governor"), format!("{}\n", governor));
                        let _ = fs::write(p.join("cpufreq/energy_performance_preference"), format!("{}\n", epp));
                    }
                }
            }
        }

        // 2. Intel P-State configuration
        let pstate_dir = sysfs.join("sys/devices/system/cpu/intel_pstate");
        if pstate_dir.exists() {
            let no_turbo = if turbo { "0\n" } else { "1\n" };
            let max_perf = if turbo { "100\n" } else { "70\n" };
            let hwp_boost = if turbo { "1\n" } else { "0\n" };

            let _ = fs::write(pstate_dir.join("no_turbo"), no_turbo);
            let _ = fs::write(pstate_dir.join("max_perf_pct"), max_perf);
            let _ = fs::write(pstate_dir.join("hwp_dynamic_boost"), hwp_boost);
        }

        // 3. Linux Multi-Gen LRU (MGLRU) Memory Compaction
        let mglru_path = sysfs.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        if mglru_path.exists() {
            let _ = fs::write(&mglru_path, format!("{}\n", mglru_min_ttl_ms));
        }

        // 4. Synchronize powerprofilesctl if available on system
        let profile_arg = if turbo { "performance" } else { "power-saver" };
        let _ = Command::new("powerprofilesctl")
            .args(["set", profile_arg])
            .output();

        // 5. Nice level prioritization for caller thread
        #[cfg(unix)]
        unsafe {
            let prio = if turbo { -5 } else { 0 };
            libc::setpriority(libc::PRIO_PROCESS, 0, prio);
        }

        *self.active_sysfs_governor.write().unwrap() = governor.to_string();
        *self.active_sysfs_epp.write().unwrap() = epp.to_string();
        self.active_turbo.store(turbo, Ordering::SeqCst);
        self.active_mglru_min_ttl_ms.store(mglru_min_ttl_ms, Ordering::SeqCst);

        Ok(())
    }

    /// Invoke sibling kernel-optimizer `dynabook-kernel-ctl` CLI fallback
    fn invoke_kernel_ctl_profile(&self, profile: &str) -> bool {
        let binary_path = self.config.kernel_ctl_bin.as_deref().or_else(|| {
            if Path::new(SYSTEM_KERNEL_CTL_BIN).exists() {
                Some(Path::new(SYSTEM_KERNEL_CTL_BIN))
            } else if Path::new(SIBLING_KERNEL_CTL_BIN).exists() {
                Some(Path::new(SIBLING_KERNEL_CTL_BIN))
            } else {
                None
            }
        });

        if let Some(bin) = binary_path {
            if bin.exists() {
                if let Ok(output) = Command::new(bin).args(["profile", profile]).output() {
                    return output.status.success();
                }
            }
        }

        false
    }

    // ==============================================================================
    // 6. Status & Inspection
    // ==============================================================================

    /// Query comprehensive real-time status snapshot
    pub fn status(&self) -> DynatuneGovernorStatus {
        let (cpu_pkg_temp_c, battery_pct, is_discharging, ac_connected) = self.query_hardware_telemetry();
        let thermal_clamped = self.is_thermal_clamped.load(Ordering::Relaxed);
        let active_tasks = self.active_heavy_tasks.load(Ordering::Relaxed);
        let current_mode = *self.current_mode.read().unwrap();

        let safeguard_reason = if thermal_clamped {
            if cpu_pkg_temp_c >= self.config.thermal_clamp_threshold_c {
                Some(format!(
                    "CPU package temperature thermal limit breached ({:.1}°C >= {:.1}°C)",
                    cpu_pkg_temp_c, self.config.thermal_clamp_threshold_c
                ))
            } else {
                Some(format!(
                    "Critical battery discharge capacity alert ({}% < {}%)",
                    battery_pct, self.config.battery_critical_pct
                ))
            }
        } else {
            None
        };

        let concurrency_limit = if thermal_clamped {
            CLAMPED_CONCURRENCY_LIMIT
        } else {
            self.config.nominal_concurrency
        };

        let active_workloads = self.workload_registry.read().unwrap().clone();

        DynatuneGovernorStatus {
            active_heavy_tasks: active_tasks,
            current_mode,
            target_governor: self.active_sysfs_governor.read().unwrap().clone(),
            target_epp: self.active_sysfs_epp.read().unwrap().clone(),
            turbo_enabled: self.active_turbo.load(Ordering::Relaxed),
            mglru_min_ttl_ms: self.active_mglru_min_ttl_ms.load(Ordering::Relaxed),
            cpu_pkg_temp_c,
            battery_pct,
            ac_connected,
            is_discharging,
            thermal_clamped,
            safeguard_reason,
            concurrency_limit,
            ipc_socket_active: self.ipc_socket_active.load(Ordering::Relaxed),
            ipc_socket_path: self.config.socket_path.to_string_lossy().to_string(),
            last_transition: self.last_transition_reason.read().unwrap().clone(),
            total_ramp_ups: self.total_ramp_ups.load(Ordering::Relaxed),
            total_idle_drops: self.total_idle_drops.load(Ordering::Relaxed),
            total_thermal_clamps: self.total_thermal_clamps.load(Ordering::Relaxed),
            active_workloads,
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }
}

impl Default for DynatuneIpcGovernor {
    fn default() -> Self {
        Self::new()
    }
}

// ==============================================================================
// 7. RAII Workload Lease
// ==============================================================================

/// RAII Guard ensuring deterministic boost acquisition and auto-sleep idle drop
pub struct DynatuneWorkloadLease {
    governor: Arc<DynatuneIpcGovernor>,
    workload: String,
    clamped: bool,
    active: bool,
}

impl DynatuneWorkloadLease {
    /// Workload identifier string
    pub fn workload(&self) -> &str {
        &self.workload
    }

    /// Whether this lease is operating under thermal or battery clamp
    pub fn is_clamped(&self) -> bool {
        self.clamped
    }

    /// Recommended concurrency limit for this lease (1 if clamped, nominal otherwise)
    pub fn concurrency_limit(&self) -> usize {
        if self.clamped {
            CLAMPED_CONCURRENCY_LIMIT
        } else {
            self.governor.config().nominal_concurrency
        }
    }

    /// Access reference to underlying governor
    pub fn governor(&self) -> &Arc<DynatuneIpcGovernor> {
        &self.governor
    }

    /// Manually consume and drop the lease early
    pub fn finish(mut self) {
        if self.active {
            self.active = false;
            self.governor.release_lease_internal(&self.workload);
        }
    }
}

impl Drop for DynatuneWorkloadLease {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            self.governor.release_lease_internal(&self.workload);
        }
    }
}

// ==============================================================================
// 8. Helpers
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
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use tempfile::tempdir;

    fn setup_mock_sysfs(root: &Path) {
        // cpufreq
        let policy0 = root.join("sys/devices/system/cpu/cpufreq/policy0");
        let policy1 = root.join("sys/devices/system/cpu/cpufreq/policy1");
        fs::create_dir_all(&policy0).unwrap();
        fs::create_dir_all(&policy1).unwrap();
        fs::write(policy0.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(policy0.join("energy_performance_preference"), "power\n").unwrap();
        fs::write(policy1.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(policy1.join("energy_performance_preference"), "power\n").unwrap();

        // intel_pstate
        let pstate = root.join("sys/devices/system/cpu/intel_pstate");
        fs::create_dir_all(&pstate).unwrap();
        fs::write(pstate.join("no_turbo"), "1\n").unwrap();
        fs::write(pstate.join("max_perf_pct"), "70\n").unwrap();
        fs::write(pstate.join("hwp_dynamic_boost"), "0\n").unwrap();

        // mglru
        let mglru = root.join("sys/kernel/mm/lru_gen");
        fs::create_dir_all(&mglru).unwrap();
        fs::write(mglru.join("min_ttl_ms"), "0\n").unwrap();

        // power supply
        let ps = root.join("sys/class/power_supply");
        let ac = ps.join("AC");
        let bat = ps.join("BAT0");
        fs::create_dir_all(&ac).unwrap();
        fs::create_dir_all(&bat).unwrap();
        fs::write(ac.join("online"), "1\n").unwrap();
        fs::write(bat.join("capacity"), "85\n").unwrap();
        fs::write(bat.join("status"), "Full\n").unwrap();

        // hwmon
        let hwmon = root.join("sys/class/hwmon/hwmon0");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("name"), "coretemp\n").unwrap();
        fs::write(hwmon.join("temp1_input"), "52000\n").unwrap();
    }

    #[test]
    fn test_workload_boost_lease_acquisition_and_release() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("dynatune.sock");
        setup_mock_sysfs(&sysfs_root);

        let governor = Arc::new(DynatuneIpcGovernor::for_test(&sysfs_root, &sock_path));

        // Initial state is idle
        assert_eq!(governor.status().active_heavy_tasks, 0);
        assert_eq!(governor.status().current_mode, GovernorMode::PowerSave);

        // 1. Acquire lease 1 (LLM inference)
        let lease1 = governor.acquire_lease("llm_inference");
        assert_eq!(lease1.workload(), "llm_inference");
        assert!(!lease1.is_clamped());
        assert_eq!(lease1.concurrency_limit(), DEFAULT_NOMINAL_CONCURRENCY_LIMIT);

        let status_boosted = governor.status();
        assert_eq!(status_boosted.active_heavy_tasks, 1);
        assert_eq!(status_boosted.current_mode, GovernorMode::Performance);
        assert_eq!(status_boosted.target_governor, "performance");
        assert_eq!(status_boosted.target_epp, "performance");
        assert!(status_boosted.turbo_enabled);
        assert_eq!(status_boosted.mglru_min_ttl_ms, BURST_MGLRU_MIN_TTL_MS);

        // Verify sysfs files were updated
        let gov_file = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov_file.trim(), "performance");
        let epp_file = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp_file.trim(), "performance");
        let mglru_file = fs::read_to_string(sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(mglru_file.trim(), "1000");

        // 2. Acquire lease 2 (Wasm compilation)
        {
            let lease2 = governor.acquire_lease("wasm_compilation");
            assert_eq!(lease2.workload(), "wasm_compilation");
            assert_eq!(governor.status().active_heavy_tasks, 2);
        }
        // lease2 dropped, active heavy tasks back to 1, still boosted
        assert_eq!(governor.status().active_heavy_tasks, 1);
        assert_eq!(governor.status().current_mode, GovernorMode::Performance);

        // 3. Drop lease1
        drop(lease1);
        assert_eq!(governor.status().active_heavy_tasks, 0);
    }

    #[tokio::test]
    async fn test_idle_drop_transition_after_grace_period() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("dynatune.sock");
        setup_mock_sysfs(&sysfs_root);

        let governor = Arc::new(DynatuneIpcGovernor::for_test(&sysfs_root, &sock_path));

        // Start task and boost
        let lease = governor.acquire_lease("microvm_spinup");
        assert_eq!(governor.status().current_mode, GovernorMode::Performance);
        assert_eq!(governor.status().active_heavy_tasks, 1);

        // Drop task -> triggers grace period timer (50ms in test config)
        drop(lease);
        assert_eq!(governor.status().active_heavy_tasks, 0);

        // Wait for grace period to expire
        tokio::time::sleep(Duration::from_millis(100)).await;

        let status_idle = governor.status();
        assert_eq!(status_idle.current_mode, GovernorMode::PowerSave);
        assert_eq!(status_idle.target_governor, "powersave");
        assert_eq!(status_idle.target_epp, "power");
        assert!(!status_idle.turbo_enabled);
        assert_eq!(status_idle.mglru_min_ttl_ms, IDLE_MGLRU_MIN_TTL_MS);

        // Verify sysfs dropped
        let gov_file = fs::read_to_string(sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov_file.trim(), "powersave");
        let mglru_file = fs::read_to_string(sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(mglru_file.trim(), "0");
    }

    #[test]
    fn test_thermal_clamp_override_and_hysteresis_recovery() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("dynatune.sock");
        setup_mock_sysfs(&sysfs_root);

        let governor = Arc::new(DynatuneIpcGovernor::for_test(&sysfs_root, &sock_path));

        // 1. Thermal clamp condition: CPU package temp = 81.5°C >= 78.0°C
        governor.set_telemetry_override(Some(TelemetryOverride {
            cpu_temp_c: Some(81.5),
            battery_pct: Some(90),
            is_discharging: Some(false),
            ac_connected: Some(true),
        }));

        let clamped_lease = governor.acquire_lease("llm_inference");
        assert!(clamped_lease.is_clamped(), "Must engage thermal clamp");
        assert_eq!(clamped_lease.concurrency_limit(), CLAMPED_CONCURRENCY_LIMIT, "Must restrict concurrency to 1 worker");

        let status_clamped = governor.status();
        assert!(status_clamped.thermal_clamped);
        assert_eq!(status_clamped.current_mode, GovernorMode::Clamped);
        assert_eq!(status_clamped.target_governor, "powersave");
        assert_eq!(status_clamped.concurrency_limit, 1);
        assert!(status_clamped.safeguard_reason.as_ref().unwrap().contains("thermal limit breached"));

        drop(clamped_lease);

        // 2. Hysteresis check: intermediate temp 74.0°C (between 70.0 and 78.0) must REMAIN clamped!
        governor.set_telemetry_override(Some(TelemetryOverride {
            cpu_temp_c: Some(74.0),
            battery_pct: Some(90),
            is_discharging: Some(false),
            ac_connected: Some(true),
        }));

        assert!(governor.evaluate_safeguard(), "Must remain clamped due to hysteresis at 74.0°C");

        // 3. Recovery check: temperature cools below 70.0°C (e.g. 68.0°C) -> clamp disengages
        governor.set_telemetry_override(Some(TelemetryOverride {
            cpu_temp_c: Some(68.0),
            battery_pct: Some(90),
            is_discharging: Some(false),
            ac_connected: Some(true),
        }));

        assert!(!governor.evaluate_safeguard(), "Must disengage clamp when temperature drops < 70.0°C");

        // Now new lease acquires full performance boost
        let normal_lease = governor.acquire_lease("wasm_compilation");
        assert!(!normal_lease.is_clamped());
        assert_eq!(normal_lease.concurrency_limit(), DEFAULT_NOMINAL_CONCURRENCY_LIMIT);
        assert_eq!(governor.status().current_mode, GovernorMode::Performance);
    }

    #[test]
    fn test_battery_discharge_safeguard() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("dynatune.sock");
        setup_mock_sysfs(&sysfs_root);

        let governor = Arc::new(DynatuneIpcGovernor::for_test(&sysfs_root, &sock_path));

        // Battery critical: 15% (< 20%) while discharging
        governor.set_telemetry_override(Some(TelemetryOverride {
            cpu_temp_c: Some(60.0),
            battery_pct: Some(15),
            is_discharging: Some(true),
            ac_connected: Some(false),
        }));

        let lease = governor.acquire_lease("vector_batch");
        assert!(lease.is_clamped(), "Must clamp when battery < 20% on discharge");
        assert_eq!(lease.concurrency_limit(), 1);

        let status = governor.status();
        assert!(status.thermal_clamped);
        assert!(status.safeguard_reason.as_ref().unwrap().contains("battery discharge capacity alert"));
    }

    #[test]
    fn test_ipc_socket_communication_with_server() {
        let temp_dir = tempdir().unwrap();
        let sysfs_root = temp_dir.path().join("sysfs");
        let sock_path = temp_dir.path().join("mock_dynatune.sock");
        setup_mock_sysfs(&sysfs_root);

        // Bind mock Unix socket server
        let listener = UnixListener::bind(&sock_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let governor = Arc::new(DynatuneIpcGovernor::for_test(&sysfs_root, &sock_path));
        assert!(governor.check_ipc_socket(), "Socket check should detect active listener");

        // Accept connection in background thread and respond
        let sock_thread = std::thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0u8; 1024];
                    if let Ok(n) = stream.read(&mut buf) {
                        if n > 0 {
                            let resp = serde_json::json!({
                                "success": true,
                                "applied_governor": "performance",
                                "applied_epp": "performance"
                            }).to_string() + "\n";
                            let _ = stream.write_all(resp.as_bytes());
                            break;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let lease = governor.acquire_lease("llm_inference");
        assert_eq!(lease.workload(), "llm_inference");
        sock_thread.join().unwrap();
    }
}
