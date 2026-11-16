//! # `hgb-mesh`: P2P Secure Node Federation & Raft Consensus for Hagibis
//!
//! Module 1 of the Hagibis Autonomous Distributed Computing Engine.
//!
//! Provides zero-mock, production-grade distributed infrastructure:
//! - **P2P Node Federation (`p2p_mesh`)**: libp2p swarm orchestration with Tokio TCP transport,
//!   Noise protocol Ed25519 encrypted handshakes, Yamux stream multiplexing, automatic local
//!   mDNS discovery (`mdns::tokio::Behaviour`), and WAN hole punching (DCUtR / relay).
//! - **MicroVM Raft Consensus (`microvm_raft_consensus`)**: Full Raft state machine handling
//!   cluster membership, leader elections, log replication, and deterministic leader-enforced
//!   quorum commits over `MicroVmStateMigration` state handoffs. Cryptographic verification of
//!   memory checkpoints using SHA-256 and Ed25519 digital signatures.
//! - **Encrypted Swarm Transport Channels (`secure_transport`)**: Authenticated inter-node RPC
//!   envelopes with RFC 2104 HMAC-SHA256, Ed25519 digital signatures, monotonic nonces for strict
//!   replay attack prevention, and multiplexed swarm channels for task gossip, telemetry, and work stealing.

pub mod error;
pub mod microvm_raft_consensus;
pub mod p2p_mesh;
pub mod secure_transport;
pub mod distributed_edge_memory_fabric;
pub mod edge_worker_crdt_sync;
pub mod quic_distributed_compute_grid;
pub mod edge_websocket_quic_federation;
pub mod distributed_vector_fabric;

// Public re-exports
pub use error::{MeshError, Result};
pub use distributed_vector_fabric::{
    AgentEpisodicMemoryCluster, DistributedRagSearchResult, EpisodicMemoryRecord,
    LanceDbShard, LanceVecHeader, MeshNodeInfo, MeshVectorShardRouter,
    MockableMeshTransport, NodeStatus as MeshNodeStatus, P2pRagQueryEngine,
    ShardRebalanceDelta, VectorFabricError, VectorFabricQuorum,
};
pub use edge_websocket_quic_federation::{
    EdgeFederationConfig, EdgeTunnelEnvelope, EdgeTunnelMessageType,
    EdgeWebSocketFederationClient, FederatedTaskRequest, FederatedTaskResult,
    HardwareCapabilityProfile, LocalTaskExecutor, TaskExecutionEngine, TunnelState,
};
pub use quic_distributed_compute_grid::{
    ComputeCapability, MemoryPoolMetrics, NatTraversalManager, NodeComputeCapacity,
    QuicDistributedComputeGrid, QuicFrame, QuicGridConfig, QuicHeader, QuicNodeSession,
    QuicPacket, QuicPacketType, QuicSessionState, RemoteMemoryBlock, StunEndpoint, StunNatType,
    StunProtocol, TargetHardware, TaskExecutionResult, TaskWorkload, WorkloadRouteDecision,
    DEFAULT_HEARTBEAT_INTERVAL_MS, HGB_QUIC_VERSION, MAX_MISSED_HEARTBEATS,
    MAX_QUIC_DATAGRAM_SIZE, STUN_MAGIC_COOKIE,
};
pub use edge_worker_crdt_sync::{
    AgentMigrationStatus, AgentRuntimeLocation, AgentStateSnapshot, EdgeWorkerCrdtSyncClient,
    LamportEventLog, MergeSummaryReport, MeshCrdtEnvelope,
};
pub use microvm_raft_consensus::{
    AppendEntriesArgs, AppendEntriesReply, MicroVmRaftEngine, MicroVmStateMigration,
    MigrationStatus, RaftLogEntry, RaftPayload, RaftRole, RequestVoteArgs, RequestVoteReply,
};
pub use p2p_mesh::{
    DiscoverySource, HgbMeshNode, HgbSwarmConfig, P2pPeerAddressBook, PeerAddressExchangeMessage,
    PeerConnectionStatus, PeerInfo,
};
pub use secure_transport::{
    compute_hmac_sha256, compute_sha256, compute_sha256_hex, verify_hmac_sha256_constant_time,
    HgbSecurityHeader, NodeIdentity, NonceValidator, SecureEnvelope, SwarmChannelType,
    MAX_TIMESTAMP_SKEW_MS,
};
pub use distributed_edge_memory_fabric::{
    DistributedEdgeMemoryFabric, EdgeMemoryWireMessage, LanceDbVectorIndex, LwwRegister,
    MemoryRecord, MergeSummary, MerkleAntiEntropyEngine, MerkleDigest, OrSet,
    MERKLE_BUCKET_COUNT,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_end_to_end_mesh_and_raft_migration_lifecycle() {
        // 1. Initialize cryptographic identities for a 3-node cluster
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();

        let node_b_id = node_b.peer_id_string();
        let node_c_id = node_c.peer_id_string();

        // 2. Initialize Raft engines
        let mut engine_a = MicroVmRaftEngine::new(node_a.clone(), vec![node_b_id.clone(), node_c_id.clone()]);
        let mut engine_b = MicroVmRaftEngine::new(node_b.clone(), vec![engine_a.node_id.clone(), node_c_id.clone()]);

        // 3. Leader Election: Node A starts election, gathers vote from Node B
        let (vote_req, _) = engine_a.start_election();
        let vote_reply_b = engine_b.handle_request_vote(vote_req);
        assert!(vote_reply_b.vote_granted);

        let leader_elected = engine_a.handle_vote_reply(&node_b_id, vote_reply_b);
        assert!(leader_elected, "Node A must achieve majority quorum and become leader");
        assert_eq!(engine_a.role, RaftRole::Leader);

        // 4. Secure Transport Envelope: Node B sends work-stealing message to Node A
        let shared_cluster_secret = b"hagibis-production-shared-secret-key-32b";
        let mut nonce_validator_a = NonceValidator::new();

        let envelope = SecureEnvelope::seal(
            &node_b,
            shared_cluster_secret,
            &engine_a.node_id,
            SwarmChannelType::MicroVmWorkStealing,
            1,
            b"{\"request\":\"steal_vm\",\"target_load\":0.15}",
        )
        .expect("Seal work stealing envelope");

        let unwrapped = envelope
            .verify_and_open(shared_cluster_secret, &node_b.public_key(), &mut nonce_validator_a)
            .expect("Unwrap and verify work stealing envelope");
        assert!(!unwrapped.is_empty());

        // 5. Leader proposes MicroVM live state migration
        let ram_data = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let migration = MicroVmStateMigration::new(&node_a, &node_b_id, "vm-agent-ai-01", &ram_data)
            .expect("Create microvm state migration");
        let migration_id = migration.migration_id.clone();

        // Propose to Raft log
        let log_idx = engine_a.propose_migration(migration).expect("Propose migration");
        assert_eq!(log_idx, 1);

        // RAM handoff MUST fail prior to quorum commit
        assert!(
            engine_a.initiate_ram_handoff(&migration_id).is_err(),
            "Cannot handoff RAM before quorum commit"
        );

        // 6. Replicate log entry to Follower Node B
        let append_args = engine_a.build_append_entries_for_peer(&node_b_id).unwrap();
        let append_reply_b = engine_b.handle_append_entries(append_args);
        assert!(append_reply_b.success);

        // 7. Leader processes replication ACK from Node B -> Quorum Commit Reached (2/3)!
        let committed_ids = engine_a
            .handle_append_entries_reply(&node_b_id, append_reply_b)
            .expect("Commit evaluation");
        assert_eq!(committed_ids, vec![migration_id.clone()]);
        assert_eq!(engine_a.commit_index, 1);

        // 8. Now Leader authorizes RAM state handoff
        let authorized_migration = engine_a
            .initiate_ram_handoff(&migration_id)
            .expect("Initiate RAM handoff must succeed after quorum commit");
        assert_eq!(authorized_migration.status, MigrationStatus::HandoffInitiated);

        // 9. Target Node B verifies cryptographic integrity of RAM checkpoint & signature
        authorized_migration
            .verify_integrity(&node_a.public_key())
            .expect("Cryptographic verification of state checkpoint must succeed");
    }

    #[tokio::test]
    async fn test_edge_worker_crdt_sync_integration() {
        let client = EdgeWorkerCrdtSyncClient::new("local-daemon-alpha", b"edge-secret-32b");
        let active_nodes = client.active_nodes().await;
        assert!(active_nodes.contains(&"local-daemon-alpha".to_string()));

        let envelope = client.export_sync_envelope("cloudflare_edge").await;
        assert!(envelope.verify(b"edge-secret-32b"));
    }
}
