//! # Resident Daemon Shared-Memory Heartbeat Watchdog
//!
//! Sub-millisecond, lock-free periodic heartbeat updater stored in POSIX
//! shared memory (`/dev/shm/hgb_heartbeat.bin` with fallback to `std::env::temp_dir().join("hgb_heartbeat.bin")`).
//!
//! Monitors daemon health, active worker count, memory RSS, and last tick timestamp.
//! Provides `check_daemon_health(max_stale_secs: u64) -> DaemonLivenessStatus` with kernel
//! process liveness detection.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

use hgb_core::Result;

/// Magic identifier for Hagibis Heartbeat Shared Memory ("HGBHEART")
pub const HEARTBEAT_MAGIC: u64 = 0x4847424845415254;

/// Specification version for the heartbeat format
pub const HEARTBEAT_VERSION: u32 = 1;

/// Default shared-memory heartbeat path
pub const DEFAULT_HEARTBEAT_PATH: &str = "/dev/shm/hgb_heartbeat.bin";

/// Header size in bytes
pub const HEARTBEAT_HEADER_SIZE: usize = 64;

/// Cacheline-aligned lock-free binary heartbeat header
#[repr(C, align(64))]
pub struct HeartbeatHeader {
    pub magic: u64,
    pub version: u32,
    pub flags: AtomicU32,
    pub pid: AtomicU32,
    pub active_workers: AtomicU32,
    pub start_timestamp_secs: AtomicU64,
    pub last_tick_secs: AtomicU64,
    pub tick_sequence: AtomicU64,
    pub memory_rss_kb: AtomicU64,
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<HeartbeatHeader>() == HEARTBEAT_HEADER_SIZE);

/// Liveness status evaluation outcome
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DaemonLivenessStatus {
    /// Daemon process is running and updating heartbeat within threshold
    Alive {
        pid: u32,
        last_tick_secs_ago: u64,
        active_workers: u32,
        memory_rss_mb: f64,
        sequence: u64,
        uptime_secs: u64,
    },
    /// Daemon process exists in kernel table but heartbeat has ceased updating
    Stale {
        pid: u32,
        last_tick_secs_ago: u64,
        active_workers: u32,
        memory_rss_mb: f64,
        max_allowed_stale_secs: u64,
    },
    /// Heartbeat file exists but daemon process PID is dead
    ProcessDead {
        pid: u32,
        last_tick_secs_ago: u64,
    },
    /// No heartbeat file found
    NotRunning,
    /// Header was present but magic or version corrupted
    CorruptHeader(String),
}

/// High-level heartbeat telemetry snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatStatusSnapshot {
    pub magic: u64,
    pub version: u32,
    pub pid: u32,
    pub active_workers: u32,
    pub start_timestamp_secs: u64,
    pub last_tick_secs: u64,
    pub tick_sequence: u64,
    pub memory_rss_mb: f64,
    pub uptime_secs: u64,
    pub last_tick_secs_ago: u64,
}

/// Resolve path to the shared heartbeat file
pub fn resolve_heartbeat_path() -> PathBuf {
    let shm_dir = Path::new("/dev/shm");
    if shm_dir.is_dir() && is_shm_writable(shm_dir) {
        PathBuf::from(DEFAULT_HEARTBEAT_PATH)
    } else {
        std::env::temp_dir().join("hgb_heartbeat.bin")
    }
}

fn is_shm_writable(shm_dir: &Path) -> bool {
    let probe = shm_dir.join(format!(".hgb_hb_probe_{}", std::process::id()));
    match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

/// Periodic atomic heartbeat updater stored in shared memory
pub struct HeartbeatWatchdog {
    path: PathBuf,
    mmap: MmapMut,
    unlink_on_drop: bool,
}

impl HeartbeatWatchdog {
    /// Create or open the default resident daemon heartbeat watchdog
    pub fn new_or_default() -> Result<Self> {
        let path = resolve_heartbeat_path();
        Self::open_or_create(&path)
    }

    /// Open or initialize a heartbeat watchdog at a custom path
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        file.set_len(HEARTBEAT_HEADER_SIZE as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let initial_header = HeartbeatHeader {
            magic: HEARTBEAT_MAGIC,
            version: HEARTBEAT_VERSION,
            flags: AtomicU32::new(1),
            pid: AtomicU32::new(std::process::id()),
            active_workers: AtomicU32::new(0),
            start_timestamp_secs: AtomicU64::new(now_secs),
            last_tick_secs: AtomicU64::new(now_secs),
            tick_sequence: AtomicU64::new(0),
            memory_rss_kb: AtomicU64::new((crate::get_memory_rss_mb() * 1024.0) as u64),
            _reserved: [0u8; 8],
        };

        let raw_bytes: [u8; HEARTBEAT_HEADER_SIZE] = unsafe { std::mem::transmute(initial_header) };
        mmap[..HEARTBEAT_HEADER_SIZE].copy_from_slice(&raw_bytes);
        mmap.flush()?;

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            unlink_on_drop: false,
        })
    }

    #[inline]
    fn header(&self) -> &HeartbeatHeader {
        unsafe { &*(self.mmap.as_ptr() as *const HeartbeatHeader) }
    }

    /// Record a single atomic heartbeat tick
    pub fn tick(&self, active_workers: u32, memory_rss_mb: f64) -> Result<()> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let h = self.header();
        h.active_workers.store(active_workers, Ordering::Release);
        h.last_tick_secs.store(now_secs, Ordering::Release);
        h.tick_sequence.fetch_add(1, Ordering::Release);
        h.memory_rss_kb
            .store((memory_rss_mb * 1024.0) as u64, Ordering::Release);

        Ok(())
    }

    pub fn update_workers(&self, active_workers: u32) {
        self.header().active_workers.store(active_workers, Ordering::Release);
    }

    pub fn snapshot(&self) -> HeartbeatStatusSnapshot {
        let h = self.header();
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let start = h.start_timestamp_secs.load(Ordering::Acquire);
        let last_tick = h.last_tick_secs.load(Ordering::Acquire);
        let rss_kb = h.memory_rss_kb.load(Ordering::Acquire);

        HeartbeatStatusSnapshot {
            magic: h.magic,
            version: h.version,
            pid: h.pid.load(Ordering::Acquire),
            active_workers: h.active_workers.load(Ordering::Acquire),
            start_timestamp_secs: start,
            last_tick_secs: last_tick,
            tick_sequence: h.tick_sequence.load(Ordering::Acquire),
            memory_rss_mb: (rss_kb as f64) / 1024.0,
            uptime_secs: now_secs.saturating_sub(start),
            last_tick_secs_ago: now_secs.saturating_sub(last_tick),
        }
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    pub fn start_background_loop(
        watchdog: Arc<Self>,
        interval: Duration,
        active_worker_provider: Option<Arc<dyn Fn() -> u32 + Send + Sync>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let workers = active_worker_provider
                    .as_ref()
                    .map(|f| f())
                    .unwrap_or(0);
                let rss = crate::get_memory_rss_mb();
                if watchdog.tick(workers, rss).is_err() {
                    break;
                }
            }
        })
    }
}

impl Drop for HeartbeatWatchdog {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop && self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Check daemon health status using shared-memory heartbeat and kernel process probe
pub fn check_daemon_health(max_stale_secs: u64) -> DaemonLivenessStatus {
    let path = resolve_heartbeat_path();
    check_daemon_health_at(&path, max_stale_secs)
}

/// Check daemon health status at a specific heartbeat file path
pub fn check_daemon_health_at(path: &Path, max_stale_secs: u64) -> DaemonLivenessStatus {
    if !path.exists() {
        return DaemonLivenessStatus::NotRunning;
    }

    let file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(_) => return DaemonLivenessStatus::NotRunning,
    };

    let meta = match file.metadata() {
        Ok(m) => m,
        Err(_) => return DaemonLivenessStatus::NotRunning,
    };

    if meta.len() < HEARTBEAT_HEADER_SIZE as u64 {
        return DaemonLivenessStatus::CorruptHeader("Heartbeat file smaller than header".into());
    }

    let mmap = match unsafe { memmap2::Mmap::map(&file) } {
        Ok(m) => m,
        Err(e) => {
            return DaemonLivenessStatus::CorruptHeader(format!("Mmap error: {}", e))
        }
    };

    let header_ref = unsafe { &*(mmap.as_ptr() as *const HeartbeatHeader) };

    if header_ref.magic != HEARTBEAT_MAGIC {
        return DaemonLivenessStatus::CorruptHeader(format!(
            "Invalid magic 0x{:X}",
            header_ref.magic
        ));
    }
    if header_ref.version != HEARTBEAT_VERSION {
        return DaemonLivenessStatus::CorruptHeader(format!(
            "Invalid version {}",
            header_ref.version
        ));
    }

    let pid = header_ref.pid.load(Ordering::Acquire);
    let last_tick = header_ref.last_tick_secs.load(Ordering::Acquire);
    let active_workers = header_ref.active_workers.load(Ordering::Acquire);
    let sequence = header_ref.tick_sequence.load(Ordering::Acquire);
    let start_timestamp = header_ref.start_timestamp_secs.load(Ordering::Acquire);
    let memory_rss_kb = header_ref.memory_rss_kb.load(Ordering::Acquire);
    let memory_rss_mb = (memory_rss_kb as f64) / 1024.0;

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let last_tick_secs_ago = now_secs.saturating_sub(last_tick);
    let uptime_secs = now_secs.saturating_sub(start_timestamp);

    // Cross-check process existence via libc::kill(pid, 0)
    let is_alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };

    if !is_alive {
        return DaemonLivenessStatus::ProcessDead {
            pid,
            last_tick_secs_ago,
        };
    }

    if last_tick_secs_ago > max_stale_secs {
        DaemonLivenessStatus::Stale {
            pid,
            last_tick_secs_ago,
            active_workers,
            memory_rss_mb,
            max_allowed_stale_secs: max_stale_secs,
        }
    } else {
        DaemonLivenessStatus::Alive {
            pid,
            last_tick_secs_ago,
            active_workers,
            memory_rss_mb,
            sequence,
            uptime_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_watchdog_tick() {
        let test_path = std::env::temp_dir().join(format!("test_hb_{}.bin", std::process::id()));
        if test_path.exists() {
            let _ = std::fs::remove_file(&test_path);
        }

        let watchdog = HeartbeatWatchdog::open_or_create(&test_path).expect("open or create");
        let initial_snap = watchdog.snapshot();
        assert_eq!(initial_snap.magic, HEARTBEAT_MAGIC);
        assert_eq!(initial_snap.version, HEARTBEAT_VERSION);
        assert_eq!(initial_snap.pid, std::process::id());
        assert_eq!(initial_snap.active_workers, 0);

        watchdog.tick(8, 128.5).expect("tick");
        let after_tick = watchdog.snapshot();
        assert_eq!(after_tick.active_workers, 8);
        assert_eq!(after_tick.tick_sequence, 1);
        assert!((after_tick.memory_rss_mb - 128.5).abs() < 0.1);

        let status = check_daemon_health_at(&test_path, 10);
        match status {
            DaemonLivenessStatus::Alive { pid, active_workers, sequence, .. } => {
                assert_eq!(pid, std::process::id());
                assert_eq!(active_workers, 8);
                assert_eq!(sequence, 1);
            }
            other => panic!("Expected Alive status, got {:?}", other),
        }

        let _ = std::fs::remove_file(&test_path);
    }

    #[test]
    fn test_check_daemon_health_stale_and_dead() {
        let test_path = std::env::temp_dir().join(format!("test_hb_stale_{}.bin", std::process::id()));
        if test_path.exists() {
            let _ = std::fs::remove_file(&test_path);
        }

        let watchdog = HeartbeatWatchdog::open_or_create(&test_path).expect("open or create");
        watchdog.tick(4, 64.0).expect("tick");

        // Artificially age the tick timestamp by 100 seconds
        let past = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(100);
        watchdog.header().last_tick_secs.store(past, Ordering::Release);

        let status = check_daemon_health_at(&test_path, 10);
        match status {
            DaemonLivenessStatus::Stale { pid, last_tick_secs_ago, max_allowed_stale_secs, .. } => {
                assert_eq!(pid, std::process::id());
                assert!(last_tick_secs_ago >= 100);
                assert_eq!(max_allowed_stale_secs, 10);
            }
            other => panic!("Expected Stale status, got {:?}", other),
        }

        // Test with non-existent process PID 9999999
        watchdog.header().pid.store(9999999, Ordering::Release);
        let status_dead = check_daemon_health_at(&test_path, 10);
        match status_dead {
            DaemonLivenessStatus::ProcessDead { pid, .. } => {
                assert_eq!(pid, 9999999);
            }
            other => panic!("Expected ProcessDead status, got {:?}", other),
        }

        let _ = std::fs::remove_file(&test_path);
    }
}
