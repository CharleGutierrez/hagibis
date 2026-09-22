use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmCheckpoint {
    pub checkpoint_id: String,
    pub sequence: u64,
    pub label: String,
    pub node_states: HashMap<String, String>,
    pub memory: HashMap<String, String>,
    pub state_hash: String,
}

pub struct SwarmCheckpointManager {
    wal: SwarmWal,
    checkpoints: Vec<SwarmCheckpoint>,
}

impl SwarmCheckpointManager {
    pub fn new() -> Self {
        Self {
            wal: SwarmWal::new(),
            checkpoints: Vec::new(),
        }
    }

    pub fn record_event(&mut self, event_type: &str, payload_json: &str) -> SwarmEvent {
        self.wal.append(event_type, payload_json)
    }

    pub fn create_checkpoint(&mut self, label: &str, node_states: HashMap<String, String>, memory: HashMap<String, String>) -> SwarmCheckpoint {
        let sequence = self.wal.len() as u64;
        let mut h = Hasher::new();
        for (k, v) in &node_states {
            h.update(k.as_bytes());
            h.update(v.as_bytes());
        }
        for (k, v) in &memory {
            h.update(k.as_bytes());
            h.update(v.as_bytes());
        }
        let state_hash = h.finalize().to_hex().to_string();
        let checkpoint_id = format!("ckpt-{}-{}", sequence, &state_hash[..8]);

        let ckpt = SwarmCheckpoint {
            checkpoint_id,
            sequence,
            label: label.to_string(),
            node_states,
            memory,
            state_hash,
        };
        self.checkpoints.push(ckpt.clone());
        ckpt
    }

    pub fn rollback_to_checkpoint(&self, checkpoint_id: &str) -> Option<&SwarmCheckpoint> {
        self.checkpoints.iter().find(|c| c.checkpoint_id == checkpoint_id)
    }

    pub fn checkpoints_count(&self) -> usize {
        self.checkpoints.len()
    }
}
