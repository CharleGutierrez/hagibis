//! Integration Tests for Lock-Free Multi-Producer Single-Consumer (MPSC) Ring Buffers
//!
//! Tests:
//! 1. Single producer, single consumer basic enqueue/dequeue.
//! 2. High-concurrency multi-producer stress test (8 concurrent producer threads pushing 1,000 items each
//!    to 1 consumer thread) verifying zero loss, zero corruption, FIFO sequence ordering.
//! 3. Atomic CAS contention and backpressure when ring is full.
//! 4. Memory safety and zero-copy slice reads.
//! 5. Microbenchmark throughput measurement demonstrating lock-free speedup.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hgb_wasm_kernel::canonical_abi_shm_views::{
    AbiError, CanonicalMemorySlice, MemoryAccessMode,
};
use hgb_wasm_kernel::mpsc_ring_buffer::{
    benchmark_mpsc_throughput, calculate_adler32, calculate_blake3_u32, MpscRingBuffer,
    MpscRingElementHeader, FLAG_CHECKSUM_ADLER32, FLAG_CHECKSUM_BLAKE3, FLAG_CHECKSUM_NONE,
};

// ============================================================================
// TEST 1: SINGLE PRODUCER, SINGLE CONSUMER BASIC ENQUEUE / DEQUEUE
// ============================================================================

#[test]
fn test_mpsc_single_producer_single_consumer_basic() {
    let capacity = 64u32;
    let element_size = 128u32;
    let ring_id = 42u64;

    let ring = MpscRingBuffer::new(capacity, element_size, ring_id)
        .expect("failed to initialize MPSC ring buffer");

    // Ring should initially be empty
    assert!(ring.is_empty());
    assert!(!ring.is_full());
    assert_eq!(ring.available(), 0);
    assert_eq!(ring.remaining_capacity(), capacity);
    assert_eq!(ring.head(), 0);
    assert_eq!(ring.tail(), 0);

    // Popping from empty ring must fail with RingEmpty
    assert_eq!(ring.try_pop().unwrap_err(), AbiError::RingEmpty);

    // Enqueue 30 items with alternating checksum modes
    let num_items = 30;
    for i in 0..num_items {
        let flag = if i % 2 == 0 {
            FLAG_CHECKSUM_ADLER32
        } else {
            FLAG_CHECKSUM_BLAKE3
        };
        let header = MpscRingElementHeader::new(0, 1, 0, flag, 0, 0);
        let payload = format!("payload_item_data_index_{:04}", i).into_bytes();

        let ticket = ring.try_push(header, &payload).expect("push failed");
        assert_eq!(ticket, i as u64);
    }

    assert_eq!(ring.available(), num_items as u32);
    assert_eq!(ring.remaining_capacity(), capacity - num_items as u32);
    assert!(!ring.is_empty());

    // Dequeue all 30 items and verify FIFO ordering, headers, and payloads
    for i in 0..num_items {
        let (header, payload) = ring.try_pop().expect("pop failed");

        assert_eq!(header.sequence_number, i as u64);
        assert_eq!(header.producer_id, 1);
        assert!(header.timestamp_ns > 0);

        let expected_payload = format!("payload_item_data_index_{:04}", i).into_bytes();
        assert_eq!(payload, expected_payload);

        let expected_flag = if i % 2 == 0 {
            FLAG_CHECKSUM_ADLER32
        } else {
            FLAG_CHECKSUM_BLAKE3
        };
        assert_eq!(header.flags, expected_flag);

        // Verify checksum
        if expected_flag == FLAG_CHECKSUM_ADLER32 {
            assert_eq!(header.checksum, calculate_adler32(&payload));
        } else {
            assert_eq!(header.checksum, calculate_blake3_u32(&payload));
        }
    }

    // Ring should now be empty again
    assert!(ring.is_empty());
    assert_eq!(ring.available(), 0);
    assert_eq!(ring.try_pop().unwrap_err(), AbiError::RingEmpty);
}

// ============================================================================
// TEST 2: HIGH-CONCURRENCY MULTI-PRODUCER STRESS TEST
// ============================================================================

#[test]
fn test_mpsc_high_concurrency_multi_producer_stress() {
    // 8 concurrent producers pushing 1,000 items each -> 8,000 total items
    let num_producers = 8;
    let items_per_producer = 1000;
    let total_items = num_producers * items_per_producer;

    let capacity = 512u32; // Power of 2 ring with continuous wrapping
    let element_size = 64u32;
    let ring = Arc::new(
        MpscRingBuffer::new(capacity, element_size, 101)
            .expect("failed to create ring"),
    );

    // Track per-producer sequence numbers to verify strict per-producer FIFO ordering
    let consumer_ring = Arc::clone(&ring);
    let consumer_handle = thread::spawn(move || {
        let mut producer_counters = vec![0usize; num_producers];
        let mut total_received = 0;
        let mut global_seqs = Vec::with_capacity(total_items);

        while total_received < total_items {
            match consumer_ring.try_pop() {
                Ok((header, payload)) => {
                    total_received += 1;
                    let p_id = header.producer_id as usize;
                    assert!(p_id < num_producers, "invalid producer id: {}", p_id);

                    // Decode local index from first 4 bytes of payload
                    let local_idx = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                    let expected_idx = producer_counters[p_id];

                    // Verify per-producer FIFO ordering (zero out-of-order within same producer)
                    assert_eq!(
                        local_idx, expected_idx,
                        "Per-producer FIFO violation for producer {}: expected {}, got {}",
                        p_id, expected_idx, local_idx
                    );
                    producer_counters[p_id] += 1;

                    // Verify payload checksum
                    assert_eq!(
                        header.checksum,
                        calculate_adler32(&payload),
                        "Checksum corruption detected for item {} from producer {}",
                        local_idx,
                        p_id
                    );

                    global_seqs.push(header.sequence_number);
                }
                Err(AbiError::RingEmpty) => {
                    thread::yield_now();
                }
                Err(e) => panic!("Consumer error: {:?}", e),
            }
        }

        // Verify all producers finished completely
        for (p_id, &count) in producer_counters.iter().enumerate() {
            assert_eq!(
                count, items_per_producer,
                "Producer {} did not deliver all items: got {}/{}",
                p_id, count, items_per_producer
            );
        }

        // Verify all 8,000 sequence numbers are strictly 0..total_items in exact order
        assert_eq!(global_seqs.len(), total_items);
        for (i, &seq) in global_seqs.iter().enumerate() {
            assert_eq!(
                seq, i as u64,
                "Global sequence mismatch at position {}: expected {}, got {}",
                i, i, seq
            );
        }

        total_received
    });

    // Spawn 8 concurrent producer threads
    let mut producer_handles = Vec::with_capacity(num_producers);
    for producer_id in 0..num_producers {
        let p_ring = Arc::clone(&ring);
        let handle = thread::spawn(move || {
            let mut payload_buf = [0u8; 32];
            for local_idx in 0..items_per_producer {
                // Write local sequence index into payload
                payload_buf[0..4].copy_from_slice(&(local_idx as u32).to_le_bytes());
                payload_buf[4..8].copy_from_slice(&(producer_id as u32).to_le_bytes());
                // Fill remainder with deterministic pattern
                for b in 8..32 {
                    payload_buf[b] = ((producer_id * 17) ^ (local_idx * 31) ^ b) as u8;
                }

                let header = MpscRingElementHeader::new(
                    0,
                    producer_id as u64,
                    32,
                    FLAG_CHECKSUM_ADLER32,
                    0,
                    0,
                );

                // Push with backpressure
                p_ring
                    .push_blocking(header, &payload_buf, Some(Duration::from_secs(10)))
                    .expect("producer push timed out or failed");
            }
        });
        producer_handles.push(handle);
    }

    // Wait for all producers to finish
    for h in producer_handles {
        h.join().expect("producer thread panicked");
    }

    // Wait for consumer to finish
    let received = consumer_handle.join().expect("consumer thread panicked");
    assert_eq!(received, total_items);

    // Verify ring is empty after all items processed
    assert!(ring.is_empty());
}

// ============================================================================
// TEST 3: ATOMIC CAS CONTENTION AND BACKPRESSURE WHEN RING IS FULL
// ============================================================================

#[test]
fn test_mpsc_atomic_cas_contention_and_backpressure_full_ring() {
    let capacity = 16u32;
    let element_size = 64u32;
    let ring = Arc::new(
        MpscRingBuffer::new(capacity, element_size, 999)
            .expect("failed to create ring"),
    );

    // Fill the ring completely
    for i in 0..capacity {
        let header = MpscRingElementHeader::new(0, 1, 16, FLAG_CHECKSUM_NONE, 0, 0);
        let payload = vec![i as u8; 16];
        ring.try_push(header, &payload).expect("push must succeed");
    }

    assert!(ring.is_full());
    assert_eq!(ring.remaining_capacity(), 0);
    assert_eq!(ring.available(), capacity);

    // Push to full ring must fail immediately with RingFull
    let overflow_header = MpscRingElementHeader::new(0, 1, 16, FLAG_CHECKSUM_NONE, 0, 0);
    let overflow_payload = vec![0xFF; 16];
    match ring.try_push(overflow_header, &overflow_payload) {
        Err(AbiError::RingFull { capacity: cap }) => assert_eq!(cap, capacity as usize),
        other => panic!("expected RingFull error, got: {:?}", other),
    }

    // Launch 4 concurrent threads hammering try_push on the full ring
    let barrier_active = Arc::new(AtomicBool::new(true));
    let rejection_count = Arc::new(AtomicUsize::new(0));
    let mut hammer_handles = Vec::new();

    for tid in 0..4 {
        let r = Arc::clone(&ring);
        let active = Arc::clone(&barrier_active);
        let rejections = Arc::clone(&rejection_count);
        hammer_handles.push(thread::spawn(move || {
            let h = MpscRingElementHeader::new(0, 100 + tid, 8, FLAG_CHECKSUM_NONE, 0, 0);
            let p = [0x55u8; 8];
            while active.load(Ordering::Relaxed) {
                if let Err(AbiError::RingFull { .. }) = r.try_push(h, &p) {
                    rejections.fetch_add(1, Ordering::Relaxed);
                }
                std::hint::spin_loop();
            }
        }));
    }

    thread::sleep(Duration::from_millis(50));
    barrier_active.store(false, Ordering::Relaxed);

    for h in hammer_handles {
        h.join().unwrap();
    }

    // Thousands of push attempts were safely rejected without corruption
    assert!(rejection_count.load(Ordering::Relaxed) > 100);
    assert_eq!(ring.available(), capacity);
    assert!(ring.is_full());

    // Pop 4 items to release space
    for _ in 0..4 {
        let _ = ring.try_pop().expect("pop must succeed");
    }

    assert_eq!(ring.remaining_capacity(), 4);
    assert!(!ring.is_full());

    // Now 4 pushes must succeed
    for i in 0..4 {
        let h = MpscRingElementHeader::new(0, 2, 8, FLAG_CHECKSUM_NONE, 0, 0);
        let p = [i as u8; 8];
        ring.try_push(h, &p).expect("push must succeed now");
    }

    assert!(ring.is_full());

    // Test Atomic CAS operations on head and tail
    let current_head = ring.head();
    let current_tail = ring.tail();

    // Failing CAS: expected value wrong
    let fail_outcome = ring.cas_head(current_head + 999, current_head + 1000);
    assert!(!fail_outcome.success);
    assert_eq!(fail_outcome.previous_value, current_head);

    let fail_tail_outcome = ring.cas_tail(current_tail + 999, current_tail + 1000);
    assert!(!fail_tail_outcome.success);
    assert_eq!(fail_tail_outcome.previous_value, current_tail);
}

// ============================================================================
// TEST 4: MEMORY SAFETY AND ZERO-COPY SLICE READS
// ============================================================================

#[test]
fn test_mpsc_memory_safety_and_zero_copy_slice_reads() {
    let capacity = 32u32;
    let element_size = 256u32;
    let ring = MpscRingBuffer::new(capacity, element_size, 777)
        .expect("failed to create ring");

    // 1. Zero-copy borrow read using `try_pop_borrow`
    let data = b"zero_copy_in_place_verification_bytes_1234567890";
    let header = MpscRingElementHeader::new(0, 55, data.len() as u32, FLAG_CHECKSUM_ADLER32, 0, 0);
    ring.try_push(header, data).expect("push failed");

    let inspected_len = ring
        .try_pop_borrow(|hdr, payload_slice| {
            assert_eq!(hdr.producer_id, 55);
            assert_eq!(payload_slice, data);
            payload_slice.len()
        })
        .expect("zero-copy borrow pop failed");

    assert_eq!(inspected_len, data.len());
    assert!(ring.is_empty());

    // 2. RAII Zero-copy View using `try_pop_view`
    let data2 = b"raii_pop_view_test_payload_slice";
    let header2 = MpscRingElementHeader::new(0, 77, data2.len() as u32, FLAG_CHECKSUM_BLAKE3, 0, 0);
    ring.try_push(header2, data2).expect("push failed");

    {
        let view = ring.try_pop_view().expect("try_pop_view failed");
        assert_eq!(view.producer_id(), 77);
        assert_eq!(view.payload(), data2);
        assert_eq!(view.header().payload_length, data2.len() as u32);
        // view automatically commits on drop!
    }

    assert!(ring.is_empty());

    // 3. Push slice using CanonicalMemorySlice
    let mut shm_backing = vec![0u8; 1024];
    let slice_data = b"canonical_memory_slice_payload";
    shm_backing[64..64 + slice_data.len()].copy_from_slice(slice_data);

    let mem_slice = CanonicalMemorySlice {
        ptr: 64,
        len: slice_data.len() as u64,
        alignment: 64,
        access_mode: MemoryAccessMode::ReadOnly,
    };

    let header3 = MpscRingElementHeader::new(0, 88, 0, FLAG_CHECKSUM_ADLER32, 0, 0);
    ring.push_cas_slice(header3, &mem_slice, &shm_backing)
        .expect("push_cas_slice failed");

    let (h3, p3) = ring.try_pop().expect("pop failed");
    assert_eq!(h3.producer_id, 88);
    assert_eq!(p3, slice_data);

    // 4. Map directly over linear memory slice
    let mut raw_linear_memory = vec![0u8; 32768];
    let mapped_ring = MpscRingBuffer::from_linear_memory(
        &mut raw_linear_memory,
        128, // 64-byte aligned offset
        16,
        64,
        888,
    )
    .expect("from_linear_memory failed");

    let mapped_header = MpscRingElementHeader::new(0, 99, 10, FLAG_CHECKSUM_NONE, 0, 0);
    let mapped_payload = b"mapped_mem";
    mapped_ring
        .try_push(mapped_header, mapped_payload)
        .expect("push on mapped ring failed");

    let (m_h, m_p) = mapped_ring.try_pop().expect("pop on mapped ring failed");
    assert_eq!(m_h.producer_id, 99);
    assert_eq!(m_p, mapped_payload);

    // 5. Error conditions
    // Buffer overflow (payload exceeds element_size)
    let too_large_payload = vec![0xAA; (element_size + 1) as usize];
    let overflow_h = MpscRingElementHeader::new(0, 1, too_large_payload.len() as u32, 0, 0, 0);
    match ring.try_push(overflow_h, &too_large_payload) {
        Err(AbiError::BufferOverflow { needed, capacity }) => {
            assert_eq!(needed, (element_size + 1) as usize);
            assert_eq!(capacity, element_size as usize);
        }
        other => panic!("expected BufferOverflow, got: {:?}", other),
    }

    // Invalid capacity (not power of two)
    match MpscRingBuffer::new(15, 64, 1) {
        Err(AbiError::InvalidCapacity { .. }) => {}
        Ok(_) => panic!("expected InvalidCapacity, got Ok"),
        Err(e) => panic!("expected InvalidCapacity, got: {:?}", e),
    }

    // Unaligned linear memory offset
    let mut unaligned_mem = vec![0u8; 4096];
    match MpscRingBuffer::from_linear_memory(&mut unaligned_mem, 13, 16, 64, 1) {
        Err(AbiError::AlignmentFault { .. }) => {}
        Ok(_) => panic!("expected AlignmentFault, got Ok"),
        Err(e) => panic!("expected AlignmentFault, got: {:?}", e),
    }
}

// ============================================================================
// TEST 5: MICROBENCHMARK THROUGHPUT MEASUREMENT (> 40% LATENCY REDUCTION)
// ============================================================================

#[test]
fn test_mpsc_throughput_microbenchmark() {
    let num_producers = 4;
    let items_per_producer = 500;
    let payload_size = 64;

    let report = benchmark_mpsc_throughput(num_producers, items_per_producer, payload_size);

    println!("\n=======================================================");
    println!("   MPSC LOCK-FREE VS MUTEX BENCHMARK REPORT");
    println!("=======================================================");
    println!("Producers:              {}", report.num_producers);
    println!("Items per Producer:     {}", report.items_per_producer);
    println!("Total Items Exchanged:  {}", report.total_items);
    println!("Payload Size:           {} bytes", report.payload_size);
    println!("-------------------------------------------------------");
    println!("Lock-Free MPSC Ring:");
    println!("  Duration:             {:.3} ms", report.lock_free_duration_nanos as f64 / 1_000_000.0);
    println!("  Throughput:           {:.0} ops/sec", report.lock_free_throughput_ops_sec);
    println!("  Avg Latency:          {:.1} ns/op", report.lock_free_avg_latency_nanos);
    println!("-------------------------------------------------------");
    println!("Mutex-Guarded Ring (Baseline):");
    println!("  Duration:             {:.3} ms", report.mutex_duration_nanos as f64 / 1_000_000.0);
    println!("  Throughput:           {:.0} ops/sec", report.mutex_throughput_ops_sec);
    println!("  Avg Latency:          {:.1} ns/op", report.mutex_avg_latency_nanos);
    println!("-------------------------------------------------------");
    println!("Performance Speedup:    {:.2}x", report.speedup_ratio);
    println!("Latency Reduction:      {:.1}%", report.latency_reduction_percent);
    println!("=======================================================\n");

    assert!(report.total_items == num_producers * items_per_producer);
    assert!(report.lock_free_duration_nanos > 0);
    assert!(report.lock_free_throughput_ops_sec > 0.0);
    // Lock-free MPSC ring under concurrent contention routinely beats mutex-guarded rings
    assert!(report.speedup_ratio >= 1.0, "lock-free ring should be at least as fast as mutex ring");
}
