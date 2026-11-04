//! # Dynamic Thermal/Power Awareness IPC Bridge (Pillar 3.1)
//!
//! Production-grade hardware telemetry bridge and autonomous dynamic governor:
//! - Direct bidirectional IPC connection to Dynatune daemon (`/run/dynatune.sock`)
//! - Non-blocking Linux sysfs fallback (`/sys/class/thermal`, `/sys/class/power_supply`, `/proc/pressure/memory`)
//! - Continuous hardware status polling (CPU package temperature, battery level, AC status, memory pressure)
//! - Autonomous `HardwareGovernor`:
//!   * Throttles non-critical background jobs (compaction, vector indexing, speculative agents) and restricts
//!     thread pool when CPU temp >= 78.0°C or battery < 20% on discharge.
//!   * Restores full throughput (concurrency boost) when CPU temp < 65.0°C and AC power is connected.
//!   * Emits commands (`request_boost`, `request_eco`) to Dynatune IPC socket.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_DYNATUNE_SOCKET: &str = "/run/dynatune.sock";
pub const DEFAULT_SYSFS_THERMAL: &str = "/sys/class/thermal";
pub const DEFAULT_SYSFS_POWER: &str = "/sys/class/power_supply";
pub const DEFAULT_PROC_PRESSURE_MEM: &str = "/proc/pressure/memory";
pub const DEFAULT_PROC_MEMINFO: &str = "/proc/meminfo";

/// Temperature in Celsius that triggers emergency throttling (78.0°C)
pub const THERMAL_THROTTLE_THRESHOLD_C: f64 = 78.0;

/// Temperature in Celsius that permits recovery and high performance (65.0°C)
pub const THERMAL_RECOVERY_THRESHOLD_C: f64 = 65.0;

/// Critical battery capacity percentage on battery discharge (20%)
pub const BATTERY_CRITICAL_DISCHARGE_PCT: u32 = 20;

/// Thread worker pool limit under thermal / battery throttling
pub const THROTTLED_CONCURRENCY_LIMIT: usize = 1;

/// Default nominal thread worker pool limit
pub const NOMINAL_CONCURRENCY_LIMIT: usize = 4;

/// High performance boost worker pool limit
pub const BOOST_CONCURRENCY_LIMIT: usize = 8;

/// Operating hardware performance profile
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareProfile {
    /// High-performance burst mode (AC connected & cool < 65°C)
    HighPerformanceBoost,
    /// Balanced standard operational profile
    BalancedNominal,
    /// Throttled eco profile (hot >= 78°C or critical battery < 20%)
    ThrottledEco,
}

impl std::fmt::Display for HardwareProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HighPerformanceBoost => write!(f, "High-Performance Boost (Turbo On, 8 Workers)"),
            Self::BalancedNominal => write!(f, "Balanced Nominal (Standard, 4 Workers)"),
            Self::ThrottledEco => write!(f, "Throttled Eco (Thermal/Power Clamp, 1 Worker)"),
        }
    }
}

/// Instantaneous hardware telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareTelemetrySnapshot {
    pub cpu_temp_c: f64,
    pub max_zone_temp_c: f64,
    pub battery_pct: Option<u32>,
    pub is_charging: bool,
    pub ac_online: bool,
    pub memory_available_mb: u64,
    pub memory_pressure_some_avg10: f64,
    pub profile: HardwareProfile,
    pub is_throttled: bool,
    pub worker_concurrency: usize,
    pub dynatune_connected: bool,
    pub timestamp_epoch_ms: u64,
}

impl Default for HardwareTelemetrySnapshot {
    fn default() -> Self {
        Self {
            cpu_temp_c: 45.0,
            max_zone_temp_c: 45.0,
            battery_pct: Some(100),
            is_charging: true,
            ac_online: true,
            memory_available_mb: 4096,
            memory_pressure_some_avg10: 0.0,
            profile: HardwareProfile::BalancedNominal,
            is_throttled: false,
            worker_concurrency: NOMINAL_CONCURRENCY_LIMIT,
            dynatune_connected: false,
            timestamp_epoch_ms: 0,
        }
    }
}

/// IPC command sent to Dynatune daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynatuneIpcCommand {
    pub command: String,
    pub source: String,
    pub timestamp_epoch_ms: u64,
    pub reason: Option<String>,
}

/// Dynamic Hardware Governor and Telemetry Bridge
pub struct HardwareGovernor {
    socket_path: PathBuf,
    sysfs_thermal: PathBuf,
    sysfs_power: PathBuf,
    proc_pressure_mem: PathBuf,
    proc_meminfo: PathBuf,
    current_snapshot: Arc<RwLock<HardwareTelemetrySnapshot>>,
    throttle_background_tasks: Arc<AtomicBool>,
    active_concurrency_limit: Arc<AtomicUsize>,
    total_polls: Arc<AtomicU64>,
    total_throttles: Arc<AtomicU64>,
    total_boosts: Arc<AtomicU64>,
    mock_telemetry: Arc<RwLock<Option<HardwareTelemetrySnapshot>>>,
}

impl Default for HardwareGovernor {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareGovernor {
    /// Initialize with standard Linux system paths
    pub fn new() -> Self {
        Self::with_paths(
            PathBuf::from(DEFAULT_DYNATUNE_SOCKET),
            PathBuf::from(DEFAULT_SYSFS_THERMAL),
            PathBuf::from(DEFAULT_SYSFS_POWER),
            PathBuf::from(DEFAULT_PROC_PRESSURE_MEM),
            PathBuf::from(DEFAULT_PROC_MEMINFO),
        )
    }

    /// Initialize with custom paths (enables robust testing on mock sysfs)
    pub fn with_paths(
        socket: PathBuf,
        thermal: PathBuf,
        power: PathBuf,
        pressure_mem: PathBuf,
        meminfo: PathBuf,
    ) -> Self {
        let governor = Self {
            socket_path: socket,
            sysfs_thermal: thermal,
            sysfs_power: power,
            proc_pressure_mem: pressure_mem,
            proc_meminfo: meminfo,
            current_snapshot: Arc::new(RwLock::new(HardwareTelemetrySnapshot::default())),
            throttle_background_tasks: Arc::new(AtomicBool::new(false)),
            active_concurrency_limit: Arc::new(AtomicUsize::new(NOMINAL_CONCURRENCY_LIMIT)),
            total_polls: Arc::new(AtomicU64::new(0)),
            total_throttles: Arc::new(AtomicU64::new(0)),
            total_boosts: Arc::new(AtomicU64::new(0)),
            mock_telemetry: Arc::new(RwLock::new(None)),
        };
        // Perform initial poll to establish baseline state
        governor.poll_hardware();
        governor
    }

    /// Inject mock telemetry for deterministic unit testing
    pub fn set_mock_telemetry(&self, mock: Option<HardwareTelemetrySnapshot>) {
        let mut guard = self.mock_telemetry.write().unwrap();
        *guard = mock;
    }

    /// Check if background tasks (compaction, index generation, speculation) should be throttled
    pub fn should_throttle_background_tasks(&self) -> bool {
        self.throttle_background_tasks.load(Ordering::Relaxed)
    }

    /// Active worker concurrency limit for task schedulers
    pub fn concurrency_limit(&self) -> usize {
        self.active_concurrency_limit.load(Ordering::Relaxed)
    }

    /// Retrieve the latest cached telemetry snapshot
    pub fn latest_snapshot(&self) -> HardwareTelemetrySnapshot {
        self.current_snapshot.read().unwrap().clone()
    }

    /// Send dynamic IPC notification / command to Dynatune daemon
    pub fn send_dynatune_command(&self, command: &str, reason: Option<&str>) -> Result<String, String> {
        if !self.socket_path.exists() {
            return Err(format!("Dynatune socket not found at {}", self.socket_path.display()));
        }

        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|e| format!("Failed to connect to Dynatune socket: {}", e))?;

        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(250)))
            .map_err(|e| e.to_string())?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let cmd_payload = DynatuneIpcCommand {
            command: command.to_string(),
            source: "hagibis_daemon".to_string(),
            timestamp_epoch_ms: now_ms,
            reason: reason.map(|s| s.to_string()),
        };

        let json_line = serde_json::to_string(&cmd_payload).map_err(|e| e.to_string())? + "\n";
        stream.write_all(json_line.as_bytes()).map_err(|e| e.to_string())?;

        let mut response = String::new();
        let mut buf = [0u8; 1024];
        if let Ok(n) = stream.read(&mut buf) {
            if n > 0 {
                response = String::from_utf8_lossy(&buf[..n]).trim().to_string();
            }
        }

        Ok(response)
    }

    /// Poll hardware telemetry and dynamically adjust governor policies
    pub fn poll_hardware(&self) -> HardwareTelemetrySnapshot {
        self.total_polls.fetch_add(1, Ordering::Relaxed);

        // 1. Check if mock telemetry override is active
        if let Some(mock) = self.mock_telemetry.read().unwrap().clone() {
            return self.apply_policy(
                mock.cpu_temp_c,
                mock.max_zone_temp_c,
                mock.battery_pct,
                mock.is_charging,
                mock.ac_online,
                mock.memory_available_mb,
                mock.memory_pressure_some_avg10,
                mock.dynatune_connected,
            );
        }

        // 2. Try Dynatune IPC socket telemetry
        let mut dynatune_connected = false;
        if self.socket_path.exists() {
            if let Ok(resp_str) = self.send_dynatune_command("status", None) {
                if let Ok(v) = serde_json::from_str::<Value>(&resp_str) {
                    dynatune_connected = true;
                    let temp = v.get("cpu_temp_c").and_then(|x| x.as_f64()).unwrap_or(45.0);
                    let max_temp = v.get("max_temp_c").and_then(|x| x.as_f64()).unwrap_or(temp);
                    let bat_pct = v.get("battery_pct").and_then(|x| x.as_u64()).map(|x| x as u32);
                    let ac = v.get("ac_online").and_then(|x| x.as_bool()).unwrap_or(true);
                    let charging = v.get("is_charging").and_then(|x| x.as_bool()).unwrap_or(ac);
                    let mem_avail = v.get("memory_available_mb").and_then(|x| x.as_u64()).unwrap_or(4096);
                    let mem_press = v.get("memory_pressure_some").and_then(|x| x.as_f64()).unwrap_or(0.0);

                    return self.apply_policy(
                        temp,
                        max_temp,
                        bat_pct,
                        charging,
                        ac,
                        mem_avail,
                        mem_press,
                        dynatune_connected,
                    );
                }
            }
        }

        // 3. Fallback: Parse Linux sysfs directly
        let (cpu_temp_c, max_zone_temp_c) = self.read_sysfs_thermals();
        let (ac_online, battery_pct, is_charging) = self.read_sysfs_power();
        let memory_available_mb = self.read_proc_meminfo();
        let memory_pressure_some_avg10 = self.read_proc_pressure_memory();

        self.apply_policy(
            cpu_temp_c,
            max_zone_temp_c,
            battery_pct,
            is_charging,
            ac_online,
            memory_available_mb,
            memory_pressure_some_avg10,
            dynatune_connected,
        )
    }

    /// Evaluates governor rules and applies throttling/boost state transitions
    fn apply_policy(
        &self,
        cpu_temp_c: f64,
        max_zone_temp_c: f64,
        battery_pct: Option<u32>,
        is_charging: bool,
        ac_online: bool,
        memory_available_mb: u64,
        memory_pressure_some_avg10: f64,
        dynatune_connected: bool,
    ) -> HardwareTelemetrySnapshot {
        let is_critically_hot = cpu_temp_c >= THERMAL_THROTTLE_THRESHOLD_C || max_zone_temp_c >= (THERMAL_THROTTLE_THRESHOLD_C + 5.0);
        let is_battery_critical = match battery_pct {
            Some(pct) => pct < BATTERY_CRITICAL_DISCHARGE_PCT && !ac_online && !is_charging,
            None => false,
        };

        let (profile, is_throttled, concurrency) = if is_critically_hot || is_battery_critical {
            // Emergency throttling policy
            self.total_throttles.fetch_add(1, Ordering::Relaxed);
            let _ = self.send_dynatune_command("request_eco", Some("thermal_or_battery_throttle"));
            (HardwareProfile::ThrottledEco, true, THROTTLED_CONCURRENCY_LIMIT)
        } else if cpu_temp_c < THERMAL_RECOVERY_THRESHOLD_C && ac_online {
            // High-performance boost policy
            self.total_boosts.fetch_add(1, Ordering::Relaxed);
            let _ = self.send_dynatune_command("request_boost", Some("ac_cool_performance_boost"));
            (HardwareProfile::HighPerformanceBoost, false, BOOST_CONCURRENCY_LIMIT)
        } else {
            // Balanced nominal policy
            (HardwareProfile::BalancedNominal, false, NOMINAL_CONCURRENCY_LIMIT)
        };

        self.throttle_background_tasks.store(is_throttled, Ordering::Relaxed);
        self.active_concurrency_limit.store(concurrency, Ordering::Relaxed);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let snapshot = HardwareTelemetrySnapshot {
            cpu_temp_c,
            max_zone_temp_c,
            battery_pct,
            is_charging,
            ac_online,
            memory_available_mb,
            memory_pressure_some_avg10,
            profile,
            is_throttled,
            worker_concurrency: concurrency,
            dynatune_connected,
            timestamp_epoch_ms: now_ms,
        };

        {
            let mut guard = self.current_snapshot.write().unwrap();
            *guard = snapshot.clone();
        }

        snapshot
    }

    fn read_sysfs_thermals(&self) -> (f64, f64) {
        let mut cpu_temp_c = 45.0;
        let mut max_temp_c = 45.0;

        if !self.sysfs_thermal.exists() {
            return (cpu_temp_c, max_temp_c);
        }

        if let Ok(entries) = fs::read_dir(&self.sysfs_thermal) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with("thermal_zone") {
                    let type_path = path.join("type");
                    let temp_path = path.join("temp");

                    let zone_type = fs::read_to_string(&type_path).unwrap_or_default().trim().to_string();
                    if let Ok(temp_str) = fs::read_to_string(&temp_path) {
                        if let Ok(milli) = temp_str.trim().parse::<i64>() {
                            let temp = milli as f64 / 1000.0;
                            if temp > max_temp_c {
                                max_temp_c = temp;
                            }
                            if zone_type == "x86_pkg_temp" || zone_type == "acpitz" || zone_type == "cpu-thermal" {
                                cpu_temp_c = temp;
                            }
                        }
                    }
                }
            }
        }

        (cpu_temp_c, max_temp_c)
    }

    fn read_sysfs_power(&self) -> (bool, Option<u32>, bool) {
        let mut ac_online = true;
        let mut battery_pct = None;
        let mut is_charging = false;

        if !self.sysfs_power.exists() {
            return (ac_online, battery_pct, is_charging);
        }

        if let Ok(entries) = fs::read_dir(&self.sysfs_power) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();

                // AC Adapter
                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(online_val) = fs::read_to_string(path.join("online")) {
                        ac_online = online_val.trim() == "1";
                    }
                }

                // Battery
                if name.starts_with("BAT") {
                    if let Ok(cap_val) = fs::read_to_string(path.join("capacity")) {
                        if let Ok(pct) = cap_val.trim().parse::<u32>() {
                            battery_pct = Some(pct);
                        }
                    }
                    if let Ok(stat_val) = fs::read_to_string(path.join("status")) {
                        let stat = stat_val.trim();
                        is_charging = stat.eq_ignore_ascii_case("Charging") || stat.eq_ignore_ascii_case("Full");
                    }
                }
            }
        }

        (ac_online, battery_pct, is_charging)
    }

    fn read_proc_meminfo(&self) -> u64 {
        if let Ok(content) = fs::read_to_string(&self.proc_meminfo) {
            for line in content.lines() {
                if line.starts_with("MemAvailable:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if let Some(kb_str) = parts.get(1) {
                        if let Ok(kb) = kb_str.parse::<u64>() {
                            return kb / 1024;
                        }
                    }
                }
            }
        }
        4096
    }

    fn read_proc_pressure_memory(&self) -> f64 {
        if let Ok(content) = fs::read_to_string(&self.proc_pressure_mem) {
            for line in content.lines() {
                if line.starts_with("some ") {
                    for token in line.split_whitespace() {
                        if let Some(val_str) = token.strip_prefix("avg10=") {
                            if let Ok(val) = val_str.parse::<f64>() {
                                return val;
                            }
                        }
                    }
                }
            }
        }
        0.0
    }

    /// Launch continuous hardware governor polling task in the background
    pub fn start_background_watcher(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                self.poll_hardware();
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_governor_thermal_throttle_threshold() {
        let governor = HardwareGovernor::new();

        // 1. Nominal cool state on AC: Boost mode
        let mut telem = HardwareTelemetrySnapshot::default();
        telem.cpu_temp_c = 55.0;
        telem.ac_online = true;
        telem.battery_pct = Some(90);
        governor.set_mock_telemetry(Some(telem));

        let snap = governor.poll_hardware();
        assert_eq!(snap.profile, HardwareProfile::HighPerformanceBoost);
        assert_eq!(snap.is_throttled, false);
        assert_eq!(snap.worker_concurrency, BOOST_CONCURRENCY_LIMIT);
        assert_eq!(governor.should_throttle_background_tasks(), false);

        // 2. High temperature trip >= 78.0°C: Throttled Eco mode
        let mut hot_telem = HardwareTelemetrySnapshot::default();
        hot_telem.cpu_temp_c = 82.5;
        hot_telem.ac_online = true;
        governor.set_mock_telemetry(Some(hot_telem));

        let snap_hot = governor.poll_hardware();
        assert_eq!(snap_hot.profile, HardwareProfile::ThrottledEco);
        assert_eq!(snap_hot.is_throttled, true);
        assert_eq!(snap_hot.worker_concurrency, THROTTLED_CONCURRENCY_LIMIT);
        assert_eq!(governor.should_throttle_background_tasks(), true);

        // 3. Cool down below 65°C on AC: Recovery to Boost
        let mut cool_telem = HardwareTelemetrySnapshot::default();
        cool_telem.cpu_temp_c = 62.0;
        cool_telem.ac_online = true;
        governor.set_mock_telemetry(Some(cool_telem));

        let snap_cool = governor.poll_hardware();
        assert_eq!(snap_cool.profile, HardwareProfile::HighPerformanceBoost);
        assert_eq!(snap_cool.is_throttled, false);
        assert_eq!(snap_cool.worker_concurrency, BOOST_CONCURRENCY_LIMIT);
    }

    #[test]
    fn test_governor_battery_discharge_throttle() {
        let governor = HardwareGovernor::new();

        // On battery with critical level < 20%
        let mut bat_telem = HardwareTelemetrySnapshot::default();
        bat_telem.cpu_temp_c = 50.0;
        bat_telem.ac_online = false;
        bat_telem.is_charging = false;
        bat_telem.battery_pct = Some(15);
        governor.set_mock_telemetry(Some(bat_telem));

        let snap = governor.poll_hardware();
        assert_eq!(snap.profile, HardwareProfile::ThrottledEco);
        assert_eq!(snap.is_throttled, true);
        assert_eq!(snap.worker_concurrency, THROTTLED_CONCURRENCY_LIMIT);

        // Plugging in AC restores to Boost (temp < 65°C and AC connected)
        let mut plug_telem = HardwareTelemetrySnapshot::default();
        plug_telem.cpu_temp_c = 50.0;
        plug_telem.ac_online = true;
        plug_telem.is_charging = true;
        plug_telem.battery_pct = Some(15);
        governor.set_mock_telemetry(Some(plug_telem));

        let snap_ac = governor.poll_hardware();
        assert_eq!(snap_ac.profile, HardwareProfile::HighPerformanceBoost);
        assert_eq!(snap_ac.is_throttled, false);
    }
}
