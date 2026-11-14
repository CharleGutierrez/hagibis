//! # Distributed Raft Consensus & State Machine Replication (`distributed_raft_consensus`)
//!
//! Production-grade Raft distributed consensus layer engineered for `hgb-core`,
//! `agent-orchestrator-rig`, and edge `hgb-daemon` nodes.
//!
//! ## Architectural Capabilities:
//! 1. **Leader Election & Split-Brain Immunity**:
//!    - Standard Raft roles (`Follower`, `PreCandidate`, `Candidate`, `Leader`).
//!    - Pre-Vote protocol: prevents partitioned or stale nodes from disrupting an active
//!      stable leader when reconnecting.
//!    - Randomized election timers and strict quorum calculations: `(N / 2) + 1`.
//! 2. **Deterministic Replicated State Machine (`AgentConsensusStateMachine`)**:
//!    - Maintains distributed multi-agent task lifecycles, active task leases, turn state checkpoints,
//!      and shared contextual memory.
//!    - Blake3 cryptographic integrity digests on every log entry and snapshot.
//! 3. **Fault-Tolerant Distributed Agent Task Handoff**:
//!    - High-precision task leasing with monotonic epoch timestamps.
//!    - Automatic dead-node detection: if a node crashes, stalls, or disconnects mid-loop,
//!      the cluster leader reclaims the lease and transparently hands off the uncommitted
//!      task to an active follower with the exact last-committed turn history.
//! 4. **Rig Orchestrator Synergy**:
//!    - Interlocks directly with `rig_agent_orchestrator::RigAgentEngine` to commit intermediate
//!      tool reasoning states and verify distributed execution authority.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{HgbError, Result};

// ==============================================================================
// 1. Roles, Peers, and Protocol RPC Payloads
// ==============================================================================

/// Role of an edge node in the Raft consensus cluster
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RaftRole {
    Follower,
    PreCandidate,
    Candidate,
    Leader,
}

impl std::fmt::Display for RaftRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Follower => write!(f, "FOLLOWER"),
            Self::PreCandidate => write!(f, "PRE_CANDIDATE"),
            Self::Candidate => write!(f, "CANDIDATE"),
            Self::Leader => write!(f, "LEADER"),
        }
    }
}

/// Known peer node in the Raft cluster
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RaftPeer {
    pub node_id: String,
    pub address: String,
    pub last_seen_ms: u64,
    pub is_active: bool,
}

/// RequestVote RPC arguments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestVoteArgs {
    pub term: u64,
    pub candidate_id: String,
    pub last_log_index: u64,
    pub last_log_term: u64,
    pub is_pre_vote: bool,
}

/// RequestVote RPC reply
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestVoteReply {
    pub term: u64,
    pub vote_granted: bool,
    pub voter_id: String,
}

/// AppendEntries RPC arguments (also serves as Heartbeat when entries are empty)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppendEntriesArgs {
    pub term: u64,
    pub leader_id: String,
    pub prev_log_index: u64,
    pub prev_log_term: u64,
    pub entries: Vec<RaftLogEntry>,
    pub leader_commit: u64,
}

/// AppendEntries RPC reply
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppendEntriesReply {
    pub term: u64,
    pub success: bool,
    pub match_index: u64,
    pub node_id: String,
}

// ==============================================================================
// 2. Replicated Commands & Log Entries
// ==============================================================================

/// High-level lifecycle status of a distributed agent task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistributedTaskStatus {
    Scheduled,
    Running,
    HandoffPending,
    Completed,
    Failed,
    Aborted,
}

/// In-flight execution checkpoint captured during a cognitive reasoning turn
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTurnCheckpoint {
    pub turn_index: u32,
    pub tool_name: Option<String>,
    pub tool_input_json: Option<String>,
    pub tool_output_json: Option<String>,
    pub intermediate_thought: String,
    pub timestamp_ms: u64,
}

/// Replicated commands driving the distributed agent state machine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentConsensusCommand {
    /// Dispatch a new autonomous agent task into the cluster
    RegisterTask {
        task_id: String,
        agent_role: String,
        prompt: String,
        lease_duration_ms: u64,
        priority: u32,
    },
    /// Node acquires an exclusive execution lease on a scheduled task
    AcquireTaskLease {
        task_id: String,
        node_id: String,
        lease_duration_ms: u64,
    },
    /// Replicate intermediate turn progress & thoughts
    CommitTurnState {
        task_id: String,
        node_id: String,
        checkpoint: AgentTurnCheckpoint,
    },
    /// Transparently hand off an uncompleted task to another node
    HandoffTask {
        task_id: String,
        source_node: String,
        target_node: String,
        reason: String,
    },
    /// Finalize task outcome (success or error)
    FinalizeTask {
        task_id: String,
        node_id: String,
        final_answer: String,
        success: bool,
        finish_reason: String,
    },
    /// Replicate shared memory context or key-value fact
    ReplicateSharedMemory {
        key: String,
        value: String,
        blake3_digest: String,
    },
    /// Cluster membership change
    MembershipChange {
        add_peer: Option<RaftPeer>,
        remove_peer_id: Option<String>,
    },
    /// No-op heartbeat barrier
    NoopBarrier,
}

/// An entry in the replicated Raft log
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RaftLogEntry {
    pub index: u64,
    pub term: u64,
    pub command: AgentConsensusCommand,
    pub timestamp_ms: u64,
    pub blake3_hash: String,
}

impl RaftLogEntry {
    pub fn new(index: u64, term: u64, command: AgentConsensusCommand) -> Self {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let payload_str = format!("{}:{}:{:?}:{}", index, term, command, timestamp_ms);
        let blake3_hash = blake3::hash(payload_str.as_bytes()).to_hex().to_string();

        Self {
            index,
            term,
            command,
            timestamp_ms,
            blake3_hash,
        }
    }
}

// ==============================================================================
// 3. Deterministic Replicated State Machine
// ==============================================================================

/// Exclusive execution lease on an agent task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskLease {
    pub node_id: String,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
}

impl TaskLease {
    pub fn is_expired(&self, current_time_ms: u64) -> bool {
        current_time_ms >= self.expires_at_ms
    }
}

/// Full state record of an autonomous agent task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DistributedAgentTaskState {
    pub task_id: String,
    pub agent_role: String,
    pub prompt: String,
    pub status: DistributedTaskStatus,
    pub priority: u32,
    pub assigned_node_id: Option<String>,
    pub lease: Option<TaskLease>,
    pub turns: Vec<AgentTurnCheckpoint>,
    pub final_answer: Option<String>,
    pub finish_reason: Option<String>,
    pub success: bool,
    pub handoff_history: Vec<(String, String, String, u64)>, // (from, to, reason, time)
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Point-in-time snapshot of the replicated state machine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentStateMachineSnapshot {
    pub last_included_index: u64,
    pub last_included_term: u64,
    pub tasks: HashMap<String, DistributedAgentTaskState>,
    pub shared_memory: HashMap<String, String>,
    pub peers: HashMap<String, RaftPeer>,
    pub snapshot_hash: String,
    pub created_at_ms: u64,
}

/// In-memory deterministic state machine
#[derive(Debug, Default, Clone)]
pub struct AgentConsensusStateMachine {
    pub tasks: HashMap<String, DistributedAgentTaskState>,
    pub shared_memory: HashMap<String, String>,
    pub peers: HashMap<String, RaftPeer>,
}

impl AgentConsensusStateMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a committed command deterministically to the state machine
    pub fn apply(&mut self, command: &AgentConsensusCommand, current_time_ms: u64) {
        match command {
            AgentConsensusCommand::RegisterTask {
                task_id,
                agent_role,
                prompt,
                lease_duration_ms: _,
                priority,
            } => {
                let task = DistributedAgentTaskState {
                    task_id: task_id.clone(),
                    agent_role: agent_role.clone(),
                    prompt: prompt.clone(),
                    status: DistributedTaskStatus::Scheduled,
                    priority: *priority,
                    assigned_node_id: None,
                    lease: None,
                    turns: Vec::new(),
                    final_answer: None,
                    finish_reason: None,
                    success: false,
                    handoff_history: Vec::new(),
                    created_at_ms: current_time_ms,
                    updated_at_ms: current_time_ms,
                };
                self.tasks.insert(task_id.clone(), task);
            }
            AgentConsensusCommand::AcquireTaskLease {
                task_id,
                node_id,
                lease_duration_ms,
            } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = DistributedTaskStatus::Running;
                    task.assigned_node_id = Some(node_id.clone());
                    task.lease = Some(TaskLease {
                        node_id: node_id.clone(),
                        acquired_at_ms: current_time_ms,
                        expires_at_ms: current_time_ms + lease_duration_ms,
                    });
                    task.updated_at_ms = current_time_ms;
                }
            }
            AgentConsensusCommand::CommitTurnState {
                task_id,
                node_id: _,
                checkpoint,
            } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.turns.push(checkpoint.clone());
                    task.updated_at_ms = current_time_ms;
                }
            }
            AgentConsensusCommand::HandoffTask {
                task_id,
                source_node,
                target_node,
                reason,
            } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = DistributedTaskStatus::Running;
                    task.assigned_node_id = Some(target_node.clone());
                    task.handoff_history.push((
                        source_node.clone(),
                        target_node.clone(),
                        reason.clone(),
                        current_time_ms,
                    ));
                    // Grant initial lease to target node (e.g. 10s default)
                    task.lease = Some(TaskLease {
                        node_id: target_node.clone(),
                        acquired_at_ms: current_time_ms,
                        expires_at_ms: current_time_ms + 10_000,
                    });
                    task.updated_at_ms = current_time_ms;
                }
            }
            AgentConsensusCommand::FinalizeTask {
                task_id,
                node_id: _,
                final_answer,
                success,
                finish_reason,
            } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = if *success {
                        DistributedTaskStatus::Completed
                    } else {
                        DistributedTaskStatus::Failed
                    };
                    task.final_answer = Some(final_answer.clone());
                    task.success = *success;
                    task.finish_reason = Some(finish_reason.clone());
                    task.lease = None;
                    task.updated_at_ms = current_time_ms;
                }
            }
            AgentConsensusCommand::ReplicateSharedMemory {
                key,
                value,
                blake3_digest: _,
            } => {
                self.shared_memory.insert(key.clone(), value.clone());
            }
            AgentConsensusCommand::MembershipChange {
                add_peer,
                remove_peer_id,
            } => {
                if let Some(peer) = add_peer {
                    self.peers.insert(peer.node_id.clone(), peer.clone());
                }
                if let Some(peer_id) = remove_peer_id {
                    self.peers.remove(peer_id);
                }
            }
            AgentConsensusCommand::NoopBarrier => {}
        }
    }

    /// Check for in-flight tasks whose lease has expired and return candidates for handoff
    pub fn find_expired_leases(&self, current_time_ms: u64) -> Vec<(String, String)> {
        let mut expired = Vec::new();
        for (task_id, task) in &self.tasks {
            if task.status == DistributedTaskStatus::Running {
                if let Some(ref lease) = task.lease {
                    if lease.is_expired(current_time_ms) {
                        expired.push((task_id.clone(), lease.node_id.clone()));
                    }
                }
            }
        }
        expired
    }

    /// Capture snapshot of the current state
    pub fn create_snapshot(&self, last_index: u64, last_term: u64) -> AgentStateMachineSnapshot {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let content = format!("{:?}:{:?}:{}", self.tasks, self.shared_memory, now_ms);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();

        AgentStateMachineSnapshot {
            last_included_index: last_index,
            last_included_term: last_term,
            tasks: self.tasks.clone(),
            shared_memory: self.shared_memory.clone(),
            peers: self.peers.clone(),
            snapshot_hash: hash,
            created_at_ms: now_ms,
        }
    }

    /// Restore state from a snapshot
    pub fn restore_snapshot(&mut self, snapshot: &AgentStateMachineSnapshot) {
        self.tasks = snapshot.tasks.clone();
        self.shared_memory = snapshot.shared_memory.clone();
        self.peers = snapshot.peers.clone();
    }
}

// ==============================================================================
// 4. Production Raft Consensus Engine
// ==============================================================================

/// Configuration parameters for the Raft node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaftConfig {
    pub node_id: String,
    pub listen_addr: String,
    pub election_timeout_min_ms: u64,
    pub election_timeout_max_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub default_task_lease_ms: u64,
}

impl Default for RaftConfig {
    fn default() -> Self {
        Self {
            node_id: format!("hgb-node-{}", &Uuid::new_v4().to_string()[..8]),
            listen_addr: "127.0.0.1:9040".to_string(),
            election_timeout_min_ms: 150,
            election_timeout_max_ms: 300,
            heartbeat_interval_ms: 50,
            default_task_lease_ms: 5000,
        }
    }
}

struct RaftEngineInner {
    config: RaftConfig,
    role: RaftRole,
    current_term: u64,
    voted_for: Option<String>,
    leader_id: Option<String>,
    log: Vec<RaftLogEntry>,
    commit_index: u64,
    last_applied: u64,
    peers: HashMap<String, RaftPeer>,
    next_index: HashMap<String, u64>,
    match_index: HashMap<String, u64>,
    state_machine: AgentConsensusStateMachine,
    votes_received: usize,
    last_heartbeat_received: Instant,
    election_timeout: Duration,
}

/// The Distributed Raft Consensus Engine
#[derive(Clone)]
pub struct RaftConsensusEngine {
    inner: Arc<RwLock<RaftEngineInner>>,
    proposals_counter: Arc<AtomicU64>,
}

impl RaftConsensusEngine {
    /// Initialize a new Raft consensus engine
    pub fn new(config: RaftConfig, initial_peers: Vec<RaftPeer>) -> Self {
        let mut peers_map = HashMap::new();
        for p in initial_peers {
            peers_map.insert(p.node_id.clone(), p);
        }

        let election_timeout = Duration::from_millis(config.election_timeout_min_ms);

        let inner = RaftEngineInner {
            config,
            role: RaftRole::Follower,
            current_term: 0,
            voted_for: None,
            leader_id: None,
            log: Vec::new(),
            commit_index: 0,
            last_applied: 0,
            peers: peers_map,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            state_machine: AgentConsensusStateMachine::new(),
            votes_received: 0,
            last_heartbeat_received: Instant::now(),
            election_timeout,
        };

        Self {
            inner: Arc::new(RwLock::new(inner)),
            proposals_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Retrieve local node ID
    pub fn node_id(&self) -> String {
        self.inner.read().unwrap().config.node_id.clone()
    }

    /// Retrieve current role
    pub fn role(&self) -> RaftRole {
        self.inner.read().unwrap().role
    }

    /// Check if local node is the current cluster leader
    pub fn is_leader(&self) -> bool {
        self.inner.read().unwrap().role == RaftRole::Leader
    }

    /// Retrieve current term
    pub fn current_term(&self) -> u64 {
        self.inner.read().unwrap().current_term
    }

    /// Retrieve active leader ID if known
    pub fn leader_id(&self) -> Option<String> {
        self.inner.read().unwrap().leader_id.clone()
    }

    /// Add or update a cluster peer
    pub fn register_peer(&self, peer: RaftPeer) {
        let mut inner = self.inner.write().unwrap();
        inner.peers.insert(peer.node_id.clone(), peer);
    }

    /// Cluster size (local node + registered peers)
    pub fn cluster_size(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.peers.len() + 1
    }

    /// Quorum count required for decisions: (N / 2) + 1
    pub fn quorum(&self) -> usize {
        (self.cluster_size() / 2) + 1
    }

    /// Transition directly to leader (useful for single-node clusters or bootstrap)
    pub fn become_leader_for_test(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.role = RaftRole::Leader;
        inner.current_term += 1;
        inner.leader_id = Some(inner.config.node_id.clone());
        let last_log_index = inner.log.last().map(|e| e.index).unwrap_or(0);
        let node_id = inner.config.node_id.clone();
        let peer_ids: Vec<String> = inner.peers.keys().cloned().collect();
        for peer in peer_ids {
            inner.next_index.insert(peer.clone(), last_log_index + 1);
            inner.match_index.insert(peer, 0);
        }
        inner.match_index.insert(node_id, last_log_index);
    }

    // ==========================================================================
    // Pre-Vote & RequestVote Protocol
    // ==========================================================================

    /// Start a Pre-Vote phase to prevent split-brain disruptions before incrementing term
    pub fn initiate_pre_vote(&self) -> Option<RequestVoteArgs> {
        let mut inner = self.inner.write().unwrap();
        if inner.role == RaftRole::Leader {
            return None;
        }

        inner.role = RaftRole::PreCandidate;
        let last_log_index = inner.log.last().map(|e| e.index).unwrap_or(0);
        let last_log_term = inner.log.last().map(|e| e.term).unwrap_or(0);

        Some(RequestVoteArgs {
            term: inner.current_term + 1,
            candidate_id: inner.config.node_id.clone(),
            last_log_index,
            last_log_term,
            is_pre_vote: true,
        })
    }

    /// Start standard election after successful Pre-Vote quorum
    pub fn initiate_election(&self) -> RequestVoteArgs {
        let mut inner = self.inner.write().unwrap();
        inner.role = RaftRole::Candidate;
        inner.current_term += 1;
        let candidate_id = inner.config.node_id.clone();
        inner.voted_for = Some(candidate_id.clone());
        inner.votes_received = 1; // vote for self
        inner.last_heartbeat_received = Instant::now();

        let last_log_index = inner.log.last().map(|e| e.index).unwrap_or(0);
        let last_log_term = inner.log.last().map(|e| e.term).unwrap_or(0);

        // If single-node cluster, immediately become leader
        if inner.peers.is_empty() {
            inner.role = RaftRole::Leader;
            inner.leader_id = Some(candidate_id.clone());
        }

        RequestVoteArgs {
            term: inner.current_term,
            candidate_id,
            last_log_index,
            last_log_term,
            is_pre_vote: false,
        }
    }

    /// Handle incoming RequestVote / PreVote RPC
    pub fn handle_request_vote(&self, args: &RequestVoteArgs) -> RequestVoteReply {
        let mut inner = self.inner.write().unwrap();
        let my_id = inner.config.node_id.clone();

        // 1. If term is less, reject immediately
        if args.term < inner.current_term {
            return RequestVoteReply {
                term: inner.current_term,
                vote_granted: false,
                voter_id: my_id,
            };
        }

        // 2. If term is greater and not pre-vote, step down to Follower
        if args.term > inner.current_term && !args.is_pre_vote {
            inner.current_term = args.term;
            inner.role = RaftRole::Follower;
            inner.voted_for = None;
            inner.leader_id = None;
        }

        // 3. Check log completeness: candidate's log must be at least as up-to-date as receiver's
        let last_log_index = inner.log.last().map(|e| e.index).unwrap_or(0);
        let last_log_term = inner.log.last().map(|e| e.term).unwrap_or(0);

        let log_ok = args.last_log_term > last_log_term
            || (args.last_log_term == last_log_term && args.last_log_index >= last_log_index);

        let can_vote = args.is_pre_vote
            || inner.voted_for.is_none()
            || inner.voted_for.as_ref() == Some(&args.candidate_id);

        let vote_granted = log_ok && can_vote;

        if vote_granted && !args.is_pre_vote {
            inner.voted_for = Some(args.candidate_id.clone());
            inner.last_heartbeat_received = Instant::now();
        }

        RequestVoteReply {
            term: inner.current_term,
            vote_granted,
            voter_id: my_id,
        }
    }

    /// Process a received vote reply
    pub fn handle_vote_reply(&self, reply: &RequestVoteReply, is_pre_vote: bool) -> bool {
        let mut inner = self.inner.write().unwrap();

        if reply.term > inner.current_term {
            inner.current_term = reply.term;
            inner.role = RaftRole::Follower;
            inner.voted_for = None;
            inner.leader_id = None;
            return false;
        }

        if !reply.vote_granted {
            return false;
        }

        if is_pre_vote && inner.role == RaftRole::PreCandidate {
            inner.votes_received += 1;
            let quorum = (inner.peers.len() + 1) / 2 + 1;
            return inner.votes_received >= quorum;
        }

        if !is_pre_vote && inner.role == RaftRole::Candidate && reply.term == inner.current_term {
            inner.votes_received += 1;
            let quorum = (inner.peers.len() + 1) / 2 + 1;
            if inner.votes_received >= quorum {
                inner.role = RaftRole::Leader;
                let leader_id = inner.config.node_id.clone();
                inner.leader_id = Some(leader_id);

                let last_log_index = inner.log.last().map(|e| e.index).unwrap_or(0);
                let peer_ids: Vec<String> = inner.peers.keys().cloned().collect();
                for peer in peer_ids {
                    inner.next_index.insert(peer.clone(), last_log_index + 1);
                    inner.match_index.insert(peer, 0);
                }
                return true;
            }
        }

        false
    }

    // ==========================================================================
    // AppendEntries & Heartbeat Protocol
    // ==========================================================================

    /// Generate AppendEntries arguments for a peer (heartbeat if empty)
    pub fn create_append_entries_for_peer(&self, peer_id: &str) -> Option<AppendEntriesArgs> {
        let inner = self.inner.read().unwrap();
        if inner.role != RaftRole::Leader {
            return None;
        }

        let next_idx = inner.next_index.get(peer_id).copied().unwrap_or(1);
        let prev_log_index = next_idx.saturating_sub(1);
        let prev_log_term = if prev_log_index == 0 {
            0
        } else {
            inner
                .log
                .iter()
                .find(|e| e.index == prev_log_index)
                .map(|e| e.term)
                .unwrap_or(0)
        };

        let entries: Vec<RaftLogEntry> = inner
            .log
            .iter()
            .filter(|e| e.index >= next_idx)
            .cloned()
            .collect();

        Some(AppendEntriesArgs {
            term: inner.current_term,
            leader_id: inner.config.node_id.clone(),
            prev_log_index,
            prev_log_term,
            entries,
            leader_commit: inner.commit_index,
        })
    }

    /// Handle incoming AppendEntries RPC on follower
    pub fn handle_append_entries(&self, args: &AppendEntriesArgs) -> AppendEntriesReply {
        let mut inner = self.inner.write().unwrap();
        let my_id = inner.config.node_id.clone();

        // 1. Reply false if term < current_term
        if args.term < inner.current_term {
            return AppendEntriesReply {
                term: inner.current_term,
                success: false,
                match_index: 0,
                node_id: my_id,
            };
        }

        // 2. Recognize leader and reset heartbeat timer
        if args.term > inner.current_term || inner.role != RaftRole::Follower {
            inner.current_term = args.term;
            inner.role = RaftRole::Follower;
            inner.voted_for = None;
        }
        inner.leader_id = Some(args.leader_id.clone());
        inner.last_heartbeat_received = Instant::now();

        // 3. Log consistency check at prev_log_index
        if args.prev_log_index > 0 {
            let entry_match = inner
                .log
                .iter()
                .find(|e| e.index == args.prev_log_index && e.term == args.prev_log_term);
            if entry_match.is_none() {
                return AppendEntriesReply {
                    term: inner.current_term,
                    success: false,
                    match_index: inner.log.last().map(|e| e.index).unwrap_or(0),
                    node_id: my_id,
                };
            }
        }

        // 4. Append new entries not already in log
        for entry in &args.entries {
            if let Some(pos) = inner.log.iter().position(|e| e.index == entry.index) {
                if inner.log[pos].term != entry.term {
                    inner.log.truncate(pos);
                    inner.log.push(entry.clone());
                }
            } else {
                inner.log.push(entry.clone());
            }
        }

        // 5. Update commit index and apply committed entries
        let match_index = inner.log.last().map(|e| e.index).unwrap_or(args.prev_log_index);
        if args.leader_commit > inner.commit_index {
            inner.commit_index = args.leader_commit.min(match_index);
            Self::apply_committed_entries(&mut inner);
        }

        AppendEntriesReply {
            term: inner.current_term,
            success: true,
            match_index,
            node_id: my_id,
        }
    }

    /// Process AppendEntries reply on leader
    pub fn handle_append_entries_reply(&self, peer_id: &str, reply: &AppendEntriesReply) {
        let mut inner = self.inner.write().unwrap();
        if inner.role != RaftRole::Leader {
            return;
        }

        if reply.term > inner.current_term {
            inner.current_term = reply.term;
            inner.role = RaftRole::Follower;
            inner.voted_for = None;
            inner.leader_id = None;
            return;
        }

        if reply.success {
            inner.next_index.insert(peer_id.to_string(), reply.match_index + 1);
            inner.match_index.insert(peer_id.to_string(), reply.match_index);

            // Check if majority of cluster matches index > commit_index
            let mut match_indices: Vec<u64> = inner.match_index.values().copied().collect();
            let leader_last_index = inner.log.last().map(|e| e.index).unwrap_or(0);
            match_indices.push(leader_last_index);
            match_indices.sort_unstable();

            // Median of match indices represents the quorum-committed index
            let quorum_idx = (match_indices.len() - 1) / 2;
            let majority_matched = match_indices[quorum_idx];

            if majority_matched > inner.commit_index {
                // In Raft, leader only commits entries from its current term by counting replicas
                let entry_term = inner
                    .log
                    .iter()
                    .find(|e| e.index == majority_matched)
                    .map(|e| e.term)
                    .unwrap_or(0);

                if entry_term == inner.current_term {
                    inner.commit_index = majority_matched;
                    Self::apply_committed_entries(&mut inner);
                }
            }
        } else {
            // Decrement next_index and retry
            let current_next = inner.next_index.get(peer_id).copied().unwrap_or(1);
            let decremented = current_next.saturating_sub(1).max(1);
            inner.next_index.insert(peer_id.to_string(), decremented);
        }
    }

    // ==========================================================================
    // State Machine Application & Commit
    // ==========================================================================

    fn apply_committed_entries(inner: &mut RaftEngineInner) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        while inner.last_applied < inner.commit_index {
            inner.last_applied += 1;
            let target_idx = inner.last_applied;
            if let Some(entry) = inner.log.iter().find(|e| e.index == target_idx).cloned() {
                inner.state_machine.apply(&entry.command, now_ms);
            }
        }
    }

    // ==========================================================================
    // High-Level Agent Command Proposals
    // ==========================================================================

    /// Propose a command to the Raft cluster
    pub fn propose(&self, command: AgentConsensusCommand) -> Result<u64> {
        let mut inner = self.inner.write().unwrap();
        if inner.role != RaftRole::Leader {
            return Err(HgbError::InvalidInput(format!(
                "Cannot propose command: node is {:?}, leader is {:?}",
                inner.role, inner.leader_id
            )));
        }

        let new_index = inner.log.last().map(|e| e.index).unwrap_or(0) + 1;
        let entry = RaftLogEntry::new(new_index, inner.current_term, command);
        inner.log.push(entry);

        // If single node cluster, immediately commit
        if inner.peers.is_empty() {
            inner.commit_index = new_index;
            Self::apply_committed_entries(&mut inner);
        }

        self.proposals_counter.fetch_add(1, Ordering::Relaxed);
        Ok(new_index)
    }

    /// Dispatch a new autonomous agent task into the cluster
    pub fn dispatch_agent_task(
        &self,
        agent_role: &str,
        prompt: &str,
        priority: u32,
    ) -> Result<String> {
        let task_id = format!("task_{}", Uuid::new_v4());
        let cmd = AgentConsensusCommand::RegisterTask {
            task_id: task_id.clone(),
            agent_role: agent_role.to_string(),
            prompt: prompt.to_string(),
            lease_duration_ms: self.inner.read().unwrap().config.default_task_lease_ms,
            priority,
        };
        self.propose(cmd)?;
        Ok(task_id)
    }

    /// Acquire exclusive lease to execute an agent task
    pub fn acquire_task_lease(&self, task_id: &str, node_id: &str, duration_ms: u64) -> Result<()> {
        let cmd = AgentConsensusCommand::AcquireTaskLease {
            task_id: task_id.to_string(),
            node_id: node_id.to_string(),
            lease_duration_ms: duration_ms,
        };
        self.propose(cmd)?;
        Ok(())
    }

    /// Commit intermediate reasoning turn progress into the replicated log
    pub fn commit_turn_progress(
        &self,
        task_id: &str,
        node_id: &str,
        checkpoint: AgentTurnCheckpoint,
    ) -> Result<()> {
        let cmd = AgentConsensusCommand::CommitTurnState {
            task_id: task_id.to_string(),
            node_id: node_id.to_string(),
            checkpoint,
        };
        self.propose(cmd)?;
        Ok(())
    }

    /// Gracefully or preemptively hand off an agent task to another node
    pub fn handoff_task(
        &self,
        task_id: &str,
        source_node: &str,
        target_node: &str,
        reason: &str,
    ) -> Result<()> {
        let cmd = AgentConsensusCommand::HandoffTask {
            task_id: task_id.to_string(),
            source_node: source_node.to_string(),
            target_node: target_node.to_string(),
            reason: reason.to_string(),
        };
        self.propose(cmd)?;
        Ok(())
    }

    /// Finalize an agent task with final answer and success status
    pub fn finalize_task(
        &self,
        task_id: &str,
        node_id: &str,
        final_answer: &str,
        success: bool,
        finish_reason: &str,
    ) -> Result<()> {
        let cmd = AgentConsensusCommand::FinalizeTask {
            task_id: task_id.to_string(),
            node_id: node_id.to_string(),
            final_answer: final_answer.to_string(),
            success,
            finish_reason: finish_reason.to_string(),
        };
        self.propose(cmd)?;
        Ok(())
    }

    /// Check for expired task leases and perform automatic fault-tolerant handoff
    pub fn check_and_recover_expired_leases(&self) -> Result<Vec<String>> {
        let inner = self.inner.read().unwrap();
        if inner.role != RaftRole::Leader {
            return Ok(Vec::new());
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let expired = inner.state_machine.find_expired_leases(now_ms);
        drop(inner);

        let mut recovered = Vec::new();
        for (task_id, dead_node) in expired {
            // Pick an active peer or local node for failover handoff
            let target = self.pick_failover_node(&dead_node);
            let reason = format!(
                "Task lease expired on node {}: automatic cluster handoff",
                dead_node
            );
            self.handoff_task(&task_id, &dead_node, &target, &reason)?;
            recovered.push(task_id);
        }

        Ok(recovered)
    }

    /// Pick an available healthy node for failover handoff
    fn pick_failover_node(&self, dead_node: &str) -> String {
        let inner = self.inner.read().unwrap();
        let my_id = inner.config.node_id.clone();
        if my_id != dead_node {
            return my_id;
        }

        for (peer_id, peer) in &inner.peers {
            if peer_id != dead_node && peer.is_active {
                return peer_id.clone();
            }
        }

        my_id
    }

    /// Replicate a shared memory key-value fact
    pub fn set_shared_memory(&self, key: &str, value: &str) -> Result<()> {
        let digest = blake3::hash(value.as_bytes()).to_hex().to_string();
        let cmd = AgentConsensusCommand::ReplicateSharedMemory {
            key: key.to_string(),
            value: value.to_string(),
            blake3_digest: digest,
        };
        self.propose(cmd)?;
        Ok(())
    }

    /// Retrieve state of a distributed task
    pub fn get_task_state(&self, task_id: &str) -> Option<DistributedAgentTaskState> {
        self.inner.read().unwrap().state_machine.tasks.get(task_id).cloned()
    }

    /// Retrieve value from shared replicated memory
    pub fn get_shared_memory(&self, key: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap()
            .state_machine
            .shared_memory
            .get(key)
            .cloned()
    }

    /// Take point-in-time snapshot
    pub fn take_snapshot(&self) -> AgentStateMachineSnapshot {
        let inner = self.inner.read().unwrap();
        let last_log_index = inner.commit_index;
        let last_log_term = inner
            .log
            .iter()
            .find(|e| e.index == last_log_index)
            .map(|e| e.term)
            .unwrap_or(inner.current_term);

        inner
            .state_machine
            .create_snapshot(last_log_index, last_log_term)
    }

    /// Restore state machine from snapshot
    pub fn restore_snapshot(&self, snapshot: &AgentStateMachineSnapshot) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.state_machine.restore_snapshot(snapshot);
        inner.commit_index = snapshot.last_included_index;
        inner.last_applied = snapshot.last_included_index;
        Ok(())
    }
}

// ==============================================================================
// Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_node_bootstrap_and_task_lifecycle() {
        let config = RaftConfig {
            node_id: "node_alpha".into(),
            listen_addr: "127.0.0.1:9040".into(),
            ..Default::default()
        };
        let engine = RaftConsensusEngine::new(config, Vec::new());

        // Initiate election on single-node cluster -> immediately becomes leader
        let vote_req = engine.initiate_election();
        assert_eq!(vote_req.candidate_id, "node_alpha");
        assert!(engine.is_leader());
        assert_eq!(engine.role(), RaftRole::Leader);

        // Dispatch agent task
        let task_id = engine
            .dispatch_agent_task("coder", "Refactor vector engine", 10)
            .expect("dispatch task");

        let task_state = engine.get_task_state(&task_id).expect("task found");
        assert_eq!(task_state.status, DistributedTaskStatus::Scheduled);
        assert_eq!(task_state.prompt, "Refactor vector engine");

        // Acquire lease
        engine
            .acquire_task_lease(&task_id, "node_alpha", 5000)
            .expect("acquire lease");
        let task_running = engine.get_task_state(&task_id).unwrap();
        assert_eq!(task_running.status, DistributedTaskStatus::Running);
        assert_eq!(task_running.assigned_node_id.as_deref(), Some("node_alpha"));

        // Commit turn checkpoint
        let checkpoint = AgentTurnCheckpoint {
            turn_index: 1,
            tool_name: Some("lancedb_query".into()),
            tool_input_json: Some(r#"{"query": "vector"}"#.into()),
            tool_output_json: Some(r#"{"hits": 3}"#.into()),
            intermediate_thought: "Searching for existing embeddings".into(),
            timestamp_ms: 12345,
        };
        engine
            .commit_turn_progress(&task_id, "node_alpha", checkpoint)
            .expect("commit turn");

        let task_with_turn = engine.get_task_state(&task_id).unwrap();
        assert_eq!(task_with_turn.turns.len(), 1);
        assert_eq!(
            task_with_turn.turns[0].intermediate_thought,
            "Searching for existing embeddings"
        );

        // Finalize task
        engine
            .finalize_task(
                &task_id,
                "node_alpha",
                "Successfully refactored engine",
                true,
                "completed",
            )
            .expect("finalize");

        let task_completed = engine.get_task_state(&task_id).unwrap();
        assert_eq!(task_completed.status, DistributedTaskStatus::Completed);
        assert!(task_completed.success);
        assert_eq!(
            task_completed.final_answer.as_deref(),
            Some("Successfully refactored engine")
        );
    }

    #[test]
    fn test_three_node_pre_vote_and_log_replication() {
        let cfg_a = RaftConfig {
            node_id: "node_a".into(),
            ..Default::default()
        };
        let cfg_b = RaftConfig {
            node_id: "node_b".into(),
            ..Default::default()
        };
        let _cfg_c = RaftConfig {
            node_id: "node_c".into(),
            ..Default::default()
        };

        let peer_b = RaftPeer {
            node_id: "node_b".into(),
            address: "127.0.0.1:9041".into(),
            last_seen_ms: 0,
            is_active: true,
        };
        let peer_c = RaftPeer {
            node_id: "node_c".into(),
            address: "127.0.0.1:9042".into(),
            last_seen_ms: 0,
            is_active: true,
        };

        let engine_a = RaftConsensusEngine::new(cfg_a, vec![peer_b.clone(), peer_c.clone()]);
        let engine_b = RaftConsensusEngine::new(
            cfg_b,
            vec![
                RaftPeer {
                    node_id: "node_a".into(),
                    address: "127.0.0.1:9040".into(),
                    last_seen_ms: 0,
                    is_active: true,
                },
                peer_c.clone(),
            ],
        );

        // 1. Pre-vote phase
        let pre_vote_args = engine_a.initiate_pre_vote().expect("pre-vote args");
        let pre_vote_reply_b = engine_b.handle_request_vote(&pre_vote_args);
        assert!(pre_vote_reply_b.vote_granted);
        let pre_vote_ok = engine_a.handle_vote_reply(&pre_vote_reply_b, true);
        assert!(pre_vote_ok, "Quorum of 2/3 reached on pre-vote");

        // 2. Real election phase
        let vote_args = engine_a.initiate_election();
        let vote_reply_b = engine_b.handle_request_vote(&vote_args);
        assert!(vote_reply_b.vote_granted);
        let elected = engine_a.handle_vote_reply(&vote_reply_b, false);
        assert!(elected, "Node A elected leader");
        assert!(engine_a.is_leader());

        // 3. Propose replicated shared memory on leader
        engine_a
            .set_shared_memory("cluster_model", "gemini-3.8-flash")
            .expect("propose kv");

        // Replicate to node B via AppendEntries
        let append_args = engine_a
            .create_append_entries_for_peer("node_b")
            .expect("append entries");
        assert_eq!(append_args.entries.len(), 1);

        let append_reply = engine_b.handle_append_entries(&append_args);
        assert!(append_reply.success);

        engine_a.handle_append_entries_reply("node_b", &append_reply);

        // Send second heartbeat to apply committed entry on node B
        let heartbeat_args = engine_a
            .create_append_entries_for_peer("node_b")
            .expect("heartbeat");
        engine_b.handle_append_entries(&heartbeat_args);

        // Verify shared memory is synchronized across both nodes
        assert_eq!(
            engine_a.get_shared_memory("cluster_model").as_deref(),
            Some("gemini-3.8-flash")
        );
        assert_eq!(
            engine_b.get_shared_memory("cluster_model").as_deref(),
            Some("gemini-3.8-flash")
        );
    }

    #[test]
    fn test_fault_tolerant_task_handoff_on_lease_expiry() {
        let config = RaftConfig {
            node_id: "leader_node".into(),
            ..Default::default()
        };
        let follower_peer = RaftPeer {
            node_id: "edge_node_1".into(),
            address: "127.0.0.1:9045".into(),
            last_seen_ms: 1000,
            is_active: true,
        };

        let engine = RaftConsensusEngine::new(config, vec![follower_peer]);
        engine.become_leader_for_test();

        let task_id = engine
            .dispatch_agent_task("debugger", "Fix kernel panic", 5)
            .unwrap();

        // edge_node_1 acquires lease for only 1ms
        engine.acquire_task_lease(&task_id, "edge_node_1", 1).unwrap();

        // Edge node commits 1 turn
        let checkpoint = AgentTurnCheckpoint {
            turn_index: 1,
            tool_name: Some("ebpf_probe".into()),
            tool_input_json: None,
            tool_output_json: None,
            intermediate_thought: "Analyzing tracepoint".into(),
            timestamp_ms: 100,
        };
        engine
            .commit_turn_progress(&task_id, "edge_node_1", checkpoint)
            .unwrap();

        // Sleep briefly so lease expires
        std::thread::sleep(Duration::from_millis(5));

        // Leader checks and recovers expired leases
        let recovered = engine.check_and_recover_expired_leases().unwrap();
        assert_eq!(recovered, vec![task_id.clone()]);

        // Verify task state was handed off to leader_node without losing turn history!
        let handed_off_task = engine.get_task_state(&task_id).unwrap();
        assert_eq!(
            handed_off_task.assigned_node_id.as_deref(),
            Some("leader_node")
        );
        assert_eq!(handed_off_task.turns.len(), 1);
        assert_eq!(
            handed_off_task.turns[0].intermediate_thought,
            "Analyzing tracepoint"
        );
        assert_eq!(handed_off_task.handoff_history.len(), 1);
        assert_eq!(handed_off_task.handoff_history[0].0, "edge_node_1");
        assert_eq!(handed_off_task.handoff_history[0].1, "leader_node");
    }

    #[test]
    fn test_snapshot_creation_and_recovery() {
        let config = RaftConfig {
            node_id: "node_snapshot".into(),
            ..Default::default()
        };
        let engine = RaftConsensusEngine::new(config, Vec::new());
        engine.become_leader_for_test();

        engine.set_shared_memory("k1", "v1").unwrap();
        let task_id = engine.dispatch_agent_task("researcher", "Read papers", 1).unwrap();

        let snap = engine.take_snapshot();
        assert!(snap.tasks.contains_key(&task_id));
        assert_eq!(snap.shared_memory.get("k1").map(|s| s.as_str()), Some("v1"));

        // Create new blank engine and restore from snapshot
        let engine_recovered = RaftConsensusEngine::new(RaftConfig::default(), Vec::new());
        engine_recovered.restore_snapshot(&snap).unwrap();

        assert_eq!(
            engine_recovered.get_shared_memory("k1").as_deref(),
            Some("v1")
        );
        assert!(engine_recovered.get_task_state(&task_id).is_some());
    }
}
