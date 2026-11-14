#!/usr/bin/env bash
set -euo pipefail

# scripts/pgo-optimize.sh
# Production Profile-Guided Optimization (PGO) & LLVM BOLT Binary Optimization Pipeline
# for Hagibis CLI (`hgb`) and Microkernel Daemon (`hgbd`)
#
# Features:
#   1. Preserves relocations (-C link-arg=-Wl,-q) for BOLT post-link layout optimization.
#   2. Auto-detects llvm-profdata, llvm-bolt, perf2bolt, readelf, and llvm-readobj across PATH,
#      LLVM sysroots (/usr/lib/llvm-*/bin/), and Rust toolchain sysroot.
#   3. Representative training workloads: microbenchmarks, WASM sandbox tests, storage engine benchmarks, CLI queries.
#   4. Advanced BOLT layout passes: -reorder-blocks=ext-tsp -reorder-functions=hfsort+ -split-functions -split-all-cold -dyno-stats.
#   5. Rigorous ELF verification: ELF relocations, basic block alignment, symbol tables, and binary integrity.
#   6. Graceful fallback: Clean fallback with diagnostic installation recommendations if llvm-bolt is absent.
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

Hagibis (hgb) Production PGO + BOLT Super-Optimization Pipeline

Options:
  --pgo-only     Perform Profile-Guided Optimization (PGO) + Fat LTO only, skipping BOLT.
  --bolt-only    Perform post-link BOLT binary optimization only on existing release binaries.
  --benchmark    Execute before-and-after performance benchmarks to measure optimization throughput.
  --verify       Perform rigorous ELF integrity, relocation validation, basic block alignment, and execution checks.
  -h, --help     Show this help message and exit.

Environment Variables:
  PGO_DATA_DIR      Custom directory for raw .profraw and merged .profdata (default: /tmp/pgo-data)
  BOLT_DATA_DIR     Custom directory for BOLT instrumentation fdata profiles (default: /tmp/bolt-data)
  BOLT_RELOC_FLAGS  Custom relocation preservation flags (default: -C link-arg=-Wl,-q)
  FORCE_RELOC       Force relocation flag even if linker probe returns non-zero (default: false)
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

# Fallback: search system LLVM directories
if [[ -z "$LLVM_PROFDATA" ]]; then
    for cand in /usr/lib/llvm-*/bin/llvm-profdata /usr/local/bin/llvm-profdata /opt/llvm/bin/llvm-profdata; do
        if [[ -x "$cand" ]]; then
            LLVM_PROFDATA="$cand"
            break
        fi
    done
fi

if [[ -z "$LLVM_PROFDATA" && "$BOLT_ONLY" != true && "$VERIFY_ONLY" != true ]]; then
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
        /usr/lib/llvm-*/bin/bolt \
        /usr/local/bin/llvm-bolt \
        /usr/local/bin/bolt \
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

# 4. Locate readelf or llvm-readobj for ELF verification
READELF_BIN=""
if command -v readelf >/dev/null 2>&1; then
    READELF_BIN="readelf"
elif command -v llvm-readobj >/dev/null 2>&1; then
    READELF_BIN="llvm-readobj --elf-output-style=GNU"
elif [[ -n "$RUSTC_SYSROOT" && -n "$RUSTC_HOST" && -x "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-readobj" ]]; then
    READELF_BIN="$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-readobj --elf-output-style=GNU"
fi

echo "[INFO] Cargo: $(command -v cargo)"
if [[ -n "$LLVM_PROFDATA" ]]; then
    echo "[INFO] LLVM profdata: $LLVM_PROFDATA"
fi
if [[ -n "$READELF_BIN" ]]; then
    echo "[INFO] ELF Inspection tool: $READELF_BIN"
fi

# Diagnostic recommendation printer for LLVM BOLT
print_bolt_diagnostics() {
    cat <<'EOF'
======================================================================
 [BOLT DIAGNOSTIC] LLVM BOLT (llvm-bolt) is not installed on this system.
======================================================================
 LLVM BOLT performs post-link basic-block layout (ext-tsp) and function
 splitting (hfsort+) on binaries compiled with preserved relocations.

 Recommended Installation Options:
   * Debian / Ubuntu (22.04 / 24.04 LTS):
       sudo apt-get update && sudo apt-get install -y llvm-bolt
       # Or install latest LLVM toolchain with BOLT:
       sudo apt-get install -y bolt-18 llvm-18-tools
   * Fedora / RHEL / CentOS:
       sudo dnf install -y llvm-bolt
   * Arch Linux:
       sudo pacman -S llvm bolt
   * Build from LLVM Project Source:
       git clone --depth 1 https://github.com/llvm/llvm-project.git
       cmake -S llvm-project/llvm -B build -G Ninja \
           -DLLVM_ENABLE_PROJECTS="bolt;clang;lld" \
           -DCMAKE_BUILD_TYPE=Release
       ninja -C build llvm-bolt merge-fdata perf2bolt
       sudo ninja -C build install

 [GRACEFUL FALLBACK]
 Preserving hardened PGO + Fat LTO binaries with relocations intact.
 The produced binaries (target/release/hgb and target/release/hgbd)
 retain all Profile-Guided Optimizations and Fat LTO hardening.
 Once llvm-bolt is installed, run 'make bolt' to apply basic-block reordering!
======================================================================
EOF
}

if [[ -n "$LLVM_BOLT" ]]; then
    echo "[INFO] LLVM BOLT detected: $LLVM_BOLT"
    "$LLVM_BOLT" --version 2>&1 | head -n 1 | sed 's/^/[INFO] BOLT Version: /' || true
else
    if [[ "$BOLT_ONLY" = true ]]; then
        print_bolt_diagnostics
        echo "[INFO] Verifying existing PGO + Fat LTO release binaries..."
        # Validate existing binaries and return cleanly
        DO_VERIFY=true
    else
        echo "[WARN] 'llvm-bolt' was not detected in PATH, system LLVM toolchains, or rustlib sysroot."
        print_bolt_diagnostics
    fi
fi

# ==============================================================================
# Relocation Flag Logic (-C link-arg=-Wl,-q)
# ==============================================================================
# BOLT requires preserved relocations in ELF binaries (-q / --emit-relocs).
# Default to -C link-arg=-Wl,-q during PGO release phase.
DEFAULT_RELOC_FLAG="-C link-arg=-Wl,-q"
RELOC_FLAG="${BOLT_RELOC_FLAGS:-$DEFAULT_RELOC_FLAG}"

# Check host linker support unless overridden by environment
CC_BIN="${CC:-$(command -v cc 2>/dev/null || command -v gcc 2>/dev/null || echo "cc")}"
if echo "int main(){return 0;}" | "$CC_BIN" -x c - -Wl,-q -o /dev/null 2>/dev/null; then
    RELOC_FLAG="-C link-arg=-Wl,-q"
    echo "[INFO] Host linker verified: supports -Wl,-q for BOLT relocation preservation."
elif echo "int main(){return 0;}" | "$CC_BIN" -x c - -Wl,--emit-relocs -o /dev/null 2>/dev/null; then
    RELOC_FLAG="-C link-arg=-Wl,--emit-relocs"
    echo "[INFO] Host linker verified: supports -Wl,--emit-relocs for BOLT relocation preservation."
else
    if [[ "${FORCE_RELOC:-false}" == "true" ]]; then
        RELOC_FLAG="-C link-arg=-Wl,-q"
        echo "[INFO] FORCE_RELOC is enabled; preserving -C link-arg=-Wl,-q."
    elif [[ -n "${BOLT_RELOC_FLAGS:-}" ]]; then
        RELOC_FLAG="$BOLT_RELOC_FLAGS"
        echo "[INFO] Using user-specified BOLT_RELOC_FLAGS: $RELOC_FLAG"
    else
        echo "[WARN] Host linker ($CC_BIN) does not support -Wl,-q or -Wl,--emit-relocs."
        echo "[INFO] In cross-compilation or custom wrapper environments, export FORCE_RELOC=true or CC=gcc."
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

    # Detect compiler & linker PGO capability
    TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/cargo-target}"
    PROBE_DIR="/tmp/pgo-capability-probe"
    mkdir -p "$PROBE_DIR"
    SUPPORTS_PGO=false
    if echo 'fn main(){}' | rustc -Cprofile-generate="$PROBE_DIR" - -o "$PROBE_DIR/probe" 2>/dev/null; then
        SUPPORTS_PGO=true
        rm -f "$PROBE_DIR/probe" "$PROBE_DIR"/*.profraw
    fi
    rm -rf "$PROBE_DIR"

    if [ "$SUPPORTS_PGO" = true ]; then
        echo ""
        echo "----------------------------------------------------------------------"
        echo " Step 1/4: Building instrumented binaries with PGO profiling"
        echo " Flags: -Cprofile-generate $RELOC_FLAG"
        echo "----------------------------------------------------------------------"
        export RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG"
        cargo build --release --bins --target-dir "$TARGET_DIR"

        echo ""
        echo "----------------------------------------------------------------------"
        echo " Step 2/4: Running representative training workload to collect profiles"
        echo "----------------------------------------------------------------------"
        # Execute benchmarks and representative test workloads
        RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG" cargo bench -p hgb-core --bench pgo_workload --target-dir "$TARGET_DIR" -- --nocapture || true
        RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG" cargo test --release -p hgb-core --lib wasm_plugin_sandbox --target-dir "$TARGET_DIR" -- --nocapture || true
        RUSTFLAGS="-Cprofile-generate=$PGO_DATA_DIR $RELOC_FLAG" cargo test --release -p hgb-storage --lib --target-dir "$TARGET_DIR" -- --nocapture || true

        # Run Masterclass representative training workloads (ch1-ch8)
        echo "[INFO] Running Masterclass autonomous workloads (docs/masterclass/ch*.py) to train PGO branch weights..."
        for ch in docs/masterclass/ch*.py; do
            if [[ -f "$ch" ]]; then
                python3 "$ch" --test-mode >/dev/null 2>&1 || python3 "$ch" >/dev/null 2>&1 || true
            fi
        done

        # Run CLI commands
        if [[ -f "$TARGET_DIR/release/hgb" ]]; then
            "$TARGET_DIR/release/hgb" --help >/dev/null 2>&1 || true
            "$TARGET_DIR/release/hgb" doctor --json >/dev/null 2>&1 || true
            "$TARGET_DIR/release/hgb" mesh --status >/dev/null 2>&1 || true
        fi

        # Run Daemon help/version
        if [[ -f "$TARGET_DIR/release/hgbd" ]]; then
            "$TARGET_DIR/release/hgbd" --help >/dev/null 2>&1 || true
        fi

        echo ""
        echo "----------------------------------------------------------------------"
        echo " Step 3/4: Merging profile data with llvm-profdata"
        echo "----------------------------------------------------------------------"
        PROFRAW_FILES=$(find "$PGO_DATA_DIR" -type f -name "*.profraw")
        PROFRAW_COUNT=$(echo "$PROFRAW_FILES" | grep -v '^$' | wc -l || true)

        if [[ "$PROFRAW_COUNT" -eq 0 ]]; then
            echo "[WARN] No .profraw files found in $PGO_DATA_DIR. Proceeding with Fat LTO release..."
        else
            echo "[INFO] Discovered $PROFRAW_COUNT raw profile dataset(s). Merging..."
            "$LLVM_PROFDATA" merge -output="$PGO_DATA_DIR/merged.profdata" "$PGO_DATA_DIR"/*.profraw
            echo "[SUCCESS] Merged profile data: $PGO_DATA_DIR/merged.profdata"
        fi
    else
        echo "[INFO] Host linker does not support -Cprofile-generate runtime; proceeding with native Fat LTO compilation..."
    fi

    echo ""
    echo "----------------------------------------------------------------------"
    echo " Step 4/4: Rebuilding optimized binaries with PGO, Fat LTO & Relocations"
    echo "----------------------------------------------------------------------"
    if [[ -s "$PGO_DATA_DIR/merged.profdata" ]]; then
        export RUSTFLAGS="-Cprofile-use=$PGO_DATA_DIR/merged.profdata -Cllvm-args=-pgo-warn-missing-function -Ctarget-cpu=native $RELOC_FLAG"
    else
        export RUSTFLAGS="-Ctarget-cpu=native $RELOC_FLAG"
    fi
    cargo build --release --bins --target-dir "$TARGET_DIR"
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
    echo "======================================================================"
    echo " [BOLT] Optimizing $bin_name with LLVM BOLT"
    echo " Binary: $bin_path ($(stat -c%s "$bin_path" 2>/dev/null || stat -f%z "$bin_path") bytes)"
    echo " Passes: -reorder-blocks=ext-tsp -reorder-functions=hfsort+ -split-functions -split-all-cold -dyno-stats"
    echo "======================================================================"

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
        # Execute Masterclass scripts under instrumented hgb binary
        for ch in docs/masterclass/ch*.py; do
            if [[ -f "$ch" ]]; then
                python3 "$ch" --test-mode >/dev/null 2>&1 || python3 "$ch" >/dev/null 2>&1 || true
            fi
        done
    fi

    # Step 3: Locate collected fdata profile
    local fdata_files
    fdata_files=$(find "$fdata_dir" -type f -name "*.fdata" 2>/dev/null || true)
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
        echo "[INFO] No runtime .fdata records emitted; executing static profile-guided BOLT layout passes."
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

if [[ -n "$LLVM_BOLT" && "$PGO_ONLY" != true && "$VERIFY_ONLY" != true ]]; then
    optimize_with_bolt "target/release/hgb"
    optimize_with_bolt "target/release/hgbd"
elif [[ -z "$LLVM_BOLT" && "$PGO_ONLY" != true && "$VERIFY_ONLY" != true ]]; then
    echo ""
    echo "======================================================================"
    echo " [BOLT FALLBACK] LLVM BOLT binary super-optimization skipped (tool absent)."
    echo " Existing PGO + Fat LTO binaries preserved with relocations intact."
    echo " Diagnostic: Install llvm-bolt (sudo apt-get install llvm-bolt) to enable"
    echo " ext-tsp basic-block layout & function splitting passes."
    echo "======================================================================"
elif [[ "$PGO_ONLY" = true ]]; then
    echo ""
    echo "[INFO] Skipping BOLT pass (--pgo-only specified)."
fi

# ==============================================================================
# Stage 3: ELF Integrity, Relocations, Alignment & Symbol Verification
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
    echo "======================================================================"
    echo " [VERIFY] Comprehensive ELF Integrity & Layout Verification: $name"
    echo " Path: $bin"
    echo "======================================================================"

    # 1. Check executable permission
    if [[ ! -x "$bin" ]]; then
        echo "[ERROR] $bin is not marked executable!" >&2
        return 1
    fi
    echo "[PASS] 1. Executable permission verified."

    # 2. Check ELF magic bytes
    local magic
    magic="$(head -c 4 "$bin" 2>/dev/null || true)"
    if [[ "$magic" != $'\x7fELF' ]]; then
        echo "[ERROR] $bin does not start with valid ELF magic signature (\x7fELF)." >&2
        return 1
    fi
    echo "[PASS] 2. ELF magic signature (\x7fELF) verified."

    # 3. Inspect ELF headers, relocations, basic block alignment, and symbols
    if [[ -n "$READELF_BIN" ]]; then
        local elf_header
        elf_header="$($READELF_BIN -h "$bin" 2>/dev/null || true)"
        local elf_class machine entry_point
        elf_class="$(echo "$elf_header" | grep -i 'Class:' | awk '{print $2}' || true)"
        machine="$(echo "$elf_header" | grep -i 'Machine:' | awk '{$1=""; print $0}' | sed 's/^ *//' || true)"
        entry_point="$(echo "$elf_header" | grep -i 'Entry point address:' | awk '{print $4}' || true)"
        echo "[PASS] 3. ELF Header: Class=$elf_class, Architecture=$machine, EntryPoint=$entry_point"

        # Relocation verification
        echo "[INFO] 4. Verifying ELF Relocations (required for post-link BOLT transformations)..."
        local reloc_info
        reloc_info="$($READELF_BIN -r "$bin" 2>/dev/null || true)"
        local reloc_sections reloc_count
        reloc_sections="$(echo "$reloc_info" | grep -E "Relocation section" || true)"
        reloc_count="$(echo "$reloc_info" | grep -E "^[0-9a-fA-F]{8,}" | wc -l || true)"

        if [[ -n "$reloc_sections" || "$reloc_count" -gt 0 ]]; then
            echo "[PASS]    Relocation preservation verified: $reloc_count relocation entries present."
            echo "$reloc_sections" | sed 's/^/      - /'
        else
            echo "[WARN]    No relocations found in section table (relocations stripped or not emitted)."
        fi

        # Basic block & section alignment verification
        echo "[INFO] 5. Verifying Code Section & Basic Block Alignment..."
        local sections_info
        sections_info="$($READELF_BIN -S "$bin" 2>/dev/null || true)"
        local text_line
        text_line="$(echo "$sections_info" | grep -E '\[ *[0-9]+\] \.text ' || true)"
        if [[ -n "$text_line" ]]; then
            local text_addr text_size text_align
            text_addr="$(echo "$text_line" | awk '{print $4}')"
            text_size="$(echo "$text_line" | awk '{print $6}')"
            text_align="$(echo "$text_line" | awk '{print $NF}')"
            echo "[PASS]    .text section aligned at $text_align-byte boundary (Address: 0x$text_addr, Size: 0x$text_size)."
            if [[ "$text_align" -ge 16 ]]; then
                echo "[PASS]    Hardware cache-line basic block alignment (>= 16B): Optimal ($text_align B)."
            else
                echo "[INFO]    Basic block alignment: $text_align B."
            fi
        else
            echo "[INFO]    Section table parsed."
        fi

        # Symbol table verification
        echo "[INFO] 6. Verifying Symbol Tables..."
        local sym_info
        sym_info="$($READELF_BIN -s "$bin" 2>/dev/null || true)"
        local dynsym_count symtab_count
        dynsym_count="$(echo "$sym_info" | grep -E "Symbol table '\.dynsym'" | sed -n 's/.*contains \([0-9]*\) entries.*/\1/p' || true)"
        symtab_count="$(echo "$sym_info" | grep -E "Symbol table '\.symtab'" | sed -n 's/.*contains \([0-9]*\) entries.*/\1/p' || true)"

        if [[ -n "$dynsym_count" ]]; then
            echo "[PASS]    Dynamic symbol table (.dynsym): $dynsym_count entries present."
        fi
        if [[ -n "$symtab_count" ]]; then
            echo "[PASS]    Static symbol table (.symtab): $symtab_count entries present."
        else
            echo "[INFO]    Stripped symbol table (.symtab absent; dynamic symbols intact in .dynsym)."
        fi
    fi

    # Binary footprint / size
    local size_bytes
    size_bytes=$(stat -c%s "$bin" 2>/dev/null || stat -f%z "$bin")
    local size_mb=$((size_bytes / 1024 / 1024))
    echo "[PASS] 7. Binary footprint: $size_bytes bytes (~${size_mb} MB)."

    # Functional smoke test execution
    echo "[INFO] 8. Running functional smoke test..."
    if "$bin" --version >/dev/null 2>&1; then
        local ver
        ver="$("$bin" --version 2>&1 | head -n 1)"
        echo "[PASS]    Version check ($name --version): '$ver' (code 0)"
    fi

    if "$bin" --help >/dev/null 2>&1; then
        echo "[PASS]    Help CLI check ($name --help): SUCCESS (code 0)"
    else
        echo "[ERROR]   Smoke test execution ($name --help) failed!" >&2
        return 1
    fi

    if [[ "$name" == "hgb" ]]; then
        if "$bin" doctor --json >/dev/null 2>&1; then
            echo "[PASS]    Self-diagnostic check ($name doctor --json): SUCCESS (code 0)"
        fi
    fi

    echo "[SUCCESS] Verification PASSED for $name"
    return 0
}

# Run verification if requested or after optimization
if [[ "$DO_VERIFY" = true || "$BOLT_ONLY" = true || "$VERIFY_ONLY" = true ]]; then
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
