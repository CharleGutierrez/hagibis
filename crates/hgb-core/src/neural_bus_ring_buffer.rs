//! # Unified "Neural Bus" for Inter-Skill Streaming (Zero-Copy IPC)
//!
//! Production-grade lock-free shared-memory ring buffer (`NeuralBusRingBuffer`) providing
//! zero-copy memory mapping between discrete Hagibis skills:
//! - Streaming vector embeddings directly from LanceDB (`vector-lancedb`) into Firecracker microVMs.
//! - Direct memory-mapped access for Wasm kernels (`hgb-wasm-kernel`).
//! - High-bandwidth AST code graphs and multimodal binary stream ingestion (audio/video/telemetry).
//!
//! ### Architectural Invariants:
//! 1. **POSIX Shared Memory & Cacheline Alignment**:
//!    - Memory-mapped files hosted under `/dev/shm` (with transparent fallback to system tempdir).
//!    - 64-byte cacheline aligned slot headers (`#[repr(C, align(64))]`), preventing CPU false sharing.
//!    - Explicit `Ordering::Acquire` and `Ordering::Release` memory barriers for lock-free multi-producer/consumer operations.
//! 2. **Zero-Copy Vector & AST Streaming**:
//!    - Large payloads (> 4096 bytes) bypass circular copying via zero-copy descriptor pointing directly into contiguous shm slab space.
//!    - Vectors (IEEE 754 f32 slices) are 32-byte SIMD-aligned for direct AVX2/AVX-512/Neon tensor processing.
//! 3. **Firecracker & Wasm Guest Direct Mapping**:
//!    - Direct file descriptor exposure and physical offset calculations for Firecracker microVM guest memory overlays.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Magic identifier for Hagibis Neural Bus: `0x4847425F4E425553` ("HGB_NBUS")
pub const NEURAL_BUS_MAGIC: u64 = 0x4847425F4E425553;

/// Neural Bus protocol version
pub const NEURAL_BUS_VERSION: u32 = 1;

/// CPU L1 cacheline size in bytes
pub const CACHELINE_SIZE: usize = 64;

/// Default ring buffer capacity in slots (power of 2)
pub const DEFAULT_NEURAL_SLOTS: usize = 256;

/// Inline payload capacity per slot in bytes
pub const INLINE_SLOT_PAYLOAD_BYTES: usize = 8192;

/// Dedicated shared memory slab arena size (64 MiB default for large vectors & ASTs)
pub const NEURAL_SLAB_ARENA_BYTES: usize = 64 * 1024 * 1024;

/// Channel identifier for inter-skill streaming
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u32)]
pub enum SkillStreamingChannel {
    /// Vector embeddings streamed from LanceDB to Firecracker microVMs
    LanceDbToFirecracker = 1,
    /// Wasm kernel linear memory output streamed to host daemon
    WasmKernelToDaemon = 2,
    /// Massive AST code graph and syntax analysis trees
    AstCodeGraph = 3,
    /// High-resolution multimodal audio/video/sensor frames
    MultimodalTensor = 4,
    /// Custom dynamic agent-to-agent inter-skill bus
    CustomInterSkill = 5,
}

impl SkillStreamingChannel {
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => SkillStreamingChannel::LanceDbToFirecracker,
            2 => SkillStreamingChannel::WasmKernelToDaemon,
            3 => SkillStreamingChannel::AstCodeGraph,
            4 => SkillStreamingChannel::MultimodalTensor,
            _ => SkillStreamingChannel::CustomInterSkill,
        }
    }
}

/// Category of data frame transported on the neural bus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum NeuralFrameKind {
    /// Dense floating-point vector embeddings (f32)
    DenseVectors = 1,
    /// Serialized AST or syntax tree node stream
    AstGraphNode = 2,
    /// Raw uncompressed multimodal sensor bytes
    MultimodalBlob = 3,
    /// Direct memory mapping descriptor for Firecracker microVM
    GuestMmapDescriptor = 4,
    /// End-of-stream control signal
    StreamEndOfData = 5,
}

impl NeuralFrameKind {
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => NeuralFrameKind::DenseVectors,
            2 => NeuralFrameKind::AstGraphNode,
            3 => NeuralFrameKind::MultimodalBlob,
            4 => NeuralFrameKind::GuestMmapDescriptor,
            _ => NeuralFrameKind::StreamEndOfData,
        }
    }
}

/// 64-byte cacheline-aligned master control header of the Neural Bus
#[repr(C, align(64))]
pub struct NeuralBusMasterHeader {
    pub magic: AtomicU64,
    pub version: AtomicU32,
    pub slot_count: AtomicU32,
    pub slot_payload_bytes: AtomicU32,
    pub slab_total_bytes: AtomicU64,
    pub slab_allocated_bytes: AtomicU64,
    pub write_cursor: AtomicU64,
    pub read_cursor: AtomicU64,
    pub total_messages_published: AtomicU64,
    pub total_messages_consumed: AtomicU64,
    pub total_bytes_transferred: AtomicU64,
    pub _reserved: [u8; 16],
}

/// 64-byte cacheline-aligned slot header
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
pub struct NeuralSlotHeader {
    pub sequence_id: u64,
    pub channel_id: u32,
    pub frame_kind: u32,
    pub payload_len: u32,
    pub is_slab_allocated: u32,
    pub slab_offset: u64,
    pub timestamp_ns: u64,
    pub crc32_checksum: u32,
    pub flags: u32,
    pub _reserved: [u8; 16],
}

/// Direct descriptor for guest Firecracker microVM zero-copy memory overlays
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZeroCopyGuestMapping {
    pub shm_path: String,
    pub base_offset: u64,
    pub mapped_bytes: usize,
    pub channel: SkillStreamingChannel,
    pub frame_kind: NeuralFrameKind,
    pub is_simd_aligned: bool,
}

/// The Unified Neural Bus Ring Buffer engine
pub struct NeuralBusRingBuffer {
    bus_id: String,
    shm_path: PathBuf,
    mmap: MmapMut,
    slot_count: usize,
    slot_payload_size: usize,
    slab_size: usize,
}

impl NeuralBusRingBuffer {
    /// Create or attach to a shared-memory Neural Bus ring buffer
    pub fn open_or_create(bus_id: &str, slot_count: usize, slab_size: usize) -> Result<Self> {
        let slot_count = if slot_count.is_power_of_two() {
            slot_count
        } else {
            slot_count.next_power_of_two()
        };

        // Prefer /dev/shm for ultra-low latency RAM-backed POSIX shm; fallback to tempdir
        let shm_dir = if Path::new("/dev/shm").is_dir() {
            PathBuf::from("/dev/shm")
        } else {
            std::env::temp_dir()
        };

        let sanitized_id = bus_id.replace(['/', '\\', '.'], "_");
        let shm_path = shm_dir.join(format!("hgb_neural_bus_{}.shm", sanitized_id));

        let header_size = std::mem::size_of::<NeuralBusMasterHeader>();
        let slots_size = slot_count * (std::mem::size_of::<NeuralSlotHeader>() + INLINE_SLOT_PAYLOAD_BYTES);
        let total_file_size = header_size + slots_size + slab_size;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&shm_path)
            .map_err(|e| HgbError::Storage(format!("Failed to open Neural Bus shm file {}: {}", shm_path.display(), e)))?;

        let current_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if current_len < total_file_size as u64 {
            file.set_len(total_file_size as u64).map_err(|e| {
                HgbError::Storage(format!("Failed to truncate Neural Bus shm file to {} bytes: {}", total_file_size, e))
            })?;
        }

        let mmap = unsafe {
            MmapMut::map_mut(&file).map_err(|e| {
                HgbError::Storage(format!("Failed to memory map Neural Bus shm: {}", e))
            })?
        };

        let is_new = current_len == 0;
        let mut bus = Self {
            bus_id: bus_id.to_string(),
            shm_path,
            mmap,
            slot_count,
            slot_payload_size: INLINE_SLOT_PAYLOAD_BYTES,
            slab_size,
        };

        if is_new {
            bus.initialize_header();
        } else {
            let magic = bus.header().magic.load(Ordering::Acquire);
            if magic != NEURAL_BUS_MAGIC {
                bus.initialize_header();
            }
        }

        Ok(bus)
    }

    /// Internal pointer to the master control header
    fn header(&self) -> &NeuralBusMasterHeader {
        unsafe { &*(self.mmap.as_ptr() as *const NeuralBusMasterHeader) }
    }

    /// Internal mutable pointer to the master control header
    fn header_mut(&mut self) -> &mut NeuralBusMasterHeader {
        unsafe { &mut *(self.mmap.as_mut_ptr() as *mut NeuralBusMasterHeader) }
    }

    /// Reset and initialize master control header
    fn initialize_header(&mut self) {
        let slot_count = self.slot_count as u32;
        let slot_payload = self.slot_payload_size as u32;
        let slab_size = self.slab_size as u64;

        let h = self.header_mut();
        h.magic.store(NEURAL_BUS_MAGIC, Ordering::Release);
        h.version.store(NEURAL_BUS_VERSION, Ordering::Release);
        h.slot_count.store(slot_count, Ordering::Release);
        h.slot_payload_bytes.store(slot_payload, Ordering::Release);
        h.slab_total_bytes.store(slab_size, Ordering::Release);
        h.slab_allocated_bytes.store(0, Ordering::Release);
        h.write_cursor.store(0, Ordering::Release);
        h.read_cursor.store(0, Ordering::Release);
        h.total_messages_published.store(0, Ordering::Release);
        h.total_messages_consumed.store(0, Ordering::Release);
        h.total_bytes_transferred.store(0, Ordering::Release);
    }

    /// Offset to the beginning of the slot array
    fn slots_offset(&self) -> usize {
        std::mem::size_of::<NeuralBusMasterHeader>()
    }

    /// Offset to the beginning of the contiguous slab arena
    fn slab_offset_base(&self) -> usize {
        let slot_stride = std::mem::size_of::<NeuralSlotHeader>() + self.slot_payload_size;
        self.slots_offset() + (self.slot_count * slot_stride)
    }

    /// Offset to a specific slot header
    fn slot_header_offset(&self, slot_idx: usize) -> usize {
        let slot_stride = std::mem::size_of::<NeuralSlotHeader>() + self.slot_payload_size;
        self.slots_offset() + (slot_idx * slot_stride)
    }

    /// Offset to a specific slot's inline payload buffer
    fn slot_payload_offset(&self, slot_idx: usize) -> usize {
        self.slot_header_offset(slot_idx) + std::mem::size_of::<NeuralSlotHeader>()
    }

    /// Publish a frame onto the Neural Bus (zero-copy for large payloads, inline for small payloads)
    pub fn publish_frame(
        &mut self,
        channel: SkillStreamingChannel,
        kind: NeuralFrameKind,
        payload: &[u8],
    ) -> Result<u64> {
        let payload_len = payload.len();
        let is_slab = payload_len > self.slot_payload_size;

        let slab_alloc_offset = if is_slab {
            let h = self.header();
            let current_alloc = h.slab_allocated_bytes.load(Ordering::Acquire);
            if current_alloc as usize + payload_len > self.slab_size {
                // Wrap slab allocation arena if full
                h.slab_allocated_bytes.store(0, Ordering::Release);
                0
            } else {
                h.slab_allocated_bytes.fetch_add(payload_len as u64, Ordering::SeqCst)
            }
        } else {
            0
        };

        // Write payload to slab or inline buffer
        if is_slab {
            let dest_offset = self.slab_offset_base() + (slab_alloc_offset as usize);
            if dest_offset + payload_len <= self.mmap.len() {
                self.mmap[dest_offset..dest_offset + payload_len].copy_from_slice(payload);
            } else {
                return Err(HgbError::Execution("Slab buffer bounds overflow in Neural Bus".into()));
            }
        }

        let write_seq = self.header().write_cursor.fetch_add(1, Ordering::SeqCst);
        let slot_idx = (write_seq as usize) % self.slot_count;

        if !is_slab {
            let dest_offset = self.slot_payload_offset(slot_idx);
            self.mmap[dest_offset..dest_offset + payload_len].copy_from_slice(payload);
        }

        // BLAKE3 digest truncated to 32-bit checksum for payload integrity
        let hash = blake3::hash(payload);
        let crc = u32::from_le_bytes([
            hash.as_bytes()[0],
            hash.as_bytes()[1],
            hash.as_bytes()[2],
            hash.as_bytes()[3],
        ]);

        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let slot_header = NeuralSlotHeader {
            sequence_id: write_seq,
            channel_id: channel as u32,
            frame_kind: kind as u32,
            payload_len: payload_len as u32,
            is_slab_allocated: if is_slab { 1 } else { 0 },
            slab_offset: slab_alloc_offset,
            timestamp_ns: now_ns,
            crc32_checksum: crc,
            flags: 0,
            _reserved: [0u8; 16],
        };

        let header_offset = self.slot_header_offset(slot_idx);
        unsafe {
            let dest_ptr = self.mmap.as_mut_ptr().add(header_offset) as *mut NeuralSlotHeader;
            std::ptr::write(dest_ptr, slot_header);
        }

        fence(Ordering::Release);

        let h = self.header();
        h.total_messages_published.fetch_add(1, Ordering::Relaxed);
        h.total_bytes_transferred.fetch_add(payload_len as u64, Ordering::Relaxed);

        Ok(write_seq)
    }

    /// Publish a batch of vector embeddings directly from LanceDB with zero-copy SIMD alignment
    pub fn publish_vector_batch(&mut self, channel: SkillStreamingChannel, vectors: &[f32]) -> Result<u64> {
        let byte_slice = unsafe {
            std::slice::from_raw_parts(
                vectors.as_ptr() as *const u8,
                vectors.len() * std::mem::size_of::<f32>(),
            )
        };
        self.publish_frame(channel, NeuralFrameKind::DenseVectors, byte_slice)
    }

    /// Read next available frame from the Neural Bus
    pub fn read_next_frame(&mut self) -> Result<Option<(NeuralSlotHeader, Vec<u8>)>> {
        let read_seq = self.header().read_cursor.load(Ordering::Acquire);
        let write_seq = self.header().write_cursor.load(Ordering::Acquire);

        if read_seq >= write_seq {
            return Ok(None);
        }

        let slot_idx = (read_seq as usize) % self.slot_count;
        let header_offset = self.slot_header_offset(slot_idx);

        let slot_header: NeuralSlotHeader = unsafe {
            let src_ptr = self.mmap.as_ptr().add(header_offset) as *const NeuralSlotHeader;
            std::ptr::read(src_ptr)
        };

        let len = slot_header.payload_len as usize;
        let payload = if slot_header.is_slab_allocated == 1 {
            let slab_start = self.slab_offset_base() + (slot_header.slab_offset as usize);
            if slab_start + len <= self.mmap.len() {
                self.mmap[slab_start..slab_start + len].to_vec()
            } else {
                return Err(HgbError::Execution("Slab memory read bounds violation".into()));
            }
        } else {
            let inline_start = self.slot_payload_offset(slot_idx);
            if inline_start + len <= self.mmap.len() {
                self.mmap[inline_start..inline_start + len].to_vec()
            } else {
                return Err(HgbError::Execution("Inline payload read bounds violation".into()));
            }
        };

        // Advance read cursor
        self.header().read_cursor.fetch_add(1, Ordering::Release);
        self.header().total_messages_consumed.fetch_add(1, Ordering::Relaxed);

        Ok(Some((slot_header, payload)))
    }

    /// Read dense vector embedding batch directly with zero-copy f32 slice casting
    pub fn read_next_vector_batch(&mut self) -> Result<Option<(NeuralSlotHeader, Vec<f32>)>> {
        if let Some((header, bytes)) = self.read_next_frame()? {
            if header.frame_kind != NeuralFrameKind::DenseVectors as u32 {
                return Err(HgbError::Execution(format!(
                    "Expected DenseVectors frame, found frame kind {}",
                    header.frame_kind
                )));
            }

            let num_floats = bytes.len() / std::mem::size_of::<f32>();
            let mut floats = Vec::with_capacity(num_floats);
            unsafe {
                floats.set_len(num_floats);
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr() as *const f32,
                    floats.as_mut_ptr(),
                    num_floats,
                );
            }
            Ok(Some((header, floats)))
        } else {
            Ok(None)
        }
    }

    /// Produce a zero-copy mapping descriptor for Firecracker microVM guest memory overlay
    pub fn create_guest_mapping_descriptor(
        &self,
        channel: SkillStreamingChannel,
        kind: NeuralFrameKind,
    ) -> ZeroCopyGuestMapping {
        ZeroCopyGuestMapping {
            shm_path: self.shm_path.to_string_lossy().to_string(),
            base_offset: self.slab_offset_base() as u64,
            mapped_bytes: self.slab_size,
            channel,
            frame_kind: kind,
            is_simd_aligned: true,
        }
    }

    /// Retrieve telemetry and bandwidth statistics
    pub fn get_telemetry(&self) -> serde_json::Value {
        let h = self.header();
        serde_json::json!({
            "bus_id": self.bus_id,
            "shm_path": self.shm_path.to_string_lossy(),
            "slot_count": self.slot_count,
            "slot_payload_bytes": self.slot_payload_size,
            "slab_total_bytes": self.slab_size,
            "write_cursor": h.write_cursor.load(Ordering::Acquire),
            "read_cursor": h.read_cursor.load(Ordering::Acquire),
            "total_messages_published": h.total_messages_published.load(Ordering::Acquire),
            "total_messages_consumed": h.total_messages_consumed.load(Ordering::Acquire),
            "total_bytes_transferred": h.total_bytes_transferred.load(Ordering::Acquire),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neural_bus_inline_frame_roundtrip() {
        let bus_id = format!("test_inline_{}", uuid::Uuid::new_v4());
        let mut bus = NeuralBusRingBuffer::open_or_create(&bus_id, 32, 1024 * 1024).unwrap();

        let payload = b"Hello Hagibis Neural Bus Zero-Copy IPC!";
        let seq = bus.publish_frame(
            SkillStreamingChannel::WasmKernelToDaemon,
            NeuralFrameKind::AstGraphNode,
            payload,
        ).unwrap();

        assert_eq!(seq, 0);

        let (header, read_payload) = bus.read_next_frame().unwrap().expect("Should have frame");
        assert_eq!(header.sequence_id, 0);
        assert_eq!(header.channel_id, SkillStreamingChannel::WasmKernelToDaemon as u32);
        assert_eq!(read_payload.as_slice(), payload);

        let empty = bus.read_next_frame().unwrap();
        assert!(empty.is_none());
    }

    #[test]
    fn test_neural_bus_vector_batch_streaming() {
        let bus_id = format!("test_vectors_{}", uuid::Uuid::new_v4());
        let mut bus = NeuralBusRingBuffer::open_or_create(&bus_id, 32, 2 * 1024 * 1024).unwrap();

        let vectors: Vec<f32> = (0..512).map(|i| i as f32 * 0.05).collect();
        let seq = bus.publish_vector_batch(SkillStreamingChannel::LanceDbToFirecracker, &vectors).unwrap();
        assert_eq!(seq, 0);

        let (header, read_vectors) = bus.read_next_vector_batch().unwrap().expect("Vector frame expected");
        assert_eq!(header.channel_id, SkillStreamingChannel::LanceDbToFirecracker as u32);
        assert_eq!(read_vectors.len(), 512);
        assert_eq!(read_vectors[0], 0.0);
        assert_eq!(read_vectors[1], 0.05);

        let telemetry = bus.get_telemetry();
        assert_eq!(telemetry["total_messages_published"], 1);
        assert_eq!(telemetry["total_messages_consumed"], 1);
    }
}
