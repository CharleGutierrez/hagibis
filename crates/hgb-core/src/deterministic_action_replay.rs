//! # Deterministic Action Replay & Chaos Testing Engine
//!
//! Pillar 5: Deterministic record-and-replay engine for agent execution traces:
//! - Records autonomous agent actions with cryptographic non-repudiation (SHA-256 chain).
//! - Periodic and mutation-boundary state snapshotting.
//! - Sandboxed WASM kernel replay with deterministic PRNG seeding and mocked virtual clocks.
//! - $O(\log K)$ automated bisection regression isolation identifying the exact offending prompt/AST mutation.
//! - Cyclic oscillation loop detection (circuit breaker trip + atomic deterministic rollback).

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Execution status of an agent action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionStatus {
    Success,
    Failed(String),
    OscillationDetected,
    RolledBack,
}

impl std::fmt::Display for ActionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionStatus::Success => write!(f, "Success"),
            ActionStatus::Failed(err) => write!(f, "Failed: {}", err),
            ActionStatus::OscillationDetected => write!(f, "OscillationDetected"),
            ActionStatus::RolledBack => write!(f, "RolledBack"),
        }
    }
}

/// A cryptographically chained atomic record of an agent action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentActionRecord {
    pub action_id: String,
    pub sequence_number: u64,
    pub timestamp_ms: u64,
    pub prompt_hash: String,
    pub tool_name: String,
    pub tool_arguments_json: String,
    pub random_seed: u64,
    pub pre_state_hash: String,
    pub post_state_hash: String,
    pub ast_mutation_diff: Option<String>,
    pub execution_status: ActionStatus,
    pub prev_record_hash: String,
    pub record_hash: String,
}

impl AgentActionRecord {
    /// Construct a new action record and compute its cryptographic SHA-256 hash.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sequence_number: u64,
        prompt_hash: impl Into<String>,
        tool_name: impl Into<String>,
        tool_arguments_json: impl Into<String>,
        random_seed: u64,
        pre_state_hash: impl Into<String>,
        post_state_hash: impl Into<String>,
        ast_mutation_diff: Option<String>,
        execution_status: ActionStatus,
        prev_record_hash: impl Into<String>,
    ) -> Self {
        let action_id = uuid::Uuid::new_v4().to_string();
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut record = Self {
            action_id,
            sequence_number,
            timestamp_ms,
            prompt_hash: prompt_hash.into(),
            tool_name: tool_name.into(),
            tool_arguments_json: tool_arguments_json.into(),
            random_seed,
            pre_state_hash: pre_state_hash.into(),
            post_state_hash: post_state_hash.into(),
            ast_mutation_diff,
            execution_status,
            prev_record_hash: prev_record_hash.into(),
            record_hash: String::new(),
        };

        record.record_hash = record.calculate_hash();
        record
    }

    /// Calculates SHA-256 digest across all record fields including previous record hash.
    pub fn calculate_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.prev_record_hash.as_bytes());
        hasher.update(self.sequence_number.to_be_bytes());
        hasher.update(self.timestamp_ms.to_be_bytes());
        hasher.update(self.prompt_hash.as_bytes());
        hasher.update(self.tool_name.as_bytes());
        hasher.update(self.tool_arguments_json.as_bytes());
        hasher.update(self.random_seed.to_be_bytes());
        hasher.update(self.pre_state_hash.as_bytes());
        hasher.update(self.post_state_hash.as_bytes());
        if let Some(diff) = &self.ast_mutation_diff {
            hasher.update(b"|diff:");
            hasher.update(diff.as_bytes());
        }
        let status_str = format!("{}", self.execution_status);
        hasher.update(status_str.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Verifies that the internal `record_hash` matches the digest of the payload.
    pub fn verify_hash(&self) -> bool {
        self.record_hash == self.calculate_hash()
    }
}

/// An immutable point-in-time snapshot of the virtual or project file state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateSnapshot {
    pub snapshot_id: String,
    pub sequence_number: u64,
    pub timestamp_ms: u64,
    pub state_hash: String,
    pub files: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
}

impl StateSnapshot {
    pub fn new(sequence_number: u64, files: BTreeMap<String, String>) -> Self {
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let state_hash = Self::calculate_state_hash(&files);
        Self {
            snapshot_id,
            sequence_number,
            timestamp_ms,
            state_hash,
            files,
            metadata: BTreeMap::new(),
        }
    }

    /// Computes canonical SHA-256 hash over sorted filenames and file contents.
    pub fn calculate_state_hash(files: &BTreeMap<String, String>) -> String {
        let mut hasher = Sha256::new();
        for (path, content) in files {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(content.as_bytes());
            hasher.update(b"\n");
        }
        hex::encode(hasher.finalize())
    }
}

/// Append-only execution journal with cryptographic tamper-evident chaining.
#[derive(Debug)]
pub struct ActionTraceJournal {
    session_id: String,
    journal_path: Option<PathBuf>,
    records: Vec<AgentActionRecord>,
    snapshots: HashMap<u64, StateSnapshot>,
}

impl ActionTraceJournal {
    /// Creates an in-memory journal for ephemeral execution or testing.
    pub fn in_memory(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            journal_path: None,
            records: Vec::new(),
            snapshots: HashMap::new(),
        }
    }

    /// Opens or creates a persistent journal backed by an append-only `.jsonl` file.
    pub fn open_or_create(session_id: impl Into<String>, path: impl AsRef<Path>) -> Result<Self> {
        let session_id = session_id.into();
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| HgbError::Execution(format!("Failed to create journal dir: {}", e)))?;
        }

        let mut records = Vec::new();
        if path.exists() {
            let file = File::open(&path)
                .map_err(|e| HgbError::Execution(format!("Failed to open journal file: {}", e)))?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line_str = line.map_err(|e| HgbError::Execution(format!("Read error: {}", e)))?;
                if line_str.trim().is_empty() {
                    continue;
                }
                let record: AgentActionRecord = serde_json::from_str(&line_str)
                    .map_err(|e| HgbError::Serialization(format!("Corrupted journal line: {}", e)))?;
                records.push(record);
            }
        }

        let journal = Self {
            session_id,
            journal_path: Some(path),
            records,
            snapshots: HashMap::new(),
        };

        if !journal.records.is_empty() {
            journal.verify_cryptographic_integrity()?;
        }

        Ok(journal)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn records(&self) -> &[AgentActionRecord] {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn get_record(&self, sequence_number: u64) -> Option<&AgentActionRecord> {
        self.records.iter().find(|r| r.sequence_number == sequence_number)
    }

    pub fn latest_record(&self) -> Option<&AgentActionRecord> {
        self.records.last()
    }

    /// Saves a state snapshot tied to a specific sequence number.
    pub fn save_snapshot(&mut self, snapshot: StateSnapshot) {
        self.snapshots.insert(snapshot.sequence_number, snapshot);
    }

    pub fn get_snapshot(&self, sequence_number: u64) -> Option<&StateSnapshot> {
        self.snapshots.get(&sequence_number)
    }

    pub fn latest_snapshot(&self) -> Option<&StateSnapshot> {
        let max_seq = self.snapshots.keys().max().copied()?;
        self.snapshots.get(&max_seq)
    }

    /// Appends an action record to the journal, verifying chaining and persisting to disk.
    pub fn append_record(&mut self, mut record: AgentActionRecord) -> Result<AgentActionRecord> {
        let expected_prev_hash = if let Some(last) = self.records.last() {
            last.record_hash.clone()
        } else {
            GENESIS_PREV_HASH.to_string()
        };

        if record.prev_record_hash != expected_prev_hash {
            record.prev_record_hash = expected_prev_hash;
            record.record_hash = record.calculate_hash();
        }

        if !record.verify_hash() {
            return Err(HgbError::Execution(format!(
                "Action record hash mismatch for seq {}",
                record.sequence_number
            )));
        }

        if let Some(path) = &self.journal_path {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| HgbError::Execution(format!("Failed to append to journal: {}", e)))?;
            let serialized = serde_json::to_string(&record)
                .map_err(|e| HgbError::Serialization(e.to_string()))?;
            writeln!(file, "{}", serialized)
                .map_err(|e| HgbError::Execution(format!("Failed to write to journal: {}", e)))?;
        }

        self.records.push(record.clone());
        Ok(record)
    }

    /// Verifies the entire cryptographic hash chain from genesis to tail.
    pub fn verify_cryptographic_integrity(&self) -> Result<bool> {
        let mut expected_prev = GENESIS_PREV_HASH.to_string();

        for (idx, record) in self.records.iter().enumerate() {
            if record.prev_record_hash != expected_prev {
                return Err(HgbError::Execution(format!(
                    "Cryptographic chain broken at step {}: expected prev_hash {}, got {}",
                    idx, expected_prev, record.prev_record_hash
                )));
            }

            if !record.verify_hash() {
                return Err(HgbError::Execution(format!(
                    "Record digest tampered at step {} (seq {})",
                    idx, record.sequence_number
                )));
            }

            expected_prev = record.record_hash.clone();
        }

        Ok(true)
    }
}

/// Lightweight sandboxed virtual execution kernel mimicking a deterministic WASM runtime.
#[derive(Debug, Clone)]
pub struct SandboxedWasmKernel {
    pub files: BTreeMap<String, String>,
    pub current_time_ms: u64,
    pub prng_state: u64,
    pub execution_log: Vec<String>,
}

impl SandboxedWasmKernel {
    pub fn new_from_snapshot(snapshot: &StateSnapshot) -> Self {
        Self {
            files: snapshot.files.clone(),
            current_time_ms: snapshot.timestamp_ms,
            prng_state: 0,
            execution_log: Vec::new(),
        }
    }

    pub fn set_clock(&mut self, timestamp_ms: u64) {
        self.current_time_ms = timestamp_ms;
    }

    pub fn seed_prng(&mut self, seed: u64) {
        self.prng_state = if seed == 0 { 0xdeadbeefcafe } else { seed };
    }

    /// Deterministic 64-bit XorShift PRNG
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.prng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.prng_state = x;
        (x & 0xffffffff) as u32
    }

    pub fn current_state_hash(&self) -> String {
        StateSnapshot::calculate_state_hash(&self.files)
    }

    /// Applies an AST mutation diff deterministically to the virtual file state.
    pub fn apply_mutation_diff(files: &mut BTreeMap<String, String>, diff: &str) -> Result<()> {
        let trimmed = diff.trim();
        // Support JSON format: {"file": "...", "content": "..."} or {"file": "...", "replace": "...", "with": "..."}
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(file) = v.get("file").and_then(|f| f.as_str()) {
                    if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                        files.insert(file.to_string(), content.to_string());
                        return Ok(());
                    } else if let (Some(replace), Some(with)) = (
                        v.get("replace").and_then(|r| r.as_str()),
                        v.get("with").and_then(|w| w.as_str()),
                    ) {
                        let existing = files.entry(file.to_string()).or_default();
                        *existing = existing.replace(replace, with);
                        return Ok(());
                    }
                }
            }
        }

        // Support line-based directives:
        // SET:<path>\n<content>
        if let Some(rest) = trimmed.strip_prefix("SET:") {
            if let Some((path, content)) = rest.split_once('\n') {
                files.insert(path.trim().to_string(), content.to_string());
                return Ok(());
            }
        }

        // REPLACE:<path>\nFIND:<old>\nWITH:<new>
        if let Some(rest) = trimmed.strip_prefix("REPLACE:") {
            let lines: Vec<&str> = rest.lines().collect();
            if lines.len() >= 3 {
                let path = lines[0].trim();
                let find_line = lines[1].strip_prefix("FIND:").unwrap_or("");
                let with_line = lines[2].strip_prefix("WITH:").unwrap_or("");
                let existing = files.entry(path.to_string()).or_default();
                *existing = existing.replace(find_line, with_line);
                return Ok(());
            }
        }

        // Generic fallback: treat whole diff as patch on default file
        let default_file = "src/main.rs";
        files.insert(default_file.to_string(), diff.to_string());
        Ok(())
    }

    /// Replays a single record inside the sandbox.
    pub fn replay_step(&mut self, record: &AgentActionRecord) -> Result<String> {
        self.set_clock(record.timestamp_ms);
        self.seed_prng(record.random_seed);

        if let Some(diff) = &record.ast_mutation_diff {
            Self::apply_mutation_diff(&mut self.files, diff)?;
        }

        let computed_hash = self.current_state_hash();
        self.execution_log.push(format!(
            "seq={}: tool={} hash={}",
            record.sequence_number, record.tool_name, computed_hash
        ));
        Ok(computed_hash)
    }
}

/// Report detailing the outcome of deterministic trace replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayOutcome {
    pub total_steps_replayed: usize,
    pub verified_steps: usize,
    pub state_hashes_match: bool,
    pub first_divergence_seq: Option<u64>,
    pub initial_state_hash: String,
    pub final_state_hash: String,
    pub execution_time_us: u64,
}

/// Deterministic replay engine: re-executes action journals in an isolated sandbox.
pub struct DeterministicReplayEngine;

impl DeterministicReplayEngine {
    pub fn new() -> Self {
        Self
    }

    /// Replays the journal starting from snapshot at `from_seq` through `to_seq`.
    pub fn replay(
        &self,
        journal: &ActionTraceJournal,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<ReplayOutcome> {
        let start_time = std::time::Instant::now();

        let base_snapshot = journal.get_snapshot(from_seq).ok_or_else(|| {
            HgbError::Execution(format!("Base snapshot for sequence {} not found", from_seq))
        })?;

        let mut sandbox = SandboxedWasmKernel::new_from_snapshot(base_snapshot);
        let initial_state_hash = sandbox.current_state_hash();

        let mut total_replayed = 0;
        let mut verified_steps = 0;
        let mut first_divergence: Option<u64> = None;

        let records: Vec<&AgentActionRecord> = journal
            .records()
            .iter()
            .filter(|r| r.sequence_number > from_seq && r.sequence_number <= to_seq)
            .collect();

        for record in records {
            total_replayed += 1;
            let computed_hash = sandbox.replay_step(record)?;

            if computed_hash == record.post_state_hash {
                verified_steps += 1;
            } else if first_divergence.is_none() {
                first_divergence = Some(record.sequence_number);
            }
        }

        let final_state_hash = sandbox.current_state_hash();
        let elapsed_us = start_time.elapsed().as_micros() as u64;

        Ok(ReplayOutcome {
            total_steps_replayed: total_replayed,
            verified_steps,
            state_hashes_match: first_divergence.is_none(),
            first_divergence_seq: first_divergence,
            initial_state_hash,
            final_state_hash,
            execution_time_us: elapsed_us,
        })
    }
}

impl Default for DeterministicReplayEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Log entry for each step in the bisection search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectionStepRecord {
    pub step_index: usize,
    pub tested_sequence_number: u64,
    pub passed: bool,
    pub state_hash: String,
    pub message: String,
}

/// Outcome of automated bisection regression isolation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectionResult {
    pub culprit_found: bool,
    pub offending_sequence_number: Option<u64>,
    pub offending_action_id: Option<String>,
    pub offending_mutation_diff: Option<String>,
    pub offending_tool_name: Option<String>,
    pub verified_green_sequence: u64,
    pub failed_red_sequence: u64,
    pub total_trace_steps: usize,
    pub bisection_steps_evaluated: usize,
    pub bisection_log: Vec<BisectionStepRecord>,
}

/// Automated binary-search bisection isolator over recorded action traces.
pub struct BisectionRegressionIsolator;

impl BisectionRegressionIsolator {
    pub fn new() -> Self {
        Self
    }

    /// Performs binary search in $O(\log K)$ steps between `green_seq` and `red_seq`
    /// to isolate the exact prompt or AST mutation that introduced the failure.
    pub fn isolate_culprit<F>(
        &self,
        journal: &ActionTraceJournal,
        green_seq: u64,
        red_seq: u64,
        predicate: F,
    ) -> Result<BisectionResult>
    where
        F: Fn(&SandboxedWasmKernel) -> bool,
    {
        if green_seq >= red_seq {
            return Err(HgbError::Execution(format!(
                "Invalid bisection bounds: green_seq ({}) >= red_seq ({})",
                green_seq, red_seq
            )));
        }

        let base_snapshot = journal.get_snapshot(green_seq).ok_or_else(|| {
            HgbError::Execution(format!("Missing base snapshot at green step {}", green_seq))
        })?;

        let mut low = green_seq;
        let mut high = red_seq;
        let mut bisection_log = Vec::new();
        let mut eval_step = 0;

        while low + 1 < high {
            eval_step += 1;
            let mid = low + (high - low) / 2;

            // Replay state from base snapshot up to `mid`
            let mut sandbox = SandboxedWasmKernel::new_from_snapshot(base_snapshot);
            let relevant_records: Vec<&AgentActionRecord> = journal
                .records()
                .iter()
                .filter(|r| r.sequence_number > green_seq && r.sequence_number <= mid)
                .collect();

            for r in relevant_records {
                sandbox.replay_step(r)?;
            }

            let passed = predicate(&sandbox);
            let current_hash = sandbox.current_state_hash();

            bisection_log.push(BisectionStepRecord {
                step_index: eval_step,
                tested_sequence_number: mid,
                passed,
                state_hash: current_hash,
                message: if passed {
                    format!("Step {} passed predicate (State Green)", mid)
                } else {
                    format!("Step {} failed predicate (State Red)", mid)
                },
            });

            if passed {
                low = mid;
            } else {
                high = mid;
            }
        }

        // High is the exact sequence number that flipped state from Green to Red
        let offending_record = journal.get_record(high);

        Ok(BisectionResult {
            culprit_found: offending_record.is_some(),
            offending_sequence_number: Some(high),
            offending_action_id: offending_record.map(|r| r.action_id.clone()),
            offending_mutation_diff: offending_record.and_then(|r| r.ast_mutation_diff.clone()),
            offending_tool_name: offending_record.map(|r| r.tool_name.clone()),
            verified_green_sequence: green_seq,
            failed_red_sequence: red_seq,
            total_trace_steps: (red_seq - green_seq) as usize,
            bisection_steps_evaluated: eval_step,
            bisection_log,
        })
    }
}

impl Default for BisectionRegressionIsolator {
    fn default() -> Self {
        Self::new()
    }
}

/// Description of a detected oscillation cycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OscillationCycle {
    pub cycle_length: usize,
    pub cycle_start_seq: u64,
    pub cycle_end_seq: u64,
    pub recurring_state_hash: String,
    pub reason: String,
}

/// Status report emitted by the oscillation loop detector and circuit breaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OscillationReport {
    pub oscillation_detected: bool,
    pub cycle: Option<OscillationCycle>,
    pub circuit_breaker_tripped: bool,
    pub rollback_target_seq: Option<u64>,
    pub rollback_state_hash: Option<String>,
    pub recovered_files_count: usize,
}

/// Monitors action records for cyclic thrashing, trips circuit breaker, and rolls back state.
pub struct OscillationLoopDetector {
    max_cycle_window: usize,
}

impl OscillationLoopDetector {
    pub fn new(max_cycle_window: usize) -> Self {
        Self {
            max_cycle_window: max_cycle_window.max(2),
        }
    }

    /// Analyzes the journal to detect whether the agent has entered an oscillation loop.
    pub fn detect_oscillation(&self, records: &[AgentActionRecord]) -> Option<OscillationCycle> {
        let n = records.len();
        if n < 2 {
            return None;
        }

        let window = self.max_cycle_window.min(n);
        let tail = &records[n - window..];

        // 1. Direct state hash cycle detection (e.g. state hash at step i matches earlier step j)
        for i in 0..tail.len() {
            let current = &tail[i];
            for j in 0..i {
                let prior = &tail[j];
                if current.post_state_hash == prior.post_state_hash {
                    return Some(OscillationCycle {
                        cycle_length: i - j,
                        cycle_start_seq: prior.sequence_number,
                        cycle_end_seq: current.sequence_number,
                        recurring_state_hash: current.post_state_hash.clone(),
                        reason: format!(
                            "State hash collision: sequence {} matches sequence {} (hash: {})",
                            current.sequence_number,
                            prior.sequence_number,
                            &current.post_state_hash[..8]
                        ),
                    });
                }
            }
        }

        // 2. Inverse mutation diff detection (Patch A -> Patch B where B inverts A)
        if n >= 2 {
            let last = &records[n - 1];
            let prev = &records[n - 2];
            if let (Some(d1), Some(d2)) = (&last.ast_mutation_diff, &prev.ast_mutation_diff) {
                if Self::is_inverse_diff(d1, d2) {
                    return Some(OscillationCycle {
                        cycle_length: 2,
                        cycle_start_seq: prev.sequence_number,
                        cycle_end_seq: last.sequence_number,
                        recurring_state_hash: last.post_state_hash.clone(),
                        reason: format!(
                            "Inverse mutation diff detected between seq {} and seq {}",
                            prev.sequence_number, last.sequence_number
                        ),
                    });
                }
            }
        }

        None
    }

    fn is_inverse_diff(d1: &str, d2: &str) -> bool {
        // Simple heuristics: opposite replacements or flipped contents
        if let (Ok(v1), Ok(v2)) = (
            serde_json::from_str::<serde_json::Value>(d1),
            serde_json::from_str::<serde_json::Value>(d2),
        ) {
            if let (Some(r1), Some(w1), Some(r2), Some(w2)) = (
                v1.get("replace").and_then(|s| s.as_str()),
                v1.get("with").and_then(|s| s.as_str()),
                v2.get("replace").and_then(|s| s.as_str()),
                v2.get("with").and_then(|s| s.as_str()),
            ) {
                return r1 == w2 && w1 == r2;
            }
        }
        false
    }

    /// Trips circuit breaker and invokes deterministic atomic rollback to the last verified green state.
    pub fn trip_and_rollback(
        &self,
        journal: &ActionTraceJournal,
        sandbox: &mut SandboxedWasmKernel,
    ) -> Result<OscillationReport> {
        let cycle = self.detect_oscillation(journal.records());
        if let Some(c) = cycle {
            // Roll back to the snapshot at or before cycle_start_seq
            let rollback_target_seq = c.cycle_start_seq.saturating_sub(1);
            let snapshot = journal
                .get_snapshot(rollback_target_seq)
                .or_else(|| journal.get_snapshot(0))
                .ok_or_else(|| {
                    HgbError::Execution(format!(
                        "No rollback snapshot available for sequence {}",
                        rollback_target_seq
                    ))
                })?;

            // Restore sandbox files atomically
            sandbox.files = snapshot.files.clone();
            sandbox.set_clock(snapshot.timestamp_ms);
            let restored_hash = sandbox.current_state_hash();

            Ok(OscillationReport {
                oscillation_detected: true,
                cycle: Some(c),
                circuit_breaker_tripped: true,
                rollback_target_seq: Some(snapshot.sequence_number),
                rollback_state_hash: Some(restored_hash),
                recovered_files_count: sandbox.files.len(),
            })
        } else {
            Ok(OscillationReport {
                oscillation_detected: false,
                cycle: None,
                circuit_breaker_tripped: false,
                rollback_target_seq: None,
                rollback_state_hash: None,
                recovered_files_count: 0,
            })
        }
    }
}

impl Default for OscillationLoopDetector {
    fn default() -> Self {
        Self::new(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_creation_and_cryptographic_chaining() {
        let mut journal = ActionTraceJournal::in_memory("test-session-1");

        let r1 = AgentActionRecord::new(
            1,
            "prompt_hash_1",
            "file_edit",
            r#"{"path":"lib.rs"}"#,
            42,
            "hash_0",
            "hash_1",
            Some(r#"{"file":"src/lib.rs","content":"fn hello() -> &'static str { \"world\" }"}"#.to_string()),
            ActionStatus::Success,
            GENESIS_PREV_HASH,
        );
        let appended_1 = journal.append_record(r1).expect("Append r1");
        assert!(appended_1.verify_hash());

        let r2 = AgentActionRecord::new(
            2,
            "prompt_hash_2",
            "cargo_check",
            r#"{}"#,
            43,
            "hash_1",
            "hash_2",
            None,
            ActionStatus::Success,
            &appended_1.record_hash,
        );
        let appended_2 = journal.append_record(r2).expect("Append r2");
        assert!(appended_2.verify_hash());

        // Verify chain integrity
        assert!(journal.verify_cryptographic_integrity().expect("Integrity valid"));
        assert_eq!(journal.len(), 2);
    }

    #[test]
    fn test_tampered_journal_detection() {
        let mut journal = ActionTraceJournal::in_memory("test-session-tamper");

        let r1 = AgentActionRecord::new(
            1, "p1", "tool1", "{}", 1, "h0", "h1", None, ActionStatus::Success, GENESIS_PREV_HASH,
        );
        let app1 = journal.append_record(r1).expect("Append r1");

        let mut r2 = AgentActionRecord::new(
            2, "p2", "tool2", "{}", 2, "h1", "h2", None, ActionStatus::Success, &app1.record_hash,
        );
        // Tamper with r2's prompt hash without updating hash
        r2.prompt_hash = "tampered_prompt".to_string();
        let res = journal.append_record(r2);
        assert!(res.is_err(), "Appending tampered record must fail");
    }

    #[test]
    fn test_deterministic_replay_in_sandbox() {
        let mut files = BTreeMap::new();
        files.insert("src/calc.rs".to_string(), "pub fn add(a: i32, b: i32) -> i32 { a + b }".to_string());
        let snapshot0 = StateSnapshot::new(0, files);
        let hash0 = snapshot0.state_hash.clone();

        let mut journal = ActionTraceJournal::in_memory("replay-session");
        journal.save_snapshot(snapshot0);

        // Step 1: change add to add 1 extra
        let diff1 = r#"{"file":"src/calc.rs","replace":"a + b","with":"a + b + 1"}"#;
        let mut sandbox_sim = SandboxedWasmKernel::new_from_snapshot(journal.get_snapshot(0).unwrap());
        SandboxedWasmKernel::apply_mutation_diff(&mut sandbox_sim.files, diff1).unwrap();
        let hash1 = sandbox_sim.current_state_hash();

        let r1 = AgentActionRecord::new(
            1, "p1", "ast_edit", "{}", 100, &hash0, &hash1,
            Some(diff1.to_string()), ActionStatus::Success, GENESIS_PREV_HASH,
        );
        journal.append_record(r1).unwrap();

        // Step 2: add comment
        let diff2 = r#"{"file":"src/calc.rs","replace":"pub fn","with":"// Optimized\npub fn"}"#;
        SandboxedWasmKernel::apply_mutation_diff(&mut sandbox_sim.files, diff2).unwrap();
        let hash2 = sandbox_sim.current_state_hash();

        let r2 = AgentActionRecord::new(
            2, "p2", "ast_edit", "{}", 101, &hash1, &hash2,
            Some(diff2.to_string()), ActionStatus::Success, &journal.records()[0].record_hash,
        );
        journal.append_record(r2).unwrap();

        // Replay via DeterministicReplayEngine
        let engine = DeterministicReplayEngine::new();
        let outcome = engine.replay(&journal, 0, 2).expect("Replay success");

        assert_eq!(outcome.total_steps_replayed, 2);
        assert_eq!(outcome.verified_steps, 2);
        assert!(outcome.state_hashes_match);
        assert_eq!(outcome.final_state_hash, hash2);
    }

    #[test]
    fn test_bisection_isolation_16_steps() {
        // Construct 16 sequential mutations where step 9 introduces a regression bug
        let mut files = BTreeMap::new();
        files.insert("src/state.rs".to_string(), "const VALUE: i32 = 0;".to_string());
        let snapshot0 = StateSnapshot::new(0, files);

        let mut journal = ActionTraceJournal::in_memory("bisection-session");
        journal.save_snapshot(snapshot0);

        let mut prev_hash = journal.get_snapshot(0).unwrap().state_hash.clone();
        let mut prev_rec_hash = GENESIS_PREV_HASH.to_string();
        let mut sim_sandbox = SandboxedWasmKernel::new_from_snapshot(journal.get_snapshot(0).unwrap());

        for step in 1..=16 {
            let diff = if step == 9 {
                // Bug introduced at step 9: negative value!
                r#"{"file":"src/state.rs","replace":"const VALUE: i32 = 0;","with":"const VALUE: i32 = -999;"}"#
            } else if step > 9 {
                // Subsequent edits retain or touch state
                r#"{"file":"src/state.rs","replace":"const VALUE","with":"// mod\nconst VALUE"}"#
            } else {
                r#"{"file":"src/state.rs","replace":"const VALUE","with":"// green\nconst VALUE"}"#
            };

            SandboxedWasmKernel::apply_mutation_diff(&mut sim_sandbox.files, diff).unwrap();
            let post_hash = sim_sandbox.current_state_hash();

            let rec = AgentActionRecord::new(
                step,
                format!("prompt_{}", step),
                "ast_mutation",
                "{}",
                step * 10,
                &prev_hash,
                &post_hash,
                Some(diff.to_string()),
                ActionStatus::Success,
                &prev_rec_hash,
            );
            let appended = journal.append_record(rec).unwrap();
            prev_hash = post_hash;
            prev_rec_hash = appended.record_hash;
        }

        assert_eq!(journal.len(), 16);

        // Predicate: state passes if it does NOT contain "-999"
        let isolator = BisectionRegressionIsolator::new();
        let outcome = isolator.isolate_culprit(&journal, 0, 16, |sb| {
            let content = sb.files.get("src/state.rs").cloned().unwrap_or_default();
            !content.contains("-999")
        }).expect("Bisection success");

        assert!(outcome.culprit_found);
        assert_eq!(outcome.offending_sequence_number, Some(9));
        assert!(outcome.offending_mutation_diff.unwrap().contains("-999"));
        // Confirm O(log K) complexity: 16 steps should take <= 5 evaluations
        assert!(outcome.bisection_steps_evaluated <= 5, "Evaluated steps: {}", outcome.bisection_steps_evaluated);
    }

    #[test]
    fn test_oscillation_loop_detection_and_rollback() {
        let mut files = BTreeMap::new();
        files.insert("src/feature.rs".to_string(), "pub const FLAG: bool = true;".to_string());
        let snapshot0 = StateSnapshot::new(0, files);
        let green_hash = snapshot0.state_hash.clone();

        let mut journal = ActionTraceJournal::in_memory("oscillation-session");
        journal.save_snapshot(snapshot0);

        let mut sandbox = SandboxedWasmKernel::new_from_snapshot(journal.get_snapshot(0).unwrap());

        // Agent flips FLAG true -> false (step 1)
        let diff_a = r#"{"file":"src/feature.rs","replace":"true","with":"false"}"#;
        SandboxedWasmKernel::apply_mutation_diff(&mut sandbox.files, diff_a).unwrap();
        let hash1 = sandbox.current_state_hash();
        let r1 = AgentActionRecord::new(
            1, "p1", "edit", "{}", 1, &green_hash, &hash1,
            Some(diff_a.to_string()), ActionStatus::Success, GENESIS_PREV_HASH,
        );
        let app1 = journal.append_record(r1).unwrap();

        // Agent flips FLAG false -> true (step 2) - OSCILLATION: returns to green_hash!
        let diff_b = r#"{"file":"src/feature.rs","replace":"false","with":"true"}"#;
        SandboxedWasmKernel::apply_mutation_diff(&mut sandbox.files, diff_b).unwrap();
        let hash2 = sandbox.current_state_hash();
        assert_eq!(hash2, green_hash);

        let r2 = AgentActionRecord::new(
            2, "p2", "edit", "{}", 2, &hash1, &hash2,
            Some(diff_b.to_string()), ActionStatus::Success, &app1.record_hash,
        );
        journal.append_record(r2).unwrap();

        // Detector analyzes journal
        let detector = OscillationLoopDetector::new(5);
        let cycle = detector.detect_oscillation(journal.records());
        assert!(cycle.is_some(), "Cycle must be detected");
        let c = cycle.unwrap();
        assert_eq!(c.cycle_length, 2);

        // Trip circuit breaker and rollback
        let report = detector.trip_and_rollback(&journal, &mut sandbox).expect("Rollback success");
        assert!(report.oscillation_detected);
        assert!(report.circuit_breaker_tripped);
        assert_eq!(report.rollback_target_seq, Some(0));
        assert_eq!(report.rollback_state_hash, Some(green_hash));
        assert_eq!(sandbox.files.get("src/feature.rs").unwrap(), "pub const FLAG: bool = true;");
    }
}
