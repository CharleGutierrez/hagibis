#!/usr/bin/env python3
"""
test_memory_leak_and_cycle_hysteresis.py
Unit & Integration Test Suite for Memory Leak Rate, Instantaneous Spikes,
and Autonomous Code Generation Cycle / Infinite Recursion Detection.
"""

import os
import sys
import json
import time
import shutil
import tempfile
import unittest
from pathlib import Path

# Add skills dir to sys.path
SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from ch8_circuit_breaker_governor import (
    AutonomicCircuitBreakerGovernor,
    CircuitState,
    MemoryTracker,
    RecursionAndCycleDetector,
    DEFAULT_MEMORY_SPIKE_BYTES,
    DEFAULT_MAX_LEAK_RATE_BYTES_PER_SEC,
    DEFAULT_MAX_TOTAL_MEMORY_BYTES
)


class TestMemorySpikeAndLeakRateSupervision(unittest.TestCase):

    def setUp(self):
        self.tracker = MemoryTracker(
            spike_threshold_bytes=100 * 1024 * 1024,      # 100 MB
            max_leak_rate_bytes_per_sec=2.0 * 1024 * 1024, # 2 MB/s
            max_total_memory_bytes=1024 * 1024 * 1024,    # 1 GB
            history_capacity=10
        )

    def test_instantaneous_memory_spike_detection(self):
        # Reading 1: 50 MB
        breach, reason, details = self.tracker.record_reading(50 * 1024 * 1024, timestamp=1.0)
        self.assertFalse(breach)

        # Reading 2: 180 MB (+130 MB spike > 100 MB threshold)
        breach2, reason2, details2 = self.tracker.record_reading(180 * 1024 * 1024, timestamp=2.0)
        self.assertTrue(breach2)
        self.assertEqual(details2.get("kind"), "SPIKE")
        self.assertIn("spike breached", reason2)

    def test_memory_leak_rate_limit_detection(self):
        # 3 readings: growth of 15 MB over 2 seconds = 7.5 MB/s > 2 MB/s
        self.tracker.record_reading(20 * 1024 * 1024, timestamp=1.0)
        self.tracker.record_reading(25 * 1024 * 1024, timestamp=2.0)
        breach, reason, details = self.tracker.record_reading(35 * 1024 * 1024, timestamp=3.0)

        self.assertTrue(breach)
        self.assertEqual(details.get("kind"), "LEAK_RATE")
        self.assertIn("Memory leak rate limit exceeded", reason)

    def test_monotonic_runaway_leak_detection(self):
        # 5 consecutive increasing samples with total delta >= 50 MB
        base = 10 * 1024 * 1024
        for i in range(4):
            b, _, _ = self.tracker.record_reading(base + i * 15 * 1024 * 1024, timestamp=1.0 + i * 10.0)
            self.assertFalse(b)

        # 5th consecutive increase
        b_final, reason, details = self.tracker.record_reading(base + 4 * 15 * 1024 * 1024, timestamp=50.0)
        self.assertTrue(b_final)
        self.assertEqual(details.get("kind"), "MONOTONIC_LEAK")
        self.assertIn("Monotonic runaway memory leak", reason)

    def test_hard_ceiling_breach(self):
        breach, reason, details = self.tracker.record_reading(2 * 1024 * 1024 * 1024, timestamp=1.0)
        self.assertTrue(breach)
        self.assertEqual(details.get("kind"), "HARD_CEILING")
        self.assertIn("Hard memory ceiling breached", reason)


class TestRecursionAndCycleDetection(unittest.TestCase):

    def setUp(self):
        self.detector = RecursionAndCycleDetector(
            max_recursion_depth=5,
            max_cycle_period=4,
            history_capacity=20
        )

    def test_recursion_depth_limit_breach(self):
        is_cycle, reason, details = self.detector.record_turn(
            turn_number=1,
            state_hash="hash_a",
            recursion_depth=5,
            timestamp=1.0
        )
        self.assertTrue(is_cycle)
        self.assertEqual(details.get("pattern"), "RECURSION_DEPTH_EXCEEDED")
        self.assertIn("Maximum recursion depth breached", reason)

    def test_zero_progress_stall_detection(self):
        self.detector.record_turn(1, "hash_stuck", timestamp=1.0)
        self.detector.record_turn(2, "hash_stuck", timestamp=2.0)
        is_cycle, reason, details = self.detector.record_turn(3, "hash_stuck", timestamp=3.0)

        self.assertTrue(is_cycle)
        self.assertEqual(details.get("pattern"), "ZERO_PROGRESS_STALL")
        self.assertIn("Zero-progress stall", reason)

    def test_direct_2_cycle_oscillation_detection(self):
        self.detector.record_turn(1, "state_alpha", timestamp=1.0)
        self.detector.record_turn(2, "state_beta", timestamp=2.0)
        is_cycle, reason, details = self.detector.record_turn(3, "state_alpha", timestamp=3.0)

        self.assertTrue(is_cycle)
        self.assertEqual(details.get("pattern"), "DIRECT_OSCILLATION")
        self.assertEqual(details.get("rollback_target_turn"), 1)
        self.assertIn("Direct 2-cycle oscillation detected", reason)

    def test_periodic_3_cycle_detection(self):
        # A -> B -> C -> A -> B -> C
        turns = ["hash_1", "hash_2", "hash_3", "hash_1", "hash_2", "hash_3"]
        for idx, h in enumerate(turns[:-1], start=1):
            is_c, _, _ = self.detector.record_turn(idx, h, timestamp=float(idx))
            self.assertFalse(is_c)

        is_c, reason, details = self.detector.record_turn(6, turns[-1], timestamp=6.0)
        self.assertTrue(is_c)
        self.assertEqual(details.get("pattern"), "PERIODIC_CYCLE")
        self.assertEqual(details.get("period"), 3)


class TestGovernorIntegratedMemoryAndCycleHysteresis(unittest.TestCase):

    def setUp(self):
        self.test_dir = Path(tempfile.mkdtemp(prefix="hgb_mem_cycle_test_"))
        self.state_dir = self.test_dir / "state"
        self.quarantine_dir = self.test_dir / "quarantine"
        self.registry_file = self.state_dir / "cb_reg.json"
        self.webhook_sink = self.test_dir / "events.jsonl"

        self.gov = AutonomicCircuitBreakerGovernor(
            state_dir=self.state_dir,
            quarantine_dir=self.quarantine_dir,
            registry_path=self.registry_file,
            webhook_sink=self.webhook_sink
        )

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_governor_memory_spike_trips_to_open(self):
        skill = "mem_spike_skill"
        res = self.gov.record_memory_reading(
            skill_id=skill,
            rss_bytes=10 * 1024 * 1024,
            timestamp=1.0
        )
        self.assertFalse(res["is_breach"])

        # Sudden +300 MB spike > 256 MB default threshold
        res2 = self.gov.record_memory_reading(
            skill_id=skill,
            rss_bytes=320 * 1024 * 1024,
            timestamp=2.0
        )
        self.assertTrue(res2["is_breach"])
        self.assertEqual(res2["state"], CircuitState.OPEN.value)

        # Check skill is blocked
        allowed, reason = self.gov.can_mutate(skill, current_time=2.1)
        self.assertFalse(allowed)

    def test_governor_cycle_escalates_to_perm_tripped_cryptolocked(self):
        skill = "cycle_oscillating_skill"
        target_file = self.test_dir / "oscillating.rs"
        target_file.write_text("fn test() {}")

        # Turn 1
        self.gov.record_code_generation_turn(skill, 1, "hash_state_a", timestamp=1.0, target_paths=[target_file])
        # Turn 2
        self.gov.record_code_generation_turn(skill, 2, "hash_state_b", timestamp=2.0, target_paths=[target_file])
        # Turn 3: oscillates back to state a -> PermTrippedCryptolocked!
        res3 = self.gov.record_code_generation_turn(skill, 3, "hash_state_a", timestamp=3.0, target_paths=[target_file])

        self.assertTrue(res3["is_cycle"])
        self.assertEqual(res3["state"], CircuitState.PERM_TRIPPED_CRYPTOLOCKED.value)
        self.assertIn("lockout_receipt", res3)
        self.assertTrue(res3["lockout_receipt"]["ed25519_signed"])

        # Verify permissions stripped to read-only (0444)
        self.assertFalse(os.access(target_file, os.W_OK))


if __name__ == "__main__":
    unittest.main(verbosity=2)
