//! # Distributed P2P Agent Swarms via Model Context Protocol (MCP)
//!
//! Production-grade P2P compute mesh enabling autonomous Hagibis nodes to:
//! - Advertise and discover registered sovereign MCP tools across the swarm mesh (port 8970).
//! - Negotiate dynamic sub-task delegation for complex multi-step agent prompts across optimal peers
//!   (e.g. Node A handles vector search, Node B handles WASM execution, Node C handles local Ollama LLM inference).
//! - Zero-copy cached state sharing: exchange intermediate and terminal tool outputs content-addressed
//!   by cryptographic BLAKE3 hash.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

use crate::error::{HgbError, Result};
use crate::mcp_server::HagibisMcpServer;
use crate::p2p_swarm_federation::{read_free_ram_mb, SWARM_FRAME_MAGIC};

pub const DEFAULT_P2P_MCP_PORT: u16 = 8970;
pub const DEFAULT_UDP_DISCOVERY_PORT: u16 = 8970;
pub const P2P_DISCOVERY_MAGIC: u32 = 0x48474253; // 'HGBS'
pub const DEFAULT_MCP_HEARTBEAT_SECS: u64 = 5;
pub const DEFAULT_PEER_TIMEOUT_SECS: i64 = 15;
pub const P2P_OVERLOAD_LOAD_SCORE: f32 = 0.70;

/// UDP discovery beacon broadcasted across local network nodes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pDiscoveryBeacon {
    pub magic: u32,
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub load_score: f32,
    pub available_cpu_cores: usize,
    pub free_ram_mb: u64,
    pub available_tools: Vec<String>,
    pub timestamp_secs: i64,
}

/// Context document or semantic node replicated in distributed Hagibis context graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextGraphNode {
    pub collection: String,
    pub key: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub embedding: Option<Vec<f32>>,
    pub blake3_hash: String,
    pub updated_at: i64,
}

/// Outcome report of cross-peer context graph synchronization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextGraphSyncReport {
    pub collection: String,
    pub replicated_nodes: usize,
    pub source_node_id: String,
    pub bytes_transferred: usize,
    pub duration_ms: u64,
}

/// Result match from a vector embedding shard search across the swarm mesh
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorShardMatch {
    pub id: String,
    pub filepath_or_key: String,
    pub chunk_index: usize,
    pub content_snippet: String,
    pub blake3_hash: String,
    pub score: f32,
    pub source_node_id: String,
}

/// Federated MCP tool advertising provider details across swarm nodes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederatedToolInfo {
    pub tool: P2pMcpToolAdvertisement,
    pub provider_node_id: String,
    pub provider_addr: SocketAddr,
    pub node_load_score: f32,
    pub is_local: bool,
}

/// High-level RPC and peer coordination messages for P2P MCP federation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum P2pMcpMessage {
    ContextGraphSync {
        collection: Option<String>,
        since_timestamp: Option<i64>,
        limit: Option<usize>,
        payload: Option<Vec<ContextGraphNode>>,
    },
    ContextGraphSyncResponse {
        nodes: Vec<ContextGraphNode>,
        total_count: usize,
        source_node_id: String,
        success: bool,
    },
    VectorShardQuery {
        query: String,
        top_k: usize,
        threshold: Option<f32>,
    },
    VectorShardQueryResponse {
        matches: Vec<VectorShardMatch>,
        total_searched: usize,
        source_node_id: String,
        success: bool,
    },
    DelegateToolExecution {
        subtask: P2pSubtaskSpec,
        input_data: Option<Vec<u8>>,
    },
    DelegateToolExecutionResponse {
        result: P2pSubtaskResult,
    },
    ToolFederationList,
    ToolFederationListResponse {
        tools: Vec<P2pMcpToolAdvertisement>,
        node_id: String,
    },
}

/// Registered MCP tool descriptor advertised across the swarm mesh
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pMcpToolAdvertisement {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub execution_tier: String,
    pub required_capability: Option<String>,
    pub cost_factor: f32,
}

/// Node telemetry and advertised MCP tool registry across the P2P mesh
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pMcpNodeAdvertisement {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub available_cpu_cores: usize,
    pub free_ram_mb: u64,
    pub load_score: f32,
    pub has_firecracker_kvm: bool,
    pub has_wasm_engine: bool,
    pub has_ollama: bool,
    pub advertised_tools: Vec<P2pMcpToolAdvertisement>,
    pub last_heartbeat: DateTime<Utc>,
}

impl P2pMcpNodeAdvertisement {
    /// Construct local worker advertisement populated with mandatory core MCP tools
    pub fn new(node_id: impl Into<String>, listen_addr: SocketAddr) -> Self {
        let node_id = node_id.into();
        let available_cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let free_ram_mb = read_free_ram_mb();
        let has_firecracker_kvm = crate::microvm_sandbox_executor::FirecrackerEphemeralSandbox::is_kvm_accessible();
        let has_wasm_engine = true;
        let has_ollama = crate::providers::OllamaProvider::is_available();

        Self {
            node_id,
            listen_addr,
            available_cpu_cores,
            free_ram_mb,
            load_score: 0.10,
            has_firecracker_kvm,
            has_wasm_engine,
            has_ollama,
            advertised_tools: Self::default_registered_tools(),
            last_heartbeat: Utc::now(),
        }
    }

    /// Default registered sovereign tools mandated by Hagibis MCP federation specification
    pub fn default_registered_tools() -> Vec<P2pMcpToolAdvertisement> {
        vec![
            P2pMcpToolAdvertisement {
                name: "hgb_sqlite_query".to_string(),
                description: "Execute structured SQL query (SELECT, PRAGMA) against SQLite database".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "database_path": { "type": "string" }
                    },
                    "required": ["query"]
                }),
                execution_tier: "sqlite".to_string(),
                required_capability: None,
                cost_factor: 1.0,
            },
            P2pMcpToolAdvertisement {
                name: "hgb_wasm_wit_exec".to_string(),
                description: "Execute sandboxed WebAssembly WIT tasks through Wasmtime with fuel metering".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string" },
                        "payload": { "type": "object" },
                        "wasm_path": { "type": "string" }
                    },
                    "required": ["task"]
                }),
                execution_tier: "wasm".to_string(),
                required_capability: Some("wasm".to_string()),
                cost_factor: 1.5,
            },
            P2pMcpToolAdvertisement {
                name: "hgb_system_health".to_string(),
                description: "Real-time system telemetry: CPU temp, freq, governor, RAM free/used, MGLRU".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "detailed": { "type": "boolean" }
                    }
                }),
                execution_tier: "system".to_string(),
                required_capability: None,
                cost_factor: 0.5,
            },
            P2pMcpToolAdvertisement {
                name: "hgb_cgroup_control".to_string(),
                description: "Inspect and adjust cgroup v2 resource limits for sandboxed execution".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["inspect", "adjust"] },
                        "cpu_max": { "type": "string" },
                        "memory_max": { "type": "string" }
                    }
                }),
                execution_tier: "cgroup".to_string(),
                required_capability: None,
                cost_factor: 1.0,
            },
            P2pMcpToolAdvertisement {
                name: "hgb_microvm_status".to_string(),
                description: "Inspect Firecracker microVM hardware virtualization support (/dev/kvm)".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "detailed": { "type": "boolean" }
                    }
                }),
                execution_tier: "microvm".to_string(),
                required_capability: Some("kvm".to_string()),
                cost_factor: 2.0,
            },
            P2pMcpToolAdvertisement {
                name: "hgb_ollama_vector_query".to_string(),
                description: "Search semantically nearest code chunks from local SQLite mutation vector mesh".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "top_k": { "type": "number" }
                    },
                    "required": ["query"]
                }),
                execution_tier: "vector_search".to_string(),
                required_capability: None,
                cost_factor: 1.2,
            },
            P2pMcpToolAdvertisement {
                name: "ollama_speculative_generate".to_string(),
                description: "Speculative Decoding LLM inference using fast draft model validated by target model".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string" },
                        "draft_model": { "type": "string" },
                        "target_model": { "type": "string" }
                    },
                    "required": ["prompt"]
                }),
                execution_tier: "ollama".to_string(),
                required_capability: Some("ollama".to_string()),
                cost_factor: 3.0,
            },
        ]
    }

    /// Check if this node advertises a specific MCP tool
    pub fn supports_tool(&self, tool_name: &str) -> bool {
        self.advertised_tools.iter().any(|t| t.name == tool_name)
    }

    /// Determine if this node can satisfy a subtask specification
    pub fn can_handle(&self, subtask: &P2pSubtaskSpec) -> bool {
        if !self.supports_tool(&subtask.tool_name) {
            return false;
        }
        if let Some(ref cap) = subtask.required_capability {
            match cap.as_str() {
                "kvm" => self.has_firecracker_kvm,
                "wasm" => self.has_wasm_engine,
                "ollama" => self.has_ollama,
                _ => true,
            }
        } else {
            true
        }
    }

    pub fn is_stale(&self, timeout_secs: i64) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_heartbeat);
        elapsed.num_seconds() > timeout_secs
    }

    pub fn refresh_telemetry(&mut self, active_tasks: usize) {
        self.free_ram_mb = read_free_ram_mb();
        self.available_cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        self.last_heartbeat = Utc::now();
        let task_factor = (active_tasks as f32) * 0.15;
        let mem_factor = if self.free_ram_mb < 512 {
            0.50
        } else if self.free_ram_mb < 1024 {
            0.25
        } else {
            0.05
        };
        self.load_score = (task_factor + mem_factor).clamp(0.05, 1.50);
    }
}

/// Zero-copy content-addressed state store indexed by cryptographic BLAKE3 hash
pub struct P2pContentCache {
    store: RwLock<HashMap<String, Vec<u8>>>,
    cache_dir: Option<PathBuf>,
}

impl P2pContentCache {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        if let Some(ref dir) = cache_dir {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            store: RwLock::new(HashMap::new()),
            cache_dir,
        }
    }

    /// Store raw payload into content cache, returning its BLAKE3 hex hash
    pub async fn put(&self, data: &[u8]) -> String {
        let hash = blake3::hash(data).to_hex().to_string();
        {
            let mut store = self.store.write().await;
            store.insert(hash.clone(), data.to_vec());
        }

        if let Some(ref dir) = self.cache_dir {
            let file_path = dir.join(format!("state_{}.bin", hash));
            let _ = std::fs::write(file_path, data);
        }

        hash
    }

    /// Retrieve content by BLAKE3 hash from memory or disk
    pub async fn get(&self, hash: &str) -> Option<Vec<u8>> {
        {
            let store = self.store.read().await;
            if let Some(data) = store.get(hash) {
                return Some(data.clone());
            }
        }

        if let Some(ref dir) = self.cache_dir {
            let file_path = dir.join(format!("state_{}.bin", hash));
            if file_path.exists() {
                if let Ok(data) = std::fs::read(&file_path) {
                    let mut store = self.store.write().await;
                    store.insert(hash.to_string(), data.clone());
                    return Some(data);
                }
            }
        }

        None
    }

    pub async fn contains(&self, hash: &str) -> bool {
        let store = self.store.read().await;
        store.contains_key(hash)
    }

    pub async fn len(&self) -> usize {
        let store = self.store.read().await;
        store.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Specification for an individual sub-task step within a complex multi-node agent workflow
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pSubtaskSpec {
    pub step_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub input_state_hash: Option<String>,
    pub required_capability: Option<String>,
    pub timeout_ms: u64,
}

/// A composite multi-step agent workflow spec requiring dynamic cross-node orchestration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pWorkflowSpec {
    pub workflow_id: String,
    pub prompt: String,
    pub steps: Vec<P2pSubtaskSpec>,
}

/// Routing decision determining where a sub-task will execute
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SubtaskDelegationTarget {
    LocalExecution { reason: String },
    RemoteDelegation { target_node: P2pMcpNodeAdvertisement, reason: String },
}

/// Coordinated delegation plan mapping every step of a workflow to optimal peers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubtaskNegotiationPlan {
    pub workflow_id: String,
    pub step_delegations: Vec<(String, SubtaskDelegationTarget)>,
}

/// Structured outcome of a single delegated MCP subtask execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pSubtaskResult {
    pub step_id: String,
    pub tool_name: String,
    pub executing_node_id: String,
    pub is_remote: bool,
    pub success: bool,
    pub output_json: serde_json::Value,
    pub output_state_hash: String,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// Result of complete multi-step workflow execution across the swarm mesh
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pWorkflowResult {
    pub workflow_id: String,
    pub success: bool,
    pub step_results: Vec<P2pSubtaskResult>,
    pub total_duration_ms: u64,
    pub final_state_hash: String,
}

/// P2P Binary Frame Network Packet (framed with SWARM_FRAME_MAGIC)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum P2pMcpPacket {
    Ping { from_node_id: String },
    Pong { node: P2pMcpNodeAdvertisement },
    Heartbeat(P2pMcpNodeAdvertisement),
    ToolDiscoveryRequest,
    ToolDiscoveryResponse { tools: Vec<P2pMcpToolAdvertisement>, node_id: String },
    ExecuteSubtask { subtask: P2pSubtaskSpec, input_data: Option<Vec<u8>> },
    SubtaskExecutionResult(P2pSubtaskResult),
    FetchStateRequest { blake3_hash: String },
    FetchStateResponse { blake3_hash: String, found: bool, data: Option<Vec<u8>> },
    Message(P2pMcpMessage),
}

/// Telemetry status snapshot of the P2P MCP Swarm Engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P2pMcpSwarmStatusReport {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub active_peers_count: usize,
    pub active_peers: Vec<P2pMcpNodeAdvertisement>,
    pub advertised_tools_count: usize,
    pub advertised_tools: Vec<String>,
    pub local_load_score: f32,
    pub local_free_ram_mb: u64,
    pub cached_state_entries: usize,
    pub total_subtasks_delegated: u64,
    pub total_subtasks_local: u64,
    pub total_cache_hits: u64,
    pub total_cache_misses: u64,
}

/// Distributed P2P Agent Swarm Engine via Model Context Protocol (MCP)
pub struct P2pMcpSwarmEngine {
    node_id: String,
    listen_addr: SocketAddr,
    workspace_root: PathBuf,
    local_ad: Arc<RwLock<P2pMcpNodeAdvertisement>>,
    peer_registry: Arc<RwLock<HashMap<String, P2pMcpNodeAdvertisement>>>,
    content_cache: Arc<P2pContentCache>,
    mcp_server: Arc<HagibisMcpServer>,
    context_graph: Arc<RwLock<HashMap<String, ContextGraphNode>>>,
    is_running: Arc<AtomicBool>,
    total_subtasks_delegated: AtomicU64,
    total_subtasks_local: AtomicU64,
    total_cache_hits: AtomicU64,
    total_cache_misses: AtomicU64,
}

impl P2pMcpSwarmEngine {
    pub fn new(
        node_id: impl Into<String>,
        listen_addr: SocketAddr,
        workspace_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let node_id = node_id.into();
        let ws = workspace_root.into();
        let local_ad = Arc::new(RwLock::new(P2pMcpNodeAdvertisement::new(
            node_id.clone(),
            listen_addr,
        )));
        let peer_registry = Arc::new(RwLock::new(HashMap::new()));
        let cache_dir = ws.join(".hgb").join("swarm_state_cache");
        let content_cache = Arc::new(P2pContentCache::new(Some(cache_dir)));
        let mcp_server = Arc::new(HagibisMcpServer::new(&ws));
        let context_graph = Arc::new(RwLock::new(HashMap::new()));

        Ok(Self {
            node_id,
            listen_addr,
            workspace_root: ws,
            local_ad,
            peer_registry,
            content_cache,
            mcp_server,
            context_graph,
            is_running: Arc::new(AtomicBool::new(false)),
            total_subtasks_delegated: AtomicU64::new(0),
            total_subtasks_local: AtomicU64::new(0),
            total_cache_hits: AtomicU64::new(0),
            total_cache_misses: AtomicU64::new(0),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    pub fn content_cache(&self) -> &Arc<P2pContentCache> {
        &self.content_cache
    }

    pub async fn register_peer(&self, ad: P2pMcpNodeAdvertisement) {
        if ad.node_id == self.node_id {
            return;
        }
        let mut reg = self.peer_registry.write().await;
        reg.insert(ad.node_id.clone(), ad);
    }

    pub async fn remove_peer(&self, node_id: &str) -> Option<P2pMcpNodeAdvertisement> {
        let mut reg = self.peer_registry.write().await;
        reg.remove(node_id)
    }

    pub async fn list_peers(&self) -> Vec<P2pMcpNodeAdvertisement> {
        let reg = self.peer_registry.read().await;
        reg.values().cloned().collect()
    }

    pub async fn prune_stale_peers(&self, timeout_secs: i64) -> usize {
        let mut reg = self.peer_registry.write().await;
        let before = reg.len();
        reg.retain(|_, ad| !ad.is_stale(timeout_secs));
        before - reg.len()
    }

    pub async fn local_advertisement(&self) -> P2pMcpNodeAdvertisement {
        let mut ad = self.local_ad.write().await;
        let active = self.total_subtasks_local.load(Ordering::Relaxed) as usize;
        ad.refresh_telemetry(active);
        ad.clone()
    }

    /// Discover which active peer nodes advertise support for a given MCP tool
    pub async fn discover_peers_for_tool(&self, tool_name: &str) -> Vec<P2pMcpNodeAdvertisement> {
        let peers = self.list_peers().await;
        peers
            .into_iter()
            .filter(|p| !p.is_stale(DEFAULT_PEER_TIMEOUT_SECS) && p.supports_tool(tool_name))
            .collect()
    }

    /// Dynamic Sub-task Negotiation: evaluates whether to execute locally or delegate to optimal peer
    pub async fn negotiate_subtask(&self, spec: &P2pSubtaskSpec) -> SubtaskDelegationTarget {
        let local_ad = self.local_advertisement().await;

        // If local node cannot handle requirements (e.g. requires Ollama but local doesn't have it)
        if !local_ad.can_handle(spec) {
            let candidates = self.discover_peers_for_tool(&spec.tool_name).await;
            let mut eligible: Vec<P2pMcpNodeAdvertisement> = candidates
                .into_iter()
                .filter(|p| p.can_handle(spec))
                .collect();

            if let Some(best) = select_optimal_peer(&mut eligible) {
                return SubtaskDelegationTarget::RemoteDelegation {
                    target_node: best.clone(),
                    reason: format!(
                        "Local node lacks capability for tool '{}'; delegating to peer {} ({:.2} load, {} MB free RAM)",
                        spec.tool_name, best.node_id, best.load_score, best.free_ram_mb
                    ),
                };
            } else {
                return SubtaskDelegationTarget::LocalExecution {
                    reason: format!("Local node missing capability but no active capable peers found; attempting local fallback"),
                };
            }
        }

        // If local node load is nominal, run locally
        if local_ad.load_score < P2P_OVERLOAD_LOAD_SCORE {
            return SubtaskDelegationTarget::LocalExecution {
                reason: format!("Local node load ({:.2}) is nominal (< {:.2})", local_ad.load_score, P2P_OVERLOAD_LOAD_SCORE),
            };
        }

        // If local node is overloaded, seek lighter remote peer
        let candidates = self.discover_peers_for_tool(&spec.tool_name).await;
        let mut eligible: Vec<P2pMcpNodeAdvertisement> = candidates
            .into_iter()
            .filter(|p| p.can_handle(spec))
            .collect();

        if let Some(best) = select_optimal_peer(&mut eligible) {
            if best.load_score < local_ad.load_score {
                SubtaskDelegationTarget::RemoteDelegation {
                    target_node: best.clone(),
                    reason: format!(
                        "Local node overloaded ({:.2}); delegating to peer {} ({:.2} load)",
                        local_ad.load_score, best.node_id, best.load_score
                    ),
                }
            } else {
                SubtaskDelegationTarget::LocalExecution {
                    reason: "Remote peers are equally or more loaded than local node".to_string(),
                }
            }
        } else {
            SubtaskDelegationTarget::LocalExecution {
                reason: "Local node is loaded but no eligible remote peers are available".to_string(),
            }
        }
    }

    /// Construct a complete multi-step subtask delegation plan for a workflow
    pub async fn plan_workflow(&self, workflow: &P2pWorkflowSpec) -> SubtaskNegotiationPlan {
        let mut step_delegations = Vec::with_capacity(workflow.steps.len());
        for step in &workflow.steps {
            let decision = self.negotiate_subtask(step).await;
            step_delegations.push((step.step_id.clone(), decision));
        }
        SubtaskNegotiationPlan {
            workflow_id: workflow.workflow_id.clone(),
            step_delegations,
        }
    }

    /// Execute a single sub-task locally or remotely based on dynamic negotiation
    pub async fn execute_subtask(&self, spec: P2pSubtaskSpec) -> Result<P2pSubtaskResult> {
        let target = self.negotiate_subtask(&spec).await;
        match target {
            SubtaskDelegationTarget::LocalExecution { .. } => {
                self.total_subtasks_local.fetch_add(1, Ordering::Relaxed);
                let input_data = if let Some(ref hash) = spec.input_state_hash {
                    self.content_cache.get(hash).await
                } else {
                    None
                };
                self.execute_local_subtask(&spec, input_data).await
            }
            SubtaskDelegationTarget::RemoteDelegation { target_node, reason } => {
                self.total_subtasks_delegated.fetch_add(1, Ordering::Relaxed);
                let input_data = if let Some(ref hash) = spec.input_state_hash {
                    self.content_cache.get(hash).await
                } else {
                    None
                };
                match self.dispatch_remote_subtask(&target_node, &spec, input_data).await {
                    Ok(res) => Ok(res),
                    Err(e) => {
                        eprintln!(
                            "⚠️ P2P MCP remote delegation to {} failed: {}. Falling back to local execution. Reason: {}",
                            target_node.listen_addr, e, reason
                        );
                        let fallback_input = if let Some(ref hash) = spec.input_state_hash {
                            self.content_cache.get(hash).await
                        } else {
                            None
                        };
                        let mut local_res = self.execute_local_subtask(&spec, fallback_input).await?;
                        local_res.error = Some(format!("Fallback from failed peer {}: {}", target_node.node_id, e));
                        Ok(local_res)
                    }
                }
            }
        }
    }

    /// Execute an entire multi-step agent workflow with zero-copy cached state chaining across nodes
    pub async fn execute_workflow(&self, workflow: P2pWorkflowSpec) -> Result<P2pWorkflowResult> {
        let start = Instant::now();
        let mut step_results = Vec::new();
        let mut last_state_hash = String::new();

        for mut step in workflow.steps {
            // Chain state from previous step if not explicitly provided
            if step.input_state_hash.is_none() && !last_state_hash.is_empty() {
                step.input_state_hash = Some(last_state_hash.clone());
            }

            let result = self.execute_subtask(step).await?;
            last_state_hash = result.output_state_hash.clone();
            step_results.push(result);
        }

        let total_duration_ms = start.elapsed().as_millis() as u64;
        let success = step_results.iter().all(|r| r.success);

        Ok(P2pWorkflowResult {
            workflow_id: workflow.workflow_id,
            success,
            step_results,
            total_duration_ms,
            final_state_hash: last_state_hash,
        })
    }

    /// Execute subtask locally via Hagibis MCP Server and store output in BLAKE3 cache
    pub async fn execute_local_subtask(
        &self,
        spec: &P2pSubtaskSpec,
        _input_data: Option<Vec<u8>>,
    ) -> Result<P2pSubtaskResult> {
        let start = Instant::now();
        let tool_result_str = self.mcp_server.execute_tool(&spec.tool_name, &spec.arguments).await?;

        let output_json = serde_json::from_str::<serde_json::Value>(&tool_result_str)
            .unwrap_or_else(|_| serde_json::json!({ "result": tool_result_str }));

        let raw_bytes = tool_result_str.as_bytes();
        let output_state_hash = self.content_cache.put(raw_bytes).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(P2pSubtaskResult {
            step_id: spec.step_id.clone(),
            tool_name: spec.tool_name.clone(),
            executing_node_id: self.node_id.clone(),
            is_remote: false,
            success: true,
            output_json,
            output_state_hash,
            duration_ms,
            error: None,
        })
    }

    /// Dispatch subtask to a remote peer node over TCP binary framing
    pub async fn dispatch_remote_subtask(
        &self,
        target: &P2pMcpNodeAdvertisement,
        spec: &P2pSubtaskSpec,
        input_data: Option<Vec<u8>>,
    ) -> Result<P2pSubtaskResult> {
        let packet = P2pMcpPacket::ExecuteSubtask {
            subtask: spec.clone(),
            input_data,
        };
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(spec.timeout_ms.max(3000)),
            TcpStream::connect(target.listen_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout to MCP peer {}", target.listen_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect to MCP peer {}: {}", target.listen_addr, e)))?;

        send_framed_packet(&mut stream, &payload).await?;
        let resp_payload = receive_framed_packet(&mut stream).await?;

        let resp_packet: P2pMcpPacket = bincode::deserialize(&resp_payload)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match resp_packet {
            P2pMcpPacket::SubtaskExecutionResult(mut res) => {
                res.is_remote = true;
                if !res.output_state_hash.is_empty() {
                    let json_bytes = res.output_json.to_string().into_bytes();
                    let _ = self.content_cache.put(&json_bytes).await;
                }
                Ok(res)
            }
            other => Err(HgbError::Execution(format!("Unexpected packet from MCP peer: {:?}", other))),
        }
    }

    /// Fetch cached state chunk by BLAKE3 hash from a peer node
    pub async fn fetch_state_from_peer(&self, peer_addr: SocketAddr, hash: &str) -> Result<Vec<u8>> {
        let packet = P2pMcpPacket::FetchStateRequest {
            blake3_hash: hash.to_string(),
        };
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(3000),
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout to fetch state from {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Connect error: {}", e)))?;

        send_framed_packet(&mut stream, &payload).await?;
        let resp_payload = receive_framed_packet(&mut stream).await?;

        let resp: P2pMcpPacket = bincode::deserialize(&resp_payload)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match resp {
            P2pMcpPacket::FetchStateResponse { blake3_hash, found: true, data: Some(data) } => {
                let actual_hash = blake3::hash(&data).to_hex().to_string();
                if actual_hash != blake3_hash {
                    return Err(HgbError::Execution(format!("BLAKE3 integrity check failed: expected {}, got {}", blake3_hash, actual_hash)));
                }
                self.content_cache.put(&data).await;
                self.total_cache_hits.fetch_add(1, Ordering::Relaxed);
                Ok(data)
            }
            P2pMcpPacket::FetchStateResponse { found: false, .. } => {
                self.total_cache_misses.fetch_add(1, Ordering::Relaxed);
                Err(HgbError::Execution(format!("State hash {} not found on peer {}", hash, peer_addr)))
            }
            _ => Err(HgbError::Execution("Invalid state fetch response".to_string())),
        }
    }

    /// Start resident TCP server for P2P MCP Mesh
    pub async fn start_mesh_listener(self: &Arc<Self>) -> Result<tokio::task::JoinHandle<()>> {
        let listener = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            HgbError::Execution(format!("Failed to bind P2P MCP swarm listener on {}: {}", self.listen_addr, e))
        })?;

        self.is_running.store(true, Ordering::SeqCst);
        let engine = Arc::clone(self);

        let handle = tokio::spawn(async move {
            while engine.is_running.load(Ordering::Relaxed) {
                match listener.accept().await {
                    Ok((mut stream, peer_addr)) => {
                        let eng = Arc::clone(&engine);
                        tokio::spawn(async move {
                            if let Err(e) = eng.handle_inbound_stream(&mut stream, peer_addr).await {
                                eprintln!("P2P MCP peer stream error {}: {}", peer_addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        if !engine.is_running.load(Ordering::Relaxed) {
                            break;
                        }
                        eprintln!("P2P MCP listener accept error: {}", e);
                    }
                }
            }
        });

        Ok(handle)
    }

    async fn handle_inbound_stream(&self, stream: &mut TcpStream, _peer_addr: SocketAddr) -> Result<()> {
        let payload = receive_framed_packet(stream).await?;
        let packet: P2pMcpPacket = bincode::deserialize(&payload)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match packet {
            P2pMcpPacket::Ping { .. } => {
                let local_ad = self.local_advertisement().await;
                let reply = P2pMcpPacket::Pong { node: local_ad };
                let reply_bytes = bincode::serialize(&reply).map_err(|e| HgbError::Execution(e.to_string()))?;
                send_framed_packet(stream, &reply_bytes).await?;
            }
            P2pMcpPacket::Heartbeat(ad) => {
                self.register_peer(ad).await;
                let local_ad = self.local_advertisement().await;
                let reply = P2pMcpPacket::Heartbeat(local_ad);
                let reply_bytes = bincode::serialize(&reply).map_err(|e| HgbError::Execution(e.to_string()))?;
                send_framed_packet(stream, &reply_bytes).await?;
            }
            P2pMcpPacket::ToolDiscoveryRequest => {
                let local_ad = self.local_advertisement().await;
                let reply = P2pMcpPacket::ToolDiscoveryResponse {
                    tools: local_ad.advertised_tools,
                    node_id: self.node_id.clone(),
                };
                let reply_bytes = bincode::serialize(&reply).map_err(|e| HgbError::Execution(e.to_string()))?;
                send_framed_packet(stream, &reply_bytes).await?;
            }
            P2pMcpPacket::ExecuteSubtask { subtask, input_data } => {
                let res = self.execute_local_subtask(&subtask, input_data).await?;
                let reply = P2pMcpPacket::SubtaskExecutionResult(res);
                let reply_bytes = bincode::serialize(&reply).map_err(|e| HgbError::Execution(e.to_string()))?;
                send_framed_packet(stream, &reply_bytes).await?;
            }
            P2pMcpPacket::FetchStateRequest { blake3_hash } => {
                let data = self.content_cache.get(&blake3_hash).await;
                let found = data.is_some();
                let reply = P2pMcpPacket::FetchStateResponse {
                    blake3_hash,
                    found,
                    data,
                };
                let reply_bytes = bincode::serialize(&reply).map_err(|e| HgbError::Execution(e.to_string()))?;
                send_framed_packet(stream, &reply_bytes).await?;
            }
            P2pMcpPacket::Message(msg) => {
                let reply_msg = self.handle_p2p_message(msg).await?;
                let reply = P2pMcpPacket::Message(reply_msg);
                let reply_bytes = bincode::serialize(&reply).map_err(|e| HgbError::Execution(e.to_string()))?;
                send_framed_packet(stream, &reply_bytes).await?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Process inbound RPC messages across peer nodes
    pub async fn handle_p2p_message(&self, msg: P2pMcpMessage) -> Result<P2pMcpMessage> {
        match msg {
            P2pMcpMessage::ContextGraphSync { collection, since_timestamp, limit, payload } => {
                if let Some(nodes) = payload {
                    let mut graph = self.context_graph.write().await;
                    for node in nodes {
                        graph.insert(format!("{}:{}", node.collection, node.key), node);
                    }
                }
                let graph = self.context_graph.read().await;
                let lim = limit.unwrap_or(250);
                let matching: Vec<ContextGraphNode> = graph
                    .values()
                    .filter(|n| {
                        if let Some(ref c) = collection {
                            if &n.collection != c {
                                return false;
                            }
                        }
                        if let Some(ts) = since_timestamp {
                            if n.updated_at < ts {
                                return false;
                            }
                        }
                        true
                    })
                    .take(lim)
                    .cloned()
                    .collect();

                let total = matching.len();
                Ok(P2pMcpMessage::ContextGraphSyncResponse {
                    nodes: matching,
                    total_count: total,
                    source_node_id: self.node_id.clone(),
                    success: true,
                })
            }
            P2pMcpMessage::ContextGraphSyncResponse { .. } => {
                Ok(P2pMcpMessage::ContextGraphSyncResponse {
                    nodes: Vec::new(),
                    total_count: 0,
                    source_node_id: self.node_id.clone(),
                    success: true,
                })
            }
            P2pMcpMessage::VectorShardQuery { query, top_k, threshold } => {
                let matches = self.search_local_vector_shards(&query, top_k, threshold).await;
                let total = matches.len();
                Ok(P2pMcpMessage::VectorShardQueryResponse {
                    matches,
                    total_searched: total,
                    source_node_id: self.node_id.clone(),
                    success: true,
                })
            }
            P2pMcpMessage::VectorShardQueryResponse { .. } => {
                Ok(P2pMcpMessage::VectorShardQueryResponse {
                    matches: Vec::new(),
                    total_searched: 0,
                    source_node_id: self.node_id.clone(),
                    success: true,
                })
            }
            P2pMcpMessage::DelegateToolExecution { subtask, input_data } => {
                let result = self.execute_local_subtask(&subtask, input_data).await?;
                Ok(P2pMcpMessage::DelegateToolExecutionResponse { result })
            }
            P2pMcpMessage::DelegateToolExecutionResponse { .. } => {
                Err(HgbError::Execution("Unexpected response variant in request handler".to_string()))
            }
            P2pMcpMessage::ToolFederationList => {
                let local_ad = self.local_advertisement().await;
                Ok(P2pMcpMessage::ToolFederationListResponse {
                    tools: local_ad.advertised_tools,
                    node_id: self.node_id.clone(),
                })
            }
            P2pMcpMessage::ToolFederationListResponse { .. } => {
                let local_ad = self.local_advertisement().await;
                Ok(P2pMcpMessage::ToolFederationListResponse {
                    tools: local_ad.advertised_tools,
                    node_id: self.node_id.clone(),
                })
            }
        }
    }

    /// Dispatch a request message to a remote peer and await reply
    pub async fn send_p2p_message(&self, peer_addr: SocketAddr, msg: P2pMcpMessage) -> Result<P2pMcpMessage> {
        let packet = P2pMcpPacket::Message(msg);
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(5000),
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout to peer {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect to peer {}: {}", peer_addr, e)))?;

        send_framed_packet(&mut stream, &payload).await?;
        let resp_payload = receive_framed_packet(&mut stream).await?;

        let resp_packet: P2pMcpPacket = bincode::deserialize(&resp_payload)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match resp_packet {
            P2pMcpPacket::Message(reply_msg) => Ok(reply_msg),
            other => Err(HgbError::Execution(format!("Unexpected reply packet from peer: {:?}", other))),
        }
    }

    /// Store a context node in the local context graph
    pub async fn put_context_node(&self, mut node: ContextGraphNode) -> String {
        if node.blake3_hash.is_empty() {
            node.blake3_hash = blake3::hash(node.content.as_bytes()).to_hex().to_string();
        }
        if node.updated_at == 0 {
            node.updated_at = Utc::now().timestamp();
        }
        let hash = node.blake3_hash.clone();
        let key = format!("{}:{}", node.collection, node.key);
        let mut graph = self.context_graph.write().await;
        graph.insert(key, node);
        hash
    }

    /// Retrieve a context node from the local context graph
    pub async fn get_context_node(&self, collection: &str, key: &str) -> Option<ContextGraphNode> {
        let lookup_key = format!("{}:{}", collection, key);
        let graph = self.context_graph.read().await;
        graph.get(&lookup_key).cloned()
    }

    /// List all context nodes, optionally filtered by collection
    pub async fn list_context_nodes(&self, collection: Option<&str>) -> Vec<ContextGraphNode> {
        let graph = self.context_graph.read().await;
        graph
            .values()
            .filter(|n| collection.map_or(true, |c| n.collection == c))
            .cloned()
            .collect()
    }

    /// Synchronize context graph nodes from a peer node
    pub async fn sync_context_graph_with_peer(
        &self,
        peer_addr: SocketAddr,
        collection: Option<String>,
    ) -> Result<ContextGraphSyncReport> {
        let start = Instant::now();
        let req = P2pMcpMessage::ContextGraphSync {
            collection: collection.clone(),
            since_timestamp: None,
            limit: Some(500),
            payload: None,
        };

        match self.send_p2p_message(peer_addr, req).await? {
            P2pMcpMessage::ContextGraphSyncResponse {
                nodes,
                source_node_id,
                success,
                ..
            } => {
                if !success {
                    return Err(HgbError::Execution("Remote peer reported failure syncing context graph".to_string()));
                }
                let mut bytes_transferred = 0;
                let count = nodes.len();
                let mut graph = self.context_graph.write().await;
                for node in nodes {
                    bytes_transferred += node.content.len() + node.key.len();
                    graph.insert(format!("{}:{}", node.collection, node.key), node);
                }

                Ok(ContextGraphSyncReport {
                    collection: collection.unwrap_or_else(|| "all".to_string()),
                    replicated_nodes: count,
                    source_node_id,
                    bytes_transferred,
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            other => Err(HgbError::Execution(format!("Unexpected reply syncing context graph: {:?}", other))),
        }
    }

    /// Replicate local context graph nodes to a remote peer node
    pub async fn replicate_context_graph_to_peer(
        &self,
        peer_addr: SocketAddr,
        nodes: Vec<ContextGraphNode>,
    ) -> Result<ContextGraphSyncReport> {
        let start = Instant::now();
        let count = nodes.len();
        let bytes_transferred: usize = nodes.iter().map(|n| n.content.len() + n.key.len()).sum();
        let req = P2pMcpMessage::ContextGraphSync {
            collection: None,
            since_timestamp: None,
            limit: None,
            payload: Some(nodes),
        };

        match self.send_p2p_message(peer_addr, req).await? {
            P2pMcpMessage::ContextGraphSyncResponse { source_node_id, success, .. } => {
                if !success {
                    return Err(HgbError::Execution("Remote peer rejected context graph replication".to_string()));
                }
                Ok(ContextGraphSyncReport {
                    collection: "replicated".to_string(),
                    replicated_nodes: count,
                    source_node_id,
                    bytes_transferred,
                    duration_ms: start.elapsed().as_millis() as u64,
                })
            }
            other => Err(HgbError::Execution(format!("Unexpected reply to graph replication: {:?}", other))),
        }
    }

    /// Search local vector shards and context graph entries
    pub async fn search_local_vector_shards(
        &self,
        query: &str,
        top_k: usize,
        threshold: Option<f32>,
    ) -> Vec<VectorShardMatch> {
        let thresh = threshold.unwrap_or(0.0);
        let query_lower = query.to_lowercase();
        let graph = self.context_graph.read().await;
        let mut matches = Vec::new();

        for node in graph.values() {
            let content_lower = node.content.to_lowercase();
            let key_lower = node.key.to_lowercase();
            let mut score: f32 = 0.0;

            if content_lower.contains(&query_lower) || key_lower.contains(&query_lower) {
                score = 0.85;
                if key_lower.contains(&query_lower) {
                    score += 0.10;
                }
            } else {
                let query_words: Vec<&str> = query_lower.split_whitespace().collect();
                let mut matched = 0;
                for word in &query_words {
                    if content_lower.contains(word) {
                        matched += 1;
                    }
                }
                if !query_words.is_empty() && matched > 0 {
                    score = (matched as f32 / query_words.len() as f32) * 0.70;
                }
            }

            if score > thresh {
                let snippet: String = node.content.chars().take(200).collect();
                matches.push(VectorShardMatch {
                    id: format!("{}:{}", node.collection, node.key),
                    filepath_or_key: node.key.clone(),
                    chunk_index: 0,
                    content_snippet: snippet,
                    blake3_hash: node.blake3_hash.clone(),
                    score,
                    source_node_id: self.node_id.clone(),
                });
            }
        }

        // Also check SQLite mutation vectors in workspace if present
        let db_path = self.workspace_root.join(".hgb").join("ollama_mutation_vectors.db");
        if db_path.exists() {
            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                let pattern = format!("%{}%", query);
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT id, filepath, chunk_index, blake3_hash, content FROM mutation_chunks WHERE content LIKE ?1 LIMIT ?2"
                ) {
                    if let Ok(rows) = stmt.query_map(rusqlite::params![pattern, top_k], |row| {
                        let id: String = row.get(0)?;
                        let filepath: String = row.get(1)?;
                        let chunk_index: usize = row.get(2)?;
                        let blake3_hash: String = row.get(3)?;
                        let content: String = row.get(4)?;
                        Ok((id, filepath, chunk_index, blake3_hash, content))
                    }) {
                        for item in rows.flatten() {
                            let snippet: String = item.4.chars().take(200).collect();
                            matches.push(VectorShardMatch {
                                id: item.0,
                                filepath_or_key: item.1,
                                chunk_index: item.2,
                                content_snippet: snippet,
                                blake3_hash: item.3,
                                score: 0.90,
                                source_node_id: self.node_id.clone(),
                            });
                        }
                    }
                }
            }
        }

        matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        matches.truncate(top_k);
        matches
    }

    /// Query vector shards across the entire swarm mesh
    pub async fn query_vector_shards_across_swarm(
        &self,
        query: &str,
        top_k: usize,
    ) -> Vec<VectorShardMatch> {
        let mut all_matches = self.search_local_vector_shards(query, top_k, None).await;

        let peers = self.list_peers().await;
        for peer in peers {
            if peer.is_stale(DEFAULT_PEER_TIMEOUT_SECS) {
                continue;
            }
            let req = P2pMcpMessage::VectorShardQuery {
                query: query.to_string(),
                top_k,
                threshold: Some(0.30),
            };
            match self.send_p2p_message(peer.listen_addr, req).await {
                Ok(P2pMcpMessage::VectorShardQueryResponse { matches, .. }) => {
                    all_matches.extend(matches);
                }
                Err(e) => {
                    eprintln!("Failed to query vector shard on peer {}: {}", peer.node_id, e);
                }
                _ => {}
            }
        }

        all_matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        all_matches.truncate(top_k);
        all_matches
    }

    /// Aggregate all available MCP tools across the local node and connected swarm peers
    pub async fn list_federated_tools(&self) -> Vec<FederatedToolInfo> {
        let mut result = Vec::new();
        let local_ad = self.local_advertisement().await;
        for tool in &local_ad.advertised_tools {
            result.push(FederatedToolInfo {
                tool: tool.clone(),
                provider_node_id: local_ad.node_id.clone(),
                provider_addr: local_ad.listen_addr,
                node_load_score: local_ad.load_score,
                is_local: true,
            });
        }

        let peers = self.list_peers().await;
        for peer in peers {
            if peer.is_stale(DEFAULT_PEER_TIMEOUT_SECS) {
                continue;
            }
            for tool in &peer.advertised_tools {
                result.push(FederatedToolInfo {
                    tool: tool.clone(),
                    provider_node_id: peer.node_id.clone(),
                    provider_addr: peer.listen_addr,
                    node_load_score: peer.load_score,
                    is_local: false,
                });
            }
        }
        result
    }

    /// Delegate a subtask tool execution to a specific remote peer node
    pub async fn delegate_tool_to_peer(
        &self,
        target_peer: SocketAddr,
        spec: P2pSubtaskSpec,
        input_data: Option<Vec<u8>>,
    ) -> Result<P2pSubtaskResult> {
        let req = P2pMcpMessage::DelegateToolExecution {
            subtask: spec,
            input_data,
        };
        match self.send_p2p_message(target_peer, req).await? {
            P2pMcpMessage::DelegateToolExecutionResponse { mut result } => {
                result.is_remote = true;
                self.total_subtasks_delegated.fetch_add(1, Ordering::Relaxed);
                Ok(result)
            }
            other => Err(HgbError::Execution(format!("Unexpected response to tool delegation: {:?}", other))),
        }
    }

    /// Create UDP discovery beacon payload for this node
    pub async fn create_discovery_beacon(&self) -> P2pDiscoveryBeacon {
        let ad = self.local_advertisement().await;
        let tools: Vec<String> = ad.advertised_tools.iter().map(|t| t.name.clone()).collect();
        P2pDiscoveryBeacon {
            magic: SWARM_FRAME_MAGIC,
            node_id: self.node_id.clone(),
            listen_addr: self.listen_addr,
            load_score: ad.load_score,
            available_cpu_cores: ad.available_cpu_cores,
            free_ram_mb: ad.free_ram_mb,
            available_tools: tools,
            timestamp_secs: Utc::now().timestamp(),
        }
    }

    /// Process a received UDP discovery beacon from a peer node
    pub async fn receive_discovery_beacon(&self, beacon: P2pDiscoveryBeacon) {
        if beacon.node_id == self.node_id {
            return;
        }
        let mut ad = P2pMcpNodeAdvertisement::new(beacon.node_id.clone(), beacon.listen_addr);
        ad.available_cpu_cores = beacon.available_cpu_cores;
        ad.free_ram_mb = beacon.free_ram_mb;
        ad.advertised_tools.retain(|t| beacon.available_tools.contains(&t.name));
        self.register_peer(ad).await;
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    pub async fn status(&self) -> P2pMcpSwarmStatusReport {
        let local_ad = self.local_advertisement().await;
        let peers = self.list_peers().await;
        let advertised_tools = local_ad.advertised_tools.iter().map(|t| t.name.clone()).collect();
        let cached_entries = self.content_cache.len().await;

        P2pMcpSwarmStatusReport {
            node_id: self.node_id.clone(),
            listen_addr: self.listen_addr,
            active_peers_count: peers.len(),
            active_peers: peers,
            advertised_tools_count: local_ad.advertised_tools.len(),
            advertised_tools,
            local_load_score: local_ad.load_score,
            local_free_ram_mb: local_ad.free_ram_mb,
            cached_state_entries: cached_entries,
            total_subtasks_delegated: self.total_subtasks_delegated.load(Ordering::Relaxed),
            total_subtasks_local: self.total_subtasks_local.load(Ordering::Relaxed),
            total_cache_hits: self.total_cache_hits.load(Ordering::Relaxed),
            total_cache_misses: self.total_cache_misses.load(Ordering::Relaxed),
        }
    }
}

fn select_optimal_peer(eligible: &mut [P2pMcpNodeAdvertisement]) -> Option<&P2pMcpNodeAdvertisement> {
    if eligible.is_empty() {
        return None;
    }
    eligible.sort_by(|a, b| {
        a.load_score
            .partial_cmp(&b.load_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.free_ram_mb.cmp(&a.free_ram_mb))
    });
    eligible.first()
}

async fn send_framed_packet(stream: &mut TcpStream, payload: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&SWARM_FRAME_MAGIC.to_be_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame).await.map_err(HgbError::Io)?;
    stream.flush().await.map_err(HgbError::Io)?;
    Ok(())
}

async fn receive_framed_packet(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut magic_buf = [0u8; 4];
    stream.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
    if u32::from_be_bytes(magic_buf) != SWARM_FRAME_MAGIC {
        return Err(HgbError::Execution("Invalid swarm frame magic received".to_string()));
    }

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
    let length = u32::from_be_bytes(len_buf) as usize;

    if length > 32 * 1024 * 1024 {
        return Err(HgbError::Execution(format!("Payload too large: {} bytes", length)));
    }

    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).await.map_err(HgbError::Io)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_mcp_tool_advertisement_and_discovery() {
        let dir = tempdir().unwrap();
        let engine = P2pMcpSwarmEngine::new("node_alpha", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();

        let ad = engine.local_advertisement().await;
        assert_eq!(ad.node_id, "node_alpha");
        assert!(ad.supports_tool("hgb_sqlite_query"));
        assert!(ad.supports_tool("hgb_wasm_wit_exec"));
        assert!(ad.supports_tool("hgb_system_health"));
        assert!(ad.supports_tool("hgb_cgroup_control"));
        assert!(ad.supports_tool("hgb_microvm_status"));

        let mut peer = P2pMcpNodeAdvertisement::new("node_beta", "127.0.0.1:9091".parse().unwrap());
        peer.has_ollama = true;
        peer.load_score = 0.05;

        engine.register_peer(peer).await;

        let peers = engine.discover_peers_for_tool("hgb_sqlite_query").await;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].node_id, "node_beta");
    }

    #[tokio::test]
    async fn test_dynamic_subtask_negotiation_and_delegation() {
        let dir = tempdir().unwrap();
        let engine = P2pMcpSwarmEngine::new("node_local", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();

        // 1. Nominal local execution
        let subtask = P2pSubtaskSpec {
            step_id: "step_1".to_string(),
            tool_name: "hgb_system_health".to_string(),
            arguments: serde_json::json!({ "detailed": false }),
            input_state_hash: None,
            required_capability: None,
            timeout_ms: 3000,
        };

        let target = engine.negotiate_subtask(&subtask).await;
        match target {
            SubtaskDelegationTarget::LocalExecution { .. } => {}
            _ => panic!("Expected local execution when nominal"),
        }

        // 2. Overloaded local execution -> delegate to remote peer
        let mut peer = P2pMcpNodeAdvertisement::new("node_remote", "127.0.0.1:9092".parse().unwrap());
        peer.load_score = 0.05;
        peer.free_ram_mb = 8192;
        engine.register_peer(peer).await;

        // Force local load score high
        engine.total_subtasks_local.store(8, Ordering::Relaxed);
        {
            let mut local_ad = engine.local_ad.write().await;
            local_ad.load_score = 0.95;
        }

        let overloaded_target = engine.negotiate_subtask(&subtask).await;
        match overloaded_target {
            SubtaskDelegationTarget::RemoteDelegation { target_node, .. } => {
                assert_eq!(target_node.node_id, "node_remote");
            }
            _ => panic!("Expected delegation to remote peer under high load"),
        }
    }

    #[tokio::test]
    async fn test_blake3_content_addressed_state_cache() {
        let dir = tempdir().unwrap();
        let cache = P2pContentCache::new(Some(dir.path().join("cache")));

        let sample_payload = b"{\"columns\":[\"id\",\"val\"],\"rows\":[[1,\"alpha\"]]}";
        let expected_hash = blake3::hash(sample_payload).to_hex().to_string();

        let hash = cache.put(sample_payload).await;
        assert_eq!(hash, expected_hash);
        assert!(cache.contains(&hash).await);

        let retrieved = cache.get(&hash).await.unwrap();
        assert_eq!(retrieved, sample_payload);
    }

    #[tokio::test]
    async fn test_context_graph_and_vector_shard_search() {
        let dir = tempdir().unwrap();
        let engine = P2pMcpSwarmEngine::new("node_graph", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();

        // 1. Put context nodes
        let node1 = ContextGraphNode {
            collection: "codebase".to_string(),
            key: "auth.rs".to_string(),
            content: "pub fn authenticate_user(token: &str) -> bool { token == \"secret\" }".to_string(),
            metadata: serde_json::json!({ "lang": "rust" }),
            embedding: None,
            blake3_hash: String::new(),
            updated_at: 0,
        };
        let hash1 = engine.put_context_node(node1).await;
        assert!(!hash1.is_empty());

        let node2 = ContextGraphNode {
            collection: "codebase".to_string(),
            key: "database.rs".to_string(),
            content: "pub fn connect_sqlite(db_path: &Path) -> Result<Connection> { ... }".to_string(),
            metadata: serde_json::json!({ "lang": "rust" }),
            embedding: None,
            blake3_hash: String::new(),
            updated_at: 0,
        };
        engine.put_context_node(node2).await;

        // 2. Query context nodes
        let fetched = engine.get_context_node("codebase", "auth.rs").await.unwrap();
        assert_eq!(fetched.key, "auth.rs");

        let all_nodes = engine.list_context_nodes(Some("codebase")).await;
        assert_eq!(all_nodes.len(), 2);

        // 3. Search vector shards
        let matches = engine.search_local_vector_shards("authenticate", 5, None).await;
        assert!(!matches.is_empty());
        assert_eq!(matches[0].filepath_or_key, "auth.rs");
        assert!(matches[0].score >= 0.85);

        // 4. Federated tools listing
        let tools = engine.list_federated_tools().await;
        assert!(!tools.is_empty());
        assert!(tools.iter().any(|t| t.is_local && t.tool.name == "hgb_sqlite_query"));
    }

    #[tokio::test]
    async fn test_p2p_message_handling_sync() {
        let dir = tempdir().unwrap();
        let engine = P2pMcpSwarmEngine::new("node_sync", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();

        let node = ContextGraphNode {
            collection: "notes".to_string(),
            key: "arch.md".to_string(),
            content: "Hagibis sovereign edge architecture".to_string(),
            metadata: serde_json::json!({}),
            embedding: None,
            blake3_hash: "hash_abc".to_string(),
            updated_at: 100,
        };

        // Send replication payload via P2pMcpMessage
        let msg = P2pMcpMessage::ContextGraphSync {
            collection: Some("notes".to_string()),
            since_timestamp: None,
            limit: None,
            payload: Some(vec![node]),
        };

        let reply = engine.handle_p2p_message(msg).await.unwrap();
        match reply {
            P2pMcpMessage::ContextGraphSyncResponse { nodes, total_count, success, .. } => {
                assert!(success);
                assert_eq!(total_count, 1);
                assert_eq!(nodes[0].key, "arch.md");
            }
            _ => panic!("Expected ContextGraphSyncResponse"),
        }

        // Verify discovery beacon
        let beacon = engine.create_discovery_beacon().await;
        assert_eq!(beacon.node_id, "node_sync");
        assert!(!beacon.available_tools.is_empty());
    }
}
