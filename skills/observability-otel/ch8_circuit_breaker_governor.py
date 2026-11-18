#!/usr/bin/env python3
"""
ch8_circuit_breaker_governor.py - Autopoietic Circuit-Breaker Governance Engine
================================================================================
Project Hagibis: Pillar 3 (Observability/OTel) & Pillar 4 (Autonomic Self-Healing)

Provides cryptographic state hashing (BLAKE3 / SHA-256), strict exponential backoff
dampening with jitter, automated skill quarantining after 3 consecutive failures,
and automated Human-In-The-Loop (HITL) Webhook alerts.

This module is designed for direct consumption by:
  - hagibis/docs/masterclass/ch8_autonomous_loops.py
  - hagibis/docs/masterclass/ch8_chaos_injector.py
  - hagibis/skills/observability-otel/
  - Autonomous self-healing daemons and compiler remediation pipelines.
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


# ==============================================================================
# Domain Models & Circuit Breaker States
# ==============================================================================

class CircuitState(str, Enum):
    CLOSED = "Closed"          # Healthy, mutations permitted
    HALF_OPEN = "HalfOpen"    # Recovering / dampening after partial failures
    TRIPPED = "Tripped"        # Quarantined / locked, mutations forbidden
    QUARANTINED = "Quarantined" # Quarantined with file permissions revoked


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
    total_failures: int = 0
    total_successes: int = 0
    is_quarantined: bool = False
    quarantine_path: Optional[str] = None
    quarantine_reason: Optional[str] = None
    quarantined_at: Optional[str] = None
    last_mutation_timestamp: Optional[float] = None
    pre_mutation_hash: Optional[str] = None
    post_mutation_hash: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        d = asdict(self)
        d["state"] = self.state.value
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> SkillGovernanceRecord:
        raw_state = data.get("state", CircuitState.CLOSED.value)
        try:
            state = CircuitState(raw_state)
        except ValueError:
            state = CircuitState.CLOSED

        return cls(
            skill_id=data["skill_id"],
            state=state,
            consecutive_failures=data.get("consecutive_failures", 0),
            total_failures=data.get("total_failures", 0),
            total_successes=data.get("total_successes", 0),
            is_quarantined=data.get("is_quarantined", False),
            quarantine_path=data.get("quarantine_path"),
            quarantine_reason=data.get("quarantine_reason"),
            quarantined_at=data.get("quarantined_at"),
            last_mutation_timestamp=data.get("last_mutation_timestamp"),
            pre_mutation_hash=data.get("pre_mutation_hash"),
            post_mutation_hash=data.get("post_mutation_hash")
        )


# ==============================================================================
# Autonomic Circuit Breaker Governor Implementation
# ==============================================================================

class AutonomicCircuitBreakerGovernor:
    """
    Enterprise Governor overseeing self-healing autopoietic mutation loops.
    Guarantees state immutability checking, strict dampening backoff, and
    immediate quarantine lockouts upon repeated failures.
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
    ):
        self.state_dir = Path(state_dir) if state_dir else DEFAULT_STATE_DIR
        self.quarantine_dir = Path(quarantine_dir) if quarantine_dir else DEFAULT_QUARANTINE_DIR
        self.registry_path = Path(registry_path) if registry_path else (self.state_dir / "circuit_breaker_registry.json")
        self.webhook_url = webhook_url or DEFAULT_WEBHOOK_URL
        self.webhook_sink = Path(webhook_sink) if webhook_sink else DEFAULT_WEBHOOK_SINK
        self.max_failures = max(1, max_consecutive_failures)
        self.t0 = float(t0)
        self.t_max = float(t_max)

        self._ensure_directories()
        self._registry: Dict[str, SkillGovernanceRecord] = {}
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
            # Return hash of empty string if target does not exist yet
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
            # Deterministic composite manifest of relative paths and individual file hashes
            entries: List[str] = []
            for root, dirs, files in os.walk(path):
                dirs.sort()
                for file_name in sorted(files):
                    if file_name.startswith("."):
                        continue
                    full_p = Path(root) / file_name
                    rel_p = full_p.relative_to(path).as_posix()
                    # Hash individual file
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
    # Strict Exponential Backoff Dampening
    # --------------------------------------------------------------------------
    def calculate_backoff(
        self,
        iteration: int,
        with_jitter: bool = True,
        jitter_seed: Optional[int] = None
    ) -> float:
        """
        Calculates strict exponential backoff dampening:
          T(iter) = min(T_max, T_0 * 2^(iter - 1)) + jitter
        """
        iter_clamped = max(1, iteration)
        base_backoff = min(self.t_max, self.t0 * (2.0 ** (iter_clamped - 1)))

        jitter = 0.0
        if with_jitter:
            rng = random.Random(jitter_seed) if jitter_seed is not None else random
            jitter = rng.uniform(0.05, 0.45)

        return round(base_backoff + jitter, 4)

    def apply_backoff(
        self,
        skill_id: str,
        iteration: Optional[int] = None,
        dry_run: bool = False
    ) -> float:
        """
        Computes backoff and dampens the loop by sleeping.
        Returns the duration backed off.
        """
        rec = self._get_or_create_skill(skill_id)
        current_iter = iteration if iteration is not None else max(1, rec.consecutive_failures)
        delay = self.calculate_backoff(current_iter, with_jitter=True)

        if not dry_run and delay > 0:
            time.sleep(delay)
        return delay

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
            "version": "1.0",
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
                "User-Agent": "Hagibis-Autopoietic-Governor/1.0",
                "X-Hagibis-Event": event_type
            },
            method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=3.0) as resp:
                status = resp.status
                return 200 <= status < 300
        except Exception:
            # Network failure or mock endpoint offline - sink is primary guarantee
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
                    # Strip write permissions on quarantined copy
                    os.chmod(copied_target, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
                    # Strip write permissions on source to lock out further mutations
                    try:
                        os.chmod(src, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
                    except OSError:
                        pass
                elif src.is_dir():
                    copied_target = dest_dir / "payload"
                    shutil.copytree(src, copied_target, dirs_exist_ok=True)
                    copied_path = str(copied_target)
                    # Strip write permissions recursively
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
                    try:
                        for root, dirs, files in os.walk(src):
                            for f in files:
                                os.chmod(os.path.join(root, f), 0o444)
                    except OSError:
                        pass

        # Write Quarantine Manifest
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        manifest = {
            "skill_id": skill_id,
            "quarantined_at": iso_now,
            "consecutive_failures": self.max_failures,
            "pre_mutation_hash": pre_hash,
            "post_mutation_hash": post_hash,
            "quarantine_reason": reason,
            "original_path": str(skill_path) if skill_path else None,
            "quarantine_destination": str(dest_dir),
            "permissions_locked": True
        }
        manifest_file = dest_dir / "quarantine_manifest.json"
        with open(manifest_file, "w", encoding="utf-8") as mf:
            json.dump(manifest, mf, indent=2)
        try:
            os.chmod(manifest_file, 0o444)
        except OSError:
            pass

        # Update Skill Record
        rec = self._get_or_create_skill(skill_id)
        rec.state = CircuitState.QUARANTINED
        rec.is_quarantined = True
        rec.quarantine_path = str(dest_dir)
        rec.quarantine_reason = reason
        rec.quarantined_at = iso_now
        rec.pre_mutation_hash = pre_hash
        rec.post_mutation_hash = post_hash
        self._save_registry()

        # Webhook Alert Dispatch
        webhook_payload = {
            "skill_id": skill_id,
            "action": "quarantine_alert",
            "consecutive_failures": self.max_failures,
            "threshold": self.max_failures,
            "pre_mutation_hash": pre_hash,
            "post_mutation_hash": post_hash,
            "quarantine_path": str(dest_dir),
            "error_trace": reason,
            "action_required": "Human-In-The-Loop (HITL) audit required. Mutating permissions revoked.",
            "circuit_breaker_state": CircuitState.QUARANTINED.value
        }
        self.dispatch_webhook_alert("quarantine_alert", webhook_payload)

        return manifest

    # --------------------------------------------------------------------------
    # Circuit Breaker Status & Mutation Recording
    # --------------------------------------------------------------------------
    def can_mutate(self, skill_id: str) -> Tuple[bool, str]:
        """
        Determines whether a skill is legally authorized to execute mutations.
        Returns: (can_mutate: bool, reason_message: str)
        """
        rec = self._get_or_create_skill(skill_id)
        if rec.is_quarantined or rec.state in (CircuitState.QUARANTINED, CircuitState.TRIPPED):
            return (
                False,
                f"Skill [{skill_id}] is QUARANTINED ({rec.consecutive_failures} consecutive failures). "
                f"Mutating permissions locked. Reason: {rec.quarantine_reason}"
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
        duration_ms: float = 0.0
    ) -> Dict[str, Any]:
        """
        Primary lifecycle state transition handler.
        If success: resets consecutive failures to 0, transitions to CLOSED.
        If failure: increments failures. Upon reaching max_failures (3), triggers quarantine.
        """
        rec = self._get_or_create_skill(skill_id)
        now_ts = time.time()
        iso_now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now_ts))

        attempt = MutationAttempt(
            timestamp=now_ts,
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

        rec.last_mutation_timestamp = now_ts
        rec.pre_mutation_hash = pre_hash
        rec.post_mutation_hash = post_hash

        outcome: Dict[str, Any] = {
            "skill_id": skill_id,
            "success": success,
            "timestamp": iso_now,
            "pre_hash": pre_hash,
            "post_hash": post_hash
        }

        if success:
            rec.consecutive_failures = 0
            rec.total_successes += 1
            rec.state = CircuitState.CLOSED
            outcome["state"] = CircuitState.CLOSED.value
            outcome["action"] = "MUTATION_ACCEPTED"
        else:
            rec.consecutive_failures += 1
            rec.total_failures += 1
            outcome["consecutive_failures"] = rec.consecutive_failures

            if rec.consecutive_failures >= self.max_failures:
                # Breaker Trips! Quarantine initiated!
                q_info = self.quarantine_skill(
                    skill_id=skill_id,
                    skill_path=skill_path,
                    reason=error_trace or f"Failed {rec.consecutive_failures} consecutive mutations",
                    pre_hash=pre_hash,
                    post_hash=post_hash
                )
                outcome["state"] = CircuitState.QUARANTINED.value
                outcome["action"] = "BREAKER_TRIPPED_QUARANTINED"
                outcome["quarantine_info"] = q_info
            else:
                rec.state = CircuitState.HALF_OPEN
                outcome["state"] = CircuitState.HALF_OPEN.value
                outcome["action"] = "FAILURE_DAMPENING"
                # Calculate backoff delay
                backoff = self.calculate_backoff(rec.consecutive_failures)
                outcome["recommended_backoff_sec"] = backoff

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
        to un-quarantine a skill and restore write permissions.
        """
        if skill_id not in self._registry:
            return False

        rec = self._registry[skill_id]
        rec.consecutive_failures = 0
        rec.state = CircuitState.CLOSED
        rec.is_quarantined = False
        rec.quarantine_reason = None
        rec.quarantine_path = None

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
        # Dispatch HITL reset audit webhook
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
            # Mutation completed without unhandled exception
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
            if apply_backoff_on_failure and rec_res.get("state") != CircuitState.QUARANTINED.value:
                self.apply_backoff(skill_id)
            raise

    # --------------------------------------------------------------------------
    # Persistence Helpers
    # --------------------------------------------------------------------------
    def _get_or_create_skill(self, skill_id: str) -> SkillGovernanceRecord:
        if skill_id not in self._registry:
            self._registry[skill_id] = SkillGovernanceRecord(skill_id=skill_id)
        return self._registry[skill_id]

    def _load_registry(self) -> None:
        if not self.registry_path.exists():
            return
        try:
            with open(self.registry_path, "r", encoding="utf-8") as f:
                data = json.load(f)
            for s_id, s_data in data.get("skills", {}).items():
                self._registry[s_id] = SkillGovernanceRecord.from_dict(s_data)
        except Exception as e:
            sys.stderr.write(f"[WARN] Failed to parse circuit breaker registry {self.registry_path}: {e}\n")

    def _save_registry(self) -> None:
        payload = {
            "version": "1.0",
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
    print("  ch8_circuit_breaker_governor: Unit & Integration Verification  ")
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
            t_max=8.0
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

        # 2. Test Exponential Backoff
        print("2. Testing Strict Exponential Backoff Formula...")
        b1 = gov.calculate_backoff(1, with_jitter=False)
        b2 = gov.calculate_backoff(2, with_jitter=False)
        b3 = gov.calculate_backoff(3, with_jitter=False)
        b4 = gov.calculate_backoff(4, with_jitter=False)
        assert b1 == 1.0, f"Expected 1.0, got {b1}"
        assert b2 == 2.0, f"Expected 2.0, got {b2}"
        assert b3 == 4.0, f"Expected 4.0, got {b3}"
        assert b4 == 8.0, f"Expected 8.0, got {b4}"
        # With jitter
        b1_j = gov.calculate_backoff(1, with_jitter=True)
        assert 1.05 <= b1_j <= 1.45, f"Jitter out of range: {b1_j}"
        print("   ✔ Exponential backoff dampening curve verified.")

        # 3. Test Quarantine on 3 Failures
        print("3. Testing 3 Consecutive Failures Quarantine & Permission Revocation...")
        skill_id = "test_opt_skill"
        skill_file = tmp_p / "target_skill.rs"
        skill_file.write_text("fn broken() {}")

        # Failure 1
        res1 = gov.record_mutation_attempt(skill_id, h1, h2, success=False, error_trace="SyntaxErr 1", skill_path=skill_file)
        assert res1["state"] == CircuitState.HALF_OPEN.value
        can_run, _ = gov.can_mutate(skill_id)
        assert can_run is True

        # Failure 2
        res2 = gov.record_mutation_attempt(skill_id, h1, h2, success=False, error_trace="SyntaxErr 2", skill_path=skill_file)
        assert res2["state"] == CircuitState.HALF_OPEN.value
        can_run, _ = gov.can_mutate(skill_id)
        assert can_run is True

        # Failure 3 -> MUST TRIP!
        res3 = gov.record_mutation_attempt(skill_id, h1, h2, success=False, error_trace="Fatal Type Mismatch", skill_path=skill_file)
        assert res3["state"] == CircuitState.QUARANTINED.value
        assert res3["action"] == "BREAKER_TRIPPED_QUARANTINED"

        can_run, block_msg = gov.can_mutate(skill_id)
        assert can_run is False
        assert "QUARANTINED" in block_msg

        # Verify permissions stripped
        assert not os.access(skill_file, os.W_OK), "Write permissions should be revoked on failing skill source!"
        # Verify Webhook sink entry
        sink_lines = (tmp_p / "events.jsonl").read_text().strip().split("\n")
        assert len(sink_lines) >= 1
        last_event = json.loads(sink_lines[-1])
        assert last_event["event"] == "quarantine_alert"
        assert last_event["skill_id"] == skill_id
        assert last_event["consecutive_failures"] == 3
        print("   ✔ 3-Failure quarantine, permissions lockdown, and webhook alert verified.")

        # 4. Test HITL Reset
        print("4. Testing HITL Administrative Reset...")
        gov.reset_skill(skill_id, restore_path=skill_file)
        can_run, _ = gov.can_mutate(skill_id)
        assert can_run is True
        assert os.access(skill_file, os.W_OK), "Write permissions should be restored after HITL reset"
        print("   ✔ HITL administrative recovery verified.")

        # 5. Test Guarded Mutation Context Manager
        print("5. Testing Guarded Mutation Context Manager...")
        with gov.guarded_mutation(skill_id, skill_file):
            skill_file.write_text("fn healed() -> bool { true }")
        rec = gov._registry[skill_id]
        assert rec.consecutive_failures == 0
        assert rec.state == CircuitState.CLOSED
        print("   ✔ Guarded mutation context manager verified.")

    print("=" * 75)
    print("  ✔ ALL AUTONOMIC CIRCUIT BREAKER TESTS PASSED!  ")
    print("=" * 75)
    return True


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] in ("--test", "test"):
        success = run_self_tests()
        sys.exit(0 if success else 1)
    else:
        print("ch8_circuit_breaker_governor loaded. Use --test to execute verification suite.")
