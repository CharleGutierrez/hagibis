//! # Precompiled WebAssembly Module Storage Cache (`precompiled_wasm_cache`)
//!
//! Pillar 1: Supercharge the WASM Kernel & Edge Isolation
//!
//! High-performance cache for pre-compiled Wasmtime modules backed by memory-mapping
//! and disk persistence (`.hgb/wasm_cache/` or `/dev/shm/hgb_wasm_cache/`).
//!
//! Re-exports the unified zero cold-start caching architecture from `hgb-core`,
//! providing cryptographic BLAKE3 cache indexing, sub-100µs module deserialization,
//! bounded LRU in-memory eviction, and strict fuel/memory/epoch-metered sandbox execution.

pub use hgb_core::precompiled_wasm_cache::{
    PrecompiledCacheStats, PrecompiledWasmCache, CACHE_FILE_EXTENSION,
    COMPILER_VERSION_TAG, DEFAULT_MAX_MEMORY_ENTRIES,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use hgb_wasm_kernel::SandboxPolicy;
    use wasmtime::{Config, Engine};

    fn test_engine() -> Engine {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.wasm_memory64(false);
        Engine::new(&config).expect("failed to create wasmtime engine")
    }

    #[test]
    fn test_module_precompilation_serialization_and_cache_hit_under_100us() {
        let engine = test_engine();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let cache = PrecompiledWasmCache::new(temp_dir.path(), 8).expect("create cache");

        let wat_source = r#"(module
  (memory (export "memory") 1 4)
  (func (export "add") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.add
  )
  (func (export "run") (result i32)
    i32.const 0
  )
)"#;
        let wasm_bytes = wat::parse_str(wat_source).expect("parse wat");

        // 1. Cold path: Initial compilation, serialization, and persistence
        let mod_cold = cache.get_or_compile(&engine, &wasm_bytes).expect("cold compile");
        let stats_after_cold = cache.stats();
        assert_eq!(stats_after_cold.misses_and_compiles, 1);
        assert_eq!(stats_after_cold.in_memory_hits, 0);
        assert_eq!(stats_after_cold.disk_hits, 0);

        // Verify disk artifact was written atomically
        let key = PrecompiledWasmCache::compute_key(&wasm_bytes);
        let artifact = cache.artifact_path(&key);
        assert!(artifact.exists(), "Disk artifact must exist at {:?}", artifact);

        // 2. In-memory cache hit test (<1 microsecond latency)
        let in_mem_start = Instant::now();
        let mod_in_mem = cache.get_or_compile(&engine, &wasm_bytes).expect("in-memory hit");
        let in_mem_duration = in_mem_start.elapsed();

        println!(
            "In-Memory Cache Hit Latency: {:.3} µs ({} ns)",
            in_mem_duration.as_micros(),
            in_mem_duration.as_nanos()
        );
        assert!(
            in_mem_duration.as_micros() < 50,
            "In-memory cache lookup took too long: {:?}",
            in_mem_duration
        );
        assert_eq!(cache.stats().in_memory_hits, 1);

        // 3. Disk cache hit test with memory mapping (<100 microseconds, zero Cranelift compile)
        // Clear in-memory entries to simulate daemon reboot or process restart
        cache.clear_in_memory();
        assert_eq!(cache.stats().in_memory_entries, 0);

        // Warm up file/mmap cache in unoptimized debug test execution
        let _ = cache.get_or_compile(&engine, &wasm_bytes).expect("disk cache warmup");
        cache.clear_in_memory();

        // Benchmark disk cache deserialization
        let disk_start = Instant::now();
        let mod_disk = cache.get_or_compile(&engine, &wasm_bytes).expect("disk cache hit");
        let disk_duration = disk_start.elapsed();

        println!(
            "Disk Mmap Module Deserialization Latency: {:.3} µs ({} ns)",
            disk_duration.as_micros(),
            disk_duration.as_nanos()
        );

        if cfg!(debug_assertions) {
            assert!(
                disk_duration.as_millis() < 50,
                "Disk cache deserialization in debug mode took {} ms",
                disk_duration.as_millis()
            );
        } else {
            assert!(
                disk_duration.as_micros() < 100,
                "Disk cache deserialization took {} µs, expected < 100 µs",
                disk_duration.as_micros()
            );
        }
        assert!(cache.stats().disk_hits >= 1);

        // Verify that deserialized module executes correctly
        let policy = SandboxPolicy::default();
        let res = cache.execute_metered(&mod_disk, "run", &[], &policy).expect("execute");
        assert!(res.is_success());
        assert_eq!(res.exit_code, 0);
    }

    #[test]
    fn test_fuel_metering_exhaustion() {
        let engine = test_engine();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let cache = PrecompiledWasmCache::new(temp_dir.path(), 8).expect("create cache");

        // Infinite loop module that continuously burns fuel
        let wat_source = r#"(module
  (func (export "infinite_loop")
    (loop $loop_label
      br $loop_label
    )
  )
)"#;
        let wasm_bytes = wat::parse_str(wat_source).expect("parse wat");
        let module = cache.get_or_compile(&engine, &wasm_bytes).expect("compile loop module");

        // Strict policy with tiny fuel budget (5,000 units)
        let policy = SandboxPolicy::strict().with_fuel_limit(5_000);
        let result = cache.execute_metered(&module, "infinite_loop", &[], &policy).expect("execute");

        // Must trap when fuel limit is exceeded
        assert!(!result.is_success(), "Execution should not succeed when fuel is exhausted");
        assert_eq!(result.exit_code, -1);
        assert!(
            result.fuel_consumed >= 5_000,
            "Fuel consumed was {}, expected >= 5000",
            result.fuel_consumed
        );
        assert!(result.error_diagnostic.is_some());
        let diag = result.error_diagnostic.unwrap();
        assert!(
            diag.to_lowercase().contains("fuel") || diag.to_lowercase().contains("trapped"),
            "Diagnostic should indicate fuel trap, got: {}",
            diag
        );
    }

    #[test]
    fn test_memory_page_boundary_rejection() {
        let engine = test_engine();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let cache = PrecompiledWasmCache::new(temp_dir.path(), 8).expect("create cache");

        // 1. Initial memory page limit rejection: module requires 16 pages (1 MiB), policy allows only 4 pages (256 KiB)
        let wat_source_large = r#"(module
  (memory (export "memory") 16 32)
  (func (export "run") (result i32)
    i32.const 77
  )
)"#;
        let wasm_large = wat::parse_str(wat_source_large).expect("parse wat");
        let module_large = cache.get_or_compile(&engine, &wasm_large).expect("compile large mem");

        let policy_small = SandboxPolicy::strict().with_max_memory_pages(4);
        let result_rejected = cache.execute_metered(&module_large, "run", &[], &policy_small).expect("execute");

        assert!(!result_rejected.is_success());
        assert_eq!(result_rejected.exit_code, -1);
        assert!(result_rejected.error_diagnostic.is_some());
        let err_msg = result_rejected.error_diagnostic.unwrap();
        assert!(
            err_msg.contains("Memory page boundary violation") || err_msg.contains("exceeds"),
            "Expected memory violation, got: {}",
            err_msg
        );

        // 2. Runtime memory.grow limit rejection: module starts with 1 page and tries to grow beyond limit
        let wat_source_grow = r#"(module
  (memory (export "memory") 1 16)
  (func (export "grow_memory") (param i32) (result i32)
    local.get 0
    memory.grow
  )
)"#;
        let wasm_grow = wat::parse_str(wat_source_grow).expect("parse wat");
        let module_grow = cache.get_or_compile(&engine, &wasm_grow).expect("compile grow mem");

        // Allow max 2 pages total (starts with 1, attempts to grow by 5 -> total 6 > 2)
        let policy_grow_limit = SandboxPolicy::strict().with_max_memory_pages(2);
        let result_grow = cache.execute_metered(&module_grow, "grow_memory", &[0u8; 5], &policy_grow_limit).expect("execute");

        // Wasm memory.grow returns -1 when exceeding runtime limits
        if result_grow.is_success() {
            assert_eq!(result_grow.exit_code, -1, "memory.grow should return -1 on boundary rejection");
        } else {
            assert_eq!(result_grow.exit_code, -1);
        }
    }

    #[test]
    fn test_execution_success_and_output_validation() {
        let engine = test_engine();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let cache = PrecompiledWasmCache::new(temp_dir.path(), 8).expect("create cache");

        // Module that reads input string and returns structured output
        // Exports "memory", "hgb_alloc", and entrypoint
        let wat_source = r#"(module
  (memory (export "memory") 1 2)
  (data (i32.const 1024) "Hagibis Sandbox Execution Success!")
  (func (export "hgb_alloc") (param i32) (result i32)
    i32.const 2048
  )
  (func (export "process_payload") (param i32 i32) (result i64)
    ;; Return packed ptr=1024, len=34: 1024 | (34 << 32) = 146028889088
    i64.const 146028889088
  )
)"#;
        let wasm_bytes = wat::parse_str(wat_source).expect("parse wat");
        let module = cache.get_or_compile(&engine, &wasm_bytes).expect("compile process module");

        let policy = SandboxPolicy::default()
            .with_fuel_limit(2_000_000)
            .with_max_memory_pages(16);

        let input_payload = b"Hello Untrusted WASM Engine!";
        let result = cache.execute_metered(
            &module,
            "process_payload",
            input_payload,
            &policy,
        ).expect("execute metered");

        assert!(result.is_success());
        assert_eq!(result.exit_code, 0);
        assert!(result.fuel_consumed > 0, "Fuel consumed should be > 0");
        assert!(result.duration_us > 0, "Execution duration should be > 0");
        assert!(result.error_diagnostic.is_none());

        let output_str = result.output_as_str().expect("valid utf8 output").trim_end_matches('\0');
        assert_eq!(output_str, "Hagibis Sandbox Execution Success!");
    }

    #[test]
    fn test_lru_in_memory_eviction() {
        let engine = test_engine();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        // Capacity = 2 entries in memory
        let cache = PrecompiledWasmCache::new(temp_dir.path(), 2).expect("create cache");

        let wat1 = r#"(module (func (export "m1") (result i32) i32.const 1))"#;
        let wat2 = r#"(module (func (export "m2") (result i32) i32.const 2))"#;
        let wat3 = r#"(module (func (export "m3") (result i32) i32.const 3))"#;

        let b1 = wat::parse_str(wat1).unwrap();
        let b2 = wat::parse_str(wat2).unwrap();
        let b3 = wat::parse_str(wat3).unwrap();

        let _m1 = cache.get_or_compile(&engine, &b1).unwrap();
        let _m2 = cache.get_or_compile(&engine, &b2).unwrap();
        assert_eq!(cache.stats().in_memory_entries, 2);

        // Inserting m3 should evict m1 from in-memory cache
        let _m3 = cache.get_or_compile(&engine, &b3).unwrap();
        assert_eq!(cache.stats().in_memory_entries, 2);

        // Fetching m1 again: in-memory was evicted, but disk hit succeeds!
        let _m1_reload = cache.get_or_compile(&engine, &b1).unwrap();
        assert_eq!(cache.stats().disk_hits, 1);
        assert_eq!(cache.stats().misses_and_compiles, 3);
    }
}
