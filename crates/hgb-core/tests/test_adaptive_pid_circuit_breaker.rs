//! tests/test_adaptive_pid_circuit_breaker.rs
//! Comprehensive Integration Test Suite for Hagibis Adaptive PID Circuit Breaker

use hgb_core::adaptive_pid_circuit_breaker::*;

#[test]
fn test_end_to_end_adaptive_circuit_breaker_hysteresis() {
    let mut config = CircuitBreakerConfig::default();
    config.max_consecutive_failures = 3;
    config.recovery_threshold = 3;
    config.t0_secs = 1.0;
    config.t_max_secs = 16.0;

    let cb = AdaptivePidCircuitBreaker::new(config);
    let skill_id = "test_e2e_compiler_agent";

    // 1. Initial State Check
    let (can_run, state, _) = cb.check_mutation_eligibility(skill_id, 0.0);
    assert!(can_run);
    assert_eq!(state, CircuitState::Closed);

    // 2. Consecutive Failures Trip to OPEN
    for i in 1..=3 {
        let outcome = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some(format!("Error diagnostic attempt #{}", i)),
            error_fingerprint: Some(format!("err_signature_{}", i)),
            target_paths: vec![],
            duration_ms: 15.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(i as f64),
        });

        if i < 3 {
            assert_eq!(outcome.state, CircuitState::Closed);
            assert_eq!(outcome.consecutive_failures, i);
        } else {
            assert_eq!(outcome.state, CircuitState::Open);
            assert!(outcome.is_tripped);
            assert!(outcome.backoff_delay_secs > 0.0);
        }
    }

    // 3. Blocked during Backoff Delay
    let (can_run_blocked, state_blocked, reason) =
        cb.check_mutation_eligibility(skill_id, 3.5);
    assert!(!can_run_blocked);
    assert_eq!(state_blocked, CircuitState::Open);
    assert!(reason.contains("BLOCKED"));

    // 4. Backoff Expiration -> HALF_OPEN
    let (can_run_half, state_half, reason_half) =
        cb.check_mutation_eligibility(skill_id, 30.0);
    assert!(can_run_half);
    assert_eq!(state_half, CircuitState::HalfOpen);
    assert!(reason_half.contains("HALF_OPEN"));

    // 5. Probation Successes in RECOVERY -> CLOSED
    for turn in 1..=3 {
        let succ_out = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: true,
            error_trace: None,
            error_fingerprint: None,
            target_paths: vec![],
            duration_ms: 5.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(30.0 + turn as f64),
        });

        if turn < 3 {
            assert_eq!(succ_out.state, CircuitState::Recovery);
            assert_eq!(succ_out.consecutive_successes, turn);
        } else {
            assert_eq!(succ_out.state, CircuitState::Closed);
            assert_eq!(succ_out.consecutive_failures, 0);
        }
    }
}

#[test]
fn test_memory_leak_rate_escalation_to_cryptolock() {
    let mut config = CircuitBreakerConfig::default();
    config.memory.max_leak_rate_bytes_per_sec = 2.0 * 1024.0 * 1024.0; // 2 MB/s
    let cb = AdaptivePidCircuitBreaker::new(config);
    let skill_id = "mem_leak_subsystem";

    // Sample 1: Baseline 20 MB at t=1.0
    let (b1, s1) = cb.record_memory_reading(
        skill_id,
        20 * 1024 * 1024,
        18 * 1024 * 1024,
        100,
        1.0,
        &[],
    );
    assert_eq!(b1, MemoryBreachKind::None);
    assert_eq!(s1, CircuitState::Closed);

    // Sample 2: 22 MB at t=2.0
    let (b2, s2) = cb.record_memory_reading(
        skill_id,
        22 * 1024 * 1024,
        20 * 1024 * 1024,
        120,
        2.0,
        &[],
    );
    assert_eq!(b2, MemoryBreachKind::None);
    assert_eq!(s2, CircuitState::Closed);

    // Sample 3: 40 MB at t=3.0 -> Leak growth is +20 MB over 2s = 10 MB/s > 2 MB/s limit!
    let (b3, s3) = cb.record_memory_reading(
        skill_id,
        40 * 1024 * 1024,
        38 * 1024 * 1024,
        300,
        3.0,
        &[],
    );
    assert!(matches!(b3, MemoryBreachKind::LeakRateExceeded { .. }));
    assert_eq!(s3, CircuitState::PermTrippedCryptolocked);
}

#[test]
fn test_infinite_recursion_cycle_detection_in_codegen() {
    let mut config = CircuitBreakerConfig::default();
    config.max_recursion_depth = 5;
    let cb = AdaptivePidCircuitBreaker::new(config);
    let skill_id = "codegen_recursion_agent";

    // Simulate recursive agent expansion reaching depth limit
    let out = cb.record_execution_attempt(ExecutionAttemptSpec {
        skill_id: skill_id.into(),
        success: true,
        error_trace: None,
        error_fingerprint: None,
        target_paths: vec![],
        duration_ms: 20.0,
        turn_number: Some(1),
        state_hash: Some("hash_initial".into()),
        turn_intent: Some("Recursive macro expansion".into()),
        recursion_depth: Some(5), // Breaches max_depth (5)
        current_time_secs: Some(10.0),
    });

    assert_eq!(out.state, CircuitState::PermTrippedCryptolocked);
    assert!(out.is_tripped);
    assert!(out.cycle_result.is_some());
    let cycle_info = out.cycle_result.unwrap();
    assert!(matches!(
        cycle_info.pattern,
        CyclePatternKind::RecursionDepthExceeded { .. }
    ));
    assert!(cycle_info.pivot_prescription.contains("HALT_RECURSIVE_EXPANSION"));
}

#[test]
fn test_ed25519_mcp_unlock_with_tamper_detection() {
    let cb = AdaptivePidCircuitBreaker::new(CircuitBreakerConfig::default());
    let skill_id = "tamper_resistant_kernel";

    // Trip into PERM_TRIPPED_CRYPTOLOCKED via 3 consecutive identical errors
    for _ in 0..3 {
        let _ = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("E0001 identical fault".into()),
            error_fingerprint: Some("identical_hash_xxx".into()),
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(1.0),
        });
    }

    let (blocked, state, _) = cb.check_mutation_eligibility(skill_id, 2.0);
    assert!(!blocked);
    assert_eq!(state, CircuitState::PermTrippedCryptolocked);

    let operator_seed = b"authorized-governor-operator-key-seed";
    let operator_keypair = GovernorKeypair::generate_deterministic(operator_seed);
    let operator_pub = operator_keypair.public_key_hex();
    let operator_id = "hgb_admin";
    let nonce = 5544332211u64;

    // Tampered signature should be rejected
    let tampered_sig = "ff".repeat(64);
    let tamper_res = cb.operator_unlock(McpUnlockRequest {
        skill_id: skill_id.into(),
        operator_id: operator_id.into(),
        lockout_id: "lockout_any".into(),
        nonce,
        ed25519_signature: tampered_sig,
        operator_public_key: operator_pub.clone(),
        restore_permissions: false,
    });
    assert!(tamper_res.is_err());
}
