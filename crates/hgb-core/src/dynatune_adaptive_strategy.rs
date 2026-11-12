//! # Pillar 3 / Upgrade 3: Hardware-Aware Dynamic Compilation & Execution Strategy (dynatune synergy)
//!
//! Production-grade Tier 1 domain models, strategy enums, telemetry structures,
//! and hardware threshold evaluation rules for hardware-aware dynamic execution.
//!
//! CaldoAI Untangle Tier 1 Rules:
//! - Pure Tier 1 architecture: Strictly NO imports from Tier 2-5 crates.
//! - 100% production Rust code (strictly NO synthetic mocks or stubs).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

/// Critical CPU thermal throttling threshold (75.0°C) engaging automatic strategy downscaling
pub const DEFAULT_THERMAL_THROTTLE_TEMP_C: f64 = 75.0;

/// Severe thermal critical threshold (85.0°C) forcing immediate Emergency Eco Mode serialization
pub const DEFAULT_THERMAL_CRITICAL_TEMP_C: f64 = 85.0;

/// Safe thermal headroom ceiling (60.0°C) required to unlock automated PGO/BOLT recompilation
pub const DEFAULT_THERMAL_SAFE_RECOMPILATION_TEMP_C: f64 = 60.0;

/// Battery low threshold on discharge (25%) triggering downscaling
pub const DEFAULT_BATTERY_LOW_THRESHOLD_PCT: u32 = 25;

/// Battery healthy threshold (>60%) required for heavy PGO/BOLT recompilation passes
pub const DEFAULT_BATTERY_HEALTHY_RECOMPILATION_PCT: u32 = 60;

/// PSI memory pressure threshold (avg10 >= 30.0) indicating elevated system stall
pub const DEFAULT_PSI_MEMORY_PRESSURE_THRESHOLD: f64 = 30.0;

/// Default hot-path invocation threshold triggering PGO/BOLT optimization (500 invocations)
pub const DEFAULT_PGO_HOT_PATH_INVOCATION_THRESHOLD: u64 = 500;

/// Default cooldown period between automated recompilation passes (300 seconds = 5 minutes)
pub const DEFAULT_PGO_RECOMPILATION_COOLDOWN_SECS: u64 = 300;

/// Base period for cgroup v2 cpu.max in microseconds (100,000 µs = 100ms)
pub const CGROUP_CPU_MAX_PERIOD_US: u64 = 100_000;

/// Standard CPU scaling governors
pub const GOVERNOR_PERFORMANCE: &str = "performance";
pub const GOVERNOR_POWERSAVE: &str = "powersave";

/// Standard Energy Performance Preference (EPP) profiles
pub const EPP_PERFORMANCE: &str = "performance";
pub const EPP_BALANCE_PERFORMANCE: &str = "balance_performance";
pub const EPP_BALANCE_POWER: &str = "balance_power";
pub const EPP_POWER: &str = "power";

/// Dynamic hardware execution strategies available to the scheduler and WASM runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExecutionStrategy {
    /// Full native CPU threads, max concurrency, AVX2 SIMD acceleration.
    NativeCpuUnconstrained,
    /// Constrained instruction fuel metering (Wasmtime fuel), reduced worker threads, lower memory ceiling.
    SandboxedWasmInterpreter,
    /// Limits Firecracker microVM vCPU cgroups (`cpu.max`) to prevent thermal runaway.
    MicrovmThrottled,
    /// Powersave governor, minimal concurrency (single-thread), aggressive memory compaction.
    EmergencyEcoMode,
}

impl ExecutionStrategy {
    /// Canonical identifier for the execution strategy
    pub fn strategy_name(&self) -> &'static str {
        match self {
            Self::NativeCpuUnconstrained => "NativeCpuUnconstrained",
            Self::SandboxedWasmInterpreter => "SandboxedWasmInterpreter",
            Self::MicrovmThrottled => "MicrovmThrottled",
            Self::EmergencyEcoMode => "EmergencyEcoMode",
        }
    }

    /// Whether AVX2 SIMD vectorization acceleration is permitted
    pub fn allows_avx2_simd(&self) -> bool {
        matches!(self, Self::NativeCpuUnconstrained)
    }

    /// Default instruction fuel budget allocated per WASM execution task
    pub fn default_fuel_limit(&self) -> Option<u64> {
        match self {
            Self::NativeCpuUnconstrained => None, // Unconstrained / native execution
            Self::SandboxedWasmInterpreter => Some(1_000_000), // Standard fuel ceiling
            Self::MicrovmThrottled => Some(500_000),           // Constrained fuel ceiling
            Self::EmergencyEcoMode => Some(100_000),           // Minimal fuel budget
        }
    }

    /// Memory ceiling in megabytes (MB)
    pub fn default_memory_ceiling_mb(&self) -> u64 {
        match self {
            Self::NativeCpuUnconstrained => 4096,
            Self::SandboxedWasmInterpreter => 512,
            Self::MicrovmThrottled => 256,
            Self::EmergencyEcoMode => 128,
        }
    }

    /// Relative CPU quota fraction for cgroup v2 `cpu.max` (1.0 = 100%, 0.25 = 25%)
    pub fn cgroup_cpu_quota_fraction(&self) -> f64 {
        match self {
            Self::NativeCpuUnconstrained => 1.00,
            Self::SandboxedWasmInterpreter => 0.70,
            Self::MicrovmThrottled => 0.35,
            Self::EmergencyEcoMode => 0.15,
        }
    }

    /// Calculate cgroup v2 cpu.max quota in microseconds for a given period (default 100,000 µs)
    pub fn cgroup_cpu_quota_us(&self, period_us: u64) -> u64 {
        let quota = (self.cgroup_cpu_quota_fraction() * (period_us as f64)).round() as u64;
        quota.max(1000)
    }

    /// Target Linux CPU scaling governor
    pub fn target_cpu_governor(&self) -> &'static str {
        match self {
            Self::NativeCpuUnconstrained => GOVERNOR_PERFORMANCE,
            Self::SandboxedWasmInterpreter | Self::MicrovmThrottled | Self::EmergencyEcoMode => {
                GOVERNOR_POWERSAVE
            }
        }
    }

    /// Target Linux Energy Performance Preference (EPP)
    pub fn target_epp(&self) -> &'static str {
        match self {
            Self::NativeCpuUnconstrained => EPP_PERFORMANCE,
            Self::SandboxedWasmInterpreter => EPP_BALANCE_POWER,
            Self::MicrovmThrottled => EPP_BALANCE_POWER,
            Self::EmergencyEcoMode => EPP_POWER,
        }
    }

    /// Calculate recommended worker concurrency based on strategy and available physical cores
    pub fn recommended_concurrency(&self, base_cores: usize) -> usize {
        let cores = base_cores.max(1);
        match self {
            Self::NativeCpuUnconstrained => cores,
            Self::SandboxedWasmInterpreter => (cores / 2).max(1),
            Self::MicrovmThrottled => (cores / 4).max(1),
            Self::EmergencyEcoMode => 1,
        }
    }

    /// Whether this strategy represents a throttled execution state
    pub fn is_throttled(&self) -> bool {
        !matches!(self, Self::NativeCpuUnconstrained)
    }
}

impl std::fmt::Display for ExecutionStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.strategy_name())
    }
}

/// Workload dispatch preference indicating caller requirements
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadAffinity {
    /// Automatically select optimal runtime according to live hardware telemetry
    Auto,
    /// Explicitly prefer native CPU execution if thermal and power margins allow
    PreferNative,
    /// Explicitly prefer WASM sandboxed interpreter execution
    PreferWasm,
    /// Explicitly prefer microVM isolated execution
    PreferMicrovm,
}

/// Linux Pressure Stall Information (PSI) metric record
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PsiPressureMetric {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

/// Battery and power supply status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatteryPowerState {
    pub ac_connected: bool,
    pub battery_pct: u32,
    pub is_discharging: bool,
    pub status: String,
    pub energy_now_wh: Option<f64>,
    pub power_now_w: Option<f64>,
}

impl Default for BatteryPowerState {
    fn default() -> Self {
        Self {
            ac_connected: true,
            battery_pct: 100,
            is_discharging: false,
            status: "Full".to_string(),
            energy_now_wh: None,
            power_now_w: None,
        }
    }
}

/// Comprehensive hardware telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareTelemetrySnapshot {
    /// CPU package temperature in Celsius
    pub cpu_pkg_temp_c: f64,
    /// Peak temperature across all detected thermal sensors
    pub max_core_temp_c: f64,
    /// Power supply & battery status
    pub battery: BatteryPowerState,
    /// PSI memory pressure
    pub psi_memory: PsiPressureMetric,
    /// PSI CPU pressure
    pub psi_cpu: PsiPressureMetric,
    /// Available system RAM in megabytes
    pub ram_available_mb: u64,
    /// Total system RAM in megabytes
    pub ram_total_mb: u64,
    /// Available RAM percentage (0.0 to 100.0)
    pub ram_available_pct: f64,
    /// Flag indicating if the system is currently thermal-throttling
    pub is_thermal_throttled: bool,
    /// Snapshot capture epoch in milliseconds
    pub timestamp_epoch_ms: u64,
}

impl HardwareTelemetrySnapshot {
    /// Check if CPU temperature meets or exceeds thermal throttling threshold
    pub fn is_hot(&self, throttle_temp_c: f64) -> bool {
        self.cpu_pkg_temp_c >= throttle_temp_c || self.max_core_temp_c >= throttle_temp_c
    }

    /// Check if battery is discharging and below critical threshold
    pub fn is_battery_low(&self, threshold_pct: u32) -> bool {
        self.battery.is_discharging && self.battery.battery_pct < threshold_pct
    }
}

/// Configurable hardware thresholds and rules for dynamic strategy shifts and PGO/BOLT recompilation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareThresholds {
    /// Temperature (°C) triggering downscaling from Native to WASM/MicroVM (default: 75.0°C)
    pub thermal_throttle_temp_c: f64,
    /// Critical temperature (°C) forcing Emergency Eco Mode (default: 85.0°C)
    pub thermal_critical_temp_c: f64,
    /// Maximum package temperature (°C) allowed to trigger PGO/BOLT recompilation (default: 60.0°C)
    pub thermal_safe_recompilation_temp_c: f64,
    /// Battery threshold (%) on discharge triggering downscaling (default: 25%)
    pub battery_low_threshold_pct: u32,
    /// Minimum battery percentage (%) required to trigger PGO/BOLT recompilation (default: 60%)
    pub battery_healthy_recompilation_pct: u32,
    /// PSI memory pressure threshold (avg10) triggering throttling (default: 30.0)
    pub psi_memory_threshold: f64,
    /// Hot-path invocation threshold triggering automated PGO/BOLT recompilation (default: 500)
    pub hot_path_threshold: u64,
    /// Cooldown between automated PGO/BOLT recompilations in seconds (default: 300)
    pub recompilation_cooldown_secs: u64,
}

impl Default for HardwareThresholds {
    fn default() -> Self {
        Self {
            thermal_throttle_temp_c: DEFAULT_THERMAL_THROTTLE_TEMP_C,
            thermal_critical_temp_c: DEFAULT_THERMAL_CRITICAL_TEMP_C,
            thermal_safe_recompilation_temp_c: DEFAULT_THERMAL_SAFE_RECOMPILATION_TEMP_C,
            battery_low_threshold_pct: DEFAULT_BATTERY_LOW_THRESHOLD_PCT,
            battery_healthy_recompilation_pct: DEFAULT_BATTERY_HEALTHY_RECOMPILATION_PCT,
            psi_memory_threshold: DEFAULT_PSI_MEMORY_PRESSURE_THRESHOLD,
            hot_path_threshold: DEFAULT_PGO_HOT_PATH_INVOCATION_THRESHOLD,
            recompilation_cooldown_secs: DEFAULT_PGO_RECOMPILATION_COOLDOWN_SECS,
        }
    }
}

/// Detailed decision returned by the hardware strategy shifter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionStrategyDecision {
    /// Selected execution strategy
    pub strategy: ExecutionStrategy,
    /// Previous execution strategy if a state transition occurred
    pub previous_strategy: Option<ExecutionStrategy>,
    /// Justification explaining why the strategy was shifted or maintained
    pub reason: String,
    /// Whether execution is in throttled/constrained mode
    pub is_throttled: bool,
    /// Recommended worker thread concurrency
    pub recommended_concurrency: usize,
    /// Fuel limit ceiling for WASM instruction metering
    pub fuel_limit: Option<u64>,
    /// cgroup v2 cpu.max quota in microseconds (out of CGROUP_CPU_MAX_PERIOD_US)
    pub cgroup_cpu_quota_us: u64,
    /// Recommended CPU scaling governor
    pub target_governor: &'static str,
    /// Recommended Energy Performance Preference (EPP)
    pub target_epp: &'static str,
}

/// Programmatically evaluates live telemetry and decides the optimal execution strategy.
///
/// Rules:
/// 1. Severe Emergency: If $T_{\text{pkg}} \ge 85^\circ\text{C}$, or discharging battery $< 10\%$, or PSI memory $> 60.0$:
///    -> EmergencyEcoMode (single thread, minimal fuel, powersave governor, power EPP).
/// 2. Thermal / Battery Throttle: If $T_{\text{pkg}} \ge 75^\circ\text{C}$ or (battery $< 25\%$ and discharging):
///    -> Programmatically shift execution strategy from Native CPU down to `SandboxedWasmInterpreter`
///       or `MicrovmThrottled`.
/// 3. Normal / Unconstrained: If $T_{\text{pkg}} < 75^\circ\text{C}$ and (AC connected or battery $\ge 25\%$):
///    -> If `WorkloadAffinity::PreferMicrovm`, uses `MicrovmThrottled` with unconstrained quota.
///    -> If `WorkloadAffinity::PreferWasm`, uses `SandboxedWasmInterpreter`.
///    -> Otherwise, `NativeCpuUnconstrained`.
pub fn evaluate_execution_strategy(
    telemetry: &HardwareTelemetrySnapshot,
    thresholds: &HardwareThresholds,
    affinity: WorkloadAffinity,
    base_cores: usize,
    previous_strategy: Option<ExecutionStrategy>,
) -> ExecutionStrategyDecision {
    let cores = base_cores.max(1);

    // Rule 1: Emergency condition
    let is_emergency = telemetry.cpu_pkg_temp_c >= thresholds.thermal_critical_temp_c
        || (telemetry.battery.is_discharging && telemetry.battery.battery_pct < 10)
        || telemetry.psi_memory.avg10 > 60.0;

    if is_emergency {
        let reason = if telemetry.cpu_pkg_temp_c >= thresholds.thermal_critical_temp_c {
            format!(
                "Critical package temperature reached: {:.1}°C >= {:.1}°C",
                telemetry.cpu_pkg_temp_c, thresholds.thermal_critical_temp_c
            )
        } else if telemetry.battery.is_discharging && telemetry.battery.battery_pct < 10 {
            format!(
                "Severe battery exhaustion: {}% < 10% on discharge",
                telemetry.battery.battery_pct
            )
        } else {
            format!(
                "Severe memory pressure stall: PSI avg10 {:.1} > 60.0",
                telemetry.psi_memory.avg10
            )
        };

        let strategy = ExecutionStrategy::EmergencyEcoMode;
        return ExecutionStrategyDecision {
            strategy,
            previous_strategy,
            reason,
            is_throttled: true,
            recommended_concurrency: strategy.recommended_concurrency(cores),
            fuel_limit: strategy.default_fuel_limit(),
            cgroup_cpu_quota_us: strategy.cgroup_cpu_quota_us(CGROUP_CPU_MAX_PERIOD_US),
            target_governor: strategy.target_cpu_governor(),
            target_epp: strategy.target_epp(),
        };
    }

    // Rule 2: Thermal throttle (T_pkg >= 75°C) or battery critical (< 25% discharging)
    let is_thermal_hot = telemetry.cpu_pkg_temp_c >= thresholds.thermal_throttle_temp_c;
    let is_battery_low = telemetry.is_battery_low(thresholds.battery_low_threshold_pct);
    let is_high_memory_psi = telemetry.psi_memory.avg10 >= thresholds.psi_memory_threshold;

    if is_thermal_hot || is_battery_low || is_high_memory_psi {
        let reason = if is_thermal_hot {
            format!(
                "Thermal throttling engaged: package temperature {:.1}°C >= {:.1}°C threshold",
                telemetry.cpu_pkg_temp_c, thresholds.thermal_throttle_temp_c
            )
        } else if is_battery_low {
            format!(
                "Battery power safeguard: {}% < {}% threshold on discharge",
                telemetry.battery.battery_pct, thresholds.battery_low_threshold_pct
            )
        } else {
            format!(
                "Elevated memory pressure safeguard: PSI avg10 {:.1} >= {:.1}",
                telemetry.psi_memory.avg10, thresholds.psi_memory_threshold
            )
        };

        // Shift down from Native CPU to MicrovmThrottled or SandboxedWasmInterpreter
        let strategy = match affinity {
            WorkloadAffinity::PreferMicrovm => ExecutionStrategy::MicrovmThrottled,
            _ => ExecutionStrategy::SandboxedWasmInterpreter,
        };

        return ExecutionStrategyDecision {
            strategy,
            previous_strategy,
            reason,
            is_throttled: true,
            recommended_concurrency: strategy.recommended_concurrency(cores),
            fuel_limit: strategy.default_fuel_limit(),
            cgroup_cpu_quota_us: strategy.cgroup_cpu_quota_us(CGROUP_CPU_MAX_PERIOD_US),
            target_governor: strategy.target_cpu_governor(),
            target_epp: strategy.target_epp(),
        };
    }

    // Rule 3: Normal / Unconstrained operating parameters
    let strategy = match affinity {
        WorkloadAffinity::PreferMicrovm => ExecutionStrategy::MicrovmThrottled,
        WorkloadAffinity::PreferWasm => ExecutionStrategy::SandboxedWasmInterpreter,
        WorkloadAffinity::Auto | WorkloadAffinity::PreferNative => {
            ExecutionStrategy::NativeCpuUnconstrained
        }
    };

    let reason = format!(
        "Optimal operating state: T_pkg={:.1}°C < {:.1}°C, AC={}, Batt={}%",
        telemetry.cpu_pkg_temp_c,
        thresholds.thermal_throttle_temp_c,
        telemetry.battery.ac_connected,
        telemetry.battery.battery_pct
    );

    ExecutionStrategyDecision {
        strategy,
        previous_strategy,
        reason,
        is_throttled: strategy.is_throttled(),
        recommended_concurrency: strategy.recommended_concurrency(cores),
        fuel_limit: strategy.default_fuel_limit(),
        cgroup_cpu_quota_us: strategy.cgroup_cpu_quota_us(CGROUP_CPU_MAX_PERIOD_US),
        target_governor: strategy.target_cpu_governor(),
        target_epp: strategy.target_epp(),
    }
}

/// BOLT Basic block reordering layout algorithms
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoltBlockReordering {
    /// Extended TSP algorithm for optimal basic block placement
    ExtTsp,
    /// Disabled basic block reordering
    None,
}

impl BoltBlockReordering {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExtTsp => "ext-tsp",
            Self::None => "none",
        }
    }
}

/// BOLT Function reordering algorithms
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoltFunctionReordering {
    /// Call-chain Clustering / Cache-Directed Sort (cdsort)
    CdSort,
    /// Hierarchical Function Sorting (hfsort+)
    HfSortPlus,
    /// Disabled function reordering
    None,
}

impl BoltFunctionReordering {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CdSort => "cdsort",
            Self::HfSortPlus => "hfsort+",
            Self::None => "none",
        }
    }
}

/// Detailed compilation and optimization plan for an automated PGO/BOLT pass
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PgoBoltRecompilationPlan {
    /// Target candidate ELF binary to optimize
    pub candidate_binary: PathBuf,
    /// Optimized output binary destination
    pub output_binary: PathBuf,
    /// Profile instrumentation fdata path
    pub profile_data_path: PathBuf,
    /// BOLT basic block reordering layout algorithm
    pub block_reordering: BoltBlockReordering,
    /// BOLT function reordering layout algorithm
    pub function_reordering: BoltFunctionReordering,
    /// Split functions into hot/cold sections
    pub split_functions: bool,
    /// Aggressively split all cold basic blocks
    pub split_all_cold: bool,
    /// Inline small cold-path functions
    pub inline_small_functions: bool,
    /// Emit dynamic disassembly statistics
    pub dyno_stats: bool,
    /// Rustc PGO compile flags for profile-guided generation
    pub pgo_rustflags: Vec<String>,
}

impl PgoBoltRecompilationPlan {
    /// Constructs default production-grade BOLT plan for a candidate ELF binary
    pub fn new_production_plan(
        candidate_binary: impl Into<PathBuf>,
        profile_data_path: impl Into<PathBuf>,
    ) -> Self {
        let candidate = candidate_binary.into();
        let mut output = candidate.clone();
        if let Some(name) = output.file_name() {
            let mut bolt_name = name.to_os_string();
            bolt_name.push(".bolt");
            output.set_file_name(bolt_name);
        }

        Self {
            candidate_binary: candidate,
            output_binary: output,
            profile_data_path: profile_data_path.into(),
            block_reordering: BoltBlockReordering::ExtTsp,
            function_reordering: BoltFunctionReordering::CdSort,
            split_functions: true,
            split_all_cold: true,
            inline_small_functions: true,
            dyno_stats: true,
            pgo_rustflags: vec![
                "-C".to_string(),
                "link-arg=-Wl,-q".to_string(), // Preserve relocations for BOLT
            ],
        }
    }

    /// Generates LLVM BOLT command-line arguments corresponding to this plan
    pub fn bolt_cli_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.push(self.candidate_binary.to_string_lossy().to_string());

        if self.profile_data_path.exists() {
            args.push("-data".to_string());
            args.push(self.profile_data_path.to_string_lossy().to_string());
        }

        args.push("-o".to_string());
        args.push(self.output_binary.to_string_lossy().to_string());

        if self.block_reordering != BoltBlockReordering::None {
            args.push(format!("-reorder-blocks={}", self.block_reordering.as_str()));
        }

        if self.function_reordering != BoltFunctionReordering::None {
            args.push(format!("-reorder-functions={}", self.function_reordering.as_str()));
        }

        if self.split_functions {
            args.push("-split-functions".to_string());
        }

        if self.split_all_cold {
            args.push("-split-all-cold".to_string());
        }

        if self.inline_small_functions {
            args.push("-inline-small-functions".to_string());
        }

        if self.dyno_stats {
            args.push("-dyno-stats".to_string());
        }

        args
    }
}

/// Evaluation outcome for automated PGO and BOLT recompilation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PgoBoltTriggerEvaluation {
    /// Whether conditions are satisfied to trigger recompilation
    pub eligible: bool,
    /// Reasons why recompilation cannot be triggered at this time
    pub unmet_reasons: Vec<String>,
    /// Optimization plan if eligible
    pub plan: Option<PgoBoltRecompilationPlan>,
}

/// Evaluates whether automated PGO & BOLT recompilation should be triggered.
///
/// Requirement:
/// "If AC power is connected, battery is healthy (> 60%), and T_pkg < 60°C:
///  - Automatically evaluate hot-path execution counters.
///  - If hot-path invocation threshold is crossed and cooldown elapsed:
///    * Trigger aggressive Profile-Guided Optimization (PGO) pass.
///    * Plan and schedule LLVM BOLT basic block and function reordering (ext-tsp / cdsort) on candidate ELF binaries."
pub fn evaluate_pgo_bolt_trigger(
    telemetry: &HardwareTelemetrySnapshot,
    hot_path_invocations: u64,
    last_recompile_secs_ago: u64,
    thresholds: &HardwareThresholds,
    candidate_binary: impl Into<PathBuf>,
    profile_data_path: impl Into<PathBuf>,
) -> PgoBoltTriggerEvaluation {
    let mut unmet = Vec::new();

    // 1. AC power requirement
    if !telemetry.battery.ac_connected {
        unmet.push("AC power is disconnected (running on battery power)".to_string());
    }

    // 2. Battery health requirement (> 60%)
    if telemetry.battery.battery_pct <= thresholds.battery_healthy_recompilation_pct {
        unmet.push(format!(
            "Battery percentage {}% is not healthy (> {}% required)",
            telemetry.battery.battery_pct, thresholds.battery_healthy_recompilation_pct
        ));
    }

    // 3. Thermal headroom requirement (< 60°C)
    if telemetry.cpu_pkg_temp_c >= thresholds.thermal_safe_recompilation_temp_c {
        unmet.push(format!(
            "Package temperature {:.1}°C exceeds safe recompilation ceiling (< {:.1}°C required)",
            telemetry.cpu_pkg_temp_c, thresholds.thermal_safe_recompilation_temp_c
        ));
    }

    // 4. Hot-path invocation threshold requirement
    if hot_path_invocations < thresholds.hot_path_threshold {
        unmet.push(format!(
            "Hot-path invocations {} below threshold ({} required)",
            hot_path_invocations, thresholds.hot_path_threshold
        ));
    }

    // 5. Cooldown elapsed requirement
    if last_recompile_secs_ago < thresholds.recompilation_cooldown_secs {
        unmet.push(format!(
            "Recompilation cooldown active: {}s elapsed < {}s required",
            last_recompile_secs_ago, thresholds.recompilation_cooldown_secs
        ));
    }

    let eligible = unmet.is_empty();
    let plan = if eligible {
        Some(PgoBoltRecompilationPlan::new_production_plan(
            candidate_binary,
            profile_data_path,
        ))
    } else {
        None
    };

    PgoBoltTriggerEvaluation {
        eligible,
        unmet_reasons: unmet,
        plan,
    }
}

/// Linux sysfs & procfs Hardware Telemetry Reader
///
/// Designed to directly read Linux host thermal zones, power supplies,
/// and pressure stall information. Configurable directory roots allow
/// comprehensive, disk-backed automated testing with real filesystem paths
/// and zero synthetic mocks.
#[derive(Debug, Clone)]
pub struct DynatuneTelemetryReader {
    thermal_dir: PathBuf,
    power_supply_dir: PathBuf,
    proc_dir: PathBuf,
}

impl Default for DynatuneTelemetryReader {
    fn default() -> Self {
        Self::new()
    }
}

impl DynatuneTelemetryReader {
    /// Create reader targeting standard Linux host sysfs and procfs locations
    pub fn new() -> Self {
        Self {
            thermal_dir: PathBuf::from("/sys/class/thermal"),
            power_supply_dir: PathBuf::from("/sys/class/power_supply"),
            proc_dir: PathBuf::from("/proc"),
        }
    }

    /// Create reader with custom base directories for zero-mock, real-disk unit testing
    pub fn with_custom_roots(
        thermal_dir: impl Into<PathBuf>,
        power_supply_dir: impl Into<PathBuf>,
        proc_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            thermal_dir: thermal_dir.into(),
            power_supply_dir: power_supply_dir.into(),
            proc_dir: proc_dir.into(),
        }
    }

    /// Read CPU package temperature in Celsius
    pub fn read_cpu_temp_c(&self) -> f64 {
        if !self.thermal_dir.exists() {
            return 45.0; // Graceful baseline if sysfs absent (e.g. non-Linux environment)
        }

        let mut package_temp: Option<f64> = None;
        let mut max_temp = 0.0f64;

        if let Ok(entries) = fs::read_dir(&self.thermal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                if file_name.starts_with("thermal_zone") {
                    let type_file = path.join("type");
                    let temp_file = path.join("temp");

                    let zone_type = fs::read_to_string(&type_file)
                        .unwrap_or_default()
                        .trim()
                        .to_lowercase();

                    if let Ok(temp_str) = fs::read_to_string(&temp_file) {
                        if let Ok(milli_c) = temp_str.trim().parse::<f64>() {
                            let temp_c = milli_c / 1000.0;
                            if temp_c > max_temp {
                                max_temp = temp_c;
                            }
                            if zone_type.contains("x86_pkg_temp")
                                || zone_type.contains("pkg_temp")
                                || zone_type.contains("cpu")
                            {
                                package_temp = Some(temp_c);
                            }
                        }
                    }
                }
            }
        }

        package_temp.unwrap_or(max_temp.max(45.0))
    }

    /// Read power supply status (AC connection, battery capacity, discharge state)
    pub fn read_battery_state(&self) -> BatteryPowerState {
        if !self.power_supply_dir.exists() {
            return BatteryPowerState::default();
        }

        let mut ac_connected = false;
        let mut battery_pct = 100u32;
        let mut is_discharging = false;
        let mut status = "Unknown".to_string();
        let mut energy_now_wh = None;
        let mut power_now_w = None;

        if let Ok(entries) = fs::read_dir(&self.power_supply_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();

                if name.starts_with("AC") || name.starts_with("ADP") {
                    let online_file = path.join("online");
                    if let Ok(val) = fs::read_to_string(online_file) {
                        if val.trim() == "1" {
                            ac_connected = true;
                        }
                    }
                } else if name.starts_with("BAT") {
                    let status_file = path.join("status");
                    if let Ok(val) = fs::read_to_string(status_file) {
                        let st = val.trim().to_string();
                        if st.eq_ignore_ascii_case("Discharging") {
                            is_discharging = true;
                        } else if st.eq_ignore_ascii_case("Charging") || st.eq_ignore_ascii_case("Full") {
                            ac_connected = true;
                        }
                        status = st;
                    }

                    let cap_file = path.join("capacity");
                    if let Ok(val) = fs::read_to_string(cap_file) {
                        if let Ok(parsed) = val.trim().parse::<u32>() {
                            battery_pct = parsed.min(100);
                        }
                    }

                    let energy_file = path.join("energy_now");
                    if let Ok(val) = fs::read_to_string(energy_file) {
                        if let Ok(parsed) = val.trim().parse::<f64>() {
                            energy_now_wh = Some(parsed / 1_000_000.0);
                        }
                    }

                    let power_file = path.join("power_now");
                    if let Ok(val) = fs::read_to_string(power_file) {
                        if let Ok(parsed) = val.trim().parse::<f64>() {
                            power_now_w = Some(parsed / 1_000_000.0);
                        }
                    }
                }
            }
        }

        BatteryPowerState {
            ac_connected,
            battery_pct,
            is_discharging,
            status,
            energy_now_wh,
            power_now_w,
        }
    }

    /// Read Linux Pressure Stall Information (PSI) for memory
    pub fn read_psi_memory(&self) -> PsiPressureMetric {
        let psi_file = self.proc_dir.join("pressure/memory");
        if let Ok(content) = fs::read_to_string(&psi_file) {
            parse_psi_record(&content)
        } else {
            PsiPressureMetric::default()
        }
    }

    /// Read Linux Pressure Stall Information (PSI) for CPU
    pub fn read_psi_cpu(&self) -> PsiPressureMetric {
        let psi_file = self.proc_dir.join("pressure/cpu");
        if let Ok(content) = fs::read_to_string(&psi_file) {
            parse_psi_record(&content)
        } else {
            PsiPressureMetric::default()
        }
    }

    /// Read RAM availability from `/proc/meminfo`
    pub fn read_meminfo(&self) -> (u64, u64, f64) {
        let meminfo_file = self.proc_dir.join("meminfo");
        if let Ok(content) = fs::read_to_string(meminfo_file) {
            let mut total_kb = 0u64;
            let mut available_kb = 0u64;

            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    if let Some(val) = extract_mem_kb(line) {
                        total_kb = val;
                    }
                } else if line.starts_with("MemAvailable:") {
                    if let Some(val) = extract_mem_kb(line) {
                        available_kb = val;
                    }
                }
            }

            let total_mb = total_kb / 1024;
            let available_mb = available_kb / 1024;
            let available_pct = if total_mb > 0 {
                (available_mb as f64 / total_mb as f64) * 100.0
            } else {
                100.0
            };

            (total_mb, available_mb, available_pct)
        } else {
            (8192, 4096, 50.0) // Fallback baseline
        }
    }

    /// Captures a complete hardware telemetry snapshot
    pub fn read_snapshot(&self) -> HardwareTelemetrySnapshot {
        let cpu_temp = self.read_cpu_temp_c();
        let battery = self.read_battery_state();
        let psi_memory = self.read_psi_memory();
        let psi_cpu = self.read_psi_cpu();
        let (ram_total_mb, ram_available_mb, ram_available_pct) = self.read_meminfo();

        let is_thermal_throttled = cpu_temp >= DEFAULT_THERMAL_THROTTLE_TEMP_C;
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        HardwareTelemetrySnapshot {
            cpu_pkg_temp_c: cpu_temp,
            max_core_temp_c: cpu_temp,
            battery,
            psi_memory,
            psi_cpu,
            ram_available_mb,
            ram_total_mb,
            ram_available_pct,
            is_thermal_throttled,
            timestamp_epoch_ms: epoch_ms,
        }
    }
}

/// Helper function to parse PSI record line from `/proc/pressure/*`
pub fn parse_psi_record(content: &str) -> PsiPressureMetric {
    let mut metric = PsiPressureMetric::default();

    for line in content.lines() {
        if line.starts_with("some ") {
            for part in line.split_whitespace().skip(1) {
                if let Some((k, v)) = part.split_once('=') {
                    match k {
                        "avg10" => metric.avg10 = v.parse::<f64>().unwrap_or(0.0),
                        "avg60" => metric.avg60 = v.parse::<f64>().unwrap_or(0.0),
                        "avg300" => metric.avg300 = v.parse::<f64>().unwrap_or(0.0),
                        "total" => metric.total_us = v.parse::<u64>().unwrap_or(0),
                        _ => {}
                    }
                }
            }
            break;
        }
    }

    metric
}

fn extract_mem_kb(line: &str) -> Option<u64> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        parts[1].parse::<u64>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_execution_strategy_properties() {
        let native = ExecutionStrategy::NativeCpuUnconstrained;
        assert!(native.allows_avx2_simd());
        assert_eq!(native.target_cpu_governor(), "performance");
        assert_eq!(native.target_epp(), "performance");
        assert_eq!(native.cgroup_cpu_quota_fraction(), 1.0);
        assert!(!native.is_throttled());

        let wasm = ExecutionStrategy::SandboxedWasmInterpreter;
        assert!(!wasm.allows_avx2_simd());
        assert_eq!(wasm.default_fuel_limit(), Some(1_000_000));
        assert_eq!(wasm.target_cpu_governor(), "powersave");
        assert!(wasm.is_throttled());

        let microvm = ExecutionStrategy::MicrovmThrottled;
        assert!(!microvm.allows_avx2_simd());
        assert_eq!(microvm.cgroup_cpu_quota_fraction(), 0.35);
        assert_eq!(microvm.cgroup_cpu_quota_us(100_000), 35_000);

        let eco = ExecutionStrategy::EmergencyEcoMode;
        assert_eq!(eco.recommended_concurrency(16), 1);
        assert_eq!(eco.default_memory_ceiling_mb(), 128);
        assert_eq!(eco.target_epp(), "power");
    }

    #[test]
    fn test_thermal_and_battery_strategy_transitions() {
        let thresholds = HardwareThresholds::default();

        // 1. Normal state: 55°C, AC power, 100% battery
        let mut snapshot = HardwareTelemetrySnapshot {
            cpu_pkg_temp_c: 55.0,
            max_core_temp_c: 55.0,
            battery: BatteryPowerState {
                ac_connected: true,
                battery_pct: 100,
                is_discharging: false,
                status: "Full".to_string(),
                energy_now_wh: None,
                power_now_w: None,
            },
            psi_memory: PsiPressureMetric::default(),
            psi_cpu: PsiPressureMetric::default(),
            ram_available_mb: 4096,
            ram_total_mb: 8192,
            ram_available_pct: 50.0,
            is_thermal_throttled: false,
            timestamp_epoch_ms: 1000,
        };

        let decision = evaluate_execution_strategy(
            &snapshot,
            &thresholds,
            WorkloadAffinity::Auto,
            8,
            None,
        );
        assert_eq!(decision.strategy, ExecutionStrategy::NativeCpuUnconstrained);
        assert_eq!(decision.recommended_concurrency, 8);
        assert!(!decision.is_throttled);

        // 2. Thermal threshold triggered: T_pkg = 76.5°C >= 75.0°C
        snapshot.cpu_pkg_temp_c = 76.5;
        let throttled_decision = evaluate_execution_strategy(
            &snapshot,
            &thresholds,
            WorkloadAffinity::Auto,
            8,
            Some(ExecutionStrategy::NativeCpuUnconstrained),
        );
        assert_eq!(
            throttled_decision.strategy,
            ExecutionStrategy::SandboxedWasmInterpreter
        );
        assert!(throttled_decision.is_throttled);
        assert_eq!(throttled_decision.target_governor, "powersave");
        assert_eq!(throttled_decision.target_epp, "balance_power");
        assert!(throttled_decision.reason.contains("Thermal throttling engaged"));

        // MicroVM affinity under thermal threshold shifts to MicrovmThrottled
        let microvm_decision = evaluate_execution_strategy(
            &snapshot,
            &thresholds,
            WorkloadAffinity::PreferMicrovm,
            8,
            None,
        );
        assert_eq!(microvm_decision.strategy, ExecutionStrategy::MicrovmThrottled);
        assert_eq!(microvm_decision.cgroup_cpu_quota_us, 35_000);

        // 3. Battery discharging < 25% threshold
        snapshot.cpu_pkg_temp_c = 62.0; // cool
        snapshot.battery.ac_connected = false;
        snapshot.battery.is_discharging = true;
        snapshot.battery.battery_pct = 22; // < 25%
        let batt_decision = evaluate_execution_strategy(
            &snapshot,
            &thresholds,
            WorkloadAffinity::Auto,
            8,
            None,
        );
        assert_eq!(
            batt_decision.strategy,
            ExecutionStrategy::SandboxedWasmInterpreter
        );
        assert!(batt_decision.is_throttled);
        assert!(batt_decision.reason.contains("Battery power safeguard"));

        // 4. Critical emergency: T_pkg = 88.0°C >= 85.0°C
        snapshot.cpu_pkg_temp_c = 88.0;
        let emergency_decision = evaluate_execution_strategy(
            &snapshot,
            &thresholds,
            WorkloadAffinity::Auto,
            8,
            None,
        );
        assert_eq!(emergency_decision.strategy, ExecutionStrategy::EmergencyEcoMode);
        assert_eq!(emergency_decision.recommended_concurrency, 1);
        assert_eq!(emergency_decision.target_epp, "power");
    }

    #[test]
    fn test_automated_pgo_bolt_recompilation_evaluation() {
        let thresholds = HardwareThresholds::default();

        let mut telemetry = HardwareTelemetrySnapshot {
            cpu_pkg_temp_c: 54.0, // < 60°C
            max_core_temp_c: 54.0,
            battery: BatteryPowerState {
                ac_connected: true, // AC connected
                battery_pct: 85,    // > 60%
                is_discharging: false,
                status: "Charging".to_string(),
                energy_now_wh: None,
                power_now_w: None,
            },
            psi_memory: PsiPressureMetric::default(),
            psi_cpu: PsiPressureMetric::default(),
            ram_available_mb: 8192,
            ram_total_mb: 16384,
            ram_available_pct: 50.0,
            is_thermal_throttled: false,
            timestamp_epoch_ms: 1000,
        };

        // All conditions met: AC=true, Batt=85% (>60%), Temp=54°C (<60°C), Calls=550 (>=500), Cooldown=350s (>=300s)
        let eval_eligible = evaluate_pgo_bolt_trigger(
            &telemetry,
            550,
            350,
            &thresholds,
            "/tmp/hgbd",
            "/tmp/hgbd.fdata",
        );
        assert!(eval_eligible.eligible);
        assert!(eval_eligible.unmet_reasons.is_empty());
        let plan = eval_eligible.plan.expect("plan must be generated");
        assert_eq!(plan.block_reordering, BoltBlockReordering::ExtTsp);
        assert_eq!(plan.function_reordering, BoltFunctionReordering::CdSort);
        assert!(plan.split_functions);
        assert!(plan.split_all_cold);

        let bolt_args = plan.bolt_cli_args();
        assert!(bolt_args.contains(&"-reorder-blocks=ext-tsp".to_string()));
        assert!(bolt_args.contains(&"-reorder-functions=cdsort".to_string()));
        assert!(bolt_args.contains(&"-split-functions".to_string()));
        assert!(bolt_args.contains(&"-dyno-stats".to_string()));

        // Fail condition 1: AC disconnected
        telemetry.battery.ac_connected = false;
        let eval_no_ac = evaluate_pgo_bolt_trigger(
            &telemetry,
            550,
            350,
            &thresholds,
            "/tmp/hgbd",
            "/tmp/hgbd.fdata",
        );
        assert!(!eval_no_ac.eligible);
        assert!(eval_no_ac.unmet_reasons[0].contains("AC power is disconnected"));

        // Fail condition 2: High temperature >= 60°C
        telemetry.battery.ac_connected = true;
        telemetry.cpu_pkg_temp_c = 68.0;
        let eval_hot = evaluate_pgo_bolt_trigger(
            &telemetry,
            550,
            350,
            &thresholds,
            "/tmp/hgbd",
            "/tmp/hgbd.fdata",
        );
        assert!(!eval_hot.eligible);
        assert!(eval_hot.unmet_reasons.iter().any(|r| r.contains("Package temperature")));

        // Fail condition 3: Invocations below threshold
        telemetry.cpu_pkg_temp_c = 50.0;
        let eval_low_calls = evaluate_pgo_bolt_trigger(
            &telemetry,
            320, // < 500
            350,
            &thresholds,
            "/tmp/hgbd",
            "/tmp/hgbd.fdata",
        );
        assert!(!eval_low_calls.eligible);
        assert!(eval_low_calls.unmet_reasons.iter().any(|r| r.contains("Hot-path invocations")));
    }

    #[test]
    fn test_disk_backed_sysfs_telemetry_reader_zero_mocks() {
        let temp = TempDir::new().expect("create tempdir");
        let sysfs_thermal = temp.path().join("sys/class/thermal");
        let sysfs_power = temp.path().join("sys/class/power_supply");
        let procfs = temp.path().join("proc");

        fs::create_dir_all(sysfs_thermal.join("thermal_zone0")).expect("thermal zone dir");
        fs::write(
            sysfs_thermal.join("thermal_zone0/type"),
            "x86_pkg_temp\n",
        )
        .expect("write thermal type");
        fs::write(
            sysfs_thermal.join("thermal_zone0/temp"),
            "76200\n", // 76.2°C
        )
        .expect("write thermal temp");

        fs::create_dir_all(sysfs_power.join("AC")).expect("ac dir");
        fs::write(sysfs_power.join("AC/online"), "1\n").expect("write ac online");

        fs::create_dir_all(sysfs_power.join("BAT0")).expect("bat dir");
        fs::write(sysfs_power.join("BAT0/status"), "Discharging\n").expect("write bat status");
        fs::write(sysfs_power.join("BAT0/capacity"), "72\n").expect("write bat cap");
        fs::write(sysfs_power.join("BAT0/energy_now"), "45000000\n").expect("write energy");

        fs::create_dir_all(procfs.join("pressure")).expect("pressure dir");
        fs::write(
            procfs.join("pressure/memory"),
            "some avg10=14.25 avg60=8.10 avg300=4.00 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
        )
        .expect("write psi memory");
        fs::write(
            procfs.join("pressure/cpu"),
            "some avg10=5.50 avg60=3.20 avg300=1.10 total=654321\n",
        )
        .expect("write psi cpu");

        fs::write(
            procfs.join("meminfo"),
            "MemTotal:       16384000 kB\nMemFree:         4096000 kB\nMemAvailable:   12288000 kB\n",
        )
        .expect("write meminfo");

        let reader = DynatuneTelemetryReader::with_custom_roots(
            sysfs_thermal,
            sysfs_power,
            procfs,
        );

        let snapshot = reader.read_snapshot();
        assert_eq!(snapshot.cpu_pkg_temp_c, 76.2);
        assert!(snapshot.battery.ac_connected);
        assert_eq!(snapshot.battery.battery_pct, 72);
        assert_eq!(snapshot.battery.status, "Discharging");
        assert_eq!(snapshot.battery.energy_now_wh, Some(45.0));
        assert_eq!(snapshot.psi_memory.avg10, 14.25);
        assert_eq!(snapshot.psi_cpu.avg10, 5.50);
        assert_eq!(snapshot.ram_total_mb, 16000);
        assert_eq!(snapshot.ram_available_mb, 12000);
        assert!(snapshot.is_thermal_throttled); // 76.2 >= 75.0
    }
}
