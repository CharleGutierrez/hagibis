//! Comprehensive Unit Tests for Pillar 3: MicroVM Snapshot Hot-Pool & UFFD Fast-Path Caching
//!
//! Tests:
//! 1. Snapshot pool acquisition and replenishment.
//! 2. UFFD lazy-population descriptor mechanics and fault handling.
//! 3. Latency measurement validating sub-10ms fast-path dispatch.
//! 4. Safe zero-failure fallback when virtualization capabilities are restricted.
//! 5. Multi-threaded concurrent acquisition & release stress testing.
//! 6. CoW memory mapping and `madvise(MADV_DONTNEED)` dirty page sanitization.
//! 7. Ephemeral sandbox fast-path integration.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use hgb_core::microvm_sandbox_executor::{
    EphemeralExecSpec, FirecrackerEphemeralSandbox, FirecrackerSpawnConfig,
};
use hgb_core::microvm_snapshot_cache::{
    BaseSnapshotTemplate, CowMemoryRegion, MemoryBackendType, MicrovmSnapshotPool,
    MicrovmSnapshotPoolConfig, MicrovmSnapshotPoolMetrics, PooledMicrovm, PooledMicrovmState,
    SnapshotResumeLatencyReport, UffdCapability, UffdMemoryHandler, UffdPageFaultStats,
    UffdProbe, UffdStatsSnapshot, VirtualizationProbe, PAGE_SIZE, UFFDIO_COPY, UFFDIO_REGISTER,
    UFFDIO_REGISTER_MODE_MISSING, UFFDIO_ZEROPAGE, UFFD_EVENT_PAGEFAULT,
};

// ==============================================================================
// 1. Snapshot Pool Acquisition and Replenishment Tests
// ==============================================================================

#[test]
fn test_snapshot_pool_acquisition_and_replenishment() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("hgb_pool_cache");

    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 4,
        max_pool_size: 8,
        min_idle_size: 3,
        max_reuse_count: 10,
        vcpu_count: 2,
        mem_size_mib: 128,
        target_resume_latency_ms: 10,
        enable_uffd: false, // Fallback mode for deterministic unit test environment
        kernel_path: None,
        rootfs_path: None,
        cache_dir: Some(cache_dir),
    };

    let pool = MicrovmSnapshotPool::with_config(temp_dir.path(), config).expect("initialize pool");

    // Initial state: 4 pre-warmed idle instances
    assert_eq!(pool.idle_count(), 4);
    assert_eq!(pool.active_count(), 0);

    // 1. Acquire instance 1
    let vm1 = pool.acquire().expect("acquire instance 1");
    assert_eq!(vm1.state, PooledMicrovmState::InUse);
    assert!(vm1.jail_dir.exists());
    assert_eq!(pool.idle_count(), 3);

    // 2. Acquire instance 2
    let vm2 = pool.acquire().expect("acquire instance 2");
    assert_eq!(pool.idle_count(), 2);

    // 3. Replenish: idle count (2) is below min_idle_size (3), so replenish adds 1
    let replenished = pool.replenish().expect("replenish");
    assert_eq!(replenished, 1);
    assert_eq!(pool.idle_count(), 3);

    // 4. Release vm1: resets dirty pages, increments reuse_count, returns to pool
    let vm1_id = vm1.instance_id.clone();
    pool.release(vm1).expect("release vm1");
    assert_eq!(pool.idle_count(), 4);

    // 5. Release vm2
    pool.release(vm2).expect("release vm2");
    assert_eq!(pool.idle_count(), 5);

    // 6. Evict specific instance
    pool.evict(&vm1_id).expect("evict vm1");
    assert_eq!(pool.idle_count(), 4);

    // Metrics check
    let metrics = pool.metrics();
    assert_eq!(metrics.total_acquired, 2);
    assert_eq!(metrics.total_released, 2);
    assert_eq!(metrics.dirty_resets_completed, 2);
    assert_eq!(metrics.total_evicted, 1);
}

// ==============================================================================
// 2. UFFD Lazy-Population Descriptor Mechanics and Fault Handling
// ==============================================================================

#[test]
fn test_uffd_descriptor_mechanics_and_fault_handling() {
    // 1. Verify UFFD constants and structure memory sizes
    assert_eq!(std::mem::size_of::<hgb_core::microvm_snapshot_cache::UffdMsg>(), 32);
    assert_eq!(std::mem::size_of::<hgb_core::microvm_snapshot_cache::UffdioApi>(), 24);
    assert_eq!(std::mem::size_of::<hgb_core::microvm_snapshot_cache::UffdioRegister>(), 32);
    assert_eq!(std::mem::size_of::<hgb_core::microvm_snapshot_cache::UffdioCopy>(), 40);
    assert_eq!(std::mem::size_of::<hgb_core::microvm_snapshot_cache::UffdioZeropage>(), 32);

    // 2. Verify register mode bits
    assert_eq!(UFFDIO_REGISTER_MODE_MISSING, 1 << 0);
    assert_eq!(UFFD_EVENT_PAGEFAULT, 0x12);

    // 3. Test UffdMemoryHandler fault resolution mechanics
    let handler = UffdMemoryHandler::new(-1);
    let fault_addr_1 = 0x7fff_1000u64;
    let fault_addr_2 = 0x7fff_2000u64;
    let fault_addr_3 = 0x7fff_3000u64;

    // Normal page copy
    let mut pattern_page = vec![0u8; PAGE_SIZE];
    pattern_page[0] = 0xDE;
    pattern_page[1] = 0xAD;
    pattern_page[2] = 0xBE;
    pattern_page[3] = 0xEF;

    let res1 = handler.simulate_page_fault_resolution(fault_addr_1, &pattern_page, false)
        .expect("simulate missing page fault copy");
    assert_eq!(res1, PAGE_SIZE);

    // Zero-page resolution
    let zero_page = vec![0u8; PAGE_SIZE];
    let res2 = handler.simulate_page_fault_resolution(fault_addr_2, &zero_page, true)
        .expect("simulate missing page fault zero");
    assert_eq!(res2, PAGE_SIZE);

    // Another copy fault
    let res3 = handler.simulate_page_fault_resolution(fault_addr_3, &pattern_page, false)
        .expect("simulate second missing page fault copy");
    assert_eq!(res3, PAGE_SIZE);

    // 4. Verify aggregated stats
    let stats = handler.stats().snapshot();
    assert_eq!(stats.page_faults_total, 3);
    assert_eq!(stats.copy_faults_resolved, 2);
    assert_eq!(stats.zeropage_faults_resolved, 1);
    assert!(stats.avg_resolution_time_ns > 0);
}

// ==============================================================================
// 3. Sub-10ms Fast-Path Latency Measurement Validation
// ==============================================================================

#[test]
fn test_sub_10ms_fast_path_latency_target() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("latency_pool");

    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 5,
        max_pool_size: 10,
        min_idle_size: 3,
        max_reuse_count: 100,
        vcpu_count: 2,
        mem_size_mib: 128,
        target_resume_latency_ms: 10,
        enable_uffd: false,
        kernel_path: None,
        rootfs_path: None,
        cache_dir: Some(cache_dir),
    };

    let pool = MicrovmSnapshotPool::with_config(temp_dir.path(), config).expect("pool");

    // Perform 20 sequential acquires and releases, capturing nanosecond latencies
    let iterations = 20;
    let mut latencies_us = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let report = pool.measure_resume_latency().expect("measure resume latency");

        assert!(
            report.is_sub_10ms,
            "Iteration {} failed sub-10ms target: {:.3}ms ({}us)",
            i,
            report.duration_ms,
            report.duration_us
        );
        assert!(
            report.duration_ms < 10.0,
            "Resume latency must be strictly less than 10ms, got: {:.3}ms",
            report.duration_ms
        );

        latencies_us.push(report.duration_us);
    }

    let min_us = *latencies_us.iter().min().unwrap();
    let max_us = *latencies_us.iter().max().unwrap();
    let sum_us: u64 = latencies_us.iter().sum();
    let avg_us = sum_us as f64 / iterations as f64;
    let avg_ms = avg_us / 1000.0;

    println!(
        "Sub-10ms Benchmark Result: min: {}us, avg: {:.2}us ({:.3}ms), max: {}us ({:.3}ms)",
        min_us,
        avg_us,
        avg_ms,
        max_us,
        max_us as f64 / 1000.0
    );

    // Verify average is strictly well below 10ms (< 5ms comfortably)
    assert!(
        avg_ms < 5.0,
        "Average resume latency {:.3}ms must be well below 5.0ms (target: <10ms)",
        avg_ms
    );
}

// ==============================================================================
// 4. Safe Zero-Failure Fallback When Virtualization Capabilities Are Restricted
// ==============================================================================

#[test]
fn test_safe_fallback_when_virtualization_restricted() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("restricted_pool");

    // Force virtualization restriction: non-existent kernel & rootfs, disabled UFFD
    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 2,
        max_pool_size: 4,
        min_idle_size: 2,
        max_reuse_count: 5,
        vcpu_count: 1,
        mem_size_mib: 64,
        target_resume_latency_ms: 10,
        enable_uffd: false,
        kernel_path: Some("/invalid/missing/vmlinux".into()),
        rootfs_path: Some("/invalid/missing/rootfs.ext4".into()),
        cache_dir: Some(cache_dir),
    };

    let pool = MicrovmSnapshotPool::with_config(temp_dir.path(), config).expect("pool with fallback");

    // 1. Must gracefully select Linux Landlock LSM + volatile RAM CoW Overlay sandbox
    assert_eq!(
        pool.backend_type(),
        MemoryBackendType::LandlockRamCowFallback,
        "Expected zero-failure fallback to LandlockRamCowFallback"
    );

    // 2. Acquisition must succeed instantaneously
    let vm = pool.acquire().expect("acquire from fallback pool");
    assert_eq!(vm.backend_type, MemoryBackendType::LandlockRamCowFallback);
    assert!(vm.jail_dir.exists());

    // 3. Execute isolated shell workload in fallback jail
    let exec_report = vm.execute_command(
        &["sh".to_string(), "-c".to_string(), "echo 'fallback_execution_verified'".to_string()],
        None,
        &HashMap::new(),
    ).expect("exec in fallback jail");

    assert!(exec_report.success);
    assert_eq!(exec_report.exit_code, 0);
    assert!(exec_report.stdout.contains("fallback_execution_verified"));

    // 4. Release and verify clean return
    pool.release(vm).expect("release fallback instance");
    assert_eq!(pool.idle_count(), 2);

    let metrics = pool.metrics();
    assert!(metrics.fallback_invocations >= 1);
}

// ==============================================================================
// 5. Multi-Threaded Concurrent Acquisition & Release Stress Test
// ==============================================================================

#[test]
fn test_multithreaded_pool_concurrency_stress() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("concurrent_pool");

    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 4,
        max_pool_size: 8,
        min_idle_size: 2,
        max_reuse_count: 20,
        vcpu_count: 1,
        mem_size_mib: 64,
        target_resume_latency_ms: 10,
        enable_uffd: false,
        kernel_path: None,
        rootfs_path: None,
        cache_dir: Some(cache_dir),
    };

    let pool = Arc::new(MicrovmSnapshotPool::with_config(temp_dir.path(), config).expect("pool"));
    let mut handles = Vec::new();
    let num_threads = 4;
    let ops_per_thread = 10;
    let success_count = Arc::new(AtomicUsize::new(0));

    for thread_idx in 0..num_threads {
        let pool_ref = Arc::clone(&pool);
        let sc = Arc::clone(&success_count);

        let handle = std::thread::spawn(move || {
            for op in 0..ops_per_thread {
                let vm = pool_ref.acquire().expect("concurrent acquire");
                assert_eq!(vm.state, PooledMicrovmState::InUse);

                // Small isolated workload
                let cmd = vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("echo 'thread_{}_op_{}'", thread_idx, op),
                ];
                let report = vm.execute_command(&cmd, None, &HashMap::new())
                    .expect("execute");
                assert!(report.success);

                pool_ref.release(vm).expect("concurrent release");
                sc.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().expect("thread join");
    }

    assert_eq!(
        success_count.load(Ordering::Relaxed),
        num_threads * ops_per_thread,
        "All concurrent operations must finish successfully"
    );

    let metrics = pool.metrics();
    assert_eq!(metrics.total_acquired, num_threads * ops_per_thread);
    assert_eq!(metrics.total_released, num_threads * ops_per_thread);
    assert_eq!(metrics.dirty_resets_completed, num_threads * ops_per_thread);
}

// ==============================================================================
// 6. Copy-on-Write Memory Mapping & Madvise Reset
// ==============================================================================

#[test]
fn test_cow_memory_madvise_sanitization() {
    let size = 128 * 1024; // 128 KiB
    let region = CowMemoryRegion::allocate_anonymous(size).expect("allocate anon cow memory");
    assert_eq!(region.len(), size);

    // Fill region with dirty pattern
    unsafe {
        std::ptr::write_bytes(region.as_mut_ptr(), 0xFE, 8192);
        assert_eq!(*region.as_ptr(), 0xFE);
        assert_eq!(*region.as_ptr().add(8191), 0xFE);
    }

    // Reset via madvise(MADV_DONTNEED)
    region.reset_dirty_pages().expect("madvise dirty reset");

    // Verify zeroed by Linux virtual memory subsystem
    unsafe {
        assert_eq!(*region.as_ptr(), 0x00);
        assert_eq!(*region.as_ptr().add(8191), 0x00);
    }
}

// ==============================================================================
// 7. Ephemeral Sandbox Fast-Path Integration Test
// ==============================================================================

#[test]
fn test_ephemeral_sandbox_fast_path_integration() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let sandbox = FirecrackerEphemeralSandbox::new(temp_dir.path());

    // Access snapshot pool through sandbox
    let pool = sandbox.snapshot_pool();
    assert!(pool.idle_count() > 0);

    // Execute through fast path
    let spec = EphemeralExecSpec {
        code: "echo 'Pillar 3 Fast-Path MicroVM Verified'".to_string(),
        language: "bash".to_string(),
        patch_content: None,
        timeout_ms: 3000,
        memory_mb: 128,
        vcpu: 1,
    };

    let result = sandbox.execute_with_fast_path(&spec).expect("execute fast path");
    assert!(result.success);
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("Pillar 3 Fast-Path MicroVM Verified"));
    assert!(result.duration_ms < 1000);
    assert!(result.isolation_tier.contains("Hot-Pool Cached"));
}
