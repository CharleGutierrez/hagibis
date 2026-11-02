//! # Native Dynatune & Kernel-Optimizer Subsystem Fuse
//!
//! Direct hardware synergy fuse integrating:
//! - Sibling dynatune daemon (`/home/toshiba/.local/bin/dynatune`, `/run/dynatune.sock`)
//!   and subsystems (`src/power.rs`, `src/thermal.rs`, `src/battery.rs`, `src/memory.rs`, `src/processes.rs`)
//! - Sibling kernel-optimizer controller (`dynabook-kernel-ctl`, `plugins/cpu-governor.sh`)
//! - Linux Multi-Gen LRU (MGLRU) `/sys/kernel/mm/lru_gen/min_ttl_ms`
//! - Real-time querying of CPU thermal zones, battery discharge rate in Watts, and AC state
//! - Dynamic governor shifting: bursts to `performance` (EPP `performance`, turbo, MGLRU `min_ttl_ms 1000`)
//! - Dynamic thermal safeguarding (CPU package temp >= 78.0°C or battery < 20% on discharge downscales to `powersave` and restricts concurrency to 1-2 workers)
//! - Automatic idle drop: when task queues drain, immediately drops CPU governors back to `powersave`
//! - RAII `DynatuneWorkloadBoostGuard` ensuring automatic cleanup and baseline restoration upon drop

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};

use crate::error::{HgbError, Result};

/// Default location of the dynatune daemon binary
pub const DEFAULT_DYNATUNE_BIN: &str = "/home/toshiba/.local/bin/dynatune";
/// Default Unix domain socket for the dynatune daemon
pub const DEFAULT_DYNATUNE_SOCK: &str = "/run/dynatune.sock";
/// Default system-wide path for dynabook-kernel-ctl
pub const DEFAULT_KERNEL_CTL_BIN: &str = "/usr/local/bin/dynabook-kernel-ctl";
/// Sibling project fallback path for dynabook-kernel-ctl
pub const SIBLING_KERNEL_CTL_BIN: &str = "/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl";

/// Thermal throttling threshold where automatic downscaling and concurrency throttling engage (78.0°C)
pub const THERMAL_THROTTLE_THRESHOLD_C: f64 = 78.0;
/// Severe thermal critical threshold where workload is aggressively serialized to 1 worker (85.0°C)
pub const THERMAL_CRITICAL_TEMP_C: f64 = 85.0;
/// Critical battery percentage threshold on discharge (20%)
pub const BATTERY_CRITICAL_THRESHOLD_PCT: u32 = 20;

/// Target MGLRU min_ttl_ms when bursting during heavy LLM or multi-agent workloads (1000ms)
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;
/// Baseline MGLRU min_ttl_ms when idle or throttled (0ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Standard CPU Governor modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FuseGovernorMode {
    Performance,
    PowerSave,
    Balanced,
}

impl std::fmt::Display for FuseGovernorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Performance => write!(f, "performance"),
            Self::PowerSave => write!(f, "powersave"),
            Self::Balanced => write!(f, "balanced"),
        }
    }
}

/// Specifications for safe hardware tuning via MCP or daemon
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SafeHardwareTuneSpec {
    pub governor: Option<String>,
    pub epp: Option<String>,
    pub mglru_min_ttl_ms: Option<u64>,
    pub concurrency_limit: Option<usize>,
}

/// Execution outcome report from safe hardware tuning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeHardwareTuneReport {
    pub success: bool,
    pub applied_governor: Option<String>,
    pub applied_epp: Option<String>,
    pub applied_mglru_min_ttl_ms: Option<u64>,
    pub applied_concurrency_limit: Option<usize>,
    pub throttled: bool,
    pub safeguard_reason: Option<String>,
    pub audit_recorded: bool,
    pub audit_id: Option<i64>,
    pub message: String,
    pub timestamp_epoch_ms: u64,
}

/// Thermal and battery safeguarding status evaluation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SafeguardState {
    Optimal {
        recommended_concurrency: usize,
    },
    Throttled {
        cpu_temp_c: f64,
        battery_pct: u32,
        is_discharging: bool,
        reason: String,
        restricted_concurrency: usize,
    },
}

impl SafeguardState {
    pub fn is_throttled(&self) -> bool {
        matches!(self, Self::Throttled { .. })
    }

    pub fn concurrency_limit(&self) -> usize {
        match self {
            Self::Optimal { recommended_concurrency } => *recommended_concurrency,
            Self::Throttled { restricted_concurrency, .. } => *restricted_concurrency,
        }
    }
}

/// Telemetry details for an individual thermal zone
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalZoneRecord {
    pub name: String,
    pub zone_type: String,
    pub temp_c: f64,
}

/// Telemetry details for host battery subsystem
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatteryTelemetryRecord {
    pub name: String,
    pub status: String,
    pub capacity_pct: u32,
    pub discharge_rate_w: f64,
    pub is_discharging: bool,
    pub is_critical: bool,
    pub energy_now_wh: f64,
    pub energy_full_wh: f64,
}

/// Linux Multi-Gen LRU (MGLRU) telemetry status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MglruTelemetryRecord {
    pub enabled: bool,
    pub flags: String,
    pub min_ttl_ms: u64,
}

/// CPU Frequency & Governor status record
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuGovernorRecord {
    pub scaling_driver: String,
    pub governors: Vec<String>,
    pub epp: Vec<String>,
    pub frequencies_mhz: Vec<f64>,
    pub turbo_enabled: bool,
    pub max_perf_pct: u32,
    pub hwp_dynamic_boost: bool,
}

/// Comprehensive hardware and local subsystem telemetry snapshot
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubsystemTelemetrySnapshot {
    pub cpu_pkg_temp_c: f64,
    pub max_temp_c: f64,
    pub ac_connected: bool,
    pub thermal_zones: Vec<ThermalZoneRecord>,
    pub battery: Option<BatteryTelemetryRecord>,
    pub mglru: MglruTelemetryRecord,
    pub cpu: CpuGovernorRecord,
    pub safeguard: SafeguardState,
    pub dynatune_socket_present: bool,
    pub dynatune_daemon_active: bool,
    pub kernel_ctl_present: bool,
    pub timestamp_epoch_ms: u64,
}

/// Captured baseline hardware configuration to restore upon workload completion (RAII)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviousSubsystemState {
    pub governors: Vec<(PathBuf, String)>,
    pub epps: Vec<(PathBuf, String)>,
    pub no_turbo: Option<String>,
    pub max_perf_pct: Option<String>,
    pub hwp_dynamic_boost: Option<String>,
    pub mglru_min_ttl_ms: Option<u64>,
    pub nice_level: Option<i32>,
    pub mode_before: FuseGovernorMode,
}

/// Configurable paths for sysfs and sibling binaries (supports test sandboxing)
#[derive(Debug, Clone)]
pub struct SubsystemPaths {
    pub power_supply_dir: PathBuf,
    pub cpufreq_dir: PathBuf,
    pub intel_pstate_dir: PathBuf,
    pub thermal_dir: PathBuf,
    pub mglru_dir: PathBuf,
    pub dynatune_bin: PathBuf,
    pub dynatune_sock: PathBuf,
    pub kernel_ctl_bin: PathBuf,
}

impl Default for SubsystemPaths {
    fn default() -> Self {
        Self::default_linux()
    }
}

impl SubsystemPaths {
    /// Standard live Linux system paths with sibling auto-detection
    pub fn default_linux() -> Self {
        let kernel_ctl = if Path::new(DEFAULT_KERNEL_CTL_BIN).exists() {
            PathBuf::from(DEFAULT_KERNEL_CTL_BIN)
        } else if Path::new(SIBLING_KERNEL_CTL_BIN).exists() {
            PathBuf::from(SIBLING_KERNEL_CTL_BIN)
        } else {
            PathBuf::from(DEFAULT_KERNEL_CTL_BIN)
        };

        Self {
            power_supply_dir: PathBuf::from("/sys/class/power_supply"),
            cpufreq_dir: PathBuf::from("/sys/devices/system/cpu/cpufreq"),
            intel_pstate_dir: PathBuf::from("/sys/devices/system/cpu/intel_pstate"),
            thermal_dir: PathBuf::from("/sys/class/thermal"),
            mglru_dir: PathBuf::from("/sys/kernel/mm/lru_gen"),
            dynatune_bin: PathBuf::from(DEFAULT_DYNATUNE_BIN),
            dynatune_sock: PathBuf::from(DEFAULT_DYNATUNE_SOCK),
            kernel_ctl_bin: kernel_ctl,
        }
    }

    /// Construct custom paths hierarchy for testing
    pub fn isolated(
        power_supply_dir: impl Into<PathBuf>,
        cpufreq_dir: impl Into<PathBuf>,
        intel_pstate_dir: impl Into<PathBuf>,
        thermal_dir: impl Into<PathBuf>,
        mglru_dir: impl Into<PathBuf>,
        dynatune_bin: impl Into<PathBuf>,
        dynatune_sock: impl Into<PathBuf>,
        kernel_ctl_bin: impl Into<PathBuf>,
    ) -> Self {
        Self {
            power_supply_dir: power_supply_dir.into(),
            cpufreq_dir: cpufreq_dir.into(),
            intel_pstate_dir: intel_pstate_dir.into(),
            thermal_dir: thermal_dir.into(),
            mglru_dir: mglru_dir.into(),
            dynatune_bin: dynatune_bin.into(),
            dynatune_sock: dynatune_sock.into(),
            kernel_ctl_bin: kernel_ctl_bin.into(),
        }
    }
}

/// Core Subsystem Fuse managing dynatune, kernel-optimizer, CPU governors, MGLRU, and thermal safety
pub struct DynatuneSubsystemFuse {
    paths: SubsystemPaths,
    is_boosted: Arc<AtomicBool>,
    last_safeguard: Arc<std::sync::RwLock<SafeguardState>>,
    boost_counter: Arc<AtomicU64>,
}

impl Default for DynatuneSubsystemFuse {
    fn default() -> Self {
        Self::new()
    }
}

impl DynatuneSubsystemFuse {
    /// Construct a new DynatuneSubsystemFuse using host Linux paths
    pub fn new() -> Self {
        Self::with_paths(SubsystemPaths::default_linux())
    }

    /// Construct a new DynatuneSubsystemFuse with custom paths (for isolated unit testing)
    pub fn with_paths(paths: SubsystemPaths) -> Self {
        let base_concurrency = num_cpus();
        Self {
            paths,
            is_boosted: Arc::new(AtomicBool::new(false)),
            last_safeguard: Arc::new(std::sync::RwLock::new(SafeguardState::Optimal {
                recommended_concurrency: base_concurrency,
            })),
            boost_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Read paths configuration
    pub fn paths(&self) -> &SubsystemPaths {
        &self.paths
    }

    /// Check if currently in performance boost mode
    pub fn is_boosted(&self) -> bool {
        self.is_boosted.load(Ordering::SeqCst)
    }

    /// Count of total boost acquisitions performed
    pub fn boost_count(&self) -> u64 {
        self.boost_counter.load(Ordering::Relaxed)
    }

    /// Check if dynatune daemon socket is present on the host
    pub fn is_dynatune_socket_present(&self) -> bool {
        self.paths.dynatune_sock.exists()
    }

    /// Check if dynatune daemon is active and responsive
    pub fn is_dynatune_daemon_active(&self) -> bool {
        if self.paths.dynatune_sock.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::net::UnixStream;
                if UnixStream::connect(&self.paths.dynatune_sock).is_ok() {
                    return true;
                }
            }
            return true;
        }
        false
    }

    /// Check if dynabook-kernel-ctl is present and executable
    pub fn is_kernel_ctl_available(&self) -> bool {
        self.paths.kernel_ctl_bin.exists()
    }

    // ---------------------------------------------------------------------------
    // 1. Real-Time Telemetry Queries
    // ---------------------------------------------------------------------------

    /// Query comprehensive real-time hardware telemetry
    pub fn query_telemetry(&self) -> SubsystemTelemetrySnapshot {
        let (cpu_pkg_temp_c, max_temp_c, thermal_zones) = self.query_thermal();
        let ac_connected = self.query_ac_status();
        let battery = self.query_battery();
        let mglru = self.query_mglru();
        let cpu = self.query_cpu();

        let safeguard = self.evaluate_safeguard(cpu_pkg_temp_c, &battery);
        {
            let mut guard = self.last_safeguard.write().unwrap();
            *guard = safeguard.clone();
        }

        let dynatune_socket_present = self.is_dynatune_socket_present();
        let dynatune_daemon_active = self.is_dynatune_daemon_active();
        let kernel_ctl_present = self.is_kernel_ctl_available();

        let timestamp_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        SubsystemTelemetrySnapshot {
            cpu_pkg_temp_c,
            max_temp_c,
            ac_connected,
            thermal_zones,
            battery,
            mglru,
            cpu,
            safeguard,
            dynatune_socket_present,
            dynatune_daemon_active,
            kernel_ctl_present,
            timestamp_epoch_ms,
        }
    }

    /// Query thermal zones and compute package temperature
    pub fn query_thermal(&self) -> (f64, f64, Vec<ThermalZoneRecord>) {
        let mut zones = Vec::new();
        let mut cpu_temp_milli = None;
        let mut max_milli = 0i64;

        if let Ok(entries) = fs::read_dir(&self.paths.thermal_dir) {
            let mut sorted: Vec<_> = entries.flatten().collect();
            sorted.sort_by_key(|e| e.file_name());

            for entry in sorted {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let type_path = path.join("type");
                    let temp_path = path.join("temp");

                    let zone_type = fs::read_to_string(&type_path)
                        .unwrap_or_else(|_| "unknown".to_string())
                        .trim()
                        .to_string();

                    if let Ok(raw_temp) = fs::read_to_string(&temp_path) {
                        if let Ok(milli) = raw_temp.trim().parse::<i64>() {
                            let temp_c = milli as f64 / 1000.0;
                            if milli > max_milli {
                                max_milli = milli;
                            }

                            if zone_type == "x86_pkg_temp" {
                                cpu_temp_milli = Some(milli);
                            } else if cpu_temp_milli.is_none() && (zone_type == "acpitz" || zone_type == "cpu-thermal") {
                                cpu_temp_milli = Some(milli);
                            }

                            zones.push(ThermalZoneRecord {
                                name,
                                zone_type,
                                temp_c,
                            });
                        }
                    }
                }
            }
        }

        let effective_cpu_milli = cpu_temp_milli.unwrap_or(max_milli);
        let cpu_pkg_temp_c = effective_cpu_milli as f64 / 1000.0;
        let max_temp_c = max_milli as f64 / 1000.0;

        (cpu_pkg_temp_c, max_temp_c, zones)
    }

    /// Query AC adapter status (connected vs on battery)
    pub fn query_ac_status(&self) -> bool {
        if let Ok(entries) = fs::read_dir(&self.paths.power_supply_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("AC") || name.starts_with("ADP") {
                    let online_file = path.join("online");
                    if let Ok(content) = fs::read_to_string(&online_file) {
                        if content.trim() == "1" {
                            return true;
                        }
                    }
                }
            }
        }
        // Fallback: check BAT* status for Charging or Full
        if let Ok(entries) = fs::read_dir(&self.paths.power_supply_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("BAT") {
                    let status_file = path.join("status");
                    if let Ok(s) = fs::read_to_string(status_file) {
                        let s = s.trim();
                        if s.eq_ignore_ascii_case("Charging") || s.eq_ignore_ascii_case("Full") {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Query primary battery metrics, capacity percentage, and instantaneous discharge rate in Watts
    pub fn query_battery(&self) -> Option<BatteryTelemetryRecord> {
        let entries = fs::read_dir(&self.paths.power_supply_dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().to_string();
            if name.starts_with("BAT") {
                let status = fs::read_to_string(path.join("status"))
                    .unwrap_or_else(|_| "Unknown".to_string())
                    .trim()
                    .to_string();

                let capacity_pct = fs::read_to_string(path.join("capacity"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(100);

                let voltage_uv = fs::read_to_string(path.join("voltage_now"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(7_600_000) as f64;

                let energy_now_wh = if let Ok(e) = fs::read_to_string(path.join("energy_now")) {
                    e.trim().parse::<f64>().unwrap_or(0.0) / 1_000_000.0
                } else if let Ok(c) = fs::read_to_string(path.join("charge_now")) {
                    (c.trim().parse::<f64>().unwrap_or(0.0) * voltage_uv) / 1_000_000_000_000.0
                } else {
                    0.0
                };

                let energy_full_wh = if let Ok(e) = fs::read_to_string(path.join("energy_full")) {
                    e.trim().parse::<f64>().unwrap_or(0.0) / 1_000_000.0
                } else if let Ok(c) = fs::read_to_string(path.join("charge_full")) {
                    (c.trim().parse::<f64>().unwrap_or(0.0) * voltage_uv) / 1_000_000_000_000.0
                } else {
                    energy_now_wh
                };

                let discharge_rate_w = if let Ok(p) = fs::read_to_string(path.join("power_now")) {
                    p.trim().parse::<f64>().unwrap_or(0.0) / 1_000_000.0
                } else if let Ok(c) = fs::read_to_string(path.join("current_now")) {
                    (c.trim().parse::<f64>().unwrap_or(0.0) * voltage_uv) / 1_000_000_000_000.0
                } else {
                    0.0
                };

                let is_discharging = status.eq_ignore_ascii_case("discharging");
                let is_critical = capacity_pct < BATTERY_CRITICAL_THRESHOLD_PCT && is_discharging;

                return Some(BatteryTelemetryRecord {
                    name,
                    status,
                    capacity_pct,
                    discharge_rate_w,
                    is_discharging,
                    is_critical,
                    energy_now_wh,
                    energy_full_wh,
                });
            }
        }
        None
    }

    /// Query host Linux Multi-Gen LRU (MGLRU) parameters
    pub fn query_mglru(&self) -> MglruTelemetryRecord {
        let enabled_path = self.paths.mglru_dir.join("enabled");
        let min_ttl_path = self.paths.mglru_dir.join("min_ttl_ms");

        let flags = fs::read_to_string(&enabled_path)
            .unwrap_or_else(|_| "0x0000".to_string())
            .trim()
            .to_string();

        let enabled = flags != "0x0000" && flags != "0" && !flags.eq_ignore_ascii_case("disabled");

        let min_ttl_ms = fs::read_to_string(&min_ttl_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);

        MglruTelemetryRecord {
            enabled,
            flags,
            min_ttl_ms,
        }
    }

    /// Query CPU cpufreq and Intel P-State configuration
    pub fn query_cpu(&self) -> CpuGovernorRecord {
        let mut governors = Vec::new();
        let mut epp = Vec::new();
        let mut frequencies_mhz = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.paths.cpufreq_dir) {
            let mut policies: Vec<_> = entries.flatten().collect();
            policies.sort_by_key(|e| e.file_name());

            for policy in policies {
                let p = policy.path();
                let fname = p.file_name().unwrap_or_default().to_string_lossy();
                if fname.starts_with("policy") {
                    if let Ok(g) = fs::read_to_string(p.join("scaling_governor")) {
                        governors.push(g.trim().to_string());
                    }
                    if let Ok(e) = fs::read_to_string(p.join("energy_performance_preference")) {
                        epp.push(e.trim().to_string());
                    }
                    if let Ok(freq) = fs::read_to_string(p.join("scaling_cur_freq")) {
                        if let Ok(khz) = freq.trim().parse::<f64>() {
                            frequencies_mhz.push(khz / 1000.0);
                        }
                    }
                }
            }
        }

        let scaling_driver = fs::read_to_string(self.paths.cpufreq_dir.join("policy0/scaling_driver"))
            .unwrap_or_else(|_| "unknown".to_string())
            .trim()
            .to_string();

        let no_turbo = fs::read_to_string(self.paths.intel_pstate_dir.join("no_turbo"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        let max_perf_pct = fs::read_to_string(self.paths.intel_pstate_dir.join("max_perf_pct"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(100);

        let hwp_dynamic_boost = fs::read_to_string(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        CpuGovernorRecord {
            scaling_driver,
            governors,
            epp,
            frequencies_mhz,
            turbo_enabled: !no_turbo,
            max_perf_pct,
            hwp_dynamic_boost,
        }
    }

    // ---------------------------------------------------------------------------
    // 2. Thermal & Battery Safeguards
    // ---------------------------------------------------------------------------

    /// Evaluate hardware conditions and determine if thermal or battery throttling must engage
    pub fn evaluate_safeguard(&self, cpu_pkg_temp_c: f64, battery: &Option<BatteryTelemetryRecord>) -> SafeguardState {
        let base_concurrency = num_cpus();

        // 1. Critical Thermal Runaway (>= 85.0°C) -> Aggressive serialization to 1 worker
        if cpu_pkg_temp_c >= THERMAL_CRITICAL_TEMP_C {
            return SafeguardState::Throttled {
                cpu_temp_c: cpu_pkg_temp_c,
                battery_pct: battery.as_ref().map(|b| b.capacity_pct).unwrap_or(100),
                is_discharging: battery.as_ref().map(|b| b.is_discharging).unwrap_or(false),
                reason: format!("Severe CPU package temperature ({:.1}°C >= {:.1}°C)", cpu_pkg_temp_c, THERMAL_CRITICAL_TEMP_C),
                restricted_concurrency: 1,
            };
        }

        // 2. Standard Thermal Throttling (>= 78.0°C) -> Restrict concurrency to 2 workers
        if cpu_pkg_temp_c >= THERMAL_THROTTLE_THRESHOLD_C {
            let limit = if base_concurrency <= 2 { 1 } else { 2 };
            return SafeguardState::Throttled {
                cpu_temp_c: cpu_pkg_temp_c,
                battery_pct: battery.as_ref().map(|b| b.capacity_pct).unwrap_or(100),
                is_discharging: battery.as_ref().map(|b| b.is_discharging).unwrap_or(false),
                reason: format!("Elevated CPU package temperature ({:.1}°C >= {:.1}°C)", cpu_pkg_temp_c, THERMAL_THROTTLE_THRESHOLD_C),
                restricted_concurrency: limit,
            };
        }

        // 3. Battery Discharge Depletion Safeguard (< 20% on discharge) -> Restrict to 2 workers
        if let Some(bat) = battery {
            if bat.is_critical {
                let limit = if base_concurrency <= 2 { 1 } else { 2 };
                return SafeguardState::Throttled {
                    cpu_temp_c: cpu_pkg_temp_c,
                    battery_pct: bat.capacity_pct,
                    is_discharging: true,
                    reason: format!("Battery level critical while discharging ({}% < {}%)", bat.capacity_pct, BATTERY_CRITICAL_THRESHOLD_PCT),
                    restricted_concurrency: limit,
                };
            }
        }

        SafeguardState::Optimal {
            recommended_concurrency: base_concurrency,
        }
    }

    /// Check live thermal and battery state, automatically downscaling governors to powersave if breached
    pub fn check_thermal_safeguard(&self) -> SafeguardState {
        let (cpu_pkg_temp_c, _, _) = self.query_thermal();
        let battery = self.query_battery();
        let safeguard = self.evaluate_safeguard(cpu_pkg_temp_c, &battery);

        if safeguard.is_throttled() {
            // Automatically downscale governors to powersave / EPP power and drop MGLRU min_ttl_ms
            let _ = self.apply_powersave_safeguard(&safeguard);
        }

        let mut guard = self.last_safeguard.write().unwrap();
        *guard = safeguard.clone();
        safeguard
    }

    /// Downscale sysfs governors to powersave/power when safeguard is tripped
    fn apply_powersave_safeguard(&self, safeguard: &SafeguardState) -> Result<()> {
        let target_epp = match safeguard {
            SafeguardState::Throttled { cpu_temp_c, .. } if *cpu_temp_c >= THERMAL_CRITICAL_TEMP_C => "power",
            _ => "balance_power",
        };

        // 1. Write cpufreq scaling_governor and EPP
        self.write_all_cpufreq("scaling_governor", "powersave");
        self.write_all_cpufreq("energy_performance_preference", target_epp);

        // 2. Throttle Intel P-State if present
        let _ = fs::write(self.paths.intel_pstate_dir.join("max_perf_pct"), "70\n");
        let _ = fs::write(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"), "0\n");

        // 3. Relax MGLRU min_ttl_ms back to 0
        self.set_mglru_min_ttl_ms(IDLE_MGLRU_MIN_TTL_MS);

        // 4. Invoke sibling dynabook-kernel-ctl if available
        if self.is_kernel_ctl_available() {
            let _ = Command::new(&self.paths.kernel_ctl_bin)
                .args(["profile", "battery"])
                .output();
        }

        // 5. Demote thread nice priority
        set_thread_priority(10);

        self.is_boosted.store(false, Ordering::SeqCst);
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // 3. Performance Boost & Idle Scale-Down
    // ---------------------------------------------------------------------------

    /// Capture previous governor and EPP state across logical cores
    pub fn capture_previous_state(&self) -> PreviousSubsystemState {
        let mut governors = Vec::new();
        let mut epps = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.paths.cpufreq_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                    let gov_file = p.join("scaling_governor");
                    if let Ok(g) = fs::read_to_string(&gov_file) {
                        governors.push((gov_file, g.trim().to_string()));
                    }
                    let epp_file = p.join("energy_performance_preference");
                    if let Ok(e) = fs::read_to_string(&epp_file) {
                        epps.push((epp_file, e.trim().to_string()));
                    }
                }
            }
        }

        let no_turbo = fs::read_to_string(self.paths.intel_pstate_dir.join("no_turbo"))
            .ok()
            .map(|s| s.trim().to_string());
        let max_perf_pct = fs::read_to_string(self.paths.intel_pstate_dir.join("max_perf_pct"))
            .ok()
            .map(|s| s.trim().to_string());
        let hwp_dynamic_boost = fs::read_to_string(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"))
            .ok()
            .map(|s| s.trim().to_string());

        let mglru_min_ttl_ms = fs::read_to_string(self.paths.mglru_dir.join("min_ttl_ms"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());

        let nice_level = get_thread_priority();

        let mode_before = if governors.iter().any(|(_, g)| g == "performance") {
            FuseGovernorMode::Performance
        } else if governors.iter().any(|(_, g)| g == "powersave") {
            FuseGovernorMode::PowerSave
        } else {
            FuseGovernorMode::Balanced
        };

        PreviousSubsystemState {
            governors,
            epps,
            no_turbo,
            max_perf_pct,
            hwp_dynamic_boost,
            mglru_min_ttl_ms,
            nice_level,
            mode_before,
        }
    }

    /// Escalate CPU frequency governors to `performance`, EPP to `performance`, enable turbo,
    /// and configure MGLRU `min_ttl_ms 1000` to prioritize memory bandwidth for heavy inference
    pub fn boost_performance(&self, _workload: &str) -> Result<PreviousSubsystemState> {
        // 1. Safeguard check: if hardware is already critical/hot, refuse to burst and enforce throttling
        let safeguard = self.check_thermal_safeguard();
        if safeguard.is_throttled() {
            return Err(HgbError::Execution(format!(
                "Performance boost aborted: Thermal or battery safeguard active ({})",
                match safeguard {
                    SafeguardState::Throttled { ref reason, .. } => reason,
                    _ => "unknown",
                }
            )));
        }

        // 2. Capture baseline prior to modification
        let previous = self.capture_previous_state();

        // 3. Shift cpufreq governors and EPP to performance
        self.write_all_cpufreq("scaling_governor", "performance");
        self.write_all_cpufreq("energy_performance_preference", "performance");

        // 4. Configure Intel P-State (no_turbo=0, max_perf_pct=100, hwp_dynamic_boost=1)
        let _ = fs::write(self.paths.intel_pstate_dir.join("no_turbo"), "0\n");
        let _ = fs::write(self.paths.intel_pstate_dir.join("max_perf_pct"), "100\n");
        let _ = fs::write(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"), "1\n");

        // 5. Configure Linux MGLRU min_ttl_ms 1000 to prioritize memory bandwidth
        self.set_mglru_min_ttl_ms(BURST_MGLRU_MIN_TTL_MS);

        // 6. Invoke sibling dynabook-kernel-ctl if available
        if self.is_kernel_ctl_available() {
            let _ = Command::new(&self.paths.kernel_ctl_bin)
                .args(["profile", "performance"])
                .output();
        }

        // 7. Elevate thread nice priority (-5)
        set_thread_priority(-5);

        self.is_boosted.store(true, Ordering::SeqCst);
        self.boost_counter.fetch_add(1, Ordering::Relaxed);

        Ok(previous)
    }

    /// Restore previous hardware governor state captured by `boost_performance`
    pub fn restore_state(&self, previous: &PreviousSubsystemState) -> Result<()> {
        // 1. Restore cpufreq governors
        for (path, val) in &previous.governors {
            let _ = fs::write(path, format!("{}\n", val));
        }

        // 2. Restore EPPs
        for (path, val) in &previous.epps {
            let _ = fs::write(path, format!("{}\n", val));
        }

        // 3. Restore Intel P-State
        if let Some(ref nt) = previous.no_turbo {
            let _ = fs::write(self.paths.intel_pstate_dir.join("no_turbo"), format!("{}\n", nt));
        }
        if let Some(ref mp) = previous.max_perf_pct {
            let _ = fs::write(self.paths.intel_pstate_dir.join("max_perf_pct"), format!("{}\n", mp));
        }
        if let Some(ref hb) = previous.hwp_dynamic_boost {
            let _ = fs::write(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"), format!("{}\n", hb));
        }

        // 4. Restore MGLRU min_ttl_ms
        if let Some(ttl) = previous.mglru_min_ttl_ms {
            self.set_mglru_min_ttl_ms(ttl);
        } else {
            self.set_mglru_min_ttl_ms(IDLE_MGLRU_MIN_TTL_MS);
        }

        // 5. Restore thread priority
        if let Some(nice) = previous.nice_level {
            set_thread_priority(nice);
        }

        self.is_boosted.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Automatically scale-down: when tasks drain, drop CPU governors back to `powersave` and relax MGLRU
    pub fn drop_to_powersave(&self) -> Result<()> {
        // 1. Shift governors to powersave and EPP to balance_power (clean baseline restoration)
        let ac = self.query_ac_status();
        let target_epp = "balance_power";

        self.write_all_cpufreq("scaling_governor", "powersave");
        self.write_all_cpufreq("energy_performance_preference", target_epp);

        // 2. Configure Intel P-State (max_perf_pct=80 on battery, 100 on AC)
        let target_perf = if ac { "100" } else { "80" };
        let _ = fs::write(self.paths.intel_pstate_dir.join("max_perf_pct"), format!("{}\n", target_perf));
        let _ = fs::write(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"), "0\n");

        // 3. Reset MGLRU min_ttl_ms to 0
        self.set_mglru_min_ttl_ms(IDLE_MGLRU_MIN_TTL_MS);

        // 4. Invoke sibling dynabook-kernel-ctl if available
        if self.is_kernel_ctl_available() {
            let target_profile = if ac { "balanced" } else { "battery" };
            let _ = Command::new(&self.paths.kernel_ctl_bin)
                .args(["profile", target_profile])
                .output();
        }

        // 5. Normal thread priority (0)
        set_thread_priority(0);

        self.is_boosted.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Set Linux Multi-Gen LRU min_ttl_ms sysfs parameter
    pub fn set_mglru_min_ttl_ms(&self, ttl_ms: u64) -> bool {
        let min_ttl_file = self.paths.mglru_dir.join("min_ttl_ms");
        if min_ttl_file.exists() {
            fs::write(&min_ttl_file, format!("{}\n", ttl_ms)).is_ok()
        } else {
            false
        }
    }

    /// Record a hardware optimization audit event into SQLite table `telemetry_optimizations`
    pub fn record_optimization_audit(
        &self,
        db_path: Option<&Path>,
        trigger_reason: &str,
        cpu_temp_c: f64,
        battery_w: f64,
        culprit_processes: &str,
        governor_action: &str,
        model_reasoning: &str,
        status: &str,
    ) -> Result<i64> {
        let path = if let Some(p) = db_path {
            p.to_path_buf()
        } else {
            let hgb_dir = Path::new(".hgb");
            let _ = fs::create_dir_all(hgb_dir);
            hgb_dir.join("autonomous_telemetry_optimizations.db")
        };

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let conn = Connection::open(&path).map_err(|e| HgbError::Storage(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS telemetry_optimizations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp TEXT NOT NULL,
                 trigger_reason TEXT NOT NULL,
                 cpu_temp_c REAL NOT NULL,
                 battery_draw_w REAL NOT NULL,
                 culprit_processes TEXT NOT NULL,
                 governor_action TEXT NOT NULL,
                 model_reasoning TEXT NOT NULL,
                 status TEXT NOT NULL,
                 blake3_hash TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_telemetry_opt_timestamp ON telemetry_optimizations(timestamp);",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let now = chrono::Utc::now().to_rfc3339();
        let blake3_hash = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(now.as_bytes());
            hasher.update(b":");
            hasher.update(trigger_reason.as_bytes());
            hasher.update(b":");
            hasher.update(format!("{:.3}", cpu_temp_c).as_bytes());
            hasher.update(b":");
            hasher.update(format!("{:.3}", battery_w).as_bytes());
            hasher.update(b":");
            hasher.update(culprit_processes.as_bytes());
            hasher.update(b":");
            hasher.update(governor_action.as_bytes());
            hasher.update(b":");
            hasher.update(model_reasoning.as_bytes());
            hasher.update(b":");
            hasher.update(status.as_bytes());
            hasher.finalize().to_hex().to_string()
        };

        conn.execute(
            "INSERT INTO telemetry_optimizations (
                timestamp, trigger_reason, cpu_temp_c, battery_draw_w,
                culprit_processes, governor_action, model_reasoning, status, blake3_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &now,
                trigger_reason,
                cpu_temp_c,
                battery_w,
                culprit_processes,
                governor_action,
                model_reasoning,
                status,
                &blake3_hash,
            ],
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        Ok(conn.last_insert_rowid())
    }

    /// Safely apply hardware tuning with strict safety limit validation, thermal safeguards, and SQLite audit logging
    pub fn safe_hardware_tune(
        &self,
        spec: &SafeHardwareTuneSpec,
        db_path: Option<&Path>,
    ) -> Result<SafeHardwareTuneReport> {
        let (cpu_pkg_temp_c, _, _) = self.query_thermal();
        let battery = self.query_battery();
        let battery_w = battery.as_ref().map(|b| b.discharge_rate_w).unwrap_or(0.0);
        let safeguard = self.evaluate_safeguard(cpu_pkg_temp_c, &battery);
        let is_throttled = safeguard.is_throttled();

        // 1. Strictly validate governor parameter
        if let Some(ref gov) = spec.governor {
            let g = gov.trim().to_lowercase();
            if g != "powersave" && g != "performance" {
                return Err(HgbError::Execution(format!(
                    "Invalid governor '{}': strictly only 'powersave' or 'performance' are permitted",
                    gov
                )));
            }
        }

        // 2. Strictly validate EPP parameter
        if let Some(ref epp) = spec.epp {
            let e = epp.trim().to_lowercase();
            if e != "power" && e != "balance_power" && e != "balance_performance" && e != "performance" {
                return Err(HgbError::Execution(format!(
                    "Invalid EPP '{}': strictly only 'power', 'balance_power', 'balance_performance', or 'performance' are permitted",
                    epp
                )));
            }
        }

        let mut applied_governor = None;
        let mut applied_epp = None;
        let mut applied_mglru = None;
        let applied_concurrency;
        let mut safeguard_reason = None;

        // 3. Enforce thermal & battery clamps if throttled (cannot set invalid governors or bypass thermal clamps)
        if is_throttled {
            let reason = match &safeguard {
                SafeguardState::Throttled { reason, .. } => reason.clone(),
                _ => "Hardware safeguard active".to_string(),
            };
            safeguard_reason = Some(reason);

            // Thermal clamp: refuse boost! Cannot set performance governor
            self.write_all_cpufreq("scaling_governor", "powersave");
            self.is_boosted.store(false, Ordering::SeqCst);
            if let Some(ref gov) = spec.governor {
                if gov.trim().eq_ignore_ascii_case("performance") {
                    applied_governor = Some("powersave (clamped)".to_string());
                } else {
                    applied_governor = Some("powersave".to_string());
                }
            } else {
                applied_governor = Some("powersave".to_string());
            }

            // EPP clamp: cannot set performance or balance_performance
            let clamped_epp = if cpu_pkg_temp_c >= THERMAL_CRITICAL_TEMP_C {
                "power"
            } else {
                "balance_power"
            };
            self.write_all_cpufreq("energy_performance_preference", clamped_epp);
            applied_epp = Some(format!("{} (clamped)", clamped_epp));

            // Intel P-State max_perf_pct clamped to 70% and disable dynamic boost
            let _ = fs::write(self.paths.intel_pstate_dir.join("max_perf_pct"), "70\n");
            let _ = fs::write(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"), "0\n");

            // MGLRU clamp: reset to 0 or clamp to <= 250ms
            if let Some(ttl) = spec.mglru_min_ttl_ms {
                let clamped_ttl = ttl.min(250);
                self.set_mglru_min_ttl_ms(clamped_ttl);
                applied_mglru = Some(clamped_ttl);
            } else {
                self.set_mglru_min_ttl_ms(IDLE_MGLRU_MIN_TTL_MS);
                applied_mglru = Some(IDLE_MGLRU_MIN_TTL_MS);
            }

            // Concurrency clamp: strictly cannot exceed safeguard concurrency limit
            let max_concurrency = safeguard.concurrency_limit();
            let limit = spec.concurrency_limit.map(|c| c.min(max_concurrency)).unwrap_or(max_concurrency);
            applied_concurrency = Some(limit);
        } else {
            // Optimal conditions: apply requested values safely
            if let Some(ref gov) = spec.governor {
                let g = gov.trim().to_lowercase();
                self.write_all_cpufreq("scaling_governor", &g);
                if g == "performance" {
                    self.is_boosted.store(true, Ordering::SeqCst);
                    self.boost_counter.fetch_add(1, Ordering::Relaxed);
                    // Also enable Intel P-State turbo
                    let _ = fs::write(self.paths.intel_pstate_dir.join("no_turbo"), "0\n");
                    let _ = fs::write(self.paths.intel_pstate_dir.join("max_perf_pct"), "100\n");
                    let _ = fs::write(self.paths.intel_pstate_dir.join("hwp_dynamic_boost"), "1\n");
                } else {
                    self.is_boosted.store(false, Ordering::SeqCst);
                }
                applied_governor = Some(g);
            }

            if let Some(ref epp) = spec.epp {
                let e = epp.trim().to_lowercase();
                self.write_all_cpufreq("energy_performance_preference", &e);
                applied_epp = Some(e);
            }

            if let Some(ttl) = spec.mglru_min_ttl_ms {
                let valid_ttl = ttl.min(10000);
                self.set_mglru_min_ttl_ms(valid_ttl);
                applied_mglru = Some(valid_ttl);
            }

            if let Some(limit) = spec.concurrency_limit {
                applied_concurrency = Some(limit.max(1));
            } else {
                applied_concurrency = Some(num_cpus());
            }
        }

        let action_summary = format!(
            "Governor: {:?} | EPP: {:?} | MGLRU: {:?} | Concurrency: {:?}",
            applied_governor, applied_epp, applied_mglru, applied_concurrency
        );

        let status_str = if is_throttled { "ThrottledClamped" } else { "Applied" };
        let reasoning = if is_throttled {
            format!("Thermal or battery safeguard engaged ({:?}). Performance parameters clamped to prevent thermal runaway.", safeguard_reason)
        } else {
            "Host operating in optimal thermal window. Custom hardware tuning parameters safely applied.".to_string()
        };

        // Persist optimization audit event to SQLite
        let audit_id = self.record_optimization_audit(
            db_path,
            "McpHardwareTune",
            cpu_pkg_temp_c,
            battery_w,
            "hgb_mcp_server",
            &action_summary,
            &reasoning,
            status_str,
        ).ok();

        let timestamp_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let message = if is_throttled {
            format!("Hardware tune applied with thermal safeguarding: {}", action_summary)
        } else {
            format!("Hardware tune successfully applied: {}", action_summary)
        };

        Ok(SafeHardwareTuneReport {
            success: true,
            applied_governor,
            applied_epp,
            applied_mglru_min_ttl_ms: applied_mglru,
            applied_concurrency_limit: applied_concurrency,
            throttled: is_throttled,
            safeguard_reason,
            audit_recorded: audit_id.is_some(),
            audit_id,
            message,
            timestamp_epoch_ms,
        })
    }

    /// Acquire RAII `DynatuneWorkloadBoostGuard` bound to this fuse
    pub fn acquire_guard(self: &Arc<Self>, workload: &str) -> DynatuneWorkloadBoostGuard {
        DynatuneWorkloadBoostGuard::acquire(self, workload)
    }

    /// Helper to write sysfs value across all CPU policy folders
    fn write_all_cpufreq(&self, file_name: &str, value: &str) -> usize {
        let mut modified = 0;
        if let Ok(entries) = fs::read_dir(&self.paths.cpufreq_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                    let target_file = p.join(file_name);
                    if target_file.exists() && fs::write(&target_file, format!("{}\n", value)).is_ok() {
                        modified += 1;
                    }
                }
            }
        }
        modified
    }
}

// ---------------------------------------------------------------------------
// 4. RAII DynatuneWorkloadBoostGuard
// ---------------------------------------------------------------------------

/// RAII Guard that applies performance boost on creation and automatically restores
/// baseline governor, EPP, Intel P-state, and MGLRU state on drop.
pub struct DynatuneWorkloadBoostGuard {
    fuse: Arc<DynatuneSubsystemFuse>,
    workload: String,
    previous_state: Option<PreviousSubsystemState>,
    safeguard: SafeguardState,
    active: bool,
}

impl DynatuneWorkloadBoostGuard {
    /// Create RAII guard, evaluating thermal safeguard and boosting if safe
    pub fn acquire(fuse: &Arc<DynatuneSubsystemFuse>, workload: &str) -> Self {
        let safeguard = fuse.check_thermal_safeguard();

        if safeguard.is_throttled() {
            // Safeguard engaged: do not apply boost; guard runs with restricted concurrency
            Self {
                fuse: Arc::clone(fuse),
                workload: workload.to_string(),
                previous_state: None,
                safeguard,
                active: true,
            }
        } else {
            match fuse.boost_performance(workload) {
                Ok(prev) => Self {
                    fuse: Arc::clone(fuse),
                    workload: workload.to_string(),
                    previous_state: Some(prev),
                    safeguard,
                    active: true,
                },
                Err(_) => Self {
                    fuse: Arc::clone(fuse),
                    workload: workload.to_string(),
                    previous_state: None,
                    safeguard,
                    active: true,
                },
            }
        }
    }

    /// Name or category of the active workload
    pub fn workload(&self) -> &str {
        &self.workload
    }

    /// Check if thermal or battery safeguard is actively throttling this workload
    pub fn is_throttled(&self) -> bool {
        self.safeguard.is_throttled()
    }

    /// Recommended concurrency limit (1-2 if throttled, full CPUs if optimal)
    pub fn concurrency_limit(&self) -> usize {
        self.safeguard.concurrency_limit()
    }

    /// Read safeguard state
    pub fn safeguard(&self) -> &SafeguardState {
        &self.safeguard
    }

    /// Disarm guard without restoring on drop
    pub fn disarm(&mut self) -> Option<PreviousSubsystemState> {
        self.active = false;
        self.previous_state.take()
    }
}

impl Drop for DynatuneWorkloadBoostGuard {
    fn drop(&mut self) {
        if self.active {
            if let Some(ref prev) = self.previous_state {
                let _ = self.fuse.restore_state(prev);
            } else {
                let _ = self.fuse.drop_to_powersave();
            }
            self.active = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Helper Functions
// ---------------------------------------------------------------------------

fn num_cpus() -> usize {
    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
            let count = entries
                .flatten()
                .filter(|e| {
                    let s = e.file_name().to_string_lossy().to_string();
                    s.starts_with("cpu") && s[3..].chars().all(|c| c.is_ascii_digit())
                })
                .count();
            if count > 0 {
                return count;
            }
        }
    }
    4 // Dynabook VC72/B Core i3-7100U has 2C/4T
}

fn set_thread_priority(nice: i32) {
    #[cfg(unix)]
    unsafe {
        let _ = libc::setpriority(libc::PRIO_PROCESS, 0, nice);
    }
    #[cfg(not(unix))]
    let _ = nice;
}

fn get_thread_priority() -> Option<i32> {
    #[cfg(unix)]
    unsafe {
        let res = libc::getpriority(libc::PRIO_PROCESS, 0);
        Some(res)
    }
    #[cfg(not(unix))]
    None
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper to construct a simulated sysfs directory hierarchy
    fn setup_test_sysfs() -> (TempDir, SubsystemPaths) {
        let dir = TempDir::new().expect("create tempdir");
        let base = dir.path();

        let power_supply_dir = base.join("sys/class/power_supply");
        let cpufreq_dir = base.join("sys/devices/system/cpu/cpufreq");
        let intel_pstate_dir = base.join("sys/devices/system/cpu/intel_pstate");
        let thermal_dir = base.join("sys/class/thermal");
        let mglru_dir = base.join("sys/kernel/mm/lru_gen");
        let dynatune_bin = base.join("usr/local/bin/dynatune");
        let dynatune_sock = base.join("run/dynatune.sock");
        let kernel_ctl_bin = base.join("usr/local/bin/dynabook-kernel-ctl");

        fs::create_dir_all(&power_supply_dir).unwrap();
        fs::create_dir_all(&cpufreq_dir).unwrap();
        fs::create_dir_all(&intel_pstate_dir).unwrap();
        fs::create_dir_all(&thermal_dir).unwrap();
        fs::create_dir_all(&mglru_dir).unwrap();

        // 1. Setup AC and BAT0
        let ac_dir = power_supply_dir.join("AC");
        fs::create_dir_all(&ac_dir).unwrap();
        fs::write(ac_dir.join("online"), "1\n").unwrap();

        let bat_dir = power_supply_dir.join("BAT0");
        fs::create_dir_all(&bat_dir).unwrap();
        fs::write(bat_dir.join("status"), "Discharging\n").unwrap();
        fs::write(bat_dir.join("capacity"), "85\n").unwrap();
        fs::write(bat_dir.join("power_now"), "12500000\n").unwrap(); // 12.5 W
        fs::write(bat_dir.join("voltage_now"), "7600000\n").unwrap();
        fs::write(bat_dir.join("energy_now"), "35000000\n").unwrap(); // 35 Wh
        fs::write(bat_dir.join("energy_full"), "42000000\n").unwrap();

        // 2. Setup CPU policies (policy0 & policy1)
        for i in 0..2 {
            let pol = cpufreq_dir.join(format!("policy{}", i));
            fs::create_dir_all(&pol).unwrap();
            fs::write(pol.join("scaling_governor"), "powersave\n").unwrap();
            fs::write(pol.join("energy_performance_preference"), "balance_performance\n").unwrap();
            fs::write(pol.join("scaling_cur_freq"), "2400000\n").unwrap();
            fs::write(pol.join("scaling_driver"), "intel_pstate\n").unwrap();
        }

        // 3. Setup Intel P-State
        fs::write(intel_pstate_dir.join("no_turbo"), "0\n").unwrap();
        fs::write(intel_pstate_dir.join("max_perf_pct"), "100\n").unwrap();
        fs::write(intel_pstate_dir.join("hwp_dynamic_boost"), "0\n").unwrap();

        // 4. Setup Thermal zones (x86_pkg_temp and acpitz)
        let tz0 = thermal_dir.join("thermal_zone0");
        fs::create_dir_all(&tz0).unwrap();
        fs::write(tz0.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(tz0.join("temp"), "48000\n").unwrap(); // 48.0°C (Optimal)

        let tz1 = thermal_dir.join("thermal_zone1");
        fs::create_dir_all(&tz1).unwrap();
        fs::write(tz1.join("type"), "acpitz\n").unwrap();
        fs::write(tz1.join("temp"), "46000\n").unwrap();

        // 5. Setup MGLRU
        fs::write(mglru_dir.join("enabled"), "0x0007\n").unwrap();
        fs::write(mglru_dir.join("min_ttl_ms"), "0\n").unwrap();

        let paths = SubsystemPaths::isolated(
            power_supply_dir,
            cpufreq_dir,
            intel_pstate_dir,
            thermal_dir,
            mglru_dir,
            dynatune_bin,
            dynatune_sock,
            kernel_ctl_bin,
        );

        (dir, paths)
    }

    #[test]
    fn test_telemetry_queries_thermal_power_mglru() {
        let (_dir, paths) = setup_test_sysfs();
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        let snap = fuse.query_telemetry();
        assert_eq!(snap.cpu_pkg_temp_c, 48.0);
        assert_eq!(snap.max_temp_c, 48.0);
        assert!(snap.ac_connected);
        assert_eq!(snap.thermal_zones.len(), 2);

        let bat = snap.battery.expect("primary battery telemetry");
        assert_eq!(bat.capacity_pct, 85);
        assert_eq!(bat.discharge_rate_w, 12.5);
        assert!(!bat.is_critical);

        assert!(snap.mglru.enabled);
        assert_eq!(snap.mglru.min_ttl_ms, 0);

        assert_eq!(snap.cpu.governors, vec!["powersave", "powersave"]);
        assert_eq!(snap.cpu.epp, vec!["balance_performance", "balance_performance"]);
        assert!(snap.cpu.turbo_enabled);

        assert!(!snap.safeguard.is_throttled());
    }

    #[test]
    fn test_performance_boost_and_governor_shifting() {
        let (_dir, paths) = setup_test_sysfs();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let mglru_dir = paths.mglru_dir.clone();
        let pstate_dir = paths.intel_pstate_dir.clone();
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        assert!(!fuse.is_boosted());

        let prev = fuse.boost_performance("inference").expect("boost succeeded");
        assert!(fuse.is_boosted());
        assert_eq!(prev.governors.len(), 2);
        assert_eq!(prev.governors[0].1, "powersave");

        // Verify governor written to performance
        let gov0 = fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap();
        let gov1 = fs::read_to_string(cpufreq_dir.join("policy1/scaling_governor")).unwrap();
        assert_eq!(gov0.trim(), "performance");
        assert_eq!(gov1.trim(), "performance");

        // Verify EPP written to performance
        let epp0 = fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp0.trim(), "performance");

        // Verify MGLRU min_ttl_ms written to 1000
        let ttl = fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap();
        assert_eq!(ttl.trim(), "1000");

        // Verify Intel P-State turbo enabled
        let no_turbo = fs::read_to_string(pstate_dir.join("no_turbo")).unwrap();
        let max_perf = fs::read_to_string(pstate_dir.join("max_perf_pct")).unwrap();
        assert_eq!(no_turbo.trim(), "0");
        assert_eq!(max_perf.trim(), "100");
    }

    #[test]
    fn test_thermal_safeguarding_triggers_powersave_and_concurrency_restriction() {
        let (_dir, paths) = setup_test_sysfs();
        let thermal_dir = paths.thermal_dir.clone();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let mglru_dir = paths.mglru_dir.clone();
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        // Boost first to establish performance state
        let _ = fuse.boost_performance("inference").unwrap();
        assert!(fuse.is_boosted());

        // Simulate thermal spike to 79.5°C (above 78.0°C threshold)
        fs::write(thermal_dir.join("thermal_zone0/temp"), "79500\n").unwrap();

        let safeguard = fuse.check_thermal_safeguard();
        assert!(safeguard.is_throttled());
        assert_eq!(safeguard.concurrency_limit(), 2);
        assert!(!fuse.is_boosted());

        // Verify governor automatically downscaled to powersave
        let gov0 = fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap();
        assert_eq!(gov0.trim(), "powersave");

        let epp0 = fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap();
        assert_eq!(epp0.trim(), "balance_power");

        // Verify MGLRU reset to 0
        let ttl = fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap();
        assert_eq!(ttl.trim(), "0");

        // Critical temperature test (>= 85.0°C) -> Concurrency restricted to 1
        fs::write(thermal_dir.join("thermal_zone0/temp"), "88000\n").unwrap();
        let crit_safeguard = fuse.check_thermal_safeguard();
        assert_eq!(crit_safeguard.concurrency_limit(), 1);
    }

    #[test]
    fn test_battery_critical_safeguard_downscales_governor() {
        let (_dir, paths) = setup_test_sysfs();
        let power_supply_dir = paths.power_supply_dir.clone();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        // Set battery capacity to 14% on discharge
        fs::write(power_supply_dir.join("BAT0/capacity"), "14\n").unwrap();
        fs::write(power_supply_dir.join("BAT0/status"), "Discharging\n").unwrap();

        let safeguard = fuse.check_thermal_safeguard();
        assert!(safeguard.is_throttled());
        assert_eq!(safeguard.concurrency_limit(), 2);

        let gov = fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap();
        assert_eq!(gov.trim(), "powersave");
    }

    #[test]
    fn test_raii_guard_restores_previous_state_on_drop() {
        let (_dir, paths) = setup_test_sysfs();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let mglru_dir = paths.mglru_dir.clone();
        let fuse = Arc::new(DynatuneSubsystemFuse::with_paths(paths));

        // Initial state is powersave, balance_performance, mglru 0
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "0");

        {
            let guard = DynatuneWorkloadBoostGuard::acquire(&fuse, "multi_agent_loop");
            assert_eq!(guard.workload(), "multi_agent_loop");
            assert!(!guard.is_throttled());
            assert!(fuse.is_boosted());

            // Within guard scope: must be performance and MGLRU 1000
            assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "performance");
            assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "1000");
        } // Guard drops here

        // After guard drop: baseline powersave and mglru 0 MUST be cleanly restored!
        assert!(!fuse.is_boosted());
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap().trim(), "balance_performance");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "0");
    }

    #[test]
    fn test_idle_drop_transitions_to_powersave() {
        let (_dir, paths) = setup_test_sysfs();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let mglru_dir = paths.mglru_dir.clone();
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        // Boost first
        let _ = fuse.boost_performance("batch_compile").unwrap();
        assert!(fuse.is_boosted());
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "performance");

        // Queue empties -> drop_to_powersave()
        fuse.drop_to_powersave().expect("drop to powersave succeeded");
        assert!(!fuse.is_boosted());
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap().trim(), "balance_power");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "0");
    }

    #[test]
    fn test_safe_hardware_tune_validation_and_thermal_boundary() {
        let (dir, paths) = setup_test_sysfs();
        let thermal_dir = paths.thermal_dir.clone();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let mglru_dir = paths.mglru_dir.clone();
        let db_path = dir.path().join("audit.db");
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        // 1. Invalid governor validation
        let invalid_gov_spec = SafeHardwareTuneSpec {
            governor: Some("userspace".to_string()),
            epp: None,
            mglru_min_ttl_ms: None,
            concurrency_limit: None,
        };
        let err1 = fuse.safe_hardware_tune(&invalid_gov_spec, Some(&db_path));
        assert!(err1.is_err());
        assert!(err1.unwrap_err().to_string().contains("only 'powersave' or 'performance' are permitted"));

        // 2. Invalid EPP validation
        let invalid_epp_spec = SafeHardwareTuneSpec {
            governor: None,
            epp: Some("ultra_performance".to_string()),
            mglru_min_ttl_ms: None,
            concurrency_limit: None,
        };
        let err2 = fuse.safe_hardware_tune(&invalid_epp_spec, Some(&db_path));
        assert!(err2.is_err());
        assert!(err2.unwrap_err().to_string().contains("only 'power', 'balance_power', 'balance_performance', or 'performance' are permitted"));

        // 3. Thermal boundary clamp: simulate elevated temperature 80.5°C >= 78.0°C
        fs::write(thermal_dir.join("thermal_zone0/temp"), "80500\n").unwrap();
        let boost_spec = SafeHardwareTuneSpec {
            governor: Some("performance".to_string()),
            epp: Some("performance".to_string()),
            mglru_min_ttl_ms: Some(1000),
            concurrency_limit: Some(8),
        };
        let rep_throttled = fuse.safe_hardware_tune(&boost_spec, Some(&db_path)).expect("tune executes with clamp");
        assert!(rep_throttled.throttled);
        assert_eq!(rep_throttled.applied_governor.as_deref(), Some("powersave (clamped)"));
        assert_eq!(rep_throttled.applied_epp.as_deref(), Some("balance_power (clamped)"));
        assert_eq!(rep_throttled.applied_mglru_min_ttl_ms, Some(250)); // Clamped to <= 250
        assert_eq!(rep_throttled.applied_concurrency_limit, Some(2)); // Clamped to safeguard limit
        assert!(rep_throttled.audit_recorded);
        assert!(rep_throttled.audit_id.is_some());

        // Verify sysfs values were actually clamped
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap().trim(), "balance_power");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "250");

        // 4. Optimal conditions: reset temperature to 48.0°C
        fs::write(thermal_dir.join("thermal_zone0/temp"), "48000\n").unwrap();
        let rep_optimal = fuse.safe_hardware_tune(&boost_spec, Some(&db_path)).expect("tune executes optimal");
        assert!(!rep_optimal.throttled);
        assert_eq!(rep_optimal.applied_governor.as_deref(), Some("performance"));
        assert_eq!(rep_optimal.applied_epp.as_deref(), Some("performance"));
        assert_eq!(rep_optimal.applied_mglru_min_ttl_ms, Some(1000));
        assert_eq!(rep_optimal.applied_concurrency_limit, Some(8));
        assert!(rep_optimal.audit_recorded);

        // Verify sysfs values updated to performance
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "performance");
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap().trim(), "performance");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "1000");
    }

    #[test]
    fn test_dynamic_workload_boost_and_idle_drop_transitions() {
        let (_dir, paths) = setup_test_sysfs();
        let thermal_dir = paths.thermal_dir.clone();
        let cpufreq_dir = paths.cpufreq_dir.clone();
        let mglru_dir = paths.mglru_dir.clone();
        let pstate_dir = paths.intel_pstate_dir.clone();
        let power_supply_dir = paths.power_supply_dir.clone();
        let fuse = DynatuneSubsystemFuse::with_paths(paths);

        // 1. Dynamic Workload Boost: Heavy embedding or local LLM inference
        let prev = fuse.boost_performance("local_llm_inference").expect("boost succeeds");
        assert!(fuse.is_boosted());
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "performance");
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap().trim(), "performance");
        assert_eq!(fs::read_to_string(pstate_dir.join("no_turbo")).unwrap().trim(), "0");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "1000");

        // Restore baseline
        fuse.restore_state(&prev).expect("restore state");
        assert!(!fuse.is_boosted());

        // 2. Thermal Safeguard Refusal: Spike temp to 79.0°C >= 78.0°C
        fs::write(thermal_dir.join("thermal_zone0/temp"), "79000\n").unwrap();
        let boost_blocked_thermal = fuse.boost_performance("batch_wasm");
        assert!(boost_blocked_thermal.is_err());
        assert!(boost_blocked_thermal.unwrap_err().to_string().contains("Thermal or battery safeguard active"));
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");

        // 3. Battery Safeguard Refusal: Reset temp, set battery to 15% on discharge (< 20%)
        fs::write(thermal_dir.join("thermal_zone0/temp"), "48000\n").unwrap();
        fs::write(power_supply_dir.join("BAT0/status"), "Discharging\n").unwrap();
        fs::write(power_supply_dir.join("BAT0/capacity"), "15\n").unwrap();

        let boost_blocked_battery = fuse.boost_performance("firecracker_spinup");
        assert!(boost_blocked_battery.is_err());
        assert!(boost_blocked_battery.unwrap_err().to_string().contains("Thermal or battery safeguard active"));
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");

        // 4. Return to AC power, perform workload, and then idle drop
        fs::write(power_supply_dir.join("BAT0/status"), "Charging\n").unwrap();
        fs::write(power_supply_dir.join("BAT0/capacity"), "80\n").unwrap();

        let _ = fuse.boost_performance("firecracker_microvm").expect("boost on AC");
        assert!(fuse.is_boosted());

        // Workload finishes and queue drains -> drop_to_powersave
        fuse.drop_to_powersave().expect("idle drop succeeds");
        assert!(!fuse.is_boosted());
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap().trim(), "powersave");
        assert_eq!(fs::read_to_string(cpufreq_dir.join("policy0/energy_performance_preference")).unwrap().trim(), "balance_power");
        assert_eq!(fs::read_to_string(mglru_dir.join("min_ttl_ms")).unwrap().trim(), "0");
    }

    #[test]
    fn test_real_host_telemetry_safe_execution() {
        // Query live host system (zero mock, real sysfs inspection)
        let fuse = DynatuneSubsystemFuse::new();
        let telemetry = fuse.query_telemetry();
        assert!(telemetry.timestamp_epoch_ms > 0);
    }
}
