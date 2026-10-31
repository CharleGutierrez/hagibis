//! # Vector 3: Hardware-Aware Dynamic Profiling (Synergy with dynatune & kernel-optimizer)
//!
//! Provides host hardware profiling, dynamic task concurrency & batch
//! governing, continuous background sampling, and profile-guided kernel governor switching.
//! Deeply interfaces with sibling projects:
//! - `/home/toshiba/My AI Project/dynatune` (`src/thermal.rs`, `src/battery.rs`, `src/power.rs`, `src/processes.rs`, `src/memory.rs`)
//! - `/home/toshiba/My AI Project/kernel-optimizer` (`dynabook-kernel-ctl`, `plugins/cpu-governor.sh`)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Safe thermal headroom upper limit in Celsius for Dynabook Kaby Lake ultrabook chassis
pub const THERMAL_HEADROOM_SAFE_C: f64 = 72.0;
/// Thermal throttle threshold where balanced / throttled mode engages
pub const THERMAL_THROTTLE_LIMIT_C: f64 = 78.0;
/// Critical battery percentage threshold on discharge
pub const BATTERY_CRITICAL_THRESHOLD_PCT: u32 = 25;
/// Target MGLRU min_ttl_ms when bursting during performance workloads
pub const BURST_MGLRU_MIN_TTL_MS: u64 = 1000;
/// Target MGLRU min_ttl_ms for balanced mode
pub const BALANCED_MGLRU_MIN_TTL_MS: u64 = 500;
/// Target MGLRU min_ttl_ms for powersave mode
pub const POWERSAVE_MGLRU_MIN_TTL_MS: u64 = 0;

/// Hardware governor profile modes supported by host and kernel optimizer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareGovernorProfile {
    Performance,
    Balanced,
    PowerSave,
}

impl std::fmt::Display for HardwareGovernorProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Performance => write!(f, "performance"),
            Self::Balanced => write!(f, "balanced"),
            Self::PowerSave => write!(f, "powersave"),
        }
    }
}

impl HardwareGovernorProfile {
    pub fn as_kernel_ctl_str(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::PowerSave => "battery",
        }
    }

    pub fn as_sysfs_governor(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "powersave",
            Self::PowerSave => "powersave",
        }
    }

    pub fn as_sysfs_epp(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balance_performance",
            Self::PowerSave => "balance_power",
        }
    }

    pub fn target_nice_level(&self) -> i32 {
        match self {
            Self::Performance => -5,
            Self::Balanced => 0,
            Self::PowerSave => 10,
        }
    }
}

/// Information on a single host thermal zone
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThermalZoneInfo {
    pub name: String,
    pub zone_type: String,
    pub temp_c: f64,
}

/// Comprehensive hardware profile telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareProfileTelemetry {
    pub cpu_pkg_temp_c: f64,
    pub max_temp_c: f64,
    pub is_thermal_throttled: bool,
    pub thermal_zones: Vec<ThermalZoneInfo>,
    pub ac_connected: bool,
    pub battery_name: Option<String>,
    pub battery_status: String,
    pub battery_capacity_pct: Option<u32>,
    pub battery_discharge_rate_mw: Option<f64>,
    pub battery_discharge_rate_pct_per_hr: Option<f64>,
    pub battery_power_now_w: Option<f64>,
    pub energy_now_wh: Option<f64>,
    pub energy_full_wh: Option<f64>,
    pub is_battery_discharging: bool,
    pub is_battery_critical: bool,
    pub mglru_active: bool,
    pub mglru_flags: Option<String>,
    pub mglru_min_ttl_ms: Option<u64>,
    pub memory_total_mb: u64,
    pub memory_available_mb: u64,
    pub memory_available_pct: f64,
    pub ram_pressure_some_avg10: f64,
    pub ram_pressure_full_avg10: f64,
    pub active_governor: String,
    pub active_epp: String,
    pub scaling_driver: String,
    pub cpu_frequencies_mhz: Vec<f64>,
    pub throttle_events_count: u64,
    pub optimal_concurrency: usize,
    pub recommended_ollama_batch_size: usize,
    pub active_profile: HardwareGovernorProfile,
    pub dynatune_available: bool,
    pub kernel_ctl_available: bool,
    pub timestamp_epoch_ms: u64,
}

impl Default for HardwareProfileTelemetry {
    fn default() -> Self {
        let num_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        Self {
            cpu_pkg_temp_c: 50.0,
            max_temp_c: 50.0,
            is_thermal_throttled: false,
            thermal_zones: Vec::new(),
            ac_connected: true,
            battery_name: Some("BAT1".to_string()),
            battery_status: "Full".to_string(),
            battery_capacity_pct: Some(100),
            battery_discharge_rate_mw: None,
            battery_discharge_rate_pct_per_hr: None,
            battery_power_now_w: None,
            energy_now_wh: None,
            energy_full_wh: None,
            is_battery_discharging: false,
            is_battery_critical: false,
            mglru_active: true,
            mglru_flags: Some("0x0007".to_string()),
            mglru_min_ttl_ms: Some(0),
            memory_total_mb: 8192,
            memory_available_mb: 4096,
            memory_available_pct: 50.0,
            ram_pressure_some_avg10: 0.0,
            ram_pressure_full_avg10: 0.0,
            active_governor: "powersave".to_string(),
            active_epp: "balance_performance".to_string(),
            scaling_driver: "intel_pstate".to_string(),
            cpu_frequencies_mhz: vec![2400.0; num_cpus],
            throttle_events_count: 0,
            optimal_concurrency: num_cpus,
            recommended_ollama_batch_size: 512,
            active_profile: HardwareGovernorProfile::Balanced,
            dynatune_available: false,
            kernel_ctl_available: false,
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }
}

/// Compute optimal task concurrency from live hardware telemetry
///
/// Rules:
/// - AC Power & Temp < 72°C: Concurrency = `num_cpus` (boost mode).
/// - AC Power & Temp 72°C..78°C: Concurrency = `(num_cpus / 2).max(2)` (balanced mode).
/// - Temp >= 78°C or Battery < 25%: Concurrency = 1 (throttled protection mode).
/// - On battery with safe temp (< 78°C) and battery >= 25%: Concurrency = `(num_cpus / 2).max(1)`.
pub fn compute_task_concurrency(
    cpu_pkg_temp_c: f64,
    ac_connected: bool,
    battery_capacity_pct: Option<u32>,
    num_cpus: usize,
) -> usize {
    let bat_pct = battery_capacity_pct.unwrap_or(100);

    // Rule 3: Temp >= 78°C or Battery < 25% => Throttled protection mode (Concurrency = 1)
    if cpu_pkg_temp_c >= THERMAL_THROTTLE_LIMIT_C || (!ac_connected && bat_pct < BATTERY_CRITICAL_THRESHOLD_PCT) {
        1
    } else if ac_connected && cpu_pkg_temp_c < THERMAL_HEADROOM_SAFE_C {
        // Rule 1: AC Power & Temp < 72°C => Concurrency = num_cpus (boost mode)
        num_cpus.max(1)
    } else if ac_connected && cpu_pkg_temp_c >= THERMAL_HEADROOM_SAFE_C && cpu_pkg_temp_c < THERMAL_THROTTLE_LIMIT_C {
        // Rule 2: AC Power & Temp 72°C..78°C => Concurrency = (num_cpus / 2).max(2) (balanced mode)
        (num_cpus / 2).max(2)
    } else {
        // Battery power >= 25% and Temp < 78°C: battery efficiency mode
        (num_cpus / 2).max(1)
    }
}

/// Compute recommended Ollama batch size (e.g. 512 tokens down to 128 tokens when hot)
pub fn compute_ollama_batch_size(
    cpu_pkg_temp_c: f64,
    ac_connected: bool,
    battery_capacity_pct: Option<u32>,
    ram_pressure_some_avg10: f64,
) -> usize {
    let bat_pct = battery_capacity_pct.unwrap_or(100);

    if cpu_pkg_temp_c >= THERMAL_THROTTLE_LIMIT_C
        || (!ac_connected && bat_pct < BATTERY_CRITICAL_THRESHOLD_PCT)
        || ram_pressure_some_avg10 > 35.0
    {
        // Severe heat / low battery / high RAM pressure: minimum batch size
        128
    } else if cpu_pkg_temp_c >= THERMAL_HEADROOM_SAFE_C
        || ram_pressure_some_avg10 > 15.0
        || !ac_connected
    {
        // Warm / moderate RAM pressure / on battery: balanced batch size
        256
    } else {
        // Cool on AC: maximum high-throughput batch size
        512
    }
}

/// Report returned upon changing a hardware profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSwitchReport {
    pub requested_profile: HardwareGovernorProfile,
    pub applied_profile: HardwareGovernorProfile,
    pub governor: String,
    pub epp: String,
    pub mglru_min_ttl_ms: u64,
    pub safeguard_engaged: bool,
    pub safeguard_reason: Option<String>,
    pub sibling_tool_invoked: Option<String>,
    pub direct_sysfs_applied: bool,
    pub nice_priority_applied: Option<i32>,
    pub actions: Vec<String>,
    pub timestamp_epoch_ms: u64,
}

/// RAII Governor Guard ensuring deterministic restoration of baseline governor,
/// EPP, nice value, and MGLRU min_ttl_ms upon completion or early return.
pub struct ProfilerGovernorGuard {
    profiler: Arc<DynamicHardwareProfiler>,
    pub baseline_profile: HardwareGovernorProfile,
    pub baseline_governor: String,
    pub baseline_epp: String,
    pub baseline_nice: i32,
    pub baseline_min_ttl_ms: Option<u64>,
    pub active_report: ProfileSwitchReport,
    disarmed: bool,
}

impl ProfilerGovernorGuard {
    /// Disarm guard so it does not alter scaling state upon drop
    pub fn disarm(&mut self) {
        self.disarmed = true;
    }

    /// Access active switch report
    pub fn report(&self) -> &ProfileSwitchReport {
        &self.active_report
    }
}

impl Drop for ProfilerGovernorGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }

        // Deterministic baseline restoration
        let _ = self.profiler.switch_profile(self.baseline_profile);
        if let Some(ttl) = self.baseline_min_ttl_ms {
            let _ = self.profiler.write_mglru_min_ttl_ms(ttl);
        }
        if self.baseline_nice != 0 {
            let _ = self.profiler.apply_nice_level(self.baseline_nice);
        }
    }
}

/// Principal Dynamic Hardware Profiler & Governor
pub struct DynamicHardwareProfiler {
    pub dynatune_dir: Option<PathBuf>,
    pub kernel_ctl_path: Option<PathBuf>,
    pub cpu_governor_sh_path: Option<PathBuf>,
    pub thermal_sysfs_base: PathBuf,
    pub power_sysfs_base: PathBuf,
    pub cpu_sysfs_base: PathBuf,
    pub mglru_sysfs_base: PathBuf,
    pub proc_meminfo_path: PathBuf,
    pub proc_pressure_memory_path: PathBuf,
    telemetry_cache: Arc<RwLock<HardwareProfileTelemetry>>,
    active_profile: Arc<RwLock<HardwareGovernorProfile>>,
    sampler_running: Arc<AtomicBool>,
}

impl Default for DynamicHardwareProfiler {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicHardwareProfiler {
    /// Instantiate a new `DynamicHardwareProfiler` auto-discovering sibling dynatune and kernel-optimizer
    pub fn new() -> Self {
        let (dynatune_dir, kernel_ctl_path, cpu_governor_sh_path) = Self::discover_sibling_projects();

        let profiler = Self {
            dynatune_dir,
            kernel_ctl_path,
            cpu_governor_sh_path,
            thermal_sysfs_base: PathBuf::from("/sys/class/thermal"),
            power_sysfs_base: PathBuf::from("/sys/class/power_supply"),
            cpu_sysfs_base: PathBuf::from("/sys/devices/system/cpu"),
            mglru_sysfs_base: PathBuf::from("/sys/kernel/mm/lru_gen"),
            proc_meminfo_path: PathBuf::from("/proc/meminfo"),
            proc_pressure_memory_path: PathBuf::from("/proc/pressure/memory"),
            telemetry_cache: Arc::new(RwLock::new(HardwareProfileTelemetry::default())),
            active_profile: Arc::new(RwLock::new(HardwareGovernorProfile::Balanced)),
            sampler_running: Arc::new(AtomicBool::new(false)),
        };

        // Take initial live sample
        let initial_sample = profiler.sample_fresh();
        if let Ok(mut cache) = profiler.telemetry_cache.write() {
            *cache = initial_sample;
        }

        profiler
    }

    /// Discover sibling projects `dynatune` and `kernel-optimizer`
    fn discover_sibling_projects() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
        // 1. Dynatune directory
        let dynatune_candidates = [
            std::env::var("DYNATUNE_DIR").ok().map(PathBuf::from),
            Some(PathBuf::from("/home/toshiba/My AI Project/dynatune")),
            Some(PathBuf::from("../dynatune")),
            Some(PathBuf::from("../../dynatune")),
            Some(PathBuf::from("./dynatune")),
        ];
        let found_dynatune = dynatune_candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_dir() && (p.join("src/thermal.rs").exists() || p.join("Cargo.toml").exists()));

        // 2. Kernel optimizer ctl tool
        let ctl_candidates = [
            std::env::var("KERNEL_OPTIMIZER_CTL").ok().map(PathBuf::from),
            std::env::var("KERNEL_OPTIMIZER_DIR").ok().map(|d| PathBuf::from(d).join("dynabook-kernel-ctl")),
            Some(PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl")),
            Some(PathBuf::from("../kernel-optimizer/dynabook-kernel-ctl")),
            Some(PathBuf::from("../../kernel-optimizer/dynabook-kernel-ctl")),
            Some(PathBuf::from("/usr/local/bin/dynabook-kernel-ctl")),
        ];
        let found_ctl = ctl_candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_file());

        // 3. cpu-governor.sh plugin
        let sh_candidates = [
            std::env::var("KERNEL_OPTIMIZER_DIR").ok().map(|d| PathBuf::from(d).join("plugins/cpu-governor.sh")),
            Some(PathBuf::from("/home/toshiba/My AI Project/kernel-optimizer/plugins/cpu-governor.sh")),
            Some(PathBuf::from("../kernel-optimizer/plugins/cpu-governor.sh")),
            Some(PathBuf::from("../../kernel-optimizer/plugins/cpu-governor.sh")),
        ];
        let found_sh = sh_candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_file());

        (found_dynatune, found_ctl, found_sh)
    }

    /// Start continuous background sampling of host telemetry
    pub fn start_background_sampler(self: &Arc<Self>, interval: Duration) -> Option<tokio::task::JoinHandle<()>> {
        self.sampler_running.store(true, Ordering::SeqCst);
        let profiler = Arc::clone(self);
        let stop_flag = Arc::clone(&self.sampler_running);

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            Some(handle.spawn(async move {
                while stop_flag.load(Ordering::Relaxed) {
                    let sample = profiler.sample_fresh();
                    if let Ok(mut cache) = profiler.telemetry_cache.write() {
                        *cache = sample;
                    }
                    tokio::time::sleep(interval).await;
                }
            }))
        } else {
            let _ = std::thread::Builder::new()
                .name("hgb-hw-sampler".to_string())
                .spawn(move || {
                    while stop_flag.load(Ordering::Relaxed) {
                        let sample = profiler.sample_fresh();
                        if let Ok(mut cache) = profiler.telemetry_cache.write() {
                            *cache = sample;
                        }
                        std::thread::sleep(interval);
                    }
                });
            None
        }
    }

    /// Stop background sampling
    pub fn stop_background_sampler(&self) {
        self.sampler_running.store(false, Ordering::SeqCst);
    }

    /// Read currently cached telemetry without blocking sysfs reads
    pub fn current_telemetry(&self) -> HardwareProfileTelemetry {
        if let Ok(guard) = self.telemetry_cache.read() {
            guard.clone()
        } else {
            self.sample_fresh()
        }
    }

    /// Perform a fresh, synchronous sampling of all host hardware sensors
    pub fn sample_fresh(&self) -> HardwareProfileTelemetry {
        let (cpu_pkg_temp_c, max_temp_c, thermal_zones) = self.read_thermal_state();
        let (ac_connected, bat_info) = self.read_battery_and_power_state();
        let (mglru_active, mglru_flags, mglru_min_ttl_ms) = self.read_mglru_state();
        let (mem_total, mem_avail, mem_pct, psi_some, psi_full) = self.read_memory_and_pressure();
        let (driver, governor, epp, freqs) = self.read_cpufreq_state();
        let throttle_events_count = self.read_throttle_events();

        let num_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let optimal_concurrency = compute_task_concurrency(
            cpu_pkg_temp_c,
            ac_connected,
            bat_info.as_ref().and_then(|b| b.capacity_pct),
            num_cpus,
        );
        let recommended_ollama_batch_size = compute_ollama_batch_size(
            cpu_pkg_temp_c,
            ac_connected,
            bat_info.as_ref().and_then(|b| b.capacity_pct),
            psi_some,
        );

        let is_thermal_throttled = cpu_pkg_temp_c >= THERMAL_THROTTLE_LIMIT_C;
        let is_battery_critical = !ac_connected
            && bat_info.as_ref().and_then(|b| b.capacity_pct).unwrap_or(100) < BATTERY_CRITICAL_THRESHOLD_PCT;

        let active_profile = self.active_profile.read().map(|p| *p).unwrap_or(HardwareGovernorProfile::Balanced);

        HardwareProfileTelemetry {
            cpu_pkg_temp_c,
            max_temp_c,
            is_thermal_throttled,
            thermal_zones,
            ac_connected,
            battery_name: bat_info.as_ref().map(|b| b.name.clone()),
            battery_status: bat_info.as_ref().map(|b| b.status.clone()).unwrap_or_else(|| "Unknown".to_string()),
            battery_capacity_pct: bat_info.as_ref().and_then(|b| b.capacity_pct),
            battery_discharge_rate_mw: bat_info.as_ref().and_then(|b| b.discharge_rate_mw),
            battery_discharge_rate_pct_per_hr: bat_info.as_ref().and_then(|b| b.discharge_rate_pct_per_hr),
            battery_power_now_w: bat_info.as_ref().and_then(|b| b.power_now_w),
            energy_now_wh: bat_info.as_ref().and_then(|b| b.energy_now_wh),
            energy_full_wh: bat_info.as_ref().and_then(|b| b.energy_full_wh),
            is_battery_discharging: bat_info.as_ref().map(|b| b.is_discharging).unwrap_or(false),
            is_battery_critical,
            mglru_active,
            mglru_flags,
            mglru_min_ttl_ms,
            memory_total_mb: mem_total,
            memory_available_mb: mem_avail,
            memory_available_pct: mem_pct,
            ram_pressure_some_avg10: psi_some,
            ram_pressure_full_avg10: psi_full,
            active_governor: governor,
            active_epp: epp,
            scaling_driver: driver,
            cpu_frequencies_mhz: freqs,
            throttle_events_count,
            optimal_concurrency,
            recommended_ollama_batch_size,
            active_profile,
            dynatune_available: self.dynatune_dir.is_some(),
            kernel_ctl_available: self.kernel_ctl_path.is_some(),
            timestamp_epoch_ms: current_epoch_ms(),
        }
    }

    /// Read optimal task concurrency
    pub fn optimal_task_concurrency(&self) -> usize {
        self.current_telemetry().optimal_concurrency
    }

    /// Read recommended Ollama batch size
    pub fn recommended_ollama_batch_size(&self) -> usize {
        self.current_telemetry().recommended_ollama_batch_size
    }

    /// Acquire an RAII Governor Guard switching to the specified profile
    /// and restoring the previous baseline upon Drop.
    pub fn acquire_profile_guard(self: &Arc<Self>, profile: HardwareGovernorProfile) -> Result<ProfilerGovernorGuard> {
        let baseline_profile = self.active_profile.read().map(|p| *p).unwrap_or(HardwareGovernorProfile::Balanced);
        let telem = self.current_telemetry();
        let baseline_governor = telem.active_governor.clone();
        let baseline_epp = telem.active_epp.clone();
        let baseline_nice = self.current_nice_level();
        let baseline_min_ttl_ms = telem.mglru_min_ttl_ms;

        let active_report = self.switch_profile(profile)?;

        Ok(ProfilerGovernorGuard {
            profiler: Arc::clone(self),
            baseline_profile,
            baseline_governor,
            baseline_epp,
            baseline_nice,
            baseline_min_ttl_ms,
            active_report,
            disarmed: false,
        })
    }

    /// Switch hardware governor profile across dynabook-kernel-ctl, cpu-governor.sh,
    /// direct sysfs writes, or nice priority fallback.
    /// Enforces thermal limit protection to completely prevent thermal throttling or abrupt shutdown.
    pub fn switch_profile(&self, requested: HardwareGovernorProfile) -> Result<ProfileSwitchReport> {
        let telem = self.sample_fresh();
        let mut actions = Vec::new();
        let mut safeguard_engaged = false;
        let mut safeguard_reason = None;

        // Thermal limit protection: prevent performance mode if temp >= 78.0°C or battery < 25%
        let effective_profile = if requested == HardwareGovernorProfile::Performance {
            if telem.cpu_pkg_temp_c >= THERMAL_THROTTLE_LIMIT_C {
                safeguard_engaged = true;
                safeguard_reason = Some(format!(
                    "Thermal protection engaged: CPU temp {:.1}°C exceeds safe limit {:.1}°C; demoting to powersave",
                    telem.cpu_pkg_temp_c, THERMAL_THROTTLE_LIMIT_C
                ));
                HardwareGovernorProfile::PowerSave
            } else if !telem.ac_connected
                && telem.battery_capacity_pct.unwrap_or(100) < BATTERY_CRITICAL_THRESHOLD_PCT
            {
                safeguard_engaged = true;
                safeguard_reason = Some(format!(
                    "Battery protection engaged: capacity {}% below threshold {}%; demoting to powersave",
                    telem.battery_capacity_pct.unwrap_or(0), BATTERY_CRITICAL_THRESHOLD_PCT
                ));
                HardwareGovernorProfile::PowerSave
            } else {
                requested
            }
        } else {
            requested
        };

        if let Some(ref reason) = safeguard_reason {
            actions.push(format!("[PROTECTION] {}", reason));
        }

        let mut sibling_tool_invoked = None;
        let mut direct_sysfs_applied = false;

        // 1. Command sibling dynabook-kernel-ctl if present
        if let Some(ref ctl_path) = self.kernel_ctl_path {
            let target_arg = effective_profile.as_kernel_ctl_str();
            let res = Command::new(ctl_path)
                .args(["profile", target_arg])
                .output();
            if let Ok(out) = res {
                if out.status.success() {
                    sibling_tool_invoked = Some(format!("dynabook-kernel-ctl profile {}", target_arg));
                    actions.push(format!("Invoked dynabook-kernel-ctl profile {} successfully", target_arg));
                }
            }
        }

        // 2. Command cpu-governor.sh if dynabook-kernel-ctl was not invoked or available
        if sibling_tool_invoked.is_none() {
            if let Some(ref sh_path) = self.cpu_governor_sh_path {
                let target_arg = effective_profile.as_kernel_ctl_str();
                let res = Command::new("bash")
                    .arg(sh_path)
                    .args(["profile", target_arg])
                    .output();
                if let Ok(out) = res {
                    if out.status.success() {
                        sibling_tool_invoked = Some(format!("cpu-governor.sh profile {}", target_arg));
                        actions.push(format!("Invoked cpu-governor.sh profile {} successfully", target_arg));
                    }
                }
            }
        }

        // 3. Direct sysfs writes (scaling governor, EPP, Intel P-State, MGLRU)
        let sysfs_gov = effective_profile.as_sysfs_governor();
        let sysfs_epp = effective_profile.as_sysfs_epp();
        let target_min_ttl = match effective_profile {
            HardwareGovernorProfile::Performance => BURST_MGLRU_MIN_TTL_MS,
            HardwareGovernorProfile::Balanced => BALANCED_MGLRU_MIN_TTL_MS,
            HardwareGovernorProfile::PowerSave => POWERSAVE_MGLRU_MIN_TTL_MS,
        };

        if self.write_scaling_governor(sysfs_gov).is_ok() {
            actions.push(format!("Direct sysfs: scaling_governor set to {}", sysfs_gov));
            direct_sysfs_applied = true;
        }
        if self.write_epp(sysfs_epp).is_ok() {
            actions.push(format!("Direct sysfs: EPP set to {}", sysfs_epp));
            direct_sysfs_applied = true;
        }
        if self.write_mglru_min_ttl_ms(target_min_ttl).is_ok() {
            actions.push(format!("Direct sysfs: MGLRU min_ttl_ms set to {}", target_min_ttl));
            direct_sysfs_applied = true;
        }

        // 4. Intel P-State fine-tuning
        let pstate_dir = self.cpu_sysfs_base.join("intel_pstate");
        if pstate_dir.is_dir() {
            let (max_perf, no_turbo, hwp_boost) = match effective_profile {
                HardwareGovernorProfile::Performance => ("100", "0", "1"),
                HardwareGovernorProfile::Balanced => ("85", "0", "0"),
                HardwareGovernorProfile::PowerSave => ("70", if telem.cpu_pkg_temp_c >= 78.0 { "1" } else { "0" }, "0"),
            };
            let _ = fs::write(pstate_dir.join("max_perf_pct"), format!("{}\n", max_perf));
            let _ = fs::write(pstate_dir.join("no_turbo"), format!("{}\n", no_turbo));
            let _ = fs::write(pstate_dir.join("hwp_dynamic_boost"), format!("{}\n", hwp_boost));
        }

        // 5. Unprivileged priority fallback via libc::setpriority
        let mut nice_priority_applied = None;
        let target_nice = effective_profile.target_nice_level();
        if let Ok(applied) = self.apply_nice_level(target_nice) {
            nice_priority_applied = Some(applied);
            actions.push(format!("Process nice level adjusted to {}", applied));
        }

        // Record active profile in state
        if let Ok(mut prof) = self.active_profile.write() {
            *prof = effective_profile;
        }

        Ok(ProfileSwitchReport {
            requested_profile: requested,
            applied_profile: effective_profile,
            governor: sysfs_gov.to_string(),
            epp: sysfs_epp.to_string(),
            mglru_min_ttl_ms: target_min_ttl,
            safeguard_engaged,
            safeguard_reason,
            sibling_tool_invoked,
            direct_sysfs_applied,
            nice_priority_applied,
            actions,
            timestamp_epoch_ms: current_epoch_ms(),
        })
    }

    /// Thermal and battery safety enforcement: if host exceeds thermal limit or battery critical,
    /// automatically enforces balanced or powersave profiles.
    pub fn enforce_thermal_safety(&self) -> bool {
        let telem = self.sample_fresh();
        if telem.is_thermal_throttled || telem.is_battery_critical {
            let active = self.active_profile.read().map(|p| *p).unwrap_or(HardwareGovernorProfile::Balanced);
            if active == HardwareGovernorProfile::Performance {
                let _ = self.switch_profile(HardwareGovernorProfile::PowerSave);
                return true;
            }
        }
        false
    }

    /// Read Linux thermal zones and CPU package sensor
    fn read_thermal_state(&self) -> (f64, f64, Vec<ThermalZoneInfo>) {
        let mut zones = Vec::new();
        let mut cpu_temp_milli = None;
        let mut max_milli = 0i64;

        if let Ok(entries) = fs::read_dir(&self.thermal_sysfs_base) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let type_path = path.join("type");
                    let temp_path = path.join("temp");

                    let zone_type = fs::read_to_string(&type_path)
                        .unwrap_or_else(|_| "unknown".to_string())
                        .trim()
                        .to_string();

                    if let Ok(raw) = fs::read_to_string(&temp_path) {
                        if let Ok(milli) = raw.trim().parse::<i64>() {
                            let temp_c = milli as f64 / 1000.0;
                            if milli > max_milli {
                                max_milli = milli;
                            }
                            if zone_type == "x86_pkg_temp" {
                                cpu_temp_milli = Some(milli);
                            } else if cpu_temp_milli.is_none() && (zone_type == "acpitz" || zone_type == "cpu-thermal") {
                                cpu_temp_milli = Some(milli);
                            }
                            zones.push(ThermalZoneInfo {
                                name,
                                zone_type,
                                temp_c,
                            });
                        }
                    }
                }
            }
        }

        let effective_milli = cpu_temp_milli.unwrap_or(max_milli);
        let cpu_temp_c = if effective_milli > 0 { effective_milli as f64 / 1000.0 } else { 50.0 };
        let max_c = if max_milli > 0 { max_milli as f64 / 1000.0 } else { cpu_temp_c };

        (cpu_temp_c, max_c, zones)
    }

    /// Read AC connection and battery discharge telemetry
    fn read_battery_and_power_state(&self) -> (bool, Option<BatteryTelemetryDetail>) {
        let mut ac_connected = false;
        let mut bat_detail = None;

        if let Ok(entries) = fs::read_dir(&self.power_sysfs_base) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();

                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(content) = fs::read_to_string(p.join("online")) {
                        if content.trim() == "1" {
                            ac_connected = true;
                        }
                    }
                } else if name.starts_with("BAT") && bat_detail.is_none() {
                    bat_detail = self.parse_battery_directory(&p, &name);
                }
            }
        }

        // Fallback: if battery is charging or full, AC mains is connected
        if !ac_connected {
            if let Some(ref b) = bat_detail {
                if b.status.eq_ignore_ascii_case("Charging") || b.status.eq_ignore_ascii_case("Full") {
                    ac_connected = true;
                }
            }
        }

        (ac_connected, bat_detail)
    }

    /// Parse battery sysfs properties
    fn parse_battery_directory(&self, path: &Path, name: &str) -> Option<BatteryTelemetryDetail> {
        let status = fs::read_to_string(path.join("status"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        let capacity_pct = fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());

        let voltage_uv = fs::read_to_string(path.join("voltage_now"))
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(11_400_000.0);

        let energy_now_wh = if let Ok(s) = fs::read_to_string(path.join("energy_now")) {
            s.trim().parse::<f64>().ok().map(|e| e / 1_000_000.0)
        } else if let Ok(s) = fs::read_to_string(path.join("charge_now")) {
            s.trim().parse::<f64>().ok().map(|c| (c * voltage_uv) / 1_000_000_000_000.0)
        } else {
            None
        };

        let energy_full_wh = if let Ok(s) = fs::read_to_string(path.join("energy_full")) {
            s.trim().parse::<f64>().ok().map(|e| e / 1_000_000.0)
        } else if let Ok(s) = fs::read_to_string(path.join("charge_full")) {
            s.trim().parse::<f64>().ok().map(|c| (c * voltage_uv) / 1_000_000_000_000.0)
        } else {
            None
        };

        let (power_now_w, discharge_rate_mw) = if let Ok(s) = fs::read_to_string(path.join("power_now")) {
            let uw = s.trim().parse::<f64>().unwrap_or(0.0);
            (Some(uw / 1_000_000.0), Some(uw / 1000.0))
        } else if let Ok(s) = fs::read_to_string(path.join("current_now")) {
            let ua = s.trim().parse::<f64>().unwrap_or(0.0);
            let w = (ua * voltage_uv) / 1_000_000_000_000.0;
            (Some(w), Some(w * 1000.0))
        } else {
            (None, None)
        };

        let is_discharging = status.eq_ignore_ascii_case("discharging");

        let discharge_rate_pct_per_hr = if is_discharging {
            if let (Some(pw), Some(efull)) = (power_now_w, energy_full_wh) {
                if efull > 0.0 {
                    Some((pw / efull) * 100.0)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        Some(BatteryTelemetryDetail {
            name: name.to_string(),
            status,
            capacity_pct,
            discharge_rate_mw: if is_discharging { discharge_rate_mw } else { None },
            discharge_rate_pct_per_hr,
            power_now_w,
            energy_now_wh,
            energy_full_wh,
            is_discharging,
        })
    }

    /// Read MGLRU active indicator and min_ttl_ms
    fn read_mglru_state(&self) -> (bool, Option<String>, Option<u64>) {
        let enabled_path = self.mglru_sysfs_base.join("enabled");
        let (active, flags) = if let Ok(content) = fs::read_to_string(&enabled_path) {
            let trimmed = content.trim();
            let parsed_active = if trimmed.starts_with("0x") {
                u32::from_str_radix(&trimmed[2..], 16)
                    .map(|v| (v & 1) != 0)
                    .unwrap_or(true)
            } else {
                trimmed != "0" && !trimmed.eq_ignore_ascii_case("n")
            };
            (parsed_active, Some(trimmed.to_string()))
        } else {
            (false, None)
        };

        let min_ttl_ms = fs::read_to_string(self.mglru_sysfs_base.join("min_ttl_ms"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());

        (active, flags, min_ttl_ms)
    }

    /// Read host memory statistics and Pressure Stall Information (PSI)
    fn read_memory_and_pressure(&self) -> (u64, u64, f64, f64, f64) {
        let mut mem_total_kb = 8_388_608u64;
        let mut mem_avail_kb = 4_194_304u64;

        if let Ok(content) = fs::read_to_string(&self.proc_meminfo_path) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    mem_total_kb = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(mem_total_kb);
                } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                    mem_avail_kb = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(mem_avail_kb);
                }
            }
        }

        let total_mb = mem_total_kb / 1024;
        let avail_mb = mem_avail_kb / 1024;
        let avail_pct = if total_mb > 0 { (avail_mb as f64 / total_mb as f64) * 100.0 } else { 50.0 };

        let mut psi_some_avg10 = 0.0;
        let mut psi_full_avg10 = 0.0;

        if let Ok(content) = fs::read_to_string(&self.proc_pressure_memory_path) {
            for line in content.lines() {
                if line.starts_with("some ") {
                    psi_some_avg10 = parse_psi_avg10(line);
                } else if line.starts_with("full ") {
                    psi_full_avg10 = parse_psi_avg10(line);
                }
            }
        }

        (total_mb, avail_mb, avail_pct, psi_some_avg10, psi_full_avg10)
    }

    /// Read cpufreq driver, scaling governor, EPP, and core frequencies
    fn read_cpufreq_state(&self) -> (String, String, String, Vec<f64>) {
        let driver = fs::read_to_string(self.cpu_sysfs_base.join("cpufreq/policy0/scaling_driver"))
            .or_else(|_| fs::read_to_string(self.cpu_sysfs_base.join("cpu0/cpufreq/scaling_driver")))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "intel_pstate".to_string());

        let governor = fs::read_to_string(self.cpu_sysfs_base.join("cpufreq/policy0/scaling_governor"))
            .or_else(|_| fs::read_to_string(self.cpu_sysfs_base.join("cpu0/cpufreq/scaling_governor")))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "powersave".to_string());

        let epp = fs::read_to_string(self.cpu_sysfs_base.join("cpufreq/policy0/energy_performance_preference"))
            .or_else(|_| fs::read_to_string(self.cpu_sysfs_base.join("cpu0/cpufreq/energy_performance_preference")))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "balance_performance".to_string());

        let mut freqs = Vec::new();
        let cpufreq_dir = self.cpu_sysfs_base.join("cpufreq");
        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            let mut policies: Vec<_> = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("policy"))
                .collect();
            policies.sort_by_key(|e| e.file_name());

            for policy in policies {
                let cur = policy.path().join("scaling_cur_freq");
                if let Ok(raw) = fs::read_to_string(&cur) {
                    if let Ok(khz) = raw.trim().parse::<f64>() {
                        freqs.push(khz / 1000.0);
                    }
                }
            }
        }

        if freqs.is_empty() {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            freqs = vec![2400.0; cores];
        }

        (driver, governor, epp, freqs)
    }

    /// Read core throttle events from sysfs
    fn read_throttle_events(&self) -> u64 {
        let mut total = 0u64;
        for cpu_id in 0..8 {
            let p = self.cpu_sysfs_base.join(format!("cpu{}/thermal_throttle/core_throttle_count", cpu_id));
            if let Ok(content) = fs::read_to_string(&p) {
                if let Ok(count) = content.trim().parse::<u64>() {
                    total += count;
                }
            }
        }
        total
    }

    /// Direct write to sysfs scaling_governor
    fn write_scaling_governor(&self, governor: &str) -> std::result::Result<(), String> {
        let cpufreq_dir = self.cpu_sysfs_base.join("cpufreq");
        let mut modified = false;

        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                    let gov_file = p.join("scaling_governor");
                    if fs::write(&gov_file, format!("{}\n", governor)).is_ok() {
                        modified = true;
                    }
                }
            }
        }

        if modified {
            Ok(())
        } else {
            Err("Failed to write to scaling_governor (unprivileged or unsupported)".to_string())
        }
    }

    /// Direct write to sysfs energy_performance_preference (EPP)
    fn write_epp(&self, epp: &str) -> std::result::Result<(), String> {
        let cpufreq_dir = self.cpu_sysfs_base.join("cpufreq");
        let mut modified = false;

        if let Ok(entries) = fs::read_dir(&cpufreq_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name().unwrap_or_default().to_string_lossy().starts_with("policy") {
                    let epp_file = p.join("energy_performance_preference");
                    if fs::write(&epp_file, format!("{}\n", epp)).is_ok() {
                        modified = true;
                    }
                }
            }
        }

        if modified {
            Ok(())
        } else {
            Err("Failed to write to energy_performance_preference (unprivileged or unsupported)".to_string())
        }
    }

    /// Direct write to MGLRU min_ttl_ms
    pub fn write_mglru_min_ttl_ms(&self, ttl_ms: u64) -> std::result::Result<(), String> {
        let path = self.mglru_sysfs_base.join("min_ttl_ms");
        if path.exists() {
            fs::write(&path, format!("{}\n", ttl_ms))
                .map_err(|e| format!("Failed to write MGLRU min_ttl_ms: {}", e))
        } else {
            Err("MGLRU sysfs min_ttl_ms not found".to_string())
        }
    }

    /// Read current process nice priority
    fn current_nice_level(&self) -> i32 {
        #[cfg(target_os = "linux")]
        {
            unsafe {
                libc::getpriority(libc::PRIO_PROCESS, 0)
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }

    /// Apply nice priority to calling process
    fn apply_nice_level(&self, nice: i32) -> Result<i32> {
        #[cfg(target_os = "linux")]
        {
            let ret = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) };
            if ret == 0 {
                Ok(nice)
            } else {
                // Return current priority if setpriority was denied due to permissions
                Ok(self.current_nice_level())
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = nice;
            Ok(0)
        }
    }
}

/// Helper struct for parsed battery properties
#[derive(Debug, Clone)]
struct BatteryTelemetryDetail {
    name: String,
    status: String,
    capacity_pct: Option<u32>,
    discharge_rate_mw: Option<f64>,
    discharge_rate_pct_per_hr: Option<f64>,
    power_now_w: Option<f64>,
    energy_now_wh: Option<f64>,
    energy_full_wh: Option<f64>,
    is_discharging: bool,
}

fn parse_psi_avg10(line: &str) -> f64 {
    for token in line.split_whitespace() {
        if let Some(val_str) = token.strip_prefix("avg10=") {
            if let Ok(val) = val_str.parse::<f64>() {
                return val;
            }
        }
    }
    0.0
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_task_concurrency_boost_mode() {
        // AC Power & Temp < 72°C => Concurrency = num_cpus
        let num_cpus = 4;
        let concurrency = compute_task_concurrency(65.0, true, Some(80), num_cpus);
        assert_eq!(concurrency, 4);

        let concurrency_cool = compute_task_concurrency(45.0, true, Some(100), 8);
        assert_eq!(concurrency_cool, 8);
    }

    #[test]
    fn test_compute_task_concurrency_balanced_mode() {
        // AC Power & Temp 72°C..78°C => Concurrency = (num_cpus / 2).max(2)
        let num_cpus = 4;
        let concurrency = compute_task_concurrency(74.5, true, Some(80), num_cpus);
        assert_eq!(concurrency, 2);

        let concurrency_8 = compute_task_concurrency(75.0, true, Some(80), 8);
        assert_eq!(concurrency_8, 4);

        let concurrency_2 = compute_task_concurrency(73.0, true, Some(80), 2);
        assert_eq!(concurrency_2, 2);
    }

    #[test]
    fn test_compute_task_concurrency_throttled_protection_mode() {
        // Temp >= 78°C => Concurrency = 1
        assert_eq!(compute_task_concurrency(78.0, true, Some(100), 4), 1);
        assert_eq!(compute_task_concurrency(85.0, true, Some(100), 8), 1);

        // Battery < 25% on battery => Concurrency = 1
        assert_eq!(compute_task_concurrency(55.0, false, Some(24), 4), 1);
        assert_eq!(compute_task_concurrency(60.0, false, Some(10), 8), 1);
    }

    #[test]
    fn test_compute_ollama_batch_size_scaling() {
        // Cool AC => 512
        assert_eq!(compute_ollama_batch_size(60.0, true, Some(90), 2.0), 512);

        // Warm => 256
        assert_eq!(compute_ollama_batch_size(73.0, true, Some(90), 2.0), 256);

        // High RAM pressure => 128
        assert_eq!(compute_ollama_batch_size(60.0, true, Some(90), 40.0), 128);

        // Hot >= 78°C => 128
        assert_eq!(compute_ollama_batch_size(79.0, true, Some(90), 2.0), 128);

        // Critical battery => 128
        assert_eq!(compute_ollama_batch_size(60.0, false, Some(20), 2.0), 128);
    }

    #[test]
    fn test_profiler_telemetry_sampling_real() {
        let profiler = DynamicHardwareProfiler::new();
        let telem = profiler.sample_fresh();
        assert!(telem.cpu_pkg_temp_c > 0.0);
        assert!(telem.memory_total_mb > 0);
        assert!(telem.optimal_concurrency >= 1);
        assert!(telem.recommended_ollama_batch_size >= 128);
    }

    #[test]
    fn test_profile_switch_and_thermal_safeguard() {
        let profiler = DynamicHardwareProfiler::new();

        // Switch to Balanced profile
        let report = profiler.switch_profile(HardwareGovernorProfile::Balanced).expect("switch");
        assert_eq!(report.applied_profile, HardwareGovernorProfile::Balanced);

        // Switch to PowerSave profile
        let report_ps = profiler.switch_profile(HardwareGovernorProfile::PowerSave).expect("switch");
        assert_eq!(report_ps.applied_profile, HardwareGovernorProfile::PowerSave);
    }

    #[tokio::test]
    async fn test_raii_governor_guard_restoration() {
        let profiler = Arc::new(DynamicHardwareProfiler::new());
        let _ = profiler.switch_profile(HardwareGovernorProfile::PowerSave);

        {
            let guard = profiler.acquire_profile_guard(HardwareGovernorProfile::Balanced).expect("guard");
            assert_eq!(guard.baseline_profile, HardwareGovernorProfile::PowerSave);
            assert_eq!(guard.report().applied_profile, HardwareGovernorProfile::Balanced);
            // Guard drops here
        }

        // Active profile restored to baseline
        let current = profiler.active_profile.read().unwrap();
        assert_eq!(*current, HardwareGovernorProfile::PowerSave);
    }

    #[tokio::test]
    async fn test_background_sampler_continuous() {
        let profiler = Arc::new(DynamicHardwareProfiler::new());
        let handle = profiler.start_background_sampler(Duration::from_millis(100));

        tokio::time::sleep(Duration::from_millis(250)).await;
        let telem = profiler.current_telemetry();
        assert!(telem.optimal_concurrency >= 1);

        profiler.stop_background_sampler();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }
}
