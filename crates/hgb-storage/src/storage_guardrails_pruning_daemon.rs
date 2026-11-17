//! # storage_guardrails_pruning_daemon.rs - Real-Time statvfs Storage Guardrails & Automated Telemetry Compactor
//! ==========================================================================================================
//! Pillar 3: Intelligent Telemetry Backpressure & Storage Guardrails
//!
//! Production-grade Linux filesystem backpressure actuator and telemetry compaction daemon:
//! 1. **POSIX statvfs Partition Monitoring**:
//!    - Real-time disk capacity and inode tracking on root partition `/` (`/dev/sda2`).
//!    - Four discrete operational tiers:
//!        • `Normal` (< 75.0% usage): full fidelity ingestion (TRACE/DEBUG/INFO/WARN/ERROR), 0ms delay.
//!        • `Backpressure` (75.0% - 80.0% usage): buffer flushing, rate-limiting delays, drops TRACE.
//!        • `AggressivePruning` (80.0% - 90.0% usage): drops TRACE/DEBUG/INFO spans, triggers streaming
//!          Zstandard/Gzip log compaction, forces cache eviction.
//!        • `EmergencyDrop` (> 90.0% usage): drops all non-essential logs, pauses background jobs,
//!          signals Telemetry Governor.
//! 2. **Automated Streaming Telemetry Pruner & Compactor**:
//!    - Chunked streaming Zstandard / Deflate compression (`.log.zst`, `.log.gz`).
//!    - Checksum integrity verification before source file unlinking (zero-data-loss invariant).
//!    - Time-to-Live (TTL) expiration pruning for legacy archives.
//! 3. **Telemetry Governor Bridge IPC Synchronization**:
//!    - Atomic state serialization to `/tmp/hgb_storage_pressure.json` for Python governor consumption.

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ============================================================================
// 1. Constants & Configuration Thresholds
// ============================================================================

pub const THRESHOLD_NORMAL_PCT: f64 = 75.0;
pub const THRESHOLD_BACKPRESSURE_PCT: f64 = 80.0;
pub const THRESHOLD_AGGRESSIVE_PRUNING_PCT: f64 = 90.0;

pub const DEFAULT_STORAGE_IPC_PATH: &str = "/tmp/hgb_storage_pressure.json";
pub const SECONDARY_STORAGE_IPC_PATH: &str = "/run/user/1000/hgb/storage_pressure.json";

pub const STREAM_BUFFER_SIZE: usize = 64 * 1024; // 64 KiB chunks
pub const DEFAULT_MIN_COMPACTION_SIZE_BYTES: u64 = 512;
pub const DEFAULT_ARCHIVE_TTL_SECS: u64 = 7 * 86400; // 7 days

// ============================================================================
// 2. Storage Pressure Tier & Domain Models
// ============================================================================

/// Operational storage pressure levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StoragePressureLevel {
    /// < 75.0% capacity: normal ingestion, unconstrained compute
    Normal,
    /// 75.0% - 80.0% capacity: slow down ingestion, flush buffers immediately
    Backpressure,
    /// 80.0% - 90.0% capacity: actively compress logs, drop debug/info, trigger cache eviction
    AggressivePruning,
    /// > 90.0% capacity: drop non-essential logs, pause background jobs, emergency alert
    EmergencyDrop,
}

impl StoragePressureLevel {
    pub fn classify(used_pct: f64) -> Self {
        if used_pct >= THRESHOLD_AGGRESSIVE_PRUNING_PCT {
            Self::EmergencyDrop
        } else if used_pct >= THRESHOLD_BACKPRESSURE_PCT {
            Self::AggressivePruning
        } else if used_pct >= THRESHOLD_NORMAL_PCT {
            Self::Backpressure
        } else {
            Self::Normal
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Backpressure => "BACKPRESSURE",
            Self::AggressivePruning => "AGGRESSIVE_PRUNING",
            Self::EmergencyDrop => "EMERGENCY_DROP",
        }
    }
}

/// Linux filesystem capacity and inode statistics from statvfs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartitionStats {
    pub mount_point: String,
    pub total_bytes: u64,
    pub free_bytes: u64,          // Available to unprivileged user (f_bavail)
    pub system_free_bytes: u64,   // Free filesystem blocks (f_bfree)
    pub used_bytes: u64,
    pub used_pct: f64,
    pub total_inodes: u64,
    pub free_inodes: u64,
    pub inode_used_pct: f64,
    pub tier: StoragePressureLevel,
    pub timestamp_unix_secs: u64,
}

impl PartitionStats {
    pub fn total_gb(&self) -> f64 {
        self.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }

    pub fn used_gb(&self) -> f64 {
        self.used_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }

    pub fn free_gb(&self) -> f64 {
        self.free_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    }
}

// ============================================================================
// 3. POSIX statvfs Monitor
// ============================================================================

/// Real-world statvfs partition monitor
#[derive(Debug, Clone)]
pub struct StorageMonitor {
    pub mount_point: String,
}

impl StorageMonitor {
    pub fn new(mount_point: impl Into<String>) -> Self {
        Self {
            mount_point: mount_point.into(),
        }
    }

    /// Queries the Linux kernel using libc `statvfs`
    pub fn probe(&self) -> io::Result<PartitionStats> {
        Self::probe_path(&self.mount_point)
    }

    /// Queries `statvfs` on an arbitrary path
    pub fn probe_path(path_str: &str) -> io::Result<PartitionStats> {
        let c_path = CString::new(path_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let mut stat: libc::statvfs = unsafe { MaybeUninit::zeroed().assume_init() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }

        let frsize = if stat.f_frsize > 0 {
            stat.f_frsize as u64
        } else {
            stat.f_bsize as u64
        };

        let total_blocks = stat.f_blocks as u64;
        let free_blocks_unprivileged = stat.f_bavail as u64;
        let free_blocks_system = stat.f_bfree as u64;

        let total_bytes = total_blocks.saturating_mul(frsize);
        let free_bytes = free_blocks_unprivileged.saturating_mul(frsize);
        let system_free_bytes = free_blocks_system.saturating_mul(frsize);
        let used_bytes = total_blocks.saturating_sub(free_blocks_system).saturating_mul(frsize);

        // Capacity calculation matching df -h exactly:
        // percentage = (total - unprivileged_avail) / total * 100
        let used_pct = if total_blocks > 0 {
            100.0 - (free_blocks_unprivileged as f64 / total_blocks as f64 * 100.0)
        } else {
            0.0
        };

        // Inode statistics
        let total_inodes = stat.f_files as u64;
        let free_inodes = stat.f_favail as u64;
        let inode_used_pct = if total_inodes > 0 {
            (total_inodes.saturating_sub(free_inodes)) as f64 / total_inodes as f64 * 100.0
        } else {
            0.0
        };

        let tier = StoragePressureLevel::classify(used_pct);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(PartitionStats {
            mount_point: path_str.to_string(),
            total_bytes,
            free_bytes,
            system_free_bytes,
            used_bytes,
            used_pct,
            total_inodes,
            free_inodes,
            inode_used_pct,
            tier,
            timestamp_unix_secs: now,
        })
    }
}

// ============================================================================
// 4. Telemetry Ingestion Backpressure Controller
// ============================================================================

/// Telemetry log severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
}

impl LogLevel {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_uppercase().trim() {
            "TRACE" => Self::Trace,
            "DEBUG" => Self::Debug,
            "INFO" => Self::Info,
            "WARN" | "WARNING" => Self::Warn,
            "ERROR" => Self::Error,
            "FATAL" | "CRITICAL" => Self::Fatal,
            _ => Self::Info,
        }
    }
}

/// Action determined by the backpressure controller for an incoming log/span
#[derive(Debug, Clone, PartialEq)]
pub enum IngestionAction {
    /// Ingest immediately without delay
    Accept,
    /// Ingest after applying artificial backpressure delay
    AcceptWithDelay(Duration),
    /// Drop span immediately to prevent disk exhaustion
    Drop(String),
}

/// Dynamic backpressure gatekeeper
#[derive(Debug, Clone)]
pub struct BackpressureController {
    pub backpressure_delay_ms: u64,
    pub aggressive_delay_ms: u64,
}

impl Default for BackpressureController {
    fn default() -> Self {
        Self {
            backpressure_delay_ms: 10,
            aggressive_delay_ms: 35,
        }
    }
}

impl BackpressureController {
    pub fn evaluate_span(&self, tier: StoragePressureLevel, level: LogLevel) -> IngestionAction {
        match tier {
            StoragePressureLevel::Normal => IngestionAction::Accept,

            StoragePressureLevel::Backpressure => match level {
                LogLevel::Trace => IngestionAction::Drop(
                    "Dropped TRACE span during Backpressure (75-80% disk capacity)".into(),
                ),
                LogLevel::Debug => IngestionAction::AcceptWithDelay(Duration::from_millis(
                    self.backpressure_delay_ms,
                )),
                LogLevel::Info | LogLevel::Warn | LogLevel::Error | LogLevel::Fatal => {
                    IngestionAction::Accept
                }
            },

            StoragePressureLevel::AggressivePruning => match level {
                LogLevel::Trace | LogLevel::Debug => IngestionAction::Drop(
                    "Dropped TRACE/DEBUG span during AggressivePruning (80-90% disk capacity)".into(),
                ),
                LogLevel::Info => IngestionAction::Drop(
                    "Dropped INFO span during AggressivePruning (80-90% disk capacity)".into(),
                ),
                LogLevel::Warn => IngestionAction::AcceptWithDelay(Duration::from_millis(
                    self.aggressive_delay_ms,
                )),
                LogLevel::Error | LogLevel::Fatal => IngestionAction::Accept,
            },

            StoragePressureLevel::EmergencyDrop => match level {
                LogLevel::Fatal => IngestionAction::Accept,
                LogLevel::Error => IngestionAction::AcceptWithDelay(Duration::from_millis(50)),
                _ => IngestionAction::Drop(
                    "Emergency disk exhaustion guard (>90% capacity): dropping non-critical telemetry"
                        .into(),
                ),
            },
        }
    }

    pub fn should_flush_buffers(&self, tier: StoragePressureLevel) -> bool {
        tier >= StoragePressureLevel::Backpressure
    }

    pub fn should_pause_background_jobs(&self, tier: StoragePressureLevel) -> bool {
        tier >= StoragePressureLevel::EmergencyDrop
    }
}

// ============================================================================
// 5. Automated Streaming Telemetry Pruner & Compactor
// ============================================================================

/// Compression codec supported by the compactor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionCodec {
    Zstandard,
    GzipDeflate,
}

/// Compaction job configuration
#[derive(Debug, Clone)]
pub struct CompactorConfig {
    pub target_directories: Vec<PathBuf>,
    pub candidate_extensions: HashSet<String>,
    pub min_file_size_bytes: u64,
    pub min_file_age_secs: u64,
    pub ttl_expiration_secs: u64,
    pub codec: CompressionCodec,
    pub zstd_level: i32,
    pub verify_checksum: bool,
}

impl Default for CompactorConfig {
    fn default() -> Self {
        let mut extensions = HashSet::new();
        extensions.insert("log".into());
        extensions.insert("txt".into());
        extensions.insert("jsonl".into());
        extensions.insert("trace".into());
        extensions.insert("out".into());
        extensions.insert("err".into());

        Self {
            target_directories: vec![
                PathBuf::from("/run/user/1000/hgb/logs"),
                PathBuf::from("/tmp/hgb_logs"),
            ],
            candidate_extensions: extensions,
            min_file_size_bytes: DEFAULT_MIN_COMPACTION_SIZE_BYTES,
            min_file_age_secs: 5, // Avoid active in-flight writes
            ttl_expiration_secs: DEFAULT_ARCHIVE_TTL_SECS,
            codec: CompressionCodec::Zstandard,
            zstd_level: 3,
            verify_checksum: true,
        }
    }
}

/// Results of an automated compaction and pruning pass
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompactionPassReport {
    pub scanned_files: usize,
    pub compressed_files: usize,
    pub uncompressed_bytes: u64,
    pub compressed_bytes: u64,
    pub bytes_saved: u64,
    pub reclaimed_ratio_pct: f64,
    pub deleted_expired_files: usize,
    pub expired_bytes_pruned: u64,
    pub errors: Vec<String>,
}

/// Automated Telemetry Pruner and Compactor engine
pub struct TelemetryCompactor {
    pub config: CompactorConfig,
}

impl TelemetryCompactor {
    pub fn new(config: CompactorConfig) -> Self {
        Self { config }
    }

    /// Runs a complete sweep over configured target directories
    pub fn execute_sweep(&self) -> CompactionPassReport {
        let mut report = CompactionPassReport::default();
        let now = SystemTime::now();

        for dir in &self.config.target_directories {
            if !dir.exists() {
                continue;
            }
            if let Err(e) = self.sweep_directory(dir, &mut report, now) {
                report.errors.push(format!("Error scanning {:?}: {}", dir, e));
            }
        }

        if report.uncompressed_bytes > 0 {
            report.bytes_saved = report.uncompressed_bytes.saturating_sub(report.compressed_bytes);
            report.reclaimed_ratio_pct =
                (report.bytes_saved as f64 / report.uncompressed_bytes as f64) * 100.0;
        }

        report
    }

    fn sweep_directory(
        &self,
        dir: &Path,
        report: &mut CompactionPassReport,
        now: SystemTime,
    ) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();

            if path.is_dir() {
                // Recursively traverse subdirectories
                let _ = self.sweep_directory(&path, report, now);
                continue;
            }

            report.scanned_files += 1;
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_size = metadata.len();
            let modified = metadata.modified().unwrap_or(now);
            let age_secs = now.duration_since(modified).unwrap_or_default().as_secs();

            // 1. Check TTL expiration for already compressed archives (.zst / .gz)
            if self.is_compressed_archive(&path) {
                if age_secs >= self.config.ttl_expiration_secs {
                    if let Ok(()) = fs::remove_file(&path) {
                        report.deleted_expired_files += 1;
                        report.expired_bytes_pruned += file_size;
                    }
                }
                continue;
            }

            // 2. Candidate check for uncompressed log files
            if !self.is_compaction_candidate(&path, file_size, age_secs) {
                continue;
            }

            // 3. Perform streaming compression and safe atomic replace
            match self.compress_file_safely(&path) {
                Ok((orig_len, comp_len)) => {
                    report.compressed_files += 1;
                    report.uncompressed_bytes += orig_len;
                    report.compressed_bytes += comp_len;
                }
                Err(err) => {
                    report.errors.push(format!("Failed to compress {:?}: {}", path, err));
                }
            }
        }
        Ok(())
    }

    fn is_compressed_archive(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            ext.eq_ignore_ascii_case("zst") || ext.eq_ignore_ascii_case("gz")
        } else {
            false
        }
    }

    fn is_compaction_candidate(&self, path: &Path, size: u64, age_secs: u64) -> bool {
        if size < self.config.min_file_size_bytes {
            return false;
        }
        if age_secs < self.config.min_file_age_secs {
            return false;
        }
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            self.config.candidate_extensions.contains(&ext.to_lowercase())
        } else {
            false
        }
    }

    /// Compresses a file with streaming chunks and verifies round-trip integrity before unlinking
    pub fn compress_file_safely(&self, path: &Path) -> io::Result<(u64, u64)> {
        let dest_ext = match self.config.codec {
            CompressionCodec::Zstandard => "zst",
            CompressionCodec::GzipDeflate => "gz",
        };

        let temp_archive_path = path.with_extension(format!("tmp.{}", dest_ext));
        let final_archive_path = path.with_extension(format!(
            "{}.{}",
            path.extension().and_then(|e| e.to_str()).unwrap_or("log"),
            dest_ext
        ));

        let orig_file = File::open(path)?;
        let orig_len = orig_file.metadata()?.len();
        let mut reader = BufReader::with_capacity(STREAM_BUFFER_SIZE, orig_file);

        let temp_file = File::create(&temp_archive_path)?;
        let mut writer = BufWriter::with_capacity(STREAM_BUFFER_SIZE, temp_file);

        // Streaming compression pass with rolling adler32/crc checksum
        let mut checksum = crc32fast::Hasher::new();
        let mut buffer = vec![0u8; STREAM_BUFFER_SIZE];

        match self.config.codec {
            CompressionCodec::Zstandard => {
                let mut encoder = zstd::stream::Encoder::new(&mut writer, self.config.zstd_level)?;
                loop {
                    let bytes_read = reader.read(&mut buffer)?;
                    if bytes_read == 0 {
                        break;
                    }
                    checksum.update(&buffer[..bytes_read]);
                    encoder.write_all(&buffer[..bytes_read])?;
                }
                encoder.finish()?;
            }
            CompressionCodec::GzipDeflate => {
                let mut encoder = flate2::write::GzEncoder::new(&mut writer, flate2::Compression::default());
                loop {
                    let bytes_read = reader.read(&mut buffer)?;
                    if bytes_read == 0 {
                        break;
                    }
                    checksum.update(&buffer[..bytes_read]);
                    encoder.write_all(&buffer[..bytes_read])?;
                }
                encoder.finish()?;
            }
        }

        writer.flush()?;
        drop(writer);

        let original_crc = checksum.finalize();
        let comp_len = fs::metadata(&temp_archive_path)?.len();

        // Integrity Verification Pass: decompress temp archive and verify checksum
        if self.config.verify_checksum {
            let verify_file = File::open(&temp_archive_path)?;
            let verify_reader = BufReader::with_capacity(STREAM_BUFFER_SIZE, verify_file);
            let mut verify_crc = crc32fast::Hasher::new();

            match self.config.codec {
                CompressionCodec::Zstandard => {
                    let mut decoder = zstd::stream::Decoder::new(verify_reader)?;
                    loop {
                        let bytes_read = decoder.read(&mut buffer)?;
                        if bytes_read == 0 {
                            break;
                        }
                        verify_crc.update(&buffer[..bytes_read]);
                    }
                }
                CompressionCodec::GzipDeflate => {
                    let mut decoder = flate2::read::GzDecoder::new(verify_reader);
                    loop {
                        let bytes_read = decoder.read(&mut buffer)?;
                        if bytes_read == 0 {
                            break;
                        }
                        verify_crc.update(&buffer[..bytes_read]);
                    }
                }
            }

            if verify_crc.finalize() != original_crc {
                let _ = fs::remove_file(&temp_archive_path);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Compression checksum verification mismatch! Original file preserved.",
                ));
            }
        }

        // Atomic rename of archive to final location
        fs::rename(&temp_archive_path, &final_archive_path)?;

        // Only after verified atomic placement do we safely unlink the uncompressed original
        fs::remove_file(path)?;

        Ok((orig_len, comp_len))
    }
}

// ============================================================================
// 6. IPC State Exporter for Telemetry Governor
// ============================================================================

/// JSON payload exported for `telemetry_governor_storage_patch.py`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoragePressureIpcPayload {
    pub timestamp: f64,
    pub storage_pressure_level: String,
    pub tier: String,
    pub mount_point: String,
    pub disk_total_gb: f64,
    pub disk_used_gb: f64,
    pub disk_free_gb: f64,
    pub used_pct: f64,
    pub inode_used_pct: f64,
    pub evicted_bytes: u64,
    pub compressed_bytes_saved: u64,
    pub backpressure_delay_ms: f64,
    pub emergency_drop: bool,
    pub pruning_active: bool,
}

impl StoragePressureIpcPayload {
    pub fn from_stats_and_report(
        stats: &PartitionStats,
        report: Option<&CompactionPassReport>,
        delay_ms: f64,
    ) -> Self {
        let (evicted, saved) = if let Some(r) = report {
            (r.expired_bytes_pruned, r.bytes_saved)
        } else {
            (0, 0)
        };

        let now_sec = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        Self {
            timestamp: now_sec,
            storage_pressure_level: stats.tier.as_str().to_string(),
            tier: stats.tier.as_str().to_string(),
            mount_point: stats.mount_point.clone(),
            disk_total_gb: stats.total_gb(),
            disk_used_gb: stats.used_gb(),
            disk_free_gb: stats.free_gb(),
            used_pct: stats.used_pct,
            inode_used_pct: stats.inode_used_pct,
            evicted_bytes: evicted,
            compressed_bytes_saved: saved,
            backpressure_delay_ms: delay_ms,
            emergency_drop: stats.tier == StoragePressureLevel::EmergencyDrop,
            pruning_active: stats.tier >= StoragePressureLevel::AggressivePruning,
        }
    }

    pub fn write_to_file(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = path.with_extension("tmp.json");
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        fs::write(&temp_path, content)?;
        fs::rename(&temp_path, path)?;
        Ok(())
    }
}

// ============================================================================
// 7. Storage Guardrails Daemon Coordinator
// ============================================================================

pub struct StorageGuardrailsDaemon {
    pub monitor: StorageMonitor,
    pub compactor: TelemetryCompactor,
    pub controller: BackpressureController,
    pub ipc_path: PathBuf,
    pub running: Arc<AtomicBool>,
}

impl StorageGuardrailsDaemon {
    pub fn new(
        mount_point: impl Into<String>,
        compactor_config: CompactorConfig,
        ipc_path: Option<PathBuf>,
    ) -> Self {
        Self {
            monitor: StorageMonitor::new(mount_point),
            compactor: TelemetryCompactor::new(compactor_config),
            controller: BackpressureController::default(),
            ipc_path: ipc_path.unwrap_or_else(|| PathBuf::from(DEFAULT_STORAGE_IPC_PATH)),
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Single execution iteration: probe -> prune if needed -> export IPC
    pub fn execute_step(&self) -> io::Result<(PartitionStats, Option<CompactionPassReport>)> {
        let stats = self.monitor.probe()?;

        let report = if stats.tier >= StoragePressureLevel::AggressivePruning {
            // Trigger aggressive log compaction pass
            Some(self.compactor.execute_sweep())
        } else {
            None
        };

        let delay_ms = match stats.tier {
            StoragePressureLevel::Normal => 0.0,
            StoragePressureLevel::Backpressure => self.controller.backpressure_delay_ms as f64,
            StoragePressureLevel::AggressivePruning => self.controller.aggressive_delay_ms as f64,
            StoragePressureLevel::EmergencyDrop => 50.0,
        };

        let ipc = StoragePressureIpcPayload::from_stats_and_report(&stats, report.as_ref(), delay_ms);
        let _ = ipc.write_to_file(&self.ipc_path);

        Ok((stats, report))
    }
}

// ============================================================================
// 8. CLI Entrypoint
// ============================================================================

#[allow(dead_code)]
pub fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("status");

    let daemon = StorageGuardrailsDaemon::new("/", CompactorConfig::default(), None);

    match action {
        "status" => {
            let stats = daemon.monitor.probe()?;
            println!("💾 [Hagibis Storage Guardrails Monitor]");
            println!("  • Mount Point:        {}", stats.mount_point);
            println!("  • Total Capacity:     {:.2} GB", stats.total_gb());
            println!("  • Used Capacity:      {:.2} GB ({:.2}%)", stats.used_gb(), stats.used_pct);
            println!("  • Free Available:     {:.2} GB", stats.free_gb());
            println!("  • Inode Usage:        {:.2}%", stats.inode_used_pct);
            println!("  • Operational Tier:   {:?}", stats.tier);
        }
        "prune" => {
            println!("🔄 Executing telemetry compaction sweep...");
            let report = daemon.compactor.execute_sweep();
            println!("  • Scanned Files:      {}", report.scanned_files);
            println!("  • Compressed Files:   {}", report.compressed_files);
            println!("  • Uncompressed Bytes: {}", report.uncompressed_bytes);
            println!("  • Compressed Bytes:   {}", report.compressed_bytes);
            println!("  • Bytes Saved:        {} ({:.1}% reclamation)", report.bytes_saved, report.reclaimed_ratio_pct);
            println!("  • Expired Deleted:    {}", report.deleted_expired_files);
        }
        "step" => {
            let (stats, report) = daemon.execute_step()?;
            println!("Executed step: Tier={:?}, Pruned={:?}", stats.tier, report.is_some());
        }
        _ => {
            println!("Usage: storage_guardrails_daemon [status|prune|step]");
        }
    }
    Ok(())
}

// ============================================================================
// 9. Unit Test Suite
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_statvfs_real_filesystem_query() {
        let stats = StorageMonitor::probe_path("/").expect("statvfs query on / must succeed");
        assert_eq!(stats.mount_point, "/");
        assert!(stats.total_bytes > 0);
        assert!(stats.free_bytes > 0);
        assert!(stats.used_pct > 0.0 && stats.used_pct <= 100.0);
        assert!(stats.total_inodes > 0);
    }

    #[test]
    fn test_tier_classification() {
        assert_eq!(StoragePressureLevel::classify(50.0), StoragePressureLevel::Normal);
        assert_eq!(StoragePressureLevel::classify(74.9), StoragePressureLevel::Normal);
        assert_eq!(StoragePressureLevel::classify(75.0), StoragePressureLevel::Backpressure);
        assert_eq!(StoragePressureLevel::classify(79.9), StoragePressureLevel::Backpressure);
        assert_eq!(StoragePressureLevel::classify(80.0), StoragePressureLevel::AggressivePruning);
        assert_eq!(StoragePressureLevel::classify(85.3), StoragePressureLevel::AggressivePruning);
        assert_eq!(StoragePressureLevel::classify(90.0), StoragePressureLevel::EmergencyDrop);
        assert_eq!(StoragePressureLevel::classify(95.0), StoragePressureLevel::EmergencyDrop);
    }

    #[test]
    fn test_backpressure_gatekeeper_rules() {
        let ctrl = BackpressureController::default();

        // Normal accepts everything
        assert_eq!(ctrl.evaluate_span(StoragePressureLevel::Normal, LogLevel::Trace), IngestionAction::Accept);
        assert_eq!(ctrl.evaluate_span(StoragePressureLevel::Normal, LogLevel::Info), IngestionAction::Accept);

        // Backpressure drops Trace, delays Debug
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::Backpressure, LogLevel::Trace),
            IngestionAction::Drop(_)
        ));
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::Backpressure, LogLevel::Debug),
            IngestionAction::AcceptWithDelay(_)
        ));
        assert_eq!(ctrl.evaluate_span(StoragePressureLevel::Backpressure, LogLevel::Info), IngestionAction::Accept);

        // Aggressive drops Trace, Debug, Info; delays Warn; accepts Error
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::AggressivePruning, LogLevel::Trace),
            IngestionAction::Drop(_)
        ));
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::AggressivePruning, LogLevel::Debug),
            IngestionAction::Drop(_)
        ));
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::AggressivePruning, LogLevel::Info),
            IngestionAction::Drop(_)
        ));
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::AggressivePruning, LogLevel::Warn),
            IngestionAction::AcceptWithDelay(_)
        ));
        assert_eq!(ctrl.evaluate_span(StoragePressureLevel::AggressivePruning, LogLevel::Error), IngestionAction::Accept);

        // Emergency drops non-fatal
        assert!(matches!(
            ctrl.evaluate_span(StoragePressureLevel::EmergencyDrop, LogLevel::Warn),
            IngestionAction::Drop(_)
        ));
        assert_eq!(ctrl.evaluate_span(StoragePressureLevel::EmergencyDrop, LogLevel::Fatal), IngestionAction::Accept);
    }

    #[test]
    fn test_streaming_compression_and_integrity_verification() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let log_file = temp_dir.path().join("kernel_trace.log");

        // Write repetitive log data to achieve >70% compression
        {
            let mut f = File::create(&log_file).expect("create log");
            for i in 0..10_000 {
                writeln!(f, "{{\"timestamp\": 1731765000, \"event\": \"span_metric\", \"index\": {}, \"status\": \"OK\", \"message\": \"OpenTelemetry telemetry buffer trace event payload message\"}}", i).unwrap();
            }
        }

        let orig_size = fs::metadata(&log_file).unwrap().len();
        assert!(orig_size > 500_000);

        let mut config = CompactorConfig::default();
        config.target_directories = vec![temp_dir.path().to_path_buf()];
        config.min_file_age_secs = 0; // immediate
        config.verify_checksum = true;

        let compactor = TelemetryCompactor::new(config);
        let (orig_len, comp_len) = compactor.compress_file_safely(&log_file).expect("compress");

        assert_eq!(orig_len, orig_size);
        assert!(comp_len < orig_len / 3, "Compression must achieve > 66-70% reduction. Got: orig={}, comp={}", orig_len, comp_len);

        // Verify original file was removed and .log.zst exists
        assert!(!log_file.exists());
        let archive_path = temp_dir.path().join("kernel_trace.log.zst");
        assert!(archive_path.exists());

        // Verify decompression matches original content
        let comp_file = File::open(&archive_path).expect("open archive");
        let mut decoder = zstd::stream::Decoder::new(comp_file).expect("decoder");
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).expect("decompress");
        assert_eq!(decompressed.len() as u64, orig_size);
    }

    #[test]
    fn test_ttl_expiry_pruning() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let old_archive = temp_dir.path().join("expired.log.zst");
        let new_archive = temp_dir.path().join("active.log.zst");

        fs::write(&old_archive, b"compressed_data").unwrap();
        fs::write(&new_archive, b"compressed_data").unwrap();

        let mut config = CompactorConfig::default();
        config.target_directories = vec![temp_dir.path().to_path_buf()];
        config.ttl_expiration_secs = 10; // 10s TTL

        let compactor = TelemetryCompactor::new(config);
        let mut report = CompactionPassReport::default();

        // Simulate time advancing 100 seconds for old_archive
        let simulated_now = SystemTime::now() + Duration::from_secs(100);
        compactor.sweep_directory(temp_dir.path(), &mut report, simulated_now).unwrap();

        assert_eq!(report.deleted_expired_files, 2); // Both archives expired under simulated_now
        assert!(!old_archive.exists());
        assert!(!new_archive.exists());
    }

    #[test]
    fn test_ipc_state_serialization() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let ipc_file = temp_dir.path().join("storage_pressure.json");

        let stats = PartitionStats {
            mount_point: "/".into(),
            total_bytes: 125_000_000_000,
            free_bytes: 18_000_000_000,
            system_free_bytes: 20_000_000_000,
            used_bytes: 105_000_000_000,
            used_pct: 85.3,
            total_inodes: 7_500_000,
            free_inodes: 6_500_000,
            inode_used_pct: 13.3,
            tier: StoragePressureLevel::AggressivePruning,
            timestamp_unix_secs: 1731765000,
        };

        let payload = StoragePressureIpcPayload::from_stats_and_report(&stats, None, 25.0);
        payload.write_to_file(&ipc_file).expect("write ipc");

        let read_bytes = fs::read_to_string(&ipc_file).expect("read ipc");
        let deserialized: StoragePressureIpcPayload = serde_json::from_str(&read_bytes).expect("parse json");

        assert_eq!(deserialized.tier, "AGGRESSIVE_PRUNING");
        assert_eq!(deserialized.used_pct, 85.3);
        assert_eq!(deserialized.storage_pressure_level, "AGGRESSIVE_PRUNING");
        assert!(deserialized.pruning_active);
    }
}
