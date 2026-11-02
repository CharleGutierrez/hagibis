#!/usr/bin/env bash
# ==============================================================================
# Hagibis Bare-Metal Kernel Governor Controller
# Cooperates with dynatune and kernel-optimizer on Toshiba Dynabook VC72/B
# ==============================================================================

set -e

ACTION="${1:-status}"

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
        echo "✔ Power-saving mode activated."
        ;;
    status)
        echo "📊 [Hagibis Bare-Metal Governor Telemetry]"
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
        TEMP=$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null || echo "0")
        TEMP_C=$(awk "BEGIN {print $TEMP / 1000}")
        echo "  • Governor:    $GOV"
        echo "  • EPP:         $EPP"
        echo "  • Turbo Boost: $TURBO"
        echo "  • MGLRU:       $MGLRU"
        echo "  • CPU Temp:    ${TEMP_C}°C"
        ;;
    *)
        echo "Usage: $0 [performance|balanced|powersave|status]"
        exit 1
        ;;
esac
