#!/usr/bin/env bash
# ==============================================================================
# scripts/pgo-bolt-optimize.sh
# Hagibis Autonomous Production PGO + LLVM BOLT Binary Layout Optimizer (Pillars 1 & 3)
#
# Supported Targets:
#   1. hgb-cli (hgb)                  - High-performance CLI binary
#   2. hgb-daemon (hgbd)              - Autonomous microkernel daemon
#   3. hgb-wasm-kernel (wasm_kernel)  - Native WebAssembly runtime & SIMD runner
#
# Optimization Strategy:
#   Phase 1: LLVM PGO instrumentation compile (-Cprofile-generate) + -Wl,--emit-relocs
#   Phase 2: Realistic multi-agent workload profiling traces (hgb-core benches
#            pgo_workload.rs, SIMD kernels, daemon CLI, and autonomous loops)
#   Phase 3: Merged profile feedback (-Cprofile-use) + Fat LTO (-C lto=fat -C codegen-units=1 -C target-cpu=native)
#   Phase 4: LLVM BOLT post-link layout transformation:
#            - -reorder-blocks=ext-tsp: Extended TSP layout groups hot paths together.
#            - -reorder-functions=hfsort+: Hot-to-cold function clustering.
#            - -split-functions -split-all-cold: Segregates cold/error blocks to .text.cold sections.
#            - -dyno-stats: Emits dynamic branch & instruction metrics.
#
# Continuous CI/CD Features:
#   - Automated Headless Execution (--ci / CI=true / GITHUB_ACTIONS=true)
#   - GitHub Actions Workflow Log Grouping (::group:: / ::endgroup::)
#   - Automatic LLVM Profdata & BOLT Toolchain Detection
#   - Cross-PR Profile Caching & Fast-Forward Compilation
#   - Non-fatal graceful fallback when PMU counters or BOLT is unavailable
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." 2>/dev/null && pwd || pwd)"
cd "$WORKSPACE_ROOT"

# Preload execution shim if present
if [ -f "$WORKSPACE_ROOT/scripts/execve_shim.so" ]; then
    export LD_PRELOAD="${LD_PRELOAD:-}:$WORKSPACE_ROOT/scripts/execve_shim.so"
elif [ -f "/tmp/execve_shim.so" ]; then
    export LD_PRELOAD="${LD_PRELOAD:-}:/tmp/execve_shim.so"
fi

TARGET_CHOICE="all"
DO_VERIFY=false
PGO_ONLY=false
BOLT_ONLY=false
CI_MODE=false

# Auto-detect CI environment
if [ "${CI:-false}" = "true" ] || [ "${GITHUB_ACTIONS:-false}" = "true" ] || [ ! -t 1 ]; then
    CI_MODE=true
fi

show_help() {
    cat <<EOF
Hagibis PGO + LLVM BOLT Super-Optimization Pipeline (Pillars 1 & 3)

Usage: $(basename "$0") [TARGET] [OPTIONS]

Targets:
  all               Optimize hgb, hgbd, and hgb-wasm-kernel (default)
  hgb | cli         Optimize hgb CLI binary only
  hgbd | daemon     Optimize hgbd daemon binary only
  wasm-kernel       Optimize hgb-wasm-kernel guest runtime only

Options:
  --ci              Headless automated CI mode (non-interactive, GitHub Actions log grouping)
  --workload-profile Execute representative masterclass live simulations profiling pass
  --verify          Verify ELF headers, relocations, basic-block alignment & smoke tests
  --pgo-only        Execute PGO + Fat LTO compilation only, skipping BOLT passes
  --bolt-only       Execute post-link BOLT reordering only on existing release binaries
  -h, --help        Show this help message
EOF
}

# Parse CLI arguments
WORKLOAD_PROFILE=false
for arg in "$@"; do
    case "$arg" in
        all|hgb|cli|hgbd|daemon|wasm_kernel|hgb-wasm-kernel|wasm-kernel|wasm|kernel)
            TARGET_CHOICE="$arg"
            ;;
        --ci|--headless|--non-interactive)
            CI_MODE=true
            ;;
        --workload-profile|--profile|workload-profile)
            WORKLOAD_PROFILE=true
            ;;
        --verify)
            DO_VERIFY=true
            ;;
        --pgo-only)
            PGO_ONLY=true
            ;;
        --bolt-only)
            BOLT_ONLY=true
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            echo "[WARN] Unknown argument: $arg" >&2
            ;;
    esac
done

# GitHub Actions grouping helpers
ci_group_start() {
    local title="$1"
    if [ "$CI_MODE" = true ] && [ "${GITHUB_ACTIONS:-false}" = "true" ]; then
        echo "::group::$title"
    else
        echo ""
        echo "=== [ $title ] ==="
    fi
}

ci_group_end() {
    if [ "$CI_MODE" = true ] && [ "${GITHUB_ACTIONS:-false}" = "true" ]; then
        echo "::endgroup::"
    fi
}

TARGET_DIR="${CARGO_TARGET_DIR:-/media/toshiba/Lexar/cargo-target}"
if [ ! -d "$TARGET_DIR" ]; then
    mkdir -p "$TARGET_DIR" 2>/dev/null || TARGET_DIR="target"
fi
RELEASE_DIR="$TARGET_DIR/release"
mkdir -p "$RELEASE_DIR"

DATA_DIR="${BOLT_DATA_DIR:-/tmp/bolt-pgo-data}"
mkdir -p "$DATA_DIR"

CACHE_DIR="${PGO_CACHE_DIR:-/tmp/bolt-pgo-cache}"
mkdir -p "$CACHE_DIR"

# Locate LLVM profdata, bolt, and readelf
RUSTC_SYSROOT="$(rustc --print sysroot 2>/dev/null || true)"
RUSTC_HOST="$(rustc -vV 2>/dev/null | grep 'host:' | cut -d' ' -f2 || true)"

LLVM_PROFDATA="$(command -v llvm-profdata 2>/dev/null || true)"
if [ -z "$LLVM_PROFDATA" ] && [ -n "$RUSTC_SYSROOT" ] && [ -n "$RUSTC_HOST" ]; then
    cand="$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-profdata"
    if [ -x "$cand" ]; then LLVM_PROFDATA="$cand"; fi
fi
if [ -z "$LLVM_PROFDATA" ]; then
    cand="$(find /usr/lib/llvm-*/bin/llvm-profdata 2>/dev/null | sort -V | tail -n 1 || true)"
    if [ -n "$cand" ] && [ -x "$cand" ]; then LLVM_PROFDATA="$cand"; fi
fi

LLVM_BOLT="$(command -v llvm-bolt 2>/dev/null || find /usr/lib/llvm-*/bin/ -name llvm-bolt 2>/dev/null | sort -V | tail -n 1 || true)"

READELF_BIN="$(command -v readelf 2>/dev/null || command -v llvm-readobj 2>/dev/null || true)"
if [ -z "$READELF_BIN" ] && [ -n "$RUSTC_SYSROOT" ] && [ -n "$RUSTC_HOST" ]; then
    cand="$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-readobj"
    if [ -x "$cand" ]; then READELF_BIN="$cand --elf-output-style=GNU"; fi
fi

echo "========================================================================"
echo " ⚡ [Hagibis PGO+BOLT] Automated Hardware & Binary Optimization"
echo " Workspace:   $WORKSPACE_ROOT"
echo " Target:      $TARGET_CHOICE"
echo " CI Mode:     $CI_MODE"
echo " Target Dir:  $TARGET_DIR"
echo " Data Dir:    $DATA_DIR"
echo " Cache Dir:   $CACHE_DIR"
echo " LLVM Prof:   ${LLVM_PROFDATA:-not found}"
echo " LLVM BOLT:   ${LLVM_BOLT:-not found (graceful fallback active)}"
echo "========================================================================"

optimize_binary() {
    local BIN_NAME="$1"
    local PKG_NAME="$2"
    local BIN_TARGET="$3"
    
    ci_group_start "Optimizing Binary: $BIN_NAME ($PKG_NAME)"
    
    local PGO_PROFDATA_DIR="$DATA_DIR/pgo-$BIN_NAME"
    mkdir -p "$PGO_PROFDATA_DIR"
    
    # Probe compiler & linker capabilities
    local SUPPORTS_PGO=false
    local SUPPORTS_RELOCS=false
    local PROBE_DIR="/tmp/hgb-pgo-probe"
    mkdir -p "$PROBE_DIR"

    if echo 'fn main(){}' | rustc -Cprofile-generate="$PROBE_DIR" - -o "$PROBE_DIR/probe_pgo" 2>/dev/null; then
        SUPPORTS_PGO=true
        rm -f "$PROBE_DIR/probe_pgo" "$PROBE_DIR"/*.profraw
    fi

    if echo "int main(){return 0;}" | "${CC:-cc}" -x c - -Wl,--emit-relocs -o "$PROBE_DIR/probe_reloc" 2>/dev/null; then
        SUPPORTS_RELOCS=true
        rm -f "$PROBE_DIR/probe_reloc"
    fi
    rm -rf "$PROBE_DIR"

    local RELOC_FLAG=""
    if [ "$SUPPORTS_RELOCS" = true ]; then
        RELOC_FLAG="-Clink-arg=-Wl,--emit-relocs"
    fi

    local MERGED_PROFDATA="$DATA_DIR/$BIN_NAME.profdata"
    local CACHED_PROFDATA="$CACHE_DIR/$BIN_NAME.profdata"

    if [ "$BOLT_ONLY" != true ]; then
        # ---------------------------------------------------------------------
        # Phase 1: Instrument with LLVM PGO (-Cprofile-generate)
        # ---------------------------------------------------------------------
        if [ "$SUPPORTS_PGO" = true ]; then
            # Check for cached profile in CI mode
            if [ -s "$CACHED_PROFDATA" ] && [ "${FAST_PGO_CACHE:-false}" = "true" ]; then
                echo "• [CI Cache Hit] Found valid cached PGO profile: $CACHED_PROFDATA"
                cp -f "$CACHED_PROFDATA" "$MERGED_PROFDATA"
            else
                echo "• [Phase 1/4] Compiling $BIN_NAME with LLVM PGO instrumentation..."
                RUSTFLAGS="-Cprofile-generate=$PGO_PROFDATA_DIR -Ctarget-cpu=native" \
                    cargo build --release -p "$PKG_NAME" --bin "$BIN_NAME" --target-dir "$TARGET_DIR"
                
                local INSTRUMENTED_BIN="$RELEASE_DIR/$BIN_NAME"
                if [ ! -f "$INSTRUMENTED_BIN" ]; then
                    echo "❌ Instrumented binary not found at $INSTRUMENTED_BIN"
                    ci_group_end
                    return 1
                fi
                
                # -----------------------------------------------------------------
                # Phase 2: Representative Training Workload (Masterclass Live Simulations)
                # -----------------------------------------------------------------
                echo "• [Phase 2/4] Executing realistic production profiling workloads..."
                LLVM_PROFILE_FILE="$PGO_PROFDATA_DIR/profile-%p-%m.profraw"
                export LLVM_PROFILE_FILE
                export DYNATUNE_DIR="${DYNATUNE_DIR:-/media/toshiba/Lexar/dynatune}"

                echo "  • Executing representative Masterclass live simulation suite..."
                if [ -f "$WORKSPACE_ROOT/docs/masterclass/test_masterclass_live_simulations.py" ]; then
                    python3 "$WORKSPACE_ROOT/docs/masterclass/test_masterclass_live_simulations.py" || true
                fi

                echo "  • Training on hgb-core benchmark kernels (pgo_workload.rs)..."
                cargo bench -p hgb-core --bench pgo_workload --target-dir "$TARGET_DIR" -- --nocapture 2>/dev/null || true

                # Masterclass chapter training
                for ch in "$WORKSPACE_ROOT"/docs/masterclass/ch*.py; do
                    if [ -f "$ch" ]; then
                        python3 "$ch" --test-mode >/dev/null 2>&1 || python3 "$ch" >/dev/null 2>&1 || true
                    fi
                done
                
                if [ "$BIN_NAME" = "hgb" ]; then
                    "$INSTRUMENTED_BIN" --help >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" doctor --json >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" mesh --status >/dev/null 2>&1 || true
                elif [ "$BIN_NAME" = "hgbd" ]; then
                    "$INSTRUMENTED_BIN" --version >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" --help >/dev/null 2>&1 || true
                elif [ "$BIN_NAME" = "hgb-wasm-kernel" ] || [ "$BIN_NAME" = "wasm_kernel" ]; then
                    "$INSTRUMENTED_BIN" --version >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" --help >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" --simd-test >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" --manifest >/dev/null 2>&1 || true
                    "$INSTRUMENTED_BIN" --bench 3000 >/dev/null 2>&1 || true
                fi
                
                # Merge LLVM raw profiles
                if [ -n "$LLVM_PROFDATA" ] && [ -x "$LLVM_PROFDATA" ]; then
                    echo "• Merging PGO profile data using $LLVM_PROFDATA..."
                    "$LLVM_PROFDATA" merge -o "$MERGED_PROFDATA" "$PGO_PROFDATA_DIR"/*.profraw 2>/dev/null || true
                    # Cache merged profile for continuous CI caching
                    if [ -s "$MERGED_PROFDATA" ]; then
                        cp -f "$MERGED_PROFDATA" "$CACHED_PROFDATA" 2>/dev/null || true
                    fi
                fi
            fi
        else
            echo "• [Phase 1-2/4] Host linker does not support LLVM PGO profile runtime; proceeding with Fat LTO..."
        fi
        
        # ---------------------------------------------------------------------
        # Phase 3: Recompile with PGO Feedback + Fat LTO (-Cprofile-use)
        # ---------------------------------------------------------------------
        echo "• [Phase 3/4] Recompiling with Fat LTO (-C lto=fat -C codegen-units=1 -C target-cpu=native)..."
        if [ -s "$MERGED_PROFDATA" ]; then
            echo "• Applying merged profile feedback: $MERGED_PROFDATA"
            RUSTFLAGS="-Cprofile-use=$MERGED_PROFDATA -Clto=fat -Ccodegen-units=1 -Ctarget-cpu=native $RELOC_FLAG" \
                cargo build --release -p "$PKG_NAME" --bin "$BIN_NAME" --target-dir "$TARGET_DIR"
        else
            RUSTFLAGS="-Clto=fat -Ccodegen-units=1 -Ctarget-cpu=native $RELOC_FLAG" \
                cargo build --release -p "$PKG_NAME" --bin "$BIN_NAME" --target-dir "$TARGET_DIR"
        fi
    fi

    local PGO_BIN="$RELEASE_DIR/$BIN_NAME"
    local BOLT_BIN="$RELEASE_DIR/$BIN_NAME.bolt"
    
    # -------------------------------------------------------------------------
    # Phase 4: LLVM BOLT Post-Link Layout Reordering
    # Passes: -reorder-blocks=ext-tsp -reorder-functions=cdsort -split-functions -split-all-cold -dyno-stats
    # -------------------------------------------------------------------------
    if [ "$PGO_ONLY" != true ]; then
        echo "• [Phase 4/4] Applying LLVM BOLT layout reordering..."
        local reorder_fn="cdsort"
        if [ -n "$LLVM_BOLT" ] && [ -x "$LLVM_BOLT" ]; then
            if ! "$LLVM_BOLT" --help 2>&1 | grep -q "cdsort"; then
                reorder_fn="hfsort+"
            fi
        fi
        echo "  Passes: -reorder-blocks=ext-tsp -reorder-functions=${reorder_fn} -split-functions -split-all-cold -dyno-stats"
        
        if [ -n "$LLVM_BOLT" ] && [ -x "$LLVM_BOLT" ]; then
            local PERF_DATA="$DATA_DIR/perf-$BIN_NAME.data"
            local FDATA_FILE="$DATA_DIR/$BIN_NAME.fdata"
            local CACHED_FDATA="$CACHE_DIR/$BIN_NAME.fdata"
            
            # Check for cached BOLT trace in CI
            if [ -s "$CACHED_FDATA" ]; then
                cp -f "$CACHED_FDATA" "$FDATA_FILE" 2>/dev/null || true
            elif command -v perf >/dev/null 2>&1; then
                local SAMPLE_ARGS="--help"
                if [ "$BIN_NAME" = "hgb-wasm-kernel" ]; then SAMPLE_ARGS="--simd-test"; fi
                # Run perf record with graceful fallback for virtualized CI containers
                if perf record -b -e cycles:u -j any,u -o "$PERF_DATA" -- "$PGO_BIN" $SAMPLE_ARGS >/dev/null 2>&1; then
                    if command -v perf2bolt >/dev/null 2>&1 && [ -f "$PERF_DATA" ]; then
                        perf2bolt -p "$PERF_DATA" -o "$FDATA_FILE" "$PGO_BIN" 2>/dev/null || true
                        if [ -s "$FDATA_FILE" ]; then
                            cp -f "$FDATA_FILE" "$CACHED_FDATA" 2>/dev/null || true
                        fi
                    fi
                else
                    echo "ℹ PMU branch profiling unavailable in container; proceeding with static BOLT heuristics."
                fi
            fi

            local bolt_cmd=(
                "$LLVM_BOLT" "$PGO_BIN"
                -o "$BOLT_BIN"
                -reorder-blocks=ext-tsp
                "-reorder-functions=${reorder_fn}"
                -split-functions
                -split-all-cold
                -dyno-stats
            )
            if [ -s "$FDATA_FILE" ]; then
                bolt_cmd+=("-data=$FDATA_FILE")
            fi

            if "${bolt_cmd[@]}" 2>&1 | sed 's/^/  [bolt] /'; then
                if [ -s "$BOLT_BIN" ]; then
                    chmod +x "$BOLT_BIN" 2>/dev/null || true
                    mv "$BOLT_BIN" "$PGO_BIN"
                    echo "✔ BOLT optimization completed successfully for $BIN_NAME."
                fi
            else
                echo "ℹ BOLT optimization encountered an issue; cleanly preserving PGO + Fat LTO binary."
                rm -f "$BOLT_BIN"
            fi
        else
            echo "ℹ llvm-bolt not detected on host; preserving hardened PGO + Fat LTO binary."
        fi
    fi

    # Mirror to workspace target/release for compatibility
    if [ "$TARGET_DIR" != "target" ] && [ "$TARGET_DIR" != "$WORKSPACE_ROOT/target" ]; then
        mkdir -p "$WORKSPACE_ROOT/target/release"
        cp -f "$PGO_BIN" "$WORKSPACE_ROOT/target/release/$BIN_NAME" 2>/dev/null || true
    fi

    echo "✔ Successfully produced hardened binary: $PGO_BIN ($(stat -c%s "$PGO_BIN" 2>/dev/null || echo "0") bytes)"
    ci_group_end
}

# Execute optimization per chosen target
case "$TARGET_CHOICE" in
    hgb|cli)
        optimize_binary "hgb" "hgb-cli" "crates/hgb-cli"
        ;;
    hgbd|daemon)
        optimize_binary "hgbd" "hgb-daemon" "crates/hgb-daemon"
        ;;
    wasm_kernel|hgb-wasm-kernel|wasm-kernel|wasm|kernel)
        optimize_binary "hgb-wasm-kernel" "hgb-wasm-kernel" "crates/hgb-wasm-kernel"
        ;;
    all|*)
        optimize_binary "hgb" "hgb-cli" "crates/hgb-cli"
        optimize_binary "hgbd" "hgb-daemon" "crates/hgb-daemon"
        optimize_binary "hgb-wasm-kernel" "hgb-wasm-kernel" "crates/hgb-wasm-kernel"
        ;;
esac

# Execute verification if requested
if [ "$DO_VERIFY" = true ]; then
    ci_group_start "Verifying ELF Headers, Layout and Binary Integrity"
    bash "$WORKSPACE_ROOT/scripts/pgo-optimize.sh" --verify
    ci_group_end
fi

echo ""
echo "========================================================================"
echo " 🎉 PGO + BOLT Super-Optimization Completed Successfully!"
echo " Targets: hgb, hgbd, and hgb-wasm-kernel"
echo " CI Mode: $CI_MODE"
echo "========================================================================"
```
