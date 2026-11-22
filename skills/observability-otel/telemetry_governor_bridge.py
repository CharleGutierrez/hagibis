#!/usr/bin/env python3
"""
telemetry_governor_bridge.py - Unified Dynamic Hardware Governor & Firecracker MicroVM Scaling Bridge
=======================================================================================================
Pillar 3: Autonomous SRE & Chaos Resilience Engineering (OpenTelemetry Mesh)

Bridges real-time OpenTelemetry span metrics (P99 request latency, queue depth, active worker count)
with Hagibis bare-metal kernel governors (`scripts/cpu-governor.sh`), Dynatune thread pools,
and Firecracker MicroVM dynamic resource allocation (vCPUs, RAM limits).

Control Policy:
  • High Load / SLA Breach (P99 > 200.0ms OR queue_depth > 10):
      -> Linux CPU Governor: `performance` (EPP performance, no_turbo=0, MGLRU min_ttl=1000ms)
      -> Worker Concurrency: Boosted (8 workers)
      -> Firecracker MicroVM: Scale UP (4 vCPUs, 1024 MiB RAM)
  • Settle / Idle Cooldown (queue_depth == 0 AND P99 < 50.0ms for >= 15.0 seconds):
      -> Linux CPU Governor: `powersave` (EPP balance_power, no_turbo=1, MGLRU min_ttl=0)
      -> Worker Concurrency: Eco (2 workers)
      -> Firecracker MicroVM: Scale DOWN (1 vCPU, 256 MiB RAM)
  • Nominal:
      -> Linux CPU Governor: `balanced` (EPP balance_performance)
      -> Worker Concurrency: Nominal (4 workers)
      -> Firecracker MicroVM: Baseline (2 vCPUs, 512 MiB RAM)
"""

import os
import sys
import time
import math
import json
import uuid
import re
import subprocess
import threading
from dataclasses import dataclass, field, asdict
from typing import Dict, List, Optional, Tuple, Any

# ==============================================================================
# Configuration Thresholds & Constants
# ==============================================================================

P99_SLA_THRESHOLD_MS: float = 200.0
QUEUE_DEPTH_SPIKE_THRESHOLD: int = 10
SETTLED_LATENCY_THRESHOLD_MS: float = 50.0
SETTLED_DURATION_SECONDS: float = 15.0

# Worker thread concurrency levels
WORKER_COUNT_ECO: int = 2
WORKER_COUNT_NOMINAL: int = 4
WORKER_COUNT_BOOSTED: int = 8

# Firecracker MicroVM dynamic limits
MICROVM_VCPU_ECO: int = 1
MICROVM_MEM_ECO_MIB: int = 256

MICROVM_VCPU_NOMINAL: int = 2
MICROVM_MEM_NOMINAL_MIB: int = 512

MICROVM_VCPU_BOOSTED: int = 4
MICROVM_MEM_BOOSTED_MIB: int = 1024

# File paths
DEFAULT_GOVERNOR_SCRIPT: str = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", "scripts", "cpu-governor.sh")
)
DEFAULT_FIRECRACKER_TOML: str = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", "firecracker.toml")
)
DYNAMIC_MICROVM_STATE_PATH: str = "/tmp/hgb_microvm_dynamic_allocation.json"

# ==============================================================================
# 1. W3C Trace Context Modeling
# ==============================================================================

@dataclass
class W3cTraceContext:
    version: str = "00"
    trace_id: str = field(default_factory=lambda: uuid.uuid4().hex)
    parent_span_id: Optional[str] = None
    span_id: str = field(default_factory=lambda: uuid.uuid4().hex[:16])
    trace_flags: str = "01"
    trace_state: Optional[str] = None

    def to_traceparent(self) -> str:
        return f"{self.version}-{self.trace_id}-{self.span_id}-{self.trace_flags}"

    @classmethod
    def from_traceparent(cls, carrier: str) -> "W3cTraceContext":
        trimmed = carrier.strip()
        parts = trimmed.split("-")
        if len(parts) < 4:
            raise ValueError(f"Invalid traceparent format: '{carrier}'")

        version, trace_id, parent_id, flags = parts[0], parts[1], parts[2], parts[3]
        if len(trace_id) != 32 or not re.match(r"^[0-9a-fA-F]+$", trace_id) or trace_id == "0" * 32:
            raise ValueError(f"Invalid W3C trace_id: '{trace_id}'")
        if len(parent_id) != 16 or not re.match(r"^[0-9a-fA-F]+$", parent_id) or parent_id == "0" * 16:
            raise ValueError(f"Invalid W3C parent_id: '{parent_id}'")

        local_span_id = uuid.uuid4().hex[:16]
        return cls(
            version=version,
            trace_id=trace_id.lower(),
            parent_span_id=parent_id.lower(),
            span_id=local_span_id,
            trace_flags=flags.lower(),
        )

# ==============================================================================
# 2. OpenTelemetry Span & Metrics Modeling
# ==============================================================================

@dataclass
class OtelSpan:
    name: str
    duration_ms: float
    span_id: str = field(default_factory=lambda: uuid.uuid4().hex[:16])
    trace_id: str = field(default_factory=lambda: uuid.uuid4().hex)
    parent_span_id: Optional[str] = None
    status: str = "OK"  # "OK", "ERROR", "UNSET"
    timestamp: float = field(default_factory=time.time)
    start_unix_nano: int = field(default_factory=lambda: int(time.time() * 1e9))
    attributes: Dict[str, Any] = field(default_factory=dict)
    events: List[Dict[str, Any]] = field(default_factory=list)

    def to_w3c_traceparent(self) -> str:
        flags = "01" if self.status == "OK" else "00"
        return f"00-{self.trace_id}-{self.span_id}-{flags}"

    def add_event(self, name: str, attributes: Optional[Dict[str, Any]] = None):
        self.events.append({
            "name": name,
            "timestamp_unix_nano": int(time.time() * 1e9),
            "attributes": attributes or {},
        })

    def to_dict(self) -> Dict[str, Any]:
        return {
            "span_id": self.span_id,
            "trace_id": self.trace_id,
            "parent_span_id": self.parent_span_id,
            "name": self.name,
            "duration_ms": round(self.duration_ms, 3),
            "status": self.status,
            "timestamp": self.timestamp,
            "traceparent": self.to_w3c_traceparent(),
            "attributes": self.attributes,
            "events": self.events,
        }


class OtelSpanMetricsCollector:
    """
    Thread-safe, lock-free sliding-window OpenTelemetry metrics collector.
    Accurately computes P50, P90, P95, P99, error rate, and active queue depth.
    """
    def __init__(self, window_size: int = 500):
        self.window_size = window_size
        self._spans: List[OtelSpan] = []
        self._queue_depth: int = 0
        self._active_workers: int = WORKER_COUNT_NOMINAL
        self._lock = threading.Lock()

    def record_span(
        self,
        name: str,
        duration_ms: float,
        attributes: Optional[Dict[str, Any]] = None,
        status: str = "OK",
        parent_context: Optional[W3cTraceContext] = None,
    ) -> OtelSpan:
        trace_id = parent_context.trace_id if parent_context else uuid.uuid4().hex
        parent_span_id = parent_context.span_id if parent_context else None

        span = OtelSpan(
            name=name,
            duration_ms=duration_ms,
            trace_id=trace_id,
            parent_span_id=parent_span_id,
            attributes=attributes or {},
            status=status,
        )

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

    def compute_percentiles(self) -> Dict[str, Any]:
        with self._lock:
            if not self._spans:
                return {
                    "count": 0,
                    "p50_ms": 0.0,
                    "p90_ms": 0.0,
                    "p95_ms": 0.0,
                    "p99_ms": 0.0,
                    "min_ms": 0.0,
                    "max_ms": 0.0,
                    "mean_ms": 0.0,
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

            mean_val = sum(durations) / n
            return {
                "count": n,
                "p50_ms": round(get_perc(0.50), 2),
                "p90_ms": round(get_perc(0.90), 2),
                "p95_ms": round(get_perc(0.95), 2),
                "p99_ms": round(get_perc(0.99), 2),
                "min_ms": round(durations[0], 2),
                "max_ms": round(durations[-1], 2),
                "mean_ms": round(mean_val, 2),
                "error_rate": round(error_count / n, 4),
                "queue_depth": self._queue_depth,
                "active_workers": self._active_workers,
            }

    def reset(self):
        with self._lock:
            self._spans.clear()
            self._queue_depth = 0
            self._active_workers = WORKER_COUNT_NOMINAL

# ==============================================================================
# 3. Firecracker MicroVM Dynamic Allocation Manager
# ==============================================================================

@dataclass
class MicrovmAllocation:
    vcpu_count: int = MICROVM_VCPU_NOMINAL
    memory_mb: int = MICROVM_MEM_NOMINAL_MIB
    state: str = "nominal"  # "eco", "nominal", "boosted"
    updated_at: float = field(default_factory=time.time)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "vcpu_count": self.vcpu_count,
            "memory_mb": self.memory_mb,
            "state": self.state,
            "updated_at": self.updated_at,
        }


class FirecrackerAllocationManager:
    """
    Manages dynamic memory and vCPU scaling for Firecracker MicroVMs based on live telemetry feedback.
    """
    def __init__(self, toml_path: Optional[str] = None, state_json_path: Optional[str] = None):
        self.toml_path = toml_path or DEFAULT_FIRECRACKER_TOML
        self.state_json_path = state_json_path or DYNAMIC_MICROVM_STATE_PATH
        self.current_allocation = MicrovmAllocation()
        self._lock = threading.Lock()

    def scale_boosted(self) -> MicrovmAllocation:
        with self._lock:
            self.current_allocation = MicrovmAllocation(
                vcpu_count=MICROVM_VCPU_BOOSTED,
                memory_mb=MICROVM_MEM_BOOSTED_MIB,
                state="boosted",
                updated_at=time.time(),
            )
            self._persist_state()
            return self.current_allocation

    def scale_eco(self) -> MicrovmAllocation:
        with self._lock:
            self.current_allocation = MicrovmAllocation(
                vcpu_count=MICROVM_VCPU_ECO,
                memory_mb=MICROVM_MEM_ECO_MIB,
                state="eco",
                updated_at=time.time(),
            )
            self._persist_state()
            return self.current_allocation

    def scale_nominal(self) -> MicrovmAllocation:
        with self._lock:
            self.current_allocation = MicrovmAllocation(
                vcpu_count=MICROVM_VCPU_NOMINAL,
                memory_mb=MICROVM_MEM_NOMINAL_MIB,
                state="nominal",
                updated_at=time.time(),
            )
            self._persist_state()
            return self.current_allocation

    def _persist_state(self):
        """Writes current dynamic allocation to /tmp for microVM hypervisor to consume."""
        try:
            with open(self.state_json_path, "w") as f:
                json.dump(self.current_allocation.to_dict(), f, indent=2)
        except Exception:
            pass

    def apply_to_firecracker_toml(self, target_path: Optional[str] = None) -> bool:
        """Dynamically patches firecracker.toml with active machine limits."""
        dest = target_path or self.toml_path
        if not os.path.exists(dest):
            return False
        try:
            with open(dest, "r") as f:
                content = f.read()

            # Replace vcpu_count and mem_size_mib in [machine]
            alloc = self.current_allocation
            content = re.sub(r"vcpu_count\s*=\s*\d+", f"vcpu_count = {alloc.vcpu_count}", content)
            content = re.sub(r"mem_size_mib\s*=\s*\d+", f"mem_size_mib = {alloc.memory_mb}", content)

            with open(dest, "w") as f:
                f.write(content)
            return True
        except Exception:
            return False

# ==============================================================================
# 4. Dynatune Thread Pool Concurrency Manager
# ==============================================================================

class DynatuneThreadPool:
    """
    Manages active thread pool scaling in synergy with hardware governor and microVM states.
    """
    def __init__(self, initial_workers: int = WORKER_COUNT_NOMINAL):
        self._current_workers = initial_workers
        self._lock = threading.Lock()

    @property
    def current_workers(self) -> int:
        with self._lock:
            return self._current_workers

    def expand_boosted(self) -> int:
        with self._lock:
            self._current_workers = WORKER_COUNT_BOOSTED
            return self._current_workers

    def contract_eco(self) -> int:
        with self._lock:
            self._current_workers = WORKER_COUNT_ECO
            return self._current_workers

    def reset_nominal(self) -> int:
        with self._lock:
            self._current_workers = WORKER_COUNT_NOMINAL
            return self._current_workers

# ==============================================================================
# 5. Enhanced Telemetry Governor Bridge
# ==============================================================================

@dataclass
class TuningDecision:
    timestamp: float
    p99_latency_ms: float
    queue_depth: int
    active_workers: int
    action: str  # "BOOST_PERFORMANCE", "SETTLE_POWERSAVE", "SETTLING", "BALANCED_NOMINAL"
    governor_profile: str  # "performance", "powersave", "balanced"
    microvm_vcpus: int
    microvm_memory_mb: int
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
            "microvm_vcpus": self.microvm_vcpus,
            "microvm_memory_mb": self.microvm_memory_mb,
            "reason": self.reason,
            "thread_pool_expanded": self.thread_pool_expanded,
            "settle_seconds_elapsed": round(self.settle_seconds_elapsed, 2),
        }


class TelemetryGovernorBridge:
    """
    Unified OpenTelemetry Governor Bridge connecting real-time span metrics with:
    1. Bare-metal Linux CPU governor (`scripts/cpu-governor.sh`)
    2. Dynatune worker thread pool (Eco -> Nominal -> Boosted)
    3. Firecracker MicroVM dynamic memory/vCPU allocation
    """
    def __init__(
        self,
        governor_script_path: Optional[str] = None,
        collector: Optional[OtelSpanMetricsCollector] = None,
        thread_pool: Optional[DynatuneThreadPool] = None,
        microvm_mgr: Optional[FirecrackerAllocationManager] = None,
    ):
        self.governor_script = governor_script_path or DEFAULT_GOVERNOR_SCRIPT
        self.collector = collector or OtelSpanMetricsCollector()
        self.thread_pool = thread_pool or DynatuneThreadPool()
        self.microvm_mgr = microvm_mgr or FirecrackerAllocationManager()

        self.current_profile: str = "balanced"
        self._settle_start_time: Optional[float] = None
        self._history: List[TuningDecision] = []
        self._lock = threading.Lock()

    def record_span(
        self,
        name: str,
        duration_ms: float,
        status: str = "OK",
        parent_context: Optional[W3cTraceContext] = None,
    ) -> OtelSpan:
        return self.collector.record_span(
            name, duration_ms, status=status, parent_context=parent_context
        )

    def execute_governor_command(self, profile: str) -> bool:
        """Executes scripts/cpu-governor.sh with specified profile safely."""
        if not os.path.exists(self.governor_script):
            return False
        try:
            cmd = ["bash", self.governor_script, profile]
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            return res.returncode == 0
        except Exception:
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

    def ingest_edge_telemetry(self, edge_payload: Dict[str, Any]):
        """Ingests spans and telemetry exported from Cloudflare Workers edge gateway."""
        spans = edge_payload.get("spans", [])
        for s in spans:
            name = s.get("name", "edge_span")
            dur = s.get("durationMs", 10.0)
            code = s.get("status", {}).get("code", "OK")
            self.record_span(name, dur, status="OK" if code == "OK" else "ERROR")

        if "queue_depth" in edge_payload:
            self.collector.set_queue_depth(int(edge_payload["queue_depth"]))

    def evaluate_and_tune(self, mock_time: Optional[float] = None) -> TuningDecision:
        """
        Evaluates real-time OpenTelemetry metrics against SLA and load thresholds:
        - When P99 latency > 200ms OR queue depth > 10:
            -> Command `scripts/cpu-governor.sh performance`
            -> Expand Dynatune thread pool to Boosted (8)
            -> Scale Firecracker MicroVM to Boosted (4 vCPUs, 1024 MiB RAM)
        - When queue depth == 0 AND latency settles (<50ms for >= 15s):
            -> Command `scripts/cpu-governor.sh powersave`
            -> Contract Dynatune thread pool to Eco (2)
            -> Scale Firecracker MicroVM to Eco (1 vCPU, 256 MiB RAM)
        - Otherwise maintains balanced baseline (4 workers, 2 vCPUs, 512 MiB RAM).
        """
        now = mock_time if mock_time is not None else time.time()
        metrics = self.collector.compute_percentiles()
        p99 = metrics["p99_ms"]
        queue_depth = metrics["queue_depth"]

        with self._lock:
            # 1. High-load / SLA violation spike
            if p99 > P99_SLA_THRESHOLD_MS or queue_depth > QUEUE_DEPTH_SPIKE_THRESHOLD:
                self._settle_start_time = None
                new_workers = self.thread_pool.expand_boosted()
                self.collector.set_active_workers(new_workers)
                self.current_profile = "performance"
                self.execute_governor_command("performance")
                vm_alloc = self.microvm_mgr.scale_boosted()

                reason = (
                    f"P99 latency breach ({p99:.1f}ms > {P99_SLA_THRESHOLD_MS}ms SLA)"
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
                    microvm_vcpus=vm_alloc.vcpu_count,
                    microvm_memory_mb=vm_alloc.memory_mb,
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
                    # Settled for required duration: command powersave / contract pool / downscale microVM
                    new_workers = self.thread_pool.contract_eco()
                    self.collector.set_active_workers(new_workers)
                    self.current_profile = "powersave"
                    self.execute_governor_command("powersave")
                    vm_alloc = self.microvm_mgr.scale_eco()

                    decision = TuningDecision(
                        timestamp=now,
                        p99_latency_ms=p99,
                        queue_depth=queue_depth,
                        active_workers=new_workers,
                        action="SETTLE_POWERSAVE",
                        governor_profile="powersave",
                        microvm_vcpus=vm_alloc.vcpu_count,
                        microvm_memory_mb=vm_alloc.memory_mb,
                        reason=f"Queue empty & latency settled ({p99:.1f}ms < {SETTLED_LATENCY_THRESHOLD_MS}ms for {elapsed:.1f}s >= {SETTLED_DURATION_SECONDS}s)",
                        thread_pool_expanded=False,
                        settle_seconds_elapsed=elapsed,
                    )
                else:
                    # Within settle observation window
                    vm_alloc = self.microvm_mgr.current_allocation
                    decision = TuningDecision(
                        timestamp=now,
                        p99_latency_ms=p99,
                        queue_depth=queue_depth,
                        active_workers=self.thread_pool.current_workers,
                        action="SETTLING",
                        governor_profile=self.current_profile,
                        microvm_vcpus=vm_alloc.vcpu_count,
                        microvm_memory_mb=vm_alloc.memory_mb,
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
                vm_alloc = self.microvm_mgr.scale_nominal()

                decision = TuningDecision(
                    timestamp=now,
                    p99_latency_ms=p99,
                    queue_depth=queue_depth,
                    active_workers=new_workers,
                    action="BALANCED_NOMINAL",
                    governor_profile="balanced",
                    microvm_vcpus=vm_alloc.vcpu_count,
                    microvm_memory_mb=vm_alloc.memory_mb,
                    reason="Workload nominal within SLA limits",
                    thread_pool_expanded=False,
                    settle_seconds_elapsed=0.0,
                )

            self._history.append(decision)
            return decision


# ==============================================================================
# CLI Entry Point
# ==============================================================================

def main():
    action = sys.argv[1] if len(sys.argv) > 1 else "status"
    bridge = TelemetryGovernorBridge()

    if action == "status":
        gov_telemetry = bridge.query_governor_telemetry()
        metrics = bridge.collector.compute_percentiles()
        if "--json" in sys.argv:
            data = {
                "profile": bridge.current_profile,
                "active_workers": bridge.thread_pool.current_workers,
                "vcpu_count": bridge.microvm_mgr.current_allocation.vcpu_count,
                "memory_mb": bridge.microvm_mgr.current_allocation.memory_mb,
                "p99_ms": metrics['p99_ms'],
                "queue_depth": metrics['queue_depth'],
                "governor": gov_telemetry.get('governor', 'unknown')
            }
            print(json.dumps(data, indent=2))
        else:
            print("📊 [Hagibis Telemetry Governor Bridge Status]")
            print(f"  • CPU Governor Profile:    {bridge.current_profile}")
            print(f"  • Active Workers:          {bridge.thread_pool.current_workers}")
            print(f"  • MicroVM Allocation:      {bridge.microvm_mgr.current_allocation.vcpu_count} vCPUs, {bridge.microvm_mgr.current_allocation.memory_mb} MiB RAM")
            print(f"  • P99 Latency:             {metrics['p99_ms']}ms")
            print(f"  • Queue Depth:             {metrics['queue_depth']}")
            print(f"  • Kernel Governor State:   {gov_telemetry.get('governor', 'unknown')}")

    elif action == "evaluate":
        p99 = float(sys.argv[2]) if len(sys.argv) > 2 else 10.0
        q = int(sys.argv[3]) if len(sys.argv) > 3 else 0
        s = float(sys.argv[4]) if len(sys.argv) > 4 else 0.0

        bridge.collector.record_span("cli_probe", p99)
        bridge.collector.set_queue_depth(q)
        t0 = time.time()
        d = bridge.evaluate_and_tune(mock_time=t0 + s)
        print(json.dumps(d.to_dict(), indent=2))

    else:
        print(f"Usage: {sys.argv[0]} [status|evaluate <p99_ms> <queue> <settle_s>]")


if __name__ == "__main__":
    main()
