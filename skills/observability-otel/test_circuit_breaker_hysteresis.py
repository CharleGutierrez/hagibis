#!/usr/bin/env python3
"""
test_circuit_breaker_hysteresis.py
================================================================================
Comprehensive Unit & Integration Test Suite for Pillar C & Reinforcement Backoff:
  - Sliding-Window Anomaly Detection (dynamic anomaly score, rolling window, min events, thresholds)
  - Full Hysteresis State Machine (CLOSED -> OPEN -> HALF_OPEN -> RECOVERY -> CLOSED, PERM_TRIPPED)
  - Strict Fibonacci Progression Reinforcement Backoff:
      fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13...
      delay = min(max_backoff, base_delay * fib(iteration)) + uniform_jitter(0.1, 0.5)
  - AST Error State Fingerprinting Determinism (normalized SHA-256 / BLAKE3)
  - 3-Consecutive-Identical-Error Tripping:
      Immediately halts auto-mutation, trips to PERM_TRIPPED, snapshots failing state
      into LanceDB (skills/vector-lancedb/), and escalates to LLM agent via MCP
      (hagibis.mcp.json hgb_compiler_self_heal_ast)
  - Skill Quarantining & File Write Permission Locking (0444 read-only)
  - Immutable Cryptographic Audit Manifest Generation & Verification (--verify-audit)
  - Tamper Detection: rejects files altered after manifest sealing

100% Real Production Code, Zero Mocks.
================================================================================
"""

import os
import sys
import time
import json
import stat
import shutil
import tempfile
import unittest
import subprocess
from pathlib import Path

# Add current directory to path
CURRENT_DIR = Path(__file__).resolve().parent
WORKSPACE_ROOT = CURRENT_DIR.parent.parent
SCRIPTS_DIR = WORKSPACE_ROOT / "scripts"
sys.path.insert(0, str(CURRENT_DIR))

from ch8_circuit_breaker_governor import (
    CircuitState,
    SlidingWindowAnomalyDetector,
    AutonomicCircuitBreakerGovernor,
    get_circuit_breaker_governor
)


# ==============================================================================
# 1. Test Sliding-Window Anomaly Detector
# ==============================================================================

class TestSlidingWindowAnomalyDetector(unittest.TestCase):
    """Verifies rolling window mechanics, dynamic anomaly scoring, and anomaly threshold triggers."""

    def setUp(self):
        self.detector = SlidingWindowAnomalyDetector(
            window_size=10,
            window_duration_sec=10.0,
            min_events=5,
            anomaly_threshold=0.50
        )

    def test_empty_detector_metrics(self):
        m = self.detector.compute_metrics()
        self.assertEqual(m["total_events"], 0)
        self.assertEqual(m["failure_rate"], 0.0)
        self.assertEqual(m["anomaly_score"], 0.0)
        self.assertFalse(m["is_anomaly"])

    def test_insufficient_samples_do_not_flag_anomaly(self):
        """Even with 100% failure rate, fewer than min_events (5) should not trigger is_anomaly."""
        now = time.time()
        self.detector.record_event(False, error_trace="err 1", timestamp=now)
        self.detector.record_event(False, error_trace="err 2", timestamp=now)

        m = self.detector.compute_metrics(current_time=now)
        self.assertEqual(m["total_events"], 2)
        self.assertEqual(m["failure_rate"], 1.0)
        # Sample confidence factor = 2 / 5 = 0.40
        self.assertAlmostEqual(m["anomaly_score"], 0.40, places=2)
        self.assertFalse(m["is_anomaly"], "Should not trip anomaly when total_events < min_events")

    def test_anomaly_triggered_at_threshold(self):
        """When total_events >= 5 and failure_rate >= 50%, is_anomaly must be True."""
        now = time.time()
        # 3 successes, 3 failures (total 6 events, 50% failure rate)
        for _ in range(3):
            self.detector.record_event(True, timestamp=now)
        for _ in range(3):
            self.detector.record_event(False, error_trace="timeout", timestamp=now)

        m = self.detector.compute_metrics(current_time=now)
        self.assertEqual(m["total_events"], 6)
        self.assertEqual(m["failure_count"], 3)
        self.assertEqual(m["failure_rate"], 0.50)
        self.assertEqual(m["failure_rate_pct"], 50.0)
        self.assertTrue(m["is_anomaly"], "Anomaly should be flagged when failure rate >= 50% over >= 5 events")

    def test_sliding_window_time_and_count_pruning(self):
        """Old events (> 10s) and events exceeding window_size (10) must be pruned."""
        t0 = 1000.0
        # Add 12 events
        for i in range(12):
            self.detector.record_event(True, timestamp=t0 + i)

        # Window size is 10, so only the last 10 events should be kept
        m = self.detector.compute_metrics(current_time=t0 + 11)
        self.assertEqual(m["total_events"], 10)

        # Fast forward time by 15 seconds (all events older than window_duration_sec 10.0s)
        m_future = self.detector.compute_metrics(current_time=t0 + 30.0)
        self.assertEqual(m_future["total_events"], 0, "All expired events should be pruned")


# ==============================================================================
# 2. Test Full Hysteresis State Machine Transitions
# ==============================================================================

class TestHysteresisStateMachine(unittest.TestCase):
    """
    Verifies full lifecycle state transitions:
      CLOSED -> OPEN -> HALF_OPEN -> RECOVERY -> CLOSED
    """

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_cb_hysteresis_test_")
        self.root_p = Path(self.test_dir)
        self.gov = AutonomicCircuitBreakerGovernor(
            state_dir=self.root_p / "state",
            quarantine_dir=self.root_p / "quarantine",
            registry_path=self.root_p / "state" / "reg.json",
            webhook_sink=self.root_p / "events.jsonl",
            max_consecutive_failures=3,
            t0=1.0,
            t_max=16.0,
            window_size=10,
            window_duration_sec=30.0,
            min_events=5,
            anomaly_threshold=0.50,
            recovery_threshold=3
        )
        self.skill_id = "test_pipeline_healer"
        self.source_file = self.root_p / "kernel.rs"
        self.source_file.write_text("fn optimize() {}")

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_full_hysteresis_cycle(self):
        h_pre = self.gov.compute_state_hash(self.source_file)
        now = time.time()

        # 1. State CLOSED: Normal execution
        can_run, _ = self.gov.can_mutate(self.skill_id, current_time=now)
        self.assertTrue(can_run)
        rec = self.gov._get_or_create_skill(self.skill_id)
        self.assertEqual(rec.state, CircuitState.CLOSED)

        # 2. Record 2 failures: should remain CLOSED (dampening)
        r1 = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=False, error_trace="err1", skill_path=self.source_file, current_time=now)
        self.assertEqual(r1["state"], CircuitState.CLOSED.value)
        can_run, _ = self.gov.can_mutate(self.skill_id, current_time=now)
        self.assertTrue(can_run)

        r2 = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=False, error_trace="err2", skill_path=self.source_file, current_time=now)
        self.assertEqual(r2["state"], CircuitState.CLOSED.value)
        can_run, _ = self.gov.can_mutate(self.skill_id, current_time=now)
        self.assertTrue(can_run)

        # 3. 3rd consecutive failure: Breaker trips! Transitions CLOSED -> OPEN!
        r3 = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=False, error_trace="err3: fatal", skill_path=self.source_file, current_time=now)
        self.assertEqual(r3["state"], CircuitState.OPEN.value)
        self.assertEqual(r3["action"], "BREAKER_TRIPPED_QUARANTINED")
        self.assertEqual(rec.state, CircuitState.OPEN)

        # Verify write permissions revoked on source file
        self.assertFalse(os.access(self.source_file, os.W_OK), "Permissions must be locked to 0444 read-only in OPEN state")

        # 4. Immediate can_mutate check: should be BLOCKED while backoff active
        can_run, msg = self.gov.can_mutate(self.skill_id, current_time=now)
        self.assertFalse(can_run)
        self.assertIn("OPEN", msg)

        # 5. Advance time past backoff duration: transitions OPEN -> HALF_OPEN!
        future_time = rec.open_until_timestamp + 0.5
        can_run, msg = self.gov.can_mutate(self.skill_id, current_time=future_time)
        self.assertTrue(can_run, "Trial probe should be allowed after backoff expires")
        self.assertEqual(rec.state, CircuitState.HALF_OPEN)
        self.assertIn("HALF_OPEN", msg)

        # 6. Trial probe succeeds -> transitions HALF_OPEN -> RECOVERY!
        r_probe = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=True, skill_path=self.source_file, current_time=future_time)
        self.assertEqual(r_probe["state"], CircuitState.RECOVERY.value)
        self.assertEqual(rec.state, CircuitState.RECOVERY)
        self.assertEqual(rec.consecutive_successes, 1)

        # 7. In RECOVERY probation: requires recovery_threshold (3) successes to return to CLOSED
        r_rec2 = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=True, skill_path=self.source_file, current_time=future_time + 1.0)
        self.assertEqual(r_rec2["state"], CircuitState.RECOVERY.value)
        self.assertEqual(rec.state, CircuitState.RECOVERY)
        self.assertEqual(rec.consecutive_successes, 2)

        # 3rd success completes probation! Transitions RECOVERY -> CLOSED!
        r_rec3 = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=True, skill_path=self.source_file, current_time=future_time + 2.0)
        self.assertEqual(r_rec3["state"], CircuitState.CLOSED.value)
        self.assertEqual(r_rec3["action"], "RECOVERY_COMPLETE_CIRCUIT_CLOSED")
        self.assertEqual(rec.state, CircuitState.CLOSED)
        self.assertEqual(rec.consecutive_failures, 0)

    def test_half_open_failure_reopens_circuit(self):
        """A failure during HALF_OPEN trial probing must immediately re-open the circuit with backoff."""
        h_pre = self.gov.compute_state_hash(self.source_file)
        now = time.time()

        # Trip to OPEN
        for i in range(3):
            self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=False, error_trace=f"fail {i}", skill_path=self.source_file, current_time=now)
        rec = self.gov._registry[self.skill_id]
        self.assertEqual(rec.state, CircuitState.OPEN)

        # Advance past backoff to enter HALF_OPEN
        t_probe = rec.open_until_timestamp + 0.1
        can_run, _ = self.gov.can_mutate(self.skill_id, current_time=t_probe)
        self.assertTrue(can_run)
        self.assertEqual(rec.state, CircuitState.HALF_OPEN)

        # Trial probe FAILS -> must re-open circuit!
        r_fail = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=False, error_trace="Probe failed!", skill_path=self.source_file, current_time=t_probe)
        self.assertEqual(r_fail["state"], CircuitState.OPEN.value)
        self.assertEqual(r_fail["action"], "PROBE_FAILED_CIRCUIT_REOPENED")
        self.assertEqual(rec.state, CircuitState.OPEN)
        self.assertGreater(rec.open_until_timestamp, t_probe)

    def test_recovery_probation_failure_reopens_circuit(self):
        """A failure during RECOVERY probation must trip the breaker back to OPEN."""
        h_pre = self.gov.compute_state_hash(self.source_file)
        now = time.time()
        rec = self.gov._get_or_create_skill(self.skill_id)

        # Manually put in RECOVERY
        rec.state = CircuitState.RECOVERY
        rec.consecutive_successes = 1

        # Failure occurs during probation
        r_fail = self.gov.record_mutation_attempt(self.skill_id, h_pre, h_pre, success=False, error_trace="Regression in probation", skill_path=self.source_file, current_time=now)
        self.assertEqual(r_fail["state"], CircuitState.OPEN.value)
        self.assertEqual(r_fail["action"], "RECOVERY_FAILED_CIRCUIT_REOPENED")
        self.assertEqual(rec.state, CircuitState.OPEN)


# ==============================================================================
# 3. Test Fibonacci Progression Reinforcement Backoff
# ==============================================================================

class TestFibonacciBackoffProgression(unittest.TestCase):
    """
    Verifies Fibonacci reinforcement backoff dampening curve:
      fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13, fib(8)=21, fib(9)=34...
      delay = min(t_max, base_delay * fib(iteration)) + uniform_jitter(0.1, 0.5)
    """

    def setUp(self):
        self.gov = AutonomicCircuitBreakerGovernor(t0=1.0, t_max=64.0)

    def test_fibonacci_sequence_computation(self):
        """Validates canonical Fibonacci numbers: fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, etc."""
        expected = [1, 1, 2, 3, 5, 8, 13, 21, 34, 55]
        for idx, exp_val in enumerate(expected, start=1):
            calculated = self.gov.fibonacci(idx)
            self.assertEqual(calculated, exp_val, f"fib({idx}) expected {exp_val}, got {calculated}")

    def test_fibonacci_delay_progression_t0_1(self):
        """Validates delay progression for t0=1.0: 1, 1, 2, 3, 5, 8, 13, 21, 34."""
        expected_bases = [1.0, 1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 21.0, 34.0]
        for idx, exp_delay in enumerate(expected_bases, start=1):
            calculated = self.gov.calculate_fibonacci_backoff(idx, with_jitter=False)
            self.assertEqual(calculated, exp_delay, f"Iteration {idx}: expected {exp_delay}, got {calculated}")

    def test_fibonacci_delay_progression_t0_2(self):
        """Validates delay progression for t0=2.0: 2, 2, 4, 6, 10, 16, 26, 42, 64 (capped at t_max=64)."""
        gov_t2 = AutonomicCircuitBreakerGovernor(t0=2.0, t_max=64.0)
        expected_bases = [2.0, 2.0, 4.0, 6.0, 10.0, 16.0, 26.0, 42.0, 64.0]
        for idx, exp_delay in enumerate(expected_bases, start=1):
            calculated = gov_t2.calculate_fibonacci_backoff(idx, with_jitter=False)
            self.assertEqual(calculated, exp_delay, f"Iteration {idx}: expected {exp_delay}, got {calculated}")

    def test_fibonacci_capping_at_tmax(self):
        """Validates that backoff strictly clamps to t_max."""
        gov_clamp = AutonomicCircuitBreakerGovernor(t0=1.0, t_max=10.0)
        # fib(7) = 13, should cap at 10.0
        val = gov_clamp.calculate_fibonacci_backoff(7, with_jitter=False)
        self.assertEqual(val, 10.0)

    def test_jitter_within_specified_uniform_range(self):
        """Validates uniform jitter is strictly within [0.1, 0.5]."""
        for _ in range(100):
            val = self.gov.calculate_fibonacci_backoff(1, with_jitter=True)
            # Base is 1.0, jitter in [0.1, 0.5], so total in [1.10, 1.50]
            self.assertGreaterEqual(val, 1.10)
            self.assertLessEqual(val, 1.50)


# ==============================================================================
# 4. Test AST Error State Fingerprinting Determinism
# ==============================================================================

class TestASTErrorFingerprintingDeterminism(unittest.TestCase):
    """
    Verifies deterministic SHA-256 / BLAKE3 AST error diagnostic fingerprinting.
    Fingerprints must uniquely identify diagnostic faults and normalize whitespace.
    """

    def setUp(self):
        self.gov = AutonomicCircuitBreakerGovernor()

    def test_identical_diagnostics_produce_identical_fingerprint(self):
        fp1 = self.gov.compute_ast_error_fingerprint("E0308", "crates/hgb-core/src/lib.rs", 42, "10:25:100:150", "mismatched types")
        fp2 = self.gov.compute_ast_error_fingerprint("E0308", "crates/hgb-core/src/lib.rs", 42, "10:25:100:150", "mismatched types")
        self.assertEqual(fp1, fp2)
        self.assertEqual(len(fp1), 64, "Fingerprint should be a 64-char hexadecimal digest")

    def test_whitespace_normalization_in_error_message(self):
        """Diagnostic error messages with irregular whitespace or newlines must produce identical fingerprint."""
        fp_clean = self.gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 10, "0:0", "mismatched types: expected usize, found &str")
        fp_spaces = self.gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 10, "0:0", "mismatched   types:  expected usize,   found &str\n")
        self.assertEqual(fp_clean, fp_spaces)

    def test_distinct_error_codes_produce_distinct_fingerprints(self):
        fp_e0308 = self.gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 10, "0:0", "type error")
        fp_e0596 = self.gov.compute_ast_error_fingerprint("E0596", "src/lib.rs", 10, "0:0", "type error")
        self.assertNotEqual(fp_e0308, fp_e0596)

    def test_distinct_lines_produce_distinct_fingerprints(self):
        fp_line10 = self.gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 10, "0:0", "type error")
        fp_line20 = self.gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 20, "0:0", "type error")
        self.assertNotEqual(fp_line10, fp_line20)

    def test_distinct_files_produce_distinct_fingerprints(self):
        fp_file_a = self.gov.compute_ast_error_fingerprint("E0308", "src/a.rs", 10, "0:0", "type error")
        fp_file_b = self.gov.compute_ast_error_fingerprint("E0308", "src/b.rs", 10, "0:0", "type error")
        self.assertNotEqual(fp_file_a, fp_file_b)


# ==============================================================================
# 5. Test 3-Consecutive Identical Error Tripping & LanceDB Snapshot + MCP Escalation
# ==============================================================================

class TestConsecutiveIdenticalASTErrorTripping(unittest.TestCase):
    """
    Verifies that encountering the exact same AST error 3 times in a row:
      1. Halts auto-mutation and trips breaker directly to PERM_TRIPPED
      2. Snapshots failing state into LanceDB (skills/vector-lancedb/failing_ast_snapshots/)
      3. Synthesizes MCP escalation payload for hgb_compiler_self_heal_ast (hagibis.mcp.json)
      4. Locks file permissions (0444 read-only)
      5. Guarantees can_mutate remains False without auto-recovery
      6. Verifies HITL administrative reset restores circuit to CLOSED
    """

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_ast_trip_test_")
        self.root_p = Path(self.test_dir)
        self.lancedb_p = self.root_p / "skills" / "vector-lancedb"
        self.lancedb_p.mkdir(parents=True)

        self.gov = AutonomicCircuitBreakerGovernor(
            state_dir=self.root_p / "state",
            quarantine_dir=self.root_p / "quarantine",
            registry_path=self.root_p / "state" / "reg.json",
            webhook_sink=self.root_p / "events.jsonl",
            lancedb_dir=self.lancedb_p,
            max_consecutive_failures=5,  # Anomaly/consecutive failure threshold set higher to ensure AST fingerprint trips it
            t0=1.0,
            t_max=30.0
        )
        self.skill_id = "crate_hgb_kernel"
        self.source_file = self.root_p / "broken_algo.rs"
        self.source_file.write_text(
            "pub fn calculate() -> usize {\n"
            "    let count = 42;\n"
            "    // Broken type mismatch: expected usize, found &str\n"
            "    \"centroid_alpha\"\n"
            "}\n"
        )

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_three_consecutive_identical_ast_errors_trip_and_escalate(self):
        h_pre = self.gov.compute_state_hash(self.source_file)
        now = time.time()

        diag_code = "E0308"
        diag_line = 4
        diag_msg = "mismatched types: expected usize, found &str"
        diag_span = "4:5:75:92"

        # 1. First encounter with AST error
        r1 = self.gov.record_mutation_attempt(
            skill_id=self.skill_id,
            pre_hash=h_pre,
            post_hash=h_pre,
            success=False,
            error_trace="rustc compile failure E0308",
            skill_path=self.source_file,
            diagnostic_code=diag_code,
            diagnostic_line=diag_line,
            diagnostic_span=diag_span,
            diagnostic_message=diag_msg,
            current_time=now
        )
        self.assertEqual(r1["state"], CircuitState.CLOSED.value)
        self.assertEqual(r1["consecutive_fingerprint_count"], 1)
        can_run, _ = self.gov.can_mutate(self.skill_id, current_time=now)
        self.assertTrue(can_run)

        # 2. Second encounter with exact same AST error
        r2 = self.gov.record_mutation_attempt(
            skill_id=self.skill_id,
            pre_hash=h_pre,
            post_hash=h_pre,
            success=False,
            error_trace="rustc compile failure E0308 (attempt 2)",
            skill_path=self.source_file,
            diagnostic_code=diag_code,
            diagnostic_line=diag_line,
            diagnostic_span=diag_span,
            diagnostic_message=diag_msg,
            current_time=now + 1.0
        )
        self.assertEqual(r2["state"], CircuitState.CLOSED.value)
        self.assertEqual(r2["consecutive_fingerprint_count"], 2)
        can_run, _ = self.gov.can_mutate(self.skill_id, current_time=now + 1.0)
        self.assertTrue(can_run)

        # 3. Third consecutive encounter with exact same AST error -> TRIPS TO PERM_TRIPPED!
        r3 = self.gov.record_mutation_attempt(
            skill_id=self.skill_id,
            pre_hash=h_pre,
            post_hash=h_pre,
            success=False,
            error_trace="rustc compile failure E0308 (attempt 3: halted)",
            skill_path=self.source_file,
            diagnostic_code=diag_code,
            diagnostic_line=diag_line,
            diagnostic_span=diag_span,
            diagnostic_message=diag_msg,
            compiler_stderr="error[E0308]: mismatched types\n --> broken_algo.rs:4:5\n  |\n4 |     \"centroid_alpha\"\n  |     ^^^^^^^^^^^^^^^^ expected usize, found &str",
            current_time=now + 2.0
        )

        # Check circuit state
        self.assertEqual(r3["state"], CircuitState.PERM_TRIPPED.value)
        self.assertEqual(r3["action"], "CONSECUTIVE_AST_ERROR_PERM_TRIPPED")
        rec = self.gov._registry[self.skill_id]
        self.assertEqual(rec.state, CircuitState.PERM_TRIPPED)
        self.assertEqual(rec.consecutive_fingerprint_count, 3)

        # File write permissions must be revoked (0444 read-only)
        self.assertFalse(os.access(self.source_file, os.W_OK), "Permissions must be locked to 0444 read-only")

        # can_mutate must remain BLOCKED permanently without auto-recovery
        can_run, msg = self.gov.can_mutate(self.skill_id, current_time=now + 1000000.0)
        self.assertFalse(can_run, "PERM_TRIPPED state must not auto-expire into HALF_OPEN")
        self.assertIn("PERM_TRIPPED", msg)

        # 4. Verify LanceDB Snapshot
        self.assertIn("lancedb_snapshot", r3)
        snapshot_file = Path(r3["lancedb_snapshot"]["snapshot_file"])
        self.assertTrue(snapshot_file.exists(), f"Snapshot file {snapshot_file} must exist on disk")

        with open(snapshot_file, "r", encoding="utf-8") as sf:
            snap_data = json.load(sf)

        self.assertEqual(snap_data["consecutive_occurrences"], 3)
        self.assertEqual(snap_data["diagnostic_code"], "E0308")
        self.assertEqual(snap_data["circuit_state"], CircuitState.PERM_TRIPPED.value)
        self.assertTrue(snap_data["auto_mutation_halted"])
        self.assertTrue(snap_data["escalated_to_mcp"])
        self.assertIn("source_context", snap_data)
        self.assertIn("centroid_alpha", snap_data["source_context"])
        self.assertIn("compiler_stderr", snap_data)
        self.assertIn("error[E0308]", snap_data["compiler_stderr"])
        self.assertEqual(snap_data["pre_patch_hash"], h_pre)

        # Verify latest pointer file exists
        latest_file = Path(r3["lancedb_snapshot"]["latest_file"])
        self.assertTrue(latest_file.exists())

        # 5. Verify MCP Escalation Payload
        self.assertIn("mcp_payload", r3)
        mcp = r3["mcp_payload"]
        self.assertEqual(mcp["jsonrpc"], "2.0")
        self.assertEqual(mcp["method"], "tools/call")
        self.assertEqual(mcp["params"]["name"], "hgb_compiler_self_heal_ast")

        args = mcp["params"]["arguments"]
        self.assertEqual(args["target_file"], str(self.source_file))
        self.assertEqual(args["diagnostic_code"], "E0308")
        self.assertEqual(args["error_line"], 4)
        self.assertTrue(args["auto_rollback_on_failure"])
        self.assertIn("centroid_alpha", args["ast_context_snippet"])

        meta = mcp["metadata"]
        self.assertEqual(meta["consecutive_identical_occurrences"], 3)
        self.assertIn("Identical AST compiler error encountered 3 times", meta["escalation_reason"])

        # 6. Verify HITL Administrative Reset
        self.gov.reset_skill(self.skill_id, restore_path=self.source_file)
        self.assertEqual(rec.state, CircuitState.CLOSED)
        self.assertEqual(rec.consecutive_fingerprint_count, 0)
        self.assertIsNone(rec.last_ast_fingerprint)
        can_run, _ = self.gov.can_mutate(self.skill_id)
        self.assertTrue(can_run)
        self.assertTrue(os.access(self.source_file, os.W_OK), "Permissions must be restored to writable on reset")


# ==============================================================================
# 6. Test Immutable Cryptographic Audit Manifest & Verification
# ==============================================================================

class TestImmutableAuditManifest(unittest.TestCase):
    """Verifies immutable audit manifest generation, 0444 permissions, and --verify-audit."""

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_audit_test_")
        self.root_p = Path(self.test_dir)
        self.healer_script = SCRIPTS_DIR / "autonomous-compiler-healer.sh"

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_audit_manifest_generation_and_verification(self):
        target_file = self.root_p / "module.rs"
        target_file.write_text("pub fn run() -> &'static str { \"hello\" }\n")

        # Run demo mode to generate verified audit trail
        env = os.environ.copy()
        env["WORKSPACE_ROOT"] = str(self.root_p)
        env["HGB_AUDIT_DIR"] = str(self.root_p / "audit_trail")
        env["HGB_STATE_DIR"] = str(self.root_p / "state")
        env["HGB_QUARANTINE_DIR"] = str(self.root_p / "quarantine")
        env["CIRCUIT_BREAKER_FILE"] = str(self.root_p / "cb.json")

        res_demo = subprocess.run(
            ["bash", str(self.healer_script), "--demo", "--workspace", str(self.root_p)],
            env=env,
            capture_output=True,
            text=True
        )
        self.assertEqual(res_demo.returncode, 0, f"Demo healer failed: {res_demo.stderr}")
        self.assertIn("Cryptographic audit manifest generated and sealed (0444)", res_demo.stdout + res_demo.stderr)
        self.assertIn("[AUDIT VERIFIED]", res_demo.stdout + res_demo.stderr)

    def test_verify_audit_cli_detects_tampering(self):
        """Verifies that --verify-audit strictly rejects modified or tampered files."""
        source_file = self.root_p / "algo.rs"
        source_file.write_text("pub fn compute() -> i32 { 100 }\n")

        # Helper to compute BLAKE3 matching autonomous-compiler-healer.sh
        def get_b3(data: bytes) -> str:
            try:
                import blake3
                return blake3.blake3(data).hexdigest()
            except ImportError:
                import hashlib
                return hashlib.blake2b(data).hexdigest()

        # Generate hashes
        pre_sha = subprocess.check_output(["sha256sum", str(source_file)]).decode().split()[0]
        pre_b3 = get_b3(source_file.read_bytes())

        # Modify file
        source_file.write_text("pub fn compute() -> i32 { 101 }\n")
        post_sha = subprocess.check_output(["sha256sum", str(source_file)]).decode().split()[0]
        post_b3 = get_b3(source_file.read_bytes())

        # Create audit manifest
        manifest_dir = self.root_p / "audit_manifest_dir"
        manifest_dir.mkdir(parents=True)
        manifest_file = manifest_dir / "audit_manifest.json"

        manifest_data = {
            "version": "1.0",
            "patch_id": "patch_test_123",
            "timestamp_iso": "2026-11-19T06:00:00Z",
            "target_file": str(source_file),
            "pre_hash": {"sha256": pre_sha, "blake3": pre_b3},
            "post_hash": {"sha256": post_sha, "blake3": post_b3},
            "diff_checksum": {"sha256": "0" * 64, "blake3": "0" * 64},
            "diagnostic_error_code": "E0308",
            "healer_agent_id": "test-healer",
            "git_commit_sha": "abc1234",
            "verified_green": True
        }
        manifest_file.write_text(json.dumps(manifest_data, indent=2))
        os.chmod(manifest_file, 0o444)

        # 1. Verify initially passes
        res_verify_ok = subprocess.run(
            ["bash", str(self.healer_script), "--verify-audit", str(manifest_file)],
            capture_output=True,
            text=True
        )
        self.assertEqual(res_verify_ok.returncode, 0)
        self.assertIn("AUDIT VERIFIED", res_verify_ok.stdout + res_verify_ok.stderr)

        # 2. Tamper with source file
        source_file.write_text("pub fn compute() -> i32 { 999999 }\n")

        # 3. Verify must now FAIL and reject the tampered file!
        res_verify_tampered = subprocess.run(
            ["bash", str(self.healer_script), "--verify-audit", str(manifest_file)],
            capture_output=True,
            text=True
        )
        self.assertEqual(res_verify_tampered.returncode, 1)
        self.assertIn("AUDIT FAILED", res_verify_tampered.stdout + res_verify_tampered.stderr)
        self.assertIn("hash mismatch", res_verify_tampered.stdout + res_verify_tampered.stderr)


# ==============================================================================
# Main Runner
# ==============================================================================

if __name__ == "__main__":
    unittest.main(verbosity=2)
