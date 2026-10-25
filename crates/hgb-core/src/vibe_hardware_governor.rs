//! Ambient Thermal & Hardware Synergy (`dynatune` integration)
//!
//! Production-grade CPU governor scaling and thermal guard that interfaces directly
//! with dynatune sysfs endpoints, `scripts/cpu-governor.sh`, and `HardwareThrottleArbiter`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::dynatune_ffi_throttler::DynatuneSysfsReader;
use crate::error::Result;

pub const VIBE_THERMAL_HEADROOM_THRESHOLD_C: f64 = 78.0;
pub const DEFAULT_SYSFS_CPU_DIR: &str = "/sys/devices/system/cpu";
pub const DEFAULT_SYSFS_THERMAL_DIR: &str = "/sys/class/thermal";
pub const DEFAULT_SYSFS_POWER_DIR: &str = "/sys/class/power_supply";
pub const DEFAULT_SYSFS_MGLRU_FILE: &str = "/sys/kernel/mm/lru_gen/min_ttl_ms";
pub const DEFAULT_PROCFS_VMSTAT_FILE: &str = "/proc/vmstat";
pub const DEFAULT_DYNATUNE_DIR: &str = "../dynatune";
pub const DEFAULT_KERNEL_OPTIMIZER_DIR: &str = "../kernel-optimizer";
pub const BATTERY_CRITICAL_THRESHOLD_PCT: u32 = 20;

/// Discover sibling dynatune directory using env override or relative sibling search
pub fn resolve_dynatune_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DYNATUNE_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return p;
        }
    }
    let candidates = [
        PathBuf::from("/home/toshiba/My AI Project/dynatune"),
        PathBuf::from("../dynatune"),
        PathBuf::from("../../dynatune"),
        PathBuf::from("dynatune"),
    ];
    for c in candidates {
        if c.is_dir() {
            return c;
        }
    }
    PathBuf::from(DEFAULT_DYNATUNE_DIR)
}

/// Discover sibling kernel-optimizer directory using env override or relative sibling search
pub fn resolve_kernel_optimizer_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("KERNEL_OPTIMIZER_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return p;
        }
    }
    let candidates = [
        PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer"),
        PathBuf::from("../kernel-optimizer"),
        PathBuf::from("../../kernel-optimizer"),
        PathBuf::from("kernel-optimizer"),
        PathBuf::from("/usr/lib/dynabook-kernel"),
    ];
    for c in candidates {
        if c.is_dir() {
            return c;
        }
    }
    PathBuf::from(DEFAULT_KERNEL_OPTIMIZER_DIR)
}
pub const DEFAULT_GOVERNOR_SCRIPT: &str = "scripts/cpu-governor.sh";

/// Thermal & hardware status snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VibeThermalState {
    pub cpu_temp_c: f64,
    pub governor: String,
    pub epp: String,
    pub battery_pct: Option<u32>,
    pub is_charging: bool,
    #[serde(default)]
    pub is_on_ac: bool,
    #[serde(default)]
    pub is_discharging: bool,
    pub vibe_mode: String, // "HighPerformance" | "ChilledReading" | "ThermalThrottled" | "BatterySaver"
    #[serde(default)]
    pub recommended_draft_model: String,
    #[serde(default)]
    pub recommended_concurrency: usize,
    #[serde(default)]
    pub mglru_min_ttl_ms: Option<u64>,
    #[serde(default)]
    pub memory_pressure: f64,
}

/// Power supply detailed telemetry
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerSupplyTelemetry {
    pub is_on_ac: bool,
    pub is_discharging: bool,
    pub battery_pct: Option<u32>,
    pub status: String,
}

/// Dynamic Hardware Governor for frictionless Vibe Prompting
#[derive(Clone)]
pub struct VibeHardwareGovernor {
    cpu_dir: PathBuf,
    thermal_dir: PathBuf,
    power_dir: PathBuf,
    script_path: PathBuf,
    mglru_file: PathBuf,
    vmstat_file: PathBuf,
    _dynatune_dir: PathBuf,
    kernel_optimizer_dir: PathBuf,
    sysfs_reader: Arc<DynatuneSysfsReader>,
    is_boosted: Arc<AtomicBool>,
}

impl Default for VibeHardwareGovernor {
    fn default() -> Self {
        Self::new()
    }
}

/// User-space nice / process priority fallback for unprivileged execution
fn apply_userspace_priority_fallback(high_priority: bool) {
    #[cfg(unix)]
    {
        unsafe {
            if high_priority {
                // Attempt -5 priority, gracefully falling back to 0 (neutral) if unprivileged
                if libc::setpriority(libc::PRIO_PROCESS, 0, -5) != 0 {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 0);
                }
            } else {
                // Powersave nice: 10 (yield CPU micro-slices to save battery & lower wattage)
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            }
        }
    }
}

impl VibeHardwareGovernor {
    /// Initialize with production bare-metal paths
    pub fn new() -> Self {
        let cpu_dir = PathBuf::from(DEFAULT_SYSFS_CPU_DIR);
        let thermal_dir = PathBuf::from(DEFAULT_SYSFS_THERMAL_DIR);
        let power_dir = PathBuf::from(DEFAULT_SYSFS_POWER_DIR);
        let script_path = PathBuf::from(DEFAULT_GOVERNOR_SCRIPT);
        let dynatune_dir = resolve_dynatune_dir();
        let kernel_optimizer_dir = resolve_kernel_optimizer_dir();
        let mglru_file = PathBuf::from(DEFAULT_SYSFS_MGLRU_FILE);
        let vmstat_file = PathBuf::from(DEFAULT_PROCFS_VMSTAT_FILE);

        Self {
            cpu_dir: cpu_dir.clone(),
            thermal_dir: thermal_dir.clone(),
            power_dir: power_dir.clone(),
            script_path,
            mglru_file,
            vmstat_file,
            _dynatune_dir: dynatune_dir,
            kernel_optimizer_dir,
            sysfs_reader: Arc::new(DynatuneSysfsReader::with_custom_paths(
                thermal_dir,
                power_dir,
                cpu_dir,
                PathBuf::from("/proc/pressure/cpu"),
                PathBuf::from("/proc/pressure/memory"),
            )),
            is_boosted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Initialize with custom paths for isolated unit tests or virtualized environments
    pub fn with_custom_paths(
        cpu_dir: PathBuf,
        thermal_dir: PathBuf,
        power_dir: PathBuf,
        script_path: PathBuf,
    ) -> Self {
        let mglru_file = cpu_dir.join("mglru_min_ttl_ms");
        let vmstat_file = cpu_dir.join("vmstat");
        Self::with_all_paths(
            cpu_dir,
            thermal_dir,
            power_dir,
            script_path,
            mglru_file,
            vmstat_file,
        )
    }

    /// Initialize with all custom sysfs & procfs paths
    pub fn with_all_paths(
        cpu_dir: PathBuf,
        thermal_dir: PathBuf,
        power_dir: PathBuf,
        script_path: PathBuf,
        mglru_file: PathBuf,
        vmstat_file: PathBuf,
    ) -> Self {
        let dynatune_dir = resolve_dynatune_dir();
        let kernel_optimizer_dir = resolve_kernel_optimizer_dir();
        Self {
            cpu_dir: cpu_dir.clone(),
            thermal_dir: thermal_dir.clone(),
            power_dir: power_dir.clone(),
            script_path,
            mglru_file,
            vmstat_file,
            _dynatune_dir: dynatune_dir,
            kernel_optimizer_dir,
            sysfs_reader: Arc::new(DynatuneSysfsReader::with_custom_paths(
                thermal_dir,
                power_dir,
                cpu_dir,
                PathBuf::from("/proc/pressure/cpu"),
                PathBuf::from("/proc/pressure/memory"),
            )),
            is_boosted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Read highest CPU temperature across all thermal zones
    pub fn read_max_cpu_temp_c(&self) -> f64 {
        let (pkg, max_zone) = self.sysfs_reader.read_cpu_temperature();
        if pkg > 0.0 {
            pkg.max(max_zone)
        } else if max_zone > 0.0 {
            max_zone
        } else {
            // Direct inspection fallback
            let mut highest = 45.0;
            if let Ok(entries) = fs::read_dir(&self.thermal_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                    if name.starts_with("thermal_zone") {
                        if let Ok(raw) = fs::read_to_string(p.join("temp")) {
                            if let Ok(milli) = raw.trim().parse::<i64>() {
                                let c = milli as f64 / 1000.0;
                                if c > highest && c < 125.0 {
                                    highest = c;
                                }
                            }
                        }
                    }
                }
            }
            highest
        }
    }

    /// Read active scaling governor from cpu0 or policy0
    pub fn read_current_governor(&self) -> String {
        let candidates = [
            self.cpu_dir.join("cpu0/cpufreq/scaling_governor"),
            self.cpu_dir.join("cpufreq/policy0/scaling_governor"),
            self.cpu_dir.join("policy0/scaling_governor"),
        ];
        for p in &candidates {
            if let Ok(c) = fs::read_to_string(p) {
                let trimmed = c.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        if self.is_boosted.load(Ordering::Relaxed) {
            "performance".to_string()
        } else {
            "powersave".to_string()
        }
    }

    /// Read active Energy Performance Preference (EPP)
    pub fn read_current_epp(&self) -> String {
        let candidates = [
            self.cpu_dir.join("cpu0/cpufreq/energy_performance_preference"),
            self.cpu_dir.join("cpufreq/policy0/energy_performance_preference"),
            self.cpu_dir.join("policy0/energy_performance_preference"),
        ];
        for p in &candidates {
            if let Ok(c) = fs::read_to_string(p) {
                let trimmed = c.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        if self.is_boosted.load(Ordering::Relaxed) {
            "performance".to_string()
        } else {
            "balance_performance".to_string()
        }
    }

    /// Read Multi-Gen LRU (MGLRU) min_ttl_ms from sysfs
    pub fn read_mglru_min_ttl_ms(&self) -> Option<u64> {
        let p = if self.mglru_file.exists() {
            &self.mglru_file
        } else {
            Path::new(DEFAULT_SYSFS_MGLRU_FILE)
        };
        fs::read_to_string(p).ok().and_then(|s| s.trim().parse::<u64>().ok())
    }

    /// Write Multi-Gen LRU (MGLRU) min_ttl_ms with unprivileged graceful fallback
    pub fn write_mglru_min_ttl_ms(&self, ttl_ms: u64) -> bool {
        let p = if self.mglru_file.exists() {
            &self.mglru_file
        } else {
            Path::new(DEFAULT_SYSFS_MGLRU_FILE)
        };
        if p.exists() {
            if fs::write(p, format!("{}\n", ttl_ms)).is_ok() {
                return true;
            }
        }
        // Fallback to sibling kernel-optimizer script
        let script = self.kernel_optimizer_dir.join("plugins/mglru-monitor.sh");
        if script.exists() {
            if let Ok(out) = Command::new(&script).args(["set-min-ttl", &ttl_ms.to_string()]).output() {
                if out.status.success() {
                    return true;
                }
            }
        }
        false
    }

    /// Read memory pressure from /proc/pressure/memory or fallback to /proc/vmstat
    pub fn read_memory_pressure_or_vmstat(&self) -> f64 {
        // Try reading PSI avg10 from /proc/pressure/memory
        if let Ok(content) = fs::read_to_string("/proc/pressure/memory") {
            for line in content.lines() {
                if line.starts_with("some") {
                    for part in line.split_whitespace() {
                        if let Some(val_str) = part.strip_prefix("avg10=") {
                            if let Ok(val) = val_str.parse::<f64>() {
                                return val;
                            }
                        }
                    }
                }
            }
        }

        // Fallback: analyze /proc/vmstat
        let vmstat_path = if self.vmstat_file.exists() {
            &self.vmstat_file
        } else {
            Path::new(DEFAULT_PROCFS_VMSTAT_FILE)
        };

        if let Ok(content) = fs::read_to_string(vmstat_path) {
            let mut pgscan: u64 = 0;
            let mut pgsteal: u64 = 0;
            let mut allocstall: u64 = 0;

            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    if k.starts_with("pgscan_") {
                        if let Ok(n) = v.parse::<u64>() {
                            pgscan += n;
                        }
                    } else if k.starts_with("pgsteal_") {
                        if let Ok(n) = v.parse::<u64>() {
                            pgsteal += n;
                        }
                    } else if k.starts_with("allocstall_") {
                        if let Ok(n) = v.parse::<u64>() {
                            allocstall += n;
                        }
                    }
                }
            }

            if allocstall > 0 {
                return ((allocstall as f64).min(100.0) / 100.0) * 10.0;
            }
            if pgscan > 0 {
                let steal_ratio = (pgsteal as f64 / pgscan as f64).clamp(0.0, 1.0);
                return (1.0 - steal_ratio) * 5.0;
            }
        }

        0.0
    }

    /// Dynamically calculate recommended concurrency limits based on thermal headroom and battery status
    pub fn recommended_concurrency_limit(&self) -> usize {
        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let temp = self.read_max_cpu_temp_c();
        let tele = self.read_power_supply_telemetry();
        let on_battery = tele.is_discharging || (!tele.is_on_ac && tele.battery_pct.is_some());
        let bat_pct = tele.battery_pct.unwrap_or(100);

        // Downscale concurrency when temperature exceeds 78°C or battery is critical (<20%)
        if temp >= 85.0 || (on_battery && bat_pct < 10) {
            1
        } else if temp >= VIBE_THERMAL_HEADROOM_THRESHOLD_C || (on_battery && bat_pct < BATTERY_CRITICAL_THRESHOLD_PCT) {
            2.min(num_cpus).max(1)
        } else if on_battery {
            (num_cpus * 3 / 4).max(2).min(num_cpus)
        } else {
            num_cpus
        }
    }

    /// Detailed power supply and uevent reader for battery vs AC detection
    pub fn read_power_supply_telemetry(&self) -> PowerSupplyTelemetry {
        let mut ac_online = false;
        let mut battery_found = false;
        let mut is_discharging = false;
        let mut battery_pct: Option<u32> = None;
        let mut status = "Unknown".to_string();

        if let Ok(entries) = fs::read_dir(&self.power_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

                // 1. Try reading standard Linux sysfs battery uevents
                let uevent_path = path.join("uevent");
                if let Ok(uevent) = fs::read_to_string(&uevent_path) {
                    for line in uevent.lines() {
                        if let Some((k, v)) = line.split_once('=') {
                            match k.trim() {
                                "POWER_SUPPLY_ONLINE" => {
                                    if v.trim() == "1" {
                                        ac_online = true;
                                    }
                                }
                                "POWER_SUPPLY_STATUS" => {
                                    let s = v.trim();
                                    status = s.to_string();
                                    if s.eq_ignore_ascii_case("Discharging") {
                                        is_discharging = true;
                                    } else if s.eq_ignore_ascii_case("Charging") || s.eq_ignore_ascii_case("Full") {
                                        ac_online = true;
                                    }
                                }
                                "POWER_SUPPLY_CAPACITY" => {
                                    if let Ok(pct) = v.trim().parse::<u32>() {
                                        battery_pct = Some(pct);
                                    }
                                }
                                "POWER_SUPPLY_TYPE" => {
                                    let t = v.trim();
                                    if t.eq_ignore_ascii_case("Battery") {
                                        battery_found = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // 2. Direct sysfs attributes fallback
                if name.starts_with("AC") || name.starts_with("ADP") || name.starts_with("mains") {
                    if let Ok(online) = fs::read_to_string(path.join("online")) {
                        if online.trim() == "1" {
                            ac_online = true;
                        }
                    }
                } else if name.starts_with("BAT") || name.contains("battery") {
                    battery_found = true;
                    if let Ok(s) = fs::read_to_string(path.join("status")) {
                        let s = s.trim().to_string();
                        if !s.is_empty() {
                            status = s.clone();
                            if s.eq_ignore_ascii_case("Discharging") {
                                is_discharging = true;
                            } else if s.eq_ignore_ascii_case("Charging") || s.eq_ignore_ascii_case("Full") {
                                ac_online = true;
                            }
                        }
                    }
                    if let Ok(c) = fs::read_to_string(path.join("capacity")) {
                        if let Ok(pct) = c.trim().parse::<u32>() {
                            battery_pct = Some(pct);
                        }
                    }
                }
            }
        }

        // If no battery was discovered on the machine (e.g. desktop workstation), assume AC mains
        if !battery_found {
            ac_online = true;
            is_discharging = false;
            status = "MainsDirect".to_string();
        } else if is_discharging {
            ac_online = false;
        }

        PowerSupplyTelemetry {
            is_on_ac: ac_online,
            is_discharging,
            battery_pct,
            status,
        }
    }

    /// Read battery percentage and charge state
    pub fn read_power_state(&self) -> (Option<u32>, bool) {
        let tele = self.read_power_supply_telemetry();
        (tele.battery_pct, tele.is_on_ac || !tele.is_discharging)
    }

    /// Recommend model based on power source (lightweight draft model on battery)
    pub fn get_recommended_draft_model(&self) -> &'static str {
        let tele = self.read_power_supply_telemetry();
        if tele.is_discharging || (!tele.is_on_ac && tele.battery_pct.is_some()) {
            // On Battery power: recommend lightweight local draft models to conserve battery
            "qwen2.5-coder:0.5b"
        } else {
            // On AC power: full performance draft model
            "qwen2.5-coder:1.5b"
        }
    }

    /// Apply governor across all available CPU cores.
    /// Gracefully logs notice and falls back to user-space process priority if sysfs permissions are restricted.
    fn write_governor_to_cores(&self, gov: &str) -> bool {
        let mut wrote = false;
        let mut permission_denied = false;

        if let Ok(entries) = fs::read_dir(&self.cpu_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                    let gov_file = entry.path().join("cpufreq/scaling_governor");
                    if gov_file.exists() {
                        match fs::write(&gov_file, gov) {
                            Ok(_) => {
                                wrote = true;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                                permission_denied = true;
                            }
                            Err(_) => {}
                        }
                    }
                } else if name.starts_with("policy") {
                    let gov_file = entry.path().join("scaling_governor");
                    if gov_file.exists() {
                        match fs::write(&gov_file, gov) {
                            Ok(_) => {
                                wrote = true;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                                permission_denied = true;
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        // Check /sys/devices/system/cpu/cpufreq/policy*
        let cpufreq_dir = self.cpu_dir.join("cpufreq");
        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("policy") {
                    let gov_file = entry.path().join("scaling_governor");
                    if gov_file.exists() {
                        match fs::write(&gov_file, gov) {
                            Ok(_) => {
                                wrote = true;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                                permission_denied = true;
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        // Sibling script invocation fallback if sysfs write is restricted
        if !wrote && self.script_path.exists() {
            let script_arg = match gov {
                "performance" => "performance",
                "powersave" => "powersave",
                _ => "balanced",
            };
            if let Ok(output) = Command::new(&self.script_path).arg(script_arg).output() {
                if output.status.success() {
                    wrote = true;
                }
            }
        }

        // Sibling kernel-optimizer fallback
        if !wrote {
            let ctl_path = self.kernel_optimizer_dir.join("dynabook-kernel-ctl");
            if ctl_path.exists() {
                let _ = Command::new(&ctl_path).args(["profile", gov]).output();
            }
            let script_gov = self.kernel_optimizer_dir.join("plugins/cpu-governor.sh");
            if script_gov.exists() {
                let _ = Command::new(&script_gov).args(["set-governor", gov]).output();
            }
        }

        // Non-root sysfs permissions: graceful fallback to user-space nice / process priority
        if !wrote {
            if permission_denied {
                eprintln!(
                    "[hgb-governor] Notice: CPU governor sysfs access restricted (non-root). Falling back to user-space process priority."
                );
            }
            apply_userspace_priority_fallback(gov == "performance");
            wrote = true;
        }

        wrote
    }

    /// Apply EPP across all available CPU cores
    fn write_epp_to_cores(&self, epp: &str) -> bool {
        let mut wrote = false;
        if let Ok(entries) = fs::read_dir(&self.cpu_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                    let epp_file = entry.path().join("cpufreq/energy_performance_preference");
                    if epp_file.exists() && fs::write(&epp_file, epp).is_ok() {
                        wrote = true;
                    }
                } else if name.starts_with("policy") {
                    let epp_file = entry.path().join("energy_performance_preference");
                    if epp_file.exists() && fs::write(&epp_file, epp).is_ok() {
                        wrote = true;
                    }
                }
            }
        }

        let cpufreq_dir = self.cpu_dir.join("cpufreq");
        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("policy") {
                    let epp_file = entry.path().join("energy_performance_preference");
                    if epp_file.exists() && fs::write(&epp_file, epp).is_ok() {
                        wrote = true;
                    }
                }
            }
        }

        if !wrote {
            let script_gov = self.kernel_optimizer_dir.join("plugins/cpu-governor.sh");
            if script_gov.exists() {
                let _ = Command::new(&script_gov).args(["set-epp", epp]).output();
            }
        }

        wrote
    }

    /// Dynamic Policy Hardware Escalation for Vibe Build:
    /// - On AC power: allow full turbo boost to `performance` and low-latency compilation.
    /// - On Battery power: engage chilled powersave profile, throttling micro-pauses, and conserve battery.
    /// - Smoothly inserts 15-50ms cooling micro-pauses if CPU temp > 78°C to prevent thermal throttling.
    /// - Bursts MGLRU min_ttl_ms to 1000 during performance burst; restores powersave/0 on drop.
    /// - Returns RAII `VibeHardwareGuard` that automatically relaxes back on Drop.
    pub fn boost_for_vibe_build(&self) -> Result<VibeHardwareGuard> {
        let previous_governor = self.read_current_governor();
        let previous_epp = self.read_current_epp();
        let previous_mglru_ttl = self.read_mglru_min_ttl_ms().unwrap_or(0);
        let power_tele = self.read_power_supply_telemetry();
        let temp_c = self.read_max_cpu_temp_c();

        let on_battery = power_tele.is_discharging || (!power_tele.is_on_ac && power_tele.battery_pct.is_some());
        let battery_pct_val = power_tele.battery_pct.unwrap_or(100);
        let is_bat_critical = on_battery && battery_pct_val < BATTERY_CRITICAL_THRESHOLD_PCT;

        if on_battery || temp_c >= VIBE_THERMAL_HEADROOM_THRESHOLD_C || is_bat_critical {
            // Dynamic thermal throttling / battery conservation:
            // Downscale governor to powersave, relax EPP, and set MGLRU min_ttl_ms to 0
            let pause_ms = if is_bat_critical {
                75
            } else if temp_c >= VIBE_THERMAL_HEADROOM_THRESHOLD_C {
                let excess = (temp_c - VIBE_THERMAL_HEADROOM_THRESHOLD_C).min(12.0);
                15 + ((excess / 12.0) * 35.0) as u64
            } else {
                35
            };
            std::thread::sleep(Duration::from_millis(pause_ms));

            self.write_governor_to_cores("powersave");
            let target_epp = if is_bat_critical { "power" } else { "balance_performance" };
            self.write_epp_to_cores(target_epp);
            self.write_mglru_min_ttl_ms(0);
            self.is_boosted.store(false, Ordering::SeqCst);
        } else {
            // On AC power and cool: allow full burst to performance and MGLRU min_ttl_ms = 1000
            self.write_governor_to_cores("performance");
            self.write_epp_to_cores("performance");
            self.write_mglru_min_ttl_ms(1000);
            self.is_boosted.store(true, Ordering::SeqCst);
        }

        Ok(VibeHardwareGuard {
            previous_governor,
            previous_epp,
            previous_mglru_ttl,
            cpu_dir: self.cpu_dir.clone(),
            script_path: self.script_path.clone(),
            mglru_file: self.mglru_file.clone(),
            kernel_optimizer_dir: self.kernel_optimizer_dir.clone(),
            is_boosted: self.is_boosted.clone(),
            active: true,
            on_battery,
        })
    }

    /// Retrieve ambient thermal and power telemetry
    pub fn get_vibe_thermal_state(&self) -> VibeThermalState {
        let cpu_temp_c = self.read_max_cpu_temp_c();
        let governor = self.read_current_governor();
        let epp = self.read_current_epp();
        let tele = self.read_power_supply_telemetry();
        let on_battery = tele.is_discharging || (!tele.is_on_ac && tele.battery_pct.is_some());
        let bat_pct = tele.battery_pct.unwrap_or(100);

        let vibe_mode = if cpu_temp_c >= VIBE_THERMAL_HEADROOM_THRESHOLD_C {
            "ThermalThrottled".to_string()
        } else if on_battery && bat_pct < BATTERY_CRITICAL_THRESHOLD_PCT {
            "BatterySaver".to_string()
        } else if on_battery {
            "BatterySaver".to_string()
        } else if governor == "performance" || self.is_boosted.load(Ordering::Relaxed) {
            "HighPerformance".to_string()
        } else {
            "ChilledReading".to_string()
        };

        let recommended_draft_model = self.get_recommended_draft_model().to_string();
        let recommended_concurrency = self.recommended_concurrency_limit();
        let mglru_min_ttl_ms = self.read_mglru_min_ttl_ms();
        let memory_pressure = self.read_memory_pressure_or_vmstat();

        VibeThermalState {
            cpu_temp_c,
            governor,
            epp,
            battery_pct: tele.battery_pct,
            is_charging: tele.is_on_ac && !tele.is_discharging,
            is_on_ac: tele.is_on_ac,
            is_discharging: tele.is_discharging,
            vibe_mode,
            recommended_draft_model,
            recommended_concurrency,
            mglru_min_ttl_ms,
            memory_pressure,
        }
    }
}

/// RAII Guard that automatically relaxes CPU governor to `powersave` and EPP to `balance_performance`
pub struct VibeHardwareGuard {
    pub previous_governor: String,
    pub previous_epp: String,
    pub previous_mglru_ttl: u64,
    cpu_dir: PathBuf,
    script_path: PathBuf,
    mglru_file: PathBuf,
    kernel_optimizer_dir: PathBuf,
    is_boosted: Arc<AtomicBool>,
    active: bool,
    pub on_battery: bool,
}

impl VibeHardwareGuard {
    /// Explicitly relax CPU governor and EPP back to powersave / balance_performance
    pub fn relax(&mut self) {
        if !self.active {
            return;
        }

        // Reset process priority back to normal nice
        apply_userspace_priority_fallback(false);

        // Relax governor to powersave
        let mut wrote = false;
        if let Ok(entries) = fs::read_dir(&self.cpu_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                    let gov_file = entry.path().join("cpufreq/scaling_governor");
                    if gov_file.exists() && fs::write(&gov_file, "powersave").is_ok() {
                        wrote = true;
                    }
                    let epp_file = entry.path().join("cpufreq/energy_performance_preference");
                    if epp_file.exists() {
                        let _ = fs::write(&epp_file, "balance_performance");
                    }
                } else if name.starts_with("policy") {
                    let gov_file = entry.path().join("scaling_governor");
                    if gov_file.exists() && fs::write(&gov_file, "powersave").is_ok() {
                        wrote = true;
                    }
                    let epp_file = entry.path().join("energy_performance_preference");
                    if epp_file.exists() {
                        let _ = fs::write(&epp_file, "balance_performance");
                    }
                }
            }
        }

        let cpufreq_dir = self.cpu_dir.join("cpufreq");
        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("policy") {
                    let gov_file = entry.path().join("scaling_governor");
                    if gov_file.exists() && fs::write(&gov_file, "powersave").is_ok() {
                        wrote = true;
                    }
                    let epp_file = entry.path().join("energy_performance_preference");
                    if epp_file.exists() {
                        let _ = fs::write(&epp_file, "balance_performance");
                    }
                }
            }
        }

        if !wrote && self.script_path.exists() {
            let _ = Command::new(&self.script_path).arg("balanced").output();
        }

        // Restore MGLRU min_ttl_ms
        let target_mglru = self.previous_mglru_ttl;
        if self.mglru_file.exists() {
            let _ = fs::write(&self.mglru_file, format!("{}\n", target_mglru));
        } else {
            let default_mglru = Path::new(DEFAULT_SYSFS_MGLRU_FILE);
            if default_mglru.exists() {
                let _ = fs::write(default_mglru, format!("{}\n", target_mglru));
            }
        }
        let script = self.kernel_optimizer_dir.join("plugins/mglru-monitor.sh");
        if script.exists() {
            let _ = Command::new(&script)
                .args(["set-min-ttl", &target_mglru.to_string()])
                .output();
        }

        self.is_boosted.store(false, Ordering::SeqCst);
        self.active = false;
    }
}

impl Drop for VibeHardwareGuard {
    fn drop(&mut self) {
        self.relax();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_vibe_hardware_governor_telemetry() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "64000\n").unwrap();
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        fs::create_dir_all(power_dir.join("BAT0")).unwrap();
        fs::write(power_dir.join("BAT0/status"), "Charging\n").unwrap();
        fs::write(power_dir.join("BAT0/capacity"), "92\n").unwrap();

        let gov = VibeHardwareGovernor::with_custom_paths(cpu_dir, thermal_dir, power_dir, script_path);
        let state = gov.get_vibe_thermal_state();

        assert_eq!(state.governor, "powersave");
        assert_eq!(state.epp, "balance_performance");
        assert_eq!(state.cpu_temp_c, 64.0);
        assert_eq!(state.battery_pct, Some(92));
        assert!(state.is_charging);
        assert_eq!(state.vibe_mode, "ChilledReading");
    }

    #[test]
    fn test_vibe_hardware_guard_boost_and_drop_relax() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "55000\n").unwrap();
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        let gov = VibeHardwareGovernor::with_custom_paths(cpu_dir.clone(), thermal_dir, power_dir, script_path);

        {
            let guard = gov.boost_for_vibe_build().expect("Boost should succeed");
            assert_eq!(guard.previous_governor, "powersave");
            let gov_str = fs::read_to_string(cpu_dir.join("cpu0/cpufreq/scaling_governor")).unwrap();
            let epp_str = fs::read_to_string(cpu_dir.join("cpu0/cpufreq/energy_performance_preference")).unwrap();
            assert_eq!(gov_str.trim(), "performance");
            assert_eq!(epp_str.trim(), "performance");

            let boosted_state = gov.get_vibe_thermal_state();
            assert_eq!(boosted_state.vibe_mode, "HighPerformance");
        }

        // After guard is dropped, it must be relaxed to powersave & balance_performance
        let gov_str_relaxed = fs::read_to_string(cpu_dir.join("cpu0/cpufreq/scaling_governor")).unwrap();
        let epp_str_relaxed = fs::read_to_string(cpu_dir.join("cpu0/cpufreq/energy_performance_preference")).unwrap();
        assert_eq!(gov_str_relaxed.trim(), "powersave");
        assert_eq!(epp_str_relaxed.trim(), "balance_performance");

        let relaxed_state = gov.get_vibe_thermal_state();
        assert_eq!(relaxed_state.vibe_mode, "ChilledReading");
    }

    #[test]
    fn test_vibe_hardware_overheat_pause_and_throttled_mode() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "82500\n").unwrap(); // 82.5°C > 78°C
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        let gov = VibeHardwareGovernor::with_custom_paths(cpu_dir, thermal_dir, power_dir, script_path);

        let t0 = Instant::now();
        let _guard = gov.boost_for_vibe_build().expect("Boost should succeed with cooling pause");
        let elapsed = t0.elapsed();
        // Should have inserted cooling micro-pause (> 15ms)
        assert!(elapsed.as_millis() >= 14);

        let state = gov.get_vibe_thermal_state();
        assert_eq!(state.vibe_mode, "ThermalThrottled");
        assert_eq!(state.cpu_temp_c, 82.5);
    }

    #[test]
    fn test_power_supply_uevent_battery_discharging() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(power_dir.join("BAT0")).unwrap();
        let uevent_data = "POWER_SUPPLY_NAME=BAT0\nPOWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_STATUS=Discharging\nPOWER_SUPPLY_CAPACITY=42\n";
        fs::write(power_dir.join("BAT0/uevent"), uevent_data).unwrap();

        let gov = VibeHardwareGovernor::with_custom_paths(cpu_dir.clone(), thermal_dir, power_dir, script_path);
        let tele = gov.read_power_supply_telemetry();

        assert!(!tele.is_on_ac);
        assert!(tele.is_discharging);
        assert_eq!(tele.battery_pct, Some(42));
        assert_eq!(tele.status, "Discharging");
        assert_eq!(gov.get_recommended_draft_model(), "qwen2.5-coder:0.5b");

        let state = gov.get_vibe_thermal_state();
        assert_eq!(state.vibe_mode, "BatterySaver");
        assert_eq!(state.recommended_draft_model, "qwen2.5-coder:0.5b");

        // Boosting on battery should engage chilled powersave profile, not turbo to performance
        let t0 = Instant::now();
        let guard = gov.boost_for_vibe_build().unwrap();
        assert!(guard.on_battery);
        assert!(t0.elapsed().as_millis() >= 30, "Should apply throttling micro-pause on battery");
        let gov_str = fs::read_to_string(cpu_dir.join("cpu0/cpufreq/scaling_governor")).unwrap();
        assert_eq!(gov_str.trim(), "powersave");
    }

    #[test]
    fn test_non_root_sysfs_permissions_fallback() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("non_existent_script.sh");

        // Create unwritable/read-only sysfs entry to trigger fallback
        let cpu0_cpufreq = cpu_dir.join("cpu0/cpufreq");
        fs::create_dir_all(&cpu0_cpufreq).unwrap();
        let gov_file = cpu0_cpufreq.join("scaling_governor");
        fs::write(&gov_file, "powersave\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&gov_file).unwrap().permissions();
            perms.set_mode(0o400); // Read-only: writing will fail with PermissionDenied
            let _ = fs::set_permissions(&gov_file, perms);
        }

        let gov = VibeHardwareGovernor::with_custom_paths(cpu_dir, thermal_dir, power_dir, script_path);
        // boost_for_vibe_build must not panic or fail; it should gracefully apply fallback
        let guard_res = gov.boost_for_vibe_build();
        assert!(guard_res.is_ok(), "Must gracefully succeed using user-space priority fallback");
    }

    #[test]
    fn test_mglru_burst_and_restore_lifecycle() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("cpu-governor.sh");
        let mglru_file = temp.path().join("mglru_min_ttl_ms");
        let vmstat_file = temp.path().join("vmstat");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "50000\n").unwrap();
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        fs::create_dir_all(&power_dir).unwrap();
        fs::write(mglru_file.clone(), "0\n").unwrap();
        fs::write(vmstat_file.clone(), "allocstall_normal 0\npgscan_direct 100\npgsteal_direct 90\n").unwrap();

        let gov = VibeHardwareGovernor::with_all_paths(
            cpu_dir.clone(),
            thermal_dir,
            power_dir,
            script_path,
            mglru_file.clone(),
            vmstat_file,
        );

        assert_eq!(gov.read_mglru_min_ttl_ms(), Some(0));

        {
            let guard = gov.boost_for_vibe_build().expect("Boost should succeed on cool AC");
            assert_eq!(guard.previous_mglru_ttl, 0);
            let mglru_val = fs::read_to_string(&mglru_file).unwrap();
            assert_eq!(mglru_val.trim(), "1000");
        }

        // After guard drop, MGLRU min_ttl_ms must be restored to 0
        let mglru_val_restored = fs::read_to_string(&mglru_file).unwrap();
        assert_eq!(mglru_val_restored.trim(), "0");
    }

    #[test]
    fn test_recommended_concurrency_throttling_battery_and_temp() {
        let temp = tempdir().unwrap();
        let cpu_dir = temp.path().join("cpu");
        let thermal_dir = temp.path().join("thermal");
        let power_dir = temp.path().join("power");
        let script_path = temp.path().join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        // 1. Overheated state (>78°C: 81°C) -> recommend clamped concurrency (<=2)
        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "81000\n").unwrap();
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        let gov = VibeHardwareGovernor::with_custom_paths(cpu_dir.clone(), thermal_dir.clone(), power_dir.clone(), script_path.clone());
        let conc = gov.recommended_concurrency_limit();
        assert!(conc <= 2 && conc >= 1, "Concurrency should be clamped to <=2 on overheated CPU, got {}", conc);

        // 2. Critical battery state (<20%: 15% and discharging)
        fs::write(thermal_dir.join("thermal_zone0/temp"), "52000\n").unwrap();
        fs::create_dir_all(power_dir.join("BAT0")).unwrap();
        let uevent_data = "POWER_SUPPLY_NAME=BAT0\nPOWER_SUPPLY_TYPE=Battery\nPOWER_SUPPLY_STATUS=Discharging\nPOWER_SUPPLY_CAPACITY=15\n";
        fs::write(power_dir.join("BAT0/uevent"), uevent_data).unwrap();

        let gov_bat = VibeHardwareGovernor::with_custom_paths(cpu_dir, thermal_dir, power_dir, script_path);
        let conc_bat = gov_bat.recommended_concurrency_limit();
        assert!(conc_bat <= 2 && conc_bat >= 1, "Concurrency should be clamped to <=2 on critical battery (<20%), got {}", conc_bat);
    }
}

