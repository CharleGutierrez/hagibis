//! # Pillar II: Dynamic Workload Shedding & Hardware-Software Co-Design Engine
//!
//! Subsystem providing deep hardware-software synergy between Hagibis daemon (`hgbd`),
//! bare-metal Linux sysfs kernel governors, and local sibling systems:
//! - `/home/toshiba/My AI Project/dynatune` (`power.rs`, `thermal.rs`, `battery.rs`, `memory.rs`)
//! - `/home/toshiba/My AI Project/kernel-optimizer` (`dynabook-kernel-ctl`)
//!
//! Architecture:
//! 1. Heavy Workload Tracking:
//!    - Actively tracks compute-intensive operations:
//!      * PGO builds (`scripts/pgo-optimize.sh`, `make pgo`, `make pgo-dynatune`)
//!      * Large batch vector embedding calculations / episodic indexing
//!      * Multi-agent mesh loops & swarm coordination
//!      * Heavy release/cargo compilations
//! 2. Workload Initiation & Performance Profile Boost:
//!    - On start of heavy workload, automatically escalates hardware performance:
//!      * Dispatches `scripts/cpu-governor.sh performance` or `dynabook-kernel-ctl profile performance`
//!      * Sets CPU scaling governor to `performance`
//!      * Sets Energy Performance Preference (EPP) to `performance`
//!      * Enables Intel turbo boost (`no_turbo = 0`)
//!      * Tunes Linux Multi-Gen LRU (MGLRU) `min_ttl_ms 1000` to prioritize working-set page caching
//!      * Restores full baseline worker concurrency
//! 3. Peak Thermal Load Mitigation (Dynamic Workload Shedding):
//!    - Continuously evaluates hardware telemetry:
//!      * CPU package temperature >= 78.0°C
//!      * Battery discharging critically (< 20% capacity) or high discharge spike (> 30W)
//!      * Linux MGLRU / PSI memory pressure critical (`avg10 >= 35.0` or RAM available < 6%)
//!    - When triggered, automatically executes Dynamic Workload Shedding:
//!      * Clamps CPU governor to `powersave` and EPP to `power`
//!      * Disables Intel turbo boost (`no_turbo = 1`)
//!      * Resets MGLRU `min_ttl_ms` to 0
//!      * Scales down active worker concurrency from baseline N to 1-2 workers
//!      * Pauses or sheds low-priority background jobs (e.g. background indexing, telemetry summarization, AST sweeps)
//!      * Dispatches `scripts/cpu-governor.sh powersave` or `dynabook-kernel-ctl profile battery`
//! 4. Cool-down Recovery:
//!    - When CPU temperature cools below 70.0°C and battery is stable (>= 25% or on AC mains):
//!      * Restores full worker concurrency back to baseline
//!      * If heavy workloads persist, re-escalates governor to `performance`, EPP to `performance`, and MGLRU to 1000
//!      * If no heavy workloads persist, settles safely at balanced/powersave
//!      * Automatically resumes deferred / paused background jobs

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use hgb_core::error::{HgbError, Result};

/// CPU temperature threshold where peak thermal shedding triggers (78.0°C)
pub const THERMAL_PEAK_THRESHOLD_C: f64 = 78.0;

/// CPU temperature threshold where cool-down recovery engages (70.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 70.0;

/// Critical battery capacity percentage threshold on discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;

/// Alias for backward compatibility
pub const BATTERY_CRITICAL_THRESHOLD_PCT: u32 = BATTERY_CRITICAL_DISCHARGE_PCT;

/// Safe battery recovery threshold on discharge (25%)
pub const BATTERY_RECOVERY_PCT: u32 = 25;

/// High instantaneous battery discharge draw threshold (30.0 Watts)
pub const BATTERY_SPIKE_DRAW_W: f64 = 30.0;

/// Critical Linux PSI memory pressure threshold (35.0% stall over 10s)
pub const MEMORY_PRESSURE_CRITICAL_PSI_AVG10: f64 = 35.0;

/// Critical available RAM percentage threshold (6.0%)
pub const MEMORY_AVAILABLE_CRITICAL_PCT: f64 = 6.0;

/// Target MGLRU min_ttl_ms when bursting during heavy workloads (1000 ms)
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;

/// Baseline MGLRU min_ttl_ms when idle or throttled (0 ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Throttled worker concurrency target during peak thermal load (1-2 workers)
pub const THROTTLED_CONCURRENCY_WORKERS: usize = 2;

/// Classification of heavy compute workloads
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeavyWorkloadKind {
    /// Profile-Guided Optimization builds (`scripts/pgo-optimize.sh`, `make pgo`, `make pgo-dynatune`)
    PgoBuild,
    /// Batch vector embeddings & episodic memory index construction
    BatchVectorEmbeddings,
    /// Multi-agent distributed loop execution & swarm coordination
    MultiAgentLoop,
    /// Heavy compiler invocations (cargo build --release, zig build)
    HeavyCompilation,
    /// Custom intensive compute task
    Custom,
}

impl std::fmt::Display for HeavyWorkloadKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PgoBuild => write!(f, "pgo_build"),
            Self::BatchVectorEmbeddings => write!(f, "batch_vector_embeddings"),
            Self::MultiAgentLoop => write!(f, "multi_agent_loop"),
            Self::HeavyCompilation => write!(f, "heavy_compilation"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

/// Classification of low-priority background jobs subject to shedding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobKind {
    /// Background repo semantic indexing & vector ingestion
    BackgroundIndexing,
    /// Telemetry aggregation and periodic metrics summarization
    TelemetrySummarization,
    /// Periodic AST sweeps & repo-map re-indexing
    AstSweep,
    /// Local cache compaction & DB defragmentation
    CacheCompaction,
    /// Custom background maintenance job
    Custom,
}

impl std::fmt::Display for BackgroundJobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BackgroundIndexing => write!(f, "background_indexing"),
            Self::TelemetrySummarization => write!(f, "telemetry_summarization"),
            Self::AstSweep => write!(f, "ast_sweep"),
            Self::CacheCompaction => write!(f, "cache_compaction"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

/// Operational state of a low-priority background job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobState {
    Active,
    Paused,
    Shed,
}

/// Global operating state of the Dynamic Workload Shedder
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShedderState {
    /// Normal operation at baseline concurrency and balanced/powersave governor
    Nominal,
    /// Performance mode actively engaged due to registered heavy workloads
    PerformanceBoosted,
    /// Dynamic workload shedding engaged due to peak thermal, battery, or memory pressure
    ThermalThrottled,
}

impl std::fmt::Display for ShedderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nominal => write!(f, "nominal"),
            Self::PerformanceBoosted => write!(f, "performance_boosted"),
            Self::ThermalThrottled => write!(f, "thermal_throttled"),
        }
    }
}

/// Record of an active heavy workload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveWorkload {
    pub id: String,
    pub kind: HeavyWorkloadKind,
    pub description: String,
    pub registered_at_epoch_ms: u64,
}

/// Record of a registered low-priority background job
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackgroundJobInfo {
    pub id: String,
    pub kind: BackgroundJobKind,
    pub state: BackgroundJobState,
    pub registered_at_epoch_ms: u64,
    pub paused_at_epoch_ms: Option<u64>,
}

/// Complete direct hardware telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareTelemetrySnapshot {
    pub cpu_temp_c: f64,
    pub battery_pct: u32,
    pub battery_draw_w: f64,
    pub battery_discharge_pct_per_hr: f64,
    pub is_discharging: bool,
    pub mglru_min_ttl_ms: u64,
    pub mglru_enabled: bool,
    pub psi_some_avg10: f64,
    pub memory_available_pct: f64,
    pub is_thermal_peak: bool,
    pub is_battery_critical: bool,
    pub is_memory_critical: bool,
    pub dynatune_detected: bool,
    pub kernel_ctl_detected: bool,
    pub timestamp_epoch_ms: u64,
}

/// Complete status report of the Dynamic Workload Shedder
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkloadShedderStatus {
    pub state: ShedderState,
    pub current_concurrency: usize,
    pub baseline_concurrency: usize,
    pub active_governor: String,
    pub active_epp: String,
    pub turbo_enabled: bool,
    pub mglru_min_ttl_ms: u64,
    pub active_heavy_workloads: Vec<ActiveWorkload>,
    pub background_jobs: Vec<BackgroundJobInfo>,
    pub telemetry: HardwareTelemetrySnapshot,
    pub total_shedding_events: u64,
    pub total_recovery_events: u64,
    pub total_boost_events: u64,
}

/// Outcome report of an autonomous evaluation cycle
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShedderEvaluationReport {
    pub state_changed: bool,
    pub previous_state: ShedderState,
    pub new_state: ShedderState,
    pub trigger_reason: Option<String>,
    pub current_concurrency: usize,
    pub paused_jobs_count: usize,
    pub resumed_jobs_count: usize,
    pub actions_taken: Vec<String>,
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Dynamic Workload Shedder Engine
///
/// Implements Pillar II Hardware-Software Co-Design for Hagibis:
/// - Direct hardware telemetry loops
/// - Workload-aware performance profiling for heavy builds/embeddings
/// - Peak thermal & battery load mitigation with automatic task shedding
/// - Automatic cool-down recovery with work restoration
pub struct DynamicWorkloadShedder {
    workspace_root: PathBuf,
    dynatune_dir: PathBuf,
    kernel_ctl_path: PathBuf,
    governor_script_path: PathBuf,
    baseline_concurrency: usize,
    current_concurrency: Arc<AtomicUsize>,
    state: Arc<RwLock<ShedderState>>,
    active_workloads: Arc<RwLock<HashMap<String, ActiveWorkload>>>,
    background_jobs: Arc<RwLock<HashMap<String, BackgroundJobInfo>>>,
    current_governor: Arc<RwLock<String>>,
    current_epp: Arc<RwLock<String>>,
    turbo_enabled: Arc<AtomicBool>,
    mglru_min_ttl_ms: Arc<AtomicU64>,
    total_shedding_events: Arc<AtomicU64>,
    total_recovery_events: Arc<AtomicU64>,
    total_boost_events: Arc<AtomicU64>,
    // Test simulation overrides
    simulated_temp_c: Arc<Mutex<Option<f64>>>,
    simulated_battery: Arc<Mutex<Option<(u32, bool, f64, f64)>>>, // (pct, discharging, draw_w, drop_pct_hr)
    simulated_memory: Arc<Mutex<Option<(f64, f64)>>>,             // (psi_avg10, available_pct)
    simulated_mglru_ttl: Arc<Mutex<Option<u64>>>,
}

impl DynamicWorkloadShedder {
    /// Initialize a new Dynamic Workload Shedder rooted at workspace
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let ws = workspace_root.into();
        let default_concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let dynatune_dir = PathBuf::from("/home/toshiba/My AI Project/dynatune");
        let kernel_ctl = PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl");
        let gov_script = ws.join("scripts/cpu-governor.sh");

        Self {
            workspace_root: ws,
            dynatune_dir,
            kernel_ctl_path: kernel_ctl,
            governor_script_path: gov_script,
            baseline_concurrency: default_concurrency,
            current_concurrency: Arc::new(AtomicUsize::new(default_concurrency)),
            state: Arc::new(RwLock::new(ShedderState::Nominal)),
            active_workloads: Arc::new(RwLock::new(HashMap::new())),
            background_jobs: Arc::new(RwLock::new(HashMap::new())),
            current_governor: Arc::new(RwLock::new("powersave".to_string())),
            current_epp: Arc::new(RwLock::new("balance_performance".to_string())),
            turbo_enabled: Arc::new(AtomicBool::new(false)),
            mglru_min_ttl_ms: Arc::new(AtomicU64::new(IDLE_MGLRU_MIN_TTL_MS)),
            total_shedding_events: Arc::new(AtomicU64::new(0)),
            total_recovery_events: Arc::new(AtomicU64::new(0)),
            total_boost_events: Arc::new(AtomicU64::new(0)),
            simulated_temp_c: Arc::new(Mutex::new(None)),
            simulated_battery: Arc::new(Mutex::new(None)),
            simulated_memory: Arc::new(Mutex::new(None)),
            simulated_mglru_ttl: Arc::new(Mutex::new(None)),
        }
    }

    /// Access workspace root path
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Access dynatune directory path
    pub fn dynatune_dir(&self) -> &Path {
        &self.dynatune_dir
    }

    /// Access dynabook-kernel-ctl script path
    pub fn kernel_ctl_path(&self) -> &Path {
        &self.kernel_ctl_path
    }

    /// Access cpu-governor.sh script path
    pub fn governor_script_path(&self) -> &Path {
        &self.governor_script_path
    }

    /// Set simulated CPU temperature in Celsius for testing
    pub async fn set_simulated_temp(&self, temp_c: Option<f64>) {
        let mut sim = self.simulated_temp_c.lock().await;
        *sim = temp_c;
    }

    /// Set simulated battery telemetry for testing (pct, is_discharging, draw_w, drop_pct_per_hr)
    pub async fn set_simulated_battery(&self, battery: Option<(u32, bool, f64, f64)>) {
        let mut sim = self.simulated_battery.lock().await;
        *sim = battery;
    }

    /// Set simulated memory pressure for testing (psi_some_avg10, available_pct)
    pub async fn set_simulated_memory_pressure(&self, memory: Option<(f64, f64)>) {
        let mut sim = self.simulated_memory.lock().await;
        *sim = memory;
    }

    /// Set simulated MGLRU min_ttl_ms for testing
    pub async fn set_simulated_mglru_min_ttl_ms(&self, ttl: Option<u64>) {
        let mut sim = self.simulated_mglru_ttl.lock().await;
        *sim = ttl;
    }

    /// Baseline worker concurrency configured for this system
    pub fn baseline_concurrency(&self) -> usize {
        self.baseline_concurrency
    }

    /// Current effective worker concurrency limit
    pub fn current_concurrency(&self) -> usize {
        self.current_concurrency.load(Ordering::SeqCst)
    }

    /// Check if low-priority background workloads must pause
    pub async fn should_pause_background_workloads(&self) -> bool {
        let st = *self.state.read().await;
        st == ShedderState::ThermalThrottled
    }

    /// Poll comprehensive hardware telemetry directly from Linux sysfs and sibling projects
    pub async fn poll_hardware_telemetry(&self) -> HardwareTelemetrySnapshot {
        // 1. CPU package temperature
        let cpu_temp_c = if let Some(sim) = *self.simulated_temp_c.lock().await {
            sim
        } else {
            Self::read_direct_cpu_temp()
        };

        // 2. Battery discharge telemetry (W and %/hr)
        let (battery_pct, is_discharging, battery_draw_w, battery_discharge_pct_per_hr) =
            if let Some(sim) = *self.simulated_battery.lock().await {
                sim
            } else {
                Self::read_direct_battery_telemetry()
            };

        // 3. Memory pressure (PSI & Available RAM %)
        let (psi_some_avg10, memory_available_pct) =
            if let Some(sim) = *self.simulated_memory.lock().await {
                sim
            } else {
                Self::read_direct_memory_pressure()
            };

        // 4. Linux Multi-Gen LRU (MGLRU) min_ttl_ms
        let mglru_min_ttl_ms = if let Some(sim) = *self.simulated_mglru_ttl.lock().await {
            sim
        } else {
            Self::read_direct_mglru_min_ttl_ms()
        };
        let mglru_enabled = Self::read_direct_mglru_enabled();

        // 5. Evaluate threshold flags
        let is_thermal_peak = cpu_temp_c >= THERMAL_PEAK_THRESHOLD_C;
        let is_battery_critical = is_discharging
            && (battery_pct < BATTERY_CRITICAL_DISCHARGE_PCT || battery_draw_w > BATTERY_SPIKE_DRAW_W);
        let is_memory_critical =
            psi_some_avg10 >= MEMORY_PRESSURE_CRITICAL_PSI_AVG10 || memory_available_pct < MEMORY_AVAILABLE_CRITICAL_PCT;

        let dynatune_detected = self.dynatune_dir.exists();
        let kernel_ctl_detected = self.kernel_ctl_path.exists();

        HardwareTelemetrySnapshot {
            cpu_temp_c,
            battery_pct,
            battery_draw_w,
            battery_discharge_pct_per_hr,
            is_discharging,
            mglru_min_ttl_ms,
            mglru_enabled,
            psi_some_avg10,
            memory_available_pct,
            is_thermal_peak,
            is_battery_critical,
            is_memory_critical,
            dynatune_detected,
            kernel_ctl_detected,
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }

    /// Register a heavy compute workload (PGO build, batch vector embeddings, multi-agent loop)
    /// Automatically triggers performance governor escalation if thermal conditions permit.
    pub async fn register_heavy_workload(
        self: &Arc<Self>,
        id: impl Into<String>,
        kind: HeavyWorkloadKind,
        description: impl Into<String>,
    ) -> Result<WorkloadGuard> {
        let wid = id.into();
        let desc = description.into();

        {
            let mut map = self.active_workloads.write().await;
            map.insert(
                wid.clone(),
                ActiveWorkload {
                    id: wid.clone(),
                    kind,
                    description: desc,
                    registered_at_epoch_ms: current_epoch_ms(),
                },
            );
        }

        // Check live hardware telemetry before boosting
        let telem = self.poll_hardware_telemetry().await;
        if telem.is_thermal_peak || telem.is_battery_critical || telem.is_memory_critical {
            // Under thermal peak or critical pressure; clamp immediately to safeguard hardware
            self.apply_thermal_shedding(&telem).await;
        } else {
            // Apply heavy workload initiation performance boost
            self.apply_performance_profile().await;
        }

        Ok(WorkloadGuard {
            shedder: Arc::clone(self),
            workload_id: wid,
            completed: false,
        })
    }

    /// Signal completion of a heavy compute workload
    /// Relaxes hardware performance back to nominal if no other heavy workloads remain.
    pub async fn complete_heavy_workload(&self, id: &str) -> Result<()> {
        let remaining_count = {
            let mut map = self.active_workloads.write().await;
            map.remove(id);
            map.len()
        };

        if remaining_count == 0 {
            let mut st = self.state.write().await;
            if *st == ShedderState::PerformanceBoosted {
                *st = ShedderState::Nominal;
                drop(st);
                self.apply_nominal_profile().await;
            }
        }

        Ok(())
    }

    /// Register a low-priority background job (indexing, telemetry summarization, AST sweeps)
    pub async fn register_background_job(
        &self,
        id: impl Into<String>,
        kind: BackgroundJobKind,
    ) {
        let jid = id.into();
        let is_throttled = *self.state.read().await == ShedderState::ThermalThrottled;
        let initial_state = if is_throttled {
            BackgroundJobState::Paused
        } else {
            BackgroundJobState::Active
        };

        let mut jobs = self.background_jobs.write().await;
        jobs.insert(
            jid.clone(),
            BackgroundJobInfo {
                id: jid,
                kind,
                state: initial_state,
                registered_at_epoch_ms: current_epoch_ms(),
                paused_at_epoch_ms: if is_throttled { Some(current_epoch_ms()) } else { None },
            },
        );
    }

    /// Query if a specific background job is currently paused or shed
    pub async fn is_background_job_paused(&self, id: &str) -> bool {
        let jobs = self.background_jobs.read().await;
        jobs.get(id)
            .map(|j| j.state != BackgroundJobState::Active)
            .unwrap_or(false)
    }

    /// Autonomous closed-loop evaluation cycle:
    /// - Detects thermal peaks (temp >= 78°C, battery < 20% discharging, PSI critical) -> Sheds workload
    /// - Detects cool-down recovery (temp < 70°C, battery stable) -> Restores concurrency & resumes shed tasks
    pub async fn evaluate_and_shed_or_recover(&self) -> ShedderEvaluationReport {
        let telem = self.poll_hardware_telemetry().await;
        let prev_state = *self.state.read().await;

        let should_shed = telem.is_thermal_peak || telem.is_battery_critical || telem.is_memory_critical;
        let should_recover = telem.cpu_temp_c < THERMAL_RECOVERY_THRESHOLD_C
            && (!telem.is_discharging || telem.battery_pct >= BATTERY_RECOVERY_PCT)
            && !telem.is_memory_critical;

        let mut actions = Vec::new();
        let mut paused_count = 0;
        let mut resumed_count = 0;

        if should_shed && prev_state != ShedderState::ThermalThrottled {
            // Trigger Dynamic Workload Shedding
            self.apply_thermal_shedding(&telem).await;
            self.total_shedding_events.fetch_add(1, Ordering::SeqCst);

            // Pause all registered background jobs
            {
                let mut jobs = self.background_jobs.write().await;
                for j in jobs.values_mut() {
                    if j.state == BackgroundJobState::Active {
                        j.state = BackgroundJobState::Paused;
                        j.paused_at_epoch_ms = Some(current_epoch_ms());
                        paused_count += 1;
                    }
                }
            }

            let reason = if telem.is_thermal_peak {
                format!("CPU thermal threshold exceeded ({:.1}°C >= {:.1}°C)", telem.cpu_temp_c, THERMAL_PEAK_THRESHOLD_C)
            } else if telem.is_battery_critical {
                format!("Battery critical on discharge ({}% < {}% or {:.1}W draw)", telem.battery_pct, BATTERY_CRITICAL_DISCHARGE_PCT, telem.battery_draw_w)
            } else {
                format!("Linux MGLRU/PSI memory pressure critical (avg10: {:.1}%, RAM avail: {:.1}%)", telem.psi_some_avg10, telem.memory_available_pct)
            };

            actions.push(format!("Engaged workload shedding: {}", reason));
            actions.push(format!("Scaled worker concurrency from {} to {} workers", self.baseline_concurrency, THROTTLED_CONCURRENCY_WORKERS));
            actions.push("Clamped CPU governor to powersave, EPP to power, and MGLRU min_ttl_ms to 0".to_string());
            actions.push(format!("Paused {} background maintenance tasks", paused_count));

            ShedderEvaluationReport {
                state_changed: true,
                previous_state: prev_state,
                new_state: ShedderState::ThermalThrottled,
                trigger_reason: Some(reason),
                current_concurrency: THROTTLED_CONCURRENCY_WORKERS,
                paused_jobs_count: paused_count,
                resumed_jobs_count: 0,
                actions_taken: actions,
            }
        } else if should_recover && prev_state == ShedderState::ThermalThrottled {
            // Trigger Cool-Down Recovery
            self.total_recovery_events.fetch_add(1, Ordering::SeqCst);

            // Resume all paused background jobs
            {
                let mut jobs = self.background_jobs.write().await;
                for j in jobs.values_mut() {
                    if j.state == BackgroundJobState::Paused {
                        j.state = BackgroundJobState::Active;
                        j.paused_at_epoch_ms = None;
                        resumed_count += 1;
                    }
                }
            }

            // Restore full baseline concurrency
            self.current_concurrency.store(self.baseline_concurrency, Ordering::SeqCst);

            let active_count = self.active_workloads.read().await.len();
            let new_state = if active_count > 0 {
                // Heavy workloads persist -> re-escalate to performance
                self.apply_performance_profile().await;
                ShedderState::PerformanceBoosted
            } else {
                // No active heavy workloads -> nominal balanced profile
                self.apply_nominal_profile().await;
                ShedderState::Nominal
            };

            actions.push(format!("Cool-down recovery threshold met (CPU temp: {:.1}°C < {:.1}°C)", telem.cpu_temp_c, THERMAL_RECOVERY_THRESHOLD_C));
            actions.push(format!("Restored worker concurrency to {} workers", self.baseline_concurrency));
            actions.push(format!("Resumed {} paused background tasks", resumed_count));
            if active_count > 0 {
                actions.push(format!("Re-escalated CPU to performance profile for {} active heavy workload(s)", active_count));
            }

            ShedderEvaluationReport {
                state_changed: true,
                previous_state: prev_state,
                new_state,
                trigger_reason: Some("Cool-down recovery".to_string()),
                current_concurrency: self.baseline_concurrency,
                paused_jobs_count: 0,
                resumed_jobs_count: resumed_count,
                actions_taken: actions,
            }
        } else {
            // Steady state
            ShedderEvaluationReport {
                state_changed: false,
                previous_state: prev_state,
                new_state: prev_state,
                trigger_reason: None,
                current_concurrency: self.current_concurrency(),
                paused_jobs_count: 0,
                resumed_jobs_count: 0,
                actions_taken: Vec::new(),
            }
        }
    }

    /// Launch background autonomous hardware telemetry polling loop
    pub fn start_telemetry_loop(self: &Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        let shedder = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let _ = shedder.evaluate_and_shed_or_recover().await;
            }
        })
    }

    /// Query comprehensive status snapshot
    pub async fn status(&self) -> WorkloadShedderStatus {
        let telem = self.poll_hardware_telemetry().await;
        let active_heavy: Vec<ActiveWorkload> = self.active_workloads.read().await.values().cloned().collect();
        let bg_jobs: Vec<BackgroundJobInfo> = self.background_jobs.read().await.values().cloned().collect();
        let st = *self.state.read().await;
        let gov = self.current_governor.read().await.clone();
        let epp = self.current_epp.read().await.clone();

        WorkloadShedderStatus {
            state: st,
            current_concurrency: self.current_concurrency(),
            baseline_concurrency: self.baseline_concurrency,
            active_governor: gov,
            active_epp: epp,
            turbo_enabled: self.turbo_enabled.load(Ordering::SeqCst),
            mglru_min_ttl_ms: self.mglru_min_ttl_ms.load(Ordering::SeqCst),
            active_heavy_workloads: active_heavy,
            background_jobs: bg_jobs,
            telemetry: telem,
            total_shedding_events: self.total_shedding_events.load(Ordering::SeqCst),
            total_recovery_events: self.total_recovery_events.load(Ordering::SeqCst),
            total_boost_events: self.total_boost_events.load(Ordering::SeqCst),
        }
    }

    // =========================================================================
    // Hardware Actuation Primitives
    // =========================================================================

    async fn apply_performance_profile(&self) {
        {
            let mut st = self.state.write().await;
            *st = ShedderState::PerformanceBoosted;
            let mut gov = self.current_governor.write().await;
            *gov = "performance".to_string();
            let mut epp = self.current_epp.write().await;
            *epp = "performance".to_string();
        }
        self.turbo_enabled.store(true, Ordering::SeqCst);
        self.mglru_min_ttl_ms.store(BURST_MGLRU_MIN_TTL_MS, Ordering::SeqCst);
        self.current_concurrency.store(self.baseline_concurrency, Ordering::SeqCst);
        self.total_boost_events.fetch_add(1, Ordering::SeqCst);

        self.apply_hardware_actuation("performance", "performance", true, BURST_MGLRU_MIN_TTL_MS).await;
    }

    async fn apply_thermal_shedding(&self, _telem: &HardwareTelemetrySnapshot) {
        {
            let mut st = self.state.write().await;
            *st = ShedderState::ThermalThrottled;
            let mut gov = self.current_governor.write().await;
            *gov = "powersave".to_string();
            let mut epp = self.current_epp.write().await;
            *epp = "power".to_string();
        }
        self.turbo_enabled.store(false, Ordering::SeqCst);
        self.mglru_min_ttl_ms.store(IDLE_MGLRU_MIN_TTL_MS, Ordering::SeqCst);
        self.current_concurrency.store(THROTTLED_CONCURRENCY_WORKERS, Ordering::SeqCst);

        self.apply_hardware_actuation("powersave", "power", false, IDLE_MGLRU_MIN_TTL_MS).await;
    }

    async fn apply_nominal_profile(&self) {
        {
            let mut st = self.state.write().await;
            *st = ShedderState::Nominal;
            let mut gov = self.current_governor.write().await;
            *gov = "powersave".to_string();
            let mut epp = self.current_epp.write().await;
            *epp = "balance_performance".to_string();
        }
        self.turbo_enabled.store(false, Ordering::SeqCst);
        self.mglru_min_ttl_ms.store(IDLE_MGLRU_MIN_TTL_MS, Ordering::SeqCst);
        self.current_concurrency.store(self.baseline_concurrency, Ordering::SeqCst);

        self.apply_hardware_actuation("powersave", "balance_performance", false, IDLE_MGLRU_MIN_TTL_MS).await;
    }

    async fn apply_hardware_actuation(&self, governor: &str, epp: &str, enable_turbo: bool, mglru_ttl_ms: u64) {
        #[cfg(target_os = "linux")]
        {
            // 1. Sysfs scaling_governor and energy_performance_preference
            if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) {
                        let cpufreq_dir = entry.path().join("cpufreq");
                        let gov_path = cpufreq_dir.join("scaling_governor");
                        let epp_path = cpufreq_dir.join("energy_performance_preference");
                        let _ = fs::write(&gov_path, governor);
                        let _ = fs::write(&epp_path, epp);
                    }
                }
            }

            // 2. Intel turbo boost control
            let no_turbo_path = Path::new("/sys/devices/system/cpu/intel_pstate/no_turbo");
            if no_turbo_path.exists() {
                let val = if enable_turbo { "0\n" } else { "1\n" };
                let _ = fs::write(no_turbo_path, val);
            }

            // 3. Linux Multi-Gen LRU (MGLRU) min_ttl_ms tuning
            let mglru_path = Path::new("/sys/kernel/mm/lru_gen/min_ttl_ms");
            if mglru_path.exists() {
                let _ = fs::write(mglru_path, format!("{}\n", mglru_ttl_ms));
            }
        }

        // 4. Interface with cpu-governor.sh or dynabook-kernel-ctl
        if self.governor_script_path.exists() {
            let script_mode = match governor {
                "performance" => "performance",
                "powersave" if epp == "power" => "powersave",
                _ => "balanced",
            };
            let _ = Command::new(&self.governor_script_path)
                .arg(script_mode)
                .output();
        } else if self.kernel_ctl_path.exists() {
            let profile = match governor {
                "performance" => "performance",
                "powersave" if epp == "power" => "battery",
                _ => "balanced",
            };
            let _ = Command::new(&self.kernel_ctl_path)
                .args(["profile", profile])
                .output();
        }
    }

    // =========================================================================
    // Direct Telemetry Polling Primitives
    // =========================================================================

    fn read_direct_cpu_temp() -> f64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
                let mut best_temp = 0.0;
                let mut pkg_temp = None;

                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("thermal_zone") {
                        let path = entry.path();
                        let type_path = path.join("type");
                        let temp_path = path.join("temp");

                        let zone_type = fs::read_to_string(&type_path)
                            .unwrap_or_default()
                            .trim()
                            .to_string();

                        if let Ok(raw) = fs::read_to_string(&temp_path) {
                            if let Ok(milli) = raw.trim().parse::<f64>() {
                                let deg = milli / 1000.0;
                                if deg > 0.0 && deg < 125.0 {
                                    if zone_type == "x86_pkg_temp" {
                                        pkg_temp = Some(deg);
                                    } else if pkg_temp.is_none() && (zone_type == "cpu-thermal" || zone_type == "acpitz") {
                                        pkg_temp = Some(deg);
                                    }
                                    if deg > best_temp {
                                        best_temp = deg;
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(pkg) = pkg_temp {
                    return pkg;
                }
                if best_temp > 0.0 {
                    return best_temp;
                }
            }
        }
        45.0 // Nominal baseline fallback
    }

    fn read_direct_battery_telemetry() -> (u32, bool, f64, f64) {
        #[cfg(target_os = "linux")]
        {
            if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("BAT") {
                        let path = entry.path();
                        let cap = fs::read_to_string(path.join("capacity"))
                            .ok()
                            .and_then(|s| s.trim().parse::<u32>().ok())
                            .unwrap_or(100);
                        let status = fs::read_to_string(path.join("status"))
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        let is_discharging = status.eq_ignore_ascii_case("discharging");

                        let voltage_uv = fs::read_to_string(path.join("voltage_now"))
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .unwrap_or(7_600_000) as f64;

                        let power_w = if let Some(p) = fs::read_to_string(path.join("power_now")).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                            p as f64 / 1_000_000.0
                        } else if let Some(c) = fs::read_to_string(path.join("current_now")).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                            (c as f64 * voltage_uv) / 1_000_000_000_000.0
                        } else {
                            0.0
                        };

                        let energy_full_wh = if let Some(e) = fs::read_to_string(path.join("energy_full")).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                            e as f64 / 1_000_000.0
                        } else if let Some(c) = fs::read_to_string(path.join("charge_full")).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                            (c as f64 * voltage_uv) / 1_000_000_000_000.0
                        } else {
                            40.0 // Dynabook nominal battery capacity Wh
                        };

                        let drop_pct_per_hr = if is_discharging && energy_full_wh > 0.0 {
                            (power_w / energy_full_wh) * 100.0
                        } else {
                            0.0
                        };

                        return (cap, is_discharging, power_w, drop_pct_per_hr);
                    }
                }
            }
        }
        (100, false, 0.0, 0.0) // Nominal fallback on AC mains
    }

    fn read_direct_memory_pressure() -> (f64, f64) {
        let mut psi_avg10 = 0.0;
        let mut available_pct = 100.0;

        #[cfg(target_os = "linux")]
        {
            // 1. PSI /proc/pressure/memory
            if let Ok(psi_str) = fs::read_to_string("/proc/pressure/memory") {
                for line in psi_str.lines() {
                    if line.starts_with("some") {
                        for part in line.split_whitespace() {
                            if let Some(rest) = part.strip_prefix("avg10=") {
                                if let Ok(val) = rest.parse::<f64>() {
                                    psi_avg10 = val;
                                }
                            }
                        }
                    }
                }
            }

            // 2. /proc/meminfo
            if let Ok(mem_str) = fs::read_to_string("/proc/meminfo") {
                let mut total_kb = 0u64;
                let mut avail_kb = 0u64;
                for line in mem_str.lines() {
                    if line.starts_with("MemTotal:") {
                        total_kb = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    } else if line.starts_with("MemAvailable:") {
                        avail_kb = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                }
                if total_kb > 0 {
                    available_pct = (avail_kb as f64 / total_kb as f64) * 100.0;
                }
            }
        }

        (psi_avg10, available_pct)
    }

    fn read_direct_mglru_min_ttl_ms() -> u64 {
        #[cfg(target_os = "linux")]
        {
            let path = Path::new("/sys/kernel/mm/lru_gen/min_ttl_ms");
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(ttl) = content.trim().parse::<u64>() {
                    return ttl;
                }
            }
        }
        0
    }

    fn read_direct_mglru_enabled() -> bool {
        #[cfg(target_os = "linux")]
        {
            let path = Path::new("/sys/kernel/mm/lru_gen/enabled");
            if let Ok(content) = fs::read_to_string(path) {
                let s = content.trim();
                return s != "0x0000" && s != "0";
            }
        }
        false
    }
}

/// RAII Guard that automatically completes registered heavy workload on drop
pub struct WorkloadGuard {
    shedder: Arc<DynamicWorkloadShedder>,
    workload_id: String,
    completed: bool,
}

impl WorkloadGuard {
    /// Explicitly mark workload as completed ahead of drop
    pub async fn complete(mut self) -> Result<()> {
        if !self.completed {
            self.completed = true;
            self.shedder.complete_heavy_workload(&self.workload_id).await?;
        }
        Ok(())
    }

    /// Access workload ID
    pub fn workload_id(&self) -> &str {
        &self.workload_id
    }
}

impl Drop for WorkloadGuard {
    fn drop(&mut self) {
        if !self.completed {
            let shedder = Arc::clone(&self.shedder);
            let id = self.workload_id.clone();
            tokio::spawn(async move {
                let _ = shedder.complete_heavy_workload(&id).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_workload_registration_and_performance_boost() {
        let tmp = std::env::temp_dir().join(format!("shedder_boost_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&tmp);

        let shedder = Arc::new(DynamicWorkloadShedder::new(&tmp));
        // Ensure initial cool nominal baseline
        shedder.set_simulated_temp(Some(55.0)).await;
        shedder.set_simulated_battery(Some((95, false, 0.0, 0.0))).await;

        let status_init = shedder.status().await;
        assert_eq!(status_init.state, ShedderState::Nominal);
        assert_eq!(status_init.active_governor, "powersave");
        assert_eq!(status_init.mglru_min_ttl_ms, IDLE_MGLRU_MIN_TTL_MS);

        // 1. Register heavy PGO build workload
        let guard = shedder
            .register_heavy_workload("pgo_task_001", HeavyWorkloadKind::PgoBuild, "Compiling PGO profile")
            .await
            .expect("register heavy workload");

        let status_boosted = shedder.status().await;
        assert_eq!(status_boosted.state, ShedderState::PerformanceBoosted);
        assert_eq!(status_boosted.active_governor, "performance");
        assert_eq!(status_boosted.active_epp, "performance");
        assert!(status_boosted.turbo_enabled);
        assert_eq!(status_boosted.mglru_min_ttl_ms, BURST_MGLRU_MIN_TTL_MS);
        assert_eq!(status_boosted.current_concurrency, shedder.baseline_concurrency());
        assert_eq!(status_boosted.active_heavy_workloads.len(), 1);
        assert_eq!(status_boosted.total_boost_events, 1);

        // 2. Complete heavy workload
        guard.complete().await.expect("complete workload");

        let status_relaxed = shedder.status().await;
        assert_eq!(status_relaxed.state, ShedderState::Nominal);
        assert_eq!(status_relaxed.active_governor, "powersave");
        assert_eq!(status_relaxed.mglru_min_ttl_ms, IDLE_MGLRU_MIN_TTL_MS);
        assert_eq!(status_relaxed.active_heavy_workloads.len(), 0);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_peak_thermal_threshold_and_concurrency_shedding() {
        let tmp = std::env::temp_dir().join(format!("shedder_thermal_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&tmp);

        let shedder = Arc::new(DynamicWorkloadShedder::new(&tmp));
        shedder.set_simulated_temp(Some(60.0)).await;

        // Register background maintenance tasks
        shedder.register_background_job("bg_index_1", BackgroundJobKind::BackgroundIndexing).await;
        shedder.register_background_job("bg_telem_1", BackgroundJobKind::TelemetrySummarization).await;
        shedder.register_background_job("bg_ast_1", BackgroundJobKind::AstSweep).await;

        assert!(!shedder.is_background_job_paused("bg_index_1").await);
        assert!(!shedder.is_background_job_paused("bg_telem_1").await);
        assert!(!shedder.is_background_job_paused("bg_ast_1").await);

        // Register a heavy workload
        let _guard = shedder
            .register_heavy_workload("batch_emb_1", HeavyWorkloadKind::BatchVectorEmbeddings, "Vector clustering")
            .await
            .expect("register workload");

        assert_eq!(shedder.status().await.state, ShedderState::PerformanceBoosted);

        // Simulate thermal peak: CPU temperature spikes to 81.5°C (>= 78.0°C)
        shedder.set_simulated_temp(Some(81.5)).await;

        let eval = shedder.evaluate_and_shed_or_recover().await;
        assert!(eval.state_changed);
        assert_eq!(eval.new_state, ShedderState::ThermalThrottled);
        assert_eq!(eval.current_concurrency, THROTTLED_CONCURRENCY_WORKERS);
        assert_eq!(eval.paused_jobs_count, 3);

        let status = shedder.status().await;
        assert_eq!(status.state, ShedderState::ThermalThrottled);
        assert_eq!(status.active_governor, "powersave");
        assert_eq!(status.active_epp, "power");
        assert!(!status.turbo_enabled);
        assert_eq!(status.mglru_min_ttl_ms, IDLE_MGLRU_MIN_TTL_MS);
        assert_eq!(status.current_concurrency, THROTTLED_CONCURRENCY_WORKERS);
        assert_eq!(status.total_shedding_events, 1);

        // Verify background jobs are now paused
        assert!(shedder.is_background_job_paused("bg_index_1").await);
        assert!(shedder.is_background_job_paused("bg_telem_1").await);
        assert!(shedder.is_background_job_paused("bg_ast_1").await);
        assert!(shedder.should_pause_background_workloads().await);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_battery_critical_and_memory_pressure_shedding() {
        let tmp = std::env::temp_dir().join(format!("shedder_bat_mem_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&tmp);

        let shedder = Arc::new(DynamicWorkloadShedder::new(&tmp));

        // 1. Test battery critical on discharge (<20%)
        shedder.set_simulated_temp(Some(65.0)).await;
        shedder.set_simulated_battery(Some((18, true, 18.5, 35.0))).await; // 18% Discharging

        let report_bat = shedder.evaluate_and_shed_or_recover().await;
        assert!(report_bat.state_changed);
        assert_eq!(report_bat.new_state, ShedderState::ThermalThrottled);
        assert!(report_bat.trigger_reason.unwrap().contains("Battery critical on discharge"));

        // Reset to nominal on AC
        shedder.set_simulated_battery(Some((80, false, 0.0, 0.0))).await;
        let _ = shedder.evaluate_and_shed_or_recover().await;

        // 2. Test critical Linux PSI memory pressure (avg10 >= 35.0)
        shedder.set_simulated_memory_pressure(Some((42.0, 4.5))).await;

        let report_mem = shedder.evaluate_and_shed_or_recover().await;
        assert!(report_mem.state_changed);
        assert_eq!(report_mem.new_state, ShedderState::ThermalThrottled);
        assert!(report_mem.trigger_reason.unwrap().contains("memory pressure critical"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_workload_recovery_upon_cooldown() {
        let tmp = std::env::temp_dir().join(format!("shedder_recovery_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&tmp);

        let shedder = Arc::new(DynamicWorkloadShedder::new(&tmp));

        // Register background job
        shedder.register_background_job("bg_sweep", BackgroundJobKind::AstSweep).await;

        // Register persistent heavy workload (e.g. multi-agent loop)
        let _guard = shedder
            .register_heavy_workload("agent_loop_42", HeavyWorkloadKind::MultiAgentLoop, "Swarm execution")
            .await
            .expect("register workload");

        // Force thermal throttling spike (82.0°C)
        shedder.set_simulated_temp(Some(82.0)).await;
        let report_shed = shedder.evaluate_and_shed_or_recover().await;
        assert_eq!(report_shed.new_state, ShedderState::ThermalThrottled);
        assert!(shedder.is_background_job_paused("bg_sweep").await);

        // Simulate cool-down below 70°C (64.0°C) with stable battery
        shedder.set_simulated_temp(Some(64.0)).await;
        shedder.set_simulated_battery(Some((90, false, 0.0, 0.0))).await;
        shedder.set_simulated_memory_pressure(Some((5.0, 45.0))).await;

        let report_rec = shedder.evaluate_and_shed_or_recover().await;
        assert!(report_rec.state_changed);
        assert_eq!(report_rec.new_state, ShedderState::PerformanceBoosted);
        assert_eq!(report_rec.current_concurrency, shedder.baseline_concurrency());
        assert_eq!(report_rec.resumed_jobs_count, 1);

        let status = shedder.status().await;
        assert_eq!(status.state, ShedderState::PerformanceBoosted);
        assert_eq!(status.active_governor, "performance");
        assert_eq!(status.active_epp, "performance");
        assert!(status.turbo_enabled);
        assert_eq!(status.mglru_min_ttl_ms, BURST_MGLRU_MIN_TTL_MS);
        assert_eq!(status.total_recovery_events, 1);

        // Verify background job was resumed
        assert!(!shedder.is_background_job_paused("bg_sweep").await);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_telemetry_state_serialization() {
        let tmp = std::env::temp_dir().join(format!("shedder_serde_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&tmp);

        let shedder = Arc::new(DynamicWorkloadShedder::new(&tmp));
        shedder.set_simulated_temp(Some(68.5)).await;
        shedder.set_simulated_battery(Some((88, false, 0.0, 0.0))).await;
        shedder.set_simulated_memory_pressure(Some((8.2, 52.0))).await;
        shedder.set_simulated_mglru_min_ttl_ms(Some(500)).await;

        shedder.register_background_job("bg_test", BackgroundJobKind::CacheCompaction).await;

        let status = shedder.status().await;

        // Serialize to JSON
        let json_str = serde_json::to_string_pretty(&status).expect("serialize WorkloadShedderStatus");
        assert!(json_str.contains("\"state\": \"nominal\""));
        assert!(json_str.contains("\"cpu_temp_c\": 68.5"));
        assert!(json_str.contains("\"battery_pct\": 88"));
        assert!(json_str.contains("\"psi_some_avg10\": 8.2"));
        assert!(json_str.contains("\"mglru_min_ttl_ms\": 500"));

        // Deserialize back
        let deserialized: WorkloadShedderStatus = serde_json::from_str(&json_str).expect("deserialize WorkloadShedderStatus");
        assert_eq!(deserialized, status);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_workload_guard_raii_drop() {
        let tmp = std::env::temp_dir().join(format!("shedder_guard_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&tmp);

        let shedder = Arc::new(DynamicWorkloadShedder::new(&tmp));
        shedder.set_simulated_temp(Some(50.0)).await;

        {
            let guard = shedder
                .register_heavy_workload("raii_task", HeavyWorkloadKind::HeavyCompilation, "Rust compilation")
                .await
                .expect("register workload");

            assert_eq!(guard.workload_id(), "raii_task");
            assert_eq!(shedder.status().await.active_heavy_workloads.len(), 1);
            assert_eq!(shedder.status().await.state, ShedderState::PerformanceBoosted);
            // guard is dropped here
        }

        // Allow async spawned drop cleanup to run
        tokio::time::sleep(Duration::from_millis(50)).await;

        let status = shedder.status().await;
        assert_eq!(status.active_heavy_workloads.len(), 0);
        assert_eq!(status.state, ShedderState::Nominal);

        let _ = fs::remove_dir_all(&tmp);
    }
}
