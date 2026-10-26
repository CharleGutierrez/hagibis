//! # Memory-Mapped Crash & Panic Dump Recorder
//!
//! Instantaneous shared-memory panic recording to `/dev/shm/hgb_panic.bin`
//! (with automatic fallback to `std::env::temp_dir().join("hgb_panic.bin")`).
//!
//! Captures:
//! - Magic `0x48474250414E4943` ("HGBPANIC")
//! - Schema version 1
//! - RFC3339 timestamp and epoch milliseconds
//! - Process ID (PID) and Thread name
//! - Panic source location (file, line, column)
//! - Panic payload message string
//! - Full captured backtrace

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::SystemTime;

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

use crate::{HgbError, Result};

/// Magic identifier for Hagibis Panic Dump ("HGBPANIC")
pub const SHM_PANIC_MAGIC: u64 = 0x48474250414E4943;

/// Panic dump format specification version
pub const SHM_PANIC_VERSION: u32 = 1;

/// Fixed 64-byte binary header
pub const SHM_PANIC_HEADER_SIZE: usize = 64;

/// Default filename for the crash dump
pub const SHM_PANIC_FILENAME: &str = "hgb_panic.bin";

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static INIT_HOOK: Once = Once::new();

/// Cacheline-aligned 64-byte binary header for crash dumps
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct ShmPanicHeader {
    pub magic: u64,
    pub version: u32,
    pub flags: u32,
    pub timestamp_ms: u64,
    pub pid: u32,
    pub location_line: u32,
    pub location_col: u32,
    pub thread_name_len: u32,
    pub file_len: u32,
    pub msg_len: u32,
    pub backtrace_len: u32,
    pub rfc3339_len: u32,
    pub payload_total_len: u32,
    pub _reserved: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<ShmPanicHeader>() == SHM_PANIC_HEADER_SIZE);

/// High-level parsed panic dump record
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShmPanicDump {
    pub magic: u64,
    pub version: u32,
    pub timestamp_ms: u64,
    pub timestamp_rfc3339: String,
    pub pid: u32,
    pub thread_name: String,
    pub location_file: String,
    pub location_line: u32,
    pub location_col: u32,
    pub message: String,
    pub backtrace: String,
}

/// Resolve the crash dump file path (`/dev/shm` primary, `temp_dir` fallback)
pub fn resolve_panic_dump_path() -> PathBuf {
    let shm_dir = Path::new("/dev/shm");
    if shm_dir.is_dir() && is_shm_writable(shm_dir) {
        shm_dir.join(SHM_PANIC_FILENAME)
    } else {
        std::env::temp_dir().join(SHM_PANIC_FILENAME)
    }
}

fn is_shm_writable(shm_dir: &Path) -> bool {
    let probe = shm_dir.join(format!(".hgb_panic_probe_{}", std::process::id()));
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

/// Write panic dump directly into memory-mapped shared file
pub fn write_panic_dump(dump: &ShmPanicDump) -> Result<PathBuf> {
    let path = resolve_panic_dump_path();
    let rfc_bytes = dump.timestamp_rfc3339.as_bytes();
    let thread_bytes = dump.thread_name.as_bytes();
    let file_bytes = dump.location_file.as_bytes();
    let msg_bytes = dump.message.as_bytes();
    let bt_bytes = dump.backtrace.as_bytes();

    let payload_len = rfc_bytes.len()
        + thread_bytes.len()
        + file_bytes.len()
        + msg_bytes.len()
        + bt_bytes.len();

    let total_size = SHM_PANIC_HEADER_SIZE + payload_len;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;

    file.set_len(total_size as u64)?;
    let mut mmap = unsafe { MmapMut::map_mut(&file)? };

    let header = ShmPanicHeader {
        magic: SHM_PANIC_MAGIC,
        version: SHM_PANIC_VERSION,
        flags: 1, // active valid crash dump
        timestamp_ms: dump.timestamp_ms,
        pid: dump.pid,
        location_line: dump.location_line,
        location_col: dump.location_col,
        rfc3339_len: rfc_bytes.len() as u32,
        thread_name_len: thread_bytes.len() as u32,
        file_len: file_bytes.len() as u32,
        msg_len: msg_bytes.len() as u32,
        backtrace_len: bt_bytes.len() as u32,
        payload_total_len: payload_len as u32,
        _reserved: [0u8; 4],
    };

    let header_bytes: [u8; SHM_PANIC_HEADER_SIZE] = unsafe { std::mem::transmute(header) };
    mmap[..SHM_PANIC_HEADER_SIZE].copy_from_slice(&header_bytes);

    let mut cursor = SHM_PANIC_HEADER_SIZE;
    mmap[cursor..cursor + rfc_bytes.len()].copy_from_slice(rfc_bytes);
    cursor += rfc_bytes.len();

    mmap[cursor..cursor + thread_bytes.len()].copy_from_slice(thread_bytes);
    cursor += thread_bytes.len();

    mmap[cursor..cursor + file_bytes.len()].copy_from_slice(file_bytes);
    cursor += file_bytes.len();

    mmap[cursor..cursor + msg_bytes.len()].copy_from_slice(msg_bytes);
    cursor += msg_bytes.len();

    mmap[cursor..cursor + bt_bytes.len()].copy_from_slice(bt_bytes);

    mmap.flush()?;
    Ok(path)
}

/// Read the last panic dump from shared memory or temporary storage
pub fn read_last_panic_dump() -> Option<ShmPanicDump> {
    let candidates = [
        Path::new("/dev/shm").join(SHM_PANIC_FILENAME),
        std::env::temp_dir().join(SHM_PANIC_FILENAME),
    ];

    for path in &candidates {
        if !path.exists() {
            continue;
        }
        let file = match OpenOptions::new().read(true).open(path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let meta = match file.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() < SHM_PANIC_HEADER_SIZE as u64 {
            continue;
        }

        let mmap = match unsafe { memmap2::Mmap::map(&file) } {
            Ok(m) => m,
            Err(_) => continue,
        };

        let header_slice: &[u8; SHM_PANIC_HEADER_SIZE] =
            match mmap[..SHM_PANIC_HEADER_SIZE].try_into() {
                Ok(s) => s,
                Err(_) => continue,
            };

        let header: ShmPanicHeader = unsafe { std::mem::transmute(*header_slice) };

        if header.magic != SHM_PANIC_MAGIC
            || header.version != SHM_PANIC_VERSION
            || header.flags == 0
        {
            continue;
        }

        let expected_total = SHM_PANIC_HEADER_SIZE + (header.payload_total_len as usize);
        if mmap.len() < expected_total {
            continue;
        }

        let mut cursor = SHM_PANIC_HEADER_SIZE;

        let rfc3339_end = cursor + header.rfc3339_len as usize;
        let timestamp_rfc3339 =
            String::from_utf8_lossy(&mmap[cursor..rfc3339_end]).to_string();
        cursor = rfc3339_end;

        let thread_end = cursor + header.thread_name_len as usize;
        let thread_name = String::from_utf8_lossy(&mmap[cursor..thread_end]).to_string();
        cursor = thread_end;

        let file_end = cursor + header.file_len as usize;
        let location_file = String::from_utf8_lossy(&mmap[cursor..file_end]).to_string();
        cursor = file_end;

        let msg_end = cursor + header.msg_len as usize;
        let message = String::from_utf8_lossy(&mmap[cursor..msg_end]).to_string();
        cursor = msg_end;

        let bt_end = cursor + header.backtrace_len as usize;
        let backtrace = String::from_utf8_lossy(&mmap[cursor..bt_end]).to_string();

        return Some(ShmPanicDump {
            magic: header.magic,
            version: header.version,
            timestamp_ms: header.timestamp_ms,
            timestamp_rfc3339,
            pid: header.pid,
            thread_name,
            location_file,
            location_line: header.location_line,
            location_col: header.location_col,
            message,
            backtrace,
        });
    }

    None
}

/// Clear existing crash dump records from shared memory
pub fn clear_panic_dump() -> Result<()> {
    let paths = [
        Path::new("/dev/shm").join(SHM_PANIC_FILENAME),
        std::env::temp_dir().join(SHM_PANIC_FILENAME),
    ];
    for p in &paths {
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(())
}

/// Record panic hook details into memory-mapped crash dump
pub fn record_panic_hook_info(info: &std::panic::PanicHookInfo) {
    let now = SystemTime::now();
    let timestamp_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let timestamp_rfc3339 = chrono::Utc::now().to_rfc3339();

    let pid = std::process::id();
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();

    let (location_file, location_line, location_col) = if let Some(loc) = info.location() {
        (loc.file().to_string(), loc.line(), loc.column())
    } else {
        ("unknown".to_string(), 0, 0)
    };

    let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any> panic payload".to_string()
    };

    let backtrace = std::backtrace::Backtrace::capture().to_string();

    let dump = ShmPanicDump {
        magic: SHM_PANIC_MAGIC,
        version: SHM_PANIC_VERSION,
        timestamp_ms,
        timestamp_rfc3339,
        pid,
        thread_name,
        location_file,
        location_line,
        location_col,
        message,
        backtrace,
    };

    let _ = write_panic_dump(&dump);
}

/// Register the custom panic hook while chaining previously installed hooks
pub fn install_shm_panic_hook() {
    INIT_HOOK.call_once(|| {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            record_panic_hook_info(info);
            prev_hook(info);
        }));
        HOOK_INSTALLED.store(true, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_shm_panic_dumper_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap();
        let _ = clear_panic_dump();

        let dump = ShmPanicDump {
            magic: SHM_PANIC_MAGIC,
            version: SHM_PANIC_VERSION,
            timestamp_ms: 1729940000000,
            timestamp_rfc3339: "2026-10-26T12:00:00Z".to_string(),
            pid: 1337,
            thread_name: "hgb-worker-42".to_string(),
            location_file: "crates/hgb-core/src/actor_swarm.rs".to_string(),
            location_line: 184,
            location_col: 12,
            message: "Explicit panic: memory-mapped crash dump invariant verified".to_string(),
            backtrace: "   0: std::backtrace::Backtrace::capture\n   1: hgb_core::actor_swarm::execute".to_string(),
        };

        let path = write_panic_dump(&dump).expect("write panic dump");
        assert!(path.exists());

        let read_back = read_last_panic_dump().expect("read panic dump");
        assert_eq!(read_back, dump);

        clear_panic_dump().expect("clear panic dump");
        assert!(read_last_panic_dump().is_none());
    }

    #[test]
    fn test_install_shm_panic_hook_with_catch_unwind() {
        let _guard = TEST_LOCK.lock().unwrap();
        let _ = clear_panic_dump();
        install_shm_panic_hook();

        let result = std::panic::catch_unwind(|| {
            panic!("Test panic captured by Hagibis SHM panic dumper");
        });
        assert!(result.is_err());

        let dump = read_last_panic_dump().expect("panic dump must exist");
        assert_eq!(dump.magic, SHM_PANIC_MAGIC);
        assert_eq!(dump.version, SHM_PANIC_VERSION);
        assert_eq!(dump.pid, std::process::id());
        assert!(dump.message.contains("Test panic captured by Hagibis SHM panic dumper"));

        clear_panic_dump().expect("cleanup");
    }
}
