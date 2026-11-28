//! # Real-Time Hardware Telemetry & Kernel Governor Bridge
//!
//! Cooperates with `dynatune` (`crates/hgb-core/src/dynatune_synergy.rs`)
//! and bare-metal script `scripts/cpu-governor.sh`.
//! Ingests `/sys/class/thermal/thermal_zone*/temp`, RAPL power draw,
//! and energy performance preference (EPP).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use hgb_core::dynatune_synergy::{DynatuneSynergyEngine, HardwareSafeguardReport, ThermalSeverity};
use crate::error::{NpuRouterError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernorProfile {
    Performance,
    Balanced,
    Powersave,
}

impl GovernorProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::Powersave => "powersave",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "performance" | "turbo" | "boost" => Self::Performance,
            "powersave" | "battery" | "idle" | "relax" => Self::Powersave,
            _ => Self::Balanced,
        }
    }
}

/// Instantaneous hardware telemetry reading
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveTelemetrySample {
    pub cpu_pkg_temp_c: f32,
    pub peak_temp_c: f32,
    pub thermal_headroom_c: f32,
    pub is_thermal_throttling: bool,
    pub severity: ThermalSeverity,
    pub ac_connected: bool,
    pub active_governor: String,
    pub energy_perf_preference: String,
    pub rapl_power_limit_watts: f32,
    pub timestamp: DateTime<Utc>,
}

/// Bridge to Linux kernel governor & telemetry
pub struct GovernorBridge {
    pub script_path: PathBuf,
    pub state_file: PathBuf,
    pub sysfs_root: PathBuf,
}

impl Default for GovernorBridge {
    fn default() -> Self {
        let script = if Path::new("/home/toshiba/hagibis/scripts/cpu-governor.sh").exists() {
            PathBuf::from("/home/toshiba/hagibis/scripts/cpu-governor.sh")
        } else if Path::new("/media/toshiba/Lexar/hagibis/scripts/cpu-governor.sh").exists() {
            PathBuf::from("/media/toshiba/Lexar/hagibis/scripts/cpu-governor.sh")
        } else {
            PathBuf::from("scripts/cpu-governor.sh")
        };
        Self::new(
            script,
            PathBuf::from("/tmp/hgb_governor_state.json"),
            PathBuf::from("/sys"),
        )
    }
}

impl GovernorBridge {
    pub fn new(script_path: PathBuf, state_file: PathBuf, sysfs_root: PathBuf) -> Self {
        Self {
            script_path,
            state_file,
            sysfs_root,
        }
    }

    /// Ingests live telemetry directly from kernel sysfs
    pub fn poll_telemetry(&self) -> LiveTelemetrySample {
        let mut cpu_pkg_temp_c = 45.0f32;
        let mut peak_temp_c = 45.0f32;

        let thermal_dir = self.sysfs_root.join("class/thermal");
        if let Ok(entries) = fs::read_dir(&thermal_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("thermal_zone") {
                    continue;
                }
                let path = entry.path();
                let zone_type = fs::read_to_string(path.join("type"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let temp_raw = fs::read_to_string(path.join("temp"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f32>().ok())
                    .unwrap_or(0.0);
                let temp_c = if temp_raw > 1000.0 { temp_raw / 1000.0 } else { temp_raw };

                if temp_c > peak_temp_c {
                    peak_temp_c = temp_c;
                }
                if zone_type.contains("pkg_temp") || zone_type.contains("cpu") || zone_type == "acpitz" {
                    cpu_pkg_temp_c = temp_c;
                }
            }
        }

        let tj_max = 100.0f32;
        let thermal_headroom_c = (tj_max - peak_temp_c).max(0.0);
        let severity = if peak_temp_c >= 85.0 {
            ThermalSeverity::Critical
        } else if peak_temp_c >= 75.0 {
            ThermalSeverity::Warm
        } else {
            ThermalSeverity::Normal
        };
        let is_thermal_throttling = severity == ThermalSeverity::Critical || thermal_headroom_c < 15.0;

        let ac_connected = check_ac_status(&self.sysfs_root);
        let active_governor = read_cpu_governor(&self.sysfs_root);
        let energy_perf_preference = read_epp(&self.sysfs_root);
        let rapl_power_limit_watts = read_rapl_limit(&self.sysfs_root);

        LiveTelemetrySample {
            cpu_pkg_temp_c,
            peak_temp_c,
            thermal_headroom_c,
            is_thermal_throttling,
            severity,
            ac_connected,
            active_governor,
            energy_perf_preference,
            rapl_power_limit_watts,
            timestamp: Utc::now(),
        }
    }

    /// Sets CPU governor mode via scripts/cpu-governor.sh or direct state sync
    pub fn set_governor_profile(&self, profile: GovernorProfile) -> Result<()> {
        if self.script_path.exists() {
            let status = Command::new(&self.script_path)
                .arg(profile.as_str())
                .status();
            if let Ok(st) = status {
                if !st.success() {
                    return Err(NpuRouterError::GovernorFailure(format!(
                        "cpu-governor.sh exited with code {:?}", st.code()
                    )));
                }
            }
        }

        let timestamp = Utc::now().to_rfc3339();
        let payload = serde_json::json!({
            "active_profile": profile.as_str(),
            "updated_at": timestamp
        });
        let _ = fs::write(&self.state_file, payload.to_string());
        Ok(())
    }

    /// Reads recorded governor state from /tmp/hgb_governor_state.json
    pub fn read_state_profile(&self) -> GovernorProfile {
        if let Ok(content) = fs::read_to_string(&self.state_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(prof_str) = val.get("active_profile").and_then(|v| v.as_str()) {
                    return GovernorProfile::from_str_loose(prof_str);
                }
            }
        }
        GovernorProfile::Balanced
    }

    /// Interacts with DynatuneSynergyEngine
    pub fn sync_with_dynatune(&self, engine: &DynatuneSynergyEngine) -> HardwareSafeguardReport {
        engine.evaluate_hardware_telemetry()
    }
}

fn check_ac_status(sysfs_root: &Path) -> bool {
    let power_supply = sysfs_root.join("class/power_supply");
    if let Ok(entries) = fs::read_dir(power_supply) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("AC") || name.starts_with("ADP") {
                if let Ok(online) = fs::read_to_string(entry.path().join("online")) {
                    if online.trim() == "1" {
                        return true;
                    }
                }
            }
        }
    }
    true // Default assume AC
}

fn read_cpu_governor(sysfs_root: &Path) -> String {
    let gov_path = sysfs_root.join("devices/system/cpu/cpu0/cpufreq/scaling_governor");
    fs::read_to_string(gov_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "powersave".to_string())
}

fn read_epp(sysfs_root: &Path) -> String {
    let epp_path = sysfs_root.join("devices/system/cpu/cpu0/cpufreq/energy_performance_preference");
    fs::read_to_string(epp_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "balance_performance".to_string())
}

fn read_rapl_limit(sysfs_root: &Path) -> f32 {
    let rapl_constraint = sysfs_root.join("class/powercap/intel-rapl:0/constraint_0_power_limit_uw");
    fs::read_to_string(rapl_constraint)
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|uw| uw / 1_000_000.0)
        .unwrap_or(28.0)
}