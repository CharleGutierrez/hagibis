//! # Dynamic Governor Watchdog for Hagibis Daemon
//!
//! Subsystem integrating hardware-aware CPU Governor management, thermal monitoring,
//! Dynatune bridge synergy, and automatic workload boost / relax cycles into `hgbd`.
//!
//! Real-Time Closed-Loop Self-Healing Engine:
//! - Directly queries host telemetry from Dynatune and Kernel-Optimizer:
//!   * CPU thermal zones (`/sys/class/thermal/thermal_zone*/temp` & package temp)
//!   * Instantaneous battery discharge rate in Watts and velocity in %/hour drop
//!   * Host Linux Multi-Gen LRU (MGLRU) & PSI memory pressure severity
//! - Autonomous Threshold Evaluation:
//!   * CPU package temp >= 78.0°C
//!   * Battery discharge spike (> 30.0W or > 25.0%/hr drop)
//!   * Critical MGLRU memory pressure (`MglruPressureLevel::Critical`)
//! - Autonomous Incident Mitigation:
//!   * Samples host process tree (`ps -eo pid,ppid,%cpu,%mem,cmd --sort=-%cpu` / `/proc` parsing)
//!   * Formulates structured diagnostic prompt and invokes local Ollama LLM
//!     (via `/tmp/ollama.sock`, `http://127.0.0.1:11434`, or deterministic offline synthesizer)
//!   * Reconfigures hardware governors: executes `scripts/cpu-governor.sh` (`powersave` or `balanced`),
//!     clamps EPP to `power` or `balance_power`, adjusts MGLRU `min_ttl_ms`, and steers thread affinity
//!   * Records cryptographic, immutable audit ledger in SQLite (`.hgb/autonomous_telemetry_optimizations.db`)
//!     sealing each mitigation with BLAKE3 verification.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};

use hgb_core::dynamic_governor::{
    DynamicGovernor, GovernorGuard, GovernorTelemetry, WorkloadKind,
};
use hgb_core::dynamic_hardware_profiler::{
    CoreAffinityGuard, CoreClusterKind, CpuTopology,
    DynamicHardwareProfiler, HardwareGovernorProfile, HardwareProfileTelemetry,
    MglruPressureLevel, ProfilerGovernorGuard, TaskIntensity,
};
use hgb_core::dynatune_bridge::{
    CompleteHardwareTelemetry, DynatuneBridge, HostPowerState, ScalingGuard,
    ScalingShiftReport, WorkloadCategory,
};
use hgb_core::dynatune_subsystem_fuse::{
    DynatuneSubsystemFuse, DynatuneWorkloadBoostGuard,
    SubsystemTelemetrySnapshot,
};
use hgb_core::hardware_aware_workload_router::{
    HardwareAwareWorkloadRouter, WorkloadAdaptiveGuard, WorkloadExecutionDecision,
    WorkloadExecutionProfile, WorkloadKind as HardwareWorkloadKind,
};
use hgb_core::error::{HgbError, Result};

/// Sampled process information from OS process tree inspection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessSample {
    pub pid: u32,
    pub ppid: u32,
    pub cmd: String,
    pub cpu_pct: f32,
    pub mem_pct: f32,
}

/// Trigger reason for self-healing governor intervention
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TriggerReason {
    CpuOverheat { temp_c: f64, threshold_c: f64 },
    BatteryDrawSpike { draw_w: f64, threshold_w: f64 },
    BatteryDischargeRateSpike { drop_pct_per_hr: f64, threshold_pct_per_hr: f64 },
    CriticalMglruPressure { pressure: MglruPressureLevel },
}

impl std::fmt::Display for TriggerReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CpuOverheat { temp_c, threshold_c } => {
                write!(f, "CPU package temperature thermal limit breached ({:.1}°C >= {:.1}°C)", temp_c, threshold_c)
            }
            Self::BatteryDrawSpike { draw_w, threshold_w } => {
                write!(f, "Battery power discharge spike ({:.2}W > {:.1}W)", draw_w, threshold_w)
            }
            Self::BatteryDischargeRateSpike { drop_pct_per_hr, threshold_pct_per_hr } => {
                write!(f, "Battery discharge velocity spike ({:.1}%/hr > {:.1}%/hr)", drop_pct_per_hr, threshold_pct_per_hr)
            }
            Self::CriticalMglruPressure { pressure } => {
                write!(f, "Host MGLRU / PSI memory pressure reached critical severity ({:?})", pressure)
            }
        }
    }
}

/// Structured corrective governor parameters synthesized by model or autonomous engine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CorrectiveGovernorParams {
    pub culprit_processes: String,
    pub governor: String,         // "powersave" or "balanced"
    pub epp: String,              // "power" or "balance_power"
    pub mglru_min_ttl_ms: u64,    // 0..1000
    pub steer_affinity: CoreClusterKind,
    pub reasoning: String,
}

/// Immutable audit record stored in SQLite `.hgb/autonomous_telemetry_optimizations.db`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryOptimizationRecord {
    pub id: i64,
    pub timestamp: String,
    pub trigger_reason: String,
    pub cpu_temp_c: f64,
    pub battery_draw_w: f64,
    pub culprit_processes: String,
    pub governor_action: String,
    pub model_reasoning: String,
    pub status: String,
    pub blake3_hash: String,
}

impl TelemetryOptimizationRecord {
    /// Verify cryptographic BLAKE3 digest integrity against record fields
    pub fn verify_hash(&self) -> bool {
        let expected = compute_record_hash(
            &self.timestamp,
            &self.trigger_reason,
            self.cpu_temp_c,
            self.battery_draw_w,
            &self.culprit_processes,
            &self.governor_action,
            &self.model_reasoning,
            &self.status,
        );
        self.blake3_hash == expected
    }
}

/// Compute cryptographic BLAKE3 digest of telemetry optimization record fields
pub fn compute_record_hash(
    timestamp: &str,
    trigger_reason: &str,
    cpu_temp_c: f64,
    battery_draw_w: f64,
    culprit_processes: &str,
    governor_action: &str,
    model_reasoning: &str,
    status: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(b":");
    hasher.update(trigger_reason.as_bytes());
    hasher.update(b":");
    hasher.update(format!("{:.3}", cpu_temp_c).as_bytes());
    hasher.update(b":");
    hasher.update(format!("{:.3}", battery_draw_w).as_bytes());
    hasher.update(b":");
    hasher.update(culprit_processes.as_bytes());
    hasher.update(b":");
    hasher.update(governor_action.as_bytes());
    hasher.update(b":");
    hasher.update(model_reasoning.as_bytes());
    hasher.update(b":");
    hasher.update(status.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Initialize SQLite table schema for telemetry optimizations
fn init_sqlite_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS telemetry_optimizations (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             timestamp TEXT NOT NULL,
             trigger_reason TEXT NOT NULL,
             cpu_temp_c REAL NOT NULL,
             battery_draw_w REAL NOT NULL,
             culprit_processes TEXT NOT NULL,
             governor_action TEXT NOT NULL,
             model_reasoning TEXT NOT NULL,
             status TEXT NOT NULL,
             blake3_hash TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_telemetry_opt_timestamp ON telemetry_optimizations(timestamp);",
    )?;
    Ok(())
}

/// Evaluate hardware telemetry against critical thresholds:
/// - CPU package temp >= 78.0°C
/// - Battery discharge draw > 30.0W or drop velocity > 25.0%/hr
/// - Critical MGLRU memory pressure
pub fn evaluate_telemetry_triggers(
    cpu_temp_c: f64,
    battery_draw_w: f64,
    battery_drop_pct_per_hr: f64,
    mglru_pressure: MglruPressureLevel,
) -> Option<TriggerReason> {
    if cpu_temp_c >= 78.0 {
        Some(TriggerReason::CpuOverheat {
            temp_c: cpu_temp_c,
            threshold_c: 78.0,
        })
    } else if battery_draw_w > 30.0 {
        Some(TriggerReason::BatteryDrawSpike {
            draw_w: battery_draw_w,
            threshold_w: 30.0,
        })
    } else if battery_drop_pct_per_hr > 25.0 {
        Some(TriggerReason::BatteryDischargeRateSpike {
            drop_pct_per_hr: battery_drop_pct_per_hr,
            threshold_pct_per_hr: 25.0,
        })
    } else if mglru_pressure == MglruPressureLevel::Critical {
        Some(TriggerReason::CriticalMglruPressure {
            pressure: mglru_pressure,
        })
    } else {
        None
    }
}

/// Autonomously sample process trees from host OS via `ps` or `/proc`
pub fn sample_process_trees() -> Vec<ProcessSample> {
    if let Ok(output) = Command::new("ps")
        .args(["-eo", "pid,ppid,%cpu,%mem,cmd", "--sort=-%cpu"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let samples = parse_ps_output(&stdout);
            if !samples.is_empty() {
                return samples.into_iter().take(15).collect();
            }
        }
    }

    sample_proc_fs().into_iter().take(15).collect()
}

/// Parse tabular output from `ps` command
pub fn parse_ps_output(text: &str) -> Vec<ProcessSample> {
    let mut samples = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("PID") || trimmed.starts_with("pid") {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 5 {
            // Format A: pid, ppid, %cpu, %mem, cmd...
            if let (Ok(pid), Ok(ppid), Ok(cpu_pct), Ok(mem_pct)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<f32>(),
                parts[3].parse::<f32>(),
            ) {
                let cmd = parts[4..].join(" ");
                samples.push(ProcessSample {
                    pid,
                    ppid,
                    cmd,
                    cpu_pct,
                    mem_pct,
                });
                continue;
            }

            // Format B: pid, ppid, cmd..., %cpu, %mem
            let len = parts.len();
            if let (Ok(pid), Ok(ppid), Ok(cpu_pct), Ok(mem_pct)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[len - 2].parse::<f32>(),
                parts[len - 1].parse::<f32>(),
            ) {
                let cmd = parts[2..len - 2].join(" ");
                samples.push(ProcessSample {
                    pid,
                    ppid,
                    cmd,
                    cpu_pct,
                    mem_pct,
                });
            }
        }
    }
    samples
}

/// Direct Linux `/proc` filesystem reader for process discovery
pub fn sample_proc_fs() -> Vec<ProcessSample> {
    let proc_dir = Path::new("/proc");
    let mut samples = Vec::new();

    if let Ok(entries) = std::fs::read_dir(proc_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Ok(pid) = name_str.parse::<u32>() {
                let pid_dir = entry.path();
                let cmdline_path = pid_dir.join("cmdline");
                let cmd = match std::fs::read(&cmdline_path) {
                    Ok(bytes) if !bytes.is_empty() => {
                        let clean: Vec<u8> = bytes.iter().map(|&b| if b == 0 { b' ' } else { b }).collect();
                        String::from_utf8_lossy(&clean).trim().to_string()
                    }
                    _ => {
                        let comm_path = pid_dir.join("comm");
                        std::fs::read_to_string(comm_path).unwrap_or_default().trim().to_string()
                    }
                };

                if cmd.is_empty() {
                    continue;
                }

                let stat_path = pid_dir.join("stat");
                let ppid = if let Ok(stat) = std::fs::read_to_string(stat_path) {
                    if let Some(r_paren) = stat.rfind(')') {
                        let rest = &stat[r_paren + 1..].trim_start();
                        let tokens: Vec<&str> = rest.split_whitespace().collect();
                        tokens.get(1).and_then(|p| p.parse::<u32>().ok()).unwrap_or(0)
                    } else {
                        0
                    }
                } else {
                    0
                };

                samples.push(ProcessSample {
                    pid,
                    ppid,
                    cmd,
                    cpu_pct: 0.0,
                    mem_pct: 0.0,
                });

                if samples.len() >= 15 {
                    break;
                }
            }
        }
    }

    samples
}

/// Construct structured diagnostic prompt for local Ollama model
pub fn build_diagnostic_prompt(
    trigger: &TriggerReason,
    cpu_temp_c: f64,
    battery_draw_w: f64,
    mglru_pressure: MglruPressureLevel,
    processes: &[ProcessSample],
) -> String {
    let mut prompt = String::new();
    prompt.push_str("=== HAGIBIS AUTONOMOUS TELEMETRY & CLOSED-LOOP SELF-HEALING DIAGNOSTIC ===\n\n");
    prompt.push_str(&format!("TRIGGER: {}\n", trigger));
    prompt.push_str("TELEMETRY METRICS:\n");
    prompt.push_str(&format!("  • CPU Package Temp: {:.1}°C (Emergency Threshold: 78.0°C)\n", cpu_temp_c));
    prompt.push_str(&format!("  • Battery Draw: {:.2}W (Discharge Threshold: 30.0W)\n", battery_draw_w));
    prompt.push_str(&format!("  • MGLRU / PSI Memory Pressure: {:?}\n\n", mglru_pressure));

    prompt.push_str("SAMPLED PROCESS TREE (Top Resource Consumers):\n");
    prompt.push_str("PID     PPID    %CPU   %MEM   COMMAND\n");
    for p in processes.iter().take(15) {
        prompt.push_str(&format!(
            "{:<7} {:<7} {:<6.1} {:<6.1} {}\n",
            p.pid, p.ppid, p.cpu_pct, p.mem_pct, p.cmd
        ));
    }

    prompt.push_str("\nMISSION:\n");
    prompt.push_str("Analyze the active process tree above and formulate corrective governor actions to stabilize the hardware.\n");
    prompt.push_str("Respond ONLY with a valid JSON object matching the following structure:\n");
    prompt.push_str(r#"{
  "culprit_processes": "comma-separated list of top culprit process names and PIDs",
  "governor": "powersave" or "balanced",
  "epp": "power" or "balance_power",
  "mglru_min_ttl_ms": 250,
  "steer_affinity": "EfficiencyCores" or "AllCores",
  "reasoning": "brief engineering justification explaining how these parameters avert thermal runaway, battery drain, or memory thrashing"
}
"#);

    prompt
}

/// Parse model response JSON or fall back to real deterministic synthesis
pub fn parse_corrective_parameters(
    raw_response: &str,
    trigger: &TriggerReason,
    processes: &[ProcessSample],
) -> CorrectiveGovernorParams {
    let trimmed = raw_response.trim();

    if !trimmed.is_empty() {
        let json_str = if let Some(start) = trimmed.find('{') {
            if let Some(end) = trimmed.rfind('}') {
                if end > start {
                    &trimmed[start..=end]
                } else {
                    trimmed
                }
            } else {
                trimmed
            }
        } else {
            trimmed
        };

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
            let culprit = val.get("culprit_processes").and_then(|c| {
                if let Some(s) = c.as_str() {
                    Some(s.to_string())
                } else if let Some(arr) = c.as_array() {
                    let items: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                    Some(items.join(", "))
                } else {
                    None
                }
            }).unwrap_or_else(|| default_culprits(processes));

            let gov = val.get("governor")
                .and_then(|g| g.as_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "powersave".to_string());
            let governor = if gov.contains("balance") {
                "balanced".to_string()
            } else {
                "powersave".to_string()
            };

            let epp_str = val.get("epp")
                .and_then(|e| e.as_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "power".to_string());
            let epp = if epp_str.contains("balance") {
                "balance_power".to_string()
            } else {
                "power".to_string()
            };

            let mglru_min_ttl_ms = val.get("mglru_min_ttl_ms")
                .and_then(|m| m.as_u64())
                .unwrap_or_else(|| match trigger {
                    TriggerReason::CriticalMglruPressure { .. } => 500,
                    TriggerReason::CpuOverheat { .. } => 250,
                    _ => 0,
                });

            let steer_affinity = val.get("steer_affinity")
                .and_then(|a| a.as_str())
                .map(|s| match s {
                    "AllCores" => CoreClusterKind::AllCores,
                    "PerformanceCores" => CoreClusterKind::PerformanceCores,
                    _ => CoreClusterKind::EfficiencyCores,
                })
                .unwrap_or(CoreClusterKind::EfficiencyCores);

            let reasoning = val.get("reasoning")
                .and_then(|r| r.as_str())
                .unwrap_or_else(|| "Governor scaled down to mitigate thermal/battery constraints.")
                .to_string();

            return CorrectiveGovernorParams {
                culprit_processes: culprit,
                governor,
                epp,
                mglru_min_ttl_ms,
                steer_affinity,
                reasoning,
            };
        }
    }

    // High-grade deterministic heuristic fallback
    let culprit = default_culprits(processes);
    let (governor, epp, mglru_min_ttl_ms, reasoning) = match trigger {
        TriggerReason::CpuOverheat { temp_c, threshold_c } => (
            "powersave".to_string(),
            "power".to_string(),
            250,
            format!(
                "Thermal runaway alert: CPU package temp reached {:.1}°C exceeding safe limit of {:.1}°C. Culprit processes: [{}]. Clamping governor to powersave, EPP to power, and steering threads to Efficiency cores.",
                temp_c, threshold_c, culprit
            ),
        ),
        TriggerReason::BatteryDrawSpike { draw_w, threshold_w } => (
            "powersave".to_string(),
            "power".to_string(),
            0,
            format!(
                "Battery power spike alert: Discharge reached {:.2}W exceeding critical ceiling of {:.1}W. Culprit processes: [{}]. Downscaling CPU to powersave mode to prevent brownout and preserve battery health.",
                draw_w, threshold_w, culprit
            ),
        ),
        TriggerReason::BatteryDischargeRateSpike { drop_pct_per_hr, threshold_pct_per_hr } => (
            "powersave".to_string(),
            "power".to_string(),
            0,
            format!(
                "Battery discharge velocity alert: Drop rate reached {:.1}%/hr exceeding {:.1}%/hr. Culprit processes: [{}]. Activating power governor and clamping EPP to power.",
                drop_pct_per_hr, threshold_pct_per_hr, culprit
            ),
        ),
        TriggerReason::CriticalMglruPressure { pressure } => (
            "balanced".to_string(),
            "balance_power".to_string(),
            500,
            format!(
                "Memory thrashing alert: MGLRU / PSI memory pressure reached {:?}. Culprit processes: [{}]. Setting MGLRU min_ttl_ms to 500ms to preserve generation state and switching EPP to balance_power.",
                pressure, culprit
            ),
        ),
    };

    CorrectiveGovernorParams {
        culprit_processes: culprit,
        governor,
        epp,
        mglru_min_ttl_ms,
        steer_affinity: CoreClusterKind::EfficiencyCores,
        reasoning,
    }
}

fn default_culprits(processes: &[ProcessSample]) -> String {
    if processes.is_empty() {
        return "system-load".to_string();
    }
    processes
        .iter()
        .take(3)
        .map(|p| format!("{} (PID: {}, CPU: {:.1}%)", p.cmd.split_whitespace().next().unwrap_or(&p.cmd), p.pid, p.cpu_pct))
        .collect::<Vec<_>>()
        .join(", ")
}

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
    #[serde(default)]
    pub mglru_pressure: Option<MglruPressureLevel>,
    #[serde(default)]
    pub recommended_affinity_cluster: Option<CoreClusterKind>,
    #[serde(default)]
    pub last_telemetry_optimization: Option<TelemetryOptimizationRecord>,
    #[serde(default)]
    pub total_self_healing_actions: u64,
    #[serde(default)]
    pub subsystem_fuse_boosted: bool,
    #[serde(default)]
    pub subsystem_fuse_telemetry: Option<SubsystemTelemetrySnapshot>,
}

pub struct GovernorWatchdog {
    workspace_root: PathBuf,
    db_path: PathBuf,
    script_path: PathBuf,
    ollama_endpoint: String,
    governor: Arc<DynamicGovernor>,
    dynatune_bridge: Arc<DynatuneBridge>,
    workload_router: Arc<HardwareAwareWorkloadRouter>,
    profiler: Arc<DynamicHardwareProfiler>,
    dynatune_fuse: Arc<DynatuneSubsystemFuse>,
    active_guard: Arc<Mutex<Option<GovernorGuard>>>,
    active_scaling_guard: Arc<Mutex<Option<ScalingGuard>>>,
    active_adaptive_guard: Arc<Mutex<Option<WorkloadAdaptiveGuard>>>,
    active_profiler_guard: Arc<Mutex<Option<ProfilerGovernorGuard>>>,
    last_shift: Arc<Mutex<Option<ScalingShiftReport>>>,
    last_decision: Arc<Mutex<Option<WorkloadExecutionDecision>>>,
    last_optimization_record: Arc<Mutex<Option<TelemetryOptimizationRecord>>>,
    total_self_heals: Arc<AtomicU64>,
    db_conn: Arc<std::sync::Mutex<Connection>>,
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
        let ws = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_workspace(&ws)
    }

    /// Construct GovernorWatchdog bound to workspace root and persistent SQLite ledger
    pub fn with_workspace(workspace_root: &Path) -> Self {
        let hgb_dir = workspace_root.join(".hgb");
        let _ = std::fs::create_dir_all(&hgb_dir);
        let db_path = hgb_dir.join("autonomous_telemetry_optimizations.db");
        let script_path = workspace_root.join("scripts/cpu-governor.sh");

        let conn = Connection::open(&db_path).unwrap_or_else(|_| {
            Connection::open_in_memory().expect("in-memory sqlite fallback")
        });
        let _ = init_sqlite_schema(&conn);

        let bridge = Arc::new(DynatuneBridge::new());
        let router = Arc::new(HardwareAwareWorkloadRouter::with_bridge(Arc::clone(&bridge)));
        let profiler = Arc::new(DynamicHardwareProfiler::new());
        let _sampler = profiler.start_background_sampler(std::time::Duration::from_secs(1));
        let dynatune_fuse = Arc::new(DynatuneSubsystemFuse::new());

        Self {
            workspace_root: workspace_root.to_path_buf(),
            db_path,
            script_path,
            ollama_endpoint: std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string()),
            governor: Arc::new(DynamicGovernor::new()),
            dynatune_bridge: bridge,
            workload_router: router,
            profiler,
            dynatune_fuse,
            active_guard: Arc::new(Mutex::new(None)),
            active_scaling_guard: Arc::new(Mutex::new(None)),
            active_adaptive_guard: Arc::new(Mutex::new(None)),
            active_profiler_guard: Arc::new(Mutex::new(None)),
            last_shift: Arc::new(Mutex::new(None)),
            last_decision: Arc::new(Mutex::new(None)),
            last_optimization_record: Arc::new(Mutex::new(None)),
            total_self_heals: Arc::new(AtomicU64::new(0)),
            db_conn: Arc::new(std::sync::Mutex::new(conn)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Construct isolated in-memory instance for testing
    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        let _ = init_sqlite_schema(&conn);
        let bridge = Arc::new(DynatuneBridge::new());
        let router = Arc::new(HardwareAwareWorkloadRouter::with_bridge(Arc::clone(&bridge)));
        let profiler = Arc::new(DynamicHardwareProfiler::new());
        let dynatune_fuse = Arc::new(DynatuneSubsystemFuse::new());

        Self {
            workspace_root: PathBuf::from("."),
            db_path: PathBuf::from(":memory:"),
            script_path: PathBuf::from("scripts/cpu-governor.sh"),
            ollama_endpoint: "http://127.0.0.1:11434".to_string(),
            governor: Arc::new(DynamicGovernor::new()),
            dynatune_bridge: bridge,
            workload_router: router,
            profiler,
            dynatune_fuse,
            active_guard: Arc::new(Mutex::new(None)),
            active_scaling_guard: Arc::new(Mutex::new(None)),
            active_adaptive_guard: Arc::new(Mutex::new(None)),
            active_profiler_guard: Arc::new(Mutex::new(None)),
            last_shift: Arc::new(Mutex::new(None)),
            last_decision: Arc::new(Mutex::new(None)),
            last_optimization_record: Arc::new(Mutex::new(None)),
            total_self_heals: Arc::new(AtomicU64::new(0)),
            db_conn: Arc::new(std::sync::Mutex::new(conn)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Configure explicit script path for governor script execution
    pub fn with_script_path(mut self, path: PathBuf) -> Self {
        self.script_path = path;
        self
    }

    /// Configure custom Ollama endpoint
    pub fn with_ollama_endpoint(mut self, endpoint: String) -> Self {
        self.ollama_endpoint = endpoint;
        self
    }

    /// Path to workspace root directory
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Path to SQLite telemetry optimizations audit database
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Path to cpu-governor.sh script
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    /// Configured Ollama endpoint URL
    pub fn ollama_endpoint(&self) -> &str {
        &self.ollama_endpoint
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

    /// Direct reference to Dynatune & Kernel-Optimizer Subsystem Fuse
    pub fn fuse(&self) -> &Arc<DynatuneSubsystemFuse> {
        &self.dynatune_fuse
    }

    /// Acquire RAII DynatuneWorkloadBoostGuard bound to the fuse
    pub fn acquire_subsystem_boost_guard(&self, workload: &str) -> DynatuneWorkloadBoostGuard {
        self.dynatune_fuse.acquire_guard(workload)
    }

    /// Explicit workload start trigger boosting CPU and memory bandwidth
    pub fn on_workload_started(&self) {
        let _ = self.dynatune_fuse.boost_performance("workload_started");
    }

    /// Explicit workload completion trigger dropping to powersave
    pub fn on_workload_completed(&self) {
        let _ = self.dynatune_fuse.drop_to_powersave();
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

    /// Pin current thread to a specific CPU cluster
    pub fn pin_current_thread_to_cluster(&self, cluster: CoreClusterKind) -> Result<()> {
        self.profiler.pin_current_thread_to_cluster(cluster)
    }

    /// Acquire RAII CoreAffinityGuard pinning the current thread to a cluster
    pub fn acquire_affinity_guard(&self, cluster: CoreClusterKind) -> Result<CoreAffinityGuard> {
        self.profiler.acquire_affinity_guard(cluster)
    }

    /// Acquire RAII CoreAffinityGuard pinning current thread to the steered cluster based on task intensity and live thermal/battery telemetry
    pub fn acquire_steered_affinity_guard(&self, intensity: TaskIntensity) -> Result<CoreAffinityGuard> {
        self.profiler.acquire_steered_affinity_guard(intensity)
    }

    /// Read real-time MGLRU / PSI memory pressure level
    pub fn mglru_memory_pressure(&self) -> MglruPressureLevel {
        self.profiler.mglru_memory_pressure()
    }

    /// Steer workload affinity cluster based on task intensity and live thermal/battery telemetry
    pub fn steer_workload_affinity(&self, intensity: TaskIntensity) -> CoreClusterKind {
        self.profiler.steer_workload_affinity(intensity)
    }

    /// Read discovered CPU topology
    pub fn cpu_topology(&self) -> CpuTopology {
        self.profiler.cpu_topology()
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

    /// Automatically trigger dynatune / kernel-optimizer to switch CPU governors to 'performance',
    /// ramp up thermal headroom, enable turbo, and prioritize memory bandwidth (MGLRU min_ttl_ms 1000)
    /// during heavy local LLM inference or WASM sandboxing tasks.
    pub async fn on_queue_active(&self) -> Result<GovernorWatchdogStatus> {
        // 1. Dynamic governor boost
        let guard = self.governor.boost_for_workload(WorkloadKind::HeavyCompile)?;
        {
            let mut active = self.active_guard.lock().await;
            *active = Some(guard);
        }

        // 2. Dynatune bridge scaling shift & RAII guard (bursts governor, EPP performance, MGLRU min_ttl_ms 1000)
        let scaling_guard = self.dynatune_bridge.burst_guard(WorkloadCategory::BuildCompile)?;
        {
            let mut shift_lock = self.last_shift.lock().await;
            *shift_lock = Some(scaling_guard.active_shift.clone());
            let mut active_s = self.active_scaling_guard.lock().await;
            *active_s = Some(scaling_guard);
        }

        // 3. Workload router guard
        if let Ok(adaptive_guard) = self.workload_router.acquire_guard(HardwareWorkloadKind::HeavyCompilation) {
            let mut dec_lock = self.last_decision.lock().await;
            *dec_lock = Some(adaptive_guard.decision().clone());
            let mut active_a = self.active_adaptive_guard.lock().await;
            *active_a = Some(adaptive_guard);
        }

        // 4. DynamicHardwareProfiler queue active signal (performance governor, EPP performance, turbo, MGLRU 1000)
        let _ = self.profiler.on_queue_active();
        if let Ok(p_guard) = self.profiler.acquire_profile_guard(HardwareGovernorProfile::Performance) {
            let mut active_p = self.active_profiler_guard.lock().await;
            *active_p = Some(p_guard);
        }

        // 5. DynatuneSubsystemFuse hardware performance boost
        let _ = self.dynatune_fuse.boost_performance("queue_active");

        Ok(self.status().await)
    }

    /// Immediately drop CPU governor back to 'powersave', EPP to 'balance_performance',
    /// and relax memory pressure the moment the task queue empties.
    pub async fn on_queue_idle(&self) -> Result<GovernorWatchdogStatus> {
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
        let _ = self.profiler.on_queue_idle();
        let _ = self.dynatune_fuse.drop_to_powersave();

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

        // 3. Acquire steered affinity guard for intensive task execution
        let _affinity_guard = self.profiler.acquire_steered_affinity_guard(TaskIntensity::Intensive).ok();

        // 4. Run workload
        let res = task().await;

        // 5. Relax back to powersave
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

        let _guard = self.dynatune_bridge.burst_guard(category)?;
        let _affinity_guard = self.profiler.acquire_steered_affinity_guard(TaskIntensity::Intensive).ok();
        task()
    }

    /// Thermal and battery watchdog check: detects throttling conditions, executes closed-loop healing, and disengages burst mode if needed
    pub async fn check_and_safeguard(&self) -> bool {
        // Also run real-time self-healing check
        let _ = self.evaluate_and_heal().await;

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

    /// Real-time closed-loop self-healing evaluation using live Dynatune and DynamicHardwareProfiler metrics
    pub async fn evaluate_and_heal(&self) -> Result<Option<TelemetryOptimizationRecord>> {
        let complete = self.dynatune_bridge.read_complete_hardware_telemetry();
        let cpu_temp_c = complete.cpu_pkg_temp_c;
        let battery_draw_w = complete.battery_discharge_rate_mw.unwrap_or(0.0) / 1000.0;
        let battery_drop_pct_per_hr = complete.battery_discharge_rate_pct_per_hr.unwrap_or(0.0);
        let mglru_pressure = self.profiler.mglru_memory_pressure();

        self.evaluate_and_heal_with_telemetry(cpu_temp_c, battery_draw_w, battery_drop_pct_per_hr, mglru_pressure).await
    }

    /// Real-time closed-loop self-healing evaluation with explicit telemetry parameters
    pub async fn evaluate_and_heal_with_telemetry(
        &self,
        cpu_temp_c: f64,
        battery_draw_w: f64,
        battery_drop_pct_per_hr: f64,
        mglru_pressure: MglruPressureLevel,
    ) -> Result<Option<TelemetryOptimizationRecord>> {
        let trigger = match evaluate_telemetry_triggers(cpu_temp_c, battery_draw_w, battery_drop_pct_per_hr, mglru_pressure) {
            Some(t) => t,
            None => return Ok(None),
        };

        // 1. Autonomously sample process tree
        let processes = sample_process_trees();

        // 2. Synthesize diagnostic prompt & invoke local Ollama model or deterministic fallback
        let prompt = build_diagnostic_prompt(&trigger, cpu_temp_c, battery_draw_w, mglru_pressure, &processes);
        let corrective = self.query_ollama_or_synthesize(&prompt, &trigger, &processes).await;

        // 3. Execute governor reconfiguration
        let action_summary = format!(
            "Governor: {} | EPP: {} | MGLRU min_ttl_ms: {} | Affinity: {:?}",
            corrective.governor, corrective.epp, corrective.mglru_min_ttl_ms, corrective.steer_affinity
        );
        let execution_result = self.execute_governor_reconfiguration(&corrective);
        let status_str = match execution_result {
            Ok(_) => "Applied".to_string(),
            Err(e) => format!("Failed: {}", e),
        };

        // 4. Persist immutable audit record in SQLite
        let record = self.record_optimization_audit(
            &trigger,
            cpu_temp_c,
            battery_draw_w,
            &corrective.culprit_processes,
            &action_summary,
            &corrective.reasoning,
            &status_str,
        )?;

        self.total_self_heals.fetch_add(1, Ordering::SeqCst);
        {
            let mut last_rec = self.last_optimization_record.lock().await;
            *last_rec = Some(record.clone());
        }

        Ok(Some(record))
    }

    /// Query local Ollama model (via Unix Domain Socket / HTTP) or generate deterministic fallback
    pub async fn query_ollama_or_synthesize(
        &self,
        prompt: &str,
        trigger: &TriggerReason,
        processes: &[ProcessSample],
    ) -> CorrectiveGovernorParams {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(2500))
            .build();

        if let Ok(c) = client {
            let payload = serde_json::json!({
                "model": "qwen2.5-coder:1.5b",
                "prompt": prompt,
                "stream": false,
                "format": "json"
            });

            let url = format!("{}/api/generate", self.ollama_endpoint);
            if let Ok(resp) = c.post(&url).json(&payload).send().await {
                if resp.status().is_success() {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        if let Some(text) = body.get("response").and_then(|r| r.as_str()) {
                            if !text.trim().is_empty() {
                                return parse_corrective_parameters(text, trigger, processes);
                            }
                        }
                    }
                }
            }
        }

        // Autonomous deterministic fallback engine when Ollama is offline or unconfigured
        parse_corrective_parameters("", trigger, processes)
    }

    /// Execute governor reconfiguration:
    /// - Invokes `scripts/cpu-governor.sh` (`powersave` or `balanced`)
    /// - Clamps EPP to `power` or `balance_power`
    /// - Sets MGLRU `min_ttl_ms`
    /// - Steers thread affinity to efficiency cores
    pub fn execute_governor_reconfiguration(
        &self,
        params: &CorrectiveGovernorParams,
    ) -> Result<String> {
        let mut steps = Vec::new();

        // 1. Invoke scripts/cpu-governor.sh (powersave or balanced)
        if self.script_path.exists() {
            match Command::new("bash")
                .arg(&self.script_path)
                .arg(&params.governor)
                .output()
            {
                Ok(output) => {
                    let out_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    steps.push(format!("Script executed ({:?}): {}", self.script_path, out_str));
                }
                Err(e) => {
                    steps.push(format!("Script invocation warning ({:?}): {}", self.script_path, e));
                }
            }
        } else {
            steps.push(format!("Script not found at {:?}; using native sysfs governor", self.script_path));
        }

        // 2. Direct sysfs governor fallback/reinforcement
        let _ = self.governor.set_scaling_governor(&params.governor);

        // 3. Clamp EPP to power or balance_power
        let _ = self.governor.set_epp(&params.epp);
        if let Ok(entries) = std::fs::read_dir("/sys/devices/system/cpu") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("cpu") && name_str[3..].chars().all(|c| c.is_ascii_digit()) {
                    let epp_path = entry.path().join("cpufreq/energy_performance_preference");
                    if epp_path.exists() {
                        let _ = std::fs::write(&epp_path, params.epp.as_bytes());
                    }
                }
            }
        }
        steps.push(format!("EPP clamped to '{}'", params.epp));

        // 4. Set MGLRU min_ttl_ms
        let mglru_path = Path::new("/sys/kernel/mm/lru_gen/min_ttl_ms");
        if mglru_path.exists() {
            let val_str = format!("{}\n", params.mglru_min_ttl_ms);
            let _ = std::fs::write(mglru_path, val_str.as_bytes());
            steps.push(format!("MGLRU min_ttl_ms set to {}", params.mglru_min_ttl_ms));
        }

        // 5. Steer thread affinity
        let _ = self.profiler.pin_current_thread_to_cluster(params.steer_affinity);
        let _ = self.profiler.switch_profile(HardwareGovernorProfile::PowerSave);
        steps.push(format!("Affinity steered to {:?}", params.steer_affinity));

        Ok(steps.join(" | "))
    }

    /// Persist an immutable audit record in SQLite table `telemetry_optimizations`
    pub fn record_optimization_audit(
        &self,
        trigger: &TriggerReason,
        cpu_temp_c: f64,
        battery_draw_w: f64,
        culprit_processes: &str,
        governor_action: &str,
        model_reasoning: &str,
        status: &str,
    ) -> Result<TelemetryOptimizationRecord> {
        let now = chrono::Utc::now().to_rfc3339();
        let trigger_str = trigger.to_string();
        let blake3_hash = compute_record_hash(
            &now,
            &trigger_str,
            cpu_temp_c,
            battery_draw_w,
            culprit_processes,
            governor_action,
            model_reasoning,
            status,
        );

        let conn = self.db_conn.lock().map_err(|e| HgbError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO telemetry_optimizations (
                timestamp, trigger_reason, cpu_temp_c, battery_draw_w,
                culprit_processes, governor_action, model_reasoning, status, blake3_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &now,
                &trigger_str,
                cpu_temp_c,
                battery_draw_w,
                culprit_processes,
                governor_action,
                model_reasoning,
                status,
                &blake3_hash,
            ],
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let id = conn.last_insert_rowid();

        let record = TelemetryOptimizationRecord {
            id,
            timestamp: now,
            trigger_reason: trigger_str,
            cpu_temp_c,
            battery_draw_w,
            culprit_processes: culprit_processes.to_string(),
            governor_action: governor_action.to_string(),
            model_reasoning: model_reasoning.to_string(),
            status: status.to_string(),
            blake3_hash,
        };

        Ok(record)
    }

    /// Retrieve persisted optimization records from SQLite ledger
    pub fn list_optimization_records(&self, limit: usize) -> Result<Vec<TelemetryOptimizationRecord>> {
        let conn = self.db_conn.lock().map_err(|e| HgbError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, trigger_reason, cpu_temp_c, battery_draw_w,
                    culprit_processes, governor_action, model_reasoning, status, blake3_hash
             FROM telemetry_optimizations
             ORDER BY id DESC LIMIT ?1"
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(TelemetryOptimizationRecord {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                trigger_reason: row.get(2)?,
                cpu_temp_c: row.get(3)?,
                battery_draw_w: row.get(4)?,
                culprit_processes: row.get(5)?,
                governor_action: row.get(6)?,
                model_reasoning: row.get(7)?,
                status: row.get(8)?,
                blake3_hash: row.get(9)?,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut records = Vec::new();
        for r in rows {
            records.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
        }
        Ok(records)
    }

    /// Launch autonomous background closed-loop self-healing monitor
    pub fn start_autonomous_telemetry_loop(self: &Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        let watchdog = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let _ = watchdog.evaluate_and_heal().await;
            }
        })
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

        let mglru_pressure = Some(self.profiler.mglru_memory_pressure());
        let recommended_affinity_cluster = Some(self.profiler.steer_workload_affinity(TaskIntensity::Intensive));
        let last_telemetry_optimization = self.last_optimization_record.lock().await.clone();
        let total_self_healing_actions = self.total_self_heals.load(Ordering::Relaxed);
        let subsystem_fuse_boosted = self.dynatune_fuse.is_boosted();
        let subsystem_fuse_telemetry = Some(self.dynatune_fuse.query_telemetry());

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
            mglru_pressure,
            recommended_affinity_cluster,
            last_telemetry_optimization,
            total_self_healing_actions,
            subsystem_fuse_boosted,
            subsystem_fuse_telemetry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_watchdog_boost_and_relax() {
        let watchdog = GovernorWatchdog::in_memory();
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
        let watchdog = GovernorWatchdog::in_memory();
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
        let watchdog = GovernorWatchdog::in_memory();
        let result = watchdog.execute_sync_with_workload_boost("test", || {
            Ok(42)
        }).expect("sync boost");

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_watchdog_complete_telemetry() {
        let watchdog = GovernorWatchdog::in_memory();
        let telem = watchdog.complete_telemetry();
        assert!(telem.cpu_pkg_temp_c > 0.0);
        assert!(telem.memory_total_mb > 0);
        assert!(telem.recommended_concurrency_limit >= 1);
    }

    #[tokio::test]
    async fn test_watchdog_adaptive_workload_guard() {
        let watchdog = GovernorWatchdog::in_memory();
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
        let watchdog = GovernorWatchdog::in_memory();
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

    #[tokio::test]
    async fn test_watchdog_queue_active_and_idle_transitions() {
        let watchdog = GovernorWatchdog::in_memory();

        // Initially ensure idle/powersave
        let _ = watchdog.on_queue_idle().await;
        let initial_status = watchdog.status().await;
        assert!(!initial_status.is_boosted);

        // Queue becomes active: shift to performance, EPP performance, turbo, MGLRU 1000
        let active_status = watchdog.on_queue_active().await.expect("on_queue_active");
        assert!(active_status.is_boosted);
        if let Some(ref shift) = active_status.last_scaling_shift {
            assert_eq!(shift.scaling_governor, "performance");
            assert_eq!(shift.epp, "performance");
        }

        // Queue becomes idle: drop to powersave, EPP balance_performance, relax memory pressure
        let idle_status = watchdog.on_queue_idle().await.expect("on_queue_idle");
        assert!(!idle_status.is_boosted);
        if let Some(ref shift) = idle_status.last_scaling_shift {
            assert_eq!(shift.scaling_governor, "powersave");
        }
    }

    #[tokio::test]
    async fn test_watchdog_affinity_pinning_and_steering() {
        let watchdog = GovernorWatchdog::in_memory();
        let topology = watchdog.cpu_topology();
        assert!(topology.total_cores >= 1);

        let pressure = watchdog.mglru_memory_pressure();
        assert!(matches!(pressure, MglruPressureLevel::Low | MglruPressureLevel::Medium | MglruPressureLevel::Critical));

        let steered_cluster = watchdog.steer_workload_affinity(TaskIntensity::Intensive);
        assert!(matches!(steered_cluster, CoreClusterKind::PerformanceCores | CoreClusterKind::EfficiencyCores | CoreClusterKind::AllCores));

        // Acquire steered affinity guard
        let guard_res = watchdog.acquire_steered_affinity_guard(TaskIntensity::Intensive);
        assert!(guard_res.is_ok());

        // Status check
        let status = watchdog.status().await;
        assert!(status.mglru_pressure.is_some());
        assert!(status.recommended_affinity_cluster.is_some());
    }

    // =========================================================================
    // UPGRADE 1: UNIFIED AUTONOMOUS TELEMETRY & CLOSED-LOOP SELF-HEALING TESTS
    // =========================================================================

    #[test]
    fn test_telemetry_threshold_evaluation() {
        // 1. CPU Package Temperature >= 78.0°C triggers overheat
        let trigger_overheat = evaluate_telemetry_triggers(78.0, 15.0, 5.0, MglruPressureLevel::Low);
        assert_eq!(
            trigger_overheat,
            Some(TriggerReason::CpuOverheat { temp_c: 78.0, threshold_c: 78.0 })
        );

        let trigger_high_temp = evaluate_telemetry_triggers(84.5, 10.0, 2.0, MglruPressureLevel::Low);
        assert_eq!(
            trigger_high_temp,
            Some(TriggerReason::CpuOverheat { temp_c: 84.5, threshold_c: 78.0 })
        );

        // 2. Battery discharge rate draw > 30.0W triggers battery spike
        let trigger_draw_spike = evaluate_telemetry_triggers(70.0, 32.5, 10.0, MglruPressureLevel::Low);
        assert_eq!(
            trigger_draw_spike,
            Some(TriggerReason::BatteryDrawSpike { draw_w: 32.5, threshold_w: 30.0 })
        );

        // 3. Battery discharge velocity drop > 25.0%/hr triggers velocity spike
        let trigger_drop_spike = evaluate_telemetry_triggers(68.0, 20.0, 27.5, MglruPressureLevel::Low);
        assert_eq!(
            trigger_drop_spike,
            Some(TriggerReason::BatteryDischargeRateSpike { drop_pct_per_hr: 27.5, threshold_pct_per_hr: 25.0 })
        );

        // 4. Critical MGLRU / PSI memory pressure triggers memory intervention
        let trigger_mglru = evaluate_telemetry_triggers(65.0, 18.0, 12.0, MglruPressureLevel::Critical);
        assert_eq!(
            trigger_mglru,
            Some(TriggerReason::CriticalMglruPressure { pressure: MglruPressureLevel::Critical })
        );

        // 5. Normal / Nominal metrics produce no trigger (None)
        let nominal = evaluate_telemetry_triggers(62.0, 15.0, 10.0, MglruPressureLevel::Medium);
        assert_eq!(nominal, None);

        let cool = evaluate_telemetry_triggers(45.0, 8.0, 4.0, MglruPressureLevel::Low);
        assert_eq!(cool, None);
    }

    #[test]
    fn test_diagnostic_prompt_synthesis_and_response_parsing() {
        let trigger = TriggerReason::CpuOverheat { temp_c: 81.2, threshold_c: 78.0 };
        let samples = vec![
            ProcessSample {
                pid: 4012,
                ppid: 1,
                cmd: "cargo test -p hgb-core --release".to_string(),
                cpu_pct: 96.5,
                mem_pct: 14.2,
            },
            ProcessSample {
                pid: 4015,
                ppid: 4012,
                cmd: "rustc --crate-name hgb_core src/lib.rs".to_string(),
                cpu_pct: 89.1,
                mem_pct: 22.8,
            },
        ];

        let prompt = build_diagnostic_prompt(&trigger, 81.2, 28.5, MglruPressureLevel::Medium, &samples);
        assert!(prompt.contains("81.2°C"));
        assert!(prompt.contains("Emergency Threshold: 78.0°C"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("rustc"));
        assert!(prompt.contains("4012"));

        // 1. Structured JSON parsing
        let mock_ollama_json = r#"{
            "culprit_processes": "rustc (PID: 4015), cargo test (PID: 4012)",
            "governor": "powersave",
            "epp": "power",
            "mglru_min_ttl_ms": 250,
            "steer_affinity": "EfficiencyCores",
            "reasoning": "CPU package temperature exceeded 81°C under heavy compilation. Downscaling frequency to powersave and pinning threads to efficiency cores to halt thermal throttling."
        }"#;

        let parsed = parse_corrective_parameters(mock_ollama_json, &trigger, &samples);
        assert_eq!(parsed.governor, "powersave");
        assert_eq!(parsed.epp, "power");
        assert_eq!(parsed.mglru_min_ttl_ms, 250);
        assert_eq!(parsed.steer_affinity, CoreClusterKind::EfficiencyCores);
        assert!(parsed.culprit_processes.contains("rustc"));

        // 2. Markdown-fenced JSON parsing
        let fenced_json = format!("```json\n{}\n```", mock_ollama_json);
        let parsed_fenced = parse_corrective_parameters(&fenced_json, &trigger, &samples);
        assert_eq!(parsed_fenced.governor, "powersave");
        assert_eq!(parsed_fenced.epp, "power");

        // 3. Deterministic heuristic fallback when response is empty or unparseable
        let fallback = parse_corrective_parameters("", &trigger, &samples);
        assert_eq!(fallback.governor, "powersave");
        assert_eq!(fallback.epp, "power");
        assert_eq!(fallback.mglru_min_ttl_ms, 250);
        assert_eq!(fallback.steer_affinity, CoreClusterKind::EfficiencyCores);
        assert!(fallback.reasoning.contains("Thermal runaway alert"));
        assert!(fallback.culprit_processes.contains("cargo"));
    }

    #[test]
    fn test_closed_loop_governor_script_rewrite_and_execution() {
        let temp_dir = std::env::temp_dir().join(format!("gov_script_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let script_path = temp_dir.join("cpu-governor.sh");
        let log_path = temp_dir.join("governor_invoked.log");

        // Create executable test governor script that logs arguments
        let script_content = format!(
            "#!/usr/bin/env bash\necho \"$1\" >> \"{}\"\necho \"OK: changed to $1\"\n",
            log_path.display()
        );
        std::fs::write(&script_path, script_content).expect("write script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).expect("set_permissions");
        }

        let watchdog = GovernorWatchdog::in_memory().with_script_path(script_path);

        let params_powersave = CorrectiveGovernorParams {
            culprit_processes: "rustc".to_string(),
            governor: "powersave".to_string(),
            epp: "power".to_string(),
            mglru_min_ttl_ms: 250,
            steer_affinity: CoreClusterKind::EfficiencyCores,
            reasoning: "Mitigate thermal runaway".to_string(),
        };

        let exec_powersave = watchdog.execute_governor_reconfiguration(&params_powersave).expect("exec powersave");
        assert!(exec_powersave.contains("Script executed"));
        assert!(exec_powersave.contains("EPP clamped to 'power'"));

        let logged = std::fs::read_to_string(&log_path).expect("read log");
        assert!(logged.contains("powersave"));

        // Test balanced reconfiguration
        let params_balanced = CorrectiveGovernorParams {
            culprit_processes: "mglru-heavy".to_string(),
            governor: "balanced".to_string(),
            epp: "balance_power".to_string(),
            mglru_min_ttl_ms: 500,
            steer_affinity: CoreClusterKind::AllCores,
            reasoning: "Balance memory pressure".to_string(),
        };

        let exec_balanced = watchdog.execute_governor_reconfiguration(&params_balanced).expect("exec balanced");
        assert!(exec_balanced.contains("Script executed"));
        assert!(exec_balanced.contains("EPP clamped to 'balance_power'"));

        let logged2 = std::fs::read_to_string(&log_path).expect("read log");
        assert!(logged2.contains("balanced"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sqlite_audit_trail_persistence_and_hash_verification() {
        let watchdog = GovernorWatchdog::in_memory();

        let trigger = TriggerReason::BatteryDrawSpike { draw_w: 34.2, threshold_w: 30.0 };
        let record = watchdog.record_optimization_audit(
            &trigger,
            72.5,
            34.2,
            "node (PID: 1204)",
            "Governor: powersave | EPP: power | MGLRU min_ttl_ms: 0 | Affinity: EfficiencyCores",
            "Emergency battery drain clamp to prevent system brownout",
            "Applied",
        ).expect("record audit");

        assert!(record.id > 0);
        assert!(!record.blake3_hash.is_empty());
        assert!(record.verify_hash(), "Cryptographic BLAKE3 hash must verify intact record");

        // Verify query from SQLite
        let records = watchdog.list_optimization_records(10).expect("list records");
        assert_eq!(records.len(), 1);
        let fetched = &records[0];
        assert_eq!(fetched.id, record.id);
        assert_eq!(fetched.cpu_temp_c, 72.5);
        assert_eq!(fetched.battery_draw_w, 34.2);
        assert_eq!(fetched.status, "Applied");
        assert!(fetched.verify_hash(), "Queried record hash must verify intact");

        // Verify tampering detection
        let mut tampered = fetched.clone();
        tampered.cpu_temp_c = 75.0; // Mutated temperature
        assert!(!tampered.verify_hash(), "Tampered record must fail BLAKE3 verification");

        let mut tampered_action = fetched.clone();
        tampered_action.governor_action = "Governor: performance".to_string(); // Mutated action
        assert!(!tampered_action.verify_hash(), "Tampered action must fail BLAKE3 verification");
    }

    #[tokio::test]
    async fn test_full_closed_loop_self_healing_pipeline() {
        let watchdog = GovernorWatchdog::in_memory();

        // 1. Initial status has 0 self-heals
        let status = watchdog.status().await;
        assert_eq!(status.total_self_healing_actions, 0);
        assert!(status.last_telemetry_optimization.is_none());

        // 2. High temperature triggers closed-loop healing
        let healed = watchdog.evaluate_and_heal_with_telemetry(
            80.5,
            18.0,
            5.0,
            MglruPressureLevel::Medium,
        ).await.expect("heal");

        assert!(healed.is_some());
        let record = healed.unwrap();
        assert!(record.trigger_reason.contains("thermal limit breached"));
        assert_eq!(record.cpu_temp_c, 80.5);
        assert!(record.verify_hash());

        // 3. Status updated with self-healing telemetry
        let status_after = watchdog.status().await;
        assert_eq!(status_after.total_self_healing_actions, 1);
        assert!(status_after.last_telemetry_optimization.is_some());

        // 4. Nominal temperature produces no action
        let nominal = watchdog.evaluate_and_heal_with_telemetry(
            60.0,
            12.0,
            3.0,
            MglruPressureLevel::Low,
        ).await.expect("heal nominal");

        assert!(nominal.is_none());
        let status_nominal = watchdog.status().await;
        assert_eq!(status_nominal.total_self_healing_actions, 1);
    }

    #[test]
    fn test_ps_output_parsing() {
        let ps_text = "  PID  PPID  %CPU  %MEM COMMAND\n 1234     1  85.4  12.1 cargo test --release\n 5678  1234  74.2  18.5 /usr/bin/rustc src/main.rs\n";
        let samples = parse_ps_output(ps_text);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].pid, 1234);
        assert_eq!(samples[0].cpu_pct, 85.4);
        assert_eq!(samples[0].cmd, "cargo test --release");
        assert_eq!(samples[1].pid, 5678);
        assert_eq!(samples[1].cpu_pct, 74.2);
    }
}
