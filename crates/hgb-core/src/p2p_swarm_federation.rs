//! # Pillar 2: Distributed MicroVM Mesh & WASM Swarm Federation Engine
//!
//! Production-grade distributed compute federation engine enabling `hgb-daemon` and `hgb-cli`
//! nodes to form a self-organizing peer-to-peer compute mesh across local containers,
//! LAN developer workstations, and edge/cloud instances.
//!
//! Integrates lightweight libp2p 0.57.0 transport layer (Tokio, TCP, Noise, Yamux, mDNS)
//! with self-organizing peer discovery, health telemetry scoring, Blake3 cryptographic framing,
//! bidirectional microVM/WASM sub-agent task dispatching, and distributed vector memory replication.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::RwLock;

use libp2p::{
    futures::StreamExt,
    identity::Keypair,
    mdns,
    multiaddr::{Multiaddr, Protocol},
    swarm::SwarmEvent,
    PeerId, SwarmBuilder,
};

use crate::error::{HgbError, Result};
use crate::microvm_sandbox_executor::{EphemeralExecSpec, FirecrackerEphemeralSandbox};
use crate::microvm_swarm_orchestrator::{MicroVmSwarmOrchestrator, SwarmTask, TaskComplexity};
use crate::wasm_hot_pool::WasmHotPool;

pub const SWARM_FRAME_MAGIC: u32 = 0x48474246; // "HGBF"
pub const SWARM_FRAME_HEADER_SIZE: usize = 4 + 4 + 32; // Magic (4B) + Payload Length (4B) + Blake3 Checksum (32B) = 40B
pub const DEFAULT_FEDERATION_PORT: u16 = 8970;
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 5;
pub const DEFAULT_STALE_NODE_TIMEOUT_SECS: i64 = 15;
pub const OVERLOAD_LOAD_SCORE_THRESHOLD: f32 = 0.70;

// -----------------------------------------------------------------------------
// Multiaddr & SocketAddr Conversions
// -----------------------------------------------------------------------------

/// Convert standard SocketAddr to libp2p Multiaddr format (`/ip4/{ip}/tcp/{port}`)
pub fn socket_addr_to_multiaddr(addr: &SocketAddr) -> Multiaddr {
    match addr {
        SocketAddr::V4(v4) => format!("/ip4/{}/tcp/{}", v4.ip(), v4.port())
            .parse()
            .unwrap_or_else(|_| Multiaddr::empty()),
        SocketAddr::V6(v6) => format!("/ip6/{}/tcp/{}", v6.ip(), v6.port())
            .parse()
            .unwrap_or_else(|_| Multiaddr::empty()),
    }
}

/// Convert libp2p Multiaddr format (`/ip4/{ip}/tcp/{port}`) to SocketAddr
pub fn multiaddr_to_socket_addr(addr: &Multiaddr) -> Option<SocketAddr> {
    let mut ip = None;
    let mut port = None;
    for proto in addr.iter() {
        match proto {
            Protocol::Ip4(ipv4) => ip = Some(std::net::IpAddr::V4(ipv4)),
            Protocol::Ip6(ipv6) => ip = Some(std::net::IpAddr::V6(ipv6)),
            Protocol::Tcp(p) => port = Some(p),
            _ => {}
        }
    }
    match (ip, port) {
        (Some(ip), Some(port)) => Some(SocketAddr::new(ip, port)),
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Blake3 Message Framing
// -----------------------------------------------------------------------------

/// Encode binary payload with magic header, 32-bit big-endian length, and Blake3 checksum
pub fn encode_swarm_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(SWARM_FRAME_HEADER_SIZE + payload.len());
    frame.extend_from_slice(&SWARM_FRAME_MAGIC.to_be_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let hash = blake3::hash(payload);
    frame.extend_from_slice(hash.as_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Read and verify a Blake3-framed message from an asynchronous byte stream
pub async fn read_swarm_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut magic_buf = [0u8; 4];
    reader.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
    if u32::from_be_bytes(magic_buf) != SWARM_FRAME_MAGIC {
        return Err(HgbError::Execution("Invalid swarm frame magic received".to_string()));
    }

    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
    let payload_len = u32::from_be_bytes(len_buf) as usize;

    if payload_len > 64 * 1024 * 1024 {
        return Err(HgbError::Execution(format!("Swarm payload too large: {} bytes", payload_len)));
    }

    let mut checksum = [0u8; 32];
    reader.read_exact(&mut checksum).await.map_err(HgbError::Io)?;

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await.map_err(HgbError::Io)?;

    let computed_hash = blake3::hash(&payload);
    if computed_hash.as_bytes() != &checksum {
        return Err(HgbError::Execution("Blake3 frame checksum verification failed in swarm message".to_string()));
    }

    Ok(payload)
}

/// Write a Blake3-framed message to an asynchronous byte stream
pub async fn write_swarm_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, payload: &[u8]) -> Result<()> {
    let frame = encode_swarm_frame(payload);
    writer.write_all(&frame).await.map_err(HgbError::Io)?;
    writer.flush().await.map_err(HgbError::Io)?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Task and Node Advertisement Data Structures
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwarmTaskKind {
    WasmPlugin,
    ParallelLint,
    TestSuite,
    CodeSynthesis,
    MicrovmSandbox,
    AstValidation,
    PatchVerify,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerNodeAdvertisement {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub available_cpu_cores: usize,
    pub free_ram_mb: u64,
    pub has_firecracker_kvm: bool,
    pub has_wasm_engine: bool,
    pub load_score: f32,
    pub last_heartbeat: DateTime<Utc>,
    #[serde(default)]
    pub peer_id: Option<String>,
    #[serde(default)]
    pub multiaddr: Option<String>,
}

impl WorkerNodeAdvertisement {
    pub fn new(node_id: impl Into<String>, listen_addr: SocketAddr) -> Self {
        let node_id = node_id.into();
        let available_cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let free_ram_mb = read_free_ram_mb();
        let has_firecracker_kvm = FirecrackerEphemeralSandbox::is_kvm_accessible();
        let has_wasm_engine = true;
        let multiaddr = Some(socket_addr_to_multiaddr(&listen_addr).to_string());

        Self {
            node_id,
            listen_addr,
            available_cpu_cores,
            free_ram_mb,
            has_firecracker_kvm,
            has_wasm_engine,
            load_score: 0.10,
            last_heartbeat: Utc::now(),
            peer_id: None,
            multiaddr,
        }
    }

    pub fn with_peer_id(mut self, peer_id: impl Into<String>) -> Self {
        self.peer_id = Some(peer_id.into());
        self
    }

    pub fn with_multiaddr(mut self, multiaddr: impl Into<String>) -> Self {
        self.multiaddr = Some(multiaddr.into());
        self
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
        let memory_pressure = if self.free_ram_mb < 512 {
            0.50
        } else if self.free_ram_mb < 1024 {
            0.25
        } else {
            0.05
        };

        self.load_score = (task_factor + memory_pressure).clamp(0.05, 1.50);
    }

    pub fn can_handle(&self, task: &SwarmFederationTask) -> bool {
        match task.task_type {
            SwarmTaskKind::MicrovmSandbox | SwarmTaskKind::TestSuite => true,
            SwarmTaskKind::WasmPlugin => self.has_wasm_engine,
            _ => true,
        }
    }
}

pub fn read_free_ram_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            for line in meminfo.lines() {
                if line.starts_with("MemAvailable:") || line.starts_with("MemFree:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return kb / 1024;
                        }
                    }
                }
            }
        }
    }
    4096
}

mod json_value_as_string {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(val: &Value, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = val.to_string();
        serializer.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        serde_json::from_str(&s).map_err(serde::de::Error::custom)
    }
}

mod opt_json_value_as_string {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(opt: &Option<Value>, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match opt {
            Some(val) => {
                let s = val.to_string();
                serializer.serialize_some(&s)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Option<Value>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt_s = Option::<String>::deserialize(deserializer)?;
        match opt_s {
            Some(s) => {
                let v = serde_json::from_str(&s).map_err(serde::de::Error::custom)?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmFederationTask {
    pub task_id: String,
    pub title: String,
    pub language: String,
    pub code: String,
    pub task_type: SwarmTaskKind,
    pub timeout_ms: u64,
    pub max_memory_mb: u32,
    pub priority: u32,
    #[serde(default, with = "opt_json_value_as_string")]
    pub input_payload: Option<serde_json::Value>,
}

impl SwarmFederationTask {
    pub fn new_wasm(task_id: impl Into<String>, title: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            title: title.into(),
            language: "wasm".to_string(),
            code: code.into(),
            task_type: SwarmTaskKind::WasmPlugin,
            timeout_ms: 3000,
            max_memory_mb: 64,
            priority: 10,
            input_payload: None,
        }
    }

    pub fn new_lint(task_id: impl Into<String>, title: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            title: title.into(),
            language: "ast".to_string(),
            code: code.into(),
            task_type: SwarmTaskKind::ParallelLint,
            timeout_ms: 5000,
            max_memory_mb: 128,
            priority: 5,
            input_payload: None,
        }
    }

    pub fn new_heavy(task_id: impl Into<String>, title: impl Into<String>, language: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            title: title.into(),
            language: language.into(),
            code: code.into(),
            task_type: SwarmTaskKind::MicrovmSandbox,
            timeout_ms: 10000,
            max_memory_mb: 256,
            priority: 1,
            input_payload: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmFederationResult {
    pub task_id: String,
    pub executing_node_id: String,
    pub worker_type: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub memory_mb: u32,
    #[serde(with = "json_value_as_string")]
    pub metadata: serde_json::Value,
}

// -----------------------------------------------------------------------------
// Distributed Vector Memory Sharing Data Structures & Trait
// -----------------------------------------------------------------------------

/// Lightweight metadata header broadcasted across peer nodes during vector mutations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorReplicationHeader {
    pub id: String,
    pub collection: String,
    pub dimension: usize,
    pub metadata: String,
    pub content_hash: String,
    pub lamport_ts: u64,
    pub created_at: i64,
    pub is_tombstone: bool,
}

/// Nearest-neighbor match returned from a federated vector search query
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederatedVectorMatch {
    pub id: String,
    pub collection: String,
    pub score: f32,
    pub metadata: String,
    pub source_node_id: String,
}

/// Abstract storage bridge trait connecting the Swarm engine with vector engines and CRDT meshes
#[async_trait::async_trait]
pub trait VectorMemoryBridge: Send + Sync {
    async fn on_vector_replicated(
        &self,
        header: &VectorReplicationHeader,
        embedding: Option<&[f32]>,
        origin_node_id: &str,
    ) -> Result<()>;

    async fn on_federated_query(
        &self,
        collection: &str,
        query_vec: &[f32],
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<FederatedVectorMatch>>;
}

/// In-memory vector memory bridge providing zero-dependency vector replication & cosine similarity
#[derive(Debug, Default)]
pub struct InMemoryVectorMemoryBridge {
    entries: RwLock<HashMap<String, HashMap<String, (VectorReplicationHeader, Option<Vec<f32>>)>>>,
}

impl InMemoryVectorMemoryBridge {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

pub fn vector_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

#[async_trait::async_trait]
impl VectorMemoryBridge for InMemoryVectorMemoryBridge {
    async fn on_vector_replicated(
        &self,
        header: &VectorReplicationHeader,
        embedding: Option<&[f32]>,
        _origin_node_id: &str,
    ) -> Result<()> {
        let mut guard = self.entries.write().await;
        let col = guard.entry(header.collection.clone()).or_default();
        if header.is_tombstone {
            col.remove(&header.id);
        } else {
            col.insert(header.id.clone(), (header.clone(), embedding.map(|e| e.to_vec())));
        }
        Ok(())
    }

    async fn on_federated_query(
        &self,
        collection: &str,
        query_vec: &[f32],
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<FederatedVectorMatch>> {
        let guard = self.entries.read().await;
        let col = match guard.get(collection) {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };

        let mut matches = Vec::new();
        for (id, (header, emb_opt)) in col {
            if let Some(emb) = emb_opt {
                let score = vector_cosine_similarity(emb, query_vec);
                if score >= min_score {
                    matches.push(FederatedVectorMatch {
                        id: id.clone(),
                        collection: header.collection.clone(),
                        score,
                        metadata: header.metadata.clone(),
                        source_node_id: "local".to_string(),
                    });
                }
            }
        }

        matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        matches.truncate(top_k);
        Ok(matches)
    }
}

// -----------------------------------------------------------------------------
// Swarm Protocol Packets
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwarmPacket {
    Ping { from_node_id: String, timestamp: DateTime<Utc> },
    Pong { node: WorkerNodeAdvertisement },
    Heartbeat(WorkerNodeAdvertisement),
    TaskDispatch { task: SwarmFederationTask },
    TaskResult { result: SwarmFederationResult },
    Goodbye { node_id: String },
    VectorReplicationBroadcast {
        header: VectorReplicationHeader,
        full_embedding: Option<Vec<f32>>,
        origin_node_id: String,
    },
    VectorReplicationAck {
        id: String,
        node_id: String,
        success: bool,
    },
    FederatedVectorQuery {
        query_id: String,
        collection: String,
        query_vec: Vec<f32>,
        top_k: usize,
        min_score: f32,
        origin_node_id: String,
    },
    FederatedVectorQueryResponse {
        query_id: String,
        results: Vec<FederatedVectorMatch>,
        executing_node_id: String,
        search_latency_ms: u64,
    },
}

#[derive(Debug, Clone)]
pub enum OptimalNodeDecision {
    LocalExecution { worker_tier: String, reason: String },
    RemoteDelegation { target_node: WorkerNodeAdvertisement, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmFederationStatusReport {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub active_peers_count: usize,
    pub active_peers: Vec<WorkerNodeAdvertisement>,
    pub local_load_score: f32,
    pub local_free_ram_mb: u64,
    pub local_has_kvm: bool,
    pub total_local_dispatched: u64,
    pub total_remote_dispatched: u64,
    pub total_tasks_completed: u64,
    pub avg_latency_ms: f64,
    #[serde(default)]
    pub local_peer_id: Option<String>,
    #[serde(default)]
    pub local_multiaddr: Option<String>,
    #[serde(default)]
    pub mdns_peers_discovered: usize,
}

// -----------------------------------------------------------------------------
// Libp2p P2P Mesh Transport Layer
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum P2pMeshEvent {
    PeerDiscovered {
        peer_id: String,
        multiaddr: String,
        socket_addr: Option<SocketAddr>,
        node_ad: Option<WorkerNodeAdvertisement>,
    },
    PeerExpired {
        peer_id: String,
    },
    HeartbeatReceived {
        node: WorkerNodeAdvertisement,
    },
    TaskDispatched {
        task_id: String,
        target_node: String,
    },
    TaskCompleted {
        task_id: String,
        executing_node: String,
        success: bool,
    },
    VectorReplicated {
        id: String,
        collection: String,
        origin_node_id: String,
        has_full_embedding: bool,
    },
}

/// Production-grade libp2p mesh transport enabling self-organizing discovery via mDNS,
/// explicit multiaddr dialing, heartbeat synchronization, and Blake3 message framing.
pub struct P2pMeshTransport {
    node_id: String,
    peer_id: PeerId,
    listen_addr: SocketAddr,
    listen_multiaddr: Multiaddr,
    discovered_peers: Arc<RwLock<HashMap<PeerId, Multiaddr>>>,
    discovered_nodes: Arc<RwLock<HashMap<String, WorkerNodeAdvertisement>>>,
    multiaddr_to_node_id: Arc<RwLock<HashMap<Multiaddr, String>>>,
    event_tx: tokio::sync::broadcast::Sender<P2pMeshEvent>,
    is_running: Arc<AtomicBool>,
}

impl P2pMeshTransport {
    pub fn new(node_id: impl Into<String>, listen_addr: SocketAddr) -> Self {
        let node_id = node_id.into();
        let keypair = Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let listen_multiaddr = socket_addr_to_multiaddr(&listen_addr);
        let (event_tx, _) = tokio::sync::broadcast::channel(1024);

        Self {
            node_id,
            peer_id,
            listen_addr,
            listen_multiaddr,
            discovered_peers: Arc::new(RwLock::new(HashMap::new())),
            discovered_nodes: Arc::new(RwLock::new(HashMap::new())),
            multiaddr_to_node_id: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn node_id(&self) -> &str { &self.node_id }
    pub fn peer_id(&self) -> &PeerId { &self.peer_id }
    pub fn listen_addr(&self) -> SocketAddr { self.listen_addr }
    pub fn listen_multiaddr(&self) -> &Multiaddr { &self.listen_multiaddr }
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<P2pMeshEvent> {
        self.event_tx.subscribe()
    }
    pub fn event_tx(&self) -> &tokio::sync::broadcast::Sender<P2pMeshEvent> {
        &self.event_tx
    }

    pub async fn list_discovered_peers(&self) -> Vec<(PeerId, Multiaddr)> {
        let guard = self.discovered_peers.read().await;
        guard.iter().map(|(p, m)| (*p, m.clone())).collect()
    }

    pub async fn list_discovered_nodes(&self) -> Vec<WorkerNodeAdvertisement> {
        let guard = self.discovered_nodes.read().await;
        guard.values().cloned().collect()
    }

    pub async fn get_node(&self, node_id: &str) -> Option<WorkerNodeAdvertisement> {
        let guard = self.discovered_nodes.read().await;
        guard.get(node_id).cloned()
    }

    pub async fn record_discovered_peer(
        &self,
        peer_id: PeerId,
        multiaddr: Multiaddr,
        local_ad: &WorkerNodeAdvertisement,
    ) {
        let s_addr = multiaddr_to_socket_addr(&multiaddr);
        {
            let mut peers = self.discovered_peers.write().await;
            peers.insert(peer_id, multiaddr.clone());
        }

        let mut ad_opt = None;
        if let Some(addr) = s_addr {
            if let Ok(peer_ad) = self.exchange_heartbeat(addr, local_ad).await {
                let mut nodes = self.discovered_nodes.write().await;
                nodes.insert(peer_ad.node_id.clone(), peer_ad.clone());
                let mut m2n = self.multiaddr_to_node_id.write().await;
                m2n.insert(multiaddr.clone(), peer_ad.node_id.clone());
                ad_opt = Some(peer_ad);
            }
        }

        let _ = self.event_tx.send(P2pMeshEvent::PeerDiscovered {
            peer_id: peer_id.to_string(),
            multiaddr: multiaddr.to_string(),
            socket_addr: s_addr,
            node_ad: ad_opt,
        });
    }

    pub async fn record_expired_peer(&self, peer_id: PeerId) {
        {
            let mut peers = self.discovered_peers.write().await;
            peers.remove(&peer_id);
        }
        let _ = self.event_tx.send(P2pMeshEvent::PeerExpired {
            peer_id: peer_id.to_string(),
        });
    }

    pub async fn dial_peer(
        &self,
        multiaddr: &Multiaddr,
        local_ad: &WorkerNodeAdvertisement,
    ) -> Result<WorkerNodeAdvertisement> {
        let s_addr = multiaddr_to_socket_addr(multiaddr)
            .ok_or_else(|| HgbError::Execution(format!("Multiaddr {} cannot be resolved to IP socket address", multiaddr)))?;

        let peer_ad = self.exchange_heartbeat(s_addr, local_ad).await?;
        
        let synthetic_peer = peer_ad.peer_id.as_deref()
            .and_then(|s| s.parse::<PeerId>().ok())
            .unwrap_or_else(|| {
                Keypair::generate_ed25519().public().to_peer_id()
            });

        {
            let mut peers = self.discovered_peers.write().await;
            peers.insert(synthetic_peer, multiaddr.clone());
        }
        {
            let mut nodes = self.discovered_nodes.write().await;
            nodes.insert(peer_ad.node_id.clone(), peer_ad.clone());
        }
        {
            let mut m2n = self.multiaddr_to_node_id.write().await;
            m2n.insert(multiaddr.clone(), peer_ad.node_id.clone());
        }

        let _ = self.event_tx.send(P2pMeshEvent::PeerDiscovered {
            peer_id: synthetic_peer.to_string(),
            multiaddr: multiaddr.to_string(),
            socket_addr: Some(s_addr),
            node_ad: Some(peer_ad.clone()),
        });

        Ok(peer_ad)
    }

    pub async fn exchange_heartbeat(
        &self,
        peer_addr: SocketAddr,
        local_ad: &WorkerNodeAdvertisement,
    ) -> Result<WorkerNodeAdvertisement> {
        let mut ad_with_p2p = local_ad.clone();
        ad_with_p2p.peer_id = Some(self.peer_id.to_string());
        ad_with_p2p.multiaddr = Some(self.listen_multiaddr.to_string());

        let packet = SwarmPacket::Heartbeat(ad_with_p2p);
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(3000),
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout for heartbeat to {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect for heartbeat to {}: {}", peer_addr, e)))?;

        write_swarm_frame(&mut stream, &payload).await?;

        let resp_payload = tokio::time::timeout(
            Duration::from_millis(3000),
            read_swarm_frame(&mut stream),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Heartbeat reply timeout from {}", peer_addr)))??;

        let reply: SwarmPacket = bincode::deserialize(&resp_payload)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match reply {
            SwarmPacket::Heartbeat(peer_ad) | SwarmPacket::Pong { node: peer_ad } => {
                let _ = self.event_tx.send(P2pMeshEvent::HeartbeatReceived {
                    node: peer_ad.clone(),
                });
                Ok(peer_ad)
            }
            other => Err(HgbError::Execution(format!("Unexpected reply in heartbeat exchange: {:?}", other))),
        }
    }

    pub async fn dispatch_remote_task(
        &self,
        target_addr: SocketAddr,
        task: &SwarmFederationTask,
    ) -> Result<SwarmFederationResult> {
        let packet = SwarmPacket::TaskDispatch { task: task.clone() };
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let timeout_ms = task.timeout_ms.max(5000);
        let mut stream = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            TcpStream::connect(target_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Task connection timeout to peer {}", target_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect for task dispatch to {}: {}", target_addr, e)))?;

        let _ = self.event_tx.send(P2pMeshEvent::TaskDispatched {
            task_id: task.task_id.clone(),
            target_node: target_addr.to_string(),
        });

        write_swarm_frame(&mut stream, &payload).await?;

        let resp_payload = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            read_swarm_frame(&mut stream),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Remote task execution timeout from {}", target_addr)))??;

        let response_packet: SwarmPacket = bincode::deserialize(&resp_payload)
            .map_err(|e| HgbError::Execution(format!("Deserialization error from peer {}: {}", target_addr, e)))?;

        match response_packet {
            SwarmPacket::TaskResult { result } => {
                let _ = self.event_tx.send(P2pMeshEvent::TaskCompleted {
                    task_id: result.task_id.clone(),
                    executing_node: result.executing_node_id.clone(),
                    success: result.success,
                });
                Ok(result)
            }
            other => Err(HgbError::Execution(format!("Unexpected task response packet from peer: {:?}", other))),
        }
    }

    pub fn start_mdns_discovery(self: &Arc<Self>, local_ad: Arc<RwLock<WorkerNodeAdvertisement>>) -> Result<tokio::task::JoinHandle<()>> {
        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .map_err(|e| HgbError::Execution(format!("Failed to configure libp2p TCP/Noise/Yamux: {}", e)))?
            .with_behaviour(|key| {
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            })
            .map_err(|e| HgbError::Execution(format!("Failed to build libp2p mdns behaviour: {}", e)))?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(10)))
            .build();

        let _ = swarm.listen_on(self.listen_multiaddr.clone());

        self.is_running.store(true, Ordering::SeqCst);
        let transport = Arc::clone(self);

        let handle = tokio::spawn(async move {
            while transport.is_running.load(Ordering::Relaxed) {
                tokio::select! {
                    event = swarm.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(mdns::Event::Discovered(list)) => {
                                let ad = local_ad.read().await.clone();
                                for (peer_id, multiaddr) in list {
                                    transport.record_discovered_peer(peer_id, multiaddr, &ad).await;
                                }
                            }
                            SwarmEvent::Behaviour(mdns::Event::Expired(list)) => {
                                for (peer_id, _multiaddr) in list {
                                    transport.record_expired_peer(peer_id).await;
                                }
                            }
                            _ => {}
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if !transport.is_running.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
            }
        });

        Ok(handle)
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

// -----------------------------------------------------------------------------
// Swarm Federation Engine
// -----------------------------------------------------------------------------

pub struct SwarmFederationEngine {
    node_id: String,
    listen_addr: SocketAddr,
    workspace_root: PathBuf,
    local_ad: Arc<RwLock<WorkerNodeAdvertisement>>,
    peer_registry: Arc<RwLock<HashMap<String, WorkerNodeAdvertisement>>>,
    orchestrator: Arc<MicroVmSwarmOrchestrator>,
    transport: Arc<P2pMeshTransport>,
    vector_bridge: Arc<RwLock<Option<Arc<dyn VectorMemoryBridge>>>>,
    is_running: Arc<AtomicBool>,
    total_local_dispatched: AtomicU64,
    total_remote_dispatched: AtomicU64,
    total_tasks_completed: AtomicU64,
    total_duration_ms: AtomicU64,
}

impl SwarmFederationEngine {
    pub fn new(
        node_id: impl Into<String>,
        listen_addr: SocketAddr,
        workspace_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let node_id = node_id.into();
        let ws = workspace_root.into();
        let local_ad = Arc::new(RwLock::new(WorkerNodeAdvertisement::new(
            node_id.clone(),
            listen_addr,
        )));
        let peer_registry = Arc::new(RwLock::new(HashMap::new()));
        let orchestrator = Arc::new(MicroVmSwarmOrchestrator::new(&ws)?);
        let transport = Arc::new(P2pMeshTransport::new(node_id.clone(), listen_addr));
        let vector_bridge = Arc::new(RwLock::new(None));

        Ok(Self {
            node_id,
            listen_addr,
            workspace_root: ws,
            local_ad,
            peer_registry,
            orchestrator,
            transport,
            vector_bridge,
            is_running: Arc::new(AtomicBool::new(false)),
            total_local_dispatched: AtomicU64::new(0),
            total_remote_dispatched: AtomicU64::new(0),
            total_tasks_completed: AtomicU64::new(0),
            total_duration_ms: AtomicU64::new(0),
        })
    }

    pub fn node_id(&self) -> &str { &self.node_id }
    pub fn listen_addr(&self) -> SocketAddr { self.listen_addr }
    pub fn orchestrator(&self) -> &Arc<MicroVmSwarmOrchestrator> { &self.orchestrator }
    pub fn transport(&self) -> &Arc<P2pMeshTransport> { &self.transport }

    pub async fn set_vector_memory_bridge(&self, bridge: Arc<dyn VectorMemoryBridge>) {
        let mut guard = self.vector_bridge.write().await;
        *guard = Some(bridge);
    }

    pub async fn vector_bridge(&self) -> Option<Arc<dyn VectorMemoryBridge>> {
        self.vector_bridge.read().await.clone()
    }

    pub async fn bound_listen_addr(&self) -> SocketAddr {
        self.local_ad.read().await.listen_addr
    }

    pub async fn register_peer(&self, mut ad: WorkerNodeAdvertisement) {
        if ad.node_id == self.node_id { return; }
        if ad.multiaddr.is_none() {
            ad.multiaddr = Some(socket_addr_to_multiaddr(&ad.listen_addr).to_string());
        }
        let mut reg = self.peer_registry.write().await;
        reg.insert(ad.node_id.clone(), ad.clone());

        // Also sync with transport
        let mut nodes = self.transport.discovered_nodes.write().await;
        nodes.insert(ad.node_id.clone(), ad.clone());
        let maddr = socket_addr_to_multiaddr(&ad.listen_addr);
        let mut m2n = self.transport.multiaddr_to_node_id.write().await;
        m2n.insert(maddr.clone(), ad.node_id.clone());

        if let Some(pid_str) = &ad.peer_id {
            if let Ok(pid) = pid_str.parse::<PeerId>() {
                let mut peers = self.transport.discovered_peers.write().await;
                peers.insert(pid, maddr);
            }
        }
    }

    pub async fn remove_peer(&self, node_id: &str) -> Option<WorkerNodeAdvertisement> {
        let mut reg = self.peer_registry.write().await;
        let removed = reg.remove(node_id);
        let mut nodes = self.transport.discovered_nodes.write().await;
        nodes.remove(node_id);
        removed
    }

    pub async fn prune_stale_nodes(&self, timeout_secs: i64) -> usize {
        let mut reg = self.peer_registry.write().await;
        let before = reg.len();
        reg.retain(|_, ad| !ad.is_stale(timeout_secs));
        let pruned = before - reg.len();

        let mut nodes = self.transport.discovered_nodes.write().await;
        nodes.retain(|_, ad| !ad.is_stale(timeout_secs));
        pruned
    }

    pub async fn list_peers(&self) -> Vec<WorkerNodeAdvertisement> {
        let reg = self.peer_registry.read().await;
        reg.values().cloned().collect()
    }

    pub async fn dial_peer_multiaddr(&self, multiaddr: &Multiaddr) -> Result<WorkerNodeAdvertisement> {
        let local_ad = self.local_advertisement().await;
        let peer_ad = self.transport.dial_peer(multiaddr, &local_ad).await?;
        self.register_peer(peer_ad.clone()).await;
        Ok(peer_ad)
    }

    pub async fn local_advertisement(&self) -> WorkerNodeAdvertisement {
        let mut ad = self.local_ad.write().await;
        let in_flight = self
            .total_local_dispatched
            .load(Ordering::Relaxed)
            .saturating_sub(self.total_tasks_completed.load(Ordering::Relaxed)) as usize;
        ad.refresh_telemetry(in_flight);
        ad.peer_id = Some(self.transport.peer_id().to_string());
        ad.clone()
    }

    pub async fn evaluate_placement(&self, task: &SwarmFederationTask) -> OptimalNodeDecision {
        let local_ad = self.local_advertisement().await;
        if local_ad.load_score < OVERLOAD_LOAD_SCORE_THRESHOLD {
            let tier = match task.task_type {
                SwarmTaskKind::WasmPlugin
                | SwarmTaskKind::AstValidation
                | SwarmTaskKind::PatchVerify
                | SwarmTaskKind::ParallelLint => "LocalWasmHotPool",
                _ => "LocalMicrovmSandbox",
            };
            return OptimalNodeDecision::LocalExecution {
                worker_tier: tier.to_string(),
                reason: format!("Local node load ({:.2}) is nominal (< {:.2})", local_ad.load_score, OVERLOAD_LOAD_SCORE_THRESHOLD),
            };
        }

        let peers = self.list_peers().await;
        let mut eligible: Vec<WorkerNodeAdvertisement> = peers
            .into_iter()
            .filter(|p| !p.is_stale(DEFAULT_STALE_NODE_TIMEOUT_SECS) && p.can_handle(task))
            .collect();

        if eligible.is_empty() {
            return OptimalNodeDecision::LocalExecution {
                worker_tier: "LocalFallbackUnderLoad".to_string(),
                reason: "Local node is overloaded but no active remote peers are available".to_string(),
            };
        }

        eligible.sort_by(|a, b| {
            a.load_score
                .partial_cmp(&b.load_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.free_ram_mb.cmp(&a.free_ram_mb))
        });

        let best = eligible[0].clone();
        if best.load_score < local_ad.load_score {
            OptimalNodeDecision::RemoteDelegation {
                target_node: best.clone(),
                reason: format!(
                    "Local node overloaded ({:.2}); delegating to remote peer {} ({:.2} load, {} MB free RAM)",
                    local_ad.load_score, best.node_id, best.load_score, best.free_ram_mb
                ),
            }
        } else {
            OptimalNodeDecision::LocalExecution {
                worker_tier: "LocalExecution".to_string(),
                reason: "Remote peers are equally or more loaded than local node".to_string(),
            }
        }
    }

    pub async fn dispatch_task(&self, task: SwarmFederationTask) -> Result<SwarmFederationResult> {
        let decision = self.evaluate_placement(&task).await;
        match decision {
            OptimalNodeDecision::LocalExecution { worker_tier, .. } => {
                self.total_local_dispatched.fetch_add(1, Ordering::Relaxed);
                self.execute_locally(task, &worker_tier)
            }
            OptimalNodeDecision::RemoteDelegation { target_node, reason } => {
                self.total_remote_dispatched.fetch_add(1, Ordering::Relaxed);
                match self.dispatch_remote(&target_node, &task).await {
                    Ok(result) => Ok(result),
                    Err(e) => {
                        eprintln!(
                            "⚠️ Swarm federation remote dispatch to {} failed: {}. Falling back to local execution.",
                            target_node.listen_addr, e
                        );
                        let mut fallback = self.execute_locally(task, "LocalFallbackAfterNetworkError")?;
                        fallback.metadata["fallback_from_peer"] = serde_json::json!(target_node.node_id);
                        fallback.metadata["delegation_failure"] = serde_json::json!(e.to_string());
                        fallback.metadata["delegation_reason"] = serde_json::json!(reason);
                        Ok(fallback)
                    }
                }
            }
        }
    }

    fn execute_locally(&self, task: SwarmFederationTask, worker_label: &str) -> Result<SwarmFederationResult> {
        let start = Instant::now();
        let swarm_task = SwarmTask {
            id: Some(task.task_id.clone()),
            task_name: Some(task.title.clone()),
            language: task.language.clone(),
            code: task.code.clone(),
            patch_content: None,
            isolation_tier: None,
            max_memory_mb: task.max_memory_mb,
            timeout_ms: task.timeout_ms,
            complexity: match task.task_type {
                SwarmTaskKind::WasmPlugin
                | SwarmTaskKind::AstValidation
                | SwarmTaskKind::PatchVerify
                | SwarmTaskKind::ParallelLint => Some(TaskComplexity::Lightweight),
                _ => Some(TaskComplexity::HeavyUntrusted),
            },
            input_payload: task.input_payload.clone(),
        };

        let outcome = self.orchestrator.dispatch(&swarm_task)?;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        self.total_tasks_completed.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms.fetch_add(elapsed_ms, Ordering::Relaxed);

        Ok(SwarmFederationResult {
            task_id: outcome.task_id,
            executing_node_id: self.node_id.clone(),
            worker_type: format!("{}:{}", worker_label, outcome.worker_type),
            success: outcome.success,
            exit_code: outcome.exit_code,
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            duration_ms: outcome.duration_ms,
            memory_mb: outcome.memory_mb,
            metadata: outcome.metadata,
        })
    }

    pub async fn dispatch_remote(
        &self,
        target_node: &WorkerNodeAdvertisement,
        task: &SwarmFederationTask,
    ) -> Result<SwarmFederationResult> {
        let result = self.transport.dispatch_remote_task(target_node.listen_addr, task).await?;
        self.total_tasks_completed.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms.fetch_add(result.duration_ms, Ordering::Relaxed);
        Ok(result)
    }

    pub async fn dispatch_workflow_sharded(
        &self,
        tasks: Vec<SwarmFederationTask>,
    ) -> Result<Vec<SwarmFederationResult>> {
        if tasks.is_empty() {
            return Ok(Vec::new());
        }

        let mut handles = Vec::with_capacity(tasks.len());
        for task in tasks {
            let engine = self.clone_ref();
            handles.push(tokio::spawn(async move {
                engine.dispatch_task(task).await
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            let res = handle.await.map_err(|e| HgbError::Execution(format!("Task join error: {}", e)))??;
            results.push(res);
        }

        Ok(results)
    }

    pub fn clone_ref(&self) -> Arc<Self> {
        Arc::new(Self {
            node_id: self.node_id.clone(),
            listen_addr: self.listen_addr,
            workspace_root: self.workspace_root.clone(),
            local_ad: Arc::clone(&self.local_ad),
            peer_registry: Arc::clone(&self.peer_registry),
            orchestrator: Arc::clone(&self.orchestrator),
            transport: Arc::clone(&self.transport),
            vector_bridge: Arc::clone(&self.vector_bridge),
            is_running: Arc::clone(&self.is_running),
            total_local_dispatched: AtomicU64::new(self.total_local_dispatched.load(Ordering::Relaxed)),
            total_remote_dispatched: AtomicU64::new(self.total_remote_dispatched.load(Ordering::Relaxed)),
            total_tasks_completed: AtomicU64::new(self.total_tasks_completed.load(Ordering::Relaxed)),
            total_duration_ms: AtomicU64::new(self.total_duration_ms.load(Ordering::Relaxed)),
        })
    }

    pub fn handle_remote_task(&self, task: SwarmFederationTask) -> Result<SwarmFederationResult> {
        self.execute_locally(task, "RemoteInboundPeer")
    }

    pub async fn start_tcp_listener(self: &Arc<Self>) -> Result<tokio::task::JoinHandle<()>> {
        let listener = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            HgbError::Execution(format!("Failed to bind TCP swarm listener on {}: {}", self.listen_addr, e))
        })?;

        let bound = listener.local_addr().unwrap_or(self.listen_addr);
        {
            let mut ad = self.local_ad.write().await;
            ad.listen_addr = bound;
            ad.multiaddr = Some(socket_addr_to_multiaddr(&bound).to_string());
            ad.peer_id = Some(self.transport.peer_id().to_string());
        }

        self.is_running.store(true, Ordering::SeqCst);
        let engine = Arc::clone(self);

        // Start mDNS peer discovery in background
        let _ = self.transport.start_mdns_discovery(Arc::clone(&self.local_ad));

        let handle = tokio::spawn(async move {
            while engine.is_running.load(Ordering::Relaxed) {
                match listener.accept().await {
                    Ok((mut stream, peer_addr)) => {
                        let eng = Arc::clone(&engine);
                        tokio::spawn(async move {
                            if let Err(e) = eng.handle_tcp_connection(&mut stream, peer_addr).await {
                                eprintln!("Swarm IPC connection from {} terminated: {}", peer_addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        if !engine.is_running.load(Ordering::Relaxed) { break; }
                        eprintln!("Swarm listener accept error: {}", e);
                    }
                }
            }
        });

        Ok(handle)
    }

    async fn handle_tcp_connection(&self, stream: &mut TcpStream, _peer_addr: SocketAddr) -> Result<()> {
        let payload_buf = read_swarm_frame(stream).await?;

        let packet: SwarmPacket = bincode::deserialize(&payload_buf)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match packet {
            SwarmPacket::Ping { .. } => {
                let local_ad = self.local_advertisement().await;
                let reply = SwarmPacket::Pong { node: local_ad };
                self.send_packet_reply(stream, reply).await?;
            }
            SwarmPacket::Heartbeat(ad) => {
                self.register_peer(ad).await;
                let local_ad = self.local_advertisement().await;
                let reply = SwarmPacket::Heartbeat(local_ad);
                self.send_packet_reply(stream, reply).await?;
            }
            SwarmPacket::TaskDispatch { task } => {
                let result = self.handle_remote_task(task)?;
                let reply = SwarmPacket::TaskResult { result };
                self.send_packet_reply(stream, reply).await?;
            }
            SwarmPacket::Goodbye { node_id } => {
                self.remove_peer(&node_id).await;
            }
            SwarmPacket::Pong { node } => {
                self.register_peer(node).await;
            }
            SwarmPacket::TaskResult { .. } => {}
            SwarmPacket::VectorReplicationBroadcast { header, full_embedding, origin_node_id } => {
                let mut success = true;
                if let Some(bridge) = self.vector_bridge.read().await.as_ref() {
                    if let Err(e) = bridge.on_vector_replicated(&header, full_embedding.as_deref(), &origin_node_id).await {
                        eprintln!("Vector replication bridge error on {}: {}", self.node_id, e);
                        success = false;
                    }
                }
                let _ = self.transport.event_tx.send(P2pMeshEvent::VectorReplicated {
                    id: header.id.clone(),
                    collection: header.collection.clone(),
                    origin_node_id,
                    has_full_embedding: full_embedding.is_some(),
                });
                let reply = SwarmPacket::VectorReplicationAck {
                    id: header.id,
                    node_id: self.node_id.clone(),
                    success,
                };
                self.send_packet_reply(stream, reply).await?;
            }
            SwarmPacket::VectorReplicationAck { .. } => {}
            SwarmPacket::FederatedVectorQuery { query_id, collection, query_vec, top_k, min_score, .. } => {
                let start = Instant::now();
                let mut results = Vec::new();
                if let Some(bridge) = self.vector_bridge.read().await.as_ref() {
                    if let Ok(matches) = bridge.on_federated_query(&collection, &query_vec, top_k, min_score).await {
                        results = matches;
                    }
                }
                let search_latency_ms = start.elapsed().as_millis() as u64;
                let reply = SwarmPacket::FederatedVectorQueryResponse {
                    query_id,
                    results,
                    executing_node_id: self.node_id.clone(),
                    search_latency_ms,
                };
                self.send_packet_reply(stream, reply).await?;
            }
            SwarmPacket::FederatedVectorQueryResponse { .. } => {}
        }

        Ok(())
    }

    async fn send_packet_reply(&self, stream: &mut TcpStream, packet: SwarmPacket) -> Result<()> {
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;
        write_swarm_frame(stream, &payload).await
    }

    pub async fn send_heartbeat_to_peer(&self, peer_addr: SocketAddr) -> Result<WorkerNodeAdvertisement> {
        let local_ad = self.local_advertisement().await;
        let peer_ad = self.transport.exchange_heartbeat(peer_addr, &local_ad).await?;
        self.register_peer(peer_ad.clone()).await;
        Ok(peer_ad)
    }

    /// Broadcast lightweight vector memory header or full embedding across all discovered swarm peers
    pub async fn broadcast_vector_memory(
        &self,
        header: VectorReplicationHeader,
        full_embedding: Option<Vec<f32>>,
    ) -> Result<usize> {
        let peers = self.list_peers().await;
        if peers.is_empty() {
            return Ok(0);
        }

        let packet = SwarmPacket::VectorReplicationBroadcast {
            header: header.clone(),
            full_embedding: full_embedding.clone(),
            origin_node_id: self.node_id.clone(),
        };
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut successful_replications = 0;
        for peer in peers {
            if peer.node_id == self.node_id {
                continue;
            }
            let payload_clone = payload.clone();
            let peer_addr = peer.listen_addr;

            let send_res = tokio::time::timeout(Duration::from_millis(2000), async move {
                let mut stream = TcpStream::connect(peer_addr).await.map_err(HgbError::Io)?;
                write_swarm_frame(&mut stream, &payload_clone).await?;
                let resp = read_swarm_frame(&mut stream).await?;
                let ack: SwarmPacket = bincode::deserialize(&resp).map_err(|e| HgbError::Execution(e.to_string()))?;
                match ack {
                    SwarmPacket::VectorReplicationAck { success, .. } => Ok::<bool, HgbError>(success),
                    _ => Ok::<bool, HgbError>(false),
                }
            }).await;

            if let Ok(Ok(true)) = send_res {
                successful_replications += 1;
            }
        }

        let _ = self.transport.event_tx.send(P2pMeshEvent::VectorReplicated {
            id: header.id,
            collection: header.collection,
            origin_node_id: self.node_id.clone(),
            has_full_embedding: full_embedding.is_some(),
        });

        Ok(successful_replications)
    }

    /// Query the entire swarm mesh, gathering and merging top-k nearest neighbors across all active nodes
    pub async fn federated_vector_query(
        &self,
        collection: &str,
        query_vec: &[f32],
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<FederatedVectorMatch>> {
        let mut all_matches = Vec::new();

        // 1. Query local vector bridge if configured
        if let Some(bridge) = self.vector_bridge.read().await.as_ref() {
            if let Ok(local_matches) = bridge.on_federated_query(collection, query_vec, top_k, min_score).await {
                for mut m in local_matches {
                    m.source_node_id = self.node_id.clone();
                    all_matches.push(m);
                }
            }
        }

        // 2. Concurrently broadcast query to all discovered remote swarm peers
        let peers = self.list_peers().await;
        let query_id = format!("vquery_{}", uuid::Uuid::new_v4().simple());

        let packet = SwarmPacket::FederatedVectorQuery {
            query_id: query_id.clone(),
            collection: collection.to_string(),
            query_vec: query_vec.to_vec(),
            top_k,
            min_score,
            origin_node_id: self.node_id.clone(),
        };
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut handles = Vec::new();
        for peer in peers {
            if peer.node_id == self.node_id {
                continue;
            }
            let payload_clone = payload.clone();
            let peer_addr = peer.listen_addr;
            let peer_id_str = peer.node_id.clone();

            handles.push(tokio::spawn(async move {
                let res: Result<Vec<FederatedVectorMatch>> = tokio::time::timeout(Duration::from_millis(3000), async move {
                    let mut stream = TcpStream::connect(peer_addr).await.map_err(HgbError::Io)?;
                    write_swarm_frame(&mut stream, &payload_clone).await?;
                    let resp = read_swarm_frame(&mut stream).await?;
                    let reply: SwarmPacket = bincode::deserialize(&resp).map_err(|e| HgbError::Execution(e.to_string()))?;
                    match reply {
                        SwarmPacket::FederatedVectorQueryResponse { results, executing_node_id, .. } => {
                            let mapped: Vec<FederatedVectorMatch> = results.into_iter().map(|mut r| {
                                if r.source_node_id.is_empty() || r.source_node_id == "local" {
                                    r.source_node_id = executing_node_id.clone();
                                }
                                r
                            }).collect();
                            Ok(mapped)
                        }
                        _ => Ok(Vec::new()),
                    }
                }).await.map_err(|_| HgbError::Execution(format!("Query timeout to peer {}", peer_id_str)))?;
                res
            }));
        }

        for h in handles {
            if let Ok(Ok(peer_matches)) = h.await {
                all_matches.extend(peer_matches);
            }
        }

        // Deduplicate by entry ID (keeping the highest similarity score)
        let mut deduplicated: HashMap<String, FederatedVectorMatch> = HashMap::new();
        for m in all_matches {
            match deduplicated.get(&m.id) {
                Some(existing) if existing.score >= m.score => {}
                _ => {
                    deduplicated.insert(m.id.clone(), m);
                }
            }
        }

        let mut final_results: Vec<FederatedVectorMatch> = deduplicated.into_values().collect();
        final_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        final_results.truncate(top_k);

        Ok(final_results)
    }

    pub async fn status(&self) -> SwarmFederationStatusReport {
        let local_ad = self.local_advertisement().await;
        let peers = self.list_peers().await;
        let completed = self.total_tasks_completed.load(Ordering::Relaxed);
        let duration = self.total_duration_ms.load(Ordering::Relaxed);
        let avg_latency = if completed > 0 {
            (duration as f64) / (completed as f64)
        } else {
            0.0
        };

        let transport_peers = self.transport.list_discovered_peers().await;

        SwarmFederationStatusReport {
            node_id: self.node_id.clone(),
            listen_addr: self.listen_addr,
            active_peers_count: peers.len(),
            active_peers: peers,
            local_load_score: local_ad.load_score,
            local_free_ram_mb: local_ad.free_ram_mb,
            local_has_kvm: local_ad.has_firecracker_kvm,
            total_local_dispatched: self.total_local_dispatched.load(Ordering::Relaxed),
            total_remote_dispatched: self.total_remote_dispatched.load(Ordering::Relaxed),
            total_tasks_completed: completed,
            avg_latency_ms: avg_latency,
            local_peer_id: Some(self.transport.peer_id().to_string()),
            local_multiaddr: Some(self.transport.listen_multiaddr().to_string()),
            mdns_peers_discovered: transport_peers.len(),
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        self.transport.stop();
    }
}

// -----------------------------------------------------------------------------
// Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_node_advertisement_and_heartbeat_prune() {
        let dir = tempdir().unwrap();
        let engine = SwarmFederationEngine::new("node_test_1", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();
        
        let peer_ad = WorkerNodeAdvertisement {
            node_id: "node_peer_1".to_string(),
            listen_addr: "127.0.0.1:9099".parse().unwrap(),
            available_cpu_cores: 8,
            free_ram_mb: 16384,
            has_firecracker_kvm: true,
            has_wasm_engine: true,
            load_score: 0.15,
            last_heartbeat: Utc::now(),
            peer_id: None,
            multiaddr: Some("/ip4/127.0.0.1/tcp/9099".to_string()),
        };

        engine.register_peer(peer_ad.clone()).await;
        let peers = engine.list_peers().await;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].node_id, "node_peer_1");

        // Stale node pruning
        let stale_peer = WorkerNodeAdvertisement {
            node_id: "node_peer_stale".to_string(),
            listen_addr: "127.0.0.1:9098".parse().unwrap(),
            available_cpu_cores: 4,
            free_ram_mb: 8192,
            has_firecracker_kvm: false,
            has_wasm_engine: true,
            load_score: 0.85,
            last_heartbeat: Utc::now() - chrono::Duration::seconds(60),
            peer_id: None,
            multiaddr: Some("/ip4/127.0.0.1/tcp/9098".to_string()),
        };
        engine.register_peer(stale_peer).await;
        assert_eq!(engine.list_peers().await.len(), 2);

        let pruned = engine.prune_stale_nodes(10).await;
        assert_eq!(pruned, 1);
        assert_eq!(engine.list_peers().await.len(), 1);
        assert_eq!(engine.list_peers().await[0].node_id, "node_peer_1");
    }

    #[tokio::test]
    async fn test_load_balanced_worker_selection() {
        let dir = tempdir().unwrap();
        let engine = SwarmFederationEngine::new("node_test_2", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();

        let task = SwarmFederationTask::new_lint("task_1", "Lint Test", "fn main() {}");

        // When local node load is nominal, executes locally
        let decision = engine.evaluate_placement(&task).await;
        match decision {
            OptimalNodeDecision::LocalExecution { worker_tier, .. } => {
                assert_eq!(worker_tier, "LocalWasmHotPool");
            }
            OptimalNodeDecision::RemoteDelegation { .. } => panic!("Expected local execution"),
        }

        // When peer is registered and local node is overloaded
        let idle_peer = WorkerNodeAdvertisement {
            node_id: "remote_idle_node".to_string(),
            listen_addr: "127.0.0.1:18970".parse().unwrap(),
            available_cpu_cores: 16,
            free_ram_mb: 32768,
            has_firecracker_kvm: true,
            has_wasm_engine: true,
            load_score: 0.10,
            last_heartbeat: Utc::now(),
            peer_id: None,
            multiaddr: Some("/ip4/127.0.0.1/tcp/18970".to_string()),
        };
        engine.register_peer(idle_peer).await;

        // Force local load score above threshold by simulating active in-flight tasks
        engine.total_local_dispatched.store(10, Ordering::SeqCst);

        let overloaded_decision = engine.evaluate_placement(&task).await;
        match overloaded_decision {
            OptimalNodeDecision::RemoteDelegation { target_node, .. } => {
                assert_eq!(target_node.node_id, "remote_idle_node");
            }
            OptimalNodeDecision::LocalExecution { .. } => panic!("Expected delegation to remote peer under high load"),
        }
    }

    #[tokio::test]
    async fn test_local_task_dispatch_and_status() {
        let dir = tempdir().unwrap();
        let engine = SwarmFederationEngine::new("node_test_3", "127.0.0.1:0".parse().unwrap(), dir.path()).unwrap();

        let task = SwarmFederationTask::new_lint("task_local_1", "AST Check", "pub fn compute() -> usize { 42 }");

        let result = engine.dispatch_task(task).await.unwrap();
        assert!(result.success);
        assert_eq!(result.executing_node_id, "node_test_3");

        let status = engine.status().await;
        assert_eq!(status.total_local_dispatched, 1);
        assert_eq!(status.total_tasks_completed, 1);
    }

    #[tokio::test]
    async fn test_peer_advertisement_and_mdns_discovery() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();

        let engine_a = Arc::new(SwarmFederationEngine::new("node_disc_a", "127.0.0.1:0".parse().unwrap(), dir_a.path()).unwrap());
        let engine_b = Arc::new(SwarmFederationEngine::new("node_disc_b", "127.0.0.1:0".parse().unwrap(), dir_b.path()).unwrap());

        let _join_b = engine_b.start_tcp_listener().await.unwrap();
        let b_addr = engine_b.bound_listen_addr().await;
        assert_ne!(b_addr.port(), 0);

        // Test explicit multiaddr dialing
        let b_maddr = socket_addr_to_multiaddr(&b_addr);
        assert!(b_maddr.to_string().contains("/tcp/"));

        let dialed_ad = engine_a.dial_peer_multiaddr(&b_maddr).await.unwrap();
        assert_eq!(dialed_ad.node_id, "node_disc_b");
        assert!(dialed_ad.available_cpu_cores >= 1);
        assert!(dialed_ad.free_ram_mb > 0);
        assert!(dialed_ad.load_score >= 0.05);

        // Verify peer is recorded in engine_a's peer registry and transport registry
        let peers_a = engine_a.list_peers().await;
        assert_eq!(peers_a.len(), 1);
        assert_eq!(peers_a[0].node_id, "node_disc_b");

        let transport_nodes = engine_a.transport().list_discovered_nodes().await;
        assert_eq!(transport_nodes.len(), 1);

        engine_a.stop();
        engine_b.stop();
    }

    #[tokio::test]
    async fn test_task_serialization_dispatching_and_remote_execution_roundtrip() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();

        let engine_a = Arc::new(SwarmFederationEngine::new("node_exec_a", "127.0.0.1:0".parse().unwrap(), dir_a.path()).unwrap());
        let engine_b = Arc::new(SwarmFederationEngine::new("node_exec_b", "127.0.0.1:0".parse().unwrap(), dir_b.path()).unwrap());

        let _join_b = engine_b.start_tcp_listener().await.unwrap();
        let b_addr = engine_b.bound_listen_addr().await;

        let b_ad = WorkerNodeAdvertisement::new("node_exec_b", b_addr);
        engine_a.register_peer(b_ad.clone()).await;

        // Dispatch task remotely from Node A to Node B over Blake3 framed channel
        let task = SwarmFederationTask::new_lint(
            "task_roundtrip_1",
            "Parallel AST Lint Check",
            "pub fn fibonacci(n: u32) -> u32 { if n <= 1 { n } else { fibonacci(n-1) + fibonacci(n-2) } }",
        );

        let result = engine_a.dispatch_remote(&b_ad, &task).await.unwrap();
        assert_eq!(result.task_id, "task_roundtrip_1");
        assert_eq!(result.executing_node_id, "node_exec_b");
        assert!(result.success);
        assert!(result.worker_type.contains("RemoteInboundPeer"));

        engine_a.stop();
        engine_b.stop();
    }

    #[tokio::test]
    async fn test_distributed_vector_synchronization_across_mesh_nodes() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();

        let engine_a = Arc::new(SwarmFederationEngine::new("node_vec_a", "127.0.0.1:0".parse().unwrap(), dir_a.path()).unwrap());
        let engine_b = Arc::new(SwarmFederationEngine::new("node_vec_b", "127.0.0.1:0".parse().unwrap(), dir_b.path()).unwrap());

        // Attach vector memory bridges
        let bridge_a = Arc::new(InMemoryVectorMemoryBridge::new());
        let bridge_b = Arc::new(InMemoryVectorMemoryBridge::new());
        engine_a.set_vector_memory_bridge(bridge_a.clone()).await;
        engine_b.set_vector_memory_bridge(bridge_b.clone()).await;

        let _join_a = engine_a.start_tcp_listener().await.unwrap();
        let _join_b = engine_b.start_tcp_listener().await.unwrap();

        let a_addr = engine_a.bound_listen_addr().await;
        let b_addr = engine_b.bound_listen_addr().await;

        // Register each other
        engine_a.register_peer(WorkerNodeAdvertisement::new("node_vec_b", b_addr)).await;
        engine_b.register_peer(WorkerNodeAdvertisement::new("node_vec_a", a_addr)).await;

        // 1. Broadcast vector memory from Node A to Node B
        let header = VectorReplicationHeader {
            id: "vec_embedding_alpha".to_string(),
            collection: "code_symbols".to_string(),
            dimension: 4,
            metadata: serde_json::json!({"symbol": "calculate_hash", "file": "src/crypto.rs"}).to_string(),
            content_hash: "hash_alpha_123".to_string(),
            lamport_ts: 1,
            created_at: 1000,
            is_tombstone: false,
        };
        let embedding = vec![1.0, 0.0, 0.0, 0.0];

        let replications = engine_a.broadcast_vector_memory(header.clone(), Some(embedding.clone())).await.unwrap();
        assert_eq!(replications, 1);

        // Verify Node B received and stored the vector
        let b_search = bridge_b.on_federated_query("code_symbols", &[1.0, 0.0, 0.0, 0.0], 5, 0.8).await.unwrap();
        assert_eq!(b_search.len(), 1);
        assert_eq!(b_search[0].id, "vec_embedding_alpha");

        // 2. Insert another vector directly on Node B
        let header_b = VectorReplicationHeader {
            id: "vec_embedding_beta".to_string(),
            collection: "code_symbols".to_string(),
            dimension: 4,
            metadata: serde_json::json!({"symbol": "verify_signature", "file": "src/auth.rs"}).to_string(),
            content_hash: "hash_beta_456".to_string(),
            lamport_ts: 2,
            created_at: 2000,
            is_tombstone: false,
        };
        bridge_b.on_vector_replicated(&header_b, Some(&[0.9, 0.1, 0.0, 0.0]), "node_vec_b").await.unwrap();

        // 3. Query the entire mesh from Node A - must gather top-k nearest neighbors across all active nodes
        let federated_results = engine_a.federated_vector_query("code_symbols", &[1.0, 0.0, 0.0, 0.0], 5, 0.5).await.unwrap();
        assert!(!federated_results.is_empty());
        assert_eq!(federated_results[0].id, "vec_embedding_alpha");
        assert!(federated_results[0].score >= 0.99);

        // Verify top-2 results contain beta as well
        let ids: Vec<&str> = federated_results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"vec_embedding_alpha"));
        assert!(ids.contains(&"vec_embedding_beta"));

        engine_a.stop();
        engine_b.stop();
    }
}
