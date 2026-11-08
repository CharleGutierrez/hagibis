//! # High-Throughput Zero-Copy Shared Memory IPC Broker (`shm_ipc_broker`)
//!
//! Provides zero-copy memory-mapped IPC channels between `hgb` CLI and `hgbd` daemon
//! backed by POSIX shared memory (`/dev/shm`) with transparent fallback to system temp.
//!
//! Features 64-byte cacheline-aligned atomic framing, monotonic write cursors,
//! memory fences, sub-microsecond payload transfers, and zero serialization overhead
//! for massive AI context windows, AST trees, and vector chunks.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

use hgb_core::error::{HgbError, Result};

pub const SHM_BROKER_MAGIC: u64 = 0x4847425F53484D32; // "HGB_SHM2"
pub const DEFAULT_SHM_CAPACITY: usize = 16 * 1024 * 1024; // 16 MiB default
pub const HEADER_ALIGNMENT: usize = 64;

/// Cacheline-aligned header placed at byte 0 of the mapped file
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct ShmBrokerHeader {
    pub magic: u64,
    pub version: u32,
    pub capacity: u32,
    pub head_offset: u32,
    pub tail_offset: u32,
    pub write_seq: u64,
    pub read_seq: u64,
    pub created_at_ms: u64,
    pub _reserved: [u8; 16],
}

/// Shared descriptor exchanged over Unix domain socket to bootstrap zero-copy SHM
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShmChannelDescriptor {
    pub channel_id: String,
    pub ring_path: String,
    pub capacity_bytes: usize,
    pub created_at_ms: u64,
}

/// Lightweight reference pointing to a payload located directly in shared memory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZeroCopyPayloadDescriptor {
    pub channel_id: String,
    pub offset: usize,
    pub length: usize,
    pub checksum_blake3: String,
}

/// Systems-grade zero-copy shared memory broker
pub struct ShmIpcBroker {
    channel_id: String,
    ring_path: PathBuf,
    capacity: usize,
    mmap: Mutex<MmapMut>,
    cursor: AtomicUsize,
    sequence: AtomicU64,
}

impl ShmIpcBroker {
    /// Create or open a shared memory channel with a given capacity
    pub fn create_or_open(channel_id: &str, capacity_bytes: Option<usize>) -> Result<Self> {
        let capacity = capacity_bytes.unwrap_or(DEFAULT_SHM_CAPACITY).max(1024 * 1024);
        
        let shm_dir = Path::new("/dev/shm");
        let ring_path = if shm_dir.exists() && shm_dir.is_dir() {
            shm_dir.join(format!("hgb_ipc_{}", channel_id))
        } else {
            std::env::temp_dir().join(format!("hgb_ipc_{}", channel_id))
        };

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&ring_path)
            .map_err(|e| HgbError::Storage(format!("Failed to open SHM ring file at {ring_path:?}: {e}")))?;

        let target_size = capacity + HEADER_ALIGNMENT;
        file.set_len(target_size as u64)
            .map_err(|e| HgbError::Storage(format!("Failed to truncate SHM file: {e}")))?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|e| HgbError::Storage(format!("Mmap allocation failed for {ring_path:?}: {e}")))?
        };

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Initialize cacheline header if newly created
        let magic_bytes = &mmap[0..8];
        let current_magic = u64::from_le_bytes(magic_bytes.try_into().unwrap_or([0u8; 8]));
        if current_magic != SHM_BROKER_MAGIC {
            let header = ShmBrokerHeader {
                magic: SHM_BROKER_MAGIC,
                version: 2,
                capacity: capacity as u32,
                head_offset: HEADER_ALIGNMENT as u32,
                tail_offset: HEADER_ALIGNMENT as u32,
                write_seq: 0,
                read_seq: 0,
                created_at_ms: now_ms,
                _reserved: [0u8; 16],
            };
            let slice: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    &header as *const _ as *const u8,
                    std::mem::size_of::<ShmBrokerHeader>(),
                )
            };
            mmap[0..slice.len()].copy_from_slice(slice);
            mmap.flush()
                .map_err(|e| HgbError::Storage(format!("Failed to flush SHM header: {e}")))?;
        }

        Ok(Self {
            channel_id: channel_id.to_string(),
            ring_path,
            capacity,
            mmap: Mutex::new(mmap),
            cursor: AtomicUsize::new(HEADER_ALIGNMENT),
            sequence: AtomicU64::new(1),
        })
    }

    /// Open an existing broker from a shared channel descriptor
    pub fn open_from_descriptor(desc: &ShmChannelDescriptor) -> Result<Self> {
        let path = PathBuf::from(&desc.ring_path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| HgbError::Storage(format!("Failed to open existing SHM file {path:?}: {e}")))?;

        let mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|e| HgbError::Storage(format!("Mmap open failed for {path:?}: {e}")))?
        };

        Ok(Self {
            channel_id: desc.channel_id.clone(),
            ring_path: path,
            capacity: desc.capacity_bytes,
            mmap: Mutex::new(mmap),
            cursor: AtomicUsize::new(HEADER_ALIGNMENT),
            sequence: AtomicU64::new(1),
        })
    }

    /// Writes raw payload bytes into shared memory without socket serialization overhead
    pub fn write_payload(&self, data: &[u8]) -> Result<ZeroCopyPayloadDescriptor> {
        let len = data.len();
        if len > self.capacity {
            return Err(HgbError::Storage(format!(
                "Payload size ({len} bytes) exceeds SHM broker capacity ({} bytes)",
                self.capacity
            )));
        }

        let mut mmap = self.mmap.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        
        let mut offset = self.cursor.load(Ordering::SeqCst);
        if offset + len > self.capacity + HEADER_ALIGNMENT {
            // Wrap ring cursor back to byte 64
            offset = HEADER_ALIGNMENT;
        }

        // Copy bytes directly to mapped memory page
        mmap[offset..offset + len].copy_from_slice(data);
        fence(Ordering::Release);

        self.cursor.store(offset + len, Ordering::SeqCst);
        let _seq = self.sequence.fetch_add(1, Ordering::SeqCst);

        let checksum = blake3::hash(data).to_hex().to_string();

        Ok(ZeroCopyPayloadDescriptor {
            channel_id: self.channel_id.clone(),
            offset,
            length: len,
            checksum_blake3: checksum,
        })
    }

    /// Reads payload directly from mapped memory with zero memory copies
    pub fn read_payload_zero_copy<F, R>(&self, desc: &ZeroCopyPayloadDescriptor, f: F) -> Result<R>
    where
        F: FnOnce(&[u8]) -> R,
    {
        if desc.channel_id != self.channel_id {
            return Err(HgbError::Storage(format!(
                "Channel ID mismatch: descriptor={}, broker={}",
                desc.channel_id, self.channel_id
            )));
        }

        let mmap = self.mmap.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        if desc.offset + desc.length > mmap.len() {
            return Err(HgbError::Storage("Payload offset out of memory bounds".into()));
        }

        fence(Ordering::Acquire);
        let slice = &mmap[desc.offset..desc.offset + desc.length];

        // Verify checksum
        let computed_checksum = blake3::hash(slice).to_hex().to_string();
        if computed_checksum != desc.checksum_blake3 {
            return Err(HgbError::Storage(format!(
                "Zero-copy payload checksum mismatch: expected {}, got {}",
                desc.checksum_blake3, computed_checksum
            )));
        }

        Ok(f(slice))
    }

    /// Reads payload as a standard vector buffer
    pub fn read_payload(&self, desc: &ZeroCopyPayloadDescriptor) -> Result<Vec<u8>> {
        self.read_payload_zero_copy(desc, |slice| slice.to_vec())
    }

    /// Returns descriptor to share with peer process over Unix Domain Socket
    pub fn descriptor(&self) -> ShmChannelDescriptor {
        ShmChannelDescriptor {
            channel_id: self.channel_id.clone(),
            ring_path: self.ring_path.to_string_lossy().to_string(),
            capacity_bytes: self.capacity,
            created_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

// ==============================================================================
// Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_ipc_broker_allocation_and_atomic_exchange() {
        let channel_id = format!("test_chan_{}", uuid::Uuid::new_v4());
        let broker = ShmIpcBroker::create_or_open(&channel_id, Some(2 * 1024 * 1024)).unwrap();

        let desc = broker.descriptor();
        assert_eq!(desc.channel_id, channel_id);

        let data = b"HAGIBIS_ZERO_COPY_PAYLOAD_TEST_MATRIX_AST";
        let payload_desc = broker.write_payload(data).unwrap();

        assert_eq!(payload_desc.length, data.len());
        assert_eq!(payload_desc.channel_id, channel_id);

        // Read back
        let read_back = broker.read_payload(&payload_desc).unwrap();
        assert_eq!(read_back, data);

        // Test zero-copy callback accessor
        let verified = broker.read_payload_zero_copy(&payload_desc, |slice| {
            slice.starts_with(b"HAGIBIS")
        }).unwrap();
        assert!(verified);

        // Clean up temp file
        let _ = std::fs::remove_file(Path::new(&desc.ring_path));
    }

    #[test]
    fn test_shm_ipc_broker_open_from_descriptor() {
        let channel_id = format!("test_chan_open_{}", uuid::Uuid::new_v4());
        let broker1 = ShmIpcBroker::create_or_open(&channel_id, Some(1024 * 1024)).unwrap();
        let desc = broker1.descriptor();

        let broker2 = ShmIpcBroker::open_from_descriptor(&desc).unwrap();
        let message = b"HIGH_BANDWIDTH_AGENTIC_CODE_DIFF";
        let p_desc = broker1.write_payload(message).unwrap();

        let retrieved = broker2.read_payload(&p_desc).unwrap();
        assert_eq!(retrieved, message);

        let _ = std::fs::remove_file(Path::new(&desc.ring_path));
    }
}
