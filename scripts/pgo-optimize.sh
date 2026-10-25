#!/usr/bin/env bash
set -euo pipefail

# scripts/pgo-optimize.sh
# Production Profile-Guided Optimization (PGO) & Fat LTO Pipeline for Hagibis (hgb)
#
# Workflow:
#   1. Validate cargo and llvm-profdata availability.
#   2. Instrument binaries using -Cprofile-generate.
#   3. Run representative vector benchmarks and test suites to capture execution profiles.
#   4. Merge raw profile data with llvm-profdata.
#   5. Recompile with -Cprofile-use, fat LTO, and opt-level=3 for maximum throughput.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$WORKSPACE_ROOT"

echo "======================================================================"
echo " Hagibis (hgb) Profile-Guided Optimization (PGO) & LTO Hardening"
echo " Workspace: $WORKSPACE_ROOT"
echo "======================================================================"

# 1. Validate cargo availability
if ! command -v cargo >/dev/null 2>&1; then
    echo "[ERROR] 'cargo' not found in PATH. Please install Rust and Cargo." >&2
    exit 1
fi

# 2. Locate llvm-profdata
LLVM_PROFDATA=""
if command -v llvm-profdata >/dev/null 2>&1; then
    LLVM_PROFDATA="llvm-profdata"
else
    RUSTC_SYSROOT="$(rustc --print sysroot 2>/dev/null || true)"
    RUSTC_HOST="$(rustc -vV 2>/dev/null | grep 'host:' | cut -d' ' -f2 || true)"
    if [[ -n "$RUSTC_SYSROOT" && -n "$RUSTC_HOST" ]]; then
        CANDIDATE="$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-profdata"
        if [[ -x "$CANDIDATE" ]]; then
            LLVM_PROFDATA="$CANDIDATE"
        fi
    fi
fi

if [[ -z "$LLVM_PROFDATA" ]]; then
    echo "[ERROR] 'llvm-profdata' not found in PATH or rustlib sysroot." >&2
    echo "Install rust llvm tools: rustup component add llvm-tools-preview" >&2
    echo "Or install host llvm:    sudo apt-get install llvm (or equivalent)" >&2
    exit 1
fi

echo "[INFO] Using Cargo: $(command -v cargo)"
echo "[INFO] Using LLVM profdata: $LLVM_PROFDATA"

PGO_DATA_DIR="${PGO_DATA_DIR:-/tmp/pgo-data}"
echo "[INFO] PGO Data Directory: $PGO_DATA_DIR"

# Clean previous profile data
rm -rf "$PGO_DATA_DIR"
mkdir -p "$PGO_DATA_DIR"

# 3. Instrumented build
echo ""
echo "----------------------------------------------------------------------"
echo " Step 1/4: Building instrumented binaries with -Cprofile-generate"
echo "----------------------------------------------------------------------"
export RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR"
cargo build --release

# 4. Profile collection workload
echo ""
echo "----------------------------------------------------------------------"
echo " Step 2/4: Running representative training workload to collect profiles"
echo "----------------------------------------------------------------------"
# Execute test suites across storage, vector index, and core modules
RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR" cargo test --release -p hgb-storage --lib -- --nocapture || true

# Execute CLI if binary exists
if [[ -f "target/release/hgb" ]]; then
    target/release/hgb --help >/dev/null 2>&1 || true
fi

# 5. Merge profile data
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

# 6. Rebuild with profile-use, fat LTO, and opt-level=3
echo ""
echo "----------------------------------------------------------------------"
echo " Step 4/4: Rebuilding optimized binaries with PGO, Fat LTO & opt-level=3"
echo "----------------------------------------------------------------------"
export RUSTFLAGS="-Cprofile-use=$PGO_DATA_DIR/merged.profdata -Cllvm-args=-pgo-warn-missing-function -Copt-level=3 -Clto=fat -Ccodegen-units=1 -Cpanic=abort"
cargo build --release

echo ""
echo "======================================================================"
echo " Hagibis (hgb) PGO & Fat LTO hardening pipeline completed successfully!"
echo "======================================================================"
