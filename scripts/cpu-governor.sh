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

    *)
        echo "Usage: $0 [performance|balanced|powersave|status|telemetry|evaluate <p99_ms> <queue> <settle_s>]"
        exit 1
        ;;
esac
