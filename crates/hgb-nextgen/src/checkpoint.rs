use blake3::Hasher;
use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmEvent {
    pub sequence: u64,
    pub timestamp_utc: String,
    pub event_type: String,
    pub payload_json: String,
    pub digest: String,
}

pub struct SwarmWal {
    events: Vec<SwarmEvent>,
}

impl Default for SwarmWal {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmWal {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn append(&mut self, event_type: &str, payload_json: &str) -> SwarmEvent {
        let sequence = (self.events.len() + 1) as u64;
        let timestamp_utc = chrono::Utc::now().to_rfc3339();
        let mut h = Hasher::new();
        h.update(payload_json.as_bytes());
        let digest = h.finalize().to_hex().to_string();

        let ev = SwarmEvent {
            sequence,
            timestamp_utc,
            event_type: event_type.to_string(),
            payload_json: payload_json.to_string(),
            digest,
        };
        self.events.push(ev.clone());
        ev
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// State of an individual file captured prior to mutation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileSnapshotState {
    pub path: PathBuf,
    pub hash: String,
    pub original_content: Option<Vec<u8>>,
    pub existed_before: bool,
}

/// A complete checkpoint capturing agent memory, node states, and filesystem snapshots
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmCheckpoint {
    pub checkpoint_id: String,
    pub sequence: u64,
    pub label: String,
    #[serde(default)]
    pub timestamp_utc: String,
    pub node_states: HashMap<String, String>,
    pub memory: HashMap<String, String>,
    #[serde(default)]
    pub file_snapshots: HashMap<PathBuf, FileSnapshotState>,
    pub state_hash: String,
}

/// Outcome of executing a time-travel rollback
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackReport {
    pub checkpoint_id: String,
    pub files_restored: Vec<PathBuf>,
    pub files_deleted: Vec<PathBuf>,
    pub duration_ms: u64,
}

pub struct SwarmCheckpointManager {
    wal: SwarmWal,
    checkpoints: Vec<SwarmCheckpoint>,
    pending_snapshots: HashMap<PathBuf, FileSnapshotState>,
}

impl Default for SwarmCheckpointManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmCheckpointManager {
    pub fn new() -> Self {
        Self {
            wal: SwarmWal::new(),
            checkpoints: Vec::new(),
            pending_snapshots: HashMap::new(),
        }
    }

    pub fn record_event(&mut self, event_type: &str, payload_json: &str) -> SwarmEvent {
        self.wal.append(event_type, payload_json)
    }

    /// Snapshot a file before mutation. If already snapshotted in current pending batch, retains original.
    pub fn snapshot_file<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let p = path.as_ref().to_path_buf();
        if self.pending_snapshots.contains_key(&p) {
            return Ok(());
        }

        if p.exists() && p.is_file() {
            let mut file = File::open(&p)?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;

            let mut h = Hasher::new();
            h.update(&buf);
            let hash = h.finalize().to_hex().to_string();

            self.pending_snapshots.insert(p.clone(), FileSnapshotState {
                path: p,
                hash,
                original_content: Some(buf),
                existed_before: true,
            });
        } else {
            self.pending_snapshots.insert(p.clone(), FileSnapshotState {
                path: p,
                hash: "non_existent".to_string(),
                original_content: None,
                existed_before: false,
            });
        }

        Ok(())
    }

    pub fn create_checkpoint(
        &mut self,
        label: &str,
        node_states: HashMap<String, String>,
        memory: HashMap<String, String>,
    ) -> SwarmCheckpoint {
        let sequence = self.wal.len() as u64;
        let timestamp_utc = chrono::Utc::now().to_rfc3339();
        let mut h = Hasher::new();

        for (k, v) in &node_states {
            h.update(k.as_bytes());
            h.update(v.as_bytes());
        }
        for (k, v) in &memory {
            h.update(k.as_bytes());
            h.update(v.as_bytes());
        }
        for (path, snap) in &self.pending_snapshots {
            h.update(path.to_string_lossy().as_bytes());
            h.update(snap.hash.as_bytes());
        }

        let state_hash = h.finalize().to_hex().to_string();
        let checkpoint_id = format!("ckpt-{}-{}", sequence, &state_hash[..8]);

        let file_snapshots = std::mem::take(&mut self.pending_snapshots);

        let ckpt = SwarmCheckpoint {
            checkpoint_id,
            sequence,
            label: label.to_string(),
            timestamp_utc,
            node_states,
            memory,
            file_snapshots,
            state_hash,
        };
        self.checkpoints.push(ckpt.clone());
        ckpt
    }

    /// Read checkpoint by ID (backward compatible)
    pub fn rollback_to_checkpoint(&self, checkpoint_id: &str) -> Option<&SwarmCheckpoint> {
        self.checkpoints.iter().find(|c| c.checkpoint_id == checkpoint_id)
    }

    /// Restore files from a specific checkpoint ID
    pub fn restore_files_from_checkpoint(&self, checkpoint_id: &str) -> Result<RollbackReport> {
        let start = Instant::now();
        let ckpt = self
            .rollback_to_checkpoint(checkpoint_id)
            .ok_or_else(|| HgbError::NotFound(format!("Checkpoint '{}' not found", checkpoint_id)))?;

        let mut files_restored = Vec::new();
        let mut files_deleted = Vec::new();

        for (path, snap) in &ckpt.file_snapshots {
            if snap.existed_before {
                if let Some(ref content) = snap.original_content {
                    if let Some(parent) = path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let mut file = File::create(path)?;
                    file.write_all(content)?;
                    files_restored.push(path.clone());
                }
            } else if path.exists() {
                let _ = fs::remove_file(path);
                files_deleted.push(path.clone());
            }
        }

        Ok(RollbackReport {
            checkpoint_id: checkpoint_id.to_string(),
            files_restored,
            files_deleted,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Rollback files to the most recent checkpoint with file snapshots
    pub fn rollback_latest_files(&self) -> Result<RollbackReport> {
        let ckpt = self
            .checkpoints
            .iter()
            .rev()
            .find(|c| !c.file_snapshots.is_empty())
            .or_else(|| self.checkpoints.last())
            .ok_or_else(|| HgbError::NotFound("No checkpoints available for rollback".to_string()))?;

        self.restore_files_from_checkpoint(&ckpt.checkpoint_id)
    }

    pub fn checkpoints_count(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn get_checkpoints(&self) -> &[SwarmCheckpoint] {
        &self.checkpoints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_snapshot_and_rollback() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_ckpt_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let file1 = temp_dir.join("existing.txt");
        let file2 = temp_dir.join("new_file.txt");

        fs::write(&file1, b"original content v1").unwrap();

        let mut mgr = SwarmCheckpointManager::new();

        // 1. Snapshot file1 before mutation and file2 before creation
        mgr.snapshot_file(&file1).unwrap();
        mgr.snapshot_file(&file2).unwrap();

        // 2. Create checkpoint
        let ckpt = mgr.create_checkpoint("turn_1", HashMap::new(), HashMap::new());

        // 3. Mutate file1 and create file2
        fs::write(&file1, b"mutated content v2").unwrap();
        fs::write(&file2, b"brand new file").unwrap();
        assert_eq!(fs::read(&file1).unwrap(), b"mutated content v2");
        assert!(file2.exists());

        // 4. Rollback
        let report = mgr.restore_files_from_checkpoint(&ckpt.checkpoint_id).unwrap();
        assert_eq!(report.files_restored.len(), 1);
        assert_eq!(report.files_deleted.len(), 1);

        // Verify restoration
        assert_eq!(fs::read(&file1).unwrap(), b"original content v1");
        assert!(!file2.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
