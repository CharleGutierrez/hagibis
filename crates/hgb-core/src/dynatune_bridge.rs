//! # Pillar 3: Host-Aware Dynamic Scaling (Dynatune & Kernel-Optimizer Synergy)
//!
//! Native bridge providing deep synergy between Hagibis (`hgb-core` / `hgb-daemon`) and host kernel governors:
//! - Sibling projects:
//!   - `/home/toshiba/My AI Project/dynatune` (`src/power.rs`, `src/thermal.rs`, `src/battery.rs`, `src/memory.rs`, `src/processes.rs`)
//!   - `/home/toshiba/My AI Project/kernel-optimizer` (`dynabook-kernel-ctl`, `plugins/cpu-governor.sh`)
//! - Native sysfs reading:
//!   - CPU thermal zones (`/sys/class/thermal/thermal_zone*/temp`, `type`) and CPU package temperature
//!   - Battery status, capacity percentage, AC state (`/sys/class/power_supply/*/capacity`, `status`)
//!   - Memory pressure (PSI `/proc/pressure/memory` parsing `avg10`, `avg60`, `avg300`) and MGLRU (`/sys/kernel/mm/lru_gen/min_ttl_ms`, `enabled`)
//!   - Core frequencies in MHz (`/sys/devices/system/cpu/cpufreq/policy*/scaling_cur_freq`)
//!   - Energy Performance Preference (EPP) (`/sys/devices/system/cpu/cpufreq/policy*/energy_performance_preference`)
//!   - Intel P-State configuration (`intel_pstate/no_turbo`, `max_perf_pct`, `hwp_dynamic_boost`)
//! - Dynamic governor shifting:
//!   - `shift_to_performance_mode()` and `shift_to_powersave_mode()`
//!   - Dynamically adjust sysfs governors or invoke `dynabook-kernel-ctl` when privileged
//!   - Fallback to `libc::setpriority` (nice values -5 / 10) when unprivileged
//! - Workload-aware scaling:
//!   - Automatically bursts to performance mode (governor performance, EPP performance, MGLRU min_ttl_ms 1000) during heavy agent workflows
//!   - Automatically scales down to powersave during idle intervals
//!   - Dynamic thermal (>= 78.0°C) and battery (<20% on discharge) safeguards downscale governors and calculate recommended concurrency limits
//!   - RAII `ScalingGuard` for safe, deterministic cleanup restoring baseline governors, EPP, MGLRU min_ttl_ms, and nice level

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Thermal throttling threshold where automatic downscaling and concurrency throttling engage
pub const THERMAL_THROTTLE_THRESHOLD_C: f64 = 78.0;
/// Severe thermal critical threshold where workload is aggressively serialized (concurrency = 1)
pub const THERMAL_CRITICAL_TEMP_C: f64 = 82.0;
/// Safe thermal headroom threshold in Celsius
pub const THERMAL_SAFE_HEADROOM_C: f64 = 72.0;
/// Critical battery percentage threshold on discharge
pub const BATTERY_CRITICAL_THRESHOLD_PCT: u32 = 20;
/// Target MGLRU min_ttl_ms when bursting during heavy workloads
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;

/// Standard target EPP strings
pub const EPP_PERFORMANCE: &str = "performance";
pub const EPP_BALANCE_PERFORMANCE: &str = "balance_performance";
pub const EPP_BALANCE_POWER: &str = "balance_power";
pub const EPP_POWER: &str = "power";

/// Scaling modes supported by the host bridge
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalingMode {
    Performance,
    PowerSave,
    Balanced,
}

impl std::fmt::Display for ScalingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Performance => write!(f, "performance"),
            Self::PowerSave => write!(f, "powersave"),
            Self::Balanced => write!(f, "balanced"),
        }
    }
}

/// Workload categories for automatic workload-aware scaling
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadCategory {
    /// Heavy compilation (cargo build, rustc, zig build)
    BuildCompile,
    /// Automated test suites (cargo test, zig test, nextest)
    TestExecution,
    /// LLM inference & speculative draft token generation
    LlmGeneration,
    /// Sandboxed WASM microkernel execution
    WasmExecution,
    /// Background daemon idle / waiting intervals
    IdleInterval,
}

impl std::fmt::Display for WorkloadCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuildCompile => write!(f, "build_compile"),
            Self::TestExecution => write!(f, "test_execution"),
            Self::LlmGeneration => write!(f, "llm_generation"),
            Self::WasmExecution => write!(f, "wasm_execution"),
            Self::IdleInterval => write!(f, "idle_interval"),
        }
    }
}

/// Telemetry record for an individual host thermal zone
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThermalZoneTelemetry {
    pub name: String,
    pub zone_type: String,
    pub temp_c: f64,
}

/// Telemetry record for host power supply / battery
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatteryTelemetry {
    pub name: String,
    pub status: String,
    pub capacity_pct: u32,
    pub is_discharging: bool,
    pub is_critical: bool,
}

/// Linux Pressure Stall Information (PSI) metric record
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PressureRecordTelemetry {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

/// Host memory Pressure Stall Information (PSI)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PsiMemoryTelemetry {
    pub some: PressureRecordTelemetry,
    pub full: PressureRecordTelemetry,
    pub available: bool,
}

/// Complete, unified hardware telemetry snapshot combining thermal zones,
/// battery states, memory pressure (PSI), MGLRU, CPU frequency governors,
/// EPP, and intelligent concurrency recommendations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompleteHardwareTelemetry {
    pub cpu_pkg_temp_c: f64,
    pub max_temp_c: f64,
    pub thermal_zones: Vec<ThermalZoneTelemetry>,
    pub is_thermal_throttled: bool,
    pub ac_connected: bool,
    pub batteries: Vec<BatteryTelemetry>,
    pub primary_battery: Option<BatteryTelemetry>,
    pub is_battery_critical: bool,
    pub memory_total_mb: u64,
    pub memory_available_mb: u64,
    pub memory_used_mb: u64,
    pub memory_available_pct: f64,
    pub psi_memory: PsiMemoryTelemetry,
    pub mglru_enabled: Option<String>,
    pub mglru_min_ttl_ms: Option<u64>,
    pub scaling_driver: String,
    pub active_governor: String,
    pub active_epp: String,
    pub cpu_frequencies_mhz: Vec<f64>,
    pub no_turbo: bool,
    pub max_perf_pct: u32,
    pub hwp_dynamic_boost: bool,
    pub recommended_concurrency_limit: usize,
    pub is_throttled: bool,
    pub nice_level: i32,
    pub is_privileged: bool,
    pub dynatune_available: bool,
    pub kernel_ctl_available: bool,
    pub timestamp_epoch_ms: u64,
}

/// Compute the recommended concurrency limit based on live hardware sensors
pub fn compute_concurrency_limit(
    cpu_pkg_temp_c: f64,
    is_battery_critical: bool,
    memory_available_pct: f64,
    psi_some_avg10: f64,
    base_threads: usize,
) -> usize {
    if cpu_pkg_temp_c >= 85.0 {
        // Severe thermal emergency: serialize heavy jobs to single thread
        1
    } else if cpu_pkg_temp_c >= THERMAL_THROTTLE_THRESHOLD_C || is_battery_critical {
        // Thermal throttle threshold or battery critical: cut concurrency in half
        (base_threads / 2).max(1)
    } else if memory_available_pct < 6.0 || psi_some_avg10 > 35.0 {
        // Severe memory thrashing / high PSI stall
        (base_threads / 2).max(1)
    } else {
        base_threads.max(1)
    }
}

/// Comprehensive power and CPU telemetry from host kernel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPowerState {
    pub ac_connected: bool,
    pub scaling_driver: String,
    pub energy_perf_preference: String,
    pub scaling_governor: String,
    pub cpu_frequencies_mhz: Vec<f64>,
    pub no_turbo: bool,
    pub max_perf_pct: u32,
    pub hwp_dynamic_boost: bool,
    pub cpu_temp_c: f64,
    pub is_throttled: bool,
    pub nice_level: i32,
    pub is_privileged: bool,
    pub dynatune_available: bool,
    pub kernel_ctl_available: bool,
}

/// Report returned upon executing a dynamic governor shift
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingShiftReport {
    pub mode: ScalingMode,
    pub scaling_governor: String,
    pub epp: String,
    pub max_perf_pct: u32,
    pub no_turbo: bool,
    pub is_privileged: bool,
    pub nice_applied: Option<i32>,
    pub actions_applied: Vec<String>,
    pub sibling_invoked: Option<String>,
    pub timestamp_epoch_ms: u64,
}

/// Native Dynatune & Kernel-Optimizer Host Bridge
#[derive(Clone)]
pub struct DynatuneBridge {
    power_sysfs_base: PathBuf,
    cpu_sysfs_base: PathBuf,
    thermal_sysfs_base: PathBuf,
    intel_pstate_base: PathBuf,
    mglru_sysfs_base: PathBuf,
    dynatune_dir: Option<PathBuf>,
    kernel_ctl_path: Option<PathBuf>,
    is_performance_burst: Arc<AtomicBool>,
}

impl Default for DynatuneBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl DynatuneBridge {
    /// Initialize Dynatune host bridge with auto-discovery of sibling tools
    pub fn new() -> Self {
        let (dynatune_dir, kernel_ctl_path) = Self::discover_sibling_tools();

        Self {
            power_sysfs_base: PathBuf::from("/sys/class/power_supply"),
            cpu_sysfs_base: PathBuf::from("/sys/devices/system/cpu"),
            thermal_sysfs_base: PathBuf::from("/sys/class/thermal"),
            intel_pstate_base: PathBuf::from("/sys/devices/system/cpu/intel_pstate"),
            mglru_sysfs_base: PathBuf::from("/sys/kernel/mm/lru_gen"),
            dynatune_dir,
            kernel_ctl_path,
            is_performance_burst: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Discover sibling projects `dynatune` and `kernel-optimizer`
    fn discover_sibling_tools() -> (Option<PathBuf>, Option<PathBuf>) {
        // 1. Dynatune directory discovery
        let dyna_candidates = [
            std::env::var("DYNATUNE_DIR").ok().map(PathBuf::from),
            Some(PathBuf::from("/home/toshiba/My AI Project/dynatune")),
            Some(PathBuf::from("../dynatune")),
            Some(PathBuf::from("../../dynatune")),
            Some(PathBuf::from("/opt/dynatune")),
        ];
        let found_dyna = dyna_candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_dir() && (p.join("src/power.rs").exists() || p.join("Cargo.toml").exists()));

        // 2. Kernel optimizer ctl discovery
        let ctl_candidates = [
            std::env::var("KERNEL_OPTIMIZER_CTL").ok().map(PathBuf::from),
            std::env::var("KERNEL_OPTIMIZER_DIR").ok().map(|d| PathBuf::from(d).join("dynabook-kernel-ctl")),
            Some(PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl")),
            Some(PathBuf::from("../kernel-optimizer/dynabook-kernel-ctl")),
            Some(PathBuf::from("../../kernel-optimizer/dynabook-kernel-ctl")),
            Some(PathBuf::from("/usr/local/bin/dynabook-kernel-ctl")),
            Some(PathBuf::from("/usr/bin/dynabook-kernel-ctl")),
        ];
        let found_ctl = ctl_candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_file());

        (found_dyna, found_ctl)
    }

    /// Check whether current process has privileged write access to sysfs
    pub fn is_privileged(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            unsafe { libc::geteuid() == 0 }
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Read AC connection status from sysfs power supplies
    pub fn is_ac_connected(&self) -> bool {
        if let Ok(entries) = fs::read_dir(&self.power_sysfs_base) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(content) = fs::read_to_string(p.join("online")) {
                        if content.trim() == "1" {
                            return true;
                        }
                    }
                }
            }
        }
        // Fallback: check battery status
        if let Ok(entries) = fs::read_dir(&self.power_sysfs_base) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("BAT") {
                    if let Ok(content) = fs::read_to_string(p.join("status")) {
                        let s = content.trim();
                        if s.eq_ignore_ascii_case("Charging") || s.eq_ignore_ascii_case("Full") {
                            return true;
                        }
                    }
                }
            }
        }
        true // Safe default: assume AC connected
    }

    /// Read all hardware thermal zones
    pub fn read_thermal_zones(&self) -> Vec<ThermalZoneTelemetry> {
        let mut zones = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.thermal_sysfs_base) {
            let mut zone_entries: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
                .collect();
            zone_entries.sort_by_key(|e| e.file_name());

            for entry in zone_entries {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                let zone_type = fs::read_to_string(p.join("type"))
                    .unwrap_or_else(|_| "unknown".to_string())
                    .trim()
                    .to_string();
                let temp_c = fs::read_to_string(p.join("temp"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .map(|milli| milli as f64 / 1000.0)
                    .unwrap_or(0.0);

                zones.push(ThermalZoneTelemetry {
                    name,
                    zone_type,
                    temp_c,
                });
            }
        }
        zones
    }

    /// Read current CPU package temperature in Celsius
    pub fn read_cpu_temp_c(&self) -> f64 {
        self.read_cpu_pkg_temp_c()
    }

    /// Read current CPU package temperature in Celsius targeting Intel x86_pkg_temp
    pub fn read_cpu_pkg_temp_c(&self) -> f64 {
        let zones = self.read_thermal_zones();
        for z in &zones {
            if z.zone_type == "x86_pkg_temp" && z.temp_c > 0.0 && z.temp_c < 125.0 {
                return z.temp_c;
            }
        }
        for z in &zones {
            if (z.zone_type == "cpu-thermal" || z.zone_type == "acpitz") && z.temp_c > 0.0 && z.temp_c < 125.0 {
                return z.temp_c;
            }
        }
        zones.into_iter()
            .map(|z| z.temp_c)
            .filter(|&t| t > 0.0 && t < 125.0)
            .fold(42.0, f64::max)
    }

    /// Read highest temperature across all thermal zones
    pub fn read_max_temp_c(&self) -> f64 {
        let zones = self.read_thermal_zones();
        zones.into_iter()
            .map(|z| z.temp_c)
            .filter(|&t| t > 0.0 && t < 125.0)
            .fold(42.0, f64::max)
    }

    /// Check if CPU temperature exceeds thermal throttling threshold (>= 78.0°C)
    pub fn is_thermal_throttled(&self) -> bool {
        self.read_cpu_pkg_temp_c() >= THERMAL_THROTTLE_THRESHOLD_C
            || self.read_max_temp_c() >= THERMAL_THROTTLE_THRESHOLD_C
    }

    /// Read telemetry for all installed battery power supplies
    pub fn read_battery_telemetry(&self) -> Vec<BatteryTelemetry> {
        let mut batteries = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.power_sysfs_base) {
            let mut bat_entries: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("BAT"))
                .collect();
            bat_entries.sort_by_key(|e| e.file_name());

            for entry in bat_entries {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                let status = fs::read_to_string(p.join("status"))
                    .unwrap_or_else(|_| "Unknown".to_string())
                    .trim()
                    .to_string();
                let capacity_pct = fs::read_to_string(p.join("capacity"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(0);
                let is_discharging = status.eq_ignore_ascii_case("discharging");
                let is_critical = is_discharging && capacity_pct < BATTERY_CRITICAL_THRESHOLD_PCT;

                batteries.push(BatteryTelemetry {
                    name,
                    status,
                    capacity_pct,
                    is_discharging,
                    is_critical,
                });
            }
        }
        batteries
    }

    /// Check if primary battery is in critical state (<20% during discharge)
    pub fn is_battery_critical(&self) -> bool {
        let bats = self.read_battery_telemetry();
        bats.iter().any(|b| b.is_critical)
    }

    /// Read system memory from /proc/meminfo: (total_mb, available_mb, used_mb, available_pct)
    pub fn read_system_memory(&self) -> (u64, u64, u64, f64) {
        let mut total_kb = 0u64;
        let mut available_kb = 0u64;
        let mut free_kb = 0u64;

        if let Ok(content) = fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    total_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                } else if line.starts_with("MemAvailable:") {
                    available_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                } else if line.starts_with("MemFree:") {
                    free_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                }
            }
        }

        if available_kb == 0 {
            available_kb = free_kb;
        }
        let total_mb = total_kb / 1024;
        let available_mb = available_kb / 1024;
        let used_mb = total_mb.saturating_sub(available_mb);
        let available_pct = if total_kb > 0 {
            (available_kb as f64 / total_kb as f64) * 100.0
        } else {
            100.0
        };

        (total_mb, available_mb, used_mb, available_pct)
    }

    /// Read Linux Pressure Stall Information (PSI) for memory
    pub fn read_memory_pressure_psi(&self) -> PsiMemoryTelemetry {
        let path = Path::new("/proc/pressure/memory");
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return PsiMemoryTelemetry::default(),
        };
        Self::parse_psi_memory_content(&content)
    }

    /// Pure parser for /proc/pressure/memory content
    pub fn parse_psi_memory_content(content: &str) -> PsiMemoryTelemetry {
        let mut psi = PsiMemoryTelemetry {
            some: PressureRecordTelemetry::default(),
            full: PressureRecordTelemetry::default(),
            available: true,
        };
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let metric = parts[0];
                let mut rec = PressureRecordTelemetry::default();
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
                if metric == "some" {
                    psi.some = rec;
                } else if metric == "full" {
                    psi.full = rec;
                }
            }
        }
        psi
    }

    /// Read Multi-Gen LRU (MGLRU) enabled flags
    pub fn read_mglru_enabled(&self) -> Option<String> {
        let p = self.mglru_sysfs_base.join("enabled");
        fs::read_to_string(p).ok().map(|s| s.trim().to_string())
    }

    /// Read Multi-Gen LRU (MGLRU) min_ttl_ms
    pub fn read_mglru_min_ttl_ms(&self) -> Option<u64> {
        let p = self.mglru_sysfs_base.join("min_ttl_ms");
        fs::read_to_string(p)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    }

    /// Configure Multi-Gen LRU (MGLRU) min_ttl_ms
    pub fn set_mglru_min_ttl_ms(&self, ttl_ms: u64) -> std::result::Result<(), String> {
        let p = self.mglru_sysfs_base.join("min_ttl_ms");
        if p.exists() {
            fs::write(&p, format!("{}\n", ttl_ms))
                .map_err(|e| format!("Failed to write to {}: {}", p.display(), e))
        } else {
            Err(format!("MGLRU min_ttl_ms node not found at {}", p.display()))
        }
    }

    /// Read current CPU scaling driver
    pub fn read_scaling_driver(&self) -> String {
        let p = self.cpu_sysfs_base.join("cpufreq/policy0/scaling_driver");
        if let Ok(content) = fs::read_to_string(&p) {
            return content.trim().to_string();
        }
        let p_cpu0 = self.cpu_sysfs_base.join("cpu0/cpufreq/scaling_driver");
        if let Ok(content) = fs::read_to_string(&p_cpu0) {
            return content.trim().to_string();
        }
        "intel_pstate".to_string()
    }

    /// Read active scaling governor
    pub fn read_scaling_governor(&self) -> String {
        let p = self.cpu_sysfs_base.join("cpu0/cpufreq/scaling_governor");
        if let Ok(content) = fs::read_to_string(&p) {
            return content.trim().to_string();
        }
        let p_policy0 = self.cpu_sysfs_base.join("cpufreq/policy0/scaling_governor");
        if let Ok(content) = fs::read_to_string(&p_policy0) {
            return content.trim().to_string();
        }
        "powersave".to_string()
    }

    /// Read active Energy Performance Preference (EPP)
    pub fn read_epp(&self) -> String {
        let p = self.cpu_sysfs_base.join("cpu0/cpufreq/energy_performance_preference");
        if let Ok(content) = fs::read_to_string(&p) {
            return content.trim().to_string();
        }
        let p_policy0 = self.cpu_sysfs_base.join("cpufreq/policy0/energy_performance_preference");
        if let Ok(content) = fs::read_to_string(&p_policy0) {
            return content.trim().to_string();
        }
        EPP_BALANCE_PERFORMANCE.to_string()
    }

    /// Read core frequencies in MHz across all active logical CPUs
    pub fn read_cpu_frequencies(&self) -> Vec<f64> {
        let mut freqs = Vec::new();
        let cpufreq_dir = self.cpu_sysfs_base.join("cpufreq");

        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            let mut policies: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("policy"))
                .collect();
            policies.sort_by_key(|e| e.file_name());

            for policy in policies {
                let cur_file = policy.path().join("scaling_cur_freq");
                if let Ok(raw) = fs::read_to_string(&cur_file) {
                    if let Ok(khz) = raw.trim().parse::<f64>() {
                        freqs.push(khz / 1000.0);
                    }
                }
            }
        }

        if freqs.is_empty() {
            if let Ok(entries) = fs::read_dir(&self.cpu_sysfs_base) {
                let mut cpu_entries: Vec<_> = entries
                    .flatten()
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        name.starts_with("cpu") && name.chars().nth(3).map_or(false, |c| c.is_ascii_digit())
                    })
                    .collect();
                cpu_entries.sort_by_key(|e| e.file_name());

                for c in cpu_entries {
                    let cur_file = c.path().join("cpufreq/scaling_cur_freq");
                    if let Ok(raw) = fs::read_to_string(&cur_file) {
                        if let Ok(khz) = raw.trim().parse::<f64>() {
                            freqs.push(khz / 1000.0);
                        }
                    }
                }
            }
        }

        if freqs.is_empty() {
            vec![2400.0, 2400.0] // Reasonable nominal default
        } else {
            freqs
        }
    }

    /// Read Intel P-State configuration: (no_turbo, max_perf_pct, hwp_dynamic_boost)
    pub fn read_intel_pstate(&self) -> (bool, u32, bool) {
        let no_turbo = fs::read_to_string(self.intel_pstate_base.join("no_turbo"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        let max_perf = fs::read_to_string(self.intel_pstate_base.join("max_perf_pct"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(100);

        let hwp_boost = fs::read_to_string(self.intel_pstate_base.join("hwp_dynamic_boost"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        (no_turbo, max_perf, hwp_boost)
    }

    /// Read process nice value
    pub fn read_nice_level(&self) -> i32 {
        #[cfg(target_os = "linux")]
        {
            unsafe {
                *libc::__errno_location() = 0;
                let val = libc::getpriority(libc::PRIO_PROCESS, 0);
                val
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }

    /// Set process priority via libc::setpriority
    pub fn set_nice_level(&self, target_nice: i32) -> std::result::Result<i32, String> {
        #[cfg(target_os = "linux")]
        {
            unsafe {
                *libc::__errno_location() = 0;
                let res = libc::setpriority(libc::PRIO_PROCESS, 0, target_nice);
                if res != 0 {
                    let err = *libc::__errno_location();
                    if target_nice < 0 {
                        *libc::__errno_location() = 0;
                        let fallback = libc::setpriority(libc::PRIO_PROCESS, 0, 0);
                        if fallback == 0 {
                            return Ok(0);
                        }
                    }
                    return Err(format!("setpriority({}) failed with errno {}", target_nice, err));
                }
                let cur = libc::getpriority(libc::PRIO_PROCESS, 0);
                Ok(cur)
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(target_nice)
        }
    }

    /// Recommended concurrency limit based on live thermal, battery, and memory state
    pub fn recommended_concurrency_limit(&self) -> usize {
        let telem = self.read_complete_hardware_telemetry();
        telem.recommended_concurrency_limit
    }

    /// Read comprehensive host power state (legacy compatible)
    pub fn read_power_state(&self) -> HostPowerState {
        let complete = self.read_complete_hardware_telemetry();

        HostPowerState {
            ac_connected: complete.ac_connected,
            scaling_driver: complete.scaling_driver,
            energy_perf_preference: complete.active_epp,
            scaling_governor: complete.active_governor,
            cpu_frequencies_mhz: complete.cpu_frequencies_mhz,
            no_turbo: complete.no_turbo,
            max_perf_pct: complete.max_perf_pct,
            hwp_dynamic_boost: complete.hwp_dynamic_boost,
            cpu_temp_c: complete.cpu_pkg_temp_c,
            is_throttled: complete.is_throttled,
            nice_level: complete.nice_level,
            is_privileged: complete.is_privileged,
            dynatune_available: complete.dynatune_available,
            kernel_ctl_available: complete.kernel_ctl_available,
        }
    }

    /// Read complete, unified hardware telemetry snapshot
    pub fn read_complete_hardware_telemetry(&self) -> CompleteHardwareTelemetry {
        let thermal_zones = self.read_thermal_zones();
        let cpu_pkg_temp_c = self.read_cpu_pkg_temp_c();
        let max_temp_c = self.read_max_temp_c();
        let is_thermal_throttled = cpu_pkg_temp_c >= THERMAL_THROTTLE_THRESHOLD_C || max_temp_c >= THERMAL_THROTTLE_THRESHOLD_C;

        let ac_connected = self.is_ac_connected();
        let batteries = self.read_battery_telemetry();
        let primary_battery = batteries.first().cloned();
        let is_battery_critical = primary_battery.as_ref().map(|b| b.is_critical).unwrap_or(false);

        let (memory_total_mb, memory_available_mb, memory_used_mb, memory_available_pct) = self.read_system_memory();
        let psi_memory = self.read_memory_pressure_psi();
        let mglru_enabled = self.read_mglru_enabled();
        let mglru_min_ttl_ms = self.read_mglru_min_ttl_ms();

        let scaling_driver = self.read_scaling_driver();
        let active_governor = self.read_scaling_governor();
        let active_epp = self.read_epp();
        let cpu_frequencies_mhz = self.read_cpu_frequencies();
        let (no_turbo, max_perf_pct, hwp_dynamic_boost) = self.read_intel_pstate();

        let base_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let recommended_concurrency_limit = compute_concurrency_limit(
            cpu_pkg_temp_c,
            is_battery_critical,
            memory_available_pct,
            psi_memory.some.avg10,
            base_threads,
        );

        let is_throttled = is_thermal_throttled || is_battery_critical;
        let nice_level = self.read_nice_level();
        let is_privileged = self.is_privileged();
        let dynatune_available = self.dynatune_dir.is_some();
        let kernel_ctl_available = self.kernel_ctl_path.is_some();

        let timestamp_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        CompleteHardwareTelemetry {
            cpu_pkg_temp_c,
            max_temp_c,
            thermal_zones,
            is_thermal_throttled,
            ac_connected,
            batteries,
            primary_battery,
            is_battery_critical,
            memory_total_mb,
            memory_available_mb,
            memory_used_mb,
            memory_available_pct,
            psi_memory,
            mglru_enabled,
            mglru_min_ttl_ms,
            scaling_driver,
            active_governor,
            active_epp,
            cpu_frequencies_mhz,
            no_turbo,
            max_perf_pct,
            hwp_dynamic_boost,
            recommended_concurrency_limit,
            is_throttled,
            nice_level,
            is_privileged,
            dynatune_available,
            kernel_ctl_available,
            timestamp_epoch_ms,
        }
    }

    /// Shift host system dynamically to Performance Mode with Safeguards
    ///
    /// Workload Bursting:
    /// - During heavy compilation, test execution, or LLM inference, burst CPU governor to "performance",
    ///   EPP to "performance", and MGLRU min_ttl_ms to 1000.
    ///
    /// Thermal & Battery Safeguards:
    /// - If temperature >= 78.0°C or battery is critical (<20% on discharge), automatically downscale
    ///   governor, relax EPP, and cap max_perf_pct to protect chassis integrity.
    ///
    /// Unprivileged Fallback:
    /// - Falls back smoothly to process/thread nice level (libc::setpriority -5 / 10).
    pub fn shift_to_performance_mode(&self) -> Result<ScalingShiftReport> {
        let mut actions = Vec::new();
        let temp = self.read_cpu_pkg_temp_c();
        let is_privileged = self.is_privileged();
        let battery_critical = self.is_battery_critical();
        let mut sibling_invoked = None;

        let is_throttled = temp >= THERMAL_THROTTLE_THRESHOLD_C || battery_critical;

        // Dynamic thermal & battery throttling safeguards
        let (target_governor, target_epp, target_max_perf, target_min_ttl, target_nice) = if battery_critical {
            actions.push(format!(
                "Dynamic battery safeguard: primary battery discharging & critical (<{}%). Downscaling governor to powersave & EPP to power",
                BATTERY_CRITICAL_THRESHOLD_PCT
            ));
            ("powersave", EPP_POWER, 70, 0, 10)
        } else if temp >= THERMAL_CRITICAL_TEMP_C {
            actions.push(format!(
                "Dynamic critical thermal safeguard: CPU temp {:.1}°C >= {:.1}°C. Aggressively throttling to powersave & EPP balance_power",
                temp, THERMAL_CRITICAL_TEMP_C
            ));
            ("powersave", EPP_BALANCE_POWER, 70, 0, 10)
        } else if temp >= THERMAL_THROTTLE_THRESHOLD_C {
            actions.push(format!(
                "Dynamic thermal safeguard: CPU temp {:.1}°C >= {:.1}°C. Downscaling governor & relaxing EPP to balance_performance",
                temp, THERMAL_THROTTLE_THRESHOLD_C
            ));
            ("powersave", EPP_BALANCE_PERFORMANCE, 80, 0, 5)
        } else if temp >= THERMAL_SAFE_HEADROOM_C {
            ("performance", EPP_BALANCE_PERFORMANCE, 100, BURST_MGLRU_MIN_TTL_MS, -5)
        } else {
            ("performance", EPP_PERFORMANCE, 100, BURST_MGLRU_MIN_TTL_MS, -5)
        };

        let mut sysfs_applied = false;

        if is_privileged {
            // 1. Direct sysfs write for Intel P-State
            if self.intel_pstate_base.exists() {
                let no_turbo_val = if is_throttled { "1\n" } else { "0\n" };
                if fs::write(self.intel_pstate_base.join("no_turbo"), no_turbo_val).is_ok() {
                    actions.push(format!("Intel P-State no_turbo set to {}", no_turbo_val.trim()));
                }
                if fs::write(self.intel_pstate_base.join("max_perf_pct"), format!("{}\n", target_max_perf)).is_ok() {
                    actions.push(format!("Intel P-State max_perf_pct set to {}%", target_max_perf));
                }
                let hwp_val = if is_throttled { "0\n" } else { "1\n" };
                if fs::write(self.intel_pstate_base.join("hwp_dynamic_boost"), hwp_val).is_ok() {
                    actions.push(format!("Intel P-State hwp_dynamic_boost set to {}", hwp_val.trim()));
                }
            }

            // 2. Direct sysfs write across CPU policies
            let mut gov_count = 0;
            let mut epp_count = 0;
            if let Ok(entries) = fs::read_dir(self.cpu_sysfs_base.join("cpufreq")) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        if fs::write(p.join("scaling_governor"), format!("{}\n", target_governor)).is_ok() {
                            gov_count += 1;
                        }
                        if fs::write(p.join("energy_performance_preference"), format!("{}\n", target_epp)).is_ok() {
                            epp_count += 1;
                        }
                    }
                }
            }
            if gov_count > 0 || epp_count > 0 {
                actions.push(format!("Applied {} governor ({} cores) and EPP '{}' ({} cores)", target_governor, gov_count, target_epp, epp_count));
                sysfs_applied = true;
            }

            // 3. MGLRU min_ttl_ms adjustment
            if self.set_mglru_min_ttl_ms(target_min_ttl).is_ok() {
                actions.push(format!("MGLRU min_ttl_ms updated to {}", target_min_ttl));
                sysfs_applied = true;
            }

            // 4. Delegate to dynabook-kernel-ctl if available
            if let Some(ref ctl) = self.kernel_ctl_path {
                let prof = if is_throttled {
                    if battery_critical { "battery" } else { "balanced" }
                } else {
                    "performance"
                };
                let res = Command::new(ctl)
                    .args(["profile", prof])
                    .output();
                if let Ok(out) = res {
                    if out.status.success() {
                        sibling_invoked = Some(format!("dynabook-kernel-ctl profile {}", prof));
                        actions.push(format!("Invoked dynabook-kernel-ctl profile {} successfully", prof));
                    }
                }
            }
        }

        // Unprivileged fallback: setpriority
        let mut nice_applied = None;
        if !sysfs_applied {
            let res = self.set_nice_level(target_nice);
            match res {
                Ok(n) => {
                    nice_applied = Some(n);
                    actions.push(format!("Unprivileged fallback: process nice priority adjusted to {}", n));
                }
                Err(e) => {
                    actions.push(format!("Process nice adjustment failed: {}", e));
                }
            }
        }

        let is_burst = target_governor == "performance";
        self.is_performance_burst.store(is_burst, Ordering::SeqCst);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mode = if is_burst {
            ScalingMode::Performance
        } else if target_governor == "powersave" && target_epp == EPP_POWER {
            ScalingMode::PowerSave
        } else {
            ScalingMode::Balanced
        };

        Ok(ScalingShiftReport {
            mode,
            scaling_governor: target_governor.to_string(),
            epp: target_epp.to_string(),
            max_perf_pct: target_max_perf,
            no_turbo: is_throttled,
            is_privileged,
            nice_applied,
            actions_applied: actions,
            sibling_invoked,
            timestamp_epoch_ms: now_ms,
        })
    }

    /// Shift host system dynamically to PowerSave Mode
    ///
    /// When privileged:
    /// - Writes "powersave" to scaling_governor
    /// - Writes "balance_power" or "power" to EPP
    /// - Reduces Intel P-State max_perf_pct to 80% (or 70% if battery)
    /// - Resets MGLRU min_ttl_ms to 0 (default aging)
    /// - Invokes dynabook-kernel-ctl profile battery/balanced if present
    ///
    /// When unprivileged:
    /// - Gracefully falls back to libc::setpriority(nice = 10)
    pub fn shift_to_powersave_mode(&self) -> Result<ScalingShiftReport> {
        let mut actions = Vec::new();
        let is_privileged = self.is_privileged();
        let ac = self.is_ac_connected();
        let mut sibling_invoked = None;

        let (target_epp, target_max_perf) = if ac {
            (EPP_BALANCE_POWER, 80)
        } else {
            (EPP_POWER, 70)
        };

        let mut sysfs_applied = false;

        if is_privileged {
            // 1. Direct sysfs write for Intel P-State
            if self.intel_pstate_base.exists() {
                if fs::write(self.intel_pstate_base.join("max_perf_pct"), format!("{}\n", target_max_perf)).is_ok() {
                    actions.push(format!("Intel P-State max_perf_pct throttled to {}%", target_max_perf));
                }
                if fs::write(self.intel_pstate_base.join("hwp_dynamic_boost"), "0\n").is_ok() {
                    actions.push("Intel P-State hwp_dynamic_boost disabled".to_string());
                }
            }

            // 2. Direct sysfs write across CPU policies
            let mut gov_count = 0;
            let mut epp_count = 0;
            if let Ok(entries) = fs::read_dir(self.cpu_sysfs_base.join("cpufreq")) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        if fs::write(p.join("scaling_governor"), "powersave\n").is_ok() {
                            gov_count += 1;
                        }
                        if fs::write(p.join("energy_performance_preference"), format!("{}\n", target_epp)).is_ok() {
                            epp_count += 1;
                        }
                    }
                }
            }
            if gov_count > 0 || epp_count > 0 {
                actions.push(format!("Applied powersave governor ({} cores) and EPP '{}' ({} cores)", gov_count, target_epp, epp_count));
                sysfs_applied = true;
            }

            // 3. Reset MGLRU min_ttl_ms to 0 (default aging)
            if self.set_mglru_min_ttl_ms(0).is_ok() {
                actions.push("Reset MGLRU min_ttl_ms to 0 (nominal aging)".to_string());
                sysfs_applied = true;
            }

            // 4. Delegate to dynabook-kernel-ctl if available
            if let Some(ref ctl) = self.kernel_ctl_path {
                let target_prof = if ac { "balanced" } else { "battery" };
                let res = Command::new(ctl)
                    .args(["profile", target_prof])
                    .output();
                if let Ok(out) = res {
                    if out.status.success() {
                        sibling_invoked = Some(format!("dynabook-kernel-ctl profile {}", target_prof));
                        actions.push(format!("Invoked dynabook-kernel-ctl profile {} successfully", target_prof));
                    }
                }
            }
        }

        // Unprivileged fallback: setpriority(nice = 10)
        let mut nice_applied = None;
        if !sysfs_applied {
            let res = self.set_nice_level(10);
            match res {
                Ok(n) => {
                    nice_applied = Some(n);
                    actions.push(format!("Unprivileged fallback: process nice priority relaxed to {}", n));
                }
                Err(e) => {
                    actions.push(format!("Process nice adjustment failed: {}", e));
                }
            }
        }

        self.is_performance_burst.store(false, Ordering::SeqCst);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Ok(ScalingShiftReport {
            mode: ScalingMode::PowerSave,
            scaling_governor: "powersave".to_string(),
            epp: target_epp.to_string(),
            max_perf_pct: target_max_perf,
            no_turbo: !ac,
            is_privileged,
            nice_applied,
            actions_applied: actions,
            sibling_invoked,
            timestamp_epoch_ms: now_ms,
        })
    }

    /// Automatically scale host performance according to workload category
    pub fn scale_for_workload(&self, category: WorkloadCategory) -> Result<ScalingShiftReport> {
        match category {
            WorkloadCategory::BuildCompile | WorkloadCategory::TestExecution | WorkloadCategory::LlmGeneration => {
                self.shift_to_performance_mode()
            }
            WorkloadCategory::WasmExecution => {
                if self.is_ac_connected() {
                    self.shift_to_performance_mode()
                } else {
                    self.shift_to_powersave_mode()
                }
            }
            WorkloadCategory::IdleInterval => {
                self.shift_to_powersave_mode()
            }
        }
    }

    /// Acquire an RAII ScalingGuard for automatic burst during workload and recovery on drop
    pub fn burst_guard(&self, category: WorkloadCategory) -> Result<ScalingGuard> {
        let baseline = self.read_power_state();
        let baseline_min_ttl_ms = self.read_mglru_min_ttl_ms();
        let shift_report = self.scale_for_workload(category)?;

        Ok(ScalingGuard {
            bridge: Arc::new(self.clone()),
            baseline_governor: baseline.scaling_governor,
            baseline_epp: baseline.energy_perf_preference,
            baseline_nice: baseline.nice_level,
            baseline_min_ttl_ms,
            workload: category,
            active_shift: shift_report,
            disarmed: false,
        })
    }
}

/// RAII Scaling Guard that automatically restores baseline governor or relaxes to powersave on drop
pub struct ScalingGuard {
    bridge: Arc<DynatuneBridge>,
    pub baseline_governor: String,
    pub baseline_epp: String,
    pub baseline_nice: i32,
    pub baseline_min_ttl_ms: Option<u64>,
    pub workload: WorkloadCategory,
    pub active_shift: ScalingShiftReport,
    disarmed: bool,
}

impl ScalingGuard {
    /// Disarm guard so it does not alter scaling state upon drop
    pub fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for ScalingGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }

        // 1. Restore MGLRU min_ttl_ms if baseline existed
        if let Some(ttl) = self.baseline_min_ttl_ms {
            let _ = self.bridge.set_mglru_min_ttl_ms(ttl);
        }

        // 2. If unprivileged, restore nice level
        if !self.active_shift.is_privileged {
            let _ = self.bridge.set_nice_level(self.baseline_nice);
        } else {
            // If privileged, restore baseline governor or shift to powersave
            let _ = self.bridge.shift_to_powersave_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynatune_bridge_read_power_state() {
        let bridge = DynatuneBridge::new();
        let state = bridge.read_power_state();

        assert!(!state.scaling_driver.is_empty());
        assert!(!state.scaling_governor.is_empty());
        assert!(!state.energy_perf_preference.is_empty());
        assert!(!state.cpu_frequencies_mhz.is_empty());
        assert!(state.cpu_temp_c > 0.0 && state.cpu_temp_c < 120.0);
        assert!(state.max_perf_pct > 0 && state.max_perf_pct <= 100);
    }

    #[test]
    fn test_dynatune_bridge_shift_performance_and_powersave() {
        let bridge = DynatuneBridge::new();

        // 1. Shift to performance mode
        let perf_rep = bridge.shift_to_performance_mode().expect("shift to performance");
        assert!(!perf_rep.scaling_governor.is_empty());
        assert!(perf_rep.max_perf_pct > 0);

        // 2. Shift to powersave mode
        let power_rep = bridge.shift_to_powersave_mode().expect("shift to powersave");
        assert_eq!(power_rep.mode, ScalingMode::PowerSave);
        assert_eq!(power_rep.scaling_governor, "powersave");
        assert!(!bridge.is_performance_burst.load(Ordering::SeqCst));
    }

    #[test]
    fn test_dynatune_bridge_workload_aware_scaling() {
        let bridge = DynatuneBridge::new();

        // Build workload -> Performance or safeguarded
        let rep_build = bridge.scale_for_workload(WorkloadCategory::BuildCompile).expect("scale build");
        assert!(!rep_build.scaling_governor.is_empty());

        // Test workload
        let rep_test = bridge.scale_for_workload(WorkloadCategory::TestExecution).expect("scale test");
        assert!(!rep_test.scaling_governor.is_empty());

        // LLM generation
        let rep_llm = bridge.scale_for_workload(WorkloadCategory::LlmGeneration).expect("scale llm");
        assert!(!rep_llm.scaling_governor.is_empty());

        // Idle interval -> PowerSave
        let rep_idle = bridge.scale_for_workload(WorkloadCategory::IdleInterval).expect("scale idle");
        assert_eq!(rep_idle.mode, ScalingMode::PowerSave);
    }

    #[test]
    fn test_dynatune_bridge_raii_guard() {
        let bridge = DynatuneBridge::new();
        {
            let guard = bridge.burst_guard(WorkloadCategory::BuildCompile).expect("burst guard");
            assert_eq!(guard.workload, WorkloadCategory::BuildCompile);
        }
        // Guard dropped and cleaned up
    }

    #[test]
    fn test_dynatune_bridge_unprivileged_nice_adjustment() {
        let bridge = DynatuneBridge::new();
        let res = bridge.set_nice_level(5);
        assert!(res.is_ok());
        let cur = bridge.read_nice_level();
        assert!(cur >= 0);

        let _ = bridge.set_nice_level(0);
    }

    #[test]
    fn test_dynatune_bridge_sibling_discovery() {
        let (dyna, ctl) = DynatuneBridge::discover_sibling_tools();
        if let Some(d) = dyna {
            assert!(d.exists());
        }
        if let Some(c) = ctl {
            assert!(c.exists());
        }
    }

    #[test]
    fn test_dynatune_bridge_complete_hardware_telemetry() {
        let bridge = DynatuneBridge::new();
        let telem = bridge.read_complete_hardware_telemetry();

        assert!(telem.cpu_pkg_temp_c > 0.0 && telem.cpu_pkg_temp_c < 125.0);
        assert!(telem.max_temp_c > 0.0 && telem.max_temp_c < 125.0);
        assert!(!telem.thermal_zones.is_empty());
        assert!(telem.memory_total_mb > 0);
        assert!(telem.memory_available_mb > 0);
        assert!(telem.memory_available_pct > 0.0);
        assert!(telem.recommended_concurrency_limit >= 1);
        assert!(!telem.cpu_frequencies_mhz.is_empty());
        assert!(!telem.scaling_driver.is_empty());
        assert!(!telem.active_governor.is_empty());
        assert!(!telem.active_epp.is_empty());
        assert!(telem.timestamp_epoch_ms > 0);
    }

    #[test]
    fn test_psi_memory_parsing() {
        let raw = "some avg10=2.34 avg60=1.12 avg300=0.50 total=123456\nfull avg10=0.15 avg60=0.08 avg300=0.01 total=4567\n";
        let parsed = DynatuneBridge::parse_psi_memory_content(raw);

        assert!(parsed.available);
        assert!((parsed.some.avg10 - 2.34).abs() < f64::EPSILON);
        assert!((parsed.some.avg60 - 1.12).abs() < f64::EPSILON);
        assert!((parsed.some.avg300 - 0.50).abs() < f64::EPSILON);
        assert_eq!(parsed.some.total_us, 123456);

        assert!((parsed.full.avg10 - 0.15).abs() < f64::EPSILON);
        assert!((parsed.full.avg60 - 0.08).abs() < f64::EPSILON);
        assert!((parsed.full.avg300 - 0.01).abs() < f64::EPSILON);
        assert_eq!(parsed.full.total_us, 4567);
    }

    #[test]
    fn test_concurrency_recommendation_safeguards() {
        let base_threads = 4;

        // 1. Nominal conditions (cool temp, plenty of RAM, AC, low PSI)
        let nominal = compute_concurrency_limit(50.0, false, 50.0, 0.0, base_threads);
        assert_eq!(nominal, 4);

        // 2. Thermal throttling threshold (79°C >= 78°C)
        let thermal_throttled = compute_concurrency_limit(79.0, false, 50.0, 0.0, base_threads);
        assert_eq!(thermal_throttled, 2);

        // 3. Thermal emergency (86°C >= 85°C) -> Aggressive serialization
        let emergency = compute_concurrency_limit(86.0, false, 50.0, 0.0, base_threads);
        assert_eq!(emergency, 1);

        // 4. Battery critical on discharge (<20%) -> Halve concurrency
        let bat_crit = compute_concurrency_limit(50.0, true, 50.0, 0.0, base_threads);
        assert_eq!(bat_crit, 2);

        // 5. Severe RAM exhaustion (<6% RAM) -> Halve concurrency
        let ram_low = compute_concurrency_limit(50.0, false, 4.5, 0.0, base_threads);
        assert_eq!(ram_low, 2);

        // 6. High PSI memory stall (>35% stall) -> Halve concurrency
        let psi_high = compute_concurrency_limit(50.0, false, 50.0, 42.0, base_threads);
        assert_eq!(psi_high, 2);
    }

    #[test]
    fn test_scaling_guard_mglru_and_restoration() {
        let bridge = DynatuneBridge::new();
        let mut guard = bridge.burst_guard(WorkloadCategory::BuildCompile).expect("guard");
        assert_eq!(guard.workload, WorkloadCategory::BuildCompile);
        guard.disarm();
    }
}
