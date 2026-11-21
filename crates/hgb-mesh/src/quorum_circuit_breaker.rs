//! # Quorum-Based Circuit Breaker for Hagibis Mesh Nodes
//!
//! Strengthens distributed mesh resilience by combining:
//! 1. Quorum-based cluster state validation: requires agreement from at least 2 adjacent
//!    mesh nodes before transitioning to `GlobalOpen` or `PermTripped`.
//! 2. Localized fault isolation: transient error spikes with fewer than 2 peer endorsements
//!    are isolated to `LocalOpen`, preventing cluster-wide service isolation.
//! 3. Randomized exponential backoff with full jitter smoothing:
//!    `uniform(0.0, min(t_max, t0 * 2^failures))`

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::error::{MeshError, Result};

/// Circuit breaker operational states in the distributed mesh
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CircuitBreakerState {
    /// Normal operation; requests permitted
    Closed,
    /// Trial probing state allowing controlled test invocations
    HalfOpen,
    /// Recovery probation state requiring consecutive successes before returning to Closed
    Recovery,
    /// Localized transient trip isolated to this node (insufficient quorum for global trip)
    LocalOpen,
    /// Confirmed global cluster-wide open trip endorsed by >= 2 adjacent mesh peers
    GlobalOpen,
    /// Permanent trip due to repeated identical AST failures endorsed by mesh quorum
    PermTripped,
    /// Permanent cryptographic lockout due to flapping compilation oscillation
    PermTrippedCryptolocked,
}

impl CircuitBreakerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Closed => "CLOSED",
            Self::HalfOpen => "HALF_OPEN",
            Self::Recovery => "RECOVERY",
            Self::LocalOpen => "LOCAL_OPEN",
            Self::GlobalOpen => "GLOBAL_OPEN",
            Self::PermTripped => "PERM_TRIPPED",
            Self::PermTrippedCryptolocked => "PERM_TRIPPED_CRYPTOLOCKED",
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self, Self::LocalOpen | Self::GlobalOpen | Self::PermTripped | Self::PermTrippedCryptolocked)
    }

    pub fn is_globally_tripped(&self) -> bool {
        matches!(self, Self::GlobalOpen | Self::PermTripped | Self::PermTrippedCryptolocked)
    }
}

/// Peer node endorsement for circuit breaker state transition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerEndorsement {
    pub peer_id: String,
    pub endorsed: bool,
    pub timestamp_ms: u64,
    pub signature: Option<String>,
    pub reason: String,
}

impl PeerEndorsement {
    pub fn new(peer_id: impl Into<String>, endorsed: bool, reason: impl Into<String>) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            peer_id: peer_id.into(),
            endorsed,
            timestamp_ms: now_ms,
            signature: None,
            reason: reason.into(),
        }
    }

    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }
}

/// Result of evaluating mesh quorum across peer endorsements
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuorumVoteResult {
    /// Quorum accepted (>= required_quorum peer endorsements)
    QuorumAccepted {
        achieved_peers: usize,
        required_peers: usize,
        transitioned_state: CircuitBreakerState,
    },
    /// Quorum rejected (< required_quorum peer endorsements); breaker isolated locally
    QuorumRejected {
        achieved_peers: usize,
        required_peers: usize,
        isolated_state: CircuitBreakerState,
        reason: String,
    },
}

impl QuorumVoteResult {
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::QuorumAccepted { .. })
    }

    pub fn achieved_peers(&self) -> usize {
        match self {
            Self::QuorumAccepted { achieved_peers, .. } => *achieved_peers,
            Self::QuorumRejected { achieved_peers, .. } => *achieved_peers,
        }
    }
}

/// Mesh-aware Quorum Circuit Breaker with full jitter backoff smoothing
#[derive(Debug, Clone)]
pub struct QuorumCircuitBreaker {
    pub node_id: String,
    pub skill_id: String,
    pub adjacent_peers: HashSet<String>,
    pub min_quorum_peers: usize,
    pub state: CircuitBreakerState,
    pub consecutive_failures: usize,
    pub consecutive_successes: usize,
    pub failure_threshold: usize,
    pub recovery_threshold: usize,
    pub t0_sec: f64,
    pub t_max_sec: f64,
    pub open_until_ms: u64,
}

impl QuorumCircuitBreaker {
    pub fn new(
        node_id: impl Into<String>,
        skill_id: impl Into<String>,
        adjacent_peers: Vec<String>,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            skill_id: skill_id.into(),
            adjacent_peers: adjacent_peers.into_iter().collect(),
            min_quorum_peers: 2,
            state: CircuitBreakerState::Closed,
            consecutive_failures: 0,
            consecutive_successes: 0,
            failure_threshold: 3,
            recovery_threshold: 2,
            t0_sec: 1.0,
            t_max_sec: 64.0,
            open_until_ms: 0,
        }
    }

    pub fn with_min_quorum(mut self, min_quorum: usize) -> Self {
        self.min_quorum_peers = min_quorum;
        self
    }

    pub fn with_thresholds(mut self, failure_threshold: usize, recovery_threshold: usize) -> Self {
        self.failure_threshold = failure_threshold;
        self.recovery_threshold = recovery_threshold;
        self
    }

    pub fn with_backoff_params(mut self, t0: f64, t_max: f64) -> Self {
        self.t0_sec = t0;
        self.t_max_sec = t_max;
        self
    }

    /// Check if skill invocation is currently permitted
    pub fn is_execution_permitted(&mut self, current_time_ms: u64) -> bool {
        match self.state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::HalfOpen => true,
            CircuitBreakerState::Recovery => true,
            CircuitBreakerState::PermTripped | CircuitBreakerState::PermTrippedCryptolocked => false,
            CircuitBreakerState::LocalOpen | CircuitBreakerState::GlobalOpen => {
                if current_time_ms >= self.open_until_ms && self.open_until_ms > 0 {
                    self.state = CircuitBreakerState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful execution probe
    pub fn record_success(&mut self) -> CircuitBreakerState {
        self.consecutive_failures = 0;
        match self.state {
            CircuitBreakerState::HalfOpen => {
                self.consecutive_successes = 1;
                if self.consecutive_successes >= self.recovery_threshold {
                    self.state = CircuitBreakerState::Closed;
                } else {
                    self.state = CircuitBreakerState::Recovery;
                }
            }
            CircuitBreakerState::Recovery => {
                self.consecutive_successes += 1;
                if self.consecutive_successes >= self.recovery_threshold {
                    self.state = CircuitBreakerState::Closed;
                }
            }
            CircuitBreakerState::Closed => {
                self.consecutive_successes += 1;
            }
            _ => {}
        }
        self.state
    }

    /// Calculates randomized exponential backoff with full jitter smoothing:
    ///   delay = uniform(0.0, min(t_max, t0 * 2^failures))
    pub fn calculate_full_jitter_backoff(failures: usize, t0: f64, t_max: f64) -> f64 {
        let cap = (t0 * (2.0f64.powi(failures as i32))).min(t_max);
        if cap <= 0.0 {
            return 0.0;
        }

        // Fast high-entropy uniform random generation in [0.0, cap]
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(42);
        let mut x = nanos ^ 0x5DEECE66D;
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        let frac = ((x >> 11) as f64) / ((1u64 << 53) as f64);
        frac * cap
    }

    /// Evaluates peer endorsements and applies quorum state transition:
    /// - If endorsements >= min_quorum_peers: transitions to proposed state (e.g. GlobalOpen).
    /// - If endorsements < min_quorum_peers: rejects quorum; transitions to LocalOpen.
    pub fn evaluate_quorum(
        &mut self,
        proposed_state: CircuitBreakerState,
        peer_endorsements: &[PeerEndorsement],
        current_time_ms: u64,
    ) -> QuorumVoteResult {
        // Collect unique endorsing adjacent peers
        let mut valid_peers = HashSet::new();
        for endorsement in peer_endorsements {
            if endorsement.endorsed && self.adjacent_peers.contains(&endorsement.peer_id) {
                valid_peers.insert(endorsement.peer_id.clone());
            }
        }

        let achieved = valid_peers.len();
        let backoff_sec = Self::calculate_full_jitter_backoff(
            self.consecutive_failures,
            self.t0_sec,
            self.t_max_sec,
        );
        self.open_until_ms = current_time_ms + (backoff_sec * 1000.0) as u64;

        if achieved >= self.min_quorum_peers {
            self.state = proposed_state;
            QuorumVoteResult::QuorumAccepted {
                achieved_peers: achieved,
                required_peers: self.min_quorum_peers,
                transitioned_state: proposed_state,
            }
        } else {
            // Local transient trip: prevent cluster-wide isolation!
            self.state = CircuitBreakerState::LocalOpen;
            QuorumVoteResult::QuorumRejected {
                achieved_peers: achieved,
                required_peers: self.min_quorum_peers,
                isolated_state: CircuitBreakerState::LocalOpen,
                reason: format!(
                    "Only {}/{} adjacent peers endorsed trip; isolated to LocalOpen",
                    achieved, self.min_quorum_peers
                ),
            }
        }
    }

    /// Record failure with peer endorsements
    pub fn record_failure_with_endorsements(
        &mut self,
        peer_endorsements: &[PeerEndorsement],
        current_time_ms: u64,
    ) -> (CircuitBreakerState, Option<QuorumVoteResult>) {
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;

        if self.consecutive_failures >= self.failure_threshold {
            let vote_res = self.evaluate_quorum(
                CircuitBreakerState::GlobalOpen,
                peer_endorsements,
                current_time_ms,
            );
            (self.state, Some(vote_res))
        } else {
            (self.state, None)
        }
    }
}
