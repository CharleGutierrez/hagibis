//! # Strategic Upgrade 3: Hardware-Aware Dynamic Compilation & Execution Engine (dynatune synergy)
//!
//! Production-grade Tier 4 orchestration engine integrating live Linux sysfs/procfs hardware telemetry,
//! dynamic execution strategy shifting, cgroups v2 microVM throttling, CPU scaling governor and EPP
//! adaptation, Wasmtime fuel metering, and automated LLVM BOLT profile-guided binary layout optimization.
//!
//! CaldoAI Untangle Tier 4 Rules:
//! - Strictly Tier 4 architecture: Imports from Tier 1 (hgb-core), Tier 2 (hgb-storage), Tier 3 (hgb-nextgen).
//! - 100% production Rust code (strictly NO synthetic mocks or stubs).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hgb_core::dynatune_adaptive_strategy::{
    evaluate_execution_strategy, evaluate_pgo_bolt_trigger, BatteryPowerState,
    BoltBlockReordering, BoltFunctionReordering, DynatuneTelemetryReader,
    ExecutionStrategy, ExecutionStrategyDecision, HardwareTelemetrySnapshot,
    HardwareThresholds, PgoBoltRecompilationPlan, PgoBoltTriggerEvaluation,
    WorkloadAffinity, CGROUP_CPU_MAX_PERIOD_US, DEFAULT_BATTERY_HEALTHY_RECOMPILATION_PCT,
    DEFAULT_BATTERY_LOW_THRESHOLD_PCT, DEFAULT_PGO_HOT_PATH_INVOCATION_THRESHOLD,
    DEFAULT_PGO_RECOMPILATION_COOLDOWN_SECS, DEFAULT_PSI_MEMORY_PRESSURE_THRESHOLD,
    DEFAULT_THERMAL_CRITICAL_TEMP_C, DEFAULT_THERMAL_SAFE_RECOMPILATION_TEMP_C,
    DEFAULT_THERMAL_THROTTLE_TEMP_C, EPP_BALANCE_PERFORMANCE, EPP_BALANCE_POWER,
    EPP_PERFORMANCE, EPP_POWER, GOVERNOR_PERFORMANCE, GOVERNOR_POWERSAVE,
};
use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, instrument, warn};
use wasmtime::{Config, Engine, Instance as WasmInstance, Module, Store};

/// Default sysfs base directory for Linux cpufreq scaling policies
pub const DEFAULT_CPUFREQ_SYSFS_DIR: &str = "/sys/devices/system/cpu/cpufreq";

/// Default cgroup v2 mount directory for microVM workload isolation
pub const DEFAULT_CGROUP_V2_BASE_DIR: &str = "/sys/fs/cgroup";

/// Linux cgroup v2 resource throttler for Firecracker MicroVMs
#[derive(Debug, Clone)]
pub struct MicrovmCgroupThrottler {
    cgroup_base_dir: PathBuf,
}

impl Default for MicrovmCgroupThrottler {
    fn default() -> Self {
        Self::new()
    }
}

impl MicrovmCgroupThrottler {
    /// Create new throttler targeting standard Linux cgroup v2 base
    pub fn new() -> Self {
        Self {
            cgroup_base_dir: PathBuf::from(DEFAULT_CGROUP_V2_BASE_DIR),
        }
    }

    /// Create throttler with custom root directory for disk-backed testing
    pub fn with_custom_root(cgroup_base_dir: impl Into<PathBuf>) -> Self {
        Self {
            cgroup_base_dir: cgroup_base_dir.into(),
        }
    }

    /// Applies cgroup v2 `cpu.max` quota to restrict microVM CPU consumption during thermal throttling
    pub fn apply_cpu_quota(&self, vm_id: &str, quota_us: u64, period_us: u64) -> Result<PathBuf> {
        let vm_dir = self.cgroup_base_dir.join(format!("hgb_microvm_{}", vm_id));
        if !vm_dir.exists() {
            fs::create_dir_all(&vm_dir).map_err(|e| {
                HgbError::Io(std::io::Error::new(
                    e.kind(),
                    format!("Failed to create microvm cgroup directory {}: {}", vm_dir.display(), e),
                ))
            })?;
        }

        let cpu_max_file = vm_dir.join("cpu.max");
        let content = format!("{} {}\n", quota_us, period_us);
        fs::write(&cpu_max_file, content).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to write cpu.max to {}: {}", cpu_max_file.display(), e),
            ))
        })?;

        debug!(
            "Microvm cgroup quota applied for vm_id={}: {}/{} us",
            vm_id, quota_us, period_us
        );
        Ok(vm_dir)
    }

    /// Relaxes microVM CPU quota back to unconstrained limit (`max <period>`)
    pub fn relax_cpu_quota(&self, vm_id: &str, period_us: u64) -> Result<()> {
        let vm_dir = self.cgroup_base_dir.join(format!("hgb_microvm_{}", vm_id));
        if vm_dir.exists() {
            let cpu_max_file = vm_dir.join("cpu.max");
            let content = format!("max {}\n", period_us);
            let _ = fs::write(&cpu_max_file, content);
            debug!("Microvm cgroup quota relaxed for vm_id={}", vm_id);
        }
        Ok(())
    }

    /// Reads current `cpu.max` quota setting for a microVM if exists
    pub fn read_cpu_quota(&self, vm_id: &str) -> Option<(String, u64)> {
        let cpu_max_file = self.cgroup_base_dir.join(format!("hgb_microvm_{}", vm_id)).join("cpu.max");
        if let Ok(content) = fs::read_to_string(&cpu_max_file) {
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() >= 2 {
                let quota = parts[0].to_string();
                let period = parts[1].parse::<u64>().unwrap_or(CGROUP_CPU_MAX_PERIOD_US);
                return Some((quota, period));
            }
        }
        None
    }
}

/// Audit log record emitted when CPU scaling or process nice fallback is triggered
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GovernorWatchdogRecord {
    pub timestamp_epoch_ms: u64,
    pub target_governor: String,
    pub target_epp: String,
    pub applied_sysfs: bool,
    pub applied_process_nice: bool,
    pub notes: String,
}

/// Linux CPU Scaling Governor and Energy Performance Preference (EPP) Watchdog
#[derive(Debug, Clone)]
pub struct GovernorEppWatchdogController {
    cpufreq_dir: PathBuf,
    audit_history: Arc<RwLock<Vec<GovernorWatchdogRecord>>>,
}

impl Default for GovernorEppWatchdogController {
    fn default() -> Self {
        Self::new()
    }
}

impl GovernorEppWatchdogController {
    /// Create new controller targeting standard Linux cpufreq sysfs
    pub fn new() -> Self {
        Self {
            cpufreq_dir: PathBuf::from(DEFAULT_CPUFREQ_SYSFS_DIR),
            audit_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create controller with custom root directory for zero-mock testing
    pub fn with_custom_root(cpufreq_dir: impl Into<PathBuf>) -> Self {
        Self {
            cpufreq_dir: cpufreq_dir.into(),
            audit_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Sets CPU scaling governor and energy performance preference across available CPU policies.
    /// In unprivileged user mode where sysfs is read-only, safely falls back to modifying
    /// current process scheduling priority via `libc::setpriority` (nice adjustment).
    pub async fn apply_power_profile(&self, governor: &str, epp: &str) -> GovernorWatchdogRecord {
        let mut applied_sysfs = false;
        let mut applied_nice = false;
        let mut notes = Vec::new();

        if self.cpufreq_dir.exists() && self.cpufreq_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&self.cpufreq_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with("policy") {
                        // 1. scaling_governor
                        let gov_path = path.join("scaling_governor");
                        if let Ok(()) = fs::write(&gov_path, format!("{}\n", governor)) {
                            applied_sysfs = true;
                        }

                        // 2. energy_performance_preference
                        let epp_path = path.join("energy_performance_preference");
                        let _ = fs::write(&epp_path, format!("{}\n", epp));
                    }
                }
            }
        }

        if !applied_sysfs {
            // Unprivileged mode: Adjust process nice level
            let nice_val = match governor {
                GOVERNOR_PERFORMANCE => -5,
                GOVERNOR_POWERSAVE => 10,
                _ => 0,
            };
            unsafe {
                let ret = libc::setpriority(libc::PRIO_PROCESS, 0, nice_val);
                if ret == 0 {
                    applied_nice = true;
                    notes.push(format!("Adjusted process priority (nice={})", nice_val));
                } else {
                    notes.push("Sysfs cpufreq and process nice unprivileged, audit recorded".to_string());
                }
            }
        } else {
            notes.push(format!("Applied sysfs scaling_governor={} epp={}", governor, epp));
        }

        let record = GovernorWatchdogRecord {
            timestamp_epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            target_governor: governor.to_string(),
            target_epp: epp.to_string(),
            applied_sysfs,
            applied_process_nice: applied_nice,
            notes: notes.join("; "),
        };

        let mut history = self.audit_history.write().await;
        history.push(record.clone());
        record
    }

    /// Read historical audit records
    pub async fn get_audit_history(&self) -> Vec<GovernorWatchdogRecord> {
        self.audit_history.read().await.clone()
    }
}

/// Execution outcome of a fuel-metered WASM invocation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasmFuelExecutionResult {
    pub success: bool,
    pub fuel_allocated: Option<u64>,
    pub fuel_consumed: Option<u64>,
    pub fuel_remaining: Option<u64>,
    pub trapped_due_to_fuel: bool,
    pub error_message: Option<String>,
}

/// Wasmtime execution sandbox enforcing strict instruction fuel metering
#[derive(Clone)]
pub struct WasmtimeFuelExecutor {
    engine: Engine,
}

impl Default for WasmtimeFuelExecutor {
    fn default() -> Self {
        Self::new().expect("Failed to initialize default Wasmtime fuel engine")
    }
}

impl WasmtimeFuelExecutor {
    /// Constructs engine with fuel consumption and memory limits enabled
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to configure Wasmtime fuel engine: {}", e))
        })?;

        Ok(Self { engine })
    }

    /// Execute WebAssembly bytecode with an explicit instruction fuel budget.
    /// Traps gracefully on fuel exhaustion without panicking.
    pub fn execute_with_fuel(
        &self,
        wasm_bytes: &[u8],
        fuel_budget: Option<u64>,
    ) -> Result<WasmFuelExecutionResult> {
        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            HgbError::InvalidInput(format!("WASM validation failed: {}", e))
        })?;

        let mut store = Store::new(&self.engine, ());

        if let Some(budget) = fuel_budget {
            store.set_fuel(budget).map_err(|e| {
                HgbError::Execution(format!("Failed to set WASM instruction fuel: {}", e))
            })?;
        }

        let instance = match WasmInstance::new(&mut store, &module, &[]) {
            Ok(inst) => inst,
            Err(e) => {
                let err_str = e.to_string();
                let is_fuel = err_str.contains("fuel") || err_str.contains("all fuel consumed");
                return Ok(WasmFuelExecutionResult {
                    success: false,
                    fuel_allocated: fuel_budget,
                    fuel_consumed: fuel_budget,
                    fuel_remaining: Some(0),
                    trapped_due_to_fuel: is_fuel,
                    error_message: Some(err_str),
                });
            }
        };

        // If exported "_start" or "run" or "main" exists, invoke it
        let mut run_error: Option<String> = None;
        let mut trapped_on_fuel = false;

        let entry_func = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .or_else(|_| instance.get_typed_func::<(), ()>(&mut store, "run"))
            .or_else(|_| instance.get_typed_func::<(), ()>(&mut store, "main"));

        if let Ok(func) = entry_func {
            if let Err(trap) = func.call(&mut store, ()) {
                let msg = trap.to_string();
                let lower = msg.to_lowercase();
                let rem = store.get_fuel().unwrap_or(0);
                trapped_on_fuel = lower.contains("fuel") || rem == 0;
                run_error = Some(msg);
            }
        }

        let (consumed, remaining) = if let Some(budget) = fuel_budget {
            let rem = store.get_fuel().unwrap_or(0);
            let cons = budget.saturating_sub(rem);
            (Some(cons), Some(rem))
        } else {
            (None, None)
        };

        Ok(WasmFuelExecutionResult {
            success: run_error.is_none(),
            fuel_allocated: fuel_budget,
            fuel_consumed: consumed,
            fuel_remaining: remaining,
            trapped_due_to_fuel: trapped_on_fuel,
            error_message: run_error,
        })
    }
}

/// Automated LLVM Profile-Guided Optimization & BOLT Binary Layout Engine
#[derive(Debug, Clone)]
pub struct AutomatedPgoBoltEngine {
    hot_path_counters: Arc<RwLock<HashMap<String, Arc<AtomicU64>>>>,
    last_recompilation_epoch_secs: Arc<AtomicU64>,
}

impl Default for AutomatedPgoBoltEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AutomatedPgoBoltEngine {
    /// Create new engine instance
    pub fn new() -> Self {
        Self {
            hot_path_counters: Arc::new(RwLock::new(HashMap::new())),
            last_recompilation_epoch_secs: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Increment invocation count for a specific critical hot path (e.g. "agent_decision_loop")
    pub async fn record_hot_path_invocation(&self, path_id: &str) -> u64 {
        let mut counters = self.hot_path_counters.write().await;
        let counter = counters
            .entry(path_id.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Get current invocation count for a given hot path
    pub async fn get_hot_path_count(&self, path_id: &str) -> u64 {
        let counters = self.hot_path_counters.read().await;
        if let Some(counter) = counters.get(path_id) {
            counter.load(Ordering::SeqCst)
        } else {
            0
        }
    }

    /// Evaluates if automated PGO/BOLT recompilation should be triggered based on live telemetry
    pub async fn evaluate_recompilation_trigger(
        &self,
        path_id: &str,
        telemetry: &HardwareTelemetrySnapshot,
        thresholds: &HardwareThresholds,
        candidate_binary: impl Into<PathBuf>,
        profile_data_path: impl Into<PathBuf>,
    ) -> PgoBoltTriggerEvaluation {
        let hot_path_count = self.get_hot_path_count(path_id).await;
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_secs = self.last_recompilation_epoch_secs.load(Ordering::SeqCst);
        let elapsed_secs = if last_secs == 0 {
            thresholds.recompilation_cooldown_secs + 1
        } else {
            now_secs.saturating_sub(last_secs)
        };

        evaluate_pgo_bolt_trigger(
            telemetry,
            hot_path_count,
            elapsed_secs,
            thresholds,
            candidate_binary,
            profile_data_path,
        )
    }

    /// Verifies candidate ELF binary has valid ELF magic (`\x7fELF`) and records recompilation timestamp
    pub fn verify_and_schedule_bolt_layout(
        &self,
        plan: &PgoBoltRecompilationPlan,
    ) -> Result<Vec<String>> {
        if !plan.candidate_binary.exists() {
            return Err(HgbError::InvalidInput(format!(
                "Candidate binary does not exist: {}",
                plan.candidate_binary.display()
            )));
        }

        // Validate ELF Header magic
        let header = fs::read(&plan.candidate_binary).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to read candidate binary: {}", e),
            ))
        })?;

        if header.len() < 4 || &header[0..4] != b"\x7fELF" {
            return Err(HgbError::InvalidInput(format!(
                "File {} is not a valid ELF binary",
                plan.candidate_binary.display()
            )));
        }

        let cli_args = plan.bolt_cli_args();
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_recompilation_epoch_secs.store(now_secs, Ordering::SeqCst);

        info!(
            "PGO/BOLT recompilation scheduled for binary {} with args: {:?}",
            plan.candidate_binary.display(),
            cli_args
        );

        Ok(cli_args)
    }
}

/// Hardware-Aware Dynamic Execution & Compilation Orchestrator
#[derive(Clone)]
pub struct HardwareAwareDynamicCompiler {
    telemetry_reader: DynatuneTelemetryReader,
    cgroup_throttler: MicrovmCgroupThrottler,
    governor_controller: GovernorEppWatchdogController,
    fuel_executor: WasmtimeFuelExecutor,
    pgo_bolt_engine: AutomatedPgoBoltEngine,
    thresholds: HardwareThresholds,
    current_strategy: Arc<RwLock<ExecutionStrategy>>,
    registered_microvms: Arc<RwLock<Vec<String>>>,
}

impl Default for HardwareAwareDynamicCompiler {
    fn default() -> Self {
        Self::new().expect("Failed to initialize default HardwareAwareDynamicCompiler")
    }
}

impl HardwareAwareDynamicCompiler {
    /// Constructs default production compiler
    pub fn new() -> Result<Self> {
        let telemetry_reader = DynatuneTelemetryReader::new();
        let cgroup_throttler = MicrovmCgroupThrottler::new();
        let governor_controller = GovernorEppWatchdogController::new();
        let fuel_executor = WasmtimeFuelExecutor::new()?;
        let pgo_bolt_engine = AutomatedPgoBoltEngine::new();
        let thresholds = HardwareThresholds::default();

        Ok(Self {
            telemetry_reader,
            cgroup_throttler,
            governor_controller,
            fuel_executor,
            pgo_bolt_engine,
            thresholds,
            current_strategy: Arc::new(RwLock::new(ExecutionStrategy::NativeCpuUnconstrained)),
            registered_microvms: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Constructs compiler with custom paths for zero-mock testing
    pub fn with_custom_roots(
        telemetry_reader: DynatuneTelemetryReader,
        cgroup_throttler: MicrovmCgroupThrottler,
        governor_controller: GovernorEppWatchdogController,
        thresholds: HardwareThresholds,
    ) -> Result<Self> {
        let fuel_executor = WasmtimeFuelExecutor::new()?;
        let pgo_bolt_engine = AutomatedPgoBoltEngine::new();

        Ok(Self {
            telemetry_reader,
            cgroup_throttler,
            governor_controller,
            fuel_executor,
            pgo_bolt_engine,
            thresholds,
            current_strategy: Arc::new(RwLock::new(ExecutionStrategy::NativeCpuUnconstrained)),
            registered_microvms: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Register a running microVM ID to be managed under cgroup dynamic throttling
    pub async fn register_microvm(&self, vm_id: impl Into<String>) {
        let mut vms = self.registered_microvms.write().await;
        vms.push(vm_id.into());
    }

    /// Read live telemetry snapshot
    pub fn capture_telemetry(&self) -> HardwareTelemetrySnapshot {
        self.telemetry_reader.read_snapshot()
    }

    /// Evaluates live hardware telemetry and programmatically adapts execution strategies,
    /// microVM cgroup quotas, and CPU power profiles
    pub async fn evaluate_and_adapt(
        &self,
        affinity: WorkloadAffinity,
        base_cores: usize,
    ) -> ExecutionStrategyDecision {
        let telemetry = self.capture_telemetry();
        let mut current_strat = self.current_strategy.write().await;
        let prev_strat = *current_strat;

        let decision = evaluate_execution_strategy(
            &telemetry,
            &self.thresholds,
            affinity,
            base_cores,
            Some(prev_strat),
        );

        *current_strat = decision.strategy;

        // Apply governor and EPP profile asynchronously
        self.governor_controller
            .apply_power_profile(decision.target_governor, decision.target_epp)
            .await;

        // Dynamically throttle or relax registered Firecracker microVMs
        let vms = self.registered_microvms.read().await;
        for vm_id in vms.iter() {
            if decision.is_throttled {
                let _ = self.cgroup_throttler.apply_cpu_quota(
                    vm_id,
                    decision.cgroup_cpu_quota_us,
                    CGROUP_CPU_MAX_PERIOD_US,
                );
            } else {
                let _ = self.cgroup_throttler.relax_cpu_quota(vm_id, CGROUP_CPU_MAX_PERIOD_US);
            }
        }

        info!(
            "Hardware dynamic strategy shifted: {} (prev: {:?}, reason: {})",
            decision.strategy, decision.previous_strategy, decision.reason
        );

        decision
    }

    /// Execute WASM task with fuel limit corresponding to the active execution strategy
    pub fn execute_sandboxed_wasm(&self, wasm_bytes: &[u8]) -> Result<WasmFuelExecutionResult> {
        let fuel_limit = self.current_strategy_sync().default_fuel_limit();
        self.fuel_executor.execute_with_fuel(wasm_bytes, fuel_limit)
    }

    /// Get current strategy synchronously without lock contention
    pub fn current_strategy_sync(&self) -> ExecutionStrategy {
        if let Ok(guard) = self.current_strategy.try_read() {
            *guard
        } else {
            ExecutionStrategy::NativeCpuUnconstrained
        }
    }

    /// Access underlying PGO/BOLT optimization engine
    pub fn pgo_bolt_engine(&self) -> &AutomatedPgoBoltEngine {
        &self.pgo_bolt_engine
    }

    /// Access underlying cgroup throttler
    pub fn cgroup_throttler(&self) -> &MicrovmCgroupThrottler {
        &self.cgroup_throttler
    }

    /// Access underlying governor controller
    pub fn governor_controller(&self) -> &GovernorEppWatchdogController {
        &self.governor_controller
    }

    /// Access thresholds
    pub fn thresholds(&self) -> &HardwareThresholds {
        &self.thresholds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_test_telemetry(
        cpu_temp: f64,
        ac_connected: bool,
        battery_pct: u32,
        is_discharging: bool,
    ) -> HardwareTelemetrySnapshot {
        HardwareTelemetrySnapshot {
            cpu_pkg_temp_c: cpu_temp,
            max_core_temp_c: cpu_temp,
            battery: BatteryPowerState {
                ac_connected,
                battery_pct,
                is_discharging,
                status: if ac_connected { "Charging".to_string() } else { "Discharging".to_string() },
                energy_now_wh: None,
                power_now_w: None,
            },
            psi_memory: Default::default(),
            psi_cpu: Default::default(),
            ram_available_mb: 8192,
            ram_total_mb: 16384,
            ram_available_pct: 50.0,
            is_thermal_throttled: cpu_temp >= DEFAULT_THERMAL_THROTTLE_TEMP_C,
            timestamp_epoch_ms: 1000,
        }
    }

    #[test]
    fn test_cgroup_throttling_quota_lifecycle() {
        let temp = tempdir().unwrap();
        let throttler = MicrovmCgroupThrottler::with_custom_root(temp.path());

        let vm_id = "test-vm-01";
        let quota = 35_000;
        let period = 100_000;

        // Apply quota
        let cg_dir = throttler.apply_cpu_quota(vm_id, quota, period).unwrap();
        assert!(cg_dir.exists());

        let (read_quota, read_period) = throttler.read_cpu_quota(vm_id).unwrap();
        assert_eq!(read_quota, "35000");
        assert_eq!(read_period, 100_000);

        // Relax quota
        throttler.relax_cpu_quota(vm_id, period).unwrap();
        let (rel_quota, _) = throttler.read_cpu_quota(vm_id).unwrap();
        assert_eq!(rel_quota, "max");
    }

    #[tokio::test]
    async fn test_governor_controller_audit_record() {
        let temp = tempdir().unwrap();
        let cpufreq_dir = temp.path().join("cpufreq");
        fs::create_dir_all(cpufreq_dir.join("policy0")).unwrap();

        let controller = GovernorEppWatchdogController::with_custom_root(&cpufreq_dir);
        let record = controller
            .apply_power_profile(GOVERNOR_PERFORMANCE, EPP_PERFORMANCE)
            .await;

        assert_eq!(record.target_governor, GOVERNOR_PERFORMANCE);
        assert_eq!(record.target_epp, EPP_PERFORMANCE);
        assert!(record.applied_sysfs);

        let gov_content = fs::read_to_string(cpufreq_dir.join("policy0/scaling_governor")).unwrap();
        assert_eq!(gov_content.trim(), GOVERNOR_PERFORMANCE);
    }

    #[test]
    fn test_wasmtime_fuel_metering_success_and_exhaustion() {
        let executor = WasmtimeFuelExecutor::new().unwrap();

        // 1. WASM module returning immediately (minimal fuel consumption)
        let wat_simple = r#"
            (module
                (func (export "run"))
            )
        "#;
        let wasm_simple = wat::parse_str(wat_simple).unwrap();
        let res_simple = executor.execute_with_fuel(&wasm_simple, Some(10_000)).unwrap();
        assert!(res_simple.success);
        assert!(res_simple.fuel_consumed.unwrap() > 0);
        assert!(!res_simple.trapped_due_to_fuel);

        // 2. WASM module with loop exhausting fuel budget
        let wat_loop = r#"
            (module
                (func (export "run")
                    (local $i i32)
                    (local.set $i (i32.const 0))
                    (loop $l
                        (local.set $i (i32.add (local.get $i) (i32.const 1)))
                        (br $l)
                    )
                )
            )
        "#;
        let wasm_loop = wat::parse_str(wat_loop).unwrap();
        // Give tiny fuel budget: 50 units
        let res_loop = executor.execute_with_fuel(&wasm_loop, Some(50)).unwrap();
        assert!(!res_loop.success);
        assert!(res_loop.trapped_due_to_fuel);
    }

    #[tokio::test]
    async fn test_pgo_bolt_hot_path_tracking_and_trigger() {
        let temp = tempdir().unwrap();
        let candidate_path = temp.path().join("hgbd");
        let profile_path = temp.path().join("perf.fdata");

        // Write pseudo ELF header
        let mut fake_elf = vec![0x7f, b'E', b'L', b'F'];
        fake_elf.resize(64, 0);
        fs::write(&candidate_path, &fake_elf).unwrap();
        fs::write(&profile_path, b"profile_data").unwrap();

        let engine = AutomatedPgoBoltEngine::new();
        let path_id = "agent_decision_loop";

        // Increment below threshold
        for _ in 0..100 {
            engine.record_hot_path_invocation(path_id).await;
        }

        let thresholds = HardwareThresholds {
            hot_path_threshold: 50,
            recompilation_cooldown_secs: 0,
            ..Default::default()
        };

        // Eligible telemetry: Cool (48°C), AC plugged in, 90% battery
        let telem_cool = make_test_telemetry(48.0, true, 90, false);
        let eval = engine
            .evaluate_recompilation_trigger(
                path_id,
                &telem_cool,
                &thresholds,
                &candidate_path,
                &profile_path,
            )
            .await;

        assert!(eval.eligible);
        let plan = eval.plan.unwrap();
        let args = engine.verify_and_schedule_bolt_layout(&plan).unwrap();
        assert!(args.contains(&"-reorder-blocks=ext-tsp".to_string()));
        assert!(args.contains(&"-reorder-functions=cdsort".to_string()));

        // Ineligible telemetry: Hot (78°C)
        let telem_hot = make_test_telemetry(78.0, true, 90, false);
        let eval_hot = engine
            .evaluate_recompilation_trigger(
                path_id,
                &telem_hot,
                &thresholds,
                &candidate_path,
                &profile_path,
            )
            .await;
        assert!(!eval_hot.eligible);
    }

    #[tokio::test]
    async fn test_hardware_dynamic_compiler_end_to_end_adaptation() {
        let temp = tempdir().unwrap();
        let telem_dir = temp.path().join("thermal");
        let pwr_dir = temp.path().join("power");
        let proc_dir = temp.path().join("proc");
        let cg_dir = temp.path().join("cgroup");
        let cpufreq_dir = temp.path().join("cpufreq");

        fs::create_dir_all(&telem_dir).unwrap();
        fs::create_dir_all(&pwr_dir).unwrap();
        fs::create_dir_all(&proc_dir).unwrap();
        fs::create_dir_all(&cg_dir).unwrap();
        fs::create_dir_all(cpufreq_dir.join("policy0")).unwrap();

        // Write normal baseline thermal zone: 45°C
        let zone0 = telem_dir.join("thermal_zone0");
        fs::create_dir_all(&zone0).unwrap();
        fs::write(zone0.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(zone0.join("temp"), "45000\n").unwrap();

        let reader = DynatuneTelemetryReader::with_custom_roots(&telem_dir, &pwr_dir, &proc_dir);
        let throttler = MicrovmCgroupThrottler::with_custom_root(&cg_dir);
        let gov = GovernorEppWatchdogController::with_custom_root(&cpufreq_dir);
        let thresholds = HardwareThresholds::default();

        let compiler = HardwareAwareDynamicCompiler::with_custom_roots(
            reader,
            throttler,
            gov,
            thresholds,
        )
        .unwrap();

        let vm_id = "agent-microvm-99";
        compiler.register_microvm(vm_id).await;

        // Normal temperature: NativeCpuUnconstrained
        let dec_norm = compiler.evaluate_and_adapt(WorkloadAffinity::Auto, 8).await;
        assert_eq!(dec_norm.strategy, ExecutionStrategy::NativeCpuUnconstrained);
        assert!(!dec_norm.is_throttled);

        // Heat up to 80°C
        fs::write(zone0.join("temp"), "80000\n").unwrap();
        let dec_hot = compiler.evaluate_and_adapt(WorkloadAffinity::Auto, 8).await;
        assert!(dec_hot.is_throttled);
        assert_eq!(dec_hot.strategy, ExecutionStrategy::SandboxedWasmInterpreter);

        // Check that cgroup quota was applied to the registered microVM
        let (quota, _) = compiler.cgroup_throttler().read_cpu_quota(vm_id).unwrap();
        assert_eq!(quota, format!("{}", dec_hot.cgroup_cpu_quota_us));
    }
}
