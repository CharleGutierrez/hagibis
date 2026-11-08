//! # Real-Time Kernel & Hardware Telemetry Vector Bridge (`telemetry_vector_bridge`)
//!
//! Unifies hardware telemetry (CPU thermal states, MGLRU memory pressure, battery care from `kernel-optimizer`
//! and `dynatune`) with LanceDB columnar vector storage.
//!
//! Encodes multi-dimensional hardware state metrics and time-series positional encodings into normalized
//! embeddings for sub-millisecond retrieval of historical kernel states and autonomous thermal governance.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use hgb_core::error::{HgbError, Result};
use crate::lancedb_vector_engine::{DistanceMetric, LanceDbVectorEngine, SearchQuery, VectorRecord};

pub const DEFAULT_TELEMETRY_VECTOR_DIM: usize = 128;

/// Concrete snapshot of Linux kernel and hardware telemetry metrics
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareKernelTelemetrySnapshot {
    pub snapshot_id: String,
    pub cpu_temp_celsius: f32,
    pub cpu_usage_pct: f32,
    pub mglru_min_ttl_ms: u64,
    pub memory_pressure_pct: f32,
    pub battery_charge_pct: u8,
    pub is_charging: bool,
    pub active_governor: String,
    pub active_workload_tag: String,
    pub timestamp_ms: u64,
}

impl HardwareKernelTelemetrySnapshot {
    pub fn new(
        cpu_temp_celsius: f32,
        cpu_usage_pct: f32,
        mglru_min_ttl_ms: u64,
        memory_pressure_pct: f32,
        battery_charge_pct: u8,
        is_charging: bool,
        active_governor: &str,
        active_workload_tag: &str,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            snapshot_id: uuid::Uuid::new_v4().to_string(),
            cpu_temp_celsius,
            cpu_usage_pct,
            mglru_min_ttl_ms,
            memory_pressure_pct,
            battery_charge_pct,
            is_charging,
            active_governor: active_governor.to_string(),
            active_workload_tag: active_workload_tag.to_string(),
            timestamp_ms: now_ms,
        }
    }

    /// Transforms hardware metrics and sinusoidal time-series components into a unit-normalized vector
    pub fn to_embedding_vector(&self, dim: usize) -> Vec<f32> {
        let mut vec = Vec::with_capacity(dim);

        // Normalized primary hardware metrics
        let norm_temp = ((self.cpu_temp_celsius - 30.0) / 70.0).clamp(0.0, 1.0);
        let norm_cpu = (self.cpu_usage_pct / 100.0).clamp(0.0, 1.0);
        let norm_ttl = ((self.mglru_min_ttl_ms as f32) / 10000.0).clamp(0.0, 1.0);
        let norm_mem = (self.memory_pressure_pct / 100.0).clamp(0.0, 1.0);
        let norm_bat = ((self.battery_charge_pct as f32) / 100.0).clamp(0.0, 1.0);
        let norm_chg = if self.is_charging { 1.0f32 } else { 0.0f32 };

        let gov_hash = blake3::hash(self.active_governor.as_bytes());
        let gov_factor = (gov_hash.as_bytes()[0] as f32) / 255.0;

        let tag_hash = blake3::hash(self.active_workload_tag.as_bytes());
        let tag_factor = (tag_hash.as_bytes()[0] as f32) / 255.0;

        vec.push(norm_temp);
        vec.push(norm_cpu);
        vec.push(norm_ttl);
        vec.push(norm_mem);
        vec.push(norm_bat);
        vec.push(norm_chg);
        vec.push(gov_factor);
        vec.push(tag_factor);

        // Sinusoidal positional harmonics for multi-dimensional time series representation
        for i in 8..dim {
            let freq = (i as f32) * 0.13;
            let harmonic = if i % 2 == 0 {
                (norm_temp * freq).sin() * (norm_cpu + 0.1)
            } else {
                (norm_mem * freq).cos() * (norm_ttl + 0.1)
            };
            vec.push(harmonic);
        }

        // Euclidean L2 normalization
        let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
        let norm = norm_sq.sqrt().max(1e-6);
        for x in vec.iter_mut() {
            *x /= norm;
        }

        vec
    }
}

/// A matched historical telemetry state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryMatch {
    pub snapshot: HardwareKernelTelemetrySnapshot,
    pub similarity_score: f32,
    pub historical_action_taken: String,
}

/// Unified bridge connecting hardware telemetry feeds to LanceDB vector space
pub struct TelemetryVectorBridge {
    storage_root: PathBuf,
    conn: Arc<Mutex<Connection>>,
    vector_engine: Arc<LanceDbVectorEngine>,
    sequence: AtomicU64,
}

impl TelemetryVectorBridge {
    pub fn new(storage_root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&storage_root)
            .map_err(|e| HgbError::Storage(format!("Failed to create storage dir: {e}")))?;

        let db_path = storage_root.join("telemetry_history.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .map_err(|e| HgbError::Storage(format!("Failed to open telemetry db: {e}")))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;

             CREATE TABLE IF NOT EXISTS kernel_telemetry_snapshots (
                 snapshot_id TEXT PRIMARY KEY,
                 cpu_temp REAL NOT NULL,
                 cpu_usage REAL NOT NULL,
                 mglru_ttl INTEGER NOT NULL,
                 memory_pressure REAL NOT NULL,
                 battery_charge INTEGER NOT NULL,
                 is_charging INTEGER NOT NULL,
                 governor TEXT NOT NULL,
                 workload_tag TEXT NOT NULL,
                 action_taken TEXT NOT NULL,
                 timestamp_ms INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_telemetry_ts ON kernel_telemetry_snapshots(timestamp_ms);
             CREATE INDEX IF NOT EXISTS idx_telemetry_workload ON kernel_telemetry_snapshots(workload_tag);"
        ).map_err(|e| HgbError::Storage(format!("Init telemetry schema failed: {e}")))?;

        let lance_dir = storage_root.join("vector_hardware_telemetry.lance");
        let vector_engine = LanceDbVectorEngine::open_or_create(
            &lance_dir,
            DEFAULT_TELEMETRY_VECTOR_DIM,
            DistanceMetric::Cosine,
        )?;

        Ok(Self {
            storage_root,
            conn: Arc::new(Mutex::new(conn)),
            vector_engine: Arc::new(vector_engine),
            sequence: AtomicU64::new(0),
        })
    }

    /// Ingests a new hardware telemetry snapshot into both SQLite WAL and LanceDB vector space
    pub fn ingest_telemetry_snapshot(
        &self,
        snapshot: HardwareKernelTelemetrySnapshot,
        action_taken: &str,
    ) -> Result<String> {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let id = snapshot.snapshot_id.clone();
        let embedding = snapshot.to_embedding_vector(DEFAULT_TELEMETRY_VECTOR_DIM);

        // 1. Relational insert
        {
            let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
            conn.execute(
                "INSERT INTO kernel_telemetry_snapshots (
                    snapshot_id, cpu_temp, cpu_usage, mglru_ttl, memory_pressure,
                    battery_charge, is_charging, governor, workload_tag, action_taken, timestamp_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    snapshot.snapshot_id,
                    snapshot.cpu_temp_celsius as f64,
                    snapshot.cpu_usage_pct as f64,
                    snapshot.mglru_min_ttl_ms as i64,
                    snapshot.memory_pressure_pct as f64,
                    snapshot.battery_charge_pct as i64,
                    if snapshot.is_charging { 1 } else { 0 },
                    snapshot.active_governor,
                    snapshot.active_workload_tag,
                    action_taken,
                    snapshot.timestamp_ms as i64
                ],
            ).map_err(|e| HgbError::Storage(format!("Failed to insert telemetry snapshot: {e}")))?;
        }

        // 2. Vector index insert
        let mut meta = serde_json::Map::new();
        meta.insert("snapshot_id".to_string(), serde_json::Value::String(id.clone()));
        meta.insert("workload_tag".to_string(), serde_json::Value::String(snapshot.active_workload_tag.clone()));
        meta.insert("action_taken".to_string(), serde_json::Value::String(action_taken.to_string()));

        let record = VectorRecord {
            id: id.clone(),
            vector: embedding,
            text: format!("Telemetry {} at {}C", snapshot.active_workload_tag, snapshot.cpu_temp_celsius),
            metadata: serde_json::Value::Object(meta),
            timestamp: snapshot.timestamp_ms,
            partition: "telemetry".to_string(),
        };
        let _ = self.vector_engine.upsert(vec![record]);

        Ok(id)
    }

    /// Queries similar historical hardware and thermal states
    pub fn query_similar_system_states(
        &self,
        current: &HardwareKernelTelemetrySnapshot,
        top_k: usize,
    ) -> Result<Vec<TelemetryMatch>> {
        let target_vec = current.to_embedding_vector(DEFAULT_TELEMETRY_VECTOR_DIM);
        let query = SearchQuery::new(target_vec, top_k.max(1));
        let search_results = self.vector_engine.query(&query)?;

        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let mut matches = Vec::new();

        for sr in search_results {
            let target_id = sr.id;
            let mut stmt = conn.prepare(
                "SELECT snapshot_id, cpu_temp, cpu_usage, mglru_ttl, memory_pressure,
                        battery_charge, is_charging, governor, workload_tag, action_taken, timestamp_ms
                 FROM kernel_telemetry_snapshots WHERE snapshot_id = ?1"
            ).map_err(|e| HgbError::Storage(format!("Prepare select failed: {e}")))?;

            let res = stmt.query_row(params![target_id], |row| {
                let is_chg: i64 = row.get(6)?;
                let ts: i64 = row.get(10)?;
                let snap = HardwareKernelTelemetrySnapshot {
                    snapshot_id: row.get(0)?,
                    cpu_temp_celsius: row.get::<_, f64>(1)? as f32,
                    cpu_usage_pct: row.get::<_, f64>(2)? as f32,
                    mglru_min_ttl_ms: row.get::<_, i64>(3)? as u64,
                    memory_pressure_pct: row.get::<_, f64>(4)? as f32,
                    battery_charge_pct: row.get::<_, i64>(5)? as u8,
                    is_charging: is_chg != 0,
                    active_governor: row.get(7)?,
                    active_workload_tag: row.get(8)?,
                    timestamp_ms: ts as u64,
                };
                let action: String = row.get(9)?;
                Ok((snap, action))
            });

            if let Ok((snap, action)) = res {
                matches.push(TelemetryMatch {
                    snapshot: snap,
                    similarity_score: (1.0 - sr.distance).max(0.0),
                    historical_action_taken: action,
                });
            }
        }

        Ok(matches)
    }

    /// Retrieves recent snapshots from SQLite history
    pub fn query_recent_telemetry(&self, limit: usize) -> Result<Vec<HardwareKernelTelemetrySnapshot>> {
        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let mut stmt = conn.prepare(
            "SELECT snapshot_id, cpu_temp, cpu_usage, mglru_ttl, memory_pressure,
                    battery_charge, is_charging, governor, workload_tag, timestamp_ms
             FROM kernel_telemetry_snapshots
             ORDER BY timestamp_ms DESC
             LIMIT ?1"
        ).map_err(|e| HgbError::Storage(format!("Query failed: {e}")))?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            let is_chg: i64 = row.get(6)?;
            let ts: i64 = row.get(9)?;
            Ok(HardwareKernelTelemetrySnapshot {
                snapshot_id: row.get(0)?,
                cpu_temp_celsius: row.get::<_, f64>(1)? as f32,
                cpu_usage_pct: row.get::<_, f64>(2)? as f32,
                mglru_min_ttl_ms: row.get::<_, i64>(3)? as u64,
                memory_pressure_pct: row.get::<_, f64>(4)? as f32,
                battery_charge_pct: row.get::<_, i64>(5)? as u8,
                is_charging: is_chg != 0,
                active_governor: row.get(7)?,
                active_workload_tag: row.get(8)?,
                timestamp_ms: ts as u64,
            })
        }).map_err(|e| HgbError::Storage(format!("Rows mapping failed: {e}")))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
        }
        Ok(out)
    }
}

// ==============================================================================
// 3. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_vector_embedding_and_retrieval() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bridge = TelemetryVectorBridge::new(temp_dir.path().to_path_buf()).unwrap();

        let s1 = HardwareKernelTelemetrySnapshot::new(
            72.5, 88.0, 1000, 45.0, 85, true, "performance", "compile_heavy"
        );
        let s2 = HardwareKernelTelemetrySnapshot::new(
            45.0, 12.0, 5000, 10.0, 95, false, "powersave", "idle_browsing"
        );

        bridge.ingest_telemetry_snapshot(s1.clone(), "boost_fans_schedutil").unwrap();
        bridge.ingest_telemetry_snapshot(s2.clone(), "drop_epp_powersave").unwrap();

        // Query with probe near compile heavy
        let probe = HardwareKernelTelemetrySnapshot::new(
            74.0, 92.0, 1000, 48.0, 80, true, "performance", "compile_heavy"
        );

        let matches = bridge.query_similar_system_states(&probe, 2).unwrap();
        assert!(!matches.is_empty());
        assert_eq!(matches[0].snapshot.active_workload_tag, "compile_heavy");
        assert_eq!(matches[0].historical_action_taken, "boost_fans_schedutil");
        assert!(matches[0].similarity_score > 0.85);

        let history = bridge.query_recent_telemetry(10).unwrap();
        assert_eq!(history.len(), 2);
    }
}
