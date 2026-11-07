//! Integration test for Wasmtime Multi-Tenant JIT Pooling & Snapshot FaaS Runner

use hgb_core::wasmtime_multitenant_faas_pool::{
    FaasCacheTier, FaasPoolConfig, LanguageKind,
    WasmtimeMultitenantFaasPool,
};
use std::time::Instant;
use tempfile::tempdir;

#[tokio::test]
async fn test_faas_pool_jit_pooling_and_snapshot() {
    let temp_dir = tempdir().expect("temp dir");
    let config = FaasPoolConfig::default()
        .with_instances(64)
        .with_memory_pages(256)
        .with_pooling(true)
        .with_aot_cache_dir(temp_dir.path().join("aot_cache"));

    let pool = WasmtimeMultitenantFaasPool::with_config_and_base(config, temp_dir.path().join("tenants"))
        .expect("Failed to initialize FaaS pool");

    assert!(pool.is_pooling_enabled(), "Instance pooling allocator should be active");

    let wat_code = r#"
        (module
            (memory (export "memory") 1)
            (global $counter (mut i32) (i32.const 10))
            (export "counter" (global $counter))

            (func (export "init") (result i32)
                (i32.store8 (i32.const 50) (i32.const 99))
                (global.set $counter (i32.add (global.get $counter) (i32.const 5)))
                global.get $counter
            )

            (func (export "run") (result i32)
                (i32.add (i32.load8_u (i32.const 50)) (global.get $counter))
            )
        )
    "#;

    // 1. Initialize and capture snapshot
    let (init_rep, snapshot) = pool
        .execute_and_snapshot(
            "tenant_alpha",
            LanguageKind::Wat,
            wat_code,
            "init",
            "snap_step_1",
        )
        .await
        .expect("Init and snapshot failed");

    assert_eq!(init_rep.exit_code, 15);
    assert!(snapshot.verify_integrity());
    assert_eq!(snapshot.memory_bytes[50], 99);

    // 2. Instant-restore from snapshot and execute
    let restore_start = Instant::now();
    let run_rep = pool
        .execute_faas_from_snapshot(
            "tenant_alpha",
            "snap_step_1",
            Some("run"),
            None,
            None,
            None,
        )
        .await
        .expect("Execution from snapshot failed");
    let restore_us = restore_start.elapsed().as_micros();

    println!("Instant-restore + exec duration: {} us", restore_us);
    assert_eq!(run_rep.cache_tier, FaasCacheTier::SnapshotRestored);
    assert_eq!(run_rep.exit_code, 99 + 15); // 114
    assert!(run_rep.is_success());
    assert!(run_rep.security_attestation.verify());

    // 3. Sub-millisecond spinup latency check on warm InstancePre
    let warm_rep = pool
        .execute_faas("tenant_alpha", LanguageKind::Wat, wat_code, Some("init"), None, None, None)
        .await
        .expect("Warm execution failed");

    assert_eq!(warm_rep.cache_tier, FaasCacheTier::InstancePreHit);
    println!("Warm InstancePre spinup: {} us", warm_rep.spinup_duration_us);
    assert!(warm_rep.spinup_duration_us < 2000);
}
