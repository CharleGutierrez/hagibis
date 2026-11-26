//! # Lock-Free SPSC Ring Buffers & Cryptographic WIT Validation
//!
//! Hardened with:
//! 1. Single-Producer Single-Consumer (SPSC) ring buffers with 64-byte CachePadded atomic pointers,
//!    Adler-32 slot checksums, and linear memory capping.
//! 2. Strict cryptographic validation (BLAKE3 + Ed25519) on all incoming WASM component/module payloads.
//! 3. Wasmtime fuel consumption and epoch-based interruption preemption.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use serde::{Deserialize, Serialize};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

#[cfg(not(target_arch = "wasm32"))]
use wasmtime::{self, AsContext, AsContextMut, StoreContextMut};

use crate::canonical_abi_shm_views::{
    verify_alignment, AbiError, CachePadded, CanonicalMemorySlice, MemoryAccessMode, PodCopy,
    CANONICAL_ALIGNMENT_DEFAULT, CANONICAL_HEADER_SIZE, SPSC_RING_CONTROL_HEADER_SIZE,
    SPSC_RING_ELEMENT_HEADER_SIZE, SPSC_RING_MAGIC, WASM_PAGE_SIZE_BYTES,
};

/// Compute fast Adler-32 checksum for payload integrity verification
#[inline]
pub fn calculate_adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[inline]
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

// ============================================================================
// SINGLE-PRODUCER SINGLE-CONSUMER (SPSC) RING BUFFER WITH MEMORY CAPPING
// ============================================================================

/// Fixed 32-byte zero-copy SPSC element header with checksum matching WIT `spsc-ring-element-header`
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpscRingElementHeader {
    pub sequence_number: u64,
    pub payload_length: u32,
    pub flags: u32,
    pub timestamp_ns: u64,
    pub checksum: u32,
    pub _reserved: u32,
}

unsafe impl PodCopy for SpscRingElementHeader {}

impl SpscRingElementHeader {
    pub const SIZE: usize = SPSC_RING_ELEMENT_HEADER_SIZE;

    pub fn new(sequence_number: u64, payload_length: u32, flags: u32, timestamp_ns: u64, checksum: u32) -> Self {
        Self {
            sequence_number,
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
        buf[8..12].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[12..16].copy_from_slice(&self.flags.to_le_bytes());
        buf[16..24].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[24..28].copy_from_slice(&self.checksum.to_le_bytes());
        buf[28..32].copy_from_slice(&self._reserved.to_le_bytes());
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
        let checksum = u32::from_le_bytes(slice[24..28].try_into().unwrap());
        let _reserved = u32::from_le_bytes(slice[28..32].try_into().unwrap());
        Ok(Self {
            sequence_number,
            payload_length,
            flags,
            timestamp_ns,
            checksum,
            _reserved,
        })
    }
}

const _: () = assert!(std::mem::size_of::<SpscRingElementHeader>() == 32);
const _: () = assert!(std::mem::align_of::<SpscRingElementHeader>() == 8);

/// SPSC Ring Buffer Descriptor matching WIT `spsc-ring-buffer-descriptor`
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpscRingBufferDescriptor {
    pub view_id: u64,
    pub ring_capacity: u32,
    pub element_size: u32,
    pub head_offset: u32,
    pub tail_offset: u32,
    pub buffer_offset: u32,
    pub mask: u32,
    pub linear_memory_cap_bytes: u64,
    pub is_cache_padded: bool,
}

impl SpscRingBufferDescriptor {
    pub fn new(
        view_id: u64,
        ring_capacity: u32,
        element_size: u32,
        linear_memory_cap_bytes: u64,
    ) -> Result<Self, AbiError> {
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

        let desc = Self {
            view_id,
            ring_capacity,
            element_size,
            head_offset,
            tail_offset,
            buffer_offset,
            mask,
            linear_memory_cap_bytes,
            is_cache_padded: true,
        };

        let req = desc.required_buffer_size();
        if req as u64 > linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: req,
                cap_bytes: linear_memory_cap_bytes as usize,
            });
        }

        Ok(desc)
    }

    #[inline(always)]
    pub fn slot_stride(&self) -> usize {
        let raw = SpscRingElementHeader::SIZE + (self.element_size as usize);
        (raw + 63) & !63
    }

    pub fn required_buffer_size(&self) -> usize {
        (self.buffer_offset as usize) + (self.ring_capacity as usize) * self.slot_stride()
    }

    #[inline(always)]
    pub fn slot_offset(&self, slot_idx: usize) -> usize {
        (self.buffer_offset as usize) + (slot_idx & (self.mask as usize)) * self.slot_stride()
    }
}

/// SPSC zero-copy dequeue pop result matching WIT `spsc-pop-result`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpscPopResult {
    pub header: SpscRingElementHeader,
    pub payload_slice: CanonicalMemorySlice,
    pub sequence_number: u64,
}

/// Atomic Compare-And-Swap (CAS) outcome record matching WIT `atomic-cas-outcome`
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomicCasOutcome {
    pub success: bool,
    pub previous_value: u64,
    pub current_value: u64,
}

unsafe impl PodCopy for AtomicCasOutcome {}

impl AtomicCasOutcome {
    #[inline(always)]
    pub fn new(success: bool, previous_value: u64, current_value: u64) -> Self {
        Self {
            success,
            previous_value,
            current_value,
        }
    }

    #[inline(always)]
    pub fn is_success(&self) -> bool {
        self.success
    }
}

enum SpscStorage {
    Owned(Vec<u8>),
    Mapped {
        ptr: *mut u8,
        len: usize,
    },
}

unsafe impl Send for SpscStorage {}
unsafe impl Sync for SpscStorage {}

/// Lock-free Single-Producer Single-Consumer (SPSC) ring buffer operating over Wasmtime linear memory.
/// Hardened with cache-line padding (CachePadded), atomic acquire/release index tracking,
/// memory barrier safety, and strictly enforced linear memory capping.
pub struct SpscRingBuffer {
    descriptor: SpscRingBufferDescriptor,
    head: CachePadded<AtomicU64>,
    tail: CachePadded<AtomicU64>,
    cached_tail: CachePadded<AtomicU64>,
    cached_head: CachePadded<AtomicU64>,
    storage: SpscStorage,
    base_ptr: *mut u8,
    memory_len: usize,
}

unsafe impl Send for SpscRingBuffer {}
unsafe impl Sync for SpscRingBuffer {}

impl SpscRingBuffer {
    /// Initialize a new lock-free SPSC ring buffer with owned backing memory
    pub fn new(
        capacity: u32,
        element_size: u32,
        view_id: u64,
        linear_memory_cap_bytes: u64,
    ) -> Result<Self, AbiError> {
        let descriptor = SpscRingBufferDescriptor::new(view_id, capacity, element_size, linear_memory_cap_bytes)?;
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
            cached_tail: CachePadded::new(AtomicU64::new(0)),
            cached_head: CachePadded::new(AtomicU64::new(0)),
            storage: SpscStorage::Owned(buffer),
            base_ptr,
            memory_len: total_size,
        };

        unsafe {
            ring.init_control_block();
        }

        Ok(ring)
    }

    /// Map SPSC ring buffer directly over an existing slice of Wasmtime linear memory
    pub fn from_linear_memory(
        memory: &mut [u8],
        offset: usize,
        capacity: u32,
        element_size: u32,
        view_id: u64,
        linear_memory_cap_bytes: u64,
    ) -> Result<Self, AbiError> {
        verify_alignment(offset, CANONICAL_ALIGNMENT_DEFAULT)?;
        let descriptor = SpscRingBufferDescriptor::new(view_id, capacity, element_size, linear_memory_cap_bytes)?;
        let required = descriptor.required_buffer_size();

        let end = offset.checked_add(required).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len: required,
            capacity: memory.len(),
        })?;

        if end as u64 > linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: end,
                cap_bytes: linear_memory_cap_bytes as usize,
            });
        }

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
            cached_tail: CachePadded::new(AtomicU64::new(0)),
            cached_head: CachePadded::new(AtomicU64::new(0)),
            storage: SpscStorage::Mapped {
                ptr: base_ptr,
                len: required,
            },
            base_ptr,
            memory_len: memory.len(),
        };

        unsafe {
            ring.init_control_block();
        }

        Ok(ring)
    }

    /// Format control block in linear memory
    unsafe fn init_control_block(&self) {
        let ptr = self.base_ptr;
        // 0..8: Magic
        std::ptr::copy_nonoverlapping(&SPSC_RING_MAGIC.to_le_bytes()[0], ptr, 8);
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
        // 32..40: Linear memory cap bytes
        std::ptr::copy_nonoverlapping(&self.descriptor.linear_memory_cap_bytes.to_le_bytes()[0], ptr.add(32), 8);
        // 40..44: Flags (1 = cache padded)
        std::ptr::copy_nonoverlapping(&1u32.to_le_bytes()[0], ptr.add(40), 4);
        // 64..72: Head pointer
        std::ptr::copy_nonoverlapping(&0u64.to_le_bytes()[0], ptr.add(self.descriptor.head_offset as usize), 8);
        // 128..136: Tail pointer
        std::ptr::copy_nonoverlapping(&0u64.to_le_bytes()[0], ptr.add(self.descriptor.tail_offset as usize), 8);
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

    /// Enqueue an element into the SPSC ring buffer with acquire/release ordering and memory capping.
    pub fn try_push(&self, mut header: SpscRingElementHeader, payload: &[u8]) -> Result<u64, AbiError> {
        if payload.len() > self.descriptor.element_size as usize {
            return Err(AbiError::BufferOverflow {
                needed: payload.len(),
                capacity: self.descriptor.element_size as usize,
            });
        }

        let head = self.head.load(Ordering::Relaxed);
        let mut tail = self.cached_tail.load(Ordering::Relaxed);

        if head.wrapping_sub(tail) >= self.descriptor.ring_capacity as u64 {
            tail = self.tail.load(Ordering::Acquire);
            self.cached_tail.store(tail, Ordering::Relaxed);
            if head.wrapping_sub(tail) >= self.descriptor.ring_capacity as u64 {
                return Err(AbiError::RingFull {
                    capacity: self.descriptor.ring_capacity as usize,
                });
            }
        }

        let slot_idx = (head & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);
        let slot_end = slot_offset + self.descriptor.slot_stride();

        if slot_end as u64 > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: slot_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        if slot_end > self.memory_len {
            return Err(AbiError::OutOfBounds {
                offset: slot_offset,
                len: self.descriptor.slot_stride(),
                capacity: self.memory_len,
            });
        }

        header.sequence_number = head;
        header.payload_length = payload.len() as u32;
        header.checksum = calculate_adler32(payload);

        unsafe {
            let header_ptr = self.base_ptr.add(slot_offset);
            let header_bytes = header.to_bytes();
            std::ptr::copy_nonoverlapping(header_bytes.as_ptr(), header_ptr, SpscRingElementHeader::SIZE);

            let payload_ptr = header_ptr.add(SpscRingElementHeader::SIZE);
            std::ptr::copy_nonoverlapping(payload.as_ptr(), payload_ptr, payload.len());
        }

        std::sync::atomic::fence(Ordering::Release);

        let next_head = head.wrapping_add(1);
        self.head.store(next_head, Ordering::Release);
        self.sync_head_to_memory(next_head);

        Ok(head)
    }

    /// Dequeue an element from the SPSC ring buffer with acquire/release ordering and checksum validation.
    pub fn try_pop(&self) -> Result<(SpscRingElementHeader, Vec<u8>), AbiError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.cached_head.load(Ordering::Relaxed);

        if head == tail {
            head = self.head.load(Ordering::Acquire);
            self.cached_head.store(head, Ordering::Relaxed);
            if head == tail {
                return Err(AbiError::RingEmpty);
            }
        }

        let slot_idx = (tail & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);
        let slot_end = slot_offset + self.descriptor.slot_stride();

        if slot_end as u64 > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: slot_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        if slot_end > self.memory_len {
            return Err(AbiError::OutOfBounds {
                offset: slot_offset,
                len: self.descriptor.slot_stride(),
                capacity: self.memory_len,
            });
        }

        std::sync::atomic::fence(Ordering::Acquire);

        let (header, payload) = unsafe {
            let header_ptr = self.base_ptr.add(slot_offset);
            let mut header_buf = [0u8; SpscRingElementHeader::SIZE];
            std::ptr::copy_nonoverlapping(header_ptr, header_buf.as_mut_ptr(), SpscRingElementHeader::SIZE);
            let header = SpscRingElementHeader::from_bytes(&header_buf)?;

            let payload_len = header.payload_length as usize;
            if payload_len > self.descriptor.element_size as usize {
                return Err(AbiError::BufferOverflow {
                    needed: payload_len,
                    capacity: self.descriptor.element_size as usize,
                });
            }

            let payload_ptr = header_ptr.add(SpscRingElementHeader::SIZE);
            let mut payload = vec![0u8; payload_len];
            std::ptr::copy_nonoverlapping(payload_ptr, payload.as_mut_ptr(), payload_len);

            (header, payload)
        };

        let computed_checksum = calculate_adler32(&payload);
        if header.checksum != computed_checksum {
            return Err(AbiError::CryptographicValidationFailed {
                reason: format!(
                    "SPSC ring slot checksum corruption at seq {}: header checksum 0x{:08x} != computed 0x{:08x}",
                    header.sequence_number, header.checksum, computed_checksum
                ),
            });
        }

        std::sync::atomic::fence(Ordering::Release);

        let next_tail = tail.wrapping_add(1);
        self.tail.store(next_tail, Ordering::Release);
        self.sync_tail_to_memory(next_tail);

        Ok((header, payload))
    }

    /// Zero-copy pop returning a CanonicalMemorySlice into linear memory without allocations.
    pub fn pop_zero_copy(&self) -> Result<SpscPopResult, AbiError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let mut head = self.cached_head.load(Ordering::Relaxed);

        if head == tail {
            head = self.head.load(Ordering::Acquire);
            self.cached_head.store(head, Ordering::Relaxed);
            if head == tail {
                return Err(AbiError::RingEmpty);
            }
        }

        let slot_idx = (tail & (self.descriptor.mask as u64)) as usize;
        let slot_offset = self.descriptor.slot_offset(slot_idx);
        let slot_end = slot_offset + self.descriptor.slot_stride();

        if slot_end as u64 > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: slot_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        if slot_end > self.memory_len {
            return Err(AbiError::OutOfBounds {
                offset: slot_offset,
                len: self.descriptor.slot_stride(),
                capacity: self.memory_len,
            });
        }

        std::sync::atomic::fence(Ordering::Acquire);

        let header_ptr = unsafe { self.base_ptr.add(slot_offset) };
        let mut header_buf = [0u8; SpscRingElementHeader::SIZE];
        unsafe {
            std::ptr::copy_nonoverlapping(header_ptr, header_buf.as_mut_ptr(), SpscRingElementHeader::SIZE);
        }
        let header = SpscRingElementHeader::from_bytes(&header_buf)?;

        let payload_ptr = unsafe { header_ptr.add(SpscRingElementHeader::SIZE) };
        let slice = CanonicalMemorySlice {
            ptr: payload_ptr as u64,
            len: header.payload_length as u64,
            alignment: 8,
            access_mode: MemoryAccessMode::ReadOnly,
        };

        std::sync::atomic::fence(Ordering::Release);

        let next_tail = tail.wrapping_add(1);
        self.tail.store(next_tail, Ordering::Release);
        self.sync_tail_to_memory(next_tail);

        Ok(SpscPopResult {
            header,
            payload_slice: slice,
            sequence_number: tail,
        })
    }

    #[inline(always)]
    pub fn ring_head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn ring_tail(&self) -> u64 {
        self.tail.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn ring_available(&self) -> u32 {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail) as u32
    }

    #[inline(always)]
    pub fn ring_remaining_capacity(&self) -> u32 {
        let cap = self.descriptor.ring_capacity;
        cap.saturating_sub(self.ring_available())
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.ring_available() == 0
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.ring_available() >= self.descriptor.ring_capacity
    }

    #[inline(always)]
    pub fn descriptor(&self) -> &SpscRingBufferDescriptor {
        &self.descriptor
    }

    #[inline(always)]
    pub fn memory_cap_bytes(&self) -> u64 {
        self.descriptor.linear_memory_cap_bytes
    }

    /// Atomic Compare-And-Swap on SPSC ring head with AcqRel / Acquire ordering
    pub fn cas_head(&self, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        self.compare_exchange_head(expected, desired, Ordering::AcqRel, Ordering::Acquire)
    }

    /// Atomic Compare-And-Swap on SPSC ring tail with AcqRel / Acquire ordering
    pub fn cas_tail(&self, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        self.compare_exchange_tail(expected, desired, Ordering::AcqRel, Ordering::Acquire)
    }

    /// Weak CAS on SPSC ring head
    pub fn cas_head_weak(&self, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        self.compare_exchange_weak_head(expected, desired, Ordering::AcqRel, Ordering::Acquire)
    }

    /// Weak CAS on SPSC ring tail
    pub fn cas_tail_weak(&self, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        self.compare_exchange_weak_tail(expected, desired, Ordering::AcqRel, Ordering::Acquire)
    }

    pub fn compare_exchange_head(
        &self,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<AtomicCasOutcome, AbiError> {
        let head_end = (self.descriptor.head_offset as usize) + 8;
        if (head_end as u64) > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: head_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        match self.head.compare_exchange(expected, desired, success, failure) {
            Ok(prev) => {
                self.sync_head_to_memory(desired);
                Ok(AtomicCasOutcome::new(true, prev, desired))
            }
            Err(actual) => Ok(AtomicCasOutcome::new(false, actual, actual)),
        }
    }

    pub fn compare_exchange_weak_head(
        &self,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<AtomicCasOutcome, AbiError> {
        let head_end = (self.descriptor.head_offset as usize) + 8;
        if (head_end as u64) > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: head_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        match self.head.compare_exchange_weak(expected, desired, success, failure) {
            Ok(prev) => {
                self.sync_head_to_memory(desired);
                Ok(AtomicCasOutcome::new(true, prev, desired))
            }
            Err(actual) => Ok(AtomicCasOutcome::new(false, actual, actual)),
        }
    }

    pub fn compare_exchange_tail(
        &self,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<AtomicCasOutcome, AbiError> {
        let tail_end = (self.descriptor.tail_offset as usize) + 8;
        if (tail_end as u64) > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: tail_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        match self.tail.compare_exchange(expected, desired, success, failure) {
            Ok(prev) => {
                self.sync_tail_to_memory(desired);
                Ok(AtomicCasOutcome::new(true, prev, desired))
            }
            Err(actual) => Ok(AtomicCasOutcome::new(false, actual, actual)),
        }
    }

    pub fn compare_exchange_weak_tail(
        &self,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<AtomicCasOutcome, AbiError> {
        let tail_end = (self.descriptor.tail_offset as usize) + 8;
        if (tail_end as u64) > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: tail_end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        match self.tail.compare_exchange_weak(expected, desired, success, failure) {
            Ok(prev) => {
                self.sync_tail_to_memory(desired);
                Ok(AtomicCasOutcome::new(true, prev, desired))
            }
            Err(actual) => Ok(AtomicCasOutcome::new(false, actual, actual)),
        }
    }

    /// Perform arbitrary 64-bit bounds-checked atomic Compare-And-Swap at an 8-byte aligned offset
    pub fn atomic_cas_64(
        &self,
        offset: usize,
        expected: u64,
        desired: u64,
    ) -> Result<AtomicCasOutcome, AbiError> {
        self.atomic_cas_64_ordered(offset, expected, desired, Ordering::AcqRel, Ordering::Acquire)
    }

    pub fn atomic_cas_64_ordered(
        &self,
        offset: usize,
        expected: u64,
        desired: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<AtomicCasOutcome, AbiError> {
        verify_alignment(offset, 8)?;
        let end = offset.checked_add(8).ok_or_else(|| AbiError::OutOfBounds {
            offset,
            len: 8,
            capacity: self.memory_len,
        })?;
        if (end as u64) > self.descriptor.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: end,
                cap_bytes: self.descriptor.linear_memory_cap_bytes as usize,
            });
        }
        if end > self.memory_len {
            return Err(AbiError::OutOfBounds {
                offset,
                len: 8,
                capacity: self.memory_len,
            });
        }
        let target_ptr = unsafe { self.base_ptr.add(offset) as *const AtomicU64 };
        verify_alignment(target_ptr as usize, 8)?;
        let atomic_ref = unsafe { &*target_ptr };
        match atomic_ref.compare_exchange(expected, desired, success, failure) {
            Ok(prev) => {
                if offset == self.descriptor.head_offset as usize {
                    self.head.store(desired, Ordering::Release);
                } else if offset == self.descriptor.tail_offset as usize {
                    self.tail.store(desired, Ordering::Release);
                }
                Ok(AtomicCasOutcome::new(true, prev, desired))
            }
            Err(actual) => Ok(AtomicCasOutcome::new(false, actual, actual)),
        }
    }
}

/// Global concurrent registry managing live SPSC ring buffers for Canonical WIT dispatch
pub struct SpscRingRegistry {
    rings: std::sync::RwLock<std::collections::HashMap<u64, Arc<SpscRingBuffer>>>,
}

impl SpscRingRegistry {
    pub fn new() -> Self {
        Self {
            rings: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn register(&self, ring: Arc<SpscRingBuffer>) {
        self.rings.write().unwrap().insert(ring.descriptor().view_id, ring);
    }

    pub fn unregister(&self, view_id: u64) -> Option<Arc<SpscRingBuffer>> {
        self.rings.write().unwrap().remove(&view_id)
    }

    pub fn get(&self, view_id: u64) -> Option<Arc<SpscRingBuffer>> {
        self.rings.read().unwrap().get(&view_id).cloned()
    }

    pub fn cas_head(&self, ring_id: u64, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        let ring = self.get(ring_id).ok_or(AbiError::ViewNotFound { view_id: ring_id })?;
        ring.cas_head(expected, desired)
    }

    pub fn cas_tail(&self, ring_id: u64, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        let ring = self.get(ring_id).ok_or(AbiError::ViewNotFound { view_id: ring_id })?;
        ring.cas_tail(expected, desired)
    }

    pub fn atomic_cas(&self, view_id: u64, offset: u64, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
        let ring = self.get(view_id).ok_or(AbiError::ViewNotFound { view_id })?;
        ring.atomic_cas_64(offset as usize, expected, desired)
    }
}

static GLOBAL_REGISTRY: std::sync::OnceLock<SpscRingRegistry> = std::sync::OnceLock::new();

pub fn global_spsc_registry() -> &'static SpscRingRegistry {
    GLOBAL_REGISTRY.get_or_init(SpscRingRegistry::new)
}

pub fn spsc_cas_head(ring_id: u64, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
    global_spsc_registry().cas_head(ring_id, expected, desired)
}

pub fn spsc_cas_tail(ring_id: u64, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
    global_spsc_registry().cas_tail(ring_id, expected, desired)
}

pub fn spsc_atomic_cas(view_id: u64, offset: u64, expected: u64, desired: u64) -> Result<AtomicCasOutcome, AbiError> {
    global_spsc_registry().atomic_cas(view_id, offset, expected, desired)
}

// ============================================================================
// STRICT CRYPTOGRAPHIC CHECKSUM & SIGNATURE VALIDATION (BLAKE3 + Ed25519)
// ============================================================================

/// Cryptographic manifest for verifying incoming WASM component/module integrity before instantiation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentIntegrityManifest {
    pub component_name: String,
    pub version: String,
    pub blake3_digest: [u8; 32],
    pub ed25519_public_key: [u8; 32],
    pub ed25519_signature: Vec<u8>,
    pub payload_byte_length: u64,
    pub fuel_budget: u64,
    pub max_memory_pages: u32,
}

impl ComponentIntegrityManifest {
    /// Sign component payload using Ed25519 signing key and compute BLAKE3 digest
    pub fn sign(
        component_name: impl Into<String>,
        version: impl Into<String>,
        payload: &[u8],
        signing_key: &SigningKey,
        fuel_budget: u64,
        max_memory_pages: u32,
    ) -> Self {
        let component_name = component_name.into();
        let version = version.into();
        let digest = *blake3::hash(payload).as_bytes();
        let verifying_key = signing_key.verifying_key();
        let pubkey_bytes = verifying_key.to_bytes();

        let sign_message = Self::canonical_envelope(&component_name, &version, &digest, fuel_budget, max_memory_pages);
        let signature = signing_key.sign(&sign_message);

        Self {
            component_name,
            version,
            blake3_digest: digest,
            ed25519_public_key: pubkey_bytes,
            ed25519_signature: signature.to_bytes().to_vec(),
            payload_byte_length: payload.len() as u64,
            fuel_budget,
            max_memory_pages,
        }
    }

    /// Generate canonical envelope message for digital signature
    pub fn canonical_envelope(
        name: &str,
        version: &str,
        digest: &[u8; 32],
        fuel_budget: u64,
        max_memory_pages: u32,
    ) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(b"HGB-COMPONENT-INTEGRITY:v1:");
        msg.extend_from_slice(name.as_bytes());
        msg.push(b':');
        msg.extend_from_slice(version.as_bytes());
        msg.push(b':');
        msg.extend_from_slice(digest);
        msg.extend_from_slice(&fuel_budget.to_le_bytes());
        msg.extend_from_slice(&max_memory_pages.to_le_bytes());
        msg
    }
}

/// Cryptographic verification outcome matching WIT `cryptographic-verification-outcome`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CryptographicVerificationOutcome {
    pub is_valid: bool,
    pub computed_blake3: [u8; 32],
    pub signature_verified: bool,
    pub error_diagnostic: Option<String>,
    pub verified_timestamp_ns: u64,
}

/// Strict Cryptographic WIT Validator enforcing BLAKE3 checksums and Ed25519 digital signatures
pub struct CryptographicWitValidator;

impl CryptographicWitValidator {
    /// Compute 32-byte BLAKE3 cryptographic hash of payload
    pub fn compute_blake3(data: &[u8]) -> [u8; 32] {
        *blake3::hash(data).as_bytes()
    }

    /// Verify Ed25519 digital signature over input message
    pub fn verify_ed25519_signature(
        message: &[u8],
        signature_bytes: &[u8],
        public_key_bytes: &[u8; 32],
    ) -> Result<bool, AbiError> {
        let verifying_key = VerifyingKey::from_bytes(public_key_bytes).map_err(|e| {
            AbiError::CryptographicValidationFailed {
                reason: format!("Invalid Ed25519 public key: {}", e),
            }
        })?;
        let sig_arr: [u8; 64] = signature_bytes.try_into().map_err(|_| {
            AbiError::CryptographicValidationFailed {
                reason: format!("Invalid Ed25519 signature length: expected 64 bytes, found {}", signature_bytes.len()),
            }
        })?;
        let signature = Signature::from_bytes(&sig_arr);
        match verifying_key.verify(message, &signature) {
            Ok(()) => Ok(true),
            Err(e) => Err(AbiError::CryptographicValidationFailed {
                reason: format!("Ed25519 signature verification failed: {}", e),
            }),
        }
    }

    /// Strictly validate cryptographic checksum and signature on incoming WASM component/module payload
    pub fn validate_component_payload(
        payload: &[u8],
        manifest: &ComponentIntegrityManifest,
    ) -> Result<CryptographicVerificationOutcome, AbiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        if payload.is_empty() {
            return Err(AbiError::CryptographicValidationFailed {
                reason: "Component payload is empty (0 bytes)".to_string(),
            });
        }

        if payload.len() as u64 != manifest.payload_byte_length {
            return Err(AbiError::CryptographicValidationFailed {
                reason: format!(
                    "Payload size mismatch: manifest specifies {} bytes, but payload has {} bytes",
                    manifest.payload_byte_length,
                    payload.len()
                ),
            });
        }

        // Validate WASM binary header magic: \0asm
        if payload.len() < 8 {
            return Err(AbiError::CryptographicValidationFailed {
                reason: "Payload truncated: minimum 8 bytes required for WASM header".to_string(),
            });
        }
        if payload[0..4] != [0x00, 0x61, 0x73, 0x6d] {
            return Err(AbiError::CryptographicValidationFailed {
                reason: format!("Invalid WASM magic header: expected [0x00, 0x61, 0x73, 0x6d], found {:?}", &payload[0..4]),
            });
        }

        // Constant-time BLAKE3 comparison
        let computed_digest = Self::compute_blake3(payload);
        if !constant_time_eq_32(&computed_digest, &manifest.blake3_digest) {
            return Err(AbiError::CryptographicValidationFailed {
                reason: format!(
                    "BLAKE3 checksum mismatch: expected {}, computed {}",
                    hex_encode(&manifest.blake3_digest),
                    hex_encode(&computed_digest)
                ),
            });
        }

        // Verify Ed25519 digital signature
        let envelope = ComponentIntegrityManifest::canonical_envelope(
            &manifest.component_name,
            &manifest.version,
            &manifest.blake3_digest,
            manifest.fuel_budget,
            manifest.max_memory_pages,
        );

        let sig_ok = Self::verify_ed25519_signature(
            &envelope,
            &manifest.ed25519_signature,
            &manifest.ed25519_public_key,
        )?;

        Ok(CryptographicVerificationOutcome {
            is_valid: sig_ok,
            computed_blake3: computed_digest,
            signature_verified: sig_ok,
            error_diagnostic: None,
            verified_timestamp_ns: now,
        })
    }
}

// ============================================================================
// WASMTIME FUEL CONSUMPTION, EPOCH INTERRUPTION & BOUNDED RUNTIME
// ============================================================================

/// Wasmtime Store user data tracking fuel limits and interruption status
#[derive(Debug, Clone)]
pub struct SecureStoreData {
    pub fuel_limit: u64,
    pub epoch_interrupted: Arc<AtomicBool>,
}

/// Hardened Wasmtime runtime integrating fuel metering, epoch interruption, and linear memory containment
#[cfg(not(target_arch = "wasm32"))]
pub struct WasmtimeSecureRuntime {
    engine: wasmtime::Engine,
}

#[cfg(not(target_arch = "wasm32"))]
impl WasmtimeSecureRuntime {
    /// Initialize Wasmtime secure engine with fuel metering and epoch interruption enabled
    pub fn new() -> Result<Self, AbiError> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config).map_err(|e| AbiError::ExecutionTrapped {
            message: format!("Failed to initialize Wasmtime engine: {}", e),
        })?;
        Ok(Self { engine })
    }

    pub fn engine(&self) -> &wasmtime::Engine {
        &self.engine
    }

    /// Instantiate a cryptographically verified WebAssembly module with strict memory capping,
    /// fuel metering, and epoch interruption preemption.
    pub fn instantiate_verified_module(
        &self,
        wasm_bytes: &[u8],
        manifest: &ComponentIntegrityManifest,
        max_allowed_pages: u32,
    ) -> Result<SecureWasmInstance, AbiError> {
        // Step 1: Strict Cryptographic Verification (BLAKE3 + Ed25519)
        let outcome = CryptographicWitValidator::validate_component_payload(wasm_bytes, manifest)?;
        if !outcome.is_valid {
            return Err(AbiError::CryptographicValidationFailed {
                reason: outcome.error_diagnostic.unwrap_or_else(|| "Signature or checksum verification failed".to_string()),
            });
        }

        // Step 2: Resource policy bounds check
        if manifest.max_memory_pages > max_allowed_pages {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: (manifest.max_memory_pages as usize) * WASM_PAGE_SIZE_BYTES,
                cap_bytes: (max_allowed_pages as usize) * WASM_PAGE_SIZE_BYTES,
            });
        }

        // Step 3: Module compilation
        let module = wasmtime::Module::new(&self.engine, wasm_bytes).map_err(|e| AbiError::ExecutionTrapped {
            message: format!("WASM module compilation failed: {:#}", e),
        })?;

        // Step 4: Setup store with fuel metering and epoch deadline callback
        let interrupted_flag = Arc::new(AtomicBool::new(false));
        let store_data = SecureStoreData {
            fuel_limit: manifest.fuel_budget,
            epoch_interrupted: interrupted_flag.clone(),
        };
        let mut store = wasmtime::Store::new(&self.engine, store_data);

        store.set_fuel(manifest.fuel_budget).map_err(|e| AbiError::ExecutionTrapped {
            message: format!("Failed to set fuel: {}", e),
        })?;

        // Epoch deadline setup: 1 tick deadline
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_store_ctx| {
            interrupted_flag.store(true, Ordering::SeqCst);
            Err(anyhow::anyhow!("Wasmtime epoch deadline reached: guest execution asynchronously preempted"))
        });

        // Step 5: Instantiation
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).map_err(|e| AbiError::ExecutionTrapped {
            message: format!("WASM module instantiation failed: {}", e),
        })?;

        let capped_memory_bytes = (manifest.max_memory_pages as usize) * WASM_PAGE_SIZE_BYTES;

        Ok(SecureWasmInstance {
            engine: self.engine.clone(),
            store,
            instance,
            linear_memory_cap_bytes: capped_memory_bytes,
        })
    }
}

/// Active secure WASM instance with bounded linear memory and metered execution
#[cfg(not(target_arch = "wasm32"))]
pub struct SecureWasmInstance {
    pub engine: wasmtime::Engine,
    pub store: wasmtime::Store<SecureStoreData>,
    pub instance: wasmtime::Instance,
    pub linear_memory_cap_bytes: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl SecureWasmInstance {
    /// Read remaining fuel
    pub fn get_fuel(&mut self) -> Result<u64, AbiError> {
        self.store.get_fuel().map_err(|e| AbiError::ExecutionTrapped {
            message: format!("get_fuel failed: {}", e),
        })
    }

    /// Set available fuel
    pub fn set_fuel(&mut self, fuel: u64) -> Result<(), AbiError> {
        self.store.set_fuel(fuel).map_err(|e| AbiError::ExecutionTrapped {
            message: format!("set_fuel failed: {}", e),
        })
    }

    /// Manually consume fuel
    pub fn consume_fuel(&mut self, units: u64) -> Result<u64, AbiError> {
        let current = self.store.get_fuel().map_err(|e| AbiError::ExecutionTrapped {
            message: format!("get_fuel failed: {}", e),
        })?;
        if current < units {
            return Err(AbiError::FuelExhausted {
                consumed: units,
                limit: self.store.data().fuel_limit,
            });
        }
        let remaining = current - units;
        self.store.set_fuel(remaining).map_err(|e| AbiError::ExecutionTrapped {
            message: format!("set_fuel failed: {}", e),
        })?;
        Ok(remaining)
    }

    /// Trigger asynchronous epoch increment to preempt runaway execution
    pub fn increment_epoch(&self) {
        self.engine.increment_epoch();
    }

    /// Linear memory cap in bytes
    pub fn memory_cap_bytes(&self) -> usize {
        self.linear_memory_cap_bytes
    }

    /// Exported linear memory handle
    pub fn get_linear_memory(&mut self) -> Result<wasmtime::Memory, AbiError> {
        self.instance.get_memory(&mut self.store, "memory").ok_or_else(|| {
            AbiError::ViewNotFound { view_id: 0 }
        })
    }

    /// Safely read bytes from linear memory strictly bounded by linear memory cap
    pub fn read_linear_memory(&mut self, offset: usize, len: usize) -> Result<Vec<u8>, AbiError> {
        let end = offset.checked_add(len).ok_or(AbiError::OutOfBounds {
            offset,
            len,
            capacity: self.linear_memory_cap_bytes,
        })?;
        if end > self.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: end,
                cap_bytes: self.linear_memory_cap_bytes,
            });
        }
        let mem = self.get_linear_memory()?;
        let data = mem.data(&self.store);
        if end > data.len() {
            return Err(AbiError::OutOfBounds {
                offset,
                len,
                capacity: data.len(),
            });
        }
        Ok(data[offset..end].to_vec())
    }

    /// Safely write bytes to linear memory strictly bounded by linear memory cap
    pub fn write_linear_memory(&mut self, offset: usize, slice: &[u8]) -> Result<(), AbiError> {
        let end = offset.checked_add(slice.len()).ok_or(AbiError::OutOfBounds {
            offset,
            len: slice.len(),
            capacity: self.linear_memory_cap_bytes,
        })?;
        if end > self.linear_memory_cap_bytes {
            return Err(AbiError::LinearMemoryCapExceeded {
                requested_bytes: end,
                cap_bytes: self.linear_memory_cap_bytes,
            });
        }
        let mem = self.get_linear_memory()?;
        let data = mem.data_mut(&mut self.store);
        if end > data.len() {
            return Err(AbiError::OutOfBounds {
                offset,
                len: slice.len(),
                capacity: data.len(),
            });
        }
        data[offset..end].copy_from_slice(slice);
        Ok(())
    }

    /// Create and map lock-free SPSC ring buffer directly over instance linear memory
    pub fn create_spsc_ring_buffer(
        &mut self,
        offset: usize,
        capacity: u32,
        element_size: u32,
        view_id: u64,
    ) -> Result<SpscRingBuffer, AbiError> {
        let cap_bytes = self.linear_memory_cap_bytes;
        let mem = self.get_linear_memory()?;
        let data = mem.data_mut(&mut self.store);
        SpscRingBuffer::from_linear_memory(
            data,
            offset,
            capacity,
            element_size,
            view_id,
            cap_bytes as u64,
        )
    }

    /// Invoke an exported function, strictly catching fuel exhaustion and epoch interruption traps
    pub fn call_func(&mut self, name: &str, params: &[wasmtime::Val]) -> Result<Vec<wasmtime::Val>, AbiError> {
        let func = self.instance.get_func(&mut self.store, name).ok_or_else(|| {
            AbiError::ExecutionTrapped {
                message: format!("Function '{}' not exported", name),
            }
        })?;

        let mut results = vec![wasmtime::Val::I32(0); func.ty(&self.store).results().len()];
        let res = func.call(&mut self.store, params, &mut results);

        match res {
            Ok(()) => Ok(results),
            Err(trap) => {
                let err_str = trap.to_string();
                let is_out_of_fuel = trap.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::OutOfFuel)
                    || self.store.get_fuel().unwrap_or(1) == 0
                    || err_str.contains("all fuel consumed")
                    || err_str.contains("fuel");

                if self.store.data().epoch_interrupted.load(Ordering::SeqCst) || err_str.contains("epoch") || err_str.contains("preempted") {
                    Err(AbiError::EpochInterruption {
                        reason: format!("Guest execution asynchronously preempted by epoch timer: {}", err_str),
                    })
                } else if is_out_of_fuel {
                    let limit = self.store.data().fuel_limit;
                    Err(AbiError::FuelExhausted {
                        consumed: limit,
                        limit,
                    })
                } else {
                    Err(AbiError::ExecutionTrapped {
                        message: err_str,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpsc_ring_buffer::{
        MpscRingBuffer, MpscRingElementHeader, FLAG_CHECKSUM_ADLER32,
    };
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_spsc_ring_buffer_push_pop_fifo() {
        let capacity = 8u32;
        let element_size = 64u32;
        let cap_bytes = 1024 * 1024u64;
        let ring = SpscRingBuffer::new(capacity, element_size, 100, cap_bytes)
            .expect("create SPSC ring buffer");

        assert!(ring.is_empty());
        assert!(!ring.is_full());
        assert_eq!(ring.ring_available(), 0);
        assert_eq!(ring.ring_remaining_capacity(), capacity);

        // Pop on empty should return RingEmpty
        assert_eq!(ring.try_pop().unwrap_err(), AbiError::RingEmpty);

        // Push 5 items
        for i in 0..5u64 {
            let payload = format!("spsc_msg_{}", i).into_bytes();
            let hdr = SpscRingElementHeader::new(0, payload.len() as u32, 0, i * 10, 0);
            let seq = ring.try_push(hdr, &payload).expect("push must succeed");
            assert_eq!(seq, i);
        }

        assert_eq!(ring.ring_available(), 5);
        assert!(!ring.is_empty());

        // Pop 5 items and verify strict FIFO ordering
        for i in 0..5u64 {
            let (hdr, payload) = ring.try_pop().expect("pop must succeed");
            assert_eq!(hdr.sequence_number, i);
            assert_eq!(hdr.timestamp_ns, i * 10);
            assert_eq!(payload, format!("spsc_msg_{}", i).into_bytes());
        }

        assert!(ring.is_empty());
        assert_eq!(ring.try_pop().unwrap_err(), AbiError::RingEmpty);
    }

    #[test]
    fn test_spsc_ring_buffer_overflow_backpressure() {
        let capacity = 4u32;
        let element_size = 32u32;
        let cap_bytes = 1024 * 1024u64;
        let ring = SpscRingBuffer::new(capacity, element_size, 101, cap_bytes)
            .expect("create SPSC ring buffer");

        // Fill ring to capacity
        for i in 0..capacity as u64 {
            let payload = i.to_le_bytes();
            let hdr = SpscRingElementHeader::new(0, payload.len() as u32, 0, i, 0);
            ring.try_push(hdr, &payload).expect("push must succeed");
        }

        assert!(ring.is_full());
        assert_eq!(ring.ring_remaining_capacity(), 0);

        // 5th push must fail with RingFull backpressure error
        let extra_payload = b"overflow";
        let extra_hdr = SpscRingElementHeader::new(0, extra_payload.len() as u32, 0, 999, 0);
        match ring.try_push(extra_hdr, extra_payload) {
            Err(AbiError::RingFull { capacity: c }) => assert_eq!(c, 4),
            other => panic!("Expected RingFull error, got {:?}", other),
        }

        // Dequeue one item
        let (hdr, data) = ring.try_pop().expect("pop must succeed");
        assert_eq!(hdr.sequence_number, 0);
        assert_eq!(data, 0u64.to_le_bytes());

        // Now push must succeed
        ring.try_push(extra_hdr, extra_payload)
            .expect("push must succeed after slot is freed");

        assert!(ring.is_full());
    }

    #[test]
    fn test_spsc_ring_buffer_payload_size_overflow() {
        let capacity = 4u32;
        let element_size = 16u32;
        let cap_bytes = 1024 * 1024u64;
        let ring = SpscRingBuffer::new(capacity, element_size, 102, cap_bytes)
            .expect("create SPSC ring buffer");

        let oversize_payload = vec![0x42u8; 32]; // > 16 bytes
        let hdr = SpscRingElementHeader::new(0, oversize_payload.len() as u32, 0, 1, 0);
        match ring.try_push(hdr, &oversize_payload) {
            Err(AbiError::BufferOverflow { needed, capacity }) => {
                assert_eq!(needed, 32);
                assert_eq!(capacity, 16);
            }
            other => panic!("Expected BufferOverflow, got {:?}", other),
        }
    }

    #[test]
    fn test_mpsc_ring_buffer_push_pop_fifo() {
        let capacity = 8u32;
        let element_size = 64u32;
        let ring_id = 200u64;
        let ring = MpscRingBuffer::new(capacity, element_size, ring_id)
            .expect("create MPSC ring buffer");

        assert!(ring.is_empty());
        assert_eq!(ring.available(), 0);
        assert_eq!(ring.try_pop().unwrap_err(), AbiError::RingEmpty);

        for i in 0..6u64 {
            let payload = format!("mpsc_msg_{}", i).into_bytes();
            let hdr = MpscRingElementHeader::new(0, 1, payload.len() as u32, FLAG_CHECKSUM_ADLER32, i * 5, 0);
            let ticket = ring.try_push(hdr, &payload).expect("mpsc push failed");
            assert_eq!(ticket, i);
        }

        assert_eq!(ring.available(), 6);
        assert!(!ring.is_empty());

        for i in 0..6u64 {
            let (hdr, payload) = ring.try_pop().expect("mpsc pop failed");
            assert_eq!(hdr.sequence_number, i);
            assert!(hdr.timestamp_ns > 0);
            assert_eq!(payload, format!("mpsc_msg_{}", i).into_bytes());
        }

        assert!(ring.is_empty());
        assert_eq!(ring.try_pop().unwrap_err(), AbiError::RingEmpty);
    }

    #[test]
    fn test_mpsc_ring_buffer_overflow_backpressure() {
        let capacity = 4u32;
        let element_size = 32u32;
        let ring_id = 201u64;
        let ring = MpscRingBuffer::new(capacity, element_size, ring_id)
            .expect("create MPSC ring buffer");

        for i in 0..capacity as u64 {
            let payload = i.to_le_bytes();
            let hdr = MpscRingElementHeader::new(0, 1, payload.len() as u32, FLAG_CHECKSUM_ADLER32, i, 0);
            ring.try_push(hdr, &payload).expect("push failed");
        }

        assert!(ring.is_full());
        assert_eq!(ring.remaining_capacity(), 0);

        let extra_payload = b"mpsc_full";
        let extra_hdr = MpscRingElementHeader::new(0, 1, extra_payload.len() as u32, FLAG_CHECKSUM_ADLER32, 99, 0);
        match ring.try_push(extra_hdr, extra_payload) {
            Err(AbiError::RingFull { capacity: c }) => assert_eq!(c, 4),
            other => panic!("Expected RingFull, got {:?}", other),
        }

        let (hdr, _) = ring.try_pop().expect("pop succeeds");
        assert_eq!(hdr.sequence_number, 0);

        ring.try_push(extra_hdr, extra_payload)
            .expect("subsequent push succeeds after pop");
    }

    #[test]
    fn test_mpsc_ring_buffer_concurrent_producers() {
        let capacity = 64u32;
        let element_size = 32u32;
        let ring = Arc::new(MpscRingBuffer::new(capacity, element_size, 202).expect("create MPSC ring"));

        let num_producers = 4;
        let items_per_producer = 50;
        let mut handles = Vec::new();

        for prod_id in 0..num_producers {
            let ring_clone = Arc::clone(&ring);
            handles.push(thread::spawn(move || {
                for i in 0..items_per_producer {
                    let payload = format!("p{}_{}", prod_id, i).into_bytes();
                    let hdr = MpscRingElementHeader::new(0, prod_id as u64, payload.len() as u32, FLAG_CHECKSUM_ADLER32, i as u64, 0);
                    loop {
                        match ring_clone.try_push(hdr, &payload) {
                            Ok(_) => break,
                            Err(AbiError::RingFull { .. }) => thread::yield_now(),
                            Err(e) => panic!("Push failed: {:?}", e),
                        }
                    }
                }
            }));
        }

        let total_items = num_producers * items_per_producer;
        let mut popped = 0;
        while popped < total_items {
            match ring.try_pop() {
                Ok((hdr, payload)) => {
                    assert!(hdr.payload_length > 0);
                    assert!(!payload.is_empty());
                    popped += 1;
                }
                Err(AbiError::RingEmpty) => thread::yield_now(),
                Err(e) => panic!("Pop failed: {:?}", e),
            }
        }

        for h in handles {
            h.join().expect("join producer");
        }

        assert_eq!(popped, total_items);
        assert!(ring.is_empty());
    }
}
