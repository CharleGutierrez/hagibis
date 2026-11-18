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
//! - **`InteropBenchmarkReport` / `benchmark_interop_latency`**: Real-time cycle and nanosecond
//!   latency measurement comparing zero-copy shared memory views against serde JSON.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use serde::{Deserialize, Serialize};

// ============================================================================
// CONSTANTS & ACTION CODES
// ============================================================================

/// Magic signature for Canonical ABI Invocation Header: ASCII "HGBABI01" (0x4847424142493031)
pub const CANONICAL_ABI_HEADER_MAGIC: u64 = 0x4847424142493031;

/// Exact byte size of the 64-byte aligned Canonical ABI Invocation Header
pub const CANONICAL_HEADER_SIZE: usize = 64;

/// Required alignment for NPU DMA transfers and Canonical ABI headers
pub const CANONICAL_ALIGNMENT_DEFAULT: usize = 64;

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
}

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
        }
    }
}

impl std::error::Error for AbiError {}

// ============================================================================
// POD COPY MARKER TRAIT FOR ZERO-COPY TRANSMUTATION
// ============================================================================

/// Marker trait representing Plain Old Data types safe for zero-copy memory transmutation.
///
/// # Safety
/// Types implementing this trait must have no internal padding invariants that can be violated
/// by arbitrary bit patterns, and must be valid for `Copy` and `'static`.
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
    /// Construct a new zero-copy read-only view over a linear memory buffer
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

        // Verify logical offset alignment
        verify_alignment(offset, alignment)?;

        // Verify host virtual memory pointer alignment
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

    /// View unique identifier
    #[inline(always)]
    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    /// Offset into linear memory
    #[inline(always)]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Length in bytes
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Is the view empty?
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Alignment requirement in bytes
    #[inline(always)]
    pub fn alignment(&self) -> usize {
        self.alignment
    }

    /// Memory access permissions
    #[inline(always)]
    pub fn access_mode(&self) -> MemoryAccessMode {
        self.access_mode
    }

    /// Borrow the underlying byte slice
    #[inline(always)]
    pub fn as_slice(&self) -> &'a [u8] {
        self.slice
    }

    /// Zero-copy transmute view into a typed slice of type `T: PodCopy`
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

    /// Direct zero-copy accessor for IEEE 754 f32 slices
    #[inline(always)]
    pub fn as_f32_slice(&self) -> Result<&'a [f32], AbiError> {
        self.as_typed_slice::<f32>()
    }

    /// Direct zero-copy accessor for u32 slices
    #[inline(always)]
    pub fn as_u32_slice(&self) -> Result<&'a [u32], AbiError> {
        self.as_typed_slice::<u32>()
    }

    /// Direct zero-copy accessor for u64 slices
    #[inline(always)]
    pub fn as_u64_slice(&self) -> Result<&'a [u64], AbiError> {
        self.as_typed_slice::<u64>()
    }

    /// Direct zero-copy accessor for i32 slices
    #[inline(always)]
    pub fn as_i32_slice(&self) -> Result<&'a [i32], AbiError> {
        self.as_typed_slice::<i32>()
    }

    /// Create a zero-copy subview window within this view
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
    /// Construct a new zero-copy mutable view over a linear memory buffer
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

    /// View unique identifier
    #[inline(always)]
    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    /// Offset into linear memory
    #[inline(always)]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Length in bytes
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Is the view empty?
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrow the underlying byte slice
    #[inline(always)]
    pub fn as_slice(&self) -> &[u8] {
        self.slice
    }

    /// Mutably borrow the underlying byte slice
    #[inline(always)]
    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        self.slice
    }

    /// Zero-copy transmute view into a mutable typed slice of type `T: PodCopy`
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

    /// Direct zero-copy mutable accessor for IEEE 754 f32 slices
    #[inline(always)]
    pub fn as_f32_slice_mut(&mut self) -> Result<&mut [f32], AbiError> {
        self.as_typed_slice_mut::<f32>()
    }

    /// Direct zero-copy mutable accessor for u32 slices
    #[inline(always)]
    pub fn as_u32_slice_mut(&mut self) -> Result<&mut [u32], AbiError> {
        self.as_typed_slice_mut::<u32>()
    }

    /// Write bytes into this mutable view
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
/// Exchanged across component boundaries without serialization overhead.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalInvocationHeader {
    /// Magic signature: 0x4847424142493031 ("HGBABI01")
    pub magic: u64,
    /// Protocol version: 1
    pub protocol_version: u32,
    /// Invocation action code (e.g. ACTION_TOOL_INVOKE, ACTION_VECTOR_COMPUTE)
    pub action_code: u32,
    /// Request correlation ID
    pub correlation_id: u64,
    /// Bitflags modifying payload interpretation
    pub payload_flags: u32,
    /// Byte offset where argument payload begins in linear memory
    pub arg_offset: u32,
    /// Length of argument payload in bytes
    pub arg_length: u32,
    /// Byte offset where return buffer begins in linear memory
    pub return_offset: u32,
    /// Maximum capacity allocated for return buffer
    pub return_capacity: u32,
    /// Actual length of return data written by guest (0 on entry)
    pub return_length: u32,
    /// Status code (0 = success, non-zero = error code)
    pub status_code: u32,
    /// Reserved padding to enforce exact 64-byte header size
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
    /// Create a new invocation header with designated arguments and return buffer ranges
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

    /// Serialize header to a fixed 64-byte array in little-endian byte order
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

    /// Parse header from a byte slice, validating 64-byte length and magic signature
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

    /// Write header in-place into memory slice
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

// Compile-time assertion that CanonicalInvocationHeader size is exactly 64 bytes
const _: () = assert!(std::mem::size_of::<CanonicalInvocationHeader>() == 64);
const _: () = assert!(std::mem::align_of::<CanonicalInvocationHeader>() == 64);

// ============================================================================
// CANONICAL ABI INVOCATION PAYLOAD
// ============================================================================

/// Action invocation payload bypassing serde JSON/bincode
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalAbiInvocationPayload {
    /// 64-byte invocation header
    pub header: CanonicalInvocationHeader,
    /// Optional attached pinned tensor descriptor for zero-copy NPU routing
    pub pinned_tensor: Option<PinnedTensorDescriptor>,
}

impl CanonicalAbiInvocationPayload {
    /// Create new invocation payload
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

    /// Attach a pinned tensor descriptor for zero-copy NPU routing
    pub fn with_pinned_tensor(mut self, tensor: PinnedTensorDescriptor) -> Self {
        self.pinned_tensor = Some(tensor);
        self
    }

    /// Read argument byte slice directly from linear memory without heap copying
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

    /// Read argument slice directly as typed `&[T]` without heap copying
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

    /// Write return bytes directly into linear memory without heap copying
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

    /// Write typed items directly into return buffer
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

    /// Encode payload header directly into memory at a 64-byte aligned offset
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

    /// Decode payload header directly from memory at a 64-byte aligned offset
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
/// Prevents aliasing, double-lending, and dangling views.
#[derive(Debug)]
pub struct CanonicalMemoryLender {
    capacity: usize,
    active_borrows: HashMap<u64, ActiveBorrowRecord>,
    next_view_id: u64,
    epoch: u64,
}

impl CanonicalMemoryLender {
    /// Construct a new memory lender governing a linear memory buffer of `capacity` bytes
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            active_borrows: HashMap::new(),
            next_view_id: 1,
            epoch: 1,
        }
    }

    /// Linear memory capacity governed by this lender
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of actively lent memory views
    pub fn active_borrows_count(&self) -> usize {
        self.active_borrows.len()
    }

    /// Check if a view ID is currently actively lent
    pub fn is_lent(&self, view_id: u64) -> bool {
        self.active_borrows.contains_key(&view_id)
    }

    /// Validate whether a candidate region can be lent without conflicts
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

            // Check if intervals [offset..req_end] and [active.offset..active_end] intersect
            let intersects = offset < active_end && req_end > active.offset;

            if intersects {
                // If either lease is mutable (ReadWrite or ExclusiveWrite), conflict!
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

    /// Lend a region of linear memory, creating a validated `TypedShmViewDescriptor`
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

    /// Reclaim a previously lent shared memory view
    pub fn reclaim(&mut self, view_id: u64) -> Result<(), AbiError> {
        if self.active_borrows.remove(&view_id).is_none() {
            return Err(AbiError::ViewNotFound { view_id });
        }
        self.epoch += 1;
        Ok(())
    }

    /// Lend with an RAII guard that automatically reclaims the view when dropped
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
    /// Active view ID
    pub fn view_id(&self) -> u64 {
        self.view_id
    }

    /// Attached typed view descriptor
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
    /// Create and validate a new contiguous row-major pinned tensor descriptor
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

        // Calculate standard contiguous row-major strides
        let mut strides = vec![1; shape.len()];
        for i in (0..shape.len() - 1).rev() {
            strides[i] = strides[i + 1] * shape[i + 1];
        }

        // For NPU DMA acceleration, enforce 64-byte alignment
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

    /// Total number of elements in tensor
    pub fn element_count(&self) -> usize {
        self.shape.iter().product()
    }

    /// Validate that tensor byte range fits within parent shared memory view
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

    /// Calculate the flattened 1D index given multi-dimensional coordinates
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
    /// Number of iteration passes executed
    pub iterations: usize,
    /// Number of float elements in vector payload
    pub payload_floats: usize,
    /// Raw payload bytes transferred
    pub payload_bytes: usize,
    /// Total duration for serde JSON serialization/deserialization (nanoseconds)
    pub serde_json_duration_nanos: u64,
    /// Total duration for Canonical ABI zero-copy views (nanoseconds)
    pub zero_copy_duration_nanos: u64,
    /// Measured latency reduction ratio (serde_json_duration / zero_copy_duration)
    pub speedup_ratio: f64,
    /// Estimated CPU cycles consumed by serde JSON
    pub estimated_cycles_serde: u64,
    /// Estimated CPU cycles consumed by Canonical ABI
    pub estimated_cycles_zero_copy: u64,
    /// Estimated CPU cycles saved per invocation
    pub cycles_saved: u64,
    /// Heap allocations incurred by serde JSON (bytes)
    pub serde_heap_allocations_bytes: usize,
    /// Heap allocations incurred by Canonical ABI (0 bytes!)
    pub zero_copy_heap_allocations_bytes: usize,
}

#[derive(Serialize, Deserialize)]
struct JsonPayloadDummy {
    action: u32,
    correlation_id: u64,
    tensor: Vec<f32>,
}

/// Execute a realistic latency comparison benchmark comparing zero-copy shared memory
/// view extraction against standard serde JSON serialization and parsing.
pub fn benchmark_interop_latency(payload_floats: usize, iterations: usize) -> InteropBenchmarkReport {
    let payload_bytes = payload_floats * 4;

    // Prepare synthetic embedding vector
    let mut float_vec = Vec::with_capacity(payload_floats);
    for i in 0..payload_floats {
        float_vec.push((i as f32) * 0.001);
    }

    // ------------------------------------------------------------------------
    // Benchmark 1: Serde JSON
    // ------------------------------------------------------------------------
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

    // ------------------------------------------------------------------------
    // Benchmark 2: Canonical ABI Zero-Copy Shared Memory View
    // ------------------------------------------------------------------------
    // Allocate 64-byte aligned linear memory buffer
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

    // Populate argument bytes in linear memory once
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

    // Conservative cycle estimation assuming ~3.0 GHz CPU frequency (3 cycles per nanosecond)
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

    #[repr(align(64))]
    struct AlignedMem<const N: usize>([u8; N]);

    #[test]
    fn test_zero_copy_view_creation_and_bounds() {
        let memory = AlignedMem([0u8; 1024]);
        let mem_slice = &memory.0[..];

        // Valid view
        let view = CanonicalAbiShmView::new(1, mem_slice, 0, 512, 64, MemoryAccessMode::ReadOnly)
            .expect("valid view");
        assert_eq!(view.len(), 512);
        assert_eq!(view.offset(), 0);
        assert_eq!(view.view_id(), 1);

        // Out of bounds view
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

        // 64-byte alignment at offset 0
        assert!(CanonicalAbiShmView::new(1, mem_slice, 0, 64, 64, MemoryAccessMode::ReadOnly).is_ok());

        // Misaligned offset 3 for 4-byte alignment
        let err = CanonicalAbiShmView::new(2, mem_slice, 3, 64, 4, MemoryAccessMode::ReadOnly);
        assert!(matches!(err, Err(AbiError::AlignmentFault { .. })));

        // Non-power-of-two alignment
        let err_bad_align = verify_alignment(64, 15);
        assert!(matches!(err_bad_align, Err(AbiError::AlignmentFault { .. })));
    }

    #[test]
    fn test_typed_slice_zero_copy_f32_u32() {
        let mut memory = vec![0u8; 512];

        // Write known f32 floats into memory
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

        // Write with mutable view
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

        // Lend ReadWrite region [0..1024]
        let view1 = lender.lend(0, 1024, 64, MemoryAccessMode::ReadWrite, "tensor_a")
            .expect("lend view 1");
        assert_eq!(view1.view_id, 1);

        // Attempt overlapping borrow [512..1536] (ReadWrite) -> must fail
        let conflict1 = lender.lend(512, 1024, 64, MemoryAccessMode::ReadWrite, "tensor_b");
        assert!(matches!(conflict1, Err(AbiError::BorrowConflict { .. })));

        // Attempt overlapping borrow [512..1536] (ReadOnly) -> must fail because view1 is ReadWrite
        let conflict2 = lender.lend(512, 1024, 64, MemoryAccessMode::ReadOnly, "tensor_c");
        assert!(matches!(conflict2, Err(AbiError::BorrowConflict { .. })));

        // Non-overlapping borrow [1024..2048] -> must succeed
        let view2 = lender.lend(1024, 1024, 64, MemoryAccessMode::ReadWrite, "tensor_d");
        assert!(view2.is_ok());
    }

    #[test]
    fn test_borrow_shared_readonly_overlap_allowed() {
        let mut lender = CanonicalMemoryLender::new(4096);

        // Lend ReadOnly region [0..1024]
        let view1 = lender.lend(0, 1024, 64, MemoryAccessMode::ReadOnly, "shared_a")
            .expect("lend readonly 1");
        assert_eq!(view1.view_id, 1);

        // Overlapping ReadOnly region [512..1536] -> permitted
        let view2 = lender.lend(512, 1024, 64, MemoryAccessMode::ReadOnly, "shared_b")
            .expect("lend readonly 2");
        assert_eq!(view2.view_id, 2);

        // Overlapping ReadWrite -> denied
        let conflict = lender.lend(512, 1024, 64, MemoryAccessMode::ReadWrite, "writer");
        assert!(matches!(conflict, Err(AbiError::BorrowConflict { .. })));
    }

    #[test]
    fn test_lifecycle_reclaim_and_relend() {
        let mut lender = CanonicalMemoryLender::new(2048);

        let view1 = lender.lend(0, 1024, 64, MemoryAccessMode::ExclusiveWrite, "lease_1")
            .expect("lend view 1");
        assert_eq!(lender.active_borrows_count(), 1);

        // Reclaim view 1
        lender.reclaim(view1.view_id).expect("reclaim");
        assert_eq!(lender.active_borrows_count(), 0);

        // Reclaiming already reclaimed view must error
        let err = lender.reclaim(view1.view_id);
        assert!(matches!(err, Err(AbiError::ViewNotFound { .. })));

        // Re-lending same region now succeeds
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
            // While guard is alive, lender has 1 borrow
        }
        // Guard was dropped: view should be automatically reclaimed
        assert_eq!(lender.active_borrows_count(), 0);

        // Can now lend same region immediately
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

        // Corrupt magic
        let mut corrupt = bytes;
        corrupt[0] = 0xff;
        let err = CanonicalInvocationHeader::from_bytes(&corrupt);
        assert!(matches!(err, Err(AbiError::InvalidMagic { .. })));
    }

    #[test]
    fn test_pinned_tensor_npu_descriptor_validation() {
        // 64-byte aligned offset for NPU Direct
        let desc = PinnedTensorDescriptor::new_contiguous(
            10,
            1,
            TensorDataType::F32,
            vec![2, 3, 4], // 24 elements * 4 = 96 bytes
            64,
            NpuDeviceTarget::NpuDirect,
        ).expect("create pinned tensor");

        assert_eq!(desc.element_count(), 24);
        assert_eq!(desc.byte_length, 96);
        assert_eq!(desc.strides, vec![12, 4, 1]);

        // Multi-dim linear index check: [1, 2, 3] -> 1*12 + 2*4 + 3*1 = 23
        assert_eq!(desc.linear_index(&[1, 2, 3]).unwrap(), 23);

        // Misaligned offset for NPU Direct (offset 16 != 0 mod 64)
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

        // Subview at offset 64 with len 128, aligned to 64
        let sub = parent.subview(64, 128, 64).expect("subview");
        assert_eq!(sub.offset(), 64);
        assert_eq!(sub.len(), 128);

        // Out-of-bounds subview
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
}
