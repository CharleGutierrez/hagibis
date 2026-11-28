#!/usr/bin/env bash
# ==============================================================================
# scripts/chaos-ci-runner.sh
# Hagibis v1.0 Production Automated Chaos-Injected Chaos Regression Test Suite
# ==============================================================================
# Codifies chaos fault injection (memory bloat, storage saturation, worker
# disconnect, socket partition, disk latency, near-OOM recovery) into a mandatory
# CI regression gate with strict SLA enforcement (<5000ms per scenario) & 100% healing.
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${WORKSPACE_ROOT}"

# Color constants for terminal formatting
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# SLA threshold in milliseconds (default: 5000ms per scenario)
SLA_MAX_MS="${CHAOS_SLA_MS:-5000}"

echo -e "${CYAN}${BOLD}======================================================================${NC}"
echo -e "${CYAN}${BOLD}⚡ HAGIBIS v1.0 PRODUCTION CHAOS REGRESSION RUNNER ⚡${NC}"
echo -e "${CYAN}${BOLD}======================================================================${NC}"
echo -e "${BLUE}ℹ Workspace Root:${NC} ${WORKSPACE_ROOT}"

# 1. Setup isolated test environment in memory-backed tmpfs (/dev/shm) or /tmp
if [ -d "/dev/shm" ] && [ -w "/dev/shm" ]; then
    CHAOS_SCRATCH_DIR=$(mktemp -d /dev/shm/hgb_chaos_ci_XXXXXX)
else
    CHAOS_SCRATCH_DIR=$(mktemp -d /tmp/hgb_chaos_ci_XXXXXX)
fi
export HGB_CHAOS_SCRATCH_DIR="${CHAOS_SCRATCH_DIR}"
echo -e "${BLUE}ℹ Isolated Scratch Dir:${NC} ${CHAOS_SCRATCH_DIR}"

# Cleanup scratch directory on exit
cleanup() {
    local rc=$?
    if [ -d "${CHAOS_SCRATCH_DIR}" ]; then
        rm -rf "${CHAOS_SCRATCH_DIR}" 2>/dev/null || true
    fi
    exit ${rc}
}
trap cleanup EXIT INT TERM

# 2. Configure isolated chaos communication port
if [ -z "${HGB_CHAOS_PORT:-}" ]; then
    HGB_CHAOS_PORT=$(python3 -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1]); s.close()")
    export HGB_CHAOS_PORT
fi
echo -e "${BLUE}ℹ Isolated Chaos Port:${NC} ${HGB_CHAOS_PORT}"
echo -e "${BLUE}ℹ Per-Scenario SLA Limit:${NC} ${SLA_MAX_MS}ms"
echo -e "${CYAN}----------------------------------------------------------------------${NC}"

# Target JSON report file
REPORT_JSON="${CHAOS_SCRATCH_DIR}/chaos_report.json"
CHAOS_SCRIPT="${WORKSPACE_ROOT}/docs/masterclass/ch8_chaos_injector.py"

if [ ! -f "${CHAOS_SCRIPT}" ]; then
    echo -e "${RED}✖ Error: Chaos injector script not found at ${CHAOS_SCRIPT}${NC}"
    exit 1
fi

# 3. Execute masterclass chaos injector
echo -e "${BLUE}⚡ Executing Masterclass Chaos Fault Injector Suite (7 Scenarios)...${NC}\n"
START_TIME_MS=$(python3 -c "import time; print(int(time.time() * 1000))")

set +e
python3 "${CHAOS_SCRIPT}" --output "${REPORT_JSON}"
INJECTOR_EXIT_CODE=$?
set -e

END_TIME_MS=$(python3 -c "import time; print(int(time.time() * 1000))")
TOTAL_SUITE_DURATION_MS=$((END_TIME_MS - START_TIME_MS))

if [ ! -f "${REPORT_JSON}" ]; then
    echo -e "\n${RED}✖ CRITICAL: Chaos injector failed to produce JSON report at ${REPORT_JSON}${NC}"
    exit 1
fi

# 4. Validate SLA compliance and generate summary scorecard
echo -e "\n${CYAN}${BOLD}===================================================================================================${NC}"
echo -e "${CYAN}${BOLD}               HAGIBIS v1.0 PRODUCTION CHAOS REGRESSION SCORECARD${NC}"
echo -e "${CYAN}${BOLD}===================================================================================================${NC}"

python3 - << EOF
import json
import sys

sla_limit = float("${SLA_MAX_MS}")
report_path = "${REPORT_JSON}"
injector_rc = int("${INJECTOR_EXIT_CODE}")

with open(report_path, "r") as f:
    data = json.load(f)

reports = data.get("reports", [])
total = data.get("total_scenarios", 0)
healed = data.get("healed_scenarios", 0)
resilience_pct = data.get("resilience_score_pct", 0.0)

print(" # | Scenario                               | Status   | Duration     | SLA (<${SLA_MAX_MS}ms) | Health Summary")
print("---+----------------------------------------+----------+--------------+---------------+------------------------------------")

sla_breaches = 0
failed_scenarios = 0

for idx, r in enumerate(reports, 1):
    sc_name = r.get("scenario", "unknown")
    status = r.get("status", "UNKNOWN")
    duration = float(r.get("duration_ms", 0.0))
    details = r.get("details", {})
    
    # Evaluate SLA
    sla_pass = (duration < sla_limit)
    if not sla_pass:
        sla_breaches += 1
    sla_str = "\033[32mPASS\033[0m" if sla_pass else "\033[31mFAIL (BREACH)\033[0m"
    
    if status != "HEALED":
        failed_scenarios += 1
        status_colored = f"\033[31m{status:<8}\033[0m"
    else:
        status_colored = f"\033[32m{status:<8}\033[0m"

    # Contextual health summary
    summary = "Verified"
    if sc_name == "memory_leak":
        summary = f"Reclaimed {details.get('reclaimed_mb', 0)}MB (Worker Restarted)"
    elif sc_name == "storage_saturation":
        summary = f"Evicted {details.get('evicted_keys_count', 0)} entries, vacuumed"
    elif sc_name == "worker_disconnect":
        summary = f"Failover: {details.get('active_worker_id')}, 0 loss"
    elif sc_name == "socket_heartbeat_partition":
        summary = f"Port rebind: {details.get('initial_port')} -> {details.get('failover_port')}"
    elif sc_name == "disk_io_latency":
        summary = f"Queue resized ({details.get('initial_queue_capacity')}->{details.get('adapted_queue_capacity')}), WAL verified"
    elif sc_name == "process_oom_recovery":
        summary = f"Reclaimed {details.get('heap_reclaimed_mb', 0)}MB, {details.get('rehydrated_record_count', 0)} state objs"
    elif sc_name == "agent_infinite_mutation_loop_bisection":
        summary = f"Oscillation isolated at step {details.get('culprit_step')}, rollback verified in {details.get('healing_duration_ms', 0)}ms"

    print(f"{idx:>2} | {sc_name:<38} | {status_colored} | {duration:>8.2f} ms | {sla_str:<13} | {summary}")

print("---+----------------------------------------+----------+--------------+---------------+------------------------------------")
print(f" Total Scenarios: {total} | Healed: {healed} | Resilience Score: {resilience_pct:.1f}% | SLA Breaches: {sla_breaches}")
print("===================================================================================================")

# Validation rules:
# 1. Injector exit code must be 0
# 2. Total scenarios must be at least 7
# 3. All scenarios must be HEALED (resilience_pct == 100.0)
# 4. Zero SLA breaches (duration < sla_limit)
regression_detected = False

if injector_rc != 0:
    print(f"\033[31m✖ REGRESSION: Chaos injector script exited with non-zero code {injector_rc}\033[0m")
    regression_detected = True

if total < 7:
    print(f"\033[31m✖ REGRESSION: Incomplete test suite execution (expected >=7 scenarios, ran {total})\033[0m")
    regression_detected = True

if failed_scenarios > 0 or healed != total or resilience_pct < 100.0:
    print(f"\033[31m✖ REGRESSION: {failed_scenarios} chaos scenario(s) failed autonomous recovery!\033[0m")
    regression_detected = True

if sla_breaches > 0:
    print(f"\033[31m✖ REGRESSION: {sla_breaches} scenario(s) breached strict SLA limit (<{sla_limit}ms)!\033[0m")
    regression_detected = True

if regression_detected:
    sys.exit(1)
else:
    print("\033[32m✔ SUCCESS: 100% of chaos scenarios healed autonomously within strict SLA limits! Regression gate PASSED.\033[0m")
    sys.exit(0)
EOF

VALIDATION_EXIT_CODE=$?

if [ ${VALIDATION_EXIT_CODE} -eq 0 ]; then
    echo -e "${GREEN}${BOLD}======================================================================${NC}"
    echo -e "${GREEN}${BOLD}🎉 CHAOS REGRESSION SUITE: ALL SCENARIOS HEALED WITHIN SLA (EXIT 0) 🎉${NC}"
    echo -e "${GREEN}${BOLD}======================================================================${NC}"
    exit 0
else
    echo -e "${RED}${BOLD}======================================================================${NC}"
    echo -e "${RED}${BOLD}✖ CHAOS REGRESSION SUITE: REGRESSION DETECTED (EXIT ${VALIDATION_EXIT_CODE}) ✖${NC}"
    echo -e "${RED}${BOLD}======================================================================${NC}"
    exit ${VALIDATION_EXIT_CODE}
fi
