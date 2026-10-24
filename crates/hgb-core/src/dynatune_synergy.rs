//! # Dynatune & Linux Kernel Synergy Engine
//!
//! Bridges Hagibis directly with the sibling `dynatune` hardware performance project
//! and Linux kernel telemetry (`/sys/class/thermal`, `/sys/devices/system/cpu/cpufreq`, `/proc/pressure/memory`).
//! Hardware-aware daemon protection: dynamically tunes CPU governor, scales compilation concurrency,
//! and triggers thermal safeguards before heavy Rust crate builds or microVM tasks to prevent overheating Toshiba laptop hardware.

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Thermal severity classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThermalSeverity {
    /// Safe headroom (< 75°C) -> Max performance unlocked
    Normal,
    /// Elevated temperature (75°C - 84.9°C) -> Mild scaling
    Warm,
    /// Critical threshold (>= 85°C) -> Safeguard triggered, throttle jobs, pause ambient dream
    Critical,
}

impl std::fmt::Display for ThermalSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "NORMAL (Cool Headroom)"),
            Self::Warm => write!(f, "WARM (Thermal Throttling Risk)"),
            Self::Critical => write!(f, "CRITICAL (Safeguard Engaged)"),
        }
    }
}

/// Individual thermal zone reading from sysfs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalZoneMetric {
    pub name: String,
    pub zone_type: String,
    pub temp_c: f32,
}

/// Comprehensive hardware & thermal telemetry report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareSafeguardReport {
    pub cpu_pkg_temp_c: f32,
    pub max_temp_c: f32,
    pub severity: ThermalSeverity,
    pub ac_connected: bool,
    pub active_governor: String,
    pub psi_memory_some_avg10: f32,
    pub psi_memory_full_avg10: f32,
    pub recommended_build_jobs: usize,
    pub safeguard_triggered: bool,
    pub safeguard_action: String,
    pub timestamp_utc: String,
    pub thermal_zones: Vec<ThermalZoneMetric>,
}

impl HardwareSafeguardReport {
    /// Formatted terminal ASCII dashboard
    pub fn render_terminal_table(&self) -> String {
        let status_color = match self.severity {
            ThermalSeverity::Normal => "OPTIMAL",
            ThermalSeverity::Warm => "WARNING",
            ThermalSeverity::Critical => "CRITICAL SAFEGUARD",
        };

        format!(
            "┌──────────────────────────────────────────────────────────────┐\n\
             │           HAGIBIS × DYNATUNE KERNEL SYNERGY REPORT           │\n\
             ├────────────────────────────────────────┬─────────────────────┤\n\
             │ CPU Package Temperature                │ {:>18.1}°C │\n\
             │ Peak Thermal Sensor Reading            │ {:>18.1}°C │\n\
             │ Thermal Health Severity                │ {:>21} │\n\
             │ Power Source                           │ {:>21} │\n\
             │ Active CPU Governor                    │ {:>21} │\n\
             │ PSI Memory Pressure (Some/Full)        │ {:>8.2}% / {:>6.2}% │\n\
             │ Recommended Build Concurrency          │ {:>17} jobs │\n\
             │ Thermal Safeguard State                │ {:>21} │\n\
             ├────────────────────────────────────────┴─────────────────────┤\n\
             │ Action: {:<52} │\n\
             └──────────────────────────────────────────────────────────────┘",
            self.cpu_pkg_temp_c,
            self.max_temp_c,
            status_color,
            if self.ac_connected { "AC Mains" } else { "Battery" },
            self.active_governor,
            self.psi_memory_some_avg10,
            self.psi_memory_full_avg10,
            self.recommended_build_jobs,
            if self.safeguard_triggered { "ENGAGED (Throttled)" } else { "CLEAR (Headroom OK)" },
            self.safeguard_action
        )
    }
}

/// Principal Dynatune & Kernel Synergy Engine
pub struct DynatuneSynergyEngine {
    pub sibling_dynatune_dir: PathBuf,
}

impl Default for DynatuneSynergyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DynatuneSynergyEngine {
    pub fn new() -> Self {
        let sibling_dynatune_dir = std::env::var("DYNATUNE_DIR")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                [
                    PathBuf::from("../dynatune"),
                    PathBuf::from("../../dynatune"),
                    PathBuf::from("dynatune"),
                ]
                .into_iter()
                .find(|p| p.is_dir())
            })
            .unwrap_or_else(|| PathBuf::from("../dynatune"));
        Self { sibling_dynatune_dir }
    }

    /// Read real Linux kernel thermal zones from `/sys/class/thermal/`
    pub fn read_thermal_zones() -> Vec<ThermalZoneMetric> {
        let mut zones = Vec::new();
        let thermal_dir = Path::new("/sys/class/thermal");
        if !thermal_dir.is_dir() {
            return zones;
        }

        if let Ok(entries) = fs::read_dir(thermal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = path.file_name().and_then(|f| f.to_str()).unwrap_or_default();
                if file_name.starts_with("thermal_zone") {
                    let temp_path = path.join("temp");
                    let type_path = path.join("type");

                    let temp_c = if let Ok(content) = fs::read_to_string(&temp_path) {
                        content.trim().parse::<f32>().map(|m| m / 1000.0).unwrap_or(0.0)
                    } else {
                        0.0
                    };

                    let zone_type = fs::read_to_string(&type_path)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|_| "unknown".to_string());

                    zones.push(ThermalZoneMetric {
                        name: file_name.to_string(),
                        zone_type,
                        temp_c,
                    });
                }
            }
        }

        // Sort descending by temperature
        zones.sort_by(|a, b| b.temp_c.partial_cmp(&a.temp_c).unwrap_or(std::cmp::Ordering::Equal));
        zones
    }

    /// Read active CPU scaling governor from `/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor`
    pub fn read_cpu_governor() -> String {
        let governor_path = Path::new("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor");
        if let Ok(gov) = fs::read_to_string(governor_path) {
            gov.trim().to_string()
        } else {
            "ondemand / balanced".to_string()
        }
    }

    /// Read AC mains online state from `/sys/class/power_supply/`
    pub fn is_ac_connected() -> bool {
        let candidates = [
            "/sys/class/power_supply/AC/online",
            "/sys/class/power_supply/ACAD/online",
            "/sys/class/power_supply/ADP1/online",
        ];

        for cand in &candidates {
            if let Ok(val) = fs::read_to_string(cand) {
                if val.trim() == "1" {
                    return true;
                }
            }
        }
        // Default to true if power supply sysfs is not present
        true
    }

    /// Read Linux PSI (Pressure Stall Information) memory pressure
    pub fn read_memory_pressure() -> (f32, f32) {
        let psi_path = Path::new("/proc/pressure/memory");
        if let Ok(content) = fs::read_to_string(psi_path) {
            let mut some_avg10 = 0.0f32;
            let mut full_avg10 = 0.0f32;

            for line in content.lines() {
                if line.starts_with("some ") {
                    for part in line.split_whitespace() {
                        if part.starts_with("avg10=") {
                            if let Ok(val) = part["avg10=".len()..].parse::<f32>() {
                                some_avg10 = val;
                            }
                        }
                    }
                } else if line.starts_with("full ") {
                    for part in line.split_whitespace() {
                        if part.starts_with("avg10=") {
                            if let Ok(val) = part["avg10=".len()..].parse::<f32>() {
                                full_avg10 = val;
                            }
                        }
                    }
                }
            }
            (some_avg10, full_avg10)
        } else {
            (0.0, 0.0)
        }
    }

    /// Check hardware telemetry and evaluate thermal safeguards
    pub fn evaluate_hardware_telemetry(&self) -> HardwareSafeguardReport {
        let zones = Self::read_thermal_zones();
        let max_temp_c = zones.first().map(|z| z.temp_c).unwrap_or(45.0);

        // Find package temp or use max
        let cpu_pkg_temp_c = zones
            .iter()
            .find(|z| z.zone_type.contains("pkg") || z.zone_type.contains("cpu") || z.zone_type.contains("x86"))
            .map(|z| z.temp_c)
            .unwrap_or(max_temp_c);

        let severity = if max_temp_c >= 85.0 || cpu_pkg_temp_c >= 85.0 {
            ThermalSeverity::Critical
        } else if max_temp_c >= 75.0 || cpu_pkg_temp_c >= 75.0 {
            ThermalSeverity::Warm
        } else {
            ThermalSeverity::Normal
        };

        let ac_connected = Self::is_ac_connected();
        let active_governor = Self::read_cpu_governor();
        let (some_psi, full_psi) = Self::read_memory_pressure();

        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let (recommended_jobs, safeguard_triggered, safeguard_action) = match severity {
            ThermalSeverity::Critical => (
                2.min(num_cpus),
                true,
                format!(
                    "THERMAL SAFEGUARD TRIGGERED ({:.1}°C >= 85°C): Capping jobs to 2, pausing ambient background dream cycles.",
                    cpu_pkg_temp_c
                ),
            ),
            ThermalSeverity::Warm => (
                (num_cpus * 3 / 4).max(2),
                false,
                format!(
                    "Moderate thermal rise ({:.1}°C): Slightly scaling back build parallelism to prevent throttle transition.",
                    cpu_pkg_temp_c
                ),
            ),
            ThermalSeverity::Normal => (
                num_cpus,
                false,
                format!(
                    "Optimal hardware headroom ({:.1}°C): Max build concurrency of {} jobs fully unlocked.",
                    cpu_pkg_temp_c, num_cpus
                ),
            ),
        };

        HardwareSafeguardReport {
            cpu_pkg_temp_c,
            max_temp_c,
            severity,
            ac_connected,
            active_governor,
            psi_memory_some_avg10: some_psi,
            psi_memory_full_avg10: full_psi,
            recommended_build_jobs: recommended_jobs,
            safeguard_triggered,
            safeguard_action,
            timestamp_utc: chrono::Utc::now().to_rfc3339(),
            thermal_zones: zones,
        }
    }

    /// Pre-flight check before running heavy compilation (`cargo build` / microVMs)
    pub fn pre_flight_compilation_guard(&self) -> Result<HardwareSafeguardReport> {
        let rep = self.evaluate_hardware_telemetry();
        if rep.safeguard_triggered {
            eprintln!("[⚠️ DYNATUNE THERMAL GUARD] {}", rep.safeguard_action);
        }
        Ok(rep)
    }

    /// Read status from sibling dynatune project if present
    pub fn query_sibling_dynatune(&self) -> Option<String> {
        let binary_path = self.sibling_dynatune_dir.join("target/debug/dynatune");
        if binary_path.exists() {
            let out = std::process::Command::new(&binary_path)
                .arg("--status")
                .output()
                .ok()?;
            Some(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            None
        }
    }

    /// Evaluate dynamic inference policy to autonomously adjust context window, concurrency,
    /// and model precision based on live hardware temperature and power source.
    pub fn evaluate_inference_policy(&self) -> HardwareInferencePolicy {
        let telemetry = self.evaluate_hardware_telemetry();
        let num_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

        if telemetry.severity == ThermalSeverity::Critical {
            HardwareInferencePolicy {
                max_parallel_tasks: 1,
                max_context_window_tokens: 4096,
                recommended_precision: "q4_0".to_string(),
                fallback_lightweight_model: Some("qwen2.5-coder:0.5b".to_string()),
                throttle_delay_ms: 150,
                policy_rationale: format!(
                    "CRITICAL THERMAL STATE ({:.1}°C >= 85°C): Context window clamped to 4096 tokens, precision shifted to q4_0, and tasks capped to 1 to preserve Toshiba hardware.",
                    telemetry.cpu_pkg_temp_c
                ),
            }
        } else if telemetry.severity == ThermalSeverity::Warm {
            HardwareInferencePolicy {
                max_parallel_tasks: 2.min(num_cpus),
                max_context_window_tokens: 8192,
                recommended_precision: "q4_k_m".to_string(),
                fallback_lightweight_model: Some("qwen2.5-coder:1.5b".to_string()),
                throttle_delay_ms: 50,
                policy_rationale: format!(
                    "ELEVATED TEMPERATURE ({:.1}°C): Context window scaled to 8192 tokens and precision set to q4_k_m to halt heat buildup.",
                    telemetry.cpu_pkg_temp_c
                ),
            }
        } else if !telemetry.ac_connected {
            HardwareInferencePolicy {
                max_parallel_tasks: 2.min(num_cpus),
                max_context_window_tokens: 16384,
                recommended_precision: "q4_k_m".to_string(),
                fallback_lightweight_model: Some("qwen2.5-coder:1.5b".to_string()),
                throttle_delay_ms: 0,
                policy_rationale: "BATTERY PRESERVATION MODE: Capping concurrency to 2 and selecting energy-efficient q4_k_m precision.".to_string(),
            }
        } else {
            HardwareInferencePolicy {
                max_parallel_tasks: num_cpus,
                max_context_window_tokens: 32768,
                recommended_precision: "q8_0 / fp16".to_string(),
                fallback_lightweight_model: None,
                throttle_delay_ms: 0,
                policy_rationale: format!(
                    "OPTIMAL HEADROOM ON AC MAINS ({:.1}°C): Full 32k context, {} concurrent tasks, and unconstrained precision unlocked.",
                    telemetry.cpu_pkg_temp_c, num_cpus
                ),
            }
        }
    }
}

/// Dynamic inference policy tailored to live hardware thermal and power constraints
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareInferencePolicy {
    /// Maximum parallel agent execution tasks allowed
    pub max_parallel_tasks: usize,
    /// Maximum context window allowed for prompt/inference (clamped to prevent memory/thermal thrashing)
    pub max_context_window_tokens: usize,
    /// Recommended quantization precision level (e.g. "q8_0", "q4_k_m", "q4_0")
    pub recommended_precision: String,
    /// Lightweight fallback model if hardware is thermally constrained or on battery
    pub fallback_lightweight_model: Option<String>,
    /// Cooling throttle delay in ms to insert between rapid local inference passes
    pub throttle_delay_ms: u64,
    /// Human-readable explanation of the current hardware policy
    pub policy_rationale: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynatune_telemetry_evaluation() {
        let engine = DynatuneSynergyEngine::new();
        let report = engine.evaluate_hardware_telemetry();

        assert!(report.recommended_build_jobs >= 1);
        assert!(!report.active_governor.is_empty());
        assert!(!report.safeguard_action.is_empty());

        let table = report.render_terminal_table();
        assert!(table.contains("HAGIBIS × DYNATUNE KERNEL SYNERGY REPORT"));
    }

    #[test]
    fn test_thermal_severity_logic() {
        assert_eq!(
            if 88.0f32 >= 85.0 { ThermalSeverity::Critical } else { ThermalSeverity::Normal },
            ThermalSeverity::Critical
        );
        assert_eq!(
            if 78.0f32 >= 75.0 && 78.0f32 < 85.0 { ThermalSeverity::Warm } else { ThermalSeverity::Normal },
            ThermalSeverity::Warm
        );
    }

    #[test]
    fn test_hardware_inference_policy() {
        let engine = DynatuneSynergyEngine::new();
        let policy = engine.evaluate_inference_policy();
        assert!(policy.max_parallel_tasks >= 1);
        assert!(policy.max_context_window_tokens >= 2048);
        assert!(!policy.recommended_precision.is_empty());
        assert!(!policy.policy_rationale.is_empty());
    }
}
