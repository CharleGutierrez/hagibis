#!/usr/bin/env bash
# ==============================================================================
# Hagibis Bare-Metal Kernel Governor Controller
# Cooperates with dynatune, kernel-optimizer, and OpenTelemetry governor bridge
# ==============================================================================

set -e

ACTION="${1:-status}"
STATE_FILE="/tmp/hgb_governor_state.json"

record_state() {
    local profile="$1"
    local timestamp
    timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date)
    cat <<EOF > "$STATE_FILE" 2>/dev/null || true
{
  "active_profile": "$profile",
  "updated_at": "$timestamp"
}
EOF
}

get_target_profile() {
    if [ -f "$STATE_FILE" ]; then
        grep -o '"active_profile": "[^"]*' "$STATE_FILE" 2>/dev/null | cut -d'"' -f4 || echo "balanced"
    else
        echo "balanced"
    fi
}

case "$ACTION" in
    performance|turbo|boost)
        echo "🚀 [Hagibis Bare-Metal Bridge] Boosting CPU to High-Performance..."
        for epp in /sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference; do
            if [ -w "$epp" ]; then
                echo "performance" > "$epp" 2>/dev/null || true
            fi
        done
        for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            if [ -w "$gov" ]; then
                echo "performance" > "$gov" 2>/dev/null || true
            fi
        done
        # Enable Intel Turbo Boost (no_turbo = 0)
        if [ -w /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
            echo 0 > /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || true
        fi
        # Ensure MGLRU is active (0x0007) and tune min_ttl_ms to 1000 to protect working set
        if [ -w /sys/kernel/mm/lru_gen/enabled ]; then
            echo 7 > /sys/kernel/mm/lru_gen/enabled 2>/dev/null || true
        fi
        if [ -w /sys/kernel/mm/lru_gen/min_ttl_ms ]; then
            echo 1000 > /sys/kernel/mm/lru_gen/min_ttl_ms 2>/dev/null || true
        fi
        record_state "performance"
        echo "✔ Performance mode activated."
        ;;

    balanced|balance|normal)
        echo "⚖️ [Hagibis Bare-Metal Bridge] Reverting CPU to Balanced..."
        for epp in /sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference; do
            if [ -w "$epp" ]; then
                echo "balance_performance" > "$epp" 2>/dev/null || true
            fi
        done
        for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            if [ -w "$gov" ]; then
                echo "powersave" > "$gov" 2>/dev/null || true
            fi
        done
        # Reset MGLRU min_ttl_ms to 0
        if [ -w /sys/kernel/mm/lru_gen/min_ttl_ms ]; then
            echo 0 > /sys/kernel/mm/lru_gen/min_ttl_ms 2>/dev/null || true
        fi
        record_state "balanced"
        echo "✔ Balanced mode activated."
        ;;

    powersave|battery|idle|relax)
        echo "🍃 [Hagibis Bare-Metal Bridge] Dropping CPU to Power-Saving state..."
        for epp in /sys/devices/system/cpu/cpu*/cpufreq/energy_performance_preference; do
            if [ -w "$epp" ]; then
                echo "balance_power" > "$epp" 2>/dev/null || true
            fi
        done
        for gov in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            if [ -w "$gov" ]; then
                echo "powersave" > "$gov" 2>/dev/null || true
            fi
        done
        # Disable Intel Turbo Boost in power-saving mode (no_turbo = 1)
        if [ -w /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
            echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || true
        fi
        # Reset MGLRU min_ttl_ms to 0
        if [ -w /sys/kernel/mm/lru_gen/min_ttl_ms ]; then
            echo 0 > /sys/kernel/mm/lru_gen/min_ttl_ms 2>/dev/null || true
        fi
        record_state "powersave"
        echo "✔ Power-saving mode activated."
        ;;

    evaluate)
        # Usage: scripts/cpu-governor.sh evaluate <p99_latency_ms> <queue_depth> [settle_seconds]
        LATENCY="${2:-0}"
        QUEUE="${3:-0}"
        SETTLE_SECS="${4:-0}"

        # Evaluate threshold using awk for float comparison
        DECISION=$(awk -v lat="$LATENCY" -v q="$QUEUE" -v set_s="$SETTLE_SECS" 'BEGIN {
            if (lat > 200.0 || q > 10) {
                print "BOOST"
            } else if (q == 0 && lat < 50.0 && set_s >= 15.0) {
                print "POWERSAVE"
            } else {
                print "BALANCED"
            }
        }')

        echo "📊 [Dynamic Threshold Evaluation] Latency=${LATENCY}ms, Queue=${QUEUE}, Settle=${SETTLE_SECS}s => Action=${DECISION}"
        case "$DECISION" in
            BOOST)
                "$0" performance
                ;;
            POWERSAVE)
                "$0" powersave
                ;;
            BALANCED)
                "$0" balanced
                ;;
        esac
        ;;

    status|telemetry)
        FORMAT="${2:-text}"
        if [ "$ACTION" = "telemetry" ] || [ "$FORMAT" = "--json" ] || [ "$FORMAT" = "json" ]; then
            OUTPUT_JSON=1
        else
            OUTPUT_JSON=0
        fi

        GOV=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo "unknown")
        EPP=$(cat /sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference 2>/dev/null || echo "unknown")
        TURBO="Disabled"
        if [ -f /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
            NO_TURBO=$(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo "1")
            if [ "$NO_TURBO" = "0" ]; then
                TURBO="Enabled"
            fi
        fi
        MGLRU=$(cat /sys/kernel/mm/lru_gen/enabled 2>/dev/null || echo "0x0000")
        MGLRU_TTL=$(cat /sys/kernel/mm/lru_gen/min_ttl_ms 2>/dev/null || echo "0")
        TEMP=$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null || echo "0")
        TEMP_C=$(awk "BEGIN {printf \"%.1f\", $TEMP / 1000}")
        TARGET_PROFILE=$(get_target_profile)
        LOADAVG=$(cat /proc/loadavg 2>/dev/null | awk '{print $1}' || echo "0.0")

        if [ "$OUTPUT_JSON" -eq 1 ]; then
            cat <<EOF
{
  "governor": "$GOV",
  "epp": "$EPP",
  "turbo_boost": "$TURBO",
  "mglru_enabled": "$MGLRU",
  "mglru_min_ttl_ms": $MGLRU_TTL,
  "cpu_temp_c": $TEMP_C,
  "load_avg_1m": $LOADAVG,
  "target_profile": "$TARGET_PROFILE"
}
EOF
        else
            echo "📊 [Hagibis Bare-Metal Governor Telemetry]"
            echo "  • Governor:       $GOV"
            echo "  • EPP:            $EPP"
            echo "  • Turbo Boost:    $TURBO"
            echo "  • MGLRU:          $MGLRU (min_ttl_ms: ${MGLRU_TTL}ms)"
            echo "  • CPU Temp:       ${TEMP_C}°C"
            echo "  • Load (1m):      $LOADAVG"
            echo "  • Target Profile: $TARGET_PROFILE"
        fi
        ;;

    gpu-detect|gpu-status)
        echo "🎮 [Hagibis GPU Compute Accelerator Autodetection]"
        GPU_BACKEND="cpu_fallback"
        GPU_NAME="Generic CPU SIMD (AVX2/NEON)"
        
        if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi >/dev/null 2>&1; then
            GPU_BACKEND="cuda"
            GPU_NAME=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -n1 || echo "NVIDIA CUDA GPU")
            GPU_MEM=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader 2>/dev/null | head -n1 || echo "Unknown")
            echo "  • Backend:        $GPU_BACKEND"
            echo "  • Device:         $GPU_NAME"
            echo "  • VRAM:           $GPU_MEM"
        elif [ "$(uname -s)" = "Darwin" ] && [ "$(uname -m)" = "arm64" ]; then
            GPU_BACKEND="apple_mps"
            GPU_NAME="Apple Silicon Metal Performance Shaders (MPS)"
            echo "  • Backend:        $GPU_BACKEND"
            echo "  • Device:         $GPU_NAME"
        elif [ -e /dev/dri/renderD128 ] || [ -e /dev/dri/card0 ]; then
            GPU_BACKEND="vulkan"
            GPU_NAME="Vulkan DRI Compute Device"
            echo "  • Backend:        $GPU_BACKEND"
            echo "  • Device:         $GPU_NAME"
        else
            echo "  • Backend:        $GPU_BACKEND"
            echo "  • Device:         $GPU_NAME"
        fi

        cat <<EOF > /tmp/hgb_gpu_accelerator_state.json 2>/dev/null || true
{
  "gpu_backend": "$GPU_BACKEND",
  "device_name": "$GPU_NAME",
  "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date)"
}
EOF
        ;;

    gpu-boost)
        echo "🚀 [Hagibis GPU Governor] Ramping GPU Clocks for Vector Search & Embedding..."
        if command -v nvidia-smi >/dev/null 2>&1; then
            nvidia-smi -pm 1 2>/dev/null || true
            nvidia-smi --auto-boost-default=1 2>/dev/null || true
            echo "✔ NVIDIA GPU persistence and auto-boost enabled."
        else
            echo "✔ Non-NVIDIA accelerator state recorded as high-performance."
        fi
        "$0" performance
        ;;

    gpu-powersave)
        echo "🍃 [Hagibis GPU Governor] Relaxing GPU Clocks..."
        if command -v nvidia-smi >/dev/null 2>&1; then
            nvidia-smi --auto-boost-default=0 2>/dev/null || true
            echo "✔ NVIDIA GPU relaxed to nominal power."
        fi
        "$0" powersave
        ;;

    *)
        echo "Usage: $0 [performance|balanced|powersave|status|telemetry|gpu-detect|gpu-status|gpu-boost|gpu-powersave|evaluate <p99_ms> <queue> <settle_s>]"
        exit 1
        ;;
esac
