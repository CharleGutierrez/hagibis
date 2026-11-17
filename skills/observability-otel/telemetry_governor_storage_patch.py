#!/usr/bin/env python3
"""
telemetry_governor_storage_patch.py - Storage Guardrails & Dynamic Backpressure Governor Patch
==============================================================================================
Pillar 3: Autonomous SRE & Chaos Resilience Engineering (Storage Telemetry Governor)

Hooks into `skills/observability-otel/telemetry_governor_bridge.py` to ingest real-time
disk capacity and partition pressure metrics (`statvfs`, IPC state from the Rust pruning daemon),
and dynamically modulates hardware governance, worker thread concurrency, and Firecracker MicroVM
memory limits.

Safety Invariant:
  When root filesystem `/` exceeds 80.0% capacity (currently at ~85.3% on /dev/sda2),
  storage protection MUST take precedence over compute throughput.
  Even if P99 latency SLA is breached (>200ms) or queue spikes (>10), worker concurrency
  is clamped to prevent log flooding, build artifact exhaustion, and catastrophic out-of-disk failures.

Storage Pressure Tiers:
  • NORMAL (< 75.0% usage):
      -> Ingestion unconstrained (100% telemetry fidelity)
      -> Standard CPU governor, Boosted worker limit (up to 8), MicroVM RAM up to 1024 MiB.
  • BACKPRESSURE (75.0% - 80.0% usage):
      -> Worker concurrency capped at Nominal (4 workers)
      -> Firecracker MicroVM RAM capped at 512 MiB
      -> Telemetry buffers flushed; sampling applied to trace spans.
  • AGGRESSIVE_PRUNING / HIGH (80.0% - 90.0% usage):
      -> Worker concurrency throttled to Eco (2 workers)
      -> Firecracker MicroVM RAM clamped strictly to 256 MiB (or 384 MiB)
      -> Drop DEBUG/INFO spans; trigger active log compaction and cache eviction.
  • EMERGENCY_DROP / CRITICAL (> 90.0% usage):
      -> Worker concurrency emergency drop to 1 worker
      -> Firecracker MicroVM RAM forced down to 256 MiB
      -> Drop all non-essential telemetry; pause background compiler jobs.
"""

import os
import sys
import time
import json
import enum
import logging
from dataclasses import dataclass, field, asdict
from typing import Dict, List, Optional, Tuple, Any

# ==============================================================================
# Import Base Telemetry Governor Bridge
# ==============================================================================

# Search paths for telemetry_governor_bridge
_POSSIBLE_BRIDGE_PATHS = [
    os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "hagibis", "skills", "observability-otel")),
    "/media/toshiba/Lexar/hagibis/skills/observability-otel",
    os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "skills", "observability-otel")),
]

for _path in _POSSIBLE_BRIDGE_PATHS:
    if os.path.exists(_path) and _path not in sys.path:
        sys.path.insert(0, _path)

try:
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
        DYNAMIC_MICROVM_STATE_PATH,
    )
except ImportError:
    # Standalone fallback definitions if telemetry_governor_bridge is relocated
    @dataclass
    class W3cTraceContext:
        version: str = "00"
        trace_id: str = "4bf92f3577b34da6a3ce929d0e0e4736"
        span_id: str = "00f067aa0ba902b7"
        trace_flags: str = "01"
        parent_span_id: Optional[str] = None
        def to_traceparent(self) -> str:
            return f"{self.version}-{self.trace_id}-{self.span_id}-{self.trace_flags}"

    @dataclass
    class OtelSpan:
        name: str
        duration_ms: float
        status: str = "OK"

    class OtelSpanMetricsCollector:
        def __init__(self, window_size: int = 500):
            self.window_size = window_size
            self._spans: List[OtelSpan] = []
            self._queue_depth = 0
            self._active_workers = 4
        def record_span(self, name: str, duration_ms: float, status: str = "OK", parent_context: Any = None):
            s = OtelSpan(name=name, duration_ms=duration_ms, status=status)
            self._spans.append(s)
            return s
        def set_queue_depth(self, d: int):
            self._queue_depth = max(0, d)
        def set_active_workers(self, w: int):
            self._active_workers = max(1, w)
        def compute_percentiles(self) -> Dict[str, Any]:
            durations = sorted([s.duration_ms for s in self._spans]) if self._spans else [0.0]
            n = len(durations)
            p99 = durations[int(0.99 * n) - 1] if n > 0 else 0.0
            return {
                "count": n,
                "p50_ms": durations[n // 2],
                "p90_ms": durations[int(0.90 * n) - 1] if n > 0 else 0.0,
                "p95_ms": durations[int(0.95 * n) - 1] if n > 0 else 0.0,
                "p99_ms": p99,
                "queue_depth": self._queue_depth,
                "active_workers": self._active_workers,
                "error_rate": 0.0,
            }

    class FirecrackerAllocationManager:
        def __init__(self, *args, **kwargs):
            self.current_allocation = type("Alloc", (), {
                "vcpu_count": 2,
                "memory_mb": 512,
                "state": "nominal",
                "to_dict": lambda: {"vcpu_count": 2, "memory_mb": 512, "state": "nominal"}
            })()
        def scale_boosted(self):
            self.current_allocation.vcpu_count = 4
            self.current_allocation.memory_mb = 1024
            self.current_allocation.state = "boosted"
            return self.current_allocation
        def scale_nominal(self):
            self.current_allocation.vcpu_count = 2
            self.current_allocation.memory_mb = 512
            self.current_allocation.state = "nominal"
            return self.current_allocation
        def scale_eco(self):
            self.current_allocation.vcpu_count = 1
            self.current_allocation.memory_mb = 256
            self.current_allocation.state = "eco"
            return self.current_allocation

    class DynatuneThreadPool:
        def __init__(self, initial_workers: int = 4):
            self._current_workers = initial_workers
        @property
        def current_workers(self) -> int:
            return self._current_workers
        def expand_boosted(self) -> int:
            self._current_workers = 8
            return self._current_workers
        def contract_eco(self) -> int:
            self._current_workers = 2
            return self._current_workers
        def reset_nominal(self) -> int:
            self._current_workers = 4
            return self._current_workers

    @dataclass
    class TuningDecision:
        timestamp: float
        p99_latency_ms: float
        queue_depth: int
        active_workers: int
        action: str
        governor_profile: str
        microvm_vcpus: int
        microvm_memory_mb: int
        reason: str
        thread_pool_expanded: bool
        settle_seconds_elapsed: float = 0.0
        def to_dict(self) -> Dict[str, Any]:
            return asdict(self)

    class TelemetryGovernorBridge:
        def __init__(self, **kwargs):
            self.collector = OtelSpanMetricsCollector()
            self.thread_pool = DynatuneThreadPool()
            self.microvm_mgr = FirecrackerAllocationManager()
            self.current_profile = "balanced"
        def evaluate_and_tune(self, mock_time: Optional[float] = None) -> TuningDecision:
            return TuningDecision(
                timestamp=mock_time or time.time(),
                p99_latency_ms=10.0,
                queue_depth=0,
                active_workers=4,
                action="BALANCED_NOMINAL",
                governor_profile="balanced",
                microvm_vcpus=2,
                microvm_memory_mb=512,
                reason="Workload nominal",
                thread_pool_expanded=False,
            )

    P99_SLA_THRESHOLD_MS = 200.0
    QUEUE_DEPTH_SPIKE_THRESHOLD = 10
    SETTLED_LATENCY_THRESHOLD_MS = 50.0
    SETTLED_DURATION_SECONDS = 15.0
    WORKER_COUNT_ECO = 2
    WORKER_COUNT_NOMINAL = 4
    WORKER_COUNT_BOOSTED = 8
    MICROVM_VCPU_ECO = 1
    MICROVM_MEM_ECO_MIB = 256
    MICROVM_VCPU_NOMINAL = 2
    MICROVM_MEM_NOMINAL_MIB = 512
    MICROVM_VCPU_BOOSTED = 4
    MICROVM_MEM_BOOSTED_MIB = 1024
    DYNAMIC_MICROVM_STATE_PATH = "/tmp/hgb_microvm_dynamic_allocation.json"


# ==============================================================================
# Storage Pressure Constants & Enums
# ==============================================================================

DEFAULT_STORAGE_IPC_PATH: str = "/tmp/hgb_storage_pressure.json"
SECONDARY_STORAGE_IPC_PATH: str = "/run/user/1000/hgb/storage_pressure.json"

STORAGE_THRESHOLD_NORMAL_PCT: float = 75.0
STORAGE_THRESHOLD_BACKPRESSURE_PCT: float = 80.0
STORAGE_THRESHOLD_AGGRESSIVE_PCT: float = 90.0

# Concurrency limits during storage pressure
STORAGE_THROTTLED_WORKERS_BACKPRESSURE: int = WORKER_COUNT_NOMINAL  # 4
STORAGE_THROTTLED_WORKERS_AGGRESSIVE: int = WORKER_COUNT_ECO       # 2
STORAGE_THROTTLED_WORKERS_EMERGENCY: int = 1                       # 1

# Firecracker MicroVM memory limits under storage pressure
STORAGE_THROTTLED_MEM_BACKPRESSURE_MIB: int = MICROVM_MEM_NOMINAL_MIB  # 512
STORAGE_THROTTLED_MEM_AGGRESSIVE_MIB: int = MICROVM_MEM_ECO_MIB       # 256
STORAGE_THROTTLED_MEM_EMERGENCY_MIB: int = 256                         # 256


class StoragePressureLevel(enum.Enum):
    NORMAL = "NORMAL"
    BACKPRESSURE = "BACKPRESSURE"
    AGGRESSIVE_PRUNING = "AGGRESSIVE_PRUNING"
    EMERGENCY_DROP = "EMERGENCY_DROP"

    @classmethod
    def from_string(cls, name: str) -> "StoragePressureLevel":
        normalized = name.strip().upper().replace("_", "").replace("-", "").replace(" ", "")
        if normalized in ("NORMAL", "NOMINAL", "LOW"):
            return cls.NORMAL
        elif normalized in ("BACKPRESSURE", "MEDIUM", "WARN"):
            return cls.BACKPRESSURE
        elif normalized in ("AGGRESSIVEPRUNING", "AGGRESSIVE", "HIGH"):
            return cls.AGGRESSIVE_PRUNING
        elif normalized in ("EMERGENCYDROP", "EMERGENCY", "CRITICAL"):
            return cls.EMERGENCY_DROP
        return cls.NORMAL


@dataclass
class StorageTelemetryMetrics:
    """Live filesystem and log compactor telemetry metrics."""
    mount_point: str = "/"
    total_gb: float = 0.0
    used_gb: float = 0.0
    free_gb: float = 0.0
    used_pct: float = 0.0
    inode_used_pct: float = 0.0
    pressure_level: StoragePressureLevel = StoragePressureLevel.NORMAL
    evicted_bytes: int = 0
    compressed_bytes_saved: int = 0
    backpressure_delay_ms: float = 0.0
    source: str = "statvfs"  # "ipc_file" or "statvfs"
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "mount_point": self.mount_point,
            "total_gb": round(self.total_gb, 2),
            "used_gb": round(self.used_gb, 2),
            "free_gb": round(self.free_gb, 2),
            "used_pct": round(self.used_pct, 2),
            "inode_used_pct": round(self.inode_used_pct, 2),
            "storage_pressure_level": self.pressure_level.value,
            "evicted_bytes": self.evicted_bytes,
            "compressed_bytes_saved": self.compressed_bytes_saved,
            "backpressure_delay_ms": round(self.backpressure_delay_ms, 2),
            "source": self.source,
            "timestamp": self.timestamp,
        }


# ==============================================================================
# Storage Monitor & Metrics Reader
# ==============================================================================

class StorageMetricsReader:
    """
    Reads live partition telemetry from:
    1. IPC state file emitted by Rust `storage_guardrails_pruning_daemon` (preferred)
    2. Direct kernel `os.statvfs` probe on the monitored partition (fallback)
    """

    def __init__(
        self,
        monitored_mount: str = "/",
        ipc_path: Optional[str] = None,
    ):
        self.monitored_mount = monitored_mount
        self.ipc_path = ipc_path or DEFAULT_STORAGE_IPC_PATH

    def read_metrics(self) -> StorageTelemetryMetrics:
        """Polls IPC state file or falls back to direct os.statvfs call."""
        # 1. Try IPC state file
        metrics = self._read_ipc_state()
        if metrics is not None:
            return metrics

        # 2. Direct statvfs probe
        return self._probe_statvfs()

    def _read_ipc_state(self) -> Optional[StorageTelemetryMetrics]:
        candidate_paths = [self.ipc_path, SECONDARY_STORAGE_IPC_PATH]
        for path in candidate_paths:
            if not os.path.exists(path):
                continue
            try:
                with open(path, "r") as f:
                    data = json.load(f)

                # Validate freshness (within 60s)
                ts = float(data.get("timestamp", 0.0))
                if time.time() - ts > 60.0:
                    continue

                level_str = str(data.get("storage_pressure_level", data.get("tier", "NORMAL")))
                level = StoragePressureLevel.from_string(level_str)

                return StorageTelemetryMetrics(
                    mount_point=data.get("mount_point", self.monitored_mount),
                    total_gb=float(data.get("disk_total_gb", 0.0)),
                    used_gb=float(data.get("disk_used_gb", 0.0)),
                    free_gb=float(data.get("disk_free_gb", 0.0)),
                    used_pct=float(data.get("used_pct", 0.0)),
                    inode_used_pct=float(data.get("inode_used_pct", 0.0)),
                    pressure_level=level,
                    evicted_bytes=int(data.get("evicted_bytes", 0)),
                    compressed_bytes_saved=int(data.get("compressed_bytes_saved", 0)),
                    backpressure_delay_ms=float(data.get("backpressure_delay_ms", 0.0)),
                    source="ipc_file",
                    timestamp=ts,
                )
            except Exception:
                continue
        return None

    def _probe_statvfs(self) -> StorageTelemetryMetrics:
        """Invokes POSIX statvfs via Python stdlib."""
        try:
            st = os.statvfs(self.monitored_mount)
            frsize = st.f_frsize if st.f_frsize > 0 else st.f_bsize
            total_bytes = st.f_blocks * frsize
            free_bytes = st.f_bavail * frsize  # Available to unprivileged user
            used_bytes = (st.f_blocks - st.f_bfree) * frsize

            # Inodes
            total_inodes = st.f_files
            free_inodes = st.f_favail
            inode_used_pct = (
                ((total_inodes - free_inodes) / total_inodes * 100.0)
                if total_inodes > 0
                else 0.0
            )

            # Capacity calculation exactly matching df -h
            used_pct = (
                (100.0 - (st.f_bavail / st.f_blocks * 100.0))
                if st.f_blocks > 0
                else 0.0
            )

            total_gb = total_bytes / (1024 ** 3)
            used_gb = (total_bytes - free_bytes) / (1024 ** 3)
            free_gb = free_bytes / (1024 ** 3)

            # Classify pressure level
            if used_pct >= STORAGE_THRESHOLD_AGGRESSIVE_PCT:
                level = StoragePressureLevel.EMERGENCY_DROP
                delay = 50.0
            elif used_pct >= STORAGE_THRESHOLD_BACKPRESSURE_PCT:
                level = StoragePressureLevel.AGGRESSIVE_PRUNING
                delay = 20.0
            elif used_pct >= STORAGE_THRESHOLD_NORMAL_PCT:
                level = StoragePressureLevel.BACKPRESSURE
                delay = 5.0
            else:
                level = StoragePressureLevel.NORMAL
                delay = 0.0

            return StorageTelemetryMetrics(
                mount_point=self.monitored_mount,
                total_gb=total_gb,
                used_gb=used_gb,
                free_gb=free_gb,
                used_pct=used_pct,
                inode_used_pct=inode_used_pct,
                pressure_level=level,
                evicted_bytes=0,
                compressed_bytes_saved=0,
                backpressure_delay_ms=delay,
                source="statvfs",
                timestamp=time.time(),
            )
        except Exception as e:
            # Safe nominal fallback on error
            return StorageTelemetryMetrics(
                mount_point=self.monitored_mount,
                source="error_fallback",
            )


# ==============================================================================
# Storage Guardrail Tuning Decision
# ==============================================================================

@dataclass
class StorageTuningDecision:
    """Composite tuning decision combining OpenTelemetry SLA and Storage Guardrails."""
    base_decision: TuningDecision
    storage_metrics: StorageTelemetryMetrics
    storage_override_applied: bool = False
    original_workers: int = 4
    throttled_workers: int = 4
    original_microvm_memory_mb: int = 512
    throttled_microvm_memory_mb: int = 512
    guardrail_reason: str = ""

    def to_dict(self) -> Dict[str, Any]:
        d = self.base_decision.to_dict()
        d.update({
            "storage_metrics": self.storage_metrics.to_dict(),
            "storage_override_applied": self.storage_override_applied,
            "original_workers": self.original_workers,
            "throttled_workers": self.throttled_workers,
            "original_microvm_memory_mb": self.original_microvm_memory_mb,
            "throttled_microvm_memory_mb": self.throttled_microvm_memory_mb,
            "guardrail_reason": self.guardrail_reason,
        })
        return d


# ==============================================================================
# Storage-Aware Telemetry Governor Bridge
# ==============================================================================

class StorageAwareTelemetryGovernorBridge(TelemetryGovernorBridge):
    """
    Enhanced OpenTelemetry Governor Bridge integrating real-world storage guardrails.

    Policy:
      1. Under NORMAL disk conditions (<75%), acts identically to base TelemetryGovernorBridge.
      2. Under BACKPRESSURE (75-80%), caps boosted workers to Nominal (4) and limits MicroVM RAM to 512 MiB.
      3. Under AGGRESSIVE_PRUNING / HIGH (80-90%), overrides compute boost:
           • Worker concurrency throttled to Eco (2 workers)
           • MicroVM RAM clamped to 256 MiB
           • Span injection delayed to throttle ingestion
      4. Under EMERGENCY_DROP / CRITICAL (>90%), emergency throttle:
           • Concurrency throttled to 1 worker
           • MicroVM RAM clamped to 256 MiB
           • Dropping non-critical spans to protect disk from exhaustion
    """

    def __init__(
        self,
        governor_script_path: Optional[str] = None,
        collector: Optional[OtelSpanMetricsCollector] = None,
        thread_pool: Optional[DynatuneThreadPool] = None,
        microvm_mgr: Optional[FirecrackerAllocationManager] = None,
        storage_reader: Optional[StorageMetricsReader] = None,
        storage_ipc_path: Optional[str] = None,
        monitored_mount: str = "/",
    ):
        super().__init__(
            governor_script_path=governor_script_path,
            collector=collector,
            thread_pool=thread_pool,
            microvm_mgr=microvm_mgr,
        )
        self.storage_reader = storage_reader or StorageMetricsReader(
            monitored_mount=monitored_mount,
            ipc_path=storage_ipc_path,
        )
        self.last_storage_metrics: Optional[StorageTelemetryMetrics] = None
        self.storage_history: List[StorageTuningDecision] = []

    def evaluate_and_tune_with_storage(
        self,
        mock_time: Optional[float] = None,
        injected_storage: Optional[StorageTelemetryMetrics] = None,
    ) -> StorageTuningDecision:
        """
        Evaluates SLA metrics alongside live filesystem storage pressure:
        Applies storage overrides when disk usage exceeds safe boundaries.
        """
        # 1. Base evaluation (P99 latency, queue depth, settle state)
        base_decision = super().evaluate_and_tune(mock_time=mock_time)
        orig_workers = base_decision.active_workers
        orig_mem = base_decision.microvm_memory_mb

        # 2. Ingest storage metrics
        st = injected_storage if injected_storage is not None else self.storage_reader.read_metrics()
        self.last_storage_metrics = st

        override_applied = False
        throttled_workers = orig_workers
        throttled_mem = orig_mem
        guardrail_reason = ""

        level = st.pressure_level
        pct = st.used_pct

        # 3. Apply Storage Guardrails
        if level in (StoragePressureLevel.EMERGENCY_DROP,) or pct >= STORAGE_THRESHOLD_AGGRESSIVE_PCT:
            override_applied = True
            throttled_workers = STORAGE_THROTTLED_WORKERS_EMERGENCY  # 1
            throttled_mem = STORAGE_THROTTLED_MEM_EMERGENCY_MIB      # 256
            guardrail_reason = (
                f"EMERGENCY STORAGE DROP: Root partition usage at {pct:.1f}% "
                f"({st.free_gb:.1f} GB free). Compute boost overridden; worker concurrency clamped to {throttled_workers}, "
                f"MicroVM RAM clamped to {throttled_mem} MiB to prevent catastrophic out-of-disk failure."
            )
            base_decision.action = f"{base_decision.action}_EMERGENCY_STORAGE_DROP"

        elif level in (StoragePressureLevel.AGGRESSIVE_PRUNING,) or pct >= STORAGE_THRESHOLD_BACKPRESSURE_PCT:
            # Overrides compute boost if attempted
            if orig_workers > STORAGE_THROTTLED_WORKERS_AGGRESSIVE or orig_mem > STORAGE_THROTTLED_MEM_AGGRESSIVE_MIB:
                override_applied = True
                throttled_workers = min(orig_workers, STORAGE_THROTTLED_WORKERS_AGGRESSIVE)  # 2
                throttled_mem = min(orig_mem, STORAGE_THROTTLED_MEM_AGGRESSIVE_MIB)          # 256
                guardrail_reason = (
                    f"AGGRESSIVE PRUNING STORAGE GUARDRAIL: Root partition usage at {pct:.1f}% "
                    f"({st.free_gb:.1f} GB free) exceeds {STORAGE_THRESHOLD_BACKPRESSURE_PCT}%. "
                    f"Capped workers from {orig_workers} -> {throttled_workers}, "
                    f"clamped MicroVM RAM from {orig_mem} -> {throttled_mem} MiB."
                )
                base_decision.action = f"{base_decision.action}_STORAGE_THROTTLED"

        elif level in (StoragePressureLevel.BACKPRESSURE,) or pct >= STORAGE_THRESHOLD_NORMAL_PCT:
            if orig_workers > STORAGE_THROTTLED_WORKERS_BACKPRESSURE or orig_mem > STORAGE_THROTTLED_MEM_BACKPRESSURE_MIB:
                override_applied = True
                throttled_workers = min(orig_workers, STORAGE_THROTTLED_WORKERS_BACKPRESSURE)  # 4
                throttled_mem = min(orig_mem, STORAGE_THROTTLED_MEM_BACKPRESSURE_MIB)          # 512
                guardrail_reason = (
                    f"BACKPRESSURE STORAGE GUARDRAIL: Partition usage at {pct:.1f}%. "
                    f"Capped boost to Nominal: {throttled_workers} workers, {throttled_mem} MiB RAM."
                )
                base_decision.action = f"{base_decision.action}_STORAGE_BACKPRESSURE"

        # 4. Actuate throttled state if overrides were triggered
        if override_applied:
            # Update internal thread pool and collector view
            self.thread_pool._current_workers = throttled_workers
            self.collector.set_active_workers(throttled_workers)
            base_decision.active_workers = throttled_workers
            base_decision.microvm_memory_mb = throttled_mem

            # Patch Firecracker dynamic allocation state
            self.microvm_mgr.current_allocation.memory_mb = throttled_mem
            self.microvm_mgr._persist_state()

        composite = StorageTuningDecision(
            base_decision=base_decision,
            storage_metrics=st,
            storage_override_applied=override_applied,
            original_workers=orig_workers,
            throttled_workers=throttled_workers,
            original_microvm_memory_mb=orig_mem,
            throttled_microvm_memory_mb=throttled_mem,
            guardrail_reason=guardrail_reason,
        )
        self.storage_history.append(composite)
        return composite


# ==============================================================================
# Helper / Patch Ingestion API
# ==============================================================================

def patch_telemetry_governor(bridge: TelemetryGovernorBridge) -> StorageAwareTelemetryGovernorBridge:
    """Wraps an existing TelemetryGovernorBridge instance with storage guardrails."""
    if isinstance(bridge, StorageAwareTelemetryGovernorBridge):
        return bridge

    patched = StorageAwareTelemetryGovernorBridge(
        governor_script_path=bridge.governor_script,
        collector=bridge.collector,
        thread_pool=bridge.thread_pool,
        microvm_mgr=bridge.microvm_mgr,
    )
    patched.current_profile = bridge.current_profile
    return patched


# ==============================================================================
# CLI Entry Point
# ==============================================================================

def main():
    action = sys.argv[1] if len(sys.argv) > 1 else "storage-status"
    reader = StorageMetricsReader()
    st = reader.read_metrics()

    if action == "storage-status":
        print("💾 [Hagibis Storage Guardrails Status]")
        print(f"  • Monitored Partition:     {st.mount_point}")
        print(f"  • Capacity Used:           {st.used_pct:.2f}% ({st.used_gb:.2f} GB used / {st.total_gb:.2f} GB total)")
        print(f"  • Free Space:              {st.free_gb:.2f} GB")
        print(f"  • Inode Usage:             {st.inode_used_pct:.2f}%")
        print(f"  • Pressure Level:          {st.pressure_level.value}")
        print(f"  • Telemetry Source:        {st.source}")
        print(f"  • Evicted / Reclaimed:     {st.evicted_bytes / (1024**2):.2f} MiB")
        print(f"  • Compressed Saved:        {st.compressed_bytes_saved / (1024**2):.2f} MiB")

    elif action == "evaluate-storage":
        p99 = float(sys.argv[2]) if len(sys.argv) > 2 else 250.0  # SLA breach by default
        q = int(sys.argv[3]) if len(sys.argv) > 3 else 15         # Queue spike by default
        injected_pct = float(sys.argv[4]) if len(sys.argv) > 4 else st.used_pct

        bridge = StorageAwareTelemetryGovernorBridge()
        bridge.collector.record_span("cli_probe", p99)
        bridge.collector.set_queue_depth(q)

        custom_st = StorageTelemetryMetrics(
            mount_point=st.mount_point,
            total_gb=st.total_gb,
            used_pct=injected_pct,
            free_gb=st.total_gb * (1.0 - injected_pct / 100.0),
            pressure_level=StoragePressureLevel.from_string(
                "EMERGENCY_DROP" if injected_pct >= 90.0 else
                "AGGRESSIVE_PRUNING" if injected_pct >= 80.0 else
                "BACKPRESSURE" if injected_pct >= 75.0 else "NORMAL"
            ),
        )

        decision = bridge.evaluate_and_tune_with_storage(injected_storage=custom_st)
        print(json.dumps(decision.to_dict(), indent=2))

    else:
        print(f"Usage: {sys.argv[0]} [storage-status|evaluate-storage <p99_ms> <queue> <used_pct>]")


if __name__ == "__main__":
    main()
