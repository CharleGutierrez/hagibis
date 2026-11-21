//! # `test_quorum_circuit_breaker.rs` - Mesh Resilience & Quorum Consensus Verification
//! ====================================================================================
//! Tests:
//! 1. Quorum rejection when fewer than 2 adjacent peers endorse a trip (stays LocalOpen).
//! 2. Quorum acceptance when >= 2 adjacent peers endorse a trip (transitions to GlobalOpen).
//! 3. Full jitter backoff distribution randomness within mathematical bounds.
//! 4. Execution permission semantics under LocalOpen, HalfOpen, and Closed.
//! 5. Recovery lifecycle upon successful probe execution.

use hgb_mesh::quorum_circuit_breaker::{
    CircuitBreakerState, PeerEndorsement, QuorumCircuitBreaker, QuorumVoteResult,
};

#[test]
fn test_quorum_rejection_insufficient_peer_endorsements() {
    let adjacent = vec!["node-peer-1".to_string(), "node-peer-2".to_string(), "node-peer-3".to_string()];
    let mut breaker = QuorumCircuitBreaker::new("node-origin", "vector-rag-skill", adjacent);
    let now_ms = 1_700_000_000_000u64;

    // Test with only 1 adjacent peer endorsement
    let endorsements = vec![PeerEndorsement::new("node-peer-1", true, "Transient I/O timeout")];
    let res = breaker.evaluate_quorum(CircuitBreakerState::GlobalOpen, &endorsements, now_ms);

    // Breaker must isolate to LocalOpen to protect the cluster
    assert_eq!(breaker.state, CircuitBreakerState::LocalOpen);
    match res {
        QuorumVoteResult::QuorumRejected { achieved_peers, required_peers, isolated_state, reason } => {
            assert_eq!(achieved_peers, 1);
            assert_eq!(required_peers, 2);
            assert_eq!(isolated_state, CircuitBreakerState::LocalOpen);
            assert!(reason.contains("Only 1/2 adjacent peers"));
        }
        _ => panic!("Expected QuorumRejected, got {:?}", res),
    }

    // Local execution is blocked during active backoff
    assert!(!breaker.is_execution_permitted(now_ms));
}

#[test]
fn test_quorum_acceptance_sufficient_peer_endorsements() {
    let adjacent = vec!["node-peer-1".to_string(), "node-peer-2".to_string(), "node-peer-3".to_string()];
    let mut breaker = QuorumCircuitBreaker::new("node-origin", "distributed-index-skill", adjacent);
    let now_ms = 1_700_000_000_000u64;

    // Test with 2 adjacent peer endorsements
    let endorsements = vec![
        PeerEndorsement::new("node-peer-1", true, "Corrupted index segment"),
        PeerEndorsement::new("node-peer-2", true, "Corrupted index segment"),
    ];
    let res = breaker.evaluate_quorum(CircuitBreakerState::GlobalOpen, &endorsements, now_ms);

    // State must transition to GlobalOpen
    assert_eq!(breaker.state, CircuitBreakerState::GlobalOpen);
    match res {
        QuorumVoteResult::QuorumAccepted { achieved_peers, required_peers, transitioned_state } => {
            assert_eq!(achieved_peers, 2);
            assert_eq!(required_peers, 2);
            assert_eq!(transitioned_state, CircuitBreakerState::GlobalOpen);
        }
        _ => panic!("Expected QuorumAccepted, got {:?}", res),
    }

    assert!(breaker.state.is_globally_tripped());
    assert!(!breaker.is_execution_permitted(now_ms));
}

#[test]
fn test_full_jitter_backoff_randomness_and_bounds() {
    let t0 = 1.0;
    let t_max = 32.0;

    for failures in 1..=5 {
        let mut samples = Vec::new();
        for _ in 0..50 {
            let backoff = QuorumCircuitBreaker::calculate_full_jitter_backoff(failures, t0, t_max);
            assert!(backoff >= 0.0);
            let nominal = t_max.min(t0 * 2.0f64.powi(failures as i32));
            assert!(backoff <= nominal + 0.001);
            samples.push(backoff);
        }

        // Verify distinct values (jitter distribution variance)
        let unique: std::collections::HashSet<_> = samples.iter().map(|v| (v * 100.0) as u64).collect();
        assert!(unique.len() > 10, "Full jitter backoff must generate diverse values");
    }
}

#[test]
fn test_recovery_lifecycle_and_record_success() {
    let adjacent = vec!["peer-1".to_string(), "peer-2".to_string()];
    let mut breaker = QuorumCircuitBreaker::new("local-node", "flaky-skill", adjacent)
        .with_thresholds(2, 2);
    let now_ms = 1_700_000_000_000u64;

    // Fail twice with no endorsements -> isolated to LocalOpen
    let (_, r1) = breaker.record_failure_with_endorsements(&[], now_ms);
    assert!(r1.is_none());
    assert_eq!(breaker.state, CircuitBreakerState::Closed);

    let (s2, r2) = breaker.record_failure_with_endorsements(&[], now_ms);
    assert_eq!(s2, CircuitBreakerState::LocalOpen);
    assert!(r2.is_some());
    assert!(!breaker.is_execution_permitted(now_ms));

    // Fast-forward past backoff expiration: moves to HalfOpen
    let future_ms = breaker.open_until_ms + 1000;
    assert!(breaker.is_execution_permitted(future_ms));
    assert_eq!(breaker.state, CircuitBreakerState::HalfOpen);

    // First successful probe moves to Recovery
    let post_probe1 = breaker.record_success();
    assert_eq!(post_probe1, CircuitBreakerState::Recovery);

    // Second successful probe satisfies recovery_threshold (2) and restores to Closed!
    let post_probe2 = breaker.record_success();
    assert_eq!(post_probe2, CircuitBreakerState::Closed);
    assert!(breaker.is_execution_permitted(future_ms + 100));
}
