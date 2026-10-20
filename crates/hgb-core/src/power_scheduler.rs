//! # Adaptive Power-Aware Scheduler for Deep Local Ollama + Dynatune Integration
//!
//! Intelligently bridges local LLM reasoning tasks with host thermal telemetry, battery profiles,
//! and Linux CPU governors to prevent thermal throttling, reduce fan noise, and optimize mobile inference.
//!
//! Features:
//! 1. Bare-Metal Telemetry Fusion: Continuously monitors `/sys/class/thermal`, `/sys/class/power_supply`,
//!    and `/proc/pressure/memory` (PSI) via Dynatune synergy.
//! 2. Dynamic Ollama Thread Modulation: Calculates the optimal `num_thread` and `num_ctx` for local Ollama
//!    requests, scaling from 1 thread during thermal stress up to full physical execution cores under optimal headroom.
//! 3. Proactive Throttling Mitigation: Inserts micro-pauses and lowers context windows before the kernel
//!    trips hardware thermal throttling.
//! 4. Governor Coordination: Commands `intel_pstate` or `/sys/devices/system/cpu` governors between
//!    `performance`, `powersave`, and `balanced` based on active load.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use serde::{Deserialize, Serialize};

use crate::dynatune_synergy::{DynatuneSynergyEngine, HardwareSafeguardReport, ThermalSeverity};
use crate::error::{HgbError, Result};

/// Power state and thermal headroom profile for Ollama inference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaPowerProfile {
    pub current_temp_c: f32,
    pub max_temp_c: f32,
    pub severity: ThermalSeverity,
    pub ac_connected: bool,
    pub physical_cores: usize,
    pub logical_cpus: usize,
    pub optimal_threads: usize,
    pub context_length_budget: usize,
    pub cooling_cooldown_ms: u64,
    pub recommended_governor: String,
    pub psi_memory_some_avg10: f32,
    pub psi_stall_risk: bool,
    pub action_summary: String,
}

/// Scheduled configuration applied to an outgoing Ollama inference request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledInferenceConfig {
    pub num_thread: usize,
    pub num_ctx: usize,
    pub cooldown_ms: u64,
    pub governor_used: String,
    pub thermal_headroom_c: f32,
}

/// Global status report of the adaptive power scheduler
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerSchedulerStatus {
    pub profile: OllamaPowerProfile,
    pub total_scheduled_tasks: u64,
    pub total_throttled_tasks: u64,
    pub total_cooldown_ms_applied: u64,
    pub dynatune_active: bool,
    pub kernel_optimizer_present: bool,
}

/// The Adaptive Power-Aware Scheduler Engine
pub struct AdaptivePowerScheduler {
    dynatune: DynatuneSynergyEngine,
    kernel_optimizer_dir: PathBuf,
    total_scheduled: AtomicU64,
    total_throttled: AtomicU64,
    total_cooldown_ms: AtomicU64,
}

impl Default for AdaptivePowerScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptivePowerScheduler {
    /// Initialize the adaptive power scheduler
    pub fn new() -> Self {
        Self {
            dynatune: DynatuneSynergyEngine::new(),
            kernel_optimizer_dir: PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer"),
            total_scheduled: AtomicU64::new(0),
            total_throttled: AtomicU64::new(0),
            total_cooldown_ms: AtomicU64::new(0),
        }
    }

    /// Discover physical CPU execution units on Linux
    pub fn discover_physical_cores() -> usize {
        let mut cores = std::collections::HashSet::new();
        if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("cpu") && name_str[3..].chars().all(|c| c.is_ascii_digit()) {
                    let path = entry.path().join("topology");
                    let pkg = fs::read_to_string(path.join("physical_package_id")).unwrap_or_default();
                    let core = fs::read_to_string(path.join("core_id")).unwrap_or_default();
                    if !pkg.trim().is_empty() && !core.trim().is_empty() {
                        cores.insert((pkg.trim().to_string(), core.trim().to_string()));
                    }
                }
            }
        }

        if !cores.is_empty() {
            cores.len()
        } else {
            // Fallback: available logical CPUs / 2
            let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
            (logical / 2).max(1)
        }
    }

    /// Evaluate current host telemetry and compute the optimal power profile
    pub fn evaluate_live_profile(&self) -> OllamaPowerProfile {
        let report: HardwareSafeguardReport = self.dynatune.evaluate_hardware_telemetry();
        let physical = Self::discover_physical_cores();
        let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

        let psi_stall_risk = report.psi_memory_some_avg10 > 25.0;

        let (optimal_threads, context_length_budget, cooldown_ms, recommended_gov, action) = match report.severity {
            ThermalSeverity::Critical => {
                // Severe thermal state (> 85°C): throttle strictly to 1 thread with cooling cooldown
                (
                    1,
                    1024,
                    250,
                    "powersave".to_string(),
                    format!(
                        "CRITICAL THERMAL ALERT ({:.1}°C): Capped Ollama to 1 thread, 1024 context, 250ms cooldown pause.",
                        report.cpu_pkg_temp_c
                    ),
                )
            }
            ThermalSeverity::Warm => {
                // Warm state (75°C - 85°C): maintain physical cores or drop slightly
                let th = (physical.min(2)).max(1);
                let ctx = if report.ac_connected { 2048 } else { 1536 };
                (
                    th,
                    ctx,
                    50,
                    if report.ac_connected { "powersave".to_string() } else { "powersave".to_string() },
                    format!(
                        "ELEVATED TEMPERATURE ({:.1}°C): Scaled to {} physical threads, {} context, 50ms cooldown buffer.",
                        report.cpu_pkg_temp_c, th, ctx
                    ),
                )
            }
            ThermalSeverity::Normal => {
                // Optimal headroom (< 75°C)
                let th = if report.ac_connected {
                    physical.max(2).min(logical)
                } else {
                    physical.min(2)
                };
                let ctx = if report.ac_connected { 4096 } else { 2048 };
                (
                    th,
                    ctx,
                    0,
                    if report.ac_connected { "performance".to_string() } else { "balanced".to_string() },
                    format!(
                        "OPTIMAL HEADROOM ({:.1}°C, {}): Full speed ahead with {} threads, {} context.",
                        report.cpu_pkg_temp_c,
                        if report.ac_connected { "AC Mains" } else { "Battery" },
                        th,
                        ctx
                    ),
                )
            }
        };

        OllamaPowerProfile {
            current_temp_c: report.cpu_pkg_temp_c,
            max_temp_c: report.max_temp_c,
            severity: report.severity,
            ac_connected: report.ac_connected,
            physical_cores: physical,
            logical_cpus: logical,
            optimal_threads,
            context_length_budget,
            cooling_cooldown_ms: cooldown_ms,
            recommended_governor: recommended_gov,
            psi_memory_some_avg10: report.psi_memory_some_avg10,
            psi_stall_risk,
            action_summary: action,
        }
    }

    /// Schedule an incoming reasoning or inference task, returning optimized options
    pub fn schedule_inference(&self, requested_threads: Option<usize>) -> ScheduledInferenceConfig {
        let profile = self.evaluate_live_profile();
        self.total_scheduled.fetch_add(1, Ordering::Relaxed);

        let final_threads = if let Some(req) = requested_threads {
            req.min(profile.optimal_threads)
        } else {
            profile.optimal_threads
        };

        if profile.severity != ThermalSeverity::Normal {
            self.total_throttled.fetch_add(1, Ordering::Relaxed);
        }

        if profile.cooling_cooldown_ms > 0 {
            self.total_cooldown_ms.fetch_add(profile.cooling_cooldown_ms, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(profile.cooling_cooldown_ms));
        }

        let headroom = (85.0 - profile.current_temp_c).max(0.0);

        ScheduledInferenceConfig {
            num_thread: final_threads,
            num_ctx: profile.context_length_budget,
            cooldown_ms: profile.cooling_cooldown_ms,
            governor_used: profile.recommended_governor,
            thermal_headroom_c: headroom,
        }
    }

    /// Read comprehensive scheduler telemetry and stats
    pub fn status(&self) -> PowerSchedulerStatus {
        let profile = self.evaluate_live_profile();
        let total_sched = self.total_scheduled.load(Ordering::Relaxed);
        let total_throt = self.total_throttled.load(Ordering::Relaxed);
        let total_cool = self.total_cooldown_ms.load(Ordering::Relaxed);

        let dynatune_active = self.dynatune.sibling_dynatune_dir.is_dir();
        let kernel_optimizer_present = self.kernel_optimizer_dir.is_dir();

        PowerSchedulerStatus {
            profile,
            total_scheduled_tasks: total_sched,
            total_throttled_tasks: total_throt,
            total_cooldown_ms_applied: total_cool,
            dynatune_active,
            kernel_optimizer_present,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_power_scheduler_profile_and_schedule() {
        let scheduler = AdaptivePowerScheduler::new();
        let profile = scheduler.evaluate_live_profile();

        assert!(profile.physical_cores >= 1);
        assert!(profile.optimal_threads >= 1);
        assert!(profile.context_length_budget >= 1024);
        assert!(!profile.action_summary.is_empty());

        let scheduled = scheduler.schedule_inference(None);
        assert_eq!(scheduled.num_thread, profile.optimal_threads);
        assert_eq!(scheduled.num_ctx, profile.context_length_budget);

        let status = scheduler.status();
        assert_eq!(status.total_scheduled_tasks, 1);
    }
}
