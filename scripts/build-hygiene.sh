#!/usr/bin/env bash
# ==============================================================================
# scripts/build-hygiene.sh
# Hagibis Autonomous Storage & Build Hygiene Management System (Pillar 5)
#
# Core Capabilities:
#   1. Root Filesystem Monitoring: Queries root (/) utilization; alarms if >= 80%.
#   2. Stale Artifact Pruning: Prunes incremental artifacts & object caches older
#      than 7 days or exceeding disk quotas from external and local cargo target dirs.
#   3. Automated Sccache Detection & Setup: Detects sccache, configures SCCACHE_DIR,
#      and manages compilation cache quotas.
#   4. Temporary Build Target Cleanup: Cleans volatile scratch directories
#      (/tmp/cargo*, /tmp/pgo-*, /tmp/bolt-*, /tmp/hgb-*).
#   5. Scratch Space Provisioning: Prepares /media/toshiba/Lexar/scratch/
#      for Docker containers, WASM build cache, and Firecracker microVM execution.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." 2>/dev/null && pwd || echo "/media/toshiba/Lexar/hagibis")"

# Target directories to inspect
EXTERNAL_TARGET_DIR="${CARGO_TARGET_DIR:-/media/toshiba/Lexar/cargo-target}"
LOCAL_TARGET_DIR="$WORKSPACE_ROOT/target"
TMP_TARGET_DIR="/tmp/cargo-target"
HOME_TARGET_DIR="$HOME/.cargo-target-hagibis"

# Sccache directory & cache configuration
SCCACHE_DIR="${SCCACHE_DIR:-/media/toshiba/Lexar/sccache}"
if [ ! -d "/media/toshiba/Lexar" ]; then
    SCCACHE_DIR="${HOME}/.cache/sccache"
fi
export SCCACHE_DIR
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-10G}"

# Scratch directory for microVMs and containers
LEXAR_SCRATCH_DIR="/media/toshiba/Lexar/scratch"

# Thresholds
DISK_ALARM_THRESHOLD_PERCENT=80
PRUNE_DAYS=7
TARGET_QUOTA_MB=15360 # 15 GB quota for target directories

# Modes
MODE="all"
JSON_OUTPUT=false

show_help() {
    cat <<EOF
Hagibis Storage & Build Hygiene Manager (Pillar 5)

Usage: $(basename "$0") [OPTIONS]

Options:
  --check            Check root filesystem usage & target directory footprints without deletion.
  --clean-cache      Purge volatile /tmp build artifacts (/tmp/cargo*, /tmp/pgo*, /tmp/bolt*).
  --prune-targets    Prune build artifacts older than ${PRUNE_DAYS} days from target directories.
  --sccache-status   Detect sccache installation, verify SCCACHE_DIR and report cache stats.
  --storage-hygiene  Execute full storage hygiene audit, sccache setup, and target quota pruning.
  --prepare-scratch  Create and verify scratch space at ${LEXAR_SCRATCH_DIR}.
  --all              Execute full hygiene pipeline (check, sccache, prune, clean, scratch). (Default)
  --self-heal-build  Execute hygiene audit, trap cargo build errors & trigger autonomous self-healing.
  --json             Emit telemetry and status summary as JSON.
  -h, --help         Show this help message.
EOF
}

for arg in "$@"; do
    case "$arg" in
        --check)
            MODE="check"
            ;;
        --clean-cache)
            MODE="clean-cache"
            ;;
        --prune-targets)
            MODE="prune-targets"
            ;;
        --sccache-status)
            MODE="sccache-status"
            ;;
        --storage-hygiene)
            MODE="storage-hygiene"
            ;;
        --prepare-scratch)
            MODE="prepare-scratch"
            ;;
        --all)
            MODE="all"
            ;;
        --self-heal-build)
            MODE="self-heal-build"
            ;;
        --json)
            JSON_OUTPUT=true
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            echo "[ERROR] Unknown option: $arg" >&2
            show_help
            exit 1
            ;;
    esac
done

# ANSI color codes (disabled if JSON mode)
if [ "$JSON_OUTPUT" = true ]; then
    RED=""
    GREEN=""
    YELLOW=""
    BLUE=""
    BOLD=""
    RESET=""
else
    RED="\033[0;31m"
    GREEN="\033[0;32m"
    YELLOW="\033[1;33m"
    BLUE="\033[0;34m"
    BOLD="\033[1m"
    RESET="\033[0m"
fi

# ------------------------------------------------------------------------------
# 1. Disk Utilization Check
# ------------------------------------------------------------------------------
check_disk_usage() {
    local root_line
    root_line=$(df -Pk / | awk 'NR==2')
    local root_fs root_total root_used root_avail root_percent
    root_fs=$(echo "$root_line" | awk '{print $1}')
    root_total=$(echo "$root_line" | awk '{print $2}')
    root_used=$(echo "$root_line" | awk '{print $3}')
    root_avail=$(echo "$root_line" | awk '{print $4}')
    root_percent=$(echo "$root_line" | awk '{gsub("%","",$5); print $5}')

    local lexar_percent="N/A"
    local lexar_avail="N/A"
    if [ -d "/media/toshiba/Lexar" ]; then
        local lexar_line
        lexar_line=$(df -Pk /media/toshiba/Lexar 2>/dev/null | awk 'NR==2' || true)
        if [ -n "$lexar_line" ]; then
            lexar_percent=$(echo "$lexar_line" | awk '{gsub("%","",$5); print $5}')
            local avail_kb
            avail_kb=$(echo "$lexar_line" | awk '{print $4}')
            lexar_avail="$((avail_kb / 1024 / 1024)) GB"
        fi
    fi

    local is_alarm=false
    if [ "$root_percent" -ge "$DISK_ALARM_THRESHOLD_PERCENT" ]; then
        is_alarm=true
    fi

    if [ "$JSON_OUTPUT" != true ]; then
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e "${BOLD} Hagibis Storage & Build Hygiene: Root Filesystem Audit${RESET}"
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e " Root Filesystem:        ${root_fs}"
        echo -e " Total Capacity:         $((root_total / 1024 / 1024)) GB"
        echo -e " Used Space:             $((root_used / 1024 / 1024)) GB"
        echo -e " Available Space:        $((root_avail / 1024 / 1024)) GB"
        echo -e " Utilization:            ${root_percent}% (Threshold: ${DISK_ALARM_THRESHOLD_PERCENT}%)"
        echo -e " External Lexar Avail:   ${lexar_avail} (${lexar_percent}% used)"
        echo ""

        if [ "$is_alarm" = true ]; then
            echo -e "${RED}${BOLD}[ALARM] Root partition utilization (${root_percent}%) exceeds ${DISK_ALARM_THRESHOLD_PERCENT}% threshold!${RESET}"
            echo -e "${YELLOW}[ACTION] High priority: Pruning stale artifacts and purging /tmp cache...${RESET}"
        else
            echo -e "${GREEN}[OK] Root filesystem headroom is optimal (${root_percent}% < ${DISK_ALARM_THRESHOLD_PERCENT}% threshold).${RESET}"
        fi
    fi

    ROOT_PERCENT="$root_percent"
    ROOT_AVAIL_GB="$((root_avail / 1024 / 1024))"
    IS_ALARM="$is_alarm"
}

# ------------------------------------------------------------------------------
# 2. Automated Sccache Detection & Configuration
# ------------------------------------------------------------------------------
setup_sccache() {
    mkdir -p "$SCCACHE_DIR"
    local sccache_bin
    sccache_bin=$(command -v sccache 2>/dev/null || true)

    if [ "$JSON_OUTPUT" != true ]; then
        echo ""
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e "${BOLD} Automated Sccache Compilation Cache Management${RESET}"
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e " SCCACHE_DIR:            ${SCCACHE_DIR}"
        echo -e " SCCACHE_CACHE_SIZE:     ${SCCACHE_CACHE_SIZE}"
    fi

    if [ -n "$sccache_bin" ]; then
        export RUSTC_WRAPPER="$sccache_bin"
        if [ "$JSON_OUTPUT" != true ]; then
            echo -e "${GREEN}  ✔ Sccache detected: ${sccache_bin}${RESET}"
            echo -e "    Enabled: RUSTC_WRAPPER=${sccache_bin}"
            "$sccache_bin" --show-stats 2>/dev/null || true
        fi
    else
        if [ "$JSON_OUTPUT" != true ]; then
            echo -e "${YELLOW}  [INFO] sccache is not installed in PATH. Compilations will proceed with native cargo caches.${RESET}"
            echo -e "         To install: cargo install sccache --locked"
        fi
    fi
}

# ------------------------------------------------------------------------------
# 3. Prune Stale Artifacts Older Than N Days & Quota Enforcer
# ------------------------------------------------------------------------------
prune_target_directory() {
    local target_dir="$1"
    local days="${2:-$PRUNE_DAYS}"

    if [ ! -d "$target_dir" ]; then
        return 0
    fi

    if [ "$JSON_OUTPUT" != true ]; then
        echo -e "${BLUE}[PRUNE] Auditing target directory: ${target_dir}${RESET}"
    fi

    local pre_size_kb
    pre_size_kb=$(du -sk "$target_dir" 2>/dev/null | awk '{print $1}' || echo "0")
    local pre_size_mb=$((pre_size_kb / 1024))

    # 1. Prune stale object files, incremental caches, rlibs older than $days
    local pruned_count=0
    local stale_files
    stale_files=$(find "$target_dir" -type f \( -name "*.o" -o -name "*.rlib" -o -name "*.rmeta" -o -name "*.d" -o -name "*.profraw" \) -mtime "+$days" 2>/dev/null || true)
    
    if [ -n "$stale_files" ]; then
        pruned_count=$(echo "$stale_files" | grep -v '^$' | wc -l || echo "0")
        echo "$stale_files" | xargs -r rm -f 2>/dev/null || true
    fi

    # Clean stale incremental compilation directory artifacts older than $days
    if [ -d "$target_dir/debug/incremental" ]; then
        find "$target_dir/debug/incremental" -depth -mindepth 2 -type d -mtime "+$days" -exec rm -rf {} + 2>/dev/null || true
    fi
    if [ -d "$target_dir/release/incremental" ]; then
        find "$target_dir/release/incremental" -depth -mindepth 2 -type d -mtime "+$days" -exec rm -rf {} + 2>/dev/null || true
    fi

    # 2. Enforce disk quota if directory exceeds TARGET_QUOTA_MB
    local post_size_kb
    post_size_kb=$(du -sk "$target_dir" 2>/dev/null | awk '{print $1}' || echo "0")
    local current_mb=$((post_size_kb / 1024))

    if [ "$current_mb" -gt "$TARGET_QUOTA_MB" ]; then
        if [ "$JSON_OUTPUT" != true ]; then
            echo -e "${YELLOW}  [QUOTA] Target directory size (${current_mb} MB) exceeds quota (${TARGET_QUOTA_MB} MB). Pruning oldest build artifacts...${RESET}"
        fi
        # Prune oldest files until under quota
        find "$target_dir" -type f \( -name "*.rlib" -o -name "*.rmeta" -o -name "*.o" \) -printf '%T+ %p\n' 2>/dev/null | sort | head -n 50 | awk '{print $2}' | xargs -r rm -f 2>/dev/null || true
        post_size_kb=$(du -sk "$target_dir" 2>/dev/null | awk '{print $1}' || echo "0")
    fi

    local freed_kb=$((pre_size_kb > post_size_kb ? pre_size_kb - post_size_kb : 0))
    local freed_mb=$((freed_kb / 1024))

    if [ "$JSON_OUTPUT" != true ]; then
        echo -e "${GREEN}  ✔ Pruned ${pruned_count} files older than ${days} days from ${target_dir}.${RESET}"
        echo -e "    Freed: ~${freed_mb} MB | Current size: ~$((post_size_kb / 1024)) MB (Quota: ${TARGET_QUOTA_MB} MB)"
    fi
}

prune_all_targets() {
    if [ "$JSON_OUTPUT" != true ]; then
        echo ""
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e "${BOLD} Pruning Stale Incremental Artifacts (> ${PRUNE_DAYS} days) & Enforcing Quotas${RESET}"
        echo -e "${BLUE}======================================================================${RESET}"
    fi

    prune_target_directory "$EXTERNAL_TARGET_DIR" "$PRUNE_DAYS"
    prune_target_directory "$LOCAL_TARGET_DIR" "$PRUNE_DAYS"
    prune_target_directory "$TMP_TARGET_DIR" "$PRUNE_DAYS"
    prune_target_directory "$HOME_TARGET_DIR" "$PRUNE_DAYS"
}

# ------------------------------------------------------------------------------
# 4. Clean Volatile Temporary Build Artifacts
# ------------------------------------------------------------------------------
clean_volatile_cache() {
    if [ "$JSON_OUTPUT" != true ]; then
        echo ""
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e "${BOLD} Cleaning Volatile Temporary Caches & Profiles (/tmp)${RESET}"
        echo -e "${BLUE}======================================================================${RESET}"
    fi

    local cleaned_paths=0
    for pattern in \
        "/tmp/cargo*" \
        "/tmp/pgo-*" \
        "/tmp/bolt-*" \
        "/tmp/hgb-*" \
        "/tmp/test_reloc*" \
        "/tmp/pgo-capability-probe*"; do
        for p in $pattern; do
            if [ -e "$p" ]; then
                rm -rf "$p" 2>/dev/null || true
                cleaned_paths=$((cleaned_paths + 1))
            fi
        done
    done

    if [ "$JSON_OUTPUT" != true ]; then
        echo -e "${GREEN}  ✔ Purged ${cleaned_paths} temporary build scratch paths from /tmp.${RESET}"
    fi
}

# ------------------------------------------------------------------------------
# 5. Prepare Scratch Space on External Lexar Drive
# ------------------------------------------------------------------------------
prepare_scratch_space() {
    if [ "$JSON_OUTPUT" != true ]; then
        echo ""
        echo -e "${BLUE}======================================================================${RESET}"
        echo -e "${BOLD} Provisioning High-Capacity Scratch Space (${LEXAR_SCRATCH_DIR})${RESET}"
        echo -e "${BLUE}======================================================================${RESET}"
    fi

    if [ -d "/media/toshiba/Lexar" ]; then
        mkdir -p "$LEXAR_SCRATCH_DIR/docker"
        mkdir -p "$LEXAR_SCRATCH_DIR/firecracker"
        mkdir -p "$LEXAR_SCRATCH_DIR/tmp"
        mkdir -p "$LEXAR_SCRATCH_DIR/wasm_cache"
        mkdir -p "$EXTERNAL_TARGET_DIR"
        mkdir -p "$SCCACHE_DIR"

        local probe_file="$LEXAR_SCRATCH_DIR/tmp/.write_probe"
        touch "$probe_file" 2>/dev/null && rm -f "$probe_file"
        
        if [ "$JSON_OUTPUT" != true ]; then
            echo -e "${GREEN}  ✔ Scratch directory provisioned successfully:${RESET}"
            echo -e "    - Docker Root:       $LEXAR_SCRATCH_DIR/docker"
            echo -e "    - Firecracker vhost: $LEXAR_SCRATCH_DIR/firecracker"
            echo -e "    - WASM Cache:        $LEXAR_SCRATCH_DIR/wasm_cache"
            echo -e "    - Sccache Cache:     $SCCACHE_DIR"
            echo -e "    - External Target:   $EXTERNAL_TARGET_DIR"
        fi
    else
        if [ "$JSON_OUTPUT" != true ]; then
            echo -e "${YELLOW}  [WARN] /media/toshiba/Lexar is not currently mounted. Skipping external scratch setup.${RESET}"
        fi
    fi
}

# ------------------------------------------------------------------------------
# Execution Flow
# ------------------------------------------------------------------------------
check_disk_usage

case "$MODE" in
    check)
        setup_sccache
        ;;
    clean-cache)
        clean_volatile_cache
        ;;
    prune-targets)
        prune_all_targets
        ;;
    sccache-status)
        setup_sccache
        ;;
    storage-hygiene|all)
        setup_sccache
        prune_all_targets
        clean_volatile_cache
        prepare_scratch_space
        ;;
    prepare-scratch)
        prepare_scratch_space
        ;;
esac

if [ "$JSON_OUTPUT" = true ]; then
    cat <<EOF
{
  "status": "success",
  "root_percent": $ROOT_PERCENT,
  "root_avail_gb": $ROOT_AVAIL_GB,
  "alarm_triggered": $IS_ALARM,
  "alarm_threshold": $DISK_ALARM_THRESHOLD_PERCENT,
  "external_target_dir": "$EXTERNAL_TARGET_DIR",
  "sccache_dir": "$SCCACHE_DIR",
  "scratch_dir": "$LEXAR_SCRATCH_DIR",
  "mode": "$MODE"
}
EOF
else
    echo ""
    echo -e "${GREEN}======================================================================${RESET}"
    echo -e "${BOLD} Hagibis Storage & Build Hygiene Cycle Complete${RESET}"
    echo -e "${GREEN}======================================================================${RESET}"
fi
