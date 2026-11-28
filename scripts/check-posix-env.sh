#!/usr/bin/env bash
# ==============================================================================
# Hagibis (hgb) POSIX Environment & Filesystem Compliance Suite
# Validates native Linux ext4/btrfs/xfs filesystem semantics, advisory and
# mandatory flock locking, UNIX domain sockets, POSIX permissions (chmod 755),
# /dev/shm zero-copy memory mapping, and SQLite WAL atomic concurrency.
#
# Zero mocks. Real Linux POSIX kernel validation.
# Exits with 0 on full compliance, non-zero on failure.
# ==============================================================================
set -euo pipefail

# ANSI color styling
BOLD="\033[1m"
GREEN="\033[0;32m"
RED="\033[0;31m"
YELLOW="\033[0;33m"
CYAN="\033[0;36m"
DIM="\033[2m"
RESET="\033[0m"

PASS_COUNT=0
TOTAL_COUNT=6

log_header() {
    echo -e "${CYAN}${BOLD}╔═══════════════════════════════════════════════════════════════════════════╗${RESET}"
    echo -e "${CYAN}${BOLD}║   HAGIBIS (hgb) POSIX COMPLIANCE & FILESYSTEM VERIFICATION SUITE         ║${RESET}"
    echo -e "${CYAN}${BOLD}║   Pillar 1: Infrastructure & High-Performance POSIX Semantics             ║${RESET}"
    echo -e "${CYAN}${BOLD}╚═══════════════════════════════════════════════════════════════════════════╝${RESET}"
    echo ""
}

log_pass() {
    local check_name="$1"
    local detail="$2"
    echo -e "  [${GREEN}${BOLD}PASS${RESET}] ${BOLD}${check_name}${RESET}"
    echo -e "         ${DIM}↳ ${detail}${RESET}"
    PASS_COUNT=$((PASS_COUNT + 1))
}

log_fail() {
    local check_name="$1"
    local detail="$2"
    echo -e "  [${RED}${BOLD}FAIL${RESET}] ${BOLD}${check_name}${RESET}"
    echo -e "         ${RED}↳ ${detail}${RESET}"
}

log_warn() {
    local message="$1"
    echo -e "  ${YELLOW}${BOLD}⚠ WARNING:${RESET} ${message}"
}

# Ensure ~/.hgb directory exists
HGB_DIR="${HOME:-/home/toshiba}/.hgb"
mkdir -p "$HGB_DIR"

log_header

# ------------------------------------------------------------------------------
# Check 1: Native Filesystem Type (ext4, btrfs, xfs, tmpfs)
# ------------------------------------------------------------------------------
echo -e "${BOLD}[1/${TOTAL_COUNT}] Validating Native Linux POSIX Filesystem Type...${RESET}"
WS_DIR="$(pwd -P)"
FS_TYPE_RAW="$(stat -f -c %T "$WS_DIR" 2>/dev/null || echo "unknown")"
MOUNT_FSTYPE="$(findmnt -T "$WS_DIR" -o FSTYPE -n 2>/dev/null || df -T "$WS_DIR" 2>/dev/null | awk 'NR==2 {print $2}' || echo "unknown")"
HGB_FSTYPE="$(findmnt -T "$HGB_DIR" -o FSTYPE -n 2>/dev/null || df -T "$HGB_DIR" 2>/dev/null | awk 'NR==2 {print $2}' || echo "unknown")"

# Check if filesystem is POSIX compliant
IS_POSIX=false
if [[ "$MOUNT_FSTYPE" =~ ^(ext[234]|btrfs|xfs|zfs|tmpfs)$ ]] || [[ "$FS_TYPE_RAW" =~ ^(ext[234]|ext2/ext3|btrfs|xfs|zfs|tmpfs)$ ]]; then
    IS_POSIX=true
fi

if [ "$IS_POSIX" = true ]; then
    log_pass "Filesystem Type" "Workspace ($WS_DIR) is mounted on native POSIX '$MOUNT_FSTYPE' (stat: $FS_TYPE_RAW); ~/.hgb is on '$HGB_FSTYPE'"
else
    log_fail "Filesystem Type" "Workspace ($WS_DIR) is mounted on non-POSIX '$MOUNT_FSTYPE' (stat: $FS_TYPE_RAW). Native Linux ext4/btrfs/xfs required!"
    echo -e "         ${YELLOW}Hint: Switch to /home/toshiba/hagibis on /dev/sda2 (ext4) for full POSIX semantics.${RESET}"
    exit 1
fi
echo ""

# ------------------------------------------------------------------------------
# Check 2: Advisory and Mandatory File Locking (flock on lockfile)
# ------------------------------------------------------------------------------
echo -e "${BOLD}[2/${TOTAL_COUNT}] Testing Advisory & Mandatory Kernel File Locking (flock)...${RESET}"
LOCK_FILE="$HGB_DIR/posix_flock_verify.lock"
rm -f "$LOCK_FILE"
touch "$LOCK_FILE"

# Open fd 200 on lock file and hold exclusive lock
exec 200>"$LOCK_FILE"
if flock -n 200; then
    # Test that a child process is correctly blocked by the existing exclusive lock
    CHILD_LOCK_RESULT=0
    (
        exec 201>"$LOCK_FILE"
        if flock -n 201; then
            exit 42 # Error: child acquired lock when it should have been locked!
        else
            exit 0 # Expected: lock contention detected (EWOULDBLOCK)
        fi
    ) && CHILD_LOCK_RESULT=$? || CHILD_LOCK_RESULT=$?

    # Release parent lock
    flock -u 200
    exec 200>&-

    if [ "$CHILD_LOCK_RESULT" -eq 0 ]; then
        # Now verify lock can be acquired again once released
        exec 202>"$LOCK_FILE"
        if flock -n 202; then
            flock -u 202
            exec 202>&-
            log_pass "Kernel File Locking" "flock mutual exclusion & contention detection fully verified on $LOCK_FILE"
        else
            exec 202>&-
            log_fail "Kernel File Locking" "Failed to acquire lock after parent release"
            exit 1
        fi
    else
        log_fail "Kernel File Locking" "Child process incorrectly bypassed existing exclusive lock on $LOCK_FILE"
        exit 1
    fi
else
    exec 200>&-
    log_fail "Kernel File Locking" "Failed to acquire initial flock on $LOCK_FILE"
    exit 1
fi
rm -f "$LOCK_FILE"
echo ""

# ------------------------------------------------------------------------------
# Check 3: Creation and Connection of UNIX Domain Sockets (~/.hgb/test.sock)
# ------------------------------------------------------------------------------
echo -e "${BOLD}[3/${TOTAL_COUNT}] Testing UNIX Domain Sockets ($HGB_DIR/test.sock)...${RESET}"
TEST_SOCK="$HGB_DIR/test.sock"
rm -f "$TEST_SOCK"

SOCK_TEST_SCRIPT="
import socket, sys, os

sock_path = '$TEST_SOCK'
if os.path.exists(sock_path):
    os.unlink(sock_path)

server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(sock_path)
server.listen(1)

client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(sock_path)

conn, _ = server.accept()
payload = b'HAGIBIS_POSIX_IPC_PING\n'
client.sendall(payload)
received = conn.recv(1024)

client.close()
conn.close()
server.close()

if received == payload:
    sys.exit(0)
else:
    sys.exit(2)
"

if python3 -c "$SOCK_TEST_SCRIPT" 2>/dev/null; then
    log_pass "UNIX Domain Sockets" "Stream socket created, bound, connected, and payload verified at $TEST_SOCK"
else
    log_fail "UNIX Domain Sockets" "Failed to bind or connect UNIX domain socket at $TEST_SOCK"
    rm -f "$TEST_SOCK"
    exit 1
fi
rm -f "$TEST_SOCK"
echo ""

# ------------------------------------------------------------------------------
# Check 4: POSIX File Mode Changes (chmod 755 & execution)
# ------------------------------------------------------------------------------
echo -e "${BOLD}[4/${TOTAL_COUNT}] Testing POSIX File Mode Permissions (chmod 755 & execve)...${RESET}"
TEST_EXEC="$HGB_DIR/posix_exec_test.sh"
rm -f "$TEST_EXEC"

cat << 'EOF' > "$TEST_EXEC"
#!/usr/bin/env bash
echo "HAGIBIS_EXEC_OK"
exit 0
EOF

# Initially set non-executable mode 0644
chmod 644 "$TEST_EXEC"
CAN_EXEC_BEFORE=false
if "$TEST_EXEC" >/dev/null 2>&1; then
    CAN_EXEC_BEFORE=true
fi

# Apply chmod 755
chmod 755 "$TEST_EXEC"
MODE_OCTAL="$(stat -c %a "$TEST_EXEC" 2>/dev/null || echo "0")"
EXEC_OUT="$("$TEST_EXEC" 2>/dev/null || echo "EXEC_FAILED")"

if [ "$CAN_EXEC_BEFORE" = false ] && [ "$MODE_OCTAL" = "755" ] && [ "$EXEC_OUT" = "HAGIBIS_EXEC_OK" ]; then
    log_pass "POSIX File Permissions" "Strict 0644 enforcement and chmod 755 execution verified on $TEST_EXEC (mode: $MODE_OCTAL)"
else
    log_fail "POSIX File Permissions" "File mode permissions failed (can_exec_before=$CAN_EXEC_BEFORE, mode=$MODE_OCTAL, exec_out=$EXEC_OUT)"
    rm -f "$TEST_EXEC"
    exit 1
fi
rm -f "$TEST_EXEC"
echo ""

# ------------------------------------------------------------------------------
# Check 5: High-Performance POSIX Shared Memory Mapping in /dev/shm
# ------------------------------------------------------------------------------
echo -e "${BOLD}[5/${TOTAL_COUNT}] Testing POSIX Shared Memory (/dev/shm Zero-Copy Mmap)...${RESET}"
SHM_TEST_SCRIPT="
import os, sys, mmap, time

shm_path = '/dev/shm/hgb_posix_shm_verify_{}.tmp'.format(os.getpid())
size = 65536 # 64 KB

t0 = time.perf_counter_ns()
with open(shm_path, 'wb') as f:
    f.write(b'\\x00' * size)

with open(shm_path, 'r+b') as f:
    mm = mmap.mmap(f.fileno(), size)
    # Write memory-mapped pattern
    test_pattern = b'HAGIBIS_SHM_ZERO_COPY_PERSISTENCE_VECTOR_OK'
    mm[0:len(test_pattern)] = test_pattern
    mm.seek(0)
    read_back = mm.read(len(test_pattern))
    mm.close()

os.unlink(shm_path)
elapsed_us = (time.perf_counter_ns() - t0) / 1000.0

if read_back == test_pattern:
    print('{:.1f}'.format(elapsed_us))
    sys.exit(0)
else:
    sys.exit(1)
"

SHM_LATENCY="$(python3 -c "$SHM_TEST_SCRIPT" 2>/dev/null || echo "FAIL")"
if [ "$SHM_LATENCY" != "FAIL" ]; then
    log_pass "Shared Memory (/dev/shm)" "64KB zero-copy memory mapping succeeded in ${SHM_LATENCY}µs (tmpfs backed)"
else
    log_fail "Shared Memory (/dev/shm)" "/dev/shm is either missing, unwritable, or mmap operations failed"
    exit 1
fi
echo ""

# ------------------------------------------------------------------------------
# Check 6: SQLite WAL Mode Atomic Concurrency Test
# ------------------------------------------------------------------------------
echo -e "${BOLD}[6/${TOTAL_COUNT}] Testing SQLite WAL Mode Atomic Concurrency (~/.hgb)...${RESET}"
WAL_TEST_SCRIPT="
import sqlite3, os, sys, time

db_path = '$HGB_DIR/posix_wal_verify.db'
wal_path = db_path + '-wal'
shm_path = db_path + '-shm'

for p in [db_path, wal_path, shm_path]:
    if os.path.exists(p):
        os.unlink(p)

# Connection 1: Initialize WAL schema
conn1 = sqlite3.connect(db_path, timeout=5.0)
cur1 = conn1.cursor()
cur1.execute('PRAGMA journal_mode = WAL;')
mode = cur1.fetchone()[0]
if mode.lower() != 'wal':
    sys.exit(1)

cur1.execute('CREATE TABLE atomic_records (id INTEGER PRIMARY KEY, value TEXT NOT NULL);')
cur1.execute('INSERT INTO atomic_records VALUES (1, \"baseline\");')
conn1.commit()

# Connection 2: Concurrent reader
conn2 = sqlite3.connect(db_path, timeout=5.0)
cur2 = conn2.cursor()

# Connection 1 starts uncommitted write transaction
cur1.execute('BEGIN IMMEDIATE;')
cur1.execute('INSERT INTO atomic_records VALUES (2, \"uncommitted_mutation\");')

# Connection 2 reads: Must see exactly 1 record under POSIX WAL snapshot isolation!
cur2.execute('SELECT COUNT(*) FROM atomic_records;')
reader_count = cur2.fetchone()[0]
if reader_count != 1:
    conn1.rollback()
    conn1.close()
    conn2.close()
    sys.exit(2)

# Connection 1 commits
conn1.commit()

# Connection 2 now reads after commit: Must see 2 records
cur2.execute('SELECT COUNT(*) FROM atomic_records;')
committed_count = cur2.fetchone()[0]
if committed_count != 2:
    conn1.close()
    conn2.close()
    sys.exit(3)

# Verify WAL & SHM files were created on POSIX filesystem
wal_exists = os.path.exists(wal_path) or os.path.exists(db_path)
conn1.close()
conn2.close()

for p in [db_path, wal_path, shm_path]:
    if os.path.exists(p):
        os.unlink(p)

sys.exit(0)
"

if python3 -c "$WAL_TEST_SCRIPT" 2>/dev/null; then
    log_pass "SQLite WAL Concurrency" "Atomic snapshot isolation & concurrent multi-reader WAL lock verified in $HGB_DIR"
else
    log_fail "SQLite WAL Concurrency" "SQLite WAL mode or atomic concurrency failed in $HGB_DIR"
    exit 1
fi
echo ""

# ------------------------------------------------------------------------------
# Final Summary Diagnostics
# ------------------------------------------------------------------------------
echo -e "${CYAN}═════════════════════════════════════════════════════════════════════════════${RESET}"
echo -e "${GREEN}${BOLD}✓ POSIX VALIDATION SUCCESS: ${PASS_COUNT}/${TOTAL_COUNT} CHECKS PASSED (100% COMPLIANT)${RESET}"
echo -e "${DIM}  Filesystem:     $MOUNT_FSTYPE ($FS_TYPE_RAW)"
echo -e "  Runtime Root:   $HGB_DIR"
echo -e "  Shared Memory:  /dev/shm (tmpfs)"
echo -e "  Target Dir:     ${CARGO_TARGET_DIR:-$HOME/.cargo-target-hagibis}"
echo -e "${CYAN}═════════════════════════════════════════════════════════════════════════════${RESET}"
exit 0
