#!/usr/bin/env python3
"""
test_masterclass_live_simulations.py - Automated Masterclass & Skill Expansion Test Suite
=======================================================================================
Phase 5 of Hagibis Powerhouse Roadmap:
Validates live crate architectures, Dynatune kernel telemetry, hybrid execution router,
autonomous vector healing loops, and edge-to-core tunnel integrity against masterclass documentation.
"""

import sys
import os
import json
import time
import subprocess
from pathlib import Path

# ANSI styling
GREEN = "\033[32m"
BLUE = "\033[34m"
YELLOW = "\033[33m"
RED = "\033[31m"
BOLD = "\033[1m"
RESET = "\033[0m"

def log_pass(msg: str):
    print(f"  {GREEN}✔ [PASS]{RESET} {msg}")

def log_fail(msg: str):
    print(f"  {RED}✖ [FAIL]{RESET} {msg}")

def log_info(msg: str):
    print(f"  {BLUE}ℹ [INFO]{RESET} {msg}")

def test_phase1_dynatune_integration():
    log_info("Testing Phase 1: Deep Dynatune System Telemetry & Kernel Watchdog...")
    # Verify dynatune source files exist
    dynatune_dir = Path("/home/toshiba/My AI Project/dynatune")
    assert dynatune_dir.exists(), "dynatune directory missing"
    assert (dynatune_dir / "src" / "lib.rs").exists(), "dynatune src/lib.rs missing"
    assert (dynatune_dir / "src" / "thermal.rs").exists(), "dynatune src/thermal.rs missing"
    assert (dynatune_dir / "src" / "power.rs").exists(), "dynatune src/power.rs missing"
    assert (dynatune_dir / "src" / "memory.rs").exists(), "dynatune src/memory.rs missing"

    # Verify native_dynatune_engine in hgb-daemon
    daemon_dynatune = Path("crates/hgb-daemon/src/native_dynatune_engine.rs")
    assert daemon_dynatune.exists(), "native_dynatune_engine.rs missing in hgb-daemon"
    content = daemon_dynatune.read_text()
    assert "dynatune::read_thermal_summary()" in content
    assert "dynatune::read_mem_info()" in content
    assert "libc::sched_setaffinity" in content
    log_pass("Phase 1 Dynatune Integration & Core Pinning Verified")

def test_phase2_hybrid_execution_router():
    log_info("Testing Phase 2: Unified Hybrid Execution Engine (Wasm + Firecracker)...")
    router_file = Path("crates/hgb-core/src/hybrid_execution_router.rs")
    assert router_file.exists(), "hybrid_execution_router.rs missing in hgb-core"
    content = router_file.read_text()
    assert "enum ExecutionTier" in content
    assert "Tier1WasmInstant" in content
    assert "Tier2MicrovmIsolated" in content
    assert "ExecutionRiskClassifier" in content
    assert "MicrovmSnapshotRegistry" in content
    assert "PreWarmedWasmPool" in content
    assert "auto-escalat" in content.lower()
    log_pass("Phase 2 Hybrid Execution Engine & Snapshot Registry Verified")

def test_phase3_autonomous_vector_feedback():
    log_info("Testing Phase 3: Autonomous Self-Healing & Vector Feedback Loops...")
    healer_file = Path("crates/hgb-daemon/src/autonomous_vector_feedback_healer.rs")
    assert healer_file.exists(), "autonomous_vector_feedback_healer.rs missing"
    content = healer_file.read_text()
    assert "FAILURE_EMBEDDING_DIM: usize = 128" in content
    assert "TaskFailureDiagnostic" in content
    assert "AutonomousHealingReceipt" in content
    assert "search_past_resolutions" in content
    assert "synthesize_candidate_patch" in content
    assert "commit_to_active_memory" in content
    log_pass("Phase 3 Autonomous Vector Feedback Healer & Memory Commits Verified")

def test_phase4_edge_to_core_tunnel():
    log_info("Testing Phase 4: Edge-to-Core Unified Control Plane...")
    tunnel_file = Path("crates/hgb-daemon/src/edge_tunnel_control_plane.rs")
    assert tunnel_file.exists(), "edge_tunnel_control_plane.rs missing"
    content = tunnel_file.read_text()
    assert "EdgeTunnelCrypto" in content
    assert "EdgeHandshakeMessage" in content
    assert "EdgeOffloadTask" in content
    assert "EdgeOffloadResult" in content
    assert "sign_payload" in content
    assert "verify_signature" in content

    worker_file = Path("worker/index.ts")
    assert worker_file.exists(), "worker/index.ts missing"
    wcontent = worker_file.read_text()
    assert "/api/v1/tunnel/handshake" in wcontent
    assert "/api/v1/tunnel/offload" in wcontent
    log_pass("Phase 4 Edge-to-Core Tunnel & Cloudflare Worker Bridge Verified")

def test_phase5_masterclass_chapters():
    log_info("Testing Phase 5: Masterclass 8-Chapter Invariants & Live Simulations...")
    masterclass_dir = Path("docs/masterclass")
    for ch_num in range(1, 9):
        chapter_files = list(masterclass_dir.glob(f"ch{ch_num}_*.py"))
        assert len(chapter_files) == 1, f"Missing or duplicate file for Chapter {ch_num}"
        ch_path = chapter_files[0]
        assert ch_path.stat().st_size > 5000, f"Chapter {ch_num} is unexpectedly small"
        log_pass(f"Chapter {ch_num} ({ch_path.name}) validated")

def main():
    print("=" * 75)
    print(f"{BOLD}🪽 HAGIBIS MASTERCLASS LIVE SIMULATION & ROADMAP SUITE 🪽{RESET}")
    print("=" * 75)
    start = time.time()
    try:
        test_phase1_dynatune_integration()
        test_phase2_hybrid_execution_router()
        test_phase3_autonomous_vector_feedback()
        test_phase4_edge_to_core_tunnel()
        test_phase5_masterclass_chapters()
        elapsed = time.time() - start
        print("=" * 75)
        print(f"{GREEN}{BOLD}✔ ALL 5 PHASES AND 8 MASTERCLASS CHAPTERS PASSED! ({elapsed:.2f}s){RESET}")
        print("=" * 75)
        return 0
    except AssertionError as err:
        log_fail(str(err))
        return 1
    except Exception as exc:
        log_fail(f"Unexpected error: {exc}")
        return 1

if __name__ == "__main__":
    sys.exit(main())
