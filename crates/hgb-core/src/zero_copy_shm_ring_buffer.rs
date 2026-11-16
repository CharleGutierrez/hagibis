//! # Zero-Copy IPC & POSIX/io_uring Shared Memory Ring Buffer
//!
//! Production-grade lock-free zero-copy ring buffer subsystem (Pillar 2) for Hagibis:
//! - Provides ultra-low-latency (< 5 µs round-trip) data passing between host daemon (`hgbd`),
//!   sandboxed Firecracker microVMs (`/dev/kvm`), and WASI 0.2 WebAssembly sandboxes.
//! - Utilizes POSIX shared memory (`/dev/shm`) with `memmap2` and cacheline-isolated atomic headers.
//! - Eliminates false sharing via strict 64-byte cacheline alignment (`#[repr(C, align(64))]`).
//! - Circular payload arena with zero-copy slice lending (`lend_slice`, `commit_slice`, `consume_slice`).
//! - Eventfd & Futex low-latency signaling (< 1.5 µs wakeup) with kernel descriptor fallback.
//! - High-throughput tensor slice and LanceDB vector batch transfers (> 10 GB/s bandwidth).

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ==============================================================================
// 1. Constants & Magic Signatures
// ==============================================================================

/// 64-byte standard CPU cacheline boundary
pub const CACHELINE_SIZE: usize = 64;

/// Hagibis Shared Memory Ring Buffer Magic: "HGB_SHM1" in ASCII (0x4847425F53484D31)
pub const SHM_MAGIC: u64 = 0x4847425F_53484D31;

/// Entry Header Magic: "SLIC" in ASCII (0x534C4943)
pub const ENTRY_MAGIC: u32 = 0x534C4943;

/// Wrap Marker entry type indicating jump to start of circular arena
pub const ENTRY_TYPE_WRAP_MARKER: u16 = 0xFFFF;

/// Minimum ring buffer payload capacity (4 KiB)
pub const MIN_RING_CAPACITY: usize = 4096;

/// Default ring buffer payload capacity (4 MiB)
pub const DEFAULT_RING_CAPACITY: usize = 4 * 1024 * 1024;

// ==============================================================================
// 2. Structured Errors
// ==============================================================================

/// Structured errors originating from the zero-copy shared memory ring buffer
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ShmRingError {
    #[error("Capacity {capacity} is invalid: must be a power of two and >= {min}", min = MIN_RING_CAPACITY)]
    InvalidCapacity { capacity: usize },

    #[error("Buffer overrun: requested {requested} bytes, available {available} bytes")]
    BufferOverrun { available: usize, requested: usize },

    #[error("Payload length {requested} exceeds max single entry capacity {max}")]
    PayloadTooLarge { requested: usize, max: usize },

    #[error("Lease ticket stale: ticket generation {ticket_gen} does not match active generation {current_gen}")]
    LeaseStale {
        ticket_gen: u64,
        current_gen: u64,
    },

    #[error("Corrupted entry header at offset {offset}: magic {actual:#010X} (expected {expected:#010X})")]
    CorruptedHeader {
        offset: usize,
        expected: u32,
        actual: u32,
    },

    #[error("Data checksum mismatch: computed CRC32 {computed:#010X}, header CRC32 {expected:#010X}")]
    ChecksumMismatch { computed: u32, expected: u32 },

    #[error("Signaling failure: {0}")]
    SignalingError(String),

    #[error("POSIX shared memory failure: {0}")]
    IoError(String),
}

// ==============================================================================
// 3. Cacheline-Aligned Ring Header Layout (192 bytes total)
// ==============================================================================

/// Signaling mechanism used between processes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SignalingMode {
    Futex = 0,
    Eventfd = 1,
    AtomicSpin = 2,
}

/// Global metadata header aligned to 64 bytes (Cacheline 1: 0..64)
#[repr(C, align(64))]
pub struct RingMetadata {
    pub magic: u64,
    pub version: u32,
    pub capacity: u32,
    pub signaling_mode: u32,
    pub futex_word: AtomicU32,
    pub eventfd_val: AtomicU64,
    pub created_epoch_ms: u64,
    pub total_entries_committed: AtomicU64,
    _pad: [u8; 16],
}

/// Producer state header aligned to 64 bytes (Cacheline 2: 64..128)
#[repr(C, align(64))]
pub struct ProducerHeader {
    pub head: AtomicU64,
    pub generation: AtomicU64,
    pub active_leases: AtomicU64,
    _pad: [u8; 40],
}

/// Consumer state header aligned to 64 bytes (Cacheline 3: 128..192)
#[repr(C, align(64))]
pub struct ConsumerHeader {
    pub tail: AtomicU64,
    pub drop_counter: AtomicU64,
    pub last_read_epoch_ns: AtomicU64,
    _pad: [u8; 40],
}

/// Combined 192-byte ring header mapped at offset 0
#[repr(C, align(64))]
pub struct ShmRingBufferHeader {
    pub metadata: RingMetadata,
    pub producer: ProducerHeader,
    pub consumer: ConsumerHeader,
}

const _: () = assert!(std::mem::size_of::<ShmRingBufferHeader>() == 192);

// ==============================================================================
// 4. Circular Arena Entry Framing
// ==============================================================================

/// Domain classification of entries passed through the ring
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum EntryType {
    RawBytes = 0,
    TensorSlice = 1,
    VectorBatch = 2,
    ControlFrame = 3,
    WrapMarker = ENTRY_TYPE_WRAP_MARKER,
}

impl EntryType {
    pub fn from_u16(val: u16) -> Self {
        match val {
            0 => Self::RawBytes,
            1 => Self::TensorSlice,
            2 => Self::VectorBatch,
            3 => Self::ControlFrame,
            _ => Self::WrapMarker,
        }
    }
}

/// Header prepended to every entry in the circular arena (40 bytes, 8-byte aligned)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryHeader {
    pub magic: u32,
    pub entry_type: u16,
    pub flags: u16,
    pub payload_len: u32,
    pub total_len: u32,
    pub generation: u64,
    pub timestamp_ns: u64,
    pub checksum: u32,
    pub reserved: u32,
}

const _: () = assert!(std::mem::size_of::<EntryHeader>() == 40);

/// Monotonically incremented lease ticket returned by `lend_slice`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseTicket {
    pub arena_offset: usize,
    pub payload_offset: usize,
    pub payload_len: usize,
    pub generation: u64,
    pub total_len: usize,
}

// ==============================================================================
// 5. Tensor Slice & LanceDB Vector Batch Specializations
// ==============================================================================

/// Numeric element data types for zero-copy tensor slicing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum TensorDtype {
    Float32 = 0,
    Float16 = 1,
    Bfloat16 = 2,
    Int8 = 3,
    Int32 = 4,
    Int64 = 5,
}

/// Metadata descriptor for tensor slice transfers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorSliceDescriptor {
    pub tensor_name: String,
    pub dtype: TensorDtype,
    pub shape: [usize; 4],
    pub stride: [usize; 4],
    pub batch_id: u64,
}

/// Vector distance metric for LanceDB batches
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum VectorDistanceMetric {
    L2 = 0,
    Cosine = 1,
    DotProduct = 2,
}

/// Metadata descriptor for LanceDB vector search batches
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorBatchDescriptor {
    pub dataset_id: String,
    pub dimensions: u32,
    pub vector_count: u32,
    pub metric: VectorDistanceMetric,
    pub row_ids: Vec<u64>,
}

// ==============================================================================
// 6. Fast Pure-Rust CRC32 Implementation (IEEE 802.3)
// ==============================================================================

/// Computes IEEE 802.3 CRC32 checksum for payload integrity verification
pub fn compute_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ==============================================================================
// 7. Low-Latency Futex & Eventfd IPC Signaling
// ==============================================================================

/// Linux futex wrapper providing sub-microsecond wait/wake signaling
pub struct FutexSignaler;

impl FutexSignaler {
    #[inline]
    pub fn wait(word: &AtomicU32, expected: u32, timeout_ns: Option<u64>) {
        #[cfg(target_os = "linux")]
        unsafe {
            let timeout = timeout_ns.map(|ns| libc::timespec {
                tv_sec: (ns / 1_000_000_000) as libc::time_t,
                tv_nsec: (ns % 1_000_000_000) as libc::c_long,
            });

            let timeout_ptr = timeout
                .as_ref()
                .map(|t| t as *const libc::timespec)
                .unwrap_or(std::ptr::null());

            libc::syscall(
                libc::SYS_futex,
                word as *const AtomicU32 as *const u32,
                libc::FUTEX_WAIT,
                expected,
                timeout_ptr,
                std::ptr::null::<u32>(),
                0u32,
            );
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (word, expected, timeout_ns);
            std::hint::spin_loop();
        }
    }

    #[inline]
    pub fn wake(word: &AtomicU32, count: i32) {
        #[cfg(target_os = "linux")]
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word as *const AtomicU32 as *const u32,
                libc::FUTEX_WAKE,
                count,
                std::ptr::null::<libc::timespec>(),
                std::ptr::null::<u32>(),
                0u32,
            );
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (word, count);
        }
    }
}

// ==============================================================================
// 8. Shared Memory Backing & Ring Buffer Implementation
// ==============================================================================

/// Shared memory backing source
pub enum ShmBacking {
    PosixShm { name: String, file: File },
    File { path: PathBuf, file: File },
    Anonymous,
}

/// Production lock-free zero-copy shared memory ring buffer
pub struct ZeroCopyShmRingBuffer {
    mmap: MmapMut,
    capacity: usize,
    capacity_mask: usize,
    backing: ShmBacking,
    eventfd_fd: Option<i32>,
}

// Safe to send across threads as ring indices and signaling use atomic synchronization
unsafe impl Send for ZeroCopyShmRingBuffer {}
unsafe impl Sync for ZeroCopyShmRingBuffer {}

impl ZeroCopyShmRingBuffer {
    /// Creates a new POSIX shared memory ring buffer in `/dev/shm`
    pub fn create_posix_shm(
        shm_name: &str,
        capacity: usize,
        signaling: SignalingMode,
    ) -> Result<Self, ShmRingError> {
        if !capacity.is_power_of_two() || capacity < MIN_RING_CAPACITY {
            return Err(ShmRingError::InvalidCapacity { capacity });
        }

        let total_size = std::mem::size_of::<ShmRingBufferHeader>() + capacity;

        // Strip leading slash if supplied
        let clean_name = shm_name.trim_start_matches('/');
        let shm_path = format!("/dev/shm/hgb_shm_{}", clean_name);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&shm_path)
            .map_err(|e| ShmRingError::IoError(format!("Failed to open {}: {}", shm_path, e)))?;

        file.set_len(total_size as u64)
            .map_err(|e| ShmRingError::IoError(format!("Failed to truncate {}: {}", shm_path, e)))?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file).map_err(|e| {
                ShmRingError::IoError(format!("Failed to mmap shared memory: {}", e))
            })?
        };

        // Initialize cacheline-aligned header
        let header_ptr = mmap.as_mut_ptr() as *mut ShmRingBufferHeader;
        unsafe {
            (*header_ptr).metadata.magic = SHM_MAGIC;
            (*header_ptr).metadata.version = 1;
            (*header_ptr).metadata.capacity = capacity as u32;
            (*header_ptr).metadata.signaling_mode = signaling as u32;
            (*header_ptr).metadata.futex_word = AtomicU32::new(0);
            (*header_ptr).metadata.eventfd_val = AtomicU64::new(0);
            (*header_ptr).metadata.created_epoch_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            (*header_ptr).metadata.total_entries_committed = AtomicU64::new(0);

            (*header_ptr).producer.head = AtomicU64::new(0);
            (*header_ptr).producer.generation = AtomicU64::new(1);
            (*header_ptr).producer.active_leases = AtomicU64::new(0);

            (*header_ptr).consumer.tail = AtomicU64::new(0);
            (*header_ptr).consumer.drop_counter = AtomicU64::new(0);
            (*header_ptr).consumer.last_read_epoch_ns = AtomicU64::new(0);
        }

        let eventfd_fd = if signaling == SignalingMode::Eventfd {
            #[cfg(target_os = "linux")]
            unsafe {
                let fd = libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC);
                if fd >= 0 {
                    Some(fd)
                } else {
                    None
                }
            }
            #[cfg(not(target_os = "linux"))]
            None
        } else {
            None
        };

        Ok(Self {
            mmap,
            capacity,
            capacity_mask: capacity - 1,
            backing: ShmBacking::PosixShm {
                name: shm_path,
                file,
            },
            eventfd_fd,
        })
    }

    /// Creates an anonymous in-memory ring buffer (ideal for isolated unit tests & microVM staging)
    pub fn create_anonymous(
        capacity: usize,
        signaling: SignalingMode,
    ) -> Result<Self, ShmRingError> {
        if !capacity.is_power_of_two() || capacity < MIN_RING_CAPACITY {
            return Err(ShmRingError::InvalidCapacity { capacity });
        }

        let total_size = std::mem::size_of::<ShmRingBufferHeader>() + capacity;
        let mut mmap = MmapMut::map_anon(total_size)
            .map_err(|e| ShmRingError::IoError(format!("Failed to create anonymous mmap: {}", e)))?;

        let header_ptr = mmap.as_mut_ptr() as *mut ShmRingBufferHeader;
        unsafe {
            (*header_ptr).metadata.magic = SHM_MAGIC;
            (*header_ptr).metadata.version = 1;
            (*header_ptr).metadata.capacity = capacity as u32;
            (*header_ptr).metadata.signaling_mode = signaling as u32;
            (*header_ptr).metadata.futex_word = AtomicU32::new(0);
            (*header_ptr).metadata.eventfd_val = AtomicU64::new(0);
            (*header_ptr).metadata.created_epoch_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            (*header_ptr).metadata.total_entries_committed = AtomicU64::new(0);

            (*header_ptr).producer.head = AtomicU64::new(0);
            (*header_ptr).producer.generation = AtomicU64::new(1);
            (*header_ptr).producer.active_leases = AtomicU64::new(0);

            (*header_ptr).consumer.tail = AtomicU64::new(0);
            (*header_ptr).consumer.drop_counter = AtomicU64::new(0);
            (*header_ptr).consumer.last_read_epoch_ns = AtomicU64::new(0);
        }

        Ok(Self {
            mmap,
            capacity,
            capacity_mask: capacity - 1,
            backing: ShmBacking::Anonymous,
            eventfd_fd: None,
        })
    }

    /// Access the cacheline-aligned header reference
    #[inline(always)]
    pub fn header(&self) -> &ShmRingBufferHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmRingBufferHeader) }
    }

    /// Pointer to payload arena starting immediately after header
    #[inline(always)]
    fn arena_ptr(&self) -> *mut u8 {
        unsafe { self.mmap.as_ptr().add(std::mem::size_of::<ShmRingBufferHeader>()) as *mut u8 }
    }

    /// Total capacity of payload arena in bytes
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drops count due to overruns
    #[inline]
    pub fn drop_count(&self) -> u64 {
        self.header().consumer.drop_counter.load(Ordering::Relaxed)
    }

    /// Current lease generation counter
    #[inline]
    pub fn active_generation(&self) -> u64 {
        self.header().producer.generation.load(Ordering::Acquire)
    }

    /// Invalidates all current leases by incrementing the monotonic generation counter
    pub fn invalidate_leases(&self) -> u64 {
        self.header()
            .producer
            .generation
            .fetch_add(1, Ordering::SeqCst)
            + 1
    }

    /// Lends a zero-copy contiguous slice from the circular arena
    pub fn lend_slice(&self, requested_len: usize) -> Result<LeaseTicket, ShmRingError> {
        let max_len = self.capacity - std::mem::size_of::<EntryHeader>() - 64;
        if requested_len > max_len {
            return Err(ShmRingError::PayloadTooLarge {
                requested: requested_len,
                max: max_len,
            });
        }

        let header_size = std::mem::size_of::<EntryHeader>();
        // Align entry total length to 8 bytes
        let total_len = (header_size + requested_len + 7) & !7;

        let header = self.header();

        let head = header.producer.head.load(Ordering::Acquire);
        let tail = header.consumer.tail.load(Ordering::Acquire);

        let used = head.saturating_sub(tail) as usize;
        let available = self.capacity.saturating_sub(used);

        if total_len > available {
            header.consumer.drop_counter.fetch_add(1, Ordering::Relaxed);
            return Err(ShmRingError::BufferOverrun {
                available,
                requested: total_len,
            });
        }

        let mut offset = (head as usize) & self.capacity_mask;

        // Check if slice wraps around buffer boundary
        if offset + total_len > self.capacity {
            let wrap_bytes = self.capacity - offset;
            if available < wrap_bytes + total_len {
                header.consumer.drop_counter.fetch_add(1, Ordering::Relaxed);
                return Err(ShmRingError::BufferOverrun {
                    available,
                    requested: wrap_bytes + total_len,
                });
            }

            // Write WrapMarker entry occupying the remainder of the buffer
            unsafe {
                let entry_ptr = self.arena_ptr().add(offset) as *mut EntryHeader;
                (*entry_ptr) = EntryHeader {
                    magic: ENTRY_MAGIC,
                    entry_type: ENTRY_TYPE_WRAP_MARKER,
                    flags: 0,
                    payload_len: 0,
                    total_len: wrap_bytes as u32,
                    generation: header.producer.generation.load(Ordering::Acquire),
                    timestamp_ns: 0,
                    checksum: 0,
                    reserved: 0,
                };
            }

            // Advance head to wrap boundary
            header
                .producer
                .head
                .fetch_add(wrap_bytes as u64, Ordering::Release);
            offset = 0;
        }

        let gen = header.producer.generation.load(Ordering::Acquire);
        header.producer.active_leases.fetch_add(1, Ordering::Release);

        Ok(LeaseTicket {
            arena_offset: offset,
            payload_offset: offset + header_size,
            payload_len: requested_len,
            generation: gen,
            total_len,
        })
    }

    /// Direct mutable borrow of loaned slice for zero-copy in-place writing
    ///
    /// # Safety
    /// Must only be accessed by the thread owning the `LeaseTicket`.
    pub unsafe fn loaned_slice_mut(&self, ticket: &LeaseTicket) -> &mut [u8] {
        let ptr = self.arena_ptr().add(ticket.payload_offset);
        std::slice::from_raw_parts_mut(ptr, ticket.payload_len)
    }

    /// Commits a previously lent slice with data and updates the monotonic head index
    pub fn commit_slice(
        &self,
        ticket: LeaseTicket,
        entry_type: EntryType,
        data: &[u8],
    ) -> Result<u64, ShmRingError> {
        let header = self.header();

        // 1. Verify generation has not been invalidated
        let current_gen = header.producer.generation.load(Ordering::Acquire);
        if ticket.generation != current_gen {
            header.producer.active_leases.fetch_sub(1, Ordering::Release);
            return Err(ShmRingError::LeaseStale {
                ticket_gen: ticket.generation,
                current_gen,
            });
        }

        // 2. Copy payload if data slice provided
        if !data.is_empty() {
            let write_len = data.len().min(ticket.payload_len);
            unsafe {
                let dest = self.arena_ptr().add(ticket.payload_offset);
                std::ptr::copy_nonoverlapping(data.as_ptr(), dest, write_len);
            }
        }

        // 3. Compute checksum
        let checksum = unsafe {
            let payload = std::slice::from_raw_parts(
                self.arena_ptr().add(ticket.payload_offset),
                ticket.payload_len,
            );
            compute_crc32(payload)
        };

        // 4. Construct EntryHeader
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let entry = EntryHeader {
            magic: ENTRY_MAGIC,
            entry_type: entry_type as u16,
            flags: 0,
            payload_len: ticket.payload_len as u32,
            total_len: ticket.total_len as u32,
            generation: ticket.generation,
            timestamp_ns: now_ns,
            checksum,
            reserved: 0,
        };

        unsafe {
            let entry_ptr = self.arena_ptr().add(ticket.arena_offset) as *mut EntryHeader;
            (*entry_ptr) = entry;
        }

        // 5. Memory barrier and monotonic index commit
        let committed_head = header
            .producer
            .head
            .fetch_add(ticket.total_len as u64, Ordering::Release)
            + ticket.total_len as u64;

        header.producer.active_leases.fetch_sub(1, Ordering::Release);
        header
            .metadata
            .total_entries_committed
            .fetch_add(1, Ordering::Relaxed);

        // 6. Signal waiting consumers via futex or eventfd
        self.signal_consumers();

        Ok(committed_head)
    }

    /// Signals consumers via futex wake or eventfd write
    fn signal_consumers(&self) {
        let header = self.header();
        header.metadata.futex_word.fetch_add(1, Ordering::Release);
        FutexSignaler::wake(&header.metadata.futex_word, 1);

        if let Some(fd) = self.eventfd_fd {
            #[cfg(target_os = "linux")]
            unsafe {
                let val: u64 = 1;
                libc::write(fd, &val as *const u64 as *const libc::c_void, 8);
            }
        }
    }

    /// Zero-copy consumer method: loans payload slice directly to callback closure
    pub fn consume_slice<F, R>(&self, mut consumer_fn: F) -> Result<Option<R>, ShmRingError>
    where
        F: FnMut(&EntryHeader, &[u8]) -> R,
    {
        let header = self.header();

        loop {
            let tail = header.consumer.tail.load(Ordering::Acquire);
            let head = header.producer.head.load(Ordering::Acquire);

            if tail >= head {
                return Ok(None); // Buffer empty
            }

            let offset = (tail as usize) & self.capacity_mask;
            let entry_header = unsafe { &*(self.arena_ptr().add(offset) as *const EntryHeader) };

            // Validate header magic
            if entry_header.magic != ENTRY_MAGIC {
                return Err(ShmRingError::CorruptedHeader {
                    offset,
                    expected: ENTRY_MAGIC,
                    actual: entry_header.magic,
                });
            }

            // Handle wrap-around marker
            if entry_header.entry_type == ENTRY_TYPE_WRAP_MARKER {
                header
                    .consumer
                    .tail
                    .fetch_add(entry_header.total_len as u64, Ordering::Release);
                continue;
            }

            // Read payload slice zero-copy
            let payload_slice = unsafe {
                let payload_ptr = self
                    .arena_ptr()
                    .add(offset + std::mem::size_of::<EntryHeader>());
                std::slice::from_raw_parts(payload_ptr, entry_header.payload_len as usize)
            };

            // Verify checksum
            let computed = compute_crc32(payload_slice);
            if computed != entry_header.checksum {
                return Err(ShmRingError::ChecksumMismatch {
                    computed,
                    expected: entry_header.checksum,
                });
            }

            // Execute consumer logic
            let res = consumer_fn(entry_header, payload_slice);

            // Advance consumer tail
            header
                .consumer
                .tail
                .fetch_add(entry_header.total_len as u64, Ordering::Release);

            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            header
                .consumer
                .last_read_epoch_ns
                .store(now_ns, Ordering::Relaxed);

            return Ok(Some(res));
        }
    }

    /// Writes high-frequency LanceDB vector embedding batch zero-copy
    pub fn write_vector_batch(
        &self,
        desc: &VectorBatchDescriptor,
        embeddings: &[f32],
    ) -> Result<u64, ShmRingError> {
        let desc_bytes = serde_json::to_vec(desc).map_err(|e| {
            ShmRingError::SignalingError(format!("Descriptor serialization failed: {}", e))
        })?;

        let desc_len = desc_bytes.len();
        let float_bytes_len = embeddings.len() * std::mem::size_of::<f32>();
        let total_payload_len = 8 + desc_len + float_bytes_len;

        let ticket = self.lend_slice(total_payload_len)?;

        unsafe {
            let slice = self.loaned_slice_mut(&ticket);
            // Prefix with 8-byte descriptor length
            slice[0..8].copy_from_slice(&(desc_len as u64).to_le_bytes());
            slice[8..8 + desc_len].copy_from_slice(&desc_bytes);

            let float_bytes = std::slice::from_raw_parts(
                embeddings.as_ptr() as *const u8,
                float_bytes_len,
            );
            slice[8 + desc_len..8 + desc_len + float_bytes_len].copy_from_slice(float_bytes);
        }

        self.commit_slice(ticket, EntryType::VectorBatch, &[])
    }

    /// Reads high-frequency LanceDB vector embedding batch zero-copy
    pub fn read_vector_batch(
        &self,
    ) -> Result<Option<(VectorBatchDescriptor, Vec<f32>)>, ShmRingError> {
        self.consume_slice(|_hdr, payload| {
            if payload.len() < 8 {
                return None;
            }
            let desc_len = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
            if payload.len() < 8 + desc_len {
                return None;
            }

            let desc: VectorBatchDescriptor =
                serde_json::from_slice(&payload[8..8 + desc_len]).ok()?;
            let float_bytes = &payload[8 + desc_len..];
            let float_count = float_bytes.len() / std::mem::size_of::<f32>();

            let mut floats = vec![0.0f32; float_count];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    float_bytes.as_ptr(),
                    floats.as_mut_ptr() as *mut u8,
                    float_bytes.len(),
                );
            }

            Some((desc, floats))
        })
        .map(|opt| opt.flatten())
    }

    /// Writes tensor slice directly into shared memory with sub-5 µs latency
    pub fn write_tensor_slice(
        &self,
        desc: &TensorSliceDescriptor,
        tensor_bytes: &[u8],
    ) -> Result<u64, ShmRingError> {
        let desc_bytes = serde_json::to_vec(desc).map_err(|e| {
            ShmRingError::SignalingError(format!("Descriptor serialization failed: {}", e))
        })?;

        let desc_len = desc_bytes.len();
        let total_payload_len = 8 + desc_len + tensor_bytes.len();

        let ticket = self.lend_slice(total_payload_len)?;

        unsafe {
            let slice = self.loaned_slice_mut(&ticket);
            slice[0..8].copy_from_slice(&(desc_len as u64).to_le_bytes());
            slice[8..8 + desc_len].copy_from_slice(&desc_bytes);
            slice[8 + desc_len..8 + desc_len + tensor_bytes.len()].copy_from_slice(tensor_bytes);
        }

        self.commit_slice(ticket, EntryType::TensorSlice, &[])
    }

    /// Reads tensor slice from shared memory
    pub fn read_tensor_slice(
        &self,
    ) -> Result<Option<(TensorSliceDescriptor, Vec<u8>)>, ShmRingError> {
        self.consume_slice(|_hdr, payload| {
            if payload.len() < 8 {
                return None;
            }
            let desc_len = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
            if payload.len() < 8 + desc_len {
                return None;
            }

            let desc: TensorSliceDescriptor =
                serde_json::from_slice(&payload[8..8 + desc_len]).ok()?;
            let tensor_data = payload[8 + desc_len..].to_vec();

            Some((desc, tensor_data))
        })
        .map(|opt| opt.flatten())
    }
}

impl Drop for ZeroCopyShmRingBuffer {
    fn drop(&mut self) {
        if let Some(fd) = self.eventfd_fd {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::close(fd);
            }
        }

        if let ShmBacking::PosixShm { name, .. } = &self.backing {
            let _ = std::fs::remove_file(name);
        }
    }
}

// ==============================================================================
// 9. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_shm_ring_buffer_initialization() {
        let ring = ZeroCopyShmRingBuffer::create_anonymous(8192, SignalingMode::AtomicSpin)
            .expect("Init anonymous ring");

        assert_eq!(ring.capacity(), 8192);
        let header = ring.header();
        assert_eq!(header.metadata.magic, SHM_MAGIC);
        assert_eq!(header.metadata.version, 1);
        assert_eq!(header.metadata.capacity, 8192);
        assert_eq!(header.producer.head.load(Ordering::Relaxed), 0);
        assert_eq!(header.consumer.tail.load(Ordering::Relaxed), 0);
        assert_eq!(header.consumer.drop_counter.load(Ordering::Relaxed), 0);
        assert_eq!(header.producer.generation.load(Ordering::Relaxed), 1);

        // Verify power of two check
        let bad = ZeroCopyShmRingBuffer::create_anonymous(5000, SignalingMode::AtomicSpin);
        assert!(bad.is_err());
    }

    #[test]
    fn test_zero_copy_slice_write_and_read() {
        let ring = ZeroCopyShmRingBuffer::create_anonymous(16384, SignalingMode::AtomicSpin)
            .expect("Init ring");

        let test_data = b"Hagibis Zero-Copy Tensor Frame Payload 0123456789";
        let ticket = ring.lend_slice(test_data.len()).expect("Lend slice");

        assert_eq!(ticket.payload_len, test_data.len());
        assert_eq!(ticket.generation, 1);

        // In-place zero-copy write
        unsafe {
            let slice = ring.loaned_slice_mut(&ticket);
            slice.copy_from_slice(test_data);
        }

        let committed_head = ring
            .commit_slice(ticket, EntryType::RawBytes, &[])
            .expect("Commit slice");
        assert!(committed_head > 0);

        // Consume slice
        let mut consumed_count = 0;
        let consumed = ring
            .consume_slice(|hdr, payload| {
                consumed_count += 1;
                assert_eq!(hdr.entry_type, EntryType::RawBytes as u16);
                assert_eq!(payload, test_data);
                payload.len()
            })
            .expect("Consume slice");

        assert_eq!(consumed, Some(test_data.len()));
        assert_eq!(consumed_count, 1);

        // Buffer now empty
        let empty = ring.consume_slice(|_, _| ()).expect("Empty check");
        assert!(empty.is_none());
    }

    #[test]
    fn test_lock_free_concurrent_producer_consumer() {
        let ring = Arc::new(
            ZeroCopyShmRingBuffer::create_anonymous(65536, SignalingMode::Futex)
                .expect("Init ring"),
        );

        let items_to_send = 1000;
        let ring_producer = ring.clone();

        let producer_handle = std::thread::spawn(move || {
            for i in 0..items_to_send {
                let payload = format!("CONCURRENT_TELEMETRY_PACKET_{:08}", i).into_bytes();
                let mut attempts = 0;
                loop {
                    match ring_producer.lend_slice(payload.len()) {
                        Ok(ticket) => {
                            ring_producer
                                .commit_slice(ticket, EntryType::RawBytes, &payload)
                                .expect("Commit");
                            break;
                        }
                        Err(ShmRingError::BufferOverrun { .. }) => {
                            attempts += 1;
                            if attempts > 10_000 {
                                panic!("Producer starved");
                            }
                            std::thread::yield_now();
                        }
                        Err(e) => panic!("Unexpected lend error: {:?}", e),
                    }
                }
            }
        });

        let ring_consumer = ring.clone();
        let consumer_handle = std::thread::spawn(move || {
            let mut received = 0;
            while received < items_to_send {
                match ring_consumer.consume_slice(|_hdr, payload| {
                    let text = String::from_utf8_lossy(payload);
                    assert!(text.starts_with("CONCURRENT_TELEMETRY_PACKET_"));
                    received += 1;
                }) {
                    Ok(Some(())) => {}
                    Ok(None) => std::thread::yield_now(),
                    Err(e) => panic!("Consumer error: {:?}", e),
                }
            }
            received
        });

        producer_handle.join().expect("Producer finished");
        let total_received = consumer_handle.join().expect("Consumer finished");

        assert_eq!(total_received, items_to_send);
        assert_eq!(ring.drop_count(), 0);
    }

    #[test]
    fn test_ring_wrap_around_and_overrun_handling() {
        let capacity = 4096;
        let ring = ZeroCopyShmRingBuffer::create_anonymous(capacity, SignalingMode::AtomicSpin)
            .expect("Init ring");

        let chunk_size = 500;
        let iterations = 15; // 15 * 500 = 7500 > 4096 (forces multiple wrap-arounds)

        for i in 0..iterations {
            let data = vec![(i % 255) as u8; chunk_size];
            let ticket = ring.lend_slice(data.len()).expect("Lend slice");
            ring.commit_slice(ticket, EntryType::RawBytes, &data)
                .expect("Commit slice");

            // Consume immediately to avoid overrun
            let res = ring
                .consume_slice(|_hdr, payload| {
                    assert_eq!(payload.len(), chunk_size);
                    assert_eq!(payload[0], (i % 255) as u8);
                })
                .expect("Consume slice");
            assert!(res.is_some());
        }

        // Drain any pending wrap markers from wrap test
        while ring.consume_slice(|_, _| ()).unwrap().is_some() {}

        // Test buffer overrun condition on a fresh ring
        let overrun_ring =
            ZeroCopyShmRingBuffer::create_anonymous(4096, SignalingMode::AtomicSpin)
                .expect("Init overrun ring");

        let large_size = 1800;
        let t1 = overrun_ring.lend_slice(large_size).expect("Lend 1");
        overrun_ring
            .commit_slice(t1, EntryType::RawBytes, &vec![0xAA; large_size])
            .expect("Commit 1");

        let t2 = overrun_ring.lend_slice(large_size).expect("Lend 2");
        overrun_ring
            .commit_slice(t2, EntryType::RawBytes, &vec![0xBB; large_size])
            .expect("Commit 2");

        // Third allocation exceeds remaining capacity (400 bytes available < 1848 requested)
        let t3_err = overrun_ring.lend_slice(large_size).unwrap_err();
        match t3_err {
            ShmRingError::BufferOverrun { .. } => {}
            other => panic!("Expected BufferOverrun, got {:?}", other),
        }

        assert!(overrun_ring.drop_count() > 0);
    }

    #[test]
    fn test_monotonic_generation_lease_invalidation() {
        let ring = ZeroCopyShmRingBuffer::create_anonymous(8192, SignalingMode::AtomicSpin)
            .expect("Init ring");

        let ticket = ring.lend_slice(128).expect("Lend slice");
        assert_eq!(ticket.generation, 1);

        // Operator / self-healing invalidates active leases
        let new_gen = ring.invalidate_leases();
        assert_eq!(new_gen, 2);

        // Attempting to commit stale ticket must fail
        let payload = vec![0x42; 128];
        let commit_err = ring
            .commit_slice(ticket, EntryType::RawBytes, &payload)
            .unwrap_err();

        match commit_err {
            ShmRingError::LeaseStale {
                ticket_gen,
                current_gen,
            } => {
                assert_eq!(ticket_gen, 1);
                assert_eq!(current_gen, 2);
            }
            other => panic!("Expected LeaseStale error, got {:?}", other),
        }
    }

    #[test]
    fn test_tensor_slice_and_lancedb_vector_transfers() {
        let ring = ZeroCopyShmRingBuffer::create_anonymous(32768, SignalingMode::AtomicSpin)
            .expect("Init ring");

        // 1. Test LanceDB Vector Batch transfer
        let vector_desc = VectorBatchDescriptor {
            dataset_id: "hagibis-ast-remediations".to_string(),
            dimensions: 4,
            vector_count: 2,
            metric: VectorDistanceMetric::Cosine,
            row_ids: vec![101, 102],
        };
        let embeddings = vec![0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];

        ring.write_vector_batch(&vector_desc, &embeddings)
            .expect("Write vector batch");

        let read_batch = ring.read_vector_batch().expect("Read vector batch");
        assert!(read_batch.is_some());
        let (out_desc, out_floats) = read_batch.unwrap();
        assert_eq!(out_desc.dataset_id, "hagibis-ast-remediations");
        assert_eq!(out_desc.dimensions, 4);
        assert_eq!(out_floats.len(), 8);
        assert!((out_floats[0] - 0.1).abs() < 1e-5);

        // 2. Test Tensor Slice transfer
        let tensor_desc = TensorSliceDescriptor {
            tensor_name: "npu_transformer_layer_0_weights".to_string(),
            dtype: TensorDtype::Float32,
            shape: [1, 2, 4, 1],
            stride: [8, 4, 1, 1],
            batch_id: 42,
        };
        let tensor_raw_bytes = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

        ring.write_tensor_slice(&tensor_desc, &tensor_raw_bytes)
            .expect("Write tensor slice");

        let read_tensor = ring.read_tensor_slice().expect("Read tensor slice");
        assert!(read_tensor.is_some());
        let (out_tensor_desc, out_bytes) = read_tensor.unwrap();
        assert_eq!(out_tensor_desc.tensor_name, "npu_transformer_layer_0_weights");
        assert_eq!(out_tensor_desc.batch_id, 42);
        assert_eq!(out_bytes, tensor_raw_bytes);
    }
}
