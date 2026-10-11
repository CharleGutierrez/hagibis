# 🪽 Hagibis Sovereign WebAssembly Microkernel (`hgb-wasm-kernel`)

A production-grade, freestanding WebAssembly microkernel implemented in **Zig 0.13.0** (`wasm32-freestanding`).
Designed specifically for the Hagibis PWA Hybrid Architecture to eliminate loopback IPC/HTTP latency for CPU-bound editor operations on low-power hardware (such as dual-core Intel Core i3-7100U laptops with 8GB RAM).

## Key Capabilities
- **Freestanding Memory Allocator**: Zero-libc block allocator (`wasm_alloc`, `wasm_free`) with dynamic memory paging via `@wasmMemoryGrow`.
- **Myers SES Diff Engine**: O(ND) Shortest Edit Script line diff algorithm using 64-bit FNV-1a hashing in linear memory, returning cohesive 36-byte `DiffChunk` structs.
- **Sub-Millisecond Levenshtein & Fuzzy Search**: Word-boundary aware fuzzy matching (< 200 µs for 1,200 project paths).
- **Git DAG Graph Layout**: Direct calculation of branch tracks and lane compaction for up to 32 parallel active lanes (< 50 µs for 25 commits).
- **Footprint**: **9.1 KB** compiled binary (< 3.5 KB gzipped).

## Compilation
To compile to `wasm32-freestanding`:
```bash
zig build-exe src/main.zig -target wasm32-freestanding -O ReleaseSmall --name hgb_kernel -fno-entry -rdynamic
cp hgb_kernel.wasm ../hgb-nextgen/dist/hgb_kernel.wasm
```

## Running Unit Tests
Targeting host platform:
```bash
zig test src/main.zig
```
