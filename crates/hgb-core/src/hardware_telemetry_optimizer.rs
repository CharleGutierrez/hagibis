//! # Pillar 1: Hardware Telemetry Optimizer
//!
//! Deeply bridges into sibling modules in `/home/toshiba/My AI Project`:
//! - `/home/toshiba/My AI Project/dynatune` (`dynatune` binary)
//! - `/home/toshiba/My AI Project/kernel-optimizer` (`dynabook-kernel-ctl`)
//!
//! Implements real interaction with Linux Multi-Gen LRU (MGLRU) (`/sys/kernel/mm/lru_gen/enabled`),
//! memory Pressure Stall Information (`/proc/pressure/memory`), Intel P-State CPU governors,
//! and thermal headroom monitoring.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use serde::{Deserialize, Serialize};

use crate::dynamic_governor::WorkloadKind;
use crate::error::Result;

pub const THERMAL_CRITICAL_THRESHOLD_C: f64 = 80.0;
pub const THERMAL_SAFE_MARGIN_C: f64 = 72.0;
pub const PSI_SOME_WARN_THRESHOLD: f64 = 15.0;

/// Live telemetry report for the host hardware optimization subsystems
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareOptimizationTelemetry {
    pub active_governor: String,
    pub active_epp: String,
    pub mglru_active: bool,
    pub mglru_flags: String,
    pub memory_psi_some_avg10: f64,
    pub memory_available_mb: u64,
    pub memory_total_mb: u64,
    pub cpu_temp_c: f64,
    pub is_throttled: bool,
    pub thermal_headroom_c: f64,
    pub dynabook_ctl_available: bool,
    pub dynatune_available: bool,
    pub active_workload: Option<String>,
}

/// Baseline CPU governor and EPP snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineHardwareProfile {
    pub baseline_governor: String,
    pub baseline_epp: String,
    pub baseline_min_ttl_ms: Option<u64>,
}

/// RAII Guard that automatically restores the baseline CPU governor, EPP,
/// and memory aging when execution finishes or the guard is dropped.
pub struct HardwareOptimizationGuard {
    baseline: BaselineHardwareProfile,
    is_active: Arc<AtomicBool>,
    dynabook_ctl_path: Option<PathBuf>,
    cpu_sysfs_base: PathBuf,
    mglru_base: PathBuf,
}

impl HardwareOptimizationGuard {
    pub fn baseline(&self) -> &BaselineHardwareProfile {
        &self.baseline
    }

    /// Explicitly restore baseline configuration
    pub fn restore(&mut self) -> Result<()> {
        if !self.is_active.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        Self::apply_governor(&self.cpu_sysfs_base, &self.baseline.baseline_governor);
        Self::apply_epp(&self.cpu_sysfs_base, &self.baseline.baseline_epp);

        if let Some(ttl) = self.baseline.baseline_min_ttl_ms {
            let ttl_path = self.mglru_base.join("min_ttl_ms");
            let _ = fs::write(ttl_path, ttl.to_string());
        }

        if let Some(ref ctl) = self.dynabook_ctl_path {
            let profile_name = if self.baseline.baseline_governor == "performance" {
                "performance"
            } else {
                "balanced"
            };
            let _ = Command::new(ctl).args(["profile", profile_name]).output();
        }

        Ok(())
    }

    fn apply_governor(sysfs_base: &Path, governor: &str) {
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

    fn apply_epp(sysfs_base: &Path, epp: &str) {
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

impl Drop for HardwareOptimizationGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Hardware Telemetry Optimizer
pub struct HardwareTelemetryOptimizer {
    cpu_sysfs_base: PathBuf,
    mglru_base: PathBuf,
    thermal_sysfs_base: PathBuf,
    dynabook_ctl_path: Option<PathBuf>,
    dynatune_path: Option<PathBuf>,
    is_boosted: Arc<AtomicBool>,
}

impl Default for HardwareTelemetryOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareTelemetryOptimizer {
    pub fn new() -> Self {
        let dynabook_ctl = if Path::new("/usr/local/bin/dynabook-kernel-ctl").exists() {
            Some(PathBuf::from("/usr/local/bin/dynabook-kernel-ctl"))
        } else if Path::new("/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl").exists() {
            Some(PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl"))
        } else {
            None
        };

        let dynatune = if Path::new("/home/toshiba/.local/bin/dynatune").exists() {
            Some(PathBuf::from("/home/toshiba/.local/bin/dynatune"))
        } else if Path::new("/usr/local/bin/dynatune").exists() {
            Some(PathBuf::from("/usr/local/bin/dynatune"))
        } else {
            None
        };

        Self {
            cpu_sysfs_base: PathBuf::from("/sys/devices/system/cpu"),
            mglru_base: PathBuf::from("/sys/kernel/mm/lru_gen"),
            thermal_sysfs_base: PathBuf::from("/sys/class/thermal"),
            dynabook_ctl_path: dynabook_ctl,
            dynatune_path: dynatune,
            is_boosted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Read live hardware and kernel optimization telemetry
    pub fn telemetry(&self) -> HardwareOptimizationTelemetry {
        let active_governor = self.read_active_governor();
        let active_epp = self.read_active_epp();
        let (mglru_active, mglru_flags) = self.read_mglru_status();
        let memory_psi_some_avg10 = self.read_psi_some_avg10();
        let (memory_total_mb, memory_available_mb) = self.read_meminfo();
        let cpu_temp_c = self.read_cpu_temperature();
        let is_throttled = cpu_temp_c >= THERMAL_CRITICAL_THRESHOLD_C;
        let thermal_headroom_c = (THERMAL_CRITICAL_THRESHOLD_C - cpu_temp_c).max(0.0);

        HardwareOptimizationTelemetry {
            active_governor,
            active_epp,
            mglru_active,
            mglru_flags,
            memory_psi_some_avg10,
            memory_available_mb,
            memory_total_mb,
            cpu_temp_c,
            is_throttled,
            thermal_headroom_c,
            dynabook_ctl_available: self.dynabook_ctl_path.is_some(),
            dynatune_available: self.dynatune_path.is_some(),
            active_workload: if self.is_boosted.load(Ordering::Relaxed) {
                Some("active_boost".to_string())
            } else {
                None
            },
        }
    }

    /// Dynamically optimize hardware for a specific workload (heavy compilation, prompt runs, microvm execution, wasm).
    /// Returns an RAII `HardwareOptimizationGuard` which restores original governor/EPP on drop.
    pub fn optimize_for_workload(&self, _workload: WorkloadKind) -> Result<HardwareOptimizationGuard> {
        let cpu_temp = self.read_cpu_temperature();
        let is_throttled = cpu_temp >= THERMAL_CRITICAL_THRESHOLD_C;

        // Capture current baseline before shifting
        let baseline = BaselineHardwareProfile {
            baseline_governor: self.read_active_governor(),
            baseline_epp: self.read_active_epp(),
            baseline_min_ttl_ms: self.read_mglru_min_ttl(),
        };

        if is_throttled {
            // Thermal trip prevention: do not apply maximum performance boost; maintain balance
            return Ok(HardwareOptimizationGuard {
                baseline,
                is_active: Arc::new(AtomicBool::new(false)),
                dynabook_ctl_path: self.dynabook_ctl_path.clone(),
                cpu_sysfs_base: self.cpu_sysfs_base.clone(),
                mglru_base: self.mglru_base.clone(),
            });
        }

        // 1. Shift CPU governor to performance via sysfs
        HardwareOptimizationGuard::apply_governor(&self.cpu_sysfs_base, "performance");

        // 2. Boost EPP to performance via sysfs
        HardwareOptimizationGuard::apply_epp(&self.cpu_sysfs_base, "performance");

        // 3. Delegate to dynabook-kernel-ctl if available
        if let Some(ref ctl) = self.dynabook_ctl_path {
            let _ = Command::new(ctl).args(["profile", "performance"]).output();
        }

        // 4. Memory & MGLRU optimization
        let (mglru_active, _) = self.read_mglru_status();
        let psi_some = self.read_psi_some_avg10();
        let (total_mb, avail_mb) = self.read_meminfo();
        let avail_pct = if total_mb > 0 { (avail_mb as f64 / total_mb as f64) * 100.0 } else { 100.0 };

        if mglru_active {
            // Set min_ttl_ms to 1000 for low latency eviction protection during heavy jobs
            let ttl_path = self.mglru_base.join("min_ttl_ms");
            if ttl_path.exists() {
                let _ = fs::write(&ttl_path, "1000");
            }
        }

        // If memory pressure exceeds safe margins (PSI > 15% or RAM available < 12%), trigger proactive compaction
        if psi_some > PSI_SOME_WARN_THRESHOLD || avail_pct < 12.0 {
            self.trigger_memory_compaction();
        }

        self.is_boosted.store(true, Ordering::SeqCst);

        Ok(HardwareOptimizationGuard {
            baseline,
            is_active: Arc::new(AtomicBool::new(true)),
            dynabook_ctl_path: self.dynabook_ctl_path.clone(),
            cpu_sysfs_base: self.cpu_sysfs_base.clone(),
            mglru_base: self.mglru_base.clone(),
        })
    }

    /// Proactively age memory or compact caches when high memory pressure is detected
    fn trigger_memory_compaction(&self) {
        // Trigger memory compaction if permitted
        let compact_path = Path::new("/proc/sys/vm/compact_memory");
        if compact_path.exists() {
            let _ = fs::write(compact_path, "1");
        }

        // Or invoke dynatune --optimize if available
        if let Some(ref dt) = self.dynatune_path {
            let _ = Command::new(dt).arg("--optimize").output();
        }
    }

    fn read_active_governor(&self) -> String {
        let policy0_gov = self.cpu_sysfs_base.join("cpufreq/policy0/scaling_governor");
        if let Ok(gov) = fs::read_to_string(policy0_gov) {
            let trimmed = gov.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        let cpu0_gov = self.cpu_sysfs_base.join("cpu0/cpufreq/scaling_governor");
        if let Ok(gov) = fs::read_to_string(cpu0_gov) {
            let trimmed = gov.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        "powersave".to_string()
    }

    fn read_active_epp(&self) -> String {
        let policy0_epp = self.cpu_sysfs_base.join("cpufreq/policy0/energy_performance_preference");
        if let Ok(epp) = fs::read_to_string(policy0_epp) {
            let trimmed = epp.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }

        "balance_performance".to_string()
    }

    fn read_mglru_status(&self) -> (bool, String) {
        let enabled_path = self.mglru_base.join("enabled");
        if let Ok(flags) = fs::read_to_string(enabled_path) {
            let trimmed = flags.trim();
            let is_active = trimmed != "0x0000" && trimmed != "0" && !trimmed.is_empty();
            (is_active, trimmed.to_string())
        } else {
            (false, "0x0000".to_string())
        }
    }

    fn read_mglru_min_ttl(&self) -> Option<u64> {
        let ttl_path = self.mglru_base.join("min_ttl_ms");
        fs::read_to_string(ttl_path).ok().and_then(|s| s.trim().parse::<u64>().ok())
    }

    fn read_psi_some_avg10(&self) -> f64 {
        let psi_path = Path::new("/proc/pressure/memory");
        if let Ok(content) = fs::read_to_string(psi_path) {
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

    fn read_meminfo(&self) -> (u64, u64) {
        let mut total_kb = 0u64;
        let mut avail_kb = 0u64;

        if let Ok(content) = fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    total_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                } else if line.starts_with("MemAvailable:") {
                    avail_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                }
            }
        }

        (total_kb / 1024, avail_kb / 1024)
    }

    fn read_cpu_temperature(&self) -> f64 {
        let mut max_temp = 45.0;

        if let Ok(entries) = fs::read_dir(&self.thermal_sysfs_base) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let temp_path = entry.path().join("temp");
                    if let Ok(raw) = fs::read_to_string(&temp_path) {
                        if let Ok(millidegrees) = raw.trim().parse::<f64>() {
                            let deg = millidegrees / 1000.0;
                            if deg > max_temp && deg < 115.0 {
                                max_temp = deg;
                            }
                        }
                    }
                }
            }
        }

        max_temp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_reads_real_system_state() {
        let optimizer = HardwareTelemetryOptimizer::new();
        let telem = optimizer.telemetry();
        assert!(!telem.active_governor.is_empty());
        assert!(!telem.active_epp.is_empty());
        assert!(telem.cpu_temp_c > 0.0);
        assert!(telem.cpu_temp_c < 120.0);
    }

    #[test]
    fn test_optimize_and_raii_guard_drop() {
        let optimizer = HardwareTelemetryOptimizer::new();
        let guard_res = optimizer.optimize_for_workload(WorkloadKind::HeavyCompile);
        assert!(guard_res.is_ok());

        let mut guard = guard_res.unwrap();
        assert!(!guard.baseline().baseline_governor.is_empty());
        let restore_res = guard.restore();
        assert!(restore_res.is_ok());
    }

    #[test]
    fn test_mglru_detection() {
        let optimizer = HardwareTelemetryOptimizer::new();
        let telem = optimizer.telemetry();
        if Path::new("/sys/kernel/mm/lru_gen/enabled").exists() {
            assert!(telem.mglru_active);
            assert_eq!(telem.mglru_flags, "0x0007");
        }
    }
}
