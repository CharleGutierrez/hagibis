//! Comprehensive Integration Tests for Roadmap Item 3:
//! MicroVM Snapshot Pooling & Pre-Warmed Snaps Daemon (`snapshot_pool_daemon`)
//!
//! Validates:
//! 1. Background daemon initialization and autonomous replenishment up to `min_idle_size`.
//! 2. Sub-5ms acquisition latency on pool hit.
//! 3. Dirty page sanitization and overlay rollback on instance release.
//! 4. Safe fallback when host is unprivileged.
//! 5. Comprehensive health check and exposed daemon metrics.

use std::fs;
use std::time::{Duration, Instant};

use hgb_core::microvm_snapshot_cache::{
    MemoryBackendType, MicrovmSnapshotPoolConfig, PooledMicrovmState,
};
use hgb_core::snapshot_pool_daemon::SnapshotPoolDaemon;

// ==============================================================================
// 1. Autonomous Replenishment Test
// ==============================================================================

#[tokio::test]
async fn test_snapshot_pool_daemon_autonomous_replenishment() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("hgb_pool_daemon_cache");

    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 3,
        max_pool_size: 8,
        min_idle_size: 3,
        max_reuse_count: 20,
        vcpu_count: 2,
        mem_size_mib: 64,
        target_resume_latency_ms: 5,
        enable_uffd: false, // deterministic test environment
        kernel_path: None,
        rootfs_path: None,
        cache_dir: Some(cache_dir),
    };

    let daemon = SnapshotPoolDaemon::with_config(temp_dir.path(), config).expect("init daemon");
    daemon.start_replenishment_loop();

    // Initial state: initial_pool_size pre-warmed idle instances
    assert_eq!(daemon.idle_count(), 3);
    assert_eq!(daemon.active_count(), 0);
    assert_eq!(daemon.pool_size(), 3);

    // Acquire instance 1
    let vm1 = daemon.acquire_prewarmed().await.expect("acquire vm1");
    assert_eq!(vm1.state, PooledMicrovmState::InUse);
    assert_eq!(daemon.active_count(), 1);

    // Acquire instance 2 - idle count drops to 1, which is below min_idle_size (3)
    let vm2 = daemon.acquire_prewarmed().await.expect("acquire vm2");
    assert_eq!(daemon.active_count(), 2);

    // Autonomous replenishment: background daemon replenishes up to min_idle_size
    let start_wait = Instant::now();
    let mut replenished = false;
    while start_wait.elapsed() < Duration::from_secs(3) {
        if daemon.idle_count() >= 3 {
            replenished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        replenished,
        "Autonomous daemon replenishment timed out; idle count is {}",
        daemon.idle_count()
    );
    assert!(daemon.idle_count() >= 3);

    // Release instances back
    daemon.release_instance(vm1).await.expect("release vm1");
    daemon.release_instance(vm2).await.expect("release vm2");

    let status = daemon.pool_status();
    assert!(status.is_healthy);
    assert!(status.total_acquired >= 2);
    assert!(status.total_released >= 2);

    daemon.shutdown();
}

// ==============================================================================
// 2. Sub-5ms Acquisition Latency on Pool Hit
// ==============================================================================

#[tokio::test]
async fn test_sub_5ms_acquisition_latency_on_pool_hit() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("hgb_pool_latency_cache");

    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 4,
        max_pool_size: 8,
        min_idle_size: 2,
        max_reuse_count: 50,
        vcpu_count: 1,
        mem_size_mib: 64,
        target_resume_latency_ms: 5,
        enable_uffd: false,
        kernel_path: None,
        rootfs_path: None,
        cache_dir: Some(cache_dir),
    };

    let daemon = SnapshotPoolDaemon::with_config(temp_dir.path(), config).expect("init daemon");

    // Pool is pre-warmed with 4 instances. Acquire should be a pool hit taking << 5ms.
    let start = Instant::now();
    let vm = daemon.acquire_prewarmed().await.expect("acquire prewarmed");
    let elapsed = start.elapsed();

    // Verify sub-5ms acquisition latency
    assert!(
        elapsed < Duration::from_millis(5),
        "Cold-start latency was {:?}, which exceeded 5ms limit",
        elapsed
    );

    // Verify instance state
    assert_eq!(vm.state, PooledMicrovmState::InUse);
    assert!(vm.last_resume_duration_us < 5000);

    // Verify metrics reflect pool hit
    let metrics = daemon.metrics();
    assert_eq!(metrics.pool_hits, 1);
    assert_eq!(metrics.pool_misses, 0);
    assert_eq!(metrics.hit_rate, 1.0);
    assert!(metrics.average_resume_duration_us < 5000);

    daemon.release_instance(vm).await.expect("release");
    daemon.shutdown();
}

// ==============================================================================
// 3. Dirty Page Sanitization & Overlay Rollback on Release
// ==============================================================================

#[tokio::test]
async fn test_dirty_page_sanitization_on_release() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("hgb_pool_dirty_cache");

    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 2,
        max_pool_size: 4,
        min_idle_size: 1,
        max_reuse_count: 10,
        vcpu_count: 1,
        mem_size_mib: 64,
        target_resume_latency_ms: 5,
        enable_uffd: false,
        kernel_path: None,
        rootfs_path: None,
        cache_dir: Some(cache_dir),
    };

    let daemon = SnapshotPoolDaemon::with_config(temp_dir.path(), config).expect("init daemon");

    let vm = daemon.acquire_prewarmed().await.expect("acquire");

    // Simulate workload dirtying volatile memory and writing temporary artifacts
    if let Some(ref mem) = vm.memory_region {
        unsafe {
            let ptr = mem.as_ptr() as *mut u8;
            if !ptr.is_null() && mem.len() > 8 {
                *ptr = 0xDE;
                *ptr.add(1) = 0xAD;
                *ptr.add(2) = 0xBE;
                *ptr.add(3) = 0xEF;
            }
        }
    }

    // Write file into overlay upper layer
    let upper = vm.jail_dir.join("overlay/upper");
    if upper.exists() {
        fs::write(upper.join("polluted_state.txt"), b"dirty dirty dirty").expect("write dirty");
        assert!(upper.join("polluted_state.txt").exists());
    }

    // Release back to pool - resets dirty pages and scrubs overlay
    daemon.release_instance(vm).await.expect("release instance");

    // Check daemon metrics
    let metrics = daemon.metrics();
    assert_eq!(metrics.total_released, 1);
    assert_eq!(metrics.dirty_resets_completed, 1);

    // If overlay upper existed, verify it was scrubbed clean
    if upper.exists() {
        assert!(
            !upper.join("polluted_state.txt").exists(),
            "Dirty file was not purged during sanitization"
        );
    }

    daemon.shutdown();
}

// ==============================================================================
// 4. Safe Fallback When Host is Unprivileged
// ==============================================================================

#[tokio::test]
async fn test_safe_fallback_when_host_unprivileged() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cache_dir = temp_dir.path().join("hgb_unprivileged_cache");

    // Force non-existent paths and disable UFFD to simulate unprivileged/sandboxed host
    let config = MicrovmSnapshotPoolConfig {
        initial_pool_size: 2,
        max_pool_size: 4,
        min_idle_size: 2,
        max_reuse_count: 10,
        vcpu_count: 1,
        mem_size_mib: 32,
        target_resume_latency_ms: 10,
        enable_uffd: false,
        kernel_path: Some("/nonexistent/vmlinux.bin".into()),
        rootfs_path: Some("/nonexistent/rootfs.ext4".into()),
        cache_dir: Some(cache_dir),
    };

    let daemon =
        SnapshotPoolDaemon::with_config(temp_dir.path(), config).expect("init unprivileged daemon");

    // Verify safe fallback
    let status = daemon.pool_status();
    assert!(status.is_unprivileged_fallback);
    assert_eq!(
        status.backend_type,
        MemoryBackendType::LandlockRamCowFallback.to_string()
    );
    assert!(status.is_healthy);
    assert_eq!(daemon.idle_count(), 2);

    // Health check must pass even under unprivileged fallback
    let health = daemon.health_check().await.expect("health check");
    assert!(health.is_healthy);
    assert!(health.latency_sub_5ms);
    assert!(health.dirty_page_reset_verified);

    daemon.shutdown();
}
