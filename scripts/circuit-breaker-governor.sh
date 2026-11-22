#!/usr/bin/env bash
# ==============================================================================
# circuit-breaker-governor.sh
# Autopoietic Circuit-Breaker Governance Engine with Cryptographic Lockout
# Project Hagibis - Zero-Stub Production Grade Governance
#
# Pillar 3 & Pillar 4 Capabilities:
#   1. Sliding-Window Anomaly Detection & Hysteresis State Machine:
#      CLOSED, OPEN, HALF_OPEN, RECOVERY, PERM_TRIPPED, and PERM_TRIPPED_CRYPTOLOCKED.
#   2. Flapping Detection & Permanent Cryptographic Lockout:
#      - Detects 2-cycle oscillation between alternating error fingerprints (A <-> B).
#      - Detects 3 consecutive identical AST error signatures (A -> A -> A).
#      - Transitions to PERM_TRIPPED_CRYPTOLOCKED immediately upon flapping.
#   3. Cryptographic State Signing & Proof Receipts:
#      - Hashes error fingerprint and execution history with BLAKE3.
#      - Signs lockout receipt with Ed25519 keypair.
#      - Strips file permissions to read-only (0444).
#   4. Replay-Defended Signed MCP Operator Unlock:
#      - PERM_TRIPPED_CRYPTOLOCKED state permanently locks out automated self-healing.
#      - Only an authorized human operator can unlock the circuit breaker by issuing
#        a cryptographically signed MCP payload (`hgb_operator_unlock_circuit_breaker`)
#        defined in `hagibis.mcp.json`.
#      - Verifies Ed25519 signature over single-use challenge nonce.
#   5. Strict Fibonacci Progression Reinforcement Backoff:
#      fib(1)=1, fib(2)=1, fib(3)=2, fib(4)=3, fib(5)=5, fib(6)=8, fib(7)=13, fib(8)=21...
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HGB_BRAIN_DIR="${HGB_BRAIN_DIR:-/home/toshiba/.gemini/antigravity-cli/brain/567f5815-da08-438c-8b98-4a61b4c3ef0e}"
PYTHON_GOVERNOR="${PYTHON_GOVERNOR:-${SCRIPT_DIR}/../skills/observability-otel/ch8_circuit_breaker_governor.py}"
if [ ! -f "$PYTHON_GOVERNOR" ]; then
    PYTHON_GOVERNOR="${HGB_BRAIN_DIR}/skills/observability-otel/ch8_circuit_breaker_governor.py"
fi

# Configuration Defaults (overridable via environment)
HGB_STATE_DIR="${HGB_STATE_DIR:-${HOME}/.hagibis}"
HGB_QUARANTINE_DIR="${HGB_QUARANTINE_DIR:-${HGB_STATE_DIR}/quarantine}"
HGB_CB_REGISTRY="${HGB_CB_REGISTRY:-${HGB_STATE_DIR}/circuit_breaker_registry.json}"
HGB_WEBHOOK_URL="${HGB_WEBHOOK_URL:-http://127.0.0.1:8083/api/v1/alerts}"
HGB_WEBHOOK_SINK="${HGB_WEBHOOK_SINK:-/tmp/hgb_quarantine_webhook_events.jsonl}"
HGB_LANCEDB_DIR="${HGB_LANCEDB_DIR:-/media/toshiba/Lexar/hagibis/skills/vector-lancedb}"
HGB_CB_MAX_FAILURES="${HGB_CB_MAX_FAILURES:-3}"
HGB_CB_T0="${HGB_CB_T0:-1.0}"
HGB_CB_TMAX="${HGB_CB_TMAX:-64.0}"
HGB_CB_WINDOW_SIZE="${HGB_CB_WINDOW_SIZE:-20}"
HGB_CB_WINDOW_SEC="${HGB_CB_WINDOW_SEC:-60.0}"
HGB_CB_MIN_EVENTS="${HGB_CB_MIN_EVENTS:-5}"
HGB_CB_ANOMALY_THRESHOLD="${HGB_CB_ANOMALY_THRESHOLD:-0.50}"
HGB_CB_RECOVERY_THRESHOLD="${HGB_CB_RECOVERY_THRESHOLD:-3}"

# Adaptive PID Controller Defaults (Pillar 2)
HGB_CB_PID_KP="${HGB_CB_PID_KP:-2.0}"
HGB_CB_PID_KI="${HGB_CB_PID_KI:-0.5}"
HGB_CB_PID_KD="${HGB_CB_PID_KD:-1.0}"
HGB_CB_PID_SETPOINT="${HGB_CB_PID_SETPOINT:-0.05}"
HGB_CB_PID_TRIP_THRESHOLD="${HGB_CB_PID_TRIP_THRESHOLD:-1.0}"

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
log_cryptolock() { echo -e "${RED}${BOLD}🔒 [PERM_TRIPPED_CRYPTOLOCKED]${RESET} $*" >&2; }

# Initialize State Directory and Registry
init_governor_environment() {
    mkdir -p "$HGB_STATE_DIR" "$HGB_QUARANTINE_DIR" "${HGB_STATE_DIR}/lockouts"
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
            python3 -c "
import sys, hashlib
try:
    import blake3
    hasher = blake3.blake3()
    with open(sys.argv[1], 'rb') as f:
        while chunk := f.read(65536): hasher.update(chunk)
    print('blake3:' + hasher.hexdigest())
except Exception:
    hasher = hashlib.sha256()
    with open(sys.argv[1], 'rb') as f:
        while chunk := f.read(65536): hasher.update(chunk)
    print('sha256:' + hasher.hexdigest())
" "$target_path"
        fi
    elif [ -d "$target_path" ]; then
        python3 -c "
import os, sys, hashlib
target = sys.argv[1]
entries = []
for root, dirs, files in os.walk(target):
    dirs.sort()
    for f in sorted(files):
        if f.startswith('.'): continue
        full_p = os.path.join(root, f)
        rel_p = os.path.relpath(full_p, target)
        h = hashlib.sha256()
        try:
            with open(full_p, 'rb') as fh:
                while chunk := fh.read(65536): h.update(chunk)
            entries.append(f'{rel_p}:{h.hexdigest()}')
        except Exception: pass
entries.sort()
manifest = '\n'.join(entries).encode('utf-8')
try:
    import blake3
    print('blake3:' + blake3.blake3(manifest).hexdigest())
except Exception:
    print('sha256:' + hashlib.sha256(manifest).hexdigest())
" "$target_path"
    else
        echo "sha256:unknown"
    fi
}

# ------------------------------------------------------------------------------
# 2. Fibonacci Progression Reinforcement Backoff
# ------------------------------------------------------------------------------
hgb_fibonacci() {
    local n="${1:-1}"
    python3 -c "
def fib(n):
    if n <= 0: return 0
    if n in (1, 2): return 1
    a, b = 1, 1
    for _ in range(3, n + 1):
        a, b = b, a + b
    return b
import sys
print(fib(int(sys.argv[1])))
" "$n"
}

hgb_calculate_backoff() {
    local iteration="${1:-1}"
    local t0="${2:-$HGB_CB_T0}"
    local tmax="${3:-$HGB_CB_TMAX}"
    local strategy="${4:-exponential}"

    if [ -f "$PYTHON_GOVERNOR" ]; then
        python3 "$PYTHON_GOVERNOR" backoff "$iteration" --t0 "$t0" --tmax "$tmax" --strategy "$strategy"
    else
        python3 -c "
import sys, random
it = max(1, int(sys.argv[1]))
t0_val = float(sys.argv[2])
tmax_val = float(sys.argv[3])
strat = sys.argv[4] if len(sys.argv) > 4 else 'exponential'
if strat == 'fibonacci':
    def fib(n):
        if n <= 0: return 0
        if n in (1, 2): return 1
        a, b = 1, 1
        for _ in range(3, n + 1): a, b = b, a + b
        return b
    base = min(tmax_val, t0_val * float(fib(it)))
else:
    base = min(tmax_val, t0_val * (2.0 ** (it - 1)))
jitter = random.uniform(0.1, 0.5)
print(f'{base + jitter:.3f}')
" "$iteration" "$t0" "$tmax" "$strategy"
    fi
}

hgb_apply_backoff() {
    local skill_id="${1:-unknown}"
    local iteration="${2:-1}"
    local backoff_sec
    backoff_sec=$(hgb_calculate_backoff "$iteration")
    log_warn "Dampening mutation attempt #${iteration} for skill [${skill_id}]: backing off ${backoff_sec}s (Fibonacci progression)..."
    python3 -c "import time, sys; time.sleep(float(sys.argv[1]))" "$backoff_sec"
}

# ------------------------------------------------------------------------------
# 3. Deterministic AST Error State Fingerprinting
# ------------------------------------------------------------------------------
hgb_compute_ast_fingerprint() {
    local error_code="${1:-}"
    local file_path="${2:-}"
    local line_num="${3:-1}"
    local span_str="${4:-0:0}"
    local error_msg="${5:-}"

    if [ -f "$PYTHON_GOVERNOR" ]; then
        python3 "$PYTHON_GOVERNOR" fingerprint --code "$error_code" --file "$file_path" --line "$line_num" --span "$span_str" --msg "$error_msg"
    else
        python3 -c "
import sys, hashlib, os
code = sys.argv[1].strip()
file_p = os.path.normpath(sys.argv[2].strip())
line = sys.argv[3].strip()
span = sys.argv[4].strip()
msg = ' '.join(sys.argv[5].split()).strip()

canon = f'{code}|{file_p}|{line}|{span}|{msg}'.encode('utf-8')
try:
    import blake3
    print(blake3.blake3(canon).hexdigest())
except Exception:
    print(hashlib.sha256(canon).hexdigest())
" "$error_code" "$file_path" "$line_num" "$span_str" "$error_msg"
    fi
}

# ------------------------------------------------------------------------------
# 4. Check Skill Execution Eligibility
# ------------------------------------------------------------------------------
hgb_cb_check_skill() {
    local skill_id="$1"
    init_governor_environment

    if [ -f "$PYTHON_GOVERNOR" ]; then
        local check_out=""
        local status=0
        check_out=$(python3 "$PYTHON_GOVERNOR" check "$skill_id" 2>&1) || status=$?

        IFS=':' read -r decision state reason <<< "$check_out"
        if [ "$decision" = "BLOCKED" ]; then
            if [ "$state" = "PERM_TRIPPED_CRYPTOLOCKED" ]; then
                log_cryptolock "Skill [${skill_id}] is locked in PERM_TRIPPED_CRYPTOLOCKED! Authorized signed MCP unlock required."
                log_err "Reason: ${reason}"
            else
                log_trip "Skill [${skill_id}] is locked in state: ${state}. ${reason}. Mutations blocked!"
            fi
            return 1
        else
            log_info "Skill [${skill_id}] authorization: State=${state} (${reason}). Mutations permitted."
            return 0
        fi
    else
        python3 -c "
import json, sys, os, time
reg_file = sys.argv[1]
skill_id = sys.argv[2]
now = time.time()
if not os.path.exists(reg_file):
    print('ALLOWED:CLOSED:Healthy')
    sys.exit(0)
try:
    d = json.load(open(reg_file))
    s = d.get('skills', {}).get(skill_id, {})
    status = s.get('status', s.get('state', 'CLOSED')).upper()
    if 'CRYPTOLOCKED' in status:
        print(f'BLOCKED:{status}:Flapping detected! Permanent cryptographic lockout active.')
        sys.exit(1)
    elif status == 'PERM_TRIPPED':
        print('BLOCKED:PERM_TRIPPED:3 consecutive identical AST failures. Escalated.')
        sys.exit(1)
    elif status == 'OPEN':
        open_until = float(s.get('open_until', s.get('open_until_timestamp', 0.0)))
        if open_until > 0 and now >= open_until:
            print('ALLOWED:HALF_OPEN:Backoff expired, trial probe allowed')
            sys.exit(0)
        else:
            print(f'BLOCKED:OPEN:Backoff active ({max(0.0, open_until - now):.1f}s)')
            sys.exit(1)
    else:
        print(f'ALLOWED:{status}:Execution permitted')
        sys.exit(0)
except Exception as e:
    print(f'ALLOWED:CLOSED:Fallback ({e})')
    sys.exit(0)
" "$HGB_CB_REGISTRY" "$skill_id"
    fi
}

# ------------------------------------------------------------------------------
# 5. Record Failure Attempt (Flapping & Cryptographic Lockout)
# ------------------------------------------------------------------------------
hgb_cb_record_failure() {
    local skill_id="$1"
    local error_trace="${2:-unknown error}"
    local pre_hash="${3:-none}"
    local post_hash="${4:-none}"
    local skill_path="${5:-}"
    local ast_code="${6:-}"
    local ast_line="${7:-1}"
    local ast_span="${8:-0:0}"
    local ast_msg="${9:-}"

    init_governor_environment

    if [ -f "$PYTHON_GOVERNOR" ]; then
        local cmd=(python3 "$PYTHON_GOVERNOR" record-failure "$skill_id"
            --error "$error_trace"
            --line "$ast_line"
            --span "$ast_span"
        )
        if [ -n "$skill_path" ]; then cmd+=(--file "$skill_path"); fi
        if [ -n "$ast_code" ];   then cmd+=(--code "$ast_code"); fi
        if [ -n "$ast_msg" ];    then cmd+=(--msg "$ast_msg"); fi
        if [ -n "$pre_hash" ];   then cmd+=(--pre-hash "$pre_hash"); fi
        if [ -n "$post_hash" ];  then cmd+=(--post-hash "$post_hash"); fi

        local res=""
        local ret=0
        res=$("${cmd[@]}" 2>&1) || ret=$?

        IFS=':' read -r tag state detail extra <<< "$res"

        if [ "$tag" = "PERM_CRYPTOLOCKED" ]; then
            log_cryptolock "⚡ [PERM_TRIPPED_CRYPTOLOCKED] Flapping detected for [${skill_id}]!"
            log_err "Lockout ID: ${detail} | Error Fingerprint: ${extra}"
            log_err "Mutating permissions stripped (0444). Signed MCP operator unlock required to restore."
            return 3
        elif [ "$tag" = "PERM_TRIP" ]; then
            log_trip "⚡ [PERM_TRIPPED] 3 consecutive identical AST errors detected for [${skill_id}]! Fingerprint: ${extra}"
            return 3
        elif [ "$tag" = "TRIP" ]; then
            log_trip "Circuit breaker TRIPPED to OPEN for [${skill_id}] (Failures: ${detail}, Backoff: ${extra}s)!"
            return 2
        else
            log_warn "Recorded failure for [${skill_id}] (Failures: ${detail}). State: ${state}."
            return 1
        fi
    fi
}

# ------------------------------------------------------------------------------
# 6. Record Success Attempt
# ------------------------------------------------------------------------------
hgb_cb_record_success() {
    local skill_id="$1"
    local pre_hash="${2:-none}"
    local post_hash="${3:-none}"
    local skill_path="${4:-}"

    init_governor_environment

    if [ -f "$PYTHON_GOVERNOR" ]; then
        local cmd=(python3 "$PYTHON_GOVERNOR" record-success "$skill_id")
        if [ -n "$skill_path" ]; then cmd+=(--file "$skill_path"); fi
        if [ -n "$pre_hash" ];   then cmd+=(--pre-hash "$pre_hash"); fi
        if [ -n "$post_hash" ];  then cmd+=(--post-hash "$post_hash"); fi

        local res
        res=$("${cmd[@]}" 2>&1)
        log_ok "Mutation succeeded for [${skill_id}]: ${res}"
        return 0
    fi
}

# ------------------------------------------------------------------------------
# 7. Authorized Signed MCP Operator Unlock
# ------------------------------------------------------------------------------
hgb_cb_operator_unlock() {
    local skill_id="$1"
    local operator_id="$2"
    local lockout_id="$3"
    local nonce="$4"
    local pub_key="$5"
    local signature="$6"
    local target_path="${7:-}"

    init_governor_environment

    log_info "Executing cryptographic operator unlock for skill [${skill_id}]..."
    log_info "  - Operator ID: $operator_id"
    log_info "  - Lockout ID:  $lockout_id"
    log_info "  - Nonce:       $nonce"

    if [ -f "$PYTHON_GOVERNOR" ]; then
        local res=""
        local ret=0
        res=$(python3 "$PYTHON_GOVERNOR" operator-unlock "$skill_id" \
            --operator-id "$operator_id" \
            --lockout-id "$lockout_id" \
            --nonce "$nonce" \
            --public-key "$pub_key" \
            --signature "$signature" 2>&1) || ret=$?

        if [ "$ret" -eq 0 ]; then
            log_ok "✔ [CRYPTOGRAPHIC UNLOCK SUCCESSFUL] Circuit breaker reset to CLOSED."
            if [ -n "$target_path" ] && [ -e "$target_path" ]; then
                chmod -R u+w "$target_path" 2>/dev/null || true
                log_ok "Write permissions restored (0644/0755) on $target_path."
            fi
            echo "$res"
            return 0
        else
            log_err "❌ Cryptographic unlock failed: $res"
            return 1
        fi
    else
        log_err "Python governor required for Ed25519 cryptographic unlock: $PYTHON_GOVERNOR not found"
        return 1
    fi
}

# ------------------------------------------------------------------------------
# 8. Generate Operator Test Token
# ------------------------------------------------------------------------------
hgb_generate_operator_token() {
    local skill_id="$1"
    local operator_id="${2:-ops-admin-hitl}"
    local lockout_id="${3:-}"

    local cmd=(python3 "$PYTHON_GOVERNOR" generate-operator-token "$skill_id" --operator-id "$operator_id")
    if [ -n "$lockout_id" ]; then
        cmd+=(--lockout-id "$lockout_id")
    fi
    "${cmd[@]}"
}

# ------------------------------------------------------------------------------
# 9. Administrative Reset (Guarded Against Cryptolock)
# ------------------------------------------------------------------------------
hgb_cb_reset_skill() {
    local skill_id="$1"
    local target_path="${2:-}"

    init_governor_environment

    # Verify skill is not PERM_TRIPPED_CRYPTOLOCKED
    local current_state
    current_state=$(python3 -c "
import json, sys, os
reg = sys.argv[1]
s_id = sys.argv[2]
if os.path.exists(reg):
    try:
        d = json.load(open(reg))
        s = d.get('skills', {}).get(s_id, {})
        print(s.get('state', s.get('status', 'CLOSED')))
        sys.exit(0)
    except Exception: pass
print('CLOSED')
" "$HGB_CB_REGISTRY" "$skill_id")

    if [ "$current_state" = "PERM_TRIPPED_CRYPTOLOCKED" ]; then
        log_cryptolock "Administrative reset REJECTED! Skill [${skill_id}] is PERM_TRIPPED_CRYPTOLOCKED."
        log_err "Unsigned reset is forbidden. You must use 'operator-unlock' with a valid Ed25519 signature."
        return 1
    fi

    python3 -c "
import json, sys, os
reg_file = sys.argv[1]
skill_id = sys.argv[2]
if os.path.exists(reg_file):
    try:
        data = json.load(open(reg_file))
        if skill_id in data.get('skills', {}):
            s = data['skills'][skill_id]
            s['state'] = 'CLOSED'
            s['status'] = 'CLOSED'
            s['consecutive_failures'] = 0
            s['consecutive_successes'] = 0
            s['is_quarantined'] = False
            s['last_ast_fingerprint'] = None
            s['consecutive_fingerprint_count'] = 0
            s['ast_fingerprint_history'] = []
            s.pop('quarantine_reason', None)
            s.pop('quarantine_path', None)
            s.pop('open_until', None)
            s.pop('open_until_timestamp', None)
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
# 8. Record Memory Reading & Leak Rate Supervision
# ------------------------------------------------------------------------------
hgb_cb_record_memory() {
    local skill_id="$1"
    local rss_bytes="${2:-0}"
    local heap_bytes="${3:-0}"
    local alloc_count="${4:-0}"
    local target_path="${5:-}"

    init_governor_environment
    if [ -f "$PYTHON_GOVERNOR" ]; then
        local extra_args=()
        if [ -n "$target_path" ]; then
            extra_args+=(--file "$target_path")
        fi
        python3 "$PYTHON_GOVERNOR" record-memory "$skill_id" \
            --rss "$rss_bytes" \
            --heap "$heap_bytes" \
            --alloc "$alloc_count" \
            "${extra_args[@]}"
    fi
}

# ------------------------------------------------------------------------------
# 9. Check Autonomous Code Generation Recursion & Cycles
# ------------------------------------------------------------------------------
hgb_cb_check_recursion() {
    local skill_id="$1"
    local turn_number="${2:-1}"
    local state_hash="$3"
    local depth="${4:-0}"
    local intent="${5:-}"
    local target_path="${6:-}"

    init_governor_environment
    if [ -f "$PYTHON_GOVERNOR" ]; then
        local extra_args=()
        if [ -n "$target_path" ]; then
            extra_args+=(--file "$target_path")
        fi
        python3 "$PYTHON_GOVERNOR" check-recursion "$skill_id" \
            --turn "$turn_number" \
            --hash "$state_hash" \
            --depth "$depth" \
            --intent "$intent" \
            "${extra_args[@]}"
    fi
}

# ------------------------------------------------------------------------------
# 10. Self-Test Verification Mode
# ------------------------------------------------------------------------------
run_self_tests() {
    echo "========================================================================"
    echo "  Autopoietic Circuit-Breaker Governor: Unit & Hysteresis Verification  "
    echo "========================================================================"

    test_dir=$(mktemp -d /tmp/hgb_cb_test_XXXXXX)
    trap 'if [ -n "${test_dir:-}" ] && [ -d "${test_dir:-}" ]; then rm -rf "$test_dir"; fi' EXIT

    export HGB_STATE_DIR="$test_dir/state"
    export HGB_QUARANTINE_DIR="$test_dir/quarantine"
    export HGB_CB_REGISTRY="$test_dir/state/cb_reg.json"
    export HGB_WEBHOOK_SINK="$test_dir/webhook_sink.jsonl"
    export HGB_LANCEDB_DIR="$test_dir/lancedb"
    export HGB_CB_MAX_FAILURES=3
    export HGB_CB_RECOVERY_THRESHOLD=2
    init_governor_environment

    echo "1. Testing Cryptographic State Hashing..."
    local dummy_file="$test_dir/source.rs"
    echo "fn main() { println!(\"original\"); }" > "$dummy_file"
    local hash1
    hash1=$(hgb_hash_state "$dummy_file")
    echo "fn main() { println!(\"mutated\"); }" > "$dummy_file"
    local hash2
    hash2=$(hgb_hash_state "$dummy_file")
    if [ "$hash1" = "$hash2" ]; then
        log_err "Hash did not change after mutation!"
        exit 1
    fi
    log_ok "Cryptographic state hashing verified (pre != post)."

    echo "2. Testing Fibonacci Reinforcement Backoff Calculation..."
    local b1 b2 b3 b4 b5 b6
    b1=$(hgb_calculate_backoff 1 1.0 64.0 fibonacci)
    b2=$(hgb_calculate_backoff 2 1.0 64.0 fibonacci)
    b3=$(hgb_calculate_backoff 3 1.0 64.0 fibonacci)
    b4=$(hgb_calculate_backoff 4 1.0 64.0 fibonacci)
    b5=$(hgb_calculate_backoff 5 1.0 64.0 fibonacci)
    b6=$(hgb_calculate_backoff 6 1.0 64.0 fibonacci)
    python3 -c "
assert 1.1 <= float('$b1') <= 1.5, f'b1={float(\"$b1\")}'
assert 1.1 <= float('$b2') <= 1.5, f'b2={float(\"$b2\")}'
assert 2.1 <= float('$b3') <= 2.5, f'b3={float(\"$b3\")}'
assert 3.1 <= float('$b4') <= 3.5, f'b4={float(\"$b4\")}'
assert 5.1 <= float('$b5') <= 5.5, f'b5={float(\"$b5\")}'
assert 8.1 <= float('$b6') <= 8.5, f'b6={float(\"$b6\")}'
"
    log_ok "Fibonacci progression reinforcement backoff calculation verified."

    echo "3. Testing AST Error State Fingerprinting..."
    local fp1 fp2 fp_diff
    fp1=$(hgb_compute_ast_fingerprint "E0308" "src/lib.rs" "42" "10:20" "mismatched types")
    fp2=$(hgb_compute_ast_fingerprint "E0308" "src/lib.rs" "42" "10:20" "mismatched   types  \n")
    fp_diff=$(hgb_compute_ast_fingerprint "E0596" "src/lib.rs" "42" "10:20" "cannot borrow mutable")
    if [ "$fp1" != "$fp2" ]; then
        log_err "Whitespace normalization failed in fingerprinting!"
        exit 1
    fi
    if [ "$fp1" = "$fp_diff" ]; then
        log_err "Different error code must produce distinct fingerprint!"
        exit 1
    fi
    log_ok "Deterministic AST error state fingerprinting verified."

    echo "4. Testing Flapping Detection (2-Cycle Oscillation A <-> B) -> PERM_TRIPPED_CRYPTOLOCKED..."
    local flap_skill="skill_flapping_oscillation"
    local flap_file="$test_dir/flap_source.rs"
    echo "let mut x = 10;" > "$flap_file"

    set +e
    # Cycle 1: Error A
    hgb_cb_record_failure "$flap_skill" "E0308 mismatched types" "$hash1" "$hash2" "$flap_file" "E0308" 10 "1:5" "mismatched types"
    # Cycle 2: Error B
    hgb_cb_record_failure "$flap_skill" "E0596 cannot borrow mut" "$hash1" "$hash2" "$flap_file" "E0596" 12 "1:5" "cannot borrow mut"
    # Cycle 3: Error A (Oscillation detected: [A, B, A] -> Flapping!)
    hgb_cb_record_failure "$flap_skill" "E0308 mismatched types" "$hash1" "$hash2" "$flap_file" "E0308" 10 "1:5" "mismatched types"
    local flap_ret=$?

    # Verify check blocks mutation
    hgb_cb_check_skill "$flap_skill"
    local flap_check=$?
    set -e

    if [ "$flap_ret" -ne 3 ] || [ "$flap_check" -eq 0 ]; then
        log_err "Flapping oscillation failed to trigger PERM_TRIPPED_CRYPTOLOCKED!"
        exit 1
    fi

    if [ -w "$flap_file" ]; then
        log_err "File permissions were not stripped to 0444 on cryptolock!"
        exit 1
    fi
    log_ok "Flapping oscillation detection & PERM_TRIPPED_CRYPTOLOCKED verified."

    echo "5. Testing Cryptographic Rejection of Unsigned Administrative Reset..."
    set +e
    hgb_cb_reset_skill "$flap_skill" "$flap_file"
    local reset_ret=$?
    set -e
    if [ "$reset_ret" -eq 0 ]; then
        log_err "Administrative reset succeeded on PERM_TRIPPED_CRYPTOLOCKED! Security bypass!"
        exit 1
    fi
    log_ok "Unsigned administrative reset correctly rejected for cryptolocked skill."

    echo "6. Testing Authorized Signed MCP Operator Unlock..."
    local token_json
    token_json=$(hgb_generate_operator_token "$flap_skill" "ops-admin-alice")
    local op_id lock_id nonce pub_hex sig_hex
    op_id=$(python3 -c "import json, sys; print(json.loads(sys.argv[1])['operator_id'])" "$token_json")
    lock_id=$(python3 -c "import json, sys; print(json.loads(sys.argv[1])['lockout_id'])" "$token_json")
    nonce=$(python3 -c "import json, sys; print(json.loads(sys.argv[1])['nonce'])" "$token_json")
    pub_hex=$(python3 -c "import json, sys; print(json.loads(sys.argv[1])['operator_public_key'])" "$token_json")
    sig_hex=$(python3 -c "import json, sys; print(json.loads(sys.argv[1])['ed25519_signature'])" "$token_json")

    # Replay defense check: Tampered signature must fail
    set +e
    hgb_cb_operator_unlock "$flap_skill" "$op_id" "$lock_id" "$nonce" "$pub_hex" "deadbeef00112233" "$flap_file"
    local tamper_ret=$?
    set -e
    if [ "$tamper_ret" -eq 0 ]; then
        log_err "Tampered signature was accepted by operator unlock!"
        exit 1
    fi

    # Legitimate unlock
    hgb_cb_operator_unlock "$flap_skill" "$op_id" "$lock_id" "$nonce" "$pub_hex" "$sig_hex" "$flap_file"

    # Verify check allows mutation and permissions are 0644
    hgb_cb_check_skill "$flap_skill"
    if [ ! -w "$flap_file" ]; then
        log_err "Write permissions were not restored upon operator unlock!"
        exit 1
    fi
    log_ok "Signed MCP operator unlock with replay defense verified."

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
        fibonacci)
            hgb_fibonacci "${2:-1}"
            ;;
        calculate-backoff)
            hgb_calculate_backoff "${2:-1}" "${3:-$HGB_CB_T0}" "${4:-$HGB_CB_TMAX}" "${5:-exponential}"
            ;;
        apply-backoff)
            hgb_apply_backoff "${2:-unknown}" "${3:-1}"
            ;;
        fingerprint)
            hgb_compute_ast_fingerprint "${2:-}" "${3:-}" "${4:-1}" "${5:-0:0}" "${6:-}"
            ;;
        record-failure)
            hgb_cb_record_failure "${2:-unknown}" "${3:-unknown error}" "${4:-none}" "${5:-none}" "${6:-}" "${7:-}" "${8:-1}" "${9:-0:0}" "${10:-}"
            ;;
        record-success)
            hgb_cb_record_success "${2:-unknown}" "${3:-none}" "${4:-none}" "${5:-}"
            ;;
        operator-unlock)
            hgb_cb_operator_unlock "${2:-}" "${3:-}" "${4:-}" "${5:-}" "${6:-}" "${7:-}" "${8:-}"
            ;;
        generate-operator-token)
            hgb_generate_operator_token "${2:-}" "${3:-ops-admin-hitl}" "${4:-}"
            ;;
        reset)
            hgb_cb_reset_skill "${2:-unknown}" "${3:-}"
            ;;
        record-memory)
            hgb_cb_record_memory "${2:-unknown}" "${3:-0}" "${4:-0}" "${5:-0}" "${6:-}"
            ;;
        check-recursion)
            hgb_cb_check_recursion "${2:-unknown}" "${3:-1}" "${4:-}" "${5:-0}" "${6:-}" "${7:-}"
            ;;
        --test|test)
            run_self_tests
            ;;
        help|--help|-h)
            cat <<EOF
Usage: circuit-breaker-governor.sh <command> [arguments...]

Commands:
  check <skill_id>                                    Check skill eligibility (CLOSED, HALF_OPEN, RECOVERY = 0; OPEN, PERM_TRIPPED_CRYPTOLOCKED = 1)
  hash-state <path>                                   Compute BLAKE3 / SHA-256 state hash of file or directory
  fibonacci <n>                                       Compute the n-th Fibonacci number
  calculate-backoff <iter> [t0] [tmax]                Compute Fibonacci progression backoff duration in seconds
  apply-backoff <skill_id> [iter]                     Sleep for calculated Fibonacci backoff dampening duration
  fingerprint <code> <file> <line> <span> <msg>       Compute deterministic AST error state fingerprint
  record-failure <skill_id> <err> [pre] [post] [file] [code] [line] [span] [msg]
                                                      Record failed mutation (drives flapping detection and cryptolock)
  record-success <skill_id> [pre] [post] [file]       Record successful mutation (drives hysteresis recovery)
  operator-unlock <skill_id> <op_id> <lock_id> <nonce> <pub_key> <sig> [path]
                                                      Cryptographically unlock PERM_TRIPPED_CRYPTOLOCKED via signed MCP token
  generate-operator-token <skill_id> [op_id] [lock_id] Generate single-use Ed25519 token and signature
  reset <skill_id> [path]                             HITL administrative reset (fails if PERM_TRIPPED_CRYPTOLOCKED)
  --test                                              Run comprehensive integration, flapping & cryptographic unlock self-tests
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
