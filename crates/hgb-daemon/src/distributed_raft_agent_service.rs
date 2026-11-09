//! # Distributed Raft Agent Service (`distributed_raft_agent_service`)
//!
//! Systems-level daemon service managing cluster peer discovery, Raft consensus heartbeats,
//! vector memory synchronization, and decentralized multi-agent workflow scheduling across nodes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hgb_core::error::{HgbError, Result};
use hgb_nextgen::p2p_raft_mesh_network::{
    DistributedWorkflowPlan, DistributedWorkflowStep, MeshPeerIdentity,
    P2pRaftMeshNetwork, PeerNodeTelemetry, RaftClusterCommand, RaftRole,
};

/// Service configuration for the distributed agent cluster
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedRaftServiceConfig {
    pub node_id: String,
    pub listen_addr: String,
    pub cluster_peers: Vec<String>,
    pub heartbeat_interval_ms: u64,
    pub vector_sync_interval_ms: u64,
}

impl Default for DistributedRaftServiceConfig {
    fn default() -> Self {
        Self {
            node_id: format!("hgb-node-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            listen_addr: "127.0.0.1:9040".to_string(),
            cluster_peers: Vec::new(),
            heartbeat_interval_ms: 50,
            vector_sync_interval_ms: 100,
        }
    }
}

/// The Daemon-side Distributed Raft Agent Orchestration Service
pub struct DistributedRaftAgentService {
    config: DistributedRaftServiceConfig,
    mesh_network: Arc<P2pRaftMeshNetwork>,
    is_running: Arc<RwLock<bool>>,
}

impl DistributedRaftAgentService {
    /// Initialize the daemon service with configuration
    pub fn new(config: DistributedRaftServiceConfig) -> Self {
        let mesh_network = Arc::new(P2pRaftMeshNetwork::new(
            config.node_id.clone(),
            config.listen_addr.clone(),
        ));

        Self {
            config,
            mesh_network,
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Retrieve reference to underlying mesh network
    pub fn mesh_network(&self) -> Arc<P2pRaftMeshNetwork> {
        self.mesh_network.clone()
    }

    /// Start the agent service background loop
    pub async fn start(&self) -> Result<()> {
        let mut running = self.is_running.write().await;
        *running = true;
        Ok(())
    }

    /// Stop the service
    pub async fn stop(&self) -> Result<()> {
        let mut running = self.is_running.write().await;
        *running = false;
        Ok(())
    }

    /// Propose a vector sync command across cluster
    pub fn replicate_vector_memory(
        &self,
        record_id: String,
        partition: String,
        vector: Vec<f32>,
        text: String,
        metadata: Value,
    ) -> Result<()> {
        self.mesh_network
            .enqueue_vector_sync(record_id, partition, vector, text, metadata)
    }

    /// Dispatch a multi-agent workflow DAG
    pub fn dispatch_workflow(
        &self,
        title: &str,
        steps: Vec<DistributedWorkflowStep>,
    ) -> Result<DistributedWorkflowPlan> {
        self.mesh_network.create_workflow_plan(title, steps)
    }

    /// Get real-time cluster telemetry snapshot
    pub fn get_telemetry(&self) -> Value {
        self.mesh_network.cluster_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_distributed_raft_agent_service_lifecycle() {
        let config = DistributedRaftServiceConfig {
            node_id: "test-node-1".into(),
            listen_addr: "127.0.0.1:9050".into(),
            cluster_peers: vec![],
            heartbeat_interval_ms: 20,
            vector_sync_interval_ms: 50,
        };

        let service = DistributedRaftAgentService::new(config);
        service.start().await.unwrap();

        // Enqueue vector memory
        service
            .replicate_vector_memory(
                "v1".into(),
                "default".into(),
                vec![0.5, 0.5],
                "sample prompt".into(),
                serde_json::json!({}),
            )
            .unwrap();

        let telemetry = service.get_telemetry();
        assert_eq!(telemetry["local_node_id"], "test-node-1");

        service.stop().await.unwrap();
    }
}
