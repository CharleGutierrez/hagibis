//! # Continuous Hardware Telemetry Monitor
//!
//! Production-grade real-time Linux hardware telemetry ingestion:
//! - **Thermal Throttling**: Monitors `/sys/class/thermal/thermal_zone*/temp`, trip points,
//!   and `/sys/class/hwmon/hwmon*/temp*_input` / `temp*_crit` sensors.
//! - **Memory Pressure (PSI)**: Ingests Linux kernel Pressure Stall Information from
//!   `/proc/pressure/memory` (`some avg10`, `some avg60`, `some avg300`, `full avg10`, `total`).
//! - **Memory Info**: Ingests `/proc/meminfo` (`MemTotal`, `MemAvailable`, `MemFree`, `SwapTotal`, `SwapFree`).
//! - **Emergency Throttling Thresholds**: Evaluates if junction temp >= 80.0°C (or critical trip margin < 5.0°C)
//!   or if memory pressure spikes (PSI `some avg10 >= 40.0%` or available RAM < 10%).

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

/// Configurable thresholds for thermal and memory pressure triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryThresholds {
    /// Critical thermal throttling junction temperature in degrees Celsius (default: 80.0°C)
    pub thermal_throttling_temp_c: f32,
    /// Minimum headroom margin to TjMax in degrees Celsius before emergency throttling (default: 5.0°C)
    pub critical_tj_margin_c: f32,
    /// Memory Pressure Stall Information (PSI) `some avg10` threshold in percentage (default: 40.0%)
    pub memory_psi_avg10_threshold: f32,
    /// Minimum available RAM percentage before memory exhaustion trigger (default: 10.0%)
    pub min_available_ram_pct: f32,
}

impl Default for TelemetryThresholds {
    fn default() -> Self {
        Self {
            thermal_throttling_temp_c: 80.0,
            critical_tj_margin_c: 5.0,
            memory_psi_avg10_threshold: 40.0,
            min_available_ram_pct: 10.0,
        }
    }
}

/// Linux Memory Pressure Stall Information (PSI) reading
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryPressurePsi {
    pub some_avg10: f32,
    pub some_avg60: f32,
    pub some_avg300: f32,
    pub some_total_us: u64,
    pub full_avg10: f32,
    pub full_avg60: f32,
    pub full_avg300: f32,
    pub full_total_us: u64,
}

impl Default for MemoryPressurePsi {
    fn default() -> Self {
        Self {
            some_avg10: 0.0,
            some_avg60: 0.0,
            some_avg300: 0.0,
            some_total_us: 0,
            full_avg10: 0.0,
            full_avg60: 0.0,
            full_avg300: 0.0,
            full_total_us: 0,
        }
    }
}

impl MemoryPressurePsi {
    /// Parses Linux `/proc/pressure/memory` file contents
    ///
    /// Expected format:
    /// some avg10=0.00 avg60=0.00 avg300=0.00 total=0
    /// full avg10=0.00 avg60=0.00 avg300=0.00 total=0
    pub fn parse(content: &str) -> Self {
        let mut psi = Self::default();
        for line in content.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("some ") {
                for token in rest.split_whitespace() {
                    if let Some((k, v)) = token.split_once('=') {
                        match k {
                            "avg10" => psi.some_avg10 = v.parse::<f32>().unwrap_or(0.0),
                            "avg60" => psi.some_avg60 = v.parse::<f32>().unwrap_or(0.0),
                            "avg300" => psi.some_avg300 = v.parse::<f32>().unwrap_or(0.0),
                            "total" => psi.some_total_us = v.parse::<u64>().unwrap_or(0),
                            _ => {}
                        }
                    }
                }
            } else if let Some(rest) = line.strip_prefix("full ") {
                for token in rest.split_whitespace() {
                    if let Some((k, v)) = token.split_once('=') {
                        match k {
                            "avg10" => psi.full_avg10 = v.parse::<f32>().unwrap_or(0.0),
                            "avg60" => psi.full_avg60 = v.parse::<f32>().unwrap_or(0.0),
                            "avg300" => psi.full_avg300 = v.parse::<f32>().unwrap_or(0.0),
                            "total" => psi.full_total_us = v.parse::<u64>().unwrap_or(0),
                            _ => {}
                        }
                    }
                }
            }
        }
        psi
    }
}

/// Instantaneous hardware health classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareHealthStatus {
    Normal,
    ThermalThrottled,
    MemoryPressureSpike,
    CriticalDualThrottled,
}

impl HardwareHealthStatus {
    pub fn is_throttled(&self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// Comprehensive hardware telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveHardwareTelemetry {
    pub peak_junction_temp_c: f32,
    pub tj_max_c: f32,
    pub tj_margin_c: f32,
    pub is_thermal_throttling: bool,
    pub memory_psi: MemoryPressurePsi,
    pub memory_pressure_spike: bool,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub available_ram_pct: f32,
    pub health_status: HardwareHealthStatus,
    pub timestamp_epoch_ms: u64,
}

/// Continuous Telemetry Monitor reading sysfs and procfs
pub struct HardwareTelemetryMonitor {
    sysfs_root: PathBuf,
    procfs_root: PathBuf,
    thresholds: TelemetryThresholds,
}

impl Default for HardwareTelemetryMonitor {
    fn default() -> Self {
        Self::new(PathBuf::from("/sys"), PathBuf::from("/proc"), TelemetryThresholds::default())
    }
}

impl HardwareTelemetryMonitor {
    pub fn new(sysfs_root: PathBuf, procfs_root: PathBuf, thresholds: TelemetryThresholds) -> Self {
        Self {
            sysfs_root,
            procfs_root,
            thresholds,
        }
    }

    pub fn with_thresholds(mut self, thresholds: TelemetryThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Reads thermal sensors across `/sys/class/thermal` and `/sys/class/hwmon`
    pub fn poll_thermal(&self) -> (f32, f32, f32, bool) {
        let mut peak_temp_c = 42.0f32;
        let mut tj_max_c = 100.0f32;

        // 1. Scan /sys/class/thermal/thermal_zone*
        let thermal_dir = self.sysfs_root.join("class/thermal");
        if let Ok(entries) = fs::read_dir(&thermal_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let zone_path = entry.path();
                    if let Ok(raw_str) = fs::read_to_string(zone_path.join("temp")) {
                        if let Ok(raw) = raw_str.trim().parse::<f32>() {
                            let temp_c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                            if temp_c > peak_temp_c && temp_c < 150.0 {
                                peak_temp_c = temp_c;
                            }
                        }
                    }

                    // Check trip points for critical max
                    for i in 0..6 {
                        let trip_path = zone_path.join(format!("trip_point_{}_temp", i));
                        let type_path = zone_path.join(format!("trip_point_{}_type", i));
                        if let Ok(type_str) = fs::read_to_string(&type_path) {
                            if type_str.trim() == "critical" || type_str.trim() == "hot" {
                                if let Ok(trip_raw) = fs::read_to_string(&trip_path) {
                                    if let Ok(raw) = trip_raw.trim().parse::<f32>() {
                                        let crit_c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                                        if crit_c >= 70.0 && crit_c <= 130.0 {
                                            tj_max_c = crit_c;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Scan /sys/class/hwmon/hwmon*
        let hwmon_dir = self.sysfs_root.join("class/hwmon");
        if let Ok(entries) = fs::read_dir(&hwmon_dir) {
            for entry in entries.flatten() {
                let hw_path = entry.path();
                for i in 1..8 {
                    let temp_input = hw_path.join(format!("temp{}_input", i));
                    if let Ok(raw_str) = fs::read_to_string(&temp_input) {
                        if let Ok(raw) = raw_str.trim().parse::<f32>() {
                            let temp_c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                            if temp_c > peak_temp_c && temp_c < 150.0 {
                                peak_temp_c = temp_c;
                            }
                        }
                    }
                    let temp_crit = hw_path.join(format!("temp{}_crit", i));
                    if let Ok(raw_str) = fs::read_to_string(&temp_crit) {
                        if let Ok(raw) = raw_str.trim().parse::<f32>() {
                            let crit_c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                            if crit_c >= 70.0 && crit_c <= 130.0 {
                                tj_max_c = crit_c;
                            }
                        }
                    }
                }
            }
        }

        let tj_margin_c = (tj_max_c - peak_temp_c).max(0.0);
        let is_thermal_throttling = peak_temp_c >= self.thresholds.thermal_throttling_temp_c
            || tj_margin_c <= self.thresholds.critical_tj_margin_c;

        (peak_temp_c, tj_max_c, tj_margin_c, is_thermal_throttling)
    }

    /// Reads memory pressure from `/proc/pressure/memory`
    pub fn poll_memory_pressure_psi(&self) -> (MemoryPressurePsi, bool) {
        let psi_path = self.procfs_root.join("pressure/memory");
        if let Ok(content) = fs::read_to_string(&psi_path) {
            let psi = MemoryPressurePsi::parse(&content);
            let is_spike = psi.some_avg10 >= self.thresholds.memory_psi_avg10_threshold;
            return (psi, is_spike);
        }
        (MemoryPressurePsi::default(), false)
    }

    /// Reads system memory status from `/proc/meminfo`
    pub fn poll_meminfo(&self) -> (u64, u64, f32) {
        let mut total_kb = 16 * 1024 * 1024u64; // 16 GB default
        let mut avail_kb = 12 * 1024 * 1024u64; // 12 GB default

        let meminfo_path = self.procfs_root.join("meminfo");
        if let Ok(content) = fs::read_to_string(&meminfo_path) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Some(token) = rest.split_whitespace().next() {
                        if let Ok(kb) = token.parse::<u64>() {
                            total_kb = kb;
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                    if let Some(token) = rest.split_whitespace().next() {
                        if let Ok(kb) = token.parse::<u64>() {
                            avail_kb = kb;
                        }
                    }
                }
            }
        }

        let total_mb = total_kb / 1024;
        let avail_mb = avail_kb / 1024;
        let avail_pct = if total_mb > 0 {
            ((avail_mb as f64 / total_mb as f64) * 100.0) as f32
        } else {
            100.0
        };

        (total_mb, avail_mb, avail_pct)
    }

    /// Captures a complete hardware telemetry snapshot
    pub fn snapshot(&self) -> LiveHardwareTelemetry {
        let (peak_junction_temp_c, tj_max_c, tj_margin_c, is_thermal_throttling) = self.poll_thermal();
        let (memory_psi, psi_spike) = self.poll_memory_pressure_psi();
        let (total_ram_mb, available_ram_mb, available_ram_pct) = self.poll_meminfo();

        let memory_pressure_spike = psi_spike || available_ram_pct < self.thresholds.min_available_ram_pct;

        let health_status = match (is_thermal_throttling, memory_pressure_spike) {
            (true, true) => HardwareHealthStatus::CriticalDualThrottled,
            (true, false) => HardwareHealthStatus::ThermalThrottled,
            (false, true) => HardwareHealthStatus::MemoryPressureSpike,
            (false, false) => HardwareHealthStatus::Normal,
        };

        let timestamp_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        LiveHardwareTelemetry {
            peak_junction_temp_c,
            tj_max_c,
            tj_margin_c,
            is_thermal_throttling,
            memory_psi,
            memory_pressure_spike,
            total_ram_mb,
            available_ram_mb,
            available_ram_pct,
            health_status,
            timestamp_epoch_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_psi_parser() {
        let psi_raw = "some avg10=42.15 avg60=28.30 avg300=15.10 total=987654\n\
                       full avg10=12.50 avg60=8.20 avg300=3.40 total=123456\n";
        let parsed = MemoryPressurePsi::parse(psi_raw);
        assert!((parsed.some_avg10 - 42.15).abs() < 1e-4);
        assert!((parsed.some_avg60 - 28.30).abs() < 1e-4);
        assert!((parsed.some_avg300 - 15.10).abs() < 1e-4);
        assert_eq!(parsed.some_total_us, 987654);
        assert!((parsed.full_avg10 - 12.50).abs() < 1e-4);
        assert_eq!(parsed.full_total_us, 123456);
    }

    #[test]
    fn test_thermal_and_memory_pressure_monitoring() {
        let tmp = tempdir().unwrap();
        let sys_dir = tmp.path().join("sys");
        let proc_dir = tmp.path().join("proc");

        // Set up mock thermal zone
        let zone0 = sys_dir.join("class/thermal/thermal_zone0");
        fs::create_dir_all(&zone0).unwrap();
        fs::write(zone0.join("temp"), "85500\n").unwrap(); // 85.5°C -> triggers >= 80°C throttle
        fs::write(zone0.join("trip_point_0_type"), "critical\n").unwrap();
        fs::write(zone0.join("trip_point_0_temp"), "100000\n").unwrap(); // 100°C

        // Set up mock PSI memory pressure file (some avg10 = 45.2% >= 40.0% spike)
        let pressure_dir = proc_dir.join("pressure");
        fs::create_dir_all(&pressure_dir).unwrap();
        let psi_content = "some avg10=45.20 avg60=30.00 avg300=15.00 total=500000\n\
                           full avg10=20.00 avg60=10.00 avg300=5.00 total=200000\n";
        fs::write(pressure_dir.join("memory"), psi_content).unwrap();

        // Set up mock meminfo
        let meminfo_content = "MemTotal:       16384000 kB\n\
                               MemFree:         1000000 kB\n\
                               MemAvailable:    1200000 kB\n";
        fs::write(proc_dir.join("meminfo"), meminfo_content).unwrap();

        let monitor = HardwareTelemetryMonitor::new(sys_dir, proc_dir, TelemetryThresholds::default());
        let snapshot = monitor.snapshot();

        assert!((snapshot.peak_junction_temp_c - 85.5).abs() < 0.1);
        assert!(snapshot.is_thermal_throttling);
        assert!((snapshot.memory_psi.some_avg10 - 45.20).abs() < 0.1);
        assert!(snapshot.memory_pressure_spike);
        assert_eq!(snapshot.health_status, HardwareHealthStatus::CriticalDualThrottled);
    }

    #[test]
    fn test_normal_telemetry_reading() {
        let tmp = tempdir().unwrap();
        let sys_dir = tmp.path().join("sys");
        let proc_dir = tmp.path().join("proc");

        let zone0 = sys_dir.join("class/thermal/thermal_zone0");
        fs::create_dir_all(&zone0).unwrap();
        fs::write(zone0.join("temp"), "52000\n").unwrap(); // 52.0°C -> normal

        let pressure_dir = proc_dir.join("pressure");
        fs::create_dir_all(&pressure_dir).unwrap();
        let psi_content = "some avg10=2.50 avg60=1.20 avg300=0.80 total=10000\n\
                           full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
        fs::write(pressure_dir.join("memory"), psi_content).unwrap();

        let monitor = HardwareTelemetryMonitor::new(sys_dir, proc_dir, TelemetryThresholds::default());
        let snapshot = monitor.snapshot();

        assert!((snapshot.peak_junction_temp_c - 52.0).abs() < 0.1);
        assert!(!snapshot.is_thermal_throttling);
        assert!(!snapshot.memory_pressure_spike);
        assert_eq!(snapshot.health_status, HardwareHealthStatus::Normal);
    }
}
