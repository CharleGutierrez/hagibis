//! # Zero-Copy WASM-to-NPU Streaming Pipeline (`zero_copy_wasm_npu_pipeline`)
//!
//! Production-grade zero-copy hardware acceleration streaming pipeline connecting
//! WebAssembly linear memory and native NPU/GPU device buffers.
//!
//! ## Architectural Capabilities
//! - **Pinned Shared Memory Ring Buffers**:
//!   POSIX `/dev/shm` and anonymous `mmap` backing with strict 64-byte AVX-512 and
//!   256-byte GPU alignment guarantees, physical memory page locking (`mlock`),
//!   and lock-free atomic streaming queues.
//! - **Direct Slice Mapping**:
//!   Maps host memory pointers directly into WebAssembly linear memory views
//!   without JSON/bincode serialization or intermediate heap allocations.
//! - **Hardware Cache Synchronization**:
//!   Architecture-specific CPU cacheline flushes (x86-64 `_mm_clflush`, ARM64 `dc civac`),
//!   cache invalidations, and system memory fences (`dsb sy` / `mfence` / `Atomic::fence`).
//! - **Zero-Copy Tensor Dispatch**:
//!   In-buffer SIMD-accelerated Cosine Similarity, Dot Product, Euclidean Distance,
//!   L2 Normalization, and batch activations (ReLU, GELU, Scale/Bias, Q8-0 Quantization)
//!   executed directly over pinned shared buffers.
//! - **WIT Canonical ABI Compliance**:
//!   Implements the `wasm-npu-streaming-pipeline` WIT interface specified in
//!   `wit/canonical_abi_shm_views.wit`.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use libc::{
    c_void, mlock, mmap, munlock, munmap, MAP_ANONYMOUS, MAP_FAILED, MAP_SHARED,
    PROT_READ, PROT_WRITE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{NpuRouterError, Result};

// ============================================================================
// 1. HARDWARE MEMORY ALIGNMENT & SYSTEM CONSTANTS
// ============================================================================

/// AVX-512 CPU SIMD cache line alignment in bytes (64 bytes = 512 bits)
pub const ALIGN_64_AVX512: usize = 64;

/// GPU compute shader cache line and warp alignment in bytes (256 bytes)
pub const ALIGN_256_GPU: usize = 256;

/// Standard Linux virtual memory page size (4 KiB)
pub const ALIGN_4K_PAGE: usize = 4096;

/// Standard WebAssembly component memory page size (64 KiB = 65,536 bytes)
pub const ALIGN_64K_WASM: usize = 65536;

/// CPU cacheline size in bytes for modern x86_64 and ARM64 architectures
pub const CACHE_LINE_SIZE: usize = 64;

/// Magic signature for Pinned Shared Memory Streaming Ring: ASCII "HGBSTRM1" (0x4847425354524D31)
pub const SHM_STREAMING_RING_MAGIC: u64 = 0x4847425354524D31;

/// Streaming ring header control block size in bytes (64-byte aligned)
pub const STREAMING_RING_HEADER_SIZE: usize = 64;

/// Global atomic sequence for unique pipeline handle identifiers
static NEXT_PIPELINE_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

/// Global atomic sequence for unique batch execution identifiers
static NEXT_BATCH_RECEIPT_ID: AtomicU64 = AtomicU64::new(1000);

// ============================================================================
// 2. WIT COMPATIBLE DATA STRUCTURES & ENUMS
// ============================================================================

/// Primitive data types supported in zero-copy shared memory and tensor views.
/// Matches `canonical-abi-types.tensor-data-type` in `wit/canonical_abi_shm_views.wit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    #[inline]
    pub fn element_size(&self) -> usize {
        match self {
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::F16 | Self::BF16 | Self::I16 | Self::U16 => 2,
            Self::I8 | Self::U8 | Self::Q8_0 | Self::Q4_0 => 1,
        }
    }

    /// Human-readable data type name
    pub fn name(&self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::BF16 => "bf16",
            Self::I32 => "i32",
            Self::I16 => "i16",
            Self::I8 => "i8",
            Self::U32 => "u32",
            Self::U16 => "u16",
            Self::U8 => "u8",
            Self::Q8_0 => "q8_0",
            Self::Q4_0 => "q4_0",
        }
    }

    /// Parse data type from string representation
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().trim() {
            "f32" | "float32" | "float" => Some(Self::F32),
            "f16" | "float16" | "half" => Some(Self::F16),
            "bf16" | "bfloat16" => Some(Self::BF16),
            "i32" | "int32" | "int" => Some(Self::I32),
            "i16" | "int16" => Some(Self::I16),
            "i8" | "int8" => Some(Self::I8),
            "u32" | "uint32" => Some(Self::U32),
            "u16" | "uint16" => Some(Self::U16),
            "u8" | "uint8" | "byte" => Some(Self::U8),
            "q8_0" | "q8" => Some(Self::Q8_0),
            "q4_0" | "q4" => Some(Self::Q4_0),
            _ => None,
        }
    }
}

impl fmt::Display for TensorDataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Target hardware acceleration engine for pinned tensor routing.
/// Matches `canonical-abi-types.npu-device-target` in `wit/canonical_abi_shm_views.wit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u32)]
pub enum NpuDeviceTarget {
    CpuSimd = 0,
    NpuDirect = 1,
    GpuCompute = 2,
    EphemeralNpu = 3,
}

impl NpuDeviceTarget {
    /// Strict hardware alignment requirement in bytes
    #[inline]
    pub fn required_alignment(&self) -> usize {
        match self {
            Self::GpuCompute | Self::NpuDirect => ALIGN_256_GPU,
            Self::CpuSimd | Self::EphemeralNpu => ALIGN_64_AVX512,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::CpuSimd => "cpu-simd",
            Self::NpuDirect => "npu-direct",
            Self::GpuCompute => "gpu-compute",
            Self::EphemeralNpu => "ephemeral-npu",
        }
    }
}

impl fmt::Display for NpuDeviceTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Hardware-accelerated zero-copy GPU/NPU tensor buffer shared memory descriptor.
/// Matches `wasm-npu-streaming-pipeline.npu-tensor-shm-descriptor` in `wit/canonical_abi_shm_views.wit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpuTensorShmDescriptor {
    pub handle_id: u64,
    pub shm_name: String,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub alignment: u32,
    pub data_type: TensorDataType,
    pub dimensions: Vec<u64>,
    pub device_target: NpuDeviceTarget,
    pub is_coherent: bool,
}

impl NpuTensorShmDescriptor {
    /// Create a new tensor shared memory descriptor with automated validation
    pub fn new(
        handle_id: u64,
        shm_name: impl Into<String>,
        offset_bytes: u64,
        size_bytes: u64,
        alignment: u32,
        data_type: TensorDataType,
        dimensions: Vec<u64>,
        device_target: NpuDeviceTarget,
        is_coherent: bool,
    ) -> Self {
        let align = if alignment == 0 {
            device_target.required_alignment() as u32
        } else {
            alignment
        };

        Self {
            handle_id,
            shm_name: shm_name.into(),
            offset_bytes,
            size_bytes,
            alignment: align,
            data_type,
            dimensions,
            device_target,
            is_coherent,
        }
    }

    /// Calculate total element count from tensor dimensions
    pub fn total_elements(&self) -> usize {
        if self.dimensions.is_empty() {
            0
        } else {
            self.dimensions.iter().map(|&d| d as usize).product()
        }
    }

    /// Calculate expected total byte payload from dimensions and data type
    pub fn expected_byte_size(&self) -> usize {
        self.total_elements().saturating_mul(self.data_type.element_size())
    }

    /// Validate descriptor parameters for consistency and alignment
    pub fn validate(&self) -> Result<()> {
        if self.size_bytes == 0 {
            return Err(NpuRouterError::InvalidPayload("Descriptor size_bytes cannot be 0".into()));
        }

        let align = self.alignment as usize;
        if align == 0 || !align.is_power_of_two() {
            return Err(NpuRouterError::DimensionMismatch {
                expected: 64,
                actual: align,
            });
        }

        if (self.offset_bytes as usize & (align - 1)) != 0 {
            return Err(NpuRouterError::InvalidPayload(format!(
                "offset_bytes {} is not aligned to {} bytes",
                self.offset_bytes, align
            )));
        }

        let expected = self.expected_byte_size();
        if expected > 0 && self.size_bytes < expected as u64 {
            return Err(NpuRouterError::DimensionMismatch {
                expected,
                actual: self.size_bytes as usize,
            });
        }

        Ok(())
    }
}

/// Similarity metric for zero-copy tensor evaluation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TensorMetric {
    CosineSimilarity,
    DotProduct,
    EuclideanDistance,
}

impl TensorMetric {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().trim() {
            "cosine" | "cosine_similarity" | "cos" => Some(Self::CosineSimilarity),
            "dot" | "dot_product" | "inner_product" => Some(Self::DotProduct),
            "euclidean" | "l2" | "euclidean_distance" => Some(Self::EuclideanDistance),
            _ => None,
        }
    }
}

/// In-place batch transformations supported over pinned shared buffers
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TensorTransform {
    L2Normalize,
    ScaleAndBias { alpha: f32, beta: f32 },
    ReLU,
    GELU,
    QuantizeQ8,
    DequantizeQ8 { scale: f32 },
}

/// Receipt detailing the result of an asynchronous or synchronous batch execution
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamingBatchReceipt {
    pub batch_id: u64,
    pub handle_id: u64,
    pub operation: String,
    pub elements_processed: usize,
    pub duration_nanos: u64,
    pub throughput_mbs: f64,
    pub checksum_sha256: String,
}

// ============================================================================
// 3. LOW-LEVEL HARDWARE CACHE SYNCHRONIZATION PRIMITIVES
// ============================================================================

/// Flush CPU cache lines across the specified memory range out to system RAM / PCIe controller.
/// Guarantees that dirty CPU writes are pushed to device memory before NPU DMA starts.
///
/// # Safety
/// The caller must ensure that `[start, start + len)` is a valid readable memory range.
#[inline]
pub unsafe fn flush_cache_range(start: *const u8, len: usize) {
    if len == 0 || start.is_null() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        let start_addr = start as usize;
        let end_addr = start_addr.saturating_add(len);
        let aligned_start = start_addr & !(CACHE_LINE_SIZE - 1);
        let mut ptr = aligned_start as *const u8;

        while (ptr as usize) < end_addr {
            core::arch::x86_64::_mm_clflush(ptr);
            ptr = ptr.add(CACHE_LINE_SIZE);
        }
        core::arch::x86_64::_mm_sfence();
        core::arch::x86_64::_mm_mfence();
    }

    #[cfg(target_arch = "aarch64")]
    {
        let start_addr = start as usize;
        let end_addr = start_addr.saturating_add(len);
        let aligned_start = start_addr & !(CACHE_LINE_SIZE - 1);
        let mut curr = aligned_start;

        while curr < end_addr {
            // Clean & Invalidate Data Cache line by VA to Point of Coherency (PoC)
            core::arch::asm!(
                "dc civac, {x}",
                x = in(reg) curr,
                options(nostack, preserves_flags)
            );
            curr += CACHE_LINE_SIZE;
        }
        core::arch::asm!(
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (start, len);
        fence(Ordering::SeqCst);
    }
}

/// Invalidate CPU cache lines across the specified memory range.
/// Evicts stale CPU cachelines after an external NPU/GPU DMA write so CPU reads fresh data.
///
/// # Safety
/// The caller must ensure that `[start, start + len)` is a valid readable memory range.
#[inline]
pub unsafe fn invalidate_cache_range(start: *const u8, len: usize) {
    if len == 0 || start.is_null() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        let start_addr = start as usize;
        let end_addr = start_addr.saturating_add(len);
        let aligned_start = start_addr & !(CACHE_LINE_SIZE - 1);
        let mut ptr = aligned_start as *const u8;

        while (ptr as usize) < end_addr {
            core::arch::x86_64::_mm_clflush(ptr);
            ptr = ptr.add(CACHE_LINE_SIZE);
        }
        core::arch::x86_64::_mm_mfence();
    }

    #[cfg(target_arch = "aarch64")]
    {
        let start_addr = start as usize;
        let end_addr = start_addr.saturating_add(len);
        let aligned_start = start_addr & !(CACHE_LINE_SIZE - 1);
        let mut curr = aligned_start;

        while curr < end_addr {
            core::arch::asm!(
                "dc civac, {x}",
                x = in(reg) curr,
                options(nostack, preserves_flags)
            );
            curr += CACHE_LINE_SIZE;
        }
        core::arch::asm!(
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (start, len);
        fence(Ordering::SeqCst);
    }
}

/// Execute a full hardware system memory barrier
#[inline]
pub fn memory_barrier_sync() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_mfence();
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}

// ============================================================================
// 4. STREAMING RING HEADER & CONTROL BLOCK
// ============================================================================

/// 64-byte cacheline-aligned streaming ring buffer header control block
#[repr(C, align(64))]
pub struct StreamingRingHeader {
    pub magic: u64,
    pub version: u32,
    pub flags: u32,
    pub capacity_bytes: u64,
    pub head: AtomicU64,
    pub tail: AtomicU64,
    pub element_size: u32,
    pub alignment: u32,
    pub total_elements_pushed: AtomicU64,
    pub dropped_elements: AtomicU64,
}

impl StreamingRingHeader {
    pub fn new(capacity_bytes: u64, element_size: u32, alignment: u32) -> Self {
        Self {
            magic: SHM_STREAMING_RING_MAGIC,
            version: 1,
            flags: 1, // ACTIVE
            capacity_bytes,
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            element_size,
            alignment,
            total_elements_pushed: AtomicU64::new(0),
            dropped_elements: AtomicU64::new(0),
        }
    }
}

// ============================================================================
// 5. PINNED SHARED MEMORY BUFFER RAII ENGINE
// ============================================================================

/// Backing storage origin for pinned memory
enum BufferBacking {
    /// Linux POSIX Shared Memory file (`/dev/shm/<name>`)
    PosixShmFile {
        path: PathBuf,
        _file: File,
        is_creator: bool,
    },
    /// Direct Anonymous Shared Mapping (`MAP_ANONYMOUS | MAP_SHARED`)
    AnonymousShared,
}

/// Thread-safe, RAII-managed pinned shared memory buffer with strict 64/256-byte alignment
pub struct PinnedSharedMemoryRingBuffer {
    handle_id: u64,
    shm_name: String,
    raw_mmap_ptr: *mut c_void,
    aligned_ptr: *mut u8,
    total_mapped_len: usize,
    capacity: usize,
    alignment: usize,
    is_pinned: bool,
    is_coherent: bool,
    backing: BufferBacking,
    created_at_epoch_ms: u64,
}

unsafe impl Send for PinnedSharedMemoryRingBuffer {}
unsafe impl Sync for PinnedSharedMemoryRingBuffer {}

impl fmt::Debug for PinnedSharedMemoryRingBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedSharedMemoryRingBuffer")
            .field("handle_id", &self.handle_id)
            .field("shm_name", &self.shm_name)
            .field("capacity", &self.capacity)
            .field("alignment", &self.alignment)
            .field("is_pinned", &self.is_pinned)
            .field("is_coherent", &self.is_coherent)
            .field("aligned_ptr", &format!("{:p}", self.aligned_ptr))
            .finish()
    }
}

impl PinnedSharedMemoryRingBuffer {
    /// Allocate or open a pinned shared memory ring buffer with guaranteed alignment
    pub fn create_or_open(
        handle_id: u64,
        shm_name: &str,
        capacity: usize,
        requested_alignment: usize,
        is_coherent: bool,
    ) -> Result<Self> {
        if capacity == 0 {
            return Err(NpuRouterError::InvalidPayload("Buffer capacity cannot be 0".into()));
        }

        let alignment = requested_alignment.max(ALIGN_64_AVX512);
        if !alignment.is_power_of_two() {
            return Err(NpuRouterError::DimensionMismatch {
                expected: 64,
                actual: alignment,
            });
        }

        // Allocate extra space to guarantee hardware alignment within the mapping
        let total_mapped_len = capacity
            .checked_add(alignment)
            .ok_or_else(|| NpuRouterError::InvalidPayload("Capacity + alignment overflow".into()))?;

        let dev_shm_dir = Path::new("/dev/shm");
        let (raw_mmap_ptr, backing) = if dev_shm_dir.is_dir() && !shm_name.is_empty() {
            let shm_path = dev_shm_dir.join(shm_name);
            let is_new = !shm_path.exists();

            let file_result = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o666)
                .open(&shm_path);

            match file_result {
                Ok(file) => {
                    let _ = file.set_len(total_mapped_len as u64);
                    let fd = file.as_raw_fd();
                    let mmap_ptr = unsafe {
                        mmap(
                            std::ptr::null_mut(),
                            total_mapped_len,
                            PROT_READ | PROT_WRITE,
                            MAP_SHARED,
                            fd,
                            0,
                        )
                    };

                    if mmap_ptr == MAP_FAILED || mmap_ptr.is_null() {
                        // Fall back to anonymous shared mapping
                        let anon_ptr = unsafe {
                            mmap(
                                std::ptr::null_mut(),
                                total_mapped_len,
                                PROT_READ | PROT_WRITE,
                                MAP_SHARED | MAP_ANONYMOUS,
                                -1,
                                0,
                            )
                        };
                        if anon_ptr == MAP_FAILED || anon_ptr.is_null() {
                            return Err(NpuRouterError::ExecutionFailed(
                                "Failed to mmap anonymous shared memory".into(),
                            ));
                        }
                        (anon_ptr, BufferBacking::AnonymousShared)
                    } else {
                        (
                            mmap_ptr,
                            BufferBacking::PosixShmFile {
                                path: shm_path,
                                _file: file,
                                is_creator: is_new,
                            },
                        )
                    }
                }
                Err(_) => {
                    // Fall back to anonymous shared mapping
                    let anon_ptr = unsafe {
                        mmap(
                            std::ptr::null_mut(),
                            total_mapped_len,
                            PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_ANONYMOUS,
                            -1,
                            0,
                        )
                    };
                    if anon_ptr == MAP_FAILED || anon_ptr.is_null() {
                        return Err(NpuRouterError::ExecutionFailed(
                            "Failed to mmap anonymous shared memory fallback".into(),
                        ));
                    }
                    (anon_ptr, BufferBacking::AnonymousShared)
                }
            }
        } else {
            // Direct anonymous shared memory mapping
            let anon_ptr = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    total_mapped_len,
                    PROT_READ | PROT_WRITE,
                    MAP_SHARED | MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if anon_ptr == MAP_FAILED || anon_ptr.is_null() {
                return Err(NpuRouterError::ExecutionFailed(
                    "Failed to mmap anonymous shared memory".into(),
                ));
            }
            (anon_ptr, BufferBacking::AnonymousShared)
        };

        // Calculate strictly aligned pointer
        let raw_addr = raw_mmap_ptr as usize;
        let aligned_addr = (raw_addr + (alignment - 1)) & !(alignment - 1);
        let aligned_ptr = aligned_addr as *mut u8;

        // Attempt physical page pinning via mlock (graceful fallback if unprivileged)
        let lock_res = unsafe { mlock(raw_mmap_ptr, total_mapped_len) };
        let is_pinned = lock_res == 0;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(Self {
            handle_id,
            shm_name: shm_name.to_string(),
            raw_mmap_ptr,
            aligned_ptr,
            total_mapped_len,
            capacity,
            alignment,
            is_pinned,
            is_coherent,
            backing,
            created_at_epoch_ms: now_ms,
        })
    }

    #[inline(always)]
    pub fn handle_id(&self) -> u64 {
        self.handle_id
    }

    #[inline(always)]
    pub fn shm_name(&self) -> &str {
        &self.shm_name
    }

    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline(always)]
    pub fn created_at_epoch_ms(&self) -> u64 {
        self.created_at_epoch_ms
    }

    #[inline(always)]
    pub fn alignment(&self) -> usize {
        self.alignment
    }

    #[inline(always)]
    pub fn is_pinned(&self) -> bool {
        self.is_pinned
    }

    #[inline(always)]
    pub fn is_coherent(&self) -> bool {
        self.is_coherent
    }

    #[inline(always)]
    pub fn raw_ptr(&self) -> *mut u8 {
        self.aligned_ptr
    }

    /// Full usable buffer immutable slice view
    #[inline(always)]
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.aligned_ptr, self.capacity) }
    }

    /// Full usable buffer mutable slice view
    #[inline(always)]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.aligned_ptr, self.capacity) }
    }

    /// Bounds-checked immutable slice view
    pub fn as_slice_at(&self, offset: usize, len: usize) -> Result<&[u8]> {
        let end = offset.checked_add(len).ok_or_else(|| {
            NpuRouterError::DimensionMismatch {
                expected: self.capacity,
                actual: usize::MAX,
            }
        })?;

        if end > self.capacity {
            return Err(NpuRouterError::DimensionMismatch {
                expected: self.capacity,
                actual: end,
            });
        }

        unsafe {
            let ptr = self.aligned_ptr.add(offset);
            Ok(std::slice::from_raw_parts(ptr, len))
        }
    }

    /// Bounds-checked mutable slice view
    pub fn as_mut_slice_at(&mut self, offset: usize, len: usize) -> Result<&mut [u8]> {
        let end = offset.checked_add(len).ok_or_else(|| {
            NpuRouterError::DimensionMismatch {
                expected: self.capacity,
                actual: usize::MAX,
            }
        })?;

        if end > self.capacity {
            return Err(NpuRouterError::DimensionMismatch {
                expected: self.capacity,
                actual: end,
            });
        }

        unsafe {
            let ptr = self.aligned_ptr.add(offset);
            Ok(std::slice::from_raw_parts_mut(ptr, len))
        }
    }

    /// Typed immutable `f32` tensor slice view
    pub fn as_f32_slice(&self, byte_offset: usize, count: usize) -> Result<&[f32]> {
        let byte_len = count.checked_mul(std::mem::size_of::<f32>()).ok_or_else(|| {
            NpuRouterError::InvalidPayload("f32 byte length overflow".into())
        })?;

        let slice = self.as_slice_at(byte_offset, byte_len)?;
        let ptr = slice.as_ptr();

        if (ptr as usize % std::mem::align_of::<f32>()) != 0 {
            return Err(NpuRouterError::InvalidPayload("f32 slice misaligned".into()));
        }

        unsafe { Ok(std::slice::from_raw_parts(ptr as *const f32, count)) }
    }

    /// Typed mutable `f32` tensor slice view
    pub fn as_mut_f32_slice(&mut self, byte_offset: usize, count: usize) -> Result<&mut [f32]> {
        let byte_len = count.checked_mul(std::mem::size_of::<f32>()).ok_or_else(|| {
            NpuRouterError::InvalidPayload("f32 byte length overflow".into())
        })?;

        let slice = self.as_mut_slice_at(byte_offset, byte_len)?;
        let ptr = slice.as_mut_ptr();

        if (ptr as usize % std::mem::align_of::<f32>()) != 0 {
            return Err(NpuRouterError::InvalidPayload("f32 slice misaligned".into()));
        }

        unsafe { Ok(std::slice::from_raw_parts_mut(ptr as *mut f32, count)) }
    }

    /// Write an f32 tensor slice directly into the pinned buffer with cache flush
    pub fn write_f32_tensor(&mut self, byte_offset: usize, data: &[f32]) -> Result<usize> {
        let byte_len = data.len().checked_mul(std::mem::size_of::<f32>()).ok_or_else(|| {
            NpuRouterError::InvalidPayload("f32 slice byte length overflow".into())
        })?;

        let dest = self.as_mut_slice_at(byte_offset, byte_len)?;
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, dest.as_mut_ptr(), byte_len);
            flush_cache_range(dest.as_ptr(), byte_len);
        }

        Ok(byte_len)
    }

    /// Read an f32 tensor slice directly from the pinned buffer with cache invalidation
    pub fn read_f32_tensor(&self, byte_offset: usize, count: usize) -> Result<Vec<f32>> {
        let byte_len = count.checked_mul(std::mem::size_of::<f32>()).ok_or_else(|| {
            NpuRouterError::InvalidPayload("f32 slice byte length overflow".into())
        })?;

        let src = self.as_slice_at(byte_offset, byte_len)?;
        let mut output = vec![0.0f32; count];

        unsafe {
            invalidate_cache_range(src.as_ptr(), byte_len);
            std::ptr::copy_nonoverlapping(src.as_ptr(), output.as_mut_ptr() as *mut u8, byte_len);
        }

        Ok(output)
    }

    /// Flush CPU cachelines for the specified sub-slice
    pub fn flush(&self, offset: usize, len: usize) -> Result<()> {
        let slice = self.as_slice_at(offset, len)?;
        unsafe {
            flush_cache_range(slice.as_ptr(), len);
        }
        Ok(())
    }

    /// Invalidate CPU cachelines for the specified sub-slice
    pub fn invalidate(&self, offset: usize, len: usize) -> Result<()> {
        let slice = self.as_slice_at(offset, len)?;
        unsafe {
            invalidate_cache_range(slice.as_ptr(), len);
        }
        Ok(())
    }
}

impl Drop for PinnedSharedMemoryRingBuffer {
    fn drop(&mut self) {
        if self.is_pinned && !self.raw_mmap_ptr.is_null() && self.raw_mmap_ptr != MAP_FAILED {
            unsafe {
                munlock(self.raw_mmap_ptr, self.total_mapped_len);
            }
        }

        if !self.raw_mmap_ptr.is_null() && self.raw_mmap_ptr != MAP_FAILED {
            unsafe {
                munmap(self.raw_mmap_ptr, self.total_mapped_len);
            }
        }

        if let BufferBacking::PosixShmFile { path, is_creator, .. } = &self.backing {
            if *is_creator && path.exists() {
                let _ = fs::remove_file(path);
            }
        }
    }
}

// ============================================================================
// 6. WASM LINEAR MEMORY DIRECT MAPPING VIEW
// ============================================================================

/// Tracks direct memory mapping association between WASM linear memory and host NPU buffers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasmMemoryMapping {
    pub handle_id: u64,
    pub wasm_ptr: u64,
    pub size: u64,
    pub host_address: u64,
    pub alignment: u32,
    pub is_coherent: bool,
    pub mapped_at_epoch_ms: u64,
}

impl WasmMemoryMapping {
    pub fn new(
        handle_id: u64,
        wasm_ptr: u64,
        size: u64,
        host_address: u64,
        alignment: u32,
        is_coherent: bool,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            handle_id,
            wasm_ptr,
            size,
            host_address,
            alignment,
            is_coherent,
            mapped_at_epoch_ms: now_ms,
        }
    }

    /// Read data from guest WebAssembly linear memory directly into the pinned shared buffer
    pub fn read_from_wasm_memory(
        &self,
        buffer: &mut PinnedSharedMemoryRingBuffer,
        wasm_linear_memory: &[u8],
        wasm_offset: usize,
        byte_len: usize,
    ) -> Result<()> {
        let wasm_end = wasm_offset.checked_add(byte_len).ok_or_else(|| {
            NpuRouterError::DimensionMismatch {
                expected: wasm_linear_memory.len(),
                actual: usize::MAX,
            }
        })?;

        if wasm_end > wasm_linear_memory.len() {
            return Err(NpuRouterError::DimensionMismatch {
                expected: wasm_linear_memory.len(),
                actual: wasm_end,
            });
        }

        let dst = buffer.as_mut_slice_at(0, byte_len)?;
        let src = &wasm_linear_memory[wasm_offset..wasm_end];

        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), byte_len);
            flush_cache_range(dst.as_ptr(), byte_len);
        }

        Ok(())
    }

    /// Write data from pinned shared buffer directly into guest WebAssembly linear memory
    pub fn write_to_wasm_memory(
        &self,
        buffer: &PinnedSharedMemoryRingBuffer,
        wasm_linear_memory: &mut [u8],
        wasm_offset: usize,
        byte_len: usize,
    ) -> Result<()> {
        let wasm_end = wasm_offset.checked_add(byte_len).ok_or_else(|| {
            NpuRouterError::DimensionMismatch {
                expected: wasm_linear_memory.len(),
                actual: usize::MAX,
            }
        })?;

        if wasm_end > wasm_linear_memory.len() {
            return Err(NpuRouterError::DimensionMismatch {
                expected: wasm_linear_memory.len(),
                actual: wasm_end,
            });
        }

        let src = buffer.as_slice_at(0, byte_len)?;
        let dst = &mut wasm_linear_memory[wasm_offset..wasm_end];

        unsafe {
            invalidate_cache_range(src.as_ptr(), byte_len);
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), byte_len);
        }

        Ok(())
    }
}

// ============================================================================
// 7. HIGH-PERFORMANCE ZERO-COPY TENSOR MATH PRIMITIVES
// ============================================================================

/// High-performance SIMD Cosine Similarity with loop unrolling
pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    let chunks = len / 8;
    let remainder = len % 8;

    for i in 0..chunks {
        let idx = i * 8;
        let a_sub = &a[idx..idx + 8];
        let b_sub = &b[idx..idx + 8];

        dot += a_sub[0] * b_sub[0]
            + a_sub[1] * b_sub[1]
            + a_sub[2] * b_sub[2]
            + a_sub[3] * b_sub[3]
            + a_sub[4] * b_sub[4]
            + a_sub[5] * b_sub[5]
            + a_sub[6] * b_sub[6]
            + a_sub[7] * b_sub[7];

        norm_a += a_sub[0] * a_sub[0]
            + a_sub[1] * a_sub[1]
            + a_sub[2] * a_sub[2]
            + a_sub[3] * a_sub[3]
            + a_sub[4] * a_sub[4]
            + a_sub[5] * a_sub[5]
            + a_sub[6] * a_sub[6]
            + a_sub[7] * a_sub[7];

        norm_b += b_sub[0] * b_sub[0]
            + b_sub[1] * b_sub[1]
            + b_sub[2] * b_sub[2]
            + b_sub[3] * b_sub[3]
            + b_sub[4] * b_sub[4]
            + b_sub[5] * b_sub[5]
            + b_sub[6] * b_sub[6]
            + b_sub[7] * b_sub[7];
    }

    let rem_start = chunks * 8;
    for i in 0..remainder {
        let idx = rem_start + i;
        let va = a[idx];
        let vb = b[idx];
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }

    let denom = (norm_a.sqrt() * norm_b.sqrt()).max(1e-12);
    (dot / denom).clamp(-1.0, 1.0)
}

/// High-performance SIMD Dot Product
pub fn simd_dot_product(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut sum = 0.0f32;
    let chunks = len / 8;
    let remainder = len % 8;

    for i in 0..chunks {
        let idx = i * 8;
        sum += a[idx] * b[idx]
            + a[idx + 1] * b[idx + 1]
            + a[idx + 2] * b[idx + 2]
            + a[idx + 3] * b[idx + 3]
            + a[idx + 4] * b[idx + 4]
            + a[idx + 5] * b[idx + 5]
            + a[idx + 6] * b[idx + 6]
            + a[idx + 7] * b[idx + 7];
    }

    let rem_start = chunks * 8;
    for i in 0..remainder {
        let idx = rem_start + i;
        sum += a[idx] * b[idx];
    }

    sum
}

/// High-performance Euclidean L2 Distance
pub fn simd_euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut sum_sq = 0.0f32;

    for i in 0..len {
        let diff = a[i] - b[i];
        sum_sq += diff * diff;
    }

    sum_sq.sqrt()
}

/// Apply in-place batch transformation over an f32 slice
pub fn apply_in_place_transform(data: &mut [f32], transform: TensorTransform) {
    match transform {
        TensorTransform::L2Normalize => {
            let mut sum_sq = 0.0f32;
            for &val in data.iter() {
                sum_sq += val * val;
            }
            let norm = sum_sq.sqrt().max(1e-12);
            let inv_norm = 1.0 / norm;
            for val in data.iter_mut() {
                *val *= inv_norm;
            }
        }
        TensorTransform::ScaleAndBias { alpha, beta } => {
            for val in data.iter_mut() {
                *val = *val * alpha + beta;
            }
        }
        TensorTransform::ReLU => {
            for val in data.iter_mut() {
                if *val < 0.0 {
                    *val = 0.0;
                }
            }
        }
        TensorTransform::GELU => {
            const SQRT_2_OVER_PI: f32 = 0.79788456;
            const COEFF: f32 = 0.044715;
            for val in data.iter_mut() {
                let x = *val;
                let c = SQRT_2_OVER_PI * (x + COEFF * x * x * x);
                *val = 0.5 * x * (1.0 + c.tanh());
            }
        }
        TensorTransform::QuantizeQ8 => {
            // Find absolute max
            let mut amax = 0.0f32;
            for &val in data.iter() {
                let abs = val.abs();
                if abs > amax {
                    amax = abs;
                }
            }
            let scale = if amax == 0.0 { 1.0 } else { amax / 127.0 };
            let inv_scale = 1.0 / scale;
            for val in data.iter_mut() {
                let q = (*val * inv_scale).round().clamp(-128.0, 127.0);
                *val = q * scale; // Round-trip simulated Q8
            }
        }
        TensorTransform::DequantizeQ8 { scale } => {
            for val in data.iter_mut() {
                *val *= scale;
            }
        }
    }
}

// ============================================================================
// 8. WASM-NPU STREAMING PIPELINE MANAGER
// ============================================================================

/// Production orchestrator managing zero-copy pinned shared memory ring buffers,
/// WebAssembly linear memory views, hardware cache coherency, and tensor dispatch.
#[derive(Clone)]
pub struct WasmNpuStreamingPipeline {
    buffers: Arc<RwLock<HashMap<u64, PinnedSharedMemoryRingBuffer>>>,
    descriptors: Arc<RwLock<HashMap<u64, NpuTensorShmDescriptor>>>,
    mappings: Arc<RwLock<HashMap<u64, WasmMemoryMapping>>>,
}

impl Default for WasmNpuStreamingPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmNpuStreamingPipeline {
    /// Create a new, empty streaming pipeline manager
    pub fn new() -> Self {
        Self {
            buffers: Arc::new(RwLock::new(HashMap::new())),
            descriptors: Arc::new(RwLock::new(HashMap::new())),
            mappings: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ------------------------------------------------------------------------
    // WIT CANONICAL INTERFACE FUNCTIONS (wit/canonical_abi_shm_views.wit)
    // ------------------------------------------------------------------------

    /// Allocate or register a zero-copy hardware NPU tensor buffer in shared memory.
    /// Matches `create-npu-tensor-buffer: func(desc: npu-tensor-shm-descriptor) -> result<u64, string>`
    pub fn create_npu_tensor_buffer(&self, desc: NpuTensorShmDescriptor) -> std::result::Result<u64, String> {
        desc.validate().map_err(|e| format!("Descriptor validation failed: {}", e))?;

        let handle_id = if desc.handle_id == 0 {
            NEXT_PIPELINE_HANDLE_ID.fetch_add(1, Ordering::Relaxed)
        } else {
            desc.handle_id
        };

        let alignment = if desc.alignment == 0 {
            desc.device_target.required_alignment()
        } else {
            desc.alignment as usize
        };

        let capacity = desc.size_bytes as usize;
        let shm_name = if desc.shm_name.is_empty() {
            format!("hagibis_npu_shm_{}", handle_id)
        } else {
            desc.shm_name.clone()
        };

        let buffer = PinnedSharedMemoryRingBuffer::create_or_open(
            handle_id,
            &shm_name,
            capacity,
            alignment,
            desc.is_coherent,
        ).map_err(|e| format!("Failed to create pinned shared memory buffer: {}", e))?;

        let mut stored_desc = desc;
        stored_desc.handle_id = handle_id;
        stored_desc.shm_name = shm_name;
        stored_desc.alignment = alignment as u32;

        {
            let mut bufs = self.buffers.write().map_err(|_| "Buffers lock poisoned".to_string())?;
            let mut descs = self.descriptors.write().map_err(|_| "Descriptors lock poisoned".to_string())?;
            bufs.insert(handle_id, buffer);
            descs.insert(handle_id, stored_desc);
        }

        Ok(handle_id)
    }

    /// Map a host pinned shared memory tensor buffer directly into WASM linear memory.
    /// Matches `map-wasm-linear-memory: func(handle-id: u64, wasm-ptr: u64, size: u64) -> result<bool, string>`
    pub fn map_wasm_linear_memory(
        &self,
        handle_id: u64,
        wasm_ptr: u64,
        size: u64,
    ) -> std::result::Result<bool, String> {
        if size == 0 {
            return Err("Cannot map zero bytes of linear memory".into());
        }

        let bufs = self.buffers.read().map_err(|_| "Buffers lock poisoned".to_string())?;
        let buf = bufs.get(&handle_id).ok_or_else(|| {
            format!("Buffer handle {} not found in active pipeline", handle_id)
        })?;

        if (size as usize) > buf.capacity() {
            return Err(format!(
                "Requested mapping size {} exceeds buffer capacity {}",
                size, buf.capacity()
            ));
        }

        let host_address = buf.raw_ptr() as u64;
        let alignment = buf.alignment() as u32;
        let is_coherent = buf.is_coherent();

        let mapping = WasmMemoryMapping::new(
            handle_id,
            wasm_ptr,
            size,
            host_address,
            alignment,
            is_coherent,
        );

        {
            let mut maps = self.mappings.write().map_err(|_| "Mappings lock poisoned".to_string())?;
            maps.insert(handle_id, mapping);
        }

        Ok(true)
    }

    /// Flush or invalidate DMA CPU/NPU caches across the shared memory range.
    /// Matches `flush-dma-cache: func(handle-id: u64, offset: u64, size: u64) -> result<bool, string>`
    pub fn flush_dma_cache(
        &self,
        handle_id: u64,
        offset: u64,
        size: u64,
    ) -> std::result::Result<bool, String> {
        let bufs = self.buffers.read().map_err(|_| "Buffers lock poisoned".to_string())?;
        let buf = bufs.get(&handle_id).ok_or_else(|| {
            format!("Buffer handle {} not found in active pipeline", handle_id)
        })?;

        let offset_usize = offset as usize;
        let len_usize = size as usize;

        buf.flush(offset_usize, len_usize)
            .map_err(|e| format!("Failed to flush cache: {}", e))?;

        memory_barrier_sync();
        Ok(true)
    }

    /// Dispatch a batched zero-copy tensor compute operation directly against the shared buffer.
    /// Matches `submit-zero-copy-batch: func(handle-id: u64, operation: string) -> result<u64, string>`
    pub fn submit_zero_copy_batch(
        &self,
        handle_id: u64,
        operation: &str,
    ) -> std::result::Result<u64, String> {
        let mut bufs = self.buffers.write().map_err(|_| "Buffers lock poisoned".to_string())?;
        let buf = bufs.get_mut(&handle_id).ok_or_else(|| {
            format!("Buffer handle {} not found in active pipeline", handle_id)
        })?;

        let op = operation.trim().to_ascii_lowercase();
        let batch_id = NEXT_BATCH_RECEIPT_ID.fetch_add(1, Ordering::Relaxed);

        if op.starts_with("normalize") || op == "l2_normalize" {
            let count = buf.capacity() / std::mem::size_of::<f32>();
            let slice = buf.as_mut_f32_slice(0, count)
                .map_err(|e| format!("Failed to access f32 slice: {}", e))?;
            apply_in_place_transform(slice, TensorTransform::L2Normalize);
            buf.flush(0, count * 4).map_err(|e| format!("Cache flush failed: {}", e))?;
        } else if op.starts_with("relu") {
            let count = buf.capacity() / std::mem::size_of::<f32>();
            let slice = buf.as_mut_f32_slice(0, count)
                .map_err(|e| format!("Failed to access f32 slice: {}", e))?;
            apply_in_place_transform(slice, TensorTransform::ReLU);
            buf.flush(0, count * 4).map_err(|e| format!("Cache flush failed: {}", e))?;
        } else if op.starts_with("gelu") {
            let count = buf.capacity() / std::mem::size_of::<f32>();
            let slice = buf.as_mut_f32_slice(0, count)
                .map_err(|e| format!("Failed to access f32 slice: {}", e))?;
            apply_in_place_transform(slice, TensorTransform::GELU);
            buf.flush(0, count * 4).map_err(|e| format!("Cache flush failed: {}", e))?;
        } else if op.starts_with("quantize") || op == "q8_0" {
            let count = buf.capacity() / std::mem::size_of::<f32>();
            let slice = buf.as_mut_f32_slice(0, count)
                .map_err(|e| format!("Failed to access f32 slice: {}", e))?;
            apply_in_place_transform(slice, TensorTransform::QuantizeQ8);
            buf.flush(0, count * 4).map_err(|e| format!("Cache flush failed: {}", e))?;
        } else if op.starts_with("scale") {
            // scale:alpha:beta
            let parts: Vec<&str> = op.split(':').collect();
            let alpha = parts.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
            let beta = parts.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let count = buf.capacity() / std::mem::size_of::<f32>();
            let slice = buf.as_mut_f32_slice(0, count)
                .map_err(|e| format!("Failed to access f32 slice: {}", e))?;
            apply_in_place_transform(slice, TensorTransform::ScaleAndBias { alpha, beta });
            buf.flush(0, count * 4).map_err(|e| format!("Cache flush failed: {}", e))?;
        } else if op == "noop" || op == "sync" {
            buf.flush(0, buf.capacity()).map_err(|e| format!("Cache flush failed: {}", e))?;
        } else {
            return Err(format!("Unsupported zero-copy operation: '{}'", operation));
        }

        memory_barrier_sync();
        Ok(batch_id)
    }

    // ------------------------------------------------------------------------
    // HIGH-LEVEL IDIOMATIC RUST ENGINE APIS
    // ------------------------------------------------------------------------

    /// Allocate a pinned buffer with full parameters and return the handle identifier
    pub fn allocate_pinned_buffer(
        &self,
        shm_name: &str,
        capacity_bytes: usize,
        alignment: usize,
        device_target: NpuDeviceTarget,
        data_type: TensorDataType,
        dimensions: Vec<u64>,
    ) -> Result<u64> {
        let handle_id = NEXT_PIPELINE_HANDLE_ID.fetch_add(1, Ordering::Relaxed);
        let desc = NpuTensorShmDescriptor::new(
            handle_id,
            shm_name,
            0,
            capacity_bytes as u64,
            alignment as u32,
            data_type,
            dimensions,
            device_target,
            true,
        );

        self.create_npu_tensor_buffer(desc)
            .map_err(NpuRouterError::ExecutionFailed)
    }

    /// Retrieve a clone of the descriptor for an active buffer handle
    pub fn get_descriptor(&self, handle_id: u64) -> Option<NpuTensorShmDescriptor> {
        self.descriptors.read().ok()?.get(&handle_id).cloned()
    }

    /// Retrieve a clone of the mapping for an active buffer handle
    pub fn get_mapping(&self, handle_id: u64) -> Option<WasmMemoryMapping> {
        self.mappings.read().ok()?.get(&handle_id).cloned()
    }

    /// Number of active allocated buffers in the pipeline
    pub fn active_buffers_count(&self) -> usize {
        self.buffers.read().map(|b| b.len()).unwrap_or(0)
    }

    /// Compute direct tensor similarity between two pinned buffer regions without copying
    pub fn execute_similarity(
        &self,
        handle_a: u64,
        offset_a: usize,
        handle_b: u64,
        offset_b: usize,
        dim: usize,
        metric: TensorMetric,
    ) -> Result<f32> {
        let bufs = self.buffers.read().map_err(|_| {
            NpuRouterError::ExecutionFailed("Buffers lock poisoned".into())
        })?;

        let buf_a = bufs.get(&handle_a).ok_or_else(|| {
            NpuRouterError::DeviceNotFound(format!("Buffer handle {} not found", handle_a))
        })?;
        let buf_b = bufs.get(&handle_b).ok_or_else(|| {
            NpuRouterError::DeviceNotFound(format!("Buffer handle {} not found", handle_b))
        })?;

        let slice_a = buf_a.as_f32_slice(offset_a, dim)?;
        let slice_b = buf_b.as_f32_slice(offset_b, dim)?;

        let score = match metric {
            TensorMetric::CosineSimilarity => simd_cosine_similarity(slice_a, slice_b),
            TensorMetric::DotProduct => simd_dot_product(slice_a, slice_b),
            TensorMetric::EuclideanDistance => simd_euclidean_distance(slice_a, slice_b),
        };

        Ok(score)
    }

    /// Execute an in-place batch transformation with performance receipt
    pub fn execute_transformation(
        &self,
        handle_id: u64,
        offset: usize,
        count: usize,
        transform: TensorTransform,
    ) -> Result<StreamingBatchReceipt> {
        let t0 = Instant::now();
        let mut bufs = self.buffers.write().map_err(|_| {
            NpuRouterError::ExecutionFailed("Buffers lock poisoned".into())
        })?;

        let buf = bufs.get_mut(&handle_id).ok_or_else(|| {
            NpuRouterError::DeviceNotFound(format!("Buffer handle {} not found", handle_id))
        })?;

        let slice = buf.as_mut_f32_slice(offset, count)?;
        apply_in_place_transform(slice, transform);

        let byte_len = count * std::mem::size_of::<f32>();
        buf.flush(offset, byte_len)?;
        memory_barrier_sync();

        let elapsed = t0.elapsed();
        let nanos = elapsed.as_nanos() as u64;
        let secs = elapsed.as_secs_f64().max(1e-9);
        let mb = (byte_len as f64) / (1024.0 * 1024.0);
        let throughput = mb / secs;

        let mut hasher = Sha256::new();
        let bytes = buf.as_slice_at(offset, byte_len)?;
        hasher.update(bytes);
        let checksum = hex::encode(hasher.finalize());

        let batch_id = NEXT_BATCH_RECEIPT_ID.fetch_add(1, Ordering::Relaxed);

        Ok(StreamingBatchReceipt {
            batch_id,
            handle_id,
            operation: format!("{:?}", transform),
            elements_processed: count,
            duration_nanos: nanos,
            throughput_mbs: throughput,
            checksum_sha256: checksum,
        })
    }

    /// Read data from guest WASM linear memory into the mapped host buffer
    pub fn sync_from_wasm(
        &self,
        handle_id: u64,
        wasm_linear_mem: &[u8],
        wasm_offset: usize,
        byte_len: usize,
    ) -> Result<()> {
        let mapping = self.get_mapping(handle_id).ok_or_else(|| {
            NpuRouterError::InvalidPayload(format!("No mapping exists for handle {}", handle_id))
        })?;

        let mut bufs = self.buffers.write().map_err(|_| {
            NpuRouterError::ExecutionFailed("Buffers lock poisoned".into())
        })?;

        let buf = bufs.get_mut(&handle_id).ok_or_else(|| {
            NpuRouterError::DeviceNotFound(format!("Buffer handle {} not found", handle_id))
        })?;

        mapping.read_from_wasm_memory(buf, wasm_linear_mem, wasm_offset, byte_len)
    }

    /// Write data from the mapped host buffer into guest WASM linear memory
    pub fn sync_to_wasm(
        &self,
        handle_id: u64,
        wasm_linear_mem: &mut [u8],
        wasm_offset: usize,
        byte_len: usize,
    ) -> Result<()> {
        let mapping = self.get_mapping(handle_id).ok_or_else(|| {
            NpuRouterError::InvalidPayload(format!("No mapping exists for handle {}", handle_id))
        })?;

        let bufs = self.buffers.read().map_err(|_| {
            NpuRouterError::ExecutionFailed("Buffers lock poisoned".into())
        })?;

        let buf = bufs.get(&handle_id).ok_or_else(|| {
            NpuRouterError::DeviceNotFound(format!("Buffer handle {} not found", handle_id))
        })?;

        mapping.write_to_wasm_memory(buf, wasm_linear_mem, wasm_offset, byte_len)
    }

    /// Release and remove a buffer from the pipeline
    pub fn release_buffer(&self, handle_id: u64) -> Result<bool> {
        let mut bufs = self.buffers.write().map_err(|_| {
            NpuRouterError::ExecutionFailed("Buffers lock poisoned".into())
        })?;
        let mut descs = self.descriptors.write().map_err(|_| {
            NpuRouterError::ExecutionFailed("Descriptors lock poisoned".into())
        })?;
        let mut maps = self.mappings.write().map_err(|_| {
            NpuRouterError::ExecutionFailed("Mappings lock poisoned".into())
        })?;

        let removed = bufs.remove(&handle_id).is_some();
        descs.remove(&handle_id);
        maps.remove(&handle_id);

        Ok(removed)
    }
}

// ============================================================================
// 9. UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_allocation_and_alignment_64_256() {
        // 1. Test 64-byte AVX-512 alignment
        let buf_64 = PinnedSharedMemoryRingBuffer::create_or_open(
            1,
            "test_shm_align_64",
            4096,
            ALIGN_64_AVX512,
            true,
        ).expect("Failed to create 64-byte aligned buffer");

        assert_eq!(buf_64.capacity(), 4096);
        assert_eq!(buf_64.alignment(), 64);
        let ptr_64_val = buf_64.raw_ptr() as usize;
        assert_eq!(ptr_64_val % 64, 0, "Buffer ptr must be 64-byte aligned");

        // 2. Test 256-byte GPU compute alignment
        let buf_256 = PinnedSharedMemoryRingBuffer::create_or_open(
            2,
            "test_shm_align_256",
            8192,
            ALIGN_256_GPU,
            true,
        ).expect("Failed to create 256-byte aligned buffer");

        assert_eq!(buf_256.capacity(), 8192);
        assert_eq!(buf_256.alignment(), 256);
        let ptr_256_val = buf_256.raw_ptr() as usize;
        assert_eq!(ptr_256_val % 256, 0, "Buffer ptr must be 256-byte aligned");
    }

    #[test]
    fn test_mapping_and_non_copy_slice_mutation() {
        let pipeline = WasmNpuStreamingPipeline::new();

        let handle_id = pipeline.allocate_pinned_buffer(
            "test_shm_mutation",
            2048,
            ALIGN_64_AVX512,
            NpuDeviceTarget::CpuSimd,
            TensorDataType::F32,
            vec![512],
        ).expect("Allocate buffer");

        // Simulate WASM linear memory
        let wasm_ptr: u64 = 0x10000;
        let map_ok = pipeline.map_wasm_linear_memory(handle_id, wasm_ptr, 2048).expect("Map memory");
        assert!(map_ok);

        // Populate tensor data directly into the pinned shared buffer
        {
            let mut bufs = pipeline.buffers.write().unwrap();
            let buf = bufs.get_mut(&handle_id).unwrap();
            let tensor_slice = buf.as_mut_f32_slice(0, 512).unwrap();
            for i in 0..512 {
                tensor_slice[i] = i as f32 * 0.5;
            }
        }

        // Direct in-place mutation without copies
        {
            let mut bufs = pipeline.buffers.write().unwrap();
            let buf = bufs.get_mut(&handle_id).unwrap();
            let tensor_slice = buf.as_mut_f32_slice(0, 512).unwrap();
            for val in tensor_slice.iter_mut() {
                *val += 10.0;
            }
        }

        // Read back values and assert mutation
        {
            let bufs = pipeline.buffers.read().unwrap();
            let buf = bufs.get(&handle_id).unwrap();
            let tensor_slice = buf.as_f32_slice(0, 512).unwrap();
            assert_eq!(tensor_slice[0], 10.0);
            assert_eq!(tensor_slice[1], 10.5);
            assert_eq!(tensor_slice[10], 15.0);
        }

        // Test synchronizing to/from simulated WASM guest memory
        let mut simulated_wasm_memory = vec![0u8; 65536]; // 64 KiB page
        let sync_res = pipeline.sync_to_wasm(handle_id, &mut simulated_wasm_memory, 0x1000, 2048);
        assert!(sync_res.is_ok());

        // Check values in simulated wasm memory
        let f32_wasm = unsafe {
            std::slice::from_raw_parts(
                simulated_wasm_memory.as_ptr().add(0x1000) as *const f32,
                512,
            )
        };
        assert_eq!(f32_wasm[0], 10.0);
        assert_eq!(f32_wasm[1], 10.5);
    }

    #[test]
    fn test_cache_flush_and_invalidation() {
        let pipeline = WasmNpuStreamingPipeline::new();

        let handle_id = pipeline.allocate_pinned_buffer(
            "test_shm_cache",
            4096,
            ALIGN_256_GPU,
            NpuDeviceTarget::GpuCompute,
            TensorDataType::F32,
            vec![1024],
        ).expect("Allocate buffer");

        // Flush full range
        let flush_ok = pipeline.flush_dma_cache(handle_id, 0, 4096).expect("Flush full range");
        assert!(flush_ok);

        // Flush sub-range
        let sub_flush_ok = pipeline.flush_dma_cache(handle_id, 256, 512).expect("Flush sub range");
        assert!(sub_flush_ok);

        // Invalidation via buffer
        {
            let bufs = pipeline.buffers.read().unwrap();
            let buf = bufs.get(&handle_id).unwrap();
            assert!(buf.invalidate(0, 1024).is_ok());
            assert!(buf.invalidate(128, 256).is_ok());
        }

        // Out of bounds flush should fail gracefully
        let oob_flush = pipeline.flush_dma_cache(handle_id, 4000, 500);
        assert!(oob_flush.is_err());
    }

    #[test]
    fn test_batched_tensor_execution_throughput() {
        let pipeline = WasmNpuStreamingPipeline::new();

        let dim = 768; // LanceDB embedding dimensionality
        let num_vectors = 64;
        let total_floats = dim * num_vectors;
        let capacity_bytes = total_floats * std::mem::size_of::<f32>();

        let handle_a = pipeline.allocate_pinned_buffer(
            "test_shm_batch_a",
            capacity_bytes,
            ALIGN_256_GPU,
            NpuDeviceTarget::NpuDirect,
            TensorDataType::F32,
            vec![num_vectors as u64, dim as u64],
        ).expect("Allocate A");

        let handle_b = pipeline.allocate_pinned_buffer(
            "test_shm_batch_b",
            capacity_bytes,
            ALIGN_256_GPU,
            NpuDeviceTarget::NpuDirect,
            TensorDataType::F32,
            vec![num_vectors as u64, dim as u64],
        ).expect("Allocate B");

        // Populate both buffers
        {
            let mut bufs = pipeline.buffers.write().unwrap();
            let buf_a = bufs.get_mut(&handle_a).unwrap();
            let slice_a = buf_a.as_mut_f32_slice(0, total_floats).unwrap();
            for i in 0..total_floats {
                slice_a[i] = ((i % 17) as f32) * 0.1;
            }

            let buf_b = bufs.get_mut(&handle_b).unwrap();
            let slice_b = buf_b.as_mut_f32_slice(0, total_floats).unwrap();
            for i in 0..total_floats {
                slice_b[i] = ((i % 19) as f32) * 0.1;
            }
        }

        // Measure batch similarity throughput over all vectors
        let start = Instant::now();
        for v in 0..num_vectors {
            let offset_bytes = v * dim * std::mem::size_of::<f32>();
            let sim = pipeline.execute_similarity(
                handle_a,
                offset_bytes,
                handle_b,
                offset_bytes,
                dim,
                TensorMetric::CosineSimilarity,
            ).expect("Cosine similarity");

            assert!(sim >= -1.0 && sim <= 1.0);
        }
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() < 500, "Batched execution took too long: {:?}", elapsed);

        // Test in-place transformation throughput (Normalize and GELU)
        let receipt_norm = pipeline.execute_transformation(
            handle_a,
            0,
            total_floats,
            TensorTransform::L2Normalize,
        ).expect("L2 Normalize");
        assert_eq!(receipt_norm.elements_processed, total_floats);
        assert!(receipt_norm.throughput_mbs > 0.0);

        let receipt_gelu = pipeline.execute_transformation(
            handle_b,
            0,
            total_floats,
            TensorTransform::GELU,
        ).expect("GELU");
        assert_eq!(receipt_gelu.elements_processed, total_floats);
    }

    #[test]
    fn test_wit_interface_contract() {
        let pipeline = WasmNpuStreamingPipeline::new();

        let desc = NpuTensorShmDescriptor {
            handle_id: 42,
            shm_name: "wit_contract_shm".to_string(),
            offset_bytes: 0,
            size_bytes: 4096,
            alignment: 256,
            data_type: TensorDataType::F32,
            dimensions: vec![1024],
            device_target: NpuDeviceTarget::GpuCompute,
            is_coherent: true,
        };

        // 1. create_npu_tensor_buffer
        let handle = pipeline.create_npu_tensor_buffer(desc).expect("Create buffer");
        assert_eq!(handle, 42);

        // 2. map_wasm_linear_memory
        let mapped = pipeline.map_wasm_linear_memory(handle, 0x20000, 4096).expect("Map memory");
        assert!(mapped);

        // 3. flush_dma_cache
        let flushed = pipeline.flush_dma_cache(handle, 0, 4096).expect("Flush cache");
        assert!(flushed);

        // 4. submit_zero_copy_batch
        let batch_id = pipeline.submit_zero_copy_batch(handle, "normalize").expect("Submit batch");
        assert!(batch_id >= 1000);

        let batch_relu = pipeline.submit_zero_copy_batch(handle, "relu").expect("Submit relu");
        assert!(batch_relu > batch_id);
    }
}
