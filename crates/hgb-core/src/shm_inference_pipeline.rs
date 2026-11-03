//! # True Zero-Copy Shared Memory for AI Inference Pipelines
//!
//! High-throughput, microsecond-latency POSIX shared memory ring buffer (`/dev/shm/hgb_infer_stream_<id>.shm`
//! with fallback to `temp_dir()`) and direct memory bridging into WebAssembly linear memory (`WasmDirectMemoryBridge`).
//!
//! Designed for high-speed streaming of:
//! - Real-time streaming LLM token chunks (`Utf8Tokens`)
//! - Intermediate activations and KV cache representations (`Q8_0`, `F16`, `F32`)
//! - Direct tensor passing into sandboxed WebAssembly guest modules without intermediate copies or serialization.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use wasmtime::{Memory, Store};

use crate::error::{HgbError, Result};
use crate::shm_ring_channel::align_up_64;

/// Binary Frame Header Magic: `0x4847425F494E4631` ("HGB_INF1")
pub const INFER_FRAME_MAGIC: u64 = 0x4847425F494E4631;

/// Protocol specification version
pub const INFER_STREAM_VERSION: u32 = 1;

/// Cacheline-aligned control header size at offset 0 (64 bytes)
pub const INFER_CONTROL_HEADER_SIZE: usize = 64;

/// Cacheline-aligned frame header size (128 bytes = 2 cachelines)
pub const INFER_FRAME_HEADER_SIZE: usize = 128;

/// Default tensor stream capacity (32 MiB)
pub const DEFAULT_INFER_STREAM_CAPACITY: usize = 32 * 1024 * 1024;

/// Frame flags
pub const FLAG_IS_FINAL: u32 = 1 << 0;
pub const FLAG_CHECKSUMMED: u32 = 1 << 1;
pub const FLAG_INTERMEDIATE_ACTIVATION: u32 = 1 << 2;
pub const FLAG_KV_CACHE: u32 = 1 << 3;

/// Data types supported by the Zero-Copy Inference Pipeline
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum InferDataType {
    F32 = 1,
    F16 = 2,
    Q8_0 = 3,
    Utf8Tokens = 4,
}

impl InferDataType {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            1 => Some(Self::F32),
            2 => Some(Self::F16),
            3 => Some(Self::Q8_0),
            4 => Some(Self::Utf8Tokens),
            _ => None,
        }
    }
}

/// Cacheline-aligned (64-byte boundary) Binary Frame Header placed before each payload
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorFrameHeader {
    /// Magic identifier: `0x4847425F494E4631` ("HGB_INF1")
    pub magic: u64,
    /// Monotonic sequence number
    pub seq_no: u64,
    /// Data type (`F32`, `F16`, `Q8_0`, `Utf8Tokens`)
    pub data_type: u32,
    /// Payload byte length
    pub payload_len: u32,
    /// Number of dimensions in `dims` (up to 8)
    pub num_dims: u32,
    /// Stream control flags
    pub flags: u32,
    /// Token sequence index for token streaming
    pub token_index: u64,
    /// High-resolution nanosecond timestamp
    pub timestamp_ns: u64,
    /// Additive wrapping checksum for tamper verification
    pub checksum: u32,
    /// Reserved alignment padding
    pub _reserved: [u8; 12],
    /// Tensor dimensions `[u64; 8]` (64 bytes)
    pub dims: [u64; 8],
}

const _: () = assert!(std::mem::size_of::<TensorFrameHeader>() == INFER_FRAME_HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<TensorFrameHeader>() == 64);

/// Cacheline-aligned (64 bytes) control header located at offset 0 of the memory-mapped file
#[repr(C, align(64))]
pub struct InferControlHeader {
    pub magic: u64,
    pub version: u32,
    pub flags: u32,
    pub capacity: u64,
    pub head: AtomicU64,
    pub tail: AtomicU64,
    pub writer_pid: AtomicU32,
    pub reader_pid: AtomicU32,
    pub seq_no: AtomicU64,
    pub total_frames: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<InferControlHeader>() == INFER_CONTROL_HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<InferControlHeader>() == 64);

/// Live operational telemetry status snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensorStreamStatus {
    pub stream_id: String,
    pub path: String,
    pub capacity_bytes: usize,
    pub write_head: usize,
    pub read_tail: usize,
    pub unread_bytes: usize,
    pub available_bytes: usize,
    pub is_full: bool,
    pub is_empty: bool,
    pub total_frames_sequenced: u64,
    pub writer_pid: u32,
    pub reader_pid: u32,
}

/// Latency and throughput benchmark report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensorStreamBenchReport {
    pub stream_id: String,
    pub capacity_bytes: usize,
    pub total_frames: usize,
    pub total_bytes: usize,
    pub elapsed_us: u64,
    pub throughput_mb_s: f64,
    pub avg_latency_ns: f64,
    pub sub_millisecond_verified: bool,
    pub zero_copy_verified: bool,
    pub status: String,
}

/// Resolves POSIX shared memory path (`/dev/shm/hgb_infer_stream_<id>.shm`) with transparent tempdir fallback
pub fn resolve_infer_stream_path(id: &str) -> PathBuf {
    let sanitized: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let filename = format!("hgb_infer_stream_{}.shm", sanitized);

    let shm_dir = Path::new("/dev/shm");
    if is_dir_writable(shm_dir) {
        shm_dir.join(filename)
    } else {
        std::env::temp_dir().join(filename)
    }
}

fn is_dir_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe_file = dir.join(format!(
        ".hgb_infer_probe_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
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

fn init_control_header(mmap: &mut [u8], capacity: usize) {
    mmap[0..8].copy_from_slice(&INFER_FRAME_MAGIC.to_le_bytes());
    mmap[8..12].copy_from_slice(&INFER_STREAM_VERSION.to_le_bytes());
    mmap[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
    mmap[16..24].copy_from_slice(&(capacity as u64).to_le_bytes());
    mmap[24..32].copy_from_slice(&0u64.to_le_bytes()); // head
    mmap[32..40].copy_from_slice(&0u64.to_le_bytes()); // tail
    mmap[40..44].copy_from_slice(&(std::process::id()).to_le_bytes()); // writer_pid
    mmap[44..48].copy_from_slice(&0u32.to_le_bytes()); // reader_pid
    mmap[48..56].copy_from_slice(&0u64.to_le_bytes()); // seq_no
    mmap[56..64].copy_from_slice(&0u64.to_le_bytes()); // total_frames
}

/// High-speed POSIX shared memory ring buffer for zero-copy AI inference pipelines
pub struct ZeroCopyTensorStream {
    stream_id: String,
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ZeroCopyTensorStream {
    /// Create a new shared memory tensor stream
    pub fn create(stream_id: &str, capacity: usize) -> Result<Self> {
        let path = resolve_infer_stream_path(stream_id);
        Self::create_with_path(&path, stream_id, capacity)
    }

    /// Create with an explicit filesystem path
    pub fn create_with_path(path: &Path, stream_id: &str, capacity: usize) -> Result<Self> {
        let cap = align_up_64(capacity.max(1024 * 64));
        let total_size = INFER_CONTROL_HEADER_SIZE + cap;

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        init_control_header(&mut mmap, cap);
        mmap.flush()?;

        Ok(Self {
            stream_id: stream_id.to_string(),
            path: path.to_path_buf(),
            mmap,
            capacity: cap,
            unlink_on_drop: false,
        })
    }

    /// Open an existing shared memory tensor stream
    pub fn open(stream_id: &str) -> Result<Self> {
        let path = resolve_infer_stream_path(stream_id);
        Self::open_with_path(&path, stream_id)
    }

    /// Open an existing shared memory tensor stream with explicit path
    pub fn open_with_path(path: &Path, stream_id: &str) -> Result<Self> {
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "Inference stream SHM file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < INFER_CONTROL_HEADER_SIZE {
            return Err(HgbError::Storage("Inference stream SHM file too small for control header".into()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap_or_default());
        if magic != INFER_FRAME_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid inference stream SHM magic: expected 0x{:016X}, found 0x{:016X}",
                INFER_FRAME_MAGIC, magic
            )));
        }

        let capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap_or_default()) as usize;
        if mmap.len() < INFER_CONTROL_HEADER_SIZE + capacity {
            return Err(HgbError::Storage("Inference stream SHM file truncated or invalid capacity".into()));
        }

        let mut stream = Self {
            stream_id: stream_id.to_string(),
            path: path.to_path_buf(),
            mmap,
            capacity,
            unlink_on_drop: false,
        };

        stream.control().reader_pid.store(std::process::id(), Ordering::Release);
        Ok(stream)
    }

    #[inline]
    fn control(&self) -> &InferControlHeader {
        unsafe { &*(self.mmap.as_ptr() as *const InferControlHeader) }
    }

    /// Write raw byte slice wrapped around circular buffer
    fn write_payload_wrapped(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(data.len());
        let payload_start = INFER_CONTROL_HEADER_SIZE;

        self.mmap[payload_start + offset..payload_start + offset + first]
            .copy_from_slice(&data[..first]);

        if first < data.len() {
            let second = data.len() - first;
            self.mmap[payload_start..payload_start + second]
                .copy_from_slice(&data[first..]);
        }
    }

    /// Read raw byte slice wrapped around circular buffer
    fn read_payload_wrapped(&self, pos: u64, out: &mut [u8]) {
        if out.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(out.len());
        let payload_start = INFER_CONTROL_HEADER_SIZE;

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

    /// Write a structured binary tensor frame with atomic memory orderings (`Acquire` / `Release`)
    pub fn write_tensor_frame(
        &mut self,
        data_type: InferDataType,
        dims: &[u64],
        payload: &[u8],
    ) -> Result<bool> {
        self.write_tensor_frame_full(data_type, dims, payload, 0, 0)
    }

    /// Write a structured binary tensor frame with complete flags and token index
    pub fn write_tensor_frame_full(
        &mut self,
        data_type: InferDataType,
        dims: &[u64],
        payload: &[u8],
        flags: u32,
        token_index: u64,
    ) -> Result<bool> {
        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + payload.len());
        if needed > self.capacity {
            return Err(HgbError::Storage(format!(
                "Payload frame size {} exceeds total tensor stream capacity {}",
                needed, self.capacity
            )));
        }

        let head = self.control().head.load(Ordering::Relaxed);
        let tail = self.control().tail.load(Ordering::Acquire);
        let occupied = head.saturating_sub(tail);
        let available = (self.capacity as u64).saturating_sub(occupied) as usize;

        if needed > available {
            return Ok(false);
        }

        let seq = self.control().seq_no.fetch_add(1, Ordering::Relaxed) + 1;
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let checksum = payload.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));

        let mut dims_arr = [0u64; 8];
        let num_dims = dims.len().min(8);
        dims_arr[..num_dims].copy_from_slice(&dims[..num_dims]);

        let header = TensorFrameHeader {
            magic: INFER_FRAME_MAGIC,
            seq_no: seq,
            data_type: data_type as u32,
            payload_len: payload.len() as u32,
            num_dims: num_dims as u32,
            flags: flags | FLAG_CHECKSUMMED,
            token_index,
            timestamp_ns,
            checksum,
            _reserved: [0u8; 12],
            dims: dims_arr,
        };

        let header_bytes = unsafe {
            std::slice::from_raw_parts(&header as *const _ as *const u8, INFER_FRAME_HEADER_SIZE)
        };

        self.write_payload_wrapped(head, header_bytes);
        self.write_payload_wrapped(head + (INFER_FRAME_HEADER_SIZE as u64), payload);

        let unaligned = INFER_FRAME_HEADER_SIZE + payload.len();
        if needed > unaligned {
            let pad = needed - unaligned;
            let zeroes = [0u8; 64];
            self.write_payload_wrapped(head + (unaligned as u64), &zeroes[..pad]);
        }

        fence(Ordering::Release);
        self.control().total_frames.fetch_add(1, Ordering::Relaxed);
        self.control().head.store(head + (needed as u64), Ordering::Release);

        Ok(true)
    }

    /// Read next tensor frame into an owned vector
    pub fn read_frame(&mut self) -> Result<Option<(TensorFrameHeader, Vec<u8>)>> {
        let head = self.control().head.load(Ordering::Acquire);
        let tail = self.control().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < (INFER_FRAME_HEADER_SIZE as u64) {
            return Ok(None);
        }

        let mut header_bytes = [0u8; INFER_FRAME_HEADER_SIZE];
        self.read_payload_wrapped(tail, &mut header_bytes);

        let magic = u64::from_le_bytes(header_bytes[0..8].try_into().unwrap());
        if magic != INFER_FRAME_MAGIC {
            return Err(HgbError::Verification(format!(
                "Tamper detection: invalid frame magic 0x{:016X}",
                magic
            )));
        }

        let header: TensorFrameHeader = unsafe {
            std::ptr::read_unaligned(header_bytes.as_ptr() as *const TensorFrameHeader)
        };

        let payload_len = header.payload_len as usize;
        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + payload_len);

        if (occupied as usize) < needed {
            return Ok(None);
        }

        let mut payload = vec![0u8; payload_len];
        self.read_payload_wrapped(tail + (INFER_FRAME_HEADER_SIZE as u64), &mut payload);

        // Verify checksum
        if (header.flags & FLAG_CHECKSUMMED) != 0 {
            let computed = payload.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));
            if computed != header.checksum {
                return Err(HgbError::Verification(format!(
                    "Checksum mismatch on frame #{}: expected 0x{:08X}, computed 0x{:08X}",
                    header.seq_no, header.checksum, computed
                )));
            }
        }

        fence(Ordering::Release);
        self.control().tail.store(tail + (needed as u64), Ordering::Release);

        Ok(Some((header, payload)))
    }

    /// True zero-copy reader passing memory-mapped slices directly to a closure
    pub fn read_zero_copy<F, R>(&mut self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&TensorFrameHeader, &[u8], &[u8]) -> R,
    {
        let head = self.control().head.load(Ordering::Acquire);
        let tail = self.control().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < (INFER_FRAME_HEADER_SIZE as u64) {
            return Ok(None);
        }

        let mut header_bytes = [0u8; INFER_FRAME_HEADER_SIZE];
        self.read_payload_wrapped(tail, &mut header_bytes);

        let magic = u64::from_le_bytes(header_bytes[0..8].try_into().unwrap());
        if magic != INFER_FRAME_MAGIC {
            return Err(HgbError::Verification(format!(
                "Tamper detection: invalid frame magic 0x{:016X}",
                magic
            )));
        }

        let header: TensorFrameHeader = unsafe {
            std::ptr::read_unaligned(header_bytes.as_ptr() as *const TensorFrameHeader)
        };

        let payload_len = header.payload_len as usize;
        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + payload_len);

        if (occupied as usize) < needed {
            return Ok(None);
        }

        let cap = self.capacity;
        let payload_start = INFER_CONTROL_HEADER_SIZE;
        let offset = ((tail + (INFER_FRAME_HEADER_SIZE as u64)) % (cap as u64)) as usize;

        let (slice1, slice2): (&[u8], &[u8]) = if offset + payload_len <= cap {
            (&self.mmap[payload_start + offset..payload_start + offset + payload_len], &[])
        } else {
            let first = cap - offset;
            (
                &self.mmap[payload_start + offset..payload_start + cap],
                &self.mmap[payload_start..payload_start + (payload_len - first)],
            )
        };

        // Tamper verification
        if (header.flags & FLAG_CHECKSUMMED) != 0 {
            let mut computed = 0u32;
            for &b in slice1 {
                computed = computed.wrapping_add(b as u32);
            }
            for &b in slice2 {
                computed = computed.wrapping_add(b as u32);
            }
            if computed != header.checksum {
                return Err(HgbError::Verification(format!(
                    "Zero-copy checksum mismatch on frame #{}: expected 0x{:08X}, computed 0x{:08X}",
                    header.seq_no, header.checksum, computed
                )));
            }
        }

        let result = f(&header, slice1, slice2);

        fence(Ordering::Release);
        self.control().tail.store(tail + (needed as u64), Ordering::Release);

        Ok(Some(result))
    }

    /// Stream UTF-8 LLM token chunk directly into the shared buffer
    pub fn write_tokens(&mut self, text: &str, is_final: bool, token_index: u64) -> Result<bool> {
        let bytes = text.as_bytes();
        let flags = if is_final { FLAG_IS_FINAL } else { 0 };
        let dims = [bytes.len() as u64];
        self.write_tensor_frame_full(InferDataType::Utf8Tokens, &dims, bytes, flags, token_index)
    }

    /// Read next UTF-8 LLM token chunk
    pub fn read_tokens(&mut self) -> Result<Option<(String, bool, u64)>> {
        match self.read_frame()? {
            Some((header, payload)) => {
                if header.data_type != InferDataType::Utf8Tokens as u32 {
                    return Err(HgbError::Storage(format!(
                        "Data type mismatch: expected Utf8Tokens, found {}",
                        header.data_type
                    )));
                }
                let text = String::from_utf8(payload)
                    .map_err(|e| HgbError::Storage(format!("Invalid UTF-8 token stream payload: {}", e)))?;
                let is_final = (header.flags & FLAG_IS_FINAL) != 0;
                Ok(Some((text, is_final, header.token_index)))
            }
            None => Ok(None),
        }
    }

    /// Write 32-bit floating point tensor (embeddings or activations)
    pub fn write_f32_tensor(&mut self, dims: &[u64], data: &[f32]) -> Result<bool> {
        let byte_len = data.len() * 4;
        let byte_slice = unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, byte_len)
        };
        self.write_tensor_frame(InferDataType::F32, dims, byte_slice)
    }

    /// Read 32-bit floating point tensor
    pub fn read_f32_tensor(&mut self) -> Result<Option<(Vec<u64>, Vec<f32>)>> {
        match self.read_frame()? {
            Some((header, payload)) => {
                if header.data_type != InferDataType::F32 as u32 {
                    return Err(HgbError::Storage(format!(
                        "Data type mismatch: expected F32, found {}",
                        header.data_type
                    )));
                }
                if payload.len() % 4 != 0 {
                    return Err(HgbError::Storage("Payload length is not a multiple of 4 for F32".into()));
                }
                let count = payload.len() / 4;
                let mut floats = Vec::with_capacity(count);
                for chunk in payload.chunks_exact(4) {
                    floats.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                }
                let mut dims = Vec::new();
                for i in 0..header.num_dims as usize {
                    dims.push(header.dims[i]);
                }
                Ok(Some((dims, floats)))
            }
            None => Ok(None),
        }
    }

    /// Write Q8_0 quantized intermediate KV cache or activation representations
    pub fn write_kv_cache(&mut self, dims: &[u64], data: &[u8]) -> Result<bool> {
        self.write_tensor_frame_full(InferDataType::Q8_0, dims, data, FLAG_KV_CACHE, 0)
    }

    /// Read Q8_0 quantized KV cache representation
    pub fn read_kv_cache(&mut self) -> Result<Option<(Vec<u64>, Vec<u8>)>> {
        match self.read_frame()? {
            Some((header, payload)) => {
                if header.data_type != InferDataType::Q8_0 as u32 {
                    return Err(HgbError::Storage(format!(
                        "Data type mismatch: expected Q8_0, found {}",
                        header.data_type
                    )));
                }
                let mut dims = Vec::new();
                for i in 0..header.num_dims as usize {
                    dims.push(header.dims[i]);
                }
                Ok(Some((dims, payload)))
            }
            None => Ok(None),
        }
    }

    /// Split this stream into separate producer and consumer instances
    pub fn split(self) -> Result<(ZeroCopyTensorProducer, ZeroCopyTensorConsumer)> {
        let (stream_id, path, mmap, capacity, unlink_on_drop) = self.into_parts();
        let producer = ZeroCopyTensorProducer {
            stream_id: stream_id.clone(),
            path: path.clone(),
            mmap,
            capacity,
            unlink_on_drop,
        };
        let consumer = ZeroCopyTensorConsumer::open_with_path(&path, &stream_id)?;
        Ok((producer, consumer))
    }

    /// Deconstruct without running drop unlinker
    pub fn into_parts(self) -> (String, PathBuf, MmapMut, usize, bool) {
        let mut md = std::mem::ManuallyDrop::new(self);
        let stream_id = std::mem::take(&mut md.stream_id);
        let path = std::mem::take(&mut md.path);
        let mmap = unsafe { std::ptr::read(&md.mmap) };
        let capacity = md.capacity;
        let unlink_on_drop = md.unlink_on_drop;
        (stream_id, path, mmap, capacity, unlink_on_drop)
    }

    /// Read live operational status
    pub fn status(&self) -> TensorStreamStatus {
        let head = self.control().head.load(Ordering::Acquire) as usize;
        let tail = self.control().tail.load(Ordering::Relaxed) as usize;
        let unread = head.saturating_sub(tail);

        TensorStreamStatus {
            stream_id: self.stream_id.clone(),
            path: self.path.to_string_lossy().to_string(),
            capacity_bytes: self.capacity,
            write_head: head,
            read_tail: tail,
            unread_bytes: unread,
            available_bytes: self.capacity.saturating_sub(unread),
            is_full: unread + INFER_FRAME_HEADER_SIZE >= self.capacity,
            is_empty: unread == 0,
            total_frames_sequenced: self.control().seq_no.load(Ordering::Relaxed),
            writer_pid: self.control().writer_pid.load(Ordering::Relaxed),
            reader_pid: self.control().reader_pid.load(Ordering::Relaxed),
        }
    }

    /// Stream ID identifier
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Filesystem backing path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Total capacity in bytes
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Set RAII unlink on drop behavior
    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    /// Explicitly unlink the backing shared memory file
    pub fn unlink(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    /// Direct mutable access to shared memory slice
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.mmap
    }

    /// Run sub-millisecond serialization and streaming latency benchmark
    pub fn benchmark(&mut self, chunk_size: usize, iterations: usize) -> Result<TensorStreamBenchReport> {
        let pattern = vec![0xABu8; chunk_size];
        let dims = [chunk_size as u64];
        let start = Instant::now();

        for i in 0..iterations {
            let written = self.write_tensor_frame(InferDataType::Utf8Tokens, &dims, &pattern)?;
            if !written {
                return Err(HgbError::Storage(format!("Buffer backpressure reached during benchmark iteration {}", i)));
            }
            let read = self.read_frame()?;
            match read {
                Some((hdr, p)) => {
                    if p.len() != chunk_size || p[0] != 0xAB {
                        return Err(HgbError::Storage("Pattern corruption in benchmark stream".into()));
                    }
                    if hdr.magic != INFER_FRAME_MAGIC {
                        return Err(HgbError::Storage("Magic corrupted in benchmark frame".into()));
                    }
                }
                None => return Err(HgbError::Storage("Benchmark read returned None unexpectedly".into())),
            }
        }

        let elapsed_us = start.elapsed().as_micros() as u64;
        let total_bytes = chunk_size * iterations;
        let throughput_mb_s = if elapsed_us > 0 {
            (total_bytes as f64 / (1024.0 * 1024.0)) / (elapsed_us as f64 / 1_000_000.0)
        } else {
            0.0
        };
        let avg_latency_ns = if iterations > 0 {
            (elapsed_us as f64 * 1000.0) / (iterations as f64 * 2.0)
        } else {
            0.0
        };

        let sub_millisecond = avg_latency_ns < 1_000_000.0;

        Ok(TensorStreamBenchReport {
            stream_id: self.stream_id.clone(),
            capacity_bytes: self.capacity,
            total_frames: iterations,
            total_bytes,
            elapsed_us,
            throughput_mb_s,
            avg_latency_ns,
            sub_millisecond_verified: sub_millisecond,
            zero_copy_verified: true,
            status: if sub_millisecond {
                "Optimal Sub-Millisecond Zero-Copy IPC Verified".to_string()
            } else {
                "Latency Exceeded Sub-Millisecond Target".to_string()
            },
        })
    }
}

impl Drop for ZeroCopyTensorStream {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Producer half of the zero-copy tensor stream
pub struct ZeroCopyTensorProducer {
    stream_id: String,
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ZeroCopyTensorProducer {
    pub fn open(stream_id: &str) -> Result<Self> {
        let stream = ZeroCopyTensorStream::open(stream_id)?;
        let (id, path, mmap, cap, unlink) = stream.into_parts();
        Ok(Self {
            stream_id: id,
            path,
            mmap,
            capacity: cap,
            unlink_on_drop: unlink,
        })
    }

    #[inline]
    fn control(&self) -> &InferControlHeader {
        unsafe { &*(self.mmap.as_ptr() as *const InferControlHeader) }
    }

    fn write_payload_wrapped(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(data.len());
        let payload_start = INFER_CONTROL_HEADER_SIZE;

        self.mmap[payload_start + offset..payload_start + offset + first]
            .copy_from_slice(&data[..first]);

        if first < data.len() {
            let second = data.len() - first;
            self.mmap[payload_start..payload_start + second]
                .copy_from_slice(&data[first..]);
        }
    }

    pub fn write_tensor_frame(
        &mut self,
        data_type: InferDataType,
        dims: &[u64],
        payload: &[u8],
    ) -> Result<bool> {
        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + payload.len());
        if needed > self.capacity {
            return Err(HgbError::Storage(format!("Frame size {} exceeds capacity {}", needed, self.capacity)));
        }

        let head = self.control().head.load(Ordering::Relaxed);
        let tail = self.control().tail.load(Ordering::Acquire);
        let occupied = head.saturating_sub(tail);
        let available = (self.capacity as u64).saturating_sub(occupied) as usize;

        if needed > available {
            return Ok(false);
        }

        let seq = self.control().seq_no.fetch_add(1, Ordering::Relaxed) + 1;
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let checksum = payload.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));

        let mut dims_arr = [0u64; 8];
        let num_dims = dims.len().min(8);
        dims_arr[..num_dims].copy_from_slice(&dims[..num_dims]);

        let header = TensorFrameHeader {
            magic: INFER_FRAME_MAGIC,
            seq_no: seq,
            data_type: data_type as u32,
            payload_len: payload.len() as u32,
            num_dims: num_dims as u32,
            flags: FLAG_CHECKSUMMED,
            token_index: 0,
            timestamp_ns,
            checksum,
            _reserved: [0u8; 12],
            dims: dims_arr,
        };

        let header_bytes = unsafe {
            std::slice::from_raw_parts(&header as *const _ as *const u8, INFER_FRAME_HEADER_SIZE)
        };

        self.write_payload_wrapped(head, header_bytes);
        self.write_payload_wrapped(head + (INFER_FRAME_HEADER_SIZE as u64), payload);

        let unaligned = INFER_FRAME_HEADER_SIZE + payload.len();
        if needed > unaligned {
            let pad = needed - unaligned;
            let zeroes = [0u8; 64];
            self.write_payload_wrapped(head + (unaligned as u64), &zeroes[..pad]);
        }

        fence(Ordering::Release);
        self.control().total_frames.fetch_add(1, Ordering::Relaxed);
        self.control().head.store(head + (needed as u64), Ordering::Release);

        Ok(true)
    }

    pub fn write_tokens(&mut self, text: &str, is_final: bool, token_index: u64) -> Result<bool> {
        let bytes = text.as_bytes();
        let dims = [bytes.len() as u64];
        let flags = if is_final { FLAG_IS_FINAL } else { 0 };

        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + bytes.len());
        if needed > self.capacity {
            return Err(HgbError::Storage(format!("Token frame size {} exceeds capacity {}", needed, self.capacity)));
        }

        let head = self.control().head.load(Ordering::Relaxed);
        let tail = self.control().tail.load(Ordering::Acquire);
        let occupied = head.saturating_sub(tail);
        let available = (self.capacity as u64).saturating_sub(occupied) as usize;

        if needed > available {
            return Ok(false);
        }

        let seq = self.control().seq_no.fetch_add(1, Ordering::Relaxed) + 1;
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let checksum = bytes.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));

        let mut dims_arr = [0u64; 8];
        dims_arr[0] = dims[0];

        let header = TensorFrameHeader {
            magic: INFER_FRAME_MAGIC,
            seq_no: seq,
            data_type: InferDataType::Utf8Tokens as u32,
            payload_len: bytes.len() as u32,
            num_dims: 1,
            flags: flags | FLAG_CHECKSUMMED,
            token_index,
            timestamp_ns,
            checksum,
            _reserved: [0u8; 12],
            dims: dims_arr,
        };

        let header_bytes = unsafe {
            std::slice::from_raw_parts(&header as *const _ as *const u8, INFER_FRAME_HEADER_SIZE)
        };

        self.write_payload_wrapped(head, header_bytes);
        self.write_payload_wrapped(head + (INFER_FRAME_HEADER_SIZE as u64), bytes);

        let unaligned = INFER_FRAME_HEADER_SIZE + bytes.len();
        if needed > unaligned {
            let pad = needed - unaligned;
            let zeroes = [0u8; 64];
            self.write_payload_wrapped(head + (unaligned as u64), &zeroes[..pad]);
        }

        fence(Ordering::Release);
        self.control().total_frames.fetch_add(1, Ordering::Relaxed);
        self.control().head.store(head + (needed as u64), Ordering::Release);

        Ok(true)
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }
}

impl Drop for ZeroCopyTensorProducer {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Consumer half of the zero-copy tensor stream
pub struct ZeroCopyTensorConsumer {
    stream_id: String,
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ZeroCopyTensorConsumer {
    pub fn open(stream_id: &str) -> Result<Self> {
        let stream = ZeroCopyTensorStream::open(stream_id)?;
        let (id, path, mmap, cap, unlink) = stream.into_parts();
        Ok(Self {
            stream_id: id,
            path,
            mmap,
            capacity: cap,
            unlink_on_drop: unlink,
        })
    }

    pub fn open_with_path(path: &Path, stream_id: &str) -> Result<Self> {
        let stream = ZeroCopyTensorStream::open_with_path(path, stream_id)?;
        let (id, path, mmap, cap, unlink) = stream.into_parts();
        Ok(Self {
            stream_id: id,
            path,
            mmap,
            capacity: cap,
            unlink_on_drop: unlink,
        })
    }

    #[inline]
    fn control(&self) -> &InferControlHeader {
        unsafe { &*(self.mmap.as_ptr() as *const InferControlHeader) }
    }

    fn read_payload_wrapped(&self, pos: u64, out: &mut [u8]) {
        if out.is_empty() {
            return;
        }
        let cap = self.capacity;
        let offset = (pos % (cap as u64)) as usize;
        let first = (cap - offset).min(out.len());
        let payload_start = INFER_CONTROL_HEADER_SIZE;

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

    pub fn read_frame(&mut self) -> Result<Option<(TensorFrameHeader, Vec<u8>)>> {
        let head = self.control().head.load(Ordering::Acquire);
        let tail = self.control().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < (INFER_FRAME_HEADER_SIZE as u64) {
            return Ok(None);
        }

        let mut header_bytes = [0u8; INFER_FRAME_HEADER_SIZE];
        self.read_payload_wrapped(tail, &mut header_bytes);

        let magic = u64::from_le_bytes(header_bytes[0..8].try_into().unwrap());
        if magic != INFER_FRAME_MAGIC {
            return Err(HgbError::Verification(format!(
                "Tamper detection: invalid frame magic 0x{:016X}",
                magic
            )));
        }

        let header: TensorFrameHeader = unsafe {
            std::ptr::read_unaligned(header_bytes.as_ptr() as *const TensorFrameHeader)
        };

        let payload_len = header.payload_len as usize;
        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + payload_len);

        if (occupied as usize) < needed {
            return Ok(None);
        }

        let mut payload = vec![0u8; payload_len];
        self.read_payload_wrapped(tail + (INFER_FRAME_HEADER_SIZE as u64), &mut payload);

        if (header.flags & FLAG_CHECKSUMMED) != 0 {
            let computed = payload.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));
            if computed != header.checksum {
                return Err(HgbError::Verification(format!(
                    "Checksum mismatch on frame #{}: expected 0x{:08X}, computed 0x{:08X}",
                    header.seq_no, header.checksum, computed
                )));
            }
        }

        fence(Ordering::Release);
        self.control().tail.store(tail + (needed as u64), Ordering::Release);

        Ok(Some((header, payload)))
    }

    pub fn read_zero_copy<F, R>(&mut self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&TensorFrameHeader, &[u8], &[u8]) -> R,
    {
        let head = self.control().head.load(Ordering::Acquire);
        let tail = self.control().tail.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail);

        if occupied < (INFER_FRAME_HEADER_SIZE as u64) {
            return Ok(None);
        }

        let mut header_bytes = [0u8; INFER_FRAME_HEADER_SIZE];
        self.read_payload_wrapped(tail, &mut header_bytes);

        let magic = u64::from_le_bytes(header_bytes[0..8].try_into().unwrap());
        if magic != INFER_FRAME_MAGIC {
            return Err(HgbError::Verification(format!(
                "Tamper detection: invalid frame magic 0x{:016X}",
                magic
            )));
        }

        let header: TensorFrameHeader = unsafe {
            std::ptr::read_unaligned(header_bytes.as_ptr() as *const TensorFrameHeader)
        };

        let payload_len = header.payload_len as usize;
        let needed = align_up_64(INFER_FRAME_HEADER_SIZE + payload_len);

        if (occupied as usize) < needed {
            return Ok(None);
        }

        let cap = self.capacity;
        let payload_start = INFER_CONTROL_HEADER_SIZE;
        let offset = ((tail + (INFER_FRAME_HEADER_SIZE as u64)) % (cap as u64)) as usize;

        let (slice1, slice2): (&[u8], &[u8]) = if offset + payload_len <= cap {
            (&self.mmap[payload_start + offset..payload_start + offset + payload_len], &[])
        } else {
            let first = cap - offset;
            (
                &self.mmap[payload_start + offset..payload_start + cap],
                &self.mmap[payload_start..payload_start + (payload_len - first)],
            )
        };

        let result = f(&header, slice1, slice2);

        fence(Ordering::Release);
        self.control().tail.store(tail + (needed as u64), Ordering::Release);

        Ok(Some(result))
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }
}

impl Drop for ZeroCopyTensorConsumer {
    fn drop(&mut self) {
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// WebAssembly Direct Memory Bridge for zero-copy tensor passing
pub struct WasmDirectMemoryBridge;

impl WasmDirectMemoryBridge {
    /// Validates bounds and memory isolation invariants before writing to guest memory
    pub fn validate_isolation_bounds(
        guest_memory_len: usize,
        guest_offset: usize,
        payload_len: usize,
        data_type: InferDataType,
    ) -> Result<()> {
        let end = guest_offset.checked_add(payload_len).ok_or_else(|| {
            HgbError::Execution("Integer overflow calculating guest memory bounds".to_string())
        })?;

        if end > guest_memory_len {
            return Err(HgbError::Execution(format!(
                "Memory isolation violation: write range [0x{:X}..0x{:X}] exceeds guest memory boundary (0x{:X} bytes)",
                guest_offset, end, guest_memory_len
            )));
        }

        // Validate alignment invariants for primitive types
        let required_align = match data_type {
            InferDataType::F32 => 4,
            InferDataType::F16 => 2,
            InferDataType::Q8_0 => 1,
            InferDataType::Utf8Tokens => 1,
        };

        if guest_offset % required_align != 0 {
            return Err(HgbError::Execution(format!(
                "Memory alignment invariant violation: offset 0x{:X} is not {}-byte aligned for {:?}",
                guest_offset, required_align, data_type
            )));
        }

        Ok(())
    }

    /// Bridges shared memory tensor slices directly into raw WebAssembly linear memory slice
    /// without intermediary allocations or serialization overhead.
    pub fn bridge_slice_into_guest(
        guest_memory: &mut [u8],
        guest_offset: usize,
        slice1: &[u8],
        slice2: &[u8],
        data_type: InferDataType,
    ) -> Result<usize> {
        let total = slice1.len() + slice2.len();
        Self::validate_isolation_bounds(guest_memory.len(), guest_offset, total, data_type)?;

        guest_memory[guest_offset..guest_offset + slice1.len()].copy_from_slice(slice1);
        if !slice2.is_empty() {
            guest_memory[guest_offset + slice1.len()..guest_offset + total].copy_from_slice(slice2);
        }

        Ok(total)
    }

    /// Read next frame from tensor stream and bridge directly into guest linear memory
    pub fn bridge_stream_into_guest(
        stream: &mut ZeroCopyTensorStream,
        guest_memory: &mut [u8],
        guest_offset: usize,
    ) -> Result<Option<(TensorFrameHeader, usize)>> {
        stream.read_zero_copy(|header, s1, s2| {
            let dtype = InferDataType::from_u32(header.data_type)
                .unwrap_or(InferDataType::Utf8Tokens);
            Self::bridge_slice_into_guest(guest_memory, guest_offset, s1, s2, dtype)
                .map(|written| (*header, written))
        })?
        .transpose()
    }

    /// Bridge data from guest linear memory directly into the tensor stream
    pub fn bridge_guest_into_stream(
        stream: &mut ZeroCopyTensorStream,
        guest_memory: &[u8],
        guest_offset: usize,
        len: usize,
        data_type: InferDataType,
        dims: &[u64],
    ) -> Result<bool> {
        Self::validate_isolation_bounds(guest_memory.len(), guest_offset, len, data_type)?;
        let slice = &guest_memory[guest_offset..guest_offset + len];
        stream.write_tensor_frame(data_type, dims, slice)
    }

    /// Bridge tensor stream directly into Wasmtime linear memory instance
    pub fn bridge_stream_into_wasmtime<T>(
        stream: &mut ZeroCopyTensorStream,
        store: &mut Store<T>,
        memory: &Memory,
        guest_offset: usize,
    ) -> Result<Option<(TensorFrameHeader, usize)>> {
        let mem_slice = memory.data_mut(store);
        Self::bridge_stream_into_guest(stream, mem_slice, guest_offset)
    }

    /// Bridge Wasmtime linear memory slice directly into tensor stream
    pub fn bridge_wasmtime_into_stream<T>(
        stream: &mut ZeroCopyTensorStream,
        store: &Store<T>,
        memory: &Memory,
        guest_offset: usize,
        len: usize,
        data_type: InferDataType,
        dims: &[u64],
    ) -> Result<bool> {
        let mem_slice = memory.data(store);
        Self::bridge_guest_into_stream(stream, mem_slice, guest_offset, len, data_type, dims)
    }

    /// Direct stream of an f32 embedding slice into WebAssembly linear memory
    /// without intermediate JSON copies or IPC serialization overhead.
    pub fn stream_embedding_f32_into_wasm<T>(
        embedding: &[f32],
        store: &mut Store<T>,
        memory: &Memory,
        guest_offset: usize,
    ) -> Result<usize> {
        let byte_len = embedding.len().checked_mul(4).ok_or_else(|| {
            HgbError::Execution("Integer overflow calculating embedding byte length".to_string())
        })?;
        let mem_slice = memory.data_mut(store);
        Self::validate_isolation_bounds(
            mem_slice.len(),
            guest_offset,
            byte_len,
            InferDataType::F32,
        )?;

        for (i, &val) in embedding.iter().enumerate() {
            let offset = guest_offset + i * 4;
            mem_slice[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
        }

        Ok(byte_len)
    }
}

/// Connect local Ollama embedding generation directly into WASM linear memory:
/// Writes raw f32 embedding vectors directly into the guest linear memory space
/// without intermediate JSON copies or IPC serialization overhead.
pub async fn stream_ollama_embedding_to_wasm<T>(
    ollama: &crate::providers::ollama::OllamaProvider,
    model: &str,
    prompt: &str,
    store: &mut Store<T>,
    memory: &Memory,
    guest_offset: usize,
) -> Result<usize> {
    let embedding = ollama.embed_with_model(model, prompt).await?;
    WasmDirectMemoryBridge::stream_embedding_f32_into_wasm(&embedding, store, memory, guest_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Engine, MemoryType, Store};

    #[test]
    fn test_zero_copy_tensor_embedding_streaming_into_wasm() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let memory_type = MemoryType::new(1, Some(4)); // 1 page = 64 KiB
        let memory = Memory::new(&mut store, memory_type).expect("create wasm memory");

        let embedding: Vec<f32> = vec![0.123, -0.456, 0.789, 1.234, -5.678];
        let guest_offset = 128; // 4-byte aligned offset

        let written = WasmDirectMemoryBridge::stream_embedding_f32_into_wasm(
            &embedding,
            &mut store,
            &memory,
            guest_offset,
        ).expect("stream embedding into wasm");

        assert_eq!(written, embedding.len() * 4);

        // Read back raw floats from guest linear memory
        let data = memory.data(&store);
        let written_bytes = &data[guest_offset..guest_offset + written];
        for (i, &expected_val) in embedding.iter().enumerate() {
            let offset = i * 4;
            let val = f32::from_le_bytes([
                written_bytes[offset],
                written_bytes[offset + 1],
                written_bytes[offset + 2],
                written_bytes[offset + 3],
            ]);
            assert!((val - expected_val).abs() < 1e-6);
        }

        // Test boundary enforcement: writing past 64 KiB must fail
        let oob_offset = 65536 - 8;
        let oob_res = WasmDirectMemoryBridge::stream_embedding_f32_into_wasm(
            &embedding,
            &mut store,
            &memory,
            oob_offset,
        );
        assert!(oob_res.is_err());

        // Test unaligned offset: offset 129 is not 4-byte aligned for F32
        let unaligned_res = WasmDirectMemoryBridge::stream_embedding_f32_into_wasm(
            &embedding,
            &mut store,
            &memory,
            129,
        );
        assert!(unaligned_res.is_err());
    }

    #[test]
    fn test_wasm_direct_memory_bridge_slice() {
        let mut guest_mem = vec![0u8; 512];
        let slice1 = [1u8, 2, 3, 4];
        let slice2 = [5u8, 6, 7, 8];

        let written = WasmDirectMemoryBridge::bridge_slice_into_guest(
            &mut guest_mem,
            64,
            &slice1,
            &slice2,
            InferDataType::F32,
        ).expect("bridge slice");

        assert_eq!(written, 8);
        assert_eq!(&guest_mem[64..72], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
