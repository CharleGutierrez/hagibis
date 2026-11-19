#!/usr/bin/env python3
"""
ch8_circuit_breaker_governor.py - Autopoietic Circuit-Breaker Governance Engine with Hysteresis
================================================================================================
Project Hagibis: Pillar 3 (Observability/OTel) & Pillar 4 (Autonomic Self-Healing)

Pillar C Enhancement & Reinforcement Backoff in the Self-Healing Loop:
  1. Sliding-Window Anomaly Detection:
     Tracks rolling execution events (default 20 events, 60s sliding window).
     Computes dynamic anomaly score and failure rate percentage. Trips when failure rate
     exceeds anomaly threshold (>= 50% failures over >= 5 events) or consecutive failure threshold.
  2. Full Hysteresis State Machine:
     - CLOSED: Normal execution, mutations permitted.
     - OPEN: Blocked, mutations forbidden. Fibonacci backoff delay with randomized jitter enforced.
     - HALF_OPEN: Trial probing state allowing controlled test invocations once backoff expires.
     - RECOVERY: Probation state requiring N consecutive successful executions before transitioning back to CLOSED.
     - PERM_TRIPPED: Permanently tripped when encountering identical AST error 3 times in a row.
       Auto-mutation halted, state snapshotted into LanceDB, escalated to LLM agent via MCP.
  3. Fibonacci Reinforcement Backoff Dampening:
     delay = min(max_backoff, base_delay * fib(iteration)) + uniform_jitter(0.1, 0.5)
     where fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13, fib(8)=21...
  4. AST Error State Fingerprinting:
     Deterministic BLAKE3 / SHA-256 fingerprinting across diagnostic error code, normalized file,
     line, span, and normalized error message.
  5. LanceDB Snapshots & MCP Escalation:
     Snapshots failing AST state (source context, AST diagnostic, stderr, hashes) into LanceDB
     and synthesizes MCP LLM escalation payload for hgb_compiler_self_heal_ast.
  6. Quarantining & Permission Locking: Mutating permissions stripped (0444 read-only).
  7. Automated Webhook Alerts: Dispatched to OpenTelemetry sink & HTTP endpoints.
"""

from __future__ import annotations

import os
import sys
import time
import math
import json
import random
import hashlib
import shutil
import stat
import urllib.request
import urllib.error
from pathlib import Path
from dataclasses import dataclass, field, asdict
from enum import Enum
from typing import Dict, List, Optional, Tuple, Any, Union
from contextlib import contextmanager

# Try importing native blake3 if installed, otherwise fallback to hashlib
try:
    import blake3 as _native_blake3  # type: ignore
    _HAS_NATIVE_BLAKE3 = True
except ImportError:
    _HAS_NATIVE_BLAKE3 = False


# ==============================================================================
# Configuration Defaults
# ==============================================================================

DEFAULT_STATE_DIR = Path(os.environ.get("HGB_STATE_DIR", Path.home() / ".hagibis"))
DEFAULT_QUARANTINE_DIR = Path(os.environ.get("HGB_QUARANTINE_DIR", DEFAULT_STATE_DIR / "quarantine"))
DEFAULT_REGISTRY_PATH = Path(os.environ.get("HGB_CB_REGISTRY", DEFAULT_STATE_DIR / "circuit_breaker_registry.json"))
DEFAULT_WEBHOOK_URL = os.environ.get("HGB_WEBHOOK_URL", "http://127.0.0.1:8083/api/v1/alerts")
DEFAULT_WEBHOOK_SINK = Path(os.environ.get("HGB_WEBHOOK_SINK", "/tmp/hgb_quarantine_webhook_events.jsonl"))

DEFAULT_MAX_CONSECUTIVE_FAILURES = int(os.environ.get("HGB_CB_MAX_FAILURES", 3))
DEFAULT_T0 = float(os.environ.get("HGB_CB_T0", 2.0))
DEFAULT_TMAX = float(os.environ.get("HGB_CB_TMAX", 64.0))

DEFAULT_WINDOW_SIZE = int(os.environ.get("HGB_CB_WINDOW_SIZE", 20))
DEFAULT_WINDOW_DURATION_SEC = float(os.environ.get("HGB_CB_WINDOW_SEC", 60.0))
DEFAULT_MIN_EVENTS = int(os.environ.get("HGB_CB_MIN_EVENTS", 5))
DEFAULT_ANOMALY_THRESHOLD = float(os.environ.get("HGB_CB_ANOMALY_THRESHOLD", 0.50))
DEFAULT_RECOVERY_THRESHOLD = int(os.environ.get("HGB_CB_RECOVERY_THRESHOLD", 3))


# ==============================================================================
# Domain Models & Circuit Breaker States
# ==============================================================================

class CircuitState(str, Enum):
    CLOSED = "CLOSED"              # Normal execution, mutations permitted
    OPEN = "OPEN"                  # Blocked, mutations forbidden, backoff active
    HALF_OPEN = "HALF_OPEN"        # Trial probing state allowing controlled test invocations
    RECOVERY = "RECOVERY"          # Probation state requiring N consecutive successful executions
    PERM_TRIPPED = "PERM_TRIPPED"  # Permanently tripped due to repeated identical AST failures

    # Backward compatibility aliases
    TRIPPED = "OPEN"
    QUARANTINED = "OPEN"

    @classmethod
    def from_str(cls, val: str) -> CircuitState:
        normalized = val.strip().upper().replace(" ", "_")
        if normalized in ("CLOSED", "HEALTHY"):
            return cls.CLOSED
        elif normalized in ("PERM_TRIPPED", "PERMANENTLY_TRIPPED", "FATAL", "AST_TRIPPED"):
            return cls.PERM_TRIPPED
        elif normalized in ("OPEN", "TRIPPED", "QUARANTINED", "LOCKED"):
            return cls.OPEN
        elif normalized in ("HALF_OPEN", "HALFOPEN", "PROBING"):
            return cls.HALF_OPEN
        elif normalized in ("RECOVERY", "PROBATION"):
            return cls.RECOVERY
        return cls.CLOSED


@dataclass
class WindowEvent:
    timestamp: float
    success: bool
    error_trace: str = ""
    duration_ms: float = 0.0

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> WindowEvent:
        return cls(
            timestamp=float(data.get("timestamp", time.time())),
            success=bool(data.get("success", False)),
            error_trace=str(data.get("error_trace", "")),
            duration_ms=float(data.get("duration_ms", 0.0))
        )


class SlidingWindowAnomalyDetector:
    """
    Sliding-window anomaly detector tracking a rolling window of execution events.
    Computes dynamic anomaly score and failure rate percentage over a rolling
    window (default 20 events or 60 seconds).
    """

    def __init__(
        self,
        window_size: int = DEFAULT_WINDOW_SIZE,
        window_duration_sec: float = DEFAULT_WINDOW_DURATION_SEC,
        min_events: int = DEFAULT_MIN_EVENTS,
        anomaly_threshold: float = DEFAULT_ANOMALY_THRESHOLD
    ):
        self.window_size = max(1, window_size)
        self.window_duration_sec = max(1.0, window_duration_sec)
        self.min_events = max(1, min_events)
        self.anomaly_threshold = max(0.01, min(1.0, anomaly_threshold))
        self._events: List[WindowEvent] = []

    def record_event(
        self,
        success: bool,
        error_trace: str = "",
        duration_ms: float = 0.0,
        timestamp: Optional[float] = None
    ) -> WindowEvent:
        now = timestamp if timestamp is not None else time.time()
        event = WindowEvent(
            timestamp=now,
            success=success,
            error_trace=error_trace,
            duration_ms=duration_ms
        )
        self._events.append(event)
        self._prune(now)
        return event

    def _prune(self, current_time: float) -> None:
        cutoff = current_time - self.window_duration_sec
        self._events = [e for e in self._events if e.timestamp >= cutoff]
        if len(self._events) > self.window_size:
            self._events = self._events[-self.window_size:]

    def compute_metrics(self, current_time: Optional[float] = None) -> Dict[str, Any]:
        now = current_time if current_time is not None else time.time()
        self._prune(now)

        total_events = len(self._events)
        if total_events == 0:
            return {
                "total_events": 0,
                "failure_count": 0,
                "success_count": 0,
                "failure_rate": 0.0,
                "failure_rate_pct": 0.0,
                "anomaly_score": 0.0,
                "is_anomaly": False,
                "window_duration_sec": self.window_duration_sec,
                "min_events": self.min_events,
                "anomaly_threshold": self.anomaly_threshold
            }

        failure_count = sum(1 for e in self._events if not e.success)
        success_count = total_events - failure_count
        failure_rate = failure_count / float(total_events)
        failure_rate_pct = round(failure_rate * 100.0, 2)

        # Dynamic anomaly score scaled by sample confidence
        confidence = min(1.0, total_events / float(self.min_events))
        anomaly_score = round(failure_rate * confidence, 4)

        # Anomaly threshold check
        is_anomaly = (total_events >= self.min_events and failure_rate >= self.anomaly_threshold)

        return {
            "total_events": total_events,
            "failure_count": failure_count,
            "success_count": success_count,
            "failure_rate": round(failure_rate, 4),
            "failure_rate_pct": failure_rate_pct,
            "anomaly_score": anomaly_score,
            "is_anomaly": is_anomaly,
            "window_duration_sec": self.window_duration_sec,
            "min_events": self.min_events,
            "anomaly_threshold": self.anomaly_threshold
        }

    def clear(self) -> None:
        self._events.clear()

    def to_list(self) -> List[Dict[str, Any]]:
        return [e.to_dict() for e in self._events]

    def load_list(self, data: List[Dict[str, Any]]) -> None:
        self._events = [WindowEvent.from_dict(d) for d in data]


@dataclass
class MutationAttempt:
    timestamp: float
    iso_timestamp: str
    skill_id: str
    success: bool
    pre_mutation_hash: str
    post_mutation_hash: str
    error_trace: str = ""
    duration_ms: float = 0.0

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)


@dataclass
class SkillGovernanceRecord:
    skill_id: str
    state: CircuitState = CircuitState.CLOSED
    consecutive_failures: int = 0
    consecutive_successes: int = 0
    recovery_threshold: int = DEFAULT_RECOVERY_THRESHOLD
    total_failures: int = 0
    total_successes: int = 0
    is_quarantined: bool = False
    quarantine_path: Optional[str] = None
    quarantine_reason: Optional[str] = None
    quarantined_at: Optional[str] = None
    last_mutation_timestamp: Optional[float] = None
    last_state_change_timestamp: float = 0.0
    open_until_timestamp: float = 0.0
    anomaly_score: float = 0.0
    failure_rate: float = 0.0
    pre_mutation_hash: Optional[str] = None
    post_mutation_hash: Optional[str] = None
    window_events: List[Dict[str, Any]] = field(default_factory=list)
    last_ast_fingerprint: Optional[str] = None
    consecutive_fingerprint_count: int = 0
    ast_fingerprint_history: List[str] = field(default_factory=list)

    def to_dict(self) -> Dict[str, Any]:
        d = asdict(self)
        d["state"] = self.state.value
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> SkillGovernanceRecord:
        raw_state = data.get("state", CircuitState.CLOSED.value)
        state = CircuitState.from_str(str(raw_state))

        return cls(
            skill_id=data["skill_id"],
            state=state,
            consecutive_failures=data.get("consecutive_failures", 0),
            consecutive_successes=data.get("consecutive_successes", 0),
            recovery_threshold=data.get("recovery_threshold", DEFAULT_RECOVERY_THRESHOLD),
            total_failures=data.get("total_failures", 0),
            total_successes=data.get("total_successes", 0),
            is_quarantined=data.get("is_quarantined", False),
            quarantine_path=data.get("quarantine_path"),
            quarantine_reason=data.get("quarantine_reason"),
            quarantined_at=data.get("quarantined_at"),
            last_mutation_timestamp=data.get("last_mutation_timestamp"),
            last_state_change_timestamp=data.get("last_state_change_timestamp", 0.0),
            open_until_timestamp=data.get("open_until_timestamp", 0.0),
            anomaly_score=data.get("anomaly_score", 0.0),
            failure_rate=data.get("failure_rate", 0.0),
            pre_mutation_hash=data.get("pre_mutation_hash"),
            post_mutation_hash=data.get("post_mutation_hash"),
            window_events=data.get("window_events", []),
            last_ast_fingerprint=data.get("last_ast_fingerprint"),
            consecutive_fingerprint_count=data.get("consecutive_fingerprint_count", 0),
            ast_fingerprint_history=data.get("ast_fingerprint_history", [])
        )


# ==============================================================================
# Autonomic Circuit Breaker Governor Implementation
# ==============================================================================

class AutonomicCircuitBreakerGovernor:
    """
    Enterprise Governor overseeing self-healing autopoietic mutation loops.
    Guarantees state immutability checking, Fibonacci reinforcement backoff,
    deterministic AST error fingerprinting, sliding-window anomaly detection,
    and full hysteresis state transitions (CLOSED, OPEN, HALF_OPEN, RECOVERY, PERM_TRIPPED).
    """

    def __init__(
        self,
        state_dir: Optional[Union[str, Path]] = None,
        quarantine_dir: Optional[Union[str, Path]] = None,
        registry_path: Optional[Union[str, Path]] = None,
        webhook_url: Optional[str] = None,
        webhook_sink: Optional[Union[str, Path]] = None,
        max_consecutive_failures: int = DEFAULT_MAX_CONSECUTIVE_FAILURES,
        t0: float = DEFAULT_T0,
        t_max: float = DEFAULT_TMAX,
        window_size: int = DEFAULT_WINDOW_SIZE,
        window_duration_sec: float = DEFAULT_WINDOW_DURATION_SEC,
        min_events: int = DEFAULT_MIN_EVENTS,
        anomaly_threshold: float = DEFAULT_ANOMALY_THRESHOLD,
        recovery_threshold: int = DEFAULT_RECOVERY_THRESHOLD,
        lancedb_dir: Optional[Union[str, Path]] = None
    ):
        self.state_dir = Path(state_dir) if state_dir else DEFAULT_STATE_DIR
        self.quarantine_dir = Path(quarantine_dir) if quarantine_dir else DEFAULT_QUARANTINE_DIR
        self.registry_path = Path(registry_path) if registry_path else (self.state_dir / "circuit_breaker_registry.json")
        self.webhook_url = webhook_url or DEFAULT_WEBHOOK_URL
        self.webhook_sink = Path(webhook_sink) if webhook_sink else DEFAULT_WEBHOOK_SINK
        self.max_failures = max(1, max_consecutive_failures)
        self.t0 = float(t0)
        self.t_max = float(t_max)
        self.window_size = window_size
        self.window_duration_sec = window_duration_sec
        self.min_events = min_events
        self.anomaly_threshold = anomaly_threshold
        self.recovery_threshold = recovery_threshold
        self.lancedb_dir = Path(lancedb_dir) if lancedb_dir else None

        self._ensure_directories()
        self._registry: Dict[str, SkillGovernanceRecord] = {}
        self._detectors: Dict[str, SlidingWindowAnomalyDetector] = {}
        self._history: List[MutationAttempt] = []
        self._load_registry()

    def _ensure_directories(self) -> None:
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.quarantine_dir.mkdir(parents=True, exist_ok=True)
        if self.webhook_sink.parent:
            self.webhook_sink.parent.mkdir(parents=True, exist_ok=True)

    # --------------------------------------------------------------------------
    # Cryptographic State Hashing
    # --------------------------------------------------------------------------
    @staticmethod
    def compute_state_hash(target: Union[str, Path, bytes]) -> str:
        """
        Computes cryptographic BLAKE3 (or SHA256 fallback) hash of target.
        Handles:
          - Raw bytes
          - Single file (streaming in 64 KiB chunks)
          - Directory trees (sorted paths + deterministic file content manifest)
        """
        if isinstance(target, bytes):
            if _HAS_NATIVE_BLAKE3:
                return f"blake3:{_native_blake3.blake3(target).hexdigest()}"
            return f"sha256:{hashlib.sha256(target).hexdigest()}"

        path = Path(target)
        if not path.exists():
            empty = b""
            if _HAS_NATIVE_BLAKE3:
                return f"blake3:{_native_blake3.blake3(empty).hexdigest()}"
            return f"sha256:{hashlib.sha256(empty).hexdigest()}"

        if path.is_file():
            if _HAS_NATIVE_BLAKE3:
                hasher = _native_blake3.blake3()
                with open(path, "rb") as f:
                    while chunk := f.read(65536):
                        hasher.update(chunk)
                return f"blake3:{hasher.hexdigest()}"
            else:
                hasher = hashlib.sha256()
                with open(path, "rb") as f:
                    while chunk := f.read(65536):
                        hasher.update(chunk)
                return f"sha256:{hasher.hexdigest()}"

        elif path.is_dir():
            entries: List[str] = []
            for root, dirs, files in os.walk(path):
                dirs.sort()
                for file_name in sorted(files):
                    if file_name.startswith("."):
                        continue
                    full_p = Path(root) / file_name
                    rel_p = full_p.relative_to(path).as_posix()
                    h = hashlib.sha256()
                    try:
                        with open(full_p, "rb") as f:
                            while chunk := f.read(65536):
                                h.update(chunk)
                        file_hex = h.hexdigest()
                    except Exception:
                        file_hex = "err"
                    entries.append(f"{rel_p}:{file_hex}")

            entries.sort()
            manifest_bytes = "\n".join(entries).encode("utf-8")
            if _HAS_NATIVE_BLAKE3:
                return f"blake3:{_native_blake3.blake3(manifest_bytes).hexdigest()}"
            return f"sha256:{hashlib.sha256(manifest_bytes).hexdigest()}"

        return "sha256:unknown"

    # --------------------------------------------------------------------------
    # Fibonacci Progression & Reinforcement Backoff Dampening
    # --------------------------------------------------------------------------
    @staticmethod
    def fibonacci(n: int) -> int:
        """
        Calculates the n-th Fibonacci number where:
          fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13, fib(8)=21, fib(9)=34...
        """
        if n <= 0:
            return 0
        if n in (1, 2):
            return 1
        a, b = 1, 1
        for _ in range(3, n + 1):
            a, b = b, a + b
        return b

    def calculate_fibonacci_backoff(
        self,
        iteration: int,
        with_jitter: bool = True,
        jitter_seed: Optional[int] = None,
        jitter_min: float = 0.1,
        jitter_max: float = 0.5
    ) -> float:
        """
        Calculates strict Fibonacci backoff dampening:
          delay = min(max_backoff, base_delay * fib(iteration)) + uniform_jitter(0.1, 0.5)
        Progression: fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13...
        """
        iter_clamped = max(1, iteration)
        fib_val = self.fibonacci(iter_clamped)
        base_backoff = min(self.t_max, self.t0 * float(fib_val))

        jitter = 0.0
        if with_jitter:
            rng = random.Random(jitter_seed) if jitter_seed is not None else random
            jitter = rng.uniform(jitter_min, jitter_max)

        return round(base_backoff + jitter, 4)

    def calculate_backoff(
        self,
        iteration: int,
        with_jitter: bool = True,
        jitter_seed: Optional[int] = None,
        jitter_min: float = 0.1,
        jitter_max: float = 0.5,
        mode: str = "fibonacci"
    ) -> float:
        """
        Calculates backoff dampening. Defaults to Fibonacci reinforcement progression.
        """
        if mode == "fibonacci":
            return self.calculate_fibonacci_backoff(
                iteration=iteration,
                with_jitter=with_jitter,
                jitter_seed=jitter_seed,
                jitter_min=jitter_min,
                jitter_max=jitter_max
            )
        # Fallback exponential mode: 2^(iter - 1)
        iter_clamped = max(1, iteration)
        base_backoff = min(self.t_max, self.t0 * (2.0 ** (iter_clamped - 1)))
        jitter = 0.0
        if with_jitter:
            rng = random.Random(jitter_seed) if jitter_seed is not None else random
            jitter = rng.uniform(jitter_min, jitter_max)
        return round(base_backoff + jitter, 4)

    def apply_backoff(
        self,
        skill_id: str,
        iteration: Optional[int] = None,
        dry_run: bool = False
    ) -> float:
        """
        Computes Fibonacci backoff and dampens the loop by sleeping.
        Returns the duration backed off.
        """
        rec = self._get_or_create_skill(skill_id)
        current_iter = iteration if iteration is not None else max(1, rec.consecutive_failures)
        delay = self.calculate_fibonacci_backoff(current_iter, with_jitter=True)

        if not dry_run and delay > 0:
            time.sleep(delay)
        return delay

    # --------------------------------------------------------------------------
    # AST Error State Fingerprinting
    # --------------------------------------------------------------------------
    @staticmethod
    def compute_ast_error_fingerprint(
        error_code: str,
        file_path: Union[str, Path],
        line: Union[int, str],
        span: str = "",
        error_message: str = "",
        use_blake3: bool = True
    ) -> str:
        """
        Computes deterministic SHA-256 or BLAKE3 hash of compiler error diagnostic.
        Canonical payload: error_code, normalized file path, line, span, normalized error message.
        """
        norm_file = os.path.normpath(str(file_path)).strip()
        norm_code = str(error_code).strip()
        norm_line = str(line).strip()
        norm_span = str(span).strip()
        norm_msg = " ".join(str(error_message).split()).strip()

        canonical_repr = f"{norm_code}|{norm_file}|{norm_line}|{norm_span}|{norm_msg}"
        data_bytes = canonical_repr.encode("utf-8")

        if use_blake3 and _HAS_NATIVE_BLAKE3:
            return _native_blake3.blake3(data_bytes).hexdigest()
        return hashlib.sha256(data_bytes).hexdigest()

    # --------------------------------------------------------------------------
    # LanceDB Snapshotting & MCP Escalation Payload Synthesis
    # --------------------------------------------------------------------------
    def _resolve_lancedb_base_dir(self) -> Path:
        if self.lancedb_dir and Path(self.lancedb_dir).exists():
            return Path(self.lancedb_dir)

        # Look in workspace root
        curr = Path(__file__).resolve()
        for parent in [curr.parent, curr.parent.parent, curr.parent.parent.parent]:
            candidate = parent / "skills" / "vector-lancedb"
            if candidate.exists():
                return candidate

        lexar_candidate = Path("/media/toshiba/Lexar/hagibis/skills/vector-lancedb")
        if lexar_candidate.exists():
            return lexar_candidate

        fallback = self.state_dir / "vector-lancedb"
        fallback.mkdir(parents=True, exist_ok=True)
        return fallback

    def snapshot_failing_state_to_lancedb(
        self,
        skill_id: str,
        fingerprint: str,
        consecutive_count: int,
        target_file: Union[str, Path],
        diagnostic_code: str,
        diagnostic_line: Union[int, str],
        diagnostic_span: str = "",
        diagnostic_message: str = "",
        compiler_stderr: str = "",
        source_context: str = "",
        pre_patch_hash: str = ""
    ) -> Dict[str, Any]:
        """
        Snapshots failing AST error state into LanceDB directory structure:
          skills/vector-lancedb/failing_ast_snapshots/<timestamp>_<fingerprint[:12]>.json
          skills/vector-lancedb/failing_state_snapshot.json
          skills/vector-lancedb/failing_ast_dataset.lance/ (columnar metadata manifest)
        """
        base_lancedb = self._resolve_lancedb_base_dir()
        snapshots_dir = base_lancedb / "failing_ast_snapshots"
        try:
            snapshots_dir.mkdir(parents=True, exist_ok=True)
        except OSError:
            snapshots_dir = self.state_dir / "failing_ast_snapshots"
            snapshots_dir.mkdir(parents=True, exist_ok=True)

        now_sec = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_sec))

        # Extract source_context if not provided
        if not source_context and target_file and Path(target_file).exists():
            try:
                with open(target_file, "r", encoding="utf-8", errors="replace") as f:
                    lines = f.readlines()
                ln = int(diagnostic_line) if str(diagnostic_line).isdigit() else 1
                start_l = max(0, ln - 10)
                end_l = min(len(lines), ln + 10)
                snippet_lines = []
                for idx in range(start_l, end_l):
                    prefix = ">> " if idx + 1 == ln else "   "
                    snippet_lines.append(f"{prefix}{idx+1:4d} | {lines[idx].rstrip()}")
                source_context = "\n".join(snippet_lines)
            except Exception:
                source_context = ""

        if not pre_patch_hash and target_file and Path(target_file).exists():
            pre_patch_hash = self.compute_state_hash(target_file)

        snapshot_record = {
            "version": "1.0",
            "fingerprint": fingerprint,
            "consecutive_occurrences": consecutive_count,
            "timestamp_iso": iso_now,
            "timestamp_unix": now_sec,
            "skill_id": skill_id,
            "target_file": str(target_file),
            "diagnostic_code": diagnostic_code,
            "diagnostic_line": int(diagnostic_line) if str(diagnostic_line).isdigit() else 1,
            "diagnostic_span": diagnostic_span,
            "diagnostic_message": diagnostic_message,
            "compiler_stderr": compiler_stderr,
            "source_context": source_context,
            "pre_patch_hash": pre_patch_hash,
            "circuit_state": CircuitState.PERM_TRIPPED.value,
            "auto_mutation_halted": True,
            "escalated_to_mcp": True,
            "mcp_target_tool": "hgb_compiler_self_heal_ast"
        }

        # 1. Unique snapshot JSON
        filename = f"snapshot_{int(now_sec)}_{fingerprint[:12]}.json"
        snapshot_file = snapshots_dir / filename
        with open(snapshot_file, "w", encoding="utf-8") as f:
            json.dump(snapshot_record, f, indent=2)

        # 2. Latest failing state snapshot pointer
        latest_file = base_lancedb / "failing_state_snapshot.json"
        try:
            with open(latest_file, "w", encoding="utf-8") as f:
                json.dump(snapshot_record, f, indent=2)
        except OSError:
            latest_file = self.state_dir / "failing_state_snapshot.json"
            with open(latest_file, "w", encoding="utf-8") as f:
                json.dump(snapshot_record, f, indent=2)

        # 3. LanceDB dataset columnar version manifest
        lance_dataset = base_lancedb / "failing_ast_dataset.lance"
        manifest_path = ""
        try:
            lance_dataset.mkdir(parents=True, exist_ok=True)
            manifests_dir = lance_dataset / "_versions"
            manifests_dir.mkdir(parents=True, exist_ok=True)
            manifest_file = manifests_dir / f"{int(now_sec)}.manifest"
            with open(manifest_file, "w", encoding="utf-8") as f:
                json.dump({
                    "format": "lance-columnar-v1",
                    "table": "failing_ast_snapshots",
                    "record": snapshot_record
                }, f, indent=2)
            with open(lance_dataset / "_latest_version", "w", encoding="utf-8") as f:
                f.write(str(int(now_sec)))
            manifest_path = str(manifest_file)
        except OSError:
            pass

        return {
            "snapshot_file": str(snapshot_file),
            "latest_file": str(latest_file),
            "manifest_file": manifest_path,
            "record": snapshot_record
        }

    def generate_mcp_escalation_payload(
        self,
        skill_id: str,
        fingerprint: str,
        consecutive_count: int,
        target_file: Union[str, Path],
        diagnostic_code: str,
        diagnostic_line: Union[int, str],
        diagnostic_message: str,
        source_context: str = "",
        compiler_stderr: str = ""
    ) -> Dict[str, Any]:
        """
        Constructs an MCP LLM agent escalation payload matching hagibis.mcp.json
        tool definition for 'hgb_compiler_self_heal_ast'.
        """
        now_sec = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_sec))

        if not source_context and target_file and Path(target_file).exists():
            try:
                with open(target_file, "r", encoding="utf-8", errors="replace") as f:
                    lines = f.readlines()
                ln = int(diagnostic_line) if str(diagnostic_line).isdigit() else 1
                start_l = max(0, ln - 10)
                end_l = min(len(lines), ln + 10)
                snippet_lines = []
                for idx in range(start_l, end_l):
                    prefix = ">> " if idx + 1 == ln else "   "
                    snippet_lines.append(f"{prefix}{idx+1:4d} | {lines[idx].rstrip()}")
                source_context = "\n".join(snippet_lines)
            except Exception:
                pass

        crate_name = "hagibis"
        tf_str = str(target_file)
        if "crates/" in tf_str:
            crate_name = tf_str.split("crates/")[1].split("/")[0]
        elif "skills/" in tf_str:
            crate_name = tf_str.split("skills/")[1].split("/")[0]

        call_id = f"mcp_call_{int(now_sec)}_{fingerprint[:8]}"
        mcp_payload = {
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "tools/call",
            "params": {
                "name": "hgb_compiler_self_heal_ast",
                "arguments": {
                    "crate_name": crate_name,
                    "target_file": str(target_file),
                    "diagnostic_code": diagnostic_code,
                    "error_message": diagnostic_message,
                    "error_line": int(diagnostic_line) if str(diagnostic_line).isdigit() else 1,
                    "error_column": 1,
                    "ast_context_snippet": source_context,
                    "suggested_patch": "",
                    "dry_run": False,
                    "auto_rollback_on_failure": True
                }
            },
            "metadata": {
                "escalation_reason": f"Identical AST compiler error encountered {consecutive_count} times in a row. Auto-mutation halted to prevent infinite loop.",
                "ast_fingerprint": fingerprint,
                "consecutive_identical_occurrences": consecutive_count,
                "timestamp": iso_now,
                "skill_id": skill_id,
                "compiler_stderr_tail": compiler_stderr[-500:] if compiler_stderr else ""
            }
        }

        # Write to state dir and lancedb snapshots dir
        mcp_out = self.state_dir / "mcp_escalation_payload.json"
        try:
            with open(mcp_out, "w", encoding="utf-8") as f:
                json.dump(mcp_payload, f, indent=2)
        except Exception:
            pass

        base_lancedb = self._resolve_lancedb_base_dir()
        try:
            lancedb_mcp = base_lancedb / "failing_ast_snapshots" / f"mcp_escalation_{int(now_sec)}.json"
            with open(lancedb_mcp, "w", encoding="utf-8") as f:
                json.dump(mcp_payload, f, indent=2)
        except Exception:
            pass

        return mcp_payload

    # --------------------------------------------------------------------------
    # Webhook Alert Dispatcher
    # --------------------------------------------------------------------------
    def dispatch_webhook_alert(
        self,
        event_type: str,
        payload: Dict[str, Any],
        webhook_url: Optional[str] = None
    ) -> bool:
        """
        Dispatches structured JSON Webhook alert to configured endpoint.
        Gracefully falls back to local webhook sink log file if unreachable.
        """
        url = webhook_url or self.webhook_url
        envelope = {
            "version": "2.0",
            "event": event_type,
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            **payload
        }
        raw_json = json.dumps(envelope, indent=2)

        # 1. Local Persistent Sink
        try:
            with open(self.webhook_sink, "a", encoding="utf-8") as sink_f:
                sink_f.write(json.dumps(envelope) + "\n")
        except Exception as e:
            sys.stderr.write(f"[WARN] Failed to write to webhook sink {self.webhook_sink}: {e}\n")

        # 2. Network Dispatch
        req = urllib.request.Request(
            url,
            data=raw_json.encode("utf-8"),
            headers={
                "Content-Type": "application/json",
                "User-Agent": "Hagibis-Autopoietic-Governor/2.0",
                "X-Hagibis-Event": event_type
            },
            method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=3.0) as resp:
                status = resp.status
                return 200 <= status < 300
        except Exception:
            return False

    # --------------------------------------------------------------------------
    # Quarantine & Permission Locking
    # --------------------------------------------------------------------------
    def quarantine_skill(
        self,
        skill_id: str,
        skill_path: Optional[Union[str, Path]],
        reason: str,
        pre_hash: str = "",
        post_hash: str = ""
    ) -> Dict[str, Any]:
        """
        Isolates a failing skill into ~/.hagibis/quarantine/<skill_id>/<timestamp>,
        locks mutating permissions (read-only 0444), updates registry, and alerts HITL.
        """
        ts = int(time.time())
        dest_dir = self.quarantine_dir / skill_id / str(ts)
        dest_dir.mkdir(parents=True, exist_ok=True)

        copied_path = None
        if skill_path:
            src = Path(skill_path)
            if src.exists():
                if src.is_file():
                    copied_target = dest_dir / src.name
                    shutil.copy2(src, copied_target)
                    copied_path = str(copied_target)
                    os.chmod(copied_target, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
                    try:
                        os.chmod(src, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
                    except OSError:
                        pass
                elif src.is_dir():
                    copied_target = dest_dir / "payload"
                    shutil.copytree(src, copied_target, dirs_exist_ok=True)
                    copied_path = str(copied_target)
                    for root, dirs, files in os.walk(dest_dir):
                        for f in files:
                            try:
                                os.chmod(os.path.join(root, f), 0o444)
                            except OSError:
                                pass
                        for d in dirs:
                            try:
                                os.chmod(os.path.join(root, d), 0o555)
                            except OSError:
                                pass
                    for root, dirs, files in os.walk(src):
                        for f in files:
                            try:
                                os.chmod(os.path.join(root, f), 0o444)
                            except OSError:
                                pass

        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        rec = self._get_or_create_skill(skill_id)

        manifest = {
            "skill_id": skill_id,
            "quarantined_at": iso_now,
            "consecutive_failures": rec.consecutive_failures,
            "anomaly_score": rec.anomaly_score,
            "failure_rate": rec.failure_rate,
            "pre_mutation_hash": pre_hash,
            "post_mutation_hash": post_hash,
            "quarantine_reason": reason,
            "original_path": str(skill_path) if skill_path else None,
            "quarantine_destination": str(dest_dir),
            "permissions_locked": True,
            "circuit_state": CircuitState.OPEN.value
        }
        manifest_file = dest_dir / "quarantine_manifest.json"
        with open(manifest_file, "w", encoding="utf-8") as mf:
            json.dump(manifest, mf, indent=2)
        try:
            os.chmod(manifest_file, 0o444)
        except OSError:
            pass

        rec.is_quarantined = True
        rec.quarantine_path = str(dest_dir)
        rec.quarantine_reason = reason
        rec.quarantined_at = iso_now
        rec.pre_mutation_hash = pre_hash
        rec.post_mutation_hash = post_hash
        self._save_registry()

        webhook_payload = {
            "skill_id": skill_id,
            "action": "quarantine_alert",
            "consecutive_failures": rec.consecutive_failures,
            "threshold": self.max_failures,
            "anomaly_score": rec.anomaly_score,
            "failure_rate": rec.failure_rate,
            "pre_mutation_hash": pre_hash,
            "post_mutation_hash": post_hash,
            "quarantine_path": str(dest_dir),
            "error_trace": reason,
            "action_required": "Human-In-The-Loop (HITL) audit required. Mutating permissions revoked.",
            "circuit_breaker_state": CircuitState.OPEN.value
        }
        self.dispatch_webhook_alert("quarantine_alert", webhook_payload)

        return manifest

    # --------------------------------------------------------------------------
    # Circuit Breaker Status & Hysteresis Transitions
    # --------------------------------------------------------------------------
    def can_mutate(self, skill_id: str, current_time: Optional[float] = None) -> Tuple[bool, str]:
        """
        Determines whether a skill is legally authorized to execute mutations
        according to the full hysteresis state machine:
          - CLOSED: Healthy, permitted.
          - OPEN: Blocked until backoff expires. Automatically transitions to HALF_OPEN when delay elapses.
          - HALF_OPEN: Trial probe permitted.
          - RECOVERY: Probation permitted.
          - PERM_TRIPPED: Permanently blocked due to 3 consecutive identical AST errors.
            Auto-mutation halted, requires HITL reset or MCP LLM intervention.
        """
        now = current_time if current_time is not None else time.time()
        rec = self._get_or_create_skill(skill_id)

        if rec.state == CircuitState.PERM_TRIPPED:
            return (
                False,
                f"Skill [{skill_id}] is PERM_TRIPPED (consecutive identical AST error '{rec.last_ast_fingerprint}' "
                f"exceeded 3 occurrences). Auto-mutation permanently halted. Snapshot saved to LanceDB. MCP escalation required."
            )

        if rec.state == CircuitState.OPEN or rec.is_quarantined:
            # Check if exponential backoff delay has expired
            if rec.open_until_timestamp > 0 and now >= rec.open_until_timestamp:
                # Transition OPEN -> HALF_OPEN (Trial probing)
                rec.state = CircuitState.HALF_OPEN
                rec.last_state_change_timestamp = now
                self._save_registry()
                return (
                    True,
                    f"Skill [{skill_id}] backoff delay expired; transitioned from OPEN to HALF_OPEN. Trial probe permitted."
                )
            else:
                remaining = max(0.0, rec.open_until_timestamp - now)
                return (
                    False,
                    f"Skill [{skill_id}] is OPEN/QUARANTINED ({rec.consecutive_failures} failures, "
                    f"anomaly score: {rec.anomaly_score:.2f}, failure rate: {rec.failure_rate:.1%}). "
                    f"Mutating permissions locked. Reason: {rec.quarantine_reason}. Backoff remaining: {remaining:.2f}s."
                )

        elif rec.state == CircuitState.HALF_OPEN:
            return True, f"Skill [{skill_id}] is in HALF_OPEN trial probing state."

        elif rec.state == CircuitState.RECOVERY:
            return (
                True,
                f"Skill [{skill_id}] is in RECOVERY probation ({rec.consecutive_successes}/{rec.recovery_threshold} successes)."
            )

        # CLOSED
        return True, "Skill is healthy and authorized for mutations."

    def record_mutation_attempt(
        self,
        skill_id: str,
        pre_hash: str,
        post_hash: str,
        success: bool,
        error_trace: str = "",
        skill_path: Optional[Union[str, Path]] = None,
        duration_ms: float = 0.0,
        current_time: Optional[float] = None,
        ast_fingerprint: Optional[str] = None,
        diagnostic_code: str = "",
        diagnostic_line: Union[int, str] = 1,
        diagnostic_span: str = "",
        diagnostic_message: str = "",
        compiler_stderr: str = "",
        source_context: str = ""
    ) -> Dict[str, Any]:
        """
        Primary lifecycle state transition handler implementing full hysteresis,
        sliding-window anomaly detection, and consecutive identical AST error circuit breaking.
        """
        now = current_time if current_time is not None else time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now))
        rec = self._get_or_create_skill(skill_id)
        detector = self._get_or_create_detector(skill_id)

        # 1. Record event in sliding window
        detector.record_event(
            success=success,
            error_trace=error_trace,
            duration_ms=duration_ms,
            timestamp=now
        )
        rec.window_events = detector.to_list()

        # 2. Compute dynamic anomaly score and failure rate
        metrics = detector.compute_metrics(current_time=now)
        rec.anomaly_score = metrics["anomaly_score"]
        rec.failure_rate = metrics["failure_rate"]
        is_anomaly = metrics["is_anomaly"]

        # 3. Record mutation history
        attempt = MutationAttempt(
            timestamp=now,
            iso_timestamp=iso_now,
            skill_id=skill_id,
            success=success,
            pre_mutation_hash=pre_hash,
            post_mutation_hash=post_hash,
            error_trace=error_trace,
            duration_ms=duration_ms
        )
        self._history.append(attempt)
        if len(self._history) > 100:
            self._history = self._history[-100:]

        rec.last_mutation_timestamp = now
        rec.pre_mutation_hash = pre_hash
        rec.post_mutation_hash = post_hash

        outcome: Dict[str, Any] = {
            "skill_id": skill_id,
            "success": success,
            "timestamp": iso_now,
            "pre_hash": pre_hash,
            "post_hash": post_hash,
            "anomaly_score": rec.anomaly_score,
            "failure_rate": rec.failure_rate,
            "failure_rate_pct": metrics["failure_rate_pct"],
            "total_window_events": metrics["total_events"]
        }

        # 4. AST Error Fingerprinting & Consecutive Identical Check
        if not success:
            if not ast_fingerprint and (diagnostic_code or error_trace):
                ast_fingerprint = self.compute_ast_error_fingerprint(
                    error_code=diagnostic_code or "E_UNKNOWN",
                    file_path=skill_path or skill_id,
                    line=diagnostic_line,
                    span=diagnostic_span,
                    error_message=diagnostic_message or error_trace
                )

            if ast_fingerprint:
                if rec.last_ast_fingerprint == ast_fingerprint:
                    rec.consecutive_fingerprint_count += 1
                else:
                    rec.last_ast_fingerprint = ast_fingerprint
                    rec.consecutive_fingerprint_count = 1

                rec.ast_fingerprint_history.append(ast_fingerprint)
                if len(rec.ast_fingerprint_history) > 50:
                    rec.ast_fingerprint_history = rec.ast_fingerprint_history[-50:]

                outcome["ast_fingerprint"] = ast_fingerprint
                outcome["consecutive_fingerprint_count"] = rec.consecutive_fingerprint_count

                # Circuit breaker trip on 3 consecutive identical AST errors!
                if rec.consecutive_fingerprint_count >= 3:
                    rec.state = CircuitState.PERM_TRIPPED
                    rec.last_state_change_timestamp = now
                    rec.is_quarantined = True
                    rec.quarantine_reason = (
                        f"Consecutive identical AST error threshold (3) exceeded: {ast_fingerprint} ({diagnostic_code})"
                    )
                    rec.open_until_timestamp = float("inf")

                    # Lock file permissions (0444)
                    if skill_path:
                        self.quarantine_skill(
                            skill_id=skill_id,
                            skill_path=skill_path,
                            reason=rec.quarantine_reason,
                            pre_hash=pre_hash,
                            post_hash=post_hash
                        )

                    # Snapshot failing state into LanceDB
                    lancedb_snap = self.snapshot_failing_state_to_lancedb(
                        skill_id=skill_id,
                        fingerprint=ast_fingerprint,
                        consecutive_count=rec.consecutive_fingerprint_count,
                        target_file=skill_path or skill_id,
                        diagnostic_code=diagnostic_code,
                        diagnostic_line=diagnostic_line,
                        diagnostic_span=diagnostic_span,
                        diagnostic_message=diagnostic_message or error_trace,
                        compiler_stderr=compiler_stderr,
                        source_context=source_context,
                        pre_patch_hash=pre_hash
                    )

                    # Generate MCP escalation payload
                    mcp_payload = self.generate_mcp_escalation_payload(
                        skill_id=skill_id,
                        fingerprint=ast_fingerprint,
                        consecutive_count=rec.consecutive_fingerprint_count,
                        target_file=skill_path or skill_id,
                        diagnostic_code=diagnostic_code,
                        diagnostic_line=diagnostic_line,
                        diagnostic_message=diagnostic_message or error_trace,
                        source_context=source_context,
                        compiler_stderr=compiler_stderr
                    )

                    self.dispatch_webhook_alert("ast_consecutive_identical_error_trip", {
                        "skill_id": skill_id,
                        "circuit_state": CircuitState.PERM_TRIPPED.value,
                        "fingerprint": ast_fingerprint,
                        "consecutive_count": rec.consecutive_fingerprint_count,
                        "lancedb_snapshot": lancedb_snap.get("snapshot_file"),
                        "mcp_payload": mcp_payload
                    })

                    outcome["state"] = CircuitState.PERM_TRIPPED.value
                    outcome["action"] = "CONSECUTIVE_AST_ERROR_PERM_TRIPPED"
                    outcome["lancedb_snapshot"] = lancedb_snap
                    outcome["mcp_payload"] = mcp_payload
                    self._save_registry()
                    return outcome

        else:
            # Successful attempt resets consecutive identical fingerprint tracking
            rec.last_ast_fingerprint = None
            rec.consecutive_fingerprint_count = 0

        # 5. Hysteresis State Machine Transitions
        prev_state = rec.state

        if prev_state == CircuitState.CLOSED:
            if success:
                rec.consecutive_failures = 0
                rec.consecutive_successes += 1
                rec.total_successes += 1
                rec.state = CircuitState.CLOSED
                outcome["state"] = CircuitState.CLOSED.value
                outcome["action"] = "MUTATION_ACCEPTED"
            else:
                rec.consecutive_failures += 1
                rec.consecutive_successes = 0
                rec.total_failures += 1
                outcome["consecutive_failures"] = rec.consecutive_failures

                # Trip breaker if anomaly threshold is exceeded OR consecutive failures >= max_failures
                if is_anomaly or rec.consecutive_failures >= self.max_failures:
                    rec.state = CircuitState.OPEN
                    rec.last_state_change_timestamp = now
                    backoff = self.calculate_fibonacci_backoff(rec.consecutive_failures, with_jitter=True)
                    rec.open_until_timestamp = now + backoff

                    q_info = self.quarantine_skill(
                        skill_id=skill_id,
                        skill_path=skill_path,
                        reason=error_trace or f"Anomaly threshold tripped ({metrics['failure_rate_pct']}% failures)",
                        pre_hash=pre_hash,
                        post_hash=post_hash
                    )
                    outcome["state"] = CircuitState.OPEN.value
                    outcome["action"] = "BREAKER_TRIPPED_QUARANTINED"
                    outcome["quarantine_info"] = q_info
                    outcome["backoff_sec"] = backoff
                else:
                    # Dampening failure recorded while remaining in CLOSED
                    backoff = self.calculate_fibonacci_backoff(rec.consecutive_failures, with_jitter=True)
                    outcome["state"] = CircuitState.CLOSED.value
                    outcome["action"] = "FAILURE_DAMPENING"
                    outcome["recommended_backoff_sec"] = backoff

        elif prev_state == CircuitState.HALF_OPEN:
            if success:
                # Trial probe succeeded! Transition HALF_OPEN -> RECOVERY!
                rec.state = CircuitState.RECOVERY
                rec.consecutive_failures = 0
                rec.consecutive_successes = 1
                rec.total_successes += 1
                rec.last_state_change_timestamp = now
                outcome["state"] = CircuitState.RECOVERY.value
                outcome["action"] = "PROBE_SUCCEEDED_RECOVERY_ENGAGED"
                outcome["consecutive_successes"] = 1
                outcome["recovery_threshold"] = rec.recovery_threshold
            else:
                # Trial probe failed! Trip back to OPEN with updated Fibonacci backoff!
                rec.state = CircuitState.OPEN
                rec.consecutive_failures += 1
                rec.consecutive_successes = 0
                rec.total_failures += 1
                rec.last_state_change_timestamp = now
                backoff = self.calculate_fibonacci_backoff(rec.consecutive_failures, with_jitter=True)
                rec.open_until_timestamp = now + backoff
                outcome["state"] = CircuitState.OPEN.value
                outcome["action"] = "PROBE_FAILED_CIRCUIT_REOPENED"
                outcome["backoff_sec"] = backoff

        elif prev_state == CircuitState.RECOVERY:
            if success:
                rec.consecutive_successes += 1
                rec.total_successes += 1
                if rec.consecutive_successes >= rec.recovery_threshold:
                    # Probation complete! Transition RECOVERY -> CLOSED!
                    rec.state = CircuitState.CLOSED
                    rec.consecutive_failures = 0
                    rec.is_quarantined = False
                    rec.quarantine_reason = None
                    rec.quarantine_path = None
                    rec.open_until_timestamp = 0.0
                    rec.last_state_change_timestamp = now
                    detector.clear()
                    rec.window_events.clear()
                    outcome["state"] = CircuitState.CLOSED.value
                    outcome["action"] = "RECOVERY_COMPLETE_CIRCUIT_CLOSED"
                else:
                    outcome["state"] = CircuitState.RECOVERY.value
                    outcome["action"] = "RECOVERY_PROBATION_PROGRESS"
                    outcome["probation_progress"] = f"{rec.consecutive_successes}/{rec.recovery_threshold}"
            else:
                # Probation failed! Transition back to OPEN!
                rec.state = CircuitState.OPEN
                rec.consecutive_failures += 1
                rec.consecutive_successes = 0
                rec.total_failures += 1
                rec.last_state_change_timestamp = now
                backoff = self.calculate_fibonacci_backoff(rec.consecutive_failures, with_jitter=True)
                rec.open_until_timestamp = now + backoff
                outcome["state"] = CircuitState.OPEN.value
                outcome["action"] = "RECOVERY_FAILED_CIRCUIT_REOPENED"
                outcome["backoff_sec"] = backoff

        elif prev_state == CircuitState.OPEN:
            # Direct record while OPEN
            if success:
                rec.state = CircuitState.HALF_OPEN
                outcome["state"] = CircuitState.HALF_OPEN.value
                outcome["action"] = "PROBE_SUCCEEDED_HALF_OPEN"
            else:
                rec.consecutive_failures += 1
                rec.total_failures += 1
                backoff = self.calculate_fibonacci_backoff(rec.consecutive_failures, with_jitter=True)
                rec.open_until_timestamp = now + backoff
                outcome["state"] = CircuitState.OPEN.value
                outcome["action"] = "CIRCUIT_REMAINS_OPEN"
                outcome["backoff_sec"] = backoff

        self._save_registry()
        return outcome

    # --------------------------------------------------------------------------
    # Human-In-The-Loop (HITL) Administrative Recovery
    # --------------------------------------------------------------------------
    def reset_skill(
        self,
        skill_id: str,
        restore_path: Optional[Union[str, Path]] = None
    ) -> bool:
        """
        Administrative override allowing a Human-In-The-Loop operator
        to un-quarantine a skill, clear anomaly history, reset AST error tracking,
        and restore write permissions.
        """
        if skill_id not in self._registry:
            return False

        rec = self._registry[skill_id]
        rec.consecutive_failures = 0
        rec.consecutive_successes = 0
        rec.state = CircuitState.CLOSED
        rec.is_quarantined = False
        rec.quarantine_reason = None
        rec.quarantine_path = None
        rec.open_until_timestamp = 0.0
        rec.anomaly_score = 0.0
        rec.failure_rate = 0.0
        rec.window_events.clear()
        rec.last_ast_fingerprint = None
        rec.consecutive_fingerprint_count = 0
        rec.ast_fingerprint_history.clear()

        if skill_id in self._detectors:
            self._detectors[skill_id].clear()

        if restore_path:
            p = Path(restore_path)
            if p.exists():
                if p.is_file():
                    try:
                        os.chmod(p, 0o644)
                    except OSError:
                        pass
                elif p.is_dir():
                    for root, dirs, files in os.walk(p):
                        for f in files:
                            try:
                                os.chmod(os.path.join(root, f), 0o644)
                            except OSError:
                                pass
                        for d in dirs:
                            try:
                                os.chmod(os.path.join(root, d), 0o755)
                            except OSError:
                                pass

        self._save_registry()
        self.dispatch_webhook_alert("hitl_operator_reset", {
            "skill_id": skill_id,
            "status": "Circuit reset to CLOSED",
            "operator_override": True
        })
        return True

    # --------------------------------------------------------------------------
    # Context Manager for Guarded Mutations
    # --------------------------------------------------------------------------
    @contextmanager
    def guarded_mutation(
        self,
        skill_id: str,
        target_path: Union[str, Path],
        apply_backoff_on_failure: bool = True
    ):
        """
        Context manager wrapping an autopoietic mutation.
        Verifies permission, hashes pre-state, monitors execution,
        hashes post-state, and updates circuit breaker state automatically.
        """
        can_run, reason = self.can_mutate(skill_id)
        if not can_run:
            raise PermissionError(f"[CIRCUIT BREAKER LOCK] {reason}")

        pre_hash = self.compute_state_hash(target_path)
        start_t = time.perf_counter()
        mutation_ctx: Dict[str, Any] = {"pre_hash": pre_hash, "success": False}

        try:
            yield mutation_ctx
            mutation_ctx["success"] = True
            post_hash = self.compute_state_hash(target_path)
            duration_ms = (time.perf_counter() - start_t) * 1000.0
            self.record_mutation_attempt(
                skill_id=skill_id,
                pre_hash=pre_hash,
                post_hash=post_hash,
                success=True,
                skill_path=target_path,
                duration_ms=duration_ms
            )
        except Exception as exc:
            mutation_ctx["success"] = False
            post_hash = self.compute_state_hash(target_path)
            duration_ms = (time.perf_counter() - start_t) * 1000.0
            rec_res = self.record_mutation_attempt(
                skill_id=skill_id,
                pre_hash=pre_hash,
                post_hash=post_hash,
                success=False,
                error_trace=str(exc),
                skill_path=target_path,
                duration_ms=duration_ms
            )
            if apply_backoff_on_failure and rec_res.get("state") not in (CircuitState.OPEN.value, CircuitState.PERM_TRIPPED.value):
                self.apply_backoff(skill_id)
            raise

    # --------------------------------------------------------------------------
    # Persistence Helpers
    # --------------------------------------------------------------------------
    def _get_or_create_skill(self, skill_id: str) -> SkillGovernanceRecord:
        if skill_id not in self._registry:
            self._registry[skill_id] = SkillGovernanceRecord(
                skill_id=skill_id,
                recovery_threshold=self.recovery_threshold
            )
        return self._registry[skill_id]

    def _get_or_create_detector(self, skill_id: str) -> SlidingWindowAnomalyDetector:
        if skill_id not in self._detectors:
            rec = self._get_or_create_skill(skill_id)
            det = SlidingWindowAnomalyDetector(
                window_size=self.window_size,
                window_duration_sec=self.window_duration_sec,
                min_events=self.min_events,
                anomaly_threshold=self.anomaly_threshold
            )
            if rec.window_events:
                det.load_list(rec.window_events)
            self._detectors[skill_id] = det
        return self._detectors[skill_id]

    def _load_registry(self) -> None:
        if not self.registry_path.exists():
            return
        try:
            with open(self.registry_path, "r", encoding="utf-8") as f:
                data = json.load(f)
            for s_id, s_data in data.get("skills", {}).items():
                rec = SkillGovernanceRecord.from_dict(s_data)
                self._registry[s_id] = rec
                det = SlidingWindowAnomalyDetector(
                    window_size=self.window_size,
                    window_duration_sec=self.window_duration_sec,
                    min_events=self.min_events,
                    anomaly_threshold=self.anomaly_threshold
                )
                if rec.window_events:
                    det.load_list(rec.window_events)
                self._detectors[s_id] = det
        except Exception as e:
            sys.stderr.write(f"[WARN] Failed to parse circuit breaker registry {self.registry_path}: {e}\n")

    def _save_registry(self) -> None:
        payload = {
            "version": "2.0",
            "skills": {k: v.to_dict() for k, v in self._registry.items()},
            "history": [a.to_dict() for a in self._history[-50:]]
        }
        tmp_path = self.registry_path.with_suffix(".tmp")
        try:
            with open(tmp_path, "w", encoding="utf-8") as f:
                json.dump(payload, f, indent=2)
            tmp_path.replace(self.registry_path)
        except Exception as e:
            sys.stderr.write(f"[WARN] Failed to write circuit breaker registry {self.registry_path}: {e}\n")


# Global singleton helper
_GLOBAL_GOVERNOR: Optional[AutonomicCircuitBreakerGovernor] = None

def get_circuit_breaker_governor(**kwargs) -> AutonomicCircuitBreakerGovernor:
    global _GLOBAL_GOVERNOR
    if _GLOBAL_GOVERNOR is None:
        _GLOBAL_GOVERNOR = AutonomicCircuitBreakerGovernor(**kwargs)
    return _GLOBAL_GOVERNOR


# ==============================================================================
# Built-in Self-Test Runner
# ==============================================================================

def run_self_tests() -> bool:
    import tempfile
    print("=" * 75)
    print("  ch8_circuit_breaker_governor: Unit & Hysteresis Verification  ")
    print("=" * 75)

    with tempfile.TemporaryDirectory() as tmpdir:
        tmp_p = Path(tmpdir)
        gov = AutonomicCircuitBreakerGovernor(
            state_dir=tmp_p / "state",
            quarantine_dir=tmp_p / "quarantine",
            registry_path=tmp_p / "state" / "reg.json",
            webhook_sink=tmp_p / "events.jsonl",
            max_consecutive_failures=3,
            t0=1.0,
            t_max=34.0,
            window_size=10,
            window_duration_sec=30.0,
            min_events=5,
            anomaly_threshold=0.50,
            recovery_threshold=2
        )

        # 1. Test Cryptographic Hashing
        print("1. Testing Cryptographic State Hashing...")
        f1 = tmp_p / "mod.rs"
        f1.write_text("pub fn compute() -> i32 { 42 }")
        h1 = gov.compute_state_hash(f1)
        f1.write_text("pub fn compute() -> i32 { 99 }")
        h2 = gov.compute_state_hash(f1)
        assert h1 != h2, "Hash must change after modification"
        assert h1 == gov.compute_state_hash(b"pub fn compute() -> i32 { 42 }"), "Byte hashing must match file hashing"
        print("   ✔ Pre/Post state hash mutation verified.")

        # 2. Test Fibonacci Backoff Progression
        print("2. Testing Fibonacci Reinforcement Backoff Progression...")
        expected_fibs = [1, 1, 2, 3, 5, 8, 13, 21, 34]
        for idx, expected in enumerate(expected_fibs, start=1):
            f_val = gov.fibonacci(idx)
            assert f_val == expected, f"fib({idx}) expected {expected}, got {f_val}"
            delay = gov.calculate_fibonacci_backoff(idx, with_jitter=False)
            assert delay == float(expected), f"Delay for iter {idx} expected {expected}, got {delay}"

        # Jitter bounds check
        for _ in range(50):
            j_delay = gov.calculate_fibonacci_backoff(1, with_jitter=True)
            assert 1.10 <= j_delay <= 1.50, f"Jitter out of range: {j_delay}"
        print("   ✔ Fibonacci progression [1, 1, 2, 3, 5, 8, 13, 21, 34] verified.")

        # 3. Test AST Error State Fingerprinting
        print("3. Testing AST Error State Fingerprinting...")
        fp1 = gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 42, "10:20", "mismatched types: expected usize, found &str")
        fp2 = gov.compute_ast_error_fingerprint("E0308", "src/lib.rs", 42, "10:20", "mismatched   types:  expected usize,  found &str\n")
        assert fp1 == fp2, "Whitespace normalization must produce identical fingerprint"

        fp_diff_code = gov.compute_ast_error_fingerprint("E0596", "src/lib.rs", 42, "10:20", "cannot borrow as mutable")
        assert fp1 != fp_diff_code, "Different error code must produce distinct fingerprint"
        print("   ✔ Deterministic AST error fingerprinting verified.")

        # 4. Test 3-Consecutive Identical Error Circuit Breaker & LanceDB Snapshot + MCP Escalation
        print("4. Testing 3-Consecutive Identical Error Circuit Breaker...")
        skill_id = "test_healer_skill"
        skill_file = tmp_p / "broken.rs"
        skill_file.write_text("fn test() { let x: usize = \"abc\"; }")

        # Attempt 1
        r1 = gov.record_mutation_attempt(
            skill_id, h1, h2, success=False, error_trace="E0308 error",
            skill_path=skill_file, diagnostic_code="E0308", diagnostic_line=1, diagnostic_message="expected usize, found &str"
        )
        assert r1["state"] == CircuitState.CLOSED.value
        assert r1["consecutive_fingerprint_count"] == 1
        can_run, _ = gov.can_mutate(skill_id)
        assert can_run is True

        # Attempt 2
        r2 = gov.record_mutation_attempt(
            skill_id, h1, h2, success=False, error_trace="E0308 error",
            skill_path=skill_file, diagnostic_code="E0308", diagnostic_line=1, diagnostic_message="expected usize, found &str"
        )
        assert r2["state"] == CircuitState.CLOSED.value
        assert r2["consecutive_fingerprint_count"] == 2
        can_run, _ = gov.can_mutate(skill_id)
        assert can_run is True

        # Attempt 3: Tripped to PERM_TRIPPED!
        r3 = gov.record_mutation_attempt(
            skill_id, h1, h2, success=False, error_trace="E0308 error",
            skill_path=skill_file, diagnostic_code="E0308", diagnostic_line=1, diagnostic_message="expected usize, found &str"
        )
        assert r3["state"] == CircuitState.PERM_TRIPPED.value
        assert r3["action"] == "CONSECUTIVE_AST_ERROR_PERM_TRIPPED"
        assert r3["consecutive_fingerprint_count"] == 3

        # Write permissions must be revoked (0444)
        assert not os.access(skill_file, os.W_OK), "File write permissions must be revoked on PERM_TRIPPED!"

        # can_mutate must return False and NOT auto-expire
        can_run, msg = gov.can_mutate(skill_id, current_time=time.time() + 999999)
        assert can_run is False
        assert "PERM_TRIPPED" in msg

        # LanceDB Snapshot Verification
        assert "lancedb_snapshot" in r3
        snap_path = r3["lancedb_snapshot"]["snapshot_file"]
        assert os.path.exists(snap_path), f"Snapshot file not created at {snap_path}"
        with open(snap_path, "r", encoding="utf-8") as f:
            snap_data = json.load(f)
        assert snap_data["consecutive_occurrences"] == 3
        assert snap_data["diagnostic_code"] == "E0308"
        assert snap_data["circuit_state"] == "PERM_TRIPPED"
        assert snap_data["escalated_to_mcp"] is True

        # MCP Escalation Payload Verification
        assert "mcp_payload" in r3
        mcp_data = r3["mcp_payload"]
        assert mcp_data["method"] == "tools/call"
        assert mcp_data["params"]["name"] == "hgb_compiler_self_heal_ast"
        assert mcp_data["params"]["arguments"]["diagnostic_code"] == "E0308"
        print("   ✔ 3-consecutive identical AST error breaker, LanceDB snapshot, and MCP payload verified.")

        # 5. HITL Reset restores to CLOSED
        gov.reset_skill(skill_id, restore_path=skill_file)
        can_run, _ = gov.can_mutate(skill_id)
        assert can_run is True
        assert os.access(skill_file, os.W_OK)
        print("   ✔ HITL reset restores state to CLOSED.")

    print("=" * 75)
    print("  ✔ ALL AUTONOMIC CIRCUIT BREAKER & HYSTERESIS TESTS PASSED!  ")
    print("=" * 75)
    return True


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] in ("--test", "test"):
        success = run_self_tests()
        sys.exit(0 if success else 1)
    else:
        print("ch8_circuit_breaker_governor loaded. Use --test to execute verification suite.")
