//! # Dynamic Governor Watchdog for Hagibis Daemon
//!
//! Subsystem integrating hardware-aware CPU Governor management, thermal monitoring,
//! and automatic workload boost / relax cycles into `hgbd`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};

use hgb_core::dynamic_governor::{
    DynamicGovernor, GovernorGuard, GovernorTelemetry, WorkloadKind,
    THERMAL_THROTTLE_THRESHOLD_C,
};
use hgb_core::{HgbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorWatchdogStatus {
    pub is_active: bool,
    pub is_boosted: bool,
    pub current_workload: Option<String>,
    pub telemetry: GovernorTelemetry,
    pub thermal_protection_engaged: bool,
}

pub struct GovernorWatchdog {
    governor: Arc<DynamicGovernor>,
    active_guard: Arc<Mutex<Option<GovernorGuard>>>,
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
            active_guard: Arc::new(Mutex::new(None)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Boost CPU governor for heavy workload
    pub async fn boost(&self, workload_str: &str) -> Result<GovernorWatchdogStatus> {
        let workload = match workload_str.to_lowercase().as_str() {
            "compile" | "heavycompile" => WorkloadKind::HeavyCompile,
            "test" | "testexecution" => WorkloadKind::TestExecution,
            "llm" | "inference" | "localllminference" => WorkloadKind::LocalLlmInference,
            "wasm" | "wasmexecution" => WorkloadKind::WasmExecution,
            _ => WorkloadKind::HeavyCompile,
        };

        let guard = self.governor.boost_for_workload(workload)?;
        {
            let mut active = self.active_guard.lock().await;
            *active = Some(guard);
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
        let _ = self.governor.set_scaling_governor("powersave");
        let _ = self.governor.set_epp("balance_performance");

        Ok(self.status().await)
    }

    /// Thermal watchdog loop: checks temperature and relaxes if thermal threshold exceeded
    pub async fn check_and_safeguard(&self) -> bool {
        let temp = self.governor.read_max_cpu_temp_c();
        if temp >= THERMAL_THROTTLE_THRESHOLD_C {
            let mut active = self.active_guard.lock().await;
            if active.is_some() {
                // Drop boost guard to prevent hardware tripping
                active.take();
                let _ = self.governor.set_scaling_governor("powersave");
                let _ = self.governor.set_epp("balance_performance");
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

        GovernorWatchdogStatus {
            is_active: true,
            is_boosted,
            current_workload,
            telemetry: tel,
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

        let relaxed = watchdog.relax().await.expect("relax");
        assert!(!relaxed.is_boosted);
        assert_eq!(relaxed.current_workload, None);
    }
}
