//! # MicroVM Tool Execution Log & Artifact Storage (`microvm_execution_log`)
//!
//! Production-grade SQLite-backed persistent store for ephemeral Firecracker and Landlock microVM
//! tool execution logs, output artifacts, stdout/stderr streams, and resource consumption telemetry.

use hgb_core::{HgbError, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::microvm_crdt_sync::ToolArtifactRecord;

/// Detailed persistent record of a sandboxed microVM job execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MicrovmExecutionRecord {
    pub job_id: String,
    pub command_or_code: String,
    pub language: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub spinup_duration_ms: u64,
    pub memory_consumed_mb: u32,
    pub blast_radius_bytes: usize,
    pub isolation_tier: String,
    pub worker_id: usize,
    pub was_stolen: bool,
    pub artifacts: Vec<ToolArtifactRecord>,
    pub created_at_ms: u64,
}

impl MicrovmExecutionRecord {
    pub fn new(
        job_id: impl Into<String>,
        command_or_code: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            job_id: job_id.into(),
            command_or_code: command_or_code.into(),
            language: language.into(),
            success: true,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            spinup_duration_ms: 0,
            memory_consumed_mb: 256,
            blast_radius_bytes: 0,
            isolation_tier: "FirecrackerHardwareMicroVM".to_string(),
            worker_id: 0,
            was_stolen: false,
            artifacts: Vec::new(),
            created_at_ms: now,
        }
    }
}

/// Persistent SQLite Store for microVM execution telemetry and tool artifacts
pub struct MicrovmExecutionLogStore {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl MicrovmExecutionLogStore {
    /// Initialize or open microVM execution log database at path
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("Failed to open microvm log DB: {}", e)))?;

        // Initialize tables and indexes
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;

             CREATE TABLE IF NOT EXISTS microvm_execution_logs (
                 job_id TEXT PRIMARY KEY,
                 command_or_code TEXT NOT NULL,
                 language TEXT NOT NULL,
                 success INTEGER NOT NULL,
                 exit_code INTEGER NOT NULL,
                 stdout TEXT NOT NULL,
                 stderr TEXT NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 spinup_duration_ms INTEGER NOT NULL,
                 memory_consumed_mb INTEGER NOT NULL,
                 blast_radius_bytes INTEGER NOT NULL,
                 isolation_tier TEXT NOT NULL,
                 worker_id INTEGER NOT NULL,
                 was_stolen INTEGER NOT NULL,
                 artifacts_json TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_microvm_logs_created ON microvm_execution_logs(created_at_ms DESC);

             CREATE TABLE IF NOT EXISTS microvm_artifacts (
                 artifact_id TEXT PRIMARY KEY,
                 job_id TEXT NOT NULL,
                 vm_id TEXT NOT NULL,
                 path TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 artifact_type TEXT NOT NULL,
                 size_bytes INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_microvm_artifacts_job ON microvm_artifacts(job_id);",
        ).map_err(|e| HgbError::Storage(format!("Failed to initialize microvm log schema: {}", e)))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    /// Access the SQLite database file path
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Record a completed microVM job execution and its captured artifacts
    pub fn record_execution(&self, record: &MicrovmExecutionRecord) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let artifacts_json = serde_json::to_string(&record.artifacts)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        conn.execute(
            "INSERT OR REPLACE INTO microvm_execution_logs (
                job_id, command_or_code, language, success, exit_code, stdout, stderr,
                duration_ms, spinup_duration_ms, memory_consumed_mb, blast_radius_bytes,
                isolation_tier, worker_id, was_stolen, artifacts_json, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                record.job_id,
                record.command_or_code,
                record.language,
                if record.success { 1 } else { 0 },
                record.exit_code,
                record.stdout,
                record.stderr,
                record.duration_ms as i64,
                record.spinup_duration_ms as i64,
                record.memory_consumed_mb as i64,
                record.blast_radius_bytes as i64,
                record.isolation_tier,
                record.worker_id as i64,
                if record.was_stolen { 1 } else { 0 },
                artifacts_json,
                record.created_at_ms as i64,
            ],
        ).map_err(|e| HgbError::Storage(format!("Failed to insert microvm execution log: {}", e)))?;

        // Insert individual artifacts into index table
        for art in &record.artifacts {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO microvm_artifacts (
                    artifact_id, job_id, vm_id, path, content_hash, artifact_type, size_bytes, created_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    art.artifact_id,
                    record.job_id,
                    art.vm_id,
                    art.path,
                    art.content_hash,
                    art.artifact_type,
                    art.size_bytes as i64,
                    art.created_at_ms as i64,
                ],
            );
        }

        Ok(())
    }

    /// Retrieve a microVM execution log by job ID
    pub fn get_log(&self, job_id: &str) -> Result<Option<MicrovmExecutionRecord>> {
        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let mut stmt = conn.prepare(
            "SELECT job_id, command_or_code, language, success, exit_code, stdout, stderr,
                    duration_ms, spinup_duration_ms, memory_consumed_mb, blast_radius_bytes,
                    isolation_tier, worker_id, was_stolen, artifacts_json, created_at_ms
             FROM microvm_execution_logs WHERE job_id = ?1",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut rows = stmt.query(params![job_id]).map_err(|e| HgbError::Storage(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
            let artifacts_str: String = row.get(14).unwrap_or_default();
            let artifacts: Vec<ToolArtifactRecord> = serde_json::from_str(&artifacts_str).unwrap_or_default();

            Ok(Some(MicrovmExecutionRecord {
                job_id: row.get(0).unwrap_or_default(),
                command_or_code: row.get(1).unwrap_or_default(),
                language: row.get(2).unwrap_or_default(),
                success: row.get::<_, i64>(3).unwrap_or(0) == 1,
                exit_code: row.get(4).unwrap_or(0),
                stdout: row.get(5).unwrap_or_default(),
                stderr: row.get(6).unwrap_or_default(),
                duration_ms: row.get::<_, i64>(7).unwrap_or(0) as u64,
                spinup_duration_ms: row.get::<_, i64>(8).unwrap_or(0) as u64,
                memory_consumed_mb: row.get::<_, i64>(9).unwrap_or(0) as u32,
                blast_radius_bytes: row.get::<_, i64>(10).unwrap_or(0) as usize,
                isolation_tier: row.get(11).unwrap_or_default(),
                worker_id: row.get::<_, i64>(12).unwrap_or(0) as usize,
                was_stolen: row.get::<_, i64>(13).unwrap_or(0) == 1,
                artifacts,
                created_at_ms: row.get::<_, i64>(15).unwrap_or(0) as u64,
            }))
        } else {
            Ok(None)
        }
    }

    /// Query the most recent execution logs up to limit
    pub fn query_logs(&self, limit: usize) -> Result<Vec<MicrovmExecutionRecord>> {
        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let mut stmt = conn.prepare(
            "SELECT job_id, command_or_code, language, success, exit_code, stdout, stderr,
                    duration_ms, spinup_duration_ms, memory_consumed_mb, blast_radius_bytes,
                    isolation_tier, worker_id, was_stolen, artifacts_json, created_at_ms
             FROM microvm_execution_logs
             ORDER BY created_at_ms DESC
             LIMIT ?1",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            let artifacts_str: String = row.get(14).unwrap_or_default();
            let artifacts: Vec<ToolArtifactRecord> = serde_json::from_str(&artifacts_str).unwrap_or_default();

            Ok(MicrovmExecutionRecord {
                job_id: row.get(0).unwrap_or_default(),
                command_or_code: row.get(1).unwrap_or_default(),
                language: row.get(2).unwrap_or_default(),
                success: row.get::<_, i64>(3).unwrap_or(0) == 1,
                exit_code: row.get(4).unwrap_or(0),
                stdout: row.get(5).unwrap_or_default(),
                stderr: row.get(6).unwrap_or_default(),
                duration_ms: row.get::<_, i64>(7).unwrap_or(0) as u64,
                spinup_duration_ms: row.get::<_, i64>(8).unwrap_or(0) as u64,
                memory_consumed_mb: row.get::<_, i64>(9).unwrap_or(0) as u32,
                blast_radius_bytes: row.get::<_, i64>(10).unwrap_or(0) as usize,
                isolation_tier: row.get(11).unwrap_or_default(),
                worker_id: row.get::<_, i64>(12).unwrap_or(0) as usize,
                was_stolen: row.get::<_, i64>(13).unwrap_or(0) == 1,
                artifacts,
                created_at_ms: row.get::<_, i64>(15).unwrap_or(0) as u64,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut list = Vec::new();
        for r in rows {
            if let Ok(rec) = r {
                list.push(rec);
            }
        }
        Ok(list)
    }

    /// Retrieve artifacts recorded for a specific job
    pub fn get_artifacts_for_job(&self, job_id: &str) -> Result<Vec<ToolArtifactRecord>> {
        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let mut stmt = conn.prepare(
            "SELECT artifact_id, vm_id, path, content_hash, artifact_type, size_bytes, created_at_ms
             FROM microvm_artifacts WHERE job_id = ?1 ORDER BY created_at_ms ASC",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map(params![job_id], |row| {
            Ok(ToolArtifactRecord {
                artifact_id: row.get(0).unwrap_or_default(),
                vm_id: row.get(1).unwrap_or_default(),
                path: row.get(2).unwrap_or_default(),
                content_hash: row.get(3).unwrap_or_default(),
                artifact_type: row.get(4).unwrap_or_default(),
                size_bytes: row.get::<_, i64>(5).unwrap_or(0) as usize,
                created_at_ms: row.get::<_, i64>(6).unwrap_or(0) as u64,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut list = Vec::new();
        for r in rows {
            if let Ok(rec) = r {
                list.push(rec);
            }
        }
        Ok(list)
    }

    /// Total number of logged executions
    pub fn total_logged(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| HgbError::Storage("Mutex poisoned".into()))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM microvm_execution_logs",
            [],
            |r| r.get(0),
        ).map_err(|e| HgbError::Storage(e.to_string()))?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_microvm_execution_log_store_lifecycle() {
        let temp_db = std::env::temp_dir().join(format!(
            "microvm_log_test_{}.db",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));

        let store = MicrovmExecutionLogStore::new(&temp_db).expect("create log store");

        let mut rec = MicrovmExecutionRecord::new("job_101", "echo 'test'", "bash");
        rec.stdout = "test output\n".to_string();
        rec.exit_code = 0;
        rec.duration_ms = 45;
        rec.spinup_duration_ms = 12;
        rec.isolation_tier = "Linux Landlock LSM + RAM CoW OverlaySandbox (/dev/shm)".to_string();

        let artifact = ToolArtifactRecord::new(
            "art_001",
            "vm_1",
            "out.txt",
            b"output artifact content",
            "text/plain",
        );
        rec.artifacts.push(artifact.clone());

        store.record_execution(&rec).expect("record execution");

        let fetched = store.get_log("job_101").expect("get log").expect("found");
        assert_eq!(fetched.job_id, "job_101");
        assert_eq!(fetched.stdout, "test output\n");
        assert_eq!(fetched.artifacts.len(), 1);
        assert_eq!(fetched.artifacts[0].artifact_id, "art_001");

        let artifacts = store.get_artifacts_for_job("job_101").expect("get artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].content_hash, artifact.content_hash);

        let recent = store.query_logs(10).expect("query logs");
        assert_eq!(recent.len(), 1);
        assert_eq!(store.total_logged().expect("total"), 1);

        let _ = std::fs::remove_file(&temp_db);
    }
}
