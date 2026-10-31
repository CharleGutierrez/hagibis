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
};
use hgb_core::dynamic_hardware_profiler::{
    DynamicHardwareProfiler, HardwareGovernorProfile, HardwareProfileTelemetry,
    ProfilerGovernorGuard,
};
use hgb_core::dynatune_bridge::{
    CompleteHardwareTelemetry, DynatuneBridge, HostPowerState, ScalingGuard,
    ScalingShiftReport, WorkloadCategory,
};
use hgb_core::hardware_aware_workload_router::{
    HardwareAwareWorkloadRouter, WorkloadAdaptiveGuard, WorkloadExecutionDecision,
    WorkloadExecutionProfile, WorkloadKind as HardwareWorkloadKind,
};
use hgb_core::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorWatchdogStatus {
    pub is_active: bool,
    pub is_boosted: bool,
    pub current_workload: Option<String>,
    pub telemetry: GovernorTelemetry,
    pub host_power_state: Option<HostPowerState>,
    pub complete_telemetry: Option<CompleteHardwareTelemetry>,
    pub last_scaling_shift: Option<ScalingShiftReport>,
    #[serde(default)]
    pub last_workload_decision: Option<WorkloadExecutionDecision>,
    #[serde(default)]
    pub execution_profile: Option<WorkloadExecutionProfile>,
    pub thermal_protection_engaged: bool,
    pub battery_protection_engaged: bool,
    pub recommended_concurrency_limit: usize,
    #[serde(default)]
    pub optimal_concurrency: usize,
    #[serde(default)]
    pub recommended_ollama_batch_size: usize,
    #[serde(default)]
    pub profiler_telemetry: Option<HardwareProfileTelemetry>,
}

pub struct GovernorWatchdog {
    governor: Arc<DynamicGovernor>,
    dynatune_bridge: Arc<DynatuneBridge>,
    workload_router: Arc<HardwareAwareWorkloadRouter>,
    profiler: Arc<DynamicHardwareProfiler>,
    active_guard: Arc<Mutex<Option<GovernorGuard>>>,
    active_scaling_guard: Arc<Mutex<Option<ScalingGuard>>>,
    active_adaptive_guard: Arc<Mutex<Option<WorkloadAdaptiveGuard>>>,
    active_profiler_guard: Arc<Mutex<Option<ProfilerGovernorGuard>>>,
    last_shift: Arc<Mutex<Option<ScalingShiftReport>>>,
    last_decision: Arc<Mutex<Option<WorkloadExecutionDecision>>>,
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
        let bridge = Arc::new(DynatuneBridge::new());
        let router = Arc::new(HardwareAwareWorkloadRouter::with_bridge(Arc::clone(&bridge)));
        let profiler = Arc::new(DynamicHardwareProfiler::new());
        let _sampler = profiler.start_background_sampler(std::time::Duration::from_secs(1));

        Self {
            governor: Arc::new(DynamicGovernor::new()),
            dynatune_bridge: bridge,
            workload_router: router,
            profiler,
            active_guard: Arc::new(Mutex::new(None)),
            active_scaling_guard: Arc::new(Mutex::new(None)),
            active_adaptive_guard: Arc::new(Mutex::new(None)),
            active_profiler_guard: Arc::new(Mutex::new(None)),
            last_shift: Arc::new(Mutex::new(None)),
            last_decision: Arc::new(Mutex::new(None)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Access Dynatune bridge
    pub fn bridge(&self) -> &Arc<DynatuneBridge> {
        &self.dynatune_bridge
    }

    /// Access Hardware-Aware Workload Router
    pub fn router(&self) -> &Arc<HardwareAwareWorkloadRouter> {
        &self.workload_router
    }

    /// Access Dynamic Hardware Profiler
    pub fn profiler(&self) -> &Arc<DynamicHardwareProfiler> {
        &self.profiler
    }

    /// Read optimal task concurrency dynamically computed by DynamicHardwareProfiler
    pub fn optimal_task_concurrency(&self) -> usize {
        self.profiler.optimal_task_concurrency()
    }

    /// Read recommended Ollama token batch size dynamically computed by DynamicHardwareProfiler
    pub fn recommended_ollama_batch_size(&self) -> usize {
        self.profiler.recommended_ollama_batch_size()
    }

    /// Read live hardware profile telemetry
    pub fn hardware_profile_telemetry(&self) -> HardwareProfileTelemetry {
        self.profiler.current_telemetry()
    }

    /// Evaluate a workload and compute an execution decision
    pub fn evaluate_workload(&self, workload: HardwareWorkloadKind) -> WorkloadExecutionDecision {
        self.workload_router.evaluate_workload(workload)
    }

    /// Acquire RAII WorkloadAdaptiveGuard directly for heavy operations
    pub fn acquire_workload_guard(&self, workload: HardwareWorkloadKind) -> Result<WorkloadAdaptiveGuard> {
        self.workload_router.acquire_guard(workload)
    }

    /// Execute an async workload wrapped with WorkloadAdaptiveGuard
    pub async fn execute_with_adaptive_guard<F, Fut, R>(&self, workload: HardwareWorkloadKind, task: F) -> Result<R>
    where
        F: FnOnce(WorkloadExecutionDecision) -> Fut,
        Fut: std::future::Future<Output = Result<R>>,
    {
        self.workload_router.execute_async_with_guard(workload, task).await
    }

    /// Execute a sync workload wrapped with WorkloadAdaptiveGuard
    pub fn execute_sync_with_adaptive_guard<F, R>(&self, workload: HardwareWorkloadKind, task: F) -> Result<R>
    where
        F: FnOnce(&WorkloadExecutionDecision) -> Result<R>,
    {
        self.workload_router.execute_with_guard(workload, task)
    }

    /// Read complete, unified hardware telemetry snapshot
    pub fn complete_telemetry(&self) -> CompleteHardwareTelemetry {
        self.dynatune_bridge.read_complete_hardware_telemetry()
    }

    /// Boost CPU governor for heavy workload
    pub async fn boost(&self, workload_str: &str) -> Result<GovernorWatchdogStatus> {
        let (workload, category, hw_kind) = match workload_str.to_lowercase().as_str() {
            "compile" | "heavycompile" | "build" => (WorkloadKind::HeavyCompile, WorkloadCategory::BuildCompile, HardwareWorkloadKind::HeavyCompilation),
            "test" | "testexecution" => (WorkloadKind::TestExecution, WorkloadCategory::TestExecution, HardwareWorkloadKind::HeavyCompilation),
            "llm" | "inference" | "localllminference" => (WorkloadKind::LocalLlmInference, WorkloadCategory::LlmGeneration, HardwareWorkloadKind::HeavyInference),
            "wasm" | "wasmexecution" => (WorkloadKind::WasmExecution, WorkloadCategory::WasmExecution, HardwareWorkloadKind::WasmSandboxed),
            "vector" | "vectorbatch" => (WorkloadKind::HeavyCompile, WorkloadCategory::BuildCompile, HardwareWorkloadKind::VectorBatch),
            _ => (WorkloadKind::HeavyCompile, WorkloadCategory::BuildCompile, HardwareWorkloadKind::HeavyCompilation),
        };

        // 1. Dynamic governor boost
        let guard = self.governor.boost_for_workload(workload)?;
        {
            let mut active = self.active_guard.lock().await;
            *active = Some(guard);
        }

        // 2. Dynatune bridge scaling shift & RAII guard (bursts governor, EPP, and MGLRU min_ttl_ms 1000)
        let scaling_guard = self.dynatune_bridge.burst_guard(category)?;
        {
            let mut shift_lock = self.last_shift.lock().await;
            *shift_lock = Some(scaling_guard.active_shift.clone());
            let mut active_s = self.active_scaling_guard.lock().await;
            *active_s = Some(scaling_guard);
        }

        // 3. WorkloadAdaptiveGuard acquisition for router profile
        if let Ok(adaptive_guard) = self.workload_router.acquire_guard(hw_kind) {
            let mut dec_lock = self.last_decision.lock().await;
            *dec_lock = Some(adaptive_guard.decision().clone());
            let mut active_a = self.active_adaptive_guard.lock().await;
            *active_a = Some(adaptive_guard);
        }

        // 4. DynamicHardwareProfiler profile boost with RAII guard
        if let Ok(p_guard) = self.profiler.acquire_profile_guard(HardwareGovernorProfile::Performance) {
            let mut active_p = self.active_profiler_guard.lock().await;
            *active_p = Some(p_guard);
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
        {
            let mut active_a = self.active_adaptive_guard.lock().await;
            if let Some(guard) = active_a.take() {
                drop(guard);
            }
        }
        {
            let mut active_p = self.active_profiler_guard.lock().await;
            if let Some(guard) = active_p.take() {
                drop(guard);
            }
        }

        let _ = self.governor.set_scaling_governor("powersave");
        let _ = self.governor.set_epp("balance_performance");
        let _ = self.profiler.switch_profile(HardwareGovernorProfile::PowerSave);

        let shift = self.dynatune_bridge.shift_to_powersave_mode()?;
        {
            let mut shift_lock = self.last_shift.lock().await;
            *shift_lock = Some(shift);
        }

        Ok(self.status().await)
    }

    /// Execute a heavy asynchronous task wrapped with dynamic workload boosting and automatic relaxation
    pub async fn execute_with_workload_boost<F, Fut, R>(&self, workload: &str, task: F) -> Result<R>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<R>>,
    {
        // 1. Pre-execution safeguard check
        self.check_and_safeguard().await;

        // 2. Burst governor and MGLRU
        let _ = self.boost(workload).await;

        // 3. Run workload
        let res = task().await;

        // 4. Relax back to powersave
        let _ = self.relax().await;

        res
    }

    /// Execute a heavy synchronous task wrapped with dynamic workload boosting and RAII drop restoration
    pub fn execute_sync_with_workload_boost<F, R>(&self, workload: &str, task: F) -> Result<R>
    where
        F: FnOnce() -> Result<R>,
    {
        let category = match workload.to_lowercase().as_str() {
            "compile" | "heavycompile" | "build" => WorkloadCategory::BuildCompile,
            "test" | "testexecution" => WorkloadCategory::TestExecution,
            "llm" | "inference" | "localllminference" => WorkloadCategory::LlmGeneration,
            "wasm" | "wasmexecution" => WorkloadCategory::WasmExecution,
            _ => WorkloadCategory::BuildCompile,
        };

        // Acquire RAII guard
        let _guard = self.dynatune_bridge.burst_guard(category)?;
        task()
    }

    /// Thermal and battery watchdog check: detects throttling conditions and disengages burst mode if needed
    pub async fn check_and_safeguard(&self) -> bool {
        let complete = self.dynatune_bridge.read_complete_hardware_telemetry();
        let profiler_throttled = self.profiler.enforce_thermal_safety();
        if complete.is_throttled || profiler_throttled {
            let mut active = self.active_guard.lock().await;
            let mut active_s = self.active_scaling_guard.lock().await;
            let mut active_a = self.active_adaptive_guard.lock().await;
            let mut active_p = self.active_profiler_guard.lock().await;
            if active.is_some() || active_s.is_some() || active_a.is_some() || active_p.is_some() {
                active.take();
                active_s.take();
                active_a.take();
                active_p.take();
                let _ = self.governor.set_scaling_governor("powersave");
                let _ = self.governor.set_epp("balance_power");
                let _ = self.profiler.switch_profile(HardwareGovernorProfile::PowerSave);
                if let Ok(shift) = self.dynatune_bridge.shift_to_powersave_mode() {
                    let mut shift_lock = self.last_shift.lock().await;
                    *shift_lock = Some(shift);
                }
                return true;
            }
        }
        false
    }

    /// Query watchdog status and complete hardware telemetry
    pub async fn status(&self) -> GovernorWatchdogStatus {
        let active = self.active_guard.lock().await;
        let active_s = self.active_scaling_guard.lock().await;
        let active_a = self.active_adaptive_guard.lock().await;
        let active_p = self.active_profiler_guard.lock().await;
        let is_boosted = active.is_some() || active_s.is_some() || active_a.is_some() || active_p.is_some();
        let current_workload = active.as_ref().map(|g| g.workload.to_string());
        let tel = self.governor.telemetry();
        let complete_tel = self.dynatune_bridge.read_complete_hardware_telemetry();
        let thermal_protection_engaged = complete_tel.is_thermal_throttled;
        let battery_protection_engaged = complete_tel.is_battery_critical;
        let recommended_concurrency_limit = complete_tel.recommended_concurrency_limit;
        let optimal_concurrency = self.profiler.optimal_task_concurrency();
        let recommended_ollama_batch_size = self.profiler.recommended_ollama_batch_size();
        let profiler_telemetry = Some(self.profiler.current_telemetry());
        let host_power = Some(self.dynatune_bridge.read_power_state());
        let last_shift = self.last_shift.lock().await.clone();
        let last_decision = self.last_decision.lock().await.clone();
        let execution_profile = last_decision.as_ref().map(|d| d.profile);

        GovernorWatchdogStatus {
            is_active: true,
            is_boosted,
            current_workload,
            telemetry: tel,
            host_power_state: host_power,
            complete_telemetry: Some(complete_tel),
            last_scaling_shift: last_shift,
            last_workload_decision: last_decision,
            execution_profile,
            thermal_protection_engaged,
            battery_protection_engaged,
            recommended_concurrency_limit,
            optimal_concurrency,
            recommended_ollama_batch_size,
            profiler_telemetry,
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
        assert!(status.complete_telemetry.is_some());
        assert!(status.last_scaling_shift.is_some());
        assert!(status.recommended_concurrency_limit >= 1);

        let relaxed = watchdog.relax().await.expect("relax");
        assert!(!relaxed.is_boosted);
        assert_eq!(relaxed.current_workload, None);
    }

    #[tokio::test]
    async fn test_watchdog_execute_with_workload_boost() {
        let watchdog = GovernorWatchdog::new();
        let result = watchdog
            .execute_with_workload_boost("llm", || async {
                Ok("inference_complete".to_string())
            })
            .await
            .expect("execute with boost");

        assert_eq!(result, "inference_complete");
        let status = watchdog.status().await;
        assert!(!status.is_boosted);
    }

    #[test]
    fn test_watchdog_execute_sync_with_workload_boost() {
        let watchdog = GovernorWatchdog::new();
        let result = watchdog.execute_sync_with_workload_boost("test", || {
            Ok(42)
        }).expect("sync boost");

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_watchdog_complete_telemetry() {
        let watchdog = GovernorWatchdog::new();
        let telem = watchdog.complete_telemetry();
        assert!(telem.cpu_pkg_temp_c > 0.0);
        assert!(telem.memory_total_mb > 0);
        assert!(telem.recommended_concurrency_limit >= 1);
    }

    #[tokio::test]
    async fn test_watchdog_adaptive_workload_guard() {
        let watchdog = GovernorWatchdog::new();
        let decision = watchdog.evaluate_workload(HardwareWorkloadKind::HeavyCompilation);
        assert!(decision.recommended_concurrency >= 1);

        let async_res = watchdog
            .execute_with_adaptive_guard(HardwareWorkloadKind::HeavyInference, |dec| async move {
                assert!(!dec.target_governor.is_empty());
                Ok("inference_adaptive_ok".to_string())
            })
            .await
            .expect("execute with adaptive guard");
        assert_eq!(async_res, "inference_adaptive_ok");
    }

    #[tokio::test]
    async fn test_watchdog_dynamic_hardware_profiler_synergy() {
        let watchdog = GovernorWatchdog::new();
        let concurrency = watchdog.optimal_task_concurrency();
        assert!(concurrency >= 1);

        let batch_size = watchdog.recommended_ollama_batch_size();
        assert!(batch_size >= 128 && batch_size <= 512);

        let profiler_telem = watchdog.hardware_profile_telemetry();
        assert!(profiler_telem.cpu_pkg_temp_c > 0.0);
        assert!(profiler_telem.memory_total_mb > 0);

        let status = watchdog.status().await;
        assert_eq!(status.optimal_concurrency, concurrency);
        assert_eq!(status.recommended_ollama_batch_size, batch_size);
        assert!(status.profiler_telemetry.is_some());
    }
}
