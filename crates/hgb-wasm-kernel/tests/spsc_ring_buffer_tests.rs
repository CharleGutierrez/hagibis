//! # SPSC Ring Buffer Comprehensive Unit Tests
//!
//! Verifies lock-free Single-Producer Single-Consumer (SPSC) ring buffer throughput,
//! cache-line padding (CachePadded), atomic acquire/release index tracking,
//! zero-copy slicing, Adler-32 checksum integrity, and linear memory capping.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use hgb_wasm_kernel::canonical_abi_shm_views::{
    calculate_adler32, AbiError, CachePadded, CanonicalMemorySlice, MemoryAccessMode,
    SpscPopResult, SpscRingBuffer, SpscRingBufferDescriptor, SpscRingElementHeader,
    CANONICAL_ALIGNMENT_DEFAULT, SPSC_RING_MAGIC,
};

#[test]
fn test_spsc_ring_buffer_single_thread_throughput() {
    let capacity = 1024u32;
    let element_size = 64u32;
    let cap_bytes = 1024 * 1024u64; // 1 MiB cap
    let ring = SpscRingBuffer::new(capacity, element_size, 1, cap_bytes).expect("create SPSC ring");

    assert_eq!(ring.ring_head(), 0);
    assert_eq!(ring.ring_tail(), 0);
    assert_eq!(ring.ring_available(), 0);
    assert_eq!(ring.ring_remaining_capacity(), capacity);
    assert!(ring.is_empty());
    assert!(!ring.is_full());

    let iterations = 20_000u64;
    let payload_template = b"HAGIBIS_SPSC_ZERO_COPY_PAYLOAD_TEST_V1_2026";
    let start = Instant::now();

    for i in 0..iterations {
        let hdr = SpscRingElementHeader::new(0, payload_template.len() as u32, 0, i, 0);
        let seq = ring.try_push(hdr, payload_template).expect("push");
        assert_eq!(seq, i);

        let (pop_hdr, pop_data) = ring.try_pop().expect("pop");
        assert_eq!(pop_hdr.sequence_number, i);
        assert_eq!(pop_hdr.timestamp_ns, i);
        assert_eq!(&pop_data, payload_template);
    }

    let elapsed = start.elapsed();
    let ops_per_sec = (iterations as f64) / elapsed.as_secs_f64();
    println!("SPSC single-thread throughput: {:.2} ops/sec in {:?}", ops_per_sec, elapsed);
    assert!(ops_per_sec > 100_000.0, "Throughput must exceed 100k ops/sec");
    assert!(ring.is_empty());
}

#[test]
fn test_spsc_ring_buffer_multithreaded_producer_consumer() {
    let capacity = 128u32;
    let element_size = 32u32;
    let cap_bytes = 2 * 1024 * 1024u64;
    let ring = Arc::new(SpscRingBuffer::new(capacity, element_size, 42, cap_bytes).expect("create SPSC ring"));

    let iterations = 10_000u64;

    // Producer Thread
    let prod_ring = Arc::clone(&ring);
    let prod_handle = thread::spawn(move || {
        for i in 0..iterations {
            let data = i.to_le_bytes();
            let hdr = SpscRingElementHeader::new(0, data.len() as u32, 0, i, 0);
            loop {
                match prod_ring.try_push(hdr, &data) {
                    Ok(_) => break,
                    Err(AbiError::RingFull { .. }) => thread::yield_now(),
                    Err(e) => panic!("Producer error: {:?}", e),
                }
            }
        }
    });

    // Consumer Thread
    let cons_ring = Arc::clone(&ring);
    let cons_handle = thread::spawn(move || {
        let mut received = Vec::with_capacity(iterations as usize);
        while (received.len() as u64) < iterations {
            match cons_ring.try_pop() {
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

    prod_handle.join().expect("producer join");
    let received = cons_handle.join().expect("consumer join");

    assert_eq!(received.len(), iterations as usize);
    for i in 0..iterations {
        assert_eq!(received[i as usize], i);
    }
    assert!(ring.is_empty());
}

#[test]
fn test_spsc_ring_buffer_zero_copy_pop() {
    let capacity = 16u32;
    let element_size = 64u32;
    let cap_bytes = 512 * 1024u64;
    let ring = SpscRingBuffer::new(capacity, element_size, 7, cap_bytes).expect("ring");

    let payload = b"ZERO_COPY_IN_LINEAR_MEMORY_WITHOUT_ALLOCATIONS";
    let hdr = SpscRingElementHeader::new(0, payload.len() as u32, 1, 9999, 0);

    let seq = ring.try_push(hdr, payload).expect("push");
    assert_eq!(seq, 0);

    let pop_res = ring.pop_zero_copy().expect("pop zero copy");
    assert_eq!(pop_res.sequence_number, 0);
    assert_eq!(pop_res.header.timestamp_ns, 9999);
    assert_eq!(pop_res.payload_slice.len, payload.len() as u64);
    assert_eq!(pop_res.payload_slice.access_mode, MemoryAccessMode::ReadOnly);

    // Read directly from the linear memory pointer
    let slice_bytes = unsafe {
        std::slice::from_raw_parts(pop_res.payload_slice.ptr as *const u8, pop_res.payload_slice.len as usize)
    };
    assert_eq!(slice_bytes, payload);
    assert!(ring.is_empty());
}

#[repr(align(64))]
struct AlignedBuffer<const N: usize>([u8; N]);

#[test]
fn test_spsc_ring_buffer_checksum_corruption_detection() {
    let capacity = 8u32;
    let element_size = 32u32;
    let cap_bytes = 256 * 1024u64;
    let mut mem = Box::new(AlignedBuffer([0u8; 16384]));

    let ring = SpscRingBuffer::from_linear_memory(&mut mem.0, 64, capacity, element_size, 10, cap_bytes)
        .expect("from linear memory");

    let payload = b"INTEGRITY_CHECK_ORIGINAL";
    let hdr = SpscRingElementHeader::new(0, payload.len() as u32, 0, 12345, 0);
    ring.try_push(hdr, payload).expect("push");

    // Slot offset: buffer_offset = 256, slot 0 = 256
    // Payload starts after SpscRingElementHeader::SIZE (32 bytes)
    let payload_in_memory_offset = 64 + 256 + 32;
    // Tamper with one byte in memory
    mem.0[payload_in_memory_offset] ^= 0xFF;

    let pop_result = ring.try_pop();
    assert!(matches!(pop_result, Err(AbiError::CryptographicValidationFailed { .. })));
}

#[test]
fn test_spsc_ring_buffer_linear_memory_capping_enforcement() {
    let capacity = 32u32;
    let element_size = 64u32;
    let desc = SpscRingBufferDescriptor::new(1, capacity, element_size, 100_000).expect("desc");
    let required_size = desc.required_buffer_size();

    // 1. Attempt to create with linear memory cap smaller than required buffer size
    let cap_too_small = (required_size - 1) as u64;
    let err = SpscRingBuffer::new(capacity, element_size, 1, cap_too_small);
    assert!(matches!(err, Err(AbiError::LinearMemoryCapExceeded { .. })));

    // 2. Map into linear memory slice that is larger, but bounded by tight cap
    let mut big_mem = Box::new(AlignedBuffer([0u8; 65536]));
    let tight_cap = (required_size + 64) as u64;
    let ring = SpscRingBuffer::from_linear_memory(&mut big_mem.0, 64, capacity, element_size, 1, tight_cap)
        .expect("map with tight cap");

    assert_eq!(ring.memory_cap_bytes(), tight_cap);
}

#[test]
fn test_spsc_ring_buffer_cache_padded_alignment() {
    assert_eq!(std::mem::align_of::<CachePadded<AtomicU64>>(), 64);
    assert_eq!(std::mem::size_of::<CachePadded<AtomicU64>>(), 64);
    assert_eq!(std::mem::align_of::<CachePadded<AtomicU32>>(), 64);
    assert_eq!(std::mem::size_of::<CachePadded<AtomicU32>>(), 64);

    let padded = CachePadded::new(AtomicU64::new(42));
    assert_eq!(padded.load(Ordering::SeqCst), 42);
}

#[test]
fn test_spsc_ring_buffer_power_of_two_and_backpressure() {
    for &invalid_cap in &[0u32, 3, 5, 6, 7, 9, 15, 100, 1000] {
        let err = SpscRingBuffer::new(invalid_cap, 32, 1, 1024 * 1024);
        assert!(matches!(err, Err(AbiError::InvalidCapacity { .. })));
    }

    let ring = SpscRingBuffer::new(4, 16, 1, 65536).expect("ring cap 4");
    assert_eq!(ring.ring_remaining_capacity(), 4);

    for i in 0..4 {
        let hdr = SpscRingElementHeader::new(0, 4, 0, i, 0);
        ring.try_push(hdr, &[1, 2, 3, 4]).expect("push 4");
    }

    assert!(ring.is_full());
    assert_eq!(ring.ring_available(), 4);
    assert_eq!(ring.ring_remaining_capacity(), 0);

    // 5th push must fail with RingFull
    let overflow_hdr = SpscRingElementHeader::new(0, 4, 0, 99, 0);
    let overflow_err = ring.try_push(overflow_hdr, &[5, 6, 7, 8]);
    assert!(matches!(overflow_err, Err(AbiError::RingFull { capacity: 4 })));

    // Pop 1 element
    let _ = ring.try_pop().expect("pop 1");
    assert!(!ring.is_full());
    assert_eq!(ring.ring_remaining_capacity(), 1);

    // Push 1 element now succeeds
    ring.try_push(overflow_hdr, &[5, 6, 7, 8]).expect("push into freed slot");
}

#[test]
fn test_spsc_atomic_cas_head_and_tail_correctness() {
    let capacity = 16u32;
    let element_size = 32u32;
    let cap_bytes = 1024 * 1024u64;
    let ring = SpscRingBuffer::new(capacity, element_size, 100, cap_bytes).expect("create SPSC ring");

    // Head starts at 0
    assert_eq!(ring.ring_head(), 0);

    // Contested CAS on head: expect 5, but it is 0 -> should fail with success=false, prev=0
    let failure = ring.cas_head(5, 10).expect("cas head call");
    assert!(!failure.is_success());
    assert_eq!(failure.previous_value, 0);
    assert_eq!(failure.current_value, 0);
    assert_eq!(ring.ring_head(), 0);

    // Successful CAS on head: expect 0, desired 1 -> should succeed
    let success = ring.cas_head(0, 1).expect("cas head call");
    assert!(success.is_success());
    assert_eq!(success.previous_value, 0);
    assert_eq!(success.current_value, 1);
    assert_eq!(ring.ring_head(), 1);

    // Tail CAS
    assert_eq!(ring.ring_tail(), 0);
    let tail_success = ring.cas_tail(0, 1).expect("cas tail call");
    assert!(tail_success.is_success());
    assert_eq!(tail_success.previous_value, 0);
    assert_eq!(tail_success.current_value, 1);
    assert_eq!(ring.ring_tail(), 1);
}

#[test]
fn test_spsc_atomic_cas_64_arbitrary_bounds_and_alignment() {
    let capacity = 16u32;
    let element_size = 64u32;
    let cap_bytes = 1024 * 1024u64;
    let ring = SpscRingBuffer::new(capacity, element_size, 101, cap_bytes).expect("create SPSC ring");

    // Unaligned offset must be rejected
    let unaligned = ring.atomic_cas_64(3, 0, 42);
    assert!(matches!(unaligned, Err(AbiError::AlignmentFault { .. })));

    // Aligned offset (e.g. 64)
    let cas1 = ring.atomic_cas_64(64, 0, 12345).expect("cas aligned");
    assert!(cas1.is_success());
    assert_eq!(cas1.current_value, 12345);

    // CAS with contested value
    let cas2 = ring.atomic_cas_64(64, 0, 99999).expect("cas contested");
    assert!(!cas2.is_success());
    assert_eq!(cas2.previous_value, 12345);
}

#[test]
fn test_spsc_atomic_cas_high_concurrency_contention() {
    let capacity = 64u32;
    let element_size = 64u32;
    let cap_bytes = 1024 * 1024u64;
    let ring = Arc::new(SpscRingBuffer::new(capacity, element_size, 102, cap_bytes).expect("create SPSC ring"));

    let num_threads = 8;
    let increments_per_thread = 500u64;
    let target_offset = 64usize; // aligned offset

    let mut handles = vec![];
    for _ in 0..num_threads {
        let r = Arc::clone(&ring);
        handles.push(thread::spawn(move || {
            for _ in 0..increments_per_thread {
                loop {
                    // Try atomic CAS loop until success
                    let current = match r.atomic_cas_64(target_offset, 0, 0) {
                        Ok(res) => res.previous_value,
                        Err(e) => panic!("atomic cas error: {:?}", e),
                    };
                    let outcome = r.atomic_cas_64(target_offset, current, current + 1).expect("cas increment");
                    if outcome.is_success() {
                        break;
                    }
                    thread::yield_now();
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join");
    }

    // Verify exact total count
    let final_res = ring.atomic_cas_64(target_offset, 0, 0).expect("final read");
    assert_eq!(final_res.previous_value, (num_threads as u64) * increments_per_thread);
}

#[test]
fn test_spsc_registry_wit_functions() {
    use hgb_wasm_kernel::canonical_abi_shm_views::{
        global_spsc_registry, spsc_atomic_cas, spsc_cas_head, spsc_cas_tail,
    };

    let ring = Arc::new(SpscRingBuffer::new(32, 64, 999, 1024 * 1024).expect("create ring"));
    global_spsc_registry().register(ring);

    // Call WIT global dispatch functions
    let head_res = spsc_cas_head(999, 0, 5).expect("spsc_cas_head");
    assert!(head_res.is_success());
    assert_eq!(head_res.current_value, 5);

    let tail_res = spsc_cas_tail(999, 0, 3).expect("spsc_cas_tail");
    assert!(tail_res.is_success());
    assert_eq!(tail_res.current_value, 3);

    let atomic_res = spsc_atomic_cas(999, 256, 0, 777).expect("spsc_atomic_cas");
    assert!(atomic_res.is_success());
    assert_eq!(atomic_res.current_value, 777);

    // Unknown ring id returns ViewNotFound
    let not_found = spsc_cas_head(88888, 0, 1);
    assert!(matches!(not_found, Err(AbiError::ViewNotFound { .. })));
}
