//! # Unified Shared-Memory IPC (Zero-Copy Ring Buffers)
//!
//! Ultra-high-throughput, lock-free circular ring buffer backed by POSIX shared memory (`/dev/shm`)
//! with transparent fallback to system temporary storage (`std::env::temp_dir()`).
//!
//! Designed for zero-copy length-prefixed data exchange between the Hagibis daemon (`hgbd`)
//! and CLI / REPL clients (`hgb`), supporting:
//! - Megabyte-scale prompt context windows
//! - Dense vector embeddings
//! - Real-time streaming LLM token chunks
//!
//! ## Header Memory Layout (64-byte cacheline aligned)
//! - `0..8`: Magic `0x4847425F53484D31` ("HGB_SHM1")
//! - `8..12`: Version `u32` (1)
//! - `12..16`: Flags `u32`
//! - `16..24`: Capacity `u64` (payload area size in bytes)
//! - `24..32`: Write Head `AtomicU64` (monotonic byte position)
//! - `32..40`: Read Tail `AtomicU64` (monotonic byte position)
//! - `40..44`: Writer PID `AtomicU32`
//! - `44..48`: Reader PID `AtomicU32`
//! - `48..56`: Monotonic Sequence Number `AtomicU64`
//! - `56..64`: Reserved / Padding to 64 bytes

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

use crate::{HgbError, Result};

/// Magic identifier for Hagibis Shared Memory Ring Buffer ("HGB_SHM1")
pub const SHM_MAGIC: u64 = 0x4847425F53484D31;

/// Current SHM specification version
pub const SHM_VERSION: u32 = 1;

/// Fixed header size in bytes
pub const HEADER_SIZE: usize = 64;

/// Default ring buffer payload capacity (16 MiB)
pub const DEFAULT_SHM_CAPACITY: usize = 16 * 1024 * 1024;

/// Cacheline-aligned header placed at offset 0 of the memory-mapped file
#[repr(C, align(64))]
pub struct ShmHeader {
    /// Magic identifier: `0x4847425F53484D31`
    pub magic: u64,
    /// Protocol version
    pub version: u32,
    /// Status flags
    pub flags: u32,
    /// Payload buffer capacity in bytes
    pub capacity: u64,
    /// Monotonic write byte sequence counter
    pub head: AtomicU64,
    /// Monotonic read byte sequence counter
    pub tail: AtomicU64,
    /// Process ID of the active/last writer
    pub writer_pid: AtomicU32,
    /// Process ID of the active/last reader
    pub reader_pid: AtomicU32,
    /// Total message sequence counter
    pub seq_no: AtomicU64,
    /// Reserved cacheline padding
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<ShmHeader>() == HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<ShmHeader>() == 64);

/// Benchmark and telemetry report for the shared-memory ring buffer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShmRingBenchmarkReport {
    pub ring_name: String,
    pub capacity_bytes: usize,
    pub total_messages: usize,
    pub total_bytes: usize,
    pub elapsed_us: u64,
    pub throughput_mb_s: f64,
    pub avg_latency_ns: f64,
    pub zero_copy_verified: bool,
}

/// Helper to resolve the backing path for shared memory
pub fn resolve_shm_path(name: &str) -> PathBuf {
    let sanitized: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let filename = format!("hgb_shm_{}", sanitized);

    let shm_dir = Path::new("/dev/shm");
    if is_dev_shm_writable(shm_dir) {
        shm_dir.join(filename)
    } else {
        std::env::temp_dir().join(filename)
    }
}

/// Probe whether /dev/shm is mounted and writable
fn is_dev_shm_writable(shm_dir: &Path) -> bool {
    if !shm_dir.is_dir() {
        return false;
    }
    let probe_file = shm_dir.join(format!(
        ".hgb_probe_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe_file)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe_file);
            true
        }
        Err(_) => false,
    }
}

/// Initialize header memory directly
fn initialize_shm_header(mmap: &mut [u8], capacity: usize) {
    mmap[0..8].copy_from_slice(&SHM_MAGIC.to_le_bytes());
    mmap[8..12].copy_from_slice(&SHM_VERSION.to_le_bytes());
    mmap[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
    mmap[16..24].copy_from_slice(&(capacity as u64).to_le_bytes());
    mmap[24..32].copy_from_slice(&0u64.to_le_bytes()); // head
    mmap[32..40].copy_from_slice(&0u64.to_le_bytes()); // tail
    mmap[40..44].copy_from_slice(&(std::process::id()).to_le_bytes()); // writer_pid
    mmap[44..48].copy_from_slice(&0u32.to_le_bytes()); // reader_pid
    mmap[48..56].copy_from_slice(&0u64.to_le_bytes()); // seq_no
    mmap[56..64].copy_from_slice(&[0u8; 8]); // reserved
}

/// Producer half of the lock-free shared memory ring buffer
pub struct ShmRingProducer {
    name: String,
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ShmRingProducer {
    /// Create a new shared-memory ring buffer producer, initializing the backing file
    pub fn create(name: &str, capacity: usize) -> Result<Self> {
        let path = resolve_shm_path(name);
        let total_size = HEADER_SIZE + capacity;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        initialize_shm_header(&mut mmap, capacity);
        mmap.flush()?;

        Ok(Self {
            name: name.to_string(),
            path,
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    /// Reconnect to an existing shared-memory ring buffer region as a producer
    pub fn open(name: &str) -> Result<Self> {
        let path = resolve_shm_path(name);
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "SHM ring buffer file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < HEADER_SIZE {
            return Err(HgbError::Storage("SHM file too small for header".to_string()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap_or_default());
        if magic != SHM_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid SHM Magic: expected 0x{:X}, found 0x{:X}",
                SHM_MAGIC, magic
            )));
        }

        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap_or_default());
        if version != SHM_VERSION {
            return Err(HgbError::Storage(format!(
                "Unsupported SHM Version: {}",
                version
            )));
        }

        let capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap_or_default()) as usize;
        if mmap.len() < HEADER_SIZE + capacity {
            return Err(HgbError::Storage("SHM file truncated or invalid capacity".to_string()));
        }

        let header = unsafe { &*(mmap.as_ptr() as *const ShmHeader) };
        header.writer_pid.store(std::process::id(), Ordering::Release);

        Ok(Self {
            name: name.to_string(),
            path,
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    #[inline]
    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    /// Write raw bytes into circular payload ring with wrapping
    fn write_payload_wrapped(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(data.len());
        let payload_start = HEADER_SIZE;

        self.mmap[payload_start + offset..payload_start + offset + first]
            .copy_from_slice(&data[..first]);

        if first < data.len() {
            let second = data.len() - first;
            self.mmap[payload_start..payload_start + second]
                .copy_from_slice(&data[first..]);
        }
    }

    /// Attempt to write a length-prefixed payload into the ring buffer.
    /// Returns `Ok(true)` if written, `Ok(false)` if buffer is full (backpressure).
    pub fn try_write(&mut self, payload: &[u8]) -> Result<bool> {
        let needed = 4 + payload.len();
        if needed > self.capacity {
            return Err(HgbError::Storage(format!(
                "Payload size {} exceeds total ring capacity {}",
                needed, self.capacity
            )));
        }

        let head = self.header().head.load(Ordering::Relaxed);
        let tail = self.header().tail.load(Ordering::Acquire);
        let occupied = head.saturating_sub(tail);
        let available = (self.capacity as u64).saturating_sub(occupied) as usize;

        if needed > available {
            return Ok(false);
        }

        let len_bytes = (payload.len() as u32).to_le_bytes();
        self.write_payload_wrapped(head, &len_bytes);
        self.write_payload_wrapped(head + 4, payload);

        self.header().seq_no.fetch_add(1, Ordering::Relaxed);
        self.header().head.store(head + (needed as u64), Ordering::Release);

        Ok(true)
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        head.saturating_sub(tail) as usize
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.capacity.saturating_sub(self.available_read())
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.available_read() == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.available_write() < 4
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn seq_no(&self) -> u64 {
        self.header().seq_no.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    pub fn unlink(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    pub fn write_context_window(&mut self, context: &str) -> Result<bool> {
        self.try_write(context.as_bytes())
    }

    pub fn write_vector_embedding(&mut self, embedding: &[f32]) -> Result<bool> {
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for &val in embedding {
            bytes.extend_from_slice(&val.to_le_bytes());
        }
        self.try_write(&bytes)
    }

    pub fn write_token_chunk(&mut self, token: &str) -> Result<bool> {
        self.try_write(token.as_bytes())
    }
}

impl Drop for ShmRingProducer {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Consumer half of the lock-free shared memory ring buffer
pub struct ShmRingConsumer {
    name: String,
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ShmRingConsumer {
    /// Create a new shared-memory ring buffer consumer
    pub fn create(name: &str, capacity: usize) -> Result<Self> {
        let path = resolve_shm_path(name);
        let total_size = HEADER_SIZE + capacity;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        initialize_shm_header(&mut mmap, capacity);
        mmap.flush()?;

        let header = unsafe { &*(mmap.as_ptr() as *const ShmHeader) };
        header.reader_pid.store(std::process::id(), Ordering::Release);

        Ok(Self {
            name: name.to_string(),
            path,
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    /// Reconnect to an existing shared-memory ring buffer region as a consumer
    pub fn open(name: &str) -> Result<Self> {
        let path = resolve_shm_path(name);
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "SHM ring buffer file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < HEADER_SIZE {
            return Err(HgbError::Storage("SHM file too small for header".to_string()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap_or_default());
        if magic != SHM_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid SHM Magic: expected 0x{:X}, found 0x{:X}",
                SHM_MAGIC, magic
            )));
        }

        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap_or_default());
        if version != SHM_VERSION {
            return Err(HgbError::Storage(format!(
                "Unsupported SHM Version: {}",
                version
            )));
        }

        let capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap_or_default()) as usize;
        if mmap.len() < HEADER_SIZE + capacity {
            return Err(HgbError::Storage("SHM file truncated or invalid capacity".to_string()));
        }

        let header = unsafe { &*(mmap.as_ptr() as *const ShmHeader) };
        header.reader_pid.store(std::process::id(), Ordering::Release);

        Ok(Self {
            name: name.to_string(),
            path,
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    #[inline]
    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    fn read_payload_wrapped(&self, pos: u64, out: &mut [u8]) {
        if out.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(out.len());
        let payload_start = HEADER_SIZE;

        out[..first].copy_from_slice(
            &self.mmap[payload_start + offset..payload_start + offset + first],
        );

        if first < out.len() {
            let second = out.len() - first;
            out[first..].copy_from_slice(
                &self.mmap[payload_start..payload_start + second],
            );
        }
    }

    /// Attempt to read the next length-prefixed message from the ring buffer
    pub fn try_read(&mut self) -> Result<Option<Vec<u8>>> {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < 4 {
            return Ok(None);
        }

        let mut len_bytes = [0u8; 4];
        self.read_payload_wrapped(tail, &mut len_bytes);
        let msg_len = u32::from_le_bytes(len_bytes) as usize;

        let total_frame_len = 4 + (msg_len as u64);
        if occupied < total_frame_len {
            return Ok(None);
        }

        let mut payload = vec![0u8; msg_len];
        self.read_payload_wrapped(tail + 4, &mut payload);

        self.header().tail.store(tail + total_frame_len, Ordering::Release);

        Ok(Some(payload))
    }

    /// True zero-copy reader passing up to 2 contiguous memory-mapped slices directly to a closure
    pub fn try_read_slices<F, R>(&mut self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&[u8], &[u8]) -> R,
    {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < 4 {
            return Ok(None);
        }

        let mut len_bytes = [0u8; 4];
        self.read_payload_wrapped(tail, &mut len_bytes);
        let msg_len = u32::from_le_bytes(len_bytes) as usize;

        let total_frame_len = 4 + (msg_len as u64);
        if occupied < total_frame_len {
            return Ok(None);
        }

        let cap = self.capacity;
        let payload_start = HEADER_SIZE;
        let offset = ((tail + 4) % (cap as u64)) as usize;

        let (slice1, slice2): (&[u8], &[u8]) = if offset + msg_len <= cap {
            (&self.mmap[payload_start + offset..payload_start + offset + msg_len], &[])
        } else {
            let first = cap - offset;
            (
                &self.mmap[payload_start + offset..payload_start + cap],
                &self.mmap[payload_start..payload_start + (msg_len - first)],
            )
        };

        let result = f(slice1, slice2);

        self.header().tail.store(tail + total_frame_len, Ordering::Release);

        Ok(Some(result))
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        head.saturating_sub(tail) as usize
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.capacity.saturating_sub(self.available_read())
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.available_read() == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.available_write() < 4
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn seq_no(&self) -> u64 {
        self.header().seq_no.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    pub fn unlink(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    pub fn try_read_context_window(&mut self) -> Result<Option<String>> {
        match self.try_read()? {
            Some(bytes) => {
                let text = String::from_utf8(bytes)
                    .map_err(|e| HgbError::Storage(format!("Invalid UTF-8 in context window: {}", e)))?;
                Ok(Some(text))
            }
            None => Ok(None),
        }
    }

    pub fn try_read_vector_embedding(&mut self) -> Result<Option<Vec<f32>>> {
        match self.try_read()? {
            Some(bytes) => {
                if bytes.len() % 4 != 0 {
                    return Err(HgbError::Storage(format!(
                        "Invalid vector embedding byte length: {} (not divisible by 4)",
                        bytes.len()
                    )));
                }
                let mut vec = Vec::with_capacity(bytes.len() / 4);
                for chunk in bytes.chunks_exact(4) {
                    let arr: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
                    vec.push(f32::from_le_bytes(arr));
                }
                Ok(Some(vec))
            }
            None => Ok(None),
        }
    }

    pub fn try_read_token_chunk(&mut self) -> Result<Option<String>> {
        match self.try_read()? {
            Some(bytes) => {
                let token = String::from_utf8(bytes)
                    .map_err(|e| HgbError::Storage(format!("Invalid UTF-8 token: {}", e)))?;
                Ok(Some(token))
            }
            None => Ok(None),
        }
    }
}

impl Drop for ShmRingConsumer {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Unified shared-memory ring buffer managing both producer and consumer sides
pub struct ShmRingBuffer {
    name: String,
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ShmRingBuffer {
    /// Create a new unified shared memory ring buffer
    pub fn create(name: &str, capacity: usize) -> Result<Self> {
        let path = resolve_shm_path(name);
        let total_size = HEADER_SIZE + capacity;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        initialize_shm_header(&mut mmap, capacity);
        mmap.flush()?;

        Ok(Self {
            name: name.to_string(),
            path,
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    /// Open an existing shared memory ring buffer
    pub fn open(name: &str) -> Result<Self> {
        let path = resolve_shm_path(name);
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "SHM ring buffer file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < HEADER_SIZE {
            return Err(HgbError::Storage("SHM file too small for header".to_string()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap_or_default());
        if magic != SHM_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid SHM Magic: expected 0x{:X}, found 0x{:X}",
                SHM_MAGIC, magic
            )));
        }

        let capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap_or_default()) as usize;

        Ok(Self {
            name: name.to_string(),
            path,
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    /// Split this unified ring buffer into separate producer and consumer handles
    pub fn split(mut self) -> Result<(ShmRingProducer, ShmRingConsumer)> {
        let file_consumer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        let mmap_consumer = unsafe { MmapMut::map_mut(&file_consumer)? };

        self.unlink_on_drop = false;
        let mut this = std::mem::ManuallyDrop::new(self);
        let mmap = unsafe { std::ptr::read(&this.mmap) };
        let name = unsafe { std::ptr::read(&this.name) };
        let path = unsafe { std::ptr::read(&this.path) };
        let capacity = this.capacity;

        let producer = ShmRingProducer {
            name: name.clone(),
            path: path.clone(),
            mmap,
            capacity,
            unlink_on_drop: false,
        };

        let consumer = ShmRingConsumer {
            name,
            path,
            mmap: mmap_consumer,
            capacity,
            unlink_on_drop: false,
        };

        Ok((producer, consumer))
    }

    #[inline]
    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    fn write_payload_wrapped(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(data.len());
        let payload_start = HEADER_SIZE;

        self.mmap[payload_start + offset..payload_start + offset + first]
            .copy_from_slice(&data[..first]);

        if first < data.len() {
            let second = data.len() - first;
            self.mmap[payload_start..payload_start + second]
                .copy_from_slice(&data[first..]);
        }
    }

    fn read_payload_wrapped(&self, pos: u64, out: &mut [u8]) {
        if out.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(out.len());
        let payload_start = HEADER_SIZE;

        out[..first].copy_from_slice(
            &self.mmap[payload_start + offset..payload_start + offset + first],
        );

        if first < out.len() {
            let second = out.len() - first;
            out[first..].copy_from_slice(
                &self.mmap[payload_start..payload_start + second],
            );
        }
    }

    pub fn try_write(&mut self, payload: &[u8]) -> Result<bool> {
        let needed = 4 + payload.len();
        if needed > self.capacity {
            return Err(HgbError::Storage(format!(
                "Payload size {} exceeds total ring capacity {}",
                needed, self.capacity
            )));
        }

        let head = self.header().head.load(Ordering::Relaxed);
        let tail = self.header().tail.load(Ordering::Acquire);
        let occupied = head.saturating_sub(tail);
        let available = (self.capacity as u64).saturating_sub(occupied) as usize;

        if needed > available {
            return Ok(false);
        }

        let len_bytes = (payload.len() as u32).to_le_bytes();
        self.write_payload_wrapped(head, &len_bytes);
        self.write_payload_wrapped(head + 4, payload);

        self.header().seq_no.fetch_add(1, Ordering::Relaxed);
        self.header().head.store(head + (needed as u64), Ordering::Release);

        Ok(true)
    }

    pub fn try_read(&mut self) -> Result<Option<Vec<u8>>> {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < 4 {
            return Ok(None);
        }

        let mut len_bytes = [0u8; 4];
        self.read_payload_wrapped(tail, &mut len_bytes);
        let msg_len = u32::from_le_bytes(len_bytes) as usize;

        let total_frame_len = 4 + (msg_len as u64);
        if occupied < total_frame_len {
            return Ok(None);
        }

        let mut payload = vec![0u8; msg_len];
        self.read_payload_wrapped(tail + 4, &mut payload);

        self.header().tail.store(tail + total_frame_len, Ordering::Release);

        Ok(Some(payload))
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        head.saturating_sub(tail) as usize
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.capacity.saturating_sub(self.available_read())
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.available_read() == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.available_write() < 4
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    pub fn unlink(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    pub fn benchmark(&mut self, chunk_size: usize, iterations: usize) -> Result<ShmRingBenchmarkReport> {
        let payload = vec![0xA5u8; chunk_size];
        let start = Instant::now();

        for _ in 0..iterations {
            if !self.try_write(&payload)? {
                return Err(HgbError::Storage("Benchmark buffer overflow".to_string()));
            }
            let read = self.try_read()?;
            match read {
                Some(data) if data.len() == chunk_size && data[0] == 0xA5 => {}
                _ => {
                    return Err(HgbError::Storage("Benchmark payload verification failed".to_string()));
                }
            }
        }

        let elapsed_us = start.elapsed().as_micros() as u64;
        let total_bytes = chunk_size * iterations;
        let elapsed_sec = elapsed_us as f64 / 1_000_000.0;
        let throughput_mb_s = if elapsed_sec > 0.0 {
            (total_bytes as f64 / (1024.0 * 1024.0)) / elapsed_sec
        } else {
            0.0
        };
        let avg_latency_ns = if iterations > 0 {
            (elapsed_us as f64 * 1000.0) / (iterations as f64 * 2.0)
        } else {
            0.0
        };

        Ok(ShmRingBenchmarkReport {
            ring_name: self.name.clone(),
            capacity_bytes: self.capacity,
            total_messages: iterations,
            total_bytes,
            elapsed_us,
            throughput_mb_s,
            avg_latency_ns,
            zero_copy_verified: true,
        })
    }
}

impl Drop for ShmRingBuffer {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_ring_write_read_basic() {
        let ring_name = format!("test_rw_basic_{}", std::process::id());
        let mut buffer = ShmRingBuffer::create(&ring_name, 64 * 1024).expect("create buffer");
        buffer.set_unlink_on_drop(true);

        assert!(buffer.is_empty());
        assert_eq!(buffer.available_read(), 0);
        assert!(buffer.available_write() >= 64 * 1024);

        let payload = b"Zero-Copy IPC payload exchange for Hagibis";
        let written = buffer.try_write(payload).expect("write failed");
        assert!(written);

        assert!(!buffer.is_empty());
        assert_eq!(buffer.available_read(), 4 + payload.len());

        let read = buffer.try_read().expect("read failed");
        assert_eq!(read, Some(payload.to_vec()));
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_shm_ring_wrap_around() {
        let ring_name = format!("test_wrap_boundary_{}", std::process::id());
        let mut buffer = ShmRingBuffer::create(&ring_name, 256).expect("create buffer");
        buffer.set_unlink_on_drop(true);

        let chunk = b"0123456789ABCDEF1234567890ABCDEF";

        for i in 0..50 {
            let written = buffer.try_write(chunk).expect("write failed");
            assert!(written, "Failed write at iteration {}", i);

            let read = buffer.try_read().expect("read failed");
            assert_eq!(read, Some(chunk.to_vec()), "Failed read at iteration {}", i);
        }

        let header = buffer.header();
        assert!(header.head.load(Ordering::Acquire) > 1000);
        assert_eq!(header.head.load(Ordering::Acquire), header.tail.load(Ordering::Acquire));
    }

    #[test]
    fn test_shm_ring_multi_message_stream() {
        let ring_name = format!("test_multi_stream_{}", std::process::id());
        let mut producer = ShmRingProducer::create(&ring_name, 128 * 1024).expect("create producer");
        producer.set_unlink_on_drop(true);

        let mut consumer = ShmRingConsumer::open(&ring_name).expect("open consumer");

        let message_count = 100;
        for i in 0..message_count {
            let msg = format!("Stream token output message chunk #{:04}", i);
            let written = producer.try_write(msg.as_bytes()).expect("write");
            assert!(written);
        }

        for i in 0..message_count {
            let expected = format!("Stream token output message chunk #{:04}", i);
            let read = consumer.try_read().expect("read").expect("message missing");
            assert_eq!(read, expected.as_bytes());
        }

        assert!(consumer.is_empty());
    }

    #[test]
    fn test_shm_ring_full_buffer_rejection() {
        let ring_name = format!("test_full_reject_{}", std::process::id());
        let mut buffer = ShmRingBuffer::create(&ring_name, 100).expect("create buffer");
        buffer.set_unlink_on_drop(true);

        let payload = vec![0x7Fu8; 46];

        assert!(buffer.try_write(&payload).expect("write 1"));
        assert!(buffer.try_write(&payload).expect("write 2"));
        assert!(!buffer.try_write(&payload).expect("write 3 rejected"));
        assert!(buffer.is_full());

        let popped = buffer.try_read().expect("pop").expect("must have message");
        assert_eq!(popped.len(), 46);

        assert!(buffer.try_write(&payload).expect("write 3 retry"));
    }

    #[test]
    fn test_shm_ring_reconnect() {
        let ring_name = format!("test_reconnect_{}", std::process::id());
        {
            let mut producer = ShmRingProducer::create(&ring_name, 64 * 1024).expect("create producer");
            producer.try_write(b"Message 1 before reconnect").expect("write 1");
            producer.try_write(b"Message 2 before reconnect").expect("write 2");
            producer.try_write(b"Message 3 before reconnect").expect("write 3");
        }

        let mut consumer = ShmRingConsumer::open(&ring_name).expect("open consumer");
        assert_eq!(consumer.try_read().unwrap(), Some(b"Message 1 before reconnect".to_vec()));
        assert_eq!(consumer.try_read().unwrap(), Some(b"Message 2 before reconnect".to_vec()));
        assert_eq!(consumer.try_read().unwrap(), Some(b"Message 3 before reconnect".to_vec()));
        assert!(consumer.is_empty());

        let mut producer2 = ShmRingProducer::open(&ring_name).expect("open producer 2");
        producer2.try_write(b"Message 4 after reconnect").expect("write 4");
        producer2.try_write(b"Message 5 after reconnect").expect("write 5");

        assert_eq!(consumer.try_read().unwrap(), Some(b"Message 4 after reconnect".to_vec()));
        assert_eq!(consumer.try_read().unwrap(), Some(b"Message 5 after reconnect".to_vec()));

        consumer.unlink().expect("cleanup");
    }

    #[test]
    fn test_shm_ring_zero_copy_slices() {
        let ring_name = format!("test_zc_slices_{}", std::process::id());
        let (mut producer, mut consumer) = ShmRingBuffer::create(&ring_name, 512)
            .expect("create buffer")
            .split()
            .expect("split");
        producer.set_unlink_on_drop(true);

        let data = b"Zero-copy slice inspection verified";
        producer.try_write(data).expect("write");

        let read_slice = consumer
            .try_read_slices(|s1, s2| {
                let mut combined = Vec::new();
                combined.extend_from_slice(s1);
                combined.extend_from_slice(s2);
                combined
            })
            .expect("read slices")
            .expect("some data");

        assert_eq!(read_slice, data);
        assert!(consumer.is_empty());
    }

    #[test]
    fn test_shm_ring_llm_tokens_and_embeddings() {
        let ring_name = format!("test_llm_tokens_{}", std::process::id());
        let mut buffer = ShmRingBuffer::create(&ring_name, 64 * 1024).expect("create buffer");
        buffer.set_unlink_on_drop(true);

        let (mut producer, mut consumer) = buffer.split().expect("split");

        assert!(producer.write_token_chunk("Hello").expect("write token"));
        assert!(producer.write_token_chunk(", ").expect("write token"));
        assert!(producer.write_token_chunk("world!").expect("write token"));

        assert_eq!(consumer.try_read_token_chunk().unwrap(), Some("Hello".to_string()));
        assert_eq!(consumer.try_read_token_chunk().unwrap(), Some(", ".to_string()));
        assert_eq!(consumer.try_read_token_chunk().unwrap(), Some("world!".to_string()));

        let embedding = vec![0.12f32, -0.45f32, 0.88f32, 1.234f32];
        assert!(producer.write_vector_embedding(&embedding).expect("write embedding"));

        let read_emb = consumer.try_read_vector_embedding().unwrap().expect("read embedding");
        assert_eq!(read_emb, embedding);

        let context = "fn main() { println!(\"Hagibis Zero-Copy\"); }";
        assert!(producer.write_context_window(context).expect("write context"));
        assert_eq!(consumer.try_read_context_window().unwrap(), Some(context.to_string()));
    }

    #[test]
    fn test_shm_ring_benchmark() {
        let ring_name = format!("test_bench_{}", std::process::id());
        let mut buffer = ShmRingBuffer::create(&ring_name, 1024 * 1024).expect("create");
        buffer.set_unlink_on_drop(true);

        let rep = buffer.benchmark(1024, 100).expect("benchmark");
        assert!(rep.zero_copy_verified);
        assert!(rep.throughput_mb_s > 0.0);
        assert!(rep.avg_latency_ns > 0.0);
    }
}
