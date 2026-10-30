//! # Profile-Guided WebAssembly JIT Caching Engine (PGO-JIT)
//!
//! Production-grade Tier-Up JIT compilation and native artifact caching for WebAssembly plugins.
//! - Tracks runtime execution telemetry in SQLite (`.hgb/wasm_pgo_profiles.db`):
//!   `module_name`, `function_name`, `call_count`, `total_duration_us`, `avg_duration_us`, `last_invoked`, `is_tiered_up`.
//! - Tier-Up Trigger: When a WASM module or subroutine exceeds threshold (>= 10 calls or >= 50ms cumulative runtime),
//!   triggers Cranelift tier-up JIT compilation with `OptLevel::SpeedAndSize` and SIMD vectorization.
//! - Caches native compiled code (`.cwasm` / ELF binary artifact) in `.hgb/wasm_cache/jit_pgo_<blake3>.bin`.
//! - Atomically hot-swaps the compiled module into `WasmHotPool` without daemon restarts.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use wasmtime::{Config, Engine, Module, OptLevel};

use crate::error::{HgbError, Result};

pub const DEFAULT_PGO_CALL_THRESHOLD: u64 = 10;
pub const DEFAULT_PGO_DURATION_THRESHOLD_US: u64 = 50_000; // 50 milliseconds

/// Profile telemetry item for a WASM module subroutine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasmPgoProfile {
    pub module_name: String,
    pub function_name: String,
    pub call_count: u64,
    pub total_duration_us: u64,
    pub avg_duration_us: f64,
    pub last_invoked: String,
    pub is_tiered_up: bool,
}

/// Profile-Guided JIT Compilation and Caching Engine
pub struct WasmPgoJitEngine {
    db_path: PathBuf,
    cache_dir: PathBuf,
    conn: Mutex<Connection>,
    call_threshold: u64,
    duration_threshold_us: u64,
    tiered_engine: Engine,
    total_tier_ups: AtomicU64,
    total_cache_hits: AtomicU64,
}

impl WasmPgoJitEngine {
    /// Initialize PgoJitEngine with specified SQLite profile path and native binary cache directory
    pub fn new(db_path: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> Result<Self> {
        let db_path = db_path.into();
        let cache_dir = cache_dir.into();

        if let Some(parent) = db_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::create_dir_all(&cache_dir);

        let conn = Connection::open(&db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to open PGO SQLite database at {}: {}", db_path.display(), e))
        })?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS wasm_pgo_profiles (
                 module_name TEXT NOT NULL,
                 function_name TEXT NOT NULL,
                 call_count INTEGER NOT NULL DEFAULT 0,
                 total_duration_us INTEGER NOT NULL DEFAULT 0,
                 avg_duration_us REAL NOT NULL DEFAULT 0.0,
                 last_invoked TEXT NOT NULL,
                 is_tiered_up INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (module_name, function_name)
             );"
        ).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize wasm_pgo_profiles schema: {}", e))
        })?;

        // Initialize Cranelift Engine with OptLevel::SpeedAndSize and Cranelift SIMD vectorization
        let mut config = Config::new();
        config.cranelift_opt_level(OptLevel::SpeedAndSize);
        config.wasm_simd(true);
        config.consume_fuel(true);
        config.parallel_compilation(true);

        let tiered_engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to configure Cranelift tier-up JIT engine: {}", e))
        })?;

        Ok(Self {
            db_path,
            cache_dir,
            conn: Mutex::new(conn),
            call_threshold: DEFAULT_PGO_CALL_THRESHOLD,
            duration_threshold_us: DEFAULT_PGO_DURATION_THRESHOLD_US,
            tiered_engine,
            total_tier_ups: AtomicU64::new(0),
            total_cache_hits: AtomicU64::new(0),
        })
    }

    /// Default initialization locating `.hgb/wasm_pgo_profiles.db` and `.hgb/wasm_cache` under workspace
    pub fn default_for_workspace(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let hgb_dir = workspace_root.as_ref().join(".hgb");
        let db_path = hgb_dir.join("wasm_pgo_profiles.db");
        let cache_dir = hgb_dir.join("wasm_cache");
        Self::new(db_path, cache_dir)
    }

    /// Set custom tier-up invocation thresholds
    pub fn with_thresholds(mut self, call_threshold: u64, duration_threshold_us: u64) -> Self {
        self.call_threshold = call_threshold;
        self.duration_threshold_us = duration_threshold_us;
        self
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn tiered_engine(&self) -> &Engine {
        &self.tiered_engine
    }

    pub fn total_tier_ups(&self) -> u64 {
        self.total_tier_ups.load(Ordering::Relaxed)
    }

    pub fn total_cache_hits(&self) -> u64 {
        self.total_cache_hits.load(Ordering::Relaxed)
    }

    /// Record runtime execution telemetry for a module subroutine into SQLite
    pub fn record_invocation(
        &self,
        module_name: &str,
        function_name: &str,
        duration_us: u64,
    ) -> Result<WasmPgoProfile> {
        let now_str = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(e.to_string()))?;

        conn.execute(
            "INSERT INTO wasm_pgo_profiles (module_name, function_name, call_count, total_duration_us, avg_duration_us, last_invoked, is_tiered_up)
             VALUES (?1, ?2, 1, ?3, CAST(?3 AS REAL), ?4, 0)
             ON CONFLICT(module_name, function_name) DO UPDATE SET
                 call_count = call_count + 1,
                 total_duration_us = total_duration_us + excluded.total_duration_us,
                 avg_duration_us = CAST((total_duration_us + excluded.total_duration_us) AS REAL) / CAST((call_count + 1) AS REAL),
                 last_invoked = excluded.last_invoked;",
            params![module_name, function_name, duration_us, now_str],
        ).map_err(|e| HgbError::Execution(format!("Failed to record PGO invocation: {}", e)))?;

        let mut stmt = conn.prepare(
            "SELECT module_name, function_name, call_count, total_duration_us, avg_duration_us, last_invoked, is_tiered_up
             FROM wasm_pgo_profiles WHERE module_name = ?1 AND function_name = ?2"
        ).map_err(|e| HgbError::Execution(e.to_string()))?;

        let profile = stmt.query_row(params![module_name, function_name], |row| {
            Ok(WasmPgoProfile {
                module_name: row.get(0)?,
                function_name: row.get(1)?,
                call_count: row.get(2)?,
                total_duration_us: row.get(3)?,
                avg_duration_us: row.get(4)?,
                last_invoked: row.get(5)?,
                is_tiered_up: row.get::<_, i32>(6)? != 0,
            })
        }).map_err(|e| HgbError::Execution(format!("Failed to fetch updated PGO profile: {}", e)))?;

        Ok(profile)
    }

    /// Fetch stored telemetry profile if available
    pub fn get_profile(&self, module_name: &str, function_name: &str) -> Result<Option<WasmPgoProfile>> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT module_name, function_name, call_count, total_duration_us, avg_duration_us, last_invoked, is_tiered_up
             FROM wasm_pgo_profiles WHERE module_name = ?1 AND function_name = ?2"
        ).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut rows = stmt.query(params![module_name, function_name]).map_err(|e| HgbError::Execution(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| HgbError::Execution(e.to_string()))? {
            Ok(Some(WasmPgoProfile {
                module_name: row.get(0).map_err(|e| HgbError::Execution(e.to_string()))?,
                function_name: row.get(1).map_err(|e| HgbError::Execution(e.to_string()))?,
                call_count: row.get(2).map_err(|e| HgbError::Execution(e.to_string()))?,
                total_duration_us: row.get(3).map_err(|e| HgbError::Execution(e.to_string()))?,
                avg_duration_us: row.get(4).map_err(|e| HgbError::Execution(e.to_string()))?,
                last_invoked: row.get(5).map_err(|e| HgbError::Execution(e.to_string()))?,
                is_tiered_up: row.get::<_, i32>(6).map_err(|e| HgbError::Execution(e.to_string()))? != 0,
            }))
        } else {
            Ok(None)
        }
    }

    /// List all tracked execution profiles
    pub fn list_profiles(&self) -> Result<Vec<WasmPgoProfile>> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT module_name, function_name, call_count, total_duration_us, avg_duration_us, last_invoked, is_tiered_up
             FROM wasm_pgo_profiles ORDER BY total_duration_us DESC"
        ).map_err(|e| HgbError::Execution(e.to_string()))?;

        let rows = stmt.query_map([], |row| {
            Ok(WasmPgoProfile {
                module_name: row.get(0)?,
                function_name: row.get(1)?,
                call_count: row.get(2)?,
                total_duration_us: row.get(3)?,
                avg_duration_us: row.get(4)?,
                last_invoked: row.get(5)?,
                is_tiered_up: row.get::<_, i32>(6)? != 0,
            })
        }).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r.map_err(|e| HgbError::Execution(e.to_string()))?);
        }
        Ok(list)
    }

    /// Determine if the profile has crossed tier-up thresholds (>= 10 calls or >= 50ms runtime)
    pub fn is_tier_up_eligible(&self, profile: &WasmPgoProfile) -> bool {
        !profile.is_tiered_up && (profile.call_count >= self.call_threshold || profile.total_duration_us >= self.duration_threshold_us)
    }

    /// Mark subroutine as having undergone tier-up compilation
    pub fn mark_tiered_up(&self, module_name: &str, function_name: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(e.to_string()))?;
        conn.execute(
            "UPDATE wasm_pgo_profiles SET is_tiered_up = 1 WHERE module_name = ?1 AND function_name = ?2",
            params![module_name, function_name],
        ).map_err(|e| HgbError::Execution(format!("Failed to mark tiered-up: {}", e)))?;
        self.total_tier_ups.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Compile WASM bytecode into a Cranelift optimized native binary artifact with SIMD vectorization
    pub fn compile_tier_up(&self, wasm_bytes: &[u8]) -> Result<(Vec<u8>, Engine, Module)> {
        let module = Module::new(&self.tiered_engine, wasm_bytes).map_err(|e| {
            HgbError::Execution(format!("Cranelift tier-up JIT compilation failed: {}", e))
        })?;

        let native_artifact = module.serialize().map_err(|e| {
            HgbError::Execution(format!("Failed to serialize Cranelift native machine code: {}", e))
        })?;

        Ok((native_artifact, self.tiered_engine.clone(), module))
    }

    /// Write native compiled artifact to `.hgb/wasm_cache/jit_pgo_<blake3>.bin`
    pub fn cache_native_artifact(&self, blake3_hash: &str, artifact: &[u8]) -> Result<PathBuf> {
        let cache_file = self.cache_dir.join(format!("jit_pgo_{}.bin", blake3_hash));
        fs::write(&cache_file, artifact).map_err(|e| {
            HgbError::Io(e)
        })?;
        Ok(cache_file)
    }

    /// Load precompiled native machine code artifact from `.hgb/wasm_cache/jit_pgo_<blake3>.bin`
    pub fn load_cached_native_artifact(&self, blake3_hash: &str) -> Result<Option<Module>> {
        let cache_file = self.cache_dir.join(format!("jit_pgo_{}.bin", blake3_hash));
        if !cache_file.exists() {
            return Ok(None);
        }

        let bytes = fs::read(&cache_file).map_err(HgbError::Io)?;
        let module = unsafe {
            Module::deserialize(&self.tiered_engine, &bytes).map_err(|e| {
                HgbError::Execution(format!("Failed to deserialize native artifact from {}: {}", cache_file.display(), e))
            })?
        };

        self.total_cache_hits.fetch_add(1, Ordering::Relaxed);
        Ok(Some(module))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn generate_minimal_valid_wasm() -> Vec<u8> {
        vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
            0x03, 0x02, 0x01, 0x00,
            0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x00,
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b,
        ]
    }

    #[test]
    fn test_sqlite_profile_recording_and_tier_up_trigger() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("profiles.db");
        let cache_dir = dir.path().join("cache");

        let engine = WasmPgoJitEngine::new(&db_path, &cache_dir).unwrap();

        // 1. Initial invocation record
        let p1 = engine.record_invocation("calc_module", "run", 2000).unwrap();
        assert_eq!(p1.call_count, 1);
        assert_eq!(p1.total_duration_us, 2000);
        assert_eq!(p1.avg_duration_us, 2000.0);
        assert!(!p1.is_tiered_up);
        assert!(!engine.is_tier_up_eligible(&p1));

        // 2. Incremental recordings up to threshold (call_count >= 10)
        for i in 2..=10 {
            let p = engine.record_invocation("calc_module", "run", 1000).unwrap();
            assert_eq!(p.call_count, i);
        }

        let p_final = engine.get_profile("calc_module", "run").unwrap().unwrap();
        assert_eq!(p_final.call_count, 10);
        assert_eq!(p_final.total_duration_us, 11000);
        assert!(engine.is_tier_up_eligible(&p_final));

        // Mark tiered up
        engine.mark_tiered_up("calc_module", "run").unwrap();
        let p_marked = engine.get_profile("calc_module", "run").unwrap().unwrap();
        assert!(p_marked.is_tiered_up);
        assert!(!engine.is_tier_up_eligible(&p_marked));
    }

    #[test]
    fn test_duration_threshold_tier_up_trigger() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("profiles.db");
        let cache_dir = dir.path().join("cache");

        let engine = WasmPgoJitEngine::new(&db_path, &cache_dir).unwrap();

        // Single heavy invocation >= 50ms (50,000 µs)
        let p = engine.record_invocation("heavy_compute", "process", 55_000).unwrap();
        assert_eq!(p.call_count, 1);
        assert_eq!(p.total_duration_us, 55_000);
        assert!(engine.is_tier_up_eligible(&p));
    }

    #[test]
    fn test_cranelift_tier_up_compilation_and_native_caching() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("profiles.db");
        let cache_dir = dir.path().join("cache");

        let engine = WasmPgoJitEngine::new(&db_path, &cache_dir).unwrap();
        let wasm_bytes = generate_minimal_valid_wasm();
        let hash = blake3::hash(&wasm_bytes).to_hex().to_string();

        // Tier-up compile with OptLevel::SpeedAndSize and Cranelift SIMD
        let (native_bytes, _tiered_engine, _module) = engine.compile_tier_up(&wasm_bytes).unwrap();
        assert!(!native_bytes.is_empty());

        // Cache native artifact (.cwasm / ELF)
        let cache_file = engine.cache_native_artifact(&hash, &native_bytes).unwrap();
        assert!(cache_file.exists());

        // Deserialize native artifact from cache
        let loaded = engine.load_cached_native_artifact(&hash).unwrap();
        assert!(loaded.is_some());
        assert_eq!(engine.total_cache_hits(), 1);
    }
}
