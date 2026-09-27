//! # CircularCircuitBreaker - Agent Oscillation & Thrashing Circuit Breaker
//!
//! Protects vibe coding agents from falling into infinite error-correction loops.
//! Tracks Blake3 state hashes of modified files and error fingerprints across consecutive turns.
//! Trips the circuit breaker upon detecting cyclic oscillation (e.g., State A -> State B -> State A),
//! halts token waste, and generates an actionable pivot prescription to break the deadlocked assumption.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot of the codebase state at a specific agent turn
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeStateSnapshot {
    pub turn_number: usize,
    pub composite_hash: String,
    pub file_hashes: HashMap<String, String>,
    pub error_fingerprint: Option<String>,
    pub intent_summary: String,
    pub timestamp_epoch: u64,
}

/// Nature of the detected oscillation loop
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LoopPatternKind {
    DirectOscillation,   // A -> B -> A
    PeriodicCycle,       // A -> B -> C -> A
    ErrorThrashing,      // Repeated identical compiler/test error across turns
}

/// Comprehensive report on circuit breaker status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerReport {
    pub is_tripped: bool,
    pub current_turn: usize,
    pub detected_pattern: Option<LoopPatternKind>,
    pub loop_description: String,
    pub rollback_target_turn: Option<usize>,
    pub pivot_prescription: String,
    pub recorded_snapshots_count: usize,
}

pub struct CircularCircuitBreaker {
    pub history: Vec<CodeStateSnapshot>,
    pub max_history: usize,
    pub error_repetition_threshold: usize,
}

impl Default for CircularCircuitBreaker {
    fn default() -> Self {
        Self::new(20, 3)
    }
}

impl CircularCircuitBreaker {
    pub fn new(max_history: usize, error_repetition_threshold: usize) -> Self {
        Self {
            history: Vec::new(),
            max_history,
            error_repetition_threshold,
        }
    }

    /// Reset breaker history
    pub fn reset(&mut self) {
        self.history.clear();
    }

    /// Record the current turn's file modifications and check for circular oscillation
    pub fn record_and_evaluate(
        &mut self,
        turn_number: usize,
        files: &[(&str, &str)],
        error_output: Option<&str>,
        intent: &str,
    ) -> Result<CircuitBreakerReport> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 1. Calculate Blake3 hash per file
        let mut file_hashes = HashMap::new();
        let mut combined_bytes = Vec::new();

        for (path, content) in files {
            let h = blake3::hash(content.as_bytes()).to_hex().to_string();
            combined_bytes.extend_from_slice(path.as_bytes());
            combined_bytes.extend_from_slice(h.as_bytes());
            file_hashes.insert(path.to_string(), h);
        }

        let composite_hash = blake3::hash(&combined_bytes).to_hex().to_string();

        // 2. Normalize error output into invariant fingerprint
        let error_fingerprint = error_output.map(|err| {
            // Strip volatile line numbers and timestamps to group recurring errors
            err.lines()
                .filter(|l| l.contains("error") || l.contains("panic") || l.contains("FAILED"))
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ")
        });

        let snapshot = CodeStateSnapshot {
            turn_number,
            composite_hash: composite_hash.clone(),
            file_hashes,
            error_fingerprint: error_fingerprint.clone(),
            intent_summary: intent.to_string(),
            timestamp_epoch: now,
        };

        // 3. Detect oscillation against historical turns
        let mut is_tripped = false;
        let mut loop_pattern = None;
        let mut loop_desc = String::new();
        let mut rollback_turn = None;
        let mut pivot = String::new();

        // A. Direct Oscillation Check (A -> B -> A)
        if self.history.len() >= 2 {
            let prev_prev = &self.history[self.history.len() - 2];
            if prev_prev.composite_hash == composite_hash {
                is_tripped = true;
                loop_pattern = Some(LoopPatternKind::DirectOscillation);
                rollback_turn = Some(prev_prev.turn_number);
                loop_desc = format!(
                    "Direct state oscillation detected: Turn {} reverted files to identical hash as Turn {}.",
                    turn_number, prev_prev.turn_number
                );
                pivot = format!(
                    "CIRCUIT BREAKER TRIPPED: You are oscillating between two conflicting solutions for '{}'. Stop toggling this code path. Propose an alternate architectural abstraction or introduce an intermediate adapter.",
                    intent
                );
            }
        }

        // B. Periodic Cycle Check (A -> B -> C -> A)
        if !is_tripped && self.history.len() >= 3 {
            for past in self.history.iter() {
                if past.composite_hash == composite_hash {
                    is_tripped = true;
                    loop_pattern = Some(LoopPatternKind::PeriodicCycle);
                    rollback_turn = Some(past.turn_number);
                    loop_desc = format!(
                        "Periodic cycle detected: Current state in Turn {} matches past state from Turn {}.",
                        turn_number, past.turn_number
                    );
                    pivot = format!(
                        "CIRCUIT BREAKER TRIPPED: A multi-turn circular loop was identified across turns {}..{}. Discard staged approach and rollback to Turn {}.",
                        past.turn_number, turn_number, past.turn_number
                    );
                    break;
                }
            }
        }

        // C. Error Thrashing Check (same recurring error 3+ times)
        if !is_tripped && error_fingerprint.is_some() {
            let target_fp = error_fingerprint.as_ref().unwrap();
            if !target_fp.trim().is_empty() {
                let mut repeat_count = 0;
                for past in self.history.iter().rev() {
                    if past.error_fingerprint.as_ref() == Some(target_fp) {
                        repeat_count += 1;
                    }
                }
                if repeat_count >= self.error_repetition_threshold - 1 {
                    is_tripped = true;
                    loop_pattern = Some(LoopPatternKind::ErrorThrashing);
                    loop_desc = format!(
                        "Compiler/test error thrashing detected: Error fingerprint has recurred {} times: '{}'",
                        repeat_count + 1,
                        target_fp
                    );
                    pivot = format!(
                        "CIRCUIT BREAKER TRIPPED: Consecutive attempts have failed with identical error '{}'. Do not attempt mechanical patches. Re-read type definitions or inspect dependencies.",
                        target_fp
                    );
                }
            }
        }

        // Push current turn to history
        self.history.push(snapshot);
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        if !is_tripped {
            loop_desc = "State progression is linear and non-oscillating.".to_string();
            pivot = "Clean state progression. Proceed with next step.".to_string();
        }

        Ok(CircuitBreakerReport {
            is_tripped,
            current_turn: turn_number,
            detected_pattern: loop_pattern,
            loop_description: loop_desc,
            rollback_target_turn: rollback_turn,
            pivot_prescription: pivot,
            recorded_snapshots_count: self.history.len(),
        })
    }
}
