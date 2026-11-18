#!/usr/bin/env bash
# ==============================================================================
# circuit_breaker_governor.sh
# Autopoietic Circuit-Breaker Governance Engine (Pillar 3 & Pillar 4)
# Project Hagibis - Zero-Stub Production Grade Governance
#
# Capabilities:
#   1. Cryptographic State Hashing: Deterministic BLAKE3 / SHA256 hashing of
#      AST and codebase source trees before & after each healing mutation.
#   2. Strict Exponential Backoff Dampening:
#      T(iter) = min(T_max, T_0 * 2^(iter - 1)) + jitter
#   3. Automated Skill Quarantining:
#      Locks mutating permissions and isolates skill upon 3 consecutive failures.
#   4. Automated HITL Webhook Alerts:
#      Dispatches structured JSON payloads to OTel / Alertmanager endpoint.
#   5. Administrative Recovery:
#      Allows Human-In-The-Loop (HITL) unlocking and post-mortem audit.
# ==============================================================================
set -euo pipefail

# Configuration Defaults (overridable via environment)
HGB_STATE_DIR="${HGB_STATE_DIR:-${HOME}/.hagibis}"
HGB_QUARANTINE_DIR="${HGB_QUARANTINE_DIR:-${HGB_STATE_DIR}/quarantine}"
HGB_CB_REGISTRY="${HGB_CB_REGISTRY:-${HGB_STATE_DIR}/circuit_breaker_registry.json}"
HGB_WEBHOOK_URL="${HGB_WEBHOOK_URL:-http://127.0.0.1:8083/api/v1/alerts}"
HGB_WEBHOOK_SINK="${HGB_WEBHOOK_SINK:-/tmp/hgb_quarantine_webhook_events.jsonl}"
HGB_CB_MAX_FAILURES="${HGB_CB_MAX_FAILURES:-3}"
HGB_CB_T0="${HGB_CB_T0:-2.0}"
HGB_CB_TMAX="${HGB_CB_TMAX:-64.0}"

# ANSI Colors
RED="\033[31m"
GREEN="\033[32m"
YELLOW="\033[33m"
BLUE="\033[34m"
MAGENTA="\033[35m"
CYAN="\033[36m"
BOLD="\033[1m"
RESET="\033[0m"

log_info()  { echo -e "${BLUE}ℹ [CB-INFO]${RESET} $*" >&2; }
log_ok()    { echo -e "${GREEN}✔ [CB-OK]${RESET} $*" >&2; }
log_warn()  { echo -e "${YELLOW}⚠ [CB-WARN]${RESET} $*" >&2; }
log_err()   { echo -e "${RED}✖ [CB-ERR]${RESET} $*" >&2; }
log_trip()  { echo -e "${MAGENTA}${BOLD}⚡ [CIRCUIT-BREAKER TRIPPED]${RESET} $*" >&2; }

# Initialize State Directory and Registry
init_governor_environment() {
    mkdir -p "$HGB_STATE_DIR" "$HGB_QUARANTINE_DIR"
    if [ ! -f "$HGB_CB_REGISTRY" ]; then
        echo '{"version":"1.0","skills":{},"history":[]}' > "$HGB_CB_REGISTRY"
    fi
}

# ------------------------------------------------------------------------------
# 1. Cryptographic State Hashing
# ------------------------------------------------------------------------------
hgb_hash_state() {
    local target_path="${1:-}"
    if [ -z "$target_path" ] || [ ! -e "$target_path" ]; then
        echo "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" # empty hash
        return 0
    fi

    # Determine hash engine (BLAKE3 if b3sum present, else SHA-256)
    local use_b3=false
    if command -v b3sum >/dev/null 2>&1; then
        use_b3=true
    fi

    if [ -f "$target_path" ]; then
        if [ "$use_b3" = true ]; then
            local h
            h=$(b3sum --no-names "$target_path" 2>/dev/null | awk '{print $1}')
            echo "blake3:${h}"
        else
            local h
            h=$(sha256sum "$target_path" | awk '{print $1}')
            echo "sha256:${h}"
        fi
    elif [ -d "$target_path" ]; then
        # Deterministic composite hash across sorted directory file paths & contents
        local manifest_hash
        manifest_hash=$(python3 -c "
import os, sys, hashlib

def hash_file(p):
    h = hashlib.sha256()
    try:
        with open(p, 'rb') as f:
            while chunk := f.read(65536):
                h.update(chunk)
        return h.hexdigest()
    except Exception:
        return 'error'

target = sys.argv[1]
entries = []
for root, dirs, files in os.walk(target):
    dirs.sort()
    for f in sorted(files):
        if f.startswith('.'):
            continue
        full_p = os.path.join(root, f)
        rel_p = os.path.relpath(full_p, target)
        entries.append(f'{rel_p}:{hash_file(full_p)}')

entries.sort()
manifest = '\n'.join(entries).encode('utf-8')
print('sha256:' + hashlib.sha256(manifest).hexdigest())
" "$target_path")
        echo "$manifest_hash"
    else
        echo "sha256:unknown"
    fi
}

# ------------------------------------------------------------------------------
# 2. Strict Exponential Backoff Dampening
# ------------------------------------------------------------------------------
hgb_calculate_backoff() {
    local iteration="${1:-1}"
    local t0="${2:-$HGB_CB_T0}"
    local tmax="${3:-$HGB_CB_TMAX}"

    python3 -c "
import sys, random
iteration = max(1, int(sys.argv[1]))
t0 = float(sys.argv[2])
tmax = float(sys.argv[3])

# Strict exponential backoff: T(iter) = min(T_max, T_0 * 2^(iter - 1))
base_backoff = min(tmax, t0 * (2.0 ** (iteration - 1)))
# Jitter dampening: pseudo-random float between 0.05 and 0.45s
jitter = random.uniform(0.05, 0.45)
total_backoff = base_backoff + jitter
print(f'{total_backoff:.3f}')
" "$iteration" "$t0" "$tmax"
}

hgb_apply_backoff() {
    local skill_id="${1:-unknown}"
    local iteration="${2:-1}"
    local backoff_sec
    backoff_sec=$(hgb_calculate_backoff "$iteration")
    log_warn "Dampening mutation attempt #${iteration} for skill [${skill_id}]: backing off ${backoff_sec}s..."
    python3 -c "import time, sys; time.sleep(float(sys.argv[1]))" "$backoff_sec"
}

# ------------------------------------------------------------------------------
# 3. Webhook Dispatcher
# ------------------------------------------------------------------------------
hgb_send_webhook() {
    local event_type="$1"
    local skill_id="$2"
    local error_trace="$3"
    local pre_hash="${4:-unknown}"
    local post_hash="${5:-unknown}"
    local quarantine_path="${6:-none}"

    init_governor_environment

    local now_iso
    now_iso=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

    local payload
    payload=$(python3 -c "
import json, sys
data = {
    'event': sys.argv[1],
    'skill_id': sys.argv[2],
    'error_trace': sys.argv[3],
    'pre_mutation_hash': sys.argv[4],
    'post_mutation_hash': sys.argv[5],
    'quarantine_path': sys.argv[6],
    'consecutive_failures': int(sys.argv[7]),
    'threshold': int(sys.argv[8]),
    'timestamp': sys.argv[9],
    'action_required': 'Human-In-The-Loop (HITL) audit required. Mutating permissions revoked.',
    'circuit_breaker_state': 'Tripped'
}
print(json.dumps(data, indent=2))
" "$event_type" "$skill_id" "$error_trace" "$pre_hash" "$post_hash" "$quarantine_path" "$HGB_CB_MAX_FAILURES" "$HGB_CB_MAX_FAILURES" "$now_iso")

    # Persist locally to webhook sink log
    echo "$payload" | tr '\n' ' ' >> "$HGB_WEBHOOK_SINK"
    echo "" >> "$HGB_WEBHOOK_SINK"

    # Attempt asynchronous HTTP POST dispatch
    if command -v curl >/dev/null 2>&1; then
        local http_code
        http_code=$(curl -s -S -o /dev/null -w "%{http_code}" -X POST \
            -H "Content-Type: application/json" \
            -H "X-Hagibis-Governor: autopoietic-circuit-breaker" \
            -d "$payload" \
            --connect-timeout 2 \
            --max-time 4 \
            "$HGB_WEBHOOK_URL" 2>/dev/null || echo "000")

        if [ "$http_code" = "200" ] || [ "$http_code" = "201" ] || [ "$http_code" = "202" ]; then
            log_ok "Automated Webhook alert dispatched successfully to $HGB_WEBHOOK_URL (HTTP $http_code)."
        else
            log_warn "Webhook endpoint unreachable (HTTP $http_code). Alert persisted to local sink: $HGB_WEBHOOK_SINK."
        fi
    else
        log_warn "curl not found; alert logged to sink: $HGB_WEBHOOK_SINK."
    fi
}

# ------------------------------------------------------------------------------
# 4. Skill Quarantining & Permission Locking
# ------------------------------------------------------------------------------
hgb_quarantine_skill() {
    local skill_id="$1"
    local skill_path="$2"
    local error_trace="$3"
    local pre_hash="${4:-unknown}"
    local post_hash="${5:-unknown}"

    init_governor_environment

    local timestamp
    timestamp=$(date +%s)
    local target_quarantine="${HGB_QUARANTINE_DIR}/${skill_id}/${timestamp}"
    mkdir -p "$target_quarantine"

    log_trip "Quarantining skill [${skill_id}] after ${HGB_CB_MAX_FAILURES} consecutive failures!"

    # Copy mutating artifacts to quarantine sandbox
    if [ -n "$skill_path" ] && [ -e "$skill_path" ]; then
        cp -a "$skill_path" "$target_quarantine/payload" 2>/dev/null || true
        # Remove write permission on active source to prevent rogue mutations
        chmod -R a-w "$skill_path" 2>/dev/null || true
    fi

    # Write quarantine manifest
    local manifest_file="${target_quarantine}/quarantine_manifest.json"
    cat <<EOF > "$manifest_file"
{
  "skill_id": "$skill_id",
  "quarantined_at": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "consecutive_failures": $HGB_CB_MAX_FAILURES,
  "pre_mutation_hash": "$pre_hash",
  "post_mutation_hash": "$post_hash",
  "error_trace": $(python3 -c "import json, sys; print(json.dumps(sys.argv[1]))" "$error_trace"),
  "source_path": "$skill_path",
  "permissions_locked": true
}
EOF
    chmod 0444 "$manifest_file" 2>/dev/null || true

    # Lock mutating permissions on quarantined payload & directory (Read-Only: 0444/0555)
    chmod -R a-w "$target_quarantine" 2>/dev/null || true
    log_ok "Mutating file permissions locked (0444 read-only) on active source and quarantine sandbox."

    # Update Registry
    python3 -c "
import json, sys, os
reg_file = sys.argv[1]
skill_id = sys.argv[2]
q_path = sys.argv[3]
reason = sys.argv[4]

data = {'version': '1.0', 'skills': {}, 'history': []}
if os.path.exists(reg_file):
    try: data = json.load(open(reg_file))
    except Exception: pass

skills = data.setdefault('skills', {})
s_entry = skills.setdefault(skill_id, {})
s_entry['status'] = 'Quarantined'
s_entry['consecutive_failures'] = int(sys.argv[5])
s_entry['is_quarantined'] = True
s_entry['quarantine_path'] = q_path
s_entry['quarantine_reason'] = reason
s_entry['locked_at'] = sys.argv[6]

with open(reg_file, 'w') as f:
    json.dump(data, f, indent=2)
" "$HGB_CB_REGISTRY" "$skill_id" "$target_quarantine" "$error_trace" "$HGB_CB_MAX_FAILURES" "$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    # Dispatch automated Webhook alert
    hgb_send_webhook "quarantine_alert" "$skill_id" "$error_trace" "$pre_hash" "$post_hash" "$target_quarantine"
}

# ------------------------------------------------------------------------------
# 5. Circuit Breaker Inspection & Mutation Recording
# ------------------------------------------------------------------------------
hgb_cb_check_skill() {
    local skill_id="$1"
    init_governor_environment

    local status
    status=$(python3 -c "
import json, sys, os
reg_file = sys.argv[1]
skill_id = sys.argv[2]
if not os.path.exists(reg_file):
    print('Closed')
    sys.exit(0)
try:
    data = json.load(open(reg_file))
    s = data.get('skills', {}).get(skill_id, {})
    if s.get('is_quarantined', False) or s.get('status') == 'Quarantined':
        print('Quarantined')
    elif s.get('status') == 'Tripped':
        print('Tripped')
    else:
        print(s.get('status', 'Closed'))
except Exception:
    print('Closed')
" "$HGB_CB_REGISTRY" "$skill_id")

    if [ "$status" = "Quarantined" ] || [ "$status" = "Tripped" ]; then
        log_trip "Skill [${skill_id}] is locked in state: ${status}. All mutations blocked!"
        return 1
    fi
    return 0
}

hgb_cb_record_attempt() {
    local skill_id="$1"
    local success="$2"          # "true" or "false"
    local error_trace="${3:-}"
    local pre_hash="${4:-none}"
    local post_hash="${5:-none}"
    local skill_path="${6:-}"

    init_governor_environment

    local result
    result=$(python3 -c "
import json, sys, os, time
reg_file = sys.argv[1]
skill_id = sys.argv[2]
success = sys.argv[3].lower() == 'true'
error_trace = sys.argv[4]
pre_hash = sys.argv[5]
post_hash = sys.argv[6]
max_failures = int(sys.argv[7])

data = {'version': '1.0', 'skills': {}, 'history': []}
if os.path.exists(reg_file):
    try: data = json.load(open(reg_file))
    except Exception: pass

skills = data.setdefault('skills', {})
s_entry = skills.setdefault(skill_id, {
    'status': 'Closed',
    'consecutive_failures': 0,
    'total_failures': 0,
    'total_successes': 0,
    'is_quarantined': False
})

history = data.setdefault('history', [])
history.append({
    'timestamp': time.time(),
    'skill_id': skill_id,
    'success': success,
    'pre_hash': pre_hash,
    'post_hash': post_hash,
    'error': error_trace[:200] if error_trace else ''
})
data['history'] = history[-50:]

if success:
    s_entry['consecutive_failures'] = 0
    s_entry['status'] = 'Closed'
    s_entry['total_successes'] = s_entry.get('total_successes', 0) + 1
    print('OK:0')
else:
    fails = s_entry.get('consecutive_failures', 0) + 1
    s_entry['consecutive_failures'] = fails
    s_entry['total_failures'] = s_entry.get('total_failures', 0) + 1
    if fails >= max_failures:
        s_entry['status'] = 'Tripped'
        print(f'TRIP:{fails}')
    else:
        s_entry['status'] = 'HalfOpen'
        print(f'FAIL:{fails}')

with open(reg_file, 'w') as f:
    json.dump(data, f, indent=2)
" "$HGB_CB_REGISTRY" "$skill_id" "$success" "$error_trace" "$pre_hash" "$post_hash" "$HGB_CB_MAX_FAILURES")

    local action="${result%%:*}"
    local fail_count="${result##*:}"

    if [ "$action" = "OK" ]; then
        log_ok "Mutation succeeded for [${skill_id}]. Consecutive failures reset to 0 (Circuit: Closed)."
        return 0
    elif [ "$action" = "TRIP" ]; then
        hgb_quarantine_skill "$skill_id" "$skill_path" "$error_trace" "$pre_hash" "$post_hash"
        return 2
    else
        log_warn "Recorded mutation failure for [${skill_id}] (${fail_count}/${HGB_CB_MAX_FAILURES}). Circuit state: HalfOpen."
        return 1
    fi
}

hgb_cb_reset_skill() {
    local skill_id="$1"
    local target_path="${2:-}"

    init_governor_environment
    python3 -c "
import json, sys, os
reg_file = sys.argv[1]
skill_id = sys.argv[2]
if os.path.exists(reg_file):
    try:
        data = json.load(open(reg_file))
        if skill_id in data.get('skills', {}):
            s = data['skills'][skill_id]
            s['status'] = 'Closed'
            s['consecutive_failures'] = 0
            s['is_quarantined'] = False
            s.pop('quarantine_reason', None)
            with open(reg_file, 'w') as f:
                json.dump(data, f, indent=2)
        print('RESET_OK')
    except Exception as e:
        print(f'RESET_ERR:{e}')
" "$HGB_CB_REGISTRY" "$skill_id"

    # Unlock file permissions if path provided
    if [ -n "$target_path" ] && [ -e "$target_path" ]; then
        chmod -R u+w "$target_path" 2>/dev/null || true
        log_ok "Write permissions restored on $target_path."
    fi
    log_ok "Skill [${skill_id}] circuit breaker manually reset by HITL operator."
}

# ------------------------------------------------------------------------------
# Self-Test Verification Mode
# ------------------------------------------------------------------------------
run_self_tests() {
    echo "========================================================================"
    echo "  Autopoietic Circuit-Breaker Governor: Unit & Integration Verification  "
    echo "========================================================================"

    local test_dir
    test_dir=$(mktemp -d /tmp/hgb_cb_test_XXXXXX)
    trap 'rm -rf "$test_dir"' EXIT

    export HGB_STATE_DIR="$test_dir/state"
    export HGB_QUARANTINE_DIR="$test_dir/quarantine"
    export HGB_CB_REGISTRY="$test_dir/state/cb_reg.json"
    export HGB_WEBHOOK_SINK="$test_dir/webhook_sink.jsonl"
    init_governor_environment

    echo "1. Testing Cryptographic State Hashing..."
    local dummy_file="$test_dir/source.rs"
    echo "fn main() { println!(\"original\"); }" > "$dummy_file"
    local hash1
    hash1=$(hgb_hash_state "$dummy_file")
    echo "   Original hash: $hash1"
    echo "fn main() { println!(\"mutated\"); }" > "$dummy_file"
    local hash2
    hash2=$(hgb_hash_state "$dummy_file")
    echo "   Mutated hash:  $hash2"
    if [ "$hash1" = "$hash2" ]; then
        log_err "Hash did not change after mutation!"
        exit 1
    fi
    log_ok "Cryptographic state hashing verified (pre != post)."

    echo "2. Testing Exponential Backoff Calculation..."
    local b1 b2 b3
    b1=$(hgb_calculate_backoff 1 2.0 64.0)
    b2=$(hgb_calculate_backoff 2 2.0 64.0)
    b3=$(hgb_calculate_backoff 3 2.0 64.0)
    echo "   Iter 1 backoff: ${b1}s, Iter 2: ${b2}s, Iter 3: ${b3}s"
    python3 -c "assert float('$b1') >= 2.0 and float('$b2') >= 4.0 and float('$b3') >= 8.0"
    log_ok "Exponential backoff calculation verified."

    echo "3. Testing 3-Failure Circuit Breaker Quarantine & Permission Lock..."
    local test_skill="skill_optimizer_test"
    local skill_src="$test_dir/skill_test.rs"
    echo "let mut v = 1;" > "$skill_src"

    set +e
    hgb_cb_record_attempt "$test_skill" "false" "syntax error iteration 1" "$hash1" "$hash2" "$skill_src"
    hgb_cb_check_skill "$test_skill"
    local chk1=$?
    if [ "$chk1" -ne 0 ]; then
        log_err "Skill should still be active on failure 1"
        exit 1
    fi

    hgb_cb_record_attempt "$test_skill" "false" "syntax error iteration 2" "$hash1" "$hash2" "$skill_src"
    hgb_cb_check_skill "$test_skill"
    local chk2=$?
    if [ "$chk2" -ne 0 ]; then
        log_err "Skill should still be active on failure 2"
        exit 1
    fi

    # Failure 3: Must Trip and Quarantine!
    hgb_cb_record_attempt "$test_skill" "false" "syntax error iteration 3: unresolved symbol" "$hash1" "$hash2" "$skill_src"
    hgb_cb_check_skill "$test_skill"
    local chk3=$?
    set -e

    if [ "$chk3" -eq 0 ]; then
        log_err "Circuit breaker failed to trip on 3 consecutive failures!"
        exit 1
    fi

    # Check file write permissions locked
    if [ -w "$skill_src" ]; then
        log_err "Write permissions were not stripped from failing skill!"
        exit 1
    fi

    # Check Webhook Sink
    if [ ! -s "$HGB_WEBHOOK_SINK" ]; then
        log_err "Webhook alert payload was not recorded in sink!"
        exit 1
    fi
    grep -q "quarantine_alert" "$HGB_WEBHOOK_SINK"
    grep -q "$test_skill" "$HGB_WEBHOOK_SINK"
    log_ok "Skill quarantine, permission lock, and webhook alerts verified!"

    echo "4. Testing HITL Administrative Reset..."
    hgb_cb_reset_skill "$test_skill" "$skill_src"
    hgb_cb_check_skill "$test_skill"
    if [ ! -w "$skill_src" ]; then
        log_err "Write permissions were not restored on reset!"
        exit 1
    fi
    log_ok "HITL administrative reset verified."

    echo "========================================================================"
    log_ok "All circuit-breaker governor tests passed with 100% success!"
    echo "========================================================================"
}

# ------------------------------------------------------------------------------
# Main Dispatcher
# ------------------------------------------------------------------------------
main() {
    local cmd="${1:-help}"
    case "$cmd" in
        check)
            hgb_cb_check_skill "${2:-}"
            ;;
        hash-state)
            hgb_hash_state "${2:-}"
            ;;
        calculate-backoff)
            hgb_calculate_backoff "${2:-1}" "${3:-$HGB_CB_T0}" "${4:-$HGB_CB_TMAX}"
            ;;
        apply-backoff)
            hgb_apply_backoff "${2:-unknown}" "${3:-1}"
            ;;
        record-failure)
            hgb_cb_record_attempt "${2:-unknown}" "false" "${3:-unknown error}" "${4:-none}" "${5:-none}" "${6:-}"
            ;;
        record-success)
            hgb_cb_record_attempt "${2:-unknown}" "true" "" "${3:-none}" "${4:-none}" "${5:-}"
            ;;
        quarantine)
            hgb_quarantine_skill "${2:-unknown}" "${3:-}" "${4:-manual quarantine}" "${5:-none}" "${6:-none}"
            ;;
        reset)
            hgb_cb_reset_skill "${2:-unknown}" "${3:-}"
            ;;
        send-webhook)
            hgb_send_webhook "${2:-quarantine_alert}" "${3:-unknown}" "${4:-trace}" "${5:-}" "${6:-}" "${7:-}"
            ;;
        --test|test)
            run_self_tests
            ;;
        help|--help|-h)
            cat <<EOF
Usage: circuit_breaker_governor.sh <command> [arguments...]

Commands:
  check <skill_id>                       Check if skill is allowed to mutate (exits 0 if OK, 1 if Tripped)
  hash-state <path>                      Compute BLAKE3 / SHA-256 state hash of file or directory
  calculate-backoff <iter> [t0] [tmax]   Compute exponential backoff duration in seconds
  apply-backoff <skill_id> [iter]        Sleep for calculated backoff dampening duration
  record-failure <skill_id> <err> [pre] [post] [path]  Record failed mutation attempt
  record-success <skill_id> [pre] [post] [path]        Record successful mutation (resets counter)
  quarantine <skill_id> <path> <reason>  Immediately lock permissions and quarantine skill
  reset <skill_id> [path]                HITL administrative reset (re-enables skill and write perms)
  send-webhook <event> <skill_id> <err> [pre] [post] [q_path]  Send JSON webhook alert
  --test                                 Run comprehensive integration test suite
EOF
            ;;
        *)
            log_err "Unknown command: $cmd"
            exit 1
            ;;
    esac
}

# If script is executed directly (not sourced), run main
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
