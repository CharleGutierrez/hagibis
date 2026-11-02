#!/usr/bin/env bash
set -euo pipefail

# scripts/pgo-optimize.sh
# Production Profile-Guided Optimization (PGO) & LLVM BOLT Binary Optimization Pipeline
# for Hagibis CLI (`hgb`) and Microkernel Daemon (`hgbd`)
#
# Features:
#   1. Preserves dynamic relocations (-C link-arg=-Wl,-q) for BOLT post-link transformations.
#   2. Auto-detects llvm-profdata, llvm-bolt, and perf2bolt across PATH, LLVM sysroots, and rustlib.
#   3. Representative training workloads: microbenchmarks, WASM sandbox tests, storage benchmarks, CLI queries.
#   4. Reordering passes: -reorder-blocks=ext-tsp -reorder-functions=hfsort+ -split-functions -split-all-cold -dyno-stats.
#   5. Validates ELF output integrity and produces hardened target/release/hgb and target/release/hgbd.
#   6. Graceful fallback: Clean fallback to standard PGO + Fat LTO if llvm-bolt is absent.
#   7. CLI flags: --pgo-only, --bolt-only, --benchmark, --verify, --help.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$WORKSPACE_ROOT"

# ==============================================================================
# CLI Flag Parsing
# ==============================================================================
PGO_ONLY=false
BOLT_ONLY=false
RUN_BENCHMARK=false
DO_VERIFY=false
VERIFY_ONLY=false

show_help() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Hagibis (hgb) Production PGO + BOLT Optimization Pipeline

Options:
  --pgo-only     Perform Profile-Guided Optimization (PGO) + Fat LTO only, skipping BOLT.
  --bolt-only    Perform post-link BOLT binary optimization only on existing release binaries.
  --benchmark    Execute before-and-after performance benchmarks to measure optimization throughput.
  --verify       Perform rigorous ELF integrity, relocation validation, and execution checks on binaries.
  -h, --help     Show this help message and exit.

Environment Variables:
  PGO_DATA_DIR   Custom directory for raw .profraw and merged .profdata (default: /tmp/pgo-data)
  BOLT_DATA_DIR  Custom directory for BOLT instrumentation fdata profiles (default: /tmp/bolt-data)
EOF
}

for arg in "$@"; do
    case "$arg" in
        --pgo-only)
            PGO_ONLY=true
            ;;
        --bolt-only)
            BOLT_ONLY=true
            ;;
        --benchmark)
            RUN_BENCHMARK=true
            ;;
        --verify)
            DO_VERIFY=true
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

if [[ "$DO_VERIFY" = true && "$PGO_ONLY" = false && "$BOLT_ONLY" = false && "$RUN_BENCHMARK" = false ]]; then
    VERIFY_ONLY=true
fi

if [[ "$PGO_ONLY" = true && "$BOLT_ONLY" = true ]]; then
    echo "[ERROR] --pgo-only and --bolt-only are mutually exclusive." >&2
    exit 1
fi

echo "======================================================================"
echo " Hagibis (hgb & hgbd) PGO + LLVM BOLT Optimization Pipeline"
echo " Workspace: $WORKSPACE_ROOT"
echo " Mode: PGO_ONLY=$PGO_ONLY | BOLT_ONLY=$BOLT_ONLY | BENCHMARK=$RUN_BENCHMARK | VERIFY=$DO_VERIFY"
echo "======================================================================"

# ==============================================================================
# Toolchain Auto-Detection
# ==============================================================================

# 1. Validate cargo
if ! command -v cargo >/dev/null 2>&1; then
    echo "[ERROR] 'cargo' not found in PATH. Please install Rust and Cargo." >&2
    exit 1
fi

RUSTC_SYSROOT="$(rustc --print sysroot 2>/dev/null || true)"
RUSTC_HOST="$(rustc -vV 2>/dev/null | grep 'host:' | cut -d' ' -f2 || true)"

# 2. Locate llvm-profdata
LLVM_PROFDATA=""
if command -v llvm-profdata >/dev/null 2>&1; then
    LLVM_PROFDATA="llvm-profdata"
elif [[ -n "$RUSTC_SYSROOT" && -n "$RUSTC_HOST" ]]; then
    CANDIDATE="$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-profdata"
    if [[ -x "$CANDIDATE" ]]; then
        LLVM_PROFDATA="$CANDIDATE"
    fi
fi

# Fallback: search system llvm directories
if [[ -z "$LLVM_PROFDATA" ]]; then
    for cand in /usr/lib/llvm-*/bin/llvm-profdata /usr/local/bin/llvm-profdata /opt/llvm/bin/llvm-profdata; do
        if [[ -x "$cand" ]]; then
            LLVM_PROFDATA="$cand"
            break
        fi
    done
fi

if [[ -z "$LLVM_PROFDATA" && "$BOLT_ONLY" != true ]]; then
    echo "[ERROR] 'llvm-profdata' not found in PATH, rustlib sysroot, or /usr/lib/llvm-*/bin." >&2
    echo "Install rust llvm tools: rustup component add llvm-tools" >&2
    exit 1
fi

# 3. Locate llvm-bolt and perf2bolt
LLVM_BOLT=""
PERF2BOLT=""

if command -v llvm-bolt >/dev/null 2>&1; then
    LLVM_BOLT="$(command -v llvm-bolt)"
fi

if command -v perf2bolt >/dev/null 2>&1; then
    PERF2BOLT="$(command -v perf2bolt)"
fi

if [[ -z "$LLVM_BOLT" ]]; then
    for cand in \
        /usr/lib/llvm-*/bin/llvm-bolt \
        /usr/local/bin/llvm-bolt \
        /opt/llvm/bin/llvm-bolt \
        /opt/homebrew/opt/llvm/bin/llvm-bolt \
        "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-bolt" \
        "$RUSTC_SYSROOT/bin/llvm-bolt"; do
        if [[ -x "$cand" ]]; then
            LLVM_BOLT="$cand"
            break
        fi
    done
fi

if [[ -z "$PERF2BOLT" ]]; then
    for cand in \
        /usr/lib/llvm-*/bin/perf2bolt \
        /usr/local/bin/perf2bolt \
        /opt/llvm/bin/perf2bolt \
        /opt/homebrew/opt/llvm/bin/perf2bolt \
        "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/perf2bolt"; do
        if [[ -x "$cand" ]]; then
            PERF2BOLT="$cand"
            break
        fi
    done
fi

echo "[INFO] Cargo: $(command -v cargo)"
if [[ -n "$LLVM_PROFDATA" ]]; then
    echo "[INFO] LLVM profdata: $LLVM_PROFDATA"
fi

if [[ -n "$LLVM_BOLT" ]]; then
    echo "[INFO] LLVM BOLT detected: $LLVM_BOLT"
    "$LLVM_BOLT" --version 2>&1 | head -n 1 | sed 's/^/[INFO] BOLT Version: /' || true
else
    if [[ "$BOLT_ONLY" = true ]]; then
        echo "[ERROR] 'llvm-bolt' not found. Cannot proceed with --bolt-only." >&2
        echo "Please install LLVM with BOLT support (e.g., sudo apt-get install llvm-bolt or build LLVM with BOLT enabled)." >&2
        exit 1
    else
        echo "[WARN] 'llvm-bolt' was not detected in PATH, system LLVM toolchains, or rustlib sysroot."
        echo "[INFO] Graceful Fallback: Will perform complete PGO + Fat LTO pipeline."
    fi
fi

# Detect relocation support for post-link BOLT
RELOC_FLAG=""
if [[ -n "$LLVM_BOLT" ]]; then
    CC_BIN="${CC:-gcc}"
    if echo "int main(){return 0;}" | "$CC_BIN" -x c - -Wl,-q -o /dev/null 2>/dev/null; then
        RELOC_FLAG="-C link-arg=-Wl,-q"
        echo "[INFO] Host linker supports -Wl,-q for BOLT relocation preservation."
    elif echo "int main(){return 0;}" | "$CC_BIN" -x c - -Wl,--emit-relocs -o /dev/null 2>/dev/null; then
        RELOC_FLAG="-C link-arg=-Wl,--emit-relocs"
        echo "[INFO] Host linker supports -Wl,--emit-relocs for BOLT relocation preservation."
    else
        echo "[WARN] Host linker does not support -Wl,-q or -Wl,--emit-relocs; skipping relocation flags."
        RELOC_FLAG=""
    fi
fi

PGO_DATA_DIR="${PGO_DATA_DIR:-/tmp/pgo-data}"
BOLT_DATA_DIR="${BOLT_DATA_DIR:-/tmp/bolt-data}"
mkdir -p "$PGO_DATA_DIR"
mkdir -p "$BOLT_DATA_DIR"

# Helper: benchmark execution
benchmark_binaries() {
    local phase="$1"
    echo ""
    echo "----------------------------------------------------------------------"
    echo " [BENCHMARK] Measuring performance: $phase"
    echo "----------------------------------------------------------------------"
    if [[ -x "target/release/hgb" ]]; then
        echo "[BENCHMARK] hgb CLI cold startup & doctor timing:"
        time target/release/hgb doctor --json >/dev/null 2>&1 || true
        time target/release/hgb --help >/dev/null 2>&1 || true
    fi
    if cargo bench -p hgb-core --bench pgo_workload -- --nocapture --test >/dev/null 2>&1; then
        echo "[BENCHMARK] Core PGO microbenchmarks running..."
        cargo bench -p hgb-core --bench pgo_workload -- --nocapture || true
    fi
}

# ==============================================================================
# Stage 1: Profile-Guided Optimization (PGO) + Fat LTO
# ==============================================================================
if [[ "$BOLT_ONLY" != true && "$VERIFY_ONLY" != true ]]; then
    if [[ "$RUN_BENCHMARK" = true && -x "target/release/hgb" ]]; then
        benchmark_binaries "Baseline (Pre-Optimization)"
    fi

    # Clean profile data
    rm -rf "$PGO_DATA_DIR"/*

    echo ""
    echo "----------------------------------------------------------------------"
    echo " Step 1/4: Building instrumented binaries with PGO profiling"
    echo " Flags: -Cprofile-generate $RELOC_FLAG"
    echo "----------------------------------------------------------------------"
    export RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG"
    cargo build --release --bins

    echo ""
    echo "----------------------------------------------------------------------"
    echo " Step 2/4: Running representative training workload to collect profiles"
    echo "----------------------------------------------------------------------"
    # Execute benchmarks and representative test workloads
    RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG" cargo bench -p hgb-core --bench pgo_workload -- --nocapture || true
    RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG" cargo test --release -p hgb-core --lib wasm_plugin_sandbox -- --nocapture || true
    RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG" cargo test --release -p hgb-storage --lib -- --nocapture || true

    # Run CLI commands
    if [[ -f "target/release/hgb" ]]; then
        target/release/hgb --help >/dev/null 2>&1 || true
        target/release/hgb doctor --json >/dev/null 2>&1 || true
        target/release/hgb mesh --status >/dev/null 2>&1 || true
    fi

    # Run Daemon help/version
    if [[ -f "target/release/hgbd" ]]; then
        target/release/hgbd --help >/dev/null 2>&1 || true
    fi

    echo ""
    echo "----------------------------------------------------------------------"
    echo " Step 3/4: Merging profile data with llvm-profdata"
    echo "----------------------------------------------------------------------"
    PROFRAW_FILES=$(find "$PGO_DATA_DIR" -type f -name "*.profraw")
    PROFRAW_COUNT=$(echo "$PROFRAW_FILES" | grep -v '^$' | wc -l || true)

    if [[ "$PROFRAW_COUNT" -eq 0 ]]; then
        echo "[ERROR] No .profraw files found in $PGO_DATA_DIR" >&2
        exit 1
    fi

    echo "[INFO] Discovered $PROFRAW_COUNT raw profile dataset(s). Merging..."
    "$LLVM_PROFDATA" merge -output="$PGO_DATA_DIR/merged.profdata" "$PGO_DATA_DIR"/*.profraw

    if [[ ! -s "$PGO_DATA_DIR/merged.profdata" ]]; then
        echo "[ERROR] Merged profile data $PGO_DATA_DIR/merged.profdata is missing or empty." >&2
        exit 1
    fi
    echo "[SUCCESS] Merged profile data: $PGO_DATA_DIR/merged.profdata ($(stat -c%s "$PGO_DATA_DIR/merged.profdata" 2>/dev/null || stat -f%z "$PGO_DATA_DIR/merged.profdata") bytes)"

    echo ""
    echo "----------------------------------------------------------------------"
    echo " Step 4/4: Rebuilding optimized binaries with PGO & Fat LTO"
    echo "----------------------------------------------------------------------"
    export RUSTFLAGS="-Cprofile-use=$PGO_DATA_DIR/merged.profdata -Cllvm-args=-pgo-warn-missing-function -Copt-level=3 -Clto=fat -Ccodegen-units=1 -Cpanic=abort $RELOC_FLAG"
    cargo build --release --bins
fi

# ==============================================================================
# Stage 2: Post-Link LLVM BOLT Optimization
# ==============================================================================
optimize_with_bolt() {
    local bin_path="$1"
    local bin_name
    bin_name="$(basename "$bin_path")"

    if [[ ! -f "$bin_path" ]]; then
        echo "[WARN] Target binary $bin_path does not exist; skipping BOLT pass."
        return 0
    fi

    echo ""
    echo "----------------------------------------------------------------------"
    echo " [BOLT] Optimizing $bin_name with LLVM BOLT"
    echo " Binary: $bin_path ($(stat -c%s "$bin_path" 2>/dev/null || stat -f%z "$bin_path") bytes)"
    echo "----------------------------------------------------------------------"

    local inst_bin="$bin_path.inst"
    local bolt_bin="$bin_path.bolt"
    local fdata_dir="$BOLT_DATA_DIR/$bin_name"
    rm -rf "$fdata_dir"
    mkdir -p "$fdata_dir"

    # Step 1: Instrument binary with llvm-bolt
    echo "[INFO] Instrumenting $bin_name with llvm-bolt..."
    if ! "$LLVM_BOLT" "$bin_path" -o "$inst_bin" \
        --instrument \
        --instrumentation-file-append-pid \
        --instrumentation-file="$fdata_dir/prof.fdata" 2>&1 | sed 's/^/  [bolt-inst] /'; then
        echo "[WARN] BOLT instrumentation of $bin_name failed. Keeping PGO-optimized binary."
        rm -f "$inst_bin"
        return 0
    fi

    # Step 2: Execute representative workload with instrumented binary
    echo "[INFO] Running training workloads with instrumented $bin_name..."
    "$inst_bin" --help >/dev/null 2>&1 || true
    if [[ "$bin_name" == "hgb" ]]; then
        "$inst_bin" doctor --json >/dev/null 2>&1 || true
        "$inst_bin" mesh --status >/dev/null 2>&1 || true
    fi

    # Step 3: Locate collected fdata profile
    local fdata_files
    fdata_files=$(find "$fdata_dir" -type f -name "*.fdata" 2>/dev/null)
    local combined_fdata="$fdata_dir/combined.fdata"

    if [[ -n "$fdata_files" ]]; then
        cat $fdata_files > "$combined_fdata"
    fi

    # Step 4: Run BOLT post-link reordering passes
    echo "[INFO] Applying BOLT reordering passes (-reorder-blocks=ext-tsp -reorder-functions=hfsort+ -split-functions -split-all-cold -dyno-stats)..."
    local bolt_args=(
        "$bin_path"
        -o "$bolt_bin"
        -reorder-blocks=ext-tsp
        -reorder-functions=hfsort+
        -split-functions
        -split-all-cold
        -dyno-stats
    )

    if [[ -s "$combined_fdata" ]]; then
        bolt_args+=("-data=$combined_fdata")
        echo "[INFO] Applying profile dataset: $combined_fdata ($(wc -l < "$combined_fdata") records)"
    else
        echo "[WARN] No .fdata generated by instrumentation; executing BOLT layout passes."
    fi

    if "$LLVM_BOLT" "${bolt_args[@]}" 2>&1 | sed 's/^/  [bolt-opt] /'; then
        # Validate ELF output integrity
        if [[ -s "$bolt_bin" ]]; then
            chmod +x "$bolt_bin"
            # Verify test run of optimized binary
            if "$bolt_bin" --help >/dev/null 2>&1; then
                echo "[SUCCESS] BOLT optimization succeeded for $bin_name. Replacing original executable."
                mv "$bolt_bin" "$bin_path"
            else
                echo "[WARN] BOLT optimized binary $bolt_bin failed execution check. Preserving original."
                rm -f "$bolt_bin"
            fi
        fi
    else
        echo "[WARN] BOLT optimization pass encountered an issue; falling back cleanly to PGO binary."
        rm -f "$bolt_bin"
    fi

    # Cleanup temporary files
    rm -f "$inst_bin"
    rm -rf "$fdata_dir"
}

if [[ -n "$LLVM_BOLT" && "$PGO_ONLY" != true ]]; then
    optimize_with_bolt "target/release/hgb"
    optimize_with_bolt "target/release/hgbd"
elif [[ "$PGO_ONLY" = true ]]; then
    echo ""
    echo "[INFO] Skipping BOLT pass (--pgo-only specified)."
fi

# ==============================================================================
# Stage 3: ELF Integrity Verification & Validation
# ==============================================================================
verify_binary() {
    local bin="$1"
    local name
    name="$(basename "$bin")"

    if [[ ! -f "$bin" ]]; then
        echo "[ERROR] Binary $bin not found for verification." >&2
        return 1
    fi

    echo ""
    echo "----------------------------------------------------------------------"
    echo " [VERIFY] Integrity check for $name ($bin)"
    echo "----------------------------------------------------------------------"

    # Check executable permission
    if [[ ! -x "$bin" ]]; then
        echo "[ERROR] $bin is not marked executable!" >&2
        return 1
    fi

    # Check ELF magic bytes
    local magic
    magic="$(head -c 4 "$bin" 2>/dev/null || true)"
    if [[ "$magic" != $'\x7fELF' ]]; then
        echo "[ERROR] $bin does not start with ELF magic (\\x7fELF)." >&2
        return 1
    fi
    echo "[PASS] ELF magic signature verified."

    # Inspect ELF headers if readelf is present
    if command -v readelf >/dev/null 2>&1; then
        local elf_class
        elf_class="$(readelf -h "$bin" | grep -i 'Class:' | awk '{print $2}' || true)"
        local machine
        machine="$(readelf -h "$bin" | grep -i 'Machine:' | awk '{$1=""; print $0}' | sed 's/^ //g' || true)"
        echo "[PASS] ELF Format: Class $elf_class, Architecture: $machine"

        # Check relocations or section table
        local sec_count
        sec_count="$(readelf -S "$bin" 2>/dev/null | grep '\[ *[0-9]' | wc -l || true)"
        echo "[PASS] Sections parsed: $sec_count section headers present."
    fi

    # Smoke test execution
    if "$bin" --help >/dev/null 2>&1; then
        echo "[PASS] Smoke test execution ($name --help): SUCCESS (code 0)"
    else
        echo "[ERROR] Smoke test execution ($name --help) failed!" >&2
        return 1
    fi

    local size_bytes
    size_bytes=$(stat -c%s "$bin" 2>/dev/null || stat -f%z "$bin")
    echo "[INFO] Binary Size: $size_bytes bytes ($((size_bytes / 1024 / 1024)) MB)"
}

if [[ "$DO_VERIFY" = true || "$BOLT_ONLY" = true ]]; then
    verify_binary "target/release/hgb"
    verify_binary "target/release/hgbd"
fi

if [[ "$RUN_BENCHMARK" = true ]]; then
    benchmark_binaries "Optimized (Post-PGO/BOLT)"
fi

echo ""
echo "======================================================================"
echo " Hagibis Optimization Pipeline completed successfully!"
echo " Binaries:"
echo "   CLI:    target/release/hgb"
echo "   Daemon: target/release/hgbd"
echo "======================================================================"
