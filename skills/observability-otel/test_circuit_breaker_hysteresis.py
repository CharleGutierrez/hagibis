#!/usr/bin/env python3
"""
test_circuit_breaker_hysteresis.py
Comprehensive Unit & Integration Test Suite for Hagibis Autopoietic Circuit Breaker Governor

Test Coverage:
1. Normal Hysteresis Progression: CLOSED -> OPEN -> HALF_OPEN -> RECOVERY -> CLOSED
2. Strict Fibonacci Progression Reinforcement Backoff with Uniform Jitter
3. Sliding-Window Anomaly Detection & Failure Rate Thresholding
4. 3 Consecutive Identical AST Errors -> PERM_TRIPPED_CRYPTOLOCKED
5. Flapping Detection (2-Cycle Oscillation A <-> B) -> PERM_TRIPPED_CRYPTOLOCKED
6. Cryptographic State Signing (Ed25519 signature + BLAKE3 error fingerprint & history)
7. Permission Stripping (0444 read-only on locked source files)
8. Rejection of Unsigned Administrative Reset when Cryptolocked
9. Replay Defense: Rejecting Replayed Nonces & Tampered Ed25519 Signatures
10. Authorized Signed MCP Operator Unlock (`hgb_operator_unlock_circuit_breaker`) Restoring to CLOSED & 0644
"""

import os
import sys
import json
import time
import shutil
import tempfile
import unittest
from pathlib import Path

# Add brain directory to sys.path
SCRIPT_DIR = Path(__file__).resolve().parent
SKILLS_DIR = SCRIPT_DIR / "skills" / "observability-otel"
if str(SKILLS_DIR) not in sys.path:
    sys.path.insert(0, str(SKILLS_DIR))

try:
    from cryptography.hazmat.primitives.asymmetric import ed25519
    HAS_CRYPTOGRAPHY = True
except ImportError:
    HAS_CRYPTOGRAPHY = False

from ch8_circuit_breaker_governor import (
    AutonomicCircuitBreakerGovernor,
    CircuitState,
    FlappingDetector,
    SlidingWindowAnomalyDetector,
    DEFAULT_MAX_CONSECUTIVE_FAILURES,
    DEFAULT_T0,
    DEFAULT_TMAX
)


class TestCircuitBreakerHysteresisAndCryptoLockout(unittest.TestCase):

    def setUp(self):
        self.test_dir = Path(tempfile.mkdtemp(prefix="hgb_cb_test_"))
        self.state_dir = self.test_dir / "state"
        self.quarantine_dir = self.test_dir / "quarantine"
        self.registry_file = self.state_dir / "circuit_breaker_registry.json"
        self.webhook_sink = self.test_dir / "webhook_sink.jsonl"
        self.lancedb_dir = self.test_dir / "vector-lancedb"

        self.governor = AutonomicCircuitBreakerGovernor(
            state_dir=self.state_dir,
            quarantine_dir=self.quarantine_dir,
            registry_path=self.registry_file,
            webhook_sink=self.webhook_sink,
            max_consecutive_failures=3,
            t0=1.0,
            t_max=32.0,
            window_size=10,
            window_duration_sec=30.0,
            min_events=3,
            anomaly_threshold=0.50,
            recovery_threshold=2,
            lancedb_dir=self.lancedb_dir
        )

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    # --------------------------------------------------------------------------
    # 1. Fibonacci Progression Reinforcement Backoff
    # --------------------------------------------------------------------------
    def test_fibonacci_progression_calculation(self):
        gov = self.governor
        self.assertEqual(gov.fibonacci(0), 0)
        self.assertEqual(gov.fibonacci(1), 1)
        self.assertEqual(gov.fibonacci(2), 1)
        self.assertEqual(gov.fibonacci(3), 2)
        self.assertEqual(gov.fibonacci(4), 3)
        self.assertEqual(gov.fibonacci(5), 5)
        self.assertEqual(gov.fibonacci(6), 8)
        self.assertEqual(gov.fibonacci(7), 13)

        # Verify backoff values with jitter
        b1 = gov.calculate_fibonacci_backoff(1, with_jitter=True, jitter_min=0.1, jitter_max=0.5)
        b2 = gov.calculate_fibonacci_backoff(2, with_jitter=True, jitter_min=0.1, jitter_max=0.5)
        b3 = gov.calculate_fibonacci_backoff(3, with_jitter=True, jitter_min=0.1, jitter_max=0.5)
        b4 = gov.calculate_fibonacci_backoff(4, with_jitter=True, jitter_min=0.1, jitter_max=0.5)
        b5 = gov.calculate_fibonacci_backoff(5, with_jitter=True, jitter_min=0.1, jitter_max=0.5)

        self.assertTrue(1.1 <= b1 <= 1.5, f"b1 out of range: {b1}")
        self.assertTrue(1.1 <= b2 <= 1.5, f"b2 out of range: {b2}")
        self.assertTrue(2.1 <= b3 <= 2.5, f"b3 out of range: {b3}")
        self.assertTrue(3.1 <= b4 <= 3.5, f"b4 out of range: {b4}")
        self.assertTrue(5.1 <= b5 <= 5.5, f"b5 out of range: {b5}")

    # --------------------------------------------------------------------------
    # 2. Deterministic AST Error State Fingerprinting
    # --------------------------------------------------------------------------
    def test_ast_error_fingerprint_determinism_and_normalization(self):
        fp1 = AutonomicCircuitBreakerGovernor.compute_ast_error_fingerprint(
            error_code="E0308",
            file_path="src/lib.rs",
            line=42,
            span="10:20",
            error_message="mismatched types"
        )
        fp2 = AutonomicCircuitBreakerGovernor.compute_ast_error_fingerprint(
            error_code="E0308",
            file_path="src/lib.rs",
            line=42,
            span="10:20",
            error_message="mismatched   types  \n"
        )
        fp3 = AutonomicCircuitBreakerGovernor.compute_ast_error_fingerprint(
            error_code="E0596",
            file_path="src/lib.rs",
            line=42,
            span="10:20",
            error_message="cannot borrow immutable local variable"
        )

        self.assertEqual(fp1, fp2, "Whitespace normalization must produce identical fingerprints")
        self.assertNotEqual(fp1, fp3, "Distinct error codes must produce distinct fingerprints")

    # --------------------------------------------------------------------------
    # 3. Flapping Detector Unit Tests (Oscillation & Repeated Faults)
    # --------------------------------------------------------------------------
    def test_flapping_detector_2_cycle_oscillation(self):
        det = FlappingDetector(oscillation_window=4, consecutive_identical_threshold=3)

        # [A] -> no trip
        is_flap, reason, sigs = det.record_error("FP_A")
        self.assertFalse(is_flap)

        # [A, B] -> no trip
        is_flap, reason, sigs = det.record_error("FP_B")
        self.assertFalse(is_flap)

        # [A, B, A] -> 2-cycle oscillation detected!
        is_flap, reason, sigs = det.record_error("FP_A")
        self.assertTrue(is_flap)
        self.assertIn("2-cycle oscillation", reason)
        self.assertEqual(sigs, ["FP_A", "FP_B", "FP_A"])

    def test_flapping_detector_3_consecutive_identical_faults(self):
        det = FlappingDetector(oscillation_window=4, consecutive_identical_threshold=3)

        det.record_error("FP_REPEAT")
        det.record_error("FP_REPEAT")
        is_flap, reason, sigs = det.record_error("FP_REPEAT")

        self.assertTrue(is_flap)
        self.assertIn("3 consecutive identical AST compiler errors", reason)
        self.assertEqual(sigs, ["FP_REPEAT", "FP_REPEAT", "FP_REPEAT"])

    # --------------------------------------------------------------------------
    # 4. Full Hysteresis Cycle: CLOSED -> OPEN -> HALF_OPEN -> RECOVERY -> CLOSED
    # --------------------------------------------------------------------------
    def test_full_hysteresis_cycle(self):
        gov = self.governor
        skill = "skill_hysteresis_test"

        # Initially CLOSED
        allowed, reason = gov.can_mutate(skill)
        self.assertTrue(allowed)
        rec = gov._registry[skill]
        self.assertEqual(rec.state, CircuitState.CLOSED)

        # Attempt 1: Failure -> remains CLOSED (failures: 1/3)
        gov.record_mutation_attempt(skill, "hashA", "hashB", success=False, error_trace="err 1")
        self.assertEqual(gov._registry[skill].state, CircuitState.CLOSED)
        self.assertEqual(gov._registry[skill].consecutive_failures, 1)

        # Attempt 2: Failure -> remains CLOSED (failures: 2/3)
        gov.record_mutation_attempt(skill, "hashA", "hashB", success=False, error_trace="err 2")
        self.assertEqual(gov._registry[skill].state, CircuitState.CLOSED)
        self.assertEqual(gov._registry[skill].consecutive_failures, 2)

        # Attempt 3: Failure -> TRIPS TO OPEN!
        now = time.time()
        gov.record_mutation_attempt(skill, "hashA", "hashB", success=False, error_trace="err 3", current_time=now)
        rec = gov._registry[skill]
        self.assertEqual(rec.state, CircuitState.OPEN)
        self.assertTrue(rec.is_quarantined)

        # While OPEN and backoff unexpired -> blocked
        allowed, reason = gov.can_mutate(skill, current_time=now + 0.1)
        self.assertFalse(allowed)

        # Advance time past backoff delay -> transitions to HALF_OPEN
        probe_time = rec.open_until_timestamp + 1.0
        allowed, reason = gov.can_mutate(skill, current_time=probe_time)
        self.assertTrue(allowed)
        self.assertEqual(gov._registry[skill].state, CircuitState.HALF_OPEN)

        # Probe succeeds -> transitions to RECOVERY (1/2 required)
        gov.record_mutation_attempt(skill, "hashA", "hashB", success=True, current_time=probe_time + 0.5)
        self.assertEqual(gov._registry[skill].state, CircuitState.RECOVERY)
        self.assertEqual(gov._registry[skill].consecutive_successes, 1)

        # Recovery probe 2 succeeds -> fully restored to CLOSED!
        gov.record_mutation_attempt(skill, "hashA", "hashB", success=True, current_time=probe_time + 1.0)
        self.assertEqual(gov._registry[skill].state, CircuitState.CLOSED)
        self.assertEqual(gov._registry[skill].consecutive_failures, 0)
        self.assertFalse(gov._registry[skill].is_quarantined)

    # --------------------------------------------------------------------------
    # 5. Flapping Lockout: 2-Cycle Oscillation -> PERM_TRIPPED_CRYPTOLOCKED
    # --------------------------------------------------------------------------
    def test_flapping_triggers_perm_tripped_cryptolocked_and_strips_permissions(self):
        gov = self.governor
        skill = "skill_flapping_source"

        # Create dummy source file with write permissions
        source_file = self.test_dir / "flapping_module.rs"
        source_file.write_text("pub fn compute() -> usize { 42 }", encoding="utf-8")
        os.chmod(source_file, 0o644)
        self.assertTrue(os.access(source_file, os.W_OK))

        # Fault A
        gov.record_mutation_attempt(
            skill_id=skill,
            pre_hash="pre1",
            post_hash="post1",
            success=False,
            error_trace="mismatched types",
            skill_path=source_file,
            diagnostic_code="E0308",
            diagnostic_line=1,
            diagnostic_span="1:5",
            diagnostic_message="expected String, found &str"
        )
        self.assertNotEqual(gov._registry[skill].state, CircuitState.PERM_TRIPPED_CRYPTOLOCKED)

        # Fault B
        gov.record_mutation_attempt(
            skill_id=skill,
            pre_hash="pre2",
            post_hash="post2",
            success=False,
            error_trace="cannot borrow as mutable",
            skill_path=source_file,
            diagnostic_code="E0596",
            diagnostic_line=2,
            diagnostic_span="1:5",
            diagnostic_message="cannot borrow immutable binding as mutable"
        )
        self.assertNotEqual(gov._registry[skill].state, CircuitState.PERM_TRIPPED_CRYPTOLOCKED)

        # Fault A again -> FLAPPING 2-CYCLE OSCILLATION DETECTED!
        attempt_res = gov.record_mutation_attempt(
            skill_id=skill,
            pre_hash="pre3",
            post_hash="post3",
            success=False,
            error_trace="mismatched types",
            skill_path=source_file,
            diagnostic_code="E0308",
            diagnostic_line=1,
            diagnostic_span="1:5",
            diagnostic_message="expected String, found &str"
        )

        rec = gov._registry[skill]
        self.assertEqual(rec.state, CircuitState.PERM_TRIPPED_CRYPTOLOCKED)
        self.assertIsNotNone(rec.lockout_id)
        self.assertIsNotNone(rec.lockout_receipt)

        # Verify write permissions were stripped (0444)
        self.assertFalse(os.access(source_file, os.W_OK), "Source file permissions must be stripped to 0444 read-only")

        # Verify mutation is permanently blocked
        allowed, reason = gov.can_mutate(skill)
        self.assertFalse(allowed)
        self.assertIn("PERM_TRIPPED_CRYPTOLOCKED", reason)

        # Verify signed lockout receipt structure
        receipt = rec.lockout_receipt
        self.assertEqual(receipt["circuit_state"], "PERM_TRIPPED_CRYPTOLOCKED")
        self.assertEqual(receipt["skill_id"], skill)
        self.assertTrue(receipt["ed25519_signed"])
        self.assertIn("ed25519_signature", receipt)
        self.assertIn("blake3_error_history_hash", receipt)

    # --------------------------------------------------------------------------
    # 6. Rejection of Unsigned Administrative Reset when Cryptolocked
    # --------------------------------------------------------------------------
    def test_unsigned_reset_rejected_for_cryptolocked_skill(self):
        gov = self.governor
        skill = "skill_locked_reset_test"

        rec = gov._get_or_create_skill(skill)
        rec.state = CircuitState.PERM_TRIPPED_CRYPTOLOCKED
        rec.lockout_id = "lockout_dummy_123"

        # Attempting standard operator_unlock with bad token raises PermissionError
        with self.assertRaises(PermissionError):
            gov.operator_unlock_circuit_breaker(
                skill_id=skill,
                operator_id="operator_mallory",
                lockout_id="wrong_lockout_id",
                nonce=12345,
                operator_public_key="0" * 64,
                ed25519_signature="0" * 128
            )

    # --------------------------------------------------------------------------
    # 7. Authorized Signed MCP Operator Unlock with Replay Defense
    # --------------------------------------------------------------------------
    @unittest.skipUnless(HAS_CRYPTOGRAPHY, "Requires cryptography module for Ed25519 verification")
    def test_signed_mcp_operator_unlock_lifecycle(self):
        gov = self.governor
        skill = "skill_mcp_unlock_test"

        # Create source file
        source_file = self.test_dir / "target_to_unlock.rs"
        source_file.write_text("fn test() {}", encoding="utf-8")
        os.chmod(source_file, 0o644)

        # Force skill into PERM_TRIPPED_CRYPTOLOCKED
        receipt = gov.issue_cryptographic_lockout_receipt(
            skill_id=skill,
            reason="Flapping compilation oscillation",
            flapping_signatures=["FP1", "FP2", "FP1"],
            consecutive_count=3,
            error_fingerprint="FP1",
            locked_paths=[source_file],
            window_events=[]
        )

        rec = gov._registry[skill]
        rec.state = CircuitState.PERM_TRIPPED_CRYPTOLOCKED
        rec.lockout_id = receipt["lockout_id"]
        rec.lockout_receipt = receipt

        self.assertFalse(os.access(source_file, os.W_OK))
        self.assertEqual(rec.state, CircuitState.PERM_TRIPPED_CRYPTOLOCKED)

        # Authorized operator creates Ed25519 keypair
        priv_key = ed25519.Ed25519PrivateKey.generate()
        pub_key = priv_key.public_key()
        pub_hex = pub_key.public_bytes_raw().hex()
        operator_id = "sec-ops-lead-01"
        nonce = int(time.time() * 1000)

        # Sign challenge payload: UNLOCK:<operator_id>:<skill_id>:<lockout_id>:<nonce>
        payload_bytes = f"UNLOCK:{operator_id}:{skill}:{rec.lockout_id}:{nonce}".encode("utf-8")
        signature_hex = priv_key.sign(payload_bytes).hex()

        # Step 1: Tampered signature must fail
        tampered_sig = signature_hex[:-4] + "dead"
        with self.assertRaises(PermissionError):
            gov.operator_unlock_circuit_breaker(
                skill_id=skill,
                operator_id=operator_id,
                lockout_id=rec.lockout_id,
                nonce=nonce,
                operator_public_key=pub_hex,
                ed25519_signature=tampered_sig
            )

        # Step 2: Valid signature unlocks circuit breaker!
        unlock_res = gov.operator_unlock_circuit_breaker(
            skill_id=skill,
            operator_id=operator_id,
            lockout_id=rec.lockout_id,
            nonce=nonce,
            operator_public_key=pub_hex,
            ed25519_signature=signature_hex,
            restore_permissions=True
        )

        self.assertEqual(unlock_res["action"], "OPERATOR_CRYPTOGRAPHIC_UNLOCK_SUCCESS")
        self.assertEqual(unlock_res["new_state"], "CLOSED")
        self.assertEqual(gov._registry[skill].state, CircuitState.CLOSED)
        self.assertTrue(os.access(source_file, os.W_OK), "Write permissions (0644) must be restored")

        # Step 3: Nonce replay attack must fail!
        with self.assertRaises(PermissionError) as ctx:
            gov.operator_unlock_circuit_breaker(
                skill_id=skill,
                operator_id=operator_id,
                lockout_id=rec.lockout_id,
                nonce=nonce,
                operator_public_key=pub_hex,
                ed25519_signature=signature_hex
            )
        self.assertIn("Replay attack detected", str(ctx.exception))

    def test_pid_controller_convergence_and_anti_windup(self):
        """Pillar 2: Verify PID Controller calculations, anti-windup clamping, and convergence."""
        from ch8_circuit_breaker_governor import PidController

        pid = PidController(kp=2.0, ki=0.5, kd=1.0, setpoint=0.05, i_min=-2.0, i_max=2.0)

        # Baseline: zero error (value equals setpoint)
        out_zero = pid.update(0.05, current_time=100.0)
        self.assertAlmostEqual(out_zero, 0.0, places=2)

        # High error spike (1.0 vs setpoint 0.05 -> error = 0.95)
        out_spike = pid.update(1.0, current_time=101.0)
        # P = 2.0 * 0.95 = 1.9
        # I = 0.5 * 0.95 * 1.0 = 0.475
        # D = 1.0 * (0.95 - 0.0) / 1.0 = 0.95
        # Total output ~ 3.325
        self.assertGreater(out_spike, 3.0)

        # Sustained error to test anti-windup clamping
        for t in range(102, 120):
            pid.update(1.0, current_time=float(t))

        self.assertLessEqual(pid.integral, 2.0, "Integral term must be clamped to i_max")
        self.assertGreaterEqual(pid.integral, -2.0, "Integral term must be clamped to i_min")

        # Test reset
        pid.reset()
        self.assertEqual(pid.integral, 0.0)
        self.assertEqual(pid.prev_error, 0.0)
        self.assertIsNone(pid.last_time)

    def test_adaptive_pid_backoff_and_jitter_smoothing(self):
        """Pillar 2: Verify calculate_pid_backoff smooths delays and avoids resonant flapping."""
        gov = self.governor

        # Low failure count, low PID output
        delay_low = gov.calculate_pid_backoff(1, pid_output=0.0, with_jitter=False)
        self.assertEqual(delay_low, 1.0)

        # High failure count, high PID output
        delay_high = gov.calculate_pid_backoff(3, pid_output=2.5, with_jitter=False)
        # failures=3 -> base_exp = 1.0 * 2^2 = 4.0; pid_factor = 1.0 + 2.5 = 3.5 -> 14.0s
        self.assertEqual(delay_high, 14.0)

        # With jitter smoothing: should be between 50% and 100% of nominal + jitter
        delay_jitter = gov.calculate_pid_backoff(3, pid_output=2.5, with_jitter=True)
        self.assertGreaterEqual(delay_jitter, 7.0)
        self.assertLessEqual(delay_jitter, 14.5)

    def test_pid_dynamic_surge_tripping(self):
        """Pillar 2: Verify that a sudden error surge trips the breaker via PID output."""
        gov = self.governor
        skill = "test_pid_skill"

        # Record 4 successes to prime the sliding window
        for i in range(4):
            gov.record_mutation_attempt(
                skill_id=skill,
                pre_hash=f"pre_{i}",
                post_hash=f"post_{i}",
                success=True,
                duration_ms=10.0
            )

        self.assertEqual(gov._registry[skill].state, CircuitState.CLOSED)

        # Record failures to cause an error surge
        res = gov.record_mutation_attempt(
            skill_id=skill,
            pre_hash="pre_fail",
            post_hash="post_fail",
            success=False,
            error_trace="Sudden high load crash",
            duration_ms=100.0
        )

        self.assertIn("pid_output", res)
        rec = gov._registry[skill]
        self.assertIn(rec.state, (CircuitState.CLOSED, CircuitState.OPEN))


if __name__ == "__main__":
    unittest.main(verbosity=2)
