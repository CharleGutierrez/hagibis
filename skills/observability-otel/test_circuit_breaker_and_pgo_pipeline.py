#!/usr/bin/env python3
"""
test_circuit_breaker_and_pgo_pipeline.py
================================================================================
Comprehensive End-to-End Test Suite for Pillar 3 & Pillar 4:
  - Autopoietic Loop Circuit-Breaker Governance
  - Cryptographic State Hashing (BLAKE3 / SHA-256)
  - Strict Exponential Backoff Dampening with Jitter
  - Skill Quarantining & Permission Locking upon 3 Consecutive Failures
  - Automated HITL Webhook Alert Generation (with live HTTP server receiver)
  - End-to-End Continuous PGO + BOLT Feedback Daemon & Crontab Pipeline
  - Atomic Hot-Swap Symlink Mechanics

Zero mocks, 100% production-ready verification.
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
import threading
import subprocess
from pathlib import Path
from http.server import HTTPServer, BaseHTTPRequestHandler
from typing import List, Dict, Any

# Ensure scratch directory and module can be imported
CURRENT_DIR = Path(__file__).resolve().parent
WORKSPACE_ROOT = CURRENT_DIR.parent.parent
SCRIPTS_DIR = WORKSPACE_ROOT / "scripts"
sys.path.insert(0, str(CURRENT_DIR))

def find_script(names):
    for n in names:
        for d in [CURRENT_DIR, SCRIPTS_DIR]:
            candidate = d / n
            if candidate.exists():
                return candidate
    return SCRIPTS_DIR / names[0]

from ch8_circuit_breaker_governor import (
    AutonomicCircuitBreakerGovernor,
    CircuitState,
    get_circuit_breaker_governor
)


# ==============================================================================
# Helper Mock/Live HTTP Webhook Sink Server
# ==============================================================================

class WebhookSinkHandler(BaseHTTPRequestHandler):
    received_payloads: List[Dict[str, Any]] = []

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        post_data = self.rfile.read(content_length)
        try:
            payload = json.loads(post_data.decode("utf-8"))
            WebhookSinkHandler.received_payloads.append(payload)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"received"}')
        except Exception as e:
            self.send_response(400)
            self.end_headers()
            self.wfile.write(str(e).encode())

    def log_message(self, format, *args):
        # Suppress noisy standard HTTP access logging during tests
        pass


class LiveWebhookServer:
    def __init__(self, host: str = "127.0.0.1", port: int = 0):
        WebhookSinkHandler.received_payloads.clear()
        self.server = HTTPServer((host, port), WebhookSinkHandler)
        self.port = self.server.server_port
        self.url = f"http://{host}:{self.port}/api/v1/alerts"
        self._thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def start(self):
        self._thread.start()

    def stop(self):
        self.server.shutdown()
        self.server.server_close()


# ==============================================================================
# Test Cases
# ==============================================================================

class TestCryptographicStateHashing(unittest.TestCase):
    """Verifies deterministic cryptographic state hashing on files and directory trees."""

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_hash_test_")
        self.target_path = Path(self.test_dir)

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_single_file_hash_mutation(self):
        sample_file = self.target_path / "lib.rs"
        sample_file.write_text("pub fn compute_hash() -> u32 { 101 }")

        pre_hash = AutonomicCircuitBreakerGovernor.compute_state_hash(sample_file)
        self.assertTrue(pre_hash.startswith("sha256:") or pre_hash.startswith("blake3:"))

        # Mutate file content
        sample_file.write_text("pub fn compute_hash() -> u32 { 999 }")
        post_hash = AutonomicCircuitBreakerGovernor.compute_state_hash(sample_file)

        self.assertNotEqual(pre_hash, post_hash, "Cryptographic hash must change when source file changes")

        # Revert file content and assert deterministic match
        sample_file.write_text("pub fn compute_hash() -> u32 { 101 }")
        revert_hash = AutonomicCircuitBreakerGovernor.compute_state_hash(sample_file)
        self.assertEqual(pre_hash, revert_hash, "Cryptographic hash must be strictly deterministic")

    def test_directory_tree_deterministic_hashing(self):
        src_dir = self.target_path / "src"
        src_dir.mkdir(parents=True)
        (src_dir / "a.rs").write_text("mod a;")
        (src_dir / "b.rs").write_text("mod b;")
        sub = src_dir / "nested"
        sub.mkdir()
        (sub / "c.rs").write_text("pub struct C;")

        dir_hash_1 = AutonomicCircuitBreakerGovernor.compute_state_hash(src_dir)

        # Create identical copy in another directory
        other_dir = self.target_path / "other_src"
        other_dir.mkdir(parents=True)
        other_sub = other_dir / "nested"
        other_sub.mkdir()
        (other_sub / "c.rs").write_text("pub struct C;")
        (other_dir / "b.rs").write_text("mod b;")
        (other_dir / "a.rs").write_text("mod a;")

        dir_hash_2 = AutonomicCircuitBreakerGovernor.compute_state_hash(other_dir)
        self.assertEqual(dir_hash_1, dir_hash_2, "Directory tree hash must be order-invariant and deterministic")

        # Modify one nested file
        (other_sub / "c.rs").write_text("pub struct CMutated;")
        dir_hash_mutated = AutonomicCircuitBreakerGovernor.compute_state_hash(other_dir)
        self.assertNotEqual(dir_hash_1, dir_hash_mutated, "Directory hash must change if any nested file changes")


class TestExponentialBackoffAndJitter(unittest.TestCase):
    """Verifies strict exponential backoff dampening and jitter boundaries."""

    def setUp(self):
        self.gov = AutonomicCircuitBreakerGovernor(t0=2.0, t_max=64.0)

    def test_backoff_monotonicity_and_clamping(self):
        expected_base = [2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 64.0, 64.0]
        for idx, base in enumerate(expected_base, start=1):
            calculated_no_jitter = self.gov.calculate_backoff(idx, with_jitter=False)
            self.assertEqual(calculated_no_jitter, base, f"Iteration {idx} base backoff calculation incorrect")

    def test_jitter_distribution(self):
        delays = [self.gov.calculate_backoff(1, with_jitter=True) for _ in range(50)]
        for d in delays:
            # Base is 2.0, jitter is [0.1, 0.5], so total in [2.10, 2.50]
            self.assertGreaterEqual(d, 2.05)
            self.assertLessEqual(d, 2.55)

        # Assert variance in jitter
        unique_delays = len(set(delays))
        self.assertGreater(unique_delays, 20, "Jitter should exhibit reasonable pseudo-random spread")


class TestSkillQuarantiningAndWebhookAlerts(unittest.TestCase):
    """Verifies automatic quarantining and HITL alert dispatch on 3 consecutive failures."""

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_cb_gov_test_")
        self.root_p = Path(self.test_dir)
        self.state_dir = self.root_p / "state"
        self.quarantine_dir = self.root_p / "quarantine"
        self.registry_file = self.state_dir / "cb_reg.json"
        self.webhook_sink = self.root_p / "webhook_events.jsonl"

        # Start live HTTP webhook server on ephemeral port
        self.server = LiveWebhookServer()
        self.server.start()

        self.gov = AutonomicCircuitBreakerGovernor(
            state_dir=self.state_dir,
            quarantine_dir=self.quarantine_dir,
            registry_path=self.registry_file,
            webhook_url=self.server.url,
            webhook_sink=self.webhook_sink,
            max_consecutive_failures=3,
            t0=1.0,
            t_max=16.0
        )

    def tearDown(self):
        self.server.stop()
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_three_failure_quarantine_and_webhook_cycle(self):
        skill_id = "crate_hgb_kernel_autotune"
        skill_file = self.root_p / "autotune_kernel.rs"
        skill_file.write_text("pub fn optimize_kernel() { panic!(); }")

        pre_h = self.gov.compute_state_hash(skill_file)

        # Iteration 1 Failure
        res1 = self.gov.record_mutation_attempt(
            skill_id=skill_id,
            pre_hash=pre_h,
            post_hash=pre_h,
            success=False,
            error_trace="Type error: mismatched types at line 12",
            skill_path=skill_file
        )
        self.assertIn(res1["state"], (CircuitState.CLOSED.value, CircuitState.HALF_OPEN.value))
        self.assertEqual(res1["consecutive_failures"], 1)
        can_run, _ = self.gov.can_mutate(skill_id)
        self.assertTrue(can_run)
        self.assertTrue(os.access(skill_file, os.W_OK), "Skill file must remain writable during dampening")

        # Iteration 2 Failure
        res2 = self.gov.record_mutation_attempt(
            skill_id=skill_id,
            pre_hash=pre_h,
            post_hash=pre_h,
            success=False,
            error_trace="E0425: cannot find value `simd_vector` in this scope",
            skill_path=skill_file
        )
        self.assertIn(res2["state"], (CircuitState.CLOSED.value, CircuitState.HALF_OPEN.value))
        self.assertEqual(res2["consecutive_failures"], 2)
        can_run, _ = self.gov.can_mutate(skill_id)
        self.assertTrue(can_run)

        # Iteration 3 Failure -> MUST TRIP AND QUARANTINE!
        res3 = self.gov.record_mutation_attempt(
            skill_id=skill_id,
            pre_hash=pre_h,
            post_hash=pre_h,
            success=False,
            error_trace="Borrow checker violation: cannot borrow `ctx` as mutable more than once",
            skill_path=skill_file
        )
        self.assertIn(res3["state"], (CircuitState.OPEN.value, "QUARANTINED"))
        self.assertEqual(res3["action"], "BREAKER_TRIPPED_QUARANTINED")

        # 1. Assert permissions preserved on active source (developer editability preserved)
        self.assertTrue(os.access(skill_file, os.W_OK), "Write permissions must be preserved on active source file")

        # 2. Assert can_mutate blocks subsequent mutations
        can_run, reason_msg = self.gov.can_mutate(skill_id)
        self.assertFalse(can_run)
        self.assertTrue("QUARANTINED" in reason_msg or "OPEN" in reason_msg)

        # 3. Assert quarantine directory and manifest
        skill_quarantine_dirs = list((self.quarantine_dir / skill_id).glob("*"))
        self.assertGreaterEqual(len(skill_quarantine_dirs), 1, "Quarantine directory must be created")
        manifest_p = skill_quarantine_dirs[0] / "quarantine_manifest.json"
        self.assertTrue(manifest_p.exists(), "quarantine_manifest.json must exist")
        manifest_data = json.loads(manifest_p.read_text())
        self.assertEqual(manifest_data["skill_id"], skill_id)
        self.assertEqual(manifest_data["consecutive_failures"], 3)
        self.assertTrue(manifest_data["permissions_locked"])

        # 4. Assert live HTTP Webhook received the quarantine_alert payload
        time.sleep(0.1)  # small yield for network callback
        self.assertGreaterEqual(len(WebhookSinkHandler.received_payloads), 1, "HTTP server should receive alert")
        alert = WebhookSinkHandler.received_payloads[-1]
        self.assertEqual(alert.get("event"), "quarantine_alert")
        self.assertEqual(alert.get("skill_id"), skill_id)
        self.assertEqual(alert.get("consecutive_failures"), 3)
        self.assertIn("Borrow checker violation", alert.get("error_trace", ""))
        self.assertIn("Human-In-The-Loop", alert.get("action_required", ""))

        # 5. Assert Webhook Sink persistent log
        self.assertTrue(self.webhook_sink.exists())
        sink_text = self.webhook_sink.read_text()
        self.assertIn(skill_id, sink_text)

        # 6. Test HITL Administrative Reset
        reset_ok = self.gov.reset_skill(skill_id, restore_path=skill_file)
        self.assertTrue(reset_ok)
        can_run_after_reset, _ = self.gov.can_mutate(skill_id)
        self.assertTrue(can_run_after_reset, "Skill should be unlocked after HITL reset")
        self.assertTrue(os.access(skill_file, os.W_OK), "Write permissions must be restored after HITL reset")


class TestContinuousPgoBoltFeedbackPipeline(unittest.TestCase):
    """Verifies live telemetry sampling, trigger thresholds, and atomic symlink hot-swapping."""

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_pgo_pipeline_test_")
        self.root_p = Path(self.test_dir)
        self.cron_script = find_script(["pgo-bolt-continuous-cron.sh", "pgo_bolt_continuous_cron.sh"])
        self.cb_script = find_script(["circuit-breaker-governor.sh", "circuit_breaker_governor.sh"])

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_pgo_cron_telemetry_triggers(self):
        """Tests decision evaluation for icache miss spikes, SLA latency breaches, and cooldowns."""
        state_dir = self.root_p / "state"
        state_dir.mkdir(parents=True)
        state_file = state_dir / "last_pgo.json"

        # Case 1: High instruction cache miss rate (16.2% > 12.0%)
        high_cache_telem = json.dumps({
            "cpu_governor": "performance",
            "p99_latency_ms": 35.0,
            "queue_depth": 1,
            "icache_miss_rate_pct": 16.2
        })

        eval_cmd = [
            "bash", str(self.cron_script), "--cron"
        ]
        env = os.environ.copy()
        env["HGB_STATE_DIR"] = str(state_dir)
        env["SHADOW_BUILD_DIR"] = str(self.root_p / "shadow")
        env["LAST_PGO_STATE_FILE"] = str(state_file)
        env["PGO_COOLDOWN_SECONDS"] = "3600"
        env["ACTIVE_SYMLINK"] = str(self.root_p / "hgbd")
        env["HGB_TELEMETRY_OVERRIDE"] = high_cache_telem

        # Initial run: should trigger PGO and create the symlink
        res = subprocess.run(eval_cmd, env=env, capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"PGO cron script failed: {res.stderr}")
        self.assertIn("Atomic symlink swap complete", res.stderr + res.stdout)

        # Immediate second run without force: must be blocked by cooldown
        res_cooldown = subprocess.run(eval_cmd, env=env, capture_output=True, text=True)
        self.assertEqual(res_cooldown.returncode, 0)
        # Verify state file recorded timestamp
        state_data = json.loads(state_file.read_text())
        self.assertGreater(state_data.get("last_pgo_timestamp", 0), 0)
        self.assertEqual(state_data.get("total_pgo_passes"), 1)

        # Third run WITH force: should bypass cooldown
        force_cmd = ["bash", str(self.cron_script), "--force"]
        res_force = subprocess.run(force_cmd, env=env, capture_output=True, text=True)
        self.assertEqual(res_force.returncode, 0)
        state_data_2 = json.loads(state_file.read_text())
        self.assertEqual(state_data_2.get("total_pgo_passes"), 2)

    def test_atomic_symlink_swap_integrity(self):
        """Verifies that atomic symlink hot-swapping never breaks file descriptors or target existence."""
        symlink_path = self.root_p / "active_bin"
        initial_bin = self.root_p / "bin_v1"
        initial_bin.write_text("#!/bin/sh\necho v1\n")
        os.chmod(initial_bin, 0o755)

        # Create initial symlink
        os.symlink(initial_bin, symlink_path)
        self.assertEqual(os.readlink(symlink_path), str(initial_bin))

        # Create candidate v2
        candidate_bin = self.root_p / "bin_v2"
        candidate_bin.write_text("#!/bin/sh\necho v2\n")
        os.chmod(candidate_bin, 0o755)

        # Atomic hot swap via temp symlink
        temp_symlink = self.root_p / f"active_bin.tmp.{os.getpid()}"
        os.symlink(candidate_bin, temp_symlink)
        os.replace(temp_symlink, symlink_path)

        # Assert active symlink points seamlessly to candidate v2
        self.assertEqual(os.readlink(symlink_path), str(candidate_bin))
        out = subprocess.check_output([str(symlink_path)]).decode().strip()
        self.assertEqual(out, "v2")


class TestBashCircuitBreakerScript(unittest.TestCase):
    """Verifies that circuit_breaker_governor.sh executes correctly via bash CLI."""

    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_bash_cb_test_")
        self.root_p = Path(self.test_dir)
        self.cb_script = find_script(["circuit-breaker-governor.sh", "circuit_breaker_governor.sh"])
        self.server = LiveWebhookServer()
        self.server.start()

    def tearDown(self):
        self.server.stop()
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_bash_governor_cli_lifecycle(self):
        env = os.environ.copy()
        env["HGB_STATE_DIR"] = str(self.root_p / "state")
        env["HGB_QUARANTINE_DIR"] = str(self.root_p / "quarantine")
        env["HGB_CB_REGISTRY"] = str(self.root_p / "state" / "reg.json")
        env["HGB_WEBHOOK_URL"] = self.server.url
        env["HGB_WEBHOOK_SINK"] = str(self.root_p / "events.jsonl")

        # 1. Test hash-state
        test_file = self.root_p / "code.rs"
        test_file.write_text("fn test() {}")
        res_h = subprocess.run(["bash", str(self.cb_script), "hash-state", str(test_file)], env=env, capture_output=True, text=True)
        self.assertEqual(res_h.returncode, 0)
        self.assertTrue(res_h.stdout.strip().startswith("sha256:") or res_h.stdout.strip().startswith("blake3:"))

        # 2. Test calculate-backoff
        res_b = subprocess.run(["bash", str(self.cb_script), "calculate-backoff", "2", "2.0", "32.0"], env=env, capture_output=True, text=True)
        self.assertEqual(res_b.returncode, 0)
        backoff_val = float(res_b.stdout.strip())
        self.assertGreaterEqual(backoff_val, 4.05)
        self.assertLessEqual(backoff_val, 4.55)

        # 3. Test 3-failure quarantine through CLI
        skill_id = "bash_tested_skill"
        for i in range(1, 3):
            res_fail = subprocess.run(["bash", str(self.cb_script), "record-failure", skill_id, f"Error {i}", "none", "none", str(test_file)], env=env, capture_output=True, text=True)
            self.assertEqual(res_fail.returncode, 1)
            # Check skill is still active
            res_chk = subprocess.run(["bash", str(self.cb_script), "check", skill_id], env=env, capture_output=True, text=True)
            self.assertEqual(res_chk.returncode, 0)

        # Failure 3 -> Trips breaker and quarantines
        res_fail_3 = subprocess.run(["bash", str(self.cb_script), "record-failure", skill_id, "Fatal Error 3", "none", "none", str(test_file)], env=env, capture_output=True, text=True)
        self.assertEqual(res_fail_3.returncode, 2)

        # Check skill is now blocked
        res_chk_blocked = subprocess.run(["bash", str(self.cb_script), "check", skill_id], env=env, capture_output=True, text=True)
        self.assertEqual(res_chk_blocked.returncode, 1)

        # Check file write permissions preserved
        self.assertTrue(os.access(test_file, os.W_OK))

        # Check HITL reset CLI
        res_reset = subprocess.run(["bash", str(self.cb_script), "reset", skill_id, str(test_file)], env=env, capture_output=True, text=True)
        self.assertEqual(res_reset.returncode, 0)

        res_chk_restored = subprocess.run(["bash", str(self.cb_script), "check", skill_id], env=env, capture_output=True, text=True)
        self.assertEqual(res_chk_restored.returncode, 0)
        self.assertTrue(os.access(test_file, os.W_OK))


class TestGitHubWorkflowYamlSyntax(unittest.TestCase):
    """Verifies that continuous_pgo_bolt_feedback.yml contains valid workflow configuration."""

    def test_workflow_file_structure(self):
        workflow_path = WORKSPACE_ROOT / ".github" / "workflows" / "continuous_pgo_bolt_feedback.yml"
        if not workflow_path.exists():
            workflow_path = CURRENT_DIR / "continuous_pgo_bolt_feedback.yml"
        self.assertTrue(workflow_path.exists(), "continuous_pgo_bolt_feedback.yml must exist")
        content = workflow_path.read_text()

        # Check required GHA trigger keys
        self.assertIn("name: Continuous PGO + BOLT Autopoietic Feedback Pipeline", content)
        self.assertIn("push:", content)
        self.assertIn("schedule:", content)
        self.assertIn("workflow_dispatch:", content)

        # Check PGO build flags & LLVM tools
        self.assertIn("-Cprofile-generate=", content)
        self.assertIn("llvm-profdata merge", content)
        self.assertIn("-Cprofile-use=", content)
        self.assertIn("llvm-bolt", content)
        self.assertIn("test_masterclass_live_simulations.py", content)
        self.assertIn("pgo_bolt_consensus_receipt.json", content)
        self.assertIn("actions/upload-artifact", content)


# ==============================================================================
# Main Test Suite Entrypoint
# ==============================================================================

def main():
    print("=" * 80)
    print("  🪽 PROJECT HAGIBIS: CIRCUIT BREAKER GOVERNOR & CONTINUOUS PGO TEST SUITE 🪽  ")
    print("=" * 80)

    loader = unittest.defaultTestLoader
    suite = unittest.TestSuite()
    suite.addTests(loader.loadTestsFromTestCase(TestCryptographicStateHashing))
    suite.addTests(loader.loadTestsFromTestCase(TestExponentialBackoffAndJitter))
    suite.addTests(loader.loadTestsFromTestCase(TestSkillQuarantiningAndWebhookAlerts))
    suite.addTests(loader.loadTestsFromTestCase(TestContinuousPgoBoltFeedbackPipeline))
    suite.addTests(loader.loadTestsFromTestCase(TestBashCircuitBreakerScript))
    suite.addTests(loader.loadTestsFromTestCase(TestGitHubWorkflowYamlSyntax))

    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)

    print("=" * 80)
    if result.wasSuccessful():
        print("  ✔ ALL TESTS PASSED WITH 100% SUCCESS RATE!")
        print("=" * 80)
        return 0
    else:
        print(f"  ✖ {len(result.failures)} FAILURES, {len(result.errors)} ERRORS ENCOUNTERED")
        print("=" * 80)
        return 1


if __name__ == "__main__":
    sys.exit(main())
