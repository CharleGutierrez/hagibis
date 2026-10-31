//! # Swarm Federation Daemon Background Service
//!
//! Exposes `SwarmDaemonService` running resident in `hgbd`, enabling multiple
//! Hagibis daemons across physical machines, containers, or edge nodes to federate,
//! synchronize heartbeats, and cooperatively execute compute tasks.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use hgb_core::error::{HgbError, Result};
use hgb_core::p2p_swarm_federation::{
    SwarmFederationEngine, SwarmFederationResult, SwarmFederationStatusReport,
    SwarmFederationTask, WorkerNodeAdvertisement, DEFAULT_FEDERATION_PORT,
    DEFAULT_HEARTBEAT_INTERVAL_SECS, DEFAULT_STALE_NODE_TIMEOUT_SECS,
};
use hgb_core::p2p_mcp_swarm::{
    ContextGraphNode, ContextGraphSyncReport, FederatedToolInfo, P2pMcpSwarmEngine,
    P2pMcpSwarmStatusReport, P2pSubtaskResult, P2pSubtaskSpec, P2pWorkflowResult,
    P2pWorkflowSpec, VectorShardMatch,
};

pub const DEFAULT_SWARM_PORT: u16 = DEFAULT_FEDERATION_PORT;

/// Configuration for resident Swarm Daemon Service
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SwarmDaemonConfig {
    pub port: u16,
    pub heartbeat_interval_secs: u64,
    pub stale_node_timeout_secs: i64,
    pub bootstrap_peers: Vec<SocketAddr>,
}

impl Default for SwarmDaemonConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_SWARM_PORT,
            heartbeat_interval_secs: DEFAULT_HEARTBEAT_INTERVAL_SECS,
            stale_node_timeout_secs: DEFAULT_STALE_NODE_TIMEOUT_SECS,
            bootstrap_peers: Vec::new(),
        }
    }
}

/// Status snapshot of Swarm Daemon Service
pub type SwarmServiceStatusSnapshot = SwarmFederationStatusReport;

/// Resident Swarm Daemon Service
pub struct SwarmDaemonService {
    engine: Arc<SwarmFederationEngine>,
    mcp_swarm: Arc<P2pMcpSwarmEngine>,
    listen_port: u16,
    is_running: Arc<AtomicBool>,
    tcp_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    maintenance_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl SwarmDaemonService {
    pub fn new(workspace_root: impl Into<PathBuf>, listen_port: u16) -> Result<Self> {
        let ws = workspace_root.into();
        let node_id = format!("hgbd_{}", uuid::Uuid::new_v4().simple());
        let listen_addr = format!("0.0.0.0:{}", listen_port)
            .parse::<SocketAddr>()
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        let engine = Arc::new(SwarmFederationEngine::new(node_id.clone(), listen_addr, ws.clone())?);
        let mcp_swarm = Arc::new(P2pMcpSwarmEngine::new(node_id, listen_addr, ws)?);

        Ok(Self {
            engine,
            mcp_swarm,
            listen_port,
            is_running: Arc::new(AtomicBool::new(false)),
            tcp_handle: Arc::new(Mutex::new(None)),
            maintenance_handle: Arc::new(Mutex::new(None)),
        })
    }

    pub fn port(&self) -> u16 {
        self.listen_port
    }

    pub fn engine(&self) -> &Arc<SwarmFederationEngine> {
        &self.engine
    }

    pub fn mcp_swarm(&self) -> &Arc<P2pMcpSwarmEngine> {
        &self.mcp_swarm
    }

    pub async fn start(&self) -> Result<()> {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // 1. Start TCP listener for binary framed IPC
        let tcp_join = self.engine.start_tcp_listener().await?;
        *self.tcp_handle.lock().await = Some(tcp_join);

        let mcp_join = self.mcp_swarm.start_mesh_listener().await?;
        tokio::spawn(mcp_join);

        // 2. Start background heartbeat and stale node pruning maintenance task
        let engine_clone = Arc::clone(&self.engine);
        let running_clone = Arc::clone(&self.is_running);

        let maintenance_join = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(DEFAULT_HEARTBEAT_INTERVAL_SECS));
            while running_clone.load(Ordering::Relaxed) {
                interval.tick().await;

                // A. Prune dead or stale nodes
                let pruned = engine_clone.prune_stale_nodes(DEFAULT_STALE_NODE_TIMEOUT_SECS).await;
                if pruned > 0 {
                    println!("🕸️ Swarm Federation: pruned {} inactive peer(s)", pruned);
                }

                // B. Broadcast heartbeats to all active peers
                let peers = engine_clone.list_peers().await;
                for peer in peers {
                    let eng = Arc::clone(&engine_clone);
                    tokio::spawn(async move {
                        let _ = eng.send_heartbeat_to_peer(peer.listen_addr).await;
                    });
                }
            }
        });

        *self.maintenance_handle.lock().await = Some(maintenance_join);
        Ok(())
    }

    pub async fn add_peer(&self, peer_addr: SocketAddr) -> Result<WorkerNodeAdvertisement> {
        self.engine.send_heartbeat_to_peer(peer_addr).await
    }

    pub async fn dispatch_task(&self, task: SwarmFederationTask) -> Result<SwarmFederationResult> {
        self.engine.dispatch_task(task).await
    }

    pub async fn dispatch_sharded_workflow(
        &self,
        tasks: Vec<SwarmFederationTask>,
    ) -> Result<Vec<SwarmFederationResult>> {
        self.engine.dispatch_workflow_sharded(tasks).await
    }

    pub async fn dispatch_mcp_subtask(&self, spec: P2pSubtaskSpec) -> Result<P2pSubtaskResult> {
        self.mcp_swarm.execute_subtask(spec).await
    }

    pub async fn execute_mcp_workflow(&self, workflow: P2pWorkflowSpec) -> Result<P2pWorkflowResult> {
        self.mcp_swarm.execute_workflow(workflow).await
    }

    pub async fn mcp_status(&self) -> P2pMcpSwarmStatusReport {
        self.mcp_swarm.status().await
    }

    pub async fn put_context_node(&self, node: ContextGraphNode) -> String {
        self.mcp_swarm.put_context_node(node).await
    }

    pub async fn get_context_node(&self, collection: &str, key: &str) -> Option<ContextGraphNode> {
        self.mcp_swarm.get_context_node(collection, key).await
    }

    pub async fn list_context_nodes(&self, collection: Option<&str>) -> Vec<ContextGraphNode> {
        self.mcp_swarm.list_context_nodes(collection).await
    }

    pub async fn sync_context_graph(
        &self,
        peer_addr: SocketAddr,
        collection: Option<String>,
    ) -> Result<ContextGraphSyncReport> {
        self.mcp_swarm.sync_context_graph_with_peer(peer_addr, collection).await
    }

    pub async fn query_vector_shards(&self, query: &str, top_k: usize) -> Vec<VectorShardMatch> {
        self.mcp_swarm.query_vector_shards_across_swarm(query, top_k).await
    }

    pub async fn list_federated_tools(&self) -> Vec<FederatedToolInfo> {
        self.mcp_swarm.list_federated_tools().await
    }

    pub async fn status(&self) -> SwarmFederationStatusReport {
        self.engine.status().await
    }

    pub async fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        self.engine.stop();
        self.mcp_swarm.stop();

        if let Some(handle) = self.tcp_handle.lock().await.take() {
            handle.abort();
        }
        if let Some(handle) = self.maintenance_handle.lock().await.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_swarm_daemon_service_mcp_and_context_graph() {
        let tmp = std::env::temp_dir().join(format!("hgb_swarm_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let service = SwarmDaemonService::new(&tmp, 0).unwrap();
        let mcp_status = service.mcp_status().await;
        assert!(!mcp_status.node_id.is_empty());
        assert!(!mcp_status.advertised_tools.is_empty());

        let node = ContextGraphNode {
            collection: "swarm_test".to_string(),
            key: "test_doc.md".to_string(),
            content: "Hagibis sovereign distributed swarm".to_string(),
            metadata: serde_json::json!({}),
            embedding: None,
            blake3_hash: String::new(),
            updated_at: 0,
        };

        let hash = service.put_context_node(node).await;
        assert!(!hash.is_empty());

        let fetched = service.get_context_node("swarm_test", "test_doc.md").await.unwrap();
        assert_eq!(fetched.key, "test_doc.md");

        let matches = service.query_vector_shards("sovereign", 3).await;
        assert!(!matches.is_empty());
        assert_eq!(matches[0].filepath_or_key, "test_doc.md");

        let federated = service.list_federated_tools().await;
        assert!(!federated.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
