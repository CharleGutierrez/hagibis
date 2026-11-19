#!/usr/bin/env python3
"""
test_circuit_breaker_hysteresis.py
================================================================================
Comprehensive Unit & Integration Test Suite for Pillar C:
  - Sliding-Window Anomaly Detection (dynamic anomaly score, rolling window, min events, thresholds)
  - Full Hysteresis State Machine (CLOSED -> OPEN -> HALF_OPEN -> RECOVERY -> CLOSED)
  - Strict Jittered Exponential Backoff:
      delay = min(max_backoff, base_delay * 2^(failures)) + uniform_jitter(0.1, 0.5)
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
# 3. Test Strict Jittered Exponential Backoff
# ==============================================================================

class TestJitteredExponentialBackoff(unittest.TestCase):
    """Verifies strict exponential backoff dampening and uniform jitter bounds."""

    def setUp(self):
        self.gov = AutonomicCircuitBreakerGovernor(t0=2.0, t_max=64.0)

    def test_backoff_monotonicity(self):
        expected_bases = [2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 64.0]
        for idx, base in enumerate(expected_bases, start=1):
            calculated = self.gov.calculate_backoff(idx, with_jitter=False)
            self.assertEqual(calculated, base)

    def test_jitter_within_specified_uniform_range(self):
        """Validates uniform jitter is strictly within [0.1, 0.5]."""
        for _ in range(100):
            val = self.gov.calculate_backoff(1, with_jitter=True)
            # Base is 2.0, jitter in [0.1, 0.5], so total in [2.10, 2.50]
            self.assertGreaterEqual(val, 2.10)
            self.assertLessEqual(val, 2.50)


# ==============================================================================
# 4. Test Immutable Cryptographic Audit Manifest & Verification
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
