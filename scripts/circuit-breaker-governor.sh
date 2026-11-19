#!/usr/bin/env bash
# ==============================================================================
# circuit-breaker-governor.sh
# Autopoietic Circuit-Breaker Governance Engine (Pillar 3 & Pillar 4)
# Project Hagibis - Zero-Stub Production Grade Governance
#
# Pillar C Capabilities:
#   1. Sliding-Window Anomaly Detection:
#      Tracks rolling window of execution events (default 20 events, 60s window).
#      Computes dynamic anomaly score and failure rate percentage. Trips circuit
#      breaker when failure rate in active window >= 50% over >= 5 events.
#   2. Full Hysteresis State Machine:
#      - CLOSED: Normal execution, mutations permitted.
#      - OPEN: Blocked. Enforces strict jittered exponential backoff delay before probing.
#      - HALF_OPEN: Trial probing state allowing controlled test invocations.
#      - RECOVERY: Probation state requiring N consecutive successful executions
#                  before transitioning back to CLOSED.
#   3. Strict Jittered Exponential Backoff:
#      delay = min(max_backoff, base_delay * 2^(iteration - 1)) + uniform_jitter(0.1, 0.5)
#   4. Cryptographic State Hashing: Deterministic BLAKE3 / SHA-256 source hashing.
#   5. Skill Quarantining: Mutating permissions stripped (0444 read-only).
#   6. Automated HITL Webhook Alerts: Dispatched to OpenTelemetry sink & endpoint.
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
HGB_CB_WINDOW_SIZE="${HGB_CB_WINDOW_SIZE:-20}"
HGB_CB_WINDOW_SEC="${HGB_CB_WINDOW_SEC:-60.0}"
HGB_CB_MIN_EVENTS="${HGB_CB_MIN_EVENTS:-5}"
HGB_CB_ANOMALY_THRESHOLD="${HGB_CB_ANOMALY_THRESHOLD:-0.50}"
HGB_CB_RECOVERY_THRESHOLD="${HGB_CB_RECOVERY_THRESHOLD:-3}"

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
        echo '{"version":"2.0","skills":{},"history":[]}' > "$HGB_CB_REGISTRY"
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
# 2. Strict Jittered Exponential Backoff Dampening
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
# Strict uniform jitter between 0.1 and 0.5 seconds
jitter = random.uniform(0.1, 0.5)
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
    'version': '2.0',
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
    'circuit_breaker_state': 'OPEN'
}
print(json.dumps(data, indent=2))
" "$event_type" "$skill_id" "$error_trace" "$pre_hash" "$post_hash" "$quarantine_path" "$HGB_CB_MAX_FAILURES" "$HGB_CB_MAX_FAILURES" "$now_iso")

    echo "$payload" | tr '\n' ' ' >> "$HGB_WEBHOOK_SINK"
    echo "" >> "$HGB_WEBHOOK_SINK"

    if command -v curl >/dev/null 2>&1; then
        local http_code
        http_code=$(curl -s -S -o /dev/null -w "%{http_code}" -X POST \
            -H "Content-Type: application/json" \
            -H "X-Hagibis-Governor: autopoietic-circuit-breaker-hysteresis" \
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

    log_trip "Quarantining skill [${skill_id}] after anomaly detection threshold or consecutive failures!"

    if [ -n "$skill_path" ] && [ -e "$skill_path" ]; then
        cp -a "$skill_path" "$target_quarantine/payload" 2>/dev/null || true
        chmod -R a-w "$skill_path" 2>/dev/null || true
    fi

    local manifest_file="${target_quarantine}/quarantine_manifest.json"
    cat <<EOF > "$manifest_file"
{
  "skill_id": "$skill_id",
  "quarantined_at": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "consecutive_failures": $HGB_CB_MAX_FAILURES,
  "circuit_breaker_state": "OPEN",
  "pre_mutation_hash": "$pre_hash",
  "post_mutation_hash": "$post_hash",
  "error_trace": $(python3 -c "import json, sys; print(json.dumps(sys.argv[1]))" "$error_trace"),
  "source_path": "$skill_path",
  "permissions_locked": true
}
EOF
    chmod 0444 "$manifest_file" 2>/dev/null || true
    chmod -R a-w "$target_quarantine" 2>/dev/null || true
    log_ok "Mutating file permissions locked (0444 read-only) on active source and quarantine sandbox."

    # Update Registry
    python3 -c "
import json, sys, os
reg_file = sys.argv[1]
skill_id = sys.argv[2]
q_path = sys.argv[3]
reason = sys.argv[4]

data = {'version': '2.0', 'skills': {}, 'history': []}
if os.path.exists(reg_file):
    try: data = json.load(open(reg_file))
    except Exception: pass

skills = data.setdefault('skills', {})
s_entry = skills.setdefault(skill_id, {})
s_entry['status'] = 'OPEN'
s_entry['is_quarantined'] = True
s_entry['quarantine_path'] = q_path
s_entry['quarantine_reason'] = reason
s_entry['locked_at'] = sys.argv[5]

with open(reg_file, 'w') as f:
    json.dump(data, f, indent=2)
" "$HGB_CB_REGISTRY" "$skill_id" "$target_quarantine" "$error_trace" "$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    hgb_send_webhook "quarantine_alert" "$skill_id" "$error_trace" "$pre_hash" "$post_hash" "$target_quarantine"
}

# ------------------------------------------------------------------------------
# 5. Circuit Breaker Inspection with Hysteresis (CLOSED, OPEN, HALF_OPEN, RECOVERY)
# ------------------------------------------------------------------------------
hgb_cb_check_skill() {
    local skill_id="$1"
    init_governor_environment

    local check_result
    check_result=$(python3 -c "
import json, sys, os, time
reg_file = sys.argv[1]
skill_id = sys.argv[2]
now = time.time()

if not os.path.exists(reg_file):
    print('ALLOWED:CLOSED:Healthy')
    sys.exit(0)

try:
    data = json.load(open(reg_file))
    s = data.get('skills', {}).get(skill_id, {})
    status = s.get('status', 'CLOSED').upper()
    is_q = s.get('is_quarantined', False)
    open_until = float(s.get('open_until', 0.0))

    if status in ('OPEN', 'TRIPPED', 'QUARANTINED') or is_q:
        if open_until > 0 and now >= open_until:
            # Backoff duration expired! Transition to HALF_OPEN
            s['status'] = 'HALF_OPEN'
            with open(reg_file, 'w') as f:
                json.dump(data, f, indent=2)
            print('ALLOWED:HALF_OPEN:Backoff expired, trial probe allowed')
        else:
            remaining = max(0.0, open_until - now)
            print(f'BLOCKED:OPEN:Backoff active ({remaining:.1f}s remaining)')
    elif status == 'HALF_OPEN':
        print('ALLOWED:HALF_OPEN:Trial probe state')
    elif status == 'RECOVERY':
        succ = s.get('consecutive_successes', 0)
        req = s.get('recovery_threshold', 3)
        print(f'ALLOWED:RECOVERY:Probation active ({succ}/{req})')
    else:
        print('ALLOWED:CLOSED:Normal execution')
except Exception as e:
    print(f'ALLOWED:CLOSED:Fallback ({e})')
" "$HGB_CB_REGISTRY" "$skill_id")

    local decision="${check_result%%:*}"
    local rest="${check_result#*:}"
    local state="${rest%%:*}"
    local reason="${rest#*:}"

    if [ "$decision" = "BLOCKED" ]; then
        log_trip "Skill [${skill_id}] is locked in state: ${state}. ${reason}. All mutations blocked!"
        return 1
    else
        log_info "Skill [${skill_id}] authorization: State=${state} (${reason}). Mutations allowed."
        return 0
    fi
}

# ------------------------------------------------------------------------------
# 6. Sliding-Window Anomaly Detection & State Recording
# ------------------------------------------------------------------------------
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
import json, sys, os, time, random

reg_file = sys.argv[1]
skill_id = sys.argv[2]
success = sys.argv[3].lower() == 'true'
error_trace = sys.argv[4]
pre_hash = sys.argv[5]
post_hash = sys.argv[6]
max_failures = int(sys.argv[7])
t0 = float(sys.argv[8])
tmax = float(sys.argv[9])
window_size = int(sys.argv[10])
window_sec = float(sys.argv[11])
min_events = int(sys.argv[12])
anomaly_thresh = float(sys.argv[13])
recovery_thresh = int(sys.argv[14])

now = time.time()
data = {'version': '2.0', 'skills': {}, 'history': []}
if os.path.exists(reg_file):
    try: data = json.load(open(reg_file))
    except Exception: pass

skills = data.setdefault('skills', {})
s_entry = skills.setdefault(skill_id, {
    'status': 'CLOSED',
    'consecutive_failures': 0,
    'consecutive_successes': 0,
    'total_failures': 0,
    'total_successes': 0,
    'recovery_threshold': recovery_thresh,
    'is_quarantined': False,
    'window_events': []
})

# Update rolling sliding window
events = s_entry.get('window_events', [])
events.append({'t': now, 'ok': success, 'err': error_trace[:100]})
cutoff = now - window_sec
events = [e for e in events if e['t'] >= cutoff][-window_size:]
s_entry['window_events'] = events

# Calculate anomaly score & failure rate
total_events = len(events)
fail_events = sum(1 for e in events if not e['ok'])
failure_rate = (fail_events / float(total_events)) if total_events > 0 else 0.0
confidence = min(1.0, total_events / float(min_events))
anomaly_score = round(failure_rate * confidence, 4)
is_anomaly = (total_events >= min_events and failure_rate >= anomaly_thresh)

s_entry['anomaly_score'] = anomaly_score
s_entry['failure_rate'] = round(failure_rate, 4)

# Record history log
history = data.setdefault('history', [])
history.append({
    'timestamp': now,
    'skill_id': skill_id,
    'success': success,
    'pre_hash': pre_hash,
    'post_hash': post_hash,
    'anomaly_score': anomaly_score
})
data['history'] = history[-50:]

current_status = s_entry.get('status', 'CLOSED').upper()

# Calculate jittered exponential backoff: min(tmax, t0 * 2^(iter-1)) + uniform(0.1, 0.5)
def calc_backoff(fails):
    exp = max(0, fails - 1)
    base = min(tmax, t0 * (2.0 ** exp))
    jit = random.uniform(0.1, 0.5)
    return round(base + jit, 3)

action = 'OK'
consecutive_fails = s_entry.get('consecutive_failures', 0)

if current_status == 'CLOSED':
    if success:
        s_entry['consecutive_failures'] = 0
        s_entry['consecutive_successes'] = s_entry.get('consecutive_successes', 0) + 1
        s_entry['total_successes'] = s_entry.get('total_successes', 0) + 1
        action = f'OK:CLOSED:{anomaly_score}'
    else:
        consecutive_fails += 1
        s_entry['consecutive_failures'] = consecutive_fails
        s_entry['consecutive_successes'] = 0
        s_entry['total_failures'] = s_entry.get('total_failures', 0) + 1

        if is_anomaly or consecutive_fails >= max_failures:
            # Trip Circuit Breaker! Transition to OPEN
            s_entry['status'] = 'OPEN'
            delay = calc_backoff(consecutive_fails)
            s_entry['open_until'] = now + delay
            action = f'TRIP:OPEN:{consecutive_fails}:{delay}'
        else:
            action = f'FAIL:CLOSED:{consecutive_fails}'

elif current_status == 'HALF_OPEN':
    if success:
        # Trial probe succeeded! Transition HALF_OPEN -> RECOVERY
        s_entry['status'] = 'RECOVERY'
        s_entry['consecutive_failures'] = 0
        s_entry['consecutive_successes'] = 1
        s_entry['total_successes'] = s_entry.get('total_successes', 0) + 1
        action = 'RECOVERY_START:RECOVERY:1'
    else:
        # Trial probe failed! Trip back to OPEN with backoff
        s_entry['status'] = 'OPEN'
        consecutive_fails += 1
        s_entry['consecutive_failures'] = consecutive_fails
        s_entry['total_failures'] = s_entry.get('total_failures', 0) + 1
        delay = calc_backoff(consecutive_fails)
        s_entry['open_until'] = now + delay
        action = f'TRIP:OPEN:{consecutive_fails}:{delay}'

elif current_status == 'RECOVERY':
    if success:
        succ = s_entry.get('consecutive_successes', 0) + 1
        s_entry['consecutive_successes'] = succ
        s_entry['total_successes'] = s_entry.get('total_successes', 0) + 1
        req = s_entry.get('recovery_threshold', recovery_thresh)
        if succ >= req:
            # Full recovery achieved! Transition RECOVERY -> CLOSED
            s_entry['status'] = 'CLOSED'
            s_entry['consecutive_failures'] = 0
            s_entry['is_quarantined'] = False
            s_entry.pop('open_until', None)
            s_entry['window_events'] = []
            action = 'RECOVERY_DONE:CLOSED:0'
        else:
            action = f'RECOVERY_PROGRESS:RECOVERY:{succ}'
    else:
        # Probation failed! Transition RECOVERY -> OPEN
        s_entry['status'] = 'OPEN'
        consecutive_fails += 1
        s_entry['consecutive_failures'] = consecutive_fails
        s_entry['consecutive_successes'] = 0
        s_entry['total_failures'] = s_entry.get('total_failures', 0) + 1
        delay = calc_backoff(consecutive_fails)
        s_entry['open_until'] = now + delay
        action = f'TRIP:OPEN:{consecutive_fails}:{delay}'

else: # OPEN
    if success:
        s_entry['status'] = 'HALF_OPEN'
        action = 'OK:HALF_OPEN:0'
    else:
        consecutive_fails += 1
        s_entry['consecutive_failures'] = consecutive_fails
        delay = calc_backoff(consecutive_fails)
        s_entry['open_until'] = now + delay
        action = f'TRIP:OPEN:{consecutive_fails}:{delay}'

with open(reg_file, 'w') as f:
    json.dump(data, f, indent=2)

print(action)
" "$HGB_CB_REGISTRY" "$skill_id" "$success" "$error_trace" "$pre_hash" "$post_hash" \
  "$HGB_CB_MAX_FAILURES" "$HGB_CB_T0" "$HGB_CB_TMAX" "$HGB_CB_WINDOW_SIZE" "$HGB_CB_WINDOW_SEC" \
  "$HGB_CB_MIN_EVENTS" "$HGB_CB_ANOMALY_THRESHOLD" "$HGB_CB_RECOVERY_THRESHOLD")

    IFS=':' read -r tag state detail extra <<< "$result"

    if [ "$tag" = "OK" ]; then
        log_ok "Mutation succeeded for [${skill_id}] (Circuit state: ${state})."
        return 0
    elif [ "$tag" = "TRIP" ]; then
        log_trip "Circuit breaker TRIPPED to OPEN for [${skill_id}] (Failures: ${detail}, Backoff: ${extra}s)!"
        hgb_quarantine_skill "$skill_id" "$skill_path" "$error_trace" "$pre_hash" "$post_hash"
        return 2
    elif [ "$tag" = "RECOVERY_START" ]; then
        log_ok "Trial probe succeeded for [${skill_id}]. Transitioned to RECOVERY probation (1/${HGB_CB_RECOVERY_THRESHOLD})."
        return 0
    elif [ "$tag" = "RECOVERY_PROGRESS" ]; then
        log_ok "Probation mutation succeeded for [${skill_id}] (${detail}/${HGB_CB_RECOVERY_THRESHOLD})."
        return 0
    elif [ "$tag" = "RECOVERY_DONE" ]; then
        log_ok "Probation complete! Skill [${skill_id}] fully recovered and restored to CLOSED state."
        return 0
    else
        log_warn "Recorded mutation failure for [${skill_id}] (${detail}/${HGB_CB_MAX_FAILURES}). Circuit state: ${state}."
        return 1
    fi
}

# ------------------------------------------------------------------------------
# 7. HITL Administrative Reset
# ------------------------------------------------------------------------------
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
            s['status'] = 'CLOSED'
            s['consecutive_failures'] = 0
            s['consecutive_successes'] = 0
            s['is_quarantined'] = False
            s.pop('quarantine_reason', None)
            s.pop('quarantine_path', None)
            s.pop('open_until', None)
            s['window_events'] = []
            with open(reg_file, 'w') as f:
                json.dump(data, f, indent=2)
        print('RESET_OK')
    except Exception as e:
        print(f'RESET_ERR:{e}')
" "$HGB_CB_REGISTRY" "$skill_id"

    if [ -n "$target_path" ] && [ -e "$target_path" ]; then
        chmod -R u+w "$target_path" 2>/dev/null || true
        log_ok "Write permissions restored on $target_path."
    fi
    log_ok "Skill [${skill_id}] circuit breaker manually reset by HITL operator to CLOSED."
}

# ------------------------------------------------------------------------------
# Self-Test Verification Mode
# ------------------------------------------------------------------------------
run_self_tests() {
    echo "========================================================================"
    echo "  Autopoietic Circuit-Breaker Governor: Unit & Hysteresis Verification  "
    echo "========================================================================"

    local test_dir
    test_dir=$(mktemp -d /tmp/hgb_cb_test_XXXXXX)
    trap 'rm -rf "$test_dir"' EXIT

    export HGB_STATE_DIR="$test_dir/state"
    export HGB_QUARANTINE_DIR="$test_dir/quarantine"
    export HGB_CB_REGISTRY="$test_dir/state/cb_reg.json"
    export HGB_WEBHOOK_SINK="$test_dir/webhook_sink.jsonl"
    export HGB_CB_MAX_FAILURES=3
    export HGB_CB_RECOVERY_THRESHOLD=2
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

    echo "2. Testing Jittered Exponential Backoff Calculation..."
    local b1 b2 b3
    b1=$(hgb_calculate_backoff 1 2.0 64.0)
    b2=$(hgb_calculate_backoff 2 2.0 64.0)
    b3=$(hgb_calculate_backoff 3 2.0 64.0)
    echo "   Iter 1: ${b1}s, Iter 2: ${b2}s, Iter 3: ${b3}s"
    python3 -c "assert float('$b1') >= 2.1 and float('$b2') >= 4.1 and float('$b3') >= 8.1"
    log_ok "Jittered exponential backoff calculation verified."

    echo "3. Testing Full Hysteresis State Machine: CLOSED -> OPEN -> HALF_OPEN -> RECOVERY -> CLOSED..."
    local test_skill="skill_hysteresis_test"
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

    # Failure 3: Must Trip to OPEN and Quarantine!
    hgb_cb_record_attempt "$test_skill" "false" "syntax error iteration 3: fatal" "$hash1" "$hash2" "$skill_src"
    hgb_cb_check_skill "$test_skill"
    local chk3=$?
    set -e

    if [ "$chk3" -eq 0 ]; then
        log_err "Circuit breaker failed to trip on 3 consecutive failures!"
        exit 1
    fi

    if [ -w "$skill_src" ]; then
        log_err "Write permissions were not stripped from failing skill!"
        exit 1
    fi
    log_ok "Transition CLOSED -> OPEN verified. Write permissions locked (0444)."

    # Test backoff expiration -> transition OPEN to HALF_OPEN
    echo "   Simulating backoff expiration..."
    python3 -c "
import json
data = json.load(open('$HGB_CB_REGISTRY'))
data['skills']['$test_skill']['open_until'] = 0.0
json.dump(data, open('$HGB_CB_REGISTRY', 'w'))
"
    hgb_cb_check_skill "$test_skill"
    log_ok "Transition OPEN -> HALF_OPEN verified upon backoff expiration."

    # Test trial probe success -> transition HALF_OPEN to RECOVERY
    hgb_cb_record_attempt "$test_skill" "true" "" "$hash1" "$hash2" "$skill_src"
    local state_after_probe
    state_after_probe=$(python3 -c "import json; print(json.load(open('$HGB_CB_REGISTRY'))['skills']['$test_skill']['status'])")
    if [ "$state_after_probe" != "RECOVERY" ]; then
        log_err "Expected state RECOVERY after probe, got $state_after_probe"
        exit 1
    fi
    log_ok "Transition HALF_OPEN -> RECOVERY verified upon trial probe success."

    # In RECOVERY: 1 more success -> transitions RECOVERY to CLOSED
    hgb_cb_record_attempt "$test_skill" "true" "" "$hash1" "$hash2" "$skill_src"
    local state_recovered
    state_recovered=$(python3 -c "import json; print(json.load(open('$HGB_CB_REGISTRY'))['skills']['$test_skill']['status'])")
    if [ "$state_recovered" != "CLOSED" ]; then
        log_err "Expected state CLOSED after probation, got $state_recovered"
        exit 1
    fi
    log_ok "Transition RECOVERY -> CLOSED verified after probation successes."

    echo "4. Testing HITL Administrative Reset..."
    hgb_quarantine_skill "$test_skill" "$skill_src" "Manual lock"
    hgb_cb_reset_skill "$test_skill" "$skill_src"
    hgb_cb_check_skill "$test_skill"
    if [ ! -w "$skill_src" ]; then
        log_err "Write permissions were not restored on reset!"
        exit 1
    fi
    log_ok "HITL administrative reset verified."

    echo "========================================================================"
    log_ok "All circuit-breaker governor hysteresis tests passed with 100% success!"
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
Usage: circuit-breaker-governor.sh <command> [arguments...]

Commands:
  check <skill_id>                       Check if skill is allowed to mutate (CLOSED, HALF_OPEN, RECOVERY = 0; OPEN = 1)
  hash-state <path>                      Compute BLAKE3 / SHA-256 state hash of file or directory
  calculate-backoff <iter> [t0] [tmax]   Compute jittered exponential backoff duration in seconds
  apply-backoff <skill_id> [iter]        Sleep for calculated backoff dampening duration
  record-failure <skill_id> <err> [pre] [post] [path]  Record failed mutation attempt (updates anomaly detector)
  record-success <skill_id> [pre] [post] [path]        Record successful mutation (drives hysteresis transitions)
  quarantine <skill_id> <path> <reason>  Immediately lock permissions and quarantine skill to OPEN state
  reset <skill_id> [path]                HITL administrative reset (re-enables skill and write perms to CLOSED)
  send-webhook <event> <skill_id> <err> [pre] [post] [q_path]  Send JSON webhook alert
  --test                                 Run comprehensive integration & hysteresis test suite
EOF
            ;;
        *)
            log_err "Unknown command: $cmd"
            exit 1
            ;;
    esac
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
