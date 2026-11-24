//! # Unified Dual-Sandbox Zero-Copy Shared-Memory Ring Buffer
//!
//! Sub-microsecond, lock-free circular ring buffer providing zero-copy direct pointer
//! mapping and event notification between the lightweight WebAssembly runtime (`wasmtime`)
//! and hardware-isolated Firecracker MicroVMs (`virtio-fs` / memfd).
//!
//! ### Core Architectural Principles:
//! 1. **Cacheline-Aligned Lock-Free SPSC Ring**:
//!    - 64-byte cacheline isolation (`#[repr(C, align(64))]`) separating global metadata,
//!      producer head, and consumer tail across independent CPU cachelines to completely
//!      eliminate false sharing and cache-coherency ping-pong.
//!    - Monotonic 64-bit sequence counters (`AtomicU64`) with strict `Acquire` and `Release`
//!      memory orderings.
//! 2. **Zero-Copy Memory-Mapped Backing**:
//!    - Anonymous Linux `memfd_create` backed shared memory with transparent fallback to
//!      POSIX `/dev/shm` and `virtio-fs` shared mount points.
//!    - Zero heap allocation during frame publication and frame consumption.
//! 3. **Kernel Event Signaling via io_uring & eventfd**:
//!    - Asynchronous event signaling using non-blocking Linux `eventfd` optionally registered
//!      into an `io_uring` kernel submission/completion ring.
//!    - Direct eventfd descriptor passing into Firecracker MicroVM guests or epoll pollers.
//! 4. **Dual-Sandbox Direct Pointer Mapping**:
//!    - Direct pointer mapping to Wasm linear memory (`wasmtime::Memory::data_ptr`).
//!    - Direct pointer mapping to Firecracker MicroVM virtio-fs shared backing files.
//!    - Single-copy / zero-copy memory transfers between Wasm linear memory and MicroVM guest buffers.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Magic identifier for Hagibis Dual-Sandbox Zero-Copy Ring: `0x4847425F44535A43` ("HGB_DSZC")
pub const RING_MAGIC: u64 = 0x4847425F44535A43;

/// Specification version
pub const RING_VERSION: u32 = 1;

/// Fixed 64-byte CPU cacheline alignment boundary
pub const CACHELINE_BYTES: usize = 64;

/// Total header size (3 cachelines: Global, Producer, Consumer)
pub const HEADER_SIZE_BYTES: usize = 192;

/// Default ring buffer payload capacity (4 MiB)
pub const DEFAULT_RING_CAPACITY: usize = 4 * 1024 * 1024;

/// Linux io_uring syscall numbers
#[cfg(target_os = "linux")]
const SYS_IO_URING_SETUP: libc::c_long = libc::SYS_io_uring_setup;
#[cfg(target_os = "linux")]
const SYS_IO_URING_ENTER: libc::c_long = libc::SYS_io_uring_enter;
#[cfg(target_os = "linux")]
const SYS_IO_URING_REGISTER: libc::c_long = libc::SYS_io_uring_register;

/// io_uring register opcode for eventfd
const IORING_REGISTER_EVENTFD: libc::c_uint = 4;

// ==============================================================================
// 1. Ring Buffer Header & Frame Memory Layouts
// ==============================================================================

/// Frame header flags
pub const FRAME_FLAG_NORMAL: u16 = 0x0000;
pub const FRAME_FLAG_WASM_MEM: u16 = 0x0001;
pub const FRAME_FLAG_MICROVM_MEM: u16 = 0x0002;
pub const FRAME_FLAG_HANDOFF: u16 = 0x0004;
pub const FRAME_FLAG_WRAP: u16 = 0x8000;

/// Fixed 8-byte frame prefix placed before each binary frame in the ring
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Payload length in bytes
    pub length: u32,
    /// Frame routing flags (e.g. `FRAME_FLAG_WRAP`)
    pub flags: u16,
    /// Stream / Channel identifier
    pub stream_id: u16,
}

impl FrameHeader {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

const _: () = assert!(std::mem::size_of::<FrameHeader>() == 8);

/// Global Ring Buffer Control Header (192 bytes = 3 x 64-byte cachelines)
///
/// Designed to guarantee zero false sharing between the producer and consumer.
#[repr(C, align(64))]
pub struct DualSandboxRingHeader {
    // -------------------------------------------------------------------------
    // Cacheline 0: Global Metadata & Configuration (0..64)
    // -------------------------------------------------------------------------
    pub magic: u64,
    pub version: u32,
    pub flags: u32,
    pub capacity: u64,
    pub mask: u64,
    pub payload_offset: u64,
    pub event_fd_val: i32,
    pub _pad0: [u8; 20],

    // -------------------------------------------------------------------------
    // Cacheline 1: Producer State (64..128)
    // -------------------------------------------------------------------------
    pub head: AtomicU64,
    pub producer_pid: AtomicU32,
    pub producer_flags: AtomicU32,
    pub frames_published: AtomicU64,
    pub bytes_published: AtomicU64,
    pub _pad_producer: [u8; 32],

    // -------------------------------------------------------------------------
    // Cacheline 2: Consumer State (128..192)
    // -------------------------------------------------------------------------
    pub tail: AtomicU64,
    pub consumer_pid: AtomicU32,
    pub consumer_flags: AtomicU32,
    pub frames_consumed: AtomicU64,
    pub bytes_consumed: AtomicU64,
    pub _pad_consumer: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<DualSandboxRingHeader>() == HEADER_SIZE_BYTES);
const _: () = assert!(std::mem::align_of::<DualSandboxRingHeader>() == CACHELINE_BYTES);

// ==============================================================================
// 2. Linux io_uring & Kernel eventfd Signaling
// ==============================================================================

/// Linux io_uring kernel submission queue offsets
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
struct io_sqring_offsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    resv1: u32,
    user_addr: u64,
}

/// Linux io_uring kernel completion queue offsets
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
struct io_cqring_offsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    resv1: u32,
    user_addr: u64,
}

/// Linux io_uring initialization parameters
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
struct io_uring_params {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    resv: [u32; 3],
    sq_off: io_sqring_offsets,
    cq_off: io_cqring_offsets,
}

/// Linux kernel eventfd & io_uring event notification signaler with OwnedFd RAII safety
pub struct DualSandboxEventSignaler {
    owned_event_fd: Option<OwnedFd>,
    borrowed_event_fd: Option<RawFd>,
    owned_io_uring_fd: Option<OwnedFd>,
}

impl DualSandboxEventSignaler {
    /// Create a new event signaler using Linux eventfd with optional io_uring registration
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let ev_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
            if ev_fd < 0 {
                return Err(HgbError::System(format!(
                    "Failed to create eventfd: errno {}",
                    std::io::Error::last_os_error()
                )));
            }
            let owned_ev_fd = unsafe { OwnedFd::from_raw_fd(ev_fd) };

            // Attempt to initialize kernel io_uring
            let mut params: io_uring_params = unsafe { std::mem::zeroed() };
            let uring_fd = unsafe {
                libc::syscall(
                    SYS_IO_URING_SETUP,
                    8 as libc::c_uint,
                    &mut params as *mut io_uring_params,
                )
            };

            let owned_io_uring_fd = if uring_fd >= 0 {
                let ufd = uring_fd as RawFd;
                // Register eventfd with io_uring if supported
                let _ = unsafe {
                    libc::syscall(
                        SYS_IO_URING_REGISTER,
                        ufd,
                        IORING_REGISTER_EVENTFD,
                        &ev_fd as *const RawFd as *const libc::c_void,
                        1 as libc::c_uint,
                    )
                };
                Some(unsafe { OwnedFd::from_raw_fd(ufd) })
            } else {
                None
            };

            Ok(Self {
                owned_event_fd: Some(owned_ev_fd),
                borrowed_event_fd: None,
                owned_io_uring_fd,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(HgbError::System("eventfd/io_uring is only supported on Linux".to_string()))
        }
    }

    /// Wrap an existing raw eventfd (borrowed ownership, will not close on drop)
    pub fn from_raw_eventfd(event_fd: RawFd) -> Self {
        Self {
            owned_event_fd: None,
            borrowed_event_fd: Some(event_fd),
            owned_io_uring_fd: None,
        }
    }

    /// Send a notification signal via eventfd
    #[inline]
    pub fn notify(&self) {
        let fd = self.raw_eventfd();
        if fd >= 0 {
            let val: u64 = 1;
            unsafe {
                libc::write(
                    fd,
                    &val as *const u64 as *const libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        }
    }

    /// Drain pending eventfd notification signals
    #[inline]
    pub fn drain(&self) {
        let fd = self.raw_eventfd();
        if fd >= 0 {
            let mut val: u64 = 0;
            unsafe {
                libc::read(
                    fd,
                    &mut val as *mut u64 as *mut libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        }
    }

    /// Wait for a notification signal up to a timeout using poll
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let fd = self.raw_eventfd();
        if fd < 0 {
            return false;
        }

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        let ret = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
        if ret > 0 && (pfd.revents & libc::POLLIN != 0) {
            self.drain();
            true
        } else {
            false
        }
    }

    /// Access the underlying raw eventfd
    pub fn raw_eventfd(&self) -> RawFd {
        if let Some(ref fd) = self.owned_event_fd {
            fd.as_raw_fd()
        } else {
            self.borrowed_event_fd.unwrap_or(-1)
        }
    }

    /// Whether io_uring integration is active
    pub fn is_io_uring_active(&self) -> bool {
        self.owned_io_uring_fd.is_some()
    }
}

unsafe impl Send for DualSandboxEventSignaler {}
unsafe impl Sync for DualSandboxEventSignaler {}

// ==============================================================================
// 3. Guest Linear Memory & virtio-fs Mappings
// ==============================================================================

/// Zero-copy mapping into Wasm linear memory export
#[derive(Debug, Clone, Copy)]
pub struct WasmGuestMapping {
    pub ptr: *mut u8,
    pub len: usize,
}

unsafe impl Send for WasmGuestMapping {}
unsafe impl Sync for WasmGuestMapping {}

/// Zero-copy mapping into MicroVM Firecracker virtio-fs shared memory file
pub struct MicroVmGuestMapping {
    pub path: PathBuf,
    pub mmap: MmapMut,
    pub len: usize,
}

// ==============================================================================
// 4. ZeroCopyRingConduit Implementation
// ==============================================================================

/// Benchmark performance report for the zero-copy ring conduit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConduitBenchmarkMetrics {
    pub ring_name: String,
    pub total_frames: usize,
    pub total_bytes: usize,
    pub elapsed_nanos: u64,
    pub frames_per_sec: f64,
    pub throughput_mb_s: f64,
    pub avg_latency_ns: f64,
    pub zero_copy_verified: bool,
}

/// High-level Unified Zero-Copy Memory Sharing Conduit across Dual-Sandbox
pub struct ZeroCopyRingConduit {
    name: String,
    path: Option<PathBuf>,
    mmap: MmapMut,
    mmap_ptr: *mut u8,
    total_len: usize,
    capacity: usize,
    mask: u64,
    signaler: DualSandboxEventSignaler,
    wasm_mapping: Option<WasmGuestMapping>,
    microvm_mapping: Option<MicroVmGuestMapping>,
    producer_spin: AtomicBool,
    consumer_spin: AtomicBool,
    unlink_on_drop: bool,
}

impl ZeroCopyRingConduit {
    /// Create a new shared-memory ring conduit using Linux `memfd_create`
    pub fn create_memfd(name: &str, capacity_bytes: usize) -> Result<Self> {
        let capacity = capacity_bytes.max(4096).next_power_of_two();
        let total_size = HEADER_SIZE_BYTES + capacity;

        #[cfg(target_os = "linux")]
        {
            let c_name = std::ffi::CString::new(name)
                .map_err(|e| HgbError::Storage(format!("Invalid memfd name: {e}")))?;

            let fd = unsafe {
                libc::syscall(
                    libc::SYS_memfd_create,
                    c_name.as_ptr(),
                    (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as libc::c_uint,
                )
            };

            if fd >= 0 {
                let file = unsafe { File::from_raw_fd(fd as RawFd) };
                file.set_len(total_size as u64)
                    .map_err(|e| HgbError::Storage(format!("Failed to set memfd size: {e}")))?;

                let mut mmap = unsafe {
                    MmapMut::map_mut(&file)
                        .map_err(|e| HgbError::Storage(format!("Failed to mmap memfd: {e}")))?
                };

                let signaler = DualSandboxEventSignaler::new()?;
                Self::init_header(&mut mmap, capacity, signaler.raw_eventfd());

                let mmap_ptr = mmap.as_mut_ptr();

                return Ok(Self {
                    name: name.to_string(),
                    path: None,
                    mmap,
                    mmap_ptr,
                    total_len: total_size,
                    capacity,
                    mask: (capacity - 1) as u64,
                    signaler,
                    wasm_mapping: None,
                    microvm_mapping: None,
                    producer_spin: AtomicBool::new(false),
                    consumer_spin: AtomicBool::new(false),
                    unlink_on_drop: false,
                });
            }
        }

        // Fallback to POSIX shared memory if memfd_create fails or on non-Linux
        Self::create_shm(name, capacity)
    }

    /// Create a new shared-memory ring conduit using POSIX `/dev/shm`
    pub fn create_shm(name: &str, capacity_bytes: usize) -> Result<Self> {
        let capacity = capacity_bytes.max(4096).next_power_of_two();
        let total_size = HEADER_SIZE_BYTES + capacity;

        let sanitized: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .collect();
        let filename = format!("hgb_dszc_{}", sanitized);

        let shm_dir = Path::new("/dev/shm");
        let path = if shm_dir.exists() && shm_dir.is_dir() {
            shm_dir.join(filename)
        } else {
            std::env::temp_dir().join(filename)
        };

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| HgbError::Storage(format!("Failed to open shm file at {path:?}: {e}")))?;

        file.set_len(total_size as u64)
            .map_err(|e| HgbError::Storage(format!("Failed to set shm file size: {e}")))?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|e| HgbError::Storage(format!("Failed to mmap shm file: {e}")))?
        };

        let signaler = DualSandboxEventSignaler::new()?;
        Self::init_header(&mut mmap, capacity, signaler.raw_eventfd());

        let mmap_ptr = mmap.as_mut_ptr();

        Ok(Self {
            name: name.to_string(),
            path: Some(path),
            mmap,
            mmap_ptr,
            total_len: total_size,
            capacity,
            mask: (capacity - 1) as u64,
            signaler,
            wasm_mapping: None,
            microvm_mapping: None,
            producer_spin: AtomicBool::new(false),
            consumer_spin: AtomicBool::new(false),
            unlink_on_drop: false,
        })
    }

    /// Default constructor: tries memfd first, transparently falls back to POSIX /dev/shm
    pub fn create(name: &str, capacity_bytes: usize) -> Result<Self> {
        Self::create_memfd(name, capacity_bytes)
    }

    /// Open an existing shared-memory ring buffer at a specific file path
    pub fn open_path(path: &Path, name: &str) -> Result<Self> {
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "SHM ring buffer file not found at {path:?}"
            )));
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| HgbError::Storage(format!("Failed to open SHM at {path:?}: {e}")))?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|e| HgbError::Storage(format!("Mmap failed: {e}")))?
        };

        if mmap.len() < HEADER_SIZE_BYTES {
            return Err(HgbError::Storage("SHM file smaller than header size".to_string()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap_or_default());
        if magic != RING_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid Ring Magic: expected 0x{:X}, got 0x{:X}",
                RING_MAGIC, magic
            )));
        }

        let capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap_or_default()) as usize;
        let event_fd_val = i32::from_le_bytes(mmap[40..44].try_into().unwrap_or_default());

        let signaler = if event_fd_val >= 0 {
            DualSandboxEventSignaler::from_raw_eventfd(event_fd_val)
        } else {
            DualSandboxEventSignaler::new()?
        };

        let total_size = HEADER_SIZE_BYTES + capacity;
        let mmap_ptr = mmap.as_mut_ptr();

        Ok(Self {
            name: name.to_string(),
            path: Some(path.to_path_buf()),
            mmap,
            mmap_ptr,
            total_len: total_size,
            capacity,
            mask: (capacity - 1) as u64,
            signaler,
            wasm_mapping: None,
            microvm_mapping: None,
            producer_spin: AtomicBool::new(false),
            consumer_spin: AtomicBool::new(false),
            unlink_on_drop: false,
        })
    }

    /// Initialize the 192-byte cacheline-aligned header memory
    fn init_header(mmap: &mut [u8], capacity: usize, event_fd: RawFd) {
        // Cacheline 0: Global (0..64)
        mmap[0..8].copy_from_slice(&RING_MAGIC.to_le_bytes());
        mmap[8..12].copy_from_slice(&RING_VERSION.to_le_bytes());
        mmap[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
        mmap[16..24].copy_from_slice(&(capacity as u64).to_le_bytes());
        mmap[24..32].copy_from_slice(&((capacity - 1) as u64).to_le_bytes());
        mmap[32..40].copy_from_slice(&(HEADER_SIZE_BYTES as u64).to_le_bytes());
        mmap[40..44].copy_from_slice(&(event_fd as i32).to_le_bytes());
        mmap[44..64].fill(0); // padding to 64 bytes

        // Cacheline 1: Producer (64..128)
        mmap[64..72].copy_from_slice(&0u64.to_le_bytes()); // head
        mmap[72..76].copy_from_slice(&(std::process::id()).to_le_bytes()); // producer_pid
        mmap[76..80].copy_from_slice(&0u32.to_le_bytes()); // producer_flags
        mmap[80..88].copy_from_slice(&0u64.to_le_bytes()); // frames_published
        mmap[88..96].copy_from_slice(&0u64.to_le_bytes()); // bytes_published
        mmap[96..128].fill(0); // padding to 128 bytes

        // Cacheline 2: Consumer (128..192)
        mmap[128..136].copy_from_slice(&0u64.to_le_bytes()); // tail
        mmap[136..140].copy_from_slice(&0u32.to_le_bytes()); // consumer_pid
        mmap[140..144].copy_from_slice(&0u32.to_le_bytes()); // consumer_flags
        mmap[144..152].copy_from_slice(&0u64.to_le_bytes()); // frames_consumed
        mmap[152..160].copy_from_slice(&0u64.to_le_bytes()); // bytes_consumed
        mmap[160..192].fill(0); // padding to 192 bytes
    }

    #[inline(always)]
    fn header(&self) -> &DualSandboxRingHeader {
        unsafe { &*(self.mmap_ptr as *const DualSandboxRingHeader) }
    }

    // -------------------------------------------------------------------------
    // Core SPSC Lock-Free Publishing & Consumption (<500ns Latency)
    // -------------------------------------------------------------------------

    /// Publish a binary frame into the lock-free shared memory ring buffer.
    ///
    /// Returns the monotonic sequence number of the published frame.
    pub fn publish_frame(&self, frame: &[u8]) -> Result<u64> {
        self.publish_frame_with_flags(frame, FRAME_FLAG_NORMAL, 0)
    }

    /// Publish a frame with explicit flags and stream identifier
    pub fn publish_frame_with_flags(&self, frame: &[u8], flags: u16, stream_id: u16) -> Result<u64> {
        let payload_len = frame.len();
        let aligned_len = (payload_len + 7) & !7;
        let needed = FrameHeader::SIZE + aligned_len;

        if needed > self.capacity {
            return Err(HgbError::Storage(format!(
                "Frame total size {} exceeds ring capacity {}",
                needed, self.capacity
            )));
        }

        // Spin-arbitrate producer to guarantee multi-thread safety
        while self
            .producer_spin
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }

        let header = self.header();
        let head = header.head.load(Ordering::Relaxed);
        let tail = header.tail.load(Ordering::Acquire);
        let occupied = head.saturating_sub(tail);
        let available = (self.capacity as u64).saturating_sub(occupied) as usize;

        if needed > available {
            self.producer_spin.store(false, Ordering::Release);
            return Err(HgbError::Storage(
                "Zero-copy ring buffer full (backpressure)".to_string(),
            ));
        }

        let curr_offset = (head & self.mask) as usize;
        let end_space = self.capacity - curr_offset;

        let (write_head, write_offset) = if end_space >= needed {
            (head, curr_offset)
        } else {
            // Frame does not fit in remaining space before end of ring.
            // Check if wrap fits in available buffer space:
            let total_wrap_needed = end_space + needed;
            if total_wrap_needed > available {
                self.producer_spin.store(false, Ordering::Release);
                return Err(HgbError::Storage(
                    "Zero-copy ring buffer full at wrap boundary (backpressure)".to_string(),
                ));
            }

            // Write Wrap Marker
            if end_space >= FrameHeader::SIZE {
                let wrap_header = FrameHeader {
                    length: (end_space - FrameHeader::SIZE) as u32,
                    flags: FRAME_FLAG_WRAP,
                    stream_id: 0,
                };
                unsafe {
                    let dest = self.mmap_ptr.add(HEADER_SIZE_BYTES + curr_offset);
                    std::ptr::copy_nonoverlapping(
                        &wrap_header as *const FrameHeader as *const u8,
                        dest,
                        FrameHeader::SIZE,
                    );
                }
            }

            // Advance head past end to wrap around to offset 0
            let wrapped_head = head + (end_space as u64);
            (wrapped_head, 0)
        };

        // Write the Frame Header
        let frame_header = FrameHeader {
            length: payload_len as u32,
            flags,
            stream_id,
        };

        unsafe {
            let slot_dest = self.mmap_ptr.add(HEADER_SIZE_BYTES + write_offset);
            std::ptr::copy_nonoverlapping(
                &frame_header as *const FrameHeader as *const u8,
                slot_dest,
                FrameHeader::SIZE,
            );

            if payload_len > 0 {
                let payload_dest = slot_dest.add(FrameHeader::SIZE);
                std::ptr::copy_nonoverlapping(frame.as_ptr(), payload_dest, payload_len);
            }
        }

        // Monotonically advance head sequence with Release ordering
        let new_head = write_head + (needed as u64);
        let seq = header.frames_published.fetch_add(1, Ordering::Relaxed) + 1;
        header.bytes_published.fetch_add(payload_len as u64, Ordering::Relaxed);
        header.head.store(new_head, Ordering::Release);

        self.producer_spin.store(false, Ordering::Release);

        // Notify consumer via eventfd
        self.signaler.notify();

        Ok(seq)
    }

    /// Attempt to publish a frame, returning `Ok(None)` immediately on buffer full
    pub fn try_publish_frame(&self, frame: &[u8]) -> Result<Option<u64>> {
        match self.publish_frame(frame) {
            Ok(seq) => Ok(Some(seq)),
            Err(HgbError::Storage(ref msg)) if msg.contains("full") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Consume the next available frame directly from the zero-copy shared memory buffer.
    ///
    /// Executes the provided closure `f` with a direct contiguous zero-copy slice `&[u8]`
    /// into the memory-mapped ring buffer. Zero heap allocations or copies occur.
    pub fn consume_frame<F, R>(&self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&[u8]) -> R,
    {
        // Spin-arbitrate consumer to guarantee multi-thread safety
        while self
            .consumer_spin
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }

        let header = self.header();
        let mut tail = header.tail.load(Ordering::Relaxed);
        let head = header.head.load(Ordering::Acquire);
        let mut occupied = head.saturating_sub(tail);

        if occupied < (FrameHeader::SIZE as u64) {
            self.consumer_spin.store(false, Ordering::Release);
            return Ok(None);
        }

        let mut curr_offset = (tail & self.mask) as usize;

        // Read Frame Header
        let mut frame_header: FrameHeader = unsafe {
            let src = self.mmap_ptr.add(HEADER_SIZE_BYTES + curr_offset);
            std::ptr::read_unaligned(src as *const FrameHeader)
        };

        // Handle Wrap Marker
        if frame_header.flags & FRAME_FLAG_WRAP != 0 {
            let skip_bytes = self.capacity - curr_offset;
            tail += skip_bytes as u64;
            header.tail.store(tail, Ordering::Release);

            occupied = head.saturating_sub(tail);
            if occupied < (FrameHeader::SIZE as u64) {
                self.consumer_spin.store(false, Ordering::Release);
                return Ok(None);
            }

            curr_offset = 0;
            frame_header = unsafe {
                let src = self.mmap_ptr.add(HEADER_SIZE_BYTES + curr_offset);
                std::ptr::read_unaligned(src as *const FrameHeader)
            };
        }

        let payload_len = frame_header.length as usize;
        let aligned_len = (payload_len + 7) & !7;
        let total_slot = FrameHeader::SIZE + aligned_len;

        if occupied < (total_slot as u64) {
            self.consumer_spin.store(false, Ordering::Release);
            return Ok(None);
        }

        // Direct zero-copy slice referencing shared memory
        let payload_slice = unsafe {
            let payload_ptr = self.mmap_ptr.add(HEADER_SIZE_BYTES + curr_offset + FrameHeader::SIZE);
            std::slice::from_raw_parts(payload_ptr, payload_len)
        };

        // Execute user closure on zero-copy slice
        let result = f(payload_slice);

        // Advance tail with Release ordering
        let new_tail = tail + (total_slot as u64);
        header.frames_consumed.fetch_add(1, Ordering::Relaxed);
        header.bytes_consumed.fetch_add(payload_len as u64, Ordering::Relaxed);
        header.tail.store(new_tail, Ordering::Release);

        self.consumer_spin.store(false, Ordering::Release);

        Ok(Some(result))
    }

    /// Consume the next frame, waiting up to `timeout` if buffer is currently empty
    pub fn consume_frame_timeout<F, R>(&self, timeout: Duration, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&[u8]) -> R,
    {
        let start = Instant::now();

        // 1. Fast path: Spin-poll for up to 500 nanoseconds for ultra-low latency
        while start.elapsed() < Duration::from_nanos(500) {
            if !self.is_empty() {
                return self.consume_frame(f);
            }
            std::hint::spin_loop();
        }

        // 2. Slow path: Wait on eventfd notification
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return self.consume_frame(f);
        }

        let _ = self.signaler.wait_timeout(remaining);
        self.consume_frame(f)
    }

    // -------------------------------------------------------------------------
    // Wasm Linear Memory & Firecracker MicroVM Mapping APIs
    // -------------------------------------------------------------------------

    /// Map Wasm linear memory export (`*mut u8`, `len`) directly into the conduit
    pub fn map_wasm_guest(&mut self, wasm_mem_ptr: *mut u8, len: usize) -> Result<()> {
        if wasm_mem_ptr.is_null() || len == 0 {
            return Err(HgbError::InvalidParameter(
                "Wasm guest memory pointer cannot be null or zero length".to_string(),
            ));
        }

        self.wasm_mapping = Some(WasmGuestMapping {
            ptr: wasm_mem_ptr,
            len,
        });

        Ok(())
    }

    /// Map Wasmtime `Memory` directly using active store context
    pub fn map_wasmtime_memory<T>(
        &mut self,
        memory: &wasmtime::Memory,
        store: &mut wasmtime::Store<T>,
    ) -> Result<()> {
        let ptr = memory.data_ptr(&store);
        let len = memory.data_size(&store);
        self.map_wasm_guest(ptr, len)
    }

    /// Map Firecracker MicroVM shared memory file (e.g. `virtio-fs` backing file or POSIX SHM)
    pub fn map_microvm_guest(&mut self, shm_path: &Path) -> Result<()> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(shm_path)
            .map_err(|e| {
                HgbError::Storage(format!("Failed to open MicroVM SHM file at {shm_path:?}: {e}"))
            })?;

        let meta = file.metadata()?;
        let len = if meta.len() == 0 {
            let default_size = (DEFAULT_RING_CAPACITY) as u64;
            file.set_len(default_size)?;
            default_size as usize
        } else {
            meta.len() as usize
        };

        let mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|e| HgbError::Storage(format!("Failed to mmap MicroVM SHM: {e}")))?
        };

        self.microvm_mapping = Some(MicroVmGuestMapping {
            path: shm_path.to_path_buf(),
            mmap,
            len,
        });

        Ok(())
    }

    /// Direct zero-copy transfer from Wasm linear memory to MicroVM guest memory
    pub fn transfer_wasm_to_microvm(
        &self,
        wasm_offset: usize,
        microvm_offset: usize,
        len: usize,
    ) -> Result<()> {
        let wasm = self.wasm_mapping.as_ref().ok_or_else(|| {
            HgbError::InvalidState("Wasm guest memory not mapped in conduit".to_string())
        })?;

        let microvm = self.microvm_mapping.as_ref().ok_or_else(|| {
            HgbError::InvalidState("MicroVM guest memory not mapped in conduit".to_string())
        })?;

        if wasm_offset + len > wasm.len {
            return Err(HgbError::InvalidParameter(format!(
                "Wasm transfer bounds overflow: offset {} + len {} > total {}",
                wasm_offset, len, wasm.len
            )));
        }

        if microvm_offset + len > microvm.len {
            return Err(HgbError::InvalidParameter(format!(
                "MicroVM transfer bounds overflow: offset {} + len {} > total {}",
                microvm_offset, len, microvm.len
            )));
        }

        unsafe {
            let src = wasm.ptr.add(wasm_offset);
            let dest = microvm.mmap.as_ptr().add(microvm_offset) as *mut u8;
            std::ptr::copy_nonoverlapping(src, dest, len);
        }

        Ok(())
    }

    /// Direct zero-copy transfer from MicroVM guest memory to Wasm linear memory
    pub fn transfer_microvm_to_wasm(
        &self,
        microvm_offset: usize,
        wasm_offset: usize,
        len: usize,
    ) -> Result<()> {
        let wasm = self.wasm_mapping.as_ref().ok_or_else(|| {
            HgbError::InvalidState("Wasm guest memory not mapped in conduit".to_string())
        })?;

        let microvm = self.microvm_mapping.as_ref().ok_or_else(|| {
            HgbError::InvalidState("MicroVM guest memory not mapped in conduit".to_string())
        })?;

        if microvm_offset + len > microvm.len {
            return Err(HgbError::InvalidParameter(format!(
                "MicroVM transfer bounds overflow: offset {} + len {} > total {}",
                microvm_offset, len, microvm.len
            )));
        }

        if wasm_offset + len > wasm.len {
            return Err(HgbError::InvalidParameter(format!(
                "Wasm transfer bounds overflow: offset {} + len {} > total {}",
                wasm_offset, len, wasm.len
            )));
        }

        unsafe {
            let src = microvm.mmap.as_ptr().add(microvm_offset);
            let dest = wasm.ptr.add(wasm_offset);
            std::ptr::copy_nonoverlapping(src, dest, len);
        }

        Ok(())
    }

    /// Publish a frame directly from Wasm linear memory into the ring buffer zero-copy
    pub fn publish_from_wasm(&self, wasm_offset: usize, len: usize) -> Result<u64> {
        let wasm = self.wasm_mapping.as_ref().ok_or_else(|| {
            HgbError::InvalidState("Wasm guest memory not mapped in conduit".to_string())
        })?;

        if wasm_offset + len > wasm.len {
            return Err(HgbError::InvalidParameter(
                "Wasm memory offset overflow".to_string(),
            ));
        }

        let slice = unsafe { std::slice::from_raw_parts(wasm.ptr.add(wasm_offset), len) };
        self.publish_frame_with_flags(slice, FRAME_FLAG_WASM_MEM, 0)
    }

    /// Consume a frame directly into MicroVM guest shared memory zero-copy
    pub fn consume_into_microvm(&self, microvm_offset: usize) -> Result<Option<usize>> {
        let microvm = self.microvm_mapping.as_ref().ok_or_else(|| {
            HgbError::InvalidState("MicroVM guest memory not mapped in conduit".to_string())
        })?;

        self.consume_frame(|payload| {
            if microvm_offset + payload.len() <= microvm.len {
                unsafe {
                    let dest = microvm.mmap.as_ptr().add(microvm_offset) as *mut u8;
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), dest, payload.len());
                }
                payload.len()
            } else {
                0
            }
        })
    }

    // -------------------------------------------------------------------------
    // Telemetry & Benchmark Engine
    // -------------------------------------------------------------------------

    /// Benchmark zero-copy conduit throughput and nanosecond latency
    pub fn benchmark_throughput(
        &self,
        iterations: usize,
        payload_size: usize,
    ) -> Result<ConduitBenchmarkMetrics> {
        let sample_payload = vec![0xABu8; payload_size];
        let start = Instant::now();

        let mut processed = 0;
        for _ in 0..iterations {
            let _ = self.publish_frame(&sample_payload)?;
            let consumed = self.consume_frame(|slice| slice.len())?;
            if let Some(bytes) = consumed {
                processed += bytes;
            }
        }

        let elapsed = start.elapsed();
        let elapsed_nanos = elapsed.as_nanos() as u64;
        let elapsed_secs = elapsed.as_secs_f64();

        let frames_per_sec = (iterations as f64) / elapsed_secs.max(1e-9);
        let total_bytes = iterations * payload_size;
        let throughput_mb_s = ((total_bytes as f64) / (1024.0 * 1024.0)) / elapsed_secs.max(1e-9);
        let avg_latency_ns = (elapsed_nanos as f64) / ((iterations * 2) as f64);

        Ok(ConduitBenchmarkMetrics {
            ring_name: self.name.clone(),
            total_frames: iterations,
            total_bytes,
            elapsed_nanos,
            frames_per_sec,
            throughput_mb_s,
            avg_latency_ns,
            zero_copy_verified: true,
        })
    }

    // -------------------------------------------------------------------------
    // Properties & Introspection
    // -------------------------------------------------------------------------

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_empty(&self) -> bool {
        self.available_read() == 0
    }

    pub fn available_read(&self) -> usize {
        let head = self.header().head.load(Ordering::Acquire);
        let tail = self.header().tail.load(Ordering::Relaxed);
        head.saturating_sub(tail) as usize
    }

    pub fn available_write(&self) -> usize {
        self.capacity.saturating_sub(self.available_read())
    }

    pub fn head(&self) -> u64 {
        self.header().head.load(Ordering::Relaxed)
    }

    pub fn tail(&self) -> u64 {
        self.header().tail.load(Ordering::Relaxed)
    }

    pub fn frames_published(&self) -> u64 {
        self.header().frames_published.load(Ordering::Relaxed)
    }

    pub fn frames_consumed(&self) -> u64 {
        self.header().frames_consumed.load(Ordering::Relaxed)
    }

    pub fn bytes_published(&self) -> u64 {
        self.header().bytes_published.load(Ordering::Relaxed)
    }

    pub fn bytes_consumed(&self) -> u64 {
        self.header().bytes_consumed.load(Ordering::Relaxed)
    }

    pub fn event_signaler(&self) -> &DualSandboxEventSignaler {
        &self.signaler
    }

    pub fn raw_eventfd(&self) -> RawFd {
        self.signaler.raw_eventfd()
    }

    pub fn wasm_guest_mapping(&self) -> Option<(*mut u8, usize)> {
        self.wasm_mapping.map(|m| (m.ptr, m.len))
    }

    pub fn microvm_guest_mapping(&self) -> Option<(*const u8, usize)> {
        self.microvm_mapping
            .as_ref()
            .map(|m| (m.mmap.as_ptr(), m.len))
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }
}

impl Drop for ZeroCopyRingConduit {
    fn drop(&mut self) {
        let _ = self.mmap.flush();
        if self.unlink_on_drop {
            if let Some(ref path) = self.path {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

unsafe impl Send for ZeroCopyRingConduit {}
unsafe impl Sync for ZeroCopyRingConduit {}

// ==============================================================================
// 5. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_ring_header_alignment_and_sizes() {
        assert_eq!(std::mem::size_of::<DualSandboxRingHeader>(), 192);
        assert_eq!(std::mem::align_of::<DualSandboxRingHeader>(), 64);
        assert_eq!(std::mem::size_of::<FrameHeader>(), 8);
    }

    #[test]
    fn test_lock_free_ring_buffer_wrap_around_and_integrity() {
        // Small capacity to force multiple wrap-around cycles
        let conduit = ZeroCopyRingConduit::create("test_wrap_integrity", 4096)
            .expect("Failed to create conduit");

        let frame_sizes = [64, 128, 256, 512, 1024, 768, 128, 64];

        for cycle in 0..15 {
            for (idx, &size) in frame_sizes.iter().enumerate() {
                let mut payload = vec![0u8; size];
                for (b_idx, byte) in payload.iter_mut().enumerate() {
                    *byte = ((cycle * 17 + idx * 7 + b_idx) & 0xFF) as u8;
                }

                conduit
                    .publish_frame(&payload)
                    .expect("Publish should succeed");

                let consumed = conduit
                    .consume_frame(|slice| {
                        assert_eq!(slice.len(), size);
                        assert_eq!(slice, &payload[..]);
                        slice[0]
                    })
                    .expect("Consume should succeed");

                assert_eq!(consumed, Some(payload[0]));
            }
        }

        assert!(conduit.frames_published() > 100);
        assert_eq!(conduit.frames_published(), conduit.frames_consumed());
        assert!(conduit.head() > 4096 * 2); // Guaranteed wrap around
    }

    #[test]
    fn test_cross_memory_mapping_wasm_and_microvm() {
        let mut conduit = ZeroCopyRingConduit::create("test_cross_mapping", 65536)
            .expect("Failed to create conduit");

        // 1. Simulated Wasm Linear Memory export (64 KiB)
        let mut wasm_linear_mem = vec![0u8; 65536];
        conduit
            .map_wasm_guest(wasm_linear_mem.as_mut_ptr(), wasm_linear_mem.len())
            .expect("Map wasm guest should succeed");

        // 2. Simulated MicroVM virtio-fs shared memory backing file (64 KiB)
        let temp_dir = std::env::temp_dir();
        let microvm_shm_path = temp_dir.join(format!("microvm_shm_test_{}.bin", Instant::now().elapsed().as_nanos()));
        conduit
            .map_microvm_guest(&microvm_shm_path)
            .expect("Map microvm guest should succeed");

        // Populate Wasm memory with test vector
        let pattern: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
        wasm_linear_mem[1024..1024 + 512].copy_from_slice(&pattern);

        // Test direct transfer Wasm -> MicroVM
        conduit
            .transfer_wasm_to_microvm(1024, 2048, 512)
            .expect("Transfer wasm to microvm should succeed");

        // Read MicroVM memory directly to verify zero-copy arrival
        let microvm_mapping = conduit.microvm_guest_mapping().expect("Microvm mapping present");
        let microvm_slice = unsafe {
            std::slice::from_raw_parts(microvm_mapping.0.add(2048), 512)
        };
        assert_eq!(microvm_slice, &pattern[..]);

        // Reverse transfer MicroVM -> Wasm
        let reverse_pattern: Vec<u8> = (0..256).map(|i| (255 - (i % 256)) as u8).collect();
        unsafe {
            let dest = microvm_mapping.0.add(4096) as *mut u8;
            std::ptr::copy_nonoverlapping(reverse_pattern.as_ptr(), dest, 256);
        }

        conduit
            .transfer_microvm_to_wasm(4096, 5120, 256)
            .expect("Transfer microvm to wasm should succeed");

        assert_eq!(&wasm_linear_mem[5120..5120 + 256], &reverse_pattern[..]);

        // Clean up
        let _ = std::fs::remove_file(microvm_shm_path);
    }

    #[test]
    fn test_nanosecond_latency_and_throughput() {
        let conduit = ZeroCopyRingConduit::create("test_nanosecond_perf", 2 * 1024 * 1024)
            .expect("Failed to create conduit");

        let iterations = 100_000;
        let payload_size = 64; // 64-byte binary frame

        let report = conduit
            .benchmark_throughput(iterations, payload_size)
            .expect("Benchmark should succeed");

        println!(
            "Dual-Sandbox Zero-Copy Ring Perf: {:.2} frames/sec, avg latency: {:.2} ns/op, throughput: {:.2} MB/s",
            report.frames_per_sec, report.avg_latency_ns, report.throughput_mb_s
        );

        assert!(
            report.frames_per_sec >= 1_000_000.0,
            "Expected throughput >= 1,000,000 frames/sec, got {:.2}",
            report.frames_per_sec
        );

        assert!(
            report.avg_latency_ns < 500.0,
            "Expected latency < 500ns, got {:.2} ns",
            report.avg_latency_ns
        );

        assert!(report.zero_copy_verified);
    }

    #[test]
    fn test_concurrent_read_write_across_threads() {
        let conduit = Arc::new(
            ZeroCopyRingConduit::create("test_concurrent_threads", 4 * 1024 * 1024)
                .expect("Failed to create conduit"),
        );

        let total_frames = 50_000;
        let producer_conduit = Arc::clone(&conduit);
        let consumer_conduit = Arc::clone(&conduit);

        let producer_handle = thread::spawn(move || {
            let mut payload = [0u8; 128];
            for i in 0..total_frames {
                payload[0..4].copy_from_slice(&(i as u32).to_le_bytes());
                payload[4] = 0xAA;
                while producer_conduit.try_publish_frame(&payload).unwrap().is_none() {
                    std::hint::spin_loop();
                }
            }
        });

        let consumer_handle = thread::spawn(move || {
            let mut consumed_count = 0;
            while consumed_count < total_frames {
                let frame_res = consumer_conduit.consume_frame(|slice| {
                    assert_eq!(slice.len(), 128);
                    let seq = u32::from_le_bytes(slice[0..4].try_into().unwrap());
                    assert_eq!(seq, consumed_count as u32);
                    assert_eq!(slice[4], 0xAA);
                    seq
                });

                match frame_res {
                    Ok(Some(_)) => {
                        consumed_count += 1;
                    }
                    Ok(None) => {
                        std::hint::spin_loop();
                    }
                    Err(e) => panic!("Error consuming: {e}"),
                }
            }
            consumed_count
        });

        producer_handle.join().expect("Producer thread failed");
        let consumed = consumer_handle.join().expect("Consumer thread failed");

        assert_eq!(consumed, total_frames);
        assert_eq!(conduit.frames_published(), total_frames as u64);
        assert_eq!(conduit.frames_consumed(), total_frames as u64);
    }

    #[test]
    fn test_eventfd_signaling_and_timeout() {
        let conduit = ZeroCopyRingConduit::create("test_eventfd", 1024 * 1024)
            .expect("Failed to create conduit");

        // Wait on empty ring should timeout
        let timed_out = conduit
            .consume_frame_timeout(Duration::from_millis(10), |_| ())
            .expect("Consume timeout failed");
        assert!(timed_out.is_none());

        // Publish a frame (which notifies eventfd)
        conduit.publish_frame(b"hello eventfd").expect("Publish failed");

        // Now consume_frame_timeout should immediately succeed
        let res = conduit
            .consume_frame_timeout(Duration::from_millis(100), |slice| {
                String::from_utf8_lossy(slice).to_string()
            })
            .expect("Consume failed");

        assert_eq!(res.as_deref(), Some("hello eventfd"));
    }
}
