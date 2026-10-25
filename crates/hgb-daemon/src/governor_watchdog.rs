//! # Dynamic Governor Watchdog for Hagibis Daemon
//!
//! Subsystem integrating hardware-aware CPU Governor management, thermal monitoring,
//! Dynatune bridge synergy, and automatic workload boost / relax cycles into `hgbd`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};

use hgb_core::dynamic_governor::{
    DynamicGovernor, GovernorGuard, GovernorTelemetry, WorkloadKind,
    THERMAL_THROTTLE_THRESHOLD_C,
};
use hgb_core::dynatune_bridge::{
    DynatuneBridge, HostPowerState, ScalingGuard, ScalingShiftReport, WorkloadCategory,
};
use hgb_core::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorWatchdogStatus {
    pub is_active: bool,
    pub is_boosted: bool,
    pub current_workload: Option<String>,
    pub telemetry: GovernorTelemetry,
    pub host_power_state: Option<HostPowerState>,
    pub last_scaling_shift: Option<ScalingShiftReport>,
    pub thermal_protection_engaged: bool,
}

pub struct GovernorWatchdog {
    governor: Arc<DynamicGovernor>,
    dynatune_bridge: Arc<DynatuneBridge>,
    active_guard: Arc<Mutex<Option<GovernorGuard>>>,
    active_scaling_guard: Arc<Mutex<Option<ScalingGuard>>>,
    last_shift: Arc<Mutex<Option<ScalingShiftReport>>>,
    #[allow(dead_code)]
    is_running: Arc<AtomicBool>,
}

impl Default for GovernorWatchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl GovernorWatchdog {
    pub fn new() -> Self {
        Self {
            governor: Arc::new(DynamicGovernor::new()),
            dynatune_bridge: Arc::new(DynatuneBridge::new()),
            active_guard: Arc::new(Mutex::new(None)),
            active_scaling_guard: Arc::new(Mutex::new(None)),
            last_shift: Arc::new(Mutex::new(None)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Access Dynatune bridge
    pub fn bridge(&self) -> &Arc<DynatuneBridge> {
        &self.dynatune_bridge
    }

    /// Boost CPU governor for heavy workload
    pub async fn boost(&self, workload_str: &str) -> Result<GovernorWatchdogStatus> {
        let (workload, category) = match workload_str.to_lowercase().as_str() {
            "compile" | "heavycompile" | "build" => (WorkloadKind::HeavyCompile, WorkloadCategory::BuildCompile),
            "test" | "testexecution" => (WorkloadKind::TestExecution, WorkloadCategory::TestExecution),
            "llm" | "inference" | "localllminference" => (WorkloadKind::LocalLlmInference, WorkloadCategory::LlmGeneration),
            "wasm" | "wasmexecution" => (WorkloadKind::WasmExecution, WorkloadCategory::WasmExecution),
            _ => (WorkloadKind::HeavyCompile, WorkloadCategory::BuildCompile),
        };

        // 1. Dynamic governor boost
        let guard = self.governor.boost_for_workload(workload)?;
        {
            let mut active = self.active_guard.lock().await;
            *active = Some(guard);
        }

        // 2. Dynatune bridge scaling shift & RAII guard
        let scaling_guard = self.dynatune_bridge.burst_guard(category)?;
        {
            let mut shift_lock = self.last_shift.lock().await;
            *shift_lock = Some(scaling_guard.active_shift.clone());
            let mut active_s = self.active_scaling_guard.lock().await;
            *active_s = Some(scaling_guard);
        }

        Ok(self.status().await)
    }

    /// Relax CPU governor back to powersave / baseline
    pub async fn relax(&self) -> Result<GovernorWatchdogStatus> {
        {
            let mut active = self.active_guard.lock().await;
            if let Some(guard) = active.take() {
                drop(guard);
            }
        }
        {
            let mut active_s = self.active_scaling_guard.lock().await;
            if let Some(guard) = active_s.take() {
                drop(guard);
            }
        }

        let _ = self.governor.set_scaling_governor("powersave");
        let _ = self.governor.set_epp("balance_performance");

        let shift = self.dynatune_bridge.shift_to_powersave_mode()?;
        {
            let mut shift_lock = self.last_shift.lock().await;
            *shift_lock = Some(shift);
        }

        Ok(self.status().await)
    }

    /// Thermal watchdog loop: checks temperature and relaxes if thermal threshold exceeded
    pub async fn check_and_safeguard(&self) -> bool {
        let temp = self.governor.read_max_cpu_temp_c();
        if temp >= THERMAL_THROTTLE_THRESHOLD_C {
            let mut active = self.active_guard.lock().await;
            let mut active_s = self.active_scaling_guard.lock().await;
            if active.is_some() || active_s.is_some() {
                active.take();
                active_s.take();
                let _ = self.governor.set_scaling_governor("powersave");
                let _ = self.governor.set_epp("balance_performance");
                let _ = self.dynatune_bridge.shift_to_powersave_mode();
                return true;
            }
        }
        false
    }

    /// Query watchdog status and hardware telemetry
    pub async fn status(&self) -> GovernorWatchdogStatus {
        let active = self.active_guard.lock().await;
        let is_boosted = active.is_some();
        let current_workload = active.as_ref().map(|g| g.workload.to_string());
        let tel = self.governor.telemetry();
        let thermal_protection_engaged = tel.cpu_temp_c >= THERMAL_THROTTLE_THRESHOLD_C;
        let host_power = Some(self.dynatune_bridge.read_power_state());
        let last_shift = self.last_shift.lock().await.clone();

        GovernorWatchdogStatus {
            is_active: true,
            is_boosted,
            current_workload,
            telemetry: tel,
            host_power_state: host_power,
            last_scaling_shift: last_shift,
            thermal_protection_engaged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_watchdog_boost_and_relax() {
        let watchdog = GovernorWatchdog::new();
        let status = watchdog.boost("compile").await.expect("boost");
        assert!(status.is_boosted);
        assert_eq!(status.current_workload, Some("heavy_compile".to_string()));
        assert!(status.host_power_state.is_some());
        assert!(status.last_scaling_shift.is_some());

        let relaxed = watchdog.relax().await.expect("relax");
        assert!(!relaxed.is_boosted);
        assert_eq!(relaxed.current_workload, None);
    }
}
