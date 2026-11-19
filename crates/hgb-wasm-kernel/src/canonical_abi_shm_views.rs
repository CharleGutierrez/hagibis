//! # Canonical ABI & Zero-Copy Shared Memory Views (`canonical_abi_shm_views`)
//!
//! Production-grade Canonical ABI zero-copy inter-component memory exchange for `hgb-wasm-kernel`.
//! Replaces JSON/bincode serialization overhead with 64-byte aligned linear memory views,
//! active borrow/lend lifecycle tracking, and zero-copy tensor routing for NPU acceleration.
//!
//! ## Architecture
//! - **`CanonicalAbiShmView` / `CanonicalAbiShmViewMut`**: Zero-copy typed memory views with
//!   strict slice bounds checking, power-of-two pointer alignment, and zero-cost transmutation.
//! - **`CanonicalAbiInvocationPayload`**: Fixed 64-byte aligned header layout bypassing heap serde
//!   copies, direct argument slice referencing, and in-place return buffer writing.
//! - **`CanonicalMemoryLender`**: Aliasing and race prevention lifecycle manager tracking active
//!   leases, preventing double-lending, and enforcing Rust-equivalent `&` / `&mut` borrowing rules.
//! - **`PinnedTensorDescriptor`**: Multi-dimensional tensor descriptors with 64-byte alignment
//!   for direct DMA NPU/GPU neural accelerator routing.
//! - **`LockFreeShmRingBuffer`**: Wait-free zero-copy producer-consumer ring buffer with
//!   64-byte cacheline-padded `AtomicU64` head/tail pointers, acquire-release memory ordering,
//!   and direct host/WASM linear memory page mapping.
//! - **`ShmRingDescriptor` & `ShmRingElementHeader`**: WIT flat-buffer binary layouts matching
//!   `hagibis:wasm-kernel/canonical-abi-types`.
//! - **`InteropBenchmarkReport` / `benchmark_interop_latency`**: Real-time cycle and nanosecond
//!   latency measurement comparing zero-copy shared memory views against serde JSON.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ============================================================================
// CONSTANTS & ACTION CODES
// ============================================================================

/// Magic signature for Canonical ABI Invocation Header: ASCII "HGBABI01" (0x4847424142493031)
pub const CANONICAL_ABI_HEADER_MAGIC: u64 = 0x4847424142493031;

/// Exact byte size of the 64-byte aligned Canonical ABI Invocation Header
pub const CANONICAL_HEADER_SIZE: usize = 64;

/// Required alignment for NPU DMA transfers, Canonical ABI headers, and Ring buffers
pub const CANONICAL_ALIGNMENT_DEFAULT: usize = 64;

/// Magic signature for Shared Memory Ring Buffer: ASCII "HGBSHMRG" (0x48474253484D5247)
pub const SHM_RING_MAGIC: u64 = 0x48474253484D5247;

/// Header control block size in bytes
pub const SHM_RING_CONTROL_HEADER_SIZE: usize = 64;

/// Fixed 24-byte element header size
pub const SHM_RING_ELEMENT_HEADER_SIZE: usize = 24;

/// Current Canonical ABI Protocol Version
pub const PROTOCOL_VERSION_1: u32 = 1;

/// Standard Action Codes for zero-copy dispatch
pub const ACTION_PING: u32 = 0x0000;
pub const ACTION_TOOL_INVOKE: u32 = 0x0001;
pub const ACTION_VECTOR_COMPUTE: u32 = 0x0002;
pub const ACTION_NPU_ROUTE: u32 = 0x0003;
pub const ACTION_MEMORY_LEND: u32 = 0x0004;
pub const ACTION_MEMORY_RECLAIM: u32 = 0x0005;

// ============================================================================
// ABI ERROR DEFINITIONS
// ============================================================================

/// Error conditions encountered during zero-copy Canonical ABI shared memory operations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbiError {
    /// Linear memory access requested outside buffer capacity
    OutOfBounds { offset: usize, len: usize, capacity: usize },
    /// Pointer or memory offset does not meet required alignment boundary
    AlignmentFault { ptr_or_offset: usize, required_alignment: usize },
    /// Memory lease requested overlaps with an active conflicting lease
    BorrowConflict { requested_offset: usize, requested_len: usize, conflict_view_id: u64, reason: String },
    /// Memory view was already lent or registered under the given ID
    DoubleLend { view_id: u64, offset: usize },
    /// Specified shared memory view ID was not found or was already reclaimed
    ViewNotFound { view_id: u64 },
    /// Invalid Canonical ABI magic bytes in payload header
    InvalidMagic { expected: u64, found: u64 },
    /// Return buffer capacity is insufficient for output payload
    BufferOverflow { needed: usize, capacity: usize },
    /// NPU tensor routing failed or tensor descriptor is invalid
    NpuRoutingFailed { reason: String },
    /// Guest execution trapped or was terminated by sandbox
    ExecutionTrapped { message: String },
    /// Tensor dimensions or stride parameters are invalid
    InvalidDimensions { reason: String },
    /// Typed slice transmutation type mismatch or byte alignment mismatch
    TypeMismatch { expected: &'static str, found: &'static str },
    /// Permission denied: capability token check failed or unauthorized memory mutation attempted
    PermissionDenied,
    /// Ring buffer is full (backpressure condition)
    RingFull { capacity: usize },
    /// Ring buffer is empty (no elements available to pop)
    RingEmpty,
    /// Ring buffer capacity is invalid (must be a non-zero power of two)
    InvalidCapacity { capacity: usize, reason: String },
}

/// Type alias aligning with Canonical ABI error naming conventions
pub type CanonicalAbiError = AbiError;

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { offset, len, capacity } => {
                write!(f, "Out-of-bounds linear memory access: range [{}..{}] exceeds capacity {}", offset, offset.saturating_add(*len), capacity)
            }
            Self::AlignmentFault { ptr_or_offset, required_alignment } => {
                write!(f, "Memory alignment fault: address/offset 0x{:x} is not aligned to {} bytes", ptr_or_offset, required_alignment)
            }
            Self::BorrowConflict { requested_offset, requested_len, conflict_view_id, reason } => {
                write!(f, "Borrow conflict on [{}..{}]: view {} {}", requested_offset, requested_offset.saturating_add(*requested_len), conflict_view_id, reason)
            }
            Self::DoubleLend { view_id, offset } => {
                write!(f, "Double-lend detected: view {} at offset 0x{:x} is already actively lent", view_id, offset)
            }
            Self::ViewNotFound { view_id } => {
                write!(f, "Shared memory view {} was not found or has been reclaimed", view_id)
            }
            Self::InvalidMagic { expected, found } => {
                write!(f, "Invalid Canonical ABI magic header: expected 0x{:016x}, found 0x{:016x}", expected, found)
            }
            Self::BufferOverflow { needed, capacity } => {
                write!(f, "Buffer overflow: payload requires {} bytes but return buffer only provides {} bytes", needed, capacity)
            }
            Self::NpuRoutingFailed { reason } => {
                write!(f, "NPU routing failed: {}", reason)
            }
            Self::ExecutionTrapped { message } => {
                write!(f, "Guest execution trapped: {}", message)
            }
            Self::InvalidDimensions { reason } => {
                write!(f, "Invalid tensor dimensions: {}", reason)
            }
            Self::TypeMismatch { expected, found } => {
                write!(f, "Type mismatch: expected {}, found {}", expected, found)
            }
            Self::PermissionDenied => {
                write!(f, "Permission denied: capability token check failed or unauthorized memory mutation attempted")
            }
            Self::RingFull { capacity } => {
                write!(f, "Lock-free SHM ring buffer is full: capacity {} reached (backpressure)", capacity)
            }
            Self::RingEmpty => {
                write!(f, "Lock-free SHM ring buffer is empty: no elements available to pop")
            }
            Self::InvalidCapacity { capacity, reason } => {
                write!(f, "Invalid ring buffer capacity {}: {}", capacity, reason)
            }
        }
    }
}

impl std::error::Error for AbiError {}

// ============================================================================
// POD COPY MARKER TRAIT FOR ZERO-COPY TRANSMUTATION
// ============================================================================

/// Marker trait representing Plain Old Data types safe for zero-copy memory transmutation.
pub unsafe trait PodCopy: Copy + 'static {}

unsafe impl PodCopy for u8 {}
unsafe impl PodCopy for u16 {}
unsafe impl PodCopy for u32 {}
unsafe impl PodCopy for u64 {}
unsafe impl PodCopy for i8 {}
unsafe impl PodCopy for i16 {}
unsafe impl PodCopy for i32 {}
unsafe impl PodCopy for i64 {}
unsafe impl PodCopy for f32 {}
unsafe impl PodCopy for f64 {}

// ============================================================================
// CACHELINE PADDING (64-BYTE ALIGNED ATOMIC WRAPPER)
// ============================================================================

/// 64-byte cacheline-padded wrapper to isolate atomic variables and prevent false sharing
#[repr(align(64))]
#[derive(Debug)]
pub struct CachePadded<T> {
    pub value: T,
}

impl<T> CachePadded<T> {
    pub const fn new(value: T) -> Self {
        Self { value }
    }
}

impl<T> std::ops::Deref for CachePadded<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> std::ops::DerefMut for CachePadded<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

const _: () = assert!(std::mem::align_of::<CachePadded<AtomicU64>>() == 64);
const _: () = assert!(std::mem::size_of::<CachePadded<AtomicU64>>() == 64);

// ============================================================================
// ENUMS & COMMON WIT-COMPLIANT DATA STRUCTURES
// ============================================================================

/// Access permissions for shared linear memory slices
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum MemoryAccessMode {
    /// Shared immutable view (multiple readers permitted)
    ReadOnly = 0,
    /// Shared mutable view (cooperative access)
    ReadWrite = 1,
    /// Exclusive mutable view (single writer, no other readers or writers)
    ExclusiveWrite = 2,
}

/// Primitive data types supported in zero-copy shared memory and tensor views
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum TensorDataType {
    F32 = 0,
    F16 = 1,
    BF16 = 2,
    I32 = 3,
    I16 = 4,
    I8 = 5,
    U32 = 6,
    U16 = 7,
    U8 = 8,
    Q8_0 = 9,
    Q4_0 = 10,
}

impl TensorDataType {
    /// Element size in bytes
    pub fn element_size(&self) -> usize {
        match self {
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::F16 | Self::BF16 | Self::I16 | Self::U16 => 2,
            Self::I8 | Self::U8 | Self::Q8_0 | Self::Q4_0 => 1,
        }
    }
}

/// Target hardware acceleration engine for pinned tensor routing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum NpuDeviceTarget {
    CpuSimd = 0,
    NpuDirect = 1,
    GpuCompute = 2,
    EphemeralNpu = 3,
}

/// Low-level slice into WebAssembly linear memory (matching WIT record)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalMemorySlice {
    pub ptr: u64,
    pub len: u64,
    pub alignment: u32,
    pub access_mode: MemoryAccessMode,
}

/// High-level typed shared memory descriptor for cross-component zero-copy exchange (matching WIT record)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedShmViewDescriptor {
    pub view_id: u64,
    pub slice: CanonicalMemorySlice,
    pub element_type: String,
    pub element_stride: u32,
    pub element_count: u64,
    pub version_epoch: u64,
    pub flags: u32,
}

// ============================================================================
// ZERO-COPY SHM RING BUFFER WIT FLAT-BUFFER DESCRIPTORS
// ============================================================================

/// Fixed 24-byte zero-copy ring element header matching WIT `shm-ring-element-header`
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmRingElementHeader {
    pub sequence_number: u64,
    pub payload_length: u32,
    pub flags: u32,
    pub timestamp_ns: u64,
}

unsafe impl PodCopy for ShmRingElementHeader {}

impl ShmRingElementHeader {
    pub const SIZE: usize = SHM_RING_ELEMENT_HEADER_SIZE;

    pub fn new(sequence_number: u64, payload_length: u32, flags: u32, timestamp_ns: u64) -> Self {
        Self {
            sequence_number,
            payload_length,
            flags,
            timestamp_ns,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.sequence_number.to_le_bytes());
        buf[8..12].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[12..16].copy_from_slice(&self.flags.to_le_bytes());
        buf[16..24].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf
    }

    pub fn from_bytes(slice: &[u8]) -> Result<Self, AbiError> {
        if slice.len() < Self::SIZE {
            return Err(AbiError::OutOfBounds {
                offset: 0,
                len: Self::SIZE,
                capacity: slice.len(),
            });
        }
        let sequence_number = u64::from_le_bytes(slice[0..8].try_into().unwrap());
        let payload_length = u32::from_le_bytes(slice[8..12].try_into().unwrap());
        let flags = u32::from_le_bytes(slice[12..16].try_into().unwrap());
        let timestamp_ns = u64::from_le_bytes(slice[16..24].try_into().unwrap());
        Ok(Self {
            sequence_number,
            payload_length,
            flags,
            timestamp_ns,
        })
    }
}

const _: () = assert!(std::mem::size_of::<ShmRingElementHeader>() == 24);
const _: () = assert!(std::mem::align_of::<ShmRingElementHeader>() == 8);

/// Zero-copy lock-free ring buffer descriptor matching WIT `shm-ring-buffer-descriptor`
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmRingDescriptor {
    pub view_id: u64,
    pub ring_capacity: u32,
    pub element_size: u32,
    pub head_offset: u32,
    pub tail_offset: u32,
    pub buffer_offset: u32,
    pub mask: u32,
}

impl ShmRingDescriptor {
    pub fn new(view_id: u64, ring_capacity: u32, element_size: u32) -> Result<Self, AbiError> {
        if ring_capacity == 0 || (ring_capacity & (ring_capacity - 1)) != 0 {
            return Err(AbiError::InvalidCapacity {
                capacity: ring_capacity as usize,
                reason: "Ring capacity must be a non-zero power of two".to_string(),
            });
        }
        if element_size == 0 {
            return Err(AbiError::TypeMismatch {
                expected: "non-zero element size",
                found: "zero element size",
            });
        }

        let head_offset = 64u32;
        let tail_offset = 128u32;
        let buffer_offset = 256u32;
        let mask = ring_capacity - 1;

        Ok(Self {
            view_id,
            ring_capacity,
            element_size,
            head_offset,
            tail_offset,
            buffer_offset,
            mask,
        })
    }

    /// 64-byte aligned slot stride in bytes
    #[inline(always)]
    pub fn slot_stride(&self) -> usize {
        let raw = ShmRingElementHeader::SIZE + (self.element_size as usize);
        (raw + 63) & !63
    }

    /// Total required linear memory buffer size in bytes
    pub fn required_buffer_size(&self) -> usize {
        (self.buffer_offset as usize) + (self.ring_capacity as usize) * self.slot_stride()
    }

    /// Byte offset into memory for a specific slot index
    #[inline(always)]
    pub fn slot_offset(&self, slot_idx: usize) -> usize {
        (self.buffer_offset as usize) + (slot_idx & (self.mask as usize)) * self.slot_stride()
    }
}

/// Zero-copy pop result matching WIT `shm-ring-pop-result`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmRingPopResult {
    pub header: ShmRingElementHeader,
    pub payload_slice: CanonicalMemorySlice,
    pub sequence_number: u64,
}

// ============================================================================
// LOCK-FREE SHM RING BUFFER CONDUIT
// ============================================================================

enum RingStorage {
    Owned(Vec<u8>),
    Mapped {
        ptr: *mut u8,
        len: usize,
    },
}

unsafe impl Send for RingStorage {}
unsafe impl Sync for RingStorage {}

/// Lock-free, zero-copy producer-consumer ring buffer conduit.
/// Maps host memory pages into WASM linear memory with acquire-release synchronization.
pub struct LockFreeShmRingBuffer {
    descriptor: ShmRingDescriptor,
    head: CachePadded<AtomicU64>,
    tail: CachePadded<AtomicU64>,
    storage: RingStorage,
    base_ptr: *mut u8,
}

unsafe impl Send for LockFreeShmRingBuffer {}
unsafe impl Sync for LockFreeShmRingBuffer {}

impl LockFreeShmRingBuffer {
    /// Initialize a new lock-free ring buffer with owned backing memory
    pub fn new(capacity: u32, element_size: u32, view_id: u64) -> Result<Self, AbiError> {
        let descriptor = ShmRingDescriptor::new(view_id, capacity, element_size)?;
        let total_size = descriptor.required_buffer_size();

        // Ensure 64-byte alignment
        let aligned_alloc_size = total_size + 64;
        let mut buffer = vec![0u8; aligned_alloc_size];

        let raw_addr = buffer.as_mut_ptr() as usize;
        let align_offset = if raw_addr % 64 == 0 {
            0
        } else {
            64 - (raw_addr % 64)
        };

        let base_ptr = unsafe { buffer.as_mut_ptr().add(align_offset) };

        let ring = Self {
            descriptor,
            head: CachePadded::new(AtomicU64::new(0)),
            tail: CachePadded::new(AtomicU64::new(0)),
            storage: RingStorage::Owned(buffer),
            base_ptr,
        };

        unsafe {
            ring.init_control_block();
        }

        Ok(ring)
    }

    /// Map a lock-free ring buffer directly over an existing WASM linear memory slice or host memory pages
    pub fn from_linear_memory(
        memory: &mut [u8],
        offset: usize,
        capacity: u32,
        element_size: u32,
        view_id: u64,
    ) -> Result<Self, AbiError> {
        verify_alignment(offset, CANONICAL_ALIGNMENT_DEFAULT)?;
        let descriptor = ShmRingDescriptor::new(view_id, capacity, element_size)?;
        let required = descriptor.required_buffer_size();

        let end = offset.checked_add(required).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len: required,
            capacity: memory.len(),
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset,
                len: required,
                capacity: memory.len(),
            });
        }

        let base_ptr = unsafe { memory.as_mut_ptr().add(offset) };
        verify_alignment(base_ptr as usize, CANONICAL_ALIGNMENT_DEFAULT)?;

        let ring = Self {
            descriptor,
            head: CachePadded::new(AtomicU64::new(0)),
            tail: CachePadded::new(AtomicU64::new(0)),
            storage: RingStorage::Mapped {
                ptr: base_ptr,
                len: required,
            },
            base_ptr,
        };

        unsafe {
            ring.init_control_block();
        }

        Ok(ring)
    }

    /// Format control block at base_ptr
    unsafe fn init_control_block(&self) {
        let ptr = self.base_ptr;
        // 0..8: Magic
        std::ptr::copy_nonoverlapping(&SHM_RING_MAGIC.to_le_bytes()[0], ptr, 8);
        // 8..12: Capacity
        std::ptr::copy_nonoverlapping(&self.descriptor.ring_capacity.to_le_bytes()[0], ptr.add(8), 4);
        // 12..16: Element size
        std::ptr::copy_nonoverlapping(&self.descriptor.element_size.to_le_bytes()[0], ptr.add(12), 4);
        // 16..20: Mask
        std::ptr::copy_nonoverlapping(&self.descriptor.mask.to_le_bytes()[0], ptr.add(16), 4);
        // 20..24: Buffer offset
        std::ptr::copy_nonoverlapping(&self.descriptor.buffer_offset.to_le_bytes()[0], ptr.add(20), 4);
        // 24..32: View ID
        std::ptr::copy_nonoverlapping(&self.descriptor.view_id.to_le_bytes()[0], ptr.add(24), 8);
        // 64..72: Head pointer
        std::ptr::copy_nonoverlapping(&0u64.to_le_bytes()[0], ptr.add(self.descriptor.head_offset as usize), 8);
        // 128..136: Tail pointer
        std::ptr::copy_nonoverlapping(&0u64.to_le_bytes()[0], ptr.add(self.descriptor.tail_offset as usize), 8);
    }

    unsafe fn write_slot(&self, slot_offset: usize, header: &ShmRingElementHeader, payload: &[u8]) {
        let header_ptr = self.base_ptr.add(slot_offset);
        let header_bytes = header.to_bytes();
        std::ptr::copy_nonoverlapping(header_bytes.as_ptr(), header_ptr, ShmRingElementHeader::SIZE);

        let payload_ptr = header_ptr.add(ShmRingElementHeader::SIZE);
        std::ptr::copy_nonoverlapping(payload.as_ptr(), payload_ptr, payload.len());
    }

    unsafe fn read_slot(&self, slot_offset: usize) -> Result<(ShmRingElementHeader, Vec<u8>), AbiError> {
        let header_ptr = self.base_ptr.add(slot_offset);
        let mut header_buf = [0u8; ShmRingElementHeader::SIZE];
        std::ptr::copy_nonoverlapping(header_ptr, header_buf.as_mut_ptr(), ShmRingElementHeader::SIZE);
        let header = ShmRingElementHeader::from_bytes(&header_buf)?;

        let payload_len = header.payload_length as usize;
        if payload_len > self.descriptor.element_size as usize {
            return Err(AbiError::BufferOverflow {
                needed: payload_len,
                capacity: self.descriptor.element_size as usize,
            });
        }

        let payload_ptr = header_ptr.add(ShmRingElementHeader::SIZE);
        let mut payload = vec![0u8; payload_len];
        std::ptr::copy_nonoverlapping(payload_ptr, payload.as_mut_ptr(), payload_len);

        Ok((header, payload))
    }

    #[inline(always)]
    fn sync_head_to_memory(&self, head: u64) {
        unsafe {
            let ptr = self.base_ptr.add(self.descriptor.head_offset as usize) as *mut u64;
            std::ptr::write_volatile(ptr, head);
        }
    }

    #[inline(always)]
    fn sync_tail_to_memory(&self, tail: u64) {
        unsafe {
            let ptr = self.base_ptr.add(self.descriptor.tail_offset as usize) as *mut u64;
            std::ptr::write_volatile(ptr, tail);
        }
    }

    /// Enqueue an element into the ring buffer with acquire/release ordering.
    pub fn try_push(&self, mut header: ShmRingElementHeader, payload: &[u8]) -> Result<u64, AbiError> {
        if payload.len() > self.descriptor.element_size as usize {
            return Err(AbiError::BufferOverflow {
                needed: payload.len(),
                capacity: self.descriptor.element_size as usize,
            });
        }

        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        // Check if ring is full: (head - tail) >= capacity
        if head.wrapping_sub(tail) >= self.descriptor.ring_capacity as u64 {
            return Err(AbiError::RingFull {
                capacity: self.descriptor.ring_capacity as usize,
            });
        }

        let slot_idx = (head & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);

        header.sequence_number = head;
        header.payload_length = payload.len() as u32;

        unsafe {
            self.write_slot(slot_offset, &header, payload);
        }

        let next_head = head.wrapping_add(1);
        self.head.store(next_head, Ordering::Release);
        self.sync_head_to_memory(next_head);

        Ok(head)
    }

    /// Dequeue an element from the ring buffer with acquire/release ordering.
    pub fn try_pop(&self) -> Result<(ShmRingElementHeader, Vec<u8>), AbiError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        // Check if ring is empty: head == tail
        if head == tail {
            return Err(AbiError::RingEmpty);
        }

        let slot_idx = (tail & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);

        let (header, payload) = unsafe {
            self.read_slot(slot_offset)?
        };

        let next_tail = tail.wrapping_add(1);
        self.tail.store(next_tail, Ordering::Release);
        self.sync_tail_to_memory(next_tail);

        Ok((header, payload))
    }

    /// True zero-copy pop returning a CanonicalMemorySlice into linear memory without allocations
    pub fn pop_zero_copy(&self) -> Result<ShmRingPopResult, AbiError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if head == tail {
            return Err(AbiError::RingEmpty);
        }

        let slot_idx = (tail & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);

        let header_ptr = unsafe { self.base_ptr.add(slot_offset) };
        let mut header_buf = [0u8; ShmRingElementHeader::SIZE];
        unsafe {
            std::ptr::copy_nonoverlapping(header_ptr, header_buf.as_mut_ptr(), ShmRingElementHeader::SIZE);
        }
        let header = ShmRingElementHeader::from_bytes(&header_buf)?;

        let payload_ptr = unsafe { header_ptr.add(ShmRingElementHeader::SIZE) } as u64;
        let slice = CanonicalMemorySlice {
            ptr: payload_ptr,
            len: header.payload_length as u64,
            alignment: 8,
            access_mode: MemoryAccessMode::ReadOnly,
        };

        let next_tail = tail.wrapping_add(1);
        self.tail.store(next_tail, Ordering::Release);
        self.sync_tail_to_memory(next_tail);

        Ok(ShmRingPopResult {
            header,
            payload_slice: slice,
            sequence_number: tail,
        })
    }

    /// In-place slice inspection without heap allocation
    pub fn pop_slice_with<F, R>(&self, f: F) -> Result<(ShmRingElementHeader, R), AbiError>
    where
        F: FnOnce(&[u8]) -> R,
    {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if head == tail {
            return Err(AbiError::RingEmpty);
        }

        let slot_idx = (tail & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);

        let header_ptr = unsafe { self.base_ptr.add(slot_offset) };
        let mut header_buf = [0u8; ShmRingElementHeader::SIZE];
        unsafe {
            std::ptr::copy_nonoverlapping(header_ptr, header_buf.as_mut_ptr(), ShmRingElementHeader::SIZE);
        }
        let header = ShmRingElementHeader::from_bytes(&header_buf)?;

        let payload_len = header.payload_length as usize;
        let payload_ptr = unsafe { header_ptr.add(ShmRingElementHeader::SIZE) };
        let raw_slice = unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) };

        let result = f(raw_slice);

        let next_tail = tail.wrapping_add(1);
        self.tail.store(next_tail, Ordering::Release);
        self.sync_tail_to_memory(next_tail);

        Ok((header, result))
    }

    /// Current head sequence counter
    #[inline(always)]
    pub fn ring_head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// Current tail sequence counter
    #[inline(always)]
    pub fn ring_tail(&self) -> u64 {
        self.tail.load(Ordering::Acquire)
    }

    /// Number of elements available for popping
    #[inline(always)]
    pub fn ring_available(&self) -> u32 {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail) as u32
    }

    /// Remaining write capacity before backpressure triggers
    #[inline(always)]
    pub fn ring_remaining_capacity(&self) -> u32 {
        let cap = self.descriptor.ring_capacity;
        cap.saturating_sub(self.ring_available())
    }

    /// Check if ring buffer is empty
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.ring_available() == 0
    }

    /// Check if ring buffer is full
    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.ring_available() >= self.descriptor.ring_capacity
    }

    /// Associated descriptor
    #[inline(always)]
    pub fn descriptor(&self) -> &ShmRingDescriptor {
        &self.descriptor
    }

    /// Push into ring buffer presenting a MemoryCapabilityToken.
    /// Strictly rejects ReadOnlyCapabilityToken with AbiError::PermissionDenied!
    pub fn push_with_token(
        &self,
        token: &MemoryCapabilityToken,
        secret: &[u8],
        header: ShmRingElementHeader,
        payload: &[u8],
    ) -> Result<u64, AbiError> {
        token.verify(secret)?;
        if !token.can_write() {
            return Err(AbiError::PermissionDenied);
        }
        self.try_push(header, payload)
    }

    /// Push into ring buffer presenting a ReadWriteCapabilityToken
    pub fn push_with_rw_token(
        &self,
        token: &ReadWriteCapabilityToken,
        secret: &[u8],
        header: ShmRingElementHeader,
        payload: &[u8],
    ) -> Result<u64, AbiError> {
        token.verify(secret)?;
        self.try_push(header, payload)
    }

    /// Push into ring buffer presenting a ReadOnlyCapabilityToken.
    /// STRICTLY FORBIDDEN: Always returns Err(AbiError::PermissionDenied).
    pub fn push_with_ro_token(
        &self,
        token: &ReadOnlyCapabilityToken,
        secret: &[u8],
        _header: ShmRingElementHeader,
        _payload: &[u8],
    ) -> Result<u64, AbiError> {
        token.verify(secret)?;
        Err(AbiError::PermissionDenied)
    }

    /// Pop from ring buffer presenting a MemoryCapabilityToken
    pub fn pop_with_token(
        &self,
        token: &MemoryCapabilityToken,
        secret: &[u8],
    ) -> Result<(ShmRingElementHeader, Vec<u8>), AbiError> {
        token.verify(secret)?;
        self.try_pop()
    }

    /// Pop from ring buffer presenting a ReadOnlyCapabilityToken (reading is permitted!)
    pub fn pop_with_ro_token(
        &self,
        token: &ReadOnlyCapabilityToken,
        secret: &[u8],
    ) -> Result<(ShmRingElementHeader, Vec<u8>), AbiError> {
        token.verify(secret)?;
        self.try_pop()
    }
}

// ============================================================================
// MULTI-MEMORY CAPABILITY SECURITY & READ-ONLY TOKENS
// ============================================================================

/// Memory capability permission level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum CapabilityPermission {
    ReadOnly = 0,
    ReadWrite = 1,
}

/// Functional usage role of a linear memory in multi-memory Wasmtime components
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum MemoryUsageRole {
    PrivateHeap = 0,
    SharedShmRing = 1,
    TensorDma = 2,
    IoBuffer = 3,
}

/// Standard WebAssembly page size: 64 KiB (65536 bytes)
pub const WASM_PAGE_SIZE_BYTES: usize = 65536;

/// Unforgeable cryptographically signed read-only capability token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadOnlyCapabilityToken {
    pub token_id: u64,
    pub memory_index: u32,
    pub view_id: u64,
    pub nonce: u64,
    pub allowed_offset: usize,
    pub allowed_len: usize,
    pub signature: [u8; 32],
}

impl ReadOnlyCapabilityToken {
    pub fn compute_signature(
        secret: &[u8],
        token_id: u64,
        memory_index: u32,
        view_id: u64,
        nonce: u64,
        allowed_offset: usize,
        allowed_len: usize,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(secret);
        hasher.update(b":hgb:cap:ro:v1:");
        hasher.update(&token_id.to_le_bytes());
        hasher.update(&memory_index.to_le_bytes());
        hasher.update(&view_id.to_le_bytes());
        hasher.update(&nonce.to_le_bytes());
        hasher.update(&(allowed_offset as u64).to_le_bytes());
        hasher.update(&(allowed_len as u64).to_le_bytes());
        let result = hasher.finalize();
        let mut sig = [0u8; 32];
        sig.copy_from_slice(&result);
        sig
    }

    pub fn issue(
        secret: &[u8],
        token_id: u64,
        memory_index: u32,
        view_id: u64,
        nonce: u64,
        allowed_offset: usize,
        allowed_len: usize,
    ) -> Self {
        let signature = Self::compute_signature(
            secret,
            token_id,
            memory_index,
            view_id,
            nonce,
            allowed_offset,
            allowed_len,
        );
        Self {
            token_id,
            memory_index,
            view_id,
            nonce,
            allowed_offset,
            allowed_len,
            signature,
        }
    }

    pub fn verify(&self, secret: &[u8]) -> Result<(), AbiError> {
        let expected = Self::compute_signature(
            secret,
            self.token_id,
            self.memory_index,
            self.view_id,
            self.nonce,
            self.allowed_offset,
            self.allowed_len,
        );
        if self.signature != expected {
            return Err(AbiError::PermissionDenied);
        }
        Ok(())
    }

    pub fn is_valid(&self, secret: &[u8]) -> bool {
        self.verify(secret).is_ok()
    }

    pub fn allows_range(&self, offset: usize, len: usize) -> bool {
        let req_end = match offset.checked_add(len) {
            Some(end) => end,
            None => return false,
        };
        let token_end = match self.allowed_offset.checked_add(self.allowed_len) {
            Some(end) => end,
            None => return false,
        };
        offset >= self.allowed_offset && req_end <= token_end
    }

    pub fn permission(&self) -> CapabilityPermission {
        CapabilityPermission::ReadOnly
    }

    pub fn can_write(&self) -> bool {
        false
    }

    pub fn can_read(&self) -> bool {
        true
    }

    pub fn reject_mutation(&self) -> Result<(), AbiError> {
        Err(AbiError::PermissionDenied)
    }
}

/// Cryptographically signed read-write capability token for privileged channels
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadWriteCapabilityToken {
    pub token_id: u64,
    pub memory_index: u32,
    pub view_id: u64,
    pub nonce: u64,
    pub allowed_offset: usize,
    pub allowed_len: usize,
    pub signature: [u8; 32],
}

impl ReadWriteCapabilityToken {
    pub fn compute_signature(
        secret: &[u8],
        token_id: u64,
        memory_index: u32,
        view_id: u64,
        nonce: u64,
        allowed_offset: usize,
        allowed_len: usize,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(secret);
        hasher.update(b":hgb:cap:rw:v1:");
        hasher.update(&token_id.to_le_bytes());
        hasher.update(&memory_index.to_le_bytes());
        hasher.update(&view_id.to_le_bytes());
        hasher.update(&nonce.to_le_bytes());
        hasher.update(&(allowed_offset as u64).to_le_bytes());
        hasher.update(&(allowed_len as u64).to_le_bytes());
        let result = hasher.finalize();
        let mut sig = [0u8; 32];
        sig.copy_from_slice(&result);
        sig
    }

    pub fn issue(
        secret: &[u8],
        token_id: u64,
        memory_index: u32,
        view_id: u64,
        nonce: u64,
        allowed_offset: usize,
        allowed_len: usize,
    ) -> Self {
        let signature = Self::compute_signature(
            secret,
            token_id,
            memory_index,
            view_id,
            nonce,
            allowed_offset,
            allowed_len,
        );
        Self {
            token_id,
            memory_index,
            view_id,
            nonce,
            allowed_offset,
            allowed_len,
            signature,
        }
    }

    pub fn verify(&self, secret: &[u8]) -> Result<(), AbiError> {
        let expected = Self::compute_signature(
            secret,
            self.token_id,
            self.memory_index,
            self.view_id,
            self.nonce,
            self.allowed_offset,
            self.allowed_len,
        );
        if self.signature != expected {
            return Err(AbiError::PermissionDenied);
        }
        Ok(())
    }

    pub fn is_valid(&self, secret: &[u8]) -> bool {
        self.verify(secret).is_ok()
    }

    pub fn allows_range(&self, offset: usize, len: usize) -> bool {
        let req_end = match offset.checked_add(len) {
            Some(end) => end,
            None => return false,
        };
        let token_end = match self.allowed_offset.checked_add(self.allowed_len) {
            Some(end) => end,
            None => return false,
        };
        offset >= self.allowed_offset && req_end <= token_end
    }

    pub fn permission(&self) -> CapabilityPermission {
        CapabilityPermission::ReadWrite
    }

    pub fn can_write(&self) -> bool {
        true
    }

    pub fn can_read(&self) -> bool {
        true
    }
}

/// Generic capability token representing either ReadOnly or ReadWrite access
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryCapabilityToken {
    ReadOnly(ReadOnlyCapabilityToken),
    ReadWrite(ReadWriteCapabilityToken),
}

impl MemoryCapabilityToken {
    pub fn permission(&self) -> CapabilityPermission {
        match self {
            Self::ReadOnly(t) => t.permission(),
            Self::ReadWrite(t) => t.permission(),
        }
    }

    pub fn memory_index(&self) -> u32 {
        match self {
            Self::ReadOnly(t) => t.memory_index,
            Self::ReadWrite(t) => t.memory_index,
        }
    }

    pub fn view_id(&self) -> u64 {
        match self {
            Self::ReadOnly(t) => t.view_id,
            Self::ReadWrite(t) => t.view_id,
        }
    }

    pub fn verify(&self, secret: &[u8]) -> Result<(), AbiError> {
        match self {
            Self::ReadOnly(t) => t.verify(secret),
            Self::ReadWrite(t) => t.verify(secret),
        }
    }

    pub fn allows_range(&self, offset: usize, len: usize) -> bool {
        match self {
            Self::ReadOnly(t) => t.allows_range(offset, len),
            Self::ReadWrite(t) => t.allows_range(offset, len),
        }
    }

    pub fn can_write(&self) -> bool {
        matches!(self, Self::ReadWrite(_))
    }

    pub fn can_read(&self) -> bool {
        true
    }
}

/// Linear memory descriptor supporting Wasmtime multi-memory configurations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearMemoryDescriptor {
    pub memory_index: u32,
    pub name: String,
    pub initial_pages: u32,
    pub maximum_pages: Option<u32>,
    pub is_shared: bool,
    pub role: MemoryUsageRole,
    pub access_mode: MemoryAccessMode,
    pub size_bytes: usize,
}

impl LinearMemoryDescriptor {
    pub fn new(
        memory_index: u32,
        name: impl Into<String>,
        initial_pages: u32,
        maximum_pages: Option<u32>,
        is_shared: bool,
        role: MemoryUsageRole,
        access_mode: MemoryAccessMode,
    ) -> Self {
        let size_bytes = (initial_pages as usize) * WASM_PAGE_SIZE_BYTES;
        Self {
            memory_index,
            name: name.into(),
            initial_pages,
            maximum_pages,
            is_shared,
            role,
            access_mode,
            size_bytes,
        }
    }

    pub fn new_private_heap(memory_index: u32, initial_pages: u32, maximum_pages: Option<u32>) -> Self {
        Self::new(
            memory_index,
            format!("memory_{}_heap", memory_index),
            initial_pages,
            maximum_pages,
            false,
            MemoryUsageRole::PrivateHeap,
            MemoryAccessMode::ReadWrite,
        )
    }

    pub fn new_shared_readonly_ring(memory_index: u32, pages: u32) -> Self {
        Self::new(
            memory_index,
            format!("memory_{}_shm_ring_ro", memory_index),
            pages,
            Some(pages),
            true,
            MemoryUsageRole::SharedShmRing,
            MemoryAccessMode::ReadOnly,
        )
    }

    pub fn new_shared_readwrite(memory_index: u32, pages: u32, role: MemoryUsageRole) -> Self {
        Self::new(
            memory_index,
            format!("memory_{}_shm_rw", memory_index),
            pages,
            Some(pages),
            true,
            role,
            MemoryAccessMode::ReadWrite,
        )
    }

    pub fn capacity_bytes(&self) -> usize {
        self.size_bytes
    }
}

/// Multi-memory component runtime manager supporting Wasmtime multi-memory isolation.
#[derive(Debug)]
pub struct MultiMemoryComponentRuntime {
    descriptors: HashMap<u32, LinearMemoryDescriptor>,
    buffers: HashMap<u32, Vec<u8>>,
    signing_secret: Vec<u8>,
}

impl MultiMemoryComponentRuntime {
    pub fn new(descriptors: Vec<LinearMemoryDescriptor>, signing_secret: impl Into<Vec<u8>>) -> Result<Self, AbiError> {
        let mut desc_map = HashMap::new();
        let mut buf_map = HashMap::new();

        for desc in descriptors {
            let buf = vec![0u8; desc.size_bytes];
            desc_map.insert(desc.memory_index, desc.clone());
            buf_map.insert(desc.memory_index, buf);
        }

        Ok(Self {
            descriptors: desc_map,
            buffers: buf_map,
            signing_secret: signing_secret.into(),
        })
    }

    pub fn descriptor(&self, memory_index: u32) -> Option<&LinearMemoryDescriptor> {
        self.descriptors.get(&memory_index)
    }

    pub fn memory_count(&self) -> usize {
        self.descriptors.len()
    }

    pub fn get_memory(&self, memory_index: u32) -> Result<&[u8], AbiError> {
        self.buffers
            .get(&memory_index)
            .map(|v| v.as_slice())
            .ok_or_else(|| AbiError::ViewNotFound { view_id: memory_index as u64 })
    }

    pub fn get_memory_mut(&mut self, memory_index: u32) -> Result<&mut [u8], AbiError> {
        self.buffers
            .get_mut(&memory_index)
            .map(|v| v.as_mut_slice())
            .ok_or_else(|| AbiError::ViewNotFound { view_id: memory_index as u64 })
    }

    pub fn create_view(
        &self,
        memory_index: u32,
        offset: usize,
        len: usize,
        alignment: usize,
    ) -> Result<CanonicalAbiShmView<'_>, AbiError> {
        let desc = self.descriptors.get(&memory_index)
            .ok_or_else(|| AbiError::ViewNotFound { view_id: memory_index as u64 })?;
        let buf = self.buffers.get(&memory_index).unwrap();
        CanonicalAbiShmView::new(memory_index as u64, buf, offset, len, alignment, desc.access_mode)
    }

    pub fn create_view_mut(
        &mut self,
        memory_index: u32,
        offset: usize,
        len: usize,
        alignment: usize,
    ) -> Result<CanonicalAbiShmViewMut<'_>, AbiError> {
        let desc = self.descriptors.get(&memory_index)
            .ok_or_else(|| AbiError::ViewNotFound { view_id: memory_index as u64 })?;
        if desc.access_mode == MemoryAccessMode::ReadOnly {
            return Err(AbiError::PermissionDenied);
        }
        let buf = self.buffers.get_mut(&memory_index).unwrap();
        CanonicalAbiShmViewMut::new(memory_index as u64, buf, offset, len, alignment, desc.access_mode)
    }

    pub fn acquire_view_with_ro_token<'a>(
        &'a self,
        token: &ReadOnlyCapabilityToken,
        alignment: usize,
    ) -> Result<CanonicalAbiShmView<'a>, AbiError> {
        token.verify(&self.signing_secret)?;

        let desc = self.descriptors.get(&token.memory_index)
            .ok_or_else(|| AbiError::ViewNotFound { view_id: token.view_id })?;

        let buf = self.buffers.get(&token.memory_index)
            .ok_or_else(|| AbiError::ViewNotFound { view_id: token.view_id })?;

        CanonicalAbiShmView::new(
            token.view_id,
            buf,
            token.allowed_offset,
            token.allowed_len,
            alignment,
            desc.access_mode,
        )
    }

    pub fn acquire_view_mut_with_ro_token<'a>(
        &'a mut self,
        token: &ReadOnlyCapabilityToken,
        _alignment: usize,
    ) -> Result<CanonicalAbiShmViewMut<'a>, AbiError> {
        token.verify(&self.signing_secret)?;
        Err(AbiError::PermissionDenied)
    }

    pub fn write_with_ro_token(
        &mut self,
        token: &ReadOnlyCapabilityToken,
        _offset: usize,
        _data: &[u8],
    ) -> Result<usize, AbiError> {
        token.verify(&self.signing_secret)?;
        Err(AbiError::PermissionDenied)
    }
}

/// Helper function to verify pointer and offset alignment
#[inline(always)]
pub fn verify_alignment(ptr_or_offset: usize, required_alignment: usize) -> Result<(), AbiError> {
    if required_alignment == 0 || (required_alignment & (required_alignment - 1)) != 0 {
        return Err(AbiError::AlignmentFault {
            ptr_or_offset,
            required_alignment,
        });
    }
    if ptr_or_offset % required_alignment != 0 {
        return Err(AbiError::AlignmentFault {
            ptr_or_offset,
            required_alignment,
        });
    }
    Ok(())
}

// ============================================================================
// CANONICAL ABI SHARED MEMORY VIEWS (READ-ONLY & MUTABLE)
// ============================================================================

/// Zero-copy read-only slice over linear memory with bounds checking and alignment verification
#[derive(Debug)]
pub struct CanonicalAbiShmView<'a> {
    view_id: u64,
    offset: usize,
    len: usize,
    alignment: usize,
    access_mode: MemoryAccessMode,
    slice: &'a [u8],
}

impl<'a> CanonicalAbiShmView<'a> {
    pub fn new(
        view_id: u64,
        memory: &'a [u8],
        offset: usize,
        len: usize,
        alignment: usize,
        access_mode: MemoryAccessMode,
    ) -> Result<Self, AbiError> {
        let end = offset.checked_add(len).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len,
            capacity: memory.len(),
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset,
                len,
                capacity: memory.len(),
            });
        }

        verify_alignment(offset, alignment)?;
        let raw_ptr = memory[offset..].as_ptr() as usize;
        verify_alignment(raw_ptr, alignment)?;

        Ok(Self {
            view_id,
            offset,
            len,
            alignment,
            access_mode,
            slice: &memory[offset..end],
        })
    }

    #[inline(always)]
    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    #[inline(always)]
    pub fn offset(&self) -> usize {
        self.offset
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn alignment(&self) -> usize {
        self.alignment
    }

    #[inline(always)]
    pub fn access_mode(&self) -> MemoryAccessMode {
        self.access_mode
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &'a [u8] {
        self.slice
    }

    pub fn as_typed_slice<T: PodCopy>(&self) -> Result<&'a [T], AbiError> {
        let t_size = std::mem::size_of::<T>();
        let t_align = std::mem::align_of::<T>();

        if t_size == 0 {
            return Err(AbiError::TypeMismatch {
                expected: "non-zero-sized type",
                found: "zero-sized type",
            });
        }

        if self.len % t_size != 0 {
            return Err(AbiError::OutOfBounds {
                offset: self.offset,
                len: self.len,
                capacity: self.len - (self.len % t_size),
            });
        }

        let raw_ptr = self.slice.as_ptr() as usize;
        verify_alignment(raw_ptr, t_align)?;

        let element_count = self.len / t_size;
        let typed_slice = unsafe {
            std::slice::from_raw_parts(self.slice.as_ptr() as *const T, element_count)
        };
        Ok(typed_slice)
    }

    #[inline(always)]
    pub fn as_f32_slice(&self) -> Result<&'a [f32], AbiError> {
        self.as_typed_slice::<f32>()
    }

    #[inline(always)]
    pub fn as_u32_slice(&self) -> Result<&'a [u32], AbiError> {
        self.as_typed_slice::<u32>()
    }

    #[inline(always)]
    pub fn as_u64_slice(&self) -> Result<&'a [u64], AbiError> {
        self.as_typed_slice::<u64>()
    }

    #[inline(always)]
    pub fn as_i32_slice(&self) -> Result<&'a [i32], AbiError> {
        self.as_typed_slice::<i32>()
    }

    pub fn subview(&self, sub_offset: usize, sub_len: usize, new_alignment: usize) -> Result<Self, AbiError> {
        let end = sub_offset.checked_add(sub_len).ok_or_else(|| AbiError::OutOfBounds {
            offset: sub_offset,
            len: sub_len,
            capacity: self.len,
        })?;

        if end > self.len {
            return Err(AbiError::OutOfBounds {
                offset: sub_offset,
                len: sub_len,
                capacity: self.len,
            });
        }

        let abs_offset = self.offset + sub_offset;
        verify_alignment(abs_offset, new_alignment)?;

        let raw_ptr = self.slice[sub_offset..].as_ptr() as usize;
        verify_alignment(raw_ptr, new_alignment)?;

        Ok(Self {
            view_id: self.view_id,
            offset: abs_offset,
            len: sub_len,
            alignment: new_alignment,
            access_mode: self.access_mode,
            slice: &self.slice[sub_offset..end],
        })
    }
}

/// Zero-copy mutable slice over linear memory with bounds checking and alignment verification
#[derive(Debug)]
pub struct CanonicalAbiShmViewMut<'a> {
    view_id: u64,
    offset: usize,
    len: usize,
    alignment: usize,
    access_mode: MemoryAccessMode,
    slice: &'a mut [u8],
}

impl<'a> CanonicalAbiShmViewMut<'a> {
    pub fn new(
        view_id: u64,
        memory: &'a mut [u8],
        offset: usize,
        len: usize,
        alignment: usize,
        access_mode: MemoryAccessMode,
    ) -> Result<Self, AbiError> {
        let end = offset.checked_add(len).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len,
            capacity: memory.len(),
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset,
                len,
                capacity: memory.len(),
            });
        }

        verify_alignment(offset, alignment)?;
        let raw_ptr = memory[offset..].as_ptr() as usize;
        verify_alignment(raw_ptr, alignment)?;

        Ok(Self {
            view_id,
            offset,
            len,
            alignment,
            access_mode,
            slice: &mut memory[offset..end],
        })
    }

    #[inline(always)]
    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    #[inline(always)]
    pub fn offset(&self) -> usize {
        self.offset
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[u8] {
        self.slice
    }

    #[inline(always)]
    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        self.slice
    }

    pub fn as_typed_slice_mut<T: PodCopy>(&mut self) -> Result<&mut [T], AbiError> {
        let t_size = std::mem::size_of::<T>();
        let t_align = std::mem::align_of::<T>();

        if t_size == 0 {
            return Err(AbiError::TypeMismatch {
                expected: "non-zero-sized type",
                found: "zero-sized type",
            });
        }

        if self.len % t_size != 0 {
            return Err(AbiError::OutOfBounds {
                offset: self.offset,
                len: self.len,
                capacity: self.len - (self.len % t_size),
            });
        }

        let raw_ptr = self.slice.as_ptr() as usize;
        verify_alignment(raw_ptr, t_align)?;

        let element_count = self.len / t_size;
        let typed_slice = unsafe {
            std::slice::from_raw_parts_mut(self.slice.as_mut_ptr() as *mut T, element_count)
        };
        Ok(typed_slice)
    }

    #[inline(always)]
    pub fn as_f32_slice_mut(&mut self) -> Result<&mut [f32], AbiError> {
        self.as_typed_slice_mut::<f32>()
    }

    #[inline(always)]
    pub fn as_u32_slice_mut(&mut self) -> Result<&mut [u32], AbiError> {
        self.as_typed_slice_mut::<u32>()
    }

    pub fn write_bytes(&mut self, src: &[u8]) -> Result<usize, AbiError> {
        if src.len() > self.len {
            return Err(AbiError::BufferOverflow {
                needed: src.len(),
                capacity: self.len,
            });
        }
        self.slice[..src.len()].copy_from_slice(src);
        Ok(src.len())
    }
}

// ============================================================================
// CANONICAL ABI INVOCATION HEADER (64-BYTE ALIGNED)
// ============================================================================

/// 64-byte aligned Canonical ABI Invocation Header layout.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalInvocationHeader {
    pub magic: u64,
    pub protocol_version: u32,
    pub action_code: u32,
    pub correlation_id: u64,
    pub payload_flags: u32,
    pub arg_offset: u32,
    pub arg_length: u32,
    pub return_offset: u32,
    pub return_capacity: u32,
    pub return_length: u32,
    pub status_code: u32,
    pub reserved: [u8; 12],
}

impl Default for CanonicalInvocationHeader {
    fn default() -> Self {
        Self {
            magic: CANONICAL_ABI_HEADER_MAGIC,
            protocol_version: PROTOCOL_VERSION_1,
            action_code: ACTION_PING,
            correlation_id: 0,
            payload_flags: 0,
            arg_offset: 0,
            arg_length: 0,
            return_offset: 0,
            return_capacity: 0,
            return_length: 0,
            status_code: 0,
            reserved: [0u8; 12],
        }
    }
}

impl CanonicalInvocationHeader {
    pub fn new(
        action_code: u32,
        correlation_id: u64,
        arg_offset: u32,
        arg_length: u32,
        return_offset: u32,
        return_capacity: u32,
    ) -> Self {
        Self {
            magic: CANONICAL_ABI_HEADER_MAGIC,
            protocol_version: PROTOCOL_VERSION_1,
            action_code,
            correlation_id,
            payload_flags: 0,
            arg_offset,
            arg_length,
            return_offset,
            return_capacity,
            return_length: 0,
            status_code: 0,
            reserved: [0u8; 12],
        }
    }

    pub fn to_bytes(&self) -> [u8; CANONICAL_HEADER_SIZE] {
        let mut buf = [0u8; CANONICAL_HEADER_SIZE];
        buf[0..8].copy_from_slice(&self.magic.to_le_bytes());
        buf[8..12].copy_from_slice(&self.protocol_version.to_le_bytes());
        buf[12..16].copy_from_slice(&self.action_code.to_le_bytes());
        buf[16..24].copy_from_slice(&self.correlation_id.to_le_bytes());
        buf[24..28].copy_from_slice(&self.payload_flags.to_le_bytes());
        buf[28..32].copy_from_slice(&self.arg_offset.to_le_bytes());
        buf[32..36].copy_from_slice(&self.arg_length.to_le_bytes());
        buf[36..40].copy_from_slice(&self.return_offset.to_le_bytes());
        buf[40..44].copy_from_slice(&self.return_capacity.to_le_bytes());
        buf[44..48].copy_from_slice(&self.return_length.to_le_bytes());
        buf[48..52].copy_from_slice(&self.status_code.to_le_bytes());
        buf[52..64].copy_from_slice(&self.reserved);
        buf
    }

    pub fn from_bytes(slice: &[u8]) -> Result<Self, AbiError> {
        if slice.len() < CANONICAL_HEADER_SIZE {
            return Err(AbiError::OutOfBounds {
                offset: 0,
                len: CANONICAL_HEADER_SIZE,
                capacity: slice.len(),
            });
        }

        let magic = u64::from_le_bytes(slice[0..8].try_into().unwrap());
        if magic != CANONICAL_ABI_HEADER_MAGIC {
            return Err(AbiError::InvalidMagic {
                expected: CANONICAL_ABI_HEADER_MAGIC,
                found: magic,
            });
        }

        let protocol_version = u32::from_le_bytes(slice[8..12].try_into().unwrap());
        let action_code = u32::from_le_bytes(slice[12..16].try_into().unwrap());
        let correlation_id = u64::from_le_bytes(slice[16..24].try_into().unwrap());
        let payload_flags = u32::from_le_bytes(slice[24..28].try_into().unwrap());
        let arg_offset = u32::from_le_bytes(slice[28..32].try_into().unwrap());
        let arg_length = u32::from_le_bytes(slice[32..36].try_into().unwrap());
        let return_offset = u32::from_le_bytes(slice[36..40].try_into().unwrap());
        let return_capacity = u32::from_le_bytes(slice[40..44].try_into().unwrap());
        let return_length = u32::from_le_bytes(slice[44..48].try_into().unwrap());
        let status_code = u32::from_le_bytes(slice[48..52].try_into().unwrap());

        let mut reserved = [0u8; 12];
        reserved.copy_from_slice(&slice[52..64]);

        Ok(Self {
            magic,
            protocol_version,
            action_code,
            correlation_id,
            payload_flags,
            arg_offset,
            arg_length,
            return_offset,
            return_capacity,
            return_length,
            status_code,
            reserved,
        })
    }

    pub fn write_to_slice(&self, dest: &mut [u8]) -> Result<(), AbiError> {
        if dest.len() < CANONICAL_HEADER_SIZE {
            return Err(AbiError::BufferOverflow {
                needed: CANONICAL_HEADER_SIZE,
                capacity: dest.len(),
            });
        }
        dest[..CANONICAL_HEADER_SIZE].copy_from_slice(&self.to_bytes());
        Ok(())
    }
}

const _: () = assert!(std::mem::size_of::<CanonicalInvocationHeader>() == 64);
const _: () = assert!(std::mem::align_of::<CanonicalInvocationHeader>() == 64);

// ============================================================================
// CANONICAL ABI INVOCATION PAYLOAD
// ============================================================================

/// Action invocation payload bypassing serde JSON/bincode
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalAbiInvocationPayload {
    pub header: CanonicalInvocationHeader,
    pub pinned_tensor: Option<PinnedTensorDescriptor>,
}

impl CanonicalAbiInvocationPayload {
    pub fn new(
        action_code: u32,
        correlation_id: u64,
        arg_offset: u32,
        arg_length: u32,
        return_offset: u32,
        return_capacity: u32,
    ) -> Self {
        Self {
            header: CanonicalInvocationHeader::new(
                action_code,
                correlation_id,
                arg_offset,
                arg_length,
                return_offset,
                return_capacity,
            ),
            pinned_tensor: None,
        }
    }

    pub fn with_pinned_tensor(mut self, tensor: PinnedTensorDescriptor) -> Self {
        self.pinned_tensor = Some(tensor);
        self
    }

    pub fn read_arg_slice<'a>(&self, memory: &'a [u8]) -> Result<&'a [u8], AbiError> {
        let start = self.header.arg_offset as usize;
        let len = self.header.arg_length as usize;
        let end = start.checked_add(len).ok_or_else(|| AbiError::OutOfBounds {
            offset: start,
            len,
            capacity: memory.len(),
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset: start,
                len,
                capacity: memory.len(),
            });
        }

        Ok(&memory[start..end])
    }

    pub fn read_arg_typed<'a, T: PodCopy>(&self, memory: &'a [u8]) -> Result<&'a [T], AbiError> {
        let raw_bytes = self.read_arg_slice(memory)?;
        let t_size = std::mem::size_of::<T>();
        let t_align = std::mem::align_of::<T>();

        if t_size == 0 || raw_bytes.len() % t_size != 0 {
            return Err(AbiError::TypeMismatch {
                expected: "aligned element multiple",
                found: "unaligned byte length",
            });
        }

        let raw_ptr = raw_bytes.as_ptr() as usize;
        verify_alignment(raw_ptr, t_align)?;

        let count = raw_bytes.len() / t_size;
        let typed_slice = unsafe {
            std::slice::from_raw_parts(raw_bytes.as_ptr() as *const T, count)
        };
        Ok(typed_slice)
    }

    pub fn write_return_slice(&mut self, memory: &mut [u8], data: &[u8]) -> Result<usize, AbiError> {
        let start = self.header.return_offset as usize;
        let capacity = self.header.return_capacity as usize;

        if data.len() > capacity {
            return Err(AbiError::BufferOverflow {
                needed: data.len(),
                capacity,
            });
        }

        let end = start.checked_add(data.len()).ok_or_else(|| AbiError::OutOfBounds {
            offset: start,
            len: data.len(),
            capacity: memory.len(),
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset: start,
                len: data.len(),
                capacity: memory.len(),
            });
        }

        memory[start..end].copy_from_slice(data);
        self.header.return_length = data.len() as u32;
        self.header.status_code = 0;
        Ok(data.len())
    }

    pub fn write_return_typed<T: PodCopy>(&mut self, memory: &mut [u8], items: &[T]) -> Result<usize, AbiError> {
        let byte_len = items.len().checked_mul(std::mem::size_of::<T>()).ok_or_else(|| {
            AbiError::BufferOverflow {
                needed: usize::MAX,
                capacity: self.header.return_capacity as usize,
            }
        })?;

        let byte_slice = unsafe {
            std::slice::from_raw_parts(items.as_ptr() as *const u8, byte_len)
        };
        self.write_return_slice(memory, byte_slice)
    }

    pub fn encode_to_memory(&self, memory: &mut [u8], header_offset: usize) -> Result<(), AbiError> {
        verify_alignment(header_offset, CANONICAL_ALIGNMENT_DEFAULT)?;
        let end = header_offset.checked_add(CANONICAL_HEADER_SIZE).ok_or_else(|| {
            AbiError::OutOfBounds {
                offset: header_offset,
                len: CANONICAL_HEADER_SIZE,
                capacity: memory.len(),
            }
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset: header_offset,
                len: CANONICAL_HEADER_SIZE,
                capacity: memory.len(),
            });
        }

        self.header.write_to_slice(&mut memory[header_offset..end])
    }

    pub fn decode_from_memory(memory: &[u8], header_offset: usize) -> Result<Self, AbiError> {
        verify_alignment(header_offset, CANONICAL_ALIGNMENT_DEFAULT)?;
        let end = header_offset.checked_add(CANONICAL_HEADER_SIZE).ok_or_else(|| {
            AbiError::OutOfBounds {
                offset: header_offset,
                len: CANONICAL_HEADER_SIZE,
                capacity: memory.len(),
            }
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset: header_offset,
                len: CANONICAL_HEADER_SIZE,
                capacity: memory.len(),
            });
        }

        let header = CanonicalInvocationHeader::from_bytes(&memory[header_offset..end])?;
        Ok(Self {
            header,
            pinned_tensor: None,
        })
    }
}

// ============================================================================
// CANONICAL MEMORY LENDER (LIFECYCLE MANAGER & ALIASING PREVENTER)
// ============================================================================

/// Active borrow metadata stored by the lender
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBorrowRecord {
    pub view_id: u64,
    pub offset: usize,
    pub len: usize,
    pub alignment: usize,
    pub access_mode: MemoryAccessMode,
    pub epoch: u64,
    pub tag: String,
}

/// Lifecycle manager tracking active borrows over linear memory.
#[derive(Debug)]
pub struct CanonicalMemoryLender {
    capacity: usize,
    active_borrows: HashMap<u64, ActiveBorrowRecord>,
    next_view_id: u64,
    epoch: u64,
}

impl CanonicalMemoryLender {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            active_borrows: HashMap::new(),
            next_view_id: 1,
            epoch: 1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn active_borrows_count(&self) -> usize {
        self.active_borrows.len()
    }

    pub fn is_lent(&self, view_id: u64) -> bool {
        self.active_borrows.contains_key(&view_id)
    }

    pub fn check_conflicts(&self, offset: usize, len: usize, mode: MemoryAccessMode) -> Result<(), AbiError> {
        let req_end = offset.checked_add(len).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len,
            capacity: self.capacity,
        })?;

        if req_end > self.capacity {
            return Err(AbiError::OutOfBounds {
                offset,
                len,
                capacity: self.capacity,
            });
        }

        for (view_id, active) in &self.active_borrows {
            let active_end = active.offset + active.len;
            let intersects = offset < active_end && req_end > active.offset;

            if intersects {
                let is_exclusive = mode != MemoryAccessMode::ReadOnly
                    || active.access_mode != MemoryAccessMode::ReadOnly;

                if is_exclusive {
                    let reason = format!(
                        "conflicts with active view {} ([{}..{}], {:?})",
                        view_id, active.offset, active_end, active.access_mode
                    );
                    return Err(AbiError::BorrowConflict {
                        requested_offset: offset,
                        requested_len: len,
                        conflict_view_id: *view_id,
                        reason,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn lend(
        &mut self,
        offset: usize,
        len: usize,
        alignment: usize,
        mode: MemoryAccessMode,
        tag: impl Into<String>,
    ) -> Result<TypedShmViewDescriptor, AbiError> {
        verify_alignment(offset, alignment)?;
        self.check_conflicts(offset, len, mode)?;

        let view_id = self.next_view_id;
        self.next_view_id += 1;
        self.epoch += 1;

        let record = ActiveBorrowRecord {
            view_id,
            offset,
            len,
            alignment,
            access_mode: mode,
            epoch: self.epoch,
            tag: tag.into(),
        };

        self.active_borrows.insert(view_id, record);

        let desc = TypedShmViewDescriptor {
            view_id,
            slice: CanonicalMemorySlice {
                ptr: offset as u64,
                len: len as u64,
                alignment: alignment as u32,
                access_mode: mode,
            },
            element_type: "u8".to_string(),
            element_stride: 1,
            element_count: len as u64,
            version_epoch: self.epoch,
            flags: mode as u32,
        };

        Ok(desc)
    }

    pub fn reclaim(&mut self, view_id: u64) -> Result<(), AbiError> {
        if self.active_borrows.remove(&view_id).is_none() {
            return Err(AbiError::ViewNotFound { view_id });
        }
        self.epoch += 1;
        Ok(())
    }

    pub fn lend_guard<'a>(
        &'a mut self,
        offset: usize,
        len: usize,
        alignment: usize,
        mode: MemoryAccessMode,
        tag: impl Into<String>,
    ) -> Result<LentRegionGuard<'a>, AbiError> {
        let desc = self.lend(offset, len, alignment, mode, tag)?;
        Ok(LentRegionGuard {
            lender: self,
            view_id: desc.view_id,
            descriptor: desc,
        })
    }
}

/// RAII guard providing automatic memory lease reclamation on drop
pub struct LentRegionGuard<'a> {
    lender: &'a mut CanonicalMemoryLender,
    view_id: u64,
    descriptor: TypedShmViewDescriptor,
}

impl<'a> LentRegionGuard<'a> {
    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    pub fn descriptor(&self) -> &TypedShmViewDescriptor {
        &self.descriptor
    }
}

impl<'a> Drop for LentRegionGuard<'a> {
    fn drop(&mut self) {
        let _ = self.lender.reclaim(self.view_id);
    }
}

// ============================================================================
// PINNED TENSOR DESCRIPTOR FOR ZERO-COPY NPU ROUTING
// ============================================================================

/// Pinned tensor descriptor for zero-copy DMA and NPU tensor acceleration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedTensorDescriptor {
    pub tensor_id: u64,
    pub shm_view_id: u64,
    pub dtype: TensorDataType,
    pub shape: Vec<usize>,
    pub strides: Vec<usize>,
    pub byte_offset: usize,
    pub byte_length: usize,
    pub alignment: usize,
    pub device_target: NpuDeviceTarget,
    pub is_pinned: bool,
}

impl PinnedTensorDescriptor {
    pub fn new_contiguous(
        tensor_id: u64,
        shm_view_id: u64,
        dtype: TensorDataType,
        shape: Vec<usize>,
        byte_offset: usize,
        device_target: NpuDeviceTarget,
    ) -> Result<Self, AbiError> {
        if shape.is_empty() {
            return Err(AbiError::InvalidDimensions {
                reason: "tensor shape cannot be empty".to_string(),
            });
        }

        let mut element_count: usize = 1;
        for &dim in &shape {
            if dim == 0 {
                return Err(AbiError::InvalidDimensions {
                    reason: "tensor dimension size cannot be zero".to_string(),
                });
            }
            element_count = element_count.checked_mul(dim).ok_or_else(|| {
                AbiError::InvalidDimensions {
                    reason: "dimension overflow".to_string(),
                }
            })?;
        }

        let byte_length = element_count.checked_mul(dtype.element_size()).ok_or_else(|| {
            AbiError::InvalidDimensions {
                reason: "byte length calculation overflow".to_string(),
            }
        })?;

        let mut strides = vec![1; shape.len()];
        for i in (0..shape.len() - 1).rev() {
            strides[i] = strides[i + 1] * shape[i + 1];
        }

        let required_alignment = if device_target == NpuDeviceTarget::NpuDirect {
            CANONICAL_ALIGNMENT_DEFAULT
        } else {
            dtype.element_size()
        };

        verify_alignment(byte_offset, required_alignment)?;

        Ok(Self {
            tensor_id,
            shm_view_id,
            dtype,
            shape,
            strides,
            byte_offset,
            byte_length,
            alignment: required_alignment,
            device_target,
            is_pinned: true,
        })
    }

    pub fn element_count(&self) -> usize {
        self.shape.iter().product()
    }

    pub fn validate_against_view(&self, view: &TypedShmViewDescriptor) -> Result<(), AbiError> {
        let view_offset = view.slice.ptr as usize;
        let view_len = view.slice.len as usize;

        if self.byte_offset < view_offset {
            return Err(AbiError::NpuRoutingFailed {
                reason: format!(
                    "Tensor byte_offset {} starts before view base offset {}",
                    self.byte_offset, view_offset
                ),
            });
        }

        let tensor_end = self.byte_offset + self.byte_length;
        let view_end = view_offset + view_len;

        if tensor_end > view_end {
            return Err(AbiError::NpuRoutingFailed {
                reason: format!(
                    "Tensor bounds [{}..{}] exceed parent view bounds [{}..{}]",
                    self.byte_offset, tensor_end, view_offset, view_end
                ),
            });
        }

        Ok(())
    }

    pub fn linear_index(&self, coords: &[usize]) -> Result<usize, AbiError> {
        if coords.len() != self.shape.len() {
            return Err(AbiError::InvalidDimensions {
                reason: format!("Expected {} coordinates, got {}", self.shape.len(), coords.len()),
            });
        }

        let mut idx = 0;
        for i in 0..coords.len() {
            if coords[i] >= self.shape[i] {
                return Err(AbiError::OutOfBounds {
                    offset: coords[i],
                    len: 1,
                    capacity: self.shape[i],
                });
            }
            idx += coords[i] * self.strides[i];
        }
        Ok(idx)
    }
}

// ============================================================================
// BENCHMARK HELPER (ZERO-COPY VS SERDE JSON LATENCY & CYCLES)
// ============================================================================

/// Benchmark execution report comparing zero-copy Canonical ABI against serde JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteropBenchmarkReport {
    pub iterations: usize,
    pub payload_floats: usize,
    pub payload_bytes: usize,
    pub serde_json_duration_nanos: u64,
    pub zero_copy_duration_nanos: u64,
    pub speedup_ratio: f64,
    pub estimated_cycles_serde: u64,
    pub estimated_cycles_zero_copy: u64,
    pub cycles_saved: u64,
    pub serde_heap_allocations_bytes: usize,
    pub zero_copy_heap_allocations_bytes: usize,
}

#[derive(Serialize, Deserialize)]
struct JsonPayloadDummy {
    action: u32,
    correlation_id: u64,
    tensor: Vec<f32>,
}

pub fn benchmark_interop_latency(payload_floats: usize, iterations: usize) -> InteropBenchmarkReport {
    let payload_bytes = payload_floats * 4;

    let mut float_vec = Vec::with_capacity(payload_floats);
    for i in 0..payload_floats {
        float_vec.push((i as f32) * 0.001);
    }

    let dummy = JsonPayloadDummy {
        action: ACTION_VECTOR_COMPUTE,
        correlation_id: 42,
        tensor: float_vec.clone(),
    };

    let serde_start = Instant::now();
    let mut serde_heap_bytes = 0;
    for _ in 0..iterations {
        let serialized = serde_json::to_vec(&dummy).expect("serde json serialization");
        serde_heap_bytes = serialized.len();
        let deserialized: JsonPayloadDummy = serde_json::from_slice(&serialized).expect("serde json deserialization");
        let _ = deserialized.tensor.len();
    }
    let serde_duration = serde_start.elapsed().as_nanos() as u64;

    let total_buf_size = 64 + payload_bytes + 64;
    let mut memory_buffer = vec![0u8; total_buf_size];

    let header_offset = 0;
    let arg_offset = 64;
    let mut payload = CanonicalAbiInvocationPayload::new(
        ACTION_VECTOR_COMPUTE,
        42,
        arg_offset as u32,
        payload_bytes as u32,
        0,
        0,
    );

    let float_bytes = unsafe {
        std::slice::from_raw_parts(float_vec.as_ptr() as *const u8, payload_bytes)
    };
    memory_buffer[arg_offset..arg_offset + payload_bytes].copy_from_slice(float_bytes);
    payload.encode_to_memory(&mut memory_buffer, header_offset).expect("encode payload");

    let zero_copy_start = Instant::now();
    let mut sum_elements = 0;
    for _ in 0..iterations {
        let decoded_payload = CanonicalAbiInvocationPayload::decode_from_memory(&memory_buffer, header_offset)
            .expect("decode payload");
        let view = decoded_payload.read_arg_typed::<f32>(&memory_buffer).expect("read typed f32 slice");
        sum_elements += view.len();
    }
    let zero_copy_duration = zero_copy_start.elapsed().as_nanos() as u64;
    let _ = sum_elements;

    let speedup_ratio = if zero_copy_duration > 0 {
        serde_duration as f64 / zero_copy_duration as f64
    } else {
        serde_duration as f64
    };

    let cycles_per_nano = 3;
    let est_cycles_serde = serde_duration * cycles_per_nano;
    let est_cycles_zero_copy = zero_copy_duration * cycles_per_nano;
    let cycles_saved = est_cycles_serde.saturating_sub(est_cycles_zero_copy) / (iterations as u64).max(1);

    InteropBenchmarkReport {
        iterations,
        payload_floats,
        payload_bytes,
        serde_json_duration_nanos: serde_duration,
        zero_copy_duration_nanos: zero_copy_duration,
        speedup_ratio,
        estimated_cycles_serde: est_cycles_serde,
        estimated_cycles_zero_copy: est_cycles_zero_copy,
        cycles_saved,
        serde_heap_allocations_bytes: serde_heap_bytes * iterations,
        zero_copy_heap_allocations_bytes: 0,
    }
}

// ============================================================================
// COMPREHENSIVE UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[repr(align(64))]
    struct AlignedMem<const N: usize>([u8; N]);

    #[test]
    fn test_zero_copy_view_creation_and_bounds() {
        let memory = AlignedMem([0u8; 1024]);
        let mem_slice = &memory.0[..];

        let view = CanonicalAbiShmView::new(1, mem_slice, 0, 512, 64, MemoryAccessMode::ReadOnly)
            .expect("valid view");
        assert_eq!(view.len(), 512);
        assert_eq!(view.offset(), 0);
        assert_eq!(view.view_id(), 1);

        let err = CanonicalAbiShmView::new(2, mem_slice, 600, 500, 4, MemoryAccessMode::ReadOnly);
        match err {
            Err(AbiError::OutOfBounds { offset, len, capacity }) => {
                assert_eq!(offset, 600);
                assert_eq!(len, 500);
                assert_eq!(capacity, 1024);
            }
            _ => panic!("Expected OutOfBounds error"),
        }
    }

    #[test]
    fn test_alignment_verification_4_8_16_64() {
        let memory = AlignedMem([0u8; 256]);
        let mem_slice = &memory.0[..];

        assert!(CanonicalAbiShmView::new(1, mem_slice, 0, 64, 64, MemoryAccessMode::ReadOnly).is_ok());

        let err = CanonicalAbiShmView::new(2, mem_slice, 3, 64, 4, MemoryAccessMode::ReadOnly);
        assert!(matches!(err, Err(AbiError::AlignmentFault { .. })));

        let err_bad_align = verify_alignment(64, 15);
        assert!(matches!(err_bad_align, Err(AbiError::AlignmentFault { .. })));
    }

    #[test]
    fn test_typed_slice_zero_copy_f32_u32() {
        let mut memory = vec![0u8; 512];

        let floats = [1.25f32, 2.5f32, 3.75f32, 5.0f32];
        let bytes = unsafe {
            std::slice::from_raw_parts(floats.as_ptr() as *const u8, 16)
        };
        memory[64..80].copy_from_slice(bytes);

        let view = CanonicalAbiShmView::new(1, &memory, 64, 16, 4, MemoryAccessMode::ReadOnly)
            .expect("create f32 view");
        let typed = view.as_f32_slice().expect("as_f32_slice");
        assert_eq!(typed.len(), 4);
        assert_eq!(typed, &[1.25, 2.5, 3.75, 5.0]);

        let mut view_mut = CanonicalAbiShmViewMut::new(2, &mut memory, 128, 16, 4, MemoryAccessMode::ReadWrite)
            .expect("view_mut");
        let out_slice = view_mut.as_f32_slice_mut().expect("as_f32_slice_mut");
        out_slice[0] = 99.0;
        out_slice[3] = 42.0;

        assert_eq!(f32::from_le_bytes(memory[128..132].try_into().unwrap()), 99.0);
        assert_eq!(f32::from_le_bytes(memory[140..144].try_into().unwrap()), 42.0);
    }

    #[test]
    fn test_borrow_conflict_exclusive_overlap_denied() {
        let mut lender = CanonicalMemoryLender::new(4096);

        let view1 = lender.lend(0, 1024, 64, MemoryAccessMode::ReadWrite, "tensor_a")
            .expect("lend view 1");
        assert_eq!(view1.view_id, 1);

        let conflict1 = lender.lend(512, 1024, 64, MemoryAccessMode::ReadWrite, "tensor_b");
        assert!(matches!(conflict1, Err(AbiError::BorrowConflict { .. })));

        let conflict2 = lender.lend(512, 1024, 64, MemoryAccessMode::ReadOnly, "tensor_c");
        assert!(matches!(conflict2, Err(AbiError::BorrowConflict { .. })));

        let view2 = lender.lend(1024, 1024, 64, MemoryAccessMode::ReadWrite, "tensor_d");
        assert!(view2.is_ok());
    }

    #[test]
    fn test_borrow_shared_readonly_overlap_allowed() {
        let mut lender = CanonicalMemoryLender::new(4096);

        let view1 = lender.lend(0, 1024, 64, MemoryAccessMode::ReadOnly, "shared_a")
            .expect("lend readonly 1");
        assert_eq!(view1.view_id, 1);

        let view2 = lender.lend(512, 1024, 64, MemoryAccessMode::ReadOnly, "shared_b")
            .expect("lend readonly 2");
        assert_eq!(view2.view_id, 2);

        let conflict = lender.lend(512, 1024, 64, MemoryAccessMode::ReadWrite, "writer");
        assert!(matches!(conflict, Err(AbiError::BorrowConflict { .. })));
    }

    #[test]
    fn test_lifecycle_reclaim_and_relend() {
        let mut lender = CanonicalMemoryLender::new(2048);

        let view1 = lender.lend(0, 1024, 64, MemoryAccessMode::ExclusiveWrite, "lease_1")
            .expect("lend view 1");
        assert_eq!(lender.active_borrows_count(), 1);

        lender.reclaim(view1.view_id).expect("reclaim");
        assert_eq!(lender.active_borrows_count(), 0);

        let err = lender.reclaim(view1.view_id);
        assert!(matches!(err, Err(AbiError::ViewNotFound { .. })));

        let view2 = lender.lend(0, 1024, 64, MemoryAccessMode::ExclusiveWrite, "lease_2");
        assert!(view2.is_ok());
    }

    #[test]
    fn test_raii_guard_automatic_drop_reclaim() {
        let mut lender = CanonicalMemoryLender::new(2048);

        {
            let guard = lender.lend_guard(0, 512, 64, MemoryAccessMode::ReadWrite, "guard_scope")
                .expect("lend guard");
            assert_eq!(guard.view_id(), 1);
        }
        assert_eq!(lender.active_borrows_count(), 0);

        let relend = lender.lend(0, 512, 64, MemoryAccessMode::ReadWrite, "relend_after_drop");
        assert!(relend.is_ok());
    }

    #[test]
    fn test_canonical_abi_invocation_payload_header_roundtrip() {
        let header = CanonicalInvocationHeader::new(
            ACTION_TOOL_INVOKE,
            9999,
            64,
            128,
            256,
            512,
        );

        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), 64);

        let decoded = CanonicalInvocationHeader::from_bytes(&bytes).expect("from_bytes");
        assert_eq!(decoded.magic, CANONICAL_ABI_HEADER_MAGIC);
        assert_eq!(decoded.protocol_version, 1);
        assert_eq!(decoded.action_code, ACTION_TOOL_INVOKE);
        assert_eq!(decoded.correlation_id, 9999);
        assert_eq!(decoded.arg_offset, 64);
        assert_eq!(decoded.arg_length, 128);
        assert_eq!(decoded.return_offset, 256);
        assert_eq!(decoded.return_capacity, 512);

        let mut corrupt = bytes;
        corrupt[0] = 0xff;
        let err = CanonicalInvocationHeader::from_bytes(&corrupt);
        assert!(matches!(err, Err(AbiError::InvalidMagic { .. })));
    }

    #[test]
    fn test_pinned_tensor_npu_descriptor_validation() {
        let desc = PinnedTensorDescriptor::new_contiguous(
            10,
            1,
            TensorDataType::F32,
            vec![2, 3, 4],
            64,
            NpuDeviceTarget::NpuDirect,
        ).expect("create pinned tensor");

        assert_eq!(desc.element_count(), 24);
        assert_eq!(desc.byte_length, 96);
        assert_eq!(desc.strides, vec![12, 4, 1]);

        assert_eq!(desc.linear_index(&[1, 2, 3]).unwrap(), 23);

        let misaligned = PinnedTensorDescriptor::new_contiguous(
            11,
            1,
            TensorDataType::F32,
            vec![2, 3, 4],
            16,
            NpuDeviceTarget::NpuDirect,
        );
        assert!(matches!(misaligned, Err(AbiError::AlignmentFault { .. })));
    }

    #[test]
    fn test_subview_windowing_and_alignment() {
        let memory = AlignedMem([0u8; 1024]);
        let mem_slice = &memory.0[..];
        let parent = CanonicalAbiShmView::new(1, mem_slice, 0, 512, 64, MemoryAccessMode::ReadOnly)
            .expect("parent view");

        let sub = parent.subview(64, 128, 64).expect("subview");
        assert_eq!(sub.offset(), 64);
        assert_eq!(sub.len(), 128);

        let oob = parent.subview(400, 200, 4);
        assert!(matches!(oob, Err(AbiError::OutOfBounds { .. })));
    }

    #[test]
    fn test_benchmark_helper_execution() {
        let report = benchmark_interop_latency(128, 50);
        assert_eq!(report.iterations, 50);
        assert_eq!(report.payload_floats, 128);
        assert_eq!(report.payload_bytes, 512);
        assert_eq!(report.zero_copy_heap_allocations_bytes, 0);
        assert!(report.speedup_ratio > 0.0);
    }

    // ========================================================================
    // PILLAR B LOCK-FREE SHM RING BUFFER TESTS
    // ========================================================================

    #[test]
    fn test_shm_ring_descriptor_and_power_of_two() {
        // Valid power of two capacities
        for &cap in &[2u32, 4, 8, 16, 64, 1024] {
            let desc = ShmRingDescriptor::new(100, cap, 64).expect("power of two descriptor");
            assert_eq!(desc.ring_capacity, cap);
            assert_eq!(desc.mask, cap - 1);
            assert_eq!(desc.head_offset, 64);
            assert_eq!(desc.tail_offset, 128);
            assert_eq!(desc.buffer_offset, 256);
            assert_eq!(desc.slot_stride() % 64, 0); // 64-byte aligned slot stride
        }

        // Invalid non-power-of-two capacities must fail
        for &bad_cap in &[0u32, 3, 5, 7, 10, 100, 1000] {
            let err = ShmRingDescriptor::new(100, bad_cap, 64);
            assert!(matches!(err, Err(AbiError::InvalidCapacity { .. })));
        }

        // Zero element size must fail
        let err_elem = ShmRingDescriptor::new(100, 8, 0);
        assert!(matches!(err_elem, Err(AbiError::TypeMismatch { .. })));
    }

    #[test]
    fn test_lock_free_ring_push_pop_zero_copy() {
        let ring = LockFreeShmRingBuffer::new(8, 64, 1).expect("create ring");
        assert_eq!(ring.ring_available(), 0);
        assert!(ring.is_empty());
        assert!(!ring.is_full());

        let payload_1 = b"zero_copy_payload_alpha";
        let header_1 = ShmRingElementHeader::new(0, payload_1.len() as u32, 0x1, 123456789);
        let seq_1 = ring.try_push(header_1, payload_1).expect("push 1");
        assert_eq!(seq_1, 0);
        assert_eq!(ring.ring_available(), 1);
        assert_eq!(ring.ring_head(), 1);
        assert_eq!(ring.ring_tail(), 0);

        let payload_2 = b"zero_copy_payload_beta";
        let header_2 = ShmRingElementHeader::new(0, payload_2.len() as u32, 0x2, 123456799);
        let seq_2 = ring.try_push(header_2, payload_2).expect("push 2");
        assert_eq!(seq_2, 1);
        assert_eq!(ring.ring_available(), 2);
        assert_eq!(ring.ring_head(), 2);

        // Pop zero copy with CanonicalMemorySlice
        let pop_res_1 = ring.pop_zero_copy().expect("pop zero copy 1");
        assert_eq!(pop_res_1.sequence_number, 0);
        assert_eq!(pop_res_1.header.flags, 0x1);
        assert_eq!(pop_res_1.header.payload_length, payload_1.len() as u32);
        assert_eq!(pop_res_1.payload_slice.len, payload_1.len() as u64);
        assert_eq!(ring.ring_available(), 1);
        assert_eq!(ring.ring_tail(), 1);

        // Pop with in-place inspection closure
        let (hdr_2, matched) = ring.pop_slice_with(|data| {
            assert_eq!(data, payload_2);
            true
        }).expect("pop slice with 2");
        assert!(matched);
        assert_eq!(hdr_2.flags, 0x2);
        assert_eq!(ring.ring_available(), 0);
        assert!(ring.is_empty());
    }

    #[test]
    fn test_ring_wraparound_and_backpressure() {
        let capacity = 4u32;
        let ring = LockFreeShmRingBuffer::new(capacity, 32, 2).expect("create small ring");

        // Fill ring to capacity
        for i in 0..capacity {
            let data = format!("msg_{}", i);
            let hdr = ShmRingElementHeader::new(0, data.len() as u32, i, 1000 + (i as u64));
            let seq = ring.try_push(hdr, data.as_bytes()).expect("push item");
            assert_eq!(seq, i as u64);
        }

        assert_eq!(ring.ring_available(), 4);
        assert!(ring.is_full());
        assert_eq!(ring.ring_remaining_capacity(), 0);

        // 5th push must be rejected due to backpressure (ring full)
        let overflow_data = b"overflow_attempt";
        let overflow_hdr = ShmRingElementHeader::new(0, overflow_data.len() as u32, 999, 9999);
        let err = ring.try_push(overflow_hdr, overflow_data);
        assert!(matches!(err, Err(AbiError::RingFull { capacity: 4 })));

        // Pop 2 items to relieve backpressure
        let (h0, p0) = ring.try_pop().expect("pop 0");
        assert_eq!(h0.sequence_number, 0);
        assert_eq!(p0, b"msg_0");

        let (h1, p1) = ring.try_pop().expect("pop 1");
        assert_eq!(h1.sequence_number, 1);
        assert_eq!(p1, b"msg_1");

        assert_eq!(ring.ring_available(), 2);
        assert!(!ring.is_full());

        // Push 2 new items (head advances to 4 and 5, wrapping slots 0 and 1!)
        for i in 4..6 {
            let data = format!("wrapped_msg_{}", i);
            let hdr = ShmRingElementHeader::new(0, data.len() as u32, i, 2000 + (i as u64));
            let seq = ring.try_push(hdr, data.as_bytes()).expect("push wrapped item");
            assert_eq!(seq, i as u64);
        }

        assert_eq!(ring.ring_available(), 4);
        assert!(ring.is_full());

        // Drain all items in exact monotonic order
        for expected_seq in 2..6 {
            let (hdr, payload) = ring.try_pop().expect("drain item");
            assert_eq!(hdr.sequence_number, expected_seq);
            let expected_prefix = if expected_seq < 4 { "msg_" } else { "wrapped_msg_" };
            let expected_str = format!("{}{}", expected_prefix, expected_seq);
            assert_eq!(payload, expected_str.as_bytes());
        }

        assert_eq!(ring.ring_available(), 0);
        assert!(ring.is_empty());

        // Popping empty ring returns RingEmpty
        let empty_err = ring.try_pop();
        assert!(matches!(empty_err, Err(AbiError::RingEmpty)));
    }

    #[test]
    fn test_ring_linear_memory_mapping() {
        let mut linear_memory = AlignedMem([0u8; 8192]);
        let offset = 64; // 64-byte aligned offset

        let ring = LockFreeShmRingBuffer::from_linear_memory(
            &mut linear_memory.0,
            offset,
            8,
            64,
            777,
        ).expect("map ring from linear memory");

        // Verify control block written directly into linear memory
        let magic_bytes = &linear_memory.0[offset..offset + 8];
        let magic = u64::from_le_bytes(magic_bytes.try_into().unwrap());
        assert_eq!(magic, SHM_RING_MAGIC);

        let cap_bytes = &linear_memory.0[offset + 8..offset + 12];
        let cap = u32::from_le_bytes(cap_bytes.try_into().unwrap());
        assert_eq!(cap, 8);

        // Push and verify in-memory head update
        let hdr = ShmRingElementHeader::new(0, 5, 0xabc, 42);
        ring.try_push(hdr, b"hello").expect("push into mapped ring");

        let head_bytes = &linear_memory.0[offset + 64..offset + 72];
        let mem_head = u64::from_le_bytes(head_bytes.try_into().unwrap());
        assert_eq!(mem_head, 1);

        // Pop and verify in-memory tail update
        let (pop_hdr, pop_data) = ring.try_pop().expect("pop from mapped ring");
        assert_eq!(pop_hdr.sequence_number, 0);
        assert_eq!(pop_data, b"hello");

        let tail_bytes = &linear_memory.0[offset + 128..offset + 136];
        let mem_tail = u64::from_le_bytes(tail_bytes.try_into().unwrap());
        assert_eq!(mem_tail, 1);
    }

    #[test]
    fn test_ring_capability_token_borrow_safety() {
        let secret = b"hagibis_microkernel_super_secret_key_2026";
        let ring = LockFreeShmRingBuffer::new(8, 64, 42).expect("create ring");

        let ro_token = ReadOnlyCapabilityToken::issue(
            secret,
            1,
            1,
            42,
            1001,
            0,
            ring.descriptor().required_buffer_size(),
        );

        let rw_token = ReadWriteCapabilityToken::issue(
            secret,
            2,
            1,
            42,
            1002,
            0,
            ring.descriptor().required_buffer_size(),
        );

        // 1. Push with ReadOnlyCapabilityToken MUST BE REJECTED!
        let hdr = ShmRingElementHeader::new(0, 4, 0, 0);
        let ro_push_err = ring.push_with_ro_token(&ro_token, secret, hdr, b"test");
        assert!(matches!(ro_push_err, Err(AbiError::PermissionDenied)));

        let generic_ro = MemoryCapabilityToken::ReadOnly(ro_token.clone());
        let generic_ro_err = ring.push_with_token(&generic_ro, secret, hdr, b"test");
        assert!(matches!(generic_ro_err, Err(AbiError::PermissionDenied)));

        // 2. Push with ReadWriteCapabilityToken succeeds
        let rw_push = ring.push_with_rw_token(&rw_token, secret, hdr, b"data");
        assert!(rw_push.is_ok());

        // 3. Pop with ReadOnlyCapabilityToken is permitted (read access)
        let pop_res = ring.pop_with_ro_token(&ro_token, secret);
        assert!(pop_res.is_ok());
        let (_, data) = pop_res.unwrap();
        assert_eq!(data, b"data");

        // 4. Tampered token signature fails verification
        let mut tampered_ro = ro_token;
        tampered_ro.nonce = 999999; // corrupt nonce
        let tampered_err = ring.pop_with_ro_token(&tampered_ro, secret);
        assert!(matches!(tampered_err, Err(AbiError::PermissionDenied)));
    }

    #[test]
    fn test_multithreaded_concurrency_stress() {
        let capacity = 64u32;
        let ring = Arc::new(LockFreeShmRingBuffer::new(capacity, 16, 99).expect("create ring"));
        let iterations = 2000u64;

        let producer_ring = Arc::clone(&ring);
        let producer_thread = thread::spawn(move || {
            for i in 0..iterations {
                let payload = i.to_le_bytes();
                let hdr = ShmRingElementHeader::new(0, 8, 0, i);
                loop {
                    match producer_ring.try_push(hdr, &payload) {
                        Ok(_) => break,
                        Err(AbiError::RingFull { .. }) => thread::yield_now(),
                        Err(e) => panic!("Producer error: {:?}", e),
                    }
                }
            }
        });

        let consumer_ring = Arc::clone(&ring);
        let consumer_thread = thread::spawn(move || {
            let mut received = Vec::with_capacity(iterations as usize);
            while (received.len() as u64) < iterations {
                match consumer_ring.try_pop() {
                    Ok((hdr, data)) => {
                        let val = u64::from_le_bytes(data[0..8].try_into().unwrap());
                        assert_eq!(hdr.timestamp_ns, val);
                        received.push(val);
                    }
                    Err(AbiError::RingEmpty) => thread::yield_now(),
                    Err(e) => panic!("Consumer error: {:?}", e),
                }
            }
            received
        });

        producer_thread.join().expect("producer join");
        let received = consumer_thread.join().expect("consumer join");

        assert_eq!(received.len(), iterations as usize);
        for i in 0..iterations {
            assert_eq!(received[i as usize], i);
        }
        assert!(ring.is_empty());
    }
}
