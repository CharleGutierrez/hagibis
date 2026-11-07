//! # Hardware-Aware Dynamic Acceleration Bridge (`HardwareDynamicFfiBridge`)
//!
//! Production-grade hardware telemetry reading and racing governor orchestration bridge
//! connecting `hgb-daemon` directly to Linux sysfs, procfs, the sibling Dynatune daemon
//! (`/run/dynatune.sock`), and Kernel-Optimizer (`dynabook-kernel-ctl`, `plugins/cpu-governor.sh`).
//!
//! ## Key Subsystems:
//! 1. **Direct Linux Sysfs & Procfs Telemetry Reading**:
//!    - Thermal Zones: `/sys/class/thermal/thermal_zone*/temp` with exact millidegree Celsius precision.
//!    - Battery State: `/sys/class/power_supply/BAT0/status`, `capacity`, `power_now` (or `current_now * voltage_now`).
//!    - Memory Pressure: `/proc/pressure/memory` some/full stall metrics & Multi-Gen LRU (MGLRU) reclaim efficiency in `/proc/vmstat`.
//! 2. **Real-Time Dynamic Governor Signaling**:
//!    - On heavy workload trigger (`HeavyCompile`, `VectorSearchBurst`, `LlmInference`):
//!      * Shift CPU frequency governor to `performance`
//!      * Set Energy Performance Preference (EPP) to `performance`
//!      * Enable Intel Turbo Boost (`intel_pstate/no_turbo = 0`)
//!      * Configure Linux MGLRU memory compaction (`/sys/kernel/mm/lru_gen/min_ttl_ms 1000`)
//!    - On thermal safety limit (> 78.0°C) or critical battery discharge (< 20%):
//!      * Proactively throttle to `powersave` and clamp concurrency to 1 worker.
//!      * Hysteresis recovery: Disengages only when cooled < 70.0°C and battery >= 22% (or on AC).
//!    - On idle timeout:
//!      * Gracefully decay to `powersave` and EPP `balance_power`, resetting MGLRU `min_ttl_ms 0`.
//! 3. **RAII `HardwareAccelerationGuard`**:
//!    - Guaranteed automatic rollback to baseline upon drop.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use hgb_core::error::{HgbError, Result};

// ==============================================================================
// 1. Constants & Tuning Thresholds
// ==============================================================================

/// Default Unix domain socket path for Dynatune daemon
pub const DEFAULT_DYNATUNE_SOCK: &str = "/run/dynatune.sock";

/// System-wide install path for dynabook-kernel-ctl
pub const SYSTEM_KERNEL_CTL_BIN: &str = "/usr/local/bin/dynabook-kernel-ctl";

/// Sibling workspace path for dynabook-kernel-ctl
pub const SIBLING_KERNEL_CTL_BIN: &str = "/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl";

/// Sibling path for cpu-governor.sh plugin
pub const SIBLING_CPU_GOVERNOR_SCRIPT: &str = "/home/toshiba/My AI Project/kernel-optimizer/plugins/cpu-governor.sh";

/// Secondary cpu-governor script fallback
pub const SECONDARY_CPU_GOVERNOR_SCRIPT: &str = "/home/toshiba/My AI Project/hagibis/scripts/cpu-governor.sh";

/// Emergency thermal throttle threshold in Celsius (78.0°C)
pub const THERMAL_CLAMP_THRESHOLD_C: f64 = 78.0;

/// Thermal recovery threshold in Celsius with hysteresis (70.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 70.0;

/// Critical battery capacity percentage threshold on discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;

/// Battery recovery threshold with hysteresis (22%)
pub const BATTERY_RECOVERY_DISCHARGE_PCT: u32 = 22;

/// Linux Multi-Gen LRU (MGLRU) burst target (1000ms) to prioritize memory compaction
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;

/// Linux Multi-Gen LRU (MGLRU) baseline / idle target (0ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Default auto-sleep idle grace period before dropping into ultra-low-power mode
pub const DEFAULT_IDLE_GRACE_PERIOD_SECS: u64 = 3;

/// Clamped concurrency limit under thermal or critical battery stress (1 worker)
pub const CLAMPED_CONCURRENCY_LIMIT: usize = 1;

/// Default nominal concurrency limit when operating under healthy thermal conditions
pub const DEFAULT_NOMINAL_CONCURRENCY_LIMIT: usize = 4;

/// High-throughput boost concurrency limit
pub const DEFAULT_BOOST_CONCURRENCY_LIMIT: usize = 8;

// ==============================================================================
// 2. Data Types & Models
// ==============================================================================

/// Heavy workloads triggering proactive hardware-aware dynamic ramp-up
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccelerationWorkload {
    /// Heavy compilation bursts (e.g. `cargo build`, `zig build`, wasm compilation)
    HeavyCompile,
    /// Vector embedding search bursts (e.g. HNSW/IVF indexing, SIMD cosine/dot bursts)
    VectorSearchBurst,
    /// Local LLM inference batches and speculative draft generation
    LlmInference,
    /// Custom heavy workload identified by string key
    Custom(String),
}

impl AccelerationWorkload {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().replace(['-', ' '], "_").as_str() {
            "heavy_compile" | "cargo_build" | "zig_build" | "compile" | "build" | "wasm_compilation" => {
                Self::HeavyCompile
            }
            "vector_search_burst" | "vector_search" | "vector_batch" | "embedding" | "rag" => {
                Self::VectorSearchBurst
            }
            "llm_inference" | "llm" | "inference" | "generation" => Self::LlmInference,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::HeavyCompile => "heavy_compile",
            Self::VectorSearchBurst => "vector_search_burst",
            Self::LlmInference => "llm_inference",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for AccelerationWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Target Governor & Power Profiles
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareGovernorProfile {
    /// High-performance burst mode (governor: `performance`, EPP: `performance`, turbo: on, MGLRU: 1000)
    BurstPerformance,
    /// Balanced standard mode (governor: `powersave`, EPP: `balance_performance`, turbo: on, MGLRU: 0)
    BalancedNominal,
    /// Graceful idle / power-saving mode (governor: `powersave`, EPP: `balance_power`, turbo: off, MGLRU: 0)
    DecayedIdle,
    /// Emergency safety clamped under thermal or battery distress (governor: `powersave`, EPP: `power`, concurrency: 1)
    EmergencyClamped,
}

impl HardwareGovernorProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BurstPerformance => "burst_performance",
            Self::BalancedNominal => "balanced_nominal",
            Self::DecayedIdle => "decayed_idle",
            Self::EmergencyClamped => "emergency_clamped",
        }
    }
}

impl std::fmt::Display for HardwareGovernorProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Individual thermal zone telemetry reading with millidegree Celsius precision
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalZoneMetric {
    pub zone_id: usize,
    pub name: String,
    pub zone_type: String,
    pub temp_c: f64,
    pub temp_milli_c: i64,
}

/// Thermal subsystem telemetry snapshot
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalTelemetry {
    pub cpu_pkg_temp_c: f64,
    pub max_zone_temp_c: f64,
    pub zones: Vec<ThermalZoneMetric>,
    pub is_thermal_clamped: bool,
    pub is_thermal_recovery: bool,
}

/// Battery subsystem telemetry snapshot
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatteryTelemetry {
    pub battery_name: String,
    pub status: String,
    pub capacity_pct: u32,
    pub power_now_w: f64,
    pub power_now_uw: u64,
    pub is_discharging: bool,
    pub ac_online: bool,
    pub is_battery_critical: bool,
    pub energy_now_wh: f64,
    pub energy_full_wh: f64,
    pub health_pct: f64,
}

/// Linux Pressure Stall Information (PSI) record
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct MemoryPressureRecord {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

/// Memory pressure & MGLRU reclaim efficiency telemetry snapshot
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryTelemetry {
    pub psi_some: MemoryPressureRecord,
    pub psi_full: MemoryPressureRecord,
    pub pgscan_total: u64,
    pub pgsteal_total: u64,
    pub mglru_reclaim_efficiency_pct: f64,
    pub mglru_enabled: bool,
    pub mglru_min_ttl_ms: u64,
    pub ram_total_mb: u64,
    pub ram_available_mb: u64,
    pub ram_available_pct: f64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
}

/// Safety clamp evaluation state
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SafetyClampState {
    Nominal,
    ThermalClamped {
        cpu_temp_c: f64,
        threshold_c: f64,
    },
    BatteryDischargeClamped {
        battery_pct: u32,
        threshold_pct: u32,
    },
}

impl SafetyClampState {
    pub fn is_clamped(&self) -> bool {
        !matches!(self, Self::Nominal)
    }

    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Nominal => None,
            Self::ThermalClamped { cpu_temp_c, threshold_c } => Some(format!(
                "Thermal safety limit exceeded: CPU temperature {:.2}°C >= {:.2}°C",
                cpu_temp_c, threshold_c
            )),
            Self::BatteryDischargeClamped { battery_pct, threshold_pct } => Some(format!(
                "Critical battery discharge alert: battery capacity {}% < {}%",
                battery_pct, threshold_pct
            )),
        }
    }
}

/// Comprehensive real-time telemetry snapshot returned by `HardwareDynamicFfiBridge`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareBridgeTelemetrySnapshot {
    pub thermal: ThermalTelemetry,
    pub battery: BatteryTelemetry,
    pub memory: MemoryTelemetry,
    pub clamp_state: SafetyClampState,
    pub current_profile: HardwareGovernorProfile,
    pub active_governor: String,
    pub active_epp: String,
    pub turbo_enabled: bool,
    pub mglru_min_ttl_ms: u64,
    pub active_heavy_tasks: usize,
    pub active_workloads: HashMap<String, usize>,
    pub concurrency_limit: usize,
    pub dynatune_socket_active: bool,
    pub kernel_optimizer_available: bool,
    pub last_transition_reason: String,
    pub total_burst_escalations: u64,
    pub total_idle_decays: u64,
    pub total_safety_clamps: u64,
    pub timestamp_epoch_ms: u64,
}

/// Deterministic telemetry overrides for unit tests
#[derive(Debug, Clone, Default)]
pub struct HardwareTelemetryOverride {
    pub cpu_temp_c: Option<f64>,
    pub battery_pct: Option<u32>,
    pub is_discharging: Option<bool>,
    pub ac_online: Option<bool>,
    pub psi_some_avg10: Option<f64>,
    pub mglru_reclaim_efficiency: Option<f64>,
}

/// Configuration settings for `HardwareDynamicFfiBridge`
#[derive(Debug, Clone)]
pub struct HardwareBridgeConfig {
    pub sysfs_root: PathBuf,
    pub procfs_root: PathBuf,
    pub dynatune_sock: PathBuf,
    pub kernel_ctl_bin: Option<PathBuf>,
    pub cpu_governor_script: Option<PathBuf>,
    pub idle_grace_period: Duration,
    pub thermal_clamp_threshold_c: f64,
    pub thermal_recovery_threshold_c: f64,
    pub battery_critical_discharge_pct: u32,
    pub battery_recovery_discharge_pct: u32,
    pub nominal_concurrency: usize,
    pub boost_concurrency: usize,
}

impl Default for HardwareBridgeConfig {
    fn default() -> Self {
        let kernel_ctl_bin = if Path::new(SYSTEM_KERNEL_CTL_BIN).exists() {
            Some(PathBuf::from(SYSTEM_KERNEL_CTL_BIN))
        } else if Path::new(SIBLING_KERNEL_CTL_BIN).exists() {
            Some(PathBuf::from(SIBLING_KERNEL_CTL_BIN))
        } else {
            None
        };

        let cpu_governor_script = if Path::new(SIBLING_CPU_GOVERNOR_SCRIPT).exists() {
            Some(PathBuf::from(SIBLING_CPU_GOVERNOR_SCRIPT))
        } else if Path::new(SECONDARY_CPU_GOVERNOR_SCRIPT).exists() {
            Some(PathBuf::from(SECONDARY_CPU_GOVERNOR_SCRIPT))
        } else {
            None
        };

        Self {
            sysfs_root: PathBuf::from("/"),
            procfs_root: PathBuf::from("/"),
            dynatune_sock: PathBuf::from(DEFAULT_DYNATUNE_SOCK),
            kernel_ctl_bin,
            cpu_governor_script,
            idle_grace_period: Duration::from_secs(DEFAULT_IDLE_GRACE_PERIOD_SECS),
            thermal_clamp_threshold_c: THERMAL_CLAMP_THRESHOLD_C,
            thermal_recovery_threshold_c: THERMAL_RECOVERY_THRESHOLD_C,
            battery_critical_discharge_pct: BATTERY_CRITICAL_DISCHARGE_PCT,
            battery_recovery_discharge_pct: BATTERY_RECOVERY_DISCHARGE_PCT,
            nominal_concurrency: DEFAULT_NOMINAL_CONCURRENCY_LIMIT,
            boost_concurrency: DEFAULT_BOOST_CONCURRENCY_LIMIT,
        }
    }
}

// ==============================================================================
// 3. HardwareDynamicFfiBridge Core Implementation
// ==============================================================================

/// Production-grade Hardware Dynamic FFI & Racing Governor Bridge
pub struct HardwareDynamicFfiBridge {
    config: HardwareBridgeConfig,
    active_heavy_tasks: Arc<AtomicUsize>,
    workload_registry: Arc<RwLock<HashMap<String, usize>>>,
    current_profile: Arc<RwLock<HardwareGovernorProfile>>,
    is_clamped: Arc<AtomicBool>,
    idle_generation: Arc<AtomicU64>,
    dynatune_socket_active: Arc<AtomicBool>,
    total_burst_escalations: Arc<AtomicU64>,
    total_idle_decays: Arc<AtomicU64>,
    total_safety_clamps: Arc<AtomicU64>,
    last_transition_reason: Arc<RwLock<String>>,
    telemetry_override: Arc<RwLock<Option<HardwareTelemetryOverride>>>,
    idle_notify: Arc<Notify>,
    active_governor: Arc<RwLock<String>>,
    active_epp: Arc<RwLock<String>>,
    active_turbo: Arc<AtomicBool>,
    active_mglru_min_ttl_ms: Arc<AtomicU64>,
    guard_counter: Arc<AtomicU64>,
}

impl HardwareDynamicFfiBridge {
    /// Create new bridge with default production configuration
    pub fn new() -> Arc<Self> {
        Self::with_config(HardwareBridgeConfig::default())
    }

    /// Create bridge with custom configuration
    pub fn with_config(config: HardwareBridgeConfig) -> Arc<Self> {
        let bridge = Arc::new(Self {
            config,
            active_heavy_tasks: Arc::new(AtomicUsize::new(0)),
            workload_registry: Arc::new(RwLock::new(HashMap::new())),
            current_profile: Arc::new(RwLock::new(HardwareGovernorProfile::DecayedIdle)),
            is_clamped: Arc::new(AtomicBool::new(false)),
            idle_generation: Arc::new(AtomicU64::new(0)),
            dynatune_socket_active: Arc::new(AtomicBool::new(false)),
            total_burst_escalations: Arc::new(AtomicU64::new(0)),
            total_idle_decays: Arc::new(AtomicU64::new(0)),
            total_safety_clamps: Arc::new(AtomicU64::new(0)),
            last_transition_reason: Arc::new(RwLock::new(
                "Initialized in decayed idle power-saving mode".to_string(),
            )),
            telemetry_override: Arc::new(RwLock::new(None)),
            idle_notify: Arc::new(Notify::new()),
            active_governor: Arc::new(RwLock::new("powersave".to_string())),
            active_epp: Arc::new(RwLock::new("balance_power".to_string())),
            active_turbo: Arc::new(AtomicBool::new(false)),
            active_mglru_min_ttl_ms: Arc::new(AtomicU64::new(0)),
            guard_counter: Arc::new(AtomicU64::new(0)),
        });

        bridge.check_dynatune_socket();
        bridge
    }

    /// Create isolated bridge for unit testing with mocked sysfs & procfs directories
    pub fn for_test(
        sysfs_root: impl AsRef<Path>,
        procfs_root: impl AsRef<Path>,
        sock_path: impl AsRef<Path>,
    ) -> Arc<Self> {
        let mut config = HardwareBridgeConfig::default();
        config.sysfs_root = sysfs_root.as_ref().to_path_buf();
        config.procfs_root = procfs_root.as_ref().to_path_buf();
        config.dynatune_sock = sock_path.as_ref().to_path_buf();
        config.idle_grace_period = Duration::from_millis(50);
        Self::with_config(config)
    }

    /// Access active configuration
    pub fn config(&self) -> &HardwareBridgeConfig {
        &self.config
    }

    /// Set deterministic telemetry override for testing
    pub fn set_telemetry_override(&self, ov: Option<HardwareTelemetryOverride>) {
        let mut guard = self.telemetry_override.write().unwrap();
        *guard = ov;
    }

    // ==============================================================================
    // 4. Telemetry Collection (Sysfs & Procfs)
    // ==============================================================================

    /// Collect comprehensive hardware telemetry snapshot
    pub fn query_telemetry(&self) -> HardwareBridgeTelemetrySnapshot {
        let thermal = self.read_thermal_telemetry();
        let battery = self.read_battery_telemetry();
        let memory = self.read_memory_telemetry();
        let clamp_state = self.evaluate_safety_clamp(&thermal, &battery);

        let active_tasks = self.active_heavy_tasks.load(Ordering::Relaxed);
        let current_profile = *self.current_profile.read().unwrap();
        let active_workloads = self.workload_registry.read().unwrap().clone();
        let is_clamped = clamp_state.is_clamped();

        let concurrency_limit = if is_clamped {
            CLAMPED_CONCURRENCY_LIMIT
        } else if active_tasks > 0 {
            self.config.boost_concurrency
        } else {
            self.config.nominal_concurrency
        };

        HardwareBridgeTelemetrySnapshot {
            thermal,
            battery,
            memory,
            clamp_state,
            current_profile,
            active_governor: self.active_governor.read().unwrap().clone(),
            active_epp: self.active_epp.read().unwrap().clone(),
            turbo_enabled: self.active_turbo.load(Ordering::Relaxed),
            mglru_min_ttl_ms: self.active_mglru_min_ttl_ms.load(Ordering::Relaxed),
            active_heavy_tasks: active_tasks,
            active_workloads,
            concurrency_limit,
            dynatune_socket_active: self.dynatune_socket_active.load(Ordering::Relaxed),
            kernel_optimizer_available: self.config.kernel_ctl_bin.is_some()
                || self.config.cpu_governor_script.is_some(),
            last_transition_reason: self.last_transition_reason.read().unwrap().clone(),
            total_burst_escalations: self.total_burst_escalations.load(Ordering::Relaxed),
            total_idle_decays: self.total_idle_decays.load(Ordering::Relaxed),
            total_safety_clamps: self.total_safety_clamps.load(Ordering::Relaxed),
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }

    /// Read live thermal telemetry with exact millidegree Celsius precision
    pub fn read_thermal_telemetry(&self) -> ThermalTelemetry {
        if let Some(ref ov) = *self.telemetry_override.read().unwrap() {
            if let Some(temp_c) = ov.cpu_temp_c {
                let temp_milli = (temp_c * 1000.0).round() as i64;
                return ThermalTelemetry {
                    cpu_pkg_temp_c: temp_c,
                    max_zone_temp_c: temp_c,
                    zones: vec![ThermalZoneMetric {
                        zone_id: 0,
                        name: "thermal_zone0".to_string(),
                        zone_type: "x86_pkg_temp".to_string(),
                        temp_c,
                        temp_milli_c: temp_milli,
                    }],
                    is_thermal_clamped: temp_c >= self.config.thermal_clamp_threshold_c,
                    is_thermal_recovery: temp_c < self.config.thermal_recovery_threshold_c,
                };
            }
        }

        let sysfs = &self.config.sysfs_root;
        let mut zones = Vec::new();
        let mut cpu_temp_milli = None;
        let mut max_milli = 0i64;

        // 1. Primary: Direct /sys/class/thermal/thermal_zone*
        let thermal_dir = sysfs.join("sys/class/thermal");
        if let Ok(entries) = fs::read_dir(&thermal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("thermal_zone") {
                    let zone_id = name
                        .strip_prefix("thermal_zone")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);

                    let zone_type = fs::read_to_string(path.join("type"))
                        .unwrap_or_else(|_| "unknown".to_string())
                        .trim()
                        .to_string();

                    if let Ok(raw_temp) = fs::read_to_string(path.join("temp")) {
                        if let Ok(milli) = raw_temp.trim().parse::<i64>() {
                            let temp_c = milli as f64 / 1000.0;
                            if milli > max_milli {
                                max_milli = milli;
                            }

                            // Match x86 package temp
                            if zone_type == "x86_pkg_temp" {
                                cpu_temp_milli = Some(milli);
                            } else if cpu_temp_milli.is_none()
                                && (zone_type == "acpitz" || zone_type == "cpu-thermal")
                            {
                                cpu_temp_milli = Some(milli);
                            }

                            zones.push(ThermalZoneMetric {
                                zone_id,
                                name: name.to_string(),
                                zone_type,
                                temp_c,
                                temp_milli_c: milli,
                            });
                        }
                    }
                }
            }
        }

        // 2. Secondary fallback: /sys/class/hwmon/hwmon*/
        if cpu_temp_milli.is_none() {
            let hwmon_dir = sysfs.join("sys/class/hwmon");
            if let Ok(entries) = fs::read_dir(&hwmon_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = fs::read_to_string(p.join("name")).unwrap_or_default();
                    if name.trim() == "coretemp" {
                        if let Ok(inputs) = fs::read_dir(&p) {
                            for input in inputs.flatten() {
                                let fname = input.file_name().to_string_lossy().to_string();
                                if fname.starts_with("temp") && fname.ends_with("_input") {
                                    if let Ok(raw) = fs::read_to_string(input.path()) {
                                        if let Ok(milli) = raw.trim().parse::<i64>() {
                                            if milli > max_milli {
                                                max_milli = milli;
                                            }
                                            if cpu_temp_milli.is_none() {
                                                cpu_temp_milli = Some(milli);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let effective_milli = cpu_temp_milli.unwrap_or(if max_milli > 0 { max_milli } else { 45000 });
        let cpu_pkg_temp_c = effective_milli as f64 / 1000.0;
        let max_zone_temp_c = if max_milli > 0 { max_milli as f64 / 1000.0 } else { cpu_pkg_temp_c };

        ThermalTelemetry {
            cpu_pkg_temp_c,
            max_zone_temp_c,
            zones,
            is_thermal_clamped: cpu_pkg_temp_c >= self.config.thermal_clamp_threshold_c,
            is_thermal_recovery: cpu_pkg_temp_c < self.config.thermal_recovery_threshold_c,
        }
    }

    /// Read live battery telemetry from `/sys/class/power_supply/BAT*`
    pub fn read_battery_telemetry(&self) -> BatteryTelemetry {
        if let Some(ref ov) = *self.telemetry_override.read().unwrap() {
            let cap = ov.battery_pct.unwrap_or(85);
            let dis = ov.is_discharging.unwrap_or(false);
            let ac = ov.ac_online.unwrap_or(!dis);
            return BatteryTelemetry {
                battery_name: "BAT0".to_string(),
                status: if dis { "Discharging".to_string() } else { "Charging".to_string() },
                capacity_pct: cap,
                power_now_w: if dis { 15.0 } else { 0.0 },
                power_now_uw: if dis { 15_000_000 } else { 0 },
                is_discharging: dis,
                ac_online: ac,
                is_battery_critical: dis && cap < self.config.battery_critical_discharge_pct,
                energy_now_wh: 45.0,
                energy_full_wh: 50.0,
                health_pct: 90.0,
            };
        }

        let sysfs = &self.config.sysfs_root;
        let ps_dir = sysfs.join("sys/class/power_supply");

        let mut battery_name = "BAT0".to_string();
        let mut status = "Unknown".to_string();
        let mut capacity_pct = 100u32;
        let mut power_now_uw = 0u64;
        let mut is_discharging = false;
        let mut ac_online = true;
        let mut energy_now_wh = 0.0;
        let mut energy_full_wh = 0.0;
        let mut health_pct = 100.0;

        if let Ok(entries) = fs::read_dir(&ps_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(online_raw) = fs::read_to_string(path.join("online")) {
                        ac_online = online_raw.trim() == "1";
                    }
                } else if name.starts_with("BAT") {
                    battery_name = name;
                    if let Ok(st) = fs::read_to_string(path.join("status")) {
                        status = st.trim().to_string();
                        is_discharging = status.eq_ignore_ascii_case("discharging");
                        if status.eq_ignore_ascii_case("charging") || status.eq_ignore_ascii_case("full") {
                            ac_online = true;
                        }
                    }

                    if let Ok(cap_raw) = fs::read_to_string(path.join("capacity")) {
                        if let Ok(cap) = cap_raw.trim().parse::<u32>() {
                            capacity_pct = cap;
                        }
                    }

                    // Read power_now (microwatts) or fallback to current_now * voltage_now
                    if let Ok(p_raw) = fs::read_to_string(path.join("power_now")) {
                        if let Ok(p) = p_raw.trim().parse::<u64>() {
                            power_now_uw = p;
                        }
                    } else {
                        let current_ua = fs::read_to_string(path.join("current_now"))
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .unwrap_or(0);
                        let voltage_uv = fs::read_to_string(path.join("voltage_now"))
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .unwrap_or(7_600_000);
                        power_now_uw = (current_ua * voltage_uv) / 1_000_000;
                    }

                    let voltage_uv = fs::read_to_string(path.join("voltage_now"))
                        .ok()
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .unwrap_or(7_600_000.0);

                    // Energy now
                    if let Ok(e_raw) = fs::read_to_string(path.join("energy_now")) {
                        if let Ok(e) = e_raw.trim().parse::<f64>() {
                            energy_now_wh = e / 1_000_000.0;
                        }
                    } else if let Ok(c_raw) = fs::read_to_string(path.join("charge_now")) {
                        if let Ok(c) = c_raw.trim().parse::<f64>() {
                            energy_now_wh = (c * voltage_uv) / 1_000_000_000_000.0;
                        }
                    }

                    // Energy full
                    if let Ok(e_raw) = fs::read_to_string(path.join("energy_full")) {
                        if let Ok(e) = e_raw.trim().parse::<f64>() {
                            energy_full_wh = e / 1_000_000.0;
                        }
                    } else if let Ok(c_raw) = fs::read_to_string(path.join("charge_full")) {
                        if let Ok(c) = c_raw.trim().parse::<f64>() {
                            energy_full_wh = (c * voltage_uv) / 1_000_000_000_000.0;
                        }
                    }

                    // Design energy
                    let energy_design_wh = fs::read_to_string(path.join("energy_full_design"))
                        .ok()
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .map(|e| e / 1_000_000.0)
                        .unwrap_or(energy_full_wh);

                    if energy_design_wh > 0.0 {
                        health_pct = ((energy_full_wh / energy_design_wh) * 100.0).clamp(0.0, 100.0);
                    }
                }
            }
        }

        let power_now_w = power_now_uw as f64 / 1_000_000.0;
        let is_battery_critical = is_discharging && capacity_pct < self.config.battery_critical_discharge_pct;

        BatteryTelemetry {
            battery_name,
            status,
            capacity_pct,
            power_now_w,
            power_now_uw,
            is_discharging,
            ac_online,
            is_battery_critical,
            energy_now_wh,
            energy_full_wh,
            health_pct,
        }
    }

    /// Read memory pressure metrics from `/proc/pressure/memory` & MGLRU reclaim efficiency from `/proc/vmstat`
    pub fn read_memory_telemetry(&self) -> MemoryTelemetry {
        let procfs = &self.config.procfs_root;
        let sysfs = &self.config.sysfs_root;

        // 1. PSI: /proc/pressure/memory
        let mut psi_some = MemoryPressureRecord::default();
        let mut psi_full = MemoryPressureRecord::default();

        let pressure_path = procfs.join("proc/pressure/memory");
        if let Ok(content) = fs::read_to_string(&pressure_path) {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 {
                    let mut rec = MemoryPressureRecord::default();
                    for part in &parts[1..] {
                        if let Some((k, v)) = part.split_once('=') {
                            match k {
                                "avg10" => rec.avg10 = v.parse::<f64>().unwrap_or(0.0),
                                "avg60" => rec.avg60 = v.parse::<f64>().unwrap_or(0.0),
                                "avg300" => rec.avg300 = v.parse::<f64>().unwrap_or(0.0),
                                "total" => rec.total_us = v.parse::<u64>().unwrap_or(0),
                                _ => {}
                            }
                        }
                    }
                    if parts[0] == "some" {
                        psi_some = rec;
                    } else if parts[0] == "full" {
                        psi_full = rec;
                    }
                }
            }
        }

        // 2. /proc/vmstat: pgscan & pgsteal
        let mut pgscan_total = 0u64;
        let mut pgsteal_total = 0u64;

        let vmstat_path = procfs.join("proc/vmstat");
        if let Ok(content) = fs::read_to_string(&vmstat_path) {
            for line in content.lines() {
                let mut iter = line.split_whitespace();
                if let (Some(key), Some(val_str)) = (iter.next(), iter.next()) {
                    if let Ok(val) = val_str.parse::<u64>() {
                        if key.starts_with("pgscan_") {
                            pgscan_total += val;
                        } else if key.starts_with("pgsteal_") {
                            pgsteal_total += val;
                        }
                    }
                }
            }
        }

        let mglru_reclaim_efficiency_pct = if pgscan_total > 0 {
            ((pgsteal_total as f64 / pgscan_total as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            100.0
        };

        // 3. /sys/kernel/mm/lru_gen/
        let mglru_enabled_path = sysfs.join("sys/kernel/mm/lru_gen/enabled");
        let mglru_enabled = fs::read_to_string(&mglru_enabled_path)
            .ok()
            .and_then(|s| {
                let trimmed = s.trim();
                if let Some(rest) = trimmed.strip_prefix("0x") {
                    u32::from_str_radix(rest, 16).ok()
                } else {
                    trimmed.parse::<u32>().ok()
                }
            })
            .map(|flags| (flags & 1) != 0)
            .unwrap_or(false);

        let mglru_min_ttl_path = sysfs.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        let mglru_min_ttl_ms = fs::read_to_string(&mglru_min_ttl_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);

        // 4. /proc/meminfo
        let meminfo_path = procfs.join("proc/meminfo");
        let mut ram_total_mb = 8192;
        let mut ram_available_mb = 4096;
        let mut swap_total_mb = 4096;
        let mut swap_free_mb = 4096;

        if let Ok(content) = fs::read_to_string(&meminfo_path) {
            for line in content.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let kb = v
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    match k.trim() {
                        "MemTotal" => ram_total_mb = kb / 1024,
                        "MemAvailable" => ram_available_mb = kb / 1024,
                        "SwapTotal" => swap_total_mb = kb / 1024,
                        "SwapFree" => swap_free_mb = kb / 1024,
                        _ => {}
                    }
                }
            }
        }

        let swap_used_mb = swap_total_mb.saturating_sub(swap_free_mb);
        let ram_available_pct = if ram_total_mb > 0 {
            (ram_available_mb as f64 / ram_total_mb as f64) * 100.0
        } else {
            100.0
        };

        // Check override
        if let Some(ref ov) = *self.telemetry_override.read().unwrap() {
            if let Some(eff) = ov.mglru_reclaim_efficiency {
                return MemoryTelemetry {
                    psi_some,
                    psi_full,
                    pgscan_total,
                    pgsteal_total,
                    mglru_reclaim_efficiency_pct: eff,
                    mglru_enabled,
                    mglru_min_ttl_ms,
                    ram_total_mb,
                    ram_available_mb,
                    ram_available_pct,
                    swap_total_mb,
                    swap_used_mb,
                };
            }
        }

        MemoryTelemetry {
            psi_some,
            psi_full,
            pgscan_total,
            pgsteal_total,
            mglru_reclaim_efficiency_pct,
            mglru_enabled,
            mglru_min_ttl_ms,
            ram_total_mb,
            ram_available_mb,
            ram_available_pct,
            swap_total_mb,
            swap_used_mb,
        }
    }

    // ==============================================================================
    // 5. Safety Clamp & Hysteresis Evaluation
    // ==============================================================================

    /// Evaluate thermal and battery safety clamp status with strict hysteresis recovery
    pub fn evaluate_safety_clamp(
        &self,
        thermal: &ThermalTelemetry,
        battery: &BatteryTelemetry,
    ) -> SafetyClampState {
        let was_clamped = self.is_clamped.load(Ordering::Relaxed);

        if was_clamped {
            // Hysteresis Recovery:
            // Must cool below thermal_recovery_threshold_c (< 70.0°C)
            // AND battery must not be critically discharging (either not discharging OR capacity >= recovery threshold 22%)
            let thermal_recovered = thermal.cpu_pkg_temp_c < self.config.thermal_recovery_threshold_c;
            let battery_recovered = !battery.is_discharging
                || battery.capacity_pct >= self.config.battery_recovery_discharge_pct
                || battery.ac_online;

            if thermal_recovered && battery_recovered {
                self.is_clamped.store(false, Ordering::SeqCst);
                SafetyClampState::Nominal
            } else if !thermal_recovered {
                SafetyClampState::ThermalClamped {
                    cpu_temp_c: thermal.cpu_pkg_temp_c,
                    threshold_c: self.config.thermal_clamp_threshold_c,
                }
            } else {
                SafetyClampState::BatteryDischargeClamped {
                    battery_pct: battery.capacity_pct,
                    threshold_pct: self.config.battery_critical_discharge_pct,
                }
            }
        } else {
            // Trigger Check:
            // Temp >= 78.0°C OR battery < 20% on discharge
            let thermal_breach = thermal.cpu_pkg_temp_c >= self.config.thermal_clamp_threshold_c;
            let battery_breach = battery.is_discharging
                && (battery.capacity_pct < self.config.battery_critical_discharge_pct);

            if thermal_breach {
                self.is_clamped.store(true, Ordering::SeqCst);
                SafetyClampState::ThermalClamped {
                    cpu_temp_c: thermal.cpu_pkg_temp_c,
                    threshold_c: self.config.thermal_clamp_threshold_c,
                }
            } else if battery_breach {
                self.is_clamped.store(true, Ordering::SeqCst);
                SafetyClampState::BatteryDischargeClamped {
                    battery_pct: battery.capacity_pct,
                    threshold_pct: self.config.battery_critical_discharge_pct,
                }
            } else {
                SafetyClampState::Nominal
            }
        }
    }

    // ==============================================================================
    // 6. Dynamic Workload Acceleration & RAII Guard Acquisition
    // ==============================================================================

    /// Acquire RAII Hardware Acceleration Guard for an intensive workload
    ///
    /// Dynamically signals the kernel governor to ramp up frequencies (`performance`, EPP `performance`, MGLRU `1000ms`).
    /// If thermal or battery clamp engages, gracefully throttles to `powersave` and clamps concurrency to 1 worker.
    pub fn acquire_acceleration(
        self: &Arc<Self>,
        workload: AccelerationWorkload,
    ) -> HardwareAccelerationGuard {
        let guard_id = self.guard_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let prev_count = self.active_heavy_tasks.fetch_add(1, Ordering::SeqCst);
        self.idle_generation.fetch_add(1, Ordering::SeqCst);

        // Update workload registry
        {
            let mut reg = self.workload_registry.write().unwrap();
            *reg.entry(workload.to_string()).or_insert(0) += 1;
        }

        // Query telemetry and evaluate clamp
        let thermal = self.read_thermal_telemetry();
        let battery = self.read_battery_telemetry();
        let clamp_state = self.evaluate_safety_clamp(&thermal, &battery);

        if clamp_state.is_clamped() {
            // Safeguard engaged -> throttle to powersave and clamp concurrency to 1 worker
            self.total_safety_clamps.fetch_add(1, Ordering::Relaxed);
            *self.current_profile.write().unwrap() = HardwareGovernorProfile::EmergencyClamped;
            *self.last_transition_reason.write().unwrap() = format!(
                "Safeguard clamp engaged for workload '{}': {}",
                workload,
                clamp_state.reason().unwrap_or_default()
            );

            // Apply emergency powersave state
            let _ = self.apply_hardware_profile("powersave", "power", false, IDLE_MGLRU_MIN_TTL_MS);

            HardwareAccelerationGuard {
                bridge: Arc::clone(self),
                workload,
                clamped: true,
                active: true,
                acquired_at: Instant::now(),
                guard_id,
            }
        } else {
            // Proactive Burst Escalation
            if prev_count == 0 || *self.current_profile.read().unwrap() != HardwareGovernorProfile::BurstPerformance {
                self.proactive_burst_escalation(&workload);
            }

            HardwareAccelerationGuard {
                bridge: Arc::clone(self),
                workload,
                clamped: false,
                active: true,
                acquired_at: Instant::now(),
                guard_id,
            }
        }
    }

    /// Proactively ramp up CPU frequencies and memory protection for burst workload
    fn proactive_burst_escalation(&self, workload: &AccelerationWorkload) {
        self.total_burst_escalations.fetch_add(1, Ordering::Relaxed);
        *self.current_profile.write().unwrap() = HardwareGovernorProfile::BurstPerformance;
        *self.last_transition_reason.write().unwrap() = format!(
            "Burst escalation triggered by '{}': governor performance, EPP performance, MGLRU 1000ms",
            workload
        );

        // 1. Primary: IPC command over /run/dynatune.sock
        let _ = self.send_dynatune_ipc_command("ramp_up", Some("performance"), Some("performance"), true, Some(BURST_MGLRU_MIN_TTL_MS));

        // 2. Direct sysfs writes
        let _ = self.apply_hardware_profile("performance", "performance", true, BURST_MGLRU_MIN_TTL_MS);

        // 3. Fallback CLI execution of dynabook-kernel-ctl & cpu-governor.sh
        let _ = self.invoke_kernel_ctl_profile("performance");

        // 4. Synchronize powerprofilesctl
        let _ = Command::new("powerprofilesctl")
            .args(["set", "performance"])
            .output();

        // 5. Nice priority boost
        #[cfg(unix)]
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, -5);
        }
    }

    /// Gracefully decay governor into ultra-low-power idle mode
    fn graceful_idle_decay(&self) {
        self.total_idle_decays.fetch_add(1, Ordering::Relaxed);
        *self.current_profile.write().unwrap() = HardwareGovernorProfile::DecayedIdle;
        *self.last_transition_reason.write().unwrap() =
            "Idle timeout: Task queue drained for grace period, decayed to powersave / balance_power".to_string();

        // 1. Primary: IPC command over /run/dynatune.sock
        let _ = self.send_dynatune_ipc_command("idle_drop", Some("powersave"), Some("balance_power"), false, Some(IDLE_MGLRU_MIN_TTL_MS));

        // 2. Direct sysfs writes: governor powersave, EPP balance_power, turbo off, MGLRU 0
        let _ = self.apply_hardware_profile("powersave", "balance_power", false, IDLE_MGLRU_MIN_TTL_MS);

        // 3. Fallback CLI execution of dynabook-kernel-ctl battery profile
        let _ = self.invoke_kernel_ctl_profile("battery");

        // 4. Synchronize powerprofilesctl
        let _ = Command::new("powerprofilesctl")
            .args(["set", "power-saver"])
            .output();

        // 5. Reset nice priority
        #[cfg(unix)]
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, 0);
        }

        // 6. Notify waiters
        self.idle_notify.notify_waiters();
    }

    /// Release workload lease internally when guard is dropped
    pub(crate) fn release_guard_internal(self: &Arc<Self>, workload: &AccelerationWorkload) {
        // Decrement workload registry
        {
            let mut reg = self.workload_registry.write().unwrap();
            let key = workload.to_string();
            if let Some(count) = reg.get_mut(&key) {
                if *count > 1 {
                    *count -= 1;
                } else {
                    reg.remove(&key);
                }
            }
        }

        // Decrement active task counter
        let prev = self.active_heavy_tasks.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            // Task queue drained to 0 -> initiate graceful idle decay after grace period
            let decay_gen = self.idle_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let bridge_clone = Arc::clone(self);
            let grace_period = self.config.idle_grace_period;

            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    tokio::time::sleep(grace_period).await;
                    if bridge_clone.idle_generation.load(Ordering::SeqCst) == decay_gen
                        && bridge_clone.active_heavy_tasks.load(Ordering::SeqCst) == 0
                    {
                        bridge_clone.graceful_idle_decay();
                    }
                });
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(grace_period);
                    if bridge_clone.idle_generation.load(Ordering::SeqCst) == decay_gen
                        && bridge_clone.active_heavy_tasks.load(Ordering::SeqCst) == 0
                    {
                        bridge_clone.graceful_idle_decay();
                    }
                });
            }
        }
    }

    /// Force immediate idle decay without waiting for grace period
    pub fn force_idle_decay(&self) {
        self.idle_generation.fetch_add(1, Ordering::SeqCst);
        self.graceful_idle_decay();
    }

    /// Notification handle fired whenever governor transitions into idle decay
    pub fn idle_notify(&self) -> &Arc<Notify> {
        &self.idle_notify
    }

    // ==============================================================================
    // 7. Direct Sysfs & Host Control Interfaces
    // ==============================================================================

    /// Apply hardware state directly to Linux sysfs
    fn apply_hardware_profile(
        &self,
        governor: &str,
        epp: &str,
        turbo: bool,
        mglru_min_ttl_ms: u64,
    ) -> Result<()> {
        let sysfs = &self.config.sysfs_root;

        // 1. CPU Scaling Governor & EPP across all policies
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
            // Alternate policy path under cpu*/cpufreq
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

        // 3. Linux Multi-Gen LRU (MGLRU) min_ttl_ms
        let mglru_path = sysfs.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        if mglru_path.exists() {
            let _ = fs::write(&mglru_path, format!("{}\n", mglru_min_ttl_ms));
        }

        *self.active_governor.write().unwrap() = governor.to_string();
        *self.active_epp.write().unwrap() = epp.to_string();
        self.active_turbo.store(turbo, Ordering::SeqCst);
        self.active_mglru_min_ttl_ms.store(mglru_min_ttl_ms, Ordering::SeqCst);

        Ok(())
    }

    /// Check if Dynatune socket `/run/dynatune.sock` is responsive
    pub fn check_dynatune_socket(&self) -> bool {
        let sock_path = &self.config.dynatune_sock;
        if sock_path.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::net::UnixStream;
                if let Ok(mut stream) = UnixStream::connect(sock_path) {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                    let ping = serde_json::json!({ "action": "ping" }).to_string() + "\n";
                    if stream.write_all(ping.as_bytes()).is_ok() {
                        self.dynatune_socket_active.store(true, Ordering::SeqCst);
                        return true;
                    }
                }
            }
        }
        self.dynatune_socket_active.store(false, Ordering::SeqCst);
        false
    }

    /// Send dynamic command over `/run/dynatune.sock`
    fn send_dynatune_ipc_command(
        &self,
        action: &str,
        governor: Option<&str>,
        epp: Option<&str>,
        turbo: bool,
        mglru_min_ttl_ms: Option<u64>,
    ) -> bool {
        let sock_path = &self.config.dynatune_sock;
        if !sock_path.exists() {
            self.dynatune_socket_active.store(false, Ordering::SeqCst);
            return false;
        }

        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            if let Ok(mut stream) = UnixStream::connect(sock_path) {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));

                let payload = serde_json::json!({
                    "action": action,
                    "governor": governor,
                    "epp": epp,
                    "turbo": turbo,
                    "mglru_min_ttl_ms": mglru_min_ttl_ms,
                    "timestamp_ms": current_epoch_ms(),
                });

                let msg = format!("{}\n", payload);
                if stream.write_all(msg.as_bytes()).is_ok() {
                    self.dynatune_socket_active.store(true, Ordering::SeqCst);
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    return true;
                }
            }
        }

        self.dynatune_socket_active.store(false, Ordering::SeqCst);
        false
    }

    /// Invoke sibling kernel-optimizer `dynabook-kernel-ctl` CLI fallback
    fn invoke_kernel_ctl_profile(&self, profile: &str) -> bool {
        if let Some(ref bin) = self.config.kernel_ctl_bin {
            if bin.exists() {
                if let Ok(output) = Command::new(bin).args(["profile", profile]).output() {
                    return output.status.success();
                }
            }
        }

        if let Some(ref script) = self.config.cpu_governor_script {
            if script.exists() {
                if let Ok(output) = Command::new("bash").args([script.to_string_lossy().as_ref(), "profile", profile]).output() {
                    return output.status.success();
                }
            }
        }

        false
    }
}

// ==============================================================================
// 8. RAII HardwareAccelerationGuard
// ==============================================================================

/// RAII Guard ensuring deterministic boost acquisition and automatic rollback upon drop
pub struct HardwareAccelerationGuard {
    bridge: Arc<HardwareDynamicFfiBridge>,
    workload: AccelerationWorkload,
    clamped: bool,
    active: bool,
    acquired_at: Instant,
    guard_id: u64,
}

impl HardwareAccelerationGuard {
    /// Workload identifier triggering this acceleration lease
    pub fn workload(&self) -> &AccelerationWorkload {
        &self.workload
    }

    /// Whether this lease is operating under thermal or battery safety clamp
    pub fn is_clamped(&self) -> bool {
        self.clamped
    }

    /// Recommended concurrency limit for this lease (1 if clamped, boost nominal otherwise)
    pub fn concurrency_limit(&self) -> usize {
        if self.clamped {
            CLAMPED_CONCURRENCY_LIMIT
        } else {
            self.bridge.config().boost_concurrency
        }
    }

    /// Duration since lease acquisition
    pub fn elapsed(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    /// Unique guard lease identifier
    pub fn guard_id(&self) -> u64 {
        self.guard_id
    }

    /// Access underlying bridge reference
    pub fn bridge(&self) -> &Arc<HardwareDynamicFfiBridge> {
        &self.bridge
    }

    /// Manually consume and release the acceleration guard early
    pub fn finish(mut self) {
        if self.active {
            self.active = false;
            self.bridge.release_guard_internal(&self.workload);
        }
    }
}

impl Drop for HardwareAccelerationGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            self.bridge.release_guard_internal(&self.workload);
        }
    }
}

// ==============================================================================
// 9. Helpers
// ==============================================================================

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ==============================================================================
// 10. Comprehensive Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use tempfile::tempdir;

    fn setup_mock_hardware_environment(root: &Path) {
        // 1. cpufreq
        let policy0 = root.join("sys/devices/system/cpu/cpufreq/policy0");
        let policy1 = root.join("sys/devices/system/cpu/cpufreq/policy1");
        fs::create_dir_all(&policy0).unwrap();
        fs::create_dir_all(&policy1).unwrap();
        fs::write(policy0.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(policy0.join("energy_performance_preference"), "balance_power\n").unwrap();
        fs::write(policy1.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(policy1.join("energy_performance_preference"), "balance_power\n").unwrap();

        // 2. intel_pstate
        let pstate = root.join("sys/devices/system/cpu/intel_pstate");
        fs::create_dir_all(&pstate).unwrap();
        fs::write(pstate.join("no_turbo"), "1\n").unwrap();
        fs::write(pstate.join("max_perf_pct"), "70\n").unwrap();
        fs::write(pstate.join("hwp_dynamic_boost"), "0\n").unwrap();

        // 3. mglru
        let mglru = root.join("sys/kernel/mm/lru_gen");
        fs::create_dir_all(&mglru).unwrap();
        fs::write(mglru.join("enabled"), "0x0007\n").unwrap();
        fs::write(mglru.join("min_ttl_ms"), "0\n").unwrap();

        // 4. thermal zones
        let zone0 = root.join("sys/class/thermal/thermal_zone0");
        let zone3 = root.join("sys/class/thermal/thermal_zone3");
        fs::create_dir_all(&zone0).unwrap();
        fs::create_dir_all(&zone3).unwrap();
        fs::write(zone0.join("type"), "acpitz\n").unwrap();
        fs::write(zone0.join("temp"), "42000\n").unwrap();
        fs::write(zone3.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(zone3.join("temp"), "54321\n").unwrap();

        // 5. power supply
        let ps = root.join("sys/class/power_supply");
        let ac = ps.join("ADP1");
        let bat = ps.join("BAT0");
        fs::create_dir_all(&ac).unwrap();
        fs::create_dir_all(&bat).unwrap();
        fs::write(ac.join("online"), "1\n").unwrap();
        fs::write(bat.join("capacity"), "88\n").unwrap();
        fs::write(bat.join("status"), "Full\n").unwrap();
        fs::write(bat.join("power_now"), "12500000\n").unwrap();
        fs::write(bat.join("energy_now"), "44000000\n").unwrap();
        fs::write(bat.join("energy_full"), "50000000\n").unwrap();
        fs::write(bat.join("energy_full_design"), "50000000\n").unwrap();

        // 6. procfs: pressure & vmstat & meminfo
        let proc = root.join("proc");
        let pressure = proc.join("pressure");
        fs::create_dir_all(&pressure).unwrap();

        let psi_content = "some avg10=1.25 avg60=0.85 avg300=0.45 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        fs::write(pressure.join("memory"), psi_content).unwrap();

        let vmstat_content = "pgscan_kswapd 10000\npgscan_direct 2000\npgsteal_kswapd 9500\npgsteal_direct 1900\n";
        fs::write(proc.join("vmstat"), vmstat_content).unwrap();

        let meminfo_content = "MemTotal:        8192000 kB\nMemFree:         2048000 kB\nMemAvailable:    4096000 kB\nSwapTotal:       4096000 kB\nSwapFree:        4096000 kB\n";
        fs::write(proc.join("meminfo"), meminfo_content).unwrap();
    }

    #[test]
    fn test_sysfs_procfs_telemetry_reading() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path();
        setup_mock_hardware_environment(root);

        let sock_path = root.join("dynatune.sock");
        let bridge = HardwareDynamicFfiBridge::for_test(root, root, &sock_path);

        let telemetry = bridge.query_telemetry();

        // Verify Thermal Zones with millidegree precision
        assert_eq!(telemetry.thermal.zones.len(), 2);
        assert_eq!(telemetry.thermal.cpu_pkg_temp_c, 54.321);
        let zone3 = telemetry.thermal.zones.iter().find(|z| z.zone_type == "x86_pkg_temp").unwrap();
        assert_eq!(zone3.temp_milli_c, 54321);
        assert_eq!(zone3.temp_c, 54.321);
        assert!(!telemetry.thermal.is_thermal_clamped);

        // Verify Battery Telemetry
        assert_eq!(telemetry.battery.capacity_pct, 88);
        assert_eq!(telemetry.battery.status, "Full");
        assert!(telemetry.battery.ac_online);
        assert!(!telemetry.battery.is_discharging);
        assert!(!telemetry.battery.is_battery_critical);
        assert_eq!(telemetry.battery.power_now_uw, 12_500_000);
        assert_eq!(telemetry.battery.power_now_w, 12.5);
        assert_eq!(telemetry.battery.health_pct, 100.0);

        // Verify Memory Pressure & MGLRU Reclaim Efficiency
        assert_eq!(telemetry.memory.psi_some.avg10, 1.25);
        assert_eq!(telemetry.memory.psi_some.avg60, 0.85);
        assert_eq!(telemetry.memory.psi_some.avg300, 0.45);
        assert_eq!(telemetry.memory.psi_some.total_us, 123456);

        assert_eq!(telemetry.memory.pgscan_total, 12000);
        assert_eq!(telemetry.memory.pgsteal_total, 11400);
        // 11400 / 12000 * 100% = 95.0%
        assert_eq!(telemetry.memory.mglru_reclaim_efficiency_pct, 95.0);
        assert!(telemetry.memory.mglru_enabled);
        assert_eq!(telemetry.memory.mglru_min_ttl_ms, 0);
        assert_eq!(telemetry.memory.ram_total_mb, 8000);
        assert_eq!(telemetry.memory.ram_available_mb, 4000);
    }

    #[test]
    fn test_heavy_workload_burst_escalation() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path();
        setup_mock_hardware_environment(root);

        let sock_path = root.join("dynatune.sock");
        let bridge = HardwareDynamicFfiBridge::for_test(root, root, &sock_path);

        assert_eq!(bridge.query_telemetry().active_heavy_tasks, 0);
        assert_eq!(bridge.query_telemetry().current_profile, HardwareGovernorProfile::DecayedIdle);

        // 1. HeavyCompile burst escalation
        let guard1 = bridge.acquire_acceleration(AccelerationWorkload::HeavyCompile);
        assert_eq!(guard1.workload(), &AccelerationWorkload::HeavyCompile);
        assert!(!guard1.is_clamped());
        assert_eq!(guard1.concurrency_limit(), DEFAULT_BOOST_CONCURRENCY_LIMIT);

        let status_boosted = bridge.query_telemetry();
        assert_eq!(status_boosted.active_heavy_tasks, 1);
        assert_eq!(status_boosted.current_profile, HardwareGovernorProfile::BurstPerformance);
        assert_eq!(status_boosted.active_governor, "performance");
        assert_eq!(status_boosted.active_epp, "performance");
        assert!(status_boosted.turbo_enabled);
        assert_eq!(status_boosted.mglru_min_ttl_ms, BURST_MGLRU_MIN_TTL_MS);

        // Verify written sysfs files
        let gov_p0 = fs::read_to_string(root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov_p0.trim(), "performance");
        let epp_p0 = fs::read_to_string(root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp_p0.trim(), "performance");
        let turbo_f = fs::read_to_string(root.join("sys/devices/system/cpu/intel_pstate/no_turbo")).unwrap();
        assert_eq!(turbo_f.trim(), "0");
        let mglru_f = fs::read_to_string(root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(mglru_f.trim(), "1000");

        // 2. VectorSearchBurst concurrent escalation
        {
            let guard2 = bridge.acquire_acceleration(AccelerationWorkload::VectorSearchBurst);
            assert_eq!(guard2.workload(), &AccelerationWorkload::VectorSearchBurst);
            assert_eq!(bridge.query_telemetry().active_heavy_tasks, 2);
            assert_eq!(bridge.query_telemetry().current_profile, HardwareGovernorProfile::BurstPerformance);
        }

        // guard2 dropped -> active tasks drops to 1, still boosted
        assert_eq!(bridge.query_telemetry().active_heavy_tasks, 1);
        assert_eq!(bridge.query_telemetry().current_profile, HardwareGovernorProfile::BurstPerformance);

        drop(guard1);
        assert_eq!(bridge.query_telemetry().active_heavy_tasks, 0);
    }

    #[tokio::test]
    async fn test_idle_timeout_graceful_decay() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path();
        setup_mock_hardware_environment(root);

        let sock_path = root.join("dynatune.sock");
        let bridge = HardwareDynamicFfiBridge::for_test(root, root, &sock_path);

        // Acquire LlmInference
        let guard = bridge.acquire_acceleration(AccelerationWorkload::LlmInference);
        assert_eq!(bridge.query_telemetry().current_profile, HardwareGovernorProfile::BurstPerformance);
        assert_eq!(bridge.query_telemetry().active_heavy_tasks, 1);

        // Drop guard -> initiates 50ms test grace period
        drop(guard);
        assert_eq!(bridge.query_telemetry().active_heavy_tasks, 0);

        // Wait 100ms for grace period to elapse
        tokio::time::sleep(Duration::from_millis(100)).await;

        let status_decayed = bridge.query_telemetry();
        assert_eq!(status_decayed.current_profile, HardwareGovernorProfile::DecayedIdle);
        assert_eq!(status_decayed.active_governor, "powersave");
        assert_eq!(status_decayed.active_epp, "balance_power");
        assert!(!status_decayed.turbo_enabled);
        assert_eq!(status_decayed.mglru_min_ttl_ms, IDLE_MGLRU_MIN_TTL_MS);

        // Check sysfs files
        let gov_p0 = fs::read_to_string(root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov_p0.trim(), "powersave");
        let epp_p0 = fs::read_to_string(root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp_p0.trim(), "balance_power");
        let mglru_f = fs::read_to_string(root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(mglru_f.trim(), "0");
    }

    #[test]
    fn test_thermal_clamp_and_hysteresis_recovery() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path();
        setup_mock_hardware_environment(root);

        let sock_path = root.join("dynatune.sock");
        let bridge = HardwareDynamicFfiBridge::for_test(root, root, &sock_path);

        // 1. Thermal spike: 81.5°C >= 78.0°C
        bridge.set_telemetry_override(Some(HardwareTelemetryOverride {
            cpu_temp_c: Some(81.5),
            battery_pct: Some(90),
            is_discharging: Some(false),
            ac_online: Some(true),
            psi_some_avg10: None,
            mglru_reclaim_efficiency: None,
        }));

        let clamped_guard = bridge.acquire_acceleration(AccelerationWorkload::HeavyCompile);
        assert!(clamped_guard.is_clamped(), "Must engage thermal safety clamp");
        assert_eq!(clamped_guard.concurrency_limit(), CLAMPED_CONCURRENCY_LIMIT, "Must clamp concurrency to 1 worker");

        let status_clamped = bridge.query_telemetry();
        assert_eq!(status_clamped.current_profile, HardwareGovernorProfile::EmergencyClamped);
        assert_eq!(status_clamped.active_governor, "powersave");
        assert_eq!(status_clamped.concurrency_limit, 1);
        assert!(status_clamped.clamp_state.is_clamped());
        assert!(status_clamped.clamp_state.reason().unwrap().contains("Thermal safety limit exceeded"));

        drop(clamped_guard);

        // 2. Hysteresis Check: Intermediate temperature 74.0°C (between 70.0°C and 78.0°C) must REMAIN clamped!
        bridge.set_telemetry_override(Some(HardwareTelemetryOverride {
            cpu_temp_c: Some(74.0),
            battery_pct: Some(90),
            is_discharging: Some(false),
            ac_online: Some(true),
            psi_some_avg10: None,
            mglru_reclaim_efficiency: None,
        }));

        let thermal = bridge.read_thermal_telemetry();
        let battery = bridge.read_battery_telemetry();
        let clamp_eval = bridge.evaluate_safety_clamp(&thermal, &battery);
        assert!(clamp_eval.is_clamped(), "Hysteresis must hold clamp active at 74.0°C");

        // 3. Recovery: Temperature cools strictly below 70.0°C (e.g. 68.0°C)
        bridge.set_telemetry_override(Some(HardwareTelemetryOverride {
            cpu_temp_c: Some(68.0),
            battery_pct: Some(90),
            is_discharging: Some(false),
            ac_online: Some(true),
            psi_some_avg10: None,
            mglru_reclaim_efficiency: None,
        }));

        let thermal_cool = bridge.read_thermal_telemetry();
        let battery_cool = bridge.read_battery_telemetry();
        let clamp_recovered = bridge.evaluate_safety_clamp(&thermal_cool, &battery_cool);
        assert!(!clamp_recovered.is_clamped(), "Clamp must disengage once cooled below 70.0°C");

        // Now new acceleration request escalates to full burst performance!
        let normal_guard = bridge.acquire_acceleration(AccelerationWorkload::LlmInference);
        assert!(!normal_guard.is_clamped());
        assert_eq!(normal_guard.concurrency_limit(), DEFAULT_BOOST_CONCURRENCY_LIMIT);
        assert_eq!(bridge.query_telemetry().current_profile, HardwareGovernorProfile::BurstPerformance);
    }

    #[test]
    fn test_battery_aware_throttling() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path();
        setup_mock_hardware_environment(root);

        let sock_path = root.join("dynatune.sock");
        let bridge = HardwareDynamicFfiBridge::for_test(root, root, &sock_path);

        // Battery critical: 15% (< 20%) while discharging
        bridge.set_telemetry_override(Some(HardwareTelemetryOverride {
            cpu_temp_c: Some(58.0),
            battery_pct: Some(15),
            is_discharging: Some(true),
            ac_online: Some(false),
            psi_some_avg10: None,
            mglru_reclaim_efficiency: None,
        }));

        let guard = bridge.acquire_acceleration(AccelerationWorkload::VectorSearchBurst);
        assert!(guard.is_clamped(), "Must clamp on battery discharge < 20%");
        assert_eq!(guard.concurrency_limit(), CLAMPED_CONCURRENCY_LIMIT);

        let status = bridge.query_telemetry();
        assert_eq!(status.current_profile, HardwareGovernorProfile::EmergencyClamped);
        assert!(status.clamp_state.reason().unwrap().contains("Critical battery discharge alert"));

        drop(guard);

        // AC adapter plugged in -> recovers
        bridge.set_telemetry_override(Some(HardwareTelemetryOverride {
            cpu_temp_c: Some(58.0),
            battery_pct: Some(15),
            is_discharging: Some(false),
            ac_online: Some(true),
            psi_some_avg10: None,
            mglru_reclaim_efficiency: None,
        }));

        let normal_guard = bridge.acquire_acceleration(AccelerationWorkload::HeavyCompile);
        assert!(!normal_guard.is_clamped());
        assert_eq!(normal_guard.concurrency_limit(), DEFAULT_BOOST_CONCURRENCY_LIMIT);
    }

    #[test]
    fn test_ipc_socket_signaling_integration() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path();
        setup_mock_hardware_environment(root);

        let sock_path = root.join("mock_dynatune.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        let bridge = HardwareDynamicFfiBridge::for_test(root, root, &sock_path);
        assert!(bridge.check_dynatune_socket());

        // Spawn mock dynatune daemon listener thread
        let handle = std::thread::spawn(move || {
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

        let guard = bridge.acquire_acceleration(AccelerationWorkload::HeavyCompile);
        assert_eq!(guard.workload(), &AccelerationWorkload::HeavyCompile);
        handle.join().unwrap();
    }
}
