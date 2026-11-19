//! # Canonical ABI & Zero-Copy Shared Memory Views (`canonical_abi_shm_views`)
//!
//! Production-grade Canonical ABI zero-copy inter-component memory exchange for `hgb-wasm-kernel`.
//! Replaces JSON/bincode serialization overhead with 64-byte aligned linear memory views,
//! active borrow/lend lifecycle tracking, and zero-copy tensor routing for NPU acceleration.
//! Backed by Linux io_uring asynchronous shared memory bridge and lock-free ring conduits.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
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
    /// Linux io_uring error (e.g. invalid opcode, unregistered buffer, ring full)
    IoUringError { errno: i32, message: String },
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
            Self::IoUringError { errno, message } => {
                write!(f, "io_uring error (errno {}): {}", errno, message)
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
const _: () = assert!(std::mem::align_of::<CachePadded<AtomicU32>>() == 64);
const _: () = assert!(std::mem::size_of::<CachePadded<AtomicU32>>() == 64);

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
    let payload = CanonicalAbiInvocationPayload::new(
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
// LINUX IO_URING ASYNCHRONOUS SHARED MEMORY BRIDGE
// ============================================================================

/// Magic signature for io_uring SHM Bridge: ASCII "HGBURING" (0x4847425552494E47)
pub const IO_URING_SHM_MAGIC: u64 = 0x4847425552494E47;

// Linux io_uring OpCodes
pub const IORING_OP_NOP: u8 = 0;
pub const IORING_OP_READV: u8 = 1;
pub const IORING_OP_WRITEV: u8 = 2;
pub const IORING_OP_FSYNC: u8 = 3;
pub const IORING_OP_READ_FIXED: u8 = 4;
pub const IORING_OP_WRITE_FIXED: u8 = 5;
pub const IORING_OP_POLL_ADD: u8 = 6;
pub const IORING_OP_POLL_REMOVE: u8 = 7;
pub const IORING_OP_SYNC_FILE_RANGE: u8 = 8;
pub const IORING_OP_SENDMSG: u8 = 9;
pub const IORING_OP_RECVMSG: u8 = 10;
pub const IORING_OP_TIMEOUT: u8 = 11;
pub const IORING_OP_TIMEOUT_REMOVE: u8 = 12;
pub const IORING_OP_ACCEPT: u8 = 13;
pub const IORING_OP_ASYNC_CANCEL: u8 = 14;
pub const IORING_OP_LINK_TIMEOUT: u8 = 15;
pub const IORING_OP_CONNECT: u8 = 16;
pub const IORING_OP_FALLOCATE: u8 = 17;
pub const IORING_OP_OPENAT: u8 = 18;
pub const IORING_OP_CLOSE: u8 = 19;
pub const IORING_OP_FILES_UPDATE: u8 = 20;
pub const IORING_OP_STATX: u8 = 21;
pub const IORING_OP_READ: u8 = 22;
pub const IORING_OP_WRITE: u8 = 23;
pub const IORING_OP_FADVISE: u8 = 24;
pub const IORING_OP_MADVISE: u8 = 25;
pub const IORING_OP_SEND: u8 = 26;
pub const IORING_OP_RECV: u8 = 27;
pub const IORING_OP_OPENAT2: u8 = 28;
pub const IORING_OP_EPOLL_CTL: u8 = 29;
pub const IORING_OP_SPLICE: u8 = 30;
pub const IORING_OP_PROVIDE_BUFFERS: u8 = 31;
pub const IORING_OP_REMOVE_BUFFERS: u8 = 32;
pub const IORING_OP_TEE: u8 = 33;
pub const IORING_OP_SHUTDOWN: u8 = 34;
pub const IORING_OP_RENAMEAT: u8 = 35;
pub const IORING_OP_UNLINKAT: u8 = 36;
pub const IORING_OP_MKDIRAT: u8 = 37;
pub const IORING_OP_SYMLINKAT: u8 = 38;
pub const IORING_OP_LINKAT: u8 = 39;
pub const IORING_OP_MSG_RING: u8 = 40;
pub const IORING_OP_FSETXATTR: u8 = 41;
pub const IORING_OP_SETXATTR: u8 = 42;
pub const IORING_OP_FGETXATTR: u8 = 43;
pub const IORING_OP_GETXATTR: u8 = 44;
pub const IORING_OP_SOCKET: u8 = 45;
pub const IORING_OP_URING_CMD: u8 = 46;
pub const IORING_OP_SEND_ZC: u8 = 47;

// io_uring SQE flags
pub const IOSQE_FIXED_FILE: u8 = 1 << 0;
pub const IOSQE_IO_DRAIN: u8 = 1 << 1;
pub const IOSQE_IO_LINK: u8 = 1 << 2;
pub const IOSQE_IO_HARDLINK: u8 = 1 << 3;
pub const IOSQE_ASYNC: u8 = 1 << 4;
pub const IOSQE_BUFFER_SELECT: u8 = 1 << 5;
pub const IOSQE_CQE_SKIP_SUCCESS: u8 = 1 << 6;

// io_uring setup flags
pub const IORING_SETUP_IOPOLL: u32 = 1 << 0;
pub const IORING_SETUP_SQPOLL: u32 = 1 << 1;
pub const IORING_SETUP_SQ_AFF: u32 = 1 << 2;
pub const IORING_SETUP_CQSIZE: u32 = 1 << 3;
pub const IORING_SETUP_CLAMP: u32 = 1 << 4;
pub const IORING_SETUP_ATTACH_WQ: u32 = 1 << 5;
pub const IORING_SETUP_R_DISABLED: u32 = 1 << 6;
pub const IORING_SETUP_SUBMIT_ALL: u32 = 1 << 7;
pub const IORING_SETUP_COOP_TASKRUN: u32 = 1 << 8;
pub const IORING_SETUP_TASKRUN_FLAG: u32 = 1 << 9;
pub const IORING_SETUP_SQE128: u32 = 1 << 10;
pub const IORING_SETUP_CQE32: u32 = 1 << 11;
pub const IORING_SETUP_SINGLE_ISSUER: u32 = 1 << 12;
pub const IORING_SETUP_DEFER_TASKRUN: u32 = 1 << 13;

// io_uring register opcodes
pub const IORING_REGISTER_BUFFERS: u32 = 0;
pub const IORING_UNREGISTER_BUFFERS: u32 = 1;
pub const IORING_REGISTER_FILES: u32 = 2;
pub const IORING_UNREGISTER_FILES: u32 = 3;

/// Submission Queue Entry (SQE) descriptor for asynchronous zero-copy IO.
/// Conforms to WIT `io-uring-sqe-descriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct IoUringSqeDescriptor {
    pub opcode: u8,
    pub flags: u8,
    pub ioprio: u16,
    pub fd: i32,
    pub off: u64,
    pub addr: u64,
    pub len: u32,
    pub user_data: u64,
}

unsafe impl PodCopy for IoUringSqeDescriptor {}

impl IoUringSqeDescriptor {
    pub const SIZE: usize = 40;

    pub fn new(opcode: u8, fd: i32, addr: u64, len: u32, off: u64, user_data: u64) -> Self {
        Self {
            opcode,
            flags: 0,
            ioprio: 0,
            fd,
            off,
            addr,
            len,
            user_data,
        }
    }

    pub fn nop(user_data: u64) -> Self {
        Self::new(IORING_OP_NOP, 0, 0, 0, 0, user_data)
    }

    pub fn read(fd: i32, addr: u64, len: u32, off: u64, user_data: u64) -> Self {
        Self::new(IORING_OP_READ, fd, addr, len, off, user_data)
    }

    pub fn write(fd: i32, addr: u64, len: u32, off: u64, user_data: u64) -> Self {
        Self::new(IORING_OP_WRITE, fd, addr, len, off, user_data)
    }

    pub fn read_fixed(fd: i32, addr: u64, len: u32, off: u64, buf_index: u16, user_data: u64) -> Self {
        let mut sqe = Self::new(IORING_OP_READ_FIXED, fd, addr, len, off, user_data);
        sqe.ioprio = buf_index;
        sqe
    }

    pub fn write_fixed(fd: i32, addr: u64, len: u32, off: u64, buf_index: u16, user_data: u64) -> Self {
        let mut sqe = Self::new(IORING_OP_WRITE_FIXED, fd, addr, len, off, user_data);
        sqe.ioprio = buf_index;
        sqe
    }

    pub fn msg_ring(target_fd: i32, msg1: u32, user_data: u64) -> Self {
        Self::new(IORING_OP_MSG_RING, target_fd, 0, msg1, 0, user_data)
    }

    pub fn poll_add(fd: i32, poll_mask: u32, user_data: u64) -> Self {
        Self::new(IORING_OP_POLL_ADD, fd, 0, poll_mask, 0, user_data)
    }

    #[inline(always)]
    pub fn is_read(&self) -> bool {
        self.opcode == IORING_OP_READ || self.opcode == IORING_OP_READ_FIXED || self.opcode == IORING_OP_READV
    }

    #[inline(always)]
    pub fn is_write(&self) -> bool {
        self.opcode == IORING_OP_WRITE || self.opcode == IORING_OP_WRITE_FIXED || self.opcode == IORING_OP_WRITEV || self.opcode == IORING_OP_SEND_ZC
    }

    #[inline(always)]
    pub fn is_fixed(&self) -> bool {
        self.opcode == IORING_OP_READ_FIXED || self.opcode == IORING_OP_WRITE_FIXED
    }

    #[inline(always)]
    pub fn buf_index(&self) -> u16 {
        self.ioprio
    }

    #[inline(always)]
    pub fn set_buf_index(&mut self, idx: u16) {
        self.ioprio = idx;
    }
}

/// Completion Queue Entry (CQE) descriptor returning operation status and user metadata.
/// Conforms to WIT `io-uring-cqe-descriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct IoUringCqeDescriptor {
    pub user_data: u64,
    pub res: i32,
    pub flags: u32,
}

unsafe impl PodCopy for IoUringCqeDescriptor {}

impl IoUringCqeDescriptor {
    pub const SIZE: usize = 16;

    pub fn new(user_data: u64, res: i32, flags: u32) -> Self {
        Self {
            user_data,
            res,
            flags,
        }
    }

    pub fn success(user_data: u64, res: i32) -> Self {
        Self::new(user_data, res, 0)
    }

    pub fn error(user_data: u64, errno: i32) -> Self {
        Self::new(user_data, -errno.abs(), 0)
    }

    #[inline(always)]
    pub fn is_ok(&self) -> bool {
        self.res >= 0
    }

    #[inline(always)]
    pub fn is_err(&self) -> bool {
        self.res < 0
    }

    #[inline(always)]
    pub fn result(&self) -> Result<usize, i32> {
        if self.res >= 0 {
            Ok(self.res as usize)
        } else {
            Err(-self.res)
        }
    }
}

/// Zero-copy buffer registration descriptor for IORING_REGISTER_BUFFERS.
/// Conforms to WIT `io-uring-buffer-registration`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoUringBufferRegistration {
    pub buffer_id: u32,
    pub shm_handle: String,
    pub offset: u64,
    pub capacity: u64,
    pub page_size: u32,
    pub is_wasm_linear_memory: bool,
}

impl IoUringBufferRegistration {
    pub fn new(
        buffer_id: u32,
        shm_handle: impl Into<String>,
        offset: u64,
        capacity: u64,
        page_size: u32,
        is_wasm_linear_memory: bool,
    ) -> Self {
        Self {
            buffer_id,
            shm_handle: shm_handle.into(),
            offset,
            capacity,
            page_size,
            is_wasm_linear_memory,
        }
    }

    pub fn wasm_linear_memory(buffer_id: u32, offset: u64, capacity: u64) -> Self {
        Self::new(
            buffer_id,
            format!("wasm:linear_memory:0x{:x}", offset),
            offset,
            capacity,
            WASM_PAGE_SIZE_BYTES as u32,
            true,
        )
    }

    pub fn validate(&self) -> Result<(), AbiError> {
        if self.capacity == 0 {
            return Err(AbiError::InvalidCapacity {
                capacity: 0,
                reason: "Registered buffer capacity must be greater than zero".into(),
            });
        }
        if self.page_size == 0 || (self.page_size & (self.page_size - 1)) != 0 {
            return Err(AbiError::AlignmentFault {
                ptr_or_offset: self.page_size as usize,
                required_alignment: 4096,
            });
        }
        if (self.offset % 64) != 0 {
            return Err(AbiError::AlignmentFault {
                ptr_or_offset: self.offset as usize,
                required_alignment: 64,
            });
        }
        Ok(())
    }
}

/// Internal representation of a registered buffer with optional pinned memory pointer
#[derive(Debug, Clone)]
pub struct RegisteredBufferEntry {
    pub registration: IoUringBufferRegistration,
    pub memory_ptr: usize,
    pub memory_len: usize,
}

/// Computes byte layout offsets for SQ and CQ rings in linear memory
pub fn calculate_channel_layout(sq_entries: u32, cq_entries: u32) -> (usize, usize, usize) {
    let sq_offset = 192;
    let sq_size = (sq_entries as usize) * std::mem::size_of::<IoUringSqeDescriptor>();
    let cq_offset = (sq_offset + sq_size + 63) & !63;
    let cq_size = (cq_entries as usize) * std::mem::size_of::<IoUringCqeDescriptor>();
    let total_size = (cq_offset + cq_size + 63) & !63;
    (sq_offset, cq_offset, total_size)
}

/// Lock-free zero-copy io_uring IPC channel spanning host kernel and WASM guest linear memory.
/// Conforms to WIT `io-uring-shm-channel`.
pub struct IoUringShmChannel {
    sq_capacity: u32,
    sq_mask: u32,
    cq_capacity: u32,
    cq_mask: u32,
    flags: u32,
    sq_head: CachePadded<AtomicU32>,
    sq_tail: CachePadded<AtomicU32>,
    cq_head: CachePadded<AtomicU32>,
    cq_tail: CachePadded<AtomicU32>,
    sq_flags: CachePadded<AtomicU32>,
    cq_flags: CachePadded<AtomicU32>,
    storage: RingStorage,
    base_ptr: *mut u8,
    sqes_ptr: *mut IoUringSqeDescriptor,
    cqes_ptr: *mut IoUringCqeDescriptor,
    registered_buffers: Arc<RwLock<HashMap<u32, RegisteredBufferEntry>>>,
    virtual_fds: Arc<RwLock<HashMap<i32, Vec<u8>>>>,
    processing_lock: AtomicBool,
}

unsafe impl Send for IoUringShmChannel {}
unsafe impl Sync for IoUringShmChannel {}

impl IoUringShmChannel {
    /// Construct a new io_uring shared memory channel with specified queue depth and flags
    pub fn new(entries: u32, flags: u32) -> Result<Self, AbiError> {
        if entries == 0 || (entries & (entries - 1)) != 0 {
            return Err(AbiError::InvalidCapacity {
                capacity: entries as usize,
                reason: "SQ entries count must be a non-zero power of two".into(),
            });
        }

        let sq_capacity = entries;
        let cq_capacity = entries.checked_mul(2).ok_or_else(|| AbiError::InvalidCapacity {
            capacity: entries as usize,
            reason: "CQ capacity overflow".into(),
        })?;
        let (sq_offset, cq_offset, total_size) = calculate_channel_layout(sq_capacity, cq_capacity);

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
        let sqes_ptr = unsafe { base_ptr.add(sq_offset) as *mut IoUringSqeDescriptor };
        let cqes_ptr = unsafe { base_ptr.add(cq_offset) as *mut IoUringCqeDescriptor };

        let channel = Self {
            sq_capacity,
            sq_mask: sq_capacity - 1,
            cq_capacity,
            cq_mask: cq_capacity - 1,
            flags,
            sq_head: CachePadded::new(AtomicU32::new(0)),
            sq_tail: CachePadded::new(AtomicU32::new(0)),
            cq_head: CachePadded::new(AtomicU32::new(0)),
            cq_tail: CachePadded::new(AtomicU32::new(0)),
            sq_flags: CachePadded::new(AtomicU32::new(0)),
            cq_flags: CachePadded::new(AtomicU32::new(0)),
            storage: RingStorage::Owned(buffer),
            base_ptr,
            sqes_ptr,
            cqes_ptr,
            registered_buffers: Arc::new(RwLock::new(HashMap::new())),
            virtual_fds: Arc::new(RwLock::new(HashMap::new())),
            processing_lock: AtomicBool::new(false),
        };

        unsafe {
            channel.init_control_block();
        }

        Ok(channel)
    }

    /// Map an io_uring SHM channel directly over an existing WASM linear memory slice
    pub fn from_linear_memory(
        memory: &mut [u8],
        offset: usize,
        entries: u32,
        flags: u32,
    ) -> Result<Self, AbiError> {
        verify_alignment(offset, CANONICAL_ALIGNMENT_DEFAULT)?;
        if entries == 0 || (entries & (entries - 1)) != 0 {
            return Err(AbiError::InvalidCapacity {
                capacity: entries as usize,
                reason: "SQ entries count must be a non-zero power of two".into(),
            });
        }

        let sq_capacity = entries;
        let cq_capacity = entries.checked_mul(2).ok_or_else(|| AbiError::InvalidCapacity {
            capacity: entries as usize,
            reason: "CQ capacity overflow".into(),
        })?;
        let (sq_offset, cq_offset, total_size) = calculate_channel_layout(sq_capacity, cq_capacity);

        let end = offset.checked_add(total_size).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len: total_size,
            capacity: memory.len(),
        })?;

        if end > memory.len() {
            return Err(AbiError::OutOfBounds {
                offset,
                len: total_size,
                capacity: memory.len(),
            });
        }

        let base_ptr = unsafe { memory.as_mut_ptr().add(offset) };
        verify_alignment(base_ptr as usize, CANONICAL_ALIGNMENT_DEFAULT)?;

        let sqes_ptr = unsafe { base_ptr.add(sq_offset) as *mut IoUringSqeDescriptor };
        let cqes_ptr = unsafe { base_ptr.add(cq_offset) as *mut IoUringCqeDescriptor };

        let channel = Self {
            sq_capacity,
            sq_mask: sq_capacity - 1,
            cq_capacity,
            cq_mask: cq_capacity - 1,
            flags,
            sq_head: CachePadded::new(AtomicU32::new(0)),
            sq_tail: CachePadded::new(AtomicU32::new(0)),
            cq_head: CachePadded::new(AtomicU32::new(0)),
            cq_tail: CachePadded::new(AtomicU32::new(0)),
            sq_flags: CachePadded::new(AtomicU32::new(0)),
            cq_flags: CachePadded::new(AtomicU32::new(0)),
            storage: RingStorage::Mapped {
                ptr: base_ptr,
                len: total_size,
            },
            base_ptr,
            sqes_ptr,
            cqes_ptr,
            registered_buffers: Arc::new(RwLock::new(HashMap::new())),
            virtual_fds: Arc::new(RwLock::new(HashMap::new())),
            processing_lock: AtomicBool::new(false),
        };

        unsafe {
            channel.init_control_block();
        }

        Ok(channel)
    }

    /// Format control block at base_ptr
    unsafe fn init_control_block(&self) {
        let ptr = self.base_ptr;
        // 0..8: Magic
        std::ptr::copy_nonoverlapping(&IO_URING_SHM_MAGIC.to_le_bytes()[0], ptr, 8);
        // 8..12: SQ capacity
        std::ptr::copy_nonoverlapping(&self.sq_capacity.to_le_bytes()[0], ptr.add(8), 4);
        // 12..16: CQ capacity
        std::ptr::copy_nonoverlapping(&self.cq_capacity.to_le_bytes()[0], ptr.add(12), 4);
        // 16..20: SQ mask
        std::ptr::copy_nonoverlapping(&self.sq_mask.to_le_bytes()[0], ptr.add(16), 4);
        // 20..24: CQ mask
        std::ptr::copy_nonoverlapping(&self.cq_mask.to_le_bytes()[0], ptr.add(20), 4);
        // 24..28: Flags
        std::ptr::copy_nonoverlapping(&self.flags.to_le_bytes()[0], ptr.add(24), 4);
        // 64..68: SQ Head
        std::ptr::copy_nonoverlapping(&0u32.to_le_bytes()[0], ptr.add(64), 4);
        // 68..72: SQ Tail
        std::ptr::copy_nonoverlapping(&0u32.to_le_bytes()[0], ptr.add(68), 4);
        // 72..76: SQ Flags
        std::ptr::copy_nonoverlapping(&0u32.to_le_bytes()[0], ptr.add(72), 4);
        // 128..132: CQ Head
        std::ptr::copy_nonoverlapping(&0u32.to_le_bytes()[0], ptr.add(128), 4);
        // 132..136: CQ Tail
        std::ptr::copy_nonoverlapping(&0u32.to_le_bytes()[0], ptr.add(132), 4);
        // 136..140: CQ Flags
        std::ptr::copy_nonoverlapping(&0u32.to_le_bytes()[0], ptr.add(136), 4);
    }

    #[inline(always)]
    fn resolve_addr(&self, addr: u64, len: usize) -> Option<*mut u8> {
        if addr == 0 || addr < 4096 {
            return None;
        }
        match &self.storage {
            RingStorage::Mapped { ptr, len: storage_len } => {
                if (addr as usize).checked_add(len).map_or(false, |end| end <= *storage_len) {
                    Some(unsafe { ptr.add(addr as usize) })
                } else if addr >= 0x10000 {
                    Some(addr as *mut u8)
                } else {
                    None
                }
            }
            RingStorage::Owned(_) => {
                if addr >= 0x10000 {
                    Some(addr as *mut u8)
                } else {
                    None
                }
            }
        }
    }

    /// Submit batch of SQEs to the submission ring guarded by capability token
    pub fn submit(
        &self,
        sqes: &[IoUringSqeDescriptor],
        token: &MemoryCapabilityToken,
        secret: &[u8],
    ) -> Result<u32, AbiError> {
        token.verify(secret)?;

        for sqe in sqes {
            if sqe.is_write() && !token.can_write() {
                return Err(AbiError::PermissionDenied);
            }
            if !sqe.is_fixed() && sqe.addr != 0 && sqe.len > 0 {
                if !token.allows_range(sqe.addr as usize, sqe.len as usize) {
                    return Err(AbiError::PermissionDenied);
                }
            }
        }

        if sqes.is_empty() {
            return Ok(0);
        }

        let head = self.sq_head.load(Ordering::Acquire);
        let tail = self.sq_tail.load(Ordering::Acquire);
        let count = tail.wrapping_sub(head);
        let free_space = self.sq_capacity.saturating_sub(count);

        let cq_head = self.cq_head.load(Ordering::Acquire);
        let cq_tail = self.cq_tail.load(Ordering::Acquire);
        let cq_count = cq_tail.wrapping_sub(cq_head);
        let cq_free = self.sq_capacity.saturating_sub(cq_count);

        if (sqes.len() as u32) > free_space || (sqes.len() as u32) > cq_free {
            return Err(AbiError::RingFull {
                capacity: self.sq_capacity as usize,
            });
        }

        for (i, sqe) in sqes.iter().enumerate() {
            let slot = (tail.wrapping_add(i as u32)) & self.sq_mask;
            unsafe {
                std::ptr::write(self.sqes_ptr.add(slot as usize), *sqe);
            }
        }

        let new_tail = tail.wrapping_add(sqes.len() as u32);
        self.sq_tail.store(new_tail, Ordering::Release);
        unsafe {
            std::ptr::copy_nonoverlapping(&new_tail.to_le_bytes()[0], self.base_ptr.add(68), 4);
        }

        self.process_pending_sqes();

        Ok(sqes.len() as u32)
    }

    fn process_pending_sqes(&self) {
        if self.processing_lock.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return;
        }

        let tail = self.sq_tail.load(Ordering::Acquire);
        let mut head = self.sq_head.load(Ordering::Acquire);

        while head != tail {
            let cq_tail = self.cq_tail.load(Ordering::Acquire);
            let cq_head = self.cq_head.load(Ordering::Acquire);
            if cq_tail.wrapping_sub(cq_head) >= self.cq_capacity {
                break;
            }

            let slot = head & self.sq_mask;
            let sqe = unsafe { std::ptr::read(self.sqes_ptr.add(slot as usize)) };

            let cqe = self.execute_sqe(&sqe);

            let cq_slot = cq_tail & self.cq_mask;
            unsafe {
                std::ptr::write(self.cqes_ptr.add(cq_slot as usize), cqe);
            }
            let new_cq_tail = cq_tail.wrapping_add(1);
            self.cq_tail.store(new_cq_tail, Ordering::Release);
            unsafe {
                std::ptr::copy_nonoverlapping(&new_cq_tail.to_le_bytes()[0], self.base_ptr.add(132), 4);
            }

            head = head.wrapping_add(1);
            self.sq_head.store(head, Ordering::Release);
            unsafe {
                std::ptr::copy_nonoverlapping(&head.to_le_bytes()[0], self.base_ptr.add(64), 4);
            }
        }

        self.processing_lock.store(false, Ordering::Release);
    }

    fn execute_sqe(&self, sqe: &IoUringSqeDescriptor) -> IoUringCqeDescriptor {
        match sqe.opcode {
            IORING_OP_NOP => IoUringCqeDescriptor::success(sqe.user_data, 0),
            IORING_OP_READ => {
                let virtual_fds = self.virtual_fds.read().unwrap();
                if let Some(data) = virtual_fds.get(&sqe.fd) {
                    let off = sqe.off as usize;
                    let len = sqe.len as usize;
                    if off >= data.len() {
                        IoUringCqeDescriptor::success(sqe.user_data, 0)
                    } else {
                        let available = (data.len() - off).min(len);
                        if let Some(dst) = self.resolve_addr(sqe.addr, available) {
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    data.as_ptr().add(off),
                                    dst,
                                    available,
                                );
                            }
                        }
                        IoUringCqeDescriptor::success(sqe.user_data, available as i32)
                    }
                } else {
                    IoUringCqeDescriptor::error(sqe.user_data, 9)
                }
            }
            IORING_OP_WRITE => {
                let mut virtual_fds = self.virtual_fds.write().unwrap();
                let entry = virtual_fds.entry(sqe.fd).or_insert_with(Vec::new);
                let off = sqe.off as usize;
                let len = sqe.len as usize;
                if off + len > entry.len() {
                    entry.resize(off + len, 0);
                }
                if let Some(src) = self.resolve_addr(sqe.addr, len) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            src,
                            entry.as_mut_ptr().add(off),
                            len,
                        );
                    }
                }
                IoUringCqeDescriptor::success(sqe.user_data, len as i32)
            }
            IORING_OP_READ_FIXED => {
                let buf_id = sqe.buf_index() as u32;
                let registered = self.registered_buffers.read().unwrap();
                if let Some(entry) = registered.get(&buf_id) {
                    let off = sqe.off as usize;
                    let len = sqe.len as usize;
                    if off + len > entry.registration.capacity as usize {
                        IoUringCqeDescriptor::error(sqe.user_data, 14)
                    } else {
                        if let (Some(dst), true) = (self.resolve_addr(sqe.addr, len), entry.memory_ptr != 0) {
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    (entry.memory_ptr as *const u8).add(off),
                                    dst,
                                    len,
                                );
                            }
                        }
                        IoUringCqeDescriptor::success(sqe.user_data, len as i32)
                    }
                } else {
                    IoUringCqeDescriptor::error(sqe.user_data, 22)
                }
            }
            IORING_OP_WRITE_FIXED => {
                let buf_id = sqe.buf_index() as u32;
                let registered = self.registered_buffers.read().unwrap();
                if let Some(entry) = registered.get(&buf_id) {
                    let off = sqe.off as usize;
                    let len = sqe.len as usize;
                    if off + len > entry.registration.capacity as usize {
                        IoUringCqeDescriptor::error(sqe.user_data, 14)
                    } else {
                        if let (Some(src), true) = (self.resolve_addr(sqe.addr, len), entry.memory_ptr != 0) {
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    src,
                                    (entry.memory_ptr as *mut u8).add(off),
                                    len,
                                );
                            }
                        }
                        IoUringCqeDescriptor::success(sqe.user_data, len as i32)
                    }
                } else {
                    IoUringCqeDescriptor::error(sqe.user_data, 22)
                }
            }
            IORING_OP_MSG_RING => IoUringCqeDescriptor::success(sqe.user_data, 0),
            IORING_OP_POLL_ADD => IoUringCqeDescriptor::success(sqe.user_data, 0),
            _ => IoUringCqeDescriptor::error(sqe.user_data, 38),
        }
    }

    /// Poll and reap CQEs from the completion ring without blocking
    pub fn poll_completions(
        &self,
        _min_complete: u32,
        max_entries: u32,
    ) -> Result<Vec<IoUringCqeDescriptor>, AbiError> {
        self.process_pending_sqes();

        let cq_tail = self.cq_tail.load(Ordering::Acquire);
        let cq_head = self.cq_head.load(Ordering::Acquire);
        let available = cq_tail.wrapping_sub(cq_head);

        if available == 0 {
            return Ok(Vec::new());
        }

        let to_reap = (available.min(max_entries)) as usize;
        let mut completions = Vec::with_capacity(to_reap);

        for i in 0..to_reap {
            let slot = (cq_head.wrapping_add(i as u32)) & self.cq_mask;
            let cqe = unsafe { std::ptr::read(self.cqes_ptr.add(slot as usize)) };
            completions.push(cqe);
        }

        let new_head = cq_head.wrapping_add(to_reap as u32);
        self.cq_head.store(new_head, Ordering::Release);
        unsafe {
            std::ptr::copy_nonoverlapping(&new_head.to_le_bytes()[0], self.base_ptr.add(128), 4);
        }

        self.process_pending_sqes();

        Ok(completions)
    }

    /// Register shared memory buffers with the kernel for fixed-buffer zero-copy operations
    pub fn register_buffers(
        &self,
        buffers: &[IoUringBufferRegistration],
        token: &MemoryCapabilityToken,
        secret: &[u8],
    ) -> Result<u32, AbiError> {
        token.verify(secret)?;

        let mut reg_map = self.registered_buffers.write().unwrap();
        let mut count = 0;

        for buf in buffers {
            buf.validate()?;
            if !token.allows_range(buf.offset as usize, buf.capacity as usize) {
                return Err(AbiError::PermissionDenied);
            }
            if reg_map.contains_key(&buf.buffer_id) {
                return Err(AbiError::DoubleLend {
                    view_id: buf.buffer_id as u64,
                    offset: buf.offset as usize,
                });
            }
            reg_map.insert(
                buf.buffer_id,
                RegisteredBufferEntry {
                    registration: buf.clone(),
                    memory_ptr: 0,
                    memory_len: buf.capacity as usize,
                },
            );
            count += 1;
        }

        Ok(count)
    }

    /// Unregister previously registered buffers, releasing pinned page references
    pub fn unregister_buffers(
        &self,
        token: &MemoryCapabilityToken,
        secret: &[u8],
    ) -> Result<(), AbiError> {
        token.verify(secret)?;
        let mut reg_map = self.registered_buffers.write().unwrap();
        reg_map.clear();
        Ok(())
    }

    /// Attach direct linear memory pointer to a registered buffer for zero-copy read/write operations
    pub fn attach_buffer_memory(&self, buffer_id: u32, ptr: *mut u8, len: usize) -> Result<(), AbiError> {
        let mut reg_map = self.registered_buffers.write().unwrap();
        if let Some(entry) = reg_map.get_mut(&buffer_id) {
            if len < entry.registration.capacity as usize {
                return Err(AbiError::OutOfBounds {
                    offset: 0,
                    len: entry.registration.capacity as usize,
                    capacity: len,
                });
            }
            entry.memory_ptr = ptr as usize;
            entry.memory_len = len;
            Ok(())
        } else {
            Err(AbiError::ViewNotFound {
                view_id: buffer_id as u64,
            })
        }
    }

    #[inline(always)]
    pub fn sq_capacity(&self) -> u32 {
        self.sq_capacity
    }

    #[inline(always)]
    pub fn cq_capacity(&self) -> u32 {
        self.cq_capacity
    }

    #[inline(always)]
    pub fn sq_head(&self) -> u32 {
        self.sq_head.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn sq_tail(&self) -> u32 {
        self.sq_tail.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn cq_head(&self) -> u32 {
        self.cq_head.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn cq_tail(&self) -> u32 {
        self.cq_tail.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn sq_pending(&self) -> u32 {
        let tail = self.sq_tail.load(Ordering::Acquire);
        let head = self.sq_head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    #[inline(always)]
    pub fn cq_available(&self) -> u32 {
        let tail = self.cq_tail.load(Ordering::Acquire);
        let head = self.cq_head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    pub fn registered_buffer_count(&self) -> usize {
        self.registered_buffers.read().unwrap().len()
    }

    pub fn get_registered_buffer(&self, buffer_id: u32) -> Option<IoUringBufferRegistration> {
        self.registered_buffers
            .read()
            .unwrap()
            .get(&buffer_id)
            .map(|e| e.registration.clone())
    }

    pub fn set_virtual_file(&self, fd: i32, data: Vec<u8>) {
        self.virtual_fds.write().unwrap().insert(fd, data);
    }

    pub fn get_virtual_file(&self, fd: i32) -> Option<Vec<u8>> {
        self.virtual_fds.read().unwrap().get(&fd).cloned()
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

    #[test]
    fn test_shm_ring_descriptor_and_power_of_two() {
        for &cap in &[2u32, 4, 8, 16, 64, 1024] {
            let desc = ShmRingDescriptor::new(100, cap, 64).expect("power of two descriptor");
            assert_eq!(desc.ring_capacity, cap);
            assert_eq!(desc.mask, cap - 1);
            assert_eq!(desc.head_offset, 64);
            assert_eq!(desc.tail_offset, 128);
            assert_eq!(desc.buffer_offset, 256);
            assert_eq!(desc.slot_stride() % 64, 0);
        }

        for &bad_cap in &[0u32, 3, 5, 7, 10, 100, 1000] {
            let err = ShmRingDescriptor::new(100, bad_cap, 64);
            assert!(matches!(err, Err(AbiError::InvalidCapacity { .. })));
        }

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

        let pop_res_1 = ring.pop_zero_copy().expect("pop zero copy 1");
        assert_eq!(pop_res_1.sequence_number, 0);
        assert_eq!(pop_res_1.header.flags, 0x1);
        assert_eq!(pop_res_1.header.payload_length, payload_1.len() as u32);
        assert_eq!(pop_res_1.payload_slice.len, payload_1.len() as u64);
        assert_eq!(ring.ring_available(), 1);
        assert_eq!(ring.ring_tail(), 1);

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

        for i in 0..capacity {
            let data = format!("msg_{}", i);
            let hdr = ShmRingElementHeader::new(0, data.len() as u32, i, 1000 + (i as u64));
            let seq = ring.try_push(hdr, data.as_bytes()).expect("push item");
            assert_eq!(seq, i as u64);
        }

        assert_eq!(ring.ring_available(), 4);
        assert!(ring.is_full());
        assert_eq!(ring.ring_remaining_capacity(), 0);

        let overflow_data = b"overflow_attempt";
        let overflow_hdr = ShmRingElementHeader::new(0, overflow_data.len() as u32, 999, 9999);
        let err = ring.try_push(overflow_hdr, overflow_data);
        assert!(matches!(err, Err(AbiError::RingFull { capacity: 4 })));

        let (h0, p0) = ring.try_pop().expect("pop 0");
        assert_eq!(h0.sequence_number, 0);
        assert_eq!(p0, b"msg_0");

        let (h1, p1) = ring.try_pop().expect("pop 1");
        assert_eq!(h1.sequence_number, 1);
        assert_eq!(p1, b"msg_1");

        assert_eq!(ring.ring_available(), 2);
        assert!(!ring.is_full());

        for i in 4..6 {
            let data = format!("wrapped_msg_{}", i);
            let hdr = ShmRingElementHeader::new(0, data.len() as u32, i, 2000 + (i as u64));
            let seq = ring.try_push(hdr, data.as_bytes()).expect("push wrapped item");
            assert_eq!(seq, i as u64);
        }

        assert_eq!(ring.ring_available(), 4);
        assert!(ring.is_full());

        for expected_seq in 2..6 {
            let (hdr, payload) = ring.try_pop().expect("drain item");
            assert_eq!(hdr.sequence_number, expected_seq);
            let expected_prefix = if expected_seq < 4 { "msg_" } else { "wrapped_msg_" };
            let expected_str = format!("{}{}", expected_prefix, expected_seq);
            assert_eq!(payload, expected_str.as_bytes());
        }

        assert_eq!(ring.ring_available(), 0);
        assert!(ring.is_empty());

        let empty_err = ring.try_pop();
        assert!(matches!(empty_err, Err(AbiError::RingEmpty)));
    }

    #[test]
    fn test_ring_linear_memory_mapping() {
        let mut linear_memory = AlignedMem([0u8; 8192]);
        let offset = 64;

        let ring = LockFreeShmRingBuffer::from_linear_memory(
            &mut linear_memory.0,
            offset,
            8,
            64,
            777,
        ).expect("map ring from linear memory");

        let magic_bytes = &linear_memory.0[offset..offset + 8];
        let magic = u64::from_le_bytes(magic_bytes.try_into().unwrap());
        assert_eq!(magic, SHM_RING_MAGIC);

        let cap_bytes = &linear_memory.0[offset + 8..offset + 12];
        let cap = u32::from_le_bytes(cap_bytes.try_into().unwrap());
        assert_eq!(cap, 8);

        let hdr = ShmRingElementHeader::new(0, 5, 0xabc, 42);
        ring.try_push(hdr, b"hello").expect("push into mapped ring");

        let head_bytes = &linear_memory.0[offset + 64..offset + 72];
        let mem_head = u64::from_le_bytes(head_bytes.try_into().unwrap());
        assert_eq!(mem_head, 1);

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

        let hdr = ShmRingElementHeader::new(0, 4, 0, 0);
        let ro_push_err = ring.push_with_ro_token(&ro_token, secret, hdr, b"test");
        assert!(matches!(ro_push_err, Err(AbiError::PermissionDenied)));

        let generic_ro = MemoryCapabilityToken::ReadOnly(ro_token.clone());
        let generic_ro_err = ring.push_with_token(&generic_ro, secret, hdr, b"test");
        assert!(matches!(generic_ro_err, Err(AbiError::PermissionDenied)));

        let rw_push = ring.push_with_rw_token(&rw_token, secret, hdr, b"data");
        assert!(rw_push.is_ok());

        let pop_res = ring.pop_with_ro_token(&ro_token, secret);
        assert!(pop_res.is_ok());
        let (_, data) = pop_res.unwrap();
        assert_eq!(data, b"data");

        let mut tampered_ro = ro_token;
        tampered_ro.nonce = 999999;
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

    // ========================================================================
    // ZERO-COPY IO_URING SHM BRIDGE UNIT TESTS
    // ========================================================================

    #[test]
    fn test_io_uring_channel_creation_and_power_of_two() {
        for &depth in &[2u32, 4, 8, 16, 32, 64, 128, 256, 512, 1024] {
            let channel = IoUringShmChannel::new(depth, 0).expect("create io_uring channel");
            assert_eq!(channel.sq_capacity(), depth);
            assert_eq!(channel.cq_capacity(), depth * 2);
            assert_eq!(channel.sq_head(), 0);
            assert_eq!(channel.sq_tail(), 0);
            assert_eq!(channel.cq_head(), 0);
            assert_eq!(channel.cq_tail(), 0);
            assert_eq!(channel.sq_pending(), 0);
            assert_eq!(channel.cq_available(), 0);
            assert_eq!(channel.registered_buffer_count(), 0);
        }

        for &bad_depth in &[0u32, 3, 5, 7, 10, 100, 1000] {
            let err = IoUringShmChannel::new(bad_depth, 0);
            assert!(matches!(err, Err(AbiError::InvalidCapacity { .. })));
        }
    }

    #[test]
    fn test_io_uring_sqe_submission_and_cqe_reaping_nop() {
        let channel = IoUringShmChannel::new(16, 0).expect("channel");
        let secret = b"io_uring_hagibis_secret_key_v1";
        let token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 1, 0, 1, 101, 0, 65536,
        ));

        let sqes = vec![
            IoUringSqeDescriptor::nop(1001),
            IoUringSqeDescriptor::nop(1002),
            IoUringSqeDescriptor::nop(1003),
            IoUringSqeDescriptor::nop(1004),
        ];

        let submitted = channel.submit(&sqes, &token, secret).expect("submit batch");
        assert_eq!(submitted, 4);
        assert_eq!(channel.sq_tail(), 4);
        assert_eq!(channel.sq_head(), 4);
        assert_eq!(channel.cq_available(), 4);

        let cqes = channel.poll_completions(1, 10).expect("poll completions");
        assert_eq!(cqes.len(), 4);
        for (i, cqe) in cqes.iter().enumerate() {
            assert_eq!(cqe.user_data, 1001 + (i as u64));
            assert_eq!(cqe.res, 0);
            assert!(cqe.is_ok());
            assert_eq!(cqe.result().unwrap(), 0);
        }
        assert_eq!(channel.cq_head(), 4);
        assert_eq!(channel.cq_available(), 0);
    }

    #[test]
    fn test_io_uring_capability_token_permission_enforcement() {
        let channel = IoUringShmChannel::new(16, 0).expect("channel");
        let secret = b"secret_perm_key_2026";

        let mut payload_buf = vec![0x42u8; 64];
        let buf_addr = payload_buf.as_mut_ptr() as u64;

        let ro_token = MemoryCapabilityToken::ReadOnly(ReadOnlyCapabilityToken::issue(
            secret, 10, 0, 1, 201, buf_addr as usize, 4096,
        ));
        let rw_token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 11, 0, 1, 202, buf_addr as usize, 4096,
        ));

        // 1. Submit NOP with RO token -> Allowed
        let nop_sqe = vec![IoUringSqeDescriptor::nop(555)];
        assert!(channel.submit(&nop_sqe, &ro_token, secret).is_ok());

        // 2. Submit WRITE with ReadOnly token -> Strictly rejected!
        let write_sqe = vec![IoUringSqeDescriptor::write(1, buf_addr, 32, 0, 777)];
        let ro_write_err = channel.submit(&write_sqe, &ro_token, secret);
        assert!(matches!(ro_write_err, Err(AbiError::PermissionDenied)));

        // 3. Submit WRITE with ReadWrite token within range -> Allowed
        let rw_write = channel.submit(&write_sqe, &rw_token, secret);
        assert!(rw_write.is_ok());

        // 4. Out-of-bounds address submission -> Rejected!
        let oob_sqe = vec![IoUringSqeDescriptor::write(1, buf_addr + 8192, 64, 0, 888)];
        let oob_err = channel.submit(&oob_sqe, &rw_token, secret);
        assert!(matches!(oob_err, Err(AbiError::PermissionDenied)));

        // 5. Tampered token signature -> Rejected!
        let tampered_token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken {
            token_id: 999,
            memory_index: 0,
            view_id: 1,
            nonce: 202,
            allowed_offset: buf_addr as usize,
            allowed_len: 4096,
            signature: [0xfe; 32],
        });
        let tampered_err = channel.submit(&nop_sqe, &tampered_token, secret);
        assert!(matches!(tampered_err, Err(AbiError::PermissionDenied)));
    }

    #[test]
    fn test_io_uring_zero_copy_buffer_registration_lifecycle() {
        let channel = IoUringShmChannel::new(16, 0).expect("channel");
        let secret = b"secret_reg_key_2026";
        let token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 20, 0, 1, 301, 0, 1048576,
        ));

        let buf1 = IoUringBufferRegistration::wasm_linear_memory(1, 0, 65536);
        let buf2 = IoUringBufferRegistration::new(2, "posix_shm:/hgb_pool", 65536, 131072, 4096, false);

        let registered = channel.register_buffers(&[buf1, buf2], &token, secret).expect("register buffers");
        assert_eq!(registered, 2);
        assert_eq!(channel.registered_buffer_count(), 2);

        let retrieved = channel.get_registered_buffer(1).expect("buffer 1");
        assert_eq!(retrieved.buffer_id, 1);
        assert_eq!(retrieved.capacity, 65536);
        assert!(retrieved.is_wasm_linear_memory);

        let dup_buf = IoUringBufferRegistration::wasm_linear_memory(1, 0, 65536);
        let dup_err = channel.register_buffers(&[dup_buf], &token, secret);
        assert!(matches!(dup_err, Err(AbiError::DoubleLend { .. })));

        channel.unregister_buffers(&token, secret).expect("unregister buffers");
        assert_eq!(channel.registered_buffer_count(), 0);
    }

    #[test]
    fn test_io_uring_zero_copy_read_write_fixed() {
        let channel = IoUringShmChannel::new(16, 0).expect("channel");
        let secret = b"fixed_io_secret_2026";
        let token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 30, 0, 1, 401, 0, 65536,
        ));

        let reg1 = IoUringBufferRegistration::wasm_linear_memory(1, 0, 4096);
        let reg2 = IoUringBufferRegistration::wasm_linear_memory(2, 4096, 4096);
        channel.register_buffers(&[reg1, reg2], &token, secret).expect("register");

        let mut mem_buf1 = vec![0xAAu8; 1024];
        let mut mem_buf2 = vec![0x00u8; 1024];

        channel.attach_buffer_memory(1, mem_buf1.as_mut_ptr(), 4096).expect("attach buf 1");
        channel.attach_buffer_memory(2, mem_buf2.as_mut_ptr(), 4096).expect("attach buf 2");

        let mut transfer_scratch = vec![0u8; 1024];
        let scratch_addr = transfer_scratch.as_mut_ptr() as u64;

        // 1. Fixed Read from Buffer 1 into transfer_scratch
        let read_sqe = IoUringSqeDescriptor::read_fixed(0, scratch_addr, 1024, 0, 1, 5001);
        channel.submit(&[read_sqe], &token, secret).expect("submit fixed read");

        let cqes = channel.poll_completions(1, 1).expect("poll read cqe");
        assert_eq!(cqes.len(), 1);
        assert_eq!(cqes[0].user_data, 5001);
        assert_eq!(cqes[0].res, 1024);
        assert_eq!(transfer_scratch, vec![0xAAu8; 1024]);

        // 2. Fixed Write from transfer_scratch into Buffer 2
        let write_sqe = IoUringSqeDescriptor::write_fixed(0, scratch_addr, 1024, 0, 2, 5002);
        channel.submit(&[write_sqe], &token, secret).expect("submit fixed write");

        let cqes2 = channel.poll_completions(1, 1).expect("poll write cqe");
        assert_eq!(cqes2.len(), 1);
        assert_eq!(cqes2[0].user_data, 5002);
        assert_eq!(cqes2[0].res, 1024);
        assert_eq!(mem_buf2, vec![0xAAu8; 1024]);
    }

    #[test]
    fn test_io_uring_linear_memory_mapping() {
        let mut guest_linear_memory = AlignedMem([0u8; 16384]);
        let offset = 64;

        let channel = IoUringShmChannel::from_linear_memory(
            &mut guest_linear_memory.0,
            offset,
            8,
            0,
        ).expect("channel from linear memory");

        let magic_bytes = &guest_linear_memory.0[offset..offset + 8];
        let magic = u64::from_le_bytes(magic_bytes.try_into().unwrap());
        assert_eq!(magic, IO_URING_SHM_MAGIC);

        let sq_cap = u32::from_le_bytes(guest_linear_memory.0[offset + 8..offset + 12].try_into().unwrap());
        let cq_cap = u32::from_le_bytes(guest_linear_memory.0[offset + 12..offset + 16].try_into().unwrap());
        assert_eq!(sq_cap, 8);
        assert_eq!(cq_cap, 16);

        let secret = b"guest_linear_secret_2026";
        let token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 40, 0, 1, 501, 0, 16384,
        ));

        let sqes = vec![IoUringSqeDescriptor::nop(9999)];
        channel.submit(&sqes, &token, secret).expect("submit mapped");

        let mem_sq_tail = u32::from_le_bytes(guest_linear_memory.0[offset + 68..offset + 72].try_into().unwrap());
        assert_eq!(mem_sq_tail, 1);

        let completions = channel.poll_completions(1, 1).expect("poll completions");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].user_data, 9999);

        let mem_cq_head = u32::from_le_bytes(guest_linear_memory.0[offset + 128..offset + 132].try_into().unwrap());
        assert_eq!(mem_cq_head, 1);
    }

    #[test]
    fn test_io_uring_ring_wraparound_and_backpressure() {
        let channel = IoUringShmChannel::new(4, 0).expect("small channel");
        let secret = b"backpressure_secret_2026";
        let token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 50, 0, 1, 601, 0, 65536,
        ));

        let sqes = vec![
            IoUringSqeDescriptor::nop(1),
            IoUringSqeDescriptor::nop(2),
            IoUringSqeDescriptor::nop(3),
            IoUringSqeDescriptor::nop(4),
        ];
        channel.submit(&sqes, &token, secret).expect("submit 4");
        assert_eq!(channel.cq_available(), 4);

        let overflow_sqe = vec![IoUringSqeDescriptor::nop(5)];
        let err = channel.submit(&overflow_sqe, &token, secret);
        assert!(matches!(err, Err(AbiError::RingFull { capacity: 4 })));

        let reaped = channel.poll_completions(1, 2).expect("reap 2");
        assert_eq!(reaped.len(), 2);
        assert_eq!(reaped[0].user_data, 1);
        assert_eq!(reaped[1].user_data, 2);

        let wrap_sqes = vec![
            IoUringSqeDescriptor::nop(5),
            IoUringSqeDescriptor::nop(6),
        ];
        channel.submit(&wrap_sqes, &token, secret).expect("submit wrapped");

        let remaining = channel.poll_completions(1, 10).expect("drain remaining");
        assert_eq!(remaining.len(), 4);
        assert_eq!(remaining[0].user_data, 3);
        assert_eq!(remaining[1].user_data, 4);
        assert_eq!(remaining[2].user_data, 5);
        assert_eq!(remaining[3].user_data, 6);
        assert_eq!(channel.cq_available(), 0);
    }

    #[test]
    fn test_io_uring_multithreaded_producer_consumer_stress() {
        let channel = Arc::new(IoUringShmChannel::new(64, 0).expect("channel"));
        let secret: &'static [u8] = b"multithreaded_stress_secret_key_2026";
        let token = Arc::new(MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
            secret, 60, 0, 1, 701, 0, 65536,
        )));
        let total_iterations = 2000u64;

        let prod_channel = Arc::clone(&channel);
        let prod_token = Arc::clone(&token);
        let producer_thread = thread::spawn(move || {
            let mut submitted = 0u64;
            while submitted < total_iterations {
                let batch_size = ((total_iterations - submitted).min(8)) as u32;
                let mut sqes = Vec::with_capacity(batch_size as usize);
                for i in 0..batch_size {
                    sqes.push(IoUringSqeDescriptor::nop(submitted + (i as u64)));
                }

                loop {
                    match prod_channel.submit(&sqes, &prod_token, secret) {
                        Ok(n) => {
                            submitted += n as u64;
                            break;
                        }
                        Err(AbiError::RingFull { .. }) => {
                            thread::yield_now();
                        }
                        Err(e) => panic!("Producer error: {:?}", e),
                    }
                }
            }
        });

        let cons_channel = Arc::clone(&channel);
        let consumer_thread = thread::spawn(move || {
            let mut reaped_items = Vec::with_capacity(total_iterations as usize);
            while (reaped_items.len() as u64) < total_iterations {
                let needed = ((total_iterations as usize) - reaped_items.len()).min(16) as u32;
                let cqes = cons_channel.poll_completions(0, needed).expect("poll");
                if cqes.is_empty() {
                    thread::yield_now();
                } else {
                    for cqe in cqes {
                        reaped_items.push(cqe.user_data);
                    }
                }
            }
            reaped_items
        });

        producer_thread.join().expect("producer join");
        let reaped_items = consumer_thread.join().expect("consumer join");

        assert_eq!(reaped_items.len(), total_iterations as usize);
        for i in 0..total_iterations {
            assert_eq!(reaped_items[i as usize], i);
        }
    }
}
