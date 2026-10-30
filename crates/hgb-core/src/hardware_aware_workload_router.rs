//! # Superpower 2: Dynamic Hardware-Aware Routing with Dynatune Integration
//!
//! Production-grade workload routing engine for Hagibis that inspects real-time
//! host telemetry (CPU package temperature, battery discharge rate, AC power state,
//! Linux PSI memory/CPU stall pressure, and Multi-Gen LRU) to select optimal execution
//! profiles and concurrency thresholds.
//!
//! ## Workload Kinds:
//! - `WorkloadKind::HeavyInference`: LLM text generation, speculative decoding
//! - `WorkloadKind::HeavyCompilation`: cargo build, rustc, zig build
//! - `WorkloadKind::VectorBatch`: Large-scale embedding generation and batch cosine similarity indexing
//! - `WorkloadKind::WasmSandboxed`: Sandboxed WebAssembly microkernel execution
//! - `WorkloadKind::Lightweight`: Fast AST queries, JSON formatting, metadata inspection
//!
//! ## Execution Profiles:
//! - `ExecutionProfile::MaxPerformance`:
//!   Trigger: On AC mains, CPU temp < 70°C, and RAM > 40% free.
//!   Action: Governor "performance", EPP "performance", 100% max perf, max CPU concurrency (num_cpus), full context buffer.
//! - `ExecutionProfile::BalancedAdaptive`:
//!   Trigger: AC or Battery > 50%, CPU temp 70-78°C.
//!   Action: Governor "powersave", EPP "balance_performance", 80% max perf, 50% CPU thread concurrency.
//! - `ExecutionProfile::ThermalThrottledSafe`:
//!   Trigger: CPU temp >= 78°C or Battery < 25% on discharge.
//!   Action: EPP "power", 70% max perf, concurrency throttled to 1-2 workers, compact context windows (TokenShredder), offload heavy WASM tasks to remote swarm peer if available.
//!
//! ## RAII Governor Guard:
//! - `WorkloadAdaptiveGuard`:
//!   Acquires baseline CPU power, governor, EPP, Intel P-State, MGLRU, and nice level before task execution.
//!   Applies optimal profile scaling.
//!   Deterministically restores baseline configuration on `Drop`.

use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::dynatune_bridge::{
    CompleteHardwareTelemetry, DynatuneBridge,
};
use crate::error::{HgbError, Result};
use crate::token_shredder::TokenShredder;

/// Kinds of compute workloads processed by Hagibis daemon and agent loops
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkloadKind {
    /// LLM generation, speculative decoding, large prompt synthesis
    HeavyInference,
    /// Native tool compilation (cargo build, rustc, zig, gcc)
    HeavyCompilation,
    /// Batch embedding generation, vector clustering, KNN searches
    VectorBatch,
    /// Sandboxed WASM component or microvm execution
    WasmSandboxed,
    /// AST parsing, linting, format checks, file inspection
    Lightweight,
}

impl WorkloadKind {
    pub fn is_heavy(&self) -> bool {
        matches!(self, Self::HeavyInference | Self::HeavyCompilation | Self::VectorBatch)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HeavyInference => "heavy_inference",
            Self::HeavyCompilation => "heavy_compilation",
            Self::VectorBatch => "vector_batch",
            Self::WasmSandboxed => "wasm_sandboxed",
            Self::Lightweight => "lightweight",
        }
    }
}

impl fmt::Display for WorkloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for WorkloadKind {
    type Err = HgbError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let clean = s.trim().to_lowercase().replace('-', "_");
        if clean.contains("infer") || clean.contains("llm") || clean.contains("prompt") {
            Ok(Self::HeavyInference)
        } else if clean.contains("compile") || clean.contains("build") || clean.contains("test") {
            Ok(Self::HeavyCompilation)
        } else if clean.contains("vector") || clean.contains("embed") || clean.contains("batch") {
            Ok(Self::VectorBatch)
        } else if clean.contains("wasm") || clean.contains("sandbox") {
            Ok(Self::WasmSandboxed)
        } else if clean.contains("light") || clean.contains("idle") || clean.contains("quick") {
            Ok(Self::Lightweight)
        } else {
            Ok(Self::HeavyCompilation)
        }
    }
}

impl From<WorkloadKind> for crate::dynatune_bridge::WorkloadCategory {
    fn from(k: WorkloadKind) -> Self {
        match k {
            WorkloadKind::HeavyInference => Self::LlmGeneration,
            WorkloadKind::HeavyCompilation => Self::BuildCompile,
            WorkloadKind::VectorBatch => Self::BuildCompile,
            WorkloadKind::WasmSandboxed => Self::WasmExecution,
            WorkloadKind::Lightweight => Self::IdleInterval,
        }
    }
}

impl From<WorkloadKind> for crate::dynamic_governor::WorkloadKind {
    fn from(k: WorkloadKind) -> Self {
        match k {
            WorkloadKind::HeavyInference => Self::LocalLlmInference,
            WorkloadKind::HeavyCompilation => Self::HeavyCompile,
            WorkloadKind::VectorBatch => Self::HeavyCompile,
            WorkloadKind::WasmSandboxed => Self::WasmExecution,
            WorkloadKind::Lightweight => Self::Idle,
        }
    }
}

/// Dynamic execution profiles balancing raw throughput and thermal/battery constraints
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkloadExecutionProfile {
    /// On AC mains, CPU temp < 70°C, RAM > 40% free:
    /// Governor "performance", EPP "performance", 100% max perf, max CPU concurrency (num_cpus), full context buffer.
    MaxPerformance,
    /// AC or Battery > 50%, CPU temp 70-78°C:
    /// Governor "powersave", EPP "balance_performance", 80% max perf, 50% CPU thread concurrency.
    BalancedAdaptive,
    /// CPU temp >= 78°C or Battery < 25% on discharge:
    /// Relax EPP to "power", 70% max perf, throttle concurrency to 1-2 workers, compact context windows (TokenShredder), offload heavy WASM tasks to remote swarm peer if available.
    ThermalThrottledSafe,
}

pub type ExecutionProfile = WorkloadExecutionProfile;

impl WorkloadExecutionProfile {
    pub fn governor(&self) -> &'static str {
        match self {
            Self::MaxPerformance => "performance",
            Self::BalancedAdaptive => "powersave",
            Self::ThermalThrottledSafe => "powersave",
        }
    }

    pub fn epp(&self) -> &'static str {
        match self {
            Self::MaxPerformance => "performance",
            Self::BalancedAdaptive => "balance_performance",
            Self::ThermalThrottledSafe => "power",
        }
    }

    pub fn max_perf_pct(&self) -> u32 {
        match self {
            Self::MaxPerformance => 100,
            Self::BalancedAdaptive => 80,
            Self::ThermalThrottledSafe => 70,
        }
    }

    pub fn no_turbo(&self) -> bool {
        match self {
            Self::MaxPerformance => false,
            Self::BalancedAdaptive => false,
            Self::ThermalThrottledSafe => true,
        }
    }

    pub fn mglru_min_ttl_ms(&self) -> u64 {
        match self {
            Self::MaxPerformance => 1000,
            Self::BalancedAdaptive => 500,
            Self::ThermalThrottledSafe => 0,
        }
    }

    pub fn nice_level(&self) -> i32 {
        match self {
            Self::MaxPerformance => -5,
            Self::BalancedAdaptive => 0,
            Self::ThermalThrottledSafe => 10,
        }
    }

    pub fn concurrency_threads(&self, base_cpus: usize) -> usize {
        let base = base_cpus.max(1);
        match self {
            Self::MaxPerformance => base,
            Self::BalancedAdaptive => (base / 2).max(1),
            Self::ThermalThrottledSafe => base.min(2).max(1),
        }
    }

    pub fn compact_context_windows(&self) -> bool {
        matches!(self, Self::ThermalThrottledSafe)
    }

    pub fn should_offload_wasm_to_swarm(&self) -> bool {
        matches!(self, Self::ThermalThrottledSafe)
    }
}

impl fmt::Display for WorkloadExecutionProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MaxPerformance => write!(f, "max_performance"),
            Self::BalancedAdaptive => write!(f, "balanced_adaptive"),
            Self::ThermalThrottledSafe => write!(f, "thermal_throttled_safe"),
        }
    }
}

/// Evaluated execution decision returned by `HardwareAwareWorkloadRouter`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkloadExecutionDecision {
    pub workload: WorkloadKind,
    pub profile: WorkloadExecutionProfile,
    pub target_governor: String,
    pub target_epp: String,
    pub recommended_concurrency: usize,
    pub context_compaction_recommended: bool,
    pub offload_to_swarm: bool,
    pub max_perf_pct: u32,
    pub no_turbo: bool,
    pub mglru_min_ttl_ms: u64,
    pub target_nice: i32,
    pub reason: String,
    pub cpu_temp_c: f64,
    pub battery_capacity_pct: Option<u32>,
    pub is_discharging: bool,
    pub ac_connected: bool,
    pub ram_free_pct: f64,
    pub psi_memory_some_avg10: f64,
    pub psi_cpu_some_avg10: f64,
    pub timestamp_ms: u64,
}

impl WorkloadExecutionDecision {
    pub fn should_compact_context(&self) -> bool {
        self.context_compaction_recommended
    }

    pub fn should_offload_to_swarm(&self) -> bool {
        self.offload_to_swarm
    }

    /// Compact context string using TokenShredder if profile recommends compaction
    pub fn compact_context_if_needed(&self, text: &str) -> String {
        if self.context_compaction_recommended {
            let shredder = TokenShredder::with_limits(30, 30);
            shredder.shred(text).content
        } else {
            text.to_string()
        }
    }
}

/// Core telemetry decision matrix evaluating system state against workload kind
pub fn decide_profile(
    workload: WorkloadKind,
    ac_connected: bool,
    cpu_temp_c: f64,
    battery_pct: Option<u32>,
    is_discharging: bool,
    ram_free_pct: f64,
    psi_mem_avg10: f64,
    psi_cpu_avg10: f64,
    base_threads: usize,
) -> WorkloadExecutionDecision {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Rule 1: ThermalThrottledSafe
    // Condition: temp >= 78°C or (is_discharging && battery < 25%) or severe memory exhaustion (<15% free) or high PSI stall (>35)
    let is_thermal_throttled = cpu_temp_c >= 78.0;
    let is_battery_depleted = is_discharging && battery_pct.map_or(false, |pct| pct < 25);
    let is_memory_exhausted = ram_free_pct < 15.0 || psi_mem_avg10 > 35.0;

    let (profile, reason) = if is_thermal_throttled {
        (
            WorkloadExecutionProfile::ThermalThrottledSafe,
            format!(
                "Thermal throttling engaged: CPU temp {:.1}°C >= 78.0°C. Throttling concurrency, relaxing EPP to power, and compacting context windows.",
                cpu_temp_c
            ),
        )
    } else if is_battery_depleted {
        let bat_val = battery_pct.unwrap_or(0);
        (
            WorkloadExecutionProfile::ThermalThrottledSafe,
            format!(
                "Battery power protection engaged: discharging at {}% (< 25%). Relaxing EPP to power and limiting concurrency.",
                bat_val
            ),
        )
    } else if is_memory_exhausted {
        (
            WorkloadExecutionProfile::ThermalThrottledSafe,
            format!(
                "Memory pressure protection engaged: available RAM {:.1}% (< 15%) or PSI some avg10 {:.1} > 35.0.",
                ram_free_pct, psi_mem_avg10
            ),
        )
    } else if matches!(workload, WorkloadKind::Lightweight) {
        // Lightweight tasks don't needlessly spin up MaxPerformance
        (
            WorkloadExecutionProfile::BalancedAdaptive,
            "Lightweight workload: preserving system power with balanced adaptive profile.".to_string(),
        )
    } else if ac_connected && cpu_temp_c < 70.0 && ram_free_pct > 40.0 {
        // Rule 2: MaxPerformance
        // Condition: On AC mains, temp < 70°C, RAM > 40% free
        (
            WorkloadExecutionProfile::MaxPerformance,
            format!(
                "Optimal headroom on AC mains (temp {:.1}°C < 70.0°C, RAM {:.1}% free > 40.0%): bursting governor to performance, full concurrency.",
                cpu_temp_c, ram_free_pct
            ),
        )
    } else if (ac_connected || battery_pct.map_or(false, |pct| pct > 50)) && (cpu_temp_c >= 70.0 && cpu_temp_c < 78.0) {
        // Rule 3: BalancedAdaptive
        // Condition: AC or Battery > 50%, temp 70-78°C
        (
            WorkloadExecutionProfile::BalancedAdaptive,
            format!(
                "Warm operating envelope (temp {:.1}°C in 70-78°C range) or battery > 50%: balanced adaptive profile (powersave / balance_performance).",
                cpu_temp_c
            ),
        )
    } else {
        // Default safe fallback based on power state
        if ac_connected {
            (
                WorkloadExecutionProfile::BalancedAdaptive,
                format!("Nominal dev workload on AC (temp {:.1}°C, RAM {:.1}%): balanced adaptive profile.", cpu_temp_c, ram_free_pct),
            )
        } else {
            (
                WorkloadExecutionProfile::BalancedAdaptive,
                format!("Mobile battery operation (temp {:.1}°C, RAM {:.1}%): balanced adaptive profile.", cpu_temp_c, ram_free_pct),
            )
        }
    };

    let target_governor = profile.governor().to_string();
    let target_epp = profile.epp().to_string();
    let recommended_concurrency = profile.concurrency_threads(base_threads);
    let context_compaction_recommended = profile.compact_context_windows();
    let offload_to_swarm = profile.should_offload_wasm_to_swarm() && matches!(workload, WorkloadKind::WasmSandboxed | WorkloadKind::HeavyInference);
    let max_perf_pct = profile.max_perf_pct();
    let no_turbo = profile.no_turbo();
    let mglru_min_ttl_ms = profile.mglru_min_ttl_ms();
    let target_nice = profile.nice_level();

    WorkloadExecutionDecision {
        workload,
        profile,
        target_governor,
        target_epp,
        recommended_concurrency,
        context_compaction_recommended,
        offload_to_swarm,
        max_perf_pct,
        no_turbo,
        mglru_min_ttl_ms,
        target_nice,
        reason,
        cpu_temp_c,
        battery_capacity_pct: battery_pct,
        is_discharging,
        ac_connected,
        ram_free_pct,
        psi_memory_some_avg10: psi_mem_avg10,
        psi_cpu_some_avg10: psi_cpu_avg10,
        timestamp_ms: now_ms,
    }
}

/// RAII Guard dynamically adjusting CPU governor, EPP, Intel P-State, and thread nice priority
/// during execution of heavy tasks, automatically restoring pre-task baseline settings upon drop.
pub struct WorkloadAdaptiveGuard {
    bridge: Arc<DynatuneBridge>,
    decision: WorkloadExecutionDecision,
    baseline_governor: String,
    baseline_epp: String,
    baseline_max_perf_pct: u32,
    baseline_no_turbo: bool,
    baseline_nice: i32,
    baseline_min_ttl_ms: Option<u64>,
    is_privileged: bool,
    disarmed: bool,
}

impl WorkloadAdaptiveGuard {
    /// Acquire guard and apply decision profile to host kernel
    pub fn acquire(bridge: Arc<DynatuneBridge>, decision: WorkloadExecutionDecision) -> Result<Self> {
        let is_privileged = bridge.is_privileged();
        let baseline_governor = bridge.read_scaling_governor();
        let baseline_epp = bridge.read_epp();
        let (baseline_no_turbo, baseline_max_perf_pct, _) = bridge.read_intel_pstate();
        let baseline_nice = bridge.read_nice_level();
        let baseline_min_ttl_ms = bridge.read_mglru_min_ttl_ms();

        // 1. Privileged hardware governor and EPP application
        if is_privileged {
            // Apply Intel P-State parameters
            let pstate_dir = Path::new("/sys/devices/system/cpu/intel_pstate");
            if pstate_dir.exists() {
                let no_turbo_val = if decision.no_turbo { "1\n" } else { "0\n" };
                let _ = fs::write(pstate_dir.join("no_turbo"), no_turbo_val);
                let _ = fs::write(pstate_dir.join("max_perf_pct"), format!("{}\n", decision.max_perf_pct));
                let hwp_val = if decision.profile == WorkloadExecutionProfile::MaxPerformance { "1\n" } else { "0\n" };
                let _ = fs::write(pstate_dir.join("hwp_dynamic_boost"), hwp_val);
            }

            // Apply cpufreq governors and EPP across all policies
            let cpufreq_dir = Path::new("/sys/devices/system/cpu/cpufreq");
            if let Ok(entries) = fs::read_dir(cpufreq_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        let _ = fs::write(p.join("scaling_governor"), format!("{}\n", decision.target_governor));
                        let _ = fs::write(p.join("energy_performance_preference"), format!("{}\n", decision.target_epp));
                    }
                }
            }

            // Apply MGLRU min_ttl_ms
            let _ = bridge.set_mglru_min_ttl_ms(decision.mglru_min_ttl_ms);
        }

        // 2. Unprivileged process nice priority
        let _ = bridge.set_nice_level(decision.target_nice);

        Ok(Self {
            bridge,
            decision,
            baseline_governor,
            baseline_epp,
            baseline_max_perf_pct,
            baseline_no_turbo,
            baseline_nice,
            baseline_min_ttl_ms,
            is_privileged,
            disarmed: false,
        })
    }

    /// Access active decision
    pub fn decision(&self) -> &WorkloadExecutionDecision {
        &self.decision
    }

    /// Recommended concurrency limit
    pub fn recommended_concurrency(&self) -> usize {
        self.decision.recommended_concurrency
    }

    /// Disarm guard to prevent restoration on drop
    pub fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for WorkloadAdaptiveGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }

        // 1. Restore nice level
        let _ = self.bridge.set_nice_level(self.baseline_nice);

        // 2. Restore privileged settings if previously altered
        if self.is_privileged {
            let pstate_dir = Path::new("/sys/devices/system/cpu/intel_pstate");
            if pstate_dir.exists() {
                let no_turbo_val = if self.baseline_no_turbo { "1\n" } else { "0\n" };
                let _ = fs::write(pstate_dir.join("no_turbo"), no_turbo_val);
                let _ = fs::write(pstate_dir.join("max_perf_pct"), format!("{}\n", self.baseline_max_perf_pct));
            }

            let cpufreq_dir = Path::new("/sys/devices/system/cpu/cpufreq");
            if let Ok(entries) = fs::read_dir(cpufreq_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                        let _ = fs::write(p.join("scaling_governor"), format!("{}\n", self.baseline_governor));
                        let _ = fs::write(p.join("energy_performance_preference"), format!("{}\n", self.baseline_epp));
                    }
                }
            }

            if let Some(ttl) = self.baseline_min_ttl_ms {
                let _ = self.bridge.set_mglru_min_ttl_ms(ttl);
            }
        }
    }
}

/// Hardware-Aware Workload Router interfacing with dynatune bridge
#[derive(Clone)]
pub struct HardwareAwareWorkloadRouter {
    bridge: Arc<DynatuneBridge>,
}

impl Default for HardwareAwareWorkloadRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareAwareWorkloadRouter {
    pub fn new() -> Self {
        Self {
            bridge: Arc::new(DynatuneBridge::new()),
        }
    }

    pub fn with_bridge(bridge: Arc<DynatuneBridge>) -> Self {
        Self { bridge }
    }

    pub fn bridge(&self) -> &Arc<DynatuneBridge> {
        &self.bridge
    }

    /// Evaluate current hardware telemetry and compute the optimal execution decision
    pub fn evaluate_workload(&self, workload: WorkloadKind) -> WorkloadExecutionDecision {
        let telemetry = self.bridge.read_complete_hardware_telemetry();
        self.evaluate_telemetry(workload, &telemetry)
    }

    /// Evaluate given hardware telemetry against a workload
    pub fn evaluate_telemetry(&self, workload: WorkloadKind, telemetry: &CompleteHardwareTelemetry) -> WorkloadExecutionDecision {
        let base_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let primary_bat = telemetry.batteries.first();
        let battery_pct = primary_bat.map(|b| b.capacity_pct);
        let is_discharging = primary_bat.map(|b| b.is_discharging).unwrap_or(false);

        decide_profile(
            workload,
            telemetry.ac_connected,
            telemetry.cpu_pkg_temp_c,
            battery_pct,
            is_discharging,
            telemetry.memory_available_pct,
            telemetry.psi_memory.some.avg10,
            telemetry.psi_cpu.some.avg10,
            base_threads,
        )
    }

    /// Acquire RAII guard for the given workload
    pub fn acquire_guard(&self, workload: WorkloadKind) -> Result<WorkloadAdaptiveGuard> {
        let decision = self.evaluate_workload(workload);
        WorkloadAdaptiveGuard::acquire(Arc::clone(&self.bridge), decision)
    }

    /// Execute a synchronous task wrapped with WorkloadAdaptiveGuard
    pub fn execute_with_guard<F, R>(&self, workload: WorkloadKind, task: F) -> Result<R>
    where
        F: FnOnce(&WorkloadExecutionDecision) -> Result<R>,
    {
        let guard = self.acquire_guard(workload)?;
        task(guard.decision())
    }

    /// Execute an asynchronous task wrapped with WorkloadAdaptiveGuard
    pub async fn execute_async_with_guard<F, Fut, R>(&self, workload: WorkloadKind, task: F) -> Result<R>
    where
        F: FnOnce(WorkloadExecutionDecision) -> Fut,
        Fut: std::future::Future<Output = Result<R>>,
    {
        let guard = self.acquire_guard(workload)?;
        let decision = guard.decision().clone();
        let result = task(decision).await;
        drop(guard);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_matrix_max_performance() {
        // AC connected, cool (55°C < 70°C), plentiful RAM (60% > 40%)
        let decision = decide_profile(
            WorkloadKind::HeavyCompilation,
            true,  // ac_connected
            55.0,  // cpu_temp_c
            None,  // battery_pct
            false, // is_discharging
            60.0,  // ram_free_pct
            0.0,   // psi_mem
            0.0,   // psi_cpu
            8,     // base_threads
        );

        assert_eq!(decision.profile, WorkloadExecutionProfile::MaxPerformance);
        assert_eq!(decision.target_governor, "performance");
        assert_eq!(decision.target_epp, "performance");
        assert_eq!(decision.max_perf_pct, 100);
        assert!(!decision.no_turbo);
        assert_eq!(decision.recommended_concurrency, 8);
        assert!(!decision.context_compaction_recommended);
        assert!(!decision.offload_to_swarm);
    }

    #[test]
    fn test_decision_matrix_balanced_adaptive_warm() {
        // AC connected, warm (74°C in 70-78°C range)
        let decision = decide_profile(
            WorkloadKind::HeavyInference,
            true,  // ac_connected
            74.0,  // cpu_temp_c
            None,  // battery_pct
            false, // is_discharging
            50.0,  // ram_free_pct
            2.0,   // psi_mem
            1.0,   // psi_cpu
            8,     // base_threads
        );

        assert_eq!(decision.profile, WorkloadExecutionProfile::BalancedAdaptive);
        assert_eq!(decision.target_governor, "powersave");
        assert_eq!(decision.target_epp, "balance_performance");
        assert_eq!(decision.max_perf_pct, 80);
        assert_eq!(decision.recommended_concurrency, 4); // 50% concurrency
        assert!(!decision.context_compaction_recommended);
        assert!(!decision.offload_to_swarm);
    }

    #[test]
    fn test_decision_matrix_thermal_throttling() {
        // Overheat threshold: 79.5°C >= 78.0°C
        let decision = decide_profile(
            WorkloadKind::HeavyCompilation,
            true,  // ac_connected
            79.5,  // cpu_temp_c
            None,  // battery_pct
            false, // is_discharging
            50.0,  // ram_free_pct
            0.0,   // psi_mem
            0.0,   // psi_cpu
            8,     // base_threads
        );

        assert_eq!(decision.profile, WorkloadExecutionProfile::ThermalThrottledSafe);
        assert_eq!(decision.target_governor, "powersave");
        assert_eq!(decision.target_epp, "power");
        assert_eq!(decision.max_perf_pct, 70);
        assert!(decision.no_turbo);
        assert_eq!(decision.recommended_concurrency, 2); // Throttled to 1-2 workers
        assert!(decision.context_compaction_recommended);
    }

    #[test]
    fn test_decision_matrix_battery_critical() {
        // Battery discharging at 18% (< 25%)
        let decision = decide_profile(
            WorkloadKind::HeavyInference,
            false,      // ac_connected
            62.0,       // cpu_temp_c
            Some(18),   // battery_pct
            true,       // is_discharging
            50.0,       // ram_free_pct
            0.0,        // psi_mem
            0.0,        // psi_cpu
            4,          // base_threads
        );

        assert_eq!(decision.profile, WorkloadExecutionProfile::ThermalThrottledSafe);
        assert_eq!(decision.target_governor, "powersave");
        assert_eq!(decision.target_epp, "power");
        assert_eq!(decision.recommended_concurrency, 2);
        assert!(decision.context_compaction_recommended);
        assert!(decision.offload_to_swarm);
    }

    #[test]
    fn test_decision_matrix_lightweight_workload() {
        // Even with cool AC headroom, Lightweight workload selects BalancedAdaptive
        let decision = decide_profile(
            WorkloadKind::Lightweight,
            true,  // ac_connected
            45.0,  // cpu_temp_c
            None,  // battery_pct
            false, // is_discharging
            70.0,  // ram_free_pct
            0.0,   // psi_mem
            0.0,   // psi_cpu
            8,     // base_threads
        );

        assert_eq!(decision.profile, WorkloadExecutionProfile::BalancedAdaptive);
        assert_eq!(decision.target_governor, "powersave");
        assert_eq!(decision.target_epp, "balance_performance");
    }

    #[test]
    fn test_adaptive_concurrency_scaling() {
        let base_cpus = 8;
        assert_eq!(WorkloadExecutionProfile::MaxPerformance.concurrency_threads(base_cpus), 8);
        assert_eq!(WorkloadExecutionProfile::BalancedAdaptive.concurrency_threads(base_cpus), 4);
        assert_eq!(WorkloadExecutionProfile::ThermalThrottledSafe.concurrency_threads(base_cpus), 2);

        // Edge case: single core
        let single_cpu = 1;
        assert_eq!(WorkloadExecutionProfile::MaxPerformance.concurrency_threads(single_cpu), 1);
        assert_eq!(WorkloadExecutionProfile::BalancedAdaptive.concurrency_threads(single_cpu), 1);
        assert_eq!(WorkloadExecutionProfile::ThermalThrottledSafe.concurrency_threads(single_cpu), 1);
    }

    #[test]
    fn test_context_compaction_with_token_shredder() {
        let mut decision = decide_profile(
            WorkloadKind::HeavyInference,
            false,
            81.0, // Overheating
            Some(50),
            false,
            50.0,
            0.0,
            0.0,
            4,
        );

        assert!(decision.context_compaction_recommended);
        let sample = "Line 1\nLine 2\nLine 3\n";
        let compacted = decision.compact_context_if_needed(sample);
        assert!(!compacted.is_empty());

        decision.context_compaction_recommended = false;
        let untouched = decision.compact_context_if_needed(sample);
        assert_eq!(untouched, sample);
    }

    #[test]
    fn test_workload_adaptive_guard_raii_restoration() {
        let bridge = Arc::new(DynatuneBridge::new());
        let baseline_nice = bridge.read_nice_level();

        {
            let router = HardwareAwareWorkloadRouter::with_bridge(Arc::clone(&bridge));
            let guard = router.acquire_guard(WorkloadKind::HeavyCompilation).expect("acquire guard");
            assert!(guard.recommended_concurrency() >= 1);
            assert!(!guard.decision().target_governor.is_empty());
        }

        // On drop, nice level was restored
        let restored_nice = bridge.read_nice_level();
        assert_eq!(restored_nice, baseline_nice);
    }

    #[test]
    fn test_workload_adaptive_guard_disarm() {
        let bridge = Arc::new(DynatuneBridge::new());
        let router = HardwareAwareWorkloadRouter::with_bridge(bridge);
        let mut guard = router.acquire_guard(WorkloadKind::HeavyCompilation).expect("guard");
        guard.disarm();
    }

    #[test]
    fn test_hardware_aware_router_sync_and_async_exec() {
        let router = HardwareAwareWorkloadRouter::new();

        let sync_res = router.execute_with_guard(WorkloadKind::VectorBatch, |decision| {
            assert!(decision.recommended_concurrency >= 1);
            Ok("vector_batch_complete".to_string())
        }).expect("sync exec");
        assert_eq!(sync_res, "vector_batch_complete");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let async_res = rt.block_on(async {
            router.execute_async_with_guard(WorkloadKind::HeavyInference, |decision| async move {
                assert!(decision.recommended_concurrency >= 1);
                Ok(12345)
            }).await
        }).expect("async exec");
        assert_eq!(async_res, 12345);
    }

    #[test]
    fn test_workload_kind_parsing_and_conversions() {
        assert_eq!(WorkloadKind::from_str("inference").unwrap(), WorkloadKind::HeavyInference);
        assert_eq!(WorkloadKind::from_str("compile").unwrap(), WorkloadKind::HeavyCompilation);
        assert_eq!(WorkloadKind::from_str("batch_vector").unwrap(), WorkloadKind::VectorBatch);
        assert_eq!(WorkloadKind::from_str("wasm_exec").unwrap(), WorkloadKind::WasmSandboxed);
        assert_eq!(WorkloadKind::from_str("light_ast").unwrap(), WorkloadKind::Lightweight);

        assert!(WorkloadKind::HeavyInference.is_heavy());
        assert!(WorkloadKind::HeavyCompilation.is_heavy());
        assert!(!WorkloadKind::Lightweight.is_heavy());
    }
}
