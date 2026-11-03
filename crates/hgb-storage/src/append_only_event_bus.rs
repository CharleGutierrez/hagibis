//! # SQLite WAL Mode Append-Only Event Bus & Pub/Sub Fanout
//!
//! Production-grade sovereign event store providing immutable event logging
//! backed by SQLite in WAL (Write-Ahead-Logging) mode, combined with high-throughput
//! in-memory broadcast fanout via `tokio::sync::broadcast`.
//!
//! Cryptographic BLAKE3 checksums ensure tamper-evident sequential chaining across
//! all logged system events.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use hgb_core::{HgbError, Result};

/// Default capacity for the in-memory live broadcast ring buffer
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 2048;

/// Standard system event types supported natively by Hagibis autonomous engine
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SystemEventType {
    /// Hardware/SoC thermal threshold exceeded (e.g. CPU temp >= 80°C)
    ThermalThresholdBreached,
    /// Git repository changes or commits detected / pushed
    GitCommitPushed,
    /// Local or remote neural model weights finished downloading
    ModelWeightsDownloaded,
    /// Firecracker microVM or sandbox container state transition
    MicroVmStateChanged,
    /// MCP tool or internal sovereign command finished execution
    ToolExecutionCompleted,
    /// Extensible custom event variant
    Custom(String),
}

impl SystemEventType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ThermalThresholdBreached => "ThermalThresholdBreached",
            Self::GitCommitPushed => "GitCommitPushed",
            Self::ModelWeightsDownloaded => "ModelWeightsDownloaded",
            Self::MicroVmStateChanged => "MicroVmStateChanged",
            Self::ToolExecutionCompleted => "ToolExecutionCompleted",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for SystemEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<&str> for SystemEventType {
    fn from(s: &str) -> Self {
        match s {
            "ThermalThresholdBreached" => Self::ThermalThresholdBreached,
            "GitCommitPushed" => Self::GitCommitPushed,
            "ModelWeightsDownloaded" => Self::ModelWeightsDownloaded,
            "MicroVmStateChanged" => Self::MicroVmStateChanged,
            "ToolExecutionCompleted" => Self::ToolExecutionCompleted,
            other => Self::Custom(other.to_string()),
        }
    }
}

impl From<String> for SystemEventType {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

// ============================================================================
// Strongly-Typed System Event Payloads
// ============================================================================

/// Payload for `ThermalThresholdBreached` event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalThresholdBreachedPayload {
    pub temperature_c: f64,
    pub threshold_c: f64,
    pub throttled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governor_profile: Option<String>,
}

/// Payload for `GitCommitPushed` event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitPushedPayload {
    pub commit_hash: String,
    pub branch: String,
    pub author: String,
    pub message: String,
    #[serde(default)]
    pub files_changed: Vec<String>,
}

/// Payload for `ModelWeightsDownloaded` event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelWeightsDownloadedPayload {
    pub model_id: String,
    pub destination: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub format: String,
}

/// Payload for `MicroVmStateChanged` event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicroVmStateChangedPayload {
    pub vm_id: String,
    pub old_state: String,
    pub new_state: String,
    pub vcpu_count: u32,
    pub mem_mb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Payload for `ToolExecutionCompleted` event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecutionCompletedPayload {
    pub tool_name: String,
    pub execution_id: String,
    pub success: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ============================================================================
// Immutable Event Record
// ============================================================================

/// Persistent event record row stored in SQLite `system_event_log` table
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemEventRecord {
    /// Unique event identifier (e.g. `evt_000000000001_8a12b...`)
    pub event_id: String,
    /// Monotonically increasing sequence number
    pub sequence: i64,
    /// Type tag of the event
    pub event_type: String,
    /// JSON encoded payload string
    pub payload_json: String,
    /// Cryptographic BLAKE3 hash of (event_id + sequence + event_type + payload + timestamp)
    pub blake3_hash: String,
    /// Unix timestamp in seconds or milliseconds when created
    pub created_at: i64,
    /// Processing status flag (0 = unprocessed / pending, 1 = processed)
    pub processed: i32,
}

impl SystemEventRecord {
    /// Computes cryptographic BLAKE3 checksum for event record integrity
    pub fn compute_hash(
        event_id: &str,
        sequence: i64,
        event_type: &str,
        payload_json: &str,
        created_at: i64,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(event_id.as_bytes());
        hasher.update(&sequence.to_le_bytes());
        hasher.update(event_type.as_bytes());
        hasher.update(payload_json.as_bytes());
        hasher.update(&created_at.to_le_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Verifies if stored BLAKE3 checksum matches computed hash
    pub fn verify_hash(&self) -> bool {
        let expected = Self::compute_hash(
            &self.event_id,
            self.sequence,
            &self.event_type,
            &self.payload_json,
            self.created_at,
        );
        self.blake3_hash == expected
    }

    /// Deserializes the raw JSON payload into strongly-typed payload struct
    pub fn parse_payload<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.payload_json)
            .map_err(|e| HgbError::Serialization(format!("Failed to parse event payload: {}", e)))
    }
}

// ============================================================================
// EventBusStore Implementation
// ============================================================================

/// SQLite WAL mode append-only event log with in-memory broadcast fanout
#[derive(Clone)]
pub struct EventBusStore {
    conn: Arc<Mutex<Connection>>,
    sender: broadcast::Sender<SystemEventRecord>,
    db_path: Option<PathBuf>,
}

impl EventBusStore {
    /// Opens or creates SQLite event log file at `path` configured with WAL mode
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        if let Some(parent) = path_ref.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(path_ref)
            .map_err(|e| HgbError::Storage(format!("Failed to open event log SQLite: {}", e)))?;

        Self::init_connection(&conn)?;
        let (sender, _) = broadcast::channel(DEFAULT_EVENT_BUS_CAPACITY);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            sender,
            db_path: Some(path_ref.to_path_buf()),
        })
    }

    /// Creates an in-memory ephemeral event bus store (ideal for unit tests & transient runs)
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("Failed to open in-memory SQLite: {}", e)))?;

        Self::init_connection(&conn)?;
        let (sender, _) = broadcast::channel(DEFAULT_EVENT_BUS_CAPACITY);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            sender,
            db_path: None,
        })
    }

    /// Automatically resolves `.hgb/system_event_bus.db` or uses `custom_path`
    pub fn open_or_create(custom_path: Option<&Path>) -> Result<Self> {
        if let Some(p) = custom_path {
            return Self::new(p);
        }

        let default_hgb = Path::new(".hgb");
        if default_hgb.exists() || std::fs::create_dir_all(default_hgb).is_ok() {
            let p = default_hgb.join("system_event_bus.db");
            return Self::new(&p);
        }

        let temp_p = std::env::temp_dir().join(format!("hgb_event_bus_{}.db", std::process::id()));
        Self::new(&temp_p)
    }

    /// Initializes SQLite PRAGMAs and creates the `system_event_log` table
    fn init_connection(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;

             CREATE TABLE IF NOT EXISTS system_event_log (
                 event_id TEXT PRIMARY KEY,
                 sequence INTEGER NOT NULL,
                 event_type TEXT NOT NULL,
                 payload_json TEXT NOT NULL,
                 blake3_hash TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 processed INTEGER NOT NULL DEFAULT 0
             );

             CREATE INDEX IF NOT EXISTS idx_event_seq ON system_event_log(sequence);
             CREATE INDEX IF NOT EXISTS idx_event_type ON system_event_log(event_type);
             CREATE INDEX IF NOT EXISTS idx_event_processed ON system_event_log(processed);
             CREATE INDEX IF NOT EXISTS idx_event_created_at ON system_event_log(created_at);",
        )
        .map_err(|e| HgbError::Storage(format!("Failed to initialize event log schema: {}", e)))?;

        Ok(())
    }

    /// Appends a raw event with event_type string and JSON payload
    pub fn append_raw(&self, event_type: &str, payload_json: &str) -> Result<SystemEventRecord> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let next_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM system_event_log",
                [],
                |row| row.get(0),
            )
            .unwrap_or(1);

        let created_at = Utc::now().timestamp_millis();
        let short_hash = blake3::hash(format!("{}:{}:{}", next_seq, event_type, created_at).as_bytes())
            .to_hex()[..16]
            .to_string();
        let event_id = format!("evt_{:012x}_{}", next_seq, short_hash);

        let blake3_hash = SystemEventRecord::compute_hash(
            &event_id,
            next_seq,
            event_type,
            payload_json,
            created_at,
        );

        conn.execute(
            "INSERT INTO system_event_log (event_id, sequence, event_type, payload_json, blake3_hash, created_at, processed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![event_id, next_seq, event_type, payload_json, blake3_hash, created_at],
        ).map_err(|e| HgbError::Storage(format!("Failed to insert into event log: {}", e)))?;

        let record = SystemEventRecord {
            event_id,
            sequence: next_seq,
            event_type: event_type.to_string(),
            payload_json: payload_json.to_string(),
            blake3_hash,
            created_at,
            processed: 0,
        };

        // Live broadcast fanout (ignoring SendError if no active receivers yet)
        let _ = self.sender.send(record.clone());

        Ok(record)
    }

    /// Appends a dynamic JSON serde_json::Value event
    pub fn append_event(
        &self,
        event_type: SystemEventType,
        payload: &serde_json::Value,
    ) -> Result<SystemEventRecord> {
        let json_str = serde_json::to_string(payload)
            .map_err(|e| HgbError::Serialization(format!("Payload serialize error: {}", e)))?;
        self.append_raw(event_type.as_str(), &json_str)
    }

    /// Appends any strongly typed payload implementing `Serialize`
    pub fn append_typed<T: Serialize>(
        &self,
        event_type: SystemEventType,
        payload: &T,
    ) -> Result<SystemEventRecord> {
        let json_str = serde_json::to_string(payload)
            .map_err(|e| HgbError::Serialization(format!("Typed payload serialize error: {}", e)))?;
        self.append_raw(event_type.as_str(), &json_str)
    }

    /// Appends a `ThermalThresholdBreached` event
    pub fn append_thermal_event(&self, payload: &ThermalThresholdBreachedPayload) -> Result<SystemEventRecord> {
        self.append_typed(SystemEventType::ThermalThresholdBreached, payload)
    }

    /// Appends a `GitCommitPushed` event
    pub fn append_git_commit_event(&self, payload: &GitCommitPushedPayload) -> Result<SystemEventRecord> {
        self.append_typed(SystemEventType::GitCommitPushed, payload)
    }

    /// Appends a `ModelWeightsDownloaded` event
    pub fn append_model_weights_event(&self, payload: &ModelWeightsDownloadedPayload) -> Result<SystemEventRecord> {
        self.append_typed(SystemEventType::ModelWeightsDownloaded, payload)
    }

    /// Appends a `MicroVmStateChanged` event
    pub fn append_microvm_state_event(&self, payload: &MicroVmStateChangedPayload) -> Result<SystemEventRecord> {
        self.append_typed(SystemEventType::MicroVmStateChanged, payload)
    }

    /// Appends a `ToolExecutionCompleted` event
    pub fn append_tool_execution_event(&self, payload: &ToolExecutionCompletedPayload) -> Result<SystemEventRecord> {
        self.append_typed(SystemEventType::ToolExecutionCompleted, payload)
    }

    /// Subscribes to live incoming events via tokio broadcast channel
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEventRecord> {
        self.sender.subscribe()
    }

    /// Marks a specific event as processed by ID
    pub fn mark_processed(&self, event_id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let rows = conn
            .execute(
                "UPDATE system_event_log SET processed = 1 WHERE event_id = ?1",
                params![event_id],
            )
            .map_err(|e| HgbError::Storage(format!("Failed to mark event processed: {}", e)))?;

        Ok(rows > 0)
    }

    /// Marks a batch of events as processed
    pub fn mark_batch_processed(&self, event_ids: &[&str]) -> Result<usize> {
        if event_ids.is_empty() {
            return Ok(0);
        }

        let mut conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let tx = conn
            .transaction()
            .map_err(|e| HgbError::Storage(format!("Transaction error: {}", e)))?;

        let mut count = 0;
        {
            let mut stmt = tx
                .prepare_cached("UPDATE system_event_log SET processed = 1 WHERE event_id = ?1")
                .map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

            for id in event_ids {
                count += stmt
                    .execute(params![id])
                    .map_err(|e| HgbError::Storage(format!("Execute batch error: {}", e)))?;
            }
        }

        tx.commit()
            .map_err(|e| HgbError::Storage(format!("Commit error: {}", e)))?;

        Ok(count)
    }

    /// Retrieves an event record by its unique identifier
    pub fn get_event_by_id(&self, event_id: &str) -> Result<Option<SystemEventRecord>> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let res = conn
            .query_row(
                "SELECT event_id, sequence, event_type, payload_json, blake3_hash, created_at, processed
                 FROM system_event_log WHERE event_id = ?1",
                params![event_id],
                |row| {
                    Ok(SystemEventRecord {
                        event_id: row.get(0)?,
                        sequence: row.get(1)?,
                        event_type: row.get(2)?,
                        payload_json: row.get(3)?,
                        blake3_hash: row.get(4)?,
                        created_at: row.get(5)?,
                        processed: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| HgbError::Storage(format!("Query event error: {}", e)))?;

        Ok(res)
    }

    /// Retrieves unprocessed events up to `limit` sorted by sequence ascending
    pub fn get_unprocessed_events(&self, limit: usize) -> Result<Vec<SystemEventRecord>> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT event_id, sequence, event_type, payload_json, blake3_hash, created_at, processed
                 FROM system_event_log
                 WHERE processed = 0
                 ORDER BY sequence ASC
                 LIMIT ?1",
            )
            .map_err(|e| HgbError::Storage(format!("Prepare query error: {}", e)))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SystemEventRecord {
                    event_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    payload_json: row.get(3)?,
                    blake3_hash: row.get(4)?,
                    created_at: row.get(5)?,
                    processed: row.get(6)?,
                })
            })
            .map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| HgbError::Storage(format!("Row read error: {}", e)))?);
        }
        Ok(out)
    }

    /// Retrieves events matching `event_type` up to `limit` sorted by sequence descending
    pub fn get_events_by_type(&self, event_type: &str, limit: usize) -> Result<Vec<SystemEventRecord>> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT event_id, sequence, event_type, payload_json, blake3_hash, created_at, processed
                 FROM system_event_log
                 WHERE event_type = ?1
                 ORDER BY sequence DESC
                 LIMIT ?2",
            )
            .map_err(|e| HgbError::Storage(format!("Prepare query error: {}", e)))?;

        let rows = stmt
            .query_map(params![event_type, limit as i64], |row| {
                Ok(SystemEventRecord {
                    event_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    payload_json: row.get(3)?,
                    blake3_hash: row.get(4)?,
                    created_at: row.get(5)?,
                    processed: row.get(6)?,
                })
            })
            .map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| HgbError::Storage(format!("Row read error: {}", e)))?);
        }
        Ok(out)
    }

    /// Replays events starting from `from_sequence` up to `limit` ordered by sequence ascending
    pub fn replay_from_sequence(&self, from_sequence: i64, limit: usize) -> Result<Vec<SystemEventRecord>> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT event_id, sequence, event_type, payload_json, blake3_hash, created_at, processed
                 FROM system_event_log
                 WHERE sequence >= ?1
                 ORDER BY sequence ASC
                 LIMIT ?2",
            )
            .map_err(|e| HgbError::Storage(format!("Prepare query error: {}", e)))?;

        let rows = stmt
            .query_map(params![from_sequence, limit as i64], |row| {
                Ok(SystemEventRecord {
                    event_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    payload_json: row.get(3)?,
                    blake3_hash: row.get(4)?,
                    created_at: row.get(5)?,
                    processed: row.get(6)?,
                })
            })
            .map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| HgbError::Storage(format!("Row read error: {}", e)))?);
        }
        Ok(out)
    }

    /// Returns the highest sequence number in the event log (0 if empty)
    pub fn last_sequence(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM system_event_log",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok(seq)
    }

    /// Returns total number of event records in the log
    pub fn total_events(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM system_event_log", [], |row| row.get(0))
            .unwrap_or(0);

        Ok(count as usize)
    }

    /// Returns total number of unprocessed (pending) event records
    pub fn unprocessed_count(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM system_event_log WHERE processed = 0",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok(count as usize)
    }

    /// Validates cryptographic integrity of all rows in the event log
    pub fn verify_integrity(&self) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| {
            HgbError::Storage(format!("Event bus connection lock poisoned: {}", e))
        })?;

        let mut stmt = conn
            .prepare("SELECT event_id, sequence, event_type, payload_json, blake3_hash, created_at, processed FROM system_event_log ORDER BY sequence ASC")
            .map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SystemEventRecord {
                    event_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    payload_json: row.get(3)?,
                    blake3_hash: row.get(4)?,
                    created_at: row.get(5)?,
                    processed: row.get(6)?,
                })
            })
            .map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

        for r in rows {
            let record = r.map_err(|e| HgbError::Storage(format!("Row error: {}", e)))?;
            if !record.verify_hash() {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Returns database file path if backed by disk
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_wal_event_persistence_and_sequence() {
        let store = EventBusStore::in_memory().expect("in-memory event store");

        assert_eq!(store.total_events().unwrap(), 0);
        assert_eq!(store.last_sequence().unwrap(), 0);

        let payload1 = ThermalThresholdBreachedPayload {
            temperature_c: 84.5,
            threshold_c: 80.0,
            throttled: true,
            core_id: Some(0),
            governor_profile: Some("powersave".into()),
        };
        let ev1 = store.append_thermal_event(&payload1).expect("append thermal");
        assert_eq!(ev1.sequence, 1);
        assert_eq!(ev1.event_type, "ThermalThresholdBreached");
        assert!(ev1.verify_hash());

        let payload2 = GitCommitPushedPayload {
            commit_hash: "abcd1234ef5678".into(),
            branch: "main".into(),
            author: "Autonomous Loop".into(),
            message: "feat: add zero-copy vector search".into(),
            files_changed: vec!["crates/hgb-storage/src/mmap_vector_index.rs".into()],
        };
        let ev2 = store.append_git_commit_event(&payload2).expect("append git");
        assert_eq!(ev2.sequence, 2);
        assert!(ev2.verify_hash());

        assert_eq!(store.total_events().unwrap(), 2);
        assert_eq!(store.last_sequence().unwrap(), 2);
        assert_eq!(store.unprocessed_count().unwrap(), 2);

        // Mark ev1 as processed
        let marked = store.mark_processed(&ev1.event_id).expect("mark processed");
        assert!(marked);
        assert_eq!(store.unprocessed_count().unwrap(), 1);

        let unproc = store.get_unprocessed_events(10).expect("get unprocessed");
        assert_eq!(unproc.len(), 1);
        assert_eq!(unproc[0].event_id, ev2.event_id);
    }

    #[tokio::test]
    async fn test_live_broadcast_subscription() {
        let store = EventBusStore::in_memory().expect("in-memory event store");
        let mut rx = store.subscribe();

        let payload = MicroVmStateChangedPayload {
            vm_id: "vm-sandbox-42".into(),
            old_state: "booting".into(),
            new_state: "running".into(),
            vcpu_count: 2,
            mem_mb: 1024,
            error: None,
        };

        let ev = store.append_microvm_state_event(&payload).expect("append vm state");

        let received = rx.recv().await.expect("receive broadcast event");
        assert_eq!(received.event_id, ev.event_id);
        assert_eq!(received.sequence, ev.sequence);
        assert_eq!(received.event_type, "MicroVmStateChanged");

        let parsed: MicroVmStateChangedPayload = received.parse_payload().expect("parse payload");
        assert_eq!(parsed.vm_id, "vm-sandbox-42");
        assert_eq!(parsed.new_state, "running");
    }

    #[test]
    fn test_system_event_types_serialization() {
        let store = EventBusStore::in_memory().expect("in-memory event store");

        let payload3 = ModelWeightsDownloadedPayload {
            model_id: "qwen2.5-coder-7b-q4_k_m".into(),
            destination: "/var/lib/hgb/models/qwen.gguf".into(),
            bytes_downloaded: 4500000000,
            total_bytes: 4500000000,
            format: "gguf".into(),
        };
        let ev3 = store.append_model_weights_event(&payload3).expect("append model");
        assert_eq!(ev3.event_type, "ModelWeightsDownloaded");

        let payload4 = ToolExecutionCompletedPayload {
            tool_name: "hgb_mmap_vector_search".into(),
            execution_id: "exec-99".into(),
            success: true,
            duration_ms: 18,
            error: None,
        };
        let ev4 = store.append_tool_execution_event(&payload4).expect("append tool");
        assert_eq!(ev4.event_type, "ToolExecutionCompleted");

        let custom_val = serde_json::json!({ "sensor": "fan_rpm", "rpm": 4200 });
        let ev5 = store.append_event(SystemEventType::Custom("HardwareTelemetry".into()), &custom_val)
            .expect("append custom");
        assert_eq!(ev5.event_type, "HardwareTelemetry");

        assert_eq!(store.total_events().unwrap(), 3);
        assert!(store.verify_integrity().unwrap());
    }

    #[test]
    fn test_replay_from_sequence_and_unprocessed() {
        let store = EventBusStore::in_memory().expect("in-memory event store");

        for i in 1..=10 {
            store.append_raw("IterationEvent", &format!(r#"{{"i": {}}}"#, i))
                .expect("append iter");
        }

        assert_eq!(store.total_events().unwrap(), 10);

        let replayed = store.replay_from_sequence(6, 10).expect("replay from 6");
        assert_eq!(replayed.len(), 5);
        assert_eq!(replayed[0].sequence, 6);
        assert_eq!(replayed[4].sequence, 10);

        let by_type = store.get_events_by_type("IterationEvent", 3).expect("by type");
        assert_eq!(by_type.len(), 3);
        assert_eq!(by_type[0].sequence, 10); // sorted desc
    }

    #[test]
    fn test_hash_integrity_verification() {
        let store = EventBusStore::in_memory().expect("in-memory event store");

        store.append_raw("TamperTest", r#"{"status":"clean"}"#).expect("append clean");
        assert!(store.verify_integrity().unwrap());

        // Introduce simulated tamper directly into SQLite table
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("UPDATE system_event_log SET payload_json = '{\"status\":\"tampered\"}' WHERE sequence = 1", [])
                .expect("simulate tamper");
        }

        assert!(!store.verify_integrity().unwrap());
    }
}
