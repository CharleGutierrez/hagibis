//! # Pillar 2: Distributed Micro-VM & WASM Edge Mesh Scheduler
//!
//! Production-grade distributed edge scheduler and mesh orchestrator enabling `hgb-core`,
//! `hgb-daemon`, and worker nodes across LAN/mesh networks to:
//! 1. Discover cluster daemons and worker nodes via UDP broadcast (port 8976) and TCP handshake
//!    with automatic telemetry heartbeat refreshes and stale peer pruning.
//! 2. Route task shards dynamically across a two-tier execution sandbox:
//!    - **Tier 1 (Fast/Light)**: Sub-millisecond untrusted code and WASM tool snippets executing
//!      in Wasmtime / Zig WASM sandbox with deterministic CPU instruction fuel metering.
//!    - **Tier 2 (Heavy/Secure)**: Hardware-isolated Linux KVM Firecracker micro-VM instances
//!      (or rootless Linux Landlock LSM + RAM CoW micro-jails in `/dev/shm`) for heavy autonomous
//!      agent loops, shell script execution, or compilation.
//! 3. Intelligently shard and stream multi-stage execution graphs (`DistributedTaskGraph`) across
//!    cluster nodes based on real-time load scores, thermal headroom, memory availability, and tier preferences
//!    (scheduling locally if local host is optimal).
//! 4. Resilient Aggregator: Gathers shard outcomes, handles peer communication timeouts with
//!    automatic failover and local/peer re-dispatch, and produces unified execution reports
//!    (`DistributedExecutionReport`) sealed with BLAKE3 cryptographic provenance.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use blake3::Hasher;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::RwLock;

use crate::dynatune_bridge::DynatuneBridge;
use crate::error::{HgbError, Result};
use crate::multi_tier_sandbox::{
    MultiTierSandboxRouter, SandboxExecutionRequest, SandboxExecutionTier,
};

/// Magic frame header identifying Hagibis Distributed Scheduler packets ("HGDS")
pub const DISTRIBUTED_FRAME_MAGIC: u32 = 0x48474453;
/// Default UDP port for cluster peer multicast/broadcast discovery
pub const DEFAULT_SCHEDULER_UDP_PORT: u16 = 8976;
/// Default TCP port for peer-to-peer shard dispatch and telemetry handshakes
pub const DEFAULT_SCHEDULER_TCP_PORT: u16 = 8977;
/// Default timeout threshold in seconds after which inactive nodes are pruned
pub const DEFAULT_STALE_NODE_TIMEOUT_SECS: i64 = 15;
/// Default CPU fuel limit for Tier 1 sandboxed WebAssembly execution
pub const DEFAULT_SHARD_FUEL_LIMIT: u64 = 10_000_000;
/// Maximum scheduler network packet frame size (32 MiB)
pub const MAX_SCHEDULER_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Reads free/available RAM from /proc/meminfo on Linux, fallback to 4096 MB
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

/// Multi-node advertisement descriptor representing a worker node in the edge mesh
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributedNodeDescriptor {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub supported_tiers: Vec<SandboxExecutionTier>,
    pub cpu_cores: usize,
    pub free_ram_mb: u64,
    pub active_instances: usize,
    pub load_score: f32,
    pub last_heartbeat: DateTime<Utc>,
}

impl DistributedNodeDescriptor {
    /// Initialize a new node descriptor with auto-detected hardware attributes
    pub fn new(
        node_id: impl Into<String>,
        listen_addr: SocketAddr,
        supported_tiers: Vec<SandboxExecutionTier>,
    ) -> Self {
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let free_ram_mb = read_free_ram_mb();

        Self {
            node_id: node_id.into(),
            listen_addr,
            supported_tiers,
            cpu_cores,
            free_ram_mb,
            active_instances: 0,
            load_score: 0.10,
            last_heartbeat: Utc::now(),
        }
    }

    /// Check if node heartbeat has expired beyond the stale threshold
    pub fn is_stale(&self, timeout_secs: i64) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_heartbeat);
        elapsed.num_seconds() > timeout_secs
    }

    /// Refresh node telemetry metrics and dynamically recompute load score
    pub fn refresh_telemetry(&mut self, active_instances: usize, temp_c: Option<f32>) {
        self.active_instances = active_instances;
        self.free_ram_mb = read_free_ram_mb();
        self.cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        self.last_heartbeat = Utc::now();

        self.load_score = Self::calculate_load_score(
            active_instances,
            self.free_ram_mb,
            temp_c,
        );
    }

    /// Computes dynamic composite load score factoring active instances, RAM pressure, and thermal headroom
    pub fn calculate_load_score(
        active_instances: usize,
        free_ram_mb: u64,
        temp_c: Option<f32>,
    ) -> f32 {
        let instance_factor = (active_instances as f32) * 0.15;
        let ram_factor = if free_ram_mb < 512 {
            0.50
        } else if free_ram_mb < 1024 {
            0.25
        } else {
            0.05
        };

        let thermal_factor = if let Some(temp) = temp_c {
            if temp >= 78.0 {
                0.60
            } else if temp >= 70.0 {
                0.30
            } else {
                0.0
            }
        } else {
            0.0
        };

        (instance_factor + ram_factor + thermal_factor).clamp(0.05, 3.0)
    }

    /// Check if this node is capable of executing the requested sandbox tier
    pub fn supports_tier(&self, tier: SandboxExecutionTier) -> bool {
        self.supported_tiers.contains(&tier)
    }
}

/// A single partitioned computation shard within a distributed task graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskShard {
    pub shard_id: String,
    pub task_name: String,
    pub code: String,
    pub language: String,
    pub tier_preference: Option<SandboxExecutionTier>,
    pub fuel_limit: Option<u64>,
    pub memory_limit_mb: Option<usize>,
    pub timeout_ms: u64,
    pub dependencies: Vec<String>,
    pub input_payload: serde_json::Value,
}

impl TaskShard {
    /// Construct a new task shard
    pub fn new(
        shard_id: impl Into<String>,
        code: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        let s_id = shard_id.into();
        Self {
            shard_id: s_id.clone(),
            task_name: s_id,
            code: code.into(),
            language: language.into(),
            tier_preference: None,
            fuel_limit: Some(DEFAULT_SHARD_FUEL_LIMIT),
            memory_limit_mb: Some(128),
            timeout_ms: 10_000,
            dependencies: Vec::new(),
            input_payload: serde_json::Value::Null,
        }
    }

    pub fn with_task_name(mut self, name: impl Into<String>) -> Self {
        self.task_name = name.into();
        self
    }

    pub fn with_tier(mut self, tier: SandboxExecutionTier) -> Self {
        self.tier_preference = Some(tier);
        self
    }

    pub fn with_fuel_limit(mut self, limit: u64) -> Self {
        self.fuel_limit = Some(limit);
        self
    }

    pub fn with_memory_limit_mb(mut self, mb: usize) -> Self {
        self.memory_limit_mb = Some(mb);
        self
    }

    pub fn with_memory_limit(self, mb: usize) -> Self {
        self.with_memory_limit_mb(mb)
    }

    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    pub fn with_timeout(self, ms: u64) -> Self {
        self.with_timeout_ms(ms)
    }

    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn add_dependency(mut self, dep: impl Into<String>) -> Self {
        self.dependencies.push(dep.into());
        self
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.input_payload = payload;
        self
    }

    /// Compute cryptographic BLAKE3 checksum covering shard payload and parameters
    pub fn compute_checksum(&self) -> [u8; 32] {
        let mut hasher = Hasher::new();
        hasher.update(self.shard_id.as_bytes());
        hasher.update(self.task_name.as_bytes());
        hasher.update(self.code.as_bytes());
        hasher.update(self.language.as_bytes());
        if let Some(tier) = self.tier_preference {
            hasher.update(format!("{:?}", tier).as_bytes());
        }
        if let Some(fuel) = self.fuel_limit {
            hasher.update(&fuel.to_le_bytes());
        }
        if let Some(mem) = self.memory_limit_mb {
            hasher.update(&(mem as u64).to_le_bytes());
        }
        hasher.update(&self.timeout_ms.to_le_bytes());
        for dep in &self.dependencies {
            hasher.update(dep.as_bytes());
        }
        if let Ok(serialized) = serde_json::to_vec(&self.input_payload) {
            hasher.update(&serialized);
        }
        *hasher.finalize().as_bytes()
    }

    /// Hex-encoded BLAKE3 checksum
    pub fn checksum_hex(&self) -> String {
        hex::encode(self.compute_checksum())
    }
}

/// Directed acyclic graph of interdependent or concurrent task shards
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedTaskGraph {
    pub graph_id: String,
    pub shards: Vec<TaskShard>,
}

impl DistributedTaskGraph {
    pub fn new(graph_id: impl Into<String>) -> Self {
        Self {
            graph_id: graph_id.into(),
            shards: Vec::new(),
        }
    }

    pub fn add_shard(mut self, shard: TaskShard) -> Self {
        self.shards.push(shard);
        self
    }

    /// Decomposes the task graph into topologically sorted concurrency stages.
    /// Shards within the same stage have zero mutual dependencies and can execute in parallel.
    pub fn decompose_into_stages(&self) -> Result<Vec<Vec<TaskShard>>> {
        let mut in_degrees: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        let mut shard_map: HashMap<String, TaskShard> = HashMap::new();

        for shard in &self.shards {
            shard_map.insert(shard.shard_id.clone(), shard.clone());
            in_degrees.insert(shard.shard_id.clone(), shard.dependencies.len());
            for dep in &shard.dependencies {
                adj.entry(dep.clone()).or_default().push(shard.shard_id.clone());
            }
        }

        let mut current_stage: Vec<String> = in_degrees
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(id, _)| id.clone())
            .collect();

        current_stage.sort();

        let mut stages = Vec::new();
        let mut visited_count = 0;

        while !current_stage.is_empty() {
            let mut stage_shards = Vec::new();
            let mut next_stage = Vec::new();

            for shard_id in &current_stage {
                visited_count += 1;
                if let Some(shard) = shard_map.get(shard_id) {
                    stage_shards.push(shard.clone());
                }

                if let Some(dependents) = adj.get(shard_id) {
                    for dep in dependents {
                        if let Some(deg) = in_degrees.get_mut(dep) {
                            *deg = deg.saturating_sub(1);
                            if *deg == 0 {
                                next_stage.push(dep.clone());
                            }
                        }
                    }
                }
            }

            stages.push(stage_shards);
            next_stage.sort();
            current_stage = next_stage;
        }

        if visited_count < self.shards.len() {
            return Err(HgbError::Execution(
                "Cyclic dependency detected in distributed task graph".to_string(),
            ));
        }

        Ok(stages)
    }
}

/// Execution outcome of an individual task shard
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShardExecutionOutcome {
    pub shard_id: String,
    pub executing_node_id: String,
    pub tier_used: SandboxExecutionTier,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub memory_bytes_used: u64,
    pub fuel_consumed: Option<u64>,
    pub blake3_digest: String,
}

impl ShardExecutionOutcome {
    /// Computes cryptographic BLAKE3 digest of the shard execution outcome
    pub fn compute_digest(
        shard_id: &str,
        executing_node_id: &str,
        tier_used: SandboxExecutionTier,
        exit_code: i32,
        stdout: &str,
        stderr: &str,
    ) -> String {
        let mut hasher = Hasher::new();
        hasher.update(shard_id.as_bytes());
        hasher.update(executing_node_id.as_bytes());
        hasher.update(format!("{:?}", tier_used).as_bytes());
        hasher.update(&exit_code.to_le_bytes());
        hasher.update(stdout.as_bytes());
        hasher.update(stderr.as_bytes());
        hasher.finalize().to_hex().to_string()
    }
}

/// Comprehensive report of distributed multi-node task graph execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedExecutionReport {
    pub graph_id: String,
    pub success: bool,
    pub total_shards: usize,
    pub succeeded_shards: usize,
    pub failed_shards: usize,
    pub total_duration_ms: u64,
    pub total_fuel_consumed: u64,
    pub shard_outcomes: Vec<ShardExecutionOutcome>,
    pub node_dispatch_map: HashMap<String, Vec<String>>,
    pub failover_events: Vec<String>,
    pub blake3_provenance: String,
}

impl DistributedExecutionReport {
    /// Computes unified BLAKE3 provenance hash across the entire graph execution
    pub fn compute_provenance(
        graph_id: &str,
        outcomes: &[ShardExecutionOutcome],
        total_duration_ms: u64,
        total_fuel: u64,
        failover_count: usize,
    ) -> String {
        let mut hasher = Hasher::new();
        hasher.update(graph_id.as_bytes());
        for outcome in outcomes {
            hasher.update(outcome.shard_id.as_bytes());
            hasher.update(outcome.blake3_digest.as_bytes());
            hasher.update(&outcome.exit_code.to_le_bytes());
        }
        hasher.update(&total_duration_ms.to_le_bytes());
        hasher.update(&total_fuel.to_le_bytes());
        hasher.update(&(failover_count as u64).to_le_bytes());
        hasher.finalize().to_hex().to_string()
    }
}

/// Wire protocol packet exchanged across the distributed scheduler mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulerPacket {
    DiscoveryPing {
        from_node_id: String,
        listen_addr: SocketAddr,
        timestamp: DateTime<Utc>,
    },
    DiscoveryPong(DistributedNodeDescriptor),
    Heartbeat(DistributedNodeDescriptor),
    ShardDispatch {
        shard: TaskShard,
        checksum: [u8; 32],
    },
    ShardOutcome {
        outcome: ShardExecutionOutcome,
    },
    Goodbye {
        node_id: String,
    },
}

/// Serializes and writes a framed packet over an asynchronous writer
pub async fn write_framed_packet<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    packet: &SchedulerPacket,
) -> Result<()> {
    let payload = serde_json::to_vec(packet)
        .map_err(|e| HgbError::Execution(format!("Serialization error: {}", e)))?;

    let mut header = [0u8; 8];
    header[0..4].copy_from_slice(&DISTRIBUTED_FRAME_MAGIC.to_be_bytes());
    header[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());

    writer.write_all(&header).await.map_err(HgbError::Io)?;
    writer.write_all(&payload).await.map_err(HgbError::Io)?;
    writer.flush().await.map_err(HgbError::Io)?;
    Ok(())
}

/// Reads and deserializes a framed packet from an asynchronous reader
pub async fn read_framed_packet<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<SchedulerPacket> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).await.map_err(HgbError::Io)?;

    let magic = u32::from_be_bytes(header[0..4].try_into().unwrap());
    if magic != DISTRIBUTED_FRAME_MAGIC {
        return Err(HgbError::Execution(format!(
            "Invalid scheduler frame magic 0x{:08X}, expected 0x{:08X}",
            magic, DISTRIBUTED_FRAME_MAGIC
        )));
    }

    let len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    if len > MAX_SCHEDULER_FRAME_BYTES {
        return Err(HgbError::Execution(format!(
            "Packet frame length {} exceeds max allowed {}",
            len, MAX_SCHEDULER_FRAME_BYTES
        )));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await.map_err(HgbError::Io)?;

    let packet: SchedulerPacket = serde_json::from_slice(&buf)
        .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;
    Ok(packet)
}

/// Distributed Micro-VM & WASM Edge Mesh Scheduler
pub struct DistributedMicroVmScheduler {
    node_id: String,
    listen_addr: SocketAddr,
    workspace_root: PathBuf,
    supported_tiers: Vec<SandboxExecutionTier>,
    local_descriptor: Arc<RwLock<DistributedNodeDescriptor>>,
    peer_registry: Arc<RwLock<HashMap<String, DistributedNodeDescriptor>>>,
    active_instances: Arc<AtomicUsize>,
    total_dispatched: Arc<AtomicU64>,
    total_succeeded: Arc<AtomicU64>,
    total_failed: Arc<AtomicU64>,
    total_fuel_consumed: Arc<AtomicU64>,
    sandbox_router: Arc<MultiTierSandboxRouter>,
    dynatune_bridge: Arc<DynatuneBridge>,
    is_running: Arc<AtomicBool>,
}

impl DistributedMicroVmScheduler {
    /// Initialize a new distributed scheduler with default dual-tier support
    pub fn new(
        node_id: impl Into<String>,
        listen_addr: SocketAddr,
        workspace_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let tiers = vec![
            SandboxExecutionTier::Tier1WasmZig,
            SandboxExecutionTier::Tier2FirecrackerMicroVm,
        ];
        Self::with_tiers(node_id, listen_addr, workspace_root, tiers)
    }

    /// Initialize a distributed scheduler with custom supported execution tiers
    pub fn with_tiers(
        node_id: impl Into<String>,
        listen_addr: SocketAddr,
        workspace_root: impl Into<PathBuf>,
        supported_tiers: Vec<SandboxExecutionTier>,
    ) -> Result<Self> {
        let node_id = node_id.into();
        let ws = workspace_root.into();
        let dynatune_bridge = Arc::new(DynatuneBridge::new());
        let sandbox_router = Arc::new(MultiTierSandboxRouter::with_bridge(&ws, Arc::clone(&dynatune_bridge)));

        let local_desc = DistributedNodeDescriptor::new(
            node_id.clone(),
            listen_addr,
            supported_tiers.clone(),
        );

        Ok(Self {
            node_id,
            listen_addr,
            workspace_root: ws,
            supported_tiers,
            local_descriptor: Arc::new(RwLock::new(local_desc)),
            peer_registry: Arc::new(RwLock::new(HashMap::new())),
            active_instances: Arc::new(AtomicUsize::new(0)),
            total_dispatched: Arc::new(AtomicU64::new(0)),
            total_succeeded: Arc::new(AtomicU64::new(0)),
            total_failed: Arc::new(AtomicU64::new(0)),
            total_fuel_consumed: Arc::new(AtomicU64::new(0)),
            sandbox_router,
            dynatune_bridge,
            is_running: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn supported_tiers(&self) -> &[SandboxExecutionTier] {
        &self.supported_tiers
    }

    /// Register or update a remote peer node in the active registry
    pub async fn register_peer(&self, desc: DistributedNodeDescriptor) {
        if desc.node_id == self.node_id {
            return;
        }
        let mut reg = self.peer_registry.write().await;
        reg.insert(desc.node_id.clone(), desc);
    }

    /// Manually remove a peer node from the registry
    pub async fn remove_peer(&self, node_id: &str) -> Option<DistributedNodeDescriptor> {
        let mut reg = self.peer_registry.write().await;
        reg.remove(node_id)
    }

    /// List all currently registered peer nodes
    pub async fn list_peers(&self) -> Vec<DistributedNodeDescriptor> {
        let reg = self.peer_registry.read().await;
        reg.values().cloned().collect()
    }

    /// Prune inactive peer nodes exceeding the stale timeout threshold
    pub async fn prune_stale_nodes(&self, timeout_secs: i64) -> usize {
        let mut reg = self.peer_registry.write().await;
        let initial_len = reg.len();
        reg.retain(|_, desc| !desc.is_stale(timeout_secs));
        initial_len - reg.len()
    }

    /// Obtain live updated descriptor for the local node
    pub async fn local_descriptor(&self) -> DistributedNodeDescriptor {
        let mut desc = self.local_descriptor.write().await;
        let active = self.active_instances.load(Ordering::Relaxed);
        let telem = self.dynatune_bridge.read_complete_hardware_telemetry();
        let temp = (telem.cpu_pkg_temp_c.max(telem.max_temp_c) as f32).max(0.0);
        desc.refresh_telemetry(active, Some(temp));
        desc.clone()
    }

    /// Generate comprehensive telemetry status JSON for the scheduler
    pub async fn status(&self) -> serde_json::Value {
        let local_desc = self.local_descriptor().await;
        let peers = self.list_peers().await;

        serde_json::json!({
            "status": "active",
            "node_id": self.node_id,
            "listen_addr": self.listen_addr.to_string(),
            "local_node": local_desc,
            "peer_count": peers.len(),
            "peers": peers,
            "metrics": {
                "active_instances": self.active_instances.load(Ordering::Relaxed),
                "total_dispatched": self.total_dispatched.load(Ordering::Relaxed),
                "total_succeeded": self.total_succeeded.load(Ordering::Relaxed),
                "total_failed": self.total_failed.load(Ordering::Relaxed),
                "total_fuel_consumed": self.total_fuel_consumed.load(Ordering::Relaxed),
            }
        })
    }

    /// Execute a shard locally using the MultiTierSandboxRouter (Tier 1 WASM or Tier 2 MicroVM)
    pub fn execute_shard_locally(&self, shard: &TaskShard) -> Result<ShardExecutionOutcome> {
        let start = Instant::now();
        self.active_instances.fetch_add(1, Ordering::SeqCst);

        let mut req = SandboxExecutionRequest::new(&shard.code, &shard.language)
            .with_timeout_ms(shard.timeout_ms);

        if let Some(tier) = shard.tier_preference {
            req = req.with_tier_preference(tier);
        }
        if let Some(fuel) = shard.fuel_limit {
            req = req.with_fuel_limit(fuel);
        }
        if let Some(mem) = shard.memory_limit_mb {
            req = req.with_memory_limit_mb(mem);
        }

        let exec_res = self.sandbox_router.execute(&req);
        self.active_instances.fetch_sub(1, Ordering::SeqCst);

        match exec_res {
            Ok(outcome) => {
                let success = match outcome.tier_used {
                    SandboxExecutionTier::Tier1WasmZig => outcome.exit_code >= 0,
                    SandboxExecutionTier::Tier2FirecrackerMicroVm => outcome.exit_code == 0,
                };
                if success {
                    self.total_succeeded.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.total_failed.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(fuel) = outcome.fuel_consumed {
                    self.total_fuel_consumed.fetch_add(fuel, Ordering::Relaxed);
                }
                self.total_dispatched.fetch_add(1, Ordering::Relaxed);

                let digest = ShardExecutionOutcome::compute_digest(
                    &shard.shard_id,
                    &self.node_id,
                    outcome.tier_used,
                    outcome.exit_code,
                    &outcome.stdout,
                    &outcome.stderr,
                );

                Ok(ShardExecutionOutcome {
                    shard_id: shard.shard_id.clone(),
                    executing_node_id: self.node_id.clone(),
                    tier_used: outcome.tier_used,
                    success,
                    exit_code: outcome.exit_code,
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                    duration_ms: outcome.duration_ms.max(start.elapsed().as_millis() as u64),
                    memory_bytes_used: outcome.memory_bytes_used,
                    fuel_consumed: outcome.fuel_consumed,
                    blake3_digest: digest,
                })
            }
            Err(e) => {
                self.total_failed.fetch_add(1, Ordering::Relaxed);
                self.total_dispatched.fetch_add(1, Ordering::Relaxed);

                let err_msg = format!("Shard execution error: {}", e);
                let fallback_tier = shard.tier_preference.unwrap_or(SandboxExecutionTier::Tier1WasmZig);
                let digest = ShardExecutionOutcome::compute_digest(
                    &shard.shard_id,
                    &self.node_id,
                    fallback_tier,
                    1,
                    "",
                    &err_msg,
                );

                Ok(ShardExecutionOutcome {
                    shard_id: shard.shard_id.clone(),
                    executing_node_id: self.node_id.clone(),
                    tier_used: fallback_tier,
                    success: false,
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: err_msg,
                    duration_ms: start.elapsed().as_millis() as u64,
                    memory_bytes_used: 0,
                    fuel_consumed: None,
                    blake3_digest: digest,
                })
            }
        }
    }

    /// Dispatch task shard to remote peer over TCP connection with BLAKE3 checksum verification
    pub async fn dispatch_shard_to_peer(
        &self,
        peer_addr: SocketAddr,
        shard: &TaskShard,
    ) -> Result<ShardExecutionOutcome> {
        let checksum = shard.compute_checksum();
        let packet = SchedulerPacket::ShardDispatch {
            shard: shard.clone(),
            checksum,
        };

        let timeout_duration = Duration::from_millis(shard.timeout_ms.clamp(2000, 30_000));
        let mut stream = tokio::time::timeout(
            timeout_duration,
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout to peer {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect to peer {}: {}", peer_addr, e)))?;

        write_framed_packet(&mut stream, &packet).await?;
        let response = read_framed_packet(&mut stream).await?;

        match response {
            SchedulerPacket::ShardOutcome { outcome } => {
                // Verify returned outcome belongs to the dispatched shard
                if outcome.shard_id != shard.shard_id {
                    return Err(HgbError::Execution(format!(
                        "Mismatched shard ID returned from peer: expected {}, got {}",
                        shard.shard_id, outcome.shard_id
                    )));
                }
                Ok(outcome)
            }
            other => Err(HgbError::Execution(format!(
                "Unexpected response packet from peer: {:?}",
                other
            ))),
        }
    }

    /// Selects optimal candidate nodes supporting the required execution tier,
    /// sorted by lowest load score and highest available RAM
    pub async fn rank_nodes_for_tier(
        &self,
        tier: SandboxExecutionTier,
    ) -> Vec<DistributedNodeDescriptor> {
        let local_desc = self.local_descriptor().await;
        let peers = self.list_peers().await;

        let mut candidates = Vec::new();
        if local_desc.supports_tier(tier) {
            candidates.push(local_desc.clone());
        }

        for peer in peers {
            if peer.supports_tier(tier) {
                candidates.push(peer);
            }
        }

        if candidates.is_empty() {
            candidates.push(local_desc);
        }

        candidates.sort_by(|a, b| {
            a.load_score
                .partial_cmp(&b.load_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.free_ram_mb.cmp(&a.free_ram_mb))
        });

        candidates
    }

    /// Dispatch a single shard with intelligent node routing and automated failover re-dispatch
    pub async fn dispatch_shard_with_failover(
        self: &Arc<Self>,
        shard: &TaskShard,
    ) -> Result<(ShardExecutionOutcome, String, Vec<String>)> {
        let mut failovers = Vec::new();

        // Determine required or preferred execution tier
        let required_tier = shard.tier_preference.unwrap_or_else(|| {
            let req = SandboxExecutionRequest::new(&shard.code, &shard.language);
            self.sandbox_router.analyze_complexity(&req)
        });

        let candidates = self.rank_nodes_for_tier(required_tier).await;
        let local_desc = self.local_descriptor().await;

        // Iterate through candidates in order of preference
        for candidate in candidates {
            // Local execution path
            if candidate.node_id == self.node_id {
                let outcome = self.execute_shard_locally(shard)?;
                return Ok((outcome, self.node_id.clone(), failovers));
            }

            // Remote execution path
            match self.dispatch_shard_to_peer(candidate.listen_addr, shard).await {
                Ok(outcome) => {
                    return Ok((outcome, candidate.node_id, failovers));
                }
                Err(e) => {
                    let failover_msg = format!(
                        "Peer {} ({}) failed to execute shard '{}': {}. Re-routing to next candidate node.",
                        candidate.node_id, candidate.listen_addr, shard.shard_id, e
                    );
                    failovers.push(failover_msg);
                }
            }
        }

        // Final fallback: execute locally on host
        let fallback_msg = format!(
            "All remote candidate nodes exhausted for shard '{}'. Initiating fallback execution on local node {}.",
            shard.shard_id, local_desc.node_id
        );
        failovers.push(fallback_msg);

        let outcome = self.execute_shard_locally(shard)?;
        Ok((outcome, self.node_id.clone(), failovers))
    }

    /// Dispatches a multi-stage, dual-tier task graph across the distributed mesh.
    /// Decomposes shards into topological concurrency stages, streams parallel execution,
    /// and aggregates outcomes with BLAKE3 cryptographic provenance.
    pub async fn dispatch_task_graph(
        self: &Arc<Self>,
        graph: &DistributedTaskGraph,
    ) -> Result<DistributedExecutionReport> {
        let start = Instant::now();
        let _ = self.prune_stale_nodes(DEFAULT_STALE_NODE_TIMEOUT_SECS).await;

        let stages = graph.decompose_into_stages()?;
        let total_shards = graph.shards.len();

        let mut all_outcomes = Vec::with_capacity(total_shards);
        let mut node_dispatch_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_failovers = Vec::new();
        let mut succeeded_count = 0;
        let mut failed_count = 0;
        let mut total_fuel = 0;

        for stage in stages {
            let mut stage_handles = Vec::with_capacity(stage.len());

            for shard in stage {
                let scheduler = Arc::clone(self);
                stage_handles.push(tokio::spawn(async move {
                    scheduler.dispatch_shard_with_failover(&shard).await
                }));
            }

            for handle in stage_handles {
                let (outcome, executing_node, failover_logs) = handle
                    .await
                    .map_err(|e| HgbError::Execution(format!("Task shard thread panic: {}", e)))??;

                if outcome.success {
                    succeeded_count += 1;
                } else {
                    failed_count += 1;
                }

                if let Some(fuel) = outcome.fuel_consumed {
                    total_fuel += fuel;
                }

                node_dispatch_map
                    .entry(executing_node)
                    .or_default()
                    .push(outcome.shard_id.clone());
                all_failovers.extend(failover_logs);
                all_outcomes.push(outcome);
            }
        }

        let total_duration_ms = start.elapsed().as_millis() as u64;
        let blake3_provenance = DistributedExecutionReport::compute_provenance(
            &graph.graph_id,
            &all_outcomes,
            total_duration_ms,
            total_fuel,
            all_failovers.len(),
        );

        let overall_success = failed_count == 0 && succeeded_count == total_shards;

        Ok(DistributedExecutionReport {
            graph_id: graph.graph_id.clone(),
            success: overall_success,
            total_shards,
            succeeded_shards: succeeded_count,
            failed_shards: failed_count,
            total_duration_ms,
            total_fuel_consumed: total_fuel,
            shard_outcomes: all_outcomes,
            node_dispatch_map,
            failover_events: all_failovers,
            blake3_provenance,
        })
    }

    /// Start TCP listener for receiving inbound peer shard dispatches and discovery handshakes
    pub async fn start_tcp_listener(self: &Arc<Self>) -> Result<tokio::task::JoinHandle<()>> {
        let listener = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            HgbError::Execution(format!(
                "Failed to bind TCP scheduler listener on {}: {}",
                self.listen_addr, e
            ))
        })?;

        self.is_running.store(true, Ordering::SeqCst);
        let scheduler = Arc::clone(self);

        let handle = tokio::spawn(async move {
            while scheduler.is_running.load(Ordering::Relaxed) {
                if let Ok((mut stream, _client_addr)) = listener.accept().await {
                    let sched = Arc::clone(&scheduler);
                    tokio::spawn(async move {
                        match read_framed_packet(&mut stream).await {
                            Ok(packet) => {
                                match packet {
                                    SchedulerPacket::ShardDispatch { shard, checksum } => {
                                        let expected_checksum = shard.compute_checksum();
                                        if expected_checksum == checksum {
                                            let outcome = sched.execute_shard_locally(&shard)
                                                .unwrap_or_else(|e| ShardExecutionOutcome {
                                                    shard_id: shard.shard_id.clone(),
                                                    executing_node_id: sched.node_id.clone(),
                                                    tier_used: shard.tier_preference.unwrap_or(SandboxExecutionTier::Tier1WasmZig),
                                                    success: false,
                                                    exit_code: 1,
                                                    stdout: String::new(),
                                                    stderr: e.to_string(),
                                                    duration_ms: 0,
                                                    memory_bytes_used: 0,
                                                    fuel_consumed: None,
                                                    blake3_digest: String::new(),
                                                });

                                            let resp = SchedulerPacket::ShardOutcome { outcome };
                                            let _ = write_framed_packet(&mut stream, &resp).await;
                                        } else {
                                            eprintln!("CHECKSUM MISMATCH! expected {:?} vs got {:?}", expected_checksum, checksum);
                                        }
                                    }
                                    SchedulerPacket::DiscoveryPing { .. } => {
                                        let local_desc = sched.local_descriptor().await;
                                        let resp = SchedulerPacket::DiscoveryPong(local_desc);
                                        let _ = write_framed_packet(&mut stream, &resp).await;
                                    }
                                    SchedulerPacket::Heartbeat(desc) => {
                                        sched.register_peer(desc).await;
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                eprintln!("SERVER read_framed_packet error: {:?}", e);
                            }
                        }
                    });
                }
            }
        });

        Ok(handle)
    }

    /// Start UDP multicast/broadcast peer discovery service on designated port
    pub async fn start_udp_discovery(
        self: &Arc<Self>,
        udp_port: u16,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let bind_addr: SocketAddr = format!("0.0.0.0:{}", udp_port).parse().unwrap();
        let socket = UdpSocket::bind(bind_addr).await.map_err(|e| {
            HgbError::Execution(format!("Failed to bind UDP discovery socket on {}: {}", bind_addr, e))
        })?;
        let _ = socket.set_broadcast(true);

        self.is_running.store(true, Ordering::SeqCst);
        let scheduler = Arc::clone(self);

        let handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            while scheduler.is_running.load(Ordering::Relaxed) {
                if let Ok((len, sender_addr)) = socket.recv_from(&mut buf).await {
                    if let Ok(packet) = bincode::deserialize::<SchedulerPacket>(&buf[..len]) {
                        match packet {
                            SchedulerPacket::DiscoveryPing { from_node_id, .. } => {
                                if from_node_id != scheduler.node_id {
                                    let local_desc = scheduler.local_descriptor().await;
                                    let pong = SchedulerPacket::DiscoveryPong(local_desc);
                                    if let Ok(resp_bytes) = bincode::serialize(&pong) {
                                        let _ = socket.send_to(&resp_bytes, sender_addr).await;
                                    }
                                }
                            }
                            SchedulerPacket::DiscoveryPong(desc) | SchedulerPacket::Heartbeat(desc) => {
                                if desc.node_id != scheduler.node_id {
                                    scheduler.register_peer(desc).await;
                                }
                            }
                            SchedulerPacket::Goodbye { node_id } => {
                                let _ = scheduler.remove_peer(&node_id).await;
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        Ok(handle)
    }

    /// Broadcast UDP discovery ping across local subnet
    pub async fn broadcast_discovery_ping(&self, udp_port: u16) -> Result<()> {
        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let socket = UdpSocket::bind(bind_addr).await.map_err(HgbError::Io)?;
        socket.set_broadcast(true).map_err(HgbError::Io)?;

        let ping = SchedulerPacket::DiscoveryPing {
            from_node_id: self.node_id.clone(),
            listen_addr: self.listen_addr,
            timestamp: Utc::now(),
        };

        let target_addr: SocketAddr = format!("255.255.255.255:{}", udp_port).parse().unwrap();
        let payload = bincode::serialize(&ping)
            .map_err(|e| HgbError::Execution(format!("Serialization error: {}", e)))?;

        let _ = socket.send_to(&payload, target_addr).await.map_err(HgbError::Io)?;
        Ok(())
    }

    /// Gracefully shutdown scheduler background services
    pub fn shutdown(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_node_descriptor_telemetry_and_stale_pruning() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addr: SocketAddr = "127.0.0.1:18977".parse().unwrap();
        let scheduler = DistributedMicroVmScheduler::new("node_alpha", addr, temp_dir.path()).unwrap();

        // 1. Verify local descriptor defaults
        let local_desc = scheduler.local_descriptor().await;
        assert_eq!(local_desc.node_id, "node_alpha");
        assert_eq!(local_desc.listen_addr, addr);
        assert!(local_desc.cpu_cores > 0);
        assert!(local_desc.free_ram_mb > 0);
        assert_eq!(local_desc.supported_tiers.len(), 2);
        assert!(local_desc.supports_tier(SandboxExecutionTier::Tier1WasmZig));
        assert!(local_desc.supports_tier(SandboxExecutionTier::Tier2FirecrackerMicroVm));

        // 2. Register peers
        let peer1_addr: SocketAddr = "192.168.1.101:8977".parse().unwrap();
        let mut peer1 = DistributedNodeDescriptor::new(
            "node_beta",
            peer1_addr,
            vec![SandboxExecutionTier::Tier1WasmZig],
        );
        scheduler.register_peer(peer1.clone()).await;

        let peer2_addr: SocketAddr = "192.168.1.102:8977".parse().unwrap();
        let mut peer2 = DistributedNodeDescriptor::new(
            "node_gamma",
            peer2_addr,
            vec![SandboxExecutionTier::Tier2FirecrackerMicroVm],
        );
        // Make peer2 stale
        peer2.last_heartbeat = Utc::now() - chrono::Duration::seconds(30);
        scheduler.register_peer(peer2.clone()).await;

        let peers = scheduler.list_peers().await;
        assert_eq!(peers.len(), 2);

        // 3. Prune stale nodes (timeout = 15s)
        let pruned = scheduler.prune_stale_nodes(15).await;
        assert_eq!(pruned, 1);

        let active_peers = scheduler.list_peers().await;
        assert_eq!(active_peers.len(), 1);
        assert_eq!(active_peers[0].node_id, "node_beta");

        // 4. Verify telemetry refresh modifies load score
        peer1.refresh_telemetry(4, Some(75.0));
        assert!(peer1.load_score > 0.5);
    }

    #[test]
    fn test_task_graph_sharding_and_topological_sort() {
        let mut graph = DistributedTaskGraph::new("workflow_01");

        let shard_a = TaskShard::new("shard_a", "echo 'Step A'", "bash");
        let shard_b = TaskShard::new("shard_b", "echo 'Step B'", "bash")
            .with_dependencies(vec!["shard_a".to_string()]);
        let shard_c = TaskShard::new("shard_c", "echo 'Step C'", "bash")
            .with_dependencies(vec!["shard_a".to_string()]);
        let shard_d = TaskShard::new("shard_d", "echo 'Step D'", "bash")
            .with_dependencies(vec!["shard_b".to_string(), "shard_c".to_string()]);

        graph = graph
            .add_shard(shard_a)
            .add_shard(shard_b)
            .add_shard(shard_c)
            .add_shard(shard_d);

        let stages = graph.decompose_into_stages().expect("topological sort");
        assert_eq!(stages.len(), 3);
        // Stage 0: [shard_a]
        assert_eq!(stages[0].len(), 1);
        assert_eq!(stages[0][0].shard_id, "shard_a");
        // Stage 1: [shard_b, shard_c]
        assert_eq!(stages[1].len(), 2);
        let stage1_ids: Vec<String> = stages[1].iter().map(|s| s.shard_id.clone()).collect();
        assert!(stage1_ids.contains(&"shard_b".to_string()));
        assert!(stage1_ids.contains(&"shard_c".to_string()));
        // Stage 2: [shard_d]
        assert_eq!(stages[2].len(), 1);
        assert_eq!(stages[2][0].shard_id, "shard_d");
    }

    #[test]
    fn test_task_graph_cycle_detection() {
        let mut graph = DistributedTaskGraph::new("cycle_graph");
        let shard_1 = TaskShard::new("s1", "echo 1", "bash").with_dependencies(vec!["s2".into()]);
        let shard_2 = TaskShard::new("s2", "echo 2", "bash").with_dependencies(vec!["s1".into()]);
        graph = graph.add_shard(shard_1).add_shard(shard_2);

        let res = graph.decompose_into_stages();
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("Cyclic dependency"));
    }

    #[tokio::test]
    async fn test_dual_tier_routing_tier1_wasm() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addr: SocketAddr = "127.0.0.1:18978".parse().unwrap();
        let scheduler = DistributedMicroVmScheduler::new("node_wasm", addr, temp_dir.path()).unwrap();

        let wat_code = r#"
        (module
            (func (export "run") (result i32)
                i32.const 42
            )
        )
        "#;
        let shard = TaskShard::new("wasm_shard", wat_code, "wat")
            .with_tier(SandboxExecutionTier::Tier1WasmZig)
            .with_fuel_limit(1_000_000);

        let outcome = scheduler.execute_shard_locally(&shard).expect("WASM execution");
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, 42);
        assert_eq!(outcome.tier_used, SandboxExecutionTier::Tier1WasmZig);
        assert!(outcome.fuel_consumed.is_some());
        assert!(outcome.fuel_consumed.unwrap() > 0);
        assert!(!outcome.blake3_digest.is_empty());
    }

    #[tokio::test]
    async fn test_dual_tier_routing_tier2_microvm() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addr: SocketAddr = "127.0.0.1:18979".parse().unwrap();
        let scheduler = DistributedMicroVmScheduler::new("node_microvm", addr, temp_dir.path()).unwrap();

        let shard = TaskShard::new("microvm_shard", "echo 'MicroVM Landlock Jail Online'", "bash")
            .with_tier(SandboxExecutionTier::Tier2FirecrackerMicroVm);

        let outcome = scheduler.execute_shard_locally(&shard).expect("MicroVM execution");
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.tier_used, SandboxExecutionTier::Tier2FirecrackerMicroVm);
        assert!(outcome.stdout.contains("MicroVM Landlock Jail Online"));
        assert!(!outcome.blake3_digest.is_empty());
    }

    #[tokio::test]
    async fn test_dual_tier_fuel_limit_trap() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addr: SocketAddr = "127.0.0.1:18980".parse().unwrap();
        let scheduler = DistributedMicroVmScheduler::new("node_trap", addr, temp_dir.path()).unwrap();

        let infinite_wat = r#"
        (module
            (func (export "run") (result i32)
                (loop $inf
                    br $inf
                )
                i32.const 0
            )
        )
        "#;
        let shard = TaskShard::new("inf_shard", infinite_wat, "wat")
            .with_tier(SandboxExecutionTier::Tier1WasmZig)
            .with_fuel_limit(500);

        let outcome = scheduler.execute_shard_locally(&shard).expect("execution with trap");
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, -2);
        assert!(outcome.stderr.contains("fuel"));
    }

    #[tokio::test]
    async fn test_peer_dispatch_and_failover_redispatch() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addr: SocketAddr = "127.0.0.1:18981".parse().unwrap();
        let scheduler = Arc::new(DistributedMicroVmScheduler::new("node_master", addr, temp_dir.path()).unwrap());

        // Register a dead peer with lower load score to force scheduler to try it first
        let dead_addr: SocketAddr = "127.0.0.1:59999".parse().unwrap();
        let mut dead_peer = DistributedNodeDescriptor::new(
            "node_dead",
            dead_addr,
            vec![SandboxExecutionTier::Tier1WasmZig, SandboxExecutionTier::Tier2FirecrackerMicroVm],
        );
        dead_peer.load_score = 0.01; // Lowest load score
        scheduler.register_peer(dead_peer).await;

        let shard = TaskShard::new("failover_test", "echo 'Failover succeeded'", "bash");

        // Dispatch shard: Should attempt node_dead, fail connection, record failover event, and re-dispatch locally!
        let (outcome, executing_node, failovers) = scheduler.dispatch_shard_with_failover(&shard).await.unwrap();

        assert!(outcome.success);
        assert_eq!(executing_node, "node_master");
        assert!(outcome.stdout.contains("Failover succeeded"));
        assert!(!failovers.is_empty());
        assert!(failovers[0].contains("node_dead"));
        assert!(failovers[0].contains("Re-routing"));
    }

    #[tokio::test]
    async fn test_distributed_execution_report_and_blake3_provenance() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addr: SocketAddr = "127.0.0.1:18982".parse().unwrap();
        let scheduler = Arc::new(DistributedMicroVmScheduler::new("node_agg", addr, temp_dir.path()).unwrap());

        let mut graph = DistributedTaskGraph::new("graph_provenance_01");

        let shard_1 = TaskShard::new("step1", "echo '{\"status\":\"initialized\"}'", "bash")
            .with_tier(SandboxExecutionTier::Tier2FirecrackerMicroVm);

        let wat_code = r#"
        (module
            (func (export "run") (result i32)
                i32.const 99
            )
        )
        "#;
        let shard_2 = TaskShard::new("step2", wat_code, "wat")
            .with_tier(SandboxExecutionTier::Tier1WasmZig)
            .with_dependencies(vec!["step1".to_string()]);

        graph = graph.add_shard(shard_1).add_shard(shard_2);

        let report = scheduler.dispatch_task_graph(&graph).await.unwrap();

        assert!(report.success);
        assert_eq!(report.total_shards, 2);
        assert_eq!(report.succeeded_shards, 2);
        assert_eq!(report.failed_shards, 0);
        assert_eq!(report.shard_outcomes.len(), 2);
        assert!(!report.blake3_provenance.is_empty());
        assert_eq!(report.node_dispatch_map.get("node_agg").unwrap().len(), 2);

        // Verify provenance hash consistency
        let recomputed = DistributedExecutionReport::compute_provenance(
            &report.graph_id,
            &report.shard_outcomes,
            report.total_duration_ms,
            report.total_fuel_consumed,
            report.failover_events.len(),
        );
        assert_eq!(report.blake3_provenance, recomputed);
    }

    #[tokio::test]
    async fn test_scheduler_network_roundtrip() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        // Bind to ephemeral port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let actual_addr = listener.local_addr().unwrap();
        drop(listener);

        let worker = Arc::new(DistributedMicroVmScheduler::new("node_worker", actual_addr, temp_dir.path()).unwrap());
        let _server_handle = worker.start_tcp_listener().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client_addr: SocketAddr = "127.0.0.1:18984".parse().unwrap();
        let client = Arc::new(DistributedMicroVmScheduler::new("node_client", client_addr, temp_dir.path()).unwrap());

        // Dispatch over TCP
        let shard = TaskShard::new("net_shard", "echo 'Hello over TCP mesh'", "bash")
            .with_tier(SandboxExecutionTier::Tier2FirecrackerMicroVm);

        let outcome = client.dispatch_shard_to_peer(actual_addr, &shard).await.unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.executing_node_id, "node_worker");
        assert!(outcome.stdout.contains("Hello over TCP mesh"));

        worker.shutdown();
    }
}
