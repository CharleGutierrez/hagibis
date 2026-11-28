//! # Consensus-Mediated Mesh Coordinator (`consensus_mediated_mesh_coordinator`)
//!
//! Production-grade mesh coordination engine deeply mediating all distributed agent cluster operations
//! through strict Raft consensus (`DistributedRaftEngine`).
//!
//! Guarantees:
//! - Split-brain prevention across multi-node swarms via strict majority quorum `(cluster_size / 2) + 1`.
//! - Strongly consistent, atomic state replication before state application for:
//!   - Agent Memory Graph mutations (`AgentMemoryUpdate`)
//!   - LanceDB Vector Embedding shard index allocations (`VectorShardAllocation`)
//!   - Distributed Circuit Breaker transitions (`CircuitBreakerTransition`)
//!   - Autopoietic self-healing code patch commits (`AutopoieticPatchCommit`)
//! - Automatic leader failover, heartbeat maintenance, and election timeout management.
//! - Anti-entropy integration with `DistributedEdgeMemoryFabric`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::distributed_edge_memory_fabric::DistributedEdgeMemoryFabric;
pub use crate::distributed_raft_state_machine::{
    majority_quorum, AgentMemoryUpdate, AppendEntriesRequest, AppendEntriesResponse,
    AutopoieticPatchCommit, AutopoieticPatchInfo, BreakerInfo, CircuitBreakerTransition,
    ClusterNodeInfo, DistributedLogEntry, DistributedRaftEngine, ElectionStepResult,
    MeshConsensusState, RaftCommand, RaftNode, RaftRole, RequestVoteRequest, RequestVoteResponse,
    StateSnapshot, VectorShardAllocation, VectorShardInfo,
};
use crate::error::{MeshError, Result};

/// Pluggable peer transport abstraction for asynchronous Raft consensus RPC exchange
#[async_trait]
pub trait PeerConsensusTransport: Send + Sync {
    /// Dispatches a RequestVote RPC to a peer node
    async fn send_request_vote(&self, req: RequestVoteRequest) -> Result<RequestVoteResponse>;

    /// Dispatches an AppendEntries RPC (log replication or heartbeat) to a peer node
    async fn send_append_entries(&self, req: AppendEntriesRequest) -> Result<AppendEntriesResponse>;
}

/// In-memory direct consensus transport connecting two local coordinators / Raft engines
#[derive(Clone)]
pub struct InMemoryConsensusTransport {
    engine: DistributedRaftEngine,
}

impl InMemoryConsensusTransport {
    /// Creates a new in-memory transport wired to the target engine
    pub fn new(engine: DistributedRaftEngine) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl PeerConsensusTransport for InMemoryConsensusTransport {
    async fn send_request_vote(&self, req: RequestVoteRequest) -> Result<RequestVoteResponse> {
        Ok(self.engine.handle_request_vote(req).await)
    }

    async fn send_append_entries(&self, req: AppendEntriesRequest) -> Result<AppendEntriesResponse> {
        Ok(self.engine.handle_append_entries(req).await)
    }
}

/// Partitionable transport wrapping any inner `PeerConsensusTransport` to simulate network anomalies
#[derive(Clone)]
pub struct PartitionableConsensusTransport {
    inner: Arc<dyn PeerConsensusTransport>,
    is_partitioned: Arc<RwLock<bool>>,
}

impl PartitionableConsensusTransport {
    /// Wraps an inner transport with partition control
    pub fn new(inner: Arc<dyn PeerConsensusTransport>) -> Self {
        Self {
            inner,
            is_partitioned: Arc::new(RwLock::new(false)),
        }
    }

    /// Sets partition status (true = blocked, false = connected)
    pub async fn set_partitioned(&self, partitioned: bool) {
        let mut p = self.is_partitioned.write().await;
        *p = partitioned;
    }

    /// Checks partition status
    pub async fn is_partitioned(&self) -> bool {
        *self.is_partitioned.read().await
    }
}

#[async_trait]
impl PeerConsensusTransport for PartitionableConsensusTransport {
    async fn send_request_vote(&self, req: RequestVoteRequest) -> Result<RequestVoteResponse> {
        if *self.is_partitioned.read().await {
            return Err(MeshError::Network(
                "Simulated network partition: peer is unreachable".to_string(),
            ));
        }
        self.inner.send_request_vote(req).await
    }

    async fn send_append_entries(&self, req: AppendEntriesRequest) -> Result<AppendEntriesResponse> {
        if *self.is_partitioned.read().await {
            return Err(MeshError::Network(
                "Simulated network partition: peer is unreachable".to_string(),
            ));
        }
        self.inner.send_append_entries(req).await
    }
}

/// Coordinator configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusCoordinatorConfig {
    pub node_id: String,
    pub cluster_peers: Vec<String>,
    pub election_timeout_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub quorum_timeout_ms: u64,
}

impl Default for ConsensusCoordinatorConfig {
    fn default() -> Self {
        Self {
            node_id: format!("node-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            cluster_peers: Vec::new(),
            election_timeout_ms: 150,
            heartbeat_interval_ms: 50,
            quorum_timeout_ms: 2000,
        }
    }
}

/// Production-grade mesh coordinator mediating state synchronization through strict Raft consensus
#[derive(Clone)]
pub struct ConsensusMediatedMeshCoordinator {
    node_id: String,
    config: ConsensusCoordinatorConfig,
    engine: DistributedRaftEngine,
    cluster_nodes: Arc<RwLock<HashMap<String, ClusterNodeInfo>>>,
    transports: Arc<RwLock<HashMap<String, Arc<dyn PeerConsensusTransport>>>>,
}

impl ConsensusMediatedMeshCoordinator {
    /// Creates a new consensus-mediated mesh coordinator
    pub fn new(config: ConsensusCoordinatorConfig) -> Self {
        let node_id = config.node_id.clone();
        let engine = DistributedRaftEngine::new(node_id.clone(), config.cluster_peers.clone());

        Self {
            node_id,
            config,
            engine,
            cluster_nodes: Arc::new(RwLock::new(HashMap::new())),
            transports: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Wraps an existing `DistributedRaftEngine`
    pub fn from_engine(node_id: impl Into<String>, engine: DistributedRaftEngine) -> Self {
        let node_id = node_id.into();
        let config = ConsensusCoordinatorConfig {
            node_id: node_id.clone(),
            ..Default::default()
        };

        Self {
            node_id,
            config,
            engine,
            cluster_nodes: Arc::new(RwLock::new(HashMap::new())),
            transports: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Node identifier
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Access the underlying `DistributedRaftEngine`
    pub fn engine(&self) -> &DistributedRaftEngine {
        &self.engine
    }

    /// Access dynamic cluster node registry
    pub fn cluster_nodes(&self) -> Arc<RwLock<HashMap<String, ClusterNodeInfo>>> {
        Arc::clone(&self.cluster_nodes)
    }

    /// Registers a peer transport channel
    pub async fn register_peer_transport(
        &self,
        peer_id: impl Into<String>,
        transport: Arc<dyn PeerConsensusTransport>,
    ) {
        let mut t = self.transports.write().await;
        t.insert(peer_id.into(), transport);
    }

    /// Removes a peer transport channel
    pub async fn remove_peer_transport(&self, peer_id: &str) {
        let mut t = self.transports.write().await;
        t.remove(peer_id);
    }

    /// Retrieves list of connected peer IDs with registered transports
    pub async fn connected_peers(&self) -> Vec<String> {
        let t = self.transports.read().await;
        t.keys().cloned().collect()
    }

    // =========================================================================
    // Raft Consensus Proposal & Quorum Commit Operations
    // =========================================================================

    /// Proposes an agent memory update and awaits strict quorum commit before state application
    pub async fn propose_memory_update(&self, update: AgentMemoryUpdate) -> Result<u64> {
        let command = RaftCommand::from(update);
        self.propose_command(command).await
    }

    /// Proposes a vector embedding shard allocation and awaits strict quorum commit
    pub async fn propose_vector_shard_allocation(
        &self,
        allocation: VectorShardAllocation,
    ) -> Result<u64> {
        let command = RaftCommand::from(allocation);
        self.propose_command(command).await
    }

    /// Proposes a distributed circuit breaker transition and awaits strict quorum commit
    pub async fn propose_circuit_breaker_transition(
        &self,
        transition: CircuitBreakerTransition,
    ) -> Result<u64> {
        let command = RaftCommand::from(transition);
        self.propose_command(command).await
    }

    /// Proposes an autopoietic self-healing patch commit and awaits strict quorum commit
    pub async fn propose_autopoietic_patch_commit(
        &self,
        patch: AutopoieticPatchCommit,
    ) -> Result<u64> {
        let command = RaftCommand::from(patch);
        self.propose_command(command).await
    }

    /// Proposes dynamic cluster node registration across all nodes
    pub async fn propose_register_cluster_node(
        &self,
        node_id: String,
        endpoint: String,
        role: String,
    ) -> Result<u64> {
        let command = RaftCommand::RegisterClusterNode {
            node_id,
            endpoint,
            role,
        };
        self.propose_command(command).await
    }

    /// Core proposal dispatcher:
    /// 1. Verifies local node is Leader (returns `MeshError::NotLeader` otherwise)
    /// 2. Appends command to local Raft log with cryptographic SHA-256 digest
    /// 3. Replicates to connected peers and awaits majority quorum commit before state application
    pub async fn propose_command(&self, command: RaftCommand) -> Result<u64> {
        if self.engine.role().await != RaftRole::Leader {
            return Err(MeshError::NotLeader);
        }

        // Propose to local Raft engine
        let log_index = self.engine.propose(command).await?;

        // Single-node cluster commits immediately
        if self.engine.cluster_size().await == 1 {
            return Ok(log_index);
        }

        // Multi-node cluster: replicate to peers and await majority quorum commit
        let timeout = Duration::from_millis(self.config.quorum_timeout_ms);
        self.replicate_and_await_quorum(log_index, timeout).await?;

        Ok(log_index)
    }

    /// Actively replicates pending log entries to all peer transports until target index commits or times out
    pub async fn replicate_and_await_quorum(
        &self,
        target_log_index: u64,
        timeout: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let quorum = self.engine.quorum().await;

        while start.elapsed() < timeout {
            // Check if entry is already committed
            if self.engine.commit_index().await >= target_log_index {
                // Entry committed! Broadcast updated commit index to followers
                let _ = self.broadcast_heartbeats().await;
                return Ok(());
            }

            // Gather peers and build AppendEntries requests
            let peer_transports: Vec<(String, Arc<dyn PeerConsensusTransport>)> = {
                let t = self.transports.read().await;
                t.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
            };

            for (peer_id, transport) in peer_transports {
                if let Some(req) = self.engine.build_append_entries_for_peer(&peer_id).await {
                    match transport.send_append_entries(req).await {
                        Ok(resp) => {
                            let _ = self
                                .engine
                                .handle_append_entries_response(&peer_id, resp)
                                .await;
                        }
                        Err(err) => {
                            tracing::debug!(
                                peer_id = %peer_id,
                                error = %err,
                                "AppendEntries replication attempt failed"
                            );
                        }
                    }
                }
            }

            // Re-check commit index
            if self.engine.commit_index().await >= target_log_index {
                let _ = self.broadcast_heartbeats().await;
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Quorum not reached within timeout duration
        let achieved_matches = {
            let node = self.engine.inner().read().await;
            node.match_index
                .values()
                .filter(|&&idx| idx >= target_log_index)
                .count()
        };

        Err(MeshError::QuorumNotReached {
            required: quorum,
            achieved: achieved_matches,
        })
    }

    // =========================================================================
    // Leader Election & Automatic Failover
    // =========================================================================

    /// Triggers an election cycle:
    /// - Advances local term, votes for self
    /// - Broadcasts RequestVote to all peer transports concurrently
    /// - Counts votes; if majority quorum achieved, elevates to Leader and broadcasts heartbeats
    pub async fn trigger_election(&self) -> Result<bool> {
        let step_result = self.engine.trigger_election().await;

        match step_result {
            ElectionStepResult::ElectionWon { .. } => {
                // Immediate win (e.g. single node)
                let _ = self.broadcast_heartbeats().await;
                Ok(true)
            }
            ElectionStepResult::ElectionStarted {
                vote_request,
                peers,
                ..
            } => {
                let transports = {
                    let t = self.transports.read().await;
                    peers
                        .iter()
                        .filter_map(|p| t.get(p).cloned().map(|tr| (p.clone(), tr)))
                        .collect::<Vec<_>>()
                };

                let mut won_election = false;
                for (peer_id, transport) in transports {
                    match transport.send_request_vote(vote_request.clone()).await {
                        Ok(resp) => {
                            let elected = self
                                .engine
                                .handle_vote_response(&peer_id, resp)
                                .await;
                            if elected {
                                won_election = true;
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::debug!(
                                peer_id = %peer_id,
                                error = %err,
                                "RequestVote RPC rejected or unreachable"
                            );
                        }
                    }
                }

                if won_election {
                    // Send initial heartbeats to suppress election timers on peers
                    let _ = self.broadcast_heartbeats().await;
                    Ok(true)
                } else {
                    Ok(self.engine.role().await == RaftRole::Leader)
                }
            }
            ElectionStepResult::LeaderActive { .. } => Ok(true),
            ElectionStepResult::Ticked { .. } => Ok(false),
        }
    }

    /// Broadcasts heartbeats or pending log entries to all registered peer transports
    pub async fn broadcast_heartbeats(&self) -> Result<usize> {
        if self.engine.role().await != RaftRole::Leader {
            return Ok(0);
        }

        let peer_transports: Vec<(String, Arc<dyn PeerConsensusTransport>)> = {
            let t = self.transports.read().await;
            t.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
        };

        let mut successful = 0;
        for (peer_id, transport) in peer_transports {
            if let Some(req) = self.engine.build_append_entries_for_peer(&peer_id).await {
                if let Ok(resp) = transport.send_append_entries(req).await {
                    let _ = self
                        .engine
                        .handle_append_entries_response(&peer_id, resp)
                        .await;
                    successful += 1;
                }
            }
        }

        Ok(successful)
    }

    /// Handles an incoming RequestVote RPC from a candidate peer
    pub async fn handle_request_vote(&self, req: RequestVoteRequest) -> RequestVoteResponse {
        self.engine.handle_request_vote(req).await
    }

    /// Handles an incoming AppendEntries RPC from the cluster leader
    pub async fn handle_append_entries(&self, req: AppendEntriesRequest) -> AppendEntriesResponse {
        self.engine.handle_append_entries(req).await
    }

    // =========================================================================
    // State Machine Inspection & Anti-Entropy Synchronization
    // =========================================================================

    /// Returns a copy of the globally replicated state machine
    pub async fn get_replicated_state(&self) -> MeshConsensusState {
        self.engine.get_replicated_state().await
    }

    /// Reads a value from the replicated memory graph
    pub async fn get_memory(&self, agent_id: &str, key: &str) -> Option<String> {
        let state = self.engine.get_replicated_state().await;
        state.get_memory(agent_id, key).cloned()
    }

    /// Reads an agent's complete memory graph
    pub async fn get_agent_memory(&self, agent_id: &str) -> Option<HashMap<String, String>> {
        let state = self.engine.get_replicated_state().await;
        state.get_agent_memory(agent_id).cloned()
    }

    /// Reads vector shard metadata
    pub async fn get_vector_shard(&self, shard_id: &str) -> Option<VectorShardInfo> {
        let state = self.engine.get_replicated_state().await;
        state.get_vector_shard(shard_id).cloned()
    }

    /// Reads circuit breaker state
    pub async fn get_circuit_breaker(&self, breaker_id: &str) -> Option<BreakerInfo> {
        let state = self.engine.get_replicated_state().await;
        state.get_circuit_breaker(breaker_id).cloned()
    }

    /// Reads autopoietic patch metadata
    pub async fn get_autopoietic_patch(&self, patch_id: &str) -> Option<AutopoieticPatchInfo> {
        let state = self.engine.get_replicated_state().await;
        state.get_autopoietic_patch(patch_id).cloned()
    }

    /// Returns current Raft role
    pub async fn role(&self) -> RaftRole {
        self.engine.role().await
    }

    /// Returns current Raft term
    pub async fn current_term(&self) -> u64 {
        self.engine.current_term().await
    }

    /// Returns current commit index
    pub async fn commit_index(&self) -> u64 {
        self.engine.commit_index().await
    }

    /// Returns last log index
    pub async fn last_log_index(&self) -> u64 {
        self.engine.last_log_index().await
    }

    /// Returns the currently recognized leader ID, if any
    pub async fn current_leader(&self) -> Option<String> {
        self.engine.current_leader().await
    }

    /// Creates a point-in-time state machine snapshot and compacts log
    pub async fn create_snapshot_and_compact(&self) -> Result<StateSnapshot> {
        let snapshot = self.engine.create_snapshot().await?;
        self.engine.compact_log(snapshot.last_included_index).await?;
        Ok(snapshot)
    }

    /// Installs a point-in-time snapshot onto this coordinator
    pub async fn install_snapshot(&self, snapshot: StateSnapshot) -> Result<()> {
        self.engine.install_snapshot(snapshot).await
    }

    /// Synchronizes committed memory graph updates into a `DistributedEdgeMemoryFabric` instance
    pub async fn sync_committed_to_edge_fabric(
        &self,
        fabric: &DistributedEdgeMemoryFabric,
    ) -> usize {
        let state = self.engine.get_replicated_state().await;
        let mut synchronized_count = 0;

        for (agent_id, key_values) in &state.memory_graph {
            for (key, value) in key_values {
                let _ = fabric
                    .put_record(
                        agent_id,
                        key,
                        serde_json::Value::String(value.clone()),
                        None,
                    )
                    .await;
                synchronized_count += 1;
            }
        }

        synchronized_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to setup a fully connected 3-node in-memory cluster
    async fn setup_three_node_cluster() -> (
        ConsensusMediatedMeshCoordinator,
        ConsensusMediatedMeshCoordinator,
        ConsensusMediatedMeshCoordinator,
    ) {
        let config_a = ConsensusCoordinatorConfig {
            node_id: "node-alpha".to_string(),
            cluster_peers: vec!["node-beta".to_string(), "node-gamma".to_string()],
            quorum_timeout_ms: 1000,
            ..Default::default()
        };
        let config_b = ConsensusCoordinatorConfig {
            node_id: "node-beta".to_string(),
            cluster_peers: vec!["node-alpha".to_string(), "node-gamma".to_string()],
            quorum_timeout_ms: 1000,
            ..Default::default()
        };
        let config_c = ConsensusCoordinatorConfig {
            node_id: "node-gamma".to_string(),
            cluster_peers: vec!["node-alpha".to_string(), "node-beta".to_string()],
            quorum_timeout_ms: 1000,
            ..Default::default()
        };

        let node_a = ConsensusMediatedMeshCoordinator::new(config_a);
        let node_b = ConsensusMediatedMeshCoordinator::new(config_b);
        let node_c = ConsensusMediatedMeshCoordinator::new(config_c);

        // Wire Node A transports
        node_a
            .register_peer_transport(
                "node-beta",
                Arc::new(InMemoryConsensusTransport::new(node_b.engine().clone())),
            )
            .await;
        node_a
            .register_peer_transport(
                "node-gamma",
                Arc::new(InMemoryConsensusTransport::new(node_c.engine().clone())),
            )
            .await;

        // Wire Node B transports
        node_b
            .register_peer_transport(
                "node-alpha",
                Arc::new(InMemoryConsensusTransport::new(node_a.engine().clone())),
            )
            .await;
        node_b
            .register_peer_transport(
                "node-gamma",
                Arc::new(InMemoryConsensusTransport::new(node_c.engine().clone())),
            )
            .await;

        // Wire Node C transports
        node_c
            .register_peer_transport(
                "node-alpha",
                Arc::new(InMemoryConsensusTransport::new(node_a.engine().clone())),
            )
            .await;
        node_c
            .register_peer_transport(
                "node-beta",
                Arc::new(InMemoryConsensusTransport::new(node_b.engine().clone())),
            )
            .await;

        (node_a, node_b, node_c)
    }

    #[tokio::test]
    async fn test_three_node_cluster_leader_election_and_log_replication() {
        let (node_a, node_b, node_c) = setup_three_node_cluster().await;

        // 1. Leader election: Node A starts election, gathers votes from B and C
        let won = node_a
            .trigger_election()
            .await
            .expect("Trigger election on Node A");
        assert!(won, "Node A must achieve majority quorum and become leader");
        assert_eq!(node_a.role().await, RaftRole::Leader);
        assert_eq!(node_a.current_term().await, 1);

        // 2. Propose memory update through leader
        let update = AgentMemoryUpdate {
            agent_id: "agent-researcher".into(),
            node_id: "node-alpha".into(),
            key: "investigation_target".into(),
            value: "distributed_consensus_pillar_3".into(),
            timestamp: 1000,
        };

        let log_idx = node_a
            .propose_memory_update(update)
            .await
            .expect("Propose memory update");
        assert_eq!(log_idx, 1);

        // 3. Quorum commit verification across cluster
        assert_eq!(node_a.commit_index().await, 1);
        assert_eq!(node_b.commit_index().await, 1);
        assert_eq!(node_c.commit_index().await, 1);

        // 4. Memory graph state applied deterministically across all nodes
        assert_eq!(
            node_a
                .get_memory("agent-researcher", "investigation_target")
                .await
                .as_deref(),
            Some("distributed_consensus_pillar_3")
        );
        assert_eq!(
            node_b
                .get_memory("agent-researcher", "investigation_target")
                .await
                .as_deref(),
            Some("distributed_consensus_pillar_3")
        );
        assert_eq!(
            node_c
                .get_memory("agent-researcher", "investigation_target")
                .await
                .as_deref(),
            Some("distributed_consensus_pillar_3")
        );
    }

    #[tokio::test]
    async fn test_atomic_state_replication_memory_graph_and_circuit_breakers() {
        let (node_a, node_b, node_c) = setup_three_node_cluster().await;

        // Elect Node A as leader
        assert!(node_a.trigger_election().await.unwrap());

        // 1. Propose memory update
        node_a
            .propose_memory_update(AgentMemoryUpdate {
                agent_id: "agent-sentinel".into(),
                node_id: "node-alpha".into(),
                key: "mesh_health".into(),
                value: "optimal".into(),
                timestamp: 1001,
            })
            .await
            .unwrap();

        // 2. Propose vector shard allocation
        node_a
            .propose_vector_shard_allocation(VectorShardAllocation {
                shard_id: "shard-lancedb-001".into(),
                collection: "code_patches".into(),
                vector_count: 50000,
                checksum: "sha256-shard-valid".into(),
            })
            .await
            .unwrap();

        // 3. Propose circuit breaker transition
        node_a
            .propose_circuit_breaker_transition(CircuitBreakerTransition {
                breaker_id: "breaker-compiler-ffi".into(),
                state: "Open".into(),
                reason: "Memory boundary violation detected".into(),
                timestamp: 1002,
            })
            .await
            .unwrap();

        // 4. Propose autopoietic patch commit
        node_a
            .propose_autopoietic_patch_commit(AutopoieticPatchCommit {
                patch_id: "patch-ffi-sandbox".into(),
                target_component: "hgb-wasm-kernel".into(),
                patch_checksum: "sha256-patch-8899".into(),
                author_node_id: "node-alpha".into(),
                timestamp: 1003,
            })
            .await
            .unwrap();

        // Broadcast heartbeats to ensure followers apply all committed entries
        node_a.broadcast_heartbeats().await.unwrap();

        // 5. Verify atomic state consistency on all 3 nodes
        for node in &[&node_a, &node_b, &node_c] {
            assert_eq!(
                node.get_memory("agent-sentinel", "mesh_health").await.as_deref(),
                Some("optimal")
            );

            let shard = node
                .get_vector_shard("shard-lancedb-001")
                .await
                .expect("Vector shard must be replicated");
            assert_eq!(shard.vector_count, 50000);
            assert_eq!(shard.collection, "code_patches");

            let breaker = node
                .get_circuit_breaker("breaker-compiler-ffi")
                .await
                .expect("Breaker must be replicated");
            assert_eq!(breaker.state, "Open");
            assert_eq!(breaker.reason, "Memory boundary violation detected");

            let patch = node
                .get_autopoietic_patch("patch-ffi-sandbox")
                .await
                .expect("Autopoietic patch must be replicated");
            assert_eq!(patch.target_component, "hgb-wasm-kernel");
            assert_eq!(patch.patch_checksum, "sha256-patch-8899");
        }

        // Verify cryptographic checksum parity across all 3 nodes
        let chk_a = node_a.get_replicated_state().await.compute_checksum();
        let chk_b = node_b.get_replicated_state().await.compute_checksum();
        let chk_c = node_c.get_replicated_state().await.compute_checksum();
        assert_eq!(chk_a, chk_b);
        assert_eq!(chk_b, chk_c);

        // 6. Test sync to DistributedEdgeMemoryFabric
        let identity = crate::secure_transport::NodeIdentity::generate();
        let fabric = DistributedEdgeMemoryFabric::new(
            identity,
            b"secret-32b-cluster-key-for-test-".to_vec(),
        );
        let synced = node_a.sync_committed_to_edge_fabric(&fabric).await;
        assert!(synced >= 1);
        let record = fabric
            .get_record("agent-sentinel", "mesh_health")
            .await
            .unwrap();
        assert_eq!(record.relational_payload.value, serde_json::json!("optimal"));
    }

    #[tokio::test]
    async fn test_split_brain_prevention_minority_partition() {
        let config_a = ConsensusCoordinatorConfig {
            node_id: "node-alpha".to_string(),
            cluster_peers: vec!["node-beta".to_string(), "node-gamma".to_string()],
            quorum_timeout_ms: 100, // Short timeout for partition rejection test
            ..Default::default()
        };
        let config_b = ConsensusCoordinatorConfig {
            node_id: "node-beta".to_string(),
            cluster_peers: vec!["node-alpha".to_string(), "node-gamma".to_string()],
            quorum_timeout_ms: 100,
            ..Default::default()
        };
        let config_c = ConsensusCoordinatorConfig {
            node_id: "node-gamma".to_string(),
            cluster_peers: vec!["node-alpha".to_string(), "node-beta".to_string()],
            quorum_timeout_ms: 100,
            ..Default::default()
        };

        let node_a = ConsensusMediatedMeshCoordinator::new(config_a);
        let node_b = ConsensusMediatedMeshCoordinator::new(config_b);
        let node_c = ConsensusMediatedMeshCoordinator::new(config_c);

        // Setup partitionable transports
        let t_a_to_b = Arc::new(PartitionableConsensusTransport::new(Arc::new(
            InMemoryConsensusTransport::new(node_b.engine().clone()),
        )));
        let t_a_to_c = Arc::new(PartitionableConsensusTransport::new(Arc::new(
            InMemoryConsensusTransport::new(node_c.engine().clone()),
        )));

        let t_b_to_a = Arc::new(PartitionableConsensusTransport::new(Arc::new(
            InMemoryConsensusTransport::new(node_a.engine().clone()),
        )));
        let t_b_to_c = Arc::new(PartitionableConsensusTransport::new(Arc::new(
            InMemoryConsensusTransport::new(node_c.engine().clone()),
        )));

        let t_c_to_a = Arc::new(PartitionableConsensusTransport::new(Arc::new(
            InMemoryConsensusTransport::new(node_a.engine().clone()),
        )));
        let t_c_to_b = Arc::new(PartitionableConsensusTransport::new(Arc::new(
            InMemoryConsensusTransport::new(node_b.engine().clone()),
        )));

        node_a.register_peer_transport("node-beta", t_a_to_b.clone()).await;
        node_a.register_peer_transport("node-gamma", t_a_to_c.clone()).await;

        node_b.register_peer_transport("node-alpha", t_b_to_a.clone()).await;
        node_b.register_peer_transport("node-gamma", t_b_to_c.clone()).await;

        node_c.register_peer_transport("node-alpha", t_c_to_a.clone()).await;
        node_c.register_peer_transport("node-beta", t_c_to_b.clone()).await;

        // Elect Node A as initial leader
        assert!(node_a.trigger_election().await.unwrap());

        // Commit baseline entry before partition
        node_a
            .propose_memory_update(AgentMemoryUpdate {
                agent_id: "agent-1".into(),
                node_id: "node-alpha".into(),
                key: "baseline".into(),
                value: "committed_globally".into(),
                timestamp: 100,
            })
            .await
            .unwrap();

        assert_eq!(
            node_b.get_memory("agent-1", "baseline").await.as_deref(),
            Some("committed_globally")
        );

        // =====================================================================
        // Partition cluster: {Node A} (minority: 1) vs {Node B, Node C} (majority: 2)
        // =====================================================================
        t_a_to_b.set_partitioned(true).await;
        t_a_to_c.set_partitioned(true).await;
        t_b_to_a.set_partitioned(true).await;
        t_c_to_a.set_partitioned(true).await;

        // Minority partition: Node A attempts to commit an entry
        let minority_proposal = node_a
            .propose_memory_update(AgentMemoryUpdate {
                agent_id: "agent-1".into(),
                node_id: "node-alpha".into(),
                key: "split_brain_attempt".into(),
                value: "rogue_data".into(),
                timestamp: 200,
            })
            .await;

        // Must fail with QuorumNotReached!
        assert!(
            minority_proposal.is_err(),
            "Minority partition MUST NOT commit log entries without majority quorum"
        );

        // Verify rogue data is NOT applied to Node A's state machine
        assert_eq!(
            node_a.get_memory("agent-1", "split_brain_attempt").await,
            None,
            "Uncommitted proposal must never be applied to state"
        );

        // Majority partition: Node B initiates election and achieves majority quorum (2 out of 3)
        let won_b = node_b.trigger_election().await.unwrap();
        assert!(won_b, "Majority partition {{B, C}} must successfully elect Node B");
        assert_eq!(node_b.role().await, RaftRole::Leader);

        // Majority partition successfully commits new entry
        let majority_proposal = node_b
            .propose_memory_update(AgentMemoryUpdate {
                agent_id: "agent-1".into(),
                node_id: "node-beta".into(),
                key: "legitimate_progress".into(),
                value: "majority_quorum_confirmed".into(),
                timestamp: 300,
            })
            .await;

        assert!(
            majority_proposal.is_ok(),
            "Majority partition MUST successfully achieve consensus commit"
        );

        // Verify both majority nodes committed and applied the entry
        assert_eq!(
            node_b.get_memory("agent-1", "legitimate_progress").await.as_deref(),
            Some("majority_quorum_confirmed")
        );
        assert_eq!(
            node_c.get_memory("agent-1", "legitimate_progress").await.as_deref(),
            Some("majority_quorum_confirmed")
        );

        // Isolated Node A does NOT have the entry
        assert_eq!(node_a.get_memory("agent-1", "legitimate_progress").await, None);
    }

    #[tokio::test]
    async fn test_point_in_time_snapshot_synchronization_and_recovery() {
        let (node_a, _node_b, _node_c) = setup_three_node_cluster().await;

        // Elect Node A
        assert!(node_a.trigger_election().await.unwrap());

        // Commit multiple state entries
        node_a
            .propose_memory_update(AgentMemoryUpdate {
                agent_id: "agent-architect".into(),
                node_id: "node-alpha".into(),
                key: "compiler_flag".into(),
                value: "-O3".into(),
                timestamp: 500,
            })
            .await
            .unwrap();

        node_a
            .propose_circuit_breaker_transition(CircuitBreakerTransition {
                breaker_id: "breaker-eval".into(),
                state: "HalfOpen".into(),
                reason: "Probe recovery".into(),
                timestamp: 501,
            })
            .await
            .unwrap();

        node_a
            .propose_autopoietic_patch_commit(AutopoieticPatchCommit {
                patch_id: "patch-opt-01".into(),
                target_component: "bolt_optimizer".into(),
                patch_checksum: "sha256-bolt-pass".into(),
                author_node_id: "node-alpha".into(),
                timestamp: 502,
            })
            .await
            .unwrap();

        // 1. Capture point-in-time snapshot and compact log
        let snapshot = node_a
            .create_snapshot_and_compact()
            .await
            .expect("Create snapshot and compact");

        assert_eq!(snapshot.last_included_index, 3);
        assert!(snapshot.verify_checksum());

        // 2. Initialize a fresh node (Node D / "node-delta") with empty state
        let config_d = ConsensusCoordinatorConfig {
            node_id: "node-delta".to_string(),
            cluster_peers: vec!["node-alpha".to_string()],
            ..Default::default()
        };
        let node_d = ConsensusMediatedMeshCoordinator::new(config_d);

        assert_eq!(node_d.get_memory("agent-architect", "compiler_flag").await, None);
        assert_eq!(node_d.commit_index().await, 0);

        // 3. Install snapshot onto Node D
        node_d
            .install_snapshot(snapshot)
            .await
            .expect("Install snapshot onto node-delta");

        // 4. Verify Node D perfectly recovered state
        assert_eq!(node_d.commit_index().await, 3);
        assert_eq!(
            node_d.get_memory("agent-architect", "compiler_flag").await.as_deref(),
            Some("-O3")
        );
        let breaker = node_d.get_circuit_breaker("breaker-eval").await.unwrap();
        assert_eq!(breaker.state, "HalfOpen");
        let patch = node_d.get_autopoietic_patch("patch-opt-01").await.unwrap();
        assert_eq!(patch.target_component, "bolt_optimizer");

        // Cryptographic state parity between original and restored node
        let state_a = node_a.get_replicated_state().await;
        let state_d = node_d.get_replicated_state().await;
        assert_eq!(state_a.compute_checksum(), state_d.compute_checksum());
    }
}
