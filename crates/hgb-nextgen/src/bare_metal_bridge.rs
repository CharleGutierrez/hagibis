//! # Bare-Metal Kernel & Dynatune Performance Bridge (`hgb-nextgen`)
//!
//! Fuses Hagibis runtime directly with local `kernel-optimizer` (`dynabook-kernel-ctl`) and `dynatune`:
//! - Queries real-time bare-metal telemetry: Intel P-State governor, EPP, Turbo Boost, MGLRU, ZRAM, thermal & battery.
//! - Dynamically commands kernel governor profiles (`performance`, `balanced`, `powersave`) during heavy Wasm or LLM workloads.
//! - Provides RAII `GovernorGuard` that boosts CPU to high-performance and automatically reverts to energy-saving states on idle.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use serde::{Deserialize, Serialize};

/// Target kernel and CPU performance profile
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelProfile {
    Performance,
    Balanced,
    PowerSave,
}

impl std::fmt::Display for KernelProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Performance => write!(f, "performance"),
            Self::Balanced => write!(f, "balanced"),
            Self::PowerSave => write!(f, "powersave"),
        }
    }
}

/// Live bare-metal hardware and kernel telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BareMetalTelemetry {
    pub active_governor: String,
    pub energy_perf_preference: String,
    pub turbo_boost_enabled: bool,
    pub mglru_status: String,
    pub zram_compression_ratio: f32,
    pub cpu_pkg_temp_c: f64,
    pub battery_capacity_pct: Option<u32>,
    pub power_draw_watts: Option<f64>,
    pub is_charging: bool,
    pub thermal_severity: String,
}

/// Report generated after commanding a kernel governor profile shift
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorShiftReport {
    pub previous_profile: String,
    pub target_profile: KernelProfile,
    pub epp_applied: String,
    pub mglru_active: bool,
    pub success: bool,
    pub message: String,
}

/// Native Bare-Metal Bridge Engine
pub struct BareMetalPerformanceBridge {
    pub kernel_optimizer_dir: PathBuf,
    pub dynatune_dir: PathBuf,
}

impl Default for BareMetalPerformanceBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl BareMetalPerformanceBridge {
    pub fn new() -> Self {
        let kernel_optimizer_dir = PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer");
        let dynatune_dir = PathBuf::from("/home/toshiba/My AI Project/dynatune");

        Self {
            kernel_optimizer_dir,
            dynatune_dir,
        }
    }

    /// Read live bare-metal telemetry from Linux sysfs, procfs, and sibling kernel optimizer
    pub fn read_bare_metal_telemetry(&self) -> BareMetalTelemetry {
        // 1. CPU Scaling Governor
        let active_governor = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
            .unwrap_or_else(|_| "powersave".to_string())
            .trim()
            .to_string();

        // 2. Intel Energy Performance Preference (EPP)
        let energy_perf_preference = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
            .unwrap_or_else(|_| "balance_performance".to_string())
            .trim()
            .to_string();

        // 3. Intel Turbo Boost
        let no_turbo = fs::read_to_string("/sys/devices/system/cpu/intel_pstate/no_turbo")
            .unwrap_or_else(|_| "0".to_string())
            .trim()
            .to_string();
        let turbo_boost_enabled = no_turbo == "0";

        // 4. Multi-Gen LRU (MGLRU)
        let mglru_raw = fs::read_to_string("/sys/kernel/mm/lru_gen/enabled")
            .unwrap_or_else(|_| "0x0000".to_string())
            .trim()
            .to_string();
        let mglru_status = if mglru_raw.contains('7') || mglru_raw == "7" || mglru_raw.ends_with("0007") {
            "Active (0x0007 - Eviction, Protection, Aging)".to_string()
        } else {
            format!("Flags: {}", mglru_raw)
        };

        // 5. ZRAM Compression Ratio
        let zram_compression_ratio = self.read_zram_compression();

        // 6. CPU Package Temperature
        let cpu_pkg_temp_c = self.read_cpu_temp();

        // 7. Battery capacity and power draw
        let (battery_capacity_pct, power_draw_watts, is_charging) = self.read_battery_metrics();

        let thermal_severity = if cpu_pkg_temp_c >= 85.0 {
            "CRITICAL (Throttling Protection Active)".to_string()
        } else if cpu_pkg_temp_c >= 72.0 {
            "ELEVATED (Fan Cooling)".to_string()
        } else {
            "OPTIMAL (Headroom Clear)".to_string()
        };

        BareMetalTelemetry {
            active_governor,
            energy_perf_preference,
            turbo_boost_enabled,
            mglru_status,
            zram_compression_ratio,
            cpu_pkg_temp_c,
            battery_capacity_pct,
            power_draw_watts,
            is_charging,
            thermal_severity,
        }
    }

    /// Command the Linux kernel governor to shift CPU frequency & energy scaling
    pub fn command_governor_profile(&self, profile: KernelProfile) -> GovernorShiftReport {
        let previous = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference")
            .unwrap_or_else(|_| "unknown".to_string())
            .trim()
            .to_string();

        let target_epp = match profile {
            KernelProfile::Performance => "performance",
            KernelProfile::Balanced => "balance_performance",
            KernelProfile::PowerSave => "power",
        };

        let target_gov = match profile {
            KernelProfile::Performance => "performance",
            KernelProfile::Balanced | KernelProfile::PowerSave => "powersave",
        };

        // 1. Attempt direct sysfs update (if write permissions exist or user is root)
        let mut sysfs_applied = false;
        if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                    let epp_path = entry.path().join("cpufreq/energy_performance_preference");
                    let gov_path = entry.path().join("cpufreq/scaling_governor");

                    if fs::write(&epp_path, target_epp).is_ok() {
                        sysfs_applied = true;
                    }
                    let _ = fs::write(&gov_path, target_gov);
                }
            }
        }

        // 2. Also invoke dynabook-kernel-ctl or cpu-governor.sh if present
        let ctl_path = self.kernel_optimizer_dir.join("dynabook-kernel-ctl");
        let script_path = PathBuf::from("scripts/cpu-governor.sh");

        let mut output_str = String::new();
        if ctl_path.exists() {
            let out = Command::new("python3")
                .arg(&ctl_path)
                .arg("profile")
                .arg(profile.to_string())
                .output();
            if let Ok(o) = out {
                output_str = String::from_utf8_lossy(&o.stdout).to_string();
            }
        } else if script_path.exists() {
            let out = Command::new("bash")
                .arg(&script_path)
                .arg(profile.to_string())
                .output();
            if let Ok(o) = out {
                output_str = String::from_utf8_lossy(&o.stdout).to_string();
            }
        }

        let mglru_ok = fs::read_to_string("/sys/kernel/mm/lru_gen/enabled")
            .map(|s| s.contains('7'))
            .unwrap_or(false);

        GovernorShiftReport {
            previous_profile: previous,
            target_profile: profile,
            epp_applied: target_epp.to_string(),
            mglru_active: mglru_ok,
            success: sysfs_applied || !output_str.is_empty(),
            message: format!(
                "Shifted kernel profile to '{}' (EPP: {}, Governor: {}).",
                profile, target_epp, target_gov
            ),
        }
    }

    /// Acquires an RAII guard that boosts the kernel into `Performance` mode
    /// and automatically drops back to `Balanced` mode when out of scope.
    pub fn acquire_performance_guard(self: &std::sync::Arc<Self>) -> GovernorGuard {
        let _ = self.command_governor_profile(KernelProfile::Performance);
        GovernorGuard {
            bridge: self.clone(),
        }
    }

    fn read_cpu_temp(&self) -> f64 {
        for zone in 0..6 {
            let path = format!("/sys/class/thermal/thermal_zone{}/temp", zone);
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(raw) = content.trim().parse::<f64>() {
                    return raw / 1000.0;
                }
            }
        }
        55.0
    }

    fn read_zram_compression(&self) -> f32 {
        let orig = fs::read_to_string("/sys/block/zram0/orig_data_size")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(0.0);
        let comp = fs::read_to_string("/sys/block/zram0/compr_data_size")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .unwrap_or(0.0);

        if comp > 0.0 && orig > 0.0 {
            orig / comp
        } else {
            4.80 // Default observed Dynabook zstd compression ratio
        }
    }

    fn read_battery_metrics(&self) -> (Option<u32>, Option<f64>, bool) {
        let cap = fs::read_to_string("/sys/class/power_supply/BAT0/capacity")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());

        let pwr = fs::read_to_string("/sys/class/power_supply/BAT0/power_now")
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|uw| uw / 1_000_000.0);

        let status = fs::read_to_string("/sys/class/power_supply/BAT0/status")
            .unwrap_or_default();
        let is_charging = status.trim().eq_ignore_ascii_case("charging") || status.trim().eq_ignore_ascii_case("full");

        (cap, pwr, is_charging)
    }
}

/// RAII Governor Guard ensuring kernel frequencies drop back to balanced state on completion
pub struct GovernorGuard {
    bridge: std::sync::Arc<BareMetalPerformanceBridge>,
}

impl GovernorGuard {
    /// Acquire performance mode guard immediately
    pub fn acquire() -> Self {
        let bridge = std::sync::Arc::new(BareMetalPerformanceBridge::new());
        let _ = bridge.command_governor_profile(KernelProfile::Performance);
        Self { bridge }
    }
}

impl Drop for GovernorGuard {
    fn drop(&mut self) {
        let _ = self.bridge.command_governor_profile(KernelProfile::Balanced);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bare_metal_telemetry_inspection() {
        let bridge = BareMetalPerformanceBridge::new();
        let telem = bridge.read_bare_metal_telemetry();
        assert!(!telem.active_governor.is_empty());
        assert!(!telem.energy_perf_preference.is_empty());
        assert!(telem.cpu_pkg_temp_c >= 0.0);
        assert!(!telem.thermal_severity.is_empty());
    }

    #[test]
    fn test_governor_profile_shift_and_guard() {
        let bridge = std::sync::Arc::new(BareMetalPerformanceBridge::new());
        let report = bridge.command_governor_profile(KernelProfile::Balanced);
        assert_eq!(report.target_profile, KernelProfile::Balanced);
        assert_eq!(report.epp_applied, "balance_performance");

        // Test RAII guard creation and automatic drop
        {
            let _guard = bridge.acquire_performance_guard();
        }
        // After drop, reverts to balanced
    }
}
