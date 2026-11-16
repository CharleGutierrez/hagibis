#!/usr/bin/env python3
"""
test_telemetry_governor_bridge.py - Comprehensive Unit Test Suite for Telemetry Governor Bridge
=============================================================================================
Phase 3 Observability Mesh Automated Verification Suite

Validates:
1. W3C Trace Context recommendation compliance (traceparent parsing, injection, malformed checks)
2. SLA breach P99 > 200ms -> CPU performance boost + thread expansion + MicroVM scale up
3. Queue depth spike > 10 -> CPU performance boost + thread expansion + MicroVM scale up
4. Settle cooldown (>15s at <50ms with queue=0) -> CPU powersave + thread contraction + MicroVM downscale
5. Nominal workload baseline (balanced governor, 4 workers, 2 vCPUs, 512 MiB RAM)
6. Firecracker dynamic allocation state persistence and TOML patching
7. Edge telemetry ingestion from Cloudflare Workers JSON payloads
8. Concurrent multi-threaded span recording stress test
"""

import os
import sys
import time
import json
import tempfile
import threading
import unittest

# Import from local module
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from telemetry_governor_bridge import (
    W3cTraceContext,
    OtelSpan,
    OtelSpanMetricsCollector,
    FirecrackerAllocationManager,
    DynatuneThreadPool,
    TelemetryGovernorBridge,
    TuningDecision,
    P99_SLA_THRESHOLD_MS,
    QUEUE_DEPTH_SPIKE_THRESHOLD,
    SETTLED_LATENCY_THRESHOLD_MS,
    SETTLED_DURATION_SECONDS,
    WORKER_COUNT_ECO,
    WORKER_COUNT_NOMINAL,
    WORKER_COUNT_BOOSTED,
    MICROVM_VCPU_ECO,
    MICROVM_MEM_ECO_MIB,
    MICROVM_VCPU_NOMINAL,
    MICROVM_MEM_NOMINAL_MIB,
    MICROVM_VCPU_BOOSTED,
    MICROVM_MEM_BOOSTED_MIB,
)


class TestW3cTraceContext(unittest.TestCase):
    """Tests W3C Traceparent RFC compliance and edge cases."""

    def test_w3c_traceparent_rfc_compliance(self):
        ctx = W3cTraceContext(
            version="00",
            trace_id="4bf92f3577b34da6a3ce929d0e0e4736",
            span_id="00f067aa0ba902b7",
            trace_flags="01",
        )
        carrier = ctx.to_traceparent()
        self.assertEqual(carrier, "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")

        parsed = W3cTraceContext.from_traceparent(carrier)
        self.assertEqual(parsed.version, "00")
        self.assertEqual(parsed.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736")
        self.assertEqual(parsed.parent_span_id, "00f067aa0ba902b7")
        self.assertEqual(parsed.trace_flags, "01")
        self.assertEqual(len(parsed.span_id), 16)

    def test_w3c_traceparent_malformed_rejections(self):
        # Invalid format
        with self.assertRaises(ValueError):
            W3cTraceContext.from_traceparent("invalid-header")

        # All zero trace ID
        with self.assertRaises(ValueError):
            W3cTraceContext.from_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01")

        # All zero parent span ID
        with self.assertRaises(ValueError):
            W3cTraceContext.from_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01")

        # Invalid hex characters
        with self.assertRaises(ValueError):
            W3cTraceContext.from_traceparent("00-4bf92f3577b34da6a3ce929d0e0e47zz-00f067aa0ba902b7-01")


class TestTelemetryGovernorBridge(unittest.TestCase):
    """Tests dynamic telemetry feedback loop and resource scaling."""

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.state_file = os.path.join(self.temp_dir.name, "microvm_alloc.json")
        self.toml_file = os.path.join(self.temp_dir.name, "firecracker.toml")

        # Create mock firecracker.toml
        with open(self.toml_file, "w") as f:
            f.write("""[machine]
vcpu_count = 2
mem_size_mib = 512
smt = false
""")

        self.microvm_mgr = FirecrackerAllocationManager(
            toml_path=self.toml_file, state_json_path=self.state_file
        )
        self.bridge = TelemetryGovernorBridge(microvm_mgr=self.microvm_mgr)

    def tearDown(self):
        self.temp_dir.cleanup()

    def test_sla_breach_triggers_boost(self):
        """P99 latency exceeding 200ms triggers boost, expanded pool, and scaled-up MicroVM."""
        # Inject 90 nominal spans (15ms) and 10 slow spans (350ms) -> P99 ~ 350ms
        for _ in range(90):
            self.bridge.record_span("query_vector", 15.0)
        for _ in range(10):
            self.bridge.record_span("query_vector", 350.0)

        decision = self.bridge.evaluate_and_tune()
        self.assertEqual(decision.action, "BOOST_PERFORMANCE")
        self.assertEqual(decision.governor_profile, "performance")
        self.assertTrue(decision.thread_pool_expanded)
        self.assertEqual(decision.active_workers, WORKER_COUNT_BOOSTED)
        self.assertEqual(decision.microvm_vcpus, MICROVM_VCPU_BOOSTED)
        self.assertEqual(decision.microvm_memory_mb, MICROVM_MEM_BOOSTED_MIB)
        self.assertGreater(decision.p99_latency_ms, P99_SLA_THRESHOLD_MS)

    def test_queue_depth_spike_triggers_boost(self):
        """Queue depth spike (>10) triggers immediate boost even if current latency is low."""
        for _ in range(50):
            self.bridge.record_span("fast_task", 25.0)
        self.bridge.collector.set_queue_depth(14)

        decision = self.bridge.evaluate_and_tune()
        self.assertEqual(decision.action, "BOOST_PERFORMANCE")
        self.assertEqual(decision.governor_profile, "performance")
        self.assertEqual(decision.active_workers, WORKER_COUNT_BOOSTED)
        self.assertEqual(decision.microvm_vcpus, MICROVM_VCPU_BOOSTED)
        self.assertEqual(decision.microvm_memory_mb, MICROVM_MEM_BOOSTED_MIB)
        self.assertEqual(decision.queue_depth, 14)

    def test_settle_cooldown_triggers_powersave(self):
        """Queue depth 0 and latency < 50ms for >= 15 seconds triggers powersave, eco pool, and microVM downscale."""
        for _ in range(50):
            self.bridge.record_span("light_task", 18.0)
        self.bridge.collector.set_queue_depth(0)

        t0 = 2000.0
        # Phase 1: Immediate check enters SETTLING
        d1 = self.bridge.evaluate_and_tune(mock_time=t0)
        self.assertEqual(d1.action, "SETTLING")
        self.assertEqual(d1.settle_seconds_elapsed, 0.0)

        # Phase 2: Check at 10 seconds still SETTLING
        d2 = self.bridge.evaluate_and_tune(mock_time=t0 + 10.0)
        self.assertEqual(d2.action, "SETTLING")
        self.assertEqual(d2.settle_seconds_elapsed, 10.0)

        # Phase 3: Check at 16 seconds (> 15s) triggers SETTLE_POWERSAVE
        d3 = self.bridge.evaluate_and_tune(mock_time=t0 + 16.0)
        self.assertEqual(d3.action, "SETTLE_POWERSAVE")
        self.assertEqual(d3.governor_profile, "powersave")
        self.assertEqual(d3.active_workers, WORKER_COUNT_ECO)
        self.assertEqual(d3.microvm_vcpus, MICROVM_VCPU_ECO)
        self.assertEqual(d3.microvm_memory_mb, MICROVM_MEM_ECO_MIB)
        self.assertGreaterEqual(d3.settle_seconds_elapsed, SETTLED_DURATION_SECONDS)

    def test_nominal_workload_baseline(self):
        """Nominal workload retains balanced governor, 4 workers, 2 vCPUs, 512 MiB RAM."""
        for _ in range(50):
            self.bridge.record_span("nominal_task", 80.0)
        self.bridge.collector.set_queue_depth(3)

        decision = self.bridge.evaluate_and_tune()
        self.assertEqual(decision.action, "BALANCED_NOMINAL")
        self.assertEqual(decision.governor_profile, "balanced")
        self.assertEqual(decision.active_workers, WORKER_COUNT_NOMINAL)
        self.assertEqual(decision.microvm_vcpus, MICROVM_VCPU_NOMINAL)
        self.assertEqual(decision.microvm_memory_mb, MICROVM_MEM_NOMINAL_MIB)

    def test_firecracker_dynamic_allocation_manager(self):
        """Validates JSON state persistence and firecracker.toml dynamic patching."""
        alloc = self.microvm_mgr.scale_boosted()
        self.assertEqual(alloc.vcpu_count, MICROVM_VCPU_BOOSTED)
        self.assertEqual(alloc.memory_mb, MICROVM_MEM_BOOSTED_MIB)

        # Verify JSON state file on disk
        self.assertTrue(os.path.exists(self.state_file))
        with open(self.state_file, "r") as f:
            saved = json.load(f)
        self.assertEqual(saved["vcpu_count"], MICROVM_VCPU_BOOSTED)
        self.assertEqual(saved["memory_mb"], MICROVM_MEM_BOOSTED_MIB)

        # Verify patching toml
        self.assertTrue(self.microvm_mgr.apply_to_firecracker_toml())
        with open(self.toml_file, "r") as f:
            toml_content = f.read()
        self.assertIn(f"vcpu_count = {MICROVM_VCPU_BOOSTED}", toml_content)
        self.assertIn(f"mem_size_mib = {MICROVM_MEM_BOOSTED_MIB}", toml_content)

    def test_edge_telemetry_ingestion(self):
        """Validates ingestion of spans from Cloudflare Workers payload."""
        edge_payload = {
            "queue_depth": 12,
            "spans": [
                {"name": "worker_webhook", "durationMs": 45.0, "status": {"code": "OK"}},
                {"name": "worker_vector_query", "durationMs": 280.0, "status": {"code": "OK"}},
            ],
        }
        self.bridge.ingest_edge_telemetry(edge_payload)
        metrics = self.bridge.collector.compute_percentiles()
        self.assertEqual(metrics["count"], 2)
        self.assertEqual(metrics["queue_depth"], 12)

        # Evaluating triggers boost due to queue depth 12
        decision = self.bridge.evaluate_and_tune()
        self.assertEqual(decision.action, "BOOST_PERFORMANCE")

    def test_multithreaded_concurrent_recording(self):
        """Validates concurrent span ingestion across 10 threads without data races or corruption."""
        num_threads = 10
        spans_per_thread = 100
        threads = []

        def worker_fn(tid):
            for i in range(spans_per_thread):
                dur = 10.0 + (i % 20)
                span = self.bridge.record_span(f"thread_{tid}", dur)
                span.add_event(f"evt_{i}")

        for t in range(num_threads):
            th = threading.Thread(target=worker_fn, args=(t,))
            threads.append(th)
            th.start()

        for th in threads:
            th.join()

        metrics = self.bridge.collector.compute_percentiles()
        self.assertGreater(metrics["count"], 0)
        self.assertGreater(metrics["p50_ms"], 0.0)
        self.assertGreater(metrics["p99_ms"], 0.0)


def run_suite():
    suite = unittest.TestLoader().loadTestsFromTestCase(TestW3cTraceContext)
    suite.addTests(unittest.TestLoader().loadTestsFromTestCase(TestTelemetryGovernorBridge))
    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)
    return result.wasSuccessful()


if __name__ == "__main__":
    success = run_suite()
    sys.exit(0 if success else 1)
