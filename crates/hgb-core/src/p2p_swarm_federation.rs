//! # Pillar 2: Distributed MicroVM Mesh & WASM Swarm Federation Engine
//!
//! Production-grade distributed compute federation engine enabling `hgb-daemon` and `hgb-cli`
//! nodes to form a self-organizing peer-to-peer compute mesh across local containers,
//! LAN developer workstations, and edge/cloud instances.

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

use crate::error::{HgbError, Result};
use crate::microvm_sandbox_executor::{EphemeralExecSpec, FirecrackerEphemeralSandbox};
use crate::microvm_swarm_orchestrator::{MicroVmSwarmOrchestrator, SwarmTask, TaskComplexity};
use crate::wasm_hot_pool::WasmHotPool;

pub const SWARM_FRAME_MAGIC: u32 = 0x48474246; // "HGBF"
pub const DEFAULT_FEDERATION_PORT: u16 = 8970;
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 5;
pub const DEFAULT_STALE_NODE_TIMEOUT_SECS: i64 = 15;
pub const OVERLOAD_LOAD_SCORE_THRESHOLD: f32 = 0.70;

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

        Self {
            node_id,
            listen_addr,
            available_cpu_cores,
            free_ram_mb,
            has_firecracker_kvm,
            has_wasm_engine,
            load_score: 0.10,
            last_heartbeat: Utc::now(),
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
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwarmPacket {
    Ping { from_node_id: String, timestamp: DateTime<Utc> },
    Pong { node: WorkerNodeAdvertisement },
    Heartbeat(WorkerNodeAdvertisement),
    TaskDispatch { task: SwarmFederationTask },
    TaskResult { result: SwarmFederationResult },
    Goodbye { node_id: String },
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
}

pub struct SwarmFederationEngine {
    node_id: String,
    listen_addr: SocketAddr,
    workspace_root: PathBuf,
    local_ad: Arc<RwLock<WorkerNodeAdvertisement>>,
    peer_registry: Arc<RwLock<HashMap<String, WorkerNodeAdvertisement>>>,
    orchestrator: Arc<MicroVmSwarmOrchestrator>,
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

        Ok(Self {
            node_id,
            listen_addr,
            workspace_root: ws,
            local_ad,
            peer_registry,
            orchestrator,
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

    pub async fn register_peer(&self, ad: WorkerNodeAdvertisement) {
        if ad.node_id == self.node_id { return; }
        let mut reg = self.peer_registry.write().await;
        reg.insert(ad.node_id.clone(), ad);
    }

    pub async fn remove_peer(&self, node_id: &str) -> Option<WorkerNodeAdvertisement> {
        let mut reg = self.peer_registry.write().await;
        reg.remove(node_id)
    }

    pub async fn prune_stale_nodes(&self, timeout_secs: i64) -> usize {
        let mut reg = self.peer_registry.write().await;
        let before = reg.len();
        reg.retain(|_, ad| !ad.is_stale(timeout_secs));
        before - reg.len()
    }

    pub async fn list_peers(&self) -> Vec<WorkerNodeAdvertisement> {
        let reg = self.peer_registry.read().await;
        reg.values().cloned().collect()
    }

    pub async fn local_advertisement(&self) -> WorkerNodeAdvertisement {
        let mut ad = self.local_ad.write().await;
        let in_flight = self
            .total_local_dispatched
            .load(Ordering::Relaxed)
            .saturating_sub(self.total_tasks_completed.load(Ordering::Relaxed)) as usize;
        ad.refresh_telemetry(in_flight);
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
        let packet = SwarmPacket::TaskDispatch { task: task.clone() };
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(task.timeout_ms.max(2000)),
            TcpStream::connect(target_node.listen_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout to peer {}", target_node.listen_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect to peer {}: {}", target_node.listen_addr, e)))?;

        let mut frame_header = Vec::with_capacity(8);
        frame_header.extend_from_slice(&SWARM_FRAME_MAGIC.to_be_bytes());
        frame_header.extend_from_slice(&(payload.len() as u32).to_be_bytes());

        stream.write_all(&frame_header).await.map_err(HgbError::Io)?;
        stream.write_all(&payload).await.map_err(HgbError::Io)?;
        stream.flush().await.map_err(HgbError::Io)?;

        let mut resp_magic = [0u8; 4];
        stream.read_exact(&mut resp_magic).await.map_err(HgbError::Io)?;
        if u32::from_be_bytes(resp_magic) != SWARM_FRAME_MAGIC {
            return Err(HgbError::Execution("Invalid swarm frame magic received from peer".to_string()));
        }

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;

        if resp_len > 32 * 1024 * 1024 {
            return Err(HgbError::Execution(format!("Response payload too large: {} bytes", resp_len)));
        }

        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).await.map_err(HgbError::Io)?;

        let response_packet: SwarmPacket = bincode::deserialize(&resp_buf)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match response_packet {
            SwarmPacket::TaskResult { result } => {
                self.total_tasks_completed.fetch_add(1, Ordering::Relaxed);
                self.total_duration_ms.fetch_add(result.duration_ms, Ordering::Relaxed);
                Ok(result)
            }
            other => Err(HgbError::Execution(format!("Unexpected packet response from peer: {:?}", other))),
        }
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

        self.is_running.store(true, Ordering::SeqCst);
        let engine = Arc::clone(self);

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
        let mut magic_buf = [0u8; 4];
        stream.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
        if u32::from_be_bytes(magic_buf) != SWARM_FRAME_MAGIC {
            return Err(HgbError::Execution("Invalid frame magic received".to_string()));
        }

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
        let payload_len = u32::from_be_bytes(len_buf) as usize;

        if payload_len > 32 * 1024 * 1024 {
            return Err(HgbError::Execution(format!("Payload too large: {} bytes", payload_len)));
        }

        let mut payload_buf = vec![0u8; payload_len];
        stream.read_exact(&mut payload_buf).await.map_err(HgbError::Io)?;

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
        }

        Ok(())
    }

    async fn send_packet_reply(&self, stream: &mut TcpStream, packet: SwarmPacket) -> Result<()> {
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;
        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&SWARM_FRAME_MAGIC.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        stream.write_all(&frame).await.map_err(HgbError::Io)?;
        stream.flush().await.map_err(HgbError::Io)?;
        Ok(())
    }

    pub async fn send_heartbeat_to_peer(&self, peer_addr: SocketAddr) -> Result<WorkerNodeAdvertisement> {
        let local_ad = self.local_advertisement().await;
        let packet = SwarmPacket::Heartbeat(local_ad);
        let payload = bincode::serialize(&packet).map_err(|e| HgbError::Execution(e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(2000),
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Heartbeat connection timeout to {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect for heartbeat to {}: {}", peer_addr, e)))?;

        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&SWARM_FRAME_MAGIC.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        stream.write_all(&frame).await.map_err(HgbError::Io)?;
        stream.flush().await.map_err(HgbError::Io)?;

        let mut magic_buf = [0u8; 4];
        stream.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
        if u32::from_be_bytes(magic_buf) != SWARM_FRAME_MAGIC {
            return Err(HgbError::Execution("Invalid frame magic received in heartbeat reply".to_string()));
        }

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;

        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).await.map_err(HgbError::Io)?;

        let reply: SwarmPacket = bincode::deserialize(&resp_buf)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match reply {
            SwarmPacket::Heartbeat(peer_ad) | SwarmPacket::Pong { node: peer_ad } => {
                self.register_peer(peer_ad.clone()).await;
                Ok(peer_ad)
            }
            other => Err(HgbError::Execution(format!("Unexpected heartbeat reply: {:?}", other))),
        }
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
        }
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

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
}

