//! # Pillar 4: Unified Neural-Hardware Feedback Loop (`unified_neural_hardware_loop`)
//!
//! Systems-grade bidirectional hardware-software co-design loop interfacing `hgb-daemon` directly with:
//! - Dynatune background daemon (`/run/dynatune.sock`) via bidirectional JSON-RPC IPC.
//! - Kernel-Optimizer suite (`dynabook-kernel-ctl` and `plugins/cpu-governor.sh`).
//! - Linux sysfs/procfs interfaces (`cpufreq`, `intel_pstate`, `lru_gen/min_ttl_ms`, `thermal_zone`, `power_supply`).
//!
//! ## Invariants & Operational Pipeline:
//! 1. **Proactive Workload Ramp-Up (`HardwareBoostGuard`)**:
//!    - When intensive AI workloads (LLM generation, WASM compilation, self-healing reflection, vector indexing)
//!      spin up, callers acquire an RAII `HardwareBoostGuard`.
//!    - Proactively switches cpufreq governors to `performance`, sets EPP to `performance`, enables Intel Turbo
//!      (`no_turbo = 0`), and sets Multi-Gen LRU (MGLRU) `min_ttl_ms = 1000` to safeguard active memory working sets.
//! 2. **Auto-Sleep Idle Decay**:
//!    - When the active workload lease count drops to 0 and the idle grace period (default 3 seconds) elapses,
//!      smoothly drops governor back to `powersave` and resets MGLRU `min_ttl_ms = 0`.
//! 3. **Thermal & Battery Safety Clamps**:
//!    - Evaluates thermal sensors and battery discharge telemetry before and during active leases.
//!    - **Emergency Clamp Threshold**: CPU package temp >= 78.0°C OR battery capacity < 20% on battery discharge.
//!    - Automatically clamps governor to `powersave`, EPP to `balance_power`/`power`, and rejects boost acquisitions.
//!    - **Hysteresis Recovery**: Clamps disengage only when temperature cools below 70.0°C and battery is >= 22% (or on AC).
//! 4. **Model Context Protocol (MCP) Tool**:
//!    - Declares `hgb_neural_hardware_boost` for programmatic workload acceleration from IDEs.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::response::sse::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use hgb_core::error::{HgbError, Result};
use crate::mcp_sse_server::McpSseState;

/// Canonical MCP tool name for neural hardware boost
pub const NEURAL_HARDWARE_BOOST_TOOL_NAME: &str = "hgb_neural_hardware_boost";

/// Default Dynatune Unix domain socket
pub const DEFAULT_DYNATUNE_SOCKET: &str = "/run/dynatune.sock";
/// Default sibling path to `dynabook-kernel-ctl`
pub const SIBLING_KERNEL_CTL_BIN: &str = "/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl";
/// System fallback path to `dynabook-kernel-ctl`
pub const SYSTEM_KERNEL_CTL_BIN: &str = "/usr/local/bin/dynabook-kernel-ctl";
/// Default sibling path to `plugins/cpu-governor.sh`
pub const SIBLING_CPU_GOVERNOR_SCRIPT: &str = "/home/toshiba/My AI Project/kernel-optimizer/plugins/cpu-governor.sh";
/// Secondary fallback path to `scripts/cpu-governor.sh`
pub const SECONDARY_CPU_GOVERNOR_SCRIPT: &str = "/home/toshiba/My AI Project/hagibis/scripts/cpu-governor.sh";

/// Thermal clamp trigger temperature (78.0°C)
pub const THERMAL_CLAMP_THRESHOLD_C: f64 = 78.0;
/// Thermal recovery threshold with hysteresis (70.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 70.0;
/// Critical battery percentage threshold on battery discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;
/// Battery recovery threshold with hysteresis (22%)
pub const BATTERY_RECOVERY_DISCHARGE_PCT: u32 = 22;

/// MGLRU burst protection TTL (1000ms)
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;
/// MGLRU baseline idle TTL (0ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Default socket communication timeout (500ms)
pub const DYNATUNE_SOCKET_TIMEOUT: Duration = Duration::from_millis(500);
/// Default idle grace period before dropping to powersave (3 seconds)
pub const DEFAULT_IDLE_GRACE_PERIOD_SECS: u64 = 3;

// ==============================================================================
// 1. Data Models & Hardware States
// ==============================================================================

/// Target hardware profile mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NeuralGovernorProfile {
    /// Maximum throughput: governor `performance`, EPP `performance`, turbo on, MGLRU 1000ms
    Performance,
    /// Balanced standard: governor `powersave`, EPP `balance_performance`, turbo on, MGLRU 0ms
    Balanced,
    /// Energy efficiency / auto-sleep idle: governor `powersave`, EPP `balance_power`, MGLRU 0ms
    PowerSave,
    /// Emergency safety clamped: governor `powersave`, EPP `power`, turbo off, MGLRU 0ms
    EmergencyClamped,
}

impl NeuralGovernorProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::PowerSave => "powersave",
            Self::EmergencyClamped => "emergency_clamped",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "performance" | "perf" | "boost" | "turbo" => Self::Performance,
            "balanced" | "balance" | "nominal" => Self::Balanced,
            "powersave" | "power" | "battery" | "idle" | "sleep" => Self::PowerSave,
            "clamped" | "emergency" | "throttle" => Self::EmergencyClamped,
            _ => Self::Balanced,
        }
    }
}

/// Intensive AI workload category triggering proactive hardware ramp-up
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiWorkloadKind {
    LlmInference,
    WasmCompilation,
    BatchEmbedding,
    SelfHealingReflection,
    ModelFineTuning,
    MicrovmExecution,
    GenericIntensive,
}

impl AiWorkloadKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LlmInference => "llm_inference",
            Self::WasmCompilation => "wasm_compilation",
            Self::BatchEmbedding => "batch_embedding",
            Self::SelfHealingReflection => "self_healing_reflection",
            Self::ModelFineTuning => "model_fine_tuning",
            Self::MicrovmExecution => "microvm_execution",
            Self::GenericIntensive => "generic_intensive",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "llm" | "llm_inference" | "generation" => Self::LlmInference,
            "wasm" | "wasm_compilation" | "compile" => Self::WasmCompilation,
            "embed" | "embedding" | "batch_embedding" => Self::BatchEmbedding,
            "heal" | "self_heal" | "reflection" | "self_healing_reflection" => Self::SelfHealingReflection,
            "fine_tune" | "train" | "model_fine_tuning" => Self::ModelFineTuning,
            "microvm" | "firecracker" | "microvm_execution" => Self::MicrovmExecution,
            _ => Self::GenericIntensive,
        }
    }
}

/// Safety clamp status descriptor
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SafetyClampStatus {
    Normal,
    ThermalClamped {
        cpu_temp_c: f64,
        threshold_c: f64,
    },
    BatteryDischargeClamped {
        battery_pct: u32,
        threshold_pct: u32,
    },
}

impl SafetyClampStatus {
    pub fn is_clamped(&self) -> bool {
        !matches!(self, Self::Normal)
    }

    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Normal => None,
            Self::ThermalClamped { cpu_temp_c, threshold_c } => Some(format!(
                "Thermal clamp engaged: CPU temperature {:.1}°C exceeds safety ceiling {:.1}°C",
                cpu_temp_c, threshold_c
            )),
            Self::BatteryDischargeClamped { battery_pct, threshold_pct } => Some(format!(
                "Battery clamp engaged: capacity {}% below safety ceiling {}% on discharge",
                battery_pct, threshold_pct
            )),
        }
    }
}

/// Snapshot telemetry payload of current hardware and feedback loop state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnifiedHardwareTelemetry {
    pub cpu_temp_c: f64,
    pub ac_online: bool,
    pub battery_pct: Option<u32>,
    pub is_discharging: bool,
    pub battery_power_w: f64,
    pub active_leases: usize,
    pub current_profile: NeuralGovernorProfile,
    pub scaling_governor: String,
    pub epp: String,
    pub turbo_enabled: bool,
    pub mglru_min_ttl_ms: u64,
    pub clamp_status: SafetyClampStatus,
    pub is_idle: bool,
    pub dynatune_socket_online: bool,
    pub kernel_optimizer_online: bool,
    pub timestamp_epoch_ms: u64,
}

// ==============================================================================
// 2. Hardware Controller Paths (Production & Sandboxed Tests)
// ==============================================================================

/// Configurable paths to sysfs, procfs, dynatune socket, and scripts
#[derive(Debug, Clone)]
pub struct HardwareLoopPaths {
    pub sysfs_root: PathBuf,
    pub procfs_root: PathBuf,
    pub dynatune_sock: PathBuf,
    pub kernel_ctl_bin: PathBuf,
    pub cpu_governor_script: PathBuf,
}

impl Default for HardwareLoopPaths {
    fn default() -> Self {
        Self {
            sysfs_root: PathBuf::from("/"),
            procfs_root: PathBuf::from("/"),
            dynatune_sock: PathBuf::from(DEFAULT_DYNATUNE_SOCKET),
            kernel_ctl_bin: resolve_kernel_ctl_path(),
            cpu_governor_script: resolve_cpu_governor_script_path(),
        }
    }
}

fn resolve_kernel_ctl_path() -> PathBuf {
    let sibling = PathBuf::from(SIBLING_KERNEL_CTL_BIN);
    if sibling.exists() {
        return sibling;
    }
    let system = PathBuf::from(SYSTEM_KERNEL_CTL_BIN);
    if system.exists() {
        return system;
    }
    sibling
}

fn resolve_cpu_governor_script_path() -> PathBuf {
    let sibling = PathBuf::from(SIBLING_CPU_GOVERNOR_SCRIPT);
    if sibling.exists() {
        return sibling;
    }
    let secondary = PathBuf::from(SECONDARY_CPU_GOVERNOR_SCRIPT);
    if secondary.exists() {
        return secondary;
    }
    sibling
}

// ==============================================================================
// 3. RAII Hardware Boost Guard
// ==============================================================================

/// RAII Workload Lease Guard holding active hardware boost
pub struct HardwareBoostGuard {
    loop_controller: Arc<UnifiedNeuralHardwareLoop>,
    workload_kind: AiWorkloadKind,
    workload_label: String,
    acquired_at_ms: u64,
}

impl HardwareBoostGuard {
    pub fn workload_kind(&self) -> AiWorkloadKind {
        self.workload_kind
    }

    pub fn workload_label(&self) -> &str {
        &self.workload_label
    }

    pub fn acquired_at_ms(&self) -> u64 {
        self.acquired_at_ms
    }
}

impl Drop for HardwareBoostGuard {
    fn drop(&mut self) {
        self.loop_controller.release_boost_internal(&self.workload_label);
    }
}

// ==============================================================================
// 4. Unified Neural-Hardware Loop Engine
// ==============================================================================

/// Core feedback loop coordinating workload ramp-up, auto-sleep idle, and safety clamps
pub struct UnifiedNeuralHardwareLoop {
    paths: HardwareLoopPaths,
    active_leases: Arc<AtomicUsize>,
    last_profile: Arc<RwLock<NeuralGovernorProfile>>,
    last_workload_instant: Arc<Mutex<Instant>>,
    is_clamped: Arc<AtomicBool>,
    idle_grace_period: Duration,
    mglru_ttl_ms: Arc<AtomicU64>,
}

impl UnifiedNeuralHardwareLoop {
    /// Initialize with default paths and 3s idle grace period
    pub fn new() -> Arc<Self> {
        Self::with_paths(HardwareLoopPaths::default(), Duration::from_secs(DEFAULT_IDLE_GRACE_PERIOD_SECS))
    }

    /// Initialize with custom paths and idle grace period (supports sandboxed testing)
    pub fn with_paths(paths: HardwareLoopPaths, idle_grace: Duration) -> Arc<Self> {
        Arc::new(Self {
            paths,
            active_leases: Arc::new(AtomicUsize::new(0)),
            last_profile: Arc::new(RwLock::new(NeuralGovernorProfile::Balanced)),
            last_workload_instant: Arc::new(Mutex::new(Instant::now())),
            is_clamped: Arc::new(AtomicBool::new(false)),
            idle_grace_period: idle_grace,
            mglru_ttl_ms: Arc::new(AtomicU64::new(IDLE_MGLRU_MIN_TTL_MS)),
        })
    }

    /// Inspect real-time hardware telemetry and clamp status
    pub fn collect_telemetry(&self) -> UnifiedHardwareTelemetry {
        let (cpu_temp_c, ac_online, battery_pct, is_discharging, battery_power_w) =
            self.read_thermal_and_power();
        let (scaling_governor, epp, turbo_enabled, mglru_ttl) = self.read_active_hardware_state();
        let leases = self.active_leases.load(Ordering::Relaxed);
        let current_profile = *self.last_profile.read().unwrap();
        let clamp_status = self.evaluate_clamp_status(cpu_temp_c, battery_pct, is_discharging, ac_online);

        let is_idle = leases == 0;
        let dynatune_socket_online = self.paths.dynatune_sock.exists();
        let kernel_optimizer_online = self.paths.kernel_ctl_bin.exists() || self.paths.cpu_governor_script.exists();

        UnifiedHardwareTelemetry {
            cpu_temp_c,
            ac_online,
            battery_pct,
            is_discharging,
            battery_power_w,
            active_leases: leases,
            current_profile,
            scaling_governor,
            epp,
            turbo_enabled,
            mglru_min_ttl_ms: mglru_ttl,
            clamp_status,
            is_idle,
            dynatune_socket_online,
            kernel_optimizer_online,
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }

    /// Proactively acquires hardware boost for an intensive AI workload
    pub fn acquire_boost(
        self: &Arc<Self>,
        workload: AiWorkloadKind,
        label: impl Into<String>,
    ) -> Result<HardwareBoostGuard> {
        let label_str = label.into();
        let (cpu_temp_c, ac_online, battery_pct, is_discharging, _) = self.read_thermal_and_power();
        let clamp = self.evaluate_clamp_status(cpu_temp_c, battery_pct, is_discharging, ac_online);

        if clamp.is_clamped() {
            // Refuse boost and maintain emergency clamp
            self.is_clamped.store(true, Ordering::SeqCst);
            self.apply_profile(NeuralGovernorProfile::EmergencyClamped);
            return Err(HgbError::Execution(format!(
                "Cannot acquire hardware boost: {}",
                clamp.reason().unwrap_or_else(|| "Hardware safety clamp active".to_string())
            )));
        }

        // Increment active lease counter
        let prev_leases = self.active_leases.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut inst) = self.last_workload_instant.lock() {
            *inst = Instant::now();
        }

        // Ramp up hardware if this is the first active workload
        if prev_leases == 0 {
            self.apply_profile(NeuralGovernorProfile::Performance);
        }

        Ok(HardwareBoostGuard {
            loop_controller: Arc::clone(self),
            workload_kind: workload,
            workload_label: label_str,
            acquired_at_ms: current_epoch_ms(),
        })
    }

    /// Internal drop hook releasing a workload lease
    fn release_boost_internal(&self, _label: &str) {
        let remaining = self.active_leases.fetch_sub(1, Ordering::SeqCst) - 1;
        if let Ok(mut inst) = self.last_workload_instant.lock() {
            *inst = Instant::now();
        }

        // If zero active leases remain, perform auto-sleep transition
        if remaining == 0 {
            let (cpu_temp_c, ac_online, battery_pct, is_discharging, _) = self.read_thermal_and_power();
            let clamp = self.evaluate_clamp_status(cpu_temp_c, battery_pct, is_discharging, ac_online);
            if clamp.is_clamped() {
                self.is_clamped.store(true, Ordering::SeqCst);
                self.apply_profile(NeuralGovernorProfile::EmergencyClamped);
            } else {
                // Drop to powersave and reset MGLRU TTL to 0ms
                self.apply_profile(NeuralGovernorProfile::PowerSave);
            }
        }
    }

    /// Periodic watchdog tick checking thermal clamps and auto-sleep idle decay
    pub fn watchdog_tick(&self) -> UnifiedHardwareTelemetry {
        let (cpu_temp_c, ac_online, battery_pct, is_discharging, _) = self.read_thermal_and_power();
        let clamp = self.evaluate_clamp_status(cpu_temp_c, battery_pct, is_discharging, ac_online);

        if clamp.is_clamped() {
            self.is_clamped.store(true, Ordering::SeqCst);
            self.apply_profile(NeuralGovernorProfile::EmergencyClamped);
        } else {
            let was_clamped = self.is_clamped.swap(false, Ordering::SeqCst);
            let active = self.active_leases.load(Ordering::Relaxed);

            if was_clamped {
                // Unclamped via hysteresis: restore appropriate profile
                if active > 0 {
                    self.apply_profile(NeuralGovernorProfile::Performance);
                } else {
                    self.apply_profile(NeuralGovernorProfile::Balanced);
                }
            } else if active == 0 {
                // Auto-sleep idle drop check
                let elapsed = self
                    .last_workload_instant
                    .lock()
                    .map(|inst| inst.elapsed())
                    .unwrap_or(self.idle_grace_period);

                if elapsed >= self.idle_grace_period {
                    let cur = *self.last_profile.read().unwrap();
                    if cur != NeuralGovernorProfile::PowerSave {
                        self.apply_profile(NeuralGovernorProfile::PowerSave);
                    }
                }
            }
        }

        self.collect_telemetry()
    }

    /// Evaluates thermal and battery clamp status with hysteresis
    fn evaluate_clamp_status(
        &self,
        cpu_temp_c: f64,
        battery_pct: Option<u32>,
        is_discharging: bool,
        ac_online: bool,
    ) -> SafetyClampStatus {
        let currently_clamped = self.is_clamped.load(Ordering::Relaxed);

        // 1. Thermal ceiling: >= 78.0°C engages clamp
        if cpu_temp_c >= THERMAL_CLAMP_THRESHOLD_C {
            return SafetyClampStatus::ThermalClamped {
                cpu_temp_c,
                threshold_c: THERMAL_CLAMP_THRESHOLD_C,
            };
        }

        // 2. Critical battery ceiling: < 20% on discharge engages clamp
        if let Some(pct) = battery_pct {
            if pct < BATTERY_CRITICAL_DISCHARGE_PCT && (is_discharging || !ac_online) {
                return SafetyClampStatus::BatteryDischargeClamped {
                    battery_pct: pct,
                    threshold_pct: BATTERY_CRITICAL_DISCHARGE_PCT,
                };
            }
        }

        // 3. Hysteresis: remain clamped until temperature cools below 70.0°C and battery recovers
        if currently_clamped {
            if cpu_temp_c >= THERMAL_RECOVERY_THRESHOLD_C {
                return SafetyClampStatus::ThermalClamped {
                    cpu_temp_c,
                    threshold_c: THERMAL_RECOVERY_THRESHOLD_C,
                };
            }
            if let Some(pct) = battery_pct {
                if pct < BATTERY_RECOVERY_DISCHARGE_PCT && (is_discharging || !ac_online) {
                    return SafetyClampStatus::BatteryDischargeClamped {
                        battery_pct: pct,
                        threshold_pct: BATTERY_RECOVERY_DISCHARGE_PCT,
                    };
                }
            }
        }

        SafetyClampStatus::Normal
    }

    /// Applies target governor profile across Dynatune IPC socket, kernel-optimizer scripts, or sysfs
    pub fn apply_profile(&self, profile: NeuralGovernorProfile) -> bool {
        let mut prof_guard = self.last_profile.write().unwrap();
        *prof_guard = profile;
        drop(prof_guard);

        let (target_gov, target_epp, target_no_turbo, target_mglru_ttl) = match profile {
            NeuralGovernorProfile::Performance => ("performance", "performance", "0", BURST_MGLRU_MIN_TTL_MS),
            NeuralGovernorProfile::Balanced => ("powersave", "balance_performance", "0", IDLE_MGLRU_MIN_TTL_MS),
            NeuralGovernorProfile::PowerSave => ("powersave", "balance_power", "1", IDLE_MGLRU_MIN_TTL_MS),
            NeuralGovernorProfile::EmergencyClamped => ("powersave", "power", "1", IDLE_MGLRU_MIN_TTL_MS),
        };

        self.mglru_ttl_ms.store(target_mglru_ttl, Ordering::SeqCst);

        // 1. Primary: Dynatune Unix Socket IPC
        if self.paths.dynatune_sock.exists() {
            let msg = serde_json::json!({
                "action": "set_governor",
                "governor": target_gov,
                "epp": target_epp,
                "caller": "hgb_neural_hardware_loop"
            });
            if self.dispatch_dynatune_socket(&msg).is_ok() {
                // Apply MGLRU TTL directly to sysfs as companion
                self.apply_mglru_sysfs(target_mglru_ttl);
                return true;
            }
        }

        // 2. Secondary: `dynabook-kernel-ctl profile <name>`
        if self.paths.kernel_ctl_bin.exists() {
            let cmd_prof = match profile {
                NeuralGovernorProfile::Performance => "performance",
                NeuralGovernorProfile::Balanced => "balanced",
                NeuralGovernorProfile::PowerSave | NeuralGovernorProfile::EmergencyClamped => "battery",
            };
            if let Ok(status) = Command::new(&self.paths.kernel_ctl_bin)
                .args(["profile", cmd_prof])
                .status()
            {
                if status.success() {
                    self.apply_mglru_sysfs(target_mglru_ttl);
                    return true;
                }
            }
        }

        // 3. Tertiary: `plugins/cpu-governor.sh profile <name>`
        if self.paths.cpu_governor_script.exists() {
            let cmd_prof = match profile {
                NeuralGovernorProfile::Performance => "performance",
                NeuralGovernorProfile::Balanced => "balanced",
                NeuralGovernorProfile::PowerSave | NeuralGovernorProfile::EmergencyClamped => "battery",
            };
            if let Ok(status) = Command::new(&self.paths.cpu_governor_script)
                .args(["profile", cmd_prof])
                .status()
            {
                if status.success() {
                    self.apply_mglru_sysfs(target_mglru_ttl);
                    return true;
                }
            }
        }

        // 4. Quaternary: Direct Linux sysfs writes
        self.apply_direct_sysfs(target_gov, target_epp, target_no_turbo, target_mglru_ttl)
    }

    fn dispatch_dynatune_socket(&self, payload: &Value) -> Result<()> {
        let mut stream = UnixStream::connect(&self.paths.dynatune_sock).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to Dynatune socket: {}", e))
        })?;

        stream.set_write_timeout(Some(DYNATUNE_SOCKET_TIMEOUT)).ok();
        stream.set_read_timeout(Some(DYNATUNE_SOCKET_TIMEOUT)).ok();

        let mut data = serde_json::to_vec(payload).map_err(|e| HgbError::Serialization(e.to_string()))?;
        data.push(b'\n');
        stream.write_all(&data).map_err(|e| HgbError::Io(e))?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| HgbError::Io(e))?;

        Ok(())
    }

    fn apply_direct_sysfs(
        &self,
        governor: &str,
        epp: &str,
        no_turbo: &str,
        mglru_ttl: u64,
    ) -> bool {
        let mut touched = false;

        // cpufreq policies
        let cpufreq_dir = self.paths.sysfs_root.join("sys/devices/system/cpu/cpufreq");
        if let Ok(entries) = fs::read_dir(cpufreq_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                    let gov_file = p.join("scaling_governor");
                    let epp_file = p.join("energy_performance_preference");
                    if fs::write(&gov_file, format!("{}\n", governor)).is_ok() {
                        touched = true;
                    }
                    if fs::write(&epp_file, format!("{}\n", epp)).is_ok() {
                        touched = true;
                    }
                }
            }
        }

        // intel_pstate no_turbo
        let pstate_turbo = self.paths.sysfs_root.join("sys/devices/system/cpu/intel_pstate/no_turbo");
        if pstate_turbo.exists() {
            if fs::write(&pstate_turbo, format!("{}\n", no_turbo)).is_ok() {
                touched = true;
            }
        }

        // MGLRU min_ttl_ms
        if self.apply_mglru_sysfs(mglru_ttl) {
            touched = true;
        }

        touched
    }

    fn apply_mglru_sysfs(&self, min_ttl_ms: u64) -> bool {
        let mglru_file = self.paths.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms");
        if mglru_file.exists() {
            return fs::write(&mglru_file, format!("{}\n", min_ttl_ms)).is_ok();
        }
        false
    }

    fn read_thermal_and_power(&self) -> (f64, bool, Option<u32>, bool, f64) {
        let mut cpu_temp_c = 45.0;
        let mut ac_online = true;
        let mut battery_pct = None;
        let mut is_discharging = false;
        let mut battery_power_w = 0.0;

        // Read CPU temperature from thermal zones
        let thermal_dir = self.paths.sysfs_root.join("sys/class/thermal");
        if let Ok(entries) = fs::read_dir(thermal_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("thermal_zone") {
                    let type_str = fs::read_to_string(p.join("type")).unwrap_or_default();
                    if let Ok(temp_str) = fs::read_to_string(p.join("temp")) {
                        if let Ok(milli) = temp_str.trim().parse::<f64>() {
                            let temp = milli / 1000.0;
                            if type_str.trim() == "x86_pkg_temp" {
                                cpu_temp_c = temp;
                                break;
                            } else if temp > cpu_temp_c {
                                cpu_temp_c = temp;
                            }
                        }
                    }
                }
            }
        }

        // Read Power Supply (AC & Battery)
        let ps_dir = self.paths.sysfs_root.join("sys/class/power_supply");
        if let Ok(entries) = fs::read_dir(ps_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(on_str) = fs::read_to_string(p.join("online")) {
                        ac_online = on_str.trim() == "1";
                    }
                } else if name.starts_with("BAT") && battery_pct.is_none() {
                    if let Ok(cap_str) = fs::read_to_string(p.join("capacity")) {
                        if let Ok(cap) = cap_str.trim().parse::<u32>() {
                            battery_pct = Some(cap);
                        }
                    }
                    if let Ok(stat_str) = fs::read_to_string(p.join("status")) {
                        let st = stat_str.trim().to_lowercase();
                        if st == "discharging" {
                            is_discharging = true;
                            ac_online = false;
                        } else if st == "charging" || st == "full" {
                            ac_online = true;
                        }
                    }
                    if let Ok(p_str) = fs::read_to_string(p.join("power_now")) {
                        if let Ok(uw) = p_str.trim().parse::<f64>() {
                            battery_power_w = uw / 1_000_000.0;
                        }
                    }
                }
            }
        }

        (cpu_temp_c, ac_online, battery_pct, is_discharging, battery_power_w)
    }

    fn read_active_hardware_state(&self) -> (String, String, bool, u64) {
        let gov = fs::read_to_string(self.paths.sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor"))
            .unwrap_or_else(|_| "powersave".to_string())
            .trim()
            .to_string();

        let epp = fs::read_to_string(self.paths.sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference"))
            .unwrap_or_else(|_| "balance_performance".to_string())
            .trim()
            .to_string();

        let turbo = fs::read_to_string(self.paths.sysfs_root.join("sys/devices/system/cpu/intel_pstate/no_turbo"))
            .map(|s| s.trim() == "0")
            .unwrap_or(true);

        let mglru = fs::read_to_string(self.paths.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(self.mglru_ttl_ms.load(Ordering::Relaxed));

        (gov, epp, turbo, mglru)
    }

    // ==============================================================================
    // 5. MCP Tool Implementation (`hgb_neural_hardware_boost`)
    // ==============================================================================

    /// Returns Model Context Protocol declarative tool definition
    pub fn tool_definition() -> Value {
        serde_json::json!({
            "name": NEURAL_HARDWARE_BOOST_TOOL_NAME,
            "description": "Proactively ramps up CPU scaling governors to performance, enables Intel turbo, and tunes MGLRU memory cache protection for intensive AI workloads, with auto-sleep idle decay and thermal/battery safety clamps.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["acquire_boost", "release_boost", "query_status", "force_governor"],
                        "description": "Feedback loop action: 'acquire_boost', 'release_boost', 'query_status', 'force_governor'"
                    },
                    "workload": {
                        "type": "string",
                        "description": "Workload category: 'llm_inference', 'wasm_compilation', 'batch_embedding', 'self_healing_reflection'"
                    },
                    "label": {
                        "type": "string",
                        "description": "Unique workload label or lease identifier"
                    },
                    "target_profile": {
                        "type": "string",
                        "enum": ["performance", "balanced", "powersave"],
                        "description": "Explicit target profile when forcing governor state"
                    }
                },
                "required": ["action"]
            }
        })
    }

    /// Handles MCP tool call from JSON arguments
    pub async fn handle_tool_call(
        loop_controller: &Arc<UnifiedNeuralHardwareLoop>,
        arguments: &Value,
        _session_id: &str,
    ) -> (Value, bool) {
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("query_status");
        let label = arguments.get("label").and_then(|v| v.as_str()).unwrap_or("mcp_caller");
        let workload_str = arguments.get("workload").and_then(|v| v.as_str()).unwrap_or("generic");
        let workload_kind = AiWorkloadKind::from_str_loose(workload_str);

        match action {
            "query_status" => {
                let telem = loop_controller.collect_telemetry();
                let json_val = serde_json::to_value(&telem).unwrap_or(Value::Null);
                (
                    serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&telem).unwrap_or_default() }],
                        "telemetry": json_val,
                        "isError": false
                    }),
                    false,
                )
            }

            "acquire_boost" => {
                match loop_controller.acquire_boost(workload_kind, label) {
                    Ok(guard) => {
                        // Leak the guard intentionally if the caller wants to release manually or via duration
                        let acquired_ms = guard.acquired_at_ms();
                        std::mem::forget(guard);

                        let telem = loop_controller.collect_telemetry();
                        let resp = serde_json::json!({
                            "status": "boost_acquired",
                            "workload": workload_kind.as_str(),
                            "label": label,
                            "acquired_epoch_ms": acquired_ms,
                            "active_leases": telem.active_leases,
                            "profile": telem.current_profile.as_str(),
                            "mglru_min_ttl_ms": telem.mglru_min_ttl_ms,
                        });
                        (
                            serde_json::json!({
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp).unwrap_or_default() }],
                                "isError": false
                            }),
                            false,
                        )
                    }
                    Err(e) => {
                        let resp = serde_json::json!({
                            "status": "boost_rejected",
                            "error": e.to_string(),
                            "telemetry": loop_controller.collect_telemetry(),
                        });
                        (
                            serde_json::json!({
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp).unwrap_or_default() }],
                                "isError": true
                            }),
                            true,
                        )
                    }
                }
            }

            "release_boost" => {
                loop_controller.release_boost_internal(label);
                let telem = loop_controller.collect_telemetry();
                let resp = serde_json::json!({
                    "status": "boost_released",
                    "label": label,
                    "remaining_active_leases": telem.active_leases,
                    "profile": telem.current_profile.as_str(),
                    "mglru_min_ttl_ms": telem.mglru_min_ttl_ms,
                });
                (
                    serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp).unwrap_or_default() }],
                        "isError": false
                    }),
                    false,
                )
            }

            "force_governor" => {
                let prof_str = arguments.get("target_profile").and_then(|v| v.as_str()).unwrap_or("balanced");
                let prof = NeuralGovernorProfile::from_str_loose(prof_str);
                let applied = loop_controller.apply_profile(prof);
                let telem = loop_controller.collect_telemetry();
                let resp = serde_json::json!({
                    "status": "governor_forced",
                    "target_profile": prof.as_str(),
                    "applied": applied,
                    "telemetry": telem,
                });
                (
                    serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp).unwrap_or_default() }],
                        "isError": !applied
                    }),
                    !applied,
                )
            }

            other => {
                let err_msg = format!("Unknown action '{}'. Supported: acquire_boost, release_boost, query_status, force_governor", other);
                (
                    serde_json::json!({
                        "content": [{ "type": "text", "text": err_msg }],
                        "isError": true
                    }),
                    true,
                )
            }
        }
    }
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ==============================================================================
// 6. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_mock_sysfs_environment(temp_dir: &Path) -> HardwareLoopPaths {
        let sysfs = temp_dir.join("sys");
        let procfs = temp_dir.join("proc");

        // cpufreq policy0
        let policy0 = sysfs.join("devices/system/cpu/cpufreq/policy0");
        fs::create_dir_all(&policy0).unwrap();
        fs::write(policy0.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(policy0.join("energy_performance_preference"), "balance_performance\n").unwrap();

        // intel_pstate
        let pstate = sysfs.join("devices/system/cpu/intel_pstate");
        fs::create_dir_all(&pstate).unwrap();
        fs::write(pstate.join("no_turbo"), "0\n").unwrap();

        // MGLRU
        let mglru = sysfs.join("kernel/mm/lru_gen");
        fs::create_dir_all(&mglru).unwrap();
        fs::write(mglru.join("min_ttl_ms"), "0\n").unwrap();

        // thermal_zone0
        let thermal = sysfs.join("class/thermal/thermal_zone0");
        fs::create_dir_all(&thermal).unwrap();
        fs::write(thermal.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(thermal.join("temp"), "45000\n").unwrap(); // 45.0°C

        // power_supply AC
        let ac = sysfs.join("class/power_supply/AC");
        fs::create_dir_all(&ac).unwrap();
        fs::write(ac.join("online"), "1\n").unwrap();

        // power_supply BAT0
        let bat = sysfs.join("class/power_supply/BAT0");
        fs::create_dir_all(&bat).unwrap();
        fs::write(bat.join("capacity"), "85\n").unwrap();
        fs::write(bat.join("status"), "Full\n").unwrap();

        HardwareLoopPaths {
            sysfs_root: temp_dir.to_path_buf(),
            procfs_root: procfs,
            dynatune_sock: temp_dir.join("dynatune.sock"),
            kernel_ctl_bin: temp_dir.join("dynabook-kernel-ctl"),
            cpu_governor_script: temp_dir.join("cpu-governor.sh"),
        }
    }

    #[test]
    fn test_mcp_tool_definition_schema() {
        let def = UnifiedNeuralHardwareLoop::tool_definition();
        assert_eq!(def["name"], NEURAL_HARDWARE_BOOST_TOOL_NAME);
        let schema = &def["inputSchema"];
        assert_eq!(schema["type"], "object");
        let props = &schema["properties"];
        assert!(props.get("action").is_some());
        assert!(props.get("workload").is_some());
        assert!(props.get("label").is_some());
        assert!(props.get("target_profile").is_some());
    }

    #[test]
    fn test_proactive_workload_boost_and_drop_lifecycle() {
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = create_mock_sysfs_environment(temp_dir.path());
        let loop_ctrl = UnifiedNeuralHardwareLoop::with_paths(paths.clone(), Duration::from_millis(50));

        let initial_telem = loop_ctrl.collect_telemetry();
        assert_eq!(initial_telem.active_leases, 0);

        // 1. Acquire boost
        {
            let guard = loop_ctrl
                .acquire_boost(AiWorkloadKind::LlmInference, "test_prompt_runner")
                .expect("acquire boost");

            assert_eq!(guard.workload_kind(), AiWorkloadKind::LlmInference);
            assert_eq!(loop_ctrl.active_leases.load(Ordering::SeqCst), 1);

            let boosted_telem = loop_ctrl.collect_telemetry();
            assert_eq!(boosted_telem.current_profile, NeuralGovernorProfile::Performance);
            assert_eq!(boosted_telem.mglru_min_ttl_ms, BURST_MGLRU_MIN_TTL_MS);

            // Verify sysfs write occurred
            let gov_str = fs::read_to_string(paths.sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
            assert_eq!(gov_str.trim(), "performance");
            let mglru_str = fs::read_to_string(paths.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
            assert_eq!(mglru_str.trim(), "1000");
        } // guard drops here

        // 2. Post-drop auto-sleep transition
        assert_eq!(loop_ctrl.active_leases.load(Ordering::SeqCst), 0);
        let dropped_telem = loop_ctrl.collect_telemetry();
        assert_eq!(dropped_telem.current_profile, NeuralGovernorProfile::PowerSave);
        assert_eq!(dropped_telem.mglru_min_ttl_ms, IDLE_MGLRU_MIN_TTL_MS);

        let gov_str = fs::read_to_string(paths.sysfs_root.join("sys/devices/system/cpu/cpufreq/policy0/scaling_governor")).unwrap();
        assert_eq!(gov_str.trim(), "powersave");
        let mglru_str = fs::read_to_string(paths.sysfs_root.join("sys/kernel/mm/lru_gen/min_ttl_ms")).unwrap();
        assert_eq!(mglru_str.trim(), "0");
    }

    #[test]
    fn test_thermal_safety_clamp_enforcement_and_hysteresis() {
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = create_mock_sysfs_environment(temp_dir.path());
        let loop_ctrl = UnifiedNeuralHardwareLoop::with_paths(paths.clone(), Duration::from_millis(50));

        // Simulate high temperature (82.0°C >= 78.0°C threshold)
        let thermal_file = paths.sysfs_root.join("sys/class/thermal/thermal_zone0/temp");
        fs::write(&thermal_file, "82000\n").unwrap();

        // Attempt boost: must be rejected
        let res = loop_ctrl.acquire_boost(AiWorkloadKind::WasmCompilation, "hot_compile");
        assert!(res.is_err(), "Boost must be rejected when thermal clamp engaged");

        let telem = loop_ctrl.collect_telemetry();
        assert!(telem.clamp_status.is_clamped());
        assert_eq!(telem.current_profile, NeuralGovernorProfile::EmergencyClamped);

        // Temperature cools to 74.0°C (between 70°C recovery and 78°C clamp): hysteresis must keep clamp active
        fs::write(&thermal_file, "74000\n").unwrap();
        loop_ctrl.watchdog_tick();
        assert!(loop_ctrl.collect_telemetry().clamp_status.is_clamped(), "Hysteresis must hold clamp active at 74°C");

        // Temperature cools to 68.0°C (< 70°C recovery): hysteresis unclamps
        fs::write(&thermal_file, "68000\n").unwrap();
        loop_ctrl.watchdog_tick();
        assert!(!loop_ctrl.collect_telemetry().clamp_status.is_clamped(), "Clamp must disengage below 70°C");
    }

    #[test]
    fn test_battery_critical_discharge_clamp() {
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = create_mock_sysfs_environment(temp_dir.path());
        let loop_ctrl = UnifiedNeuralHardwareLoop::with_paths(paths.clone(), Duration::from_millis(50));

        // Set battery to 15% on discharge (ac_online = 0)
        let ac_file = paths.sysfs_root.join("sys/class/power_supply/AC/online");
        fs::write(ac_file, "0\n").unwrap();
        let bat_stat = paths.sysfs_root.join("sys/class/power_supply/BAT0/status");
        fs::write(bat_stat, "Discharging\n").unwrap();
        let bat_cap = paths.sysfs_root.join("sys/class/power_supply/BAT0/capacity");
        fs::write(bat_cap, "15\n").unwrap();

        // Attempt boost: must be rejected due to battery clamp
        let res = loop_ctrl.acquire_boost(AiWorkloadKind::BatchEmbedding, "embed_job");
        assert!(res.is_err(), "Boost must be rejected when battery < 20% on discharge");
    }
}
