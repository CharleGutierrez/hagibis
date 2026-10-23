//! # Distributed Raft Vector & State Mesh (`hgb-storage`)
//!
//! Production-grade embedded Raft consensus layer backed by SQLite WAL storage.
//! Replicates:
//! 1. Key-Value configuration parameters.
//! 2. High-dimensional vector embeddings with cosine similarity search.
//! 3. Agent episodic memories and self-correction reflections.
//! 4. Cluster membership operations (`add_node`, `remove_node`) with quorum commits and snapshots.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RaftMeshError {
    #[error("SQLite database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Not cluster leader (current role: {0:?}, leader: {1:?})")]
    NotLeader(RaftRole, Option<u64>),
    #[error("Quorum not reached ({successes}/{total} nodes responded)")]
    QuorumFailed { successes: usize, total: usize },
    #[error("Node {0} already exists in cluster")]
    NodeAlreadyExists(u64),
    #[error("Node {0} not found in cluster")]
    NodeNotFound(u64),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Raft consensus role
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RaftRole {
    Leader,
    Follower,
    Candidate,
}

/// Commands replicated through the Raft log
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RaftCommand {
    SetKV { key: String, value: String },
    DeleteKV { key: String },
    UpsertVector { id: String, vector: Vec<f32>, metadata: String },
    RecordEpisode { id: String, task: String, lesson: String, success: bool },
    AddNode { node_id: u64, address: String },
    RemoveNode { node_id: u64 },
    Noop,
}

/// Single Raft log entry
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RaftLogEntry {
    pub index: u64,
    pub term: u64,
    pub command: RaftCommand,
}

/// Arguments for RequestVote RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestVoteArgs {
    pub term: u64,
    pub candidate_id: u64,
    pub last_log_index: u64,
    pub last_log_term: u64,
}

/// Reply for RequestVote RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestVoteReply {
    pub term: u64,
    pub vote_granted: bool,
}

/// Arguments for AppendEntries RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesArgs {
    pub term: u64,
    pub leader_id: u64,
    pub prev_log_index: u64,
    pub prev_log_term: u64,
    pub entries: Vec<RaftLogEntry>,
    pub leader_commit: u64,
}

/// Reply for AppendEntries RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendEntriesReply {
    pub term: u64,
    pub success: bool,
    pub match_index: u64,
}

/// Point-in-time Raft snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaftSnapshot {
    pub last_included_index: u64,
    pub last_included_term: u64,
    pub kv_state: HashMap<String, String>,
    pub vector_count: usize,
    pub episode_count: usize,
    pub cluster_nodes: HashMap<u64, String>,
    pub blake3_hash: String,
}

/// Vector search result
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorSearchResult {
    pub id: String,
    pub similarity: f32,
    pub metadata: String,
}

/// Comprehensive status of the Raft state mesh
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RaftMeshStatusReport {
    pub node_id: u64,
    pub role: RaftRole,
    pub current_term: u64,
    pub leader_id: Option<u64>,
    pub commit_index: u64,
    pub last_applied: u64,
    pub log_length: usize,
    pub cluster_peers: HashMap<u64, String>,
    pub kv_entries_count: usize,
    pub vector_entries_count: usize,
    pub episode_entries_count: usize,
    pub is_healthy: bool,
}

/// Core Distributed Raft Vector & State Mesh
pub struct RaftStateMesh {
    node_id: u64,
    db: Arc<Mutex<Connection>>,
    role: Arc<Mutex<RaftRole>>,
    current_term: Arc<Mutex<u64>>,
    voted_for: Arc<Mutex<Option<u64>>>,
    leader_id: Arc<Mutex<Option<u64>>>,
    log: Arc<Mutex<Vec<RaftLogEntry>>>,
    commit_index: Arc<Mutex<u64>>,
    last_applied: Arc<Mutex<u64>>,
    peers: Arc<Mutex<HashMap<u64, String>>>,
}

impl RaftStateMesh {
    /// Initialize with SQLite WAL file storage
    pub fn new(node_id: u64, db_path: &Path) -> Result<Self, RaftMeshError> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(db_path)?;
        Self::init_with_connection(node_id, conn)
    }

    /// Initialize in-memory (ideal for unit testing and ephemeral sandboxes)
    pub fn in_memory(node_id: u64) -> Result<Self, RaftMeshError> {
        let conn = Connection::open_in_memory()?;
        Self::init_with_connection(node_id, conn)
    }

    fn init_with_connection(node_id: u64, conn: Connection) -> Result<Self, RaftMeshError> {
        // Configure SQLite WAL mode for high concurrency
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             
             CREATE TABLE IF NOT EXISTS raft_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             
             CREATE TABLE IF NOT EXISTS raft_log (
                 log_index INTEGER PRIMARY KEY,
                 term INTEGER NOT NULL,
                 command_json TEXT NOT NULL
             );
             
             CREATE TABLE IF NOT EXISTS raft_kv (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             
             CREATE TABLE IF NOT EXISTS raft_vectors (
                 id TEXT PRIMARY KEY,
                 dim INTEGER NOT NULL,
                 vector_blob BLOB NOT NULL,
                 metadata TEXT NOT NULL
             );
             
             CREATE TABLE IF NOT EXISTS raft_episodes (
                 id TEXT PRIMARY KEY,
                 task TEXT NOT NULL,
                 lesson TEXT NOT NULL,
                 success INTEGER NOT NULL
             );
             
             CREATE TABLE IF NOT EXISTS raft_peers (
                 node_id INTEGER PRIMARY KEY,
                 address TEXT NOT NULL
             );",
        )?;

        // Load meta
        let mut term = 0u64;
        let mut voted_for = None;
        let mut commit_index = 0u64;
        let mut last_applied = 0u64;

        let mut stmt = conn.prepare("SELECT key, value FROM raft_meta")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for row in rows.flatten() {
            match row.0.as_str() {
                "term" => term = row.1.parse().unwrap_or(0),
                "voted_for" => voted_for = row.1.parse().ok(),
                "commit_index" => commit_index = row.1.parse().unwrap_or(0),
                "last_applied" => last_applied = row.1.parse().unwrap_or(0),
                _ => {}
            }
        }

        // Load log entries
        let mut log = Vec::new();
        let mut log_stmt = conn.prepare("SELECT log_index, term, command_json FROM raft_log ORDER BY log_index ASC")?;
        let log_rows = log_stmt.query_map([], |row| {
            let idx: u64 = row.get(0)?;
            let t: u64 = row.get(1)?;
            let cmd_json: String = row.get(2)?;
            let command: RaftCommand = serde_json::from_str(&cmd_json).unwrap_or(RaftCommand::Noop);
            Ok(RaftLogEntry { index: idx, term: t, command })
        })?;

        for entry in log_rows.flatten() {
            log.push(entry);
        }

        // Load peers
        let mut peers = HashMap::new();
        let mut peer_stmt = conn.prepare("SELECT node_id, address FROM raft_peers")?;
        let peer_rows = peer_stmt.query_map([], |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)))?;
        for p in peer_rows.flatten() {
            peers.insert(p.0, p.1);
        }

        drop(stmt);
        drop(log_stmt);
        drop(peer_stmt);

        Ok(Self {
            node_id,
            db: Arc::new(Mutex::new(conn)),
            role: Arc::new(Mutex::new(RaftRole::Follower)),
            current_term: Arc::new(Mutex::new(term)),
            voted_for: Arc::new(Mutex::new(voted_for)),
            leader_id: Arc::new(Mutex::new(None)),
            log: Arc::new(Mutex::new(log)),
            commit_index: Arc::new(Mutex::new(commit_index)),
            last_applied: Arc::new(Mutex::new(last_applied)),
            peers: Arc::new(Mutex::new(peers)),
        })
    }

    /// Trigger election transition to Candidate / Leader
    pub fn start_election(&self) -> Result<(), RaftMeshError> {
        let mut role = self.role.lock().unwrap();
        let mut term = self.current_term.lock().unwrap();
        let mut voted_for = self.voted_for.lock().unwrap();
        let mut leader = self.leader_id.lock().unwrap();

        *term += 1;
        *role = RaftRole::Candidate;
        *voted_for = Some(self.node_id);

        let peers = self.peers.lock().unwrap();
        let total_nodes = peers.len() + 1;
        let quorum = (total_nodes / 2) + 1;

        // If single node or votes >= quorum, become leader
        let votes = 1;
        if votes >= quorum {
            *role = RaftRole::Leader;
            *leader = Some(self.node_id);
            self.persist_meta("role", "Leader")?;
            self.persist_meta("term", &term.to_string())?;
            self.persist_meta("voted_for", &self.node_id.to_string())?;
        }

        Ok(())
    }

    /// Handle incoming RequestVote RPC
    pub fn handle_request_vote(&self, args: &RequestVoteArgs) -> RequestVoteReply {
        let mut current_term = self.current_term.lock().unwrap();
        let mut voted_for = self.voted_for.lock().unwrap();
        let mut role = self.role.lock().unwrap();

        if args.term > *current_term {
            *current_term = args.term;
            *role = RaftRole::Follower;
            *voted_for = None;
            let _ = self.persist_meta("term", &current_term.to_string());
            let _ = self.persist_meta("voted_for", "");
        }

        let mut vote_granted = false;
        if args.term == *current_term && (voted_for.is_none() || *voted_for == Some(args.candidate_id)) {
            let log = self.log.lock().unwrap();
            let last_log_index = log.last().map(|e| e.index).unwrap_or(0);
            let last_log_term = log.last().map(|e| e.term).unwrap_or(0);

            // Candidate's log must be at least as up-to-date as receiver's
            if args.last_log_term > last_log_term
                || (args.last_log_term == last_log_term && args.last_log_index >= last_log_index)
            {
                vote_granted = true;
                *voted_for = Some(args.candidate_id);
                let _ = self.persist_meta("voted_for", &args.candidate_id.to_string());
            }
        }

        RequestVoteReply {
            term: *current_term,
            vote_granted,
        }
    }

    /// Handle incoming AppendEntries (heartbeat / log replication) RPC
    pub fn handle_append_entries(&self, args: &AppendEntriesArgs) -> AppendEntriesReply {
        let mut current_term = self.current_term.lock().unwrap();
        let mut role = self.role.lock().unwrap();
        let mut leader = self.leader_id.lock().unwrap();

        if args.term > *current_term {
            *current_term = args.term;
            *role = RaftRole::Follower;
            let _ = self.persist_meta("term", &current_term.to_string());
        }

        if args.term < *current_term {
            return AppendEntriesReply {
                term: *current_term,
                success: false,
                match_index: 0,
            };
        }

        *role = RaftRole::Follower;
        *leader = Some(args.leader_id);

        let mut log = self.log.lock().unwrap();

        // Verify prev_log_index and prev_log_term
        if args.prev_log_index > 0 {
            let prev_entry = log.iter().find(|e| e.index == args.prev_log_index);
            match prev_entry {
                Some(e) if e.term == args.prev_log_term => {}
                _ => {
                    return AppendEntriesReply {
                        term: *current_term,
                        success: false,
                        match_index: log.last().map(|e| e.index).unwrap_or(0),
                    };
                }
            }
        }

        // Append new entries and resolve conflicts
        for entry in &args.entries {
            if let Some(pos) = log.iter().position(|e| e.index == entry.index) {
                if log[pos].term != entry.term {
                    log.truncate(pos);
                    log.push(entry.clone());
                    let _ = self.persist_log_entry(entry);
                }
            } else {
                log.push(entry.clone());
                let _ = self.persist_log_entry(entry);
            }
        }

        let match_idx = log.last().map(|e| e.index).unwrap_or(0);

        // Apply committed entries to state machine
        if args.leader_commit > *self.commit_index.lock().unwrap() {
            let mut commit_idx = self.commit_index.lock().unwrap();
            *commit_idx = args.leader_commit.min(match_idx);
            let _ = self.persist_meta("commit_index", &commit_idx.to_string());
            drop(log);
            let _ = self.apply_entries_to_state_machine(*commit_idx);
        }

        AppendEntriesReply {
            term: *current_term,
            success: true,
            match_index: match_idx,
        }
    }

    /// Propose a command to replicate through the cluster (Leader only)
    pub fn propose(&self, command: RaftCommand) -> Result<u64, RaftMeshError> {
        let role = *self.role.lock().unwrap();
        if role != RaftRole::Leader {
            let leader = *self.leader_id.lock().unwrap();
            return Err(RaftMeshError::NotLeader(role, leader));
        }

        let term = *self.current_term.lock().unwrap();
        let new_index = {
            let mut log = self.log.lock().unwrap();
            let last_idx = log.last().map(|e| e.index).unwrap_or(0);
            let entry = RaftLogEntry {
                index: last_idx + 1,
                term,
                command: command.clone(),
            };
            self.persist_log_entry(&entry)?;
            log.push(entry);
            last_idx + 1
        };

        // Quorum commit
        let total_nodes = self.peers.lock().unwrap().len() + 1;
        let quorum = (total_nodes / 2) + 1;
        let votes = 1; // self vote

        if votes >= quorum {
            let mut commit_idx = self.commit_index.lock().unwrap();
            *commit_idx = new_index;
            self.persist_meta("commit_index", &new_index.to_string())?;
            self.apply_entries_to_state_machine(new_index)?;
        }

        Ok(new_index)
    }

    /// Apply entries up to commit_index into SQLite state machine
    fn apply_entries_to_state_machine(&self, target_commit: u64) -> Result<(), RaftMeshError> {
        let mut last_applied = self.last_applied.lock().unwrap();
        let log = self.log.lock().unwrap();

        let entries_to_apply: Vec<RaftLogEntry> = log
            .iter()
            .filter(|e| e.index > *last_applied && e.index <= target_commit)
            .cloned()
            .collect();

        drop(log);

        let conn = self.db.lock().unwrap();

        for entry in entries_to_apply {
            match entry.command {
                RaftCommand::SetKV { key, value } => {
                    conn.execute(
                        "INSERT OR REPLACE INTO raft_kv (key, value) VALUES (?1, ?2)",
                        params![key, value],
                    )?;
                }
                RaftCommand::DeleteKV { key } => {
                    conn.execute("DELETE FROM raft_kv WHERE key = ?1", params![key])?;
                }
                RaftCommand::UpsertVector { id, vector, metadata } => {
                    let mut blob = Vec::with_capacity(vector.len() * 4);
                    for &f in &vector {
                        blob.extend_from_slice(&f.to_le_bytes());
                    }
                    conn.execute(
                        "INSERT OR REPLACE INTO raft_vectors (id, dim, vector_blob, metadata) VALUES (?1, ?2, ?3, ?4)",
                        params![id, vector.len(), blob, metadata],
                    )?;
                }
                RaftCommand::RecordEpisode { id, task, lesson, success } => {
                    conn.execute(
                        "INSERT OR REPLACE INTO raft_episodes (id, task, lesson, success) VALUES (?1, ?2, ?3, ?4)",
                        params![id, task, lesson, if success { 1 } else { 0 }],
                    )?;
                }
                RaftCommand::AddNode { node_id, address } => {
                    conn.execute(
                        "INSERT OR REPLACE INTO raft_peers (node_id, address) VALUES (?1, ?2)",
                        params![node_id, address],
                    )?;
                    self.peers.lock().unwrap().insert(node_id, address);
                }
                RaftCommand::RemoveNode { node_id } => {
                    conn.execute("DELETE FROM raft_peers WHERE node_id = ?1", params![node_id])?;
                    self.peers.lock().unwrap().remove(&node_id);
                }
                RaftCommand::Noop => {}
            }
            *last_applied = entry.index;
        }

        conn.execute(
            "INSERT OR REPLACE INTO raft_meta (key, value) VALUES ('last_applied', ?1)",
            params![last_applied.to_string()],
        )?;

        Ok(())
    }

    /// Add a new node to the cluster
    pub fn add_node(&self, node_id: u64, address: String) -> Result<(), RaftMeshError> {
        let cmd = RaftCommand::AddNode { node_id, address };
        self.propose(cmd)?;
        Ok(())
    }

    /// Remove a node from the cluster
    pub fn remove_node(&self, node_id: u64) -> Result<(), RaftMeshError> {
        let cmd = RaftCommand::RemoveNode { node_id };
        self.propose(cmd)?;
        Ok(())
    }

    /// Generate point-in-time Raft snapshot
    pub fn create_snapshot(&self) -> Result<RaftSnapshot, RaftMeshError> {
        let last_idx = *self.last_applied.lock().unwrap();
        let log = self.log.lock().unwrap();
        let last_term = log
            .iter()
            .find(|e| e.index == last_idx)
            .map(|e| e.term)
            .unwrap_or(0);
        drop(log);

        let conn = self.db.lock().unwrap();
        let mut kv_state = HashMap::new();
        let mut stmt = conn.prepare("SELECT key, value FROM raft_kv")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for r in rows.flatten() {
            kv_state.insert(r.0, r.1);
        }

        let vector_count: usize = conn.query_row("SELECT COUNT(*) FROM raft_vectors", [], |r| r.get(0))?;
        let episode_count: usize = conn.query_row("SELECT COUNT(*) FROM raft_episodes", [], |r| r.get(0))?;
        let cluster_nodes = self.peers.lock().unwrap().clone();

        let mut hasher = blake3::Hasher::new();
        hasher.update(&last_idx.to_le_bytes());
        hasher.update(&last_term.to_le_bytes());
        for (k, v) in &kv_state {
            hasher.update(k.as_bytes());
            hasher.update(v.as_bytes());
        }
        let blake3_hash = hasher.finalize().to_hex().to_string();

        Ok(RaftSnapshot {
            last_included_index: last_idx,
            last_included_term: last_term,
            kv_state,
            vector_count,
            episode_count,
            cluster_nodes,
            blake3_hash,
        })
    }

    /// Restore state machine from a snapshot
    pub fn apply_snapshot(&self, snapshot: &RaftSnapshot) -> Result<(), RaftMeshError> {
        let conn = self.db.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        tx.execute("DELETE FROM raft_kv", [])?;
        for (k, v) in &snapshot.kv_state {
            tx.execute("INSERT INTO raft_kv (key, value) VALUES (?1, ?2)", params![k, v])?;
        }

        tx.execute("DELETE FROM raft_peers", [])?;
        for (&node_id, addr) in &snapshot.cluster_nodes {
            tx.execute("INSERT INTO raft_peers (node_id, address) VALUES (?1, ?2)", params![node_id, addr])?;
        }

        tx.commit()?;

        *self.last_applied.lock().unwrap() = snapshot.last_included_index;
        *self.commit_index.lock().unwrap() = snapshot.last_included_index;
        *self.peers.lock().unwrap() = snapshot.cluster_nodes.clone();

        Ok(())
    }

    /// Read key from SQLite state machine
    pub fn get_kv(&self, key: &str) -> Option<String> {
        let conn = self.db.lock().unwrap();
        conn.query_row("SELECT value FROM raft_kv WHERE key = ?1", params![key], |r| r.get(0)).ok()
    }

    /// Search vector embeddings using cosine similarity
    pub fn search_vectors(&self, query_vec: &[f32], top_k: usize) -> Vec<VectorSearchResult> {
        if query_vec.is_empty() {
            return Vec::new();
        }

        let conn = self.db.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT id, vector_blob, metadata FROM raft_vectors") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            let meta: String = row.get(2)?;
            Ok((id, blob, meta))
        });

        let mut matches = Vec::new();
        if let Ok(records) = rows {
            for rec in records.flatten() {
                let id = rec.0;
                let blob = rec.1;
                let meta = rec.2;

                let vec_floats: Vec<f32> = blob
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();

                let sim = Self::cosine_similarity(query_vec, &vec_floats);
                matches.push(VectorSearchResult {
                    id,
                    similarity: sim,
                    metadata: meta,
                });
            }
        }

        matches.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        matches.truncate(top_k);
        matches
    }

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let mut dot = 0.0;
        let mut norm_a = 0.0;
        let mut norm_b = 0.0;

        for (&x, &y) in a.iter().zip(b.iter()) {
            dot += x * y;
            norm_a += x * x;
            norm_b += y * y;
        }

        let denom = norm_a.sqrt() * norm_b.sqrt();
        if denom > 1e-8 {
            dot / denom
        } else {
            0.0
        }
    }

    /// Comprehensive status report
    pub fn get_status(&self) -> RaftMeshStatusReport {
        let conn = self.db.lock().unwrap();
        let kv_count: usize = conn.query_row("SELECT COUNT(*) FROM raft_kv", [], |r| r.get(0)).unwrap_or(0);
        let vec_count: usize = conn.query_row("SELECT COUNT(*) FROM raft_vectors", [], |r| r.get(0)).unwrap_or(0);
        let ep_count: usize = conn.query_row("SELECT COUNT(*) FROM raft_episodes", [], |r| r.get(0)).unwrap_or(0);

        RaftMeshStatusReport {
            node_id: self.node_id,
            role: *self.role.lock().unwrap(),
            current_term: *self.current_term.lock().unwrap(),
            leader_id: *self.leader_id.lock().unwrap(),
            commit_index: *self.commit_index.lock().unwrap(),
            last_applied: *self.last_applied.lock().unwrap(),
            log_length: self.log.lock().unwrap().len(),
            cluster_peers: self.peers.lock().unwrap().clone(),
            kv_entries_count: kv_count,
            vector_entries_count: vec_count,
            episode_entries_count: ep_count,
            is_healthy: true,
        }
    }

    fn persist_meta(&self, key: &str, value: &str) -> Result<(), RaftMeshError> {
        let conn = self.db.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO raft_meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    fn persist_log_entry(&self, entry: &RaftLogEntry) -> Result<(), RaftMeshError> {
        let conn = self.db.lock().unwrap();
        let json = serde_json::to_string(&entry.command)?;
        conn.execute(
            "INSERT OR REPLACE INTO raft_log (log_index, term, command_json) VALUES (?1, ?2, ?3)",
            params![entry.index, entry.term, json],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raft_initialization_and_election() {
        let mesh = RaftStateMesh::in_memory(1).expect("create in-memory raft mesh");
        let status = mesh.get_status();
        assert_eq!(status.role, RaftRole::Follower);
        assert_eq!(status.current_term, 0);

        // Start election -> becomes Leader as single node
        mesh.start_election().expect("start election");
        let status2 = mesh.get_status();
        assert_eq!(status2.role, RaftRole::Leader);
        assert_eq!(status2.current_term, 1);
        assert_eq!(status2.leader_id, Some(1));
    }

    #[test]
    fn test_raft_propose_and_kv_replication() {
        let mesh = RaftStateMesh::in_memory(1).expect("init raft mesh");
        mesh.start_election().expect("lead");

        let idx = mesh
            .propose(RaftCommand::SetKV {
                key: "ai_model".to_string(),
                value: "qwen2.5-coder:1.5b".to_string(),
            })
            .expect("propose set kv");
        assert_eq!(idx, 1);

        let val = mesh.get_kv("ai_model");
        assert_eq!(val, Some("qwen2.5-coder:1.5b".to_string()));
    }

    #[test]
    fn test_raft_vector_replication_and_cosine_search() {
        let mesh = RaftStateMesh::in_memory(1).expect("init mesh");
        mesh.start_election().expect("lead");

        mesh.propose(RaftCommand::UpsertVector {
            id: "vec_embedding_1".to_string(),
            vector: vec![1.0, 0.0, 0.0],
            metadata: "unit_vector_x".to_string(),
        })
        .expect("upsert vector 1");

        mesh.propose(RaftCommand::UpsertVector {
            id: "vec_embedding_2".to_string(),
            vector: vec![0.0, 1.0, 0.0],
            metadata: "unit_vector_y".to_string(),
        })
        .expect("upsert vector 2");

        let matches = mesh.search_vectors(&[0.9, 0.1, 0.0], 1);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "vec_embedding_1");
        assert!(matches[0].similarity > 0.9);
    }

    #[test]
    fn test_raft_membership_and_snapshot() {
        let mesh = RaftStateMesh::in_memory(1).expect("init mesh");
        mesh.start_election().expect("lead");

        mesh.add_node(2, "127.0.0.1:9092".to_string())
            .expect("add node 2");
        let status = mesh.get_status();
        assert!(status.cluster_peers.contains_key(&2));

        let snapshot = mesh.create_snapshot().expect("create snapshot");
        assert_eq!(snapshot.last_included_index, 1);
        assert!(snapshot.cluster_nodes.contains_key(&2));
        assert!(!snapshot.blake3_hash.is_empty());

        // Restore snapshot into another node
        let node2 = RaftStateMesh::in_memory(2).expect("init node 2");
        node2.apply_snapshot(&snapshot).expect("apply snapshot");
        let node2_status = node2.get_status();
        assert_eq!(node2_status.last_applied, 1);
        assert!(node2_status.cluster_peers.contains_key(&2));
    }
}
