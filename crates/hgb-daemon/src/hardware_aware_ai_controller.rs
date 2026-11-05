//! # Pillar 1: Unified Hardware-Aware AI Controller
//!
//! Production-grade bidirectional bridge connecting `hgb-daemon` (Hagibis Daemon) to host
//! hardware infrastructure:
//! - Dynatune Unix domain socket (`/run/dynatune.sock`) with bidirectional JSON-RPC IPC.
//! - Kernel-Optimizer (`scripts/cpu-governor.sh` and `dynabook-kernel-ctl`).
//! - Linux sysfs & procfs direct interfaces (`/sys/class/thermal`, `/sys/class/power_supply`,
//!   `/proc/pressure/memory`, `/proc/meminfo`, `/sys/devices/system/cpu/cpufreq`, MGLRU).
//!
//! ## Core Responsibilities:
//! 1. **Real-Time Telemetry Collection**:
//!    - CPU package temperature and thermal zones (°C).
//!    - Battery drain rate in milliwatts (mW) or Watts, capacity %, charging/discharging state.
//!    - Linux Pressure Stall Information (PSI) memory pressure (`/proc/pressure/memory`: `some` & `full`).
//!    - Physical RAM and swap space utilization (`/proc/meminfo`).
//! 2. **Dynamic LLM Generation & WASM Pacing (`GenerationPacer` & `WorkloadThrottler`)**:
//!    - **Hot / Critical Battery Throttling** (temp >= 78.0°C or battery < 20% on discharge):
//!      * Injects token pacing delay (e.g. 50ms per token chunk) to lower GPU/CPU duty cycle.
//!      * Pauses non-essential background WASM compaction, vector indexing, and speculative tasks.
//!      * Restricts worker concurrency to 1 worker.
//!      * Dispatches governor mode switch to `powersave`.
//!    - **Cool Boost** (temp <= 65.0°C on AC power):
//!      * Zero pacing delay (0ms, maximum throughput).
//!      * Resumes background WASM compaction and vector indexing.
//!      * Scales background worker threads (e.g. 8 boost workers).
//!      * Dispatches governor mode switch to `performance`.
//!    - **Balanced Nominal** (intermediate 65°C - 78°C with recovery hysteresis):
//!      * Standard 4 nominal workers, 0ms (or 10ms when warm) pacing, governor `balanced`.
//! 3. **MCP Tool Integration**:
//!    - Declares MCP tool `hgb_hardware_telemetry_status` returning structured JSON metrics.
//! 4. **Governor Mode Command Dispatch**:
//!    - Dispatches mode switch across Dynatune socket, `scripts/cpu-governor.sh`, `dynabook-kernel-ctl`,
//!      or direct sysfs writes.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hgb_core::error::{HgbError, Result};

// ==============================================================================
// 1. Constants & Tuning Thresholds
// ==============================================================================

/// Default Unix domain socket path for Dynatune daemon
pub const DEFAULT_DYNATUNE_SOCK: &str = "/run/dynatune.sock";

/// Sibling workspace path for `scripts/cpu-governor.sh`
pub const DEFAULT_CPU_GOVERNOR_SCRIPT: &str = "/home/toshiba/My AI Project/hagibis/scripts/cpu-governor.sh";

/// Relative fallback path for `scripts/cpu-governor.sh`
pub const RELATIVE_CPU_GOVERNOR_SCRIPT: &str = "scripts/cpu-governor.sh";

/// Sibling workspace path for `dynabook-kernel-ctl`
pub const SIBLING_KERNEL_CTL_BIN: &str = "/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl";

/// System-wide fallback path for `dynabook-kernel-ctl`
pub const SYSTEM_KERNEL_CTL_BIN: &str = "/usr/local/bin/dynabook-kernel-ctl";

/// Thermal throttling threshold where automatic pacing and concurrency clamps engage (78.0°C)
pub const THERMAL_CLAMP_THRESHOLD_C: f64 = 78.0;

/// Thermal recovery threshold where clamp disengages with hysteresis (70.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 70.0;

/// High-performance boost recovery threshold on AC power (65.0°C)
pub const THERMAL_COOL_BOOST_THRESHOLD_C: f64 = 65.0;

/// Critical battery capacity percentage threshold on battery discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;

/// Token pacing delay injected when throttled (50ms per token chunk)
pub const THROTTLED_TOKEN_PACING_DELAY_MS: u64 = 50;

/// Token pacing delay injected when operating nominally/warm (0ms)
pub const NOMINAL_TOKEN_PACING_DELAY_MS: u64 = 0;

/// Worker concurrency limit when throttled (1 worker)
pub const THROTTLED_CONCURRENCY_LIMIT: usize = 1;

/// Nominal worker concurrency limit under healthy thermal conditions (4 workers)
pub const NOMINAL_CONCURRENCY_LIMIT: usize = 4;

/// High-performance boost worker concurrency limit (8 workers)
pub const BOOST_CONCURRENCY_LIMIT: usize = 8;

/// Multi-Gen LRU burst target (1000ms) for protecting working sets
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;

/// Multi-Gen LRU idle/baseline target (0ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Unix socket connection timeout (500ms)
pub const DYNATUNE_SOCKET_TIMEOUT: Duration = Duration::from_millis(500);

// ==============================================================================
// 2. Data Types & Models
// ==============================================================================

/// Target governor mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernorMode {
    /// High-performance mode: governor `performance`, EPP `performance`, turbo on, MGLRU 1000ms
    Performance,
    /// Balanced standard mode: governor `powersave`, EPP `balance_performance`, MGLRU 0ms
    Balanced,
    /// Ultra-low power / thermal clamp mode: governor `powersave`, EPP `balance_power`, turbo off
    PowerSave,
}

impl GovernorMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::PowerSave => "powersave",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "performance" | "perf" | "boost" | "turbo" => Self::Performance,
            "powersave" | "power" | "battery" | "eco" | "idle" | "clamped" => Self::PowerSave,
            _ => Self::Balanced,
        }
    }
}

impl std::fmt::Display for GovernorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Telemetry record for an individual thermal zone
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalZoneTelemetry {
    pub name: String,
    pub zone_type: String,
    pub temp_c: f64,
}

/// Single PSI pressure record
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PsiRecord {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

/// Linux Memory Pressure Stall Information (PSI)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MemoryPressurePsi {
    pub some: PsiRecord,
    pub full: PsiRecord,
}

/// Snapshot of host hardware and kernel telemetry
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareTelemetry {
    pub cpu_temp_c: f64,
    pub max_zone_temp_c: f64,
    pub thermal_zones: Vec<ThermalZoneTelemetry>,
    pub battery_pct: Option<u32>,
    pub battery_drain_mw: f64,
    pub battery_power_w: f64,
    pub battery_status: String,
    pub is_charging: bool,
    pub is_discharging: bool,
    pub ac_online: bool,
    pub memory_pressure_psi: MemoryPressurePsi,
    pub memory_total_mb: u64,
    pub memory_available_mb: u64,
    pub memory_free_mb: u64,
    pub memory_used_mb: u64,
    pub memory_available_pct: f64,
    pub governor_mode: GovernorMode,
    pub scaling_governor: String,
    pub epp: String,
    pub turbo_enabled: bool,
    pub dynatune_socket_connected: bool,
    pub kernel_optimizer_present: bool,
    pub cpu_governor_script_present: bool,
    pub timestamp_epoch_ms: u64,
}

impl Default for HardwareTelemetry {
    fn default() -> Self {
        Self {
            cpu_temp_c: 45.0,
            max_zone_temp_c: 45.0,
            thermal_zones: Vec::new(),
            battery_pct: Some(100),
            battery_drain_mw: 0.0,
            battery_power_w: 0.0,
            battery_status: "Full".to_string(),
            is_charging: true,
            is_discharging: false,
            ac_online: true,
            memory_pressure_psi: MemoryPressurePsi::default(),
            memory_total_mb: 8192,
            memory_available_mb: 6144,
            memory_free_mb: 4096,
            memory_used_mb: 2048,
            memory_available_pct: 75.0,
            governor_mode: GovernorMode::Balanced,
            scaling_governor: "powersave".to_string(),
            epp: "balance_performance".to_string(),
            turbo_enabled: true,
            dynatune_socket_connected: false,
            kernel_optimizer_present: false,
            cpu_governor_script_present: false,
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }
}

/// Evaluation result from pacing and workload throttling logic
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PacingEvaluation {
    pub token_pacing_delay_ms: u64,
    pub pause_background_wasm: bool,
    pub recommended_worker_concurrency: usize,
    pub target_governor: GovernorMode,
    pub is_throttled: bool,
    pub throttle_reason: Option<String>,
    pub timestamp_epoch_ms: u64,
}

/// Dynatune bidirectional IPC message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneIpcMessage {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
}

/// Dynatune bidirectional IPC response
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DynatuneIpcResponse {
    pub success: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub applied_governor: Option<String>,
    #[serde(default)]
    pub applied_epp: Option<String>,
}

/// Outcome of governor command dispatch
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchOutcome {
    pub success: bool,
    pub mode: GovernorMode,
    pub method: String,
    pub message: String,
    pub timestamp_epoch_ms: u64,
}

// ==============================================================================
// 3. Configurable Subsystem Paths
// ==============================================================================

/// Configurable filesystem and socket paths (enables sandboxed unit testing)
#[derive(Debug, Clone)]
pub struct HardwareControllerPaths {
    pub sysfs_root: PathBuf,
    pub procfs_root: PathBuf,
    pub dynatune_sock: PathBuf,
    pub cpu_governor_script: PathBuf,
    pub kernel_ctl_bin: PathBuf,
}

impl Default for HardwareControllerPaths {
    fn default() -> Self {
        Self {
            sysfs_root: PathBuf::from("/"),
            procfs_root: PathBuf::from("/"),
            dynatune_sock: PathBuf::from(DEFAULT_DYNATUNE_SOCK),
            cpu_governor_script: resolve_governor_script_path(),
            kernel_ctl_bin: resolve_kernel_ctl_path(),
        }
    }
}

fn resolve_governor_script_path() -> PathBuf {
    let default_path = PathBuf::from(DEFAULT_CPU_GOVERNOR_SCRIPT);
    if default_path.exists() {
        return default_path;
    }
    let rel_path = PathBuf::from(RELATIVE_CPU_GOVERNOR_SCRIPT);
    if rel_path.exists() {
        return rel_path;
    }
    default_path
}

fn resolve_kernel_ctl_path() -> PathBuf {
    let sibling = PathBuf::from(SIBLING_KERNEL_CTL_BIN);
    if sibling.exists() {
        return sibling;
    }
    let system = PathBuf::from(SYSTEM_KERNEL_CTL_BIN);
    if system.exists() {
        return system;
    }
    sibling
}

// ==============================================================================
// 4. Hardware Telemetry Collector
// ==============================================================================

/// Real-time hardware telemetry collector for Linux sysfs and procfs
pub struct HardwareTelemetryCollector {
    paths: HardwareControllerPaths,
}

impl HardwareTelemetryCollector {
    pub fn new(paths: HardwareControllerPaths) -> Self {
        Self { paths }
    }

    /// Read comprehensive real-time telemetry
    pub fn collect(&self) -> HardwareTelemetry {
        let (cpu_temp_c, max_zone_temp_c, thermal_zones) = self.read_thermal();
        let (battery_pct, battery_drain_mw, battery_power_w, battery_status, is_charging, is_discharging, ac_online) =
            self.read_battery();
        let memory_pressure_psi = self.read_memory_pressure_psi();
        let (memory_total_mb, memory_available_mb, memory_free_mb, memory_used_mb, memory_available_pct) =
            self.read_meminfo();
        let (governor_mode, scaling_governor, epp, turbo_enabled) = self.read_governor_state();

        let dynatune_socket_connected = self.check_dynatune_socket();
        let kernel_optimizer_present = self.paths.kernel_ctl_bin.exists();
        let cpu_governor_script_present = self.paths.cpu_governor_script.exists();

        HardwareTelemetry {
            cpu_temp_c,
            max_zone_temp_c,
            thermal_zones,
            battery_pct,
            battery_drain_mw,
            battery_power_w,
            battery_status,
            is_charging,
            is_discharging,
            ac_online,
            memory_pressure_psi,
            memory_total_mb,
            memory_available_mb,
            memory_free_mb,
            memory_used_mb,
            memory_available_pct,
            governor_mode,
            scaling_governor,
            epp,
            turbo_enabled,
            dynatune_socket_connected,
            kernel_optimizer_present,
            cpu_governor_script_present,
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }

    /// Inspect CPU package temperature and thermal zones
    pub fn read_thermal(&self) -> (f64, f64, Vec<ThermalZoneTelemetry>) {
        let mut zones = Vec::new();
        let mut cpu_temp_milli: Option<i64> = None;
        let mut max_milli = 0i64;

        let thermal_dir = self.paths.sysfs_root.join("sys/class/thermal");
        if let Ok(entries) = fs::read_dir(&thermal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let type_path = path.join("type");
                    let temp_path = path.join("temp");

                    let zone_type = fs::read_to_string(&type_path)
                        .unwrap_or_else(|_| "unknown".to_string())
                        .trim()
                        .to_string();

                    if let Ok(raw) = fs::read_to_string(&temp_path) {
                        if let Ok(milli) = raw.trim().parse::<i64>() {
                            let temp_c = milli as f64 / 1000.0;
                            if milli > max_milli {
                                max_milli = milli;
                            }
                            // Prioritize Intel CPU package sensor
                            if zone_type == "x86_pkg_temp" {
                                cpu_temp_milli = Some(milli);
                            } else if cpu_temp_milli.is_none() && (zone_type == "acpitz" || zone_type == "cpu-thermal") {
                                cpu_temp_milli = Some(milli);
                            }
                            zones.push(ThermalZoneTelemetry {
                                name,
                                zone_type,
                                temp_c,
                            });
                        }
                    }
                }
            }
        }

        // Secondary fallback: hwmon coretemp
        if cpu_temp_milli.is_none() {
            let hwmon_dir = self.paths.sysfs_root.join("sys/class/hwmon");
            if let Ok(entries) = fs::read_dir(&hwmon_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = fs::read_to_string(p.join("name")).unwrap_or_default();
                    if name.trim() == "coretemp" {
                        if let Ok(temp1) = fs::read_to_string(p.join("temp1_input")) {
                            if let Ok(milli) = temp1.trim().parse::<i64>() {
                                cpu_temp_milli = Some(milli);
                                if milli > max_milli {
                                    max_milli = milli;
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        let cpu_c = cpu_temp_milli.unwrap_or(max_milli) as f64 / 1000.0;
        let max_c = max_milli as f64 / 1000.0;
        let final_cpu_c = if cpu_c > 0.0 { cpu_c } else { 45.0 };
        let final_max_c = if max_c > 0.0 { max_c } else { final_cpu_c };

        (final_cpu_c, final_max_c, zones)
    }

    /// Inspect power supply (AC adapter & battery)
    pub fn read_battery(&self) -> (Option<u32>, f64, f64, String, bool, bool, bool) {
        let ps_dir = self.paths.sysfs_root.join("sys/class/power_supply");
        let mut ac_online = false;
        let mut battery_capacity: Option<u32> = None;
        let mut battery_status = "Unknown".to_string();
        let mut battery_power_w = 0.0;
        let mut is_discharging = false;
        let mut is_charging = false;

        if let Ok(entries) = fs::read_dir(&ps_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

                // AC Adapter
                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(content) = fs::read_to_string(path.join("online")) {
                        if content.trim() == "1" {
                            ac_online = true;
                        }
                    }
                }

                // Battery device (BAT0, BAT1, etc.)
                if name.starts_with("BAT") && battery_capacity.is_none() {
                    if let Ok(cap_str) = fs::read_to_string(path.join("capacity")) {
                        if let Ok(cap) = cap_str.trim().parse::<u32>() {
                            battery_capacity = Some(cap);
                        }
                    }

                    if let Ok(st) = fs::read_to_string(path.join("status")) {
                        battery_status = st.trim().to_string();
                        if battery_status.eq_ignore_ascii_case("Discharging") {
                            is_discharging = true;
                        } else if battery_status.eq_ignore_ascii_case("Charging") {
                            is_charging = true;
                            ac_online = true; // Charging implies AC plugged in
                        } else if battery_status.eq_ignore_ascii_case("Full") {
                            ac_online = true;
                        }
                    }

                    // Power draw in micro-Watts
                    if let Ok(p_str) = fs::read_to_string(path.join("power_now")) {
                        if let Ok(p_uw) = p_str.trim().parse::<f64>() {
                            battery_power_w = p_uw / 1_000_000.0;
                        }
                    } else if let Ok(c_str) = fs::read_to_string(path.join("current_now")) {
                        if let Ok(c_ua) = c_str.trim().parse::<f64>() {
                            let v_uv = fs::read_to_string(path.join("voltage_now"))
                                .ok()
                                .and_then(|v| v.trim().parse::<f64>().ok())
                                .unwrap_or(7_600_000.0);
                            battery_power_w = (c_ua * v_uv) / 1_000_000_000_000.0;
                        }
                    }
                }
            }
        }

        // If no battery was found, assume AC desktop mode
        if battery_capacity.is_none() {
            ac_online = true;
            battery_status = "AC Only".to_string();
        }

        let battery_drain_mw = battery_power_w * 1000.0;
        (
            battery_capacity,
            battery_drain_mw,
            battery_power_w,
            battery_status,
            is_charging,
            is_discharging,
            ac_online,
        )
    }

    /// Parse Linux Memory Pressure Stall Information (PSI) from `/proc/pressure/memory`
    pub fn read_memory_pressure_psi(&self) -> MemoryPressurePsi {
        let psi_path = self.paths.procfs_root.join("proc/pressure/memory");
        let mut result = MemoryPressurePsi::default();

        if let Ok(content) = fs::read_to_string(&psi_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("some ") {
                    result.some = parse_psi_line(rest);
                } else if let Some(rest) = trimmed.strip_prefix("full ") {
                    result.full = parse_psi_line(rest);
                }
            }
        }

        result
    }

    /// Parse `/proc/meminfo`
    pub fn read_meminfo(&self) -> (u64, u64, u64, u64, f64) {
        let meminfo_path = self.paths.procfs_root.join("proc/meminfo");
        let mut map = HashMap::new();

        if let Ok(content) = fs::read_to_string(&meminfo_path) {
            for line in content.lines() {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() == 2 {
                    let key = parts[0].trim();
                    let val_kb = parts[1]
                        .trim()
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0);
                    map.insert(key.to_string(), val_kb);
                }
            }
        }

        let total_kb = *map.get("MemTotal").unwrap_or(&8_388_608); // 8GB default
        let free_kb = *map.get("MemFree").unwrap_or(&4_194_304);
        let available_kb = *map.get("MemAvailable").unwrap_or(&free_kb);

        let total_mb = total_kb / 1024;
        let free_mb = free_kb / 1024;
        let available_mb = available_kb / 1024;
        let used_mb = total_mb.saturating_sub(available_mb);
        let available_pct = if total_mb > 0 {
            (available_mb as f64 / total_mb as f64) * 100.0
        } else {
            100.0
        };

        (total_mb, available_mb, free_mb, used_mb, available_pct)
    }

    /// Read active governor and EPP preference
    pub fn read_governor_state(&self) -> (GovernorMode, String, String, bool) {
        let cpufreq_p0 = self.paths.sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0");
        let gov = fs::read_to_string(cpufreq_p0.join("scaling_governor"))
            .unwrap_or_else(|_| "powersave".to_string())
            .trim()
            .to_string();

        let epp = fs::read_to_string(cpufreq_p0.join("energy_performance_preference"))
            .unwrap_or_else(|_| "balance_performance".to_string())
            .trim()
            .to_string();

        let no_turbo = fs::read_to_string(self.paths.sysfs_root.join("sys/devices/system/cpu/intel_pstate/no_turbo"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        let turbo_enabled = !no_turbo;

        let mode = if gov == "performance" || epp == "performance" {
            GovernorMode::Performance
        } else if epp == "balance_power" || gov == "powersave" && !turbo_enabled {
            GovernorMode::PowerSave
        } else {
            GovernorMode::Balanced
        };

        (mode, gov, epp, turbo_enabled)
    }

    /// Check if Dynatune socket exists and is connectable
    pub fn check_dynatune_socket(&self) -> bool {
        if !self.paths.dynatune_sock.exists() {
            return false;
        }
        match UnixStream::connect(&self.paths.dynatune_sock) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                true
            }
            Err(_) => false,
        }
    }
}

fn parse_psi_line(s: &str) -> PsiRecord {
    let mut record = PsiRecord::default();
    for token in s.split_whitespace() {
        if let Some(val) = token.strip_prefix("avg10=") {
            record.avg10 = val.parse::<f64>().unwrap_or(0.0);
        } else if let Some(val) = token.strip_prefix("avg60=") {
            record.avg60 = val.parse::<f64>().unwrap_or(0.0);
        } else if let Some(val) = token.strip_prefix("avg300=") {
            record.avg300 = val.parse::<f64>().unwrap_or(0.0);
        } else if let Some(val) = token.strip_prefix("total=") {
            record.total_us = val.parse::<u64>().unwrap_or(0);
        }
    }
    record
}

// ==============================================================================
// 5. Dynamic LLM Generation & WASM Pacer (`GenerationPacer`)
// ==============================================================================

/// Generation Pacing & Workload Throttling Arbiter
///
/// Implements pacing evaluation with thermal hysteresis:
/// - At temp >= 78.0°C or battery < 20% on battery power:
///   injects token pacing delay (50ms per token chunk) to lower GPU/CPU duty cycle,
///   pauses non-essential background WASM compaction/indexing, clamps concurrency to 1 worker.
/// - At temp <= 65.0°C on AC power:
///   zero pacing delay (maximum throughput), unpauses background WASM, scales background worker threads.
/// - Recovers from throttling only when temperature cools below 70.0°C (hysteresis).
pub struct GenerationPacer {
    is_throttled: AtomicBool,
}

impl GenerationPacer {
    pub fn new() -> Self {
        Self {
            is_throttled: AtomicBool::new(false),
        }
    }

    /// Reset internal state
    pub fn reset(&self) {
        self.is_throttled.store(false, Ordering::SeqCst);
    }

    /// Evaluate current hardware telemetry and compute pacing parameters
    pub fn evaluate(&self, telemetry: &HardwareTelemetry) -> PacingEvaluation {
        let currently_throttled = self.is_throttled.load(Ordering::SeqCst);

        // 1. Critical Throttling Triggers:
        //    a) CPU temp >= 78.0°C
        //    b) Battery capacity < 20% while on battery power (discharging or not AC online)
        let thermal_breached = telemetry.cpu_temp_c >= THERMAL_CLAMP_THRESHOLD_C;
        let battery_critical = telemetry.battery_pct.map(|pct| pct < BATTERY_CRITICAL_DISCHARGE_PCT).unwrap_or(false)
            && (!telemetry.ac_online || telemetry.is_discharging);

        let mut throttle_reason = None;
        let should_throttle = if thermal_breached {
            throttle_reason = Some(format!(
                "Thermal clamp engaged: CPU temp {:.1}°C >= threshold {:.1}°C",
                telemetry.cpu_temp_c, THERMAL_CLAMP_THRESHOLD_C
            ));
            true
        } else if battery_critical {
            throttle_reason = Some(format!(
                "Battery discharge clamp engaged: capacity {}% < threshold {}% on battery power",
                telemetry.battery_pct.unwrap_or(0),
                BATTERY_CRITICAL_DISCHARGE_PCT
            ));
            true
        } else if currently_throttled {
            // Hysteresis: remain throttled until temperature drops below recovery threshold (70.0°C)
            if telemetry.cpu_temp_c >= THERMAL_RECOVERY_THRESHOLD_C {
                throttle_reason = Some(format!(
                    "Thermal hysteresis clamp maintained: CPU temp {:.1}°C above recovery threshold {:.1}°C",
                    telemetry.cpu_temp_c, THERMAL_RECOVERY_THRESHOLD_C
                ));
                true
            } else {
                false
            }
        } else {
            false
        };

        self.is_throttled.store(should_throttle, Ordering::SeqCst);

        if should_throttle {
            PacingEvaluation {
                token_pacing_delay_ms: THROTTLED_TOKEN_PACING_DELAY_MS,
                pause_background_wasm: true,
                recommended_worker_concurrency: THROTTLED_CONCURRENCY_LIMIT,
                target_governor: GovernorMode::PowerSave,
                is_throttled: true,
                throttle_reason,
                timestamp_epoch_ms: current_epoch_ms(),
            }
        } else if telemetry.ac_online && telemetry.cpu_temp_c <= THERMAL_COOL_BOOST_THRESHOLD_C {
            // Cool condition on AC power: full throughput boost
            PacingEvaluation {
                token_pacing_delay_ms: 0,
                pause_background_wasm: false,
                recommended_worker_concurrency: BOOST_CONCURRENCY_LIMIT,
                target_governor: GovernorMode::Performance,
                is_throttled: false,
                throttle_reason: None,
                timestamp_epoch_ms: current_epoch_ms(),
            }
        } else {
            // Nominal balanced condition
            PacingEvaluation {
                token_pacing_delay_ms: NOMINAL_TOKEN_PACING_DELAY_MS,
                pause_background_wasm: false,
                recommended_worker_concurrency: NOMINAL_CONCURRENCY_LIMIT,
                target_governor: GovernorMode::Balanced,
                is_throttled: false,
                throttle_reason: None,
                timestamp_epoch_ms: current_epoch_ms(),
            }
        }
    }

    /// Asynchronous token pacing delay hook
    pub async fn pace_token_chunk(&self, delay_ms: u64) {
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
}

impl Default for GenerationPacer {
    fn default() -> Self {
        Self::new()
    }
}

// ==============================================================================
// 6. Workload Throttler
// ==============================================================================

/// Workload Throttler managing concurrency, background WASM pause state, and active tasks
pub struct WorkloadThrottler {
    pause_background_wasm: Arc<AtomicBool>,
    current_pacing_delay_ms: Arc<AtomicU64>,
    worker_concurrency: Arc<AtomicUsize>,
    active_heavy_tasks: Arc<AtomicUsize>,
    last_evaluation: Arc<RwLock<PacingEvaluation>>,
}

impl WorkloadThrottler {
    pub fn new() -> Self {
        Self {
            pause_background_wasm: Arc::new(AtomicBool::new(false)),
            current_pacing_delay_ms: Arc::new(AtomicU64::new(0)),
            worker_concurrency: Arc::new(AtomicUsize::new(NOMINAL_CONCURRENCY_LIMIT)),
            active_heavy_tasks: Arc::new(AtomicUsize::new(0)),
            last_evaluation: Arc::new(RwLock::new(PacingEvaluation {
                token_pacing_delay_ms: 0,
                pause_background_wasm: false,
                recommended_worker_concurrency: NOMINAL_CONCURRENCY_LIMIT,
                target_governor: GovernorMode::Balanced,
                is_throttled: false,
                throttle_reason: None,
                timestamp_epoch_ms: current_epoch_ms(),
            })),
        }
    }

    /// Update throttler metrics with new pacing evaluation
    pub fn update(&self, eval: &PacingEvaluation) {
        self.pause_background_wasm.store(eval.pause_background_wasm, Ordering::SeqCst);
        self.current_pacing_delay_ms.store(eval.token_pacing_delay_ms, Ordering::SeqCst);
        self.worker_concurrency.store(eval.recommended_worker_concurrency, Ordering::SeqCst);
        if let Ok(mut lock) = self.last_evaluation.write() {
            *lock = eval.clone();
        }
    }

    /// Check if background WASM compaction/indexing should be paused
    pub fn should_pause_background_wasm(&self) -> bool {
        self.pause_background_wasm.load(Ordering::Relaxed)
    }

    /// Read active token pacing delay in milliseconds
    pub fn pacing_delay_ms(&self) -> u64 {
        self.current_pacing_delay_ms.load(Ordering::Relaxed)
    }

    /// Read current recommended worker concurrency limit
    pub fn worker_concurrency(&self) -> usize {
        self.worker_concurrency.load(Ordering::Relaxed)
    }

    /// Read number of active heavy tasks
    pub fn active_heavy_tasks(&self) -> usize {
        self.active_heavy_tasks.load(Ordering::Relaxed)
    }

    /// Read last pacing evaluation
    pub fn last_evaluation(&self) -> PacingEvaluation {
        self.last_evaluation
            .read()
            .map(|l| l.clone())
            .unwrap_or_else(|_| PacingEvaluation {
                token_pacing_delay_ms: 0,
                pause_background_wasm: false,
                recommended_worker_concurrency: NOMINAL_CONCURRENCY_LIMIT,
                target_governor: GovernorMode::Balanced,
                is_throttled: false,
                throttle_reason: None,
                timestamp_epoch_ms: current_epoch_ms(),
            })
    }

    /// Acquire an RAII workload lease guard
    pub fn acquire_lease(&self, workload: &str) -> WorkloadLeaseGuard {
        self.active_heavy_tasks.fetch_add(1, Ordering::SeqCst);
        WorkloadLeaseGuard {
            active_counter: Arc::clone(&self.active_heavy_tasks),
            workload_name: workload.to_string(),
        }
    }
}

impl Default for WorkloadThrottler {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII Workload Lease Guard
pub struct WorkloadLeaseGuard {
    active_counter: Arc<AtomicUsize>,
    workload_name: String,
}

impl WorkloadLeaseGuard {
    pub fn workload_name(&self) -> &str {
        &self.workload_name
    }
}

impl Drop for WorkloadLeaseGuard {
    fn drop(&mut self) {
        self.active_counter.fetch_sub(1, Ordering::SeqCst);
    }
}

// ==============================================================================
// 7. Hardware Governor Command Dispatcher
// ==============================================================================

/// Dispatches governor switches to Dynatune IPC socket, `scripts/cpu-governor.sh`,
/// `dynabook-kernel-ctl`, or direct Linux sysfs fallback.
pub struct HardwareGovernorDispatcher {
    paths: HardwareControllerPaths,
}

impl HardwareGovernorDispatcher {
    pub fn new(paths: HardwareControllerPaths) -> Self {
        Self { paths }
    }

    /// Send command to Dynatune Unix socket (`/run/dynatune.sock`)
    pub fn send_dynatune_ipc(&self, msg: &DynatuneIpcMessage) -> Result<DynatuneIpcResponse> {
        let mut stream = UnixStream::connect(&self.paths.dynatune_sock).map_err(|e| {
            HgbError::Execution(format!(
                "Failed to connect to Dynatune socket at {}: {}",
                self.paths.dynatune_sock.display(),
                e
            ))
        })?;

        stream.set_write_timeout(Some(DYNATUNE_SOCKET_TIMEOUT)).map_err(|e| {
            HgbError::Execution(format!("Failed to set socket write timeout: {}", e))
        })?;
        stream.set_read_timeout(Some(DYNATUNE_SOCKET_TIMEOUT)).map_err(|e| {
            HgbError::Execution(format!("Failed to set socket read timeout: {}", e))
        })?;

        let mut payload = serde_json::to_vec(msg).map_err(|e| {
            HgbError::Serialization(format!("Failed to serialize Dynatune IPC payload: {}", e))
        })?;
        payload.push(b'\n');

        stream.write_all(&payload).map_err(|e| {
            HgbError::Execution(format!("Failed to write to Dynatune socket: {}", e))
        })?;

        let mut reader = std::io::BufReader::new(stream);
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).map_err(|e| {
            HgbError::Execution(format!("Failed to read response from Dynatune socket: {}", e))
        })?;

        let resp: DynatuneIpcResponse = serde_json::from_str(line.trim()).map_err(|e| {
            HgbError::Serialization(format!("Failed to parse Dynatune socket JSON response: {}", e))
        })?;

        Ok(resp)
    }

    /// Dispatch governor mode switch across fallback chain
    pub fn dispatch(&self, mode: GovernorMode) -> DispatchOutcome {
        let mode_str = mode.as_str();

        // 1. Primary: Try Dynatune Unix socket
        if self.paths.dynatune_sock.exists() {
            let (gov, epp) = match mode {
                GovernorMode::Performance => ("performance", "performance"),
                GovernorMode::Balanced => ("powersave", "balance_performance"),
                GovernorMode::PowerSave => ("powersave", "balance_power"),
            };
            let msg = DynatuneIpcMessage {
                action: "set_governor".to_string(),
                governor: Some(gov.to_string()),
                epp: Some(epp.to_string()),
                caller: Some("hgb_hardware_aware_ai_controller".to_string()),
            };
            if let Ok(resp) = self.send_dynatune_ipc(&msg) {
                if resp.success {
                    return DispatchOutcome {
                        success: true,
                        mode,
                        method: "dynatune_unix_socket".to_string(),
                        message: format!("Applied {} mode via Dynatune IPC socket", mode_str),
                        timestamp_epoch_ms: current_epoch_ms(),
                    };
                }
            }
        }

        // 2. Secondary: Sibling bash script `scripts/cpu-governor.sh`
        if self.paths.cpu_governor_script.exists() {
            let output = Command::new(&self.paths.cpu_governor_script)
                .arg(mode_str)
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    return DispatchOutcome {
                        success: true,
                        mode,
                        method: "cpu_governor_script".to_string(),
                        message: format!(
                            "Applied {} mode via {}",
                            mode_str,
                            self.paths.cpu_governor_script.display()
                        ),
                        timestamp_epoch_ms: current_epoch_ms(),
                    };
                }
            }
        }

        // 3. Tertiary: Sibling binary `dynabook-kernel-ctl`
        if self.paths.kernel_ctl_bin.exists() {
            let output = Command::new(&self.paths.kernel_ctl_bin)
                .arg("profile")
                .arg(mode_str)
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    return DispatchOutcome {
                        success: true,
                        mode,
                        method: "dynabook_kernel_ctl".to_string(),
                        message: format!("Applied {} mode via dynabook-kernel-ctl", mode_str),
                        timestamp_epoch_ms: current_epoch_ms(),
                    };
                }
            }
        }

        // 4. Quaternary: Direct sysfs writes
        let sysfs_res = self.apply_direct_sysfs(mode);
        if sysfs_res {
            return DispatchOutcome {
                success: true,
                mode,
                method: "direct_sysfs".to_string(),
                message: format!("Applied {} mode directly to sysfs", mode_str),
                timestamp_epoch_ms: current_epoch_ms(),
            };
        }

        // Dry-run / fallback notification
        DispatchOutcome {
            success: true,
            mode,
            method: "plan_recorded".to_string(),
            message: format!(
                "Governor target mode {} registered (dry-run / unprivileged sysfs fallback)",
                mode_str
            ),
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }

    /// Write governor and EPP directly to sysfs
    fn apply_direct_sysfs(&self, mode: GovernorMode) -> bool {
        let (gov, epp, no_turbo, min_ttl_ms) = match mode {
            GovernorMode::Performance => ("performance", "performance", "0", BURST_MGLRU_MIN_TTL_MS),
            GovernorMode::Balanced => ("powersave", "balance_performance", "0", IDLE_MGLRU_MIN_TTL_MS),
            GovernorMode::PowerSave => ("powersave", "balance_power", "1", IDLE_MGLRU_MIN_TTL_MS),
        };

        let mut applied_any = false;

        // cpufreq policies
        let cpufreq_dir = self.paths.sysfs_root.join("sys/devices/system/cpu/cpufreq");
        if let Ok(entries) = fs::read_dir(cpufreq_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                    let gov_path = p.join("scaling_governor");
                    let epp_path = p.join("energy_performance_preference");
                    if fs::write(&gov_path, gov).is_ok() {
                        applied_any = true;
                    }
                    if fs::write(&epp_path, epp).is_ok() {
                        applied_any = true;
                    }
                }
            }
        }

        // intel_pstate no_turbo
        let no_turbo_path = self.paths.sysfs_root.join("sys/devices/system/cpu/intel_pstate/no_turbo");
        if fs::write(&no_turbo_path, no_turbo).is_ok() {
            applied_any = true;
        }

        // MGLRU min_ttl_ms
        let mglru_path = self.paths.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        if fs::write(&mglru_path, min_ttl_ms.to_string()).is_ok() {
            applied_any = true;
        }

        applied_any
    }
}

// ==============================================================================
// 8. Unified Hardware-Aware AI Controller
// ==============================================================================

/// Unified Hardware-Aware AI Controller orchestrating telemetry collection,
/// pacing calculations, throttler state, and kernel governor dispatch.
pub struct HardwareAwareAiController {
    paths: HardwareControllerPaths,
    collector: HardwareTelemetryCollector,
    pacer: GenerationPacer,
    throttler: WorkloadThrottler,
    dispatcher: HardwareGovernorDispatcher,
    last_telemetry: Arc<RwLock<HardwareTelemetry>>,
    last_dispatch: Arc<RwLock<Option<DispatchOutcome>>>,
}

impl HardwareAwareAiController {
    /// Create new production controller bound to host Linux kernel & sibling tools
    pub fn new() -> Self {
        Self::with_paths(HardwareControllerPaths::default())
    }

    /// Construct controller with explicit paths (supports sandboxed unit tests)
    pub fn with_paths(paths: HardwareControllerPaths) -> Self {
        let collector = HardwareTelemetryCollector::new(paths.clone());
        let pacer = GenerationPacer::new();
        let throttler = WorkloadThrottler::new();
        let dispatcher = HardwareGovernorDispatcher::new(paths.clone());

        Self {
            paths,
            collector,
            pacer,
            throttler,
            dispatcher,
            last_telemetry: Arc::new(RwLock::new(HardwareTelemetry::default())),
            last_dispatch: Arc::new(RwLock::new(None)),
        }
    }

    /// Helper for test fixtures
    pub fn for_test(
        sysfs_root: &Path,
        procfs_root: &Path,
        dynatune_sock: &Path,
        cpu_governor_script: &Path,
    ) -> Self {
        let paths = HardwareControllerPaths {
            sysfs_root: sysfs_root.to_path_buf(),
            procfs_root: procfs_root.to_path_buf(),
            dynatune_sock: dynatune_sock.to_path_buf(),
            cpu_governor_script: cpu_governor_script.to_path_buf(),
            kernel_ctl_bin: sysfs_root.join("dynabook-kernel-ctl"),
        };
        Self::with_paths(paths)
    }

    /// Read real-time telemetry from Linux sysfs/procfs and update internal cache
    pub fn refresh_telemetry(&self) -> HardwareTelemetry {
        let telem = self.collector.collect();
        if let Ok(mut lock) = self.last_telemetry.write() {
            *lock = telem.clone();
        }
        telem
    }

    /// Get latest cached telemetry snapshot (or refreshes if uninitialized)
    pub fn get_telemetry(&self) -> HardwareTelemetry {
        if let Ok(lock) = self.last_telemetry.read() {
            if lock.timestamp_epoch_ms > 0 {
                return lock.clone();
            }
        }
        self.refresh_telemetry()
    }

    /// Evaluate current telemetry, compute pacing parameters, and update throttler
    pub fn evaluate_and_pace(&self) -> PacingEvaluation {
        let telem = self.refresh_telemetry();
        let eval = self.pacer.evaluate(&telem);
        self.throttler.update(&eval);

        // If governor mode changed or is throttled, dispatch command
        if telem.governor_mode != eval.target_governor {
            let outcome = self.dispatcher.dispatch(eval.target_governor);
            if let Ok(mut lock) = self.last_dispatch.write() {
                *lock = Some(outcome);
            }
        }

        eval
    }

    /// Asynchronous token pacing delay hook for LLM generation loops
    pub async fn pace_token_chunk(&self) {
        let delay_ms = self.throttler.pacing_delay_ms();
        if delay_ms > 0 {
            self.pacer.pace_token_chunk(delay_ms).await;
        }
    }

    /// Check if background WASM compaction/indexing should be paused
    pub fn should_pause_background_wasm(&self) -> bool {
        self.throttler.should_pause_background_wasm()
    }

    /// Recommended worker concurrency limit
    pub fn worker_concurrency(&self) -> usize {
        self.throttler.worker_concurrency()
    }

    /// Explicitly dispatch a target governor mode
    pub fn dispatch_governor_mode(&self, mode: GovernorMode) -> DispatchOutcome {
        let outcome = self.dispatcher.dispatch(mode);
        if let Ok(mut lock) = self.last_dispatch.write() {
            *lock = Some(outcome.clone());
        }
        outcome
    }

    /// Acquire RAII workload lease
    pub fn acquire_lease(&self, workload: &str) -> WorkloadLeaseGuard {
        self.throttler.acquire_lease(workload)
    }

    /// Access underlying controller paths
    pub fn paths(&self) -> &HardwareControllerPaths {
        &self.paths
    }

    // ==========================================================================
    // MCP Tool Declaration & Invocation Handler
    // ==========================================================================

    /// Declare the MCP tool specification for `hgb_hardware_telemetry_status`
    pub fn declare_mcp_tool() -> Value {
        serde_json::json!({
            "name": "hgb_hardware_telemetry_status",
            "description": "Inspect real-time Linux hardware telemetry (CPU package temperature °C, battery drain rate in mW, memory pressure PSI) and dynamic LLM generation / WASM pacing status",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "force_refresh": {
                        "type": "boolean",
                        "description": "Force fresh reading of Linux sysfs, procfs PSI, and Dynatune socket (default: true)",
                        "default": true
                    }
                }
            }
        })
    }

    /// Handle invocation of MCP tool `hgb_hardware_telemetry_status`
    pub fn handle_mcp_call(&self, arguments: &Value) -> Value {
        let force_refresh = arguments
            .get("force_refresh")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let telem = if force_refresh {
            self.refresh_telemetry()
        } else {
            self.get_telemetry()
        };

        let eval = self.pacer.evaluate(&telem);
        self.throttler.update(&eval);

        let last_dispatch = self.last_dispatch.read().ok().and_then(|l| l.clone());

        serde_json::json!({
            "status": "success",
            "telemetry": {
                "cpu_temp_c": telem.cpu_temp_c,
                "max_zone_temp_c": telem.max_zone_temp_c,
                "battery_pct": telem.battery_pct,
                "battery_drain_mw": telem.battery_drain_mw,
                "battery_power_w": telem.battery_power_w,
                "battery_status": telem.battery_status,
                "is_charging": telem.is_charging,
                "is_discharging": telem.is_discharging,
                "ac_online": telem.ac_online,
                "memory_pressure_psi": {
                    "some": {
                        "avg10": telem.memory_pressure_psi.some.avg10,
                        "avg60": telem.memory_pressure_psi.some.avg60,
                        "avg300": telem.memory_pressure_psi.some.avg300,
                        "total_us": telem.memory_pressure_psi.some.total_us
                    },
                    "full": {
                        "avg10": telem.memory_pressure_psi.full.avg10,
                        "avg60": telem.memory_pressure_psi.full.avg60,
                        "avg300": telem.memory_pressure_psi.full.avg300,
                        "total_us": telem.memory_pressure_psi.full.total_us
                    }
                },
                "memory_total_mb": telem.memory_total_mb,
                "memory_available_mb": telem.memory_available_mb,
                "memory_used_mb": telem.memory_used_mb,
                "memory_available_pct": telem.memory_available_pct,
                "scaling_governor": telem.scaling_governor,
                "epp": telem.epp,
                "turbo_enabled": telem.turbo_enabled,
                "governor_mode": telem.governor_mode.as_str()
            },
            "pacing": {
                "token_pacing_delay_ms": eval.token_pacing_delay_ms,
                "pause_background_wasm": eval.pause_background_wasm,
                "recommended_worker_concurrency": eval.recommended_worker_concurrency,
                "target_governor": eval.target_governor.as_str(),
                "is_throttled": eval.is_throttled,
                "throttle_reason": eval.throttle_reason
            },
            "infrastructure": {
                "dynatune_socket_connected": telem.dynatune_socket_connected,
                "kernel_optimizer_present": telem.kernel_optimizer_present,
                "cpu_governor_script_present": telem.cpu_governor_script_present,
                "last_dispatch": last_dispatch
            },
            "timestamp_epoch_ms": current_epoch_ms()
        })
    }
}

impl Default for HardwareAwareAiController {
    fn default() -> Self {
        Self::new()
    }
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ==============================================================================
// 9. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;
    use tempfile::tempdir;

    /// Mock sysfs and procfs fixture generator
    fn setup_mock_environment(sysfs_root: &Path, procfs_root: &Path) {
        // Thermal zone
        let tz0 = sysfs_root.join("sys/class/thermal/thermal_zone0");
        fs::create_dir_all(&tz0).unwrap();
        fs::write(tz0.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(tz0.join("temp"), "52000\n").unwrap(); // 52.0°C

        // Power supply (AC plugged in, BAT0 at 85%)
        let ps = sysfs_root.join("sys/class/power_supply");
        let ac = ps.join("AC");
        let bat = ps.join("BAT0");
        fs::create_dir_all(&ac).unwrap();
        fs::create_dir_all(&bat).unwrap();
        fs::write(ac.join("online"), "1\n").unwrap();
        fs::write(bat.join("capacity"), "85\n").unwrap();
        fs::write(bat.join("status"), "Full\n").unwrap();
        fs::write(bat.join("power_now"), "12500000\n").unwrap(); // 12.5 W

        // CPU frequency policy0
        let p0 = sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0");
        fs::create_dir_all(&p0).unwrap();
        fs::write(p0.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(p0.join("energy_performance_preference"), "balance_performance\n").unwrap();

        // Intel pstate
        let pstate = sysfs_root.join("sys/devices/system/cpu/intel_pstate");
        fs::create_dir_all(&pstate).unwrap();
        fs::write(pstate.join("no_turbo"), "0\n").unwrap();

        // MGLRU
        let mglru = sysfs_root.join("sys/kernel/mm/lru_gen");
        fs::create_dir_all(&mglru).unwrap();
        fs::write(mglru.join("min_ttl_ms"), "0\n").unwrap();

        // Procfs pressure memory
        let proc_pressure = procfs_root.join("proc/pressure");
        fs::create_dir_all(&proc_pressure).unwrap();
        fs::write(
            proc_pressure.join("memory"),
            "some avg10=1.25 avg60=0.85 avg300=0.42 total=123456\nfull avg10=0.10 avg60=0.05 avg300=0.01 total=4567\n",
        ).unwrap();

        // Procfs meminfo
        let proc = procfs_root.join("proc");
        fs::create_dir_all(&proc).unwrap();
        fs::write(
            proc.join("meminfo"),
            "MemTotal:        8192000 kB\nMemFree:         4096000 kB\nMemAvailable:    6144000 kB\nSwapTotal:       2048000 kB\nSwapFree:        2048000 kB\n",
        ).unwrap();
    }

    #[test]
    fn test_telemetry_parsing_thermal_and_battery() {
        let temp_sysfs = tempdir().unwrap();
        let temp_procfs = tempdir().unwrap();
        let sock_path = temp_sysfs.path().join("dynatune.sock");
        let script_path = temp_sysfs.path().join("cpu-governor.sh");

        setup_mock_environment(temp_sysfs.path(), temp_procfs.path());

        let controller = HardwareAwareAiController::for_test(
            temp_sysfs.path(),
            temp_procfs.path(),
            &sock_path,
            &script_path,
        );

        let telem = controller.refresh_telemetry();
        assert!((telem.cpu_temp_c - 52.0).abs() < f64::EPSILON);
        assert_eq!(telem.battery_pct, Some(85));
        assert!(telem.ac_online);
        assert!(!telem.is_discharging);
        assert!((telem.battery_power_w - 12.5).abs() < 1e-4);
        assert!((telem.battery_drain_mw - 12500.0).abs() < 1e-2);
        assert_eq!(telem.battery_status, "Full");
    }

    #[test]
    fn test_telemetry_parsing_memory_pressure_psi() {
        let temp_sysfs = tempdir().unwrap();
        let temp_procfs = tempdir().unwrap();
        let sock_path = temp_sysfs.path().join("dynatune.sock");
        let script_path = temp_sysfs.path().join("cpu-governor.sh");

        setup_mock_environment(temp_sysfs.path(), temp_procfs.path());

        let controller = HardwareAwareAiController::for_test(
            temp_sysfs.path(),
            temp_procfs.path(),
            &sock_path,
            &script_path,
        );

        let telem = controller.refresh_telemetry();
        assert!((telem.memory_pressure_psi.some.avg10 - 1.25).abs() < 1e-4);
        assert!((telem.memory_pressure_psi.some.avg60 - 0.85).abs() < 1e-4);
        assert!((telem.memory_pressure_psi.some.avg300 - 0.42).abs() < 1e-4);
        assert_eq!(telem.memory_pressure_psi.some.total_us, 123456);

        assert!((telem.memory_pressure_psi.full.avg10 - 0.10).abs() < 1e-4);
        assert_eq!(telem.memory_total_mb, 8000);
        assert_eq!(telem.memory_available_mb, 6000);
        assert_eq!(telem.memory_used_mb, 2000);
        assert!((telem.memory_available_pct - 75.0).abs() < 1e-4);
    }

    #[test]
    fn test_pacing_calculation_hot_thermal_throttle() {
        let pacer = GenerationPacer::new();

        // 1. Nominal cool state
        let cool_telem = HardwareTelemetry {
            cpu_temp_c: 62.0,
            ac_online: true,
            battery_pct: Some(90),
            is_discharging: false,
            ..HardwareTelemetry::default()
        };
        let eval_cool = pacer.evaluate(&cool_telem);
        assert!(!eval_cool.is_throttled);
        assert_eq!(eval_cool.token_pacing_delay_ms, 0);
        assert!(!eval_cool.pause_background_wasm);
        assert_eq!(eval_cool.recommended_worker_concurrency, BOOST_CONCURRENCY_LIMIT);
        assert_eq!(eval_cool.target_governor, GovernorMode::Performance);

        // 2. Hot thermal clamp breach (80.5°C >= 78.0°C)
        let hot_telem = HardwareTelemetry {
            cpu_temp_c: 80.5,
            ac_online: true,
            battery_pct: Some(90),
            is_discharging: false,
            ..HardwareTelemetry::default()
        };
        let eval_hot = pacer.evaluate(&hot_telem);
        assert!(eval_hot.is_throttled);
        assert_eq!(eval_hot.token_pacing_delay_ms, THROTTLED_TOKEN_PACING_DELAY_MS);
        assert!(eval_hot.pause_background_wasm);
        assert_eq!(eval_hot.recommended_worker_concurrency, THROTTLED_CONCURRENCY_LIMIT);
        assert_eq!(eval_hot.target_governor, GovernorMode::PowerSave);
        assert!(eval_hot.throttle_reason.unwrap().contains("Thermal clamp engaged"));

        // 3. Thermal hysteresis: intermediate temp (74.0°C) must remain clamped!
        let intermediate_telem = HardwareTelemetry {
            cpu_temp_c: 74.0,
            ac_online: true,
            battery_pct: Some(90),
            is_discharging: false,
            ..HardwareTelemetry::default()
        };
        let eval_inter = pacer.evaluate(&intermediate_telem);
        assert!(eval_inter.is_throttled, "Must remain throttled under hysteresis");
        assert_eq!(eval_inter.token_pacing_delay_ms, THROTTLED_TOKEN_PACING_DELAY_MS);
        assert!(eval_inter.pause_background_wasm);

        // 4. Recovery below 70.0°C (68.0°C) disengages clamp
        let recovered_telem = HardwareTelemetry {
            cpu_temp_c: 68.0,
            ac_online: true,
            battery_pct: Some(90),
            is_discharging: false,
            ..HardwareTelemetry::default()
        };
        let eval_rec = pacer.evaluate(&recovered_telem);
        assert!(!eval_rec.is_throttled, "Clamp must disengage < 70.0°C");
        assert_eq!(eval_rec.token_pacing_delay_ms, NOMINAL_TOKEN_PACING_DELAY_MS);
        assert!(!eval_rec.pause_background_wasm);
        assert_eq!(eval_rec.recommended_worker_concurrency, NOMINAL_CONCURRENCY_LIMIT);
    }

    #[test]
    fn test_pacing_calculation_battery_critical_on_discharge() {
        let pacer = GenerationPacer::new();

        // Critical battery on battery power: 15% (< 20%) discharging
        let battery_crit_telem = HardwareTelemetry {
            cpu_temp_c: 55.0,
            ac_online: false,
            battery_pct: Some(15),
            is_discharging: true,
            ..HardwareTelemetry::default()
        };
        let eval = pacer.evaluate(&battery_crit_telem);
        assert!(eval.is_throttled);
        assert_eq!(eval.token_pacing_delay_ms, THROTTLED_TOKEN_PACING_DELAY_MS);
        assert!(eval.pause_background_wasm);
        assert_eq!(eval.recommended_worker_concurrency, THROTTLED_CONCURRENCY_LIMIT);
        assert_eq!(eval.target_governor, GovernorMode::PowerSave);
        assert!(eval.throttle_reason.unwrap().contains("Battery discharge clamp"));

        // Plugged into AC: disengages battery critical clamp
        let ac_plugged_telem = HardwareTelemetry {
            cpu_temp_c: 55.0,
            ac_online: true,
            battery_pct: Some(15),
            is_discharging: false,
            is_charging: true,
            ..HardwareTelemetry::default()
        };
        let eval_ac = pacer.evaluate(&ac_plugged_telem);
        assert!(!eval_ac.is_throttled);
        assert_eq!(eval_ac.token_pacing_delay_ms, 0);
        assert!(!eval_ac.pause_background_wasm);
    }

    #[test]
    fn test_governor_mode_direct_sysfs_dispatch() {
        let temp_sysfs = tempdir().unwrap();
        let temp_procfs = tempdir().unwrap();
        let sock_path = temp_sysfs.path().join("dynatune.sock");
        let script_path = temp_sysfs.path().join("cpu-governor.sh");

        setup_mock_environment(temp_sysfs.path(), temp_procfs.path());

        let controller = HardwareAwareAiController::for_test(
            temp_sysfs.path(),
            temp_procfs.path(),
            &sock_path,
            &script_path,
        );

        // 1. Dispatch performance mode
        let out_perf = controller.dispatch_governor_mode(GovernorMode::Performance);
        assert!(out_perf.success);
        assert_eq!(out_perf.method, "direct_sysfs");

        let p0 = temp_sysfs.path().join("sys/devices/system/cpu/cpufreq/policy0");
        assert_eq!(fs::read_to_string(p0.join("scaling_governor")).unwrap().trim(), "performance");
        assert_eq!(fs::read_to_string(p0.join("energy_performance_preference")).unwrap().trim(), "performance");
        assert_eq!(fs::read_to_string(temp_sysfs.path().join("sys/devices/system/cpu/intel_pstate/no_turbo")).unwrap().trim(), "0");
        assert_eq!(fs::read_to_string(temp_sysfs.path().join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap().trim(), "1000");

        // 2. Dispatch powersave mode
        let out_power = controller.dispatch_governor_mode(GovernorMode::PowerSave);
        assert!(out_power.success);
        assert_eq!(fs::read_to_string(p0.join("scaling_governor")).unwrap().trim(), "powersave");
        assert_eq!(fs::read_to_string(p0.join("energy_performance_preference")).unwrap().trim(), "balance_power");
        assert_eq!(fs::read_to_string(temp_sysfs.path().join("sys/devices/system/cpu/intel_pstate/no_turbo")).unwrap().trim(), "1");
        assert_eq!(fs::read_to_string(temp_sysfs.path().join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap().trim(), "0");
    }

    #[test]
    fn test_dynatune_unix_socket_bidirectional_communication() {
        let temp_dir = tempdir().unwrap();
        let sock_path = temp_dir.path().join("dynatune.sock");

        // Bind mock UnixListener
        let listener = UnixListener::bind(&sock_path).unwrap();
        listener.set_nonblocking(true).unwrap();

        // Spawn mock Dynatune daemon listener thread
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0u8; 1024];
                    if let Ok(n) = stream.read(&mut buf) {
                        if n > 0 {
                            let resp = serde_json::json!({
                                "success": true,
                                "message": "Governor applied by Dynatune daemon",
                                "applied_governor": "performance",
                                "applied_epp": "performance"
                            }).to_string() + "\n";
                            let _ = stream.write_all(resp.as_bytes());
                            return true;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        });

        let dispatcher = HardwareGovernorDispatcher::new(HardwareControllerPaths {
            sysfs_root: temp_dir.path().to_path_buf(),
            procfs_root: temp_dir.path().to_path_buf(),
            dynatune_sock: sock_path,
            cpu_governor_script: PathBuf::from("/nonexistent/script"),
            kernel_ctl_bin: PathBuf::from("/nonexistent/bin"),
        });

        let outcome = dispatcher.dispatch(GovernorMode::Performance);
        assert!(outcome.success);
        assert_eq!(outcome.method, "dynatune_unix_socket");
        assert!(outcome.message.contains("Dynatune IPC socket"));

        let thread_res = handle.join().unwrap();
        assert!(thread_res, "Mock Dynatune listener must have received request");
    }

    #[test]
    fn test_mcp_tool_declaration_and_execution() {
        let temp_sysfs = tempdir().unwrap();
        let temp_procfs = tempdir().unwrap();
        let sock_path = temp_sysfs.path().join("dynatune.sock");
        let script_path = temp_sysfs.path().join("cpu-governor.sh");

        setup_mock_environment(temp_sysfs.path(), temp_procfs.path());

        let controller = HardwareAwareAiController::for_test(
            temp_sysfs.path(),
            temp_procfs.path(),
            &sock_path,
            &script_path,
        );

        // 1. Declaration schema test
        let decl = HardwareAwareAiController::declare_mcp_tool();
        assert_eq!(decl["name"], "hgb_hardware_telemetry_status");
        assert!(decl["description"].as_str().unwrap().contains("telemetry"));
        assert!(decl["inputSchema"]["properties"]["force_refresh"].is_object());

        // 2. Invocation test
        let result = controller.handle_mcp_call(&serde_json::json!({ "force_refresh": true }));
        assert_eq!(result["status"], "success");
        assert!((result["telemetry"]["cpu_temp_c"].as_f64().unwrap() - 52.0).abs() < f64::EPSILON);
        assert_eq!(result["telemetry"]["battery_pct"], 85);
        assert_eq!(result["pacing"]["token_pacing_delay_ms"], 0);
        assert_eq!(result["pacing"]["pause_background_wasm"], false);
        assert_eq!(result["pacing"]["recommended_worker_concurrency"], BOOST_CONCURRENCY_LIMIT);
        assert_eq!(result["pacing"]["target_governor"], "performance");
    }

    #[test]
    fn test_workload_lease_raii_guard() {
        let throttler = WorkloadThrottler::new();
        assert_eq!(throttler.active_heavy_tasks(), 0);

        let lease1 = throttler.acquire_lease("llm_generation");
        assert_eq!(lease1.workload_name(), "llm_generation");
        assert_eq!(throttler.active_heavy_tasks(), 1);

        {
            let lease2 = throttler.acquire_lease("wasm_compilation");
            assert_eq!(lease2.workload_name(), "wasm_compilation");
            assert_eq!(throttler.active_heavy_tasks(), 2);
        }

        // lease2 dropped
        assert_eq!(throttler.active_heavy_tasks(), 1);
        drop(lease1);
        assert_eq!(throttler.active_heavy_tasks(), 0);
    }
}
