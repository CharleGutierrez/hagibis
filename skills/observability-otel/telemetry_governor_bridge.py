#!/usr/bin/env python3
"""
telemetry_governor_bridge.py - Telemetry-Driven Dynamic Hardware Governor & Dynatune Bridge
========================================================================================
Pillar 3: Autonomous SRE & Chaos Resilience Engineering

Bridges real-time OpenTelemetry span metrics (P99 request latency, queue depth, active worker count)
with Hagibis bare-metal kernel governors (`scripts/cpu-governor.sh`) and Dynatune thread pools.

Control Policy:
  • High Load / SLA Breach: When P99 latency > 200.0ms OR queue depth > 10:
      -> Dynamically commands `scripts/cpu-governor.sh performance`
      -> Expands Dynatune thread pool (concurrency boost)
  • Settle / Idle Cooldown: When queue depth == 0 AND P99 latency < 50.0ms for >= 15.0 seconds:
      -> Safely commands `scripts/cpu-governor.sh powersave` (or balanced)
      -> Contracts Dynatune thread pool (eco preservation)
  • Nominal: Maintains balanced baseline governor profile.
"""

import os
import sys
import time
import math
import json
import uuid
import subprocess
import threading
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple, Any

# ==============================================================================
# Configuration Thresholds & Constants
# ==============================================================================

P99_SLA_THRESHOLD_MS: float = 200.0
QUEUE_DEPTH_SPIKE_THRESHOLD: int = 10
SETTLED_LATENCY_THRESHOLD_MS: float = 50.0
SETTLED_DURATION_SECONDS: float = 15.0

NOMINAL_WORKER_COUNT: int = 4
EXPANDED_WORKER_COUNT: int = 8
ECO_WORKER_COUNT: int = 2

# Path to cpu-governor.sh
DEFAULT_GOVERNOR_SCRIPT: str = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", "scripts", "cpu-governor.sh")
)

# ==============================================================================
# OpenTelemetry Span & Metrics Modeling
# ==============================================================================

@dataclass
class OtelSpan:
    name: str
    duration_ms: float
    span_id: str = field(default_factory=lambda: uuid.uuid4().hex[:16])
    trace_id: str = field(default_factory=lambda: uuid.uuid4().hex)
    status: str = "OK"  # "OK", "ERROR", "UNSET"
    timestamp: float = field(default_factory=time.time)
    attributes: Dict[str, Any] = field(default_factory=dict)

    def to_w3c_traceparent(self) -> str:
        flags = "01" if self.status == "OK" else "00"
        return f"00-{self.trace_id}-{self.span_id}-{flags}"

    def to_dict(self) -> Dict[str, Any]:
        return {
            "span_id": self.span_id,
            "trace_id": self.trace_id,
            "name": self.name,
            "duration_ms": round(self.duration_ms, 3),
            "status": self.status,
            "timestamp": self.timestamp,
            "attributes": self.attributes,
        }


class OtelSpanMetricsCollector:
    """
    In-memory OpenTelemetry rolling telemetry collector.
    Calculates sliding-window percentiles (P50, P90, P95, P99), queue depth, and error rates.
    """
    def __init__(self, window_size: int = 500):
        self.window_size = window_size
        self._spans: List[OtelSpan] = []
        self._queue_depth: int = 0
        self._active_workers: int = NOMINAL_WORKER_COUNT
        self._lock = threading.Lock()

    def record_span(self, name: str, duration_ms: float, attributes: Optional[Dict[str, Any]] = None, status: str = "OK") -> OtelSpan:
        span = OtelSpan(name=name, duration_ms=duration_ms, attributes=attributes or {}, status=status)
        with self._lock:
            self._spans.append(span)
            if len(self._spans) > self.window_size:
                self._spans.pop(0)
        return span

    def set_queue_depth(self, depth: int):
        with self._lock:
            self._queue_depth = max(0, depth)

    def increment_queue_depth(self, delta: int = 1):
        with self._lock:
            self._queue_depth = max(0, self._queue_depth + delta)

    def decrement_queue_depth(self, delta: int = 1):
        with self._lock:
            self._queue_depth = max(0, self._queue_depth - delta)

    def set_active_workers(self, count: int):
        with self._lock:
            self._active_workers = max(1, count)

    def compute_percentiles(self) -> Dict[str, float]:
        with self._lock:
            if not self._spans:
                return {
                    "count": 0,
                    "p50_ms": 0.0,
                    "p90_ms": 0.0,
                    "p95_ms": 0.0,
                    "p99_ms": 0.0,
                    "max_ms": 0.0,
                    "error_rate": 0.0,
                    "queue_depth": self._queue_depth,
                    "active_workers": self._active_workers,
                }
            durations = sorted(s.duration_ms for s in self._spans)
            n = len(durations)
            error_count = sum(1 for s in self._spans if s.status == "ERROR")

            def get_perc(p: float) -> float:
                idx = int(math.ceil(p * n)) - 1
                return durations[max(0, min(idx, n - 1))]

            return {
                "count": n,
                "p50_ms": round(get_perc(0.50), 2),
                "p90_ms": round(get_perc(0.90), 2),
                "p95_ms": round(get_perc(0.95), 2),
                "p99_ms": round(get_perc(0.99), 2),
                "max_ms": round(durations[-1], 2),
                "error_rate": round(error_count / n, 4),
                "queue_depth": self._queue_depth,
                "active_workers": self._active_workers,
            }

    def reset(self):
        with self._lock:
            self._spans.clear()
            self._queue_depth = 0
            self._active_workers = NOMINAL_WORKER_COUNT

# ==============================================================================
# Dynatune Dynamic Thread Pool Simulator / Manager
# ==============================================================================

class DynatuneThreadPool:
    """
    Manages active thread pool scaling in synergy with hardware governor state.
    """
    def __init__(self, initial_workers: int = NOMINAL_WORKER_COUNT):
        self._current_workers = initial_workers
        self._lock = threading.Lock()

    @property
    def current_workers(self) -> int:
        with self._lock:
            return self._current_workers

    def expand(self, target_workers: int = EXPANDED_WORKER_COUNT) -> int:
        with self._lock:
            prev = self._current_workers
            self._current_workers = max(self._current_workers, target_workers)
            return self._current_workers

    def contract(self, target_workers: int = ECO_WORKER_COUNT) -> int:
        with self._lock:
            self._current_workers = min(self._current_workers, target_workers)
            return self._current_workers

    def reset_nominal(self) -> int:
        with self._lock:
            self._current_workers = NOMINAL_WORKER_COUNT
            return self._current_workers

# ==============================================================================
# Dynamic Telemetry Governor Bridge
# ==============================================================================

@dataclass
class TuningDecision:
    timestamp: float
    p99_latency_ms: float
    queue_depth: int
    active_workers: int
    action: str  # "BOOST_PERFORMANCE", "SETTLE_POWERSAVE", "SETTLING", "BALANCED_NOMINAL"
    governor_profile: str  # "performance", "powersave", "balanced"
    reason: str
    thread_pool_expanded: bool
    settle_seconds_elapsed: float = 0.0

    def to_dict(self) -> Dict[str, Any]:
        return {
            "timestamp": self.timestamp,
            "p99_latency_ms": self.p99_latency_ms,
            "queue_depth": self.queue_depth,
            "active_workers": self.active_workers,
            "action": self.action,
            "governor_profile": self.governor_profile,
            "reason": self.reason,
            "thread_pool_expanded": self.thread_pool_expanded,
            "settle_seconds_elapsed": round(self.settle_seconds_elapsed, 2),
        }


class TelemetryGovernorBridge:
    """
    Dynamic telemetry bridge connecting real-time OpenTelemetry span metrics
    with bare-metal Linux cpu-governor and Dynatune thread pools.
    """
    def __init__(
        self,
        governor_script_path: Optional[str] = None,
        collector: Optional[OtelSpanMetricsCollector] = None,
        thread_pool: Optional[DynatuneThreadPool] = None,
    ):
        self.governor_script = governor_script_path or DEFAULT_GOVERNOR_SCRIPT
        self.collector = collector or OtelSpanMetricsCollector()
        self.thread_pool = thread_pool or DynatuneThreadPool()

        self.current_profile: str = "balanced"
        self._settle_start_time: Optional[float] = None
        self._history: List[TuningDecision] = []
        self._lock = threading.Lock()

    def record_span(self, name: str, duration_ms: float, status: str = "OK") -> OtelSpan:
        return self.collector.record_span(name, duration_ms, status=status)

    def execute_governor_command(self, profile: str) -> bool:
        """Executes scripts/cpu-governor.sh with specified profile safely."""
        if not os.path.exists(self.governor_script):
            return False
        try:
            cmd = ["bash", self.governor_script, profile]
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            return res.returncode == 0
        except Exception as ex:
            return False

    def query_governor_telemetry(self) -> Dict[str, Any]:
        """Queries scripts/cpu-governor.sh telemetry output in JSON."""
        if not os.path.exists(self.governor_script):
            return {"status": "unavailable", "script_missing": True}
        try:
            cmd = ["bash", self.governor_script, "telemetry"]
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            if res.returncode == 0 and res.stdout.strip():
                return json.loads(res.stdout.strip())
        except Exception:
            pass
        return {"status": "fallback", "profile": self.current_profile}

    def evaluate_and_tune(self, mock_time: Optional[float] = None) -> TuningDecision:
        """
        Evaluates real-time OpenTelemetry metrics against SLA and load thresholds:
        - When P99 latency > 200ms OR queue depth > 10:
            -> Command `scripts/cpu-governor.sh performance`
            -> Expand Dynatune thread pool
        - When queue depth == 0 AND latency settles (<50ms for >15s):
            -> Command `scripts/cpu-governor.sh powersave`
            -> Contract Dynatune thread pool
        - Otherwise maintains balanced baseline.
        """
        now = mock_time if mock_time is not None else time.time()
        metrics = self.collector.compute_percentiles()
        p99 = metrics["p99_ms"]
        queue_depth = metrics["queue_depth"]

        with self._lock:
            # 1. High-load / SLA violation spike
            if p99 > P99_SLA_THRESHOLD_MS or queue_depth > QUEUE_DEPTH_SPIKE_THRESHOLD:
                self._settle_start_time = None
                new_workers = self.thread_pool.expand(EXPANDED_WORKER_COUNT)
                self.collector.set_active_workers(new_workers)
                self.current_profile = "performance"
                self.execute_governor_command("performance")

                reason = (
                    f"P99 latency ({p99:.1f}ms > {P99_SLA_THRESHOLD_MS}ms SLA)"
                    if p99 > P99_SLA_THRESHOLD_MS
                    else f"Queue depth spike ({queue_depth} > {QUEUE_DEPTH_SPIKE_THRESHOLD})"
                )
                decision = TuningDecision(
                    timestamp=now,
                    p99_latency_ms=p99,
                    queue_depth=queue_depth,
                    active_workers=new_workers,
                    action="BOOST_PERFORMANCE",
                    governor_profile="performance",
                    reason=reason,
                    thread_pool_expanded=True,
                    settle_seconds_elapsed=0.0,
                )

            # 2. Settle condition candidate (queue == 0 AND p99 < 50ms)
            elif queue_depth == 0 and p99 < SETTLED_LATENCY_THRESHOLD_MS:
                if self._settle_start_time is None:
                    self._settle_start_time = now
                elapsed = now - self._settle_start_time

                if elapsed >= SETTLED_DURATION_SECONDS:
                    # Settled for required duration: command powersave / contract pool
                    new_workers = self.thread_pool.contract(ECO_WORKER_COUNT)
                    self.collector.set_active_workers(new_workers)
                    self.current_profile = "powersave"
                    self.execute_governor_command("powersave")

                    decision = TuningDecision(
                        timestamp=now,
                        p99_latency_ms=p99,
                        queue_depth=queue_depth,
                        active_workers=new_workers,
                        action="SETTLE_POWERSAVE",
                        governor_profile="powersave",
                        reason=f"Queue empty & latency settled ({p99:.1f}ms < {SETTLED_LATENCY_THRESHOLD_MS}ms for {elapsed:.1f}s >= {SETTLED_DURATION_SECONDS}s)",
                        thread_pool_expanded=False,
                        settle_seconds_elapsed=elapsed,
                    )
                else:
                    # Within settle observation window
                    decision = TuningDecision(
                        timestamp=now,
                        p99_latency_ms=p99,
                        queue_depth=queue_depth,
                        active_workers=self.thread_pool.current_workers,
                        action="SETTLING",
                        governor_profile=self.current_profile,
                        reason=f"Settling window in progress ({elapsed:.1f}s / {SETTLED_DURATION_SECONDS}s)",
                        thread_pool_expanded=False,
                        settle_seconds_elapsed=elapsed,
                    )

            # 3. Nominal operation: balanced
            else:
                self._settle_start_time = None
                new_workers = self.thread_pool.reset_nominal()
                self.collector.set_active_workers(new_workers)
                self.current_profile = "balanced"
                self.execute_governor_command("balanced")

                decision = TuningDecision(
                    timestamp=now,
                    p99_latency_ms=p99,
                    queue_depth=queue_depth,
                    active_workers=new_workers,
                    action="BALANCED_NOMINAL",
                    governor_profile="balanced",
                    reason="Workload nominal within SLA limits",
                    thread_pool_expanded=False,
                    settle_seconds_elapsed=0.0,
                )

            self._history.append(decision)
            return decision

# ==============================================================================
# Automated Verification Suite for Telemetry Governor Bridge
# ==============================================================================

def test_telemetry_governor_bridge_sla_breach():
    """Validates that P99 latency exceeding 200ms triggers CPU performance boost and thread expansion."""
    bridge = TelemetryGovernorBridge()
    # Inject 100 fast spans and 10 slow spans pushing P99 to ~350ms
    for _ in range(90):
        bridge.record_span("query_vector", 15.0)
    for _ in range(10):
        bridge.record_span("query_vector", 350.0)

    decision = bridge.evaluate_and_tune()
    assert decision.action == "BOOST_PERFORMANCE", f"Expected BOOST_PERFORMANCE, got {decision.action}"
    assert decision.governor_profile == "performance", f"Expected performance governor, got {decision.governor_profile}"
    assert decision.thread_pool_expanded is True
    assert decision.active_workers >= EXPANDED_WORKER_COUNT
    assert decision.p99_latency_ms > 200.0
    print(f"✔ [PASS] SLA Breach Test: P99={decision.p99_latency_ms}ms -> {decision.action} ({decision.active_workers} workers)")


def test_telemetry_governor_bridge_queue_spike():
    """Validates that a queue depth spike (>10) triggers CPU performance boost and thread expansion."""
    bridge = TelemetryGovernorBridge()
    # Latency is normal (30ms), but queue spikes to 15
    for _ in range(50):
        bridge.record_span("wasm_eval", 30.0)
    bridge.collector.set_queue_depth(15)

    decision = bridge.evaluate_and_tune()
    assert decision.action == "BOOST_PERFORMANCE"
    assert decision.governor_profile == "performance"
    assert decision.thread_pool_expanded is True
    assert decision.queue_depth > 10
    print(f"✔ [PASS] Queue Spike Test: Queue={decision.queue_depth} -> {decision.action} ({decision.active_workers} workers)")


def test_telemetry_governor_bridge_settle_powersave():
    """Validates that queue depth 0 and latency < 50ms for > 15 seconds triggers powersave."""
    bridge = TelemetryGovernorBridge()
    # Seed fast spans (<50ms) and queue = 0
    for _ in range(50):
        bridge.record_span("fast_check", 20.0)
    bridge.collector.set_queue_depth(0)

    t0 = 1000.0
    # First check: enters SETTLING
    d1 = bridge.evaluate_and_tune(mock_time=t0)
    assert d1.action == "SETTLING"
    assert d1.settle_seconds_elapsed == 0.0

    # Intermediate check at 10 seconds: still SETTLING
    d2 = bridge.evaluate_and_tune(mock_time=t0 + 10.0)
    assert d2.action == "SETTLING"
    assert d2.settle_seconds_elapsed == 10.0

    # Final check at 16 seconds (> 15s): triggers SETTLE_POWERSAVE
    d3 = bridge.evaluate_and_tune(mock_time=t0 + 16.0)
    assert d3.action == "SETTLE_POWERSAVE"
    assert d3.governor_profile == "powersave"
    assert d3.active_workers == ECO_WORKER_COUNT
    assert d3.settle_seconds_elapsed >= 15.0
    print(f"✔ [PASS] Settle Cooldown Test: Latency={d3.p99_latency_ms}ms, Settle={d3.settle_seconds_elapsed:.1f}s -> {d3.action} ({d3.active_workers} workers)")


def run_all_bridge_tests() -> bool:
    print("=" * 70)
    print("🔬 RUNNING OPENTELEMETRY GOVERNOR BRIDGE TEST SUITE 🔬")
    print("=" * 70)
    try:
        test_telemetry_governor_bridge_sla_breach()
        test_telemetry_governor_bridge_queue_spike()
        test_telemetry_governor_bridge_settle_powersave()
        print("=" * 70)
        print("🎉 ALL OPENTELEMETRY GOVERNOR BRIDGE TESTS PASSED (100% SUCCESS) 🎉")
        print("=" * 70)
        return True
    except Exception as exc:
        print(f"✖ [FAIL] Telemetry Governor Bridge Test Failed: {exc}")
        import traceback
        traceback.print_exc()
        return False


if __name__ == "__main__":
    success = run_all_bridge_tests()
    sys.exit(0 if success else 1)
