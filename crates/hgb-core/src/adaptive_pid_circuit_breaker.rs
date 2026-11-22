//! # Adaptive PID Circuit Breaker with Hysteresis & Cryptographic Lockout
//!
//! Autonomous Healing & Governance Engine for Project Hagibis (Pillars 3 & 4):
//! - **Hysteresis State Machine**: `Closed`, `LocalOpen`, `GlobalOpen`, `Open`, `HalfOpen`, `Recovery`, `PermTripped`, `PermTrippedCryptolocked`.
//! - **Memory Spike & Leak Rate Supervision**: Monitors instantaneous memory spikes, monotonic leak accumulation, and leak rate limits.
//! - **Sliding-Window Anomaly Detection**: Computes rolling anomaly scores and failure rates over sliding time windows.
//! - **Flapping Error Oscillation Detection**: Detects alternating compilation errors (A <-> B <-> A), repeated identical errors (A -> A -> A), and high-frequency transitions.
//! - **Infinite Recursion & Cycle Detection**: Detects cyclic state loops (A -> B -> ... -> A) and call recursion depth breaches in autonomous code generation loops.
//! - **Cryptographic State Signing & Proof Receipts**: Ed25519 signing and BLAKE3 error hashing for `PERM_TRIPPED_CRYPTOLOCKED` state with read-only file permission stripping (0444).
//! - **Authorized Operator MCP Unlock**: Single-use replay-defended nonce verification and Ed25519 signature validation restoring write permissions and transitioning to `Closed`.
//! - **Adaptive PID Controller**: Modulates backoff dampening and tripping thresholds dynamically to eliminate resonant thrashing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

// ==============================================================================
// 1. Constants & Default Configurations
// ==============================================================================

pub const DEFAULT_MAX_CONSECUTIVE_FAILURES: usize = 3;
pub const DEFAULT_T0_SECS: f64 = 1.0;
pub const DEFAULT_TMAX_SECS: f64 = 64.0;
pub const DEFAULT_WINDOW_SIZE: usize = 20;
pub const DEFAULT_WINDOW_DURATION_SECS: f64 = 60.0;
pub const DEFAULT_MIN_EVENTS_ANOMALY: usize = 5;
pub const DEFAULT_ANOMALY_THRESHOLD: f64 = 0.50;
pub const DEFAULT_RECOVERY_THRESHOLD: usize = 3;

// Memory Threshold Defaults
pub const DEFAULT_MEMORY_SPIKE_THRESHOLD_BYTES: u64 = 256 * 1024 * 1024; // 256 MB
pub const DEFAULT_MAX_LEAK_RATE_BYTES_PER_SEC: f64 = 10.0 * 1024.0 * 1024.0; // 10 MB/s
pub const DEFAULT_MAX_TOTAL_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GB
pub const DEFAULT_MONOTONIC_LEAK_SAMPLES: usize = 5;

// Code Generation Cycle Detection Defaults
pub const DEFAULT_MAX_RECURSION_DEPTH: usize = 12;
pub const DEFAULT_MAX_CYCLE_PERIOD: usize = 8;
pub const DEFAULT_CYCLE_HISTORY_CAPACITY: usize = 50;

// PID Controller Defaults
pub const DEFAULT_PID_KP: f64 = 2.0;
pub const DEFAULT_PID_KI: f64 = 0.5;
pub const DEFAULT_PID_KD: f64 = 1.0;
pub const DEFAULT_PID_SETPOINT: f64 = 0.05;
pub const DEFAULT_PID_IMIN: f64 = -2.0;
pub const DEFAULT_PID_IMAX: f64 = 2.0;

// ==============================================================================
// 2. Circuit Breaker States & Domain Models
// ==============================================================================

/// Full Hysteresis Circuit Breaker State Machine
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// Normal execution, mutations authorized.
    Closed,
    /// Local transient trip (single node); pending cluster quorum agreement.
    LocalOpen,
    /// Cluster-wide open trip endorsed by >= 2 adjacent mesh peers.
    GlobalOpen,
    /// Blocked, mutations forbidden, exponential backoff active.
    Open,
    /// Trial probing state allowing controlled test invocations.
    HalfOpen,
    /// Probation state requiring N consecutive successful executions before returning to Closed.
    Recovery,
    /// Permanently tripped due to repeated identical AST failures.
    PermTripped,
    /// Cryptographically locked permanent trip due to flapping, memory leak, or cycle runaway.
    PermTrippedCryptolocked,
}

impl CircuitState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Closed => "CLOSED",
            Self::LocalOpen => "LOCAL_OPEN",
            Self::GlobalOpen => "GLOBAL_OPEN",
            Self::Open => "OPEN",
            Self::HalfOpen => "HALF_OPEN",
            Self::Recovery => "RECOVERY",
            Self::PermTripped => "PERM_TRIPPED",
            Self::PermTrippedCryptolocked => "PERM_TRIPPED_CRYPTOLOCKED",
        }
    }

    pub fn from_str_lenient(s: &str) -> Self {
        let norm = s.trim().to_uppercase().replace('-', "_");
        match norm.as_str() {
            "CLOSED" | "HEALTHY" => Self::Closed,
            "LOCAL_OPEN" | "LOCAL_TRIPPED" => Self::LocalOpen,
            "GLOBAL_OPEN" | "GLOBAL_TRIPPED" | "CLUSTER_OPEN" => Self::GlobalOpen,
            "OPEN" | "TRIPPED" | "QUARANTINED" => Self::Open,
            "HALF_OPEN" | "HALFOPEN" | "PROBING" => Self::HalfOpen,
            "RECOVERY" | "PROBATION" => Self::Recovery,
            "PERM_TRIPPED" | "PERMANENTLY_TRIPPED" | "FATAL" => Self::PermTripped,
            "PERM_TRIPPED_CRYPTOLOCKED" | "CRYPTOLOCKED" | "PERM_CRYPTOLOCKED" => {
                Self::PermTrippedCryptolocked
            }
            _ => Self::Closed,
        }
    }

    pub fn is_blocked(&self) -> bool {
        matches!(
            self,
            Self::Open
                | Self::LocalOpen
                | Self::GlobalOpen
                | Self::PermTripped
                | Self::PermTrippedCryptolocked
        )
    }

    pub fn allows_trial_probe(&self) -> bool {
        matches!(self, Self::HalfOpen | Self::Recovery)
    }
}

// ==============================================================================
// 3. Memory Spike & Leak Rate Supervision Subsystem
// ==============================================================================

/// Point-in-time snapshot of process/actor memory consumption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReading {
    pub timestamp_secs: f64,
    pub rss_bytes: u64,
    pub heap_bytes: u64,
    pub allocation_count: u64,
}

/// Nature of memory limit breach
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MemoryBreachKind {
    None,
    InstantaneousSpike { delta_bytes: u64, threshold_bytes: u64 },
    LeakRateExceeded { rate_bytes_per_sec: f64, threshold_rate: f64 },
    MonotonicRunawayLeak { consecutive_leaks: usize, total_growth_bytes: u64 },
    HardMemoryCeilingBreached { current_bytes: u64, max_ceiling_bytes: u64 },
}

/// Configuration parameters for memory health supervision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryThresholdConfig {
    pub memory_spike_threshold_bytes: u64,
    pub max_leak_rate_bytes_per_sec: f64,
    pub max_total_memory_bytes: u64,
    pub monotonic_leak_sample_count: usize,
    pub history_capacity: usize,
}

impl Default for MemoryThresholdConfig {
    fn default() -> Self {
        Self {
            memory_spike_threshold_bytes: DEFAULT_MEMORY_SPIKE_THRESHOLD_BYTES,
            max_leak_rate_bytes_per_sec: DEFAULT_MAX_LEAK_RATE_BYTES_PER_SEC,
            max_total_memory_bytes: DEFAULT_MAX_TOTAL_MEMORY_BYTES,
            monotonic_leak_sample_count: DEFAULT_MONOTONIC_LEAK_SAMPLES,
            history_capacity: 20,
        }
    }
}

/// Tracks rolling memory metrics and computes leak rates & instantaneous spikes
#[derive(Debug, Clone)]
pub struct MemoryTracker {
    config: MemoryThresholdConfig,
    history: VecDeque<MemoryReading>,
}

impl MemoryTracker {
    pub fn new(config: MemoryThresholdConfig) -> Self {
        let cap = config.history_capacity.max(5);
        Self {
            config,
            history: VecDeque::with_capacity(cap),
        }
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }

    pub fn record_reading(
        &mut self,
        rss_bytes: u64,
        heap_bytes: u64,
        allocation_count: u64,
        timestamp_secs: f64,
    ) -> MemoryBreachKind {
        let reading = MemoryReading {
            timestamp_secs,
            rss_bytes,
            heap_bytes,
            allocation_count,
        };

        // 1. Check Hard Memory Ceiling
        let total_reported = rss_bytes.max(heap_bytes);
        if total_reported > self.config.max_total_memory_bytes {
            self.push_reading(reading);
            return MemoryBreachKind::HardMemoryCeilingBreached {
                current_bytes: total_reported,
                max_ceiling_bytes: self.config.max_total_memory_bytes,
            };
        }

        // 2. Check Instantaneous Spike against previous reading
        if let Some(prev) = self.history.back() {
            if reading.rss_bytes > prev.rss_bytes {
                let spike = reading.rss_bytes - prev.rss_bytes;
                if spike >= self.config.memory_spike_threshold_bytes {
                    self.push_reading(reading);
                    return MemoryBreachKind::InstantaneousSpike {
                        delta_bytes: spike,
                        threshold_bytes: self.config.memory_spike_threshold_bytes,
                    };
                }
            }
        }

        self.push_reading(reading);

        // 3. Check Monotonic Leak Growth across consecutive samples
        if self.history.len() >= self.config.monotonic_leak_sample_count {
            let samples: Vec<&MemoryReading> = self
                .history
                .iter()
                .rev()
                .take(self.config.monotonic_leak_sample_count)
                .collect();
            // samples are in reverse order: [newest, ..., oldest]
            let mut monotonic = true;
            for i in 0..(samples.len() - 1) {
                if samples[i].rss_bytes <= samples[i + 1].rss_bytes {
                    monotonic = false;
                    break;
                }
            }
            if monotonic {
                let newest = samples.first().unwrap().rss_bytes;
                let oldest = samples.last().unwrap().rss_bytes;
                let growth = newest.saturating_sub(oldest);
                if growth >= (self.config.memory_spike_threshold_bytes / 2) {
                    return MemoryBreachKind::MonotonicRunawayLeak {
                        consecutive_leaks: self.config.monotonic_leak_sample_count,
                        total_growth_bytes: growth,
                    };
                }
            }
        }

        // 4. Check Leak Rate Limit (bytes/sec) over available window
        if self.history.len() >= 3 {
            let oldest = self.history.front().unwrap();
            let newest = self.history.back().unwrap();
            let dt = newest.timestamp_secs - oldest.timestamp_secs;
            if dt > 0.5 {
                let growth = newest.rss_bytes.saturating_sub(oldest.rss_bytes) as f64;
                let rate = growth / dt;
                if rate > self.config.max_leak_rate_bytes_per_sec {
                    return MemoryBreachKind::LeakRateExceeded {
                        rate_bytes_per_sec: rate,
                        threshold_rate: self.config.max_leak_rate_bytes_per_sec,
                    };
                }
            }
        }

        MemoryBreachKind::None
    }

    fn push_reading(&mut self, reading: MemoryReading) {
        if self.history.len() >= self.config.history_capacity {
            self.history.pop_front();
        }
        self.history.push_back(reading);
    }

    pub fn current_leak_rate(&self) -> f64 {
        if self.history.len() < 2 {
            return 0.0;
        }
        let oldest = self.history.front().unwrap();
        let newest = self.history.back().unwrap();
        let dt = newest.timestamp_secs - oldest.timestamp_secs;
        if dt <= 0.001 {
            return 0.0;
        }
        let delta = newest.rss_bytes.saturating_sub(oldest.rss_bytes) as f64;
        delta / dt
    }

    pub fn latest_reading(&self) -> Option<&MemoryReading> {
        self.history.back()
    }
}

// ==============================================================================
// 4. Sliding-Window Anomaly Detection Subsystem
// ==============================================================================

/// Rolling execution event record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub timestamp_secs: f64,
    pub success: bool,
    pub error_fingerprint: Option<String>,
    pub duration_ms: f64,
}

/// Anomaly metrics evaluated over sliding window
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyMetrics {
    pub total_events: usize,
    pub failure_count: usize,
    pub success_count: usize,
    pub failure_rate: f64,
    pub failure_rate_pct: f64,
    pub anomaly_score: f64,
    pub is_anomaly: bool,
}

/// Sliding-window anomaly detector tracking rolling failure rate and confidence score
#[derive(Debug, Clone)]
pub struct SlidingWindowAnomalyDetector {
    window_size: usize,
    window_duration_secs: f64,
    min_events: usize,
    anomaly_threshold: f64,
    events: VecDeque<ExecutionEvent>,
}

impl SlidingWindowAnomalyDetector {
    pub fn new(
        window_size: usize,
        window_duration_secs: f64,
        min_events: usize,
        anomaly_threshold: f64,
    ) -> Self {
        Self {
            window_size: window_size.max(1),
            window_duration_secs: window_duration_secs.max(1.0),
            min_events: min_events.max(1),
            anomaly_threshold: anomaly_threshold.clamp(0.01, 1.0),
            events: VecDeque::with_capacity(window_size),
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn record_event(
        &mut self,
        success: bool,
        error_fingerprint: Option<String>,
        duration_ms: f64,
        timestamp_secs: f64,
    ) -> AnomalyMetrics {
        let event = ExecutionEvent {
            timestamp_secs,
            success,
            error_fingerprint,
            duration_ms,
        };
        self.events.push_back(event);
        self.prune(timestamp_secs);
        self.compute_metrics(timestamp_secs)
    }

    pub fn prune(&mut self, current_time_secs: f64) {
        let cutoff = current_time_secs - self.window_duration_secs;
        while let Some(front) = self.events.front() {
            if front.timestamp_secs < cutoff {
                self.events.pop_front();
            } else {
                break;
            }
        }
        while self.events.len() > self.window_size {
            self.events.pop_front();
        }
    }

    pub fn compute_metrics(&self, current_time_secs: f64) -> AnomalyMetrics {
        let cutoff = current_time_secs - self.window_duration_secs;
        let valid_events: Vec<&ExecutionEvent> = self
            .events
            .iter()
            .filter(|e| e.timestamp_secs >= cutoff)
            .collect();

        let total_events = valid_events.len();
        if total_events == 0 {
            return AnomalyMetrics {
                total_events: 0,
                failure_count: 0,
                success_count: 0,
                failure_rate: 0.0,
                failure_rate_pct: 0.0,
                anomaly_score: 0.0,
                is_anomaly: false,
            };
        }

        let failure_count = valid_events.iter().filter(|e| !e.success).count();
        let success_count = total_events - failure_count;
        let failure_rate = failure_count as f64 / total_events as f64;
        let failure_rate_pct = (failure_rate * 100.0 * 100.0).round() / 100.0;

        // Sample confidence factor
        let confidence = (total_events as f64 / self.min_events as f64).min(1.0);
        let anomaly_score = ((failure_rate * confidence) * 10000.0).round() / 10000.0;

        let is_anomaly =
            total_events >= self.min_events && failure_rate >= self.anomaly_threshold;

        AnomalyMetrics {
            total_events,
            failure_count,
            success_count,
            failure_rate,
            failure_rate_pct,
            anomaly_score,
            is_anomaly,
        }
    }

    pub fn events(&self) -> &VecDeque<ExecutionEvent> {
        &self.events
    }
}

// ==============================================================================
// 5. Flapping Error Oscillation Detection Subsystem
// ==============================================================================

/// Nature of detected compiler / runtime flapping error oscillation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlappingPattern {
    None,
    ConsecutiveIdenticalErrors {
        fingerprint: String,
        count: usize,
    },
    TwoCycleOscillation {
        signature_a: String,
        signature_b: String,
    },
    ThreeCycleOscillation {
        signatures: Vec<String>,
    },
    HighFrequencyTransitions {
        transitions: usize,
        sample_count: usize,
        signatures: Vec<String>,
    },
}

/// Flapping detection engine identifying oscillations between compilation errors
#[derive(Debug, Clone)]
pub struct FlappingDetector {
    consecutive_identical_threshold: usize,
    history: VecDeque<String>,
    capacity: usize,
}

impl Default for FlappingDetector {
    fn default() -> Self {
        Self::new(3, 50)
    }
}

impl FlappingDetector {
    pub fn new(consecutive_identical_threshold: usize, capacity: usize) -> Self {
        Self {
            consecutive_identical_threshold: consecutive_identical_threshold.max(2),
            history: VecDeque::with_capacity(capacity.max(10)),
            capacity: capacity.max(10),
        }
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn record_error(&mut self, fingerprint: &str) -> (bool, FlappingPattern, String) {
        if self.history.len() >= self.capacity {
            self.history.pop_front();
        }
        self.history.push_back(fingerprint.to_string());
        let hist_slice: Vec<String> = self.history.iter().cloned().collect();
        Self::detect_flapping(&hist_slice, self.consecutive_identical_threshold)
    }

    pub fn detect_flapping(
        history: &[String],
        consecutive_identical_threshold: usize,
    ) -> (bool, FlappingPattern, String) {
        let valid: Vec<&String> = history.iter().filter(|s| !s.trim().is_empty()).collect();
        if valid.len() < 2 {
            return (false, FlappingPattern::None, "Insufficient history".into());
        }

        // Check 1: N Consecutive identical errors [A, A, A]
        if valid.len() >= consecutive_identical_threshold {
            let tail = &valid[valid.len() - consecutive_identical_threshold..];
            let first = tail[0];
            if tail.iter().all(|item| *item == first) {
                let fp_short = if first.len() > 12 { &first[..12] } else { first.as_str() };
                return (
                    true,
                    FlappingPattern::ConsecutiveIdenticalErrors {
                        fingerprint: (*first).clone(),
                        count: consecutive_identical_threshold,
                    },
                    format!(
                        "Flapping detected: {} consecutive identical AST compiler errors ({})",
                        consecutive_identical_threshold, fp_short
                    ),
                );
            }
        }

        // Check 2: 2-cycle oscillation [A, B, A] (3 items)
        if valid.len() >= 3 {
            let tail3 = &valid[valid.len() - 3..];
            if tail3[0] == tail3[2] && tail3[0] != tail3[1] {
                let a_short = if tail3[0].len() > 8 { &tail3[0][..8] } else { tail3[0] };
                let b_short = if tail3[1].len() > 8 { &tail3[1][..8] } else { tail3[1] };
                return (
                    true,
                    FlappingPattern::TwoCycleOscillation {
                        signature_a: (*tail3[0]).clone(),
                        signature_b: (*tail3[1]).clone(),
                    },
                    format!(
                        "Flapping detected (2-cycle oscillation between error fingerprints: {} <-> {})",
                        a_short, b_short
                    ),
                );
            }
        }

        // Check 3: Strict 4-item 2-cycle [A, B, A, B]
        if valid.len() >= 4 {
            let tail4 = &valid[valid.len() - 4..];
            if tail4[0] == tail4[2] && tail4[1] == tail4[3] && tail4[0] != tail4[1] {
                let a_short = if tail4[0].len() > 8 { &tail4[0][..8] } else { tail4[0] };
                let b_short = if tail4[1].len() > 8 { &tail4[1][..8] } else { tail4[1] };
                return (
                    true,
                    FlappingPattern::TwoCycleOscillation {
                        signature_a: (*tail4[0]).clone(),
                        signature_b: (*tail4[1]).clone(),
                    },
                    format!(
                        "Strict 2-cycle flapping oscillation detected: {} <-> {}",
                        a_short, b_short
                    ),
                );
            }
        }

        // Check 4: 3-cycle oscillation [A, B, C, A]
        if valid.len() >= 4 {
            let tail4 = &valid[valid.len() - 4..];
            if tail4[0] == tail4[3] && tail4[0] != tail4[1] && tail4[1] != tail4[2] && tail4[0] != tail4[2] {
                let sigs = vec![(*tail4[0]).clone(), (*tail4[1]).clone(), (*tail4[2]).clone()];
                return (
                    true,
                    FlappingPattern::ThreeCycleOscillation { signatures: sigs },
                    "3-cycle compilation error oscillation detected (A -> B -> C -> A)".into(),
                );
            }
        }

        // Check 5: High-frequency transitions in 5-sample window
        if valid.len() >= 5 {
            let tail5 = &valid[valid.len() - 5..];
            let mut transitions = 0;
            for i in 1..tail5.len() {
                if tail5[i] != tail5[i - 1] {
                    transitions += 1;
                }
            }
            let unique: HashSet<&String> = tail5.iter().cloned().collect();
            if transitions >= 3 && unique.len() >= 2 {
                let sigs: Vec<String> = unique.into_iter().cloned().collect();
                return (
                    true,
                    FlappingPattern::HighFrequencyTransitions {
                        transitions,
                        sample_count: 5,
                        signatures: sigs.clone(),
                    },
                    format!(
                        "High-frequency error oscillation ({} transitions over 5 attempts) between {} error signatures",
                        transitions, sigs.len()
                    ),
                );
            }
        }

        (false, FlappingPattern::None, "No flapping detected".into())
    }
}

// ==============================================================================
// 6. Infinite Recursion & Cycle Detection in Code Generation Loops
// ==============================================================================

/// Nature of cycle / recursion loop pattern detected in code generation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CyclePatternKind {
    None,
    DirectOscillation { state_a: String, state_b: String },
    PeriodicCycle { period: usize, cycle_hashes: Vec<String> },
    ZeroProgressStall { state_hash: String, count: usize },
    RecursionDepthExceeded { current_depth: usize, max_depth: usize },
}

/// Evaluation report from code generation cycle detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleDetectionResult {
    pub is_cycle_detected: bool,
    pub pattern: CyclePatternKind,
    pub loop_description: String,
    pub rollback_target_turn: Option<usize>,
    pub pivot_prescription: String,
}

/// Snapshot of an agent code mutation turn
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeTurnSnapshot {
    pub turn_number: usize,
    pub state_hash: String,
    pub intent: String,
    pub recursion_depth: usize,
    pub timestamp_secs: f64,
}

/// Detects cyclic deadlocks, repeating diffs, and infinite recursion in autonomous coding
#[derive(Debug, Clone)]
pub struct RecursionAndCycleDetector {
    max_history: usize,
    max_recursion_depth: usize,
    max_cycle_period: usize,
    history: Vec<CodeTurnSnapshot>,
}

impl Default for RecursionAndCycleDetector {
    fn default() -> Self {
        Self::new(
            DEFAULT_CYCLE_HISTORY_CAPACITY,
            DEFAULT_MAX_RECURSION_DEPTH,
            DEFAULT_MAX_CYCLE_PERIOD,
        )
    }
}

impl RecursionAndCycleDetector {
    pub fn new(max_history: usize, max_recursion_depth: usize, max_cycle_period: usize) -> Self {
        Self {
            max_history: max_history.max(10),
            max_recursion_depth: max_recursion_depth.max(2),
            max_cycle_period: max_cycle_period.max(2),
            history: Vec::with_capacity(max_history),
        }
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }

    pub fn record_turn(
        &mut self,
        turn_number: usize,
        state_hash: &str,
        intent: &str,
        recursion_depth: usize,
        timestamp_secs: f64,
    ) -> CycleDetectionResult {
        // 1. Check Recursion Depth Ceiling
        if recursion_depth >= self.max_recursion_depth {
            return CycleDetectionResult {
                is_cycle_detected: true,
                pattern: CyclePatternKind::RecursionDepthExceeded {
                    current_depth: recursion_depth,
                    max_depth: self.max_recursion_depth,
                },
                loop_description: format!(
                    "Maximum autonomous recursion depth breached: {} >= {}",
                    recursion_depth, self.max_recursion_depth
                ),
                rollback_target_turn: self.history.first().map(|s| s.turn_number),
                pivot_prescription: "HALT_RECURSIVE_EXPANSION: Recursion depth limit breached. Escalate to human operator for architectural clarification.".into(),
            };
        }

        let snapshot = CodeTurnSnapshot {
            turn_number,
            state_hash: state_hash.to_string(),
            intent: intent.to_string(),
            recursion_depth,
            timestamp_secs,
        };

        if self.history.len() >= self.max_history {
            self.history.remove(0);
        }
        self.history.push(snapshot);

        self.evaluate_cycles()
    }

    pub fn evaluate_cycles(&self) -> CycleDetectionResult {
        let n = self.history.len();
        if n < 2 {
            return CycleDetectionResult {
                is_cycle_detected: false,
                pattern: CyclePatternKind::None,
                loop_description: "Healthy progression".into(),
                rollback_target_turn: None,
                pivot_prescription: String::new(),
            };
        }

        let current = &self.history[n - 1];

        // 1. Zero-progress stall: 3 consecutive turns produced identical state hash
        if n >= 3 {
            let tail3 = &self.history[n - 3..];
            if tail3[0].state_hash == tail3[1].state_hash
                && tail3[1].state_hash == tail3[2].state_hash
            {
                return CycleDetectionResult {
                    is_cycle_detected: true,
                    pattern: CyclePatternKind::ZeroProgressStall {
                        state_hash: current.state_hash.clone(),
                        count: 3,
                    },
                    loop_description: "Zero progress stall: 3 consecutive turns generated identical codebase hash without modification.".into(),
                    rollback_target_turn: Some(tail3[0].turn_number),
                    pivot_prescription: "PIVOT_RETRY_STRATEGY: Previous patch generator produced no-ops. Switch to alternative AST transformation or rewrite test hypothesis.".into(),
                };
            }
        }

        // 2. Direct 2-cycle oscillation: Hash A -> Hash B -> Hash A
        if n >= 3 {
            let s_prev2 = &self.history[n - 3];
            let s_prev1 = &self.history[n - 2];
            if current.state_hash == s_prev2.state_hash && current.state_hash != s_prev1.state_hash {
                return CycleDetectionResult {
                    is_cycle_detected: true,
                    pattern: CyclePatternKind::DirectOscillation {
                        state_a: current.state_hash.clone(),
                        state_b: s_prev1.state_hash.clone(),
                    },
                    loop_description: format!(
                        "Direct 2-cycle oscillation detected: State reverted to turn #{} (Hash: {})",
                        s_prev2.turn_number,
                        &current.state_hash[..8.min(current.state_hash.len())]
                    ),
                    rollback_target_turn: Some(s_prev2.turn_number),
                    pivot_prescription: "BREAK_OSCILLATION: Agent is undoing previous change. Invalidate cached assumption and force alternate branch.".into(),
                };
            }
        }

        // 3. Periodic cycle detection (period K from 3 to max_cycle_period)
        let max_k = self.max_cycle_period.min(n / 2);
        for k in 3..=max_k {
            let mut is_periodic = true;
            for i in 0..k {
                if self.history[n - 1 - i].state_hash != self.history[n - 1 - k - i].state_hash {
                    is_periodic = false;
                    break;
                }
            }
            if is_periodic {
                let cycle_hashes: Vec<String> = self.history[n - k..n]
                    .iter()
                    .map(|s| s.state_hash.clone())
                    .collect();
                let rollback_turn = self.history[n - 2 * k].turn_number;
                return CycleDetectionResult {
                    is_cycle_detected: true,
                    pattern: CyclePatternKind::PeriodicCycle {
                        period: k,
                        cycle_hashes,
                    },
                    loop_description: format!(
                        "Periodic cycle detected with period {} across {} turns",
                        k,
                        2 * k
                    ),
                    rollback_target_turn: Some(rollback_turn),
                    pivot_prescription: format!(
                        "ROLLBACK_AND_PIVOT: Cyclic loop of length {} detected. Rollback to turn #{} and synthesize alternative patch.",
                        k, rollback_turn
                    ),
                };
            }
        }

        CycleDetectionResult {
            is_cycle_detected: false,
            pattern: CyclePatternKind::None,
            loop_description: "Healthy progression".into(),
            rollback_target_turn: None,
            pivot_prescription: String::new(),
        }
    }
}

// ==============================================================================
// 7. Adaptive PID Controller Subsystem
// ==============================================================================

/// PID Controller configuration parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidConfig {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub setpoint: f64,
    pub i_min: f64,
    pub i_max: f64,
}

impl Default for PidConfig {
    fn default() -> Self {
        Self {
            kp: DEFAULT_PID_KP,
            ki: DEFAULT_PID_KI,
            kd: DEFAULT_PID_KD,
            setpoint: DEFAULT_PID_SETPOINT,
            i_min: DEFAULT_PID_IMIN,
            i_max: DEFAULT_PID_IMAX,
        }
    }
}

/// Adaptive PID Controller for modulating trip sensitivity and backoff smoothing
#[derive(Debug, Clone)]
pub struct PidController {
    config: PidConfig,
    integral: f64,
    prev_error: f64,
    last_time_secs: Option<f64>,
    output: f64,
}

impl Default for PidController {
    fn default() -> Self {
        Self::new(PidConfig::default())
    }
}

impl PidController {
    pub fn new(config: PidConfig) -> Self {
        Self {
            config,
            integral: 0.0,
            prev_error: 0.0,
            last_time_secs: None,
            output: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
        self.last_time_secs = None;
        self.output = 0.0;
    }

    pub fn update(&mut self, current_value: f64, current_time_secs: f64) -> f64 {
        let error = current_value - self.config.setpoint;
        let dt = match self.last_time_secs {
            None => 1.0,
            Some(t) => (current_time_secs - t).max(0.001),
        };

        let p_term = self.config.kp * error;

        // Anti-windup clamped integration
        self.integral += self.config.ki * error * dt;
        self.integral = self.integral.clamp(self.config.i_min, self.config.i_max);
        let i_term = self.integral;

        let d_term = self.config.kd * (error - self.prev_error) / dt;

        self.output = p_term + i_term + d_term;
        self.prev_error = error;
        self.last_time_secs = Some(current_time_secs);

        self.output
    }

    pub fn output(&self) -> f64 {
        self.output
    }

    /// Computes adaptive backoff delay modulated by PID controller output
    pub fn calculate_adaptive_backoff(
        consecutive_failures: usize,
        pid_output: f64,
        t0: f64,
        t_max: f64,
        jitter_factor: f64,
    ) -> f64 {
        let f_clamped = consecutive_failures.clamp(1, 10);
        let base_exp = t0 * (2.0f64.powi((f_clamped - 1) as i32));
        let factor = (1.0 + pid_output.max(0.0)).max(0.5);
        let nominal = (base_exp * factor).min(t_max);
        let jittered = nominal + jitter_factor.clamp(0.0, 1.0);
        jittered.min(t_max).max(t0)
    }

    /// Calculates n-th Fibonacci number
    pub fn fibonacci(n: usize) -> u64 {
        match n {
            0 => 0,
            1 | 2 => 1,
            _ => {
                let mut a = 1u64;
                let mut b = 1u64;
                for _ in 3..=n {
                    let c = a.saturating_add(b);
                    a = b;
                    b = c;
                }
                b
            }
        }
    }

    /// Strict Fibonacci backoff: delay = min(t_max, t0 * fib(iteration)) + jitter
    pub fn calculate_fibonacci_backoff(
        iteration: usize,
        t0: f64,
        t_max: f64,
        jitter: f64,
    ) -> f64 {
        let it = iteration.max(1);
        let fib = Self::fibonacci(it) as f64;
        let base = (t0 * fib).min(t_max);
        (base + jitter.clamp(0.0, 1.0)).min(t_max)
    }
}

// ==============================================================================
// 8. Cryptographic State Signing & Proof Receipts Subsystem
// ==============================================================================

/// Immutable Ed25519-signed lockout receipt issued upon escalation to PERM_TRIPPED_CRYPTOLOCKED
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptographicLockoutReceipt {
    pub lockout_id: String,
    pub skill_id: String,
    pub circuit_state: String,
    pub reason: String,
    pub trigger_pattern: String,
    pub consecutive_count: usize,
    pub blake3_fingerprint: String,
    pub state_history_hash: String,
    pub ed25519_public_key: String,
    pub ed25519_signature: String,
    pub locked_paths: Vec<String>,
    pub timestamp_iso: String,
    pub timestamp_unix: f64,
    pub mcp_unlock_required: bool,
    pub mcp_tool_name: String,
}

/// Persistent Ed25519 signing keypair for circuit breaker governor
#[derive(Debug)]
pub struct GovernorKeypair {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl GovernorKeypair {
    pub fn generate_deterministic(seed_bytes: &[u8]) -> Self {
        let hash = blake3::hash(seed_bytes);
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(hash.as_bytes());
        let signing_key = SigningKey::from_bytes(&key_bytes);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.to_bytes())
    }

    pub fn sign_payload(&self, payload: &[u8]) -> String {
        let sig: Signature = self.signing_key.sign(payload);
        hex::encode(sig.to_bytes())
    }

    pub fn verify_signature(
        public_key_hex: &str,
        payload: &[u8],
        sig_hex: &str,
    ) -> Result<bool> {
        let pub_bytes = hex::decode(public_key_hex)
            .map_err(|e| HgbError::Security(format!("Invalid public key hex: {}", e)))?;
        let sig_bytes = hex::decode(sig_hex)
            .map_err(|e| HgbError::Security(format!("Invalid signature hex: {}", e)))?;

        let pub_arr: [u8; 32] = pub_bytes
            .try_into()
            .map_err(|_| HgbError::Security("Public key must be 32 bytes".into()))?;
        let sig_arr: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| HgbError::Security("Signature must be 64 bytes".into()))?;

        let verifier = VerifyingKey::from_bytes(&pub_arr)
            .map_err(|e| HgbError::Security(format!("Malformed Ed25519 verifying key: {}", e)))?;
        let sig = Signature::from_bytes(&sig_arr);

        match verifier.verify(payload, &sig) {
            Ok(_) => Ok(true),
            Err(e) => Err(HgbError::Security(format!(
                "Ed25519 signature verification failed: {}",
                e
            ))),
        }
    }
}

/// Request to cryptographically unlock a circuit breaker via MCP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpUnlockRequest {
    pub skill_id: String,
    pub operator_id: String,
    pub lockout_id: String,
    pub nonce: u64,
    pub ed25519_signature: String,
    pub operator_public_key: String,
    pub restore_permissions: bool,
}

/// Outcome of authorized MCP operator unlock
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpUnlockReceipt {
    pub success: bool,
    pub skill_id: String,
    pub operator_id: String,
    pub lockout_id: String,
    pub restored_state: CircuitState,
    pub nonce: u64,
    pub unlocked_at_iso: String,
    pub restored_paths: Vec<String>,
    pub message: String,
}

// ==============================================================================
// 9. Comprehensive Circuit Breaker Configuration & Record
// ==============================================================================

/// Full configuration for an adaptive circuit breaker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub max_consecutive_failures: usize,
    pub t0_secs: f64,
    pub t_max_secs: f64,
    pub window_size: usize,
    pub window_duration_secs: f64,
    pub min_events_anomaly: usize,
    pub anomaly_threshold: f64,
    pub recovery_threshold: usize,
    pub memory: MemoryThresholdConfig,
    pub pid: PidConfig,
    pub max_recursion_depth: usize,
    pub max_cycle_period: usize,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: DEFAULT_MAX_CONSECUTIVE_FAILURES,
            t0_secs: DEFAULT_T0_SECS,
            t_max_secs: DEFAULT_TMAX_SECS,
            window_size: DEFAULT_WINDOW_SIZE,
            window_duration_secs: DEFAULT_WINDOW_DURATION_SECS,
            min_events_anomaly: DEFAULT_MIN_EVENTS_ANOMALY,
            anomaly_threshold: DEFAULT_ANOMALY_THRESHOLD,
            recovery_threshold: DEFAULT_RECOVERY_THRESHOLD,
            memory: MemoryThresholdConfig::default(),
            pid: PidConfig::default(),
            max_recursion_depth: DEFAULT_MAX_RECURSION_DEPTH,
            max_cycle_period: DEFAULT_MAX_CYCLE_PERIOD,
        }
    }
}

/// Internal state tracked per skill / component
#[derive(Debug)]
pub struct SkillGovernorState {
    pub skill_id: String,
    pub state: CircuitState,
    pub consecutive_failures: usize,
    pub consecutive_successes: usize,
    pub total_failures: usize,
    pub total_successes: usize,
    pub open_until_timestamp: f64,
    pub last_state_change_timestamp: f64,
    pub anomaly_score: f64,
    pub failure_rate: f64,
    pub anomaly_detector: SlidingWindowAnomalyDetector,
    pub flapping_detector: FlappingDetector,
    pub memory_tracker: MemoryTracker,
    pub cycle_detector: RecursionAndCycleDetector,
    pub pid_controller: PidController,
    pub active_lockout: Option<CryptographicLockoutReceipt>,
}

impl SkillGovernorState {
    pub fn new(skill_id: &str, config: &CircuitBreakerConfig) -> Self {
        Self {
            skill_id: skill_id.to_string(),
            state: CircuitState::Closed,
            consecutive_failures: 0,
            consecutive_successes: 0,
            total_failures: 0,
            total_successes: 0,
            open_until_timestamp: 0.0,
            last_state_change_timestamp: 0.0,
            anomaly_score: 0.0,
            failure_rate: 0.0,
            anomaly_detector: SlidingWindowAnomalyDetector::new(
                config.window_size,
                config.window_duration_secs,
                config.min_events_anomaly,
                config.anomaly_threshold,
            ),
            flapping_detector: FlappingDetector::new(config.max_consecutive_failures, 50),
            memory_tracker: MemoryTracker::new(config.memory.clone()),
            cycle_detector: RecursionAndCycleDetector::new(
                50,
                config.max_recursion_depth,
                config.max_cycle_period,
            ),
            pid_controller: PidController::new(config.pid.clone()),
            active_lockout: None,
        }
    }
}

// ==============================================================================
// 10. Specification & Outcome for Execution Attempts
// ==============================================================================

/// Specification describing an execution or code mutation attempt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionAttemptSpec {
    pub skill_id: String,
    pub success: bool,
    pub error_trace: Option<String>,
    pub error_fingerprint: Option<String>,
    pub target_paths: Vec<String>,
    pub duration_ms: f64,
    pub turn_number: Option<usize>,
    pub state_hash: Option<String>,
    pub turn_intent: Option<String>,
    pub recursion_depth: Option<usize>,
    pub current_time_secs: Option<f64>,
}

/// Comprehensive outcome of recording an execution attempt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionAttemptOutcome {
    pub skill_id: String,
    pub success: bool,
    pub state: CircuitState,
    pub consecutive_failures: usize,
    pub consecutive_successes: usize,
    pub anomaly_metrics: AnomalyMetrics,
    pub backoff_delay_secs: f64,
    pub is_tripped: bool,
    pub trip_reason: Option<String>,
    pub lockout_receipt: Option<CryptographicLockoutReceipt>,
    pub cycle_result: Option<CycleDetectionResult>,
}

// ==============================================================================
// 11. Integrated Adaptive PID Circuit Breaker Governor
// ==============================================================================

/// Primary Circuit Breaker & Hysteresis Governor
pub struct AdaptivePidCircuitBreaker {
    config: CircuitBreakerConfig,
    keypair: GovernorKeypair,
    consumed_nonces: Mutex<HashSet<u64>>,
    skills: RwLock<HashMap<String, Arc<Mutex<SkillGovernorState>>>>,
}

impl AdaptivePidCircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        let seed = b"hagibis-governor-adaptive-circuit-breaker-v2";
        let keypair = GovernorKeypair::generate_deterministic(seed);
        Self {
            config,
            keypair,
            consumed_nonces: Mutex::new(HashSet::new()),
            skills: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_seed(config: CircuitBreakerConfig, seed: &[u8]) -> Self {
        let keypair = GovernorKeypair::generate_deterministic(seed);
        Self {
            config,
            keypair,
            consumed_nonces: Mutex::new(HashSet::new()),
            skills: RwLock::new(HashMap::new()),
        }
    }

    pub fn governor_public_key(&self) -> String {
        self.keypair.public_key_hex()
    }

    fn get_or_create_skill(&self, skill_id: &str) -> Arc<Mutex<SkillGovernorState>> {
        {
            let r = self.skills.read().unwrap();
            if let Some(entry) = r.get(skill_id) {
                return Arc::clone(entry);
            }
        }
        let mut w = self.skills.write().unwrap();
        w.entry(skill_id.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(SkillGovernorState::new(
                    skill_id,
                    &self.config,
                )))
            })
            .clone()
    }

    // --------------------------------------------------------------------------
    // Eligibility Check with Hysteresis Backoff Expiration
    // --------------------------------------------------------------------------
    pub fn check_mutation_eligibility(
        &self,
        skill_id: &str,
        current_time_secs: f64,
    ) -> (bool, CircuitState, String) {
        let skill_arc = self.get_or_create_skill(skill_id);
        let mut skill = skill_arc.lock().unwrap();

        match skill.state {
            CircuitState::PermTrippedCryptolocked => (
                false,
                CircuitState::PermTrippedCryptolocked,
                format!(
                    "BLOCKED: PERM_TRIPPED_CRYPTOLOCKED. Circuit breaker locked for [{}] (Lockout ID: {:?}). Authorized signed operator MCP unlock required.",
                    skill_id,
                    skill.active_lockout.as_ref().map(|r| &r.lockout_id)
                ),
            ),
            CircuitState::PermTripped => (
                false,
                CircuitState::PermTripped,
                format!(
                    "BLOCKED: PERM_TRIPPED. 3 consecutive identical AST failures for [{}]. Auto-mutation halted.",
                    skill_id
                ),
            ),
            CircuitState::Open | CircuitState::LocalOpen | CircuitState::GlobalOpen => {
                if skill.open_until_timestamp > 0.0
                    && current_time_secs >= skill.open_until_timestamp
                {
                    // Backoff expired -> Transition to HalfOpen for controlled probe
                    skill.state = CircuitState::HalfOpen;
                    skill.last_state_change_timestamp = current_time_secs;
                    (
                        true,
                        CircuitState::HalfOpen,
                        format!(
                            "Backoff timer expired for [{}]. State transitioned to HALF_OPEN. Trial probe permitted.",
                            skill_id
                        ),
                    )
                } else {
                    let remaining = (skill.open_until_timestamp - current_time_secs).max(0.0);
                    (
                        false,
                        skill.state,
                        format!(
                            "BLOCKED: Skill [{}] is in {} state. Backoff remaining: {:.2}s.",
                            skill_id,
                            skill.state.as_str(),
                            remaining
                        ),
                    )
                }
            }
            CircuitState::HalfOpen => (
                true,
                CircuitState::HalfOpen,
                format!("Skill [{}] is in HALF_OPEN trial probing state.", skill_id),
            ),
            CircuitState::Recovery => (
                true,
                CircuitState::Recovery,
                format!(
                    "Skill [{}] is in RECOVERY probation ({}/{} successes).",
                    skill_id, skill.consecutive_successes, self.config.recovery_threshold
                ),
            ),
            CircuitState::Closed => (
                true,
                CircuitState::Closed,
                format!("Skill [{}] is healthy and authorized for mutations.", skill_id),
            ),
        }
    }

    // --------------------------------------------------------------------------
    // Record Execution Attempt (Hysteresis, Anomaly, Flapping, Cycle Detection)
    // --------------------------------------------------------------------------
    pub fn record_execution_attempt(&self, spec: ExecutionAttemptSpec) -> ExecutionAttemptOutcome {
        let now = spec
            .current_time_secs
            .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64());
        let skill_arc = self.get_or_create_skill(&spec.skill_id);
        let mut skill = skill_arc.lock().unwrap();

        // 1. Check Code Generation Cycles if state hash is provided
        let mut cycle_res = None;
        if let (Some(turn), Some(hash)) = (spec.turn_number, spec.state_hash.as_ref()) {
            let res = skill.cycle_detector.record_turn(
                turn,
                hash,
                spec.turn_intent.as_deref().unwrap_or(""),
                spec.recursion_depth.unwrap_or(0),
                now,
            );
            if res.is_cycle_detected {
                // Trip to PermTrippedCryptolocked immediately!
                let lockout = self.issue_lockout(
                    &mut skill,
                    &spec.skill_id,
                    &res.loop_description,
                    "CodeGenerationCycleDetection",
                    vec![hash.clone()],
                    &spec.target_paths,
                    now,
                );
                return ExecutionAttemptOutcome {
                    skill_id: spec.skill_id.clone(),
                    success: false,
                    state: CircuitState::PermTrippedCryptolocked,
                    consecutive_failures: skill.consecutive_failures,
                    consecutive_successes: 0,
                    anomaly_metrics: skill.anomaly_detector.compute_metrics(now),
                    backoff_delay_secs: f64::INFINITY,
                    is_tripped: true,
                    trip_reason: Some(res.loop_description.clone()),
                    lockout_receipt: Some(lockout),
                    cycle_result: Some(res),
                };
            }
            cycle_res = Some(res);
        }

        // 2. Record Event in Sliding Window
        let anomaly_metrics = skill.anomaly_detector.record_event(
            spec.success,
            spec.error_fingerprint.clone(),
            spec.duration_ms,
            now,
        );
        skill.anomaly_score = anomaly_metrics.anomaly_score;
        skill.failure_rate = anomaly_metrics.failure_rate;

        // 3. Handle Failure Path
        if !spec.success {
            skill.consecutive_failures += 1;
            skill.consecutive_successes = 0;
            skill.total_failures += 1;

            let fp = spec
                .error_fingerprint
                .clone()
                .unwrap_or_else(|| "E_UNKNOWN".into());

            // Check Flapping Oscillation
            let (is_flapping, flapping_pat, flapping_reason) =
                skill.flapping_detector.record_error(&fp);

            if is_flapping {
                // Flapping detected -> escalate to PermTrippedCryptolocked!
                let lockout = self.issue_lockout(
                    &mut skill,
                    &spec.skill_id,
                    &flapping_reason,
                    &format!("{:?}", flapping_pat),
                    vec![fp],
                    &spec.target_paths,
                    now,
                );
                return ExecutionAttemptOutcome {
                    skill_id: spec.skill_id.clone(),
                    success: false,
                    state: CircuitState::PermTrippedCryptolocked,
                    consecutive_failures: skill.consecutive_failures,
                    consecutive_successes: 0,
                    anomaly_metrics,
                    backoff_delay_secs: f64::INFINITY,
                    is_tripped: true,
                    trip_reason: Some(flapping_reason),
                    lockout_receipt: Some(lockout),
                    cycle_result: cycle_res,
                };
            }

            // Check Consecutive Failures or Anomaly Spike -> Open Circuit Breaker
            let trip_condition = skill.consecutive_failures >= self.config.max_consecutive_failures
                || anomaly_metrics.is_anomaly;

            if trip_condition {
                let failure_rate = skill.failure_rate;
                let pid_out = skill.pid_controller.update(failure_rate, now);
                let backoff = PidController::calculate_adaptive_backoff(
                    skill.consecutive_failures,
                    pid_out,
                    self.config.t0_secs,
                    self.config.t_max_secs,
                    0.25,
                );
                skill.state = CircuitState::Open;
                skill.last_state_change_timestamp = now;
                skill.open_until_timestamp = now + backoff;

                let reason = if skill.consecutive_failures >= self.config.max_consecutive_failures {
                    format!(
                        "Exceeded max consecutive failures threshold ({}/{})",
                        skill.consecutive_failures, self.config.max_consecutive_failures
                    )
                } else {
                    format!(
                        "Sliding window anomaly rate exceeded ({:.1}% failures)",
                        anomaly_metrics.failure_rate_pct
                    )
                };

                return ExecutionAttemptOutcome {
                    skill_id: spec.skill_id.clone(),
                    success: false,
                    state: CircuitState::Open,
                    consecutive_failures: skill.consecutive_failures,
                    consecutive_successes: 0,
                    anomaly_metrics,
                    backoff_delay_secs: backoff,
                    is_tripped: true,
                    trip_reason: Some(reason),
                    lockout_receipt: None,
                    cycle_result: cycle_res,
                };
            }

            // Normal backoff dampening without tripping
            let fib_backoff = PidController::calculate_fibonacci_backoff(
                skill.consecutive_failures,
                self.config.t0_secs,
                self.config.t_max_secs,
                0.2,
            );
            return ExecutionAttemptOutcome {
                skill_id: spec.skill_id.clone(),
                success: false,
                state: skill.state,
                consecutive_failures: skill.consecutive_failures,
                consecutive_successes: 0,
                anomaly_metrics,
                backoff_delay_secs: fib_backoff,
                is_tripped: false,
                trip_reason: None,
                lockout_receipt: None,
                cycle_result: cycle_res,
            };
        }

        // 4. Handle Success Path (HalfOpen -> Recovery -> Closed)
        skill.consecutive_failures = 0;
        skill.consecutive_successes += 1;
        skill.total_successes += 1;

        match skill.state {
            CircuitState::HalfOpen => {
                skill.state = CircuitState::Recovery;
                skill.last_state_change_timestamp = now;
            }
            CircuitState::Recovery => {
                if skill.consecutive_successes >= self.config.recovery_threshold {
                    skill.state = CircuitState::Closed;
                    skill.last_state_change_timestamp = now;
                    skill.open_until_timestamp = 0.0;
                    skill.pid_controller.reset();
                }
            }
            _ => {
                skill.state = CircuitState::Closed;
            }
        }

        ExecutionAttemptOutcome {
            skill_id: spec.skill_id.clone(),
            success: true,
            state: skill.state,
            consecutive_failures: 0,
            consecutive_successes: skill.consecutive_successes,
            anomaly_metrics,
            backoff_delay_secs: 0.0,
            is_tripped: false,
            trip_reason: None,
            lockout_receipt: None,
            cycle_result: cycle_res,
        }
    }

    // --------------------------------------------------------------------------
    // Memory Tracking & Automatic Tripping
    // --------------------------------------------------------------------------
    pub fn record_memory_reading(
        &self,
        skill_id: &str,
        rss_bytes: u64,
        heap_bytes: u64,
        allocation_count: u64,
        timestamp_secs: f64,
        target_paths: &[String],
    ) -> (MemoryBreachKind, CircuitState) {
        let skill_arc = self.get_or_create_skill(skill_id);
        let mut skill = skill_arc.lock().unwrap();

        let breach = skill.memory_tracker.record_reading(
            rss_bytes,
            heap_bytes,
            allocation_count,
            timestamp_secs,
        );

        match &breach {
            MemoryBreachKind::None => (MemoryBreachKind::None, skill.state),
            MemoryBreachKind::InstantaneousSpike {
                delta_bytes,
                threshold_bytes,
            } => {
                // Trip to Open
                skill.state = CircuitState::Open;
                skill.open_until_timestamp = timestamp_secs + 30.0;
                (
                    MemoryBreachKind::InstantaneousSpike {
                        delta_bytes: *delta_bytes,
                        threshold_bytes: *threshold_bytes,
                    },
                    CircuitState::Open,
                )
            }
            MemoryBreachKind::LeakRateExceeded { .. }
            | MemoryBreachKind::MonotonicRunawayLeak { .. }
            | MemoryBreachKind::HardMemoryCeilingBreached { .. } => {
                // Severe memory leak -> Escalate directly to PermTrippedCryptolocked!
                let desc = format!("Memory leak rate/runaway breach: {:?}", breach);
                let _lockout = self.issue_lockout(
                    &mut skill,
                    skill_id,
                    &desc,
                    "MemoryLeakBreach",
                    vec![format!("rss:{}", rss_bytes)],
                    target_paths,
                    timestamp_secs,
                );
                (breach, CircuitState::PermTrippedCryptolocked)
            }
        }
    }

    // --------------------------------------------------------------------------
    // Issue Cryptographic Lockout Receipt & Strip Permissions (0444)
    // --------------------------------------------------------------------------
    fn issue_lockout(
        &self,
        skill: &mut SkillGovernorState,
        skill_id: &str,
        reason: &str,
        trigger_pattern: &str,
        flapping_signatures: Vec<String>,
        target_paths: &[String],
        now: f64,
    ) -> CryptographicLockoutReceipt {
        let lockout_id = format!("lockout_{}_{:x}", now as u64, (now * 1000.0) as u64 % 0xFFFF);
        let iso_now = Utc::now().to_rfc3339();

        // BLAKE3 Hash of error fingerprint and history
        let fp_combined = flapping_signatures.join("|");
        let blake3_fp = blake3::hash(fp_combined.as_bytes()).to_hex().to_string();

        let mut events_bytes = Vec::new();
        for ev in skill.anomaly_detector.events() {
            events_bytes.extend_from_slice(ev.duration_ms.to_string().as_bytes());
            events_bytes.extend_from_slice(ev.success.to_string().as_bytes());
        }
        let state_history_hash = blake3::hash(&events_bytes).to_hex().to_string();

        // Canonical signable payload: lockout_id:skill_id:PERM_TRIPPED_CRYPTOLOCKED:fp:hist:timestamp
        let signable = format!(
            "{}:{}:PERM_TRIPPED_CRYPTOLOCKED:{}:{}:{}",
            lockout_id, skill_id, blake3_fp, state_history_hash, now as u64
        );
        let sig_hex = self.keypair.sign_payload(signable.as_bytes());
        let pub_hex = self.keypair.public_key_hex();

        // Strip file permissions to read-only (0444)
        for p in target_paths {
            let path_obj = Path::new(p);
            if path_obj.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(path_obj, fs::Permissions::from_mode(0o444));
                }
            }
        }

        let receipt = CryptographicLockoutReceipt {
            lockout_id: lockout_id.clone(),
            skill_id: skill_id.to_string(),
            circuit_state: CircuitState::PermTrippedCryptolocked.as_str().to_string(),
            reason: reason.to_string(),
            trigger_pattern: trigger_pattern.to_string(),
            consecutive_count: skill.consecutive_failures,
            blake3_fingerprint: blake3_fp,
            state_history_hash,
            ed25519_public_key: pub_hex,
            ed25519_signature: sig_hex,
            locked_paths: target_paths.to_vec(),
            timestamp_iso: iso_now,
            timestamp_unix: now,
            mcp_unlock_required: true,
            mcp_tool_name: "hgb_operator_unlock_circuit_breaker".to_string(),
        };

        skill.state = CircuitState::PermTrippedCryptolocked;
        skill.last_state_change_timestamp = now;
        skill.open_until_timestamp = f64::INFINITY;
        skill.active_lockout = Some(receipt.clone());

        receipt
    }

    // --------------------------------------------------------------------------
    // Authorized Human Operator Signed MCP Unlock
    // --------------------------------------------------------------------------
    pub fn operator_unlock(&self, req: McpUnlockRequest) -> Result<McpUnlockReceipt> {
        let skill_arc = self.get_or_create_skill(&req.skill_id);
        let mut skill = skill_arc.lock().unwrap();

        // 1. Nonce Replay Defense
        {
            let mut nonces = self.consumed_nonces.lock().unwrap();
            if nonces.contains(&req.nonce) {
                return Err(HgbError::Security(format!(
                    "Replay attack detected: nonce {} has already been consumed",
                    req.nonce
                )));
            }
            nonces.insert(req.nonce);
        }

        // 2. State Check
        if skill.state != CircuitState::PermTrippedCryptolocked
            && skill.state != CircuitState::PermTripped
        {
            return Ok(McpUnlockReceipt {
                success: true,
                skill_id: req.skill_id.clone(),
                operator_id: req.operator_id,
                lockout_id: req.lockout_id,
                restored_state: skill.state,
                nonce: req.nonce,
                unlocked_at_iso: Utc::now().to_rfc3339(),
                restored_paths: Vec::new(),
                message: format!(
                    "Skill [{}] is already in state {}. No unlock necessary.",
                    req.skill_id,
                    skill.state.as_str()
                ),
            });
        }

        // 3. Lockout ID verification
        if let Some(active) = &skill.active_lockout {
            if active.lockout_id != req.lockout_id {
                return Err(HgbError::Security(format!(
                    "Lockout ID mismatch: active lockout is '{}', requested '{}'",
                    active.lockout_id, req.lockout_id
                )));
            }
        }

        // 4. Ed25519 Cryptographic Signature Check over challenge payload
        let expected_payload = format!(
            "UNLOCK:{}:{}:{}:{}",
            req.operator_id, req.skill_id, req.lockout_id, req.nonce
        );
        GovernorKeypair::verify_signature(
            &req.operator_public_key,
            expected_payload.as_bytes(),
            &req.ed25519_signature,
        )?;

        // 5. Restore File Permissions (0644)
        let mut restored = Vec::new();
        if req.restore_permissions {
            if let Some(active) = &skill.active_lockout {
                for p_str in &active.locked_paths {
                    let path_obj = Path::new(p_str);
                    if path_obj.exists() {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mode = if path_obj.is_dir() { 0o755 } else { 0o644 };
                            if fs::set_permissions(path_obj, fs::Permissions::from_mode(mode))
                                .is_ok()
                            {
                                restored.push(p_str.clone());
                            }
                        }
                    }
                }
            }
        }

        // 6. Reset Breaker State to Closed
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        skill.state = CircuitState::Closed;
        skill.consecutive_failures = 0;
        skill.consecutive_successes = 0;
        skill.open_until_timestamp = 0.0;
        skill.last_state_change_timestamp = now;
        skill.anomaly_score = 0.0;
        skill.failure_rate = 0.0;
        skill.flapping_detector.clear();
        skill.anomaly_detector.clear();
        skill.memory_tracker.reset();
        skill.cycle_detector.reset();
        skill.pid_controller.reset();
        skill.active_lockout = None;

        Ok(McpUnlockReceipt {
            success: true,
            skill_id: req.skill_id.clone(),
            operator_id: req.operator_id.clone(),
            lockout_id: req.lockout_id,
            restored_state: CircuitState::Closed,
            nonce: req.nonce,
            unlocked_at_iso: Utc::now().to_rfc3339(),
            restored_paths: restored,
            message: format!(
                "Circuit breaker for [{}] successfully unlocked by operator [{}]. State restored to CLOSED.",
                req.skill_id, req.operator_id
            ),
        })
    }

    // --------------------------------------------------------------------------
    // Distributed Mesh Quorum Validation
    // --------------------------------------------------------------------------
    pub fn validate_mesh_quorum(
        &self,
        skill_id: &str,
        proposed_state: CircuitState,
        peer_endorsements: &[String],
        min_quorum: usize,
    ) -> (bool, usize, String) {
        if !matches!(
            proposed_state,
            CircuitState::GlobalOpen
                | CircuitState::PermTripped
                | CircuitState::PermTrippedCryptolocked
        ) {
            return (
                true,
                peer_endorsements.len(),
                format!(
                    "State {} does not require distributed quorum agreement.",
                    proposed_state.as_str()
                ),
            );
        }

        let unique: HashSet<&String> = peer_endorsements.iter().collect();
        let count = unique.len();
        if count >= min_quorum {
            (
                true,
                count,
                format!(
                    "Quorum achieved: {} adjacent peers endorsed {} for skill [{}]",
                    count,
                    proposed_state.as_str(),
                    skill_id
                ),
            )
        } else {
            (
                false,
                count,
                format!(
                    "Quorum rejected: Only {}/{} peers endorsed {}. Restricting to LOCAL_OPEN.",
                    count,
                    min_quorum,
                    proposed_state.as_str()
                ),
            )
        }
    }
}

// ==============================================================================
// 12. Unit & Integration Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hysteresis_state_transitions() {
        let mut config = CircuitBreakerConfig::default();
        config.max_consecutive_failures = 2;
        config.recovery_threshold = 2;
        config.t0_secs = 0.5;

        let cb = AdaptivePidCircuitBreaker::new(config);
        let skill_id = "test_actor_calc";

        // Initial state is Closed
        let (can_run, state, _) = cb.check_mutation_eligibility(skill_id, 100.0);
        assert!(can_run);
        assert_eq!(state, CircuitState::Closed);

        // Failure 1 -> Still Closed, backoff applied
        let out1 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("E0001".into()),
            error_fingerprint: Some("fp_err_1".into()),
            target_paths: vec![],
            duration_ms: 12.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(101.0),
        });
        assert_eq!(out1.state, CircuitState::Closed);
        assert_eq!(out1.consecutive_failures, 1);

        // Failure 2 -> Breaches max_consecutive_failures (2) -> Trips to Open!
        let out2 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("E0002".into()),
            error_fingerprint: Some("fp_err_2".into()),
            target_paths: vec![],
            duration_ms: 15.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(102.0),
        });
        assert_eq!(out2.state, CircuitState::Open);
        assert!(out2.is_tripped);

        // Check eligibility during backoff -> Blocked!
        let (can_run2, state2, _) = cb.check_mutation_eligibility(skill_id, 102.1);
        assert!(!can_run2);
        assert_eq!(state2, CircuitState::Open);

        // Advance time past backoff -> Transitions to HalfOpen
        let (can_run3, state3, _) = cb.check_mutation_eligibility(skill_id, 105.0);
        assert!(can_run3);
        assert_eq!(state3, CircuitState::HalfOpen);

        // Probe success 1 -> Transitions to Recovery
        let out_succ1 = cb.record_execution_attempt(ExecutionAttemptSpec {
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
            current_time_secs: Some(106.0),
        });
        assert_eq!(out_succ1.state, CircuitState::Recovery);

        // Probe success 2 -> Meets recovery threshold (2) -> Transitions back to Closed!
        let out_succ2 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: true,
            error_trace: None,
            error_fingerprint: None,
            target_paths: vec![],
            duration_ms: 4.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(107.0),
        });
        assert_eq!(out_succ2.state, CircuitState::Closed);
    }

    #[test]
    fn test_flapping_oscillation_escalates_to_cryptolock() {
        let cb = AdaptivePidCircuitBreaker::new(CircuitBreakerConfig::default());
        let skill_id = "flapping_compiler_agent";

        // Cycle 1: Error A
        let out1 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("E0308 mismatched types".into()),
            error_fingerprint: Some("err_sig_a".into()),
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(10.0),
        });
        assert_eq!(out1.state, CircuitState::Closed);

        // Cycle 2: Error B
        let out2 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("E0596 cannot borrow mut".into()),
            error_fingerprint: Some("err_sig_b".into()),
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(11.0),
        });
        assert_eq!(out2.state, CircuitState::Closed);

        // Cycle 3: Error A (A <-> B <-> A detected -> Flapping oscillation!)
        let out3 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("E0308 mismatched types".into()),
            error_fingerprint: Some("err_sig_a".into()),
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(12.0),
        });

        assert_eq!(out3.state, CircuitState::PermTrippedCryptolocked);
        assert!(out3.lockout_receipt.is_some());
        let receipt = out3.lockout_receipt.unwrap();
        assert!(!receipt.ed25519_signature.is_empty());
        assert_eq!(
            receipt.circuit_state,
            "PERM_TRIPPED_CRYPTOLOCKED"
        );

        // Subsequent check must be permanently blocked
        let (can_run, state, _) = cb.check_mutation_eligibility(skill_id, 9999.0);
        assert!(!can_run);
        assert_eq!(state, CircuitState::PermTrippedCryptolocked);
    }

    #[test]
    fn test_memory_leak_and_spike_supervision() {
        let mut config = CircuitBreakerConfig::default();
        config.memory.memory_spike_threshold_bytes = 100 * 1024 * 1024; // 100 MB
        config.memory.max_leak_rate_bytes_per_sec = 5.0 * 1024.0 * 1024.0; // 5 MB/s
        let cb = AdaptivePidCircuitBreaker::new(config);
        let skill_id = "mem_leak_actor";

        // Reading 1: 50 MB
        let (b1, s1) = cb.record_memory_reading(
            skill_id,
            50 * 1024 * 1024,
            40 * 1024 * 1024,
            100,
            1.0,
            &[],
        );
        assert_eq!(b1, MemoryBreachKind::None);
        assert_eq!(s1, CircuitState::Closed);

        // Reading 2: Spike +120 MB (170 MB total) -> Instantaneous Spike Breach!
        let (b2, s2) = cb.record_memory_reading(
            skill_id,
            170 * 1024 * 1024,
            160 * 1024 * 1024,
            500,
            2.0,
            &[],
        );
        assert!(matches!(b2, MemoryBreachKind::InstantaneousSpike { .. }));
        assert_eq!(s2, CircuitState::Open);
    }

    #[test]
    fn test_code_generation_cycle_detection() {
        let cb = AdaptivePidCircuitBreaker::new(CircuitBreakerConfig::default());
        let skill_id = "agent_codegen_loop";

        // Turn 1: State Hash 1
        let o1 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: true,
            error_trace: None,
            error_fingerprint: None,
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: Some(1),
            state_hash: Some("hash_state_alpha".into()),
            turn_intent: Some("Add feature".into()),
            recursion_depth: Some(1),
            current_time_secs: Some(10.0),
        });
        assert_eq!(o1.state, CircuitState::Closed);

        // Turn 2: State Hash 2
        let o2 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: true,
            error_trace: None,
            error_fingerprint: None,
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: Some(2),
            state_hash: Some("hash_state_beta".into()),
            turn_intent: Some("Fix syntax".into()),
            recursion_depth: Some(2),
            current_time_secs: Some(11.0),
        });
        assert_eq!(o2.state, CircuitState::Closed);

        // Turn 3: State Hash 1 again! Direct 2-cycle oscillation detected!
        let o3 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: true,
            error_trace: None,
            error_fingerprint: None,
            target_paths: vec![],
            duration_ms: 10.0,
            turn_number: Some(3),
            state_hash: Some("hash_state_alpha".into()),
            turn_intent: Some("Revert fix".into()),
            recursion_depth: Some(3),
            current_time_secs: Some(12.0),
        });

        assert_eq!(o3.state, CircuitState::PermTrippedCryptolocked);
        assert!(o3.is_tripped);
        assert!(o3.cycle_result.is_some());
    }

    #[test]
    fn test_signed_operator_mcp_unlock_lifecycle() {
        let cb = AdaptivePidCircuitBreaker::new(CircuitBreakerConfig::default());
        let skill_id = "skill_secure_kernel";

        // Trigger flapping to force PERM_TRIPPED_CRYPTOLOCKED
        let _ = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("Err1".into()),
            error_fingerprint: Some("fp1".into()),
            target_paths: vec![],
            duration_ms: 5.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(1.0),
        });
        let _ = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("Err2".into()),
            error_fingerprint: Some("fp2".into()),
            target_paths: vec![],
            duration_ms: 5.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(2.0),
        });
        let out3 = cb.record_execution_attempt(ExecutionAttemptSpec {
            skill_id: skill_id.into(),
            success: false,
            error_trace: Some("Err1".into()),
            error_fingerprint: Some("fp1".into()),
            target_paths: vec![],
            duration_ms: 5.0,
            turn_number: None,
            state_hash: None,
            turn_intent: None,
            recursion_depth: None,
            current_time_secs: Some(3.0),
        });
        assert_eq!(out3.state, CircuitState::PermTrippedCryptolocked);
        let lockout_id = out3.lockout_receipt.unwrap().lockout_id;

        // Operator generates keypair and signs unlock challenge
        let operator_seed = b"test-operator-authorized-key-seed-99";
        let operator_keypair = GovernorKeypair::generate_deterministic(operator_seed);
        let operator_pub = operator_keypair.public_key_hex();
        let operator_id = "secops_lead_42";
        let nonce = 9876543210u64;

        let payload = format!(
            "UNLOCK:{}:{}:{}:{}",
            operator_id, skill_id, lockout_id, nonce
        );
        let signature = operator_keypair.sign_payload(payload.as_bytes());

        // Perform MCP Unlock
        let unlock_res = cb.operator_unlock(McpUnlockRequest {
            skill_id: skill_id.into(),
            operator_id: operator_id.into(),
            lockout_id: lockout_id.clone(),
            nonce,
            ed25519_signature: signature,
            operator_public_key: operator_pub,
            restore_permissions: true,
        });
        assert!(unlock_res.is_ok());
        let receipt = unlock_res.unwrap();
        assert!(receipt.success);
        assert_eq!(receipt.restored_state, CircuitState::Closed);

        // Breaker should now be Closed and eligible to run
        let (can_run, state, _) = cb.check_mutation_eligibility(skill_id, 10.0);
        assert!(can_run);
        assert_eq!(state, CircuitState::Closed);

        // Attempting to reuse same nonce must fail with Replay Attack error
        let replay_res = cb.operator_unlock(McpUnlockRequest {
            skill_id: skill_id.into(),
            operator_id: operator_id.into(),
            lockout_id,
            nonce,
            ed25519_signature: "dummy".into(),
            operator_public_key: "00".repeat(32),
            restore_permissions: true,
        });
        assert!(replay_res.is_err());
    }
}
