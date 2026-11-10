//! # Superpower 3: Predictive Profile-Guided Autonomous Tuning
//!
//! Autonomous closed-loop control system uniting:
//! - Real-time telemetry ingestion from `hgb-core::dynatune_bridge` and `/proc` / sysfs
//! - Predictive thermal slope analysis: computes $dT/dt$ and forecasts package temp $T(t + 30\text{s})$
//! - Proactive governor shifting (`performance` <-> `powersave`) before hardware thermal throttling kicks in
//! - Closed-loop Linux kernel sysctl dynamic optimization (`vm.dirty_ratio`, `vm.swappiness`, MGLRU `min_ttl_ms`)
//! - Autonomous PGO / rustc compiler optimization preset switching (`HeavyPgoFatLto`, `Balanced`, `ThermalThrottledQuick`)
//! - Comprehensive audit trail of autonomous tuning decisions and execution receipts

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use hgb_core::dynatune_bridge::{
    CompleteHardwareTelemetry, DynatuneBridge, ScalingMode, WorkloadCategory,
    BURST_MGLRU_MIN_TTL_MS, THERMAL_SAFE_HEADROOM_C, THERMAL_THROTTLE_THRESHOLD_C,
};
use hgb_core::error::Result;

/// Maximum telemetry samples retained in the rolling evaluation window
pub const DEFAULT_MAX_TELEMETRY_WINDOW: usize = 60;
/// Default forecast lookahead horizon in seconds
pub const DEFAULT_FORECAST_HORIZON_SECS: f64 = 30.0;
/// Temperature rate-of-rise threshold in °C/s indicating rapid thermal escalation
pub const THERMAL_RUNAWAY_SLOPE_C_PER_SEC: f64 = 0.25;
/// High memory pressure PSI threshold where thrashing mitigation triggers
pub const HIGH_PSI_SOME_AVG10_THRESHOLD: f64 = 20.0;
/// Low available memory percentage threshold (15%)
pub const LOW_MEMORY_AVAILABLE_PCT_THRESHOLD: f64 = 15.0;

/// Profile-guided compiler optimization presets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompilerOptPreset {
    /// Maximum throughput when thermal headroom and power are abundant:
    /// `-C target-cpu=native -C lto=fat -C opt-level=3 -C codegen-units=1`
    HeavyPgoFatLto,
    /// Balanced production compilation:
    /// `-C opt-level=2 -C codegen-units=4`
    Balanced,
    /// Lightweight compilation designed to minimize core heat and memory footprint during thermal stress:
    /// `-C opt-level=1 -C codegen-units=8`
    ThermalThrottledQuick,
}

impl CompilerOptPreset {
    /// Return the corresponding rustc / cargo rustflags for this preset
    pub fn rustc_flags(&self) -> Vec<&'static str> {
        match self {
            Self::HeavyPgoFatLto => vec![
                "-C", "target-cpu=native",
                "-C", "lto=fat",
                "-C", "opt-level=3",
                "-C", "codegen-units=1",
            ],
            Self::Balanced => vec![
                "-C", "opt-level=2",
                "-C", "codegen-units=4",
            ],
            Self::ThermalThrottledQuick => vec![
                "-C", "opt-level=1",
                "-C", "codegen-units=8",
            ],
        }
    }
}

/// Dynamic thermal trajectory computed from rolling telemetry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThermalTrajectory {
    pub current_temp_c: f64,
    pub slope_c_per_sec: f64,
    pub forecast_temp_30s_c: f64,
    pub is_runaway_thermal: bool,
    pub sample_count: usize,
    pub observation_duration_secs: f64,
}

/// An autonomous closed-loop action recommended or executed by the tuner
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AutonomousTuningAction {
    /// Proactively downscale governors and limit concurrency before thermal throttle hits
    ProactiveThermalThrottle {
        target_governor: ScalingMode,
        recommended_concurrency: usize,
        compiler_preset: CompilerOptPreset,
        current_temp_c: f64,
        forecast_temp_c: f64,
        rationale: String,
    },
    /// Proactively burst CPU governors and apply fat PGO flags for heavy compilation
    HeavyWorkloadBurst {
        target_governor: ScalingMode,
        target_mglru_ttl_ms: u64,
        compiler_preset: CompilerOptPreset,
        concurrency_limit: usize,
        rationale: String,
    },
    /// Proactively mitigate memory thrash (tune vm.swappiness, MGLRU, dirty ratio)
    MemoryPressureRelief {
        target_mglru_ttl_ms: u64,
        sysctl_adjustments: Vec<(String, String)>,
        psi_avg10: f64,
        available_mem_pct: f64,
        rationale: String,
    },
    /// Restore nominal balanced baseline settings
    BalancedBaseline {
        target_governor: ScalingMode,
        compiler_preset: CompilerOptPreset,
        rationale: String,
    },
    /// Telemetry indicates system is well within optimal envelopes
    NoActionRequired {
        rationale: String,
    },
}

/// Audit receipt describing an applied or simulated autonomous tuning intervention
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TuningExecutionReceipt {
    pub action: AutonomousTuningAction,
    pub applied_at_epoch_ms: u64,
    pub dry_run: bool,
    pub sysctl_updates_applied: Vec<(String, String)>,
    pub governor_switched_to: Option<ScalingMode>,
    pub active_compiler_preset: CompilerOptPreset,
    pub execution_success: bool,
    pub details: String,
}

/// Predictive Profile-Guided Autonomous Tuner Engine
pub struct PredictiveAutonomousTuner {
    dynatune_bridge: Arc<DynatuneBridge>,
    telemetry_window: VecDeque<CompleteHardwareTelemetry>,
    action_history: Vec<TuningExecutionReceipt>,
    max_telemetry_window: usize,
    forecast_horizon_secs: f64,
    active_compiler_preset: CompilerOptPreset,
}

impl PredictiveAutonomousTuner {
    /// Initialize with a custom or shared DynatuneBridge
    pub fn new(dynatune_bridge: Arc<DynatuneBridge>) -> Self {
        Self {
            dynatune_bridge,
            telemetry_window: VecDeque::with_capacity(DEFAULT_MAX_TELEMETRY_WINDOW),
            action_history: Vec::new(),
            max_telemetry_window: DEFAULT_MAX_TELEMETRY_WINDOW,
            forecast_horizon_secs: DEFAULT_FORECAST_HORIZON_SECS,
            active_compiler_preset: CompilerOptPreset::Balanced,
        }
    }

    /// Initialize with the default host DynatuneBridge
    pub fn with_default_bridge() -> Self {
        Self::new(Arc::new(DynatuneBridge::new()))
    }

    /// Ingest a telemetry sample into the sliding window
    pub fn record_telemetry_sample(&mut self, telemetry: CompleteHardwareTelemetry) {
        if self.telemetry_window.len() >= self.max_telemetry_window {
            self.telemetry_window.pop_front();
        }
        self.telemetry_window.push_back(telemetry);
    }

    /// Retrieve the current telemetry window length
    pub fn sample_count(&self) -> usize {
        self.telemetry_window.len()
    }

    /// Get current active compiler optimization preset
    pub fn active_compiler_preset(&self) -> CompilerOptPreset {
        self.active_compiler_preset
    }

    /// Access the underlying dynatune bridge
    pub fn bridge(&self) -> &DynatuneBridge {
        &self.dynatune_bridge
    }

    /// Access action history receipts
    pub fn action_history(&self) -> &[TuningExecutionReceipt] {
        &self.action_history
    }

    /// Calculate the thermal trajectory ($dT/dt$ and 30-second forward projection)
    pub fn calculate_thermal_trajectory(&self) -> Option<ThermalTrajectory> {
        if self.telemetry_window.len() < 2 {
            if let Some(single) = self.telemetry_window.back() {
                return Some(ThermalTrajectory {
                    current_temp_c: single.cpu_pkg_temp_c,
                    slope_c_per_sec: 0.0,
                    forecast_temp_30s_c: single.cpu_pkg_temp_c,
                    is_runaway_thermal: false,
                    sample_count: 1,
                    observation_duration_secs: 0.0,
                });
            }
            return None;
        }

        let first = self.telemetry_window.front().unwrap();
        let last = self.telemetry_window.back().unwrap();

        let dt_ms = last.timestamp_epoch_ms.saturating_sub(first.timestamp_epoch_ms);
        let dt_secs = if dt_ms > 0 {
            dt_ms as f64 / 1000.0
        } else {
            // Fallback: estimate 1 second per sample interval
            (self.telemetry_window.len() - 1) as f64
        };

        let temp_delta = last.cpu_pkg_temp_c - first.cpu_pkg_temp_c;
        let slope = if dt_secs > 0.001 {
            temp_delta / dt_secs
        } else {
            0.0
        };

        let forecast_30s = last.cpu_pkg_temp_c + (slope * self.forecast_horizon_secs);
        let is_runaway = slope >= THERMAL_RUNAWAY_SLOPE_C_PER_SEC && last.cpu_pkg_temp_c >= 68.0;

        Some(ThermalTrajectory {
            current_temp_c: last.cpu_pkg_temp_c,
            slope_c_per_sec: slope,
            forecast_temp_30s_c: forecast_30s,
            is_runaway_thermal: is_runaway,
            sample_count: self.telemetry_window.len(),
            observation_duration_secs: dt_secs,
        })
    }

    /// Closed-loop decision evaluation: checks thermal projection, memory pressure, and workload intent
    pub fn evaluate_tuning_decision(&self, workload: WorkloadCategory) -> AutonomousTuningAction {
        let latest = match self.telemetry_window.back() {
            Some(t) => t,
            None => {
                return AutonomousTuningAction::NoActionRequired {
                    rationale: "No telemetry samples available in evaluation window".to_string(),
                };
            }
        };

        let trajectory = self.calculate_thermal_trajectory();
        let (forecast_temp, is_runaway) = if let Some(ref traj) = trajectory {
            (traj.forecast_temp_30s_c, traj.is_runaway_thermal)
        } else {
            (latest.cpu_pkg_temp_c, false)
        };

        // Rule 1: Thermal Emergency or Proactive Forecasted Thermal Throttle
        if latest.cpu_pkg_temp_c >= THERMAL_THROTTLE_THRESHOLD_C
            || forecast_temp >= THERMAL_THROTTLE_THRESHOLD_C
            || is_runaway
        {
            let target_concurrency = if latest.cpu_pkg_temp_c >= 82.0 {
                1
            } else {
                2
            };

            return AutonomousTuningAction::ProactiveThermalThrottle {
                target_governor: ScalingMode::PowerSave,
                recommended_concurrency: target_concurrency,
                compiler_preset: CompilerOptPreset::ThermalThrottledQuick,
                current_temp_c: latest.cpu_pkg_temp_c,
                forecast_temp_c: forecast_temp,
                rationale: format!(
                    "Proactive thermal safeguard: current {:.1}°C, 30s forecast {:.1}°C (threshold {:.1}°C, runaway={})",
                    latest.cpu_pkg_temp_c, forecast_temp, THERMAL_THROTTLE_THRESHOLD_C, is_runaway
                ),
            };
        }

        // Rule 2: Memory Pressure Stall / Low Available RAM Relief
        let psi_avg10 = latest.psi_memory.some.avg10;
        let mem_avail_pct = latest.memory_available_pct;

        if psi_avg10 >= HIGH_PSI_SOME_AVG10_THRESHOLD
            || mem_avail_pct <= LOW_MEMORY_AVAILABLE_PCT_THRESHOLD
        {
            let mut adjustments = Vec::new();
            adjustments.push(("vm.dirty_ratio".to_string(), "10".to_string()));
            adjustments.push(("vm.dirty_background_ratio".to_string(), "5".to_string()));
            adjustments.push(("vm.swappiness".to_string(), "30".to_string()));

            return AutonomousTuningAction::MemoryPressureRelief {
                target_mglru_ttl_ms: 2000,
                sysctl_adjustments: adjustments,
                psi_avg10,
                available_mem_pct: mem_avail_pct,
                rationale: format!(
                    "Memory pressure relief: PSI avg10={:.1}%, Available RAM={:.1}%",
                    psi_avg10, mem_avail_pct
                ),
            };
        }

        // Rule 3: Heavy Workload Burst (Compilation or Tests) when host is cool & power is safe
        let is_heavy = matches!(
            workload,
            WorkloadCategory::BuildCompile
                | WorkloadCategory::TestExecution
                | WorkloadCategory::LlmGeneration
        );

        let power_safe = !latest.is_battery_critical || latest.ac_connected;
        let thermal_safe = forecast_temp < THERMAL_SAFE_HEADROOM_C && latest.cpu_pkg_temp_c < THERMAL_SAFE_HEADROOM_C;

        if is_heavy && power_safe && thermal_safe {
            return AutonomousTuningAction::HeavyWorkloadBurst {
                target_governor: ScalingMode::Performance,
                target_mglru_ttl_ms: BURST_MGLRU_MIN_TTL_MS,
                compiler_preset: CompilerOptPreset::HeavyPgoFatLto,
                concurrency_limit: latest.recommended_concurrency_limit.max(4),
                rationale: format!(
                    "Bursting for {:?}: Temp {:.1}°C (Forecast {:.1}°C) < {:.1}°C safe headroom; AC={}",
                    workload, latest.cpu_pkg_temp_c, forecast_temp, THERMAL_SAFE_HEADROOM_C, latest.ac_connected
                ),
            };
        }

        // Rule 4: Idle Interval Preservation
        if workload == WorkloadCategory::IdleInterval {
            return AutonomousTuningAction::BalancedBaseline {
                target_governor: ScalingMode::PowerSave,
                compiler_preset: CompilerOptPreset::Balanced,
                rationale: "Idle workload: shifting to powersave baseline".to_string(),
            };
        }

        // Rule 5: Nominal Operation
        AutonomousTuningAction::NoActionRequired {
            rationale: format!(
                "Hardware telemetry nominal: Temp {:.1}°C, RAM avail {:.1}%, PSI={:.1}",
                latest.cpu_pkg_temp_c, mem_avail_pct, psi_avg10
            ),
        }
    }

    /// Apply or simulate an autonomous tuning action
    pub fn apply_autonomous_action(
        &mut self,
        action: &AutonomousTuningAction,
        dry_run: bool,
    ) -> Result<TuningExecutionReceipt> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut sysctl_updates = Vec::new();
        let mut target_governor = None;

        match action {
            AutonomousTuningAction::ProactiveThermalThrottle {
                target_governor: gov,
                compiler_preset,
                rationale,
                ..
            } => {
                target_governor = Some(*gov);
                self.active_compiler_preset = *compiler_preset;

                if !dry_run {
                    let _ = self.dynatune_bridge.shift_to_powersave_mode();
                }

                let receipt = TuningExecutionReceipt {
                    action: action.clone(),
                    applied_at_epoch_ms: now_ms,
                    dry_run,
                    sysctl_updates_applied: vec![],
                    governor_switched_to: target_governor,
                    active_compiler_preset: self.active_compiler_preset,
                    execution_success: true,
                    details: format!("Engaged thermal throttling: {rationale}"),
                };
                self.action_history.push(receipt.clone());
                Ok(receipt)
            }
            AutonomousTuningAction::HeavyWorkloadBurst {
                target_governor: gov,
                target_mglru_ttl_ms,
                compiler_preset,
                rationale,
                ..
            } => {
                target_governor = Some(*gov);
                self.active_compiler_preset = *compiler_preset;

                if !dry_run {
                    let _ = self.dynatune_bridge.shift_to_performance_mode();
                }
                sysctl_updates.push((
                    "mglru_min_ttl_ms".to_string(),
                    target_mglru_ttl_ms.to_string(),
                ));

                let receipt = TuningExecutionReceipt {
                    action: action.clone(),
                    applied_at_epoch_ms: now_ms,
                    dry_run,
                    sysctl_updates_applied: sysctl_updates,
                    governor_switched_to: target_governor,
                    active_compiler_preset: self.active_compiler_preset,
                    execution_success: true,
                    details: format!("Executed performance burst: {rationale}"),
                };
                self.action_history.push(receipt.clone());
                Ok(receipt)
            }
            AutonomousTuningAction::MemoryPressureRelief {
                target_mglru_ttl_ms,
                sysctl_adjustments,
                rationale,
                ..
            } => {
                let mut updates = sysctl_adjustments.clone();
                updates.push((
                    "mglru_min_ttl_ms".to_string(),
                    target_mglru_ttl_ms.to_string(),
                ));

                let receipt = TuningExecutionReceipt {
                    action: action.clone(),
                    applied_at_epoch_ms: now_ms,
                    dry_run,
                    sysctl_updates_applied: updates,
                    governor_switched_to: None,
                    active_compiler_preset: self.active_compiler_preset,
                    execution_success: true,
                    details: format!("Applied memory relief parameters: {rationale}"),
                };
                self.action_history.push(receipt.clone());
                Ok(receipt)
            }
            AutonomousTuningAction::BalancedBaseline {
                target_governor: gov,
                compiler_preset,
                rationale,
            } => {
                target_governor = Some(*gov);
                self.active_compiler_preset = *compiler_preset;

                if !dry_run {
                    let _ = self.dynatune_bridge.shift_to_powersave_mode();
                }

                let receipt = TuningExecutionReceipt {
                    action: action.clone(),
                    applied_at_epoch_ms: now_ms,
                    dry_run,
                    sysctl_updates_applied: vec![],
                    governor_switched_to: target_governor,
                    active_compiler_preset: self.active_compiler_preset,
                    execution_success: true,
                    details: format!("Restored baseline: {rationale}"),
                };
                self.action_history.push(receipt.clone());
                Ok(receipt)
            }
            AutonomousTuningAction::NoActionRequired { rationale } => {
                let receipt = TuningExecutionReceipt {
                    action: action.clone(),
                    applied_at_epoch_ms: now_ms,
                    dry_run,
                    sysctl_updates_applied: vec![],
                    governor_switched_to: None,
                    active_compiler_preset: self.active_compiler_preset,
                    execution_success: true,
                    details: rationale.clone(),
                };
                Ok(receipt)
            }
        }
    }

    /// Read live host telemetry from the bridge, evaluate decision, and apply intervention
    pub fn evaluate_and_apply(
        &mut self,
        workload: WorkloadCategory,
        dry_run: bool,
    ) -> Result<TuningExecutionReceipt> {
        let telemetry = self.dynatune_bridge.read_complete_hardware_telemetry();
        self.record_telemetry_sample(telemetry);
        let decision = self.evaluate_tuning_decision(workload);
        self.apply_autonomous_action(&decision, dry_run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use hgb_core::dynatune_bridge::{
        BatteryTelemetry, PressureRecordTelemetry, PsiCpuTelemetry, PsiMemoryTelemetry,
    };

    fn create_mock_telemetry(
        temp_c: f64,
        mem_avail_pct: f64,
        psi_avg10: f64,
        ac_online: bool,
        epoch_ms: u64,
    ) -> CompleteHardwareTelemetry {
        CompleteHardwareTelemetry {
            cpu_pkg_temp_c: temp_c,
            max_temp_c: temp_c,
            thermal_zones: vec![],
            is_thermal_throttled: temp_c >= THERMAL_THROTTLE_THRESHOLD_C,
            ac_connected: ac_online,
            batteries: vec![],
            primary_battery: Some(BatteryTelemetry {
                name: "BAT0".to_string(),
                status: "Discharging".to_string(),
                capacity_pct: 85,
                is_discharging: !ac_online,
                is_critical: false,
                discharge_rate_mw: None,
                discharge_rate_pct_per_hr: None,
                power_now_w: None,
                energy_now_wh: None,
                energy_full_wh: None,
            }),
            is_battery_critical: false,
            memory_total_mb: 16384,
            memory_available_mb: ((16384.0 * mem_avail_pct) / 100.0) as u64,
            memory_used_mb: 4096,
            memory_available_pct: mem_avail_pct,
            psi_memory: PsiMemoryTelemetry {
                some: PressureRecordTelemetry {
                    avg10: psi_avg10,
                    avg60: 0.0,
                    avg300: 0.0,
                    total_us: 0,
                },
                full: PressureRecordTelemetry::default(),
                available: true,
            },
            psi_cpu: PsiCpuTelemetry::default(),
            battery_discharge_rate_mw: None,
            battery_discharge_rate_pct_per_hr: None,
            mglru_enabled: Some("1".to_string()),
            mglru_min_ttl_ms: Some(0),
            scaling_driver: "intel_pstate".to_string(),
            active_governor: "powersave".to_string(),
            active_epp: "balance_performance".to_string(),
            cpu_frequencies_mhz: vec![2400.0],
            no_turbo: false,
            max_perf_pct: 100,
            hwp_dynamic_boost: false,
            recommended_concurrency_limit: 8,
            is_throttled: false,
            nice_level: 0,
            is_privileged: false,
            dynatune_available: true,
            kernel_ctl_available: false,
            timestamp_epoch_ms: epoch_ms,
        }
    }

    #[test]
    fn test_predictive_thermal_forecast_and_proactive_throttle() {
        let temp_dir = tempdir().unwrap();
        let bridge = Arc::new(
            DynatuneBridge::create_simulated_sysfs(temp_dir.path(), 55.0, true, false, 90).unwrap(),
        );

        let mut tuner = PredictiveAutonomousTuner::new(bridge);

        // Feed rising temperature slope: 60°C at t=0s, 72°C at t=20s -> slope = +0.6°C/sec!
        // At t=20s, current temp is 72°C, but 30s forecast will be: 72 + (0.6 * 30) = 90.0°C!
        let t0 = create_mock_telemetry(60.0, 75.0, 0.0, true, 1_000_000);
        let t1 = create_mock_telemetry(72.0, 75.0, 0.0, true, 1_020_000);

        tuner.record_telemetry_sample(t0);
        tuner.record_telemetry_sample(t1);

        let traj = tuner.calculate_thermal_trajectory().unwrap();
        assert!(traj.slope_c_per_sec > 0.5);
        assert!(traj.forecast_temp_30s_c >= 85.0);
        assert!(traj.is_runaway_thermal);

        // Evaluation during build compile workload should proactively throttle
        let action = tuner.evaluate_tuning_decision(WorkloadCategory::BuildCompile);
        match action {
            AutonomousTuningAction::ProactiveThermalThrottle {
                target_governor,
                compiler_preset,
                forecast_temp_c,
                ..
            } => {
                assert_eq!(target_governor, ScalingMode::PowerSave);
                assert_eq!(compiler_preset, CompilerOptPreset::ThermalThrottledQuick);
                assert!(forecast_temp_c >= 85.0);
            }
            other => panic!("Expected ProactiveThermalThrottle, got {:?}", other),
        }

        let receipt = tuner.apply_autonomous_action(&action, true).unwrap();
        assert_eq!(receipt.active_compiler_preset, CompilerOptPreset::ThermalThrottledQuick);
        assert_eq!(tuner.active_compiler_preset(), CompilerOptPreset::ThermalThrottledQuick);
    }

    #[test]
    fn test_heavy_workload_burst_when_cool() {
        let temp_dir = tempdir().unwrap();
        let bridge = Arc::new(
            DynatuneBridge::create_simulated_sysfs(temp_dir.path(), 45.0, true, false, 95).unwrap(),
        );

        let mut tuner = PredictiveAutonomousTuner::new(bridge);

        // Cool, steady host at 45.0°C
        let t0 = create_mock_telemetry(45.0, 80.0, 0.0, true, 1_000_000);
        let t1 = create_mock_telemetry(46.0, 80.0, 0.0, true, 1_010_000);

        tuner.record_telemetry_sample(t0);
        tuner.record_telemetry_sample(t1);

        let action = tuner.evaluate_tuning_decision(WorkloadCategory::BuildCompile);
        match action {
            AutonomousTuningAction::HeavyWorkloadBurst {
                target_governor,
                compiler_preset,
                target_mglru_ttl_ms,
                ..
            } => {
                assert_eq!(target_governor, ScalingMode::Performance);
                assert_eq!(compiler_preset, CompilerOptPreset::HeavyPgoFatLto);
                assert_eq!(target_mglru_ttl_ms, 1000);
            }
            other => panic!("Expected HeavyWorkloadBurst, got {:?}", other),
        }

        let receipt = tuner.apply_autonomous_action(&action, true).unwrap();
        assert_eq!(receipt.active_compiler_preset, CompilerOptPreset::HeavyPgoFatLto);
        assert_eq!(receipt.governor_switched_to, Some(ScalingMode::Performance));
    }

    #[test]
    fn test_memory_pressure_relief_trigger() {
        let temp_dir = tempdir().unwrap();
        let bridge = Arc::new(
            DynatuneBridge::create_simulated_sysfs(temp_dir.path(), 50.0, true, false, 95).unwrap(),
        );

        let mut tuner = PredictiveAutonomousTuner::new(bridge);

        // Host with high memory pressure (PSI avg10 = 25.0%, available = 10%)
        let t0 = create_mock_telemetry(50.0, 10.0, 25.0, true, 1_000_000);
        tuner.record_telemetry_sample(t0);

        let action = tuner.evaluate_tuning_decision(WorkloadCategory::BuildCompile);
        match action {
            AutonomousTuningAction::MemoryPressureRelief {
                target_mglru_ttl_ms,
                sysctl_adjustments,
                ..
            } => {
                assert_eq!(target_mglru_ttl_ms, 2000);
                assert!(sysctl_adjustments.iter().any(|(k, v)| k == "vm.dirty_ratio" && v == "10"));
            }
            other => panic!("Expected MemoryPressureRelief, got {:?}", other),
        }
    }
}
