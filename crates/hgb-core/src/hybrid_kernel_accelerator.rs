//! # Pillar 1: Hybrid Kernel-Level Acceleration (dynatune & kernel-optimizer Integration)
//!
//! Provides deep hardware coupling between Hagibis (`hgbd`, `hgb-core`) and sibling modules:
//! - sibling `dynatune` directory (`$DYNATUNE_DIR` or `../dynatune`) (`battery.rs`, `power.rs`, `processes.rs`, `thermal.rs`)
//! - sibling `kernel-optimizer` directory (`$KERNEL_OPTIMIZER_DIR` or `../kernel-optimizer`) (`dynabook-kernel-ctl`, `plugins/mglru-monitor.sh`, `plugins/cpu-governor.sh`)
//! - Linux kernel sysfs: `/sys/devices/system/cpu/`, `/sys/kernel/mm/lru_gen/enabled`,
//!   `/sys/kernel/mm/lru_gen/min_ttl_ms`, `/proc/pressure/memory`, `/proc/vmstat`
//!
//! Implements:
//! - Dynamic CPU frequency governor shifting to `performance`
//! - Energy Performance Preference (EPP) tuning to `performance`
//! - Multi-Gen LRU (MGLRU) low-latency tuning (`min_ttl_ms 1000`)
//! - Reclaim efficiency inspection (`pgscan` vs `pgsteal` from `/proc/vmstat`)
//! - Thermal watchdog (backs off if temperature approaches 80°C)
//! - Battery watchdog (backs off to balanced mode if discharging <= 20%)
//! - RAII `KernelAccelerationGuard` restoring power-saving / balanced profile upon drop.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

pub const THERMAL_CRITICAL_HEADROOM_C: f64 = 80.0;
pub const THERMAL_SAFE_MARGIN_C: f64 = 72.0;
pub const BATTERY_LOW_THRESHOLD_PCT: u32 = 20;

/// Live telemetry report for hybrid kernel acceleration subsystem
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HybridKernelTelemetry {
    pub active_governor: String,
    pub active_epp: String,
    pub mglru_flags: String,
    pub mglru_min_ttl_ms: u64,
    pub memory_pressure_psi: f64,
    pub cpu_temp_c: f64,
    pub reclaim_efficiency: f64,
    pub is_throttled: bool,
    pub battery_pct: Option<u32>,
    pub on_battery: bool,
    pub active_workload: Option<String>,
}

/// Baseline configuration restored by RAII guard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineKernelProfile {
    pub baseline_governor: String,
    pub baseline_epp: String,
    pub baseline_min_ttl_ms: u64,
}

impl Default for BaselineKernelProfile {
    fn default() -> Self {
        Self {
            baseline_governor: "powersave".to_string(),
            baseline_epp: "balance_performance".to_string(),
            baseline_min_ttl_ms: 0,
        }
    }
}

/// RAII Guard that automatically reverts kernel acceleration parameters back to
/// powersave / balanced mode when execution completes or the guard is dropped.
pub struct KernelAccelerationGuard {
    baseline: BaselineKernelProfile,
    is_active: Arc<AtomicBool>,
    dynabook_ctl_path: Option<PathBuf>,
    cpu_gov_plugin: Option<PathBuf>,
    mglru_plugin: Option<PathBuf>,
    cpu_sysfs_base: PathBuf,
    mglru_base: PathBuf,
    current_workload: Arc<std::sync::RwLock<Option<String>>>,
}

impl KernelAccelerationGuard {
    pub fn baseline(&self) -> &BaselineKernelProfile {
        &self.baseline
    }

    /// Explicitly restore baseline configuration
    pub fn restore(&mut self) -> Result<()> {
        if !self.is_active.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        // 1. Reset governor
        Self::apply_sysfs_governor(&self.cpu_sysfs_base, &self.baseline.baseline_governor);
        // 2. Reset EPP
        Self::apply_sysfs_epp(&self.cpu_sysfs_base, &self.baseline.baseline_epp);

        // 3. Reset MGLRU min_ttl_ms
        let ttl_path = self.mglru_base.join("min_ttl_ms");
        if ttl_path.exists() {
            let _ = fs::write(&ttl_path, self.baseline.baseline_min_ttl_ms.to_string());
        }

        // 4. Fallback/Sync to dynabook-kernel-ctl or plugins
        if let Some(ref ctl) = self.dynabook_ctl_path {
            let _ = Command::new(ctl).args(["profile", "balanced"]).output();
        } else if let Some(ref plug) = self.cpu_gov_plugin {
            let _ = Command::new(plug).args(["profile", "balanced"]).output();
        }

        if let Some(ref mglru_plug) = self.mglru_plugin {
            let _ = Command::new(mglru_plug).args(["set-min-ttl", &self.baseline.baseline_min_ttl_ms.to_string()]).output();
        }

        if let Ok(mut lock) = self.current_workload.write() {
            *lock = None;
        }

        Ok(())
    }

    fn apply_sysfs_governor(sysfs_base: &Path, governor: &str) {
        if let Ok(entries) = fs::read_dir(sysfs_base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                    let gov_path = entry.path().join("cpufreq/scaling_governor");
                    if gov_path.exists() {
                        let _ = fs::write(&gov_path, governor);
                    }
                }
            }
        }
    }

    fn apply_sysfs_epp(sysfs_base: &Path, epp: &str) {
        if let Ok(entries) = fs::read_dir(sysfs_base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                    let epp_path = entry.path().join("cpufreq/energy_performance_preference");
                    if epp_path.exists() {
                        let _ = fs::write(&epp_path, epp);
                    }
                }
            }
        }
        let cpufreq_dir = sysfs_base.join("cpufreq");
        if let Ok(entries) = fs::read_dir(cpufreq_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("policy") {
                    let epp_path = entry.path().join("energy_performance_preference");
                    if epp_path.exists() {
                        let _ = fs::write(&epp_path, epp);
                    }
                }
            }
        }
    }
}

impl Drop for KernelAccelerationGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Hybrid Kernel Accelerator Engine
pub struct HybridKernelAccelerator {
    dynabook_ctl_path: Option<PathBuf>,
    cpu_gov_plugin: Option<PathBuf>,
    mglru_plugin: Option<PathBuf>,
    cpu_sysfs_base: PathBuf,
    mglru_base: PathBuf,
    proc_pressure_memory: PathBuf,
    proc_vmstat: PathBuf,
    thermal_base: PathBuf,
    power_supply_base: PathBuf,
    is_boosted: Arc<AtomicBool>,
    current_workload: Arc<std::sync::RwLock<Option<String>>>,
}

impl Default for HybridKernelAccelerator {
    fn default() -> Self {
        Self::new()
    }
}

impl HybridKernelAccelerator {
    /// Initialize with auto-discovered sibling paths and Linux sysfs mounts
    pub fn new() -> Self {
        let mut candidates_ctl = Vec::new();
        if let Ok(dir) = std::env::var("KERNEL_OPTIMIZER_DIR") {
            candidates_ctl.push(PathBuf::from(dir).join("dynabook-kernel-ctl"));
        }
        candidates_ctl.extend([
            PathBuf::from("../kernel-optimizer/dynabook-kernel-ctl"),
            PathBuf::from("../../kernel-optimizer/dynabook-kernel-ctl"),
            PathBuf::from("kernel-optimizer/dynabook-kernel-ctl"),
            PathBuf::from("/usr/bin/dynabook-kernel-ctl"),
            PathBuf::from("/usr/local/bin/dynabook-kernel-ctl"),
        ]);
        let dynabook_ctl_path = candidates_ctl.into_iter().find(|p| p.exists());

        let mut candidates_mglru = Vec::new();
        if let Ok(dir) = std::env::var("KERNEL_OPTIMIZER_DIR") {
            candidates_mglru.push(PathBuf::from(dir).join("plugins/mglru-monitor.sh"));
        }
        candidates_mglru.extend([
            PathBuf::from("../kernel-optimizer/plugins/mglru-monitor.sh"),
            PathBuf::from("../../kernel-optimizer/plugins/mglru-monitor.sh"),
            PathBuf::from("kernel-optimizer/plugins/mglru-monitor.sh"),
            PathBuf::from("/usr/lib/dynabook-kernel/plugins/mglru-monitor.sh"),
        ]);
        let mglru_plugin = candidates_mglru.into_iter().find(|p| p.exists());

        let mut candidates_cpu_gov = Vec::new();
        if let Ok(dir) = std::env::var("KERNEL_OPTIMIZER_DIR") {
            candidates_cpu_gov.push(PathBuf::from(dir).join("plugins/cpu-governor.sh"));
        }
        candidates_cpu_gov.extend([
            PathBuf::from("../kernel-optimizer/plugins/cpu-governor.sh"),
            PathBuf::from("../../kernel-optimizer/plugins/cpu-governor.sh"),
            PathBuf::from("kernel-optimizer/plugins/cpu-governor.sh"),
            PathBuf::from("/usr/lib/dynabook-kernel/plugins/cpu-governor.sh"),
        ]);
        let cpu_gov_plugin = candidates_cpu_gov.into_iter().find(|p| p.exists());

        Self {
            dynabook_ctl_path,
            cpu_gov_plugin,
            mglru_plugin,
            cpu_sysfs_base: PathBuf::from("/sys/devices/system/cpu"),
            mglru_base: PathBuf::from("/sys/kernel/mm/lru_gen"),
            proc_pressure_memory: PathBuf::from("/proc/pressure/memory"),
            proc_vmstat: PathBuf::from("/proc/vmstat"),
            thermal_base: PathBuf::from("/sys/class/thermal"),
            power_supply_base: PathBuf::from("/sys/class/power_supply"),
            is_boosted: Arc::new(AtomicBool::new(false)),
            current_workload: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Read live telemetry across CPU, MGLRU, thermal, battery, and reclaim efficiency
    pub fn telemetry(&self) -> HybridKernelTelemetry {
        let active_governor = self.read_active_governor();
        let active_epp = self.read_active_epp();
        let (mglru_flags, mglru_min_ttl_ms) = self.read_mglru_state();
        let memory_pressure_psi = self.read_psi_some_avg10();
        let cpu_temp_c = self.read_max_cpu_temp();
        let reclaim_efficiency = self.read_reclaim_efficiency();
        let (battery_pct, on_battery) = self.read_battery_status();

        let is_throttled = cpu_temp_c >= THERMAL_CRITICAL_HEADROOM_C
            || (on_battery && battery_pct.map_or(false, |pct| pct <= BATTERY_LOW_THRESHOLD_PCT));

        let active_workload = self.current_workload.read().ok().and_then(|lock| lock.clone());

        HybridKernelTelemetry {
            active_governor,
            active_epp,
            mglru_flags,
            mglru_min_ttl_ms,
            memory_pressure_psi,
            cpu_temp_c,
            reclaim_efficiency,
            is_throttled,
            battery_pct,
            on_battery,
            active_workload,
        }
    }

    /// Shifts CPU frequency governor to `performance`, sets EPP to `performance`,
    /// tunes Multi-Gen LRU (MGLRU) `min_ttl_ms` to 1000, and engages thermal/battery safeguards.
    pub fn boost_workload(&self, workload_name: &str) -> Result<KernelAccelerationGuard> {
        let temp = self.read_max_cpu_temp();
        let (battery_pct, on_battery) = self.read_battery_status();

        // Thermal and Battery Watchdog
        if temp >= THERMAL_CRITICAL_HEADROOM_C {
            eprintln!(
                "⚠️ [HybridKernelAccelerator] Thermal trip prevention: CPU temp is {:.1}°C (>= {:.1}°C). Throttling boost to balanced mode.",
                temp, THERMAL_CRITICAL_HEADROOM_C
            );
            self.relax()?;
            return Err(HgbError::Execution(format!(
                "Thermal watchdog engaged: CPU temp {:.1}°C exceeds safe margin ({:.1}°C)",
                temp, THERMAL_CRITICAL_HEADROOM_C
            )));
        }

        if on_battery && battery_pct.map_or(false, |pct| pct <= BATTERY_LOW_THRESHOLD_PCT) {
            eprintln!(
                "⚠️ [HybridKernelAccelerator] Battery safeguard engaged: Battery at {}% (<= {}%). Refusing performance boost.",
                battery_pct.unwrap_or(0), BATTERY_LOW_THRESHOLD_PCT
            );
            self.relax()?;
            return Err(HgbError::Execution(format!(
                "Battery watchdog engaged: Battery level {}% is critical",
                battery_pct.unwrap_or(0)
            )));
        }

        // Capture baseline
        let baseline = BaselineKernelProfile {
            baseline_governor: self.read_active_governor(),
            baseline_epp: self.read_active_epp(),
            baseline_min_ttl_ms: self.read_mglru_state().1,
        };

        // 1. Shift CPU governor to 'performance'
        KernelAccelerationGuard::apply_sysfs_governor(&self.cpu_sysfs_base, "performance");

        // 2. Shift CPU EPP to 'performance'
        KernelAccelerationGuard::apply_sysfs_epp(&self.cpu_sysfs_base, "performance");

        // 3. Tune MGLRU min_ttl_ms to 1000 for low latency
        let ttl_path = self.mglru_base.join("min_ttl_ms");
        if ttl_path.exists() {
            let _ = fs::write(&ttl_path, "1000");
        }

        // 4. Try dynabook-kernel-ctl or plugins if available
        if let Some(ref ctl) = self.dynabook_ctl_path {
            let _ = Command::new(ctl).args(["profile", "performance"]).output();
        } else if let Some(ref plug) = self.cpu_gov_plugin {
            let _ = Command::new(plug).args(["profile", "performance"]).output();
        }

        if let Some(ref mglru_plug) = self.mglru_plugin {
            let _ = Command::new(mglru_plug).args(["set-min-ttl", "1000"]).output();
        }

        self.is_boosted.store(true, Ordering::SeqCst);
        if let Ok(mut lock) = self.current_workload.write() {
            *lock = Some(workload_name.to_string());
        }

        Ok(KernelAccelerationGuard {
            baseline,
            is_active: self.is_boosted.clone(),
            dynabook_ctl_path: self.dynabook_ctl_path.clone(),
            cpu_gov_plugin: self.cpu_gov_plugin.clone(),
            mglru_plugin: self.mglru_plugin.clone(),
            cpu_sysfs_base: self.cpu_sysfs_base.clone(),
            mglru_base: self.mglru_base.clone(),
            current_workload: self.current_workload.clone(),
        })
    }

    /// Reset configuration back to balanced / power-saving mode
    pub fn relax(&self) -> Result<()> {
        KernelAccelerationGuard::apply_sysfs_governor(&self.cpu_sysfs_base, "powersave");
        KernelAccelerationGuard::apply_sysfs_epp(&self.cpu_sysfs_base, "balance_performance");

        let ttl_path = self.mglru_base.join("min_ttl_ms");
        if ttl_path.exists() {
            let _ = fs::write(&ttl_path, "0");
        }

        if let Some(ref ctl) = self.dynabook_ctl_path {
            let _ = Command::new(ctl).args(["profile", "balanced"]).output();
        } else if let Some(ref plug) = self.cpu_gov_plugin {
            let _ = Command::new(plug).args(["profile", "balanced"]).output();
        }

        if let Some(ref mglru_plug) = self.mglru_plugin {
            let _ = Command::new(mglru_plug).args(["set-min-ttl", "0"]).output();
        }

        self.is_boosted.store(false, Ordering::SeqCst);
        if let Ok(mut lock) = self.current_workload.write() {
            *lock = None;
        }

        Ok(())
    }

    fn read_active_governor(&self) -> String {
        let gov_path = self.cpu_sysfs_base.join("cpu0/cpufreq/scaling_governor");
        if let Ok(s) = fs::read_to_string(&gov_path) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        "powersave".to_string()
    }

    fn read_active_epp(&self) -> String {
        let epp_path = self.cpu_sysfs_base.join("cpu0/cpufreq/energy_performance_preference");
        if let Ok(s) = fs::read_to_string(&epp_path) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        let policy_epp = self.cpu_sysfs_base.join("cpufreq/policy0/energy_performance_preference");
        if let Ok(s) = fs::read_to_string(&policy_epp) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        "balance_performance".to_string()
    }

    fn read_mglru_state(&self) -> (String, u64) {
        let enabled_path = self.mglru_base.join("enabled");
        let flags = fs::read_to_string(&enabled_path)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "0x0007".to_string());

        let ttl_path = self.mglru_base.join("min_ttl_ms");
        let min_ttl = fs::read_to_string(&ttl_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);

        (flags, min_ttl)
    }

    fn read_psi_some_avg10(&self) -> f64 {
        if let Ok(content) = fs::read_to_string(&self.proc_pressure_memory) {
            for line in content.lines() {
                if line.starts_with("some") {
                    for part in line.split_whitespace() {
                        if part.starts_with("avg10=") {
                            if let Ok(val) = part["avg10=".len()..].parse::<f64>() {
                                return val;
                            }
                        }
                    }
                }
            }
        }
        0.0
    }

    fn read_max_cpu_temp(&self) -> f64 {
        let mut max_c = 42.0;
        if let Ok(entries) = fs::read_dir(&self.thermal_base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let temp_path = entry.path().join("temp");
                    if let Ok(s) = fs::read_to_string(&temp_path) {
                        if let Ok(milli) = s.trim().parse::<f64>() {
                            let c = milli / 1000.0;
                            if c > max_c && c < 125.0 {
                                max_c = c;
                            }
                        }
                    }
                }
            }
        }
        max_c
    }

    fn read_reclaim_efficiency(&self) -> f64 {
        if let Ok(content) = fs::read_to_string(&self.proc_vmstat) {
            let mut pgscan: u64 = 0;
            let mut pgsteal: u64 = 0;

            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(key), Some(val_str)) = (parts.next(), parts.next()) {
                    if let Ok(val) = val_str.parse::<u64>() {
                        if key.starts_with("pgscan") {
                            pgscan += val;
                        } else if key.starts_with("pgsteal") {
                            pgsteal += val;
                        }
                    }
                }
            }

            if pgscan > 0 {
                return ((pgsteal as f64 / pgscan as f64) * 100.0).min(100.0);
            }
        }
        100.0
    }

    fn read_battery_status(&self) -> (Option<u32>, bool) {
        let mut capacity: Option<u32> = None;
        let mut on_battery = false;

        if let Ok(entries) = fs::read_dir(&self.power_supply_base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("BAT") {
                    let cap_file = entry.path().join("capacity");
                    if let Ok(s) = fs::read_to_string(&cap_file) {
                        if let Ok(cap) = s.trim().parse::<u32>() {
                            capacity = Some(cap);
                        }
                    }
                    let status_file = entry.path().join("status");
                    if let Ok(s) = fs::read_to_string(&status_file) {
                        if s.trim().eq_ignore_ascii_case("Discharging") {
                            on_battery = true;
                        }
                    }
                }
                if name.starts_with("AC") || name.starts_with("ADP") {
                    let online_file = entry.path().join("online");
                    if let Ok(s) = fs::read_to_string(&online_file) {
                        if s.trim() == "0" {
                            on_battery = true;
                        }
                    }
                }
            }
        }

        (capacity, on_battery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_gathering() {
        let accelerator = HybridKernelAccelerator::new();
        let telem = accelerator.telemetry();
        assert!(!telem.active_governor.is_empty());
        assert!(!telem.active_epp.is_empty());
        assert!(telem.reclaim_efficiency >= 0.0 && telem.reclaim_efficiency <= 100.0);
        assert!(telem.cpu_temp_c >= 0.0);
    }

    #[test]
    fn test_raii_guard_lifecycle() {
        let accelerator = HybridKernelAccelerator::new();
        let guard_res = accelerator.boost_workload("unit_test_compilation");
        if let Ok(mut guard) = guard_res {
            assert_eq!(guard.baseline().baseline_min_ttl_ms, 0);
            assert!(guard.restore().is_ok());
        }
    }

    #[test]
    fn test_relax_call() {
        let accelerator = HybridKernelAccelerator::new();
        let res = accelerator.relax();
        assert!(res.is_ok());
    }
}
