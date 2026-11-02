//! # Pillar 2: Autonomous Self-Profiling & PGO Feedback Loops
//!
//! Production-grade Continuous Autonomous Feedback Engine:
//! 1. Telemetry Monitoring:
//!    - Continuously aggregates active execution telemetry across WASM tool invocations,
//!      LLM token streaming rates, and latency percentiles (p50, p90, p95, p99).
//! 2. Autonomous Workload Hotspot Triggering:
//!    - When tool invocations cross the hotspot threshold (>500 calls) or latency SLA
//!      regression is detected (e.g. p99 latency > threshold), captures execution profiles
//!      to `.hgb/pgo_hotspots.json` and invokes `scripts/pgo-optimize.sh` in background mode.
//! 3. Hardware Synergy Engine:
//!    - Direct interface with `dynabook-kernel-ctl`, `scripts/cpu-governor.sh`, and Linux sysfs.
//!    - Heavy workload start: Sets CPU governor to `performance`, EPP to `performance`,
//!      enables Intel turbo boost, and sets MGLRU `min_ttl_ms 1000`.
//!    - Thermal warning (CPU temp >= 78°C or battery < 20% on discharge): Automatically
//!      clamps governor to `powersave`, EPP to `power`, and scales down task concurrency.
//!    - Idle queue: Automatically drops back to `powersave` and EPP `balance_power`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use hgb_core::error::{HgbError, Result};

/// Default threshold of WASM tool invocations triggering hotspot analysis (500 calls)
pub const DEFAULT_HOTSPOT_TOOL_CALL_THRESHOLD: u64 = 500;
/// Default latency SLA p99 regression threshold in milliseconds (500.0 ms)
pub const DEFAULT_LATENCY_SLA_P99_THRESHOLD_MS: f64 = 500.0;
/// Default cooldown between background PGO optimization invocations (5 minutes)
pub const DEFAULT_PGO_COOLDOWN_SECS: u64 = 300;
/// Critical CPU thermal throttling threshold (78.0°C)
pub const THERMAL_THROTTLE_TEMP_C: f64 = 78.0;
/// Severe thermal critical threshold (85.0°C)
pub const THERMAL_CRITICAL_TEMP_C: f64 = 85.0;
/// Battery reserve threshold on discharge (20%)
pub const BATTERY_DISCHARGE_CRITICAL_PCT: u32 = 20;
/// Target MGLRU min_ttl_ms when bursting during heavy workloads (1000 ms)
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;
/// Baseline MGLRU min_ttl_ms when idle or throttled (0 ms)
pub const IDLE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Telemetry metrics for an individual WASM tool
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallStats {
    pub name: String,
    pub invocations: u64,
    pub total_duration_ms: f64,
    pub min_duration_ms: f64,
    pub max_duration_ms: f64,
    pub avg_duration_ms: f64,
    pub error_count: u64,
    pub last_invoked_epoch_secs: u64,
}

impl ToolCallStats {
    pub fn new(name: String) -> Self {
        Self {
            name,
            invocations: 0,
            total_duration_ms: 0.0,
            min_duration_ms: f64::MAX,
            max_duration_ms: 0.0,
            avg_duration_ms: 0.0,
            error_count: 0,
            last_invoked_epoch_secs: current_epoch_secs(),
        }
    }

    pub fn record(&mut self, duration_ms: f64, is_err: bool) {
        self.invocations += 1;
        self.total_duration_ms += duration_ms;
        if duration_ms < self.min_duration_ms {
            self.min_duration_ms = duration_ms;
        }
        if duration_ms > self.max_duration_ms {
            self.max_duration_ms = duration_ms;
        }
        self.avg_duration_ms = self.total_duration_ms / (self.invocations as f64);
        if is_err {
            self.error_count += 1;
        }
        self.last_invoked_epoch_secs = current_epoch_secs();
    }
}

/// Telemetry metrics for LLM token streaming
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenStreamStats {
    pub total_tokens: u64,
    pub total_streaming_duration_ms: f64,
    pub stream_count: u64,
    pub tokens_per_second: f64,
    pub peak_tokens_per_second: f64,
}

impl Default for TokenStreamStats {
    fn default() -> Self {
        Self {
            total_tokens: 0,
            total_streaming_duration_ms: 0.0,
            stream_count: 0,
            tokens_per_second: 0.0,
            peak_tokens_per_second: 0.0,
        }
    }
}

impl TokenStreamStats {
    pub fn record(&mut self, tokens: usize, duration_ms: f64) {
        self.total_tokens += tokens as u64;
        self.total_streaming_duration_ms += duration_ms;
        self.stream_count += 1;

        if duration_ms > 0.0 {
            let rate = (tokens as f64) / (duration_ms / 1000.0);
            if rate > self.peak_tokens_per_second {
                self.peak_tokens_per_second = rate;
            }
        }
        if self.total_streaming_duration_ms > 0.0 {
            self.tokens_per_second = (self.total_tokens as f64) / (self.total_streaming_duration_ms / 1000.0);
        }
    }
}

/// Calculated percentiles across execution latencies
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyPercentiles {
    pub sample_count: usize,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl Default for LatencyPercentiles {
    fn default() -> Self {
        Self {
            sample_count: 0,
            p50_ms: 0.0,
            p90_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
        }
    }
}

/// Live hardware governor and telemetry profile
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareProfileTelemetry {
    pub governor: String,
    pub epp: String,
    pub turbo_enabled: bool,
    pub mglru_min_ttl_ms: u64,
    pub cpu_temp_c: f64,
    pub battery_pct: u32,
    pub is_discharging: bool,
    pub thermal_clamped: bool,
    pub current_concurrency_limit: usize,
    pub baseline_concurrency: usize,
}

/// Report snapshot captured to `.hgb/pgo_hotspots.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgoHotspotSnapshot {
    pub timestamp_rfc3339: String,
    pub trigger_reason: String,
    pub total_tool_calls: u64,
    pub tool_breakdown: Vec<ToolCallStats>,
    pub token_streaming: TokenStreamStats,
    pub latency: LatencyPercentiles,
    pub hardware: HardwareProfileTelemetry,
    pub pgo_invoked: bool,
    pub pgo_pid: Option<u32>,
    pub pgo_script_path: String,
}

/// Result returned when a PGO trigger check is performed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgoTriggerReport {
    pub triggered: bool,
    pub reason: String,
    pub snapshot_path: String,
    pub pid: Option<u32>,
    pub message: String,
}

/// Complete status report of the PGO Feedback Loop Engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgoFeedbackStatusReport {
    pub is_active: bool,
    pub is_pgo_running: bool,
    pub total_tool_calls: u64,
    pub tool_types_count: usize,
    pub token_stats: TokenStreamStats,
    pub latency_percentiles: LatencyPercentiles,
    pub hardware: HardwareProfileTelemetry,
    pub last_trigger_reason: Option<String>,
    pub last_trigger_epoch_secs: Option<u64>,
    pub trigger_count: u64,
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Hardware synergy subsystem interfacing with Linux sysfs, kernel-optimizer, and dynatune
#[derive(Debug)]
pub struct HardwareGovernorSynergy {
    workspace_root: PathBuf,
    dynabook_ctl_path: PathBuf,
    governor_script_path: PathBuf,
    baseline_concurrency: usize,
    current_concurrency: Arc<AtomicUsize>,
    thermal_clamped: Arc<AtomicBool>,
    current_governor: Arc<RwLock<String>>,
    current_epp: Arc<RwLock<String>>,
    turbo_enabled: Arc<AtomicBool>,
    mglru_min_ttl_ms: Arc<AtomicU64>,
    // Testing override flags
    simulated_temp_c: Arc<Mutex<Option<f64>>>,
    simulated_battery: Arc<Mutex<Option<(u32, bool)>>>,
}

impl HardwareGovernorSynergy {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let ws = workspace_root.into();
        let default_concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let dynabook_ctl = PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl");
        let gov_script = ws.join("scripts/cpu-governor.sh");

        Self {
            workspace_root: ws,
            dynabook_ctl_path: dynabook_ctl,
            governor_script_path: gov_script,
            baseline_concurrency: default_concurrency,
            current_concurrency: Arc::new(AtomicUsize::new(default_concurrency)),
            thermal_clamped: Arc::new(AtomicBool::new(false)),
            current_governor: Arc::new(RwLock::new("powersave".to_string())),
            current_epp: Arc::new(RwLock::new("balance_power".to_string())),
            turbo_enabled: Arc::new(AtomicBool::new(false)),
            mglru_min_ttl_ms: Arc::new(AtomicU64::new(IDLE_MGLRU_MIN_TTL_MS)),
            simulated_temp_c: Arc::new(Mutex::new(None)),
            simulated_battery: Arc::new(Mutex::new(None)),
        }
    }

    /// Set simulated hardware temperature for testing
    pub async fn set_simulated_temp(&self, temp_c: Option<f64>) {
        let mut sim = self.simulated_temp_c.lock().await;
        *sim = temp_c;
    }

    /// Set simulated battery state for testing (capacity_pct, is_discharging)
    pub async fn set_simulated_battery(&self, battery: Option<(u32, bool)>) {
        let mut sim = self.simulated_battery.lock().await;
        *sim = battery;
    }

    /// Read live CPU temperature in Celsius
    pub async fn read_cpu_temp_c(&self) -> f64 {
        if let Some(sim) = *self.simulated_temp_c.lock().await {
            return sim;
        }

        #[cfg(target_os = "linux")]
        {
            // Try /sys/class/thermal/thermal_zone*/temp
            if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
                let mut max_temp = 0.0;
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("thermal_zone") {
                        let temp_file = entry.path().join("temp");
                        if let Ok(content) = fs::read_to_string(&temp_file) {
                            if let Ok(millidegrees) = content.trim().parse::<f64>() {
                                let deg = millidegrees / 1000.0;
                                if deg > max_temp {
                                    max_temp = deg;
                                }
                            }
                        }
                    }
                }
                if max_temp > 0.0 {
                    return max_temp;
                }
            }
        }

        45.0 // Safe baseline fallback
    }

    /// Read live battery capacity and discharging state
    pub async fn read_battery_state(&self) -> (u32, bool) {
        if let Some(sim) = *self.simulated_battery.lock().await {
            return sim;
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("BAT") {
                        let cap_file = entry.path().join("capacity");
                        let status_file = entry.path().join("status");
                        let cap = fs::read_to_string(&cap_file)
                            .ok()
                            .and_then(|c| c.trim().parse::<u32>().ok())
                            .unwrap_or(100);
                        let is_discharging = fs::read_to_string(&status_file)
                            .map(|s| s.trim().eq_ignore_ascii_case("Discharging"))
                            .unwrap_or(false);
                        return (cap, is_discharging);
                    }
                }
            }
        }

        (100, false) // Fallback: 100% on AC mains
    }

    /// Apply heavy workload boost:
    /// Sets CPU governor to `performance`, sets EPP to `performance`,
    /// enables Intel turbo boost, and sets MGLRU `min_ttl_ms 1000`.
    pub async fn set_heavy_workload_boost(&self) -> Result<()> {
        if self.thermal_clamped.load(Ordering::SeqCst) {
            // Thermal clamping is active; boost is withheld for hardware protection
            return Ok(());
        }

        // 1. Update internal state
        {
            let mut gov = self.current_governor.write().await;
            *gov = "performance".to_string();
            let mut epp = self.current_epp.write().await;
            *epp = "performance".to_string();
        }
        self.turbo_enabled.store(true, Ordering::SeqCst);
        self.mglru_min_ttl_ms.store(BURST_MGLRU_MIN_TTL_MS, Ordering::SeqCst);
        self.current_concurrency.store(self.baseline_concurrency, Ordering::SeqCst);

        // 2. Apply to host sysfs / scripts
        self.apply_hardware_profile("performance", "performance", true, BURST_MGLRU_MIN_TTL_MS).await;

        Ok(())
    }

    /// Apply thermal warning mitigation:
    /// Automatically clamps governor to `powersave`, EPP to `power`,
    /// disables turbo boost, and scales down concurrency.
    pub async fn set_thermal_warning(&self, cpu_temp_c: f64, battery_pct: u32, is_discharging: bool) -> Result<()> {
        self.thermal_clamped.store(true, Ordering::SeqCst);

        // Scale concurrency down to 1 or 2 workers
        let throttled_concurrency = (self.baseline_concurrency / 2).max(1);
        self.current_concurrency.store(throttled_concurrency, Ordering::SeqCst);

        // Update state
        {
            let mut gov = self.current_governor.write().await;
            *gov = "powersave".to_string();
            let mut epp = self.current_epp.write().await;
            *epp = "power".to_string();
        }
        self.turbo_enabled.store(false, Ordering::SeqCst);
        self.mglru_min_ttl_ms.store(IDLE_MGLRU_MIN_TTL_MS, Ordering::SeqCst);

        eprintln!(
            "⚠️ [Hardware Governor Synergy] Thermal warning triggered (temp: {:.1}°C, battery: {}% discharging: {}). Clamping CPU to powersave, EPP to power, and restricting concurrency to {} workers.",
            cpu_temp_c, battery_pct, is_discharging, throttled_concurrency
        );

        // Apply to host sysfs / scripts
        self.apply_hardware_profile("powersave", "power", false, IDLE_MGLRU_MIN_TTL_MS).await;

        Ok(())
    }

    /// Clear thermal clamping if temperature and battery have recovered
    pub async fn clear_thermal_warning_if_recovered(&self, cpu_temp_c: f64, battery_pct: u32, is_discharging: bool) -> bool {
        if !self.thermal_clamped.load(Ordering::SeqCst) {
            return false;
        }

        // Recovery thresholds: temp < 72.0°C and battery >= 25% (or on AC)
        let temp_recovered = cpu_temp_c < 72.0;
        let battery_recovered = !is_discharging || battery_pct >= 25;

        if temp_recovered && battery_recovered {
            self.thermal_clamped.store(false, Ordering::SeqCst);
            self.current_concurrency.store(self.baseline_concurrency, Ordering::SeqCst);
            eprintln!(
                "✔ [Hardware Governor Synergy] Thermal warning cleared (temp: {:.1}°C, battery: {}%). Concurrency restored to {} workers.",
                cpu_temp_c, battery_pct, self.baseline_concurrency
            );
            true
        } else {
            false
        }
    }

    /// Apply idle queue drop:
    /// Immediately drops back to `powersave` and EPP `balance_power`.
    pub async fn set_idle_drop(&self) -> Result<()> {
        if self.thermal_clamped.load(Ordering::SeqCst) {
            // Already clamped to powersave/power
            return Ok(());
        }

        {
            let mut gov = self.current_governor.write().await;
            *gov = "powersave".to_string();
            let mut epp = self.current_epp.write().await;
            *epp = "balance_power".to_string();
        }
        self.turbo_enabled.store(false, Ordering::SeqCst);
        self.mglru_min_ttl_ms.store(IDLE_MGLRU_MIN_TTL_MS, Ordering::SeqCst);

        self.apply_hardware_profile("powersave", "balance_power", false, IDLE_MGLRU_MIN_TTL_MS).await;

        Ok(())
    }

    /// Read comprehensive hardware telemetry profile
    pub async fn read_telemetry(&self) -> HardwareProfileTelemetry {
        let cpu_temp_c = self.read_cpu_temp_c().await;
        let (battery_pct, is_discharging) = self.read_battery_state().await;
        let gov = self.current_governor.read().await.clone();
        let epp = self.current_epp.read().await.clone();

        HardwareProfileTelemetry {
            governor: gov,
            epp,
            turbo_enabled: self.turbo_enabled.load(Ordering::SeqCst),
            mglru_min_ttl_ms: self.mglru_min_ttl_ms.load(Ordering::SeqCst),
            cpu_temp_c,
            battery_pct,
            is_discharging,
            thermal_clamped: self.thermal_clamped.load(Ordering::SeqCst),
            current_concurrency_limit: self.current_concurrency.load(Ordering::SeqCst),
            baseline_concurrency: self.baseline_concurrency,
        }
    }

    /// Internal helper executing sysfs writes and hardware governor scripts
    async fn apply_hardware_profile(&self, governor: &str, epp: &str, enable_turbo: bool, mglru_ttl_ms: u64) {
        #[cfg(target_os = "linux")]
        {
            // 1. Direct sysfs scaling_governor and energy_performance_preference writes
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
        } else if self.dynabook_ctl_path.exists() {
            let profile = match governor {
                "performance" => "performance",
                "powersave" => "battery",
                _ => "balanced",
            };
            let _ = Command::new(&self.dynabook_ctl_path)
                .args(["profile", profile])
                .output();
        }
    }
}

/// Continuous Autonomous Feedback Engine for Self-Profiling & PGO
pub struct PgoFeedbackLoop {
    workspace_root: PathBuf,
    hotspot_file_path: PathBuf,
    pgo_script_path: PathBuf,
    tool_call_threshold: u64,
    latency_sla_p99_threshold_ms: f64,
    min_samples_for_sla: usize,
    pgo_cooldown: Duration,
    last_pgo_trigger_time: Arc<Mutex<Option<Instant>>>,
    last_trigger_reason: Arc<RwLock<Option<String>>>,
    trigger_count: Arc<AtomicU64>,
    is_pgo_running: Arc<AtomicBool>,
    tool_telemetry: Arc<RwLock<HashMap<String, ToolCallStats>>>,
    total_tool_invocations: Arc<AtomicU64>,
    token_telemetry: Arc<RwLock<TokenStreamStats>>,
    latency_history: Arc<RwLock<Vec<f64>>>,
    active_workload_counter: Arc<AtomicUsize>,
    hardware_synergy: Arc<HardwareGovernorSynergy>,
    is_running: Arc<AtomicBool>,
}

impl PgoFeedbackLoop {
    /// Construct a new PGO Feedback Loop Engine bound to workspace
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let ws = workspace_root.into();
        let hgb_dir = ws.join(".hgb");
        let _ = fs::create_dir_all(&hgb_dir);
        let hotspot_file = hgb_dir.join("pgo_hotspots.json");
        let pgo_script = ws.join("scripts/pgo-optimize.sh");
        let hardware = Arc::new(HardwareGovernorSynergy::new(&ws));

        Self {
            workspace_root: ws,
            hotspot_file_path: hotspot_file,
            pgo_script_path: pgo_script,
            tool_call_threshold: DEFAULT_HOTSPOT_TOOL_CALL_THRESHOLD,
            latency_sla_p99_threshold_ms: DEFAULT_LATENCY_SLA_P99_THRESHOLD_MS,
            min_samples_for_sla: 10,
            pgo_cooldown: Duration::from_secs(DEFAULT_PGO_COOLDOWN_SECS),
            last_pgo_trigger_time: Arc::new(Mutex::new(None)),
            last_trigger_reason: Arc::new(RwLock::new(None)),
            trigger_count: Arc::new(AtomicU64::new(0)),
            is_pgo_running: Arc::new(AtomicBool::new(false)),
            tool_telemetry: Arc::new(RwLock::new(HashMap::new())),
            total_tool_invocations: Arc::new(AtomicU64::new(0)),
            token_telemetry: Arc::new(RwLock::new(TokenStreamStats::default())),
            latency_history: Arc::new(RwLock::new(Vec::with_capacity(1000))),
            active_workload_counter: Arc::new(AtomicUsize::new(0)),
            hardware_synergy: hardware,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Configure custom hotspot threshold (tool call count)
    pub fn with_tool_call_threshold(mut self, threshold: u64) -> Self {
        self.tool_call_threshold = threshold;
        self
    }

    /// Configure custom latency SLA p99 regression threshold in milliseconds
    pub fn with_latency_sla_threshold(mut self, p99_ms: f64) -> Self {
        self.latency_sla_p99_threshold_ms = p99_ms;
        self
    }

    /// Configure custom PGO script cooldown duration
    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.pgo_cooldown = cooldown;
        self
    }

    /// Configure custom hotspot output file path
    pub fn with_hotspot_file(mut self, path: PathBuf) -> Self {
        self.hotspot_file_path = path;
        self
    }

    /// Configure custom PGO optimization script path
    pub fn with_pgo_script(mut self, path: PathBuf) -> Self {
        self.pgo_script_path = path;
        self
    }

    /// Direct reference to Hardware Governor Synergy engine
    pub fn hardware_synergy(&self) -> &Arc<HardwareGovernorSynergy> {
        &self.hardware_synergy
    }

    /// Signal that a heavy workload task has started:
    /// Escalates CPU governor to performance mode when queue transitions from idle
    pub async fn on_task_started(&self) {
        let prev = self.active_workload_counter.fetch_add(1, Ordering::SeqCst);
        if prev == 0 {
            // First task in queue -> escalate hardware to performance
            let _ = self.hardware_synergy.set_heavy_workload_boost().await;
        }
    }

    /// Signal that a task has completed:
    /// Drops CPU governor back to powersave immediately when queue drains to 0
    pub async fn on_task_completed(&self) {
        let prev = self.active_workload_counter.load(Ordering::SeqCst);
        if prev > 0 {
            self.active_workload_counter.fetch_sub(1, Ordering::SeqCst);
        }
        if prev <= 1 {
            // Queue is now idle -> immediately drop back to powersave and balance_power
            let _ = self.hardware_synergy.set_idle_drop().await;
        }
    }

    /// Record telemetry for an active WASM or native tool invocation
    pub async fn record_wasm_invocation(&self, tool_name: &str, duration_ms: f64, is_err: bool) {
        self.total_tool_invocations.fetch_add(1, Ordering::SeqCst);

        {
            let mut map = self.tool_telemetry.write().await;
            let entry = map.entry(tool_name.to_string()).or_insert_with(|| ToolCallStats::new(tool_name.to_string()));
            entry.record(duration_ms, is_err);
        }

        // Record duration into latency history
        self.record_latency(duration_ms).await;
    }

    /// Record telemetry for LLM token streaming (tokens count and stream chunk latency)
    /// Automatically applies hardware boost and MGLRU compaction when burn rate >= 80 tok/s,
    /// with strict thermal clamping at >= 78°C.
    pub async fn record_token_streaming(&self, tokens: usize, duration_ms: f64) {
        let burn_rate = if duration_ms > 0.0 {
            (tokens as f64) / (duration_ms / 1000.0)
        } else {
            0.0
        };

        {
            let mut stats = self.token_telemetry.write().await;
            stats.record(tokens, duration_ms);
        }

        // Hardware-Coordinated Thermal & Governor Binding:
        // Proactively boost CPU and configure MGLRU when token burn rate >= 80 tokens/sec
        if burn_rate >= 80.0 {
            let temp_c = self.hardware_synergy.read_cpu_temp_c().await;
            let (battery_pct, is_discharging) = self.hardware_synergy.read_battery_state().await;

            if temp_c >= THERMAL_THROTTLE_TEMP_C || (is_discharging && battery_pct <= BATTERY_DISCHARGE_CRITICAL_PCT) {
                let _ = self.hardware_synergy.set_thermal_warning(temp_c, battery_pct, is_discharging).await;
            } else {
                let _ = self.hardware_synergy.set_heavy_workload_boost().await;
            }
        }
    }

    /// Record an execution latency sample in milliseconds
    pub async fn record_latency(&self, duration_ms: f64) {
        let mut hist = self.latency_history.write().await;
        if hist.len() >= 5000 {
            // Discard oldest 1000 samples to keep memory bounded
            hist.drain(0..1000);
        }
        hist.push(duration_ms);
    }

    /// Compute live latency percentiles across recorded execution samples
    pub async fn compute_latency_percentiles(&self) -> LatencyPercentiles {
        let hist = self.latency_history.read().await;
        if hist.is_empty() {
            return LatencyPercentiles::default();
        }

        let mut sorted = hist.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let count = sorted.len();
        let p50 = sorted[(count as f64 * 0.50).floor() as usize];
        let p90 = sorted[(count as f64 * 0.90).min((count - 1) as f64).floor() as usize];
        let p95 = sorted[(count as f64 * 0.95).min((count - 1) as f64).floor() as usize];
        let p99 = sorted[(count as f64 * 0.99).min((count - 1) as f64).floor() as usize];
        let max = *sorted.last().unwrap_or(&0.0);

        LatencyPercentiles {
            sample_count: count,
            p50_ms: p50,
            p90_ms: p90,
            p95_ms: p95,
            p99_ms: p99,
            max_ms: max,
        }
    }

    /// Evaluate hardware thermal and battery limits:
    /// Clamps to powersave if temp >= 78.0°C or battery < 20% on discharge;
    /// restores concurrency if recovered.
    pub async fn evaluate_hardware_governor(&self) -> Result<()> {
        let telem = self.hardware_synergy.read_telemetry().await;

        if telem.cpu_temp_c >= THERMAL_THROTTLE_TEMP_C
            || (telem.is_discharging && telem.battery_pct < BATTERY_DISCHARGE_CRITICAL_PCT)
        {
            self.hardware_synergy
                .set_thermal_warning(telem.cpu_temp_c, telem.battery_pct, telem.is_discharging)
                .await?;
        } else if telem.thermal_clamped {
            self.hardware_synergy
                .clear_thermal_warning_if_recovered(telem.cpu_temp_c, telem.battery_pct, telem.is_discharging)
                .await;
        }

        Ok(())
    }

    /// Evaluate whether a workload hotspot or SLA regression has occurred
    pub async fn evaluate_hotspots(&self) -> Option<String> {
        let total_calls = self.total_tool_invocations.load(Ordering::SeqCst);

        // 1. Check Tool Call Hotspot Threshold (> 500 calls)
        if total_calls >= self.tool_call_threshold {
            return Some(format!(
                "Workload hotspot threshold reached: {} tool invocations (threshold: {})",
                total_calls, self.tool_call_threshold
            ));
        }

        // 2. Check Latency SLA Regression (p99 latency > SLA threshold with sufficient samples)
        let percentiles = self.compute_latency_percentiles().await;
        if percentiles.sample_count >= self.min_samples_for_sla && percentiles.p99_ms > self.latency_sla_p99_threshold_ms {
            return Some(format!(
                "Latency SLA regression detected: p99 latency {:.2}ms exceeds threshold {:.2}ms (samples: {})",
                percentiles.p99_ms, self.latency_sla_p99_threshold_ms, percentiles.sample_count
            ));
        }

        None
    }

    /// Check criteria and automatically trigger PGO optimization if thresholds are crossed
    pub async fn check_and_trigger(&self) -> Result<PgoTriggerReport> {
        // 1. Evaluate hardware thermal status first
        let _ = self.evaluate_hardware_governor().await;

        // 2. Check hotspot criteria
        let reason = match self.evaluate_hotspots().await {
            Some(r) => r,
            None => {
                return Ok(PgoTriggerReport {
                    triggered: false,
                    reason: "No hotspot threshold or SLA regression detected".to_string(),
                    snapshot_path: self.hotspot_file_path.display().to_string(),
                    pid: None,
                    message: "Workload nominal; PGO optimization withheld.".to_string(),
                });
            }
        };

        // 3. Enforce cooldown and concurrent execution guard
        if self.is_pgo_running.load(Ordering::SeqCst) {
            return Ok(PgoTriggerReport {
                triggered: false,
                reason,
                snapshot_path: self.hotspot_file_path.display().to_string(),
                pid: None,
                message: "PGO optimization already running in background.".to_string(),
            });
        }

        {
            let mut last = self.last_pgo_trigger_time.lock().await;
            if let Some(prev) = *last {
                if prev.elapsed() < self.pgo_cooldown {
                    let remaining = self.pgo_cooldown.saturating_sub(prev.elapsed()).as_secs();
                    return Ok(PgoTriggerReport {
                        triggered: false,
                        reason,
                        snapshot_path: self.hotspot_file_path.display().to_string(),
                        pid: None,
                        message: format!("PGO optimization in cooldown ({}s remaining).", remaining),
                    });
                }
            }
            *last = Some(Instant::now());
        }

        // 4. Capture snapshot and dispatch background PGO optimization
        self.dispatch_pgo_optimization(reason).await
    }

    /// Force trigger PGO optimization with an explicit reason
    pub async fn force_trigger(&self, reason: &str) -> Result<PgoTriggerReport> {
        {
            let mut last = self.last_pgo_trigger_time.lock().await;
            *last = Some(Instant::now());
        }
        self.dispatch_pgo_optimization(reason.to_string()).await
    }

    /// Capture execution profiles to `.hgb/pgo_hotspots.json` and invoke `scripts/pgo-optimize.sh`
    async fn dispatch_pgo_optimization(&self, reason: String) -> Result<PgoTriggerReport> {
        self.is_pgo_running.store(true, Ordering::SeqCst);
        self.trigger_count.fetch_add(1, Ordering::SeqCst);
        {
            let mut r = self.last_trigger_reason.write().await;
            *r = Some(reason.clone());
        }

        // 1. Gather all telemetry
        let total_calls = self.total_tool_invocations.load(Ordering::SeqCst);
        let tool_breakdown: Vec<ToolCallStats> = {
            let map = self.tool_telemetry.read().await;
            let mut list: Vec<ToolCallStats> = map.values().cloned().collect();
            list.sort_by(|a, b| b.invocations.cmp(&a.invocations));
            list
        };
        let token_stats = self.token_telemetry.read().await.clone();
        let latency = self.compute_latency_percentiles().await;
        let hardware = self.hardware_synergy.read_telemetry().await;

        // 2. Build snapshot
        let snapshot = PgoHotspotSnapshot {
            timestamp_rfc3339: chrono::Utc::now().to_rfc3339(),
            trigger_reason: reason.clone(),
            total_tool_calls: total_calls,
            tool_breakdown,
            token_streaming: token_stats,
            latency,
            hardware,
            pgo_invoked: true,
            pgo_pid: None,
            pgo_script_path: self.pgo_script_path.display().to_string(),
        };

        // 3. Serialize to .hgb/pgo_hotspots.json
        if let Some(parent) = self.hotspot_file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json_str = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        fs::write(&self.hotspot_file_path, &json_str).map_err(HgbError::Io)?;

        // 4. Invoke scripts/pgo-optimize.sh in background mode
        let mut pid = None;
        if self.pgo_script_path.exists() {
            let script = self.pgo_script_path.clone();
            let is_running_flag = Arc::clone(&self.is_pgo_running);

            // Spawn background process
            let child = Command::new("bash")
                .arg(&script)
                .arg("--pgo-only")
                .current_dir(&self.workspace_root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();

            match child {
                Ok(c) => {
                    let child_id = c.id();
                    pid = Some(child_id);
                    eprintln!(
                        "🚀 [PGO Feedback Engine] Dispatched background PGO pipeline (PID: {}): {}",
                        child_id, reason
                    );

                    // Background reaper task to clear running flag when process finishes
                    tokio::spawn(async move {
                        let mut mutable_child = c;
                        let _ = mutable_child.wait();
                        is_running_flag.store(false, Ordering::SeqCst);
                    });
                }
                Err(e) => {
                    self.is_pgo_running.store(false, Ordering::SeqCst);
                    eprintln!("[-] [PGO Feedback Engine] Failed to spawn pgo script: {}", e);
                }
            }
        } else {
            // Script not found on disk (e.g. running in stripped unit test environment)
            self.is_pgo_running.store(false, Ordering::SeqCst);
        }

        Ok(PgoTriggerReport {
            triggered: true,
            reason,
            snapshot_path: self.hotspot_file_path.display().to_string(),
            pid,
            message: "Execution snapshot captured and background PGO optimization launched.".to_string(),
        })
    }

    /// Read comprehensive status report of the PGO Feedback Loop Engine
    pub async fn status(&self) -> PgoFeedbackStatusReport {
        let total_tool_calls = self.total_tool_invocations.load(Ordering::SeqCst);
        let tool_types_count = self.tool_telemetry.read().await.len();
        let token_stats = self.token_telemetry.read().await.clone();
        let latency_percentiles = self.compute_latency_percentiles().await;
        let hardware = self.hardware_synergy.read_telemetry().await;
        let last_trigger_reason = self.last_trigger_reason.read().await.clone();
        let is_pgo_running = self.is_pgo_running.load(Ordering::SeqCst);
        let trigger_count = self.trigger_count.load(Ordering::SeqCst);

        PgoFeedbackStatusReport {
            is_active: self.is_running.load(Ordering::SeqCst),
            is_pgo_running,
            total_tool_calls,
            tool_types_count,
            token_stats,
            latency_percentiles,
            hardware,
            last_trigger_reason,
            last_trigger_epoch_secs: self.last_pgo_trigger_time.lock().await.map(|_| current_epoch_secs()),
            trigger_count,
        }
    }

    /// Start the autonomous background monitoring loop
    pub fn start_monitoring_loop(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        self.is_running.store(true, Ordering::SeqCst);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            while self.is_running.load(Ordering::SeqCst) {
                ticker.tick().await;

                // 1. Evaluate hardware thermal states
                let _ = self.evaluate_hardware_governor().await;

                // 2. Check idle queue state
                if self.active_workload_counter.load(Ordering::SeqCst) == 0 {
                    let _ = self.hardware_synergy.set_idle_drop().await;
                }

                // 3. Evaluate hotspot & SLA criteria
                let _ = self.check_and_trigger().await;
            }
        })
    }

    /// Stop the autonomous monitoring loop
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tool_telemetry_aggregation() {
        let temp_dir = std::env::temp_dir().join(format!("pgo_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let engine = PgoFeedbackLoop::new(&temp_dir)
            .with_tool_call_threshold(10)
            .with_latency_sla_threshold(100.0);

        // Record several invocations
        engine.record_wasm_invocation("ast_parser", 12.5, false).await;
        engine.record_wasm_invocation("ast_parser", 15.0, false).await;
        engine.record_wasm_invocation("ast_parser", 25.0, true).await;
        engine.record_wasm_invocation("json_squeezer", 5.0, false).await;

        let status = engine.status().await;
        assert_eq!(status.total_tool_calls, 4);
        assert_eq!(status.tool_types_count, 2);

        let map = engine.tool_telemetry.read().await;
        let ast = map.get("ast_parser").expect("ast_parser exists");
        assert_eq!(ast.invocations, 3);
        assert_eq!(ast.error_count, 1);
        assert!((ast.avg_duration_ms - 17.5).abs() < 0.1);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_token_streaming_telemetry() {
        let temp_dir = std::env::temp_dir().join(format!("pgo_token_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let engine = PgoFeedbackLoop::new(&temp_dir);
        // Stream 1: 50 tokens in 500ms (100 tps)
        engine.record_token_streaming(50, 500.0).await;
        // Stream 2: 100 tokens in 500ms (200 tps)
        engine.record_token_streaming(100, 500.0).await;

        let status = engine.status().await;
        assert_eq!(status.token_stats.total_tokens, 150);
        assert_eq!(status.token_stats.stream_count, 2);
        assert!((status.token_stats.tokens_per_second - 150.0).abs() < 1.0);
        assert!((status.token_stats.peak_tokens_per_second - 200.0).abs() < 1.0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_latency_percentiles_calculation() {
        let temp_dir = std::env::temp_dir().join(format!("pgo_lat_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let engine = PgoFeedbackLoop::new(&temp_dir);
        for i in 1..=100 {
            engine.record_latency(i as f64).await;
        }

        let p = engine.compute_latency_percentiles().await;
        assert_eq!(p.sample_count, 100);
        assert!((p.p50_ms - 51.0).abs() <= 1.0);
        assert!((p.p90_ms - 91.0).abs() <= 1.0);
        assert!((p.p99_ms - 100.0).abs() <= 1.0);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_hotspot_threshold_trigger() {
        let temp_dir = std::env::temp_dir().join(format!("pgo_hotspot_trig_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let engine = PgoFeedbackLoop::new(&temp_dir)
            .with_tool_call_threshold(5)
            .with_cooldown(Duration::from_millis(50));

        // Prior to threshold: should not trigger
        for _ in 0..4 {
            engine.record_wasm_invocation("quick_tool", 2.0, false).await;
        }
        let report1 = engine.check_and_trigger().await.unwrap();
        assert!(!report1.triggered);

        // 5th call reaches threshold
        engine.record_wasm_invocation("quick_tool", 2.0, false).await;
        let report2 = engine.check_and_trigger().await.unwrap();
        assert!(report2.triggered);
        assert!(report2.reason.contains("Workload hotspot threshold reached"));

        // Verify .hgb/pgo_hotspots.json was created
        let hotspot_file = temp_dir.join(".hgb/pgo_hotspots.json");
        assert!(hotspot_file.exists());
        let content = fs::read_to_string(&hotspot_file).unwrap();
        assert!(content.contains("quick_tool"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_latency_sla_regression_trigger() {
        let temp_dir = std::env::temp_dir().join(format!("pgo_sla_trig_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let engine = PgoFeedbackLoop::new(&temp_dir)
            .with_tool_call_threshold(1000) // high so it doesn't trigger on call count
            .with_latency_sla_threshold(300.0)
            .with_cooldown(Duration::from_millis(50));

        // Insert 20 normal samples
        for _ in 0..20 {
            engine.record_latency(50.0).await;
        }
        let rep1 = engine.check_and_trigger().await.unwrap();
        assert!(!rep1.triggered);

        // Spike 2 samples above SLA
        engine.record_latency(650.0).await;
        engine.record_latency(750.0).await;

        let rep2 = engine.check_and_trigger().await.unwrap();
        assert!(rep2.triggered);
        assert!(rep2.reason.contains("Latency SLA regression detected"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_hardware_governor_scaling_and_thermal_clamping() {
        let temp_dir = std::env::temp_dir().join(format!("pgo_hw_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let engine = PgoFeedbackLoop::new(&temp_dir);
        let hw = engine.hardware_synergy();

        // 1. Initial idle state
        engine.on_task_completed().await;
        let telem1 = hw.read_telemetry().await;
        assert_eq!(telem1.governor, "powersave");
        assert_eq!(telem1.epp, "balance_power");
        assert_eq!(telem1.mglru_min_ttl_ms, 0);

        // 2. Heavy workload starts -> boost to performance
        engine.on_task_started().await;
        let telem2 = hw.read_telemetry().await;
        assert_eq!(telem2.governor, "performance");
        assert_eq!(telem2.epp, "performance");
        assert!(telem2.turbo_enabled);
        assert_eq!(telem2.mglru_min_ttl_ms, BURST_MGLRU_MIN_TTL_MS);

        // 3. Thermal warning occurs (simulated 82.0°C)
        hw.set_simulated_temp(Some(82.0)).await;
        engine.evaluate_hardware_governor().await.unwrap();

        let telem3 = hw.read_telemetry().await;
        assert!(telem3.thermal_clamped);
        assert_eq!(telem3.governor, "powersave");
        assert_eq!(telem3.epp, "power");
        assert!(!telem3.turbo_enabled);
        assert!(telem3.current_concurrency_limit < telem3.baseline_concurrency || telem3.current_concurrency_limit == 1);

        // 4. While thermal clamped, workload boost cannot override safety
        hw.set_heavy_workload_boost().await.unwrap();
        let telem4 = hw.read_telemetry().await;
        assert_eq!(telem4.governor, "powersave"); // Still clamped

        // 5. Thermal recovery (simulated 65.0°C)
        hw.set_simulated_temp(Some(65.0)).await;
        engine.evaluate_hardware_governor().await.unwrap();

        let telem5 = hw.read_telemetry().await;
        assert!(!telem5.thermal_clamped);
        assert_eq!(telem5.current_concurrency_limit, telem5.baseline_concurrency);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
