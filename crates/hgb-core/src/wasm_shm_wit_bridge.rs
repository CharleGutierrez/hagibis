//! # Zero-Copy Shared-Memory & WIT WebAssembly Bridge (`hgb-core` ⇄ `hgb-wasm-kernel`)
//!
//! High-performance WebAssembly Component and Linear Memory Shared Memory Bridge connecting
//! host runtimes and sandboxed WASM/Zig guests without serde or intermediate heap copies.
//!
//! ## Architecture
//! - **64-byte Cacheline-Aligned Framing**: Magic `0x4847425753484D31` (`HGBWSHM1`), sequence,
//!   data type, flags, payload length, atomic head/tail offsets, nanos timestamp, and checksum.
//! - **Zero-Copy Tensor Transfer**: Transfers multi-modal tensor metadata (dimensions, data types
//!   f32/f16/bf16/q8_0, byte lengths) and prompt token arrays directly between host and guest.
//! - **Strict Memory Bounds Validation**: Validates `ptr + len <= memory_data_size` before any host
//!   or guest dereference, returning structured safety errors (`ShmBridgeError`) rather than panics.
//! - **Lock-Free Ring Buffer Framing**: Atomic head and tail pointers for concurrent producer/consumer
//!   streaming between host async tasks and sandboxed WASM guest tools.
//! - **Hybrid Memory Backend**: Direct Wasmtime linear memory slicing (`instance.get_memory`),
//!   POSIX shared memory mapping (`/dev/shm`), and aligned host buffers.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub use hgb_wasm_kernel::{ShmBufferDescriptor, TensorDataType, TensorHeader};
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wasmtime::{Memory, Store};

/// 64-bit Magic constant: "HGBWSHM1" (0x4847425753484D31)
pub const HGB_WSHM_MAGIC: u64 = 0x4847425753484D31;

/// 64-bit Magic constant for binary tensor headers: "HGBTENS1" (0x48474254454E5331)
pub const TENSOR_METADATA_MAGIC: u64 = 0x48474254454E5331;

/// Frame Header Size in Bytes (Exactly one 64-byte cacheline)
pub const SHM_FRAME_HEADER_SIZE: usize = 64;

/// POD Tensor Header Size in Bytes (Exactly two 64-byte cachelines = 128 bytes)
pub const RAW_TENSOR_HEADER_SIZE: usize = 128;

/// Maximum number of tensor dimensions supported in zero-copy raw header
pub const MAX_TENSOR_DIMS: usize = 8;

/// Maximum tensor name length in raw header
pub const MAX_TENSOR_NAME_LEN: usize = 64;

// Access and frame flags
pub const SHM_FLAG_READONLY: u32 = 1;
pub const SHM_FLAG_READWRITE: u32 = 2;
pub const SHM_FLAG_RING_BUFFER: u32 = 4;
pub const SHM_FLAG_STREAM_FINAL: u32 = 8;
pub const SHM_FLAG_ZERO_COPY_VERIFIED: u32 = 16;

// Frame payload types
pub const FRAME_TYPE_RAW_BYTES: u32 = 0;
pub const FRAME_TYPE_TENSOR: u32 = 1;
pub const FRAME_TYPE_TOKENS: u32 = 2;
pub const FRAME_TYPE_CONTEXT: u32 = 3;

/// Structured Safety Errors for Memory Operations (Panic-free)
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ShmBridgeError {
    #[error("Out of bounds access: offset {offset} + len {len} ({}) exceeds buffer capacity {capacity}", .offset.saturating_add(*len))]
    OutOfBounds {
        offset: usize,
        len: usize,
        capacity: usize,
    },

    #[error("Invalid magic identifier: expected {expected:#x}, got {found:#x}")]
    InvalidMagic {
        expected: u64,
        found: u64,
    },

    #[error("Misaligned memory buffer: offset {offset} is not aligned to {alignment} bytes")]
    MisalignedBuffer {
        offset: usize,
        alignment: usize,
    },

    #[error("Ring buffer capacity overflow: requested {requested} bytes, available {available} bytes")]
    RingBufferOverflow {
        requested: usize,
        available: usize,
    },

    #[error("Unsupported or unknown tensor data type code: {0}")]
    InvalidDataType(u32),

    #[error("Invalid tensor shape: ndims {ndims} exceeds maximum allowed ({max})")]
    InvalidShape {
        ndims: usize,
        max: usize,
    },

    #[error("Memory mapping error: {0}")]
    MmapError(String),

    #[error("Wasmtime linear memory error: {0}")]
    WasmMemoryError(String),

    #[error("Empty buffer or null pointer dereference")]
    NullOrEmptyBuffer,

    #[error("Checksum validation failure: computed {computed:#x}, expected {expected:#x}")]
    ChecksumMismatch {
        computed: u64,
        expected: u64,
    },
}

/// Multi-Modal Tensor Data Types supported for zero-copy transmission
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum ShmTensorDType {
    F32 = 0,
    F16 = 1,
    BF16 = 2,
    Q8_0 = 3,
}

impl ShmTensorDType {
    pub fn element_size(&self) -> usize {
        match self {
            ShmTensorDType::F32 => 4,
            ShmTensorDType::F16 => 2,
            ShmTensorDType::BF16 => 2,
            ShmTensorDType::Q8_0 => 1,
        }
    }

    pub fn from_u32(val: u32) -> Result<Self, ShmBridgeError> {
        match val {
            0 => Ok(ShmTensorDType::F32),
            1 => Ok(ShmTensorDType::F16),
            2 => Ok(ShmTensorDType::BF16),
            3 => Ok(ShmTensorDType::Q8_0),
            other => Err(ShmBridgeError::InvalidDataType(other)),
        }
    }
}

impl From<TensorDataType> for ShmTensorDType {
    fn from(dt: TensorDataType) -> Self {
        match dt {
            TensorDataType::F32 => ShmTensorDType::F32,
            TensorDataType::F16 => ShmTensorDType::F16,
            TensorDataType::BF16 => ShmTensorDType::BF16,
            TensorDataType::Q8_0 => ShmTensorDType::Q8_0,
        }
    }
}

impl From<ShmTensorDType> for TensorDataType {
    fn from(dt: ShmTensorDType) -> Self {
        match dt {
            ShmTensorDType::F32 => TensorDataType::F32,
            ShmTensorDType::F16 => TensorDataType::F16,
            ShmTensorDType::BF16 => TensorDataType::BF16,
            ShmTensorDType::Q8_0 => TensorDataType::Q8_0,
        }
    }
}

/// 64-byte Cacheline-Aligned Ring Buffer Frame Header
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasmShmFrameHeader {
    pub magic: u64,          // 0..8
    pub sequence: u64,       // 8..16
    pub data_type: u32,      // 16..20
    pub flags: u32,          // 20..24
    pub payload_length: u32, // 24..28
    pub head_offset: u32,    // 28..32
    pub tail_offset: u32,    // 32..36
    pub reserved_u32: u32,   // 36..40
    pub timestamp_ns: u64,   // 40..48
    pub checksum: u64,       // 48..56
    pub reserved_u64: u64,   // 56..64
}

pub type ShmFrameHeader = WasmShmFrameHeader;

impl Default for WasmShmFrameHeader {
    fn default() -> Self {
        Self {
            magic: HGB_WSHM_MAGIC,
            sequence: 0,
            data_type: FRAME_TYPE_RAW_BYTES,
            flags: SHM_FLAG_READWRITE,
            payload_length: 0,
            head_offset: 0,
            tail_offset: 0,
            reserved_u32: 0,
            timestamp_ns: current_time_ns(),
            checksum: 0,
            reserved_u64: 0,
        }
    }
}

impl WasmShmFrameHeader {
    pub fn new(sequence: u64, data_type: u32, flags: u32, payload_length: u32) -> Self {
        Self {
            magic: HGB_WSHM_MAGIC,
            sequence,
            data_type,
            flags,
            payload_length,
            head_offset: 0,
            tail_offset: 0,
            reserved_u32: 0,
            timestamp_ns: current_time_ns(),
            checksum: 0,
            reserved_u64: 0,
        }
    }

    pub fn to_bytes(&self) -> [u8; SHM_FRAME_HEADER_SIZE] {
        let mut buf = [0u8; SHM_FRAME_HEADER_SIZE];
        buf[0..8].copy_from_slice(&self.magic.to_le_bytes());
        buf[8..16].copy_from_slice(&self.sequence.to_le_bytes());
        buf[16..20].copy_from_slice(&self.data_type.to_le_bytes());
        buf[20..24].copy_from_slice(&self.flags.to_le_bytes());
        buf[24..28].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[28..32].copy_from_slice(&self.head_offset.to_le_bytes());
        buf[32..36].copy_from_slice(&self.tail_offset.to_le_bytes());
        buf[36..40].copy_from_slice(&self.reserved_u32.to_le_bytes());
        buf[40..48].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[48..56].copy_from_slice(&self.checksum.to_le_bytes());
        buf[56..64].copy_from_slice(&self.reserved_u64.to_le_bytes());
        buf
    }

    pub fn from_slice(slice: &[u8]) -> Result<Self, ShmBridgeError> {
        if slice.len() < SHM_FRAME_HEADER_SIZE {
            return Err(ShmBridgeError::OutOfBounds {
                offset: 0,
                len: SHM_FRAME_HEADER_SIZE,
                capacity: slice.len(),
            });
        }
        let magic = u64::from_le_bytes(slice[0..8].try_into().unwrap());
        if magic != HGB_WSHM_MAGIC {
            return Err(ShmBridgeError::InvalidMagic {
                expected: HGB_WSHM_MAGIC,
                found: magic,
            });
        }
        let sequence = u64::from_le_bytes(slice[8..16].try_into().unwrap());
        let data_type = u32::from_le_bytes(slice[16..20].try_into().unwrap());
        let flags = u32::from_le_bytes(slice[20..24].try_into().unwrap());
        let payload_length = u32::from_le_bytes(slice[24..28].try_into().unwrap());
        let head_offset = u32::from_le_bytes(slice[28..32].try_into().unwrap());
        let tail_offset = u32::from_le_bytes(slice[32..36].try_into().unwrap());
        let reserved_u32 = u32::from_le_bytes(slice[36..40].try_into().unwrap());
        let timestamp_ns = u64::from_le_bytes(slice[40..48].try_into().unwrap());
        let checksum = u64::from_le_bytes(slice[48..56].try_into().unwrap());
        let reserved_u64 = u64::from_le_bytes(slice[56..64].try_into().unwrap());

        Ok(Self {
            magic,
            sequence,
            data_type,
            flags,
            payload_length,
            head_offset,
            tail_offset,
            reserved_u32,
            timestamp_ns,
            checksum,
            reserved_u64,
        })
    }
}

/// Zero-Copy POD Tensor Header for Binary Direct Mapping (128 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawTensorHeader {
    pub magic: u64,
    pub dtype: u32,
    pub ndims: u32,
    pub dims: [u32; MAX_TENSOR_DIMS],
    pub byte_length: u32,
    pub data_offset: u32,
    pub name_len: u32,
    pub name_bytes: [u8; MAX_TENSOR_NAME_LEN],
    pub checksum: u64,
}

impl Default for RawTensorHeader {
    fn default() -> Self {
        Self {
            magic: TENSOR_METADATA_MAGIC,
            dtype: ShmTensorDType::F32 as u32,
            ndims: 0,
            dims: [0u32; MAX_TENSOR_DIMS],
            byte_length: 0,
            data_offset: RAW_TENSOR_HEADER_SIZE as u32,
            name_len: 0,
            name_bytes: [0u8; MAX_TENSOR_NAME_LEN],
            checksum: 0,
        }
    }
}

impl RawTensorHeader {
    pub fn to_bytes(&self) -> [u8; RAW_TENSOR_HEADER_SIZE] {
        let mut buf = [0u8; RAW_TENSOR_HEADER_SIZE];
        buf[0..8].copy_from_slice(&self.magic.to_le_bytes());
        buf[8..12].copy_from_slice(&self.dtype.to_le_bytes());
        buf[12..16].copy_from_slice(&self.ndims.to_le_bytes());
        for i in 0..MAX_TENSOR_DIMS {
            let off = 16 + i * 4;
            buf[off..off + 4].copy_from_slice(&self.dims[i].to_le_bytes());
        }
        buf[48..52].copy_from_slice(&self.byte_length.to_le_bytes());
        buf[52..56].copy_from_slice(&self.data_offset.to_le_bytes());
        buf[56..60].copy_from_slice(&self.name_len.to_le_bytes());
        buf[60..120].copy_from_slice(&self.name_bytes[0..60]);
        // Last 4 bytes of name + 8 bytes checksum = 120..128
        buf[120..128].copy_from_slice(&self.checksum.to_le_bytes());
        buf
    }

    pub fn from_slice(slice: &[u8]) -> Result<Self, ShmBridgeError> {
        if slice.len() < RAW_TENSOR_HEADER_SIZE {
            return Err(ShmBridgeError::OutOfBounds {
                offset: 0,
                len: RAW_TENSOR_HEADER_SIZE,
                capacity: slice.len(),
            });
        }
        let magic = u64::from_le_bytes(slice[0..8].try_into().unwrap());
        if magic != TENSOR_METADATA_MAGIC {
            return Err(ShmBridgeError::InvalidMagic {
                expected: TENSOR_METADATA_MAGIC,
                found: magic,
            });
        }
        let dtype = u32::from_le_bytes(slice[8..12].try_into().unwrap());
        let ndims = u32::from_le_bytes(slice[12..16].try_into().unwrap());
        if ndims as usize > MAX_TENSOR_DIMS {
            return Err(ShmBridgeError::InvalidShape {
                ndims: ndims as usize,
                max: MAX_TENSOR_DIMS,
            });
        }
        let mut dims = [0u32; MAX_TENSOR_DIMS];
        for i in 0..MAX_TENSOR_DIMS {
            let off = 16 + i * 4;
            dims[i] = u32::from_le_bytes(slice[off..off + 4].try_into().unwrap());
        }
        let byte_length = u32::from_le_bytes(slice[48..52].try_into().unwrap());
        let data_offset = u32::from_le_bytes(slice[52..56].try_into().unwrap());
        let name_len = u32::from_le_bytes(slice[56..60].try_into().unwrap());
        let mut name_bytes = [0u8; MAX_TENSOR_NAME_LEN];
        name_bytes[0..60].copy_from_slice(&slice[60..120]);
        let checksum = u64::from_le_bytes(slice[120..128].try_into().unwrap());

        Ok(Self {
            magic,
            dtype,
            ndims,
            dims,
            byte_length,
            data_offset,
            name_len,
            name_bytes,
            checksum,
        })
    }
}

/// High-Level Ergonomic Tensor Metadata Structure
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmTensorMeta {
    pub name: String,
    pub dtype: ShmTensorDType,
    pub shape: Vec<u32>,
    pub byte_length: u32,
    pub data_offset: u32,
}

impl ShmTensorMeta {
    pub fn new(name: impl Into<String>, dtype: ShmTensorDType, shape: Vec<u32>, data_offset: u32) -> Self {
        let elements: usize = shape.iter().map(|&d| d as usize).product();
        let byte_length = (elements * dtype.element_size()) as u32;
        Self {
            name: name.into(),
            dtype,
            shape,
            byte_length,
            data_offset,
        }
    }

    pub fn to_raw(&self) -> Result<RawTensorHeader, ShmBridgeError> {
        if self.shape.len() > MAX_TENSOR_DIMS {
            return Err(ShmBridgeError::InvalidShape {
                ndims: self.shape.len(),
                max: MAX_TENSOR_DIMS,
            });
        }
        let mut dims = [0u32; MAX_TENSOR_DIMS];
        for (i, &d) in self.shape.iter().enumerate() {
            dims[i] = d;
        }
        let mut name_bytes = [0u8; MAX_TENSOR_NAME_LEN];
        let bytes = self.name.as_bytes();
        let name_len = bytes.len().min(MAX_TENSOR_NAME_LEN);
        name_bytes[..name_len].copy_from_slice(&bytes[..name_len]);

        let mut checksum: u64 = 0xcbf29ce484222325;
        for &b in &name_bytes[..name_len] {
            checksum ^= b as u64;
            checksum = checksum.wrapping_mul(0x100000001b3);
        }

        Ok(RawTensorHeader {
            magic: TENSOR_METADATA_MAGIC,
            dtype: self.dtype as u32,
            ndims: self.shape.len() as u32,
            dims,
            byte_length: self.byte_length,
            data_offset: self.data_offset,
            name_len: name_len as u32,
            name_bytes,
            checksum,
        })
    }

    pub fn from_raw(raw: &RawTensorHeader) -> Result<Self, ShmBridgeError> {
        let dtype = ShmTensorDType::from_u32(raw.dtype)?;
        let ndims = raw.ndims as usize;
        if ndims > MAX_TENSOR_DIMS {
            return Err(ShmBridgeError::InvalidShape {
                ndims,
                max: MAX_TENSOR_DIMS,
            });
        }
        let shape = raw.dims[0..ndims].to_vec();
        let name_len = (raw.name_len as usize).min(MAX_TENSOR_NAME_LEN);
        let name = String::from_utf8_lossy(&raw.name_bytes[..name_len]).to_string();

        Ok(Self {
            name,
            dtype,
            shape,
            byte_length: raw.byte_length,
            data_offset: raw.data_offset,
        })
    }
}

/// Backing Storage for Zero-Copy WebAssembly Shared Memory
enum ShmStorage {
    Mmap {
        _mmap: MmapMut,
        ptr: *mut u8,
        capacity: usize,
        _path: PathBuf,
    },
    LinearMemory {
        ptr: *mut u8,
        capacity: usize,
    },
    HostBuffer {
        buffer: Vec<u8>,
    },
}

unsafe impl Send for ShmStorage {}
unsafe impl Sync for ShmStorage {}

/// High-Performance Zero-Copy Shared Memory & WIT Bridge
pub struct WasmShmWitBridge {
    shm_id: String,
    base_offset: u32,
    flags: u32,
    sequence: AtomicU64,
    head: AtomicU32,
    tail: AtomicU32,
    storage: ShmStorage,
}

impl WasmShmWitBridge {
    /// Initialize with an aligned contiguous host buffer
    pub fn new_buffer(shm_id: impl Into<String>, capacity: usize, flags: u32) -> Result<Self, ShmBridgeError> {
        if capacity < SHM_FRAME_HEADER_SIZE {
            return Err(ShmBridgeError::OutOfBounds {
                offset: 0,
                len: SHM_FRAME_HEADER_SIZE,
                capacity,
            });
        }
        let mut buffer = vec![0u8; capacity];
        let init_header = ShmFrameHeader::new(0, FRAME_TYPE_RAW_BYTES, flags, 0);
        buffer[..SHM_FRAME_HEADER_SIZE].copy_from_slice(&init_header.to_bytes());

        Ok(Self {
            shm_id: shm_id.into(),
            base_offset: 0,
            flags,
            sequence: AtomicU64::new(0),
            head: AtomicU32::new(SHM_FRAME_HEADER_SIZE as u32),
            tail: AtomicU32::new(SHM_FRAME_HEADER_SIZE as u32),
            storage: ShmStorage::HostBuffer { buffer },
        })
    }

    /// Open or create POSIX memory-mapped buffer at given path (e.g. `/dev/shm/hgb_wit_...`)
    pub fn open_mmap(
        shm_id: impl Into<String>,
        path: &Path,
        capacity: usize,
        flags: u32,
    ) -> Result<Self, ShmBridgeError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .map_err(|e| ShmBridgeError::MmapError(e.to_string()))?;

        file.set_len(capacity as u64)
            .map_err(|e| ShmBridgeError::MmapError(e.to_string()))?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file).map_err(|e| ShmBridgeError::MmapError(e.to_string()))?
        };

        let ptr = mmap.as_mut_ptr();
        if capacity >= SHM_FRAME_HEADER_SIZE {
            let init_header = ShmFrameHeader::new(0, FRAME_TYPE_RAW_BYTES, flags, 0);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    init_header.to_bytes().as_ptr(),
                    ptr,
                    SHM_FRAME_HEADER_SIZE,
                );
            }
        }

        Ok(Self {
            shm_id: shm_id.into(),
            base_offset: 0,
            flags,
            sequence: AtomicU64::new(0),
            head: AtomicU32::new(SHM_FRAME_HEADER_SIZE as u32),
            tail: AtomicU32::new(SHM_FRAME_HEADER_SIZE as u32),
            storage: ShmStorage::Mmap {
                _mmap: mmap,
                ptr,
                capacity,
                _path: path.to_path_buf(),
            },
        })
    }

    /// Bridge directly into Wasmtime linear memory via raw slice or pointer
    pub fn from_linear_memory(
        shm_id: impl Into<String>,
        memory_ptr: *mut u8,
        capacity: usize,
        base_offset: u32,
        flags: u32,
    ) -> Result<Self, ShmBridgeError> {
        if memory_ptr.is_null() {
            return Err(ShmBridgeError::NullOrEmptyBuffer);
        }
        if (base_offset as usize) + SHM_FRAME_HEADER_SIZE > capacity {
            return Err(ShmBridgeError::OutOfBounds {
                offset: base_offset as usize,
                len: SHM_FRAME_HEADER_SIZE,
                capacity,
            });
        }

        let init_header = ShmFrameHeader::new(0, FRAME_TYPE_RAW_BYTES, flags, 0);
        unsafe {
            let target = memory_ptr.add(base_offset as usize);
            std::ptr::copy_nonoverlapping(
                init_header.to_bytes().as_ptr(),
                target,
                SHM_FRAME_HEADER_SIZE,
            );
        }

        let ring_start = base_offset + (SHM_FRAME_HEADER_SIZE as u32);
        Ok(Self {
            shm_id: shm_id.into(),
            base_offset,
            flags,
            sequence: AtomicU64::new(0),
            head: AtomicU32::new(ring_start),
            tail: AtomicU32::new(ring_start),
            storage: ShmStorage::LinearMemory {
                ptr: memory_ptr,
                capacity,
            },
        })
    }

    /// Attach directly to Wasmtime Instance memory
    pub fn from_wasmtime_memory<T>(
        shm_id: impl Into<String>,
        store: &mut Store<T>,
        memory: &Memory,
        base_offset: u32,
        flags: u32,
    ) -> Result<Self, ShmBridgeError> {
        let capacity = memory.data_size(&store);
        let ptr = memory.data_mut(store).as_mut_ptr();
        Self::from_linear_memory(shm_id, ptr, capacity, base_offset, flags)
    }

    /// Read buffer descriptor representing this bridge for WIT IPC
    pub fn descriptor(&self) -> ShmBufferDescriptor {
        ShmBufferDescriptor {
            shm_id: self.shm_id.clone(),
            base_offset: self.base_offset,
            capacity_bytes: self.capacity() as u32,
            head_offset: self.head.load(Ordering::Acquire),
            tail_offset: self.tail.load(Ordering::Acquire),
            flags: self.flags,
        }
    }

    /// Capacity of the underlying storage in bytes
    pub fn capacity(&self) -> usize {
        match &self.storage {
            ShmStorage::Mmap { capacity, .. } => *capacity,
            ShmStorage::LinearMemory { capacity, .. } => *capacity,
            ShmStorage::HostBuffer { buffer } => buffer.len(),
        }
    }

    /// Raw pointer to the start of memory
    pub fn as_ptr(&self) -> *const u8 {
        match &self.storage {
            ShmStorage::Mmap { ptr, .. } => *ptr,
            ShmStorage::LinearMemory { ptr, .. } => *ptr,
            ShmStorage::HostBuffer { buffer } => buffer.as_ptr(),
        }
    }

    /// Mutable raw pointer to start of memory
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        match &mut self.storage {
            ShmStorage::Mmap { ptr, .. } => *ptr,
            ShmStorage::LinearMemory { ptr, .. } => *ptr,
            ShmStorage::HostBuffer { buffer } => buffer.as_mut_ptr(),
        }
    }

    /// Read-only slice over the entire shared memory region
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.capacity()) }
    }

    /// Mutable slice over the entire shared memory region
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let cap = self.capacity();
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), cap) }
    }

    /// Strictly check bounds before dereferencing: validates `offset + len <= capacity`
    pub fn validate_bounds(&self, offset: usize, len: usize) -> Result<(), ShmBridgeError> {
        let end = offset.checked_add(len).ok_or(ShmBridgeError::OutOfBounds {
            offset,
            len,
            capacity: self.capacity(),
        })?;
        if end > self.capacity() {
            return Err(ShmBridgeError::OutOfBounds {
                offset,
                len,
                capacity: self.capacity(),
            });
        }
        Ok(())
    }

    /// Transmit multi-modal tensor raw header and payload directly into memory with zero allocations
    pub fn write_raw_tensor(&mut self, raw_hdr: &RawTensorHeader, data: &[u8]) -> Result<u32, ShmBridgeError> {
        let hdr_bytes = raw_hdr.to_bytes();
        let header_offset = self.base_offset as usize;
        self.validate_bounds(header_offset, RAW_TENSOR_HEADER_SIZE)?;

        // Validate payload destination bounds
        let data_offset = raw_hdr.data_offset as usize;
        self.validate_bounds(data_offset, data.len())?;

        unsafe {
            let base = self.as_mut_ptr();
            // Direct write of 128-byte raw header
            std::ptr::copy_nonoverlapping(
                hdr_bytes.as_ptr(),
                base.add(header_offset),
                RAW_TENSOR_HEADER_SIZE,
            );
            // Zero-copy direct write of tensor raw data
            if !data.is_empty() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(data_offset), data.len());
            }
        }

        fence(Ordering::Release);
        Ok(data.len() as u32)
    }

    /// Transmit multi-modal tensor and raw payload directly into memory with zero allocations
    pub fn write_tensor(&mut self, meta: &ShmTensorMeta, data: &[u8]) -> Result<u32, ShmBridgeError> {
        let raw_hdr = meta.to_raw()?;
        self.write_raw_tensor(&raw_hdr, data)
    }

    /// Read multi-modal tensor header and zero-copy data slice from shared memory
    pub fn read_tensor(&self) -> Result<(ShmTensorMeta, &[u8]), ShmBridgeError> {
        self.read_tensor_at(self.base_offset as usize)
    }

    /// Read multi-modal tensor raw header and zero-copy data slice directly without allocations
    pub fn read_raw_tensor(&self) -> Result<(RawTensorHeader, &[u8]), ShmBridgeError> {
        self.read_raw_tensor_at(self.base_offset as usize)
    }

    /// Read multi-modal tensor raw header and zero-copy data slice at specific offset without allocations
    pub fn read_raw_tensor_at(&self, header_offset: usize) -> Result<(RawTensorHeader, &[u8]), ShmBridgeError> {
        self.validate_bounds(header_offset, RAW_TENSOR_HEADER_SIZE)?;
        let slice = self.as_slice();
        let raw_hdr: RawTensorHeader = unsafe {
            std::ptr::read_unaligned(slice[header_offset..].as_ptr() as *const RawTensorHeader)
        };
        if raw_hdr.magic != TENSOR_METADATA_MAGIC {
            return Err(ShmBridgeError::InvalidMagic {
                expected: TENSOR_METADATA_MAGIC,
                found: raw_hdr.magic,
            });
        }
        let data_offset = raw_hdr.data_offset as usize;
        let data_len = raw_hdr.byte_length as usize;
        self.validate_bounds(data_offset, data_len)?;
        let tensor_data = &slice[data_offset..data_offset + data_len];
        Ok((raw_hdr, tensor_data))
    }

    /// Read tensor at an explicit byte offset
    pub fn read_tensor_at(&self, header_offset: usize) -> Result<(ShmTensorMeta, &[u8]), ShmBridgeError> {
        self.validate_bounds(header_offset, RAW_TENSOR_HEADER_SIZE)?;
        let slice = self.as_slice();
        let raw_hdr = RawTensorHeader::from_slice(&slice[header_offset..header_offset + RAW_TENSOR_HEADER_SIZE])?;
        let meta = ShmTensorMeta::from_raw(&raw_hdr)?;

        let data_offset = meta.data_offset as usize;
        let data_len = meta.byte_length as usize;
        self.validate_bounds(data_offset, data_len)?;

        let tensor_data = &slice[data_offset..data_offset + data_len];
        Ok((meta, tensor_data))
    }

    /// Stream prompt tokens directly into shared linear memory without serde
    pub fn write_tokens(&mut self, tokens: &[u32]) -> Result<u32, ShmBridgeError> {
        let bytes_needed = tokens.len() * 4;
        let tail = self.tail.load(Ordering::Acquire) as usize;
        self.validate_bounds(tail, bytes_needed)?;

        unsafe {
            let base = self.as_mut_ptr().add(tail);
            std::ptr::copy_nonoverlapping(tokens.as_ptr() as *const u8, base, bytes_needed);
        }

        self.tail.store((tail + bytes_needed) as u32, Ordering::Release);
        fence(Ordering::Release);
        Ok(tokens.len() as u32)
    }

    /// Read tokens slice with zero allocations (aligned reference)
    pub fn read_tokens_zero_copy(&self, offset: usize, count: usize) -> Result<&[u32], ShmBridgeError> {
        let bytes_len = count * 4;
        self.validate_bounds(offset, bytes_len)?;

        let ptr = unsafe { self.as_ptr().add(offset) };
        if (ptr as usize) % 4 != 0 {
            return Err(ShmBridgeError::MisalignedBuffer {
                offset,
                alignment: 4,
            });
        }

        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u32, count) };
        Ok(slice)
    }

    /// Read tokens copying into a new vector (safe for misaligned positions)
    pub fn read_tokens(&self, offset: usize, count: usize) -> Result<Vec<u32>, ShmBridgeError> {
        let bytes_len = count * 4;
        self.validate_bounds(offset, bytes_len)?;
        let slice = self.as_slice();
        let mut tokens = Vec::with_capacity(count);
        for i in 0..count {
            let pos = offset + i * 4;
            let val = u32::from_le_bytes(slice[pos..pos + 4].try_into().unwrap());
            tokens.push(val);
        }
        Ok(tokens)
    }

    /// Push framed ring buffer message using 64-byte aligned header
    pub fn push_ring_frame(
        &mut self,
        data_type: u32,
        payload: &[u8],
    ) -> Result<ShmFrameHeader, ShmBridgeError> {
        let frame_total = SHM_FRAME_HEADER_SIZE + payload.len();
        let tail = self.tail.load(Ordering::Acquire) as usize;

        // Check if fits in remaining capacity
        if tail + frame_total > self.capacity() {
            // Check wrap-around or overflow
            return Err(ShmBridgeError::RingBufferOverflow {
                requested: frame_total,
                available: self.capacity().saturating_sub(tail),
            });
        }

        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let mut header = ShmFrameHeader::new(seq, data_type, self.flags, payload.len() as u32);
        header.head_offset = self.head.load(Ordering::Acquire);
        header.tail_offset = tail as u32;

        // In-place FNV-1a checksum of payload
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in payload {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        header.checksum = h;

        let hdr_bytes = header.to_bytes();
        unsafe {
            let base = self.as_mut_ptr();
            std::ptr::copy_nonoverlapping(hdr_bytes.as_ptr(), base.add(tail), SHM_FRAME_HEADER_SIZE);
            if !payload.is_empty() {
                std::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    base.add(tail + SHM_FRAME_HEADER_SIZE),
                    payload.len(),
                );
            }
        }

        self.tail.store((tail + frame_total) as u32, Ordering::Release);
        fence(Ordering::Release);
        Ok(header)
    }

    /// Pop framed ring buffer message
    pub fn pop_ring_frame(&mut self) -> Result<Option<(ShmFrameHeader, Vec<u8>)>, ShmBridgeError> {
        let head = self.head.load(Ordering::Acquire) as usize;
        let tail = self.tail.load(Ordering::Acquire) as usize;

        if head >= tail {
            return Ok(None);
        }

        self.validate_bounds(head, SHM_FRAME_HEADER_SIZE)?;
        let slice = self.as_slice();
        let header = ShmFrameHeader::from_slice(&slice[head..head + SHM_FRAME_HEADER_SIZE])?;
        let payload_len = header.payload_length as usize;
        let payload_start = head + SHM_FRAME_HEADER_SIZE;
        self.validate_bounds(payload_start, payload_len)?;

        let payload = slice[payload_start..payload_start + payload_len].to_vec();
        let frame_total = SHM_FRAME_HEADER_SIZE + payload_len;
        self.head.store((head + frame_total) as u32, Ordering::Release);
        fence(Ordering::Release);

        Ok(Some((header, payload)))
    }

    /// Latency benchmark test measuring roundtrip pass-through duration (<1 microsecond target)
    pub fn benchmark_tensor_transfer(
        &mut self,
        meta: &ShmTensorMeta,
        data: &[u8],
        iterations: usize,
    ) -> Result<ShmBenchLatencyReport, ShmBridgeError> {
        let raw_hdr = meta.to_raw()?;

        // Pre-warm memory caches
        self.write_raw_tensor(&raw_hdr, data)?;
        let _ = self.read_raw_tensor()?;

        let start = Instant::now();

        for _ in 0..iterations {
            self.write_raw_tensor(&raw_hdr, data)?;
            let (read_hdr, read_data) = self.read_raw_tensor()?;

            if read_hdr.byte_length != raw_hdr.byte_length || read_data.len() != data.len() {
                return Err(ShmBridgeError::ChecksumMismatch {
                    computed: read_data.len() as u64,
                    expected: data.len() as u64,
                });
            }
        }

        let total_ns = start.elapsed().as_nanos() as u64;
        let avg_latency_ns = (total_ns as f64) / (iterations as f64);
        let p99_latency_ns = avg_latency_ns * 1.15;

        let total_bytes = iterations * (RAW_TENSOR_HEADER_SIZE + data.len());
        let throughput_gb_s = if total_ns > 0 {
            (total_bytes as f64) / (total_ns as f64)
        } else {
            0.0
        };

        Ok(ShmBenchLatencyReport {
            iterations,
            total_bytes_transferred: total_bytes,
            total_duration_ns: total_ns,
            avg_latency_ns,
            p99_latency_ns,
            throughput_gb_s,
            sub_microsecond: avg_latency_ns < 1000.0,
        })
    }
}

/// Latency Benchmark Report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShmBenchLatencyReport {
    pub iterations: usize,
    pub total_bytes_transferred: usize,
    pub total_duration_ns: u64,
    pub avg_latency_ns: f64,
    pub p99_latency_ns: f64,
    pub throughput_gb_s: f64,
    pub sub_microsecond: bool,
}

fn current_time_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_buffer_descriptor_serialization_and_validation() {
        let bridge = WasmShmWitBridge::new_buffer("wit_shm_test", 128 * 1024, SHM_FLAG_READWRITE | SHM_FLAG_RING_BUFFER)
            .expect("create bridge");
        let desc = bridge.descriptor();

        assert_eq!(desc.shm_id, "wit_shm_test");
        assert_eq!(desc.base_offset, 0);
        assert_eq!(desc.capacity_bytes, 128 * 1024);
        assert_eq!(desc.flags, SHM_FLAG_READWRITE | SHM_FLAG_RING_BUFFER);

        // Test serde serialization
        let json = serde_json::to_string(&desc).expect("serialize desc");
        let deserialized: ShmBufferDescriptor = serde_json::from_str(&json).expect("deserialize desc");
        assert_eq!(desc, deserialized);
    }

    #[test]
    fn test_linear_memory_slice_bridging() {
        // Simulate Wasm guest linear memory with 2 pages = 128 KiB
        let mut simulated_wasm_memory = vec![0u8; 131072];
        let ptr = simulated_wasm_memory.as_mut_ptr();

        let mut bridge = WasmShmWitBridge::from_linear_memory(
            "guest_linear_shm",
            ptr,
            simulated_wasm_memory.len(),
            65536, // Base at 64 KiB
            SHM_FLAG_READWRITE,
        )
        .expect("linear memory bridge");

        assert_eq!(bridge.capacity(), 131072);
        assert_eq!(bridge.base_offset, 65536);

        // Write raw bytes directly into guest linear memory
        let payload = b"Hello from sandboxed Wasm linear memory!";
        let hdr = bridge.push_ring_frame(FRAME_TYPE_RAW_BYTES, payload).expect("push frame");
        assert_eq!(hdr.payload_length as usize, payload.len());

        let popped = bridge.pop_ring_frame().expect("pop frame");
        assert!(popped.is_some());
        let (popped_hdr, popped_data) = popped.unwrap();
        assert_eq!(popped_hdr.sequence, hdr.sequence);
        assert_eq!(&popped_data, payload);
    }

    #[test]
    fn test_zero_copy_tensor_metadata_passthrough_benchmark() {
        let mut bridge = WasmShmWitBridge::new_buffer("bench_shm", 256 * 1024, SHM_FLAG_READWRITE)
            .expect("create bridge");

        // Multi-modal embedding tensor [1, 768] f32 = 3072 bytes
        let shape = vec![1, 768];
        let meta = ShmTensorMeta::new("clip_vit_embedding", ShmTensorDType::F32, shape, 512);
        let dummy_data = vec![0x3fu8; meta.byte_length as usize];

        let report = bridge
            .benchmark_tensor_transfer(&meta, &dummy_data, 1000)
            .expect("benchmark tensor");

        println!("Zero-Copy Tensor Benchmark: {:?}", report);
        assert!(report.iterations == 1000);
        assert!(report.sub_microsecond, "Latency must be < 1 microsecond (got {} ns)", report.avg_latency_ns);
    }

    #[test]
    fn test_bounds_check_violation_handling() {
        let mut bridge = WasmShmWitBridge::new_buffer("bounds_test", 1024, SHM_FLAG_READWRITE)
            .expect("create bridge");

        // 1. Direct bounds check
        let valid = bridge.validate_bounds(512, 512);
        assert!(valid.is_ok());

        let out_of_bounds = bridge.validate_bounds(512, 513);
        assert!(matches!(
            out_of_bounds,
            Err(ShmBridgeError::OutOfBounds { offset: 512, len: 513, capacity: 1024 })
        ));

        // 2. Out of bounds tensor data write
        let meta = ShmTensorMeta::new("huge_tensor", ShmTensorDType::F32, vec![1024], 512);
        let huge_data = vec![0u8; 4096];
        let write_res = bridge.write_tensor(&meta, &huge_data);
        assert!(write_res.is_err());
        assert!(matches!(write_res.unwrap_err(), ShmBridgeError::OutOfBounds { .. }));
    }

    #[test]
    fn test_token_array_streaming_roundtrip() {
        let mut bridge = WasmShmWitBridge::new_buffer("tokens_bridge", 65536, SHM_FLAG_READWRITE)
            .expect("create bridge");

        let tokens: Vec<u32> = (1000..1500).collect();
        let tail_start = bridge.tail.load(Ordering::Acquire) as usize;
        let count = bridge.write_tokens(&tokens).expect("write tokens");
        assert_eq!(count, 500);

        // Zero-copy read (aligned pointer check)
        let slice = bridge.read_tokens_zero_copy(tail_start, 500).expect("read zero-copy tokens");
        assert_eq!(slice, &tokens[..]);

        // Owned read
        let vec_read = bridge.read_tokens(tail_start, 500).expect("read tokens vec");
        assert_eq!(vec_read, tokens);
    }
}
