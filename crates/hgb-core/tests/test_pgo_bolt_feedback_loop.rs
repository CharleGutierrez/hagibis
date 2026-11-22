//! tests/test_pgo_bolt_feedback_loop.rs
//! Comprehensive Integration Test Suite for Continuous PGO/BOLT Feedback Pipeline

use hgb_core::pgo_bolt_feedback_loop::*;
use std::fs;
use std::path::PathBuf;

#[test]
fn test_continuous_pgo_bolt_trigger_and_swap_pipeline() {
    let base_tmp = std::env::temp_dir().join(format!(
        "hgb_pgo_integ_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    fs::create_dir_all(&base_tmp).unwrap();

    let config = PgoBoltConfig {
        target_binary_name: "test_daemon".to_string(),
        active_symlink_path: base_tmp.join("active_bin"),
        shadow_build_dir: base_tmp.join("shadow"),
        profile_storage_dir: base_tmp.join("profiles"),
        backup_storage_dir: base_tmp.join("backups"),
        merged_profdata_path: base_tmp.join("merged.profdata"),
        thresholds: TriggerThresholds {
            p99_latency_threshold_ms: 150.0,
            icache_miss_threshold_pct: 10.0,
            cooldown_seconds: 10,
            ..Default::default()
        },
    };

    let pipeline = ContinuousPgoBoltFeedbackPipeline::new(config.clone());
    assert_eq!(pipeline.current_state(), PipelineLifecycleState::Idle);

    // 1. Nominal Telemetry -> No optimization triggered
    let mut telem_nominal = ContinuousTelemetrySnapshot::default();
    telem_nominal.p99_latency_ms = 40.0;
    telem_nominal.icache_miss_rate_pct = 3.2;

    let res1 = pipeline
        .execute_optimization_cycle(&telem_nominal, 1000, false)
        .unwrap();
    assert!(res1.is_none());
    assert_eq!(pipeline.current_state(), PipelineLifecycleState::Idle);

    // 2. High Cache Miss Telemetry (14.2% > 10.0%) -> Triggers Optimization Cycle
    let mut telem_spike = telem_nominal.clone();
    telem_spike.icache_miss_rate_pct = 14.2;

    let res2 = pipeline
        .execute_optimization_cycle(&telem_spike, 1000, false)
        .unwrap();
    assert!(res2.is_some());
    let swap_receipt = res2.unwrap();
    assert_eq!(swap_receipt.status, "HOT_SWAP_COMPLETED");
    assert!(config.active_symlink_path.exists());

    // 3. Post-Swap Health Check: Nominal Performance -> No Rollback
    let mut post_swap_nominal = telem_nominal.clone();
    post_swap_nominal.p99_latency_ms = 38.0; // Faster!
    let health1 = pipeline
        .verify_post_swap_health(&post_swap_nominal)
        .unwrap();
    assert!(health1.is_none());
    assert_eq!(pipeline.current_state(), PipelineLifecycleState::Cooldown);

    let _ = fs::remove_dir_all(&base_tmp);
}

#[test]
fn test_post_swap_regression_automatic_rollback() {
    let base_tmp = std::env::temp_dir().join(format!(
        "hgb_pgo_regress_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    fs::create_dir_all(&base_tmp).unwrap();

    let config = PgoBoltConfig {
        target_binary_name: "regress_daemon".to_string(),
        active_symlink_path: base_tmp.join("active_bin"),
        shadow_build_dir: base_tmp.join("shadow"),
        profile_storage_dir: base_tmp.join("profiles"),
        backup_storage_dir: base_tmp.join("backups"),
        merged_profdata_path: base_tmp.join("merged.profdata"),
        thresholds: TriggerThresholds {
            p99_latency_threshold_ms: 100.0,
            rollback_regression_threshold_pct: 15.0,
            ..Default::default()
        },
    };

    // Create initial production binary so backup and rollback have an authentic target
    fs::write(&config.active_symlink_path, b"ORIGINAL_PROD_BINARY_V1").unwrap();

    let pipeline = ContinuousPgoBoltFeedbackPipeline::new(config.clone());

    // Trigger optimization cycle with baseline 80ms P99
    let mut baseline_telem = ContinuousTelemetrySnapshot::default();
    baseline_telem.p99_latency_ms = 120.0; // triggers breach

    let swap = pipeline
        .execute_optimization_cycle(&baseline_telem, 100, true)
        .unwrap();
    assert!(swap.is_some());

    // Simulate post-swap regression: P99 explodes to 180ms (+50% regression > 15% threshold)
    let mut regressed_telem = baseline_telem.clone();
    regressed_telem.p99_latency_ms = 180.0;

    let rollback = pipeline
        .verify_post_swap_health(&regressed_telem)
        .unwrap();
    assert!(rollback.is_some());
    let rollback_receipt = rollback.unwrap();
    assert_eq!(rollback_receipt.status, "ROLLBACK_RESTORED_SUCCESS");
    assert!(rollback_receipt.post_swap_latency_regression_pct > 15.0);

    let _ = fs::remove_dir_all(&base_tmp);
}
