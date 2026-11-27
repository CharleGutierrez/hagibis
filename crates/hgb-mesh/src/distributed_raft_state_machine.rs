//! # Distributed Consensus State Machine (Raft) for `hgb-mesh`
//!
//! Production-grade canonical Raft consensus state machine providing distributed coordination,
//! split-brain prevention via strict majority quorum `(cluster_size / 2) + 1`, and strongly
//! consistent replication over Hagibis mesh components:
//! - Replicated Memory Graph state (`HashMap<String, HashMap<String, String>>`)
//! - Distributed Vector Embedding Shard registry (`HashMap<String, VectorShardInfo>`)
//! - Distributed Circuit Breaker coordination registry (`HashMap<String, BreakerInfo>`)
//! - Dynamic cluster node registration (`HashMap<String, ClusterNodeInfo>`)
//! - State machine snapshotting and log compaction.
//! - Thread-safe async engine handle (`DistributedRaftEngine` / `Arc<RwLock<RaftNode>>`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{MeshError, Result};
pub use crate::microvm_raft_consensus::RaftRole;
use crate::secure_transport::compute_sha256_hex;

/// Calculates strict majority quorum: `(cluster_size / 2) + 1`
#[inline]
pub fn majority_quorum(cluster_size: usize) -> usize {
    (cluster_size / 2) + 1
}

/// Metadata and status of a replicated vector embedding shard
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorShardInfo {
    /// Unique identifier for the vector shard
    pub shard_id: String,
    /// Collection or namespace
    pub collection: String,
    /// Total number of indexed vector embeddings in this shard
    pub vector_count: u64,
    /// Cryptographic checksum (SHA-256) of shard data / embeddings
    pub checksum: String,
    /// Timestamp when shard was synchronized (ms since UNIX epoch)
    pub updated_at: u64,
}

/// Metadata and state of a distributed circuit breaker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BreakerInfo {
    /// Unique breaker identifier (e.g., node, service, or route)
    pub breaker_id: String,
    /// Current state (e.g. "Closed", "Open", "HalfOpen")
    pub state: String,
    /// Reason triggering this state transition
    pub reason: String,
    /// Timestamp when transition occurred (ms since UNIX epoch)
    pub last_transition_timestamp: u64,
}

/// Registered cluster node metadata
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterNodeInfo {
    /// Peer/Node identifier
    pub node_id: String,
    /// Multiaddress or network endpoint (e.g., "/ip4/127.0.0.1/tcp/9000")
    pub endpoint: String,
    /// Cluster role (e.g., "core_consensus", "edge_worker", "storage")
    pub role: String,
    /// Timestamp when node registered (ms since UNIX epoch)
    pub registered_at: u64,
}

/// Strongly typed Raft command payloads applied to the replicated state machine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RaftCommand {
    /// Key-value write into an agent's memory graph
    UpdateMemoryGraph {
        agent_id: String,
        node_id: String,
        key: String,
        value: String,
        timestamp: u64,
    },
    /// Register or synchronize a vector embedding shard across the mesh
    SyncVectorEmbeddingShard {
        shard_id: String,
        collection: String,
        vector_count: u64,
        checksum: String,
    },
    /// Cluster-wide circuit breaker state transition
    SetCircuitBreakerState {
        breaker_id: String,
        state: String,
        reason: String,
        timestamp: u64,
    },
    /// Dynamic registration or update of a cluster node
    RegisterClusterNode {
        node_id: String,
        endpoint: String,
        role: String,
    },
}

/// An immutable, checksummed Raft consensus log entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DistributedLogEntry {
    /// Monotonically increasing 1-based log index
    pub index: u64,
    /// Raft term during which this entry was appended by the leader
    pub term: u64,
    /// Strongly typed consensus command payload
    pub command: RaftCommand,
    /// Creation timestamp (ms since UNIX epoch)
    pub timestamp_ms: u64,
    /// Cryptographic SHA-256 digest of `(index, term, command)`
    pub checksum: String,
}

impl DistributedLogEntry {
    /// Creates a new log entry and computes its cryptographic SHA-256 checksum
    pub fn new(term: u64, index: u64, command: RaftCommand) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let canonical = format!("{}:{}:{:?}", index, term, command);
        let checksum = compute_sha256_hex(canonical.as_bytes());

        Self {
            index,
            term,
            command,
            timestamp_ms,
            checksum,
        }
    }

    /// Verifies the cryptographic integrity of this log entry
    pub fn verify_checksum(&self) -> bool {
        let canonical = format!("{}:{}:{:?}", self.index, self.term, self.command);
        compute_sha256_hex(canonical.as_bytes()) == self.checksum
    }
}

/// RequestVote RPC arguments sent by candidate nodes during elections
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestVoteRequest {
    /// Candidate's current term
    pub term: u64,
    /// Candidate requesting vote
    pub candidate_id: String,
    /// Index of candidate's last log entry
    pub last_log_index: u64,
    /// Term of candidate's last log entry
    pub last_log_term: u64,
}

/// RequestVote RPC response returned by voting peers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestVoteResponse {
    /// Current term of receiver, for candidate to update itself
    pub term: u64,
    /// True means candidate received vote
    pub vote_granted: bool,
    /// Diagnostic reason if vote was rejected
    pub reason: Option<String>,
}

/// AppendEntries RPC arguments sent by leaders for heartbeats and log replication
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppendEntriesRequest {
    /// Leader's current term
    pub term: u64,
    /// Leader identifier so followers can redirect clients
    pub leader_id: String,
    /// Index of log entry immediately preceding new ones
    pub prev_log_index: u64,
    /// Term of prev_log_index entry
    pub prev_log_term: u64,
    /// Log entries to store (empty for heartbeat)
    pub entries: Vec<DistributedLogEntry>,
    /// Leader's commitIndex
    pub leader_commit: u64,
}

/// AppendEntries RPC response returned by followers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppendEntriesResponse {
    /// Receiver's current term, for leader to update itself
    pub term: u64,
    /// True if follower contained entry matching prev_log_index and prev_log_term
    pub success: bool,
    /// Follower's match index (highest log entry replicated)
    pub match_index: u64,
    /// Diagnostic explanation if replication rejected
    pub reason: Option<String>,
}

/// Replicated state machine representation containing mesh memory, shards, breakers, and cluster nodes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MeshConsensusState {
    /// Distributed memory graph: agent_id -> (key -> value)
    pub memory_graph: HashMap<String, HashMap<String, String>>,
    /// Vector embedding shard registry
    pub vector_shards: HashMap<String, VectorShardInfo>,
    /// Distributed circuit breaker registry
    pub circuit_breakers: HashMap<String, BreakerInfo>,
    /// Dynamic cluster node registry
    pub cluster_nodes: HashMap<String, ClusterNodeInfo>,
    /// Index of highest applied log entry
    pub last_applied_index: u64,
    /// Term of highest applied log entry
    pub last_applied_term: u64,
}

impl MeshConsensusState {
    /// Creates a new empty replicated state
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a committed Raft command deterministically
    pub fn apply_command(&mut self, command: &RaftCommand, index: u64, term: u64) {
        match command {
            RaftCommand::UpdateMemoryGraph {
                agent_id,
                node_id: _,
                key,
                value,
                timestamp: _,
            } => {
                self.memory_graph
                    .entry(agent_id.clone())
                    .or_default()
                    .insert(key.clone(), value.clone());
            }
            RaftCommand::SyncVectorEmbeddingShard {
                shard_id,
                collection,
                vector_count,
                checksum,
            } => {
                let updated_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                self.vector_shards.insert(
                    shard_id.clone(),
                    VectorShardInfo {
                        shard_id: shard_id.clone(),
                        collection: collection.clone(),
                        vector_count: *vector_count,
                        checksum: checksum.clone(),
                        updated_at,
                    },
                );
            }
            RaftCommand::SetCircuitBreakerState {
                breaker_id,
                state,
                reason,
                timestamp,
            } => {
                self.circuit_breakers.insert(
                    breaker_id.clone(),
                    BreakerInfo {
                        breaker_id: breaker_id.clone(),
                        state: state.clone(),
                        reason: reason.clone(),
                        last_transition_timestamp: *timestamp,
                    },
                );
            }
            RaftCommand::RegisterClusterNode {
                node_id,
                endpoint,
                role,
            } => {
                let registered_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                self.cluster_nodes.insert(
                    node_id.clone(),
                    ClusterNodeInfo {
                        node_id: node_id.clone(),
                        endpoint: endpoint.clone(),
                        role: role.clone(),
                        registered_at,
                    },
                );
            }
        }

        self.last_applied_index = index;
        self.last_applied_term = term;
    }

    /// Computes a deterministic SHA-256 digest of the entire replicated state
    pub fn compute_checksum(&self) -> String {
        let serialized = serde_json::to_string(self).unwrap_or_default();
        compute_sha256_hex(serialized.as_bytes())
    }

    /// Looks up a value in the memory graph
    pub fn get_memory(&self, agent_id: &str, key: &str) -> Option<&String> {
        self.memory_graph.get(agent_id)?.get(key)
    }

    /// Gets an agent's entire key-value memory mapping
    pub fn get_agent_memory(&self, agent_id: &str) -> Option<&HashMap<String, String>> {
        self.memory_graph.get(agent_id)
    }

    /// Gets vector shard metadata
    pub fn get_vector_shard(&self, shard_id: &str) -> Option<&VectorShardInfo> {
        self.vector_shards.get(shard_id)
    }

    /// Gets circuit breaker state
    pub fn get_circuit_breaker(&self, breaker_id: &str) -> Option<&BreakerInfo> {
        self.circuit_breakers.get(breaker_id)
    }

    /// Gets registered cluster node info
    pub fn get_cluster_node(&self, node_id: &str) -> Option<&ClusterNodeInfo> {
        self.cluster_nodes.get(node_id)
    }
}

/// Compact snapshot of the replicated state machine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateSnapshot {
    /// Log index up to which this snapshot was created
    pub last_included_index: u64,
    /// Term of the last included log entry
    pub last_included_term: u64,
    /// Frozen replicated state
    pub state: MeshConsensusState,
    /// SHA-256 digest of the frozen state for tamper detection
    pub snapshot_checksum: String,
    /// Creation timestamp (ms since UNIX epoch)
    pub created_at_ms: u64,
}

/// Outcome of advancing the Raft election timer
#[derive(Debug, Clone, PartialEq)]
pub enum ElectionStepResult {
    /// Timer ticked without triggering an election
    Ticked {
        current_ticks: u64,
        timeout_ticks: u64,
    },
    /// Election timeout expired; node transitioned to Candidate and initiated an election
    ElectionStarted {
        term: u64,
        candidate_id: String,
        vote_request: RequestVoteRequest,
        peers: Vec<String>,
    },
    /// Candidate gathered immediate quorum (e.g. single-node cluster) and became Leader
    ElectionWon {
        term: u64,
        leader_id: String,
    },
    /// Node is already leader; election timer is dormant
    LeaderActive {
        term: u64,
    },
}

/// Canonical Raft Consensus Node
pub struct RaftNode {
    /// Local node identifier
    pub node_id: String,
    /// Set of known cluster peer node IDs (excluding self)
    pub peers: HashSet<String>,
    /// Current Raft role: Follower, Candidate, or Leader
    pub role: RaftRole,
    /// Latest term node has seen (initialized to 0)
    pub current_term: u64,
    /// Candidate ID that received vote in current term (or None)
    pub voted_for: Option<String>,
    /// Raft replicated log entries
    pub log: Vec<DistributedLogEntry>,
    /// Index of highest log entry known to be committed
    pub commit_index: u64,
    /// Index of highest log entry applied to state machine
    pub last_applied: u64,
    /// Known current leader ID
    pub current_leader: Option<String>,

    // Candidate election state
    /// Set of peer IDs that granted vote in current term
    pub votes_received: HashSet<String>,
    /// Current elapsed election timer ticks
    pub election_timer_ticks: u64,
    /// Election timeout threshold in ticks
    pub election_timeout_ticks: u64,

    // Leader volatile state (re-initialized after election)
    /// Next log index to send to each peer: peer_id -> next_index
    pub next_index: HashMap<String, u64>,
    /// Highest log entry replicated on each peer: peer_id -> match_index
    pub match_index: HashMap<String, u64>,

    // Replicated state machine
    /// Current replicated mesh consensus state
    pub replicated_state: MeshConsensusState,
    /// History of applied log entries
    pub applied_history: Vec<DistributedLogEntry>,

    // Compaction & Snapshots
    /// Highest log index included in the latest snapshot
    pub last_included_index: u64,
    /// Term of the highest log index included in the latest snapshot
    pub last_included_term: u64,
    /// Snapshots captured by this node
    pub snapshots: Vec<StateSnapshot>,
}

impl RaftNode {
    /// Default election timeout in ticks
    pub const DEFAULT_ELECTION_TIMEOUT_TICKS: u64 = 5;

    /// Initializes a new Raft consensus node
    pub fn new(node_id: impl Into<String>, peers: Vec<String>) -> Self {
        let node_id = node_id.into();
        let peer_set: HashSet<String> = peers.into_iter().filter(|p| p != &node_id).collect();

        Self {
            node_id,
            peers: peer_set,
            role: RaftRole::Follower,
            current_term: 0,
            voted_for: None,
            log: Vec::new(),
            commit_index: 0,
            last_applied: 0,
            current_leader: None,
            votes_received: HashSet::new(),
            election_timer_ticks: 0,
            election_timeout_ticks: Self::DEFAULT_ELECTION_TIMEOUT_TICKS,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            replicated_state: MeshConsensusState::new(),
            applied_history: Vec::new(),
            last_included_index: 0,
            last_included_term: 0,
            snapshots: Vec::new(),
        }
    }

    /// Configures custom election timeout ticks
    pub fn set_election_timeout_ticks(&mut self, ticks: u64) {
        self.election_timeout_ticks = ticks.max(1);
    }

    /// Total number of nodes in the cluster (peers + self)
    #[inline]
    pub fn cluster_size(&self) -> usize {
        self.peers.len() + 1
    }

    /// Strict majority quorum required for consensus: `(cluster_size / 2) + 1`
    #[inline]
    pub fn quorum(&self) -> usize {
        majority_quorum(self.cluster_size())
    }

    /// Index of the last log entry on this node
    #[inline]
    pub fn last_log_index(&self) -> u64 {
        if let Some(last) = self.log.last() {
            last.index
        } else {
            self.last_included_index
        }
    }

    /// Term of the last log entry on this node
    #[inline]
    pub fn last_log_term(&self) -> u64 {
        if let Some(last) = self.log.last() {
            last.term
        } else {
            self.last_included_term
        }
    }

    /// Retrieves term of the entry at specified log index
    pub fn get_entry_term(&self, index: u64) -> Option<u64> {
        if index == 0 {
            return Some(0);
        }
        if index == self.last_included_index {
            return Some(self.last_included_term);
        }
        if index < self.last_included_index {
            return None; // Compacted into snapshot
        }
        let offset = (index - self.last_included_index - 1) as usize;
        self.log.get(offset).map(|e| e.term)
    }

    /// Steps the election timer forward by 1 tick
    pub fn step_election_timer(&mut self) -> ElectionStepResult {
        if self.role == RaftRole::Leader {
            return ElectionStepResult::LeaderActive {
                term: self.current_term,
            };
        }

        self.election_timer_ticks += 1;

        if self.election_timer_ticks >= self.election_timeout_ticks {
            // Trigger election
            self.role = RaftRole::Candidate;
            self.current_term += 1;
            self.voted_for = Some(self.node_id.clone());
            self.votes_received.clear();
            self.votes_received.insert(self.node_id.clone());
            self.election_timer_ticks = 0;

            // Check if single node cluster achieves immediate majority
            if self.votes_received.len() >= self.quorum() {
                self.become_leader();
                return ElectionStepResult::ElectionWon {
                    term: self.current_term,
                    leader_id: self.node_id.clone(),
                };
            }

            let vote_request = RequestVoteRequest {
                term: self.current_term,
                candidate_id: self.node_id.clone(),
                last_log_index: self.last_log_index(),
                last_log_term: self.last_log_term(),
            };

            let peers: Vec<String> = self.peers.iter().cloned().collect();

            ElectionStepResult::ElectionStarted {
                term: self.current_term,
                candidate_id: self.node_id.clone(),
                vote_request,
                peers,
            }
        } else {
            ElectionStepResult::Ticked {
                current_ticks: self.election_timer_ticks,
                timeout_ticks: self.election_timeout_ticks,
            }
        }
    }

    /// Handles an incoming RequestVote RPC from a candidate
    pub fn handle_request_vote(&mut self, req: &RequestVoteRequest) -> RequestVoteResponse {
        // 1. Reply false if term < currentTerm
        if req.term < self.current_term {
            return RequestVoteResponse {
                term: self.current_term,
                vote_granted: false,
                reason: Some(format!(
                    "Candidate term {} is lower than current term {}",
                    req.term, self.current_term
                )),
            };
        }

        // 2. If term > currentTerm, update currentTerm and step down to follower
        if req.term > self.current_term {
            self.current_term = req.term;
            self.role = RaftRole::Follower;
            self.voted_for = None;
            self.votes_received.clear();
        }

        // 3. Check vote eligibility:
        // voted_for is null or candidate_id, AND candidate's log is at least as up-to-date as receiver's log
        let can_vote_for_candidate = match &self.voted_for {
            None => true,
            Some(voted_candidate) => voted_candidate == &req.candidate_id,
        };

        let candidate_log_up_to_date = if req.last_log_term != self.last_log_term() {
            req.last_log_term > self.last_log_term()
        } else {
            req.last_log_index >= self.last_log_index()
        };

        if can_vote_for_candidate && candidate_log_up_to_date {
            self.voted_for = Some(req.candidate_id.clone());
            self.election_timer_ticks = 0; // Reset election timer on granting vote
            RequestVoteResponse {
                term: self.current_term,
                vote_granted: true,
                reason: None,
            }
        } else {
            let reason = if !can_vote_for_candidate {
                format!(
                    "Already voted for {:?} in term {}",
                    self.voted_for, self.current_term
                )
            } else {
                format!(
                    "Candidate log ({}, term {}) is less up-to-date than local log ({}, term {})",
                    req.last_log_index,
                    req.last_log_term,
                    self.last_log_index(),
                    self.last_log_term()
                )
            };

            RequestVoteResponse {
                term: self.current_term,
                vote_granted: false,
                reason: Some(reason),
            }
        }
    }

    /// Processes a RequestVoteResponse received from a peer.
    /// Returns `true` if this vote granted majority quorum and elevated node to Leader.
    pub fn handle_vote_response(&mut self, peer_id: &str, resp: &RequestVoteResponse) -> bool {
        // If peer responded with a higher term, step down to follower
        if resp.term > self.current_term {
            self.current_term = resp.term;
            self.role = RaftRole::Follower;
            self.voted_for = None;
            self.votes_received.clear();
            return false;
        }

        // Record vote if granted in current term
        if (self.role == RaftRole::Candidate || self.role == RaftRole::Leader)
            && resp.term == self.current_term
            && resp.vote_granted
        {
            self.votes_received.insert(peer_id.to_string());

            // Strict majority quorum evaluation transitions Candidate to Leader
            if self.role == RaftRole::Candidate && self.votes_received.len() >= self.quorum() {
                self.become_leader();
                return true;
            }
        }

        false
    }

    /// Internal transition to Leader state
    fn become_leader(&mut self) {
        self.role = RaftRole::Leader;
        self.current_leader = Some(self.node_id.clone());
        self.next_index.clear();
        self.match_index.clear();

        let next_idx = self.last_log_index() + 1;
        for peer in &self.peers {
            self.next_index.insert(peer.clone(), next_idx);
            self.match_index.insert(peer.clone(), 0);
        }
        self.match_index.insert(self.node_id.clone(), self.last_log_index());
    }

    /// Proposes a new consensus command (Leader only).
    /// Returns the assigned monotonically increasing log index.
    pub fn propose(&mut self, command: RaftCommand) -> Result<u64> {
        if self.role != RaftRole::Leader {
            return Err(MeshError::NotLeader);
        }

        let new_index = self.last_log_index() + 1;
        let entry = DistributedLogEntry::new(self.current_term, new_index, command);

        self.log.push(entry);
        self.match_index.insert(self.node_id.clone(), new_index);

        // Single node cluster commits immediately
        if self.cluster_size() == 1 {
            self.commit_index = new_index;
            self.apply_committed_entries();
        }

        Ok(new_index)
    }

    /// Constructs an AppendEntriesRequest for the specified peer
    pub fn build_append_entries_for_peer(&self, peer_id: &str) -> Option<AppendEntriesRequest> {
        if self.role != RaftRole::Leader {
            return None;
        }

        let next_idx = *self
            .next_index
            .get(peer_id)
            .unwrap_or(&(self.last_log_index() + 1));
        let prev_log_index = next_idx.saturating_sub(1);
        let prev_log_term = self.get_entry_term(prev_log_index).unwrap_or(0);

        // Collect entries starting from next_idx
        let entries: Vec<DistributedLogEntry> = if next_idx <= self.last_included_index {
            // Peer requires snapshot installation
            Vec::new()
        } else {
            let start_offset = (next_idx - self.last_included_index - 1) as usize;
            self.log.iter().skip(start_offset).cloned().collect()
        };

        Some(AppendEntriesRequest {
            term: self.current_term,
            leader_id: self.node_id.clone(),
            prev_log_index,
            prev_log_term,
            entries,
            leader_commit: self.commit_index,
        })
    }

    /// Handles an incoming AppendEntries RPC from the leader
    pub fn handle_append_entries(&mut self, req: &AppendEntriesRequest) -> AppendEntriesResponse {
        // 1. Reply false if term < currentTerm
        if req.term < self.current_term {
            return AppendEntriesResponse {
                term: self.current_term,
                success: false,
                match_index: self.last_log_index(),
                reason: Some(format!(
                    "Leader term {} < local term {}",
                    req.term, self.current_term
                )),
            };
        }

        // 2. If term > currentTerm, update currentTerm and become follower
        if req.term > self.current_term {
            self.current_term = req.term;
            self.role = RaftRole::Follower;
            self.voted_for = None;
        }

        // If candidate receives AppendEntries from valid leader of current term, step down
        if self.role == RaftRole::Candidate && req.term >= self.current_term {
            self.role = RaftRole::Follower;
        }

        self.current_leader = Some(req.leader_id.clone());
        self.election_timer_ticks = 0; // Reset election timer

        // 3. Log consistency check at prev_log_index
        if req.prev_log_index > self.last_log_index() {
            return AppendEntriesResponse {
                term: self.current_term,
                success: false,
                match_index: self.last_log_index(),
                reason: Some(format!(
                    "Missing entries: prev_log_index {} > last_log_index {}",
                    req.prev_log_index,
                    self.last_log_index()
                )),
            };
        }

        if req.prev_log_index > 0 {
            match self.get_entry_term(req.prev_log_index) {
                Some(local_term) if local_term == req.prev_log_term => {
                    // Terms match at prev_log_index, consistency check passed
                }
                Some(local_term) => {
                    return AppendEntriesResponse {
                        term: self.current_term,
                        success: false,
                        match_index: self.last_log_index(),
                        reason: Some(format!(
                            "Term mismatch at prev_log_index {}: expected {}, found local {}",
                            req.prev_log_index, req.prev_log_term, local_term
                        )),
                    };
                }
                None => {
                    if req.prev_log_index < self.last_included_index {
                        return AppendEntriesResponse {
                            term: self.current_term,
                            success: false,
                            match_index: self.last_included_index,
                            reason: Some("Entry compacted in snapshot".to_string()),
                        };
                    }
                }
            }
        }

        // 4. Append new entries and truncate conflicting ones
        for entry in &req.entries {
            // Verify checksum of replicated entry
            if !entry.verify_checksum() {
                return AppendEntriesResponse {
                    term: self.current_term,
                    success: false,
                    match_index: self.last_log_index(),
                    reason: Some(format!(
                        "Entry checksum validation failed at index {}",
                        entry.index
                    )),
                };
            }

            if entry.index <= self.last_included_index {
                continue;
            }

            let offset = (entry.index - self.last_included_index - 1) as usize;
            if offset < self.log.len() {
                if self.log[offset].term != entry.term {
                    // Existing entry conflicts with new one -> truncate log from here onwards
                    self.log.truncate(offset);
                    self.log.push(entry.clone());
                }
            } else {
                self.log.push(entry.clone());
            }
        }

        // 5. Update commit index
        if req.leader_commit > self.commit_index {
            self.commit_index = std::cmp::min(req.leader_commit, self.last_log_index());
            self.apply_committed_entries();
        }

        AppendEntriesResponse {
            term: self.current_term,
            success: true,
            match_index: self.last_log_index(),
            reason: None,
        }
    }

    /// Handles an AppendEntriesResponse from a follower (Leader only).
    /// Returns list of newly committed and applied log indices, if any.
    pub fn handle_append_entries_response(
        &mut self,
        peer_id: &str,
        resp: &AppendEntriesResponse,
    ) -> Result<Vec<u64>> {
        // If peer responded with higher term, step down to follower
        if resp.term > self.current_term {
            self.current_term = resp.term;
            self.role = RaftRole::Follower;
            self.voted_for = None;
            self.votes_received.clear();
            return Ok(Vec::new());
        }

        if self.role != RaftRole::Leader {
            return Ok(Vec::new());
        }

        if resp.success {
            self.match_index.insert(peer_id.to_string(), resp.match_index);
            self.next_index.insert(peer_id.to_string(), resp.match_index + 1);

            // Check if log entries can be committed by strict majority quorum
            let mut newly_committed_index = self.commit_index;
            let quorum = self.quorum();

            for n in (self.commit_index + 1)..=self.last_log_index() {
                // Leaders only commit entries from current term directly by counting replicas
                if let Some(term) = self.get_entry_term(n) {
                    if term == self.current_term {
                        // Count replicas that have replicated entry N
                        let match_count = self
                            .match_index
                            .values()
                            .filter(|&&idx| idx >= n)
                            .count();

                        if match_count >= quorum {
                            newly_committed_index = n;
                        }
                    }
                }
            }

            if newly_committed_index > self.commit_index {
                self.commit_index = newly_committed_index;
                let applied = self.apply_committed_entries();
                return Ok(applied);
            }
        } else {
            // Replication rejected (e.g. log mismatch). Decrement next_index and retry.
            let next_idx = self.next_index.entry(peer_id.to_string()).or_insert(1);
            if *next_idx > 1 {
                *next_idx = resp.match_index.max(1);
            }
        }

        Ok(Vec::new())
    }

    /// Applies all committed, unapplied log entries to the replicated state machine
    pub fn apply_committed_entries(&mut self) -> Vec<u64> {
        let mut applied_indices = Vec::new();

        while self.last_applied < self.commit_index {
            let next_idx = self.last_applied + 1;
            if next_idx <= self.last_included_index {
                self.last_applied = next_idx;
                continue;
            }

            let offset = (next_idx - self.last_included_index - 1) as usize;
            if let Some(entry) = self.log.get(offset).cloned() {
                // Apply command to state machine
                self.replicated_state
                    .apply_command(&entry.command, entry.index, entry.term);

                // If dynamic node registration, add to cluster peers if needed
                if let RaftCommand::RegisterClusterNode { ref node_id, .. } = entry.command {
                    if node_id != &self.node_id {
                        self.peers.insert(node_id.clone());
                    }
                }

                self.applied_history.push(entry);
                self.last_applied = next_idx;
                applied_indices.push(next_idx);
            } else {
                break;
            }
        }

        applied_indices
    }

    /// Captures a state snapshot up to `last_applied`
    pub fn create_snapshot(&mut self) -> Result<StateSnapshot> {
        let snapshot_checksum = self.replicated_state.compute_checksum();
        let created_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let snapshot = StateSnapshot {
            last_included_index: self.replicated_state.last_applied_index,
            last_included_term: self.replicated_state.last_applied_term,
            state: self.replicated_state.clone(),
            snapshot_checksum,
            created_at_ms,
        };

        self.snapshots.push(snapshot.clone());
        Ok(snapshot)
    }

    /// Prunes log entries up to `up_to_index` that have already been applied and snapshotted
    pub fn compact_log(&mut self, up_to_index: u64) -> Result<usize> {
        let safe_compact_limit = std::cmp::min(up_to_index, self.last_applied);
        if safe_compact_limit <= self.last_included_index {
            return Ok(0);
        }

        let compact_term = self
            .get_entry_term(safe_compact_limit)
            .unwrap_or(self.last_included_term);

        let prune_count = (safe_compact_limit - self.last_included_index) as usize;
        let actual_prune = std::cmp::min(prune_count, self.log.len());

        self.log.drain(0..actual_prune);
        self.last_included_index = safe_compact_limit;
        self.last_included_term = compact_term;

        Ok(actual_prune)
    }

    /// Installs a received snapshot onto this node
    pub fn install_snapshot(&mut self, snapshot: StateSnapshot) -> Result<()> {
        // Verify snapshot integrity
        let calculated_checksum = snapshot.state.compute_checksum();
        if calculated_checksum != snapshot.snapshot_checksum {
            return Err(MeshError::CheckpointHashMismatch {
                expected: snapshot.snapshot_checksum,
                actual: calculated_checksum,
            });
        }

        if snapshot.last_included_index <= self.last_applied {
            return Ok(()); // Already applied
        }

        // Apply state snapshot
        self.replicated_state = snapshot.state;
        self.last_applied = snapshot.last_included_index;
        self.commit_index = std::cmp::max(self.commit_index, snapshot.last_included_index);

        // Prune log up to snapshot index
        if snapshot.last_included_index >= self.last_log_index() {
            self.log.clear();
        } else if snapshot.last_included_index > self.last_included_index {
            let prune_offset = (snapshot.last_included_index - self.last_included_index) as usize;
            if prune_offset < self.log.len() {
                self.log.drain(0..prune_offset);
            } else {
                self.log.clear();
            }
        }

        self.last_included_index = snapshot.last_included_index;
        self.last_included_term = snapshot.last_included_term;

        Ok(())
    }

    /// Returns a reference to the replicated state
    #[inline]
    pub fn get_replicated_state(&self) -> &MeshConsensusState {
        &self.replicated_state
    }
}

/// Thread-safe async engine handle wrapping `Arc<RwLock<RaftNode>>`
#[derive(Clone)]
pub struct DistributedRaftEngine {
    node: Arc<RwLock<RaftNode>>,
}

impl DistributedRaftEngine {
    /// Creates a new distributed Raft engine handle
    pub fn new(node_id: impl Into<String>, peers: Vec<String>) -> Self {
        Self {
            node: Arc::new(RwLock::new(RaftNode::new(node_id, peers))),
        }
    }

    /// Wraps an existing `RaftNode`
    pub fn from_node(node: RaftNode) -> Self {
        Self {
            node: Arc::new(RwLock::new(node)),
        }
    }

    /// Wraps a shared `Arc<RwLock<RaftNode>>`
    pub fn from_shared(shared: Arc<RwLock<RaftNode>>) -> Self {
        Self { node: shared }
    }

    /// Returns a reference to the internal `Arc<RwLock<RaftNode>>`
    pub fn inner(&self) -> &Arc<RwLock<RaftNode>> {
        &self.node
    }

    /// Clones the internal `Arc<RwLock<RaftNode>>`
    pub fn clone_inner(&self) -> Arc<RwLock<RaftNode>> {
        Arc::clone(&self.node)
    }

    /// Proposes a new consensus command (Leader only)
    pub async fn propose(&self, command: RaftCommand) -> Result<u64> {
        let mut node = self.node.write().await;
        node.propose(command)
    }

    /// Advances the election timer
    pub async fn step_election_timer(&self) -> ElectionStepResult {
        let mut node = self.node.write().await;
        node.step_election_timer()
    }

    /// Handles an incoming RequestVote RPC
    pub async fn handle_request_vote(&self, req: RequestVoteRequest) -> RequestVoteResponse {
        let mut node = self.node.write().await;
        node.handle_request_vote(&req)
    }

    /// Handles an incoming AppendEntries RPC
    pub async fn handle_append_entries(&self, req: AppendEntriesRequest) -> AppendEntriesResponse {
        let mut node = self.node.write().await;
        node.handle_append_entries(&req)
    }

    /// Processes a RequestVoteResponse
    pub async fn handle_vote_response(&self, peer_id: &str, resp: RequestVoteResponse) -> bool {
        let mut node = self.node.write().await;
        node.handle_vote_response(peer_id, &resp)
    }

    /// Processes an AppendEntriesResponse
    pub async fn handle_append_entries_response(
        &self,
        peer_id: &str,
        resp: AppendEntriesResponse,
    ) -> Result<Vec<u64>> {
        let mut node = self.node.write().await;
        node.handle_append_entries_response(peer_id, &resp)
    }

    /// Builds an AppendEntriesRequest for a peer
    pub async fn build_append_entries_for_peer(&self, peer_id: &str) -> Option<AppendEntriesRequest> {
        let node = self.node.read().await;
        node.build_append_entries_for_peer(peer_id)
    }

    /// Returns a copy of the replicated consensus state
    pub async fn get_replicated_state(&self) -> MeshConsensusState {
        let node = self.node.read().await;
        node.get_replicated_state().clone()
    }

    /// Returns current role
    pub async fn role(&self) -> RaftRole {
        let node = self.node.read().await;
        node.role
    }

    /// Returns current term
    pub async fn current_term(&self) -> u64 {
        let node = self.node.read().await;
        node.current_term
    }

    /// Returns commit index
    pub async fn commit_index(&self) -> u64 {
        let node = self.node.read().await;
        node.commit_index
    }

    /// Returns last log index
    pub async fn last_log_index(&self) -> u64 {
        let node = self.node.read().await;
        node.last_log_index()
    }

    /// Creates a state snapshot
    pub async fn create_snapshot(&self) -> Result<StateSnapshot> {
        let mut node = self.node.write().await;
        node.create_snapshot()
    }

    /// Prunes log up to `up_to_index`
    pub async fn compact_log(&self, up_to_index: u64) -> Result<usize> {
        let mut node = self.node.write().await;
        node.compact_log(up_to_index)
    }

    /// Installs a snapshot
    pub async fn install_snapshot(&self, snapshot: StateSnapshot) -> Result<()> {
        let mut node = self.node.write().await;
        node.install_snapshot(snapshot)
    }
}

impl std::ops::Deref for DistributedRaftEngine {
    type Target = Arc<RwLock<RaftNode>>;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strict_majority_quorum_calculation() {
        assert_eq!(majority_quorum(1), 1);
        assert_eq!(majority_quorum(2), 2);
        assert_eq!(majority_quorum(3), 2);
        assert_eq!(majority_quorum(4), 3);
        assert_eq!(majority_quorum(5), 3);
        assert_eq!(majority_quorum(7), 4);
        assert_eq!(majority_quorum(9), 5);
    }

    #[test]
    fn test_cluster_initialization_and_candidate_election() {
        let node_a_id = "node-alpha".to_string();
        let node_b_id = "node-beta".to_string();
        let node_c_id = "node-gamma".to_string();

        let peers_a = vec![node_b_id.clone(), node_c_id.clone()];
        let peers_b = vec![node_a_id.clone(), node_c_id.clone()];
        let peers_c = vec![node_a_id.clone(), node_b_id.clone()];

        let mut node_a = RaftNode::new(&node_a_id, peers_a);
        let mut node_b = RaftNode::new(&node_b_id, peers_b);
        let mut node_c = RaftNode::new(&node_c_id, peers_c);

        // Verify initial state
        assert_eq!(node_a.role, RaftRole::Follower);
        assert_eq!(node_b.role, RaftRole::Follower);
        assert_eq!(node_c.role, RaftRole::Follower);
        assert_eq!(node_a.current_term, 0);
        assert_eq!(node_a.commit_index, 0);
        assert_eq!(node_a.cluster_size(), 3);
        assert_eq!(node_a.quorum(), 2);

        // Advance Node A's election timer until election timeout
        let mut started = false;
        let mut vote_req = None;
        for _ in 0..node_a.election_timeout_ticks {
            match node_a.step_election_timer() {
                ElectionStepResult::Ticked { .. } => {}
                ElectionStepResult::ElectionStarted {
                    term,
                    candidate_id,
                    vote_request,
                    peers,
                } => {
                    started = true;
                    assert_eq!(term, 1);
                    assert_eq!(candidate_id, node_a_id);
                    assert_eq!(peers.len(), 2);
                    vote_req = Some(vote_request);
                }
                other => panic!("Unexpected election step result: {:?}", other),
            }
        }

        assert!(started, "Node A must start an election upon timeout");
        assert_eq!(node_a.role, RaftRole::Candidate);
        assert_eq!(node_a.current_term, 1);

        // Node B receives RequestVote from Node A
        let req = vote_req.expect("Vote request must be present");
        let vote_resp_b = node_b.handle_request_vote(&req);
        assert!(vote_resp_b.vote_granted, "Node B must grant vote to Node A");
        assert_eq!(vote_resp_b.term, 1);
        assert_eq!(node_b.voted_for, Some(node_a_id.clone()));

        // Node A processes vote response from Node B -> Quorum achieved (2/3)!
        let became_leader = node_a.handle_vote_response(&node_b_id, &vote_resp_b);
        assert!(became_leader, "Node A must achieve majority quorum and become leader");
        assert_eq!(node_a.role, RaftRole::Leader);
        assert_eq!(node_a.current_leader, Some(node_a_id.clone()));

        // Node C also grants vote
        let vote_resp_c = node_c.handle_request_vote(&req);
        assert!(vote_resp_c.vote_granted);
        let _ = node_a.handle_vote_response(&node_c_id, &vote_resp_c);
        assert_eq!(node_a.votes_received.len(), 3);
    }

    #[test]
    fn test_log_replication_across_followers_with_majority_quorum() {
        let node_a_id = "node-alpha".to_string();
        let node_b_id = "node-beta".to_string();
        let node_c_id = "node-gamma".to_string();

        let mut node_a = RaftNode::new(&node_a_id, vec![node_b_id.clone(), node_c_id.clone()]);
        let mut node_b = RaftNode::new(&node_b_id, vec![node_a_id.clone(), node_c_id.clone()]);
        let mut node_c = RaftNode::new(&node_c_id, vec![node_a_id.clone(), node_b_id.clone()]);

        // Manually elect Node A as leader in term 1
        node_a.current_term = 1;
        node_a.role = RaftRole::Leader;
        node_a.become_leader();

        node_b.current_term = 1;
        node_c.current_term = 1;

        // Propose command on Leader
        let cmd = RaftCommand::UpdateMemoryGraph {
            agent_id: "agent-nexus".to_string(),
            node_id: node_a_id.clone(),
            key: "context_window_size".to_string(),
            value: "131072".to_string(),
            timestamp: 1710000000,
        };

        let proposed_idx = node_a.propose(cmd).expect("Leader propose");
        assert_eq!(proposed_idx, 1);
        assert_eq!(node_a.commit_index, 0, "Cannot commit before follower quorum replication");

        // Leader replicates entry to Node B
        let append_req_b = node_a
            .build_append_entries_for_peer(&node_b_id)
            .expect("Append entries for B");
        assert_eq!(append_req_b.entries.len(), 1);
        assert_eq!(append_req_b.prev_log_index, 0);

        let append_resp_b = node_b.handle_append_entries(&append_req_b);
        assert!(append_resp_b.success);
        assert_eq!(append_resp_b.match_index, 1);

        // Leader processes response from Node B -> Strict Majority Quorum achieved (2/3)!
        let newly_committed = node_a
            .handle_append_entries_response(&node_b_id, &append_resp_b)
            .expect("Commit evaluation");
        assert_eq!(newly_committed, vec![1]);
        assert_eq!(node_a.commit_index, 1);
        assert_eq!(node_a.last_applied, 1);

        // Verify leader state machine was updated
        assert_eq!(
            node_a.replicated_state.get_memory("agent-nexus", "context_window_size"),
            Some(&"131072".to_string())
        );

        // Subsequent heartbeat to Node B informs it of leader_commit = 1
        let heartbeat_b = node_a
            .build_append_entries_for_peer(&node_b_id)
            .expect("Heartbeat for B");
        assert_eq!(heartbeat_b.leader_commit, 1);
        assert_eq!(heartbeat_b.entries.len(), 0);

        let hb_resp_b = node_b.handle_append_entries(&heartbeat_b);
        assert!(hb_resp_b.success);
        assert_eq!(node_b.commit_index, 1);
        assert_eq!(node_b.last_applied, 1);
        assert_eq!(
            node_b.replicated_state.get_memory("agent-nexus", "context_window_size"),
            Some(&"131072".to_string())
        );

        // Replicate to Node C as well
        let append_req_c = node_a
            .build_append_entries_for_peer(&node_c_id)
            .expect("Append entries for C");
        let append_resp_c = node_c.handle_append_entries(&append_req_c);
        assert!(append_resp_c.success);
        let _ = node_a.handle_append_entries_response(&node_c_id, &append_resp_c);
        assert_eq!(node_c.commit_index, 1);
        assert_eq!(
            node_c.replicated_state.get_memory("agent-nexus", "context_window_size"),
            Some(&"131072".to_string())
        );
    }

    #[test]
    fn test_committing_and_applying_typed_state_machine_commands() {
        let mut leader = RaftNode::new("leader-node", vec!["follower-1".to_string()]);
        let mut follower = RaftNode::new("follower-1", vec!["leader-node".to_string()]);

        leader.current_term = 1;
        leader.role = RaftRole::Leader;
        leader.become_leader();
        follower.current_term = 1;

        // 1. UpdateMemoryGraph
        let cmd1 = RaftCommand::UpdateMemoryGraph {
            agent_id: "agent-007".to_string(),
            node_id: "leader-node".to_string(),
            key: "mission_status".to_string(),
            value: "operational".to_string(),
            timestamp: 1710001000,
        };
        let idx1 = leader.propose(cmd1).unwrap();

        // 2. SyncVectorEmbeddingShard
        let cmd2 = RaftCommand::SyncVectorEmbeddingShard {
            shard_id: "shard-vec-embeddings-99".to_string(),
            collection: "rag_knowledge_graph".to_string(),
            vector_count: 524288,
            checksum: "a3f5b9c1d2e4f8a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4".to_string(),
        };
        let idx2 = leader.propose(cmd2).unwrap();

        // 3. SetCircuitBreakerState
        let cmd3 = RaftCommand::SetCircuitBreakerState {
            breaker_id: "breaker-gateway-edge".to_string(),
            state: "Open".to_string(),
            reason: "P99 latency exceeded 500ms over 10s window".to_string(),
            timestamp: 1710002000,
        };
        let idx3 = leader.propose(cmd3).unwrap();

        // 4. RegisterClusterNode
        let cmd4 = RaftCommand::RegisterClusterNode {
            node_id: "worker-node-delta".to_string(),
            endpoint: "/ip4/192.168.1.50/tcp/9200".to_string(),
            role: "edge_compute_worker".to_string(),
        };
        let idx4 = leader.propose(cmd4).unwrap();

        assert_eq!(idx1, 1);
        assert_eq!(idx2, 2);
        assert_eq!(idx3, 3);
        assert_eq!(idx4, 4);

        // Replicate all entries to follower
        let req = leader
            .build_append_entries_for_peer("follower-1")
            .expect("Build entries");
        assert_eq!(req.entries.len(), 4);

        let resp = follower.handle_append_entries(&req);
        assert!(resp.success);
        assert_eq!(resp.match_index, 4);

        // Leader commits upon majority quorum (2/2)
        let committed = leader
            .handle_append_entries_response("follower-1", &resp)
            .unwrap();
        assert_eq!(committed, vec![1, 2, 3, 4]);
        assert_eq!(leader.commit_index, 4);

        // Follower applies via heartbeat
        let hb = leader
            .build_append_entries_for_peer("follower-1")
            .expect("Heartbeat");
        follower.handle_append_entries(&hb);
        assert_eq!(follower.commit_index, 4);

        // Verify applied state across both Leader and Follower
        for node in &[&leader, &follower] {
            let state = node.get_replicated_state();

            // Check UpdateMemoryGraph
            assert_eq!(
                state.get_memory("agent-007", "mission_status"),
                Some(&"operational".to_string())
            );

            // Check SyncVectorEmbeddingShard
            let shard = state.get_vector_shard("shard-vec-embeddings-99").unwrap();
            assert_eq!(shard.collection, "rag_knowledge_graph");
            assert_eq!(shard.vector_count, 524288);
            assert_eq!(
                shard.checksum,
                "a3f5b9c1d2e4f8a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4"
            );

            // Check SetCircuitBreakerState
            let breaker = state.get_circuit_breaker("breaker-gateway-edge").unwrap();
            assert_eq!(breaker.state, "Open");
            assert_eq!(
                breaker.reason,
                "P99 latency exceeded 500ms over 10s window"
            );

            // Check RegisterClusterNode
            let cluster_node = state.get_cluster_node("worker-node-delta").unwrap();
            assert_eq!(cluster_node.endpoint, "/ip4/192.168.1.50/tcp/9200");
            assert_eq!(cluster_node.role, "edge_compute_worker");

            // Verify both have updated membership
            assert!(node.peers.contains("worker-node-delta"));
        }
    }

    #[test]
    fn test_split_brain_minority_partition_rejection() {
        // 5-node cluster: N1, N2, N3, N4, N5
        // Majority quorum = (5 / 2) + 1 = 3
        let n1_id = "node-1".to_string();
        let n2_id = "node-2".to_string();
        let n3_id = "node-3".to_string();
        let n4_id = "node-4".to_string();
        let n5_id = "node-5".to_string();

        let all_nodes = vec![
            n1_id.clone(),
            n2_id.clone(),
            n3_id.clone(),
            n4_id.clone(),
            n5_id.clone(),
        ];

        let mut n1 = RaftNode::new(&n1_id, all_nodes.clone());
        let mut n2 = RaftNode::new(&n2_id, all_nodes.clone());
        let mut n3 = RaftNode::new(&n3_id, all_nodes.clone());
        let mut n4 = RaftNode::new(&n4_id, all_nodes.clone());
        let mut n5 = RaftNode::new(&n5_id, all_nodes.clone());

        assert_eq!(n1.quorum(), 3);

        // Step 1: N1 elected leader in Term 1 by quorum {N1, N2, N3}
        n1.current_term = 1;
        n1.role = RaftRole::Leader;
        n1.become_leader();

        for n in [&mut n2, &mut n3, &mut n4, &mut n5] {
            n.current_term = 1;
        }

        // Initial committed entry in Term 1 across all nodes
        let initial_cmd = RaftCommand::UpdateMemoryGraph {
            agent_id: "agent-base".to_string(),
            node_id: n1_id.clone(),
            key: "epoch".to_string(),
            value: "1".to_string(),
            timestamp: 100,
        };
        n1.propose(initial_cmd).unwrap();

        for n in [&mut n2, &mut n3, &mut n4, &mut n5] {
            let req = n1.build_append_entries_for_peer(&n.node_id).unwrap();
            let resp = n.handle_append_entries(&req);
            let _ = n1.handle_append_entries_response(&n.node_id, &resp);
        }
        // Inform all of commit
        for n in [&mut n2, &mut n3, &mut n4, &mut n5] {
            let req = n1.build_append_entries_for_peer(&n.node_id).unwrap();
            n.handle_append_entries(&req);
        }
        assert_eq!(n1.commit_index, 1);
        assert_eq!(n2.commit_index, 1);
        assert_eq!(n3.commit_index, 1);

        // Step 2: Network Partition occurs!
        // Partition A (Minority: 2 nodes): {N1, N2}
        // Partition B (Majority: 3 nodes): {N3, N4, N5}

        // In Minority Partition A:
        // Leader N1 tries to propose a new command
        let minority_cmd = RaftCommand::UpdateMemoryGraph {
            agent_id: "agent-base".to_string(),
            node_id: n1_id.clone(),
            key: "epoch".to_string(),
            value: "partition-a-rogue-value".to_string(),
            timestamp: 200,
        };
        let minority_idx = n1.propose(minority_cmd).unwrap();
        assert_eq!(minority_idx, 2);

        // N1 can only replicate to N2
        let req_to_n2 = n1.build_append_entries_for_peer(&n2_id).unwrap();
        let resp_from_n2 = n2.handle_append_entries(&req_to_n2);
        assert!(resp_from_n2.success);

        // N1 processes N2's ACK:
        // Replicas matching index 2: {N1, N2} = 2 nodes.
        // Required quorum: 3.
        let committed_on_n1 = n1.handle_append_entries_response(&n2_id, &resp_from_n2).unwrap();
        assert!(
            committed_on_n1.is_empty(),
            "Minority partition MUST NOT commit any entries without strict majority quorum"
        );
        assert_eq!(
            n1.commit_index, 1,
            "N1 commit index must remain at 1 in minority partition"
        );
        assert_eq!(
            n1.replicated_state.get_memory("agent-base", "epoch"),
            Some(&"1".to_string()),
            "N1 state machine MUST NOT apply uncommitted minority entry"
        );

        // In Majority Partition B:
        // N3 times out and initiates election in Term 2
        n3.role = RaftRole::Candidate;
        n3.current_term = 2;
        n3.voted_for = Some(n3_id.clone());
        n3.votes_received.clear();
        n3.votes_received.insert(n3_id.clone());

        let vote_req_n3 = RequestVoteRequest {
            term: 2,
            candidate_id: n3_id.clone(),
            last_log_index: n3.last_log_index(),
            last_log_term: n3.last_log_term(),
        };

        // N4 and N5 grant vote to N3
        let resp_n4 = n4.handle_request_vote(&vote_req_n3);
        assert!(resp_n4.vote_granted);
        let resp_n5 = n5.handle_request_vote(&vote_req_n3);
        assert!(resp_n5.vote_granted);

        let won_with_n4 = n3.handle_vote_response(&n4_id, &resp_n4);
        assert!(!won_with_n4, "2 votes out of 5 is not strict majority quorum (required 3)");

        let won_by_n3 = n3.handle_vote_response(&n5_id, &resp_n5);
        assert!(won_by_n3, "N3 must achieve strict majority (3/5) and win election in Term 2");
        assert_eq!(n3.role, RaftRole::Leader);

        // N3 proposes a valid command in Term 2
        let majority_cmd = RaftCommand::UpdateMemoryGraph {
            agent_id: "agent-base".to_string(),
            node_id: n3_id.clone(),
            key: "epoch".to_string(),
            value: "2-majority-authorized".to_string(),
            timestamp: 300,
        };
        let majority_idx = n3.propose(majority_cmd).unwrap();
        assert_eq!(majority_idx, 2);

        // N3 replicates to N4 and N5
        let req_to_n4 = n3.build_append_entries_for_peer(&n4_id).unwrap();
        let resp_from_n4 = n4.handle_append_entries(&req_to_n4);
        assert!(resp_from_n4.success);

        let req_to_n5 = n3.build_append_entries_for_peer(&n5_id).unwrap();
        let resp_from_n5 = n5.handle_append_entries(&req_to_n5);
        assert!(resp_from_n5.success);

        // N3 processes response from N4: replicas = {N3, N4} = 2 < 3 (Quorum not yet achieved)
        let committed_on_n3_1 = n3.handle_append_entries_response(&n4_id, &resp_from_n4).unwrap();
        assert!(committed_on_n3_1.is_empty(), "2 nodes out of 5 cannot commit");
        assert_eq!(n3.commit_index, 1);

        // N3 processes response from N5: replicas = {N3, N4, N5} = 3 >= 3 (Quorum achieved!)
        let committed_on_n3_2 = n3.handle_append_entries_response(&n5_id, &resp_from_n5).unwrap();
        assert_eq!(committed_on_n3_2, vec![2]);
        assert_eq!(n3.commit_index, 2);
        assert_eq!(
            n3.replicated_state.get_memory("agent-base", "epoch"),
            Some(&"2-majority-authorized".to_string())
        );

        // Step 3: Partition Heals!
        // Leader N3 sends AppendEntries to partitioned nodes N1 and N2
        let sync_req_to_n1 = n3.build_append_entries_for_peer(&n1_id).unwrap();
        let sync_resp_from_n1 = n1.handle_append_entries(&sync_req_to_n1);
        assert!(
            sync_resp_from_n1.success,
            "N1 must accept authoritative term 2 entry and overwrite uncommitted term 1 rogue entry"
        );

        // N1 steps down to follower because Term 2 > Term 1
        assert_eq!(n1.role, RaftRole::Follower);
        assert_eq!(n1.current_term, 2);
        assert_eq!(n1.commit_index, 2);
        assert_eq!(
            n1.replicated_state.get_memory("agent-base", "epoch"),
            Some(&"2-majority-authorized".to_string()),
            "N1 must now reflect majority authoritative state"
        );

        // Sync N2 as well
        let sync_req_to_n2 = n3.build_append_entries_for_peer(&n2_id).unwrap();
        let sync_resp_from_n2 = n2.handle_append_entries(&sync_req_to_n2);
        assert!(sync_resp_from_n2.success);
        assert_eq!(n2.commit_index, 2);
        assert_eq!(
            n2.replicated_state.get_memory("agent-base", "epoch"),
            Some(&"2-majority-authorized".to_string())
        );
    }

    #[test]
    fn test_snapshotting_and_log_compaction_lifecycle() {
        let mut node = RaftNode::new("snapshot-node", vec!["peer-1".to_string()]);
        node.current_term = 1;
        node.role = RaftRole::Leader;
        node.become_leader();

        // Propose and commit 5 commands
        for i in 1..=5 {
            let cmd = RaftCommand::UpdateMemoryGraph {
                agent_id: "agent-snap".to_string(),
                node_id: "snapshot-node".to_string(),
                key: format!("k{}", i),
                value: format!("v{}", i),
                timestamp: i as u64,
            };
            node.propose(cmd).unwrap();
        }

        // Simulate peer replication & commit all 5 entries
        let resp = AppendEntriesResponse {
            term: 1,
            success: true,
            match_index: 5,
            reason: None,
        };
        let committed = node.handle_append_entries_response("peer-1", &resp).unwrap();
        assert_eq!(committed, vec![1, 2, 3, 4, 5]);
        assert_eq!(node.commit_index, 5);
        assert_eq!(node.last_applied, 5);
        assert_eq!(node.log.len(), 5);

        // Capture snapshot up to index 5
        let snapshot = node.create_snapshot().expect("Create snapshot");
        assert_eq!(snapshot.last_included_index, 5);
        assert_eq!(snapshot.last_included_term, 1);
        assert_eq!(snapshot.state.last_applied_index, 5);

        // Prune/compact log up to index 3
        let pruned = node.compact_log(3).expect("Compact log up to 3");
        assert_eq!(pruned, 3);
        assert_eq!(node.log.len(), 2);
        assert_eq!(node.last_included_index, 3);
        assert_eq!(node.last_log_index(), 5);

        // Verify remaining entries can still be queried
        assert_eq!(node.get_entry_term(3), Some(1));
        assert_eq!(node.get_entry_term(4), Some(1));
        assert_eq!(node.get_entry_term(5), Some(1));

        // Install snapshot onto a lagging follower
        let mut lagging_node = RaftNode::new("lagging-follower", vec!["snapshot-node".to_string()]);
        lagging_node.install_snapshot(snapshot).expect("Install snapshot");
        assert_eq!(lagging_node.commit_index, 5);
        assert_eq!(lagging_node.last_applied, 5);
        assert_eq!(lagging_node.last_included_index, 5);
        assert_eq!(
            lagging_node.replicated_state.get_memory("agent-snap", "k1"),
            Some(&"v1".to_string())
        );
        assert_eq!(
            lagging_node.replicated_state.get_memory("agent-snap", "k5"),
            Some(&"v5".to_string())
        );
    }

    #[tokio::test]
    async fn test_distributed_raft_engine_async_handle() {
        let engine = DistributedRaftEngine::new("async-alpha", vec!["async-beta".to_string()]);

        // Non-leader propose must fail with NotLeader
        let cmd = RaftCommand::UpdateMemoryGraph {
            agent_id: "agent-async".to_string(),
            node_id: "async-alpha".to_string(),
            key: "test".to_string(),
            value: "async_val".to_string(),
            timestamp: 100,
        };
        let err = engine.propose(cmd.clone()).await;
        assert!(matches!(err, Err(MeshError::NotLeader)));

        // Advance election timer
        for _ in 0..5 {
            let _ = engine.step_election_timer().await;
        }

        assert_eq!(engine.role().await, RaftRole::Candidate);
        assert_eq!(engine.current_term().await, 1);

        // Grant vote from async-beta
        let vote_resp = RequestVoteResponse {
            term: 1,
            vote_granted: true,
            reason: None,
        };
        let became_leader = engine.handle_vote_response("async-beta", vote_resp).await;
        assert!(became_leader);
        assert_eq!(engine.role().await, RaftRole::Leader);

        // Propose command as leader
        let log_idx = engine.propose(cmd).await.expect("Propose as leader");
        assert_eq!(log_idx, 1);

        // Replicate to peer
        let req = engine
            .build_append_entries_for_peer("async-beta")
            .await
            .expect("Build entries");
        assert_eq!(req.entries.len(), 1);

        let resp = AppendEntriesResponse {
            term: 1,
            success: true,
            match_index: 1,
            reason: None,
        };
        let committed = engine
            .handle_append_entries_response("async-beta", resp)
            .await
            .unwrap();
        assert_eq!(committed, vec![1]);

        let state = engine.get_replicated_state().await;
        assert_eq!(
            state.get_memory("agent-async", "test"),
            Some(&"async_val".to_string())
        );
    }
}
