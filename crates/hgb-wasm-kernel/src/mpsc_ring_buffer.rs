//! # Lock-Free Multi-Producer Single-Consumer (MPSC) Ring Buffer
//!
//! Production-grade, zero-copy, lock-free Multi-Producer Single-Consumer (MPSC) ring buffer
//! with Atomic CAS slot reservation, per-slot Turnstile sequence commit tracking, 64-byte
//! cacheline-padded atomic pointers, and Adler-32 / BLAKE3 payload integrity verification.
//!
//! ## Key Architectural Features:
//! 1. **CachePadded (64-byte aligned) Atomic Pointers**: Isolates `head` and `tail` across separate
//!    L1/L2 cachelines, eliminating false sharing between concurrent producers and the consumer.
//! 2. **Multi-Producer Atomic CAS Ticket Reservation**: Producers concurrently reserve ticket
//!    slots using `compare_exchange_weak` with `Ordering::AcqRel` on `tail`.
//! 3. **Per-Slot Turnstile Sequence & Commit State**: Every slot maintains an atomic sequence counter
//!    (turnstile) ensuring producers only write to their assigned ticket slot and consumer only reads
//!    when the slot commit state matches `head + 1`, guaranteeing strict FIFO ordering without data races.
//! 4. **Single-Consumer Dequeue with Memory Fence**: Consumer acquires slot with `Ordering::Acquire`,
//!    validates payload integrity, and advances turnstile to `head + capacity` with `Ordering::Release`.
//! 5. **Zero-Copy Payload Borrow Views**: Direct reference borrowing (`MpscPopView` / `try_pop_borrow`)
//!    eliminating heap allocations and serialization overhead.
//! 6. **Adler-32 & BLAKE3 Payload Integrity**: Fast checksum validation detecting corruptions.
//! 7. **Throughput Benchmark Suite**: Direct latency/throughput comparison against mutex-guarded rings.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;
use serde::{Deserialize, Serialize};

use crate::canonical_abi_shm_views::{
    verify_alignment, AbiError, CachePadded, CanonicalMemorySlice, MemoryAccessMode, PodCopy,
    CANONICAL_ALIGNMENT_DEFAULT,
};
pub use crate::spsc_ring_buffer::{calculate_adler32, AtomicCasOutcome};

// ============================================================================
// CONSTANTS & CHECKSUM FLAGS
// ============================================================================

/// Magic signature for MPSC Ring Buffer: ASCII "HGBMPSC1" (0x4847424D50534331)
pub const MPSC_RING_MAGIC: u64 = 0x4847424D50534331;

/// Fixed 40-byte element header size for MPSC ring buffer
pub const MPSC_RING_ELEMENT_HEADER_SIZE: usize = 40;

/// Header control block offset in bytes before slot data
pub const MPSC_RING_CONTROL_HEADER_SIZE: usize = 256;

/// Flag: No checksum
pub const FLAG_CHECKSUM_NONE: u32 = 0x0000;
/// Flag: Fast Adler-32 checksum
pub const FLAG_CHECKSUM_ADLER32: u32 = 0x0001;
/// Flag: Cryptographic BLAKE3 checksum (32-bit prefix)
pub const FLAG_CHECKSUM_BLAKE3: u32 = 0x0002;
/// Mask for checksum algorithm flags
pub const CHECKSUM_FLAG_MASK: u32 = 0x000F;

/// Flag: 64-byte cacheline padded pointers and slots
pub const FLAG_CACHE_PADDED: u32 = 0x0020;
/// Flag: Payload buffer is 64-byte aligned
pub const FLAG_PAYLOAD_ALIGNED_64: u32 = 0x0040;

// ============================================================================
// CHECKSUM ALGORITHMS
// ============================================================================

/// Compute 32-bit prefix of BLAKE3 cryptographic digest for payload verification
#[inline]
pub fn calculate_blake3_u32(data: &[u8]) -> u32 {
    let hash = blake3::hash(data);
    let bytes = hash.as_bytes();
    u32::from_le_bytes(bytes[0..4].try_into().unwrap())
}

/// Compute full 32-byte BLAKE3 cryptographic hash of payload
#[inline]
pub fn calculate_blake3_full(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

#[inline]
fn current_time_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

// ============================================================================
// WIT-COMPLIANT DATA STRUCTURES
// ============================================================================

/// Fixed 40-byte zero-copy MPSC element header matching WIT `mpsc-ring-element-header`
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MpscRingElementHeader {
    pub sequence_number: u64,
    pub producer_id: u64,
    pub payload_length: u32,
    pub flags: u32,
    pub timestamp_ns: u64,
    pub checksum: u32,
    pub _reserved: u32,
}

unsafe impl PodCopy for MpscRingElementHeader {}

impl MpscRingElementHeader {
    pub const SIZE: usize = MPSC_RING_ELEMENT_HEADER_SIZE;

    pub fn new(
        sequence_number: u64,
        producer_id: u64,
        payload_length: u32,
        flags: u32,
        timestamp_ns: u64,
        checksum: u32,
    ) -> Self {
        Self {
            sequence_number,
            producer_id,
            payload_length,
            flags,
            timestamp_ns,
            checksum,
            _reserved: 0,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.sequence_number.to_le_bytes());
        buf[8..16].copy_from_slice(&self.producer_id.to_le_bytes());
        buf[16..20].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[20..24].copy_from_slice(&self.flags.to_le_bytes());
        buf[24..32].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[32..36].copy_from_slice(&self.checksum.to_le_bytes());
        buf[36..40].copy_from_slice(&self._reserved.to_le_bytes());
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
        let producer_id = u64::from_le_bytes(slice[8..16].try_into().unwrap());
        let payload_length = u32::from_le_bytes(slice[16..20].try_into().unwrap());
        let flags = u32::from_le_bytes(slice[20..24].try_into().unwrap());
        let timestamp_ns = u64::from_le_bytes(slice[24..32].try_into().unwrap());
        let checksum = u32::from_le_bytes(slice[32..36].try_into().unwrap());
        let _reserved = u32::from_le_bytes(slice[36..40].try_into().unwrap());
        Ok(Self {
            sequence_number,
            producer_id,
            payload_length,
            flags,
            timestamp_ns,
            checksum,
            _reserved,
        })
    }
}

const _: () = assert!(std::mem::size_of::<MpscRingElementHeader>() == 40);
const _: () = assert!(std::mem::align_of::<MpscRingElementHeader>() == 8);

/// MPSC Ring Buffer Descriptor matching WIT `mpsc-ring-buffer-descriptor`
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MpscRingBufferDescriptor {
    pub ring_id: u64,
    pub capacity: u32,
    pub element_size: u32,
    pub head_offset: u32,
    pub tail_offset: u32,
    pub flags: u32,
}

unsafe impl PodCopy for MpscRingBufferDescriptor {}

impl MpscRingBufferDescriptor {
    pub fn new(ring_id: u64, capacity: u32, element_size: u32) -> Result<Self, AbiError> {
        if capacity < 2 || (capacity & (capacity - 1)) != 0 {
            return Err(AbiError::InvalidCapacity {
                capacity: capacity as usize,
                reason: "MPSC ring capacity must be a power of two and at least 2".to_string(),
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
        let flags = FLAG_CACHE_PADDED | FLAG_PAYLOAD_ALIGNED_64;

        Ok(Self {
            ring_id,
            capacity,
            element_size,
            head_offset,
            tail_offset,
            flags,
        })
    }

    #[inline(always)]
    pub fn mask(&self) -> u32 {
        self.capacity - 1
    }

    #[inline(always)]
    pub fn buffer_offset(&self) -> usize {
        MPSC_RING_CONTROL_HEADER_SIZE
    }

    #[inline(always)]
    pub fn slot_stride(&self) -> usize {
        // 8 bytes turnstile seq + 40 bytes header + element_size payload
        // Padded to 64-byte alignment to isolate adjacent slots into separate cachelines
        let raw = 8 + MpscRingElementHeader::SIZE + (self.element_size as usize);
        (raw + 63) & !63
    }

    #[inline(always)]
    pub fn required_buffer_size(&self) -> usize {
        self.buffer_offset() + (self.capacity as usize) * self.slot_stride()
    }

    #[inline(always)]
    pub fn slot_offset(&self, slot_idx: usize) -> usize {
        self.buffer_offset() + (slot_idx & (self.mask() as usize)) * self.slot_stride()
    }
}

/// MPSC zero-copy dequeue pop result matching WIT `mpsc-pop-result`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpscPopResult {
    pub header: MpscRingElementHeader,
    pub payload_slice: CanonicalMemorySlice,
    pub sequence_number: u64,
}

// ============================================================================
// BACKING STORAGE
// ============================================================================

enum MpscStorage {
    Owned(Vec<u8>),
    Mapped {
        _ptr: *mut u8,
        _len: usize,
    },
}

unsafe impl Send for MpscStorage {}
unsafe impl Sync for MpscStorage {}

// ============================================================================
// LOCK-FREE MPSC RING BUFFER
// ============================================================================

/// Lock-free Multi-Producer Single-Consumer (MPSC) ring buffer operating over Wasmtime
/// linear memory or host-allocated memory.
///
/// Features:
/// - 64-byte CachePadded atomic head and tail pointers
/// - Multi-producer atomic CAS ticket reservation on `tail` with `Ordering::AcqRel`
/// - Per-slot Turnstile sequence counter ensuring commit barrier safety
/// - Single-consumer dequeue with `Ordering::Acquire`
/// - Zero-copy payload borrow views (`MpscPopView` / `try_pop_borrow`)
pub struct MpscRingBuffer {
    descriptor: MpscRingBufferDescriptor,
    head: CachePadded<AtomicU64>,
    tail: CachePadded<AtomicU64>,
    _storage: MpscStorage,
    base_ptr: *mut u8,
    memory_len: usize,
}

unsafe impl Send for MpscRingBuffer {}
unsafe impl Sync for MpscRingBuffer {}

impl fmt::Debug for MpscRingBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MpscRingBuffer")
            .field("descriptor", &self.descriptor)
            .field("head", &self.head())
            .field("tail", &self.tail())
            .field("available", &self.available())
            .field("remaining_capacity", &self.remaining_capacity())
            .field("memory_len", &self.memory_len)
            .finish()
    }
}

impl MpscRingBuffer {
    /// Initialize a new lock-free MPSC ring buffer with owned backing memory
    pub fn new(capacity: u32, element_size: u32, ring_id: u64) -> Result<Self, AbiError> {
        let descriptor = MpscRingBufferDescriptor::new(ring_id, capacity, element_size)?;
        let total_size = descriptor.required_buffer_size();

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
            _storage: MpscStorage::Owned(buffer),
            base_ptr,
            memory_len: total_size,
        };

        unsafe {
            ring.init_control_block();
            ring.init_slots();
        }

        Ok(ring)
    }

    /// Map MPSC ring buffer directly over an existing slice of Wasmtime linear memory
    pub fn from_linear_memory(
        memory: &mut [u8],
        offset: usize,
        capacity: u32,
        element_size: u32,
        ring_id: u64,
    ) -> Result<Self, AbiError> {
        verify_alignment(offset, CANONICAL_ALIGNMENT_DEFAULT)?;
        let descriptor = MpscRingBufferDescriptor::new(ring_id, capacity, element_size)?;
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
            _storage: MpscStorage::Mapped {
                _ptr: base_ptr,
                _len: required,
            },
            base_ptr,
            memory_len: required,
        };

        unsafe {
            ring.init_control_block();
            ring.init_slots();
        }

        Ok(ring)
    }

    /// Initialize the control block header in linear memory
    unsafe fn init_control_block(&self) {
        let ptr = self.base_ptr;
        // 0..8: Magic
        std::ptr::copy_nonoverlapping(&MPSC_RING_MAGIC.to_le_bytes()[0], ptr, 8);
        // 8..16: Ring ID
        std::ptr::copy_nonoverlapping(&self.descriptor.ring_id.to_le_bytes()[0], ptr.add(8), 8);
        // 16..20: Capacity
        std::ptr::copy_nonoverlapping(&self.descriptor.capacity.to_le_bytes()[0], ptr.add(16), 4);
        // 20..24: Element size
        std::ptr::copy_nonoverlapping(&self.descriptor.element_size.to_le_bytes()[0], ptr.add(20), 4);
        // 24..28: Buffer offset
        let buf_off = self.descriptor.buffer_offset() as u32;
        std::ptr::copy_nonoverlapping(&buf_off.to_le_bytes()[0], ptr.add(24), 4);
        // 28..32: Flags
        std::ptr::copy_nonoverlapping(&self.descriptor.flags.to_le_bytes()[0], ptr.add(28), 4);
        // 32..36: Mask
        let mask = self.descriptor.mask();
        std::ptr::copy_nonoverlapping(&mask.to_le_bytes()[0], ptr.add(32), 4);
        // 36..40: Slot stride
        let slot_stride = self.descriptor.slot_stride() as u32;
        std::ptr::copy_nonoverlapping(&slot_stride.to_le_bytes()[0], ptr.add(36), 4);
        // 64..72: Head pointer
        std::ptr::copy_nonoverlapping(&0u64.to_le_bytes()[0], ptr.add(self.descriptor.head_offset as usize), 8);
        // 128..136: Tail pointer
        std::ptr::copy_nonoverlapping(&0u64.to_le_bytes()[0], ptr.add(self.descriptor.tail_offset as usize), 8);
    }

    /// Initialize Turnstile sequences for each slot.
    /// Slot i is initialized to sequence i, signifying it is ready for ticket i.
    unsafe fn init_slots(&self) {
        for i in 0..self.descriptor.capacity {
            let slot_seq = self.slot_sequence(i as usize);
            slot_seq.store(i as u64, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    fn slot_sequence(&self, slot_idx: usize) -> &AtomicU64 {
        let offset = self.descriptor.slot_offset(slot_idx);
        unsafe {
            let ptr = self.base_ptr.add(offset) as *const AtomicU64;
            &*ptr
        }
    }

    #[inline(always)]
    fn slot_header_ptr(&self, slot_idx: usize) -> *mut u8 {
        let offset = self.descriptor.slot_offset(slot_idx) + 8;
        unsafe { self.base_ptr.add(offset) }
    }

    #[inline(always)]
    fn slot_payload_ptr(&self, slot_idx: usize) -> *mut u8 {
        let offset = self.descriptor.slot_offset(slot_idx) + 8 + MpscRingElementHeader::SIZE;
        unsafe { self.base_ptr.add(offset) }
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

    pub fn descriptor(&self) -> &MpscRingBufferDescriptor {
        &self.descriptor
    }

    pub fn memory_len(&self) -> usize {
        self.memory_len
    }

    /// Read current atomic head position
    #[inline(always)]
    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// Read current atomic tail position
    #[inline(always)]
    pub fn tail(&self) -> u64 {
        self.tail.load(Ordering::Acquire)
    }

    /// Query available elements ready to be popped by the consumer
    pub fn available(&self) -> u32 {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if tail > head {
            (tail - head).min(self.descriptor.capacity as u64) as u32
        } else {
            0
        }
    }

    /// Query remaining write capacity before backpressure
    pub fn remaining_capacity(&self) -> u32 {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Relaxed);
        let occupied = tail.saturating_sub(head);
        (self.descriptor.capacity as u64).saturating_sub(occupied) as u32
    }

    /// Check if ring buffer is completely empty
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.available() == 0
    }

    /// Check if ring buffer is full
    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.remaining_capacity() == 0
    }

    /// Atomic CAS on head pointer
    pub fn cas_head(&self, expected: u64, desired: u64) -> AtomicCasOutcome {
        match self.head.compare_exchange(expected, desired, Ordering::AcqRel, Ordering::Acquire) {
            Ok(prev) => {
                self.sync_head_to_memory(desired);
                AtomicCasOutcome::new(true, prev, desired)
            }
            Err(actual) => AtomicCasOutcome::new(false, actual, actual),
        }
    }

    /// Atomic CAS on tail pointer
    pub fn cas_tail(&self, expected: u64, desired: u64) -> AtomicCasOutcome {
        match self.tail.compare_exchange(expected, desired, Ordering::AcqRel, Ordering::Acquire) {
            Ok(prev) => {
                self.sync_tail_to_memory(desired);
                AtomicCasOutcome::new(true, prev, desired)
            }
            Err(actual) => AtomicCasOutcome::new(false, actual, actual),
        }
    }

    // ========================================================================
    // MULTI-PRODUCER ENQUEUE (ATOMIC CAS TICKET RESERVATION + TURNSTILE COMMIT)
    // ========================================================================

    /// Enqueue an element into the MPSC ring buffer using atomic CAS slot reservation.
    ///
    /// Multi-producer thread safety is achieved by:
    /// 1. Concurrently claiming a ticket on `tail` via `compare_exchange_weak` with `Ordering::AcqRel`.
    /// 2. Verifying the slot's turnstile sequence matches the reserved ticket.
    /// 3. Copying header and payload directly into the isolated slot buffer.
    /// 4. Committing the slot by advancing its turnstile sequence to `ticket + 1` with `Ordering::Release`.
    pub fn try_push(&self, mut header: MpscRingElementHeader, payload: &[u8]) -> Result<u64, AbiError> {
        let payload_len = payload.len();
        if payload_len > self.descriptor.element_size as usize {
            return Err(AbiError::BufferOverflow {
                needed: payload_len,
                capacity: self.descriptor.element_size as usize,
            });
        }

        // Calculate payload checksum based on flags
        let checksum = match header.flags & CHECKSUM_FLAG_MASK {
            FLAG_CHECKSUM_BLAKE3 => calculate_blake3_u32(payload),
            FLAG_CHECKSUM_NONE => 0,
            _ => calculate_adler32(payload),
        };
        header.payload_length = payload_len as u32;
        header.checksum = checksum;

        let capacity = self.descriptor.capacity as u64;
        let mask = capacity - 1;

        let mut ticket = self.tail.load(Ordering::Relaxed);
        loop {
            let head = self.head.load(Ordering::Relaxed);
            if ticket.wrapping_sub(head) >= capacity {
                let head_acq = self.head.load(Ordering::Acquire);
                if ticket.wrapping_sub(head_acq) >= capacity {
                    return Err(AbiError::RingFull {
                        capacity: capacity as usize,
                    });
                }
            }

            match self.tail.compare_exchange_weak(
                ticket,
                ticket.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    break;
                }
                Err(actual) => {
                    ticket = actual;
                    std::hint::spin_loop();
                }
            }
        }

        let slot_idx = (ticket & mask) as usize;
        let slot_seq = self.slot_sequence(slot_idx);

        // Turnstile check: wait if this slot hasn't completed previous cycle
        while slot_seq.load(Ordering::Acquire) != ticket {
            std::hint::spin_loop();
        }

        header.sequence_number = ticket;
        if header.timestamp_ns == 0 {
            header.timestamp_ns = current_time_nanos();
        }

        // Write header and payload into the reserved slot buffer
        unsafe {
            let header_ptr = self.slot_header_ptr(slot_idx);
            let header_bytes = header.to_bytes();
            std::ptr::copy_nonoverlapping(header_bytes.as_ptr(), header_ptr, MpscRingElementHeader::SIZE);

            let payload_ptr = self.slot_payload_ptr(slot_idx);
            std::ptr::copy_nonoverlapping(payload.as_ptr(), payload_ptr, payload_len);
        }

        // Turnstile sequence commit: advance sequence to ticket + 1
        slot_seq.store(ticket.wrapping_add(1), Ordering::Release);

        Ok(ticket)
    }

    /// Enqueue with backpressure: spins or yields until space is available or timeout expires
    pub fn push_blocking(
        &self,
        header: MpscRingElementHeader,
        payload: &[u8],
        timeout: Option<std::time::Duration>,
    ) -> Result<u64, AbiError> {
        let start = Instant::now();
        let mut backoff = 0;
        loop {
            match self.try_push(header, payload) {
                Ok(seq) => return Ok(seq),
                Err(AbiError::RingFull { .. }) => {
                    if let Some(t) = timeout {
                        if start.elapsed() >= t {
                            return Err(AbiError::RingFull {
                                capacity: self.descriptor.capacity as usize,
                            });
                        }
                    }
                    backoff += 1;
                    if backoff > 64 {
                        std::thread::yield_now();
                        backoff = 0;
                    } else {
                        std::hint::spin_loop();
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Push slice using Canonical ABI memory slice descriptor
    pub fn push_cas_slice(
        &self,
        header: MpscRingElementHeader,
        slice: &CanonicalMemorySlice,
        memory: &[u8],
    ) -> Result<u64, AbiError> {
        let offset = slice.ptr as usize;
        let len = slice.len as usize;
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

        let payload = &memory[offset..end];
        self.try_push(header, payload)
    }

    // ========================================================================
    // SINGLE-CONSUMER DEQUEUE (TURNSTILE VERIFICATION + MEMORY FENCE)
    // ========================================================================

    /// Dequeue an element from the MPSC ring buffer with acquire/release ordering
    /// and payload checksum integrity validation.
    pub fn try_pop(&self) -> Result<(MpscRingElementHeader, Vec<u8>), AbiError> {
        let head = self.head.load(Ordering::Relaxed);
        let capacity = self.descriptor.capacity as u64;
        let mask = capacity - 1;
        let slot_idx = (head & mask) as usize;
        let slot_seq = self.slot_sequence(slot_idx);
        let expected_seq = head.wrapping_add(1);

        if slot_seq.load(Ordering::Acquire) != expected_seq {
            return Err(AbiError::RingEmpty);
        }

        // Slot is committed by producer! Read header and payload.
        let header = unsafe {
            let header_ptr = self.slot_header_ptr(slot_idx);
            let header_slice = std::slice::from_raw_parts(header_ptr, MpscRingElementHeader::SIZE);
            MpscRingElementHeader::from_bytes(header_slice)?
        };

        let payload_len = header.payload_length as usize;
        if payload_len > self.descriptor.element_size as usize {
            return Err(AbiError::OutOfBounds {
                offset: 0,
                len: payload_len,
                capacity: self.descriptor.element_size as usize,
            });
        }

        let payload = unsafe {
            let payload_ptr = self.slot_payload_ptr(slot_idx);
            std::slice::from_raw_parts(payload_ptr, payload_len).to_vec()
        };

        // Verify payload integrity checksum if enabled
        if (header.flags & CHECKSUM_FLAG_MASK) != FLAG_CHECKSUM_NONE {
            let is_valid = match header.flags & CHECKSUM_FLAG_MASK {
                FLAG_CHECKSUM_BLAKE3 => calculate_blake3_u32(&payload) == header.checksum,
                _ => calculate_adler32(&payload) == header.checksum,
            };

            if !is_valid {
                return Err(AbiError::CryptographicValidationFailed {
                    reason: format!(
                        "Payload checksum mismatch: expected {:#x}, got calculated checksum",
                        header.checksum
                    ),
                });
            }
        }

        // Release slot turnstile for next cycle
        slot_seq.store(head.wrapping_add(capacity), Ordering::Release);

        // Advance head pointer
        let next_head = head.wrapping_add(1);
        self.head.store(next_head, Ordering::Release);
        self.sync_head_to_memory(next_head);

        Ok((header, payload))
    }

    /// Zero-copy dequeue borrow view: provides direct references to the slot's header
    /// and payload slice without heap allocation or copying.
    pub fn try_pop_borrow<R, F>(&self, f: F) -> Result<R, AbiError>
    where
        F: FnOnce(&MpscRingElementHeader, &[u8]) -> R,
    {
        let head = self.head.load(Ordering::Relaxed);
        let capacity = self.descriptor.capacity as u64;
        let mask = capacity - 1;
        let slot_idx = (head & mask) as usize;
        let slot_seq = self.slot_sequence(slot_idx);
        let expected_seq = head.wrapping_add(1);

        if slot_seq.load(Ordering::Acquire) != expected_seq {
            return Err(AbiError::RingEmpty);
        }

        let header = unsafe {
            let header_ptr = self.slot_header_ptr(slot_idx);
            let header_slice = std::slice::from_raw_parts(header_ptr, MpscRingElementHeader::SIZE);
            MpscRingElementHeader::from_bytes(header_slice)?
        };

        let payload_len = header.payload_length as usize;
        if payload_len > self.descriptor.element_size as usize {
            return Err(AbiError::OutOfBounds {
                offset: 0,
                len: payload_len,
                capacity: self.descriptor.element_size as usize,
            });
        }

        let payload_slice = unsafe {
            let payload_ptr = self.slot_payload_ptr(slot_idx);
            std::slice::from_raw_parts(payload_ptr, payload_len)
        };

        if (header.flags & CHECKSUM_FLAG_MASK) != FLAG_CHECKSUM_NONE {
            let is_valid = match header.flags & CHECKSUM_FLAG_MASK {
                FLAG_CHECKSUM_BLAKE3 => calculate_blake3_u32(payload_slice) == header.checksum,
                _ => calculate_adler32(payload_slice) == header.checksum,
            };

            if !is_valid {
                return Err(AbiError::CryptographicValidationFailed {
                    reason: format!(
                        "Payload checksum mismatch: expected {:#x}, got calculated checksum",
                        header.checksum
                    ),
                });
            }
        }

        let result = f(&header, payload_slice);

        // Release slot turnstile for next cycle
        slot_seq.store(head.wrapping_add(capacity), Ordering::Release);

        // Advance head pointer
        let next_head = head.wrapping_add(1);
        self.head.store(next_head, Ordering::Release);
        self.sync_head_to_memory(next_head);

        Ok(result)
    }

    /// Obtain an RAII zero-copy borrow view of the next available slot.
    /// The slot is automatically committed and head advanced when the view is dropped.
    pub fn try_pop_view(&self) -> Result<MpscPopView<'_>, AbiError> {
        let head = self.head.load(Ordering::Relaxed);
        let capacity = self.descriptor.capacity as u64;
        let mask = capacity - 1;
        let slot_idx = (head & mask) as usize;
        let slot_seq = self.slot_sequence(slot_idx);
        let expected_seq = head.wrapping_add(1);

        if slot_seq.load(Ordering::Acquire) != expected_seq {
            return Err(AbiError::RingEmpty);
        }

        let header = unsafe {
            let header_ptr = self.slot_header_ptr(slot_idx);
            let header_slice = std::slice::from_raw_parts(header_ptr, MpscRingElementHeader::SIZE);
            MpscRingElementHeader::from_bytes(header_slice)?
        };

        let payload_len = header.payload_length as usize;
        if payload_len > self.descriptor.element_size as usize {
            return Err(AbiError::OutOfBounds {
                offset: 0,
                len: payload_len,
                capacity: self.descriptor.element_size as usize,
            });
        }

        let payload_ptr = self.slot_payload_ptr(slot_idx);
        let payload_slice = unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) };

        if (header.flags & CHECKSUM_FLAG_MASK) != FLAG_CHECKSUM_NONE {
            let is_valid = match header.flags & CHECKSUM_FLAG_MASK {
                FLAG_CHECKSUM_BLAKE3 => calculate_blake3_u32(payload_slice) == header.checksum,
                _ => calculate_adler32(payload_slice) == header.checksum,
            };

            if !is_valid {
                return Err(AbiError::CryptographicValidationFailed {
                    reason: format!(
                        "Payload checksum mismatch: expected {:#x}, got calculated checksum",
                        header.checksum
                    ),
                });
            }
        }

        Ok(MpscPopView {
            ring: self,
            header,
            payload_ptr,
            payload_len,
            head,
            slot_idx,
            committed: false,
        })
    }

    /// Dequeue and return WIT-compliant `MpscPopResult`
    pub fn pop_result(&self) -> Result<MpscPopResult, AbiError> {
        let (header, payload) = self.try_pop()?;
        let seq = header.sequence_number;
        let slice = CanonicalMemorySlice {
            ptr: payload.as_ptr() as u64,
            len: payload.len() as u64,
            alignment: 64,
            access_mode: MemoryAccessMode::ReadOnly,
        };
        Ok(MpscPopResult {
            header,
            payload_slice: slice,
            sequence_number: seq,
        })
    }
}

// ============================================================================
// ZERO-COPY POP VIEW (RAII)
// ============================================================================

/// RAII Zero-copy payload borrow view.
/// Exposes direct in-memory references to header and payload without copying.
/// Advances `head` and turns the turnstile when dropped or committed.
pub struct MpscPopView<'a> {
    ring: &'a MpscRingBuffer,
    header: MpscRingElementHeader,
    payload_ptr: *const u8,
    payload_len: usize,
    head: u64,
    slot_idx: usize,
    committed: bool,
}

impl<'a> MpscPopView<'a> {
    #[inline(always)]
    pub fn header(&self) -> &MpscRingElementHeader {
        &self.header
    }

    #[inline(always)]
    pub fn payload(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.payload_ptr, self.payload_len) }
    }

    #[inline(always)]
    pub fn sequence_number(&self) -> u64 {
        self.header.sequence_number
    }

    #[inline(always)]
    pub fn producer_id(&self) -> u64 {
        self.header.producer_id
    }

    pub fn commit(mut self) {
        self.do_commit();
    }

    fn do_commit(&mut self) {
        if !self.committed {
            self.committed = true;
            let capacity = self.ring.descriptor.capacity as u64;
            let slot_seq = self.ring.slot_sequence(self.slot_idx);
            slot_seq.store(self.head.wrapping_add(capacity), Ordering::Release);
            let next_head = self.head.wrapping_add(1);
            self.ring.head.store(next_head, Ordering::Release);
            self.ring.sync_head_to_memory(next_head);
        }
    }
}

impl<'a> Drop for MpscPopView<'a> {
    fn drop(&mut self) {
        self.do_commit();
    }
}

// ============================================================================
// MICROBENCHMARK & THROUGHPUT MEASUREMENT SUITE
// ============================================================================

/// Throughput and latency execution report comparing lock-free MPSC ring against mutex-guarded ring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpscBenchmarkReport {
    pub num_producers: usize,
    pub items_per_producer: usize,
    pub total_items: usize,
    pub payload_size: usize,
    pub lock_free_duration_nanos: u64,
    pub lock_free_throughput_ops_sec: f64,
    pub lock_free_avg_latency_nanos: f64,
    pub mutex_duration_nanos: u64,
    pub mutex_throughput_ops_sec: f64,
    pub mutex_avg_latency_nanos: f64,
    pub speedup_ratio: f64,
    pub latency_reduction_percent: f64,
}

struct MutexRingBuffer {
    capacity: usize,
    queue: Mutex<VecDeque<Vec<u8>>>,
    not_full: Condvar,
    not_empty: Condvar,
}

impl MutexRingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            not_full: Condvar::new(),
            not_empty: Condvar::new(),
        }
    }

    fn push(&self, item: Vec<u8>) {
        let mut q = self.queue.lock().unwrap();
        while q.len() >= self.capacity {
            q = self.not_full.wait(q).unwrap();
        }
        q.push_back(item);
        self.not_empty.notify_one();
    }

    fn pop(&self) -> Vec<u8> {
        let mut q = self.queue.lock().unwrap();
        while q.is_empty() {
            q = self.not_empty.wait(q).unwrap();
        }
        let item = q.pop_front().unwrap();
        self.not_full.notify_one();
        item
    }
}

/// Run microbenchmark measuring throughput and latency of lock-free MPSC ring buffer
/// versus a standard mutex-guarded ring under multi-producer contention.
pub fn benchmark_mpsc_throughput(
    num_producers: usize,
    items_per_producer: usize,
    payload_size: usize,
) -> MpscBenchmarkReport {
    let total_items = num_producers * items_per_producer;
    // Bounded capacity to induce realistic queue contention
    let ring_capacity = 256u32;
    let payload = vec![0xABu8; payload_size];

    // ------------------------------------------------------------------------
    // 1. Lock-free MPSC Ring Buffer Benchmark
    // ------------------------------------------------------------------------
    let lock_free_ring = Arc::new(
        MpscRingBuffer::new(ring_capacity, payload_size as u32, 1)
            .expect("failed to create lock-free ring"),
    );

    let lf_ring_consumer = Arc::clone(&lock_free_ring);
    let lf_start = Instant::now();

    let consumer_handle = std::thread::spawn(move || {
        let mut popped = 0;
        while popped < total_items {
            match lf_ring_consumer.try_pop_borrow(|_, _| ()) {
                Ok(_) => popped += 1,
                Err(AbiError::RingEmpty) => std::hint::spin_loop(),
                Err(e) => panic!("consumer unexpected error: {:?}", e),
            }
        }
    });

    let mut producer_handles = Vec::with_capacity(num_producers);
    for producer_id in 0..num_producers {
        let ring = Arc::clone(&lock_free_ring);
        let p_payload = payload.clone();
        producer_handles.push(std::thread::spawn(move || {
            let header = MpscRingElementHeader::new(0, producer_id as u64, payload_size as u32, FLAG_CHECKSUM_NONE, 0, 0);
            for _ in 0..items_per_producer {
                loop {
                    match ring.try_push(header, &p_payload) {
                        Ok(_) => break,
                        Err(AbiError::RingFull { .. }) => std::hint::spin_loop(),
                        Err(e) => panic!("producer unexpected error: {:?}", e),
                    }
                }
            }
        }));
    }

    for h in producer_handles {
        h.join().unwrap();
    }
    consumer_handle.join().unwrap();
    let lock_free_duration = lf_start.elapsed();
    let lf_nanos = lock_free_duration.as_nanos() as u64;

    // ------------------------------------------------------------------------
    // 2. Mutex-guarded Ring Buffer Benchmark (Baseline)
    // ------------------------------------------------------------------------
    let mutex_ring = Arc::new(MutexRingBuffer::new(ring_capacity as usize));
    let m_ring_consumer = Arc::clone(&mutex_ring);
    let m_start = Instant::now();

    let m_consumer_handle = std::thread::spawn(move || {
        for _ in 0..total_items {
            let _ = m_ring_consumer.pop();
        }
    });

    let mut m_producer_handles = Vec::with_capacity(num_producers);
    for _ in 0..num_producers {
        let ring = Arc::clone(&mutex_ring);
        let p_payload = payload.clone();
        m_producer_handles.push(std::thread::spawn(move || {
            for _ in 0..items_per_producer {
                ring.push(p_payload.clone());
            }
        }));
    }

    for h in m_producer_handles {
        h.join().unwrap();
    }
    m_consumer_handle.join().unwrap();
    let mutex_duration = m_start.elapsed();
    let m_nanos = mutex_duration.as_nanos() as u64;

    // ------------------------------------------------------------------------
    // Metrics computation
    // ------------------------------------------------------------------------
    let lf_throughput = (total_items as f64) / lock_free_duration.as_secs_f64();
    let lf_avg_latency = (lf_nanos as f64) / (total_items as f64);

    let m_throughput = (total_items as f64) / mutex_duration.as_secs_f64();
    let m_avg_latency = (m_nanos as f64) / (total_items as f64);

    let speedup_ratio = (m_nanos as f64) / (lf_nanos.max(1) as f64);
    let latency_reduction_percent = if m_nanos > lf_nanos {
        ((m_nanos - lf_nanos) as f64 / m_nanos as f64) * 100.0
    } else {
        0.0
    };

    MpscBenchmarkReport {
        num_producers,
        items_per_producer,
        total_items,
        payload_size,
        lock_free_duration_nanos: lf_nanos,
        lock_free_throughput_ops_sec: lf_throughput,
        lock_free_avg_latency_nanos: lf_avg_latency,
        mutex_duration_nanos: m_nanos,
        mutex_throughput_ops_sec: m_throughput,
        mutex_avg_latency_nanos: m_avg_latency,
        speedup_ratio,
        latency_reduction_percent,
    }
}
