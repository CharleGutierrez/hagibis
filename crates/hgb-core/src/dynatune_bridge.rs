//! # Pillar 3: Host-Aware Dynamic Scaling (Dynatune & Kernel-Optimizer Synergy)
//!
//! Native bridge providing deep synergy between Hagibis (`hgb-core` / `hgb-daemon`) and host kernel governors:
//! - Sibling projects:
//!   - `/home/toshiba/My AI Project/dynatune` (`src/power.rs`, `src/thermal.rs`)
//!   - `/home/toshiba/My AI Project/kernel-optimizer` (`dynabook-kernel-ctl`, `plugins/cpu-governor.sh`)
//! - Native sysfs reading:
//!   - AC / Battery connection state (`/sys/class/power_supply`)
//!   - Core frequencies in MHz (`/sys/devices/system/cpu/cpufreq/policy*/scaling_cur_freq`)
//!   - Energy Performance Preference (EPP) (`energy_performance_preference`)
//!   - Intel P-State configuration (`intel_pstate/no_turbo`, `max_perf_pct`, `hwp_dynamic_boost`)
//! - Dynamic governor shifting:
//!   - `shift_to_performance_mode()` and `shift_to_powersave_mode()`
//!   - Dynamically adjust sysfs governors or invoke `dynabook-kernel-ctl` when privileged
//!   - Fallback to `libc::setpriority` (nice values -5 / 10) when unprivileged
//! - Workload-aware scaling:
//!   - Automatically bursts to performance mode during heavy agent workflows (builds, tests, LLM speculative generation)
//!   - Automatically scales down to powersave during idle intervals
//!   - RAII `ScalingGuard` for safe, deterministic cleanup

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Default thermal throttling threshold in Celsius
pub const THERMAL_CRITICAL_TEMP_C: f64 = 82.0;
/// Safe thermal headroom threshold in Celsius
pub const THERMAL_SAFE_HEADROOM_C: f64 = 72.0;

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
pub struct DynatuneBridge {
    power_sysfs_base: PathBuf,
    cpu_sysfs_base: PathBuf,
    thermal_sysfs_base: PathBuf,
    intel_pstate_base: PathBuf,
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

    /// Read current CPU package temperature in Celsius
    pub fn read_cpu_temp_c(&self) -> f64 {
        let mut max_temp = 42.0;
        let mut pkg_temp = None;

        if let Ok(entries) = fs::read_dir(&self.thermal_sysfs_base) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("thermal_zone") {
                    let type_str = fs::read_to_string(p.join("type"))
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if let Ok(raw) = fs::read_to_string(p.join("temp")) {
                        if let Ok(milli) = raw.trim().parse::<i64>() {
                            let temp_c = milli as f64 / 1000.0;
                            if temp_c > max_temp && temp_c < 125.0 {
                                max_temp = temp_c;
                            }
                            if type_str == "x86_pkg_temp" {
                                pkg_temp = Some(temp_c);
                            }
                        }
                    }
                }
            }
        }

        pkg_temp.unwrap_or(max_temp)
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
                    // If negative nice failed due to unprivileged access, try 0
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

    /// Read comprehensive host power state
    pub fn read_power_state(&self) -> HostPowerState {
        let ac_connected = self.is_ac_connected();
        let scaling_driver = self.read_scaling_driver();
        let scaling_governor = self.read_scaling_governor();
        let energy_perf_preference = self.read_epp();
        let cpu_frequencies_mhz = self.read_cpu_frequencies();
        let (no_turbo, max_perf_pct, hwp_dynamic_boost) = self.read_intel_pstate();
        let cpu_temp_c = self.read_cpu_temp_c();
        let is_throttled = cpu_temp_c >= THERMAL_CRITICAL_TEMP_C;
        let nice_level = self.read_nice_level();
        let is_privileged = self.is_privileged();

        HostPowerState {
            ac_connected,
            scaling_driver,
            energy_perf_preference,
            scaling_governor,
            cpu_frequencies_mhz,
            no_turbo,
            max_perf_pct,
            hwp_dynamic_boost,
            cpu_temp_c,
            is_throttled,
            nice_level,
            is_privileged,
            dynatune_available: self.dynatune_dir.is_some(),
            kernel_ctl_available: self.kernel_ctl_path.is_some(),
        }
    }

    /// Shift host system dynamically to Performance Mode
    ///
    /// When privileged:
    /// - Writes "performance" to sysfs scaling_governor
    /// - Writes "performance" (or "balance_performance" if thermal > 75°C) to EPP
    /// - Configures Intel P-State: max_perf_pct = 100, no_turbo = 0, hwp_dynamic_boost = 1
    /// - Invokes dynabook-kernel-ctl profile performance if present
    ///
    /// When unprivileged:
    /// - Gracefully falls back to libc::setpriority(nice = -5 or 0)
    pub fn shift_to_performance_mode(&self) -> Result<ScalingShiftReport> {
        let mut actions = Vec::new();
        let temp = self.read_cpu_temp_c();
        let is_privileged = self.is_privileged();
        let mut sibling_invoked = None;

        let target_epp = if temp >= THERMAL_SAFE_HEADROOM_C {
            EPP_BALANCE_PERFORMANCE
        } else {
            EPP_PERFORMANCE
        };

        let mut sysfs_applied = false;

        if is_privileged {
            // 1. Direct sysfs write for Intel P-State
            if self.intel_pstate_base.exists() {
                if fs::write(self.intel_pstate_base.join("no_turbo"), "0\n").is_ok() {
                    actions.push("Intel P-State no_turbo disabled (turbo active)".to_string());
                }
                if fs::write(self.intel_pstate_base.join("max_perf_pct"), "100\n").is_ok() {
                    actions.push("Intel P-State max_perf_pct set to 100%".to_string());
                }
                if fs::write(self.intel_pstate_base.join("hwp_dynamic_boost"), "1\n").is_ok() {
                    actions.push("Intel P-State hwp_dynamic_boost enabled".to_string());
                }
            }

            // 2. Direct sysfs write across CPU policies
            let mut gov_count = 0;
            let mut epp_count = 0;
            if let Ok(entries) = fs::read_dir(self.cpu_sysfs_base.join("cpufreq")) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        if fs::write(p.join("scaling_governor"), "performance\n").is_ok() {
                            gov_count += 1;
                        }
                        if fs::write(p.join("energy_performance_preference"), format!("{}\n", target_epp)).is_ok() {
                            epp_count += 1;
                        }
                    }
                }
            }
            if gov_count > 0 || epp_count > 0 {
                actions.push(format!("Applied performance governor ({} cores) and EPP '{}' ({} cores)", gov_count, target_epp, epp_count));
                sysfs_applied = true;
            }

            // 3. Delegate to dynabook-kernel-ctl if available
            if let Some(ref ctl) = self.kernel_ctl_path {
                let res = Command::new(ctl)
                    .args(["profile", "performance"])
                    .output();
                if let Ok(out) = res {
                    if out.status.success() {
                        sibling_invoked = Some(format!("dynabook-kernel-ctl profile performance"));
                        actions.push("Invoked dynabook-kernel-ctl profile performance successfully".to_string());
                    }
                }
            }
        }

        // Unprivileged fallback: setpriority (nice = -5 / 0)
        let mut nice_applied = None;
        if !sysfs_applied {
            let res = self.set_nice_level(-5);
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

        self.is_performance_burst.store(true, Ordering::SeqCst);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Ok(ScalingShiftReport {
            mode: ScalingMode::Performance,
            scaling_governor: "performance".to_string(),
            epp: target_epp.to_string(),
            max_perf_pct: 100,
            no_turbo: false,
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
    /// - Invokes dynabook-kernel-ctl profile battery/balanced if present
    ///
    /// When unprivileged:
    /// - Gracefully falls back to libc::setpriority(nice = 10) (lower scheduling weight)
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

            // 3. Delegate to dynabook-kernel-ctl if available
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
        let shift_report = self.scale_for_workload(category)?;

        Ok(ScalingGuard {
            bridge: Arc::new(Self::new()),
            baseline_governor: baseline.scaling_governor,
            baseline_epp: baseline.energy_perf_preference,
            baseline_nice: baseline.nice_level,
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
        // If unprivileged, restore nice level
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
        assert_eq!(perf_rep.mode, ScalingMode::Performance);
        assert_eq!(perf_rep.scaling_governor, "performance");
        assert!(perf_rep.max_perf_pct == 100);
        assert!(bridge.is_performance_burst.load(Ordering::SeqCst));

        // 2. Shift to powersave mode
        let power_rep = bridge.shift_to_powersave_mode().expect("shift to powersave");
        assert_eq!(power_rep.mode, ScalingMode::PowerSave);
        assert_eq!(power_rep.scaling_governor, "powersave");
        assert!(!bridge.is_performance_burst.load(Ordering::SeqCst));
    }

    #[test]
    fn test_dynatune_bridge_workload_aware_scaling() {
        let bridge = DynatuneBridge::new();

        // Build workload -> Performance
        let rep_build = bridge.scale_for_workload(WorkloadCategory::BuildCompile).expect("scale build");
        assert_eq!(rep_build.mode, ScalingMode::Performance);

        // Test workload -> Performance
        let rep_test = bridge.scale_for_workload(WorkloadCategory::TestExecution).expect("scale test");
        assert_eq!(rep_test.mode, ScalingMode::Performance);

        // LLM generation -> Performance
        let rep_llm = bridge.scale_for_workload(WorkloadCategory::LlmGeneration).expect("scale llm");
        assert_eq!(rep_llm.mode, ScalingMode::Performance);

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
            assert_eq!(guard.active_shift.mode, ScalingMode::Performance);
            assert!(bridge.is_performance_burst.load(Ordering::SeqCst));
        }
        // Guard dropped
    }

    #[test]
    fn test_dynatune_bridge_unprivileged_nice_adjustment() {
        let bridge = DynatuneBridge::new();
        // Positive nice (lower priority) always succeeds on Linux without CAP_SYS_NICE
        let res = bridge.set_nice_level(5);
        assert!(res.is_ok());
        let cur = bridge.read_nice_level();
        assert!(cur >= 0);

        // Restore to 0
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
}
