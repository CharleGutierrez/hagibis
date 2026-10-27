# Hagibis (hgb) High-Performance Autonomous Engine Makefile
# Profile-Guided Optimization (PGO), Fat LTO, Build & Test Automation

SHELL := /usr/bin/env bash
PGO_DATA_DIR ?= /tmp/pgo-data
LLVM_PROFDATA ?= $(shell command -v llvm-profdata 2>/dev/null || find $(shell rustc --print sysroot 2>/dev/null)/lib/rustlib/ -name llvm-profdata 2>/dev/null | head -n 1)

.PHONY: all check test release pgo pgo-build pgo-train pgo-merge pgo-release clean help

all: check test

help:
	@echo "Hagibis (hgb) Production Build Targets:"
	@echo "  make check        - Run workspace compiler checks across all crates"
	@echo "  make test         - Run workspace unit tests and sandbox suites"
	@echo "  make release      - Build release binaries without PGO"
	@echo "  make pgo          - End-to-end PGO pipeline (build -> train -> merge -> release)"
	@echo "  make pgo-build    - Instrument binaries with -Cprofile-generate"
	@echo "  make pgo-train    - Run representative training benchmark to collect .profraw"
	@echo "  make pgo-merge    - Merge raw profiles into merged.profdata via llvm-profdata"
	@echo "  make pgo-release  - Recompile hardened binaries with -Cprofile-use & Fat LTO"
	@echo "  make clean        - Clean build artifacts and PGO profile directory"

check:
	cargo check --workspace

test:
	cargo test --workspace

release:
	cargo build --release

# End-to-End PGO Pipeline
pgo: pgo-build pgo-train pgo-merge pgo-release
	@echo "======================================================================"
	@echo " Hagibis (hgb) PGO & Fat LTO Hardened Build Completed Successfully!"
	@echo " Binary: target/release/hgb"
	@echo "======================================================================"

# Step 1: Build instrumented binaries
pgo-build:
	@echo "[PGO] Step 1/4: Cleaning old profiles & instrumenting build..."
	rm -rf $(PGO_DATA_DIR)
	mkdir -p $(PGO_DATA_DIR)
	RUSTFLAGS="-Cprofile-generate=$(PGO_DATA_DIR)" cargo build --release

# Step 2: Run representative benchmark training workload
pgo-train:
	@echo "[PGO] Step 2/4: Running representative training workload..."
	@if [ ! -d "$(PGO_DATA_DIR)" ]; then mkdir -p $(PGO_DATA_DIR); fi
	RUSTFLAGS="-Cprofile-generate=$(PGO_DATA_DIR)" cargo bench -p hgb-core --bench pgo_workload -- --nocapture || \
	RUSTFLAGS="-Cprofile-generate=$(PGO_DATA_DIR)" cargo test -p hgb-core --lib wasm_plugin_sandbox -- --nocapture || true
	@if [ -f "target/release/hgb" ]; then \
		target/release/hgb --help >/dev/null 2>&1 || true; \
	fi

# Step 3: Merge raw .profraw files using llvm-profdata
pgo-merge:
	@echo "[PGO] Step 3/4: Merging profile datasets with llvm-profdata..."
	@if [ -z "$(LLVM_PROFDATA)" ]; then \
		echo "ERROR: llvm-profdata not found in PATH or rustlib sysroot." >&2; \
		echo "Run: rustup component add llvm-tools-preview" >&2; \
		exit 1; \
	fi
	@echo "Using llvm-profdata: $(LLVM_PROFDATA)"
	@PROFRAW_FILES=$$(find $(PGO_DATA_DIR) -type f -name "*.profraw"); \
	if [ -z "$$PROFRAW_FILES" ]; then \
		echo "ERROR: No .profraw files found in $(PGO_DATA_DIR)" >&2; \
		exit 1; \
	fi
	$(LLVM_PROFDATA) merge -output=$(PGO_DATA_DIR)/merged.profdata $(PGO_DATA_DIR)/*.profraw
	@if [ ! -s "$(PGO_DATA_DIR)/merged.profdata" ]; then \
		echo "ERROR: $(PGO_DATA_DIR)/merged.profdata is missing or empty" >&2; \
		exit 1; \
	fi
	@echo "Successfully created $(PGO_DATA_DIR)/merged.profdata"

# Step 4: Rebuild hardened release binaries using profile data, fat LTO, and opt-level=3
pgo-release:
	@echo "[PGO] Step 4/4: Compiling final binaries with -Cprofile-use & Fat LTO..."
	@if [ ! -s "$(PGO_DATA_DIR)/merged.profdata" ]; then \
		echo "ERROR: Profile data $(PGO_DATA_DIR)/merged.profdata not found. Run 'make pgo-train' and 'make pgo-merge' first." >&2; \
		exit 1; \
	fi
	RUSTFLAGS="-Cprofile-use=$(PGO_DATA_DIR)/merged.profdata -Cllvm-args=-pgo-warn-missing-function -Copt-level=3 -Clto=fat -Ccodegen-units=1 -Cpanic=abort" cargo build --release

clean:
	cargo clean
	rm -rf $(PGO_DATA_DIR)
