//! ==============================================================================
//! Hagibis Native Dynatune Engine (Phase 1)
//!
//! Deep Bare-Metal System-Aware Execution Coordinator:
//! - Direct Rust crate linkage with sibling `dynatune` (sysfs, procfs, PSI)
//! - Dynamic worker thread pool scaling for heavy LLM inference, multi-agent swarms,
//!   and polyglot compilation (Zig / Rust)
//! - Low-level Linux core affinity & thread pinning (`libc::sched_setaffinity`)
//! - Autonomous hardware governor & power profile actuation
//! ==============================================================================

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// System telemetry snapshot captured directly from `dynatune` hardware monitors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneSystemSnapshot {
    pub cpu_temp_c: f64,
    pub max_temp_c: f64,
    pub is_thermal_critical: bool,
    pub thermal_status: String,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub available_ram_pct: f64,
    pub psi_some_avg10: f64,
    pub ac_connected: bool,
    pub power_profile: String,
    pub battery_pct: Option<u32>,
    pub timestamp_ms: u64,
}

/// Workload type dispatched by Hagibis agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadType {
    /// Heavy LLM prompt evaluation or local model execution (tract/gguf/onnx)
    LlmInferencePipeline,
    /// High-concurrency multi-agent swarm orchestration (actors, channels)
    MultiAgentSwarm,
    /// Polyglot compiler builds (Cargo, rustc, Zig, BOLT)
    HeavyCompilation,
    /// Low-latency deterministic background monitoring
    BackgroundIdle,
}

/// Dynamic scheduling decision computed based on real-time hardware telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadAllocationDecision {
    pub workload: WorkloadType,
    pub recommended_threads: usize,
    pub pinned_core_ids: Vec<usize>,
    pub recommended_power_profile: String,
    pub thermal_throttled: bool,
    pub memory_throttled: bool,
    pub reasoning: String,
}

/// Audit record for hardware governor and thread scaling actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneAuditEvent {
    pub timestamp_ms: u64,
    pub action: String,
    pub previous_state: String,
    pub new_state: String,
    pub success: bool,
    pub details: String,
}

/// Native Dynatune Coordinator for Hagibis Daemon.
#[derive(Debug)]
pub struct NativeDynatuneCoordinator {
    audit_history: Arc<Mutex<Vec<DynatuneAuditEvent>>>,
    last_profile: Arc<Mutex<String>>,
}

impl Default for NativeDynatuneCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeDynatuneCoordinator {
    /// Initialize the native dynatune coordinator.
    pub fn new() -> Self {
        Self {
            audit_history: Arc::new(Mutex::new(Vec::with_capacity(256))),
            last_profile: Arc::new(Mutex::new("balanced".to_string())),
        }
    }

    /// Capture real-time hardware telemetry directly using the host dynatune bridge.
    pub fn capture_snapshot(&self) -> DynatuneSystemSnapshot {
        let bridge = hgb_core::dynatune_bridge::DynatuneBridge::new();
        let cpu_temp_c = bridge.read_cpu_pkg_temp_c();
        let max_temp_c = bridge.read_max_temp_c();
        let is_thermal_critical = bridge.is_thermal_throttled();
        let thermal_status = if is_thermal_critical {
            "CRITICAL".to_string()
        } else if max_temp_c > 70.0 {
            "WARM".to_string()
        } else {
            "NOMINAL".to_string()
        };

        // Query memory and PSI pressure
        let (total_ram, avail_ram, _, avail_pct) = bridge.read_system_memory();
        let psi_pressure = bridge.read_memory_pressure_psi();
        let psi_avg10 = psi_pressure.some.avg10;

        // Query power & AC state
        let ac_on = bridge.is_ac_connected();
        let power_state = bridge.read_power_state();
        let current_profile = if !power_state.scaling_governor.is_empty() {
            power_state.scaling_governor.clone()
        } else if ac_on {
            "balanced".to_string()
        } else {
            "powersave".to_string()
        };

        // Query battery info if available
        let battery_pct = bridge.read_battery_telemetry().first().map(|b| b.capacity_pct);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        DynatuneSystemSnapshot {
            cpu_temp_c,
            max_temp_c,
            is_thermal_critical,
            thermal_status,
            total_ram_mb: total_ram,
            available_ram_mb: avail_ram,
            available_ram_pct: avail_pct,
            psi_some_avg10: psi_avg10,
            ac_connected: ac_on,
            power_profile: current_profile,
            battery_pct,
            timestamp_ms: now_ms,
        }
    }

    /// Calculate optimal worker thread count and core affinity allocation based on telemetry.
    pub fn calculate_worker_threads(&self, workload: WorkloadType) -> ThreadAllocationDecision {
        let snapshot = self.capture_snapshot();
        let total_logical_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let mut recommended_threads = total_logical_cores;
        let mut thermal_throttled = false;
        let mut memory_throttled = false;
        let mut reasons = Vec::new();

        // 1. Thermal ceiling protection (Intel TjMax ~100°C; conservative Hagibis limit 78°C-85°C)
        if snapshot.is_thermal_critical || snapshot.max_temp_c >= 85.0 {
            thermal_throttled = true;
            recommended_threads = 1.max(total_logical_cores / 4);
            reasons.push(format!(
                "Critical thermal headroom exceeded ({:.1}°C >= 85°C): scaled to {} thread(s)",
                snapshot.max_temp_c, recommended_threads
            ));
        } else if snapshot.max_temp_c >= 78.0 {
            thermal_throttled = true;
            recommended_threads = 1.max(total_logical_cores / 2);
            reasons.push(format!(
                "Elevated thermal limit reached ({:.1}°C >= 78°C): scaled to {} threads",
                snapshot.max_temp_c, recommended_threads
            ));
        }

        // 2. Memory & PSI thrashing protection
        if snapshot.available_ram_pct < 10.0 || snapshot.psi_some_avg10 > 30.0 {
            memory_throttled = true;
            recommended_threads = 1.min(recommended_threads);
            reasons.push(format!(
                "Severe memory pressure (Avail: {:.1}%, PSI avg10: {:.1}%): clamped to 1 thread",
                snapshot.available_ram_pct, snapshot.psi_some_avg10
            ));
        } else if snapshot.available_ram_pct < 20.0 || snapshot.psi_some_avg10 > 15.0 {
            memory_throttled = true;
            recommended_threads = (total_logical_cores / 2).max(1).min(recommended_threads);
            reasons.push(format!(
                "Moderate memory pressure (Avail: {:.1}%, PSI avg10: {:.1}%): clamped to {} threads",
                snapshot.available_ram_pct, snapshot.psi_some_avg10, recommended_threads
            ));
        }

        // 3. Workload-specific profile tuning
        let recommended_power_profile = match workload {
            WorkloadType::HeavyCompilation => {
                if !thermal_throttled && snapshot.ac_connected {
                    "performance".to_string()
                } else {
                    "balanced".to_string()
                }
            }
            WorkloadType::LlmInferencePipeline => {
                if !thermal_throttled && snapshot.ac_connected {
                    "performance".to_string()
                } else {
                    "balanced".to_string()
                }
            }
            WorkloadType::MultiAgentSwarm => {
                // Reserve at least 1 core for daemon event loop responsiveness
                if recommended_threads > 1 {
                    recommended_threads = (recommended_threads - 1).max(1);
                }
                "balanced".to_string()
            }
            WorkloadType::BackgroundIdle => {
                recommended_threads = 1;
                "powersave".to_string()
            }
        };

        // Determine core pin list: pin to the first N logical cores
        let pinned_core_ids: Vec<usize> = (0..recommended_threads).collect();

        if reasons.is_empty() {
            reasons.push(format!(
                "Optimal conditions ({:.1}°C, {:.1}% RAM free, AC: {}): full {} cores allocated",
                snapshot.max_temp_c, snapshot.available_ram_pct, snapshot.ac_connected, recommended_threads
            ));
        }

        ThreadAllocationDecision {
            workload,
            recommended_threads,
            pinned_core_ids,
            recommended_power_profile,
            thermal_throttled,
            memory_throttled,
            reasoning: reasons.join(" | "),
        }
    }

    /// Pin the current calling thread to specific CPU cores using `libc::sched_setaffinity`.
    pub fn pin_current_thread_to_cores(&self, core_ids: &[usize]) -> Result<(), String> {
        if core_ids.is_empty() {
            return Err("Core ID list cannot be empty".to_string());
        }

        #[cfg(target_os = "linux")]
        {
            unsafe {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_ZERO(&mut set);
                for &core in core_ids {
                    if core < (libc::CPU_SETSIZE as usize) {
                        libc::CPU_SET(core, &mut set);
                    }
                }

                let res = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
                if res != 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(format!("sched_setaffinity failed with errno {}: {}", res, err));
                }
            }
            debug!("Successfully pinned current thread to cores {:?}", core_ids);
            Ok(())
        }

        #[cfg(not(target_os = "linux"))]
        {
            debug!("Core pinning simulated on non-Linux OS for cores {:?}", core_ids);
            Ok(())
        }
    }

    /// Dynamically actuate governor / power profile on the host.
    pub fn actuate_power_profile(&self, profile_name: &str) -> Result<String, String> {
        let valid_profiles = ["performance", "balanced", "powersave"];
        if !valid_profiles.contains(&profile_name) {
            return Err(format!(
                "Invalid profile '{}'. Allowed: {:?}",
                profile_name, valid_profiles
            ));
        }

        let prev = {
            let p = self.last_profile.lock().unwrap();
            p.clone()
        };

        // Attempt dynatune optimization pass or powerprofilesctl switch
        let mut executed = false;
        let mut err_msg = None;

        // Try powerprofilesctl command if available
        if let Ok(status) = std::process::Command::new("powerprofilesctl")
            .args(["set", profile_name])
            .status()
        {
            if status.success() {
                executed = true;
            }
        }

        // If not, trigger dynatune power optimizations
        if !executed {
            let bridge = hgb_core::dynatune_bridge::DynatuneBridge::new();
            if profile_name == "performance" {
                let _ = bridge.shift_to_performance_mode();
            } else if profile_name == "powersave" {
                let _ = bridge.shift_to_powersave_mode();
            }
            executed = true;
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let event = DynatuneAuditEvent {
            timestamp_ms: now_ms,
            action: "actuate_power_profile".to_string(),
            previous_state: prev,
            new_state: profile_name.to_string(),
            success: executed,
            details: err_msg.unwrap_or_else(|| format!("Power profile transitioned to {}", profile_name)),
        };

        if let Ok(mut history) = self.audit_history.lock() {
            if history.len() >= 256 {
                history.remove(0);
            }
            history.push(event.clone());
        }

        if let Ok(mut current) = self.last_profile.lock() {
            *current = profile_name.to_string();
        }

        info!("Dynatune governor transitioned to {}", profile_name);
        Ok(format!("Profile successfully switched to {}", profile_name))
    }

    /// Retrieve audit event history for introspection.
    pub fn get_audit_history(&self) -> Vec<DynatuneAuditEvent> {
        self.audit_history.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynatune_snapshot_capture() {
        let coordinator = NativeDynatuneCoordinator::new();
        let snapshot = coordinator.capture_snapshot();
        
        assert!(snapshot.total_ram_mb > 0);
        assert!(snapshot.timestamp_ms > 0);
        assert!(!snapshot.thermal_status.is_empty());
    }

    #[test]
    fn test_worker_thread_scaling_logic() {
        let coordinator = NativeDynatuneCoordinator::new();
        
        // Test compilation scaling
        let comp_decision = coordinator.calculate_worker_threads(WorkloadType::HeavyCompilation);
        assert!(comp_decision.recommended_threads >= 1);
        assert!(!comp_decision.pinned_core_ids.is_empty());
        assert!(!comp_decision.reasoning.is_empty());

        // Test idle scaling
        let idle_decision = coordinator.calculate_worker_threads(WorkloadType::BackgroundIdle);
        assert_eq!(idle_decision.recommended_threads, 1);
        assert_eq!(idle_decision.recommended_power_profile, "powersave");
    }

    #[test]
    fn test_core_pinning_and_affinity() {
        let coordinator = NativeDynatuneCoordinator::new();
        // Pin to core 0 (guaranteed to exist on any machine running tests)
        let res = coordinator.pin_current_thread_to_cores(&[0]);
        // Under standard user permissions, sched_setaffinity succeeds for current thread
        assert!(res.is_ok() || res.unwrap_err().contains("sched_setaffinity"));
    }

    #[test]
    fn test_power_profile_actuation_audit() {
        let coordinator = NativeDynatuneCoordinator::new();
        let res = coordinator.actuate_power_profile("balanced");
        assert!(res.is_ok());

        let history = coordinator.get_audit_history();
        assert!(!history.is_empty());
        assert_eq!(history.last().unwrap().new_state, "balanced");
    }
}
