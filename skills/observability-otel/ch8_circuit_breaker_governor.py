#!/usr/bin/env python3
"""
ch8_circuit_breaker_governor.py - Autopoietic Circuit-Breaker Governance Engine with Hysteresis & Cryptographic Lockout
========================================================================================================================
Project Hagibis: Pillar 3 (Observability/OTel) & Pillar 4 (Autonomic Self-Healing)

Pillar C Enhancement & Cryptographic State Signing in the Self-Healing Loop:
  1. Sliding-Window Anomaly Detection:
     Tracks rolling execution events (default 20 events, 60s sliding window).
     Computes dynamic anomaly score and failure rate percentage. Trips when failure rate
     exceeds anomaly threshold (>= 50% failures over >= 5 events) or consecutive failure threshold.
  2. Full Hysteresis State Machine:
     - CLOSED: Normal execution, mutations permitted.
     - OPEN: Blocked, mutations forbidden. Fibonacci backoff delay with randomized jitter enforced.
     - HALF_OPEN: Trial probing state allowing controlled test invocations once backoff expires.
     - RECOVERY: Probation state requiring N consecutive successful executions before transitioning back to CLOSED.
     - PERM_TRIPPED: Permanently tripped due to repeated identical AST failures.
     - PERM_TRIPPED_CRYPTOLOCKED: Permanently tripped and cryptographically locked due to flapping compilation
       oscillations (e.g. alternating between 2 errors) or 3 consecutive identical failures.
       All target file write permissions stripped (0444 read-only). Cryptographically signed lockout receipt
       issued using Ed25519 + BLAKE3. Enforces permanent lockout until an authorized human operator provides a
       valid signed MCP unlock payload (hgb_operator_unlock_circuit_breaker) via hagibis.mcp.json.
  3. Fibonacci Reinforcement Backoff Dampening:
     delay = min(max_backoff, base_delay * fib(iteration)) + uniform_jitter(0.1, 0.5)
     where fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13, fib(8)=21...
  4. Flapping Detection Engine:
     Detects error oscillation (e.g., flipping between 2 distinct error fingerprints [A, B, A, B] or [A, B, A])
     or 3 consecutive identical failures, triggering immediate escalation to PERM_TRIPPED_CRYPTOLOCKED.
  5. Cryptographic State Signing & Proof of Lockout (Ed25519 + BLAKE3):
     Computes BLAKE3 hash of error fingerprint & sliding window state history, and signs the lockout receipt
     using an Ed25519 signing key.
  6. Authorized Human Operator Signed MCP Unlock (hgb_operator_unlock_circuit_breaker):
     Validates cryptographic Ed25519 signature over single-use replay-defended nonce before unlocking and
     restoring write permissions.
  7. LanceDB Snapshots & MCP Escalation:
     Snapshots failing AST state (source context, AST diagnostic, stderr, hashes) into LanceDB
     and synthesizes MCP LLM escalation payload for hgb_compiler_self_heal_ast.
"""

from __future__ import annotations

import os
import sys
import time
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
from typing import Dict, List, Optional, Tuple, Any, Union, Set
from contextlib import contextmanager

# Try importing cryptography for Ed25519
try:
    from cryptography.hazmat.primitives.asymmetric import ed25519
    from cryptography.hazmat.primitives import serialization
    from cryptography.exceptions import InvalidSignature
    _HAS_CRYPTOGRAPHY = True
except ImportError:
    _HAS_CRYPTOGRAPHY = False

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
DEFAULT_T0 = float(os.environ.get("HGB_CB_T0", 1.0))
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
    CLOSED = "CLOSED"                                  # Normal execution, mutations permitted
    OPEN = "OPEN"                                      # Blocked, mutations forbidden, backoff active
    HALF_OPEN = "HALF_OPEN"                            # Trial probing state allowing controlled test invocations
    RECOVERY = "RECOVERY"                              # Probation state requiring N consecutive successful executions
    PERM_TRIPPED = "PERM_TRIPPED"                      # Permanently tripped due to repeated identical AST failures
    PERM_TRIPPED_CRYPTOLOCKED = "PERM_TRIPPED_CRYPTOLOCKED"  # Cryptographically signed lockout due to flapping

    @classmethod
    def from_str(cls, val: str) -> CircuitState:
        normalized = val.strip().upper().replace(" ", "_")
        if normalized in ("CLOSED", "HEALTHY"):
            return cls.CLOSED
        elif normalized in ("PERM_TRIPPED_CRYPTOLOCKED", "CRYPTOLOCKED", "PERM_CRYPTOLOCKED"):
            return cls.PERM_TRIPPED_CRYPTOLOCKED
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
    error_fingerprint: str = ""
    duration_ms: float = 0.0

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> WindowEvent:
        return cls(
            timestamp=float(data.get("timestamp", time.time())),
            success=bool(data.get("success", False)),
            error_trace=str(data.get("error_trace", "")),
            error_fingerprint=str(data.get("error_fingerprint", "")),
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
        error_fingerprint: str = "",
        duration_ms: float = 0.0,
        timestamp: Optional[float] = None
    ) -> WindowEvent:
        now = timestamp if timestamp is not None else time.time()
        event = WindowEvent(
            timestamp=now,
            success=success,
            error_trace=error_trace,
            error_fingerprint=error_fingerprint,
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


# ==============================================================================
# Flapping Detection Engine
# ==============================================================================

class FlappingDetector:
    """
    Detects flapping and oscillating failure loops in compilation/mutation attempts.
    Triggers when:
      1. Errors oscillate between 2 distinct error fingerprints (e.g. [A, B, A, B] or [A, B, A]).
      2. Consecutive identical AST error count >= 3 ([A, A, A]).
      3. Frequent alternating failures exceed stability threshold within sliding window.
    """

    def __init__(self, oscillation_window: int = 4, consecutive_identical_threshold: int = 3):
        self.oscillation_window = oscillation_window
        self.consecutive_identical_threshold = consecutive_identical_threshold
        self.history: List[str] = []

    def record_and_check(self, fingerprint: str) -> Tuple[bool, str, List[str]]:
        self.history.append(fingerprint)
        return self.detect_flapping(self.history, self.consecutive_identical_threshold)

    def record_error(self, fingerprint: str) -> Tuple[bool, str, List[str]]:
        return self.record_and_check(fingerprint)

    def check(self, history: List[str]) -> Tuple[bool, str, List[str]]:
        return self.detect_flapping(history, self.consecutive_identical_threshold)

    @staticmethod
    def detect_flapping(
        history_fingerprints: List[str],
        consecutive_identical_threshold: int = 3
    ) -> Tuple[bool, str, List[str]]:
        """
        Analyzes sequence of recent error fingerprints.
        Returns: (is_flapping, reason, alternating_signatures)
        """
        valid_fps = [fp for fp in history_fingerprints if fp]
        if len(valid_fps) < 2:
            return False, "Insufficient error history", []

        # Check 1: 3 Consecutive identical errors
        if len(valid_fps) >= consecutive_identical_threshold:
            tail = valid_fps[-consecutive_identical_threshold:]
            if len(set(tail)) == 1:
                return (
                    True,
                    f"Flapping detected: 3 consecutive identical AST compiler errors ({tail[0][:12]})",
                    list(tail)
                )

        # Check 2: Oscillating between 2 distinct error fingerprints
        # e.g. [A, B, A] (3 items) or [A, B, A, B] (4 items)
        if len(valid_fps) >= 3:
            tail3 = valid_fps[-3:]
            if tail3[0] == tail3[2] and tail3[0] != tail3[1]:
                return (
                    True,
                    f"Flapping detected (2-cycle oscillation between error fingerprints: {tail3[0][:8]} <-> {tail3[1][:8]})",
                    list(tail3)
                )

        if len(valid_fps) >= 4:
            tail4 = valid_fps[-4:]
            if tail4[0] == tail4[2] and tail4[1] == tail4[3] and tail4[0] != tail4[1]:
                return (
                    True,
                    f"Strict 2-cycle flapping oscillation detected: {tail4[0][:8]} <-> {tail4[1][:8]}",
                    [tail4[0], tail4[1]]
                )

        # Check 3: High variance oscillation in window (flips back and forth)
        if len(valid_fps) >= 5:
            distinct_in_window = set(valid_fps[-5:])
            if len(distinct_in_window) == 2:
                # Count transitions
                transitions = sum(1 for i in range(len(valid_fps) - 4, len(valid_fps)) if valid_fps[i] != valid_fps[i - 1])
                if transitions >= 3:
                    sigs = list(distinct_in_window)
                    return (
                        True,
                        f"High-frequency error oscillation ({transitions} transitions over 5 attempts) between {sigs[0][:8]} and {sigs[1][:8]}",
                        sigs
                    )

        return False, "No flapping detected", []


@dataclass
class MutationAttempt:
    timestamp: float
    iso_timestamp: str
    skill_id: str
    success: bool
    pre_mutation_hash: str
    post_mutation_hash: str
    error_trace: str = ""
    error_fingerprint: str = ""
    duration_ms: float = 0.0

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)


@dataclass
class CryptographicLockoutReceipt:
    lockout_id: str
    skill_id: str
    circuit_state: str
    reason: str
    flapping_signatures: List[str]
    consecutive_count: int
    blake3_fingerprint: str
    state_history_hash: str
    ed25519_public_key: str
    ed25519_signature: str
    locked_paths: List[str]
    timestamp_iso: str
    timestamp_unix: float
    mcp_unlock_required: bool = True
    mcp_tool_name: str = "hgb_operator_unlock_circuit_breaker"

    def to_dict(self) -> Dict[str, Any]:
        d = asdict(self)
        d["ed25519_signed"] = bool(self.ed25519_signature)
        d["blake3_error_history_hash"] = self.state_history_hash
        return d

    def __getitem__(self, item: str) -> Any:
        return self.to_dict()[item]

    def __contains__(self, item: str) -> bool:
        return item in self.to_dict()


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
    lockout_id: Optional[str] = None
    lockout_receipt: Optional[Dict[str, Any]] = None

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
            ast_fingerprint_history=data.get("ast_fingerprint_history", []),
            lockout_id=data.get("lockout_id"),
            lockout_receipt=data.get("lockout_receipt")
        )


# ==============================================================================
# Autonomic Circuit Breaker Governor Implementation
# ==============================================================================

class AutonomicCircuitBreakerGovernor:
    """
    Enterprise Governor overseeing self-healing autopoietic mutation loops.
    Guarantees state immutability checking, Fibonacci reinforcement backoff,
    deterministic AST error fingerprinting, sliding-window anomaly detection,
    flapping detection, and cryptographic Ed25519 lockout with signed MCP unlock.
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
        self.lockouts_dir = self.state_dir / "lockouts"
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

        self._consumed_nonces: Set[int] = set()
        self._ensure_directories()
        self._init_governor_keypair()

        self._registry: Dict[str, SkillGovernanceRecord] = {}
        self._detectors: Dict[str, SlidingWindowAnomalyDetector] = {}
        self._history: List[MutationAttempt] = []
        self._load_registry()

    def _ensure_directories(self) -> None:
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.quarantine_dir.mkdir(parents=True, exist_ok=True)
        self.lockouts_dir.mkdir(parents=True, exist_ok=True)
        if self.webhook_sink.parent:
            self.webhook_sink.parent.mkdir(parents=True, exist_ok=True)

    def _init_governor_keypair(self) -> None:
        """Initializes or loads persistent Ed25519 signing keypair for the governor."""
        key_file = self.state_dir / "governor_ed25519.key"
        self._signing_key: Optional[Any] = None
        self._public_key: Optional[Any] = None

        if _HAS_CRYPTOGRAPHY:
            if key_file.exists():
                try:
                    with open(key_file, "rb") as kf:
                        raw_priv = kf.read(32)
                    self._signing_key = ed25519.Ed25519PrivateKey.from_private_bytes(raw_priv)
                    self._public_key = self._signing_key.public_key()
                except Exception:
                    self._generate_and_save_key(key_file)
            else:
                self._generate_and_save_key(key_file)

    def _generate_and_save_key(self, key_file: Path) -> None:
        if _HAS_CRYPTOGRAPHY:
            priv = ed25519.Ed25519PrivateKey.generate()
            self._signing_key = priv
            self._public_key = priv.public_key()
            try:
                raw_bytes = priv.private_bytes_raw()
                with open(key_file, "wb") as f:
                    f.write(raw_bytes)
                os.chmod(key_file, 0o600)
            except Exception:
                pass

    @property
    def governor_public_key_hex(self) -> str:
        if self._public_key and _HAS_CRYPTOGRAPHY:
            return self._public_key.public_bytes_raw().hex()
        return "0" * 64

    # --------------------------------------------------------------------------
    # Cryptographic State Hashing
    # --------------------------------------------------------------------------
    @staticmethod
    def compute_state_hash(target: Union[str, Path, bytes]) -> str:
        """
        Computes cryptographic BLAKE3 (or SHA256 fallback) hash of target.
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
        """Calculates n-th Fibonacci number."""
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
        """
        iter_clamped = max(1, iteration)
        fib_val = self.fibonacci(iter_clamped)
        base_backoff = min(self.t_max, self.t0 * float(fib_val))

        jitter = 0.0
        if with_jitter:
            rng = random.Random(jitter_seed) if jitter_seed is not None else random
            jitter = rng.uniform(jitter_min, jitter_max)

        return round(base_backoff + jitter, 4)

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
        """
        norm_file = os.path.normpath(str(file_path)).strip()
        norm_code = str(error_code).strip()
        norm_line = str(line).strip()
        norm_span = str(span).strip()
        raw_msg = str(error_message).replace("\\n", " ").replace("\\r", " ").replace("\\t", " ")
        norm_msg = " ".join(raw_msg.split()).strip()

        canonical_repr = f"{norm_code}|{norm_file}|{norm_line}|{norm_span}|{norm_msg}"
        data_bytes = canonical_repr.encode("utf-8")

        if use_blake3 and _HAS_NATIVE_BLAKE3:
            return _native_blake3.blake3(data_bytes).hexdigest()
        return hashlib.sha256(data_bytes).hexdigest()

    # --------------------------------------------------------------------------
    # Cryptographic State Signing & Lockout Receipt Issuance
    # --------------------------------------------------------------------------
    def issue_cryptographic_lockout_receipt(
        self,
        skill_id: str,
        reason: str,
        flapping_signatures: List[str],
        consecutive_count: int,
        error_fingerprint: str,
        locked_paths: List[Union[str, Path]],
        window_events: List[Dict[str, Any]]
    ) -> CryptographicLockoutReceipt:
        """
        Issues an immutable Ed25519-signed lockout receipt with BLAKE3 state history hashing.
        """
        now_unix = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_unix))
        lockout_id = f"lockout_{int(now_unix)}_{error_fingerprint[:8]}"

        # Compute BLAKE3 state history hash chain
        history_bytes = json.dumps(window_events, sort_keys=True).encode("utf-8")
        if _HAS_NATIVE_BLAKE3:
            state_history_hash = f"blake3:{_native_blake3.blake3(history_bytes).hexdigest()}"
            fp_hash = f"blake3:{_native_blake3.blake3(error_fingerprint.encode('utf-8')).hexdigest()}"
        else:
            state_history_hash = f"sha256:{hashlib.sha256(history_bytes).hexdigest()}"
            fp_hash = f"sha256:{hashlib.sha256(error_fingerprint.encode('utf-8')).hexdigest()}"

        locked_strs = [str(p) for p in locked_paths if p]

        # Construct signable bytes
        signable_payload = (
            f"{lockout_id}:{skill_id}:PERM_TRIPPED_CRYPTOLOCKED:{fp_hash}:{state_history_hash}:{int(now_unix)}"
        ).encode("utf-8")

        # Cryptographically sign with governor Ed25519 key
        sig_hex = ""
        pub_hex = self.governor_public_key_hex
        if self._signing_key and _HAS_CRYPTOGRAPHY:
            sig_bytes = self._signing_key.sign(signable_payload)
            sig_hex = sig_bytes.hex()
        else:
            # Fallback deterministic HMAC signature if pure Python without cryptography
            h = hashlib.sha256(signable_payload).hexdigest()
            sig_hex = f"sha256_mock_sig_{h}"

        receipt = CryptographicLockoutReceipt(
            lockout_id=lockout_id,
            skill_id=skill_id,
            circuit_state=CircuitState.PERM_TRIPPED_CRYPTOLOCKED.value,
            reason=reason,
            flapping_signatures=flapping_signatures,
            consecutive_count=consecutive_count,
            blake3_fingerprint=fp_hash,
            state_history_hash=state_history_hash,
            ed25519_public_key=pub_hex,
            ed25519_signature=sig_hex,
            locked_paths=locked_strs,
            timestamp_iso=iso_now,
            timestamp_unix=now_unix,
            mcp_unlock_required=True,
            mcp_tool_name="hgb_operator_unlock_circuit_breaker"
        )

        # Strip write permissions on all locked paths (0444)
        for p in locked_paths:
            try:
                path_obj = Path(p)
                if path_obj.exists():
                    os.chmod(path_obj, 0o444)
            except OSError:
                pass

        # Write lockout receipt to state directory and seal (0444)
        receipt_file = self.lockouts_dir / f"{lockout_id}.json"
        try:
            with open(receipt_file, "w", encoding="utf-8") as f:
                json.dump(receipt.to_dict(), f, indent=2)
            os.chmod(receipt_file, 0o444)
        except OSError:
            pass

        # Also mirror into LanceDB snapshots directory
        base_lancedb = self._resolve_lancedb_base_dir()
        try:
            lance_snap = base_lancedb / "failing_ast_snapshots" / f"{lockout_id}.json"
            lance_snap.parent.mkdir(parents=True, exist_ok=True)
            with open(lance_snap, "w", encoding="utf-8") as f:
                json.dump(receipt.to_dict(), f, indent=2)
            os.chmod(lance_snap, 0o444)
        except OSError:
            pass

        # Update in-memory registry record
        rec = self._get_or_create_record(skill_id)
        rec.state = CircuitState.PERM_TRIPPED_CRYPTOLOCKED
        rec.lockout_id = lockout_id
        rec.lockout_receipt = receipt.to_dict()
        rec.is_quarantined = True
        rec.quarantine_reason = reason
        self._persist_registry()

        return receipt

    # --------------------------------------------------------------------------
    # Authorized Human Operator MCP Signed Unlock
    # --------------------------------------------------------------------------
    def operator_unlock_circuit_breaker(
        self,
        skill_id: str,
        operator_id: str,
        lockout_id: str,
        ed25519_signature: str,
        operator_public_key: str,
        nonce: int,
        restore_permissions: bool = True
    ) -> Dict[str, Any]:
        """
        Executes an authorized human operator unlock using Ed25519 signature verification.
        Resets circuit breaker from PERM_TRIPPED_CRYPTOLOCKED to CLOSED and restores
        file permissions to 0644.
        """
        rec = self._get_or_create_skill(skill_id)

        # 1. Verification: Nonce Replay Defense
        if nonce in self._consumed_nonces:
            raise PermissionError(f"Replay attack detected: nonce {nonce} has already been consumed")

        # 2. Verification: State must be PERM_TRIPPED_CRYPTOLOCKED or PERM_TRIPPED
        if rec.state not in (CircuitState.PERM_TRIPPED_CRYPTOLOCKED, CircuitState.PERM_TRIPPED):
            return {
                "success": True,
                "skill_id": skill_id,
                "state": rec.state.value,
                "message": f"Skill [{skill_id}] is already in state {rec.state.value}. No unlock necessary."
            }

        # 3. Verification: Lockout ID must match active lockout
        if rec.lockout_id and rec.lockout_id != lockout_id:
            raise PermissionError(
                f"Lockout ID mismatch: active lockout for [{skill_id}] is '{rec.lockout_id}', "
                f"provided token is for '{lockout_id}'"
            )

        # 4. Verification: Ed25519 Signature Verification
        expected_payload = f"UNLOCK:{operator_id}:{skill_id}:{lockout_id}:{nonce}".encode("utf-8")

        if _HAS_CRYPTOGRAPHY:
            try:
                pub_bytes = bytes.fromhex(operator_public_key)
                sig_bytes = bytes.fromhex(ed25519_signature)
                verifier = ed25519.Ed25519PublicKey.from_public_bytes(pub_bytes)
                verifier.verify(sig_bytes, expected_payload)
            except Exception as e:
                raise PermissionError(f"Ed25519 signature verification failed for operator unlock: {e}")
        else:
            # Fallback signature check if cryptography not present
            if not ed25519_signature or len(ed25519_signature) < 32:
                raise PermissionError("Invalid cryptographic unlock signature")

        # Record consumed nonce only after successful signature verification
        self._consumed_nonces.insert(nonce) if hasattr(self._consumed_nonces, 'insert') else self._consumed_nonces.add(nonce)

        # 5. Restore State & Reset Flapping History
        now_unix = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_unix))

        rec.state = CircuitState.CLOSED
        rec.consecutive_failures = 0
        rec.consecutive_successes = 0
        rec.is_quarantined = False
        rec.quarantine_reason = None
        rec.quarantine_path = None
        rec.open_until_timestamp = 0.0
        rec.last_state_change_timestamp = now_unix
        rec.anomaly_score = 0.0
        rec.failure_rate = 0.0
        rec.last_ast_fingerprint = None
        rec.consecutive_fingerprint_count = 0
        rec.ast_fingerprint_history.clear()
        rec.window_events.clear()

        unlocked_paths = []
        if rec.lockout_receipt and "locked_paths" in rec.lockout_receipt:
            for p_str in rec.lockout_receipt["locked_paths"]:
                p = Path(p_str)
                if p.exists() and restore_permissions:
                    try:
                        if p.is_file():
                            os.chmod(p, 0o644)
                        elif p.is_dir():
                            os.chmod(p, 0o755)
                        unlocked_paths.append(str(p))
                    except OSError:
                        pass

        rec.lockout_id = None
        rec.lockout_receipt = None
        if skill_id in self._detectors:
            self._detectors[skill_id].clear()

        self._save_registry()

        unlock_receipt = {
            "version": "2.0",
            "action": "OPERATOR_CRYPTOGRAPHIC_UNLOCK_SUCCESS",
            "skill_id": skill_id,
            "operator_id": operator_id,
            "lockout_id": lockout_id,
            "new_state": CircuitState.CLOSED.value,
            "unlocked_at": iso_now,
            "nonce": nonce,
            "permissions_restored": restore_permissions,
            "restored_paths": unlocked_paths,
            "message": f"Circuit breaker for [{skill_id}] cryptographically unlocked by operator [{operator_id}]. State restored to CLOSED."
        }

        self.dispatch_webhook_alert("hitl_operator_unlock", unlock_receipt)
        return unlock_receipt

    # --------------------------------------------------------------------------
    # Circuit Breaker Status & Hysteresis Transitions
    # --------------------------------------------------------------------------
    def can_mutate(self, skill_id: str, current_time: Optional[float] = None) -> Tuple[bool, str]:
        """
        Determines whether a skill is authorized to mutate.
        Guarantees that PERM_TRIPPED_CRYPTOLOCKED is a permanent, non-expiring lockout
        requiring signed operator unlock.
        """
        now = current_time if current_time is not None else time.time()
        rec = self._get_or_create_skill(skill_id)

        # 1. Permanent Cryptographic Lockout Check
        if rec.state == CircuitState.PERM_TRIPPED_CRYPTOLOCKED:
            return (
                False,
                f"BLOCKED: PERM_TRIPPED_CRYPTOLOCKED. Permanent cryptographic lockout active for [{skill_id}] "
                f"(Lockout ID: {rec.lockout_id}). Flapping compilation oscillation was detected. "
                f"Mutating permissions permanently locked (0444). Signed human operator MCP unlock "
                f"(hgb_operator_unlock_circuit_breaker via hagibis.mcp.json) required."
            )

        if rec.state == CircuitState.PERM_TRIPPED:
            return (
                False,
                f"BLOCKED: PERM_TRIPPED. 3 consecutive identical AST errors for [{skill_id}] "
                f"({rec.last_ast_fingerprint}). Auto-mutation halted."
            )

        if rec.state == CircuitState.OPEN or rec.is_quarantined:
            # Check if exponential backoff delay has expired
            if rec.open_until_timestamp > 0 and now >= rec.open_until_timestamp:
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
                    f"Skill [{skill_id}] is OPEN/QUARANTINED ({rec.consecutive_failures} failures). "
                    f"Mutating permissions locked. Reason: {rec.quarantine_reason}. Backoff remaining: {remaining:.2f}s."
                )

        elif rec.state == CircuitState.HALF_OPEN:
            return True, f"Skill [{skill_id}] is in HALF_OPEN trial probing state."

        elif rec.state == CircuitState.RECOVERY:
            return (
                True,
                f"Skill [{skill_id}] is in RECOVERY probation ({rec.consecutive_successes}/{rec.recovery_threshold} successes)."
            )

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
        sliding-window anomaly detection, flapping detection, and cryptographic lockout.
        """
        now = current_time if current_time is not None else time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now))
        rec = self._get_or_create_skill(skill_id)
        detector = self._get_or_create_detector(skill_id)

        # 1. Compute AST Error Fingerprint if error occurred
        if not success:
            if not ast_fingerprint and (diagnostic_code or error_trace):
                ast_fingerprint = self.compute_ast_error_fingerprint(
                    error_code=diagnostic_code or "E_UNKNOWN",
                    file_path=skill_path or skill_id,
                    line=diagnostic_line,
                    span=diagnostic_span,
                    error_message=diagnostic_message or error_trace
                )

        # 2. Record event in sliding window
        detector.record_event(
            success=success,
            error_trace=error_trace,
            error_fingerprint=ast_fingerprint or "",
            duration_ms=duration_ms,
            timestamp=now
        )
        rec.window_events = detector.to_list()

        # 3. Compute dynamic anomaly score and failure rate
        metrics = detector.compute_metrics(current_time=now)
        rec.anomaly_score = metrics["anomaly_score"]
        rec.failure_rate = metrics["failure_rate"]
        is_anomaly = metrics["is_anomaly"]

        # 4. Record mutation history
        attempt = MutationAttempt(
            timestamp=now,
            iso_timestamp=iso_now,
            skill_id=skill_id,
            success=success,
            pre_mutation_hash=pre_hash,
            post_mutation_hash=post_hash,
            error_trace=error_trace,
            error_fingerprint=ast_fingerprint or "",
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

        # 5. Flapping Detection & Consecutive Error Evaluation
        if not success and ast_fingerprint:
            rec.ast_fingerprint_history.append(ast_fingerprint)
            if len(rec.ast_fingerprint_history) > 50:
                rec.ast_fingerprint_history = rec.ast_fingerprint_history[-50:]

            # Consecutive identical count tracking
            if rec.last_ast_fingerprint == ast_fingerprint:
                rec.consecutive_fingerprint_count += 1
            else:
                rec.last_ast_fingerprint = ast_fingerprint
                rec.consecutive_fingerprint_count = 1

            outcome["ast_fingerprint"] = ast_fingerprint
            outcome["consecutive_fingerprint_count"] = rec.consecutive_fingerprint_count

            # Run Flapping Detector
            is_flapping, flapping_reason, alternating_sigs = FlappingDetector.detect_flapping(
                rec.ast_fingerprint_history,
                consecutive_identical_threshold=3
            )

            if is_flapping:
                # Transition directly to PERM_TRIPPED_CRYPTOLOCKED!
                rec.state = CircuitState.PERM_TRIPPED_CRYPTOLOCKED
                rec.last_state_change_timestamp = now
                rec.is_quarantined = True
                rec.quarantine_reason = flapping_reason
                rec.open_until_timestamp = float("inf")

                locked_targets = []
                if skill_path:
                    locked_targets.append(skill_path)
                    # Strip write permissions (0444)
                    try:
                        os.chmod(skill_path, 0o444)
                    except OSError:
                        pass

                # Issue Cryptographic Ed25519 + BLAKE3 Lockout Receipt
                lockout_receipt = self.issue_cryptographic_lockout_receipt(
                    skill_id=skill_id,
                    reason=flapping_reason,
                    flapping_signatures=alternating_sigs,
                    consecutive_count=rec.consecutive_fingerprint_count,
                    error_fingerprint=ast_fingerprint,
                    locked_paths=locked_targets,
                    window_events=rec.window_events
                )
                rec.lockout_id = lockout_receipt.lockout_id
                rec.lockout_receipt = lockout_receipt.to_dict()

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

                # Generate MCP operator unlock escalation payload
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

                self.dispatch_webhook_alert("circuit_breaker_cryptolocked", {
                    "skill_id": skill_id,
                    "circuit_state": CircuitState.PERM_TRIPPED_CRYPTOLOCKED.value,
                    "lockout_id": lockout_receipt.lockout_id,
                    "reason": flapping_reason,
                    "lockout_receipt": lockout_receipt.to_dict(),
                    "lancedb_snapshot": lancedb_snap.get("snapshot_file"),
                    "action_required": "Authorized operator cryptographic unlock required (hgb_operator_unlock_circuit_breaker)."
                })

                outcome["state"] = CircuitState.PERM_TRIPPED_CRYPTOLOCKED.value
                outcome["action"] = "FLAPPING_DETECTED_PERM_TRIPPED_CRYPTOLOCKED"
                outcome["lockout_receipt"] = lockout_receipt.to_dict()
                outcome["lancedb_snapshot"] = lancedb_snap
                outcome["mcp_payload"] = mcp_payload
                self._save_registry()
                return outcome

        elif success:
            rec.last_ast_fingerprint = None
            rec.consecutive_fingerprint_count = 0

        # 6. Hysteresis State Machine Transitions
        prev_state = rec.state

        if prev_state == CircuitState.CLOSED:
            if success:
                rec.consecutive_failures = 0
                rec.consecutive_successes += 1
                rec.total_successes += 1
                outcome["state"] = CircuitState.CLOSED.value
                outcome["action"] = "MUTATION_ACCEPTED"
            else:
                rec.consecutive_failures += 1
                rec.consecutive_successes = 0
                rec.total_failures += 1
                outcome["consecutive_failures"] = rec.consecutive_failures

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
                    backoff = self.calculate_fibonacci_backoff(rec.consecutive_failures, with_jitter=True)
                    outcome["state"] = CircuitState.CLOSED.value
                    outcome["action"] = "FAILURE_DAMPENING"
                    outcome["recommended_backoff_sec"] = backoff

        elif prev_state == CircuitState.HALF_OPEN:
            if success:
                rec.state = CircuitState.RECOVERY
                rec.consecutive_failures = 0
                rec.consecutive_successes = 1
                rec.total_successes += 1
                rec.last_state_change_timestamp = now
                outcome["state"] = CircuitState.RECOVERY.value
                outcome["action"] = "PROBE_SUCCEEDED_RECOVERY_ENGAGED"
            else:
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

        self._save_registry()
        return outcome

    # --------------------------------------------------------------------------
    # LanceDB Snapshotting & MCP Escalation Payload Synthesis
    # --------------------------------------------------------------------------
    def _resolve_lancedb_base_dir(self) -> Path:
        if self.lancedb_dir and Path(self.lancedb_dir).exists():
            return Path(self.lancedb_dir)
        curr = Path(__file__).resolve()
        for parent in [curr.parent, curr.parent.parent, curr.parent.parent.parent]:
            candidate = parent / "skills" / "vector-lancedb"
            if candidate.exists():
                return candidate
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
        base_lancedb = self._resolve_lancedb_base_dir()
        snapshots_dir = base_lancedb / "failing_ast_snapshots"
        try:
            snapshots_dir.mkdir(parents=True, exist_ok=True)
        except OSError:
            snapshots_dir = self.state_dir / "failing_ast_snapshots"
            snapshots_dir.mkdir(parents=True, exist_ok=True)

        now_sec = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_sec))

        snapshot_record = {
            "version": "2.0",
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
            "circuit_state": CircuitState.PERM_TRIPPED_CRYPTOLOCKED.value,
            "auto_mutation_halted": True,
            "escalated_to_mcp": True,
            "mcp_target_tool": "hgb_operator_unlock_circuit_breaker"
        }

        filename = f"snapshot_{int(now_sec)}_{fingerprint[:12]}.json"
        snapshot_file = snapshots_dir / filename
        with open(snapshot_file, "w", encoding="utf-8") as f:
            json.dump(snapshot_record, f, indent=2)

        return {
            "snapshot_file": str(snapshot_file),
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
        now_sec = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_sec))
        call_id = f"mcp_call_{int(now_sec)}_{fingerprint[:8]}"

        mcp_payload = {
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "tools/call",
            "params": {
                "name": "hgb_operator_unlock_circuit_breaker",
                "arguments": {
                    "skill_id": skill_id,
                    "operator_id": "operator-admin-01",
                    "lockout_id": f"lockout_{int(now_sec)}_{fingerprint[:8]}",
                    "restore_permissions": True
                }
            },
            "metadata": {
                "escalation_reason": "Flapping compilation loop detected. Auto-mutation cryptographically locked out (PERM_TRIPPED_CRYPTOLOCKED).",
                "ast_fingerprint": fingerprint,
                "consecutive_identical_occurrences": consecutive_count,
                "timestamp": iso_now,
                "skill_id": skill_id,
                "governor_public_key": self.governor_public_key_hex
            }
        }

        mcp_out = self.state_dir / "mcp_escalation_payload.json"
        try:
            with open(mcp_out, "w", encoding="utf-8") as f:
                json.dump(mcp_payload, f, indent=2)
        except Exception:
            pass

        return mcp_payload

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
                    try:
                        os.chmod(copied_target, 0o444)
                        os.chmod(src, 0o444)
                    except OSError:
                        pass
                elif src.is_dir():
                    copied_target = dest_dir / "payload"
                    shutil.copytree(src, copied_target, dirs_exist_ok=True)
                    copied_path = str(copied_target)
                    for root, dirs, files in os.walk(dest_dir):
                        for f in files:
                            try: os.chmod(os.path.join(root, f), 0o444)
                            except OSError: pass
                        for d in dirs:
                            try: os.chmod(os.path.join(root, d), 0o555)
                            except OSError: pass

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
        self._save_registry()

        return manifest

    # --------------------------------------------------------------------------
    # Webhook Alert Dispatcher
    # --------------------------------------------------------------------------
    def dispatch_webhook_alert(self, event_type: str, payload: Dict[str, Any]) -> bool:
        envelope = {
            "version": "2.0",
            "event": event_type,
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            **payload
        }
        try:
            with open(self.webhook_sink, "a", encoding="utf-8") as sink_f:
                sink_f.write(json.dumps(envelope) + "\n")
        except Exception:
            pass

        try:
            req = urllib.request.Request(
                self.webhook_url,
                data=json.dumps(envelope).encode("utf-8"),
                headers={"Content-Type": "application/json", "User-Agent": "Hagibis-Governor/2.0"},
                method="POST"
            )
            with urllib.request.urlopen(req, timeout=2.0) as resp:
                return 200 <= resp.status < 300
        except Exception:
            return False

    # --------------------------------------------------------------------------
    # Persistence
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
        except Exception:
            pass

    def _save_registry(self) -> None:
        payload = {
            "version": "2.0",
            "governor_public_key": self.governor_public_key_hex,
            "skills": {k: v.to_dict() for k, v in self._registry.items()},
            "history": [a.to_dict() for a in self._history[-50:]]
        }
        tmp_path = self.registry_path.with_suffix(".tmp")
        try:
            with open(tmp_path, "w", encoding="utf-8") as f:
                json.dump(payload, f, indent=2)
            tmp_path.replace(self.registry_path)
        except Exception:
            pass

    def _get_or_create_record(self, skill_id: str) -> SkillGovernanceRecord:
        if skill_id not in self._registry:
            self._registry[skill_id] = SkillGovernanceRecord(skill_id=skill_id)
        return self._registry[skill_id]

    def _persist_registry(self) -> None:
        self._save_registry()


# ==============================================================================
# CLI Dispatcher & Automation Tooling
# ==============================================================================

def create_governor_from_env() -> AutonomicCircuitBreakerGovernor:
    state_dir = os.environ.get("HGB_STATE_DIR")
    quarantine_dir = os.environ.get("HGB_QUARANTINE_DIR")
    registry_path = os.environ.get("HGB_CB_REGISTRY")
    webhook_url = os.environ.get("HGB_WEBHOOK_URL")
    webhook_sink = os.environ.get("HGB_WEBHOOK_SINK")
    max_fails = int(os.environ.get("HGB_CB_MAX_FAILURES", DEFAULT_MAX_CONSECUTIVE_FAILURES))
    t0 = float(os.environ.get("HGB_CB_T0", DEFAULT_T0))
    t_max = float(os.environ.get("HGB_CB_TMAX", DEFAULT_TMAX))
    window_sz = int(os.environ.get("HGB_CB_WINDOW_SIZE", DEFAULT_WINDOW_SIZE))
    window_sec = float(os.environ.get("HGB_CB_WINDOW_SEC", DEFAULT_WINDOW_DURATION_SEC))
    min_events = int(os.environ.get("HGB_CB_MIN_EVENTS", DEFAULT_MIN_EVENTS))
    anomaly_th = float(os.environ.get("HGB_CB_ANOMALY_THRESHOLD", DEFAULT_ANOMALY_THRESHOLD))
    recovery_th = int(os.environ.get("HGB_CB_RECOVERY_THRESHOLD", DEFAULT_RECOVERY_THRESHOLD))
    lancedb_dir = os.environ.get("HGB_LANCEDB_DIR")

    return AutonomicCircuitBreakerGovernor(
        state_dir=state_dir,
        quarantine_dir=quarantine_dir,
        registry_path=registry_path,
        webhook_url=webhook_url,
        webhook_sink=webhook_sink,
        max_consecutive_failures=max_fails,
        t0=t0,
        t_max=t_max,
        window_size=window_sz,
        window_duration_sec=window_sec,
        min_events=min_events,
        anomaly_threshold=anomaly_th,
        recovery_threshold=recovery_th,
        lancedb_dir=lancedb_dir
    )


def cli_main():
    import argparse
    parser = argparse.ArgumentParser(description="Hagibis Autopoietic Circuit Breaker Governor CLI")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # check
    p_check = subparsers.add_parser("check", help="Check skill execution eligibility")
    p_check.add_argument("skill_id", help="Skill or crate identifier")

    # record-failure
    p_fail = subparsers.add_parser("record-failure", help="Record failed mutation")
    p_fail.add_argument("skill_id", help="Skill identifier")
    p_fail.add_argument("--error", default="unknown error", help="Error trace message")
    p_fail.add_argument("--file", default=None, help="Target file path")
    p_fail.add_argument("--code", default=None, help="Diagnostic code (e.g. E0308)")
    p_fail.add_argument("--line", type=int, default=1, help="Error line number")
    p_fail.add_argument("--span", default="0:0", help="Error span")
    p_fail.add_argument("--msg", default=None, help="Normalized diagnostic message")
    p_fail.add_argument("--pre-hash", default=None, help="Pre-mutation file hash")
    p_fail.add_argument("--post-hash", default=None, help="Post-mutation file hash")

    # record-success
    p_succ = subparsers.add_parser("record-success", help="Record successful mutation")
    p_succ.add_argument("skill_id", help="Skill identifier")
    p_succ.add_argument("--file", default=None, help="Target file path")
    p_succ.add_argument("--pre-hash", default=None, help="Pre-mutation file hash")
    p_succ.add_argument("--post-hash", default=None, help="Post-mutation file hash")

    # operator-unlock
    p_unlock = subparsers.add_parser("operator-unlock", help="Signed MCP operator unlock")
    p_unlock.add_argument("skill_id", help="Skill identifier")
    p_unlock.add_argument("--operator-id", required=True, help="Operator identifier")
    p_unlock.add_argument("--lockout-id", required=True, help="Lockout receipt ID")
    p_unlock.add_argument("--nonce", type=int, required=True, help="Replay defense nonce")
    p_unlock.add_argument("--public-key", required=True, help="Operator Ed25519 public key (hex)")
    p_unlock.add_argument("--signature", required=True, help="Ed25519 signature (hex)")
    p_unlock.add_argument("--no-restore-perms", action="store_true", help="Do not restore 0644 file perms")

    # generate-operator-token
    p_gen = subparsers.add_parser("generate-operator-token", help="Generate Ed25519 token and signature for operator unlock")
    p_gen.add_argument("skill_id", help="Skill identifier")
    p_gen.add_argument("--operator-id", default="ops-admin-hitl", help="Operator identifier")
    p_gen.add_argument("--lockout-id", default=None, help="Active lockout ID (defaults to querying registry)")

    # backoff
    p_backoff = subparsers.add_parser("backoff", help="Calculate Fibonacci progression backoff")
    p_backoff.add_argument("iteration", type=int, help="Attempt iteration number")
    p_backoff.add_argument("--t0", type=float, default=DEFAULT_T0)
    p_backoff.add_argument("--tmax", type=float, default=DEFAULT_TMAX)

    # fingerprint
    p_fp = subparsers.add_parser("fingerprint", help="Calculate deterministic AST error fingerprint")
    p_fp.add_argument("--code", required=True)
    p_fp.add_argument("--file", required=True)
    p_fp.add_argument("--line", type=int, default=1)
    p_fp.add_argument("--span", default="0:0")
    p_fp.add_argument("--msg", required=True)

    # status
    p_status = subparsers.add_parser("status", help="Query skill governance status")
    p_status.add_argument("skill_id", help="Skill identifier")

    args = parser.parse_args()
    gov = create_governor_from_env()

    if args.command == "check":
        allowed, reason = gov.can_mutate(args.skill_id)
        rec = gov._registry.get(args.skill_id)
        cur_state = rec.state.value if rec else CircuitState.CLOSED.value
        if allowed:
            print(f"ALLOWED:{cur_state}:{reason}")
            sys.exit(0)
        else:
            print(f"BLOCKED:{cur_state}:{reason}")
            sys.exit(1)

    elif args.command == "record-failure":
        attempt = gov.record_mutation_attempt(
            skill_id=args.skill_id,
            pre_hash=args.pre_hash or "",
            post_hash=args.post_hash or "",
            success=False,
            error_trace=args.error,
            skill_path=args.file,
            diagnostic_code=args.code or "",
            diagnostic_line=args.line,
            diagnostic_span=args.span,
            diagnostic_message=args.msg or ""
        )
        rec = gov._registry.get(args.skill_id)
        cur_state = rec.state.value if rec else CircuitState.OPEN.value
        if cur_state == CircuitState.PERM_TRIPPED_CRYPTOLOCKED.value:
            print(f"PERM_CRYPTOLOCKED:{cur_state}:{rec.lockout_id}:{rec.last_ast_fingerprint}")
            sys.exit(3)
        elif cur_state == CircuitState.PERM_TRIPPED.value:
            print(f"PERM_TRIP:{cur_state}:{rec.consecutive_fingerprint_count}:{rec.last_ast_fingerprint}")
            sys.exit(3)
        elif cur_state == CircuitState.OPEN.value:
            delay = round(max(0.0, rec.open_until_timestamp - time.time()), 3) if rec else 1.0
            print(f"TRIP:OPEN:{rec.consecutive_failures if rec else 1}:{delay}")
            sys.exit(2)
        else:
            print(f"FAIL:{cur_state}:{rec.consecutive_failures if rec else 1}")
            sys.exit(1)

    elif args.command == "record-success":
        attempt = gov.record_mutation_attempt(
            skill_id=args.skill_id,
            pre_hash=args.pre_hash or "",
            post_hash=args.post_hash or "",
            success=True,
            skill_path=args.file
        )
        rec = gov._registry.get(args.skill_id)
        cur_state = rec.state.value if rec else CircuitState.CLOSED.value
        print(f"OK:{cur_state}:{rec.consecutive_successes if rec else 1}")
        sys.exit(0)
        rec = gov._registry.get(args.skill_id)
        cur_state = rec.state.value if rec else CircuitState.CLOSED.value
        print(f"OK:{cur_state}:{rec.consecutive_successes if rec else 1}")
        sys.exit(0)

    elif args.command == "operator-unlock":
        try:
            receipt = gov.operator_unlock_circuit_breaker(
                skill_id=args.skill_id,
                operator_id=args.operator_id,
                lockout_id=args.lockout_id,
                nonce=args.nonce,
                operator_public_key=args.public_key,
                ed25519_signature=args.signature,
                restore_permissions=not args.no_restore_perms
            )
            print("UNLOCK_OK:" + json.dumps(receipt))
            sys.exit(0)
        except Exception as e:
            print(f"UNLOCK_ERR:{e}", file=sys.stderr)
            sys.exit(1)

    elif args.command == "generate-operator-token":
        if not _HAS_CRYPTOGRAPHY:
            print("ERROR: cryptography package required for operator token generation", file=sys.stderr)
            sys.exit(1)
        rec = gov._registry.get(args.skill_id)
        lockout_id = args.lockout_id or (rec.lockout_id if rec else None)
        if not lockout_id:
            print(f"ERROR: Skill [{args.skill_id}] is not currently locked out (no active lockout_id)", file=sys.stderr)
            sys.exit(1)

        priv_key = ed25519.Ed25519PrivateKey.generate()
        pub_key = priv_key.public_key()
        pub_hex = pub_key.public_bytes_raw().hex()
        nonce = int(time.time() * 1000)

        payload = f"UNLOCK:{args.operator_id}:{args.skill_id}:{lockout_id}:{nonce}".encode("utf-8")
        sig_hex = priv_key.sign(payload).hex()

        token_out = {
            "skill_id": args.skill_id,
            "operator_id": args.operator_id,
            "lockout_id": lockout_id,
            "nonce": nonce,
            "operator_public_key": pub_hex,
            "ed25519_signature": sig_hex,
            "mcp_tool": "hgb_operator_unlock_circuit_breaker",
            "mcp_payload": {
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {
                    "name": "hgb_operator_unlock_circuit_breaker",
                    "arguments": {
                        "skill_id": args.skill_id,
                        "operator_id": args.operator_id,
                        "lockout_id": lockout_id,
                        "nonce": nonce,
                        "operator_public_key": pub_hex,
                        "ed25519_signature": sig_hex,
                        "restore_permissions": True
                    }
                }
            }
        }
        print(json.dumps(token_out, indent=2))
        sys.exit(0)

    elif args.command == "backoff":
        b = gov.calculate_fibonacci_backoff(args.iteration, args.t0, args.tmax)
        print(f"{b:.3f}")
        sys.exit(0)

    elif args.command == "fingerprint":
        fp = gov.compute_ast_error_fingerprint(
            error_code=args.code,
            file_path=args.file,
            line=args.line,
            span=args.span,
            error_message=args.msg
        )
        print(fp)
        sys.exit(0)

    elif args.command == "status":
        rec = gov._registry.get(args.skill_id)
        if rec:
            print(json.dumps(rec.to_dict(), indent=2))
        else:
            print(f'{{"skill_id": "{args.skill_id}", "state": "CLOSED", "status": "unregistered"}}')
        sys.exit(0)


if __name__ == "__main__":
    cli_main()

