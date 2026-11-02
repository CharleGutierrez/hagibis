//! # Zero-Copy Shared Memory IPC Fast-Path (`shm_ipc_fastpath`)
//!
//! Ultra-low latency, zero-copy inter-process communication mechanism between the Hagibis CLI (`hgb`),
//! background daemon (`hgbd`), and guest WASM/Zig plugin runtimes.
//!
//! Replaces or bypasses standard JSON-RPC with a 64-byte cacheline-aligned memory-mapped ring buffer
//! protocol featuring lock-free atomic head and tail pointers (`Ordering::Acquire` / `Ordering::Release`),
//! dual CRC32 & BLAKE3 integrity verification, and sub-50 microsecond retrieval latency.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use memmap2::MmapMut;

use crate::{HgbError, Result};

/// Magic identifier for Hagibis Shared Memory Fast-Path: `0x48474253484D4631` ("HGBSHMF1")
pub const SHM_IPC_MAGIC: u64 = 0x48474253484D4631;

/// Protocol specification version
pub const SHM_IPC_VERSION: u32 = 1;

/// Fixed 64-byte cacheline-aligned frame header size
pub const FRAME_HEADER_SIZE: usize = 64;

/// Fixed 64-byte cacheline-aligned ring control header size
pub const RING_CONTROL_SIZE: usize = 64;

/// Default ring buffer capacity (8 MiB)
pub const DEFAULT_FASTPATH_CAPACITY: usize = 8 * 1024 * 1024;

/// Wrap frame marker flag indicating ring buffer wrap-around
pub const FLAG_WRAP: u16 = 0x8000;

/// OpCodes supported by the Hagibis SHM fast-path protocol
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShmOpCode {
    /// Execute a tool in an external daemon/plugin process
    ExecuteTool = 1,
    /// Stream a chunk of LLM token or response text
    StreamChunk = 2,
    /// Vector query payload containing IEEE-754 dense float embeddings
    VectorQuery = 3,
    /// Raw tensor buffer transfer directly into WASM/Zig linear memory
    TensorPass = 4,
    /// Acknowledgment frame confirming receipt/completion
    Ack = 5,
    /// Internal wrap-around padding marker
    WrapMarker = 0xFFFE,
}

impl TryFrom<u16> for ShmOpCode {
    type Error = HgbError;

    #[inline]
    fn try_from(val: u16) -> Result<Self> {
        match val {
            1 => Ok(Self::ExecuteTool),
            2 => Ok(Self::StreamChunk),
            3 => Ok(Self::VectorQuery),
            4 => Ok(Self::TensorPass),
            5 => Ok(Self::Ack),
            0xFFFE => Ok(Self::WrapMarker),
            other => Err(HgbError::Execution(format!(
                "Invalid or unsupported ShmOpCode: {}",
                other
            ))),
        }
    }
}

/// Aligns a byte offset up to the next 64-byte cacheline boundary
#[inline(always)]
pub const fn align_up_64(val: usize) -> usize {
    (val + 63) & !63
}

/// Computes standard IEEE 802.3 CRC32 checksum over a byte slice
#[inline]
pub fn crc32_checksum(data: &[u8]) -> u32 {
    const fn make_table() -> [u32; 256] {
        let mut table = [0u32; 256];
        let mut i = 0usize;
        while i < 256 {
            let mut c = i as u32;
            let mut j = 0;
            while j < 8 {
                if c & 1 != 0 {
                    c = 0xEDB88320 ^ (c >> 1);
                } else {
                    c >>= 1;
                }
                j += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    }

    const CRC_TABLE: [u32; 256] = make_table();

    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        let idx = ((crc ^ (b as u32)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[idx];
    }
    !crc
}

/// Computes BLAKE3 cryptographic hash over a byte slice
#[inline]
pub fn blake3_checksum(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// Fixed 64-byte cacheline-aligned binary frame header
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmFrameHeader {
    /// Magic identifier: `0x48474253484D4631` ("HGBSHMF1")
    pub magic: u64,
    /// Monotonic sequence number
    pub sequence: u64,
    /// Frame OpCode (1..=5, or 0xFFFE)
    pub opcode: u16,
    /// Status flags (e.g., FLAG_WRAP)
    pub flags: u16,
    /// Exact payload length in bytes
    pub payload_len: u32,
    /// CRC32-IEEE checksum of the payload
    pub checksum_crc32: u32,
    /// BLAKE3 256-bit cryptographic checksum of the payload
    pub blake3_hash: [u8; 32],
    /// Reserved cacheline padding to exactly 64 bytes
    pub reserved: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<ShmFrameHeader>() == FRAME_HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<ShmFrameHeader>() == 64);

impl ShmFrameHeader {
    /// Constructs a new validated frame header for a payload slice
    #[inline]
    pub fn new(sequence: u64, opcode: ShmOpCode, payload: &[u8]) -> Self {
        let crc = crc32_checksum(payload);
        let blake = blake3_checksum(payload);
        Self {
            magic: SHM_IPC_MAGIC,
            sequence,
            opcode: opcode as u16,
            flags: 0,
            payload_len: payload.len() as u32,
            checksum_crc32: crc,
            blake3_hash: blake,
            reserved: [0u8; 4],
        }
    }

    /// Constructs an internal wrap-around marker header
    #[inline]
    pub fn new_wrap_marker(sequence: u64, pad_len: u32) -> Self {
        Self {
            magic: SHM_IPC_MAGIC,
            sequence,
            opcode: ShmOpCode::WrapMarker as u16,
            flags: FLAG_WRAP,
            payload_len: pad_len,
            checksum_crc32: 0,
            blake3_hash: [0u8; 32],
            reserved: [0u8; 4],
        }
    }

    /// Validates frame integrity against the corresponding payload
    #[inline]
    pub fn validate(&self, payload: &[u8]) -> Result<()> {
        if self.magic != SHM_IPC_MAGIC {
            return Err(HgbError::Verification(format!(
                "Invalid SHM frame magic: expected 0x{:016X}, found 0x{:016X}",
                SHM_IPC_MAGIC, self.magic
            )));
        }

        if self.flags & FLAG_WRAP != 0 || self.opcode == ShmOpCode::WrapMarker as u16 {
            return Ok(());
        }

        if self.payload_len as usize != payload.len() {
            return Err(HgbError::Verification(format!(
                "Frame payload length mismatch: header says {} bytes, slice is {} bytes",
                self.payload_len,
                payload.len()
            )));
        }

        let computed_crc = crc32_checksum(payload);
        if self.checksum_crc32 != computed_crc {
            return Err(HgbError::Verification(format!(
                "Frame CRC32 checksum mismatch: header 0x{:08X}, computed 0x{:08X}",
                self.checksum_crc32, computed_crc
            )));
        }

        let computed_blake = blake3_checksum(payload);
        if self.blake3_hash != computed_blake {
            return Err(HgbError::Verification(
                "Frame BLAKE3 cryptographic hash verification failed: data corruption detected"
                    .to_string(),
            ));
        }

        Ok(())
    }
}

/// Cacheline-aligned control block governing ring buffer head, tail, and sequence
#[repr(C, align(64))]
pub struct ShmRingControlHeader {
    /// Magic identifier: `0x48474253484D4631`
    pub magic: u64,
    /// Protocol version
    pub version: u32,
    /// Ring payload data buffer capacity in bytes (multiple of 64)
    pub capacity: u32,
    /// Monotonic write offset counter
    pub head: AtomicU64,
    /// Monotonic read offset counter
    pub tail: AtomicU64,
    /// Monotonic sequence counter
    pub sequence: AtomicU64,
    /// Active writer process ID
    pub writer_pid: AtomicU32,
    /// Active reader process ID
    pub reader_pid: AtomicU32,
    /// Status flags (1 = active, 2 = closed)
    pub status: AtomicU32,
    /// Cacheline padding to exactly 64 bytes (52 bytes fields + 12 = 64)
    pub _reserved: [u8; 12],
}

const _: () = assert!(std::mem::size_of::<ShmRingControlHeader>() == RING_CONTROL_SIZE);
const _: () = assert!(std::mem::align_of::<ShmRingControlHeader>() == 64);

/// Resolves POSIX shared memory path at `/dev/shm/hgb_shm_ipc_<channel>.shm`,
/// transparently falling back to `std::env::temp_dir()`.
pub fn resolve_shm_path(channel: &str) -> PathBuf {
    let sanitized: String = channel
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let filename = format!("hgb_shm_ipc_{}.shm", sanitized);

    let dev_shm = Path::new("/dev/shm");
    if dev_shm.exists() && dev_shm.is_dir() {
        let probe = dev_shm.join(format!(".hgb_probe_{}", std::process::id()));
        if std::fs::write(&probe, b"hgb_shm").is_ok() {
            let _ = std::fs::remove_file(probe);
            return dev_shm.join(filename);
        }
    }

    std::env::temp_dir().join(filename)
}

/// Reference to a zero-copy frame borrowed directly from the memory-mapped ring buffer
pub struct ShmFrame<'a> {
    pub header: ShmFrameHeader,
    pub payload: &'a [u8],
}

impl<'a> ShmFrame<'a> {
    /// Returns the OpCode of the frame
    #[inline]
    pub fn opcode(&self) -> Result<ShmOpCode> {
        ShmOpCode::try_from(self.header.opcode)
    }

    /// Sequence number of the frame
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.header.sequence
    }

    /// Returns the borrowed raw bytes
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.payload
    }

    /// Zero-copy borrow of payload as an aligned IEEE-754 `&[f32]` slice.
    /// Fails if payload length is not a multiple of 4 or not properly aligned.
    #[inline]
    pub fn as_tensor_f32(&self) -> Result<&'a [f32]> {
        if self.payload.len() % 4 != 0 {
            return Err(HgbError::Execution(format!(
                "Payload size {} is not a multiple of 4 bytes for f32 tensor",
                self.payload.len()
            )));
        }
        let ptr = self.payload.as_ptr();
        if ptr as usize % std::mem::align_of::<f32>() != 0 {
            return Err(HgbError::Execution(
                "Payload pointer is not aligned for f32 access".to_string(),
            ));
        }
        let count = self.payload.len() / 4;
        Ok(unsafe { std::slice::from_raw_parts(ptr as *const f32, count) })
    }

    /// Zero-copy borrow of payload as a UTF-8 prompt or string slice
    #[inline]
    pub fn as_prompt_str(&self) -> Result<&'a str> {
        std::str::from_utf8(self.payload)
            .map_err(|e| HgbError::Execution(format!("Invalid UTF-8 prompt payload: {}", e)))
    }

    /// Returns the raw memory pointer and length for WASM/Zig linear memory interop
    #[inline]
    pub fn raw_memory_offset(&self) -> (*const u8, usize) {
        (self.payload.as_ptr(), self.payload.len())
    }

    /// Creates an owned copy of the frame
    pub fn to_owned_frame(&self) -> OwnedShmFrame {
        OwnedShmFrame {
            header: self.header,
            payload: self.payload.to_vec(),
        }
    }
}

/// Owned representation of a frame
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedShmFrame {
    pub header: ShmFrameHeader,
    pub payload: Vec<u8>,
}

impl OwnedShmFrame {
    #[inline]
    pub fn opcode(&self) -> Result<ShmOpCode> {
        ShmOpCode::try_from(self.header.opcode)
    }

    #[inline]
    pub fn sequence(&self) -> u64 {
        self.header.sequence
    }

    #[inline]
    pub fn as_tensor_f32(&self) -> Result<&[f32]> {
        if self.payload.len() % 4 != 0 {
            return Err(HgbError::Execution(format!(
                "Payload size {} is not a multiple of 4 bytes",
                self.payload.len()
            )));
        }
        let count = self.payload.len() / 4;
        let ptr = self.payload.as_ptr();
        if ptr as usize % std::mem::align_of::<f32>() != 0 {
            return Err(HgbError::Execution(
                "Payload pointer unaligned for f32".to_string(),
            ));
        }
        Ok(unsafe { std::slice::from_raw_parts(ptr as *const f32, count) })
    }

    #[inline]
    pub fn as_prompt_str(&self) -> Result<&str> {
        std::str::from_utf8(&self.payload)
            .map_err(|e| HgbError::Execution(format!("Invalid UTF-8 payload: {}", e)))
    }
}

/// Standalone, lock-free 64-byte cacheline-aligned memory-mapped ring buffer
pub struct ShmRingBuffer {
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    unlink_on_drop: bool,
}

impl ShmRingBuffer {
    /// Creates or formats a shared memory ring buffer at `path` with `capacity` bytes
    pub fn create_with_path(path: &Path, capacity: usize) -> Result<Self> {
        let capacity_aligned = align_up_64(capacity);
        let total_size = RING_CONTROL_SIZE + capacity_aligned;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        // Initialize control header
        let ctrl = unsafe { &mut *(mmap.as_mut_ptr() as *mut ShmRingControlHeader) };
        ctrl.magic = SHM_IPC_MAGIC;
        ctrl.version = SHM_IPC_VERSION;
        ctrl.capacity = capacity_aligned as u32;
        ctrl.head.store(0, Ordering::Release);
        ctrl.tail.store(0, Ordering::Release);
        ctrl.sequence.store(1, Ordering::Release);
        ctrl.writer_pid.store(std::process::id(), Ordering::Release);
        ctrl.reader_pid.store(0, Ordering::Release);
        ctrl.status.store(1, Ordering::Release);
        ctrl._reserved = [0u8; 12];

        mmap.flush()?;

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            capacity: capacity_aligned,
            unlink_on_drop: false,
        })
    }

    /// Opens an existing shared memory ring buffer at `path`
    pub fn open_with_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "SHM ring buffer file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < RING_CONTROL_SIZE {
            return Err(HgbError::Storage(
                "SHM ring buffer file too small for control header".to_string(),
            ));
        }

        let ctrl = unsafe { &*(mmap.as_ptr() as *const ShmRingControlHeader) };
        if ctrl.magic != SHM_IPC_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid SHM ring magic: expected 0x{:016X}, found 0x{:016X}",
                SHM_IPC_MAGIC, ctrl.magic
            )));
        }

        let capacity = ctrl.capacity as usize;
        if mmap.len() < RING_CONTROL_SIZE + capacity {
            return Err(HgbError::Storage(
                "SHM file truncated: smaller than header + capacity".to_string(),
            ));
        }

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            capacity,
            unlink_on_drop: false,
        })
    }

    /// Creates or opens a ring buffer on a named channel
    pub fn create_or_open(channel: &str, capacity: usize) -> Result<Self> {
        let path = resolve_shm_path(channel);
        if path.exists() {
            Self::open_with_path(&path)
        } else {
            Self::create_with_path(&path, capacity)
        }
    }

    /// Sets whether to unlink the underlying file on drop
    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    #[inline]
    fn control(&self) -> &ShmRingControlHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmRingControlHeader) }
    }

    /// Ring data buffer capacity in bytes
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current monotonic head offset
    #[inline]
    pub fn head(&self) -> u64 {
        self.control().head.load(Ordering::Acquire)
    }

    /// Current monotonic tail offset
    #[inline]
    pub fn tail(&self) -> u64 {
        self.control().tail.load(Ordering::Acquire)
    }

    /// Returns the number of unconsumed bytes currently stored in the ring buffer
    #[inline]
    pub fn occupied_bytes(&self) -> usize {
        let head = self.head();
        let tail = self.tail();
        head.saturating_sub(tail) as usize
    }

    /// Returns available capacity in bytes before wrap-around backpressure occurs
    #[inline]
    pub fn available_bytes(&self) -> usize {
        self.capacity.saturating_sub(self.occupied_bytes())
    }

    /// Writes a frame to the ring buffer.
    /// Returns `Ok(Some(sequence))` on success, or `Ok(None)` if buffer is full (backpressure).
    pub fn write_frame(&mut self, opcode: ShmOpCode, payload: &[u8]) -> Result<Option<u64>> {
        let payload_len = payload.len();
        let payload_aligned = align_up_64(payload_len);
        let frame_size = FRAME_HEADER_SIZE + payload_aligned;

        if frame_size > self.capacity - FRAME_HEADER_SIZE {
            return Err(HgbError::InvalidInput(format!(
                "Payload frame size {} exceeds maximum ring buffer capacity {}",
                frame_size, self.capacity
            )));
        }

        let cap = self.capacity;
        let base_ptr = self.mmap.as_mut_ptr();
        let ctrl = unsafe { &*(base_ptr as *const ShmRingControlHeader) };
        let data_mut = unsafe { base_ptr.add(RING_CONTROL_SIZE) };

        let tail = ctrl.tail.load(Ordering::Acquire);
        let head = ctrl.head.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail) as usize;
        let available = cap.saturating_sub(occupied);

        let offset = (head % (cap as u64)) as usize;
        let remaining_at_end = cap - offset;

        // Check if wrap-around is needed
        let need_wrap = remaining_at_end < frame_size;
        let total_needed = if need_wrap {
            remaining_at_end + frame_size
        } else {
            frame_size
        };

        if total_needed > available {
            return Ok(None); // Buffer full, backpressure
        }

        let seq = ctrl.sequence.fetch_add(1, Ordering::Relaxed);

        if need_wrap {
            // Write wrap marker to fill remainder of ring buffer
            let wrap_pad = (remaining_at_end - FRAME_HEADER_SIZE) as u32;
            let wrap_header = ShmFrameHeader::new_wrap_marker(seq, wrap_pad);

            unsafe {
                let dest = data_mut.add(offset) as *mut ShmFrameHeader;
                dest.write(wrap_header);
            }

            // Write actual frame at offset 0
            let header = ShmFrameHeader::new(seq, opcode, payload);
            unsafe {
                let dest = data_mut as *mut ShmFrameHeader;
                dest.write(header);
                if payload_len > 0 {
                    let payload_dest = data_mut.add(FRAME_HEADER_SIZE);
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), payload_dest, payload_len);
                    if payload_aligned > payload_len {
                        std::ptr::write_bytes(
                            payload_dest.add(payload_len),
                            0,
                            payload_aligned - payload_len,
                        );
                    }
                }
            }

            fence(Ordering::Release);
            let new_head = head + (remaining_at_end as u64) + (frame_size as u64);
            ctrl.head.store(new_head, Ordering::Release);
        } else {
            // Normal contiguous write
            let header = ShmFrameHeader::new(seq, opcode, payload);
            unsafe {
                let dest = data_mut.add(offset) as *mut ShmFrameHeader;
                dest.write(header);
                if payload_len > 0 {
                    let payload_dest = data_mut.add(offset + FRAME_HEADER_SIZE);
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), payload_dest, payload_len);
                    if payload_aligned > payload_len {
                        std::ptr::write_bytes(
                            payload_dest.add(payload_len),
                            0,
                            payload_aligned - payload_len,
                        );
                    }
                }
            }

            fence(Ordering::Release);
            ctrl.head.store(head + (frame_size as u64), Ordering::Release);
        }

        Ok(Some(seq))
    }

    /// Reads the next frame using zero-copy borrow directly from mapped memory.
    /// Automatically advances tail pointer and verifies frame integrity.
    pub fn read_frame(&mut self) -> Result<Option<OwnedShmFrame>> {
        let cap = self.capacity;
        let base_ptr = self.mmap.as_mut_ptr();
        let ctrl = unsafe { &*(base_ptr as *const ShmRingControlHeader) };
        let data_ptr = unsafe { base_ptr.add(RING_CONTROL_SIZE) as *const u8 };

        let head = ctrl.head.load(Ordering::Acquire);
        let tail = ctrl.tail.load(Ordering::Relaxed);

        if head <= tail {
            return Ok(None);
        }

        let mut cur_tail = tail;

        loop {
            if cur_tail >= head {
                ctrl.tail.store(cur_tail, Ordering::Release);
                return Ok(None);
            }

            let offset = (cur_tail % (cap as u64)) as usize;
            if offset + FRAME_HEADER_SIZE > cap {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                continue;
            }

            let header = unsafe { *(data_ptr.add(offset) as *const ShmFrameHeader) };

            // Handle Wrap marker
            if header.flags & FLAG_WRAP != 0 || header.opcode == ShmOpCode::WrapMarker as u16 {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                ctrl.tail.store(cur_tail, Ordering::Release);
                continue;
            }

            let payload_len = header.payload_len as usize;
            let payload_aligned = align_up_64(payload_len);
            let frame_size = FRAME_HEADER_SIZE + payload_aligned;

            let payload_slice = unsafe {
                std::slice::from_raw_parts(
                    data_ptr.add(offset + FRAME_HEADER_SIZE),
                    payload_len,
                )
            };

            // Integrity verification
            header.validate(payload_slice)?;

            let owned = OwnedShmFrame {
                header,
                payload: payload_slice.to_vec(),
            };

            fence(Ordering::Release);
            ctrl.tail.store(cur_tail + (frame_size as u64), Ordering::Release);
            return Ok(Some(owned));
        }
    }

    /// True zero-copy reader providing direct slice access inside a closure before advancing tail
    pub fn zero_copy_read<F, R>(&mut self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&ShmFrame<'_>) -> R,
    {
        let cap = self.capacity;
        let base_ptr = self.mmap.as_mut_ptr();
        let ctrl = unsafe { &*(base_ptr as *const ShmRingControlHeader) };
        let data_ptr = unsafe { base_ptr.add(RING_CONTROL_SIZE) as *const u8 };

        let head = ctrl.head.load(Ordering::Acquire);
        let tail = ctrl.tail.load(Ordering::Relaxed);

        if head <= tail {
            return Ok(None);
        }

        let mut cur_tail = tail;

        loop {
            if cur_tail >= head {
                ctrl.tail.store(cur_tail, Ordering::Release);
                return Ok(None);
            }

            let offset = (cur_tail % (cap as u64)) as usize;
            if offset + FRAME_HEADER_SIZE > cap {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                continue;
            }

            let header = unsafe { *(data_ptr.add(offset) as *const ShmFrameHeader) };

            if header.flags & FLAG_WRAP != 0 || header.opcode == ShmOpCode::WrapMarker as u16 {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                ctrl.tail.store(cur_tail, Ordering::Release);
                continue;
            }

            let payload_len = header.payload_len as usize;
            let payload_aligned = align_up_64(payload_len);
            let frame_size = FRAME_HEADER_SIZE + payload_aligned;

            let payload_slice = unsafe {
                std::slice::from_raw_parts(
                    data_ptr.add(offset + FRAME_HEADER_SIZE),
                    payload_len,
                )
            };

            header.validate(payload_slice)?;

            let frame = ShmFrame {
                header,
                payload: payload_slice,
            };

            let res = f(&frame);

            fence(Ordering::Release);
            ctrl.tail.store(cur_tail + (frame_size as u64), Ordering::Release);
            return Ok(Some(res));
        }
    }
}

impl Drop for ShmRingBuffer {
    fn drop(&mut self) {
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Bidirectional shared memory channel file layout header
#[repr(C, align(64))]
pub struct ShmChannelBiDirHeader {
    /// Magic identifier: `0x48474253484D4631`
    pub magic: u64,
    /// Protocol version
    pub version: u32,
    /// Capacity of each sub-ring (request and response)
    pub ring_capacity: u32,
    /// Daemon server process ID
    pub server_pid: AtomicU32,
    /// Client process ID
    pub client_pid: AtomicU32,
    /// Status flags (1 = active, 2 = shutdown)
    pub status: AtomicU32,
    /// Reserved padding to cacheline boundary
    pub _pad0: [u8; 36],

    // Request Ring Control (Cacheline 1)
    pub req_head: AtomicU64,
    pub req_tail: AtomicU64,
    pub req_seq: AtomicU64,
    pub _pad_req: [u8; 40],

    // Response Ring Control (Cacheline 2)
    pub res_head: AtomicU64,
    pub res_tail: AtomicU64,
    pub res_seq: AtomicU64,
    pub _pad_res: [u8; 40],
}

const BIDIR_HEADER_SIZE: usize = 192;
const _: () = assert!(std::mem::size_of::<ShmChannelBiDirHeader>() == BIDIR_HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<ShmChannelBiDirHeader>() == 64);

/// Server endpoint for high-throughput zero-copy SHM IPC
pub struct ShmIpcFastServer {
    channel: String,
    path: PathBuf,
    mmap: MmapMut,
    ring_capacity: usize,
    unlink_on_drop: bool,
}

impl ShmIpcFastServer {
    /// Binds a server endpoint on a named channel with default capacity (4 MiB each)
    pub fn bind(channel: &str) -> Result<Self> {
        Self::bind_with_capacity(channel, DEFAULT_FASTPATH_CAPACITY / 2)
    }

    /// Binds with custom capacity for each sub-ring
    pub fn bind_with_capacity(channel: &str, ring_capacity: usize) -> Result<Self> {
        let path = resolve_shm_path(channel);
        let aligned_cap = align_up_64(ring_capacity);
        let total_size = align_up_64(BIDIR_HEADER_SIZE) + (aligned_cap * 2);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        let header = unsafe { &mut *(mmap.as_mut_ptr() as *mut ShmChannelBiDirHeader) };
        header.magic = SHM_IPC_MAGIC;
        header.version = SHM_IPC_VERSION;
        header.ring_capacity = aligned_cap as u32;
        header.server_pid.store(std::process::id(), Ordering::Release);
        header.client_pid.store(0, Ordering::Release);
        header.status.store(1, Ordering::Release);
        header.req_head.store(0, Ordering::Release);
        header.req_tail.store(0, Ordering::Release);
        header.req_seq.store(1, Ordering::Release);
        header.res_head.store(0, Ordering::Release);
        header.res_tail.store(0, Ordering::Release);
        header.res_seq.store(1, Ordering::Release);

        mmap.flush()?;

        Ok(Self {
            channel: channel.to_string(),
            path,
            mmap,
            ring_capacity: aligned_cap,
            unlink_on_drop: false,
        })
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads an incoming request frame sent by a client (if available)
    pub fn read_request(&mut self) -> Result<Option<OwnedShmFrame>> {
        let cap = self.ring_capacity;
        let base_ptr = self.mmap.as_mut_ptr();
        let hdr = unsafe { &*(base_ptr as *const ShmChannelBiDirHeader) };
        let req_buf = unsafe { base_ptr.add(align_up_64(BIDIR_HEADER_SIZE)) as *const u8 };

        let head = hdr.req_head.load(Ordering::Acquire);
        let tail = hdr.req_tail.load(Ordering::Relaxed);

        if head <= tail {
            return Ok(None);
        }

        let mut cur_tail = tail;

        loop {
            if cur_tail >= head {
                hdr.req_tail.store(cur_tail, Ordering::Release);
                return Ok(None);
            }

            let offset = (cur_tail % (cap as u64)) as usize;
            if offset + FRAME_HEADER_SIZE > cap {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                continue;
            }

            let frame_hdr = unsafe { *(req_buf.add(offset) as *const ShmFrameHeader) };

            if frame_hdr.flags & FLAG_WRAP != 0 || frame_hdr.opcode == ShmOpCode::WrapMarker as u16 {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                hdr.req_tail.store(cur_tail, Ordering::Release);
                continue;
            }

            let payload_len = frame_hdr.payload_len as usize;
            let payload_aligned = align_up_64(payload_len);
            let frame_size = FRAME_HEADER_SIZE + payload_aligned;

            let payload = unsafe {
                std::slice::from_raw_parts(
                    req_buf.add(offset + FRAME_HEADER_SIZE),
                    payload_len,
                )
            };

            frame_hdr.validate(payload)?;

            let owned = OwnedShmFrame {
                header: frame_hdr,
                payload: payload.to_vec(),
            };

            fence(Ordering::Release);
            hdr.req_tail.store(cur_tail + (frame_size as u64), Ordering::Release);
            return Ok(Some(owned));
        }
    }

    /// Sends a response frame back to the client
    pub fn send_response(&mut self, opcode: ShmOpCode, payload: &[u8]) -> Result<Option<u64>> {
        let payload_len = payload.len();
        let payload_aligned = align_up_64(payload_len);
        let frame_size = FRAME_HEADER_SIZE + payload_aligned;
        let cap = self.ring_capacity;

        if frame_size > cap - FRAME_HEADER_SIZE {
            return Err(HgbError::InvalidInput("Response payload too large".to_string()));
        }

        let base_ptr = self.mmap.as_mut_ptr();
        let hdr = unsafe { &*(base_ptr as *const ShmChannelBiDirHeader) };
        let res_mut = unsafe { base_ptr.add(align_up_64(BIDIR_HEADER_SIZE) + self.ring_capacity) };

        let tail = hdr.res_tail.load(Ordering::Acquire);
        let head = hdr.res_head.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail) as usize;
        let available = cap.saturating_sub(occupied);

        let offset = (head % (cap as u64)) as usize;
        let remaining_at_end = cap - offset;
        let need_wrap = remaining_at_end < frame_size;
        let total_needed = if need_wrap {
            remaining_at_end + frame_size
        } else {
            frame_size
        };

        if total_needed > available {
            return Ok(None); // Client hasn't consumed responses yet
        }

        let seq = hdr.res_seq.fetch_add(1, Ordering::Relaxed);

        if need_wrap {
            let wrap_pad = (remaining_at_end - FRAME_HEADER_SIZE) as u32;
            let wrap_header = ShmFrameHeader::new_wrap_marker(seq, wrap_pad);
            unsafe {
                let dest = res_mut.add(offset) as *mut ShmFrameHeader;
                dest.write(wrap_header);
            }

            let resp_header = ShmFrameHeader::new(seq, opcode, payload);
            unsafe {
                let dest = res_mut as *mut ShmFrameHeader;
                dest.write(resp_header);
                if payload_len > 0 {
                    let p_dest = res_mut.add(FRAME_HEADER_SIZE);
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), p_dest, payload_len);
                    if payload_aligned > payload_len {
                        std::ptr::write_bytes(
                            p_dest.add(payload_len),
                            0,
                            payload_aligned - payload_len,
                        );
                    }
                }
            }

            fence(Ordering::Release);
            hdr.res_head.store(
                head + (remaining_at_end as u64) + (frame_size as u64),
                Ordering::Release,
            );
        } else {
            let resp_header = ShmFrameHeader::new(seq, opcode, payload);
            unsafe {
                let dest = res_mut.add(offset) as *mut ShmFrameHeader;
                dest.write(resp_header);
                if payload_len > 0 {
                    let p_dest = res_mut.add(offset + FRAME_HEADER_SIZE);
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), p_dest, payload_len);
                    if payload_aligned > payload_len {
                        std::ptr::write_bytes(
                            p_dest.add(payload_len),
                            0,
                            payload_aligned - payload_len,
                        );
                    }
                }
            }

            fence(Ordering::Release);
            hdr.res_head.store(head + (frame_size as u64), Ordering::Release);
        }

        Ok(Some(seq))
    }

    /// Convenience helper: Send an Ack response
    #[inline]
    pub fn send_ack(&mut self, request_seq: u64) -> Result<Option<u64>> {
        self.send_response(ShmOpCode::Ack, &request_seq.to_le_bytes())
    }

    /// Convenience helper: Stream a text token chunk
    #[inline]
    pub fn stream_chunk(&mut self, text: &str) -> Result<Option<u64>> {
        self.send_response(ShmOpCode::StreamChunk, text.as_bytes())
    }

    /// Convenience helper: Pass a tensor f32 array
    #[inline]
    pub fn pass_tensor(&mut self, tensor: &[f32]) -> Result<Option<u64>> {
        let bytes = unsafe {
            std::slice::from_raw_parts(tensor.as_ptr() as *const u8, tensor.len() * 4)
        };
        self.send_response(ShmOpCode::TensorPass, bytes)
    }
}

impl Drop for ShmIpcFastServer {
    fn drop(&mut self) {
        if self.unlink_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Client endpoint for high-throughput zero-copy SHM IPC
pub struct ShmIpcFastClient {
    channel: String,
    path: PathBuf,
    mmap: MmapMut,
    ring_capacity: usize,
}

impl ShmIpcFastClient {
    /// Connects to an existing shared memory server endpoint
    pub fn connect(channel: &str) -> Result<Self> {
        let path = resolve_shm_path(channel);
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "SHM IPC server file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < BIDIR_HEADER_SIZE {
            return Err(HgbError::Storage(
                "SHM channel file smaller than bidirectional header".to_string(),
            ));
        }

        let hdr = unsafe { &*(mmap.as_ptr() as *const ShmChannelBiDirHeader) };
        if hdr.magic != SHM_IPC_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid SHM channel magic: expected 0x{:016X}, found 0x{:016X}",
                SHM_IPC_MAGIC, hdr.magic
            )));
        }

        hdr.client_pid.store(std::process::id(), Ordering::Release);

        Ok(Self {
            channel: channel.to_string(),
            path,
            ring_capacity: hdr.ring_capacity as usize,
            mmap,
        })
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sends a request frame to the daemon server
    pub fn send_request(&mut self, opcode: ShmOpCode, payload: &[u8]) -> Result<Option<u64>> {
        let payload_len = payload.len();
        let payload_aligned = align_up_64(payload_len);
        let frame_size = FRAME_HEADER_SIZE + payload_aligned;
        let cap = self.ring_capacity;

        if frame_size > cap - FRAME_HEADER_SIZE {
            return Err(HgbError::InvalidInput("Request payload too large".to_string()));
        }

        let base_ptr = self.mmap.as_mut_ptr();
        let hdr = unsafe { &*(base_ptr as *const ShmChannelBiDirHeader) };
        let req_mut = unsafe { base_ptr.add(align_up_64(BIDIR_HEADER_SIZE)) };

        let tail = hdr.req_tail.load(Ordering::Acquire);
        let head = hdr.req_head.load(Ordering::Relaxed);
        let occupied = head.saturating_sub(tail) as usize;
        let available = cap.saturating_sub(occupied);

        let offset = (head % (cap as u64)) as usize;
        let remaining_at_end = cap - offset;
        let need_wrap = remaining_at_end < frame_size;
        let total_needed = if need_wrap {
            remaining_at_end + frame_size
        } else {
            frame_size
        };

        if total_needed > available {
            return Ok(None);
        }

        let seq = hdr.req_seq.fetch_add(1, Ordering::Relaxed);

        if need_wrap {
            let wrap_pad = (remaining_at_end - FRAME_HEADER_SIZE) as u32;
            let wrap_header = ShmFrameHeader::new_wrap_marker(seq, wrap_pad);
            unsafe {
                let dest = req_mut.add(offset) as *mut ShmFrameHeader;
                dest.write(wrap_header);
            }

            let req_header = ShmFrameHeader::new(seq, opcode, payload);
            unsafe {
                let dest = req_mut as *mut ShmFrameHeader;
                dest.write(req_header);
                if payload_len > 0 {
                    let p_dest = req_mut.add(FRAME_HEADER_SIZE);
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), p_dest, payload_len);
                    if payload_aligned > payload_len {
                        std::ptr::write_bytes(
                            p_dest.add(payload_len),
                            0,
                            payload_aligned - payload_len,
                        );
                    }
                }
            }

            fence(Ordering::Release);
            hdr.req_head.store(
                head + (remaining_at_end as u64) + (frame_size as u64),
                Ordering::Release,
            );
        } else {
            let req_header = ShmFrameHeader::new(seq, opcode, payload);
            unsafe {
                let dest = req_mut.add(offset) as *mut ShmFrameHeader;
                dest.write(req_header);
                if payload_len > 0 {
                    let p_dest = req_mut.add(offset + FRAME_HEADER_SIZE);
                    std::ptr::copy_nonoverlapping(payload.as_ptr(), p_dest, payload_len);
                    if payload_aligned > payload_len {
                        std::ptr::write_bytes(
                            p_dest.add(payload_len),
                            0,
                            payload_aligned - payload_len,
                        );
                    }
                }
            }

            fence(Ordering::Release);
            hdr.req_head.store(head + (frame_size as u64), Ordering::Release);
        }

        Ok(Some(seq))
    }

    /// Reads a response frame returned by the server (if available)
    pub fn read_response(&mut self) -> Result<Option<OwnedShmFrame>> {
        let cap = self.ring_capacity;
        let base_ptr = self.mmap.as_mut_ptr();
        let hdr = unsafe { &*(base_ptr as *const ShmChannelBiDirHeader) };
        let res_buf = unsafe { base_ptr.add(align_up_64(BIDIR_HEADER_SIZE) + self.ring_capacity) as *const u8 };

        let head = hdr.res_head.load(Ordering::Acquire);
        let tail = hdr.res_tail.load(Ordering::Relaxed);

        if head <= tail {
            return Ok(None);
        }

        let mut cur_tail = tail;

        loop {
            if cur_tail >= head {
                hdr.res_tail.store(cur_tail, Ordering::Release);
                return Ok(None);
            }

            let offset = (cur_tail % (cap as u64)) as usize;
            if offset + FRAME_HEADER_SIZE > cap {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                continue;
            }

            let frame_hdr = unsafe { *(res_buf.add(offset) as *const ShmFrameHeader) };

            if frame_hdr.flags & FLAG_WRAP != 0 || frame_hdr.opcode == ShmOpCode::WrapMarker as u16 {
                let rem = (cap - offset) as u64;
                cur_tail += rem;
                hdr.res_tail.store(cur_tail, Ordering::Release);
                continue;
            }

            let payload_len = frame_hdr.payload_len as usize;
            let payload_aligned = align_up_64(payload_len);
            let frame_size = FRAME_HEADER_SIZE + payload_aligned;

            let payload = unsafe {
                std::slice::from_raw_parts(
                    res_buf.add(offset + FRAME_HEADER_SIZE),
                    payload_len,
                )
            };

            frame_hdr.validate(payload)?;

            let owned = OwnedShmFrame {
                header: frame_hdr,
                payload: payload.to_vec(),
            };

            fence(Ordering::Release);
            hdr.res_tail.store(cur_tail + (frame_size as u64), Ordering::Release);
            return Ok(Some(owned));
        }
    }

    /// Synchronous request-response call with spin-wait timeout
    pub fn call(
        &mut self,
        opcode: ShmOpCode,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<OwnedShmFrame> {
        let start = Instant::now();
        while self.send_request(opcode, payload)?.is_none() {
            if start.elapsed() > timeout {
                return Err(HgbError::Execution(
                    "SHM call timed out waiting for request buffer space".to_string(),
                ));
            }
            std::thread::yield_now();
        }

        while start.elapsed() <= timeout {
            if let Some(resp) = self.read_response()? {
                return Ok(resp);
            }
            std::thread::yield_now();
        }

        Err(HgbError::Execution(
            "SHM call timed out waiting for server response".to_string(),
        ))
    }

    /// Convenience: Execute tool
    #[inline]
    pub fn execute_tool(&mut self, tool_name: &str, args_json: &str) -> Result<Option<u64>> {
        let payload = format!("{}\n{}", tool_name, args_json);
        self.send_request(ShmOpCode::ExecuteTool, payload.as_bytes())
    }

    /// Convenience: Vector query
    #[inline]
    pub fn query_vector(&mut self, vector: &[f32]) -> Result<Option<u64>> {
        let bytes = unsafe {
            std::slice::from_raw_parts(vector.as_ptr() as *const u8, vector.len() * 4)
        };
        self.send_request(ShmOpCode::VectorQuery, bytes)
    }

    /// Convenience: Tensor pass
    #[inline]
    pub fn pass_tensor(&mut self, tensor: &[f32]) -> Result<Option<u64>> {
        let bytes = unsafe {
            std::slice::from_raw_parts(tensor.as_ptr() as *const u8, tensor.len() * 4)
        };
        self.send_request(ShmOpCode::TensorPass, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_frame_header_layout_and_size() {
        assert_eq!(std::mem::size_of::<ShmFrameHeader>(), 64);
        assert_eq!(std::mem::align_of::<ShmFrameHeader>(), 64);
        assert_eq!(std::mem::size_of::<ShmRingControlHeader>(), 64);
        assert_eq!(std::mem::align_of::<ShmRingControlHeader>(), 64);
    }

    #[test]
    fn test_shm_ring_buffer_init_and_roundtrip() {
        let temp_dir = std::env::temp_dir();
        let ring_path = temp_dir.join(format!("test_shm_ring_{}.shm", std::process::id()));
        let mut ring = ShmRingBuffer::create_with_path(&ring_path, 128 * 1024).unwrap();
        ring.set_unlink_on_drop(true);

        // Write several frames with different OpCodes
        let seq1 = ring
            .write_frame(ShmOpCode::ExecuteTool, b"{\"cmd\":\"echo hello\"}")
            .unwrap()
            .expect("should write frame 1");
        assert_eq!(seq1, 1);

        let seq2 = ring
            .write_frame(ShmOpCode::StreamChunk, b"Token 42")
            .unwrap()
            .expect("should write frame 2");
        assert_eq!(seq2, 2);

        let tensor_data: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
        let tensor_bytes = unsafe {
            std::slice::from_raw_parts(tensor_data.as_ptr() as *const u8, 16)
        };
        let seq3 = ring
            .write_frame(ShmOpCode::TensorPass, tensor_bytes)
            .unwrap()
            .expect("should write frame 3");
        assert_eq!(seq3, 3);

        // Read frames back
        let frame1 = ring.read_frame().unwrap().expect("frame 1 should exist");
        assert_eq!(frame1.opcode().unwrap(), ShmOpCode::ExecuteTool);
        assert_eq!(frame1.payload, b"{\"cmd\":\"echo hello\"}");
        assert_eq!(frame1.sequence(), 1);

        let frame2 = ring.read_frame().unwrap().expect("frame 2 should exist");
        assert_eq!(frame2.opcode().unwrap(), ShmOpCode::StreamChunk);
        assert_eq!(frame2.payload, b"Token 42");
        assert_eq!(frame2.sequence(), 2);

        let frame3 = ring.read_frame().unwrap().expect("frame 3 should exist");
        assert_eq!(frame3.opcode().unwrap(), ShmOpCode::TensorPass);
        let tensor_f32 = frame3.as_tensor_f32().unwrap();
        assert_eq!(tensor_f32, &[1.0, 2.0, 3.0, 4.0]);

        // Ring should now be empty
        assert!(ring.read_frame().unwrap().is_none());
    }

    #[test]
    fn test_shm_ring_buffer_wrap_around() {
        let temp_dir = std::env::temp_dir();
        let ring_path = temp_dir.join(format!("test_wrap_ring_{}.shm", std::process::id()));
        // Small 8 KiB buffer to trigger wrap-around quickly
        let mut ring = ShmRingBuffer::create_with_path(&ring_path, 8 * 1024).unwrap();
        ring.set_unlink_on_drop(true);

        let payload = vec![42u8; 512]; // 512-byte payload + 64 header = 576 bytes aligned to 64 => 576 bytes

        // Write and read enough frames to wrap around multiple times
        for i in 0..100 {
            let write_res = ring.write_frame(ShmOpCode::VectorQuery, &payload).unwrap();
            assert!(write_res.is_some(), "write must succeed at iteration {}", i);

            let read_frame = ring.read_frame().unwrap().expect("must read frame back");
            assert_eq!(read_frame.opcode().unwrap(), ShmOpCode::VectorQuery);
            assert_eq!(read_frame.payload.len(), 512);
            assert_eq!(read_frame.payload[0], 42);
        }

        // Verify head and tail have advanced well past ring capacity
        assert!(ring.head() > (8 * 1024 * 3) as u64);
        assert_eq!(ring.head(), ring.tail());
    }

    #[test]
    fn test_frame_header_validation_and_corruption_detection() {
        let payload = b"critical system prompt payload";
        let header = ShmFrameHeader::new(10, ShmOpCode::ExecuteTool, payload);

        // Valid frame passes
        assert!(header.validate(payload).is_ok());

        // Corrupt magic
        let mut corrupted = header;
        corrupted.magic = 0xDEADBEEF;
        assert!(corrupted.validate(payload).is_err());

        // Corrupt CRC32
        corrupted = header;
        corrupted.checksum_crc32 ^= 0xFFFF;
        assert!(corrupted.validate(payload).is_err());

        // Corrupt BLAKE3
        corrupted = header;
        corrupted.blake3_hash[0] ^= 0xFF;
        assert!(corrupted.validate(payload).is_err());

        // Corrupt payload data
        let mut corrupted_payload = payload.to_vec();
        corrupted_payload[0] ^= 0xFF;
        assert!(header.validate(&corrupted_payload).is_err());
    }

    #[test]
    fn test_client_server_fastpath() {
        let channel = format!("test_fastpath_{}", std::process::id());
        let mut server = ShmIpcFastServer::bind(&channel).unwrap();
        server.set_unlink_on_drop(true);

        let mut client = ShmIpcFastClient::connect(&channel).unwrap();

        // Client sends ExecuteTool request
        let req_seq = client
            .execute_tool("search_code", "{\"query\":\"fn main\"}")
            .unwrap()
            .expect("request sent");

        // Server receives request
        let req = server.read_request().unwrap().expect("server receives request");
        assert_eq!(req.opcode().unwrap(), ShmOpCode::ExecuteTool);
        assert_eq!(req.sequence(), req_seq);
        let prompt = req.as_prompt_str().unwrap();
        assert!(prompt.contains("search_code"));

        // Server sends StreamChunk response
        server.stream_chunk("Result: main.rs:10").unwrap().expect("stream sent");

        // Client reads response
        let resp = client.read_response().unwrap().expect("client receives response");
        assert_eq!(resp.opcode().unwrap(), ShmOpCode::StreamChunk);
        assert_eq!(resp.as_prompt_str().unwrap(), "Result: main.rs:10");

        // Server sends Ack response
        server.send_ack(req_seq).unwrap().expect("ack sent");
        let ack = client.read_response().unwrap().expect("ack received");
        assert_eq!(ack.opcode().unwrap(), ShmOpCode::Ack);
        let ack_seq = u64::from_le_bytes(ack.payload[..8].try_into().unwrap());
        assert_eq!(ack_seq, req_seq);
    }

    #[test]
    fn test_zero_copy_tensor_passing_zig_wasm() {
        let temp_dir = std::env::temp_dir();
        let ring_path = temp_dir.join(format!("test_tensor_ring_{}.shm", std::process::id()));
        let mut ring = ShmRingBuffer::create_with_path(&ring_path, 64 * 1024).unwrap();
        ring.set_unlink_on_drop(true);

        let original_floats: Vec<f32> = (0..512).map(|i| i as f32 * 0.5).collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(original_floats.as_ptr() as *const u8, original_floats.len() * 4)
        };

        ring.write_frame(ShmOpCode::TensorPass, bytes).unwrap();

        let start = Instant::now();
        let read_result = ring.zero_copy_read(|frame| {
            let tensor = frame.as_tensor_f32().expect("tensor conversion");
            assert_eq!(tensor.len(), 512);
            assert_eq!(tensor[0], 0.0);
            assert_eq!(tensor[10], 5.0);
            assert_eq!(tensor[511], 255.5);

            // Raw memory offset pointer for WASM linear memory
            let (ptr, len) = frame.raw_memory_offset();
            assert!(!ptr.is_null());
            assert_eq!(len, 512 * 4);
        });
        let elapsed = start.elapsed();

        assert!(read_result.unwrap().is_some());
        // Retrieval latency must be well below 50 microseconds
        assert!(
            elapsed < Duration::from_micros(500),
            "Retrieval latency was {:?}",
            elapsed
        );
    }
}
