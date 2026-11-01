//! # Pillar II: Distributed Multi-Node Mesh via WebAssembly (Edge Swarming)
//!
//! Production-grade distributed edge computing and WASM fragment distribution mesh enabling
//! `hgb-daemon` and `hgb-cli` instances to discover peers, shard multi-stage execution
//! graphs, route fragments based on live telemetry and cryptographic BLAKE3 integrity,
//! and aggregate asynchronous outcomes with automatic failover.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::RwLock;
use wasmtime::*;

use crate::error::{HgbError, Result};

pub const WASM_MESH_FRAME_MAGIC: u32 = 0x48475745; // "HGWE" (Hagibis Wasm Edge)
pub const DEFAULT_WASM_MESH_PORT: u16 = 8975;
pub const DEFAULT_STALE_NODE_TIMEOUT_SECS: i64 = 15;
pub const DEFAULT_FRAGMENT_FUEL_LIMIT: u64 = 10_000_000;
pub const MAX_MESH_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Live advertisement of a worker node in the WebAssembly edge mesh
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshNodeAdvertisement {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub cpu_cores: usize,
    pub free_ram_mb: u64,
    pub active_wasm_instances: usize,
    pub load_score: f32,
    pub last_heartbeat: DateTime<Utc>,
}

impl MeshNodeAdvertisement {
    pub fn new(node_id: impl Into<String>, listen_addr: SocketAddr) -> Self {
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let free_ram_mb = read_free_ram_mb();

        Self {
            node_id: node_id.into(),
            listen_addr,
            cpu_cores,
            free_ram_mb,
            active_wasm_instances: 0,
            load_score: 0.10,
            last_heartbeat: Utc::now(),
        }
    }

    pub fn is_stale(&self, timeout_secs: i64) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_heartbeat);
        elapsed.num_seconds() > timeout_secs
    }

    pub fn refresh_telemetry(&mut self, active_instances: usize) {
        self.active_wasm_instances = active_instances;
        self.free_ram_mb = read_free_ram_mb();
        self.cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        self.last_heartbeat = Utc::now();

        let instance_factor = (active_instances as f32) * 0.15;
        let ram_pressure = if self.free_ram_mb < 512 {
            0.50
        } else if self.free_ram_mb < 1024 {
            0.25
        } else {
            0.05
        };

        self.load_score = (instance_factor + ram_pressure).clamp(0.05, 2.0);
    }
}

/// Backward compatibility alias
pub type WasmNodeAdvertisement = MeshNodeAdvertisement;

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

/// A standalone execution fragment representing a partitioned piece of a distributed computation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasmExecutionFragment {
    pub fragment_id: String,
    pub wasm_module_bytes: Vec<u8>,
    pub entry_point: String,
    pub args: serde_json::Value,
    pub fuel_limit: u64,
}

impl WasmExecutionFragment {
    pub fn new(
        fragment_id: impl Into<String>,
        wasm_module_bytes: impl Into<Vec<u8>>,
        entry_point: impl Into<String>,
    ) -> Self {
        Self {
            fragment_id: fragment_id.into(),
            wasm_module_bytes: wasm_module_bytes.into(),
            entry_point: entry_point.into(),
            args: serde_json::Value::Null,
            fuel_limit: DEFAULT_FRAGMENT_FUEL_LIMIT,
        }
    }

    pub fn with_args(mut self, args: serde_json::Value) -> Self {
        self.args = args;
        self
    }

    pub fn with_fuel_limit(mut self, fuel_limit: u64) -> Self {
        self.fuel_limit = fuel_limit;
        self
    }

    /// Compute cryptographic BLAKE3 checksum covering the payload content and metadata
    pub fn compute_checksum(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.fragment_id.as_bytes());
        hasher.update(&self.wasm_module_bytes);
        hasher.update(self.entry_point.as_bytes());
        if let Ok(serialized_args) = serde_json::to_vec(&self.args) {
            hasher.update(&serialized_args);
        }
        hasher.update(&self.fuel_limit.to_le_bytes());
        *hasher.finalize().as_bytes()
    }

    /// Hex-encoded BLAKE3 checksum
    pub fn checksum_hex(&self) -> String {
        hex::encode(self.compute_checksum())
    }

    /// Verify received fragment against expected cryptographic BLAKE3 checksum
    pub fn verify_checksum(&self, expected: &[u8; 32]) -> Result<()> {
        let actual = self.compute_checksum();
        if &actual != expected {
            return Err(HgbError::Execution(format!(
                "BLAKE3 checksum verification failed for fragment '{}'. Expected {}, computed {}",
                self.fragment_id,
                hex::encode(expected),
                hex::encode(actual)
            )));
        }
        Ok(())
    }
}

/// Execution outcome returned by a mesh worker node for a fragment
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasmFragmentOutcome {
    pub fragment_id: String,
    pub executing_node_id: String,
    pub success: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
    pub fuel_consumed: u64,
    pub duration_ms: u64,
    pub blake3_checksum: String,
}

/// A node in a multi-step computation graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmGraphNode {
    pub node_id: String,
    pub wasm_module_bytes: Vec<u8>,
    pub entry_point: String,
    pub args: serde_json::Value,
    pub fuel_limit: u64,
    pub dependencies: Vec<String>,
}

/// Directed acyclic graph of interdependent WASM computation steps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmComputationGraph {
    pub workflow_id: String,
    pub nodes: Vec<WasmGraphNode>,
}

impl WasmComputationGraph {
    pub fn new(workflow_id: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            nodes: Vec::new(),
        }
    }

    pub fn add_node(&mut self, node: WasmGraphNode) {
        self.nodes.push(node);
    }
}

/// Comprehensive report of distributed multi-node mesh execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmMeshExecutionReport {
    pub workflow_id: String,
    pub success: bool,
    pub total_fragments: usize,
    pub succeeded_fragments: usize,
    pub failed_fragments: usize,
    pub total_duration_ms: u64,
    pub total_fuel_consumed: u64,
    pub outcomes: Vec<WasmFragmentOutcome>,
    pub node_dispatch_map: HashMap<String, Vec<String>>,
    pub failover_events: Vec<String>,
}

/// Packet protocol exchange across the edge mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshPacket {
    Ping { from_node_id: String, timestamp: DateTime<Utc> },
    Pong { node: MeshNodeAdvertisement },
    Heartbeat(MeshNodeAdvertisement),
    FragmentDispatch {
        fragment: WasmExecutionFragment,
        checksum: [u8; 32],
    },
    FragmentOutcome { outcome: WasmFragmentOutcome },
    Goodbye { node_id: String },
}

/// Internal host state for Wasmtime linker capture
struct WasmMeshHostState {
    stdout_buf: Vec<u8>,
    stderr_buf: Vec<u8>,
}

/// Router responsible for decomposing computation graphs, balancing load, and handling failover
pub struct WasmMeshShardRouter;

impl WasmMeshShardRouter {
    /// Decomposes a multi-step WASM computation graph into topologically sorted concurrency stages
    pub fn decompose_graph(graph: &WasmComputationGraph) -> Result<Vec<Vec<WasmExecutionFragment>>> {
        let mut in_degrees: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        let mut node_map: HashMap<String, WasmGraphNode> = HashMap::new();

        for node in &graph.nodes {
            node_map.insert(node.node_id.clone(), node.clone());
            in_degrees.insert(node.node_id.clone(), node.dependencies.len());
            for dep in &node.dependencies {
                adj.entry(dep.clone()).or_default().push(node.node_id.clone());
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
            let mut stage_fragments = Vec::new();
            let mut next_stage = Vec::new();

            for node_id in &current_stage {
                visited_count += 1;
                if let Some(node) = node_map.get(node_id) {
                    stage_fragments.push(WasmExecutionFragment {
                        fragment_id: node.node_id.clone(),
                        wasm_module_bytes: node.wasm_module_bytes.clone(),
                        entry_point: node.entry_point.clone(),
                        args: node.args.clone(),
                        fuel_limit: node.fuel_limit,
                    });
                }

                if let Some(dependents) = adj.get(node_id) {
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

            stages.push(stage_fragments);
            next_stage.sort();
            current_stage = next_stage;
        }

        if visited_count < graph.nodes.len() {
            return Err(HgbError::Execution(
                "Cyclic dependency detected in WASM computation graph".to_string(),
            ));
        }

        Ok(stages)
    }

    /// Selects optimal worker node candidates based on live load scores and available memory
    pub fn rank_nodes_by_load(
        peers: &[MeshNodeAdvertisement],
        local_ad: &MeshNodeAdvertisement,
    ) -> Vec<MeshNodeAdvertisement> {
        let mut all_nodes = Vec::with_capacity(peers.len() + 1);
        all_nodes.push(local_ad.clone());
        all_nodes.extend(peers.iter().cloned());

        all_nodes.sort_by(|a, b| {
            a.load_score
                .partial_cmp(&b.load_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.free_ram_mb.cmp(&a.free_ram_mb))
        });

        all_nodes
    }
}

/// Distributed WebAssembly Edge Mesh Engine
pub struct WasmEdgeMeshEngine {
    node_id: String,
    listen_addr: SocketAddr,
    workspace_root: PathBuf,
    local_ad: Arc<RwLock<MeshNodeAdvertisement>>,
    peer_registry: Arc<RwLock<HashMap<String, MeshNodeAdvertisement>>>,
    active_wasm_instances: Arc<AtomicUsize>,
    is_running: Arc<AtomicBool>,
    wasm_engine: Engine,
    total_local_dispatched: AtomicU64,
    total_remote_dispatched: AtomicU64,
    total_completed: AtomicU64,
    total_fuel_consumed: AtomicU64,
}

impl WasmEdgeMeshEngine {
    pub fn new(
        node_id: impl Into<String>,
        listen_addr: SocketAddr,
        workspace_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let node_id = node_id.into();
        let ws = workspace_root.into();

        let mut config = Config::new();
        config.consume_fuel(true);
        let wasm_engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize Wasmtime engine: {}", e))
        })?;

        let local_ad = Arc::new(RwLock::new(MeshNodeAdvertisement::new(
            node_id.clone(),
            listen_addr,
        )));

        Ok(Self {
            node_id,
            listen_addr,
            workspace_root: ws,
            local_ad,
            peer_registry: Arc::new(RwLock::new(HashMap::new())),
            active_wasm_instances: Arc::new(AtomicUsize::new(0)),
            is_running: Arc::new(AtomicBool::new(false)),
            wasm_engine,
            total_local_dispatched: AtomicU64::new(0),
            total_remote_dispatched: AtomicU64::new(0),
            total_completed: AtomicU64::new(0),
            total_fuel_consumed: AtomicU64::new(0),
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

    pub async fn register_peer(&self, ad: MeshNodeAdvertisement) {
        if ad.node_id == self.node_id {
            return;
        }
        let mut reg = self.peer_registry.write().await;
        reg.insert(ad.node_id.clone(), ad);
    }

    pub async fn remove_peer(&self, node_id: &str) -> Option<MeshNodeAdvertisement> {
        let mut reg = self.peer_registry.write().await;
        reg.remove(node_id)
    }

    pub async fn prune_stale_nodes(&self, timeout_secs: i64) -> usize {
        let mut reg = self.peer_registry.write().await;
        let initial = reg.len();
        reg.retain(|_, ad| !ad.is_stale(timeout_secs));
        initial - reg.len()
    }

    pub async fn list_peers(&self) -> Vec<MeshNodeAdvertisement> {
        let reg = self.peer_registry.read().await;
        reg.values().cloned().collect()
    }

    pub async fn local_advertisement(&self) -> MeshNodeAdvertisement {
        let mut ad = self.local_ad.write().await;
        let active = self.active_wasm_instances.load(Ordering::Relaxed);
        ad.refresh_telemetry(active);
        ad.clone()
    }

    /// Prepare bytecode from potential WAT, hex, or base64 representation
    fn prepare_wasm_bytes(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        if bytes.starts_with(b"\0asm") {
            return Ok(bytes.to_vec());
        }

        if let Ok(text) = std::str::from_utf8(bytes) {
            let trimmed = text.trim();
            if trimmed.starts_with("(module") || trimmed.starts_with("module") {
                return Ok(trimmed.as_bytes().to_vec());
            }

            if (trimmed.starts_with("0x") || trimmed.len() % 2 == 0)
                && trimmed.chars().all(|c| c.is_ascii_hexdigit() || c == 'x')
            {
                let hex_clean = trimmed.strip_prefix("0x").unwrap_or(trimmed);
                if let Ok(decoded) = hex::decode(hex_clean) {
                    if decoded.starts_with(b"\0asm") {
                        return Ok(decoded);
                    }
                }
            }

            if let Ok(decoded) = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                trimmed,
            ) {
                if decoded.starts_with(b"\0asm") {
                    return Ok(decoded);
                }
            }
        }

        Ok(bytes.to_vec())
    }

    /// Bind WASI and safe sandbox logging host functions into Linker
    fn bind_host_functions(&self, linker: &mut Linker<WasmMeshHostState>) -> Result<()> {
        linker
            .func_wrap(
                "wasi_snapshot_preview1",
                "fd_write",
                |mut caller: Caller<'_, WasmMeshHostState>,
                 fd: i32,
                 iovs_ptr: i32,
                 iovs_len: i32,
                 nwritten_ptr: i32|
                 -> i32 {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return 28, // EBADF
                    };

                    let mut total_written: u32 = 0;
                    for i in 0..iovs_len {
                        let iov_offset = (iovs_ptr as usize) + (i as usize) * 8;
                        let mem_data = memory.data(&caller);
                        if iov_offset + 8 > mem_data.len() {
                            return 21; // EFAULT
                        }

                        let buf_ptr = u32::from_le_bytes(
                            mem_data[iov_offset..iov_offset + 4].try_into().unwrap(),
                        ) as usize;
                        let buf_len = u32::from_le_bytes(
                            mem_data[iov_offset + 4..iov_offset + 8].try_into().unwrap(),
                        ) as usize;

                        if buf_ptr + buf_len > mem_data.len() {
                            return 21; // EFAULT
                        }

                        let bytes = mem_data[buf_ptr..buf_ptr + buf_len].to_vec();
                        let state = caller.data_mut();
                        if fd == 1 {
                            state.stdout_buf.extend_from_slice(&bytes);
                        } else if fd == 2 {
                            state.stderr_buf.extend_from_slice(&bytes);
                        }
                        total_written += buf_len as u32;
                    }

                    let mem_mut = memory.data_mut(&mut caller);
                    let nw_offset = nwritten_ptr as usize;
                    if nw_offset + 4 <= mem_mut.len() {
                        mem_mut[nw_offset..nw_offset + 4]
                            .copy_from_slice(&total_written.to_le_bytes());
                    }

                    0 // SUCCESS
                },
            )
            .map_err(|e| HgbError::Execution(format!("Failed to bind WASI fd_write: {}", e)))?;

        linker
            .func_wrap(
                "wasi_snapshot_preview1",
                "proc_exit",
                |_caller: Caller<'_, WasmMeshHostState>, _code: i32| {},
            )
            .map_err(|e| HgbError::Execution(format!("Failed to bind WASI proc_exit: {}", e)))?;

        linker
            .func_wrap(
                "env",
                "log",
                |mut caller: Caller<'_, WasmMeshHostState>, ptr: i32, len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return,
                    };
                    let mem_data = memory.data(&caller);
                    let start = ptr as usize;
                    let end = start + (len as usize);
                    if end <= mem_data.len() {
                        let bytes = mem_data[start..end].to_vec();
                        caller.data_mut().stdout_buf.extend_from_slice(&bytes);
                    }
                },
            )
            .map_err(|e| HgbError::Execution(format!("Failed to bind env log: {}", e)))?;

        linker
            .func_wrap(
                "env",
                "hgb_host_log",
                |mut caller: Caller<'_, WasmMeshHostState>, ptr: i32, len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return,
                    };
                    let mem_data = memory.data(&caller);
                    let start = ptr as usize;
                    let end = start + (len as usize);
                    if end <= mem_data.len() {
                        let bytes = mem_data[start..end].to_vec();
                        caller.data_mut().stdout_buf.extend_from_slice(&bytes);
                    }
                },
            )
            .map_err(|e| HgbError::Execution(format!("Failed to bind env hgb_host_log: {}", e)))?;

        Ok(())
    }

    /// Execute a WASM fragment locally in sandboxed Wasmtime runtime with fuel metering
    pub fn execute_fragment_locally(
        &self,
        fragment: &WasmExecutionFragment,
    ) -> Result<WasmFragmentOutcome> {
        let start = Instant::now();
        self.active_wasm_instances.fetch_add(1, Ordering::SeqCst);

        let wasm_bytes = self.prepare_wasm_bytes(&fragment.wasm_module_bytes)?;

        let module_res = Module::new(&self.wasm_engine, &wasm_bytes);
        let module = match module_res {
            Ok(m) => m,
            Err(e) => {
                self.active_wasm_instances.fetch_sub(1, Ordering::SeqCst);
                return Ok(WasmFragmentOutcome {
                    fragment_id: fragment.fragment_id.clone(),
                    executing_node_id: self.node_id.clone(),
                    success: false,
                    result: serde_json::Value::Null,
                    error: Some(format!("Module compilation error: {}", e)),
                    fuel_consumed: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    blake3_checksum: fragment.checksum_hex(),
                });
            }
        };

        let mut linker = Linker::new(&self.wasm_engine);
        if let Err(e) = self.bind_host_functions(&mut linker) {
            self.active_wasm_instances.fetch_sub(1, Ordering::SeqCst);
            return Err(e);
        }

        let mut store = Store::new(
            &self.wasm_engine,
            WasmMeshHostState {
                stdout_buf: Vec::new(),
                stderr_buf: Vec::new(),
            },
        );

        let fuel_limit = if fragment.fuel_limit == 0 {
            DEFAULT_FRAGMENT_FUEL_LIMIT
        } else {
            fragment.fuel_limit
        };

        let _ = store.set_fuel(fuel_limit);

        let instance_res = linker.instantiate(&mut store, &module);
        let instance = match instance_res {
            Ok(inst) => inst,
            Err(e) => {
                self.active_wasm_instances.fetch_sub(1, Ordering::SeqCst);
                return Ok(WasmFragmentOutcome {
                    fragment_id: fragment.fragment_id.clone(),
                    executing_node_id: self.node_id.clone(),
                    success: false,
                    result: serde_json::Value::Null,
                    error: Some(format!("Module instantiation error: {}", e)),
                    fuel_consumed: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    blake3_checksum: fragment.checksum_hex(),
                });
            }
        };

        let mut exit_code: i32 = 0;
        let mut trap_error: Option<wasmtime::Error> = None;
        let mut func_output: serde_json::Value = serde_json::Value::Null;

        if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, &fragment.entry_point) {
            match func.call(&mut store, ()) {
                Ok(ret) => {
                    exit_code = ret;
                    func_output = serde_json::json!(ret);
                }
                Err(e) => trap_error = Some(e),
            }
        } else if let Ok(func) = instance.get_typed_func::<(), i64>(&mut store, &fragment.entry_point) {
            match func.call(&mut store, ()) {
                Ok(ret) => {
                    exit_code = ret as i32;
                    func_output = serde_json::json!(ret);
                }
                Err(e) => trap_error = Some(e),
            }
        } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, &fragment.entry_point) {
            if let Err(e) = func.call(&mut store, ()) {
                trap_error = Some(e);
            } else {
                func_output = serde_json::json!({ "executed": true });
            }
        } else if let Ok(func) = instance.get_typed_func::<(i32, i32), i32>(&mut store, &fragment.entry_point) {
            let (a1, a2) = if let Some(arr) = fragment.args.as_array() {
                let v1 = arr.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let v2 = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                (v1, v2)
            } else {
                (0, 0)
            };
            match func.call(&mut store, (a1, a2)) {
                Ok(ret) => {
                    exit_code = ret;
                    func_output = serde_json::json!(ret);
                }
                Err(e) => trap_error = Some(e),
            }
        } else if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, "run") {
            match func.call(&mut store, ()) {
                Ok(ret) => {
                    exit_code = ret;
                    func_output = serde_json::json!(ret);
                }
                Err(e) => trap_error = Some(e),
            }
        } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "_start") {
            if let Err(e) = func.call(&mut store, ()) {
                trap_error = Some(e);
            } else {
                func_output = serde_json::json!({ "executed": true });
            }
        } else {
            self.active_wasm_instances.fetch_sub(1, Ordering::SeqCst);
            return Ok(WasmFragmentOutcome {
                fragment_id: fragment.fragment_id.clone(),
                executing_node_id: self.node_id.clone(),
                success: false,
                result: serde_json::Value::Null,
                error: Some(format!(
                    "Entry point '{}' not found in WASM module",
                    fragment.entry_point
                )),
                fuel_consumed: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                blake3_checksum: fragment.checksum_hex(),
            });
        }

        self.active_wasm_instances.fetch_sub(1, Ordering::SeqCst);

        let remaining_fuel = store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_limit.saturating_sub(remaining_fuel);
        let duration_ms = start.elapsed().as_millis() as u64;

        let memory_bytes = instance
            .get_memory(&mut store, "memory")
            .map(|m| m.data_size(&store) as u64)
            .unwrap_or(65536);

        let (stdout, stderr) = {
            let data = store.data();
            (
                String::from_utf8_lossy(&data.stdout_buf).to_string(),
                String::from_utf8_lossy(&data.stderr_buf).to_string(),
            )
        };

        let (success, error_msg) = if let Some(trap) = trap_error {
            let trap_msg = trap.to_string();
            let is_fuel = remaining_fuel == 0 || trap_msg.contains("fuel");
            exit_code = if is_fuel { -2 } else { -1 };
            let err_str = if is_fuel {
                "Execution trap: CPU fuel limit exhausted (OutOfFuel)".to_string()
            } else {
                format!("Execution trap: {}", trap_msg)
            };
            (false, Some(err_str))
        } else {
            (true, None)
        };

        self.total_completed.fetch_add(1, Ordering::Relaxed);
        self.total_fuel_consumed.fetch_add(fuel_consumed, Ordering::Relaxed);

        let result = serde_json::json!({
            "exit_code": exit_code,
            "output": func_output,
            "stdout": stdout,
            "stderr": stderr,
            "memory_bytes": memory_bytes,
            "fuel_consumed": fuel_consumed,
        });

        Ok(WasmFragmentOutcome {
            fragment_id: fragment.fragment_id.clone(),
            executing_node_id: self.node_id.clone(),
            success,
            result,
            error: error_msg,
            fuel_consumed,
            duration_ms,
            blake3_checksum: fragment.checksum_hex(),
        })
    }

    /// Dispatch fragment to peer over TCP stream with cryptographic BLAKE3 verification
    pub async fn dispatch_fragment_to_peer(
        &self,
        peer_addr: SocketAddr,
        fragment: &WasmExecutionFragment,
    ) -> Result<WasmFragmentOutcome> {
        let checksum = fragment.compute_checksum();
        let packet = MeshPacket::FragmentDispatch {
            fragment: fragment.clone(),
            checksum,
        };
        let payload = bincode::serialize(&packet)
            .map_err(|e| HgbError::Execution(format!("Serialization error: {}", e)))?;

        let timeout_ms = (fragment.fuel_limit / 1000).clamp(3000, 30000);
        let mut stream = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Connection timeout to peer {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect to peer {}: {}", peer_addr, e)))?;

        let mut frame_header = Vec::with_capacity(8);
        frame_header.extend_from_slice(&WASM_MESH_FRAME_MAGIC.to_be_bytes());
        frame_header.extend_from_slice(&(payload.len() as u32).to_be_bytes());

        stream.write_all(&frame_header).await.map_err(HgbError::Io)?;
        stream.write_all(&payload).await.map_err(HgbError::Io)?;
        stream.flush().await.map_err(HgbError::Io)?;

        let mut magic_buf = [0u8; 4];
        stream.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
        if u32::from_be_bytes(magic_buf) != WASM_MESH_FRAME_MAGIC {
            return Err(HgbError::Execution(
                "Invalid frame magic received from peer".into(),
            ));
        }

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        if resp_len > MAX_MESH_FRAME_BYTES {
            return Err(HgbError::Execution(format!(
                "Response payload too large: {} bytes",
                resp_len
            )));
        }

        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).await.map_err(HgbError::Io)?;

        let response_packet: MeshPacket = bincode::deserialize(&resp_buf)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match response_packet {
            MeshPacket::FragmentOutcome { outcome } => Ok(outcome),
            other => Err(HgbError::Execution(format!(
                "Unexpected response packet from peer: {:?}",
                other
            ))),
        }
    }

    /// Execute a flat batch or stage of fragments concurrently across optimal nodes with failover
    pub async fn execute_sharded_workflow(
        self: &Arc<Self>,
        workflow_id: &str,
        fragments: Vec<WasmExecutionFragment>,
    ) -> Result<WasmMeshExecutionReport> {
        let start = Instant::now();
        let total_fragments = fragments.len();

        let _ = self.prune_stale_nodes(DEFAULT_STALE_NODE_TIMEOUT_SECS).await;
        let peers = self.list_peers().await;
        let local_ad = self.local_advertisement().await;

        let ranked_nodes = WasmMeshShardRouter::rank_nodes_by_load(&peers, &local_ad);

        let mut handles: Vec<tokio::task::JoinHandle<std::result::Result<(WasmFragmentOutcome, String, Vec<String>), HgbError>>> =
            Vec::with_capacity(fragments.len());

        for (idx, fragment) in fragments.into_iter().enumerate() {
            let engine = Arc::clone(self);
            let candidates = ranked_nodes.clone();
            let target_node = candidates[idx % candidates.len()].clone();

            handles.push(tokio::spawn(async move {
                let mut failovers = Vec::new();

                // If target is local node
                if target_node.node_id == engine.node_id {
                    engine.total_local_dispatched.fetch_add(1, Ordering::Relaxed);
                    let outcome = engine.execute_fragment_locally(&fragment)?;
                    return Ok((outcome, target_node.node_id, failovers));
                }

                // If target is remote peer
                engine.total_remote_dispatched.fetch_add(1, Ordering::Relaxed);
                match engine.dispatch_fragment_to_peer(target_node.listen_addr, &fragment).await {
                    Ok(outcome) => Ok((outcome, target_node.node_id, failovers)),
                    Err(e) => {
                        let failover_msg = format!(
                            "Node {} ({}) dropped or failed fragment {}: {}. Initiating failover re-dispatch to local node.",
                            target_node.node_id, target_node.listen_addr, fragment.fragment_id, e
                        );
                        failovers.push(failover_msg);

                        // Failover: re-dispatch locally
                        engine.total_local_dispatched.fetch_add(1, Ordering::Relaxed);
                        let fallback_outcome = engine.execute_fragment_locally(&fragment)?;
                        Ok((fallback_outcome, engine.node_id.clone(), failovers))
                    }
                }
            }));
        }

        let mut outcomes = Vec::with_capacity(total_fragments);
        let mut node_dispatch_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut failover_events = Vec::new();
        let mut succeeded = 0;
        let mut failed = 0;
        let mut total_fuel = 0;

        for handle in handles {
            let (outcome, executing_node, fails): (WasmFragmentOutcome, String, Vec<String>) =
                handle.await.map_err(|e| HgbError::Execution(e.to_string()))??;

            if outcome.success {
                succeeded += 1;
            } else {
                failed += 1;
            }
            total_fuel += outcome.fuel_consumed;
            node_dispatch_map.entry(executing_node).or_default().push(outcome.fragment_id.clone());
            failover_events.extend(fails);
            outcomes.push(outcome);
        }

        Ok(WasmMeshExecutionReport {
            workflow_id: workflow_id.to_string(),
            success: failed == 0,
            total_fragments,
            succeeded_fragments: succeeded,
            failed_fragments: failed,
            total_duration_ms: start.elapsed().as_millis() as u64,
            total_fuel_consumed: total_fuel,
            outcomes,
            node_dispatch_map,
            failover_events,
        })
    }

    /// Execute a multi-stage WASM computation graph across the distributed edge mesh
    pub async fn execute_computation_graph(
        self: &Arc<Self>,
        graph: &WasmComputationGraph,
    ) -> Result<WasmMeshExecutionReport> {
        let start = Instant::now();
        let stages = WasmMeshShardRouter::decompose_graph(graph)?;

        let mut all_outcomes = Vec::new();
        let mut node_dispatch_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_failovers = Vec::new();
        let mut total_succeeded = 0;
        let mut total_failed = 0;
        let mut total_fuel = 0;

        for (stage_idx, stage_fragments) in stages.into_iter().enumerate() {
            let stage_id = format!("{}_stage_{}", graph.workflow_id, stage_idx);
            let stage_report = self.execute_sharded_workflow(&stage_id, stage_fragments).await?;

            total_succeeded += stage_report.succeeded_fragments;
            total_failed += stage_report.failed_fragments;
            total_fuel += stage_report.total_fuel_consumed;
            all_failovers.extend(stage_report.failover_events);

            for (node, frags) in stage_report.node_dispatch_map {
                node_dispatch_map.entry(node).or_default().extend(frags);
            }
            all_outcomes.extend(stage_report.outcomes);

            if !stage_report.success {
                break;
            }
        }

        let total_fragments = graph.nodes.len();

        Ok(WasmMeshExecutionReport {
            workflow_id: graph.workflow_id.clone(),
            success: total_failed == 0 && total_succeeded == total_fragments,
            total_fragments,
            succeeded_fragments: total_succeeded,
            failed_fragments: total_failed,
            total_duration_ms: start.elapsed().as_millis() as u64,
            total_fuel_consumed: total_fuel,
            outcomes: all_outcomes,
            node_dispatch_map,
            failover_events: all_failovers,
        })
    }

    /// Start TCP listener for receiving peer discovery heartbeats and fragment dispatches
    pub async fn start_tcp_listener(self: &Arc<Self>) -> Result<tokio::task::JoinHandle<()>> {
        let listener = TcpListener::bind(self.listen_addr).await.map_err(|e| {
            HgbError::Execution(format!(
                "Failed to bind TCP mesh listener on {}: {}",
                self.listen_addr, e
            ))
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
                                eprintln!("WASM mesh connection from {} closed: {}", peer_addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        if !engine.is_running.load(Ordering::Relaxed) {
                            break;
                        }
                        eprintln!("WASM mesh accept error: {}", e);
                    }
                }
            }
        });

        Ok(handle)
    }

    async fn handle_tcp_connection(
        &self,
        stream: &mut TcpStream,
        _peer_addr: SocketAddr,
    ) -> Result<()> {
        let mut magic_buf = [0u8; 4];
        stream.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
        if u32::from_be_bytes(magic_buf) != WASM_MESH_FRAME_MAGIC {
            return Err(HgbError::Execution("Invalid mesh frame magic".into()));
        }

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
        let payload_len = u32::from_be_bytes(len_buf) as usize;
        if payload_len > MAX_MESH_FRAME_BYTES {
            return Err(HgbError::Execution(format!("Payload too large: {} bytes", payload_len)));
        }

        let mut payload_buf = vec![0u8; payload_len];
        stream.read_exact(&mut payload_buf).await.map_err(HgbError::Io)?;

        let packet: MeshPacket = bincode::deserialize(&payload_buf)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match packet {
            MeshPacket::Ping { .. } => {
                let local_ad = self.local_advertisement().await;
                let reply = MeshPacket::Pong { node: local_ad };
                self.send_packet_reply(stream, reply).await?;
            }
            MeshPacket::Heartbeat(ad) => {
                self.register_peer(ad).await;
                let local_ad = self.local_advertisement().await;
                let reply = MeshPacket::Heartbeat(local_ad);
                self.send_packet_reply(stream, reply).await?;
            }
            MeshPacket::FragmentDispatch { fragment, checksum } => {
                // Cryptographic tamper verification
                if let Err(e) = fragment.verify_checksum(&checksum) {
                    let outcome = WasmFragmentOutcome {
                        fragment_id: fragment.fragment_id.clone(),
                        executing_node_id: self.node_id.clone(),
                        success: false,
                        result: serde_json::Value::Null,
                        error: Some(format!("Payload verification error: {}", e)),
                        fuel_consumed: 0,
                        duration_ms: 0,
                        blake3_checksum: fragment.checksum_hex(),
                    };
                    let reply = MeshPacket::FragmentOutcome { outcome };
                    self.send_packet_reply(stream, reply).await?;
                    return Ok(());
                }

                let outcome = self.execute_fragment_locally(&fragment).unwrap_or_else(|e| {
                    WasmFragmentOutcome {
                        fragment_id: fragment.fragment_id.clone(),
                        executing_node_id: self.node_id.clone(),
                        success: false,
                        result: serde_json::Value::Null,
                        error: Some(e.to_string()),
                        fuel_consumed: 0,
                        duration_ms: 0,
                        blake3_checksum: fragment.checksum_hex(),
                    }
                });

                let reply = MeshPacket::FragmentOutcome { outcome };
                self.send_packet_reply(stream, reply).await?;
            }
            MeshPacket::Goodbye { node_id } => {
                self.remove_peer(&node_id).await;
            }
            MeshPacket::Pong { node } => {
                self.register_peer(node).await;
            }
            MeshPacket::FragmentOutcome { .. } => {}
        }

        Ok(())
    }

    async fn send_packet_reply(&self, stream: &mut TcpStream, packet: MeshPacket) -> Result<()> {
        let payload = bincode::serialize(&packet)
            .map_err(|e| HgbError::Execution(format!("Serialization error: {}", e)))?;
        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&WASM_MESH_FRAME_MAGIC.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        stream.write_all(&frame).await.map_err(HgbError::Io)?;
        stream.flush().await.map_err(HgbError::Io)?;
        Ok(())
    }

    /// Send direct heartbeat to a peer over TCP
    pub async fn send_heartbeat_to_peer(&self, peer_addr: SocketAddr) -> Result<MeshNodeAdvertisement> {
        let local_ad = self.local_advertisement().await;
        let packet = MeshPacket::Heartbeat(local_ad);
        let payload = bincode::serialize(&packet)
            .map_err(|e| HgbError::Execution(format!("Serialization error: {}", e)))?;

        let mut stream = tokio::time::timeout(
            Duration::from_millis(3000),
            TcpStream::connect(peer_addr),
        )
        .await
        .map_err(|_| HgbError::Execution(format!("Heartbeat connection timeout to {}", peer_addr)))?
        .map_err(|e| HgbError::Execution(format!("Failed to connect to {}: {}", peer_addr, e)))?;

        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&WASM_MESH_FRAME_MAGIC.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        stream.write_all(&frame).await.map_err(HgbError::Io)?;
        stream.flush().await.map_err(HgbError::Io)?;

        let mut magic_buf = [0u8; 4];
        stream.read_exact(&mut magic_buf).await.map_err(HgbError::Io)?;
        if u32::from_be_bytes(magic_buf) != WASM_MESH_FRAME_MAGIC {
            return Err(HgbError::Execution("Invalid mesh frame magic in reply".into()));
        }

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.map_err(HgbError::Io)?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;

        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).await.map_err(HgbError::Io)?;

        let reply: MeshPacket = bincode::deserialize(&resp_buf)
            .map_err(|e| HgbError::Execution(format!("Deserialization error: {}", e)))?;

        match reply {
            MeshPacket::Heartbeat(peer_ad) | MeshPacket::Pong { node: peer_ad } => {
                self.register_peer(peer_ad.clone()).await;
                Ok(peer_ad)
            }
            other => Err(HgbError::Execution(format!("Unexpected reply packet: {:?}", other))),
        }
    }

    /// Broadcast UDP beacon announcement for local zero-config peer discovery
    pub async fn broadcast_udp_heartbeat(&self, broadcast_port: u16) -> Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(HgbError::Io)?;
        let _ = socket.set_broadcast(true);

        let local_ad = self.local_advertisement().await;
        let packet = MeshPacket::Heartbeat(local_ad);
        let payload = bincode::serialize(&packet)
            .map_err(|e| HgbError::Execution(format!("Serialization error: {}", e)))?;

        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&WASM_MESH_FRAME_MAGIC.to_be_bytes());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        let target_addr: SocketAddr = format!("255.255.255.255:{}", broadcast_port)
            .parse()
            .map_err(|e| HgbError::Execution(format!("Invalid broadcast address: {}", e)))?;
        let _ = socket.send_to(&frame, target_addr).await;
        Ok(())
    }

    /// Start background UDP discovery listener
    pub async fn start_udp_discovery_listener(
        self: &Arc<Self>,
        port: u16,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let socket = UdpSocket::bind(addr).await.map_err(|e| {
            HgbError::Execution(format!("Failed to bind UDP discovery listener on {}: {}", addr, e))
        })?;
        let _ = socket.set_broadcast(true);

        self.is_running.store(true, Ordering::SeqCst);
        let engine = Arc::clone(self);

        let handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            while engine.is_running.load(Ordering::Relaxed) {
                match socket.recv_from(&mut buf).await {
                    Ok((len, _peer_src)) => {
                        if len < 8 {
                            continue;
                        }
                        let magic = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
                        if magic != WASM_MESH_FRAME_MAGIC {
                            continue;
                        }
                        let payload_len =
                            u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
                        if 8 + payload_len > len {
                            continue;
                        }

                        if let Ok(packet) =
                            bincode::deserialize::<MeshPacket>(&buf[8..8 + payload_len])
                        {
                            match packet {
                                MeshPacket::Heartbeat(ad) | MeshPacket::Pong { node: ad } => {
                                    engine.register_peer(ad).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        if !engine.is_running.load(Ordering::Relaxed) {
                            break;
                        }
                        eprintln!("UDP discovery error: {}", e);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_node_registration_heartbeat_and_stale_pruning() {
        let tmp = std::env::temp_dir().join(format!("wasm_mesh_test_node_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        let engine = WasmEdgeMeshEngine::new("mesh-node-alpha", "127.0.0.1:9201".parse().unwrap(), &tmp)
            .expect("Engine init");

        // 1. Initial peers empty
        assert_eq!(engine.list_peers().await.len(), 0);

        // 2. Register fresh peer
        let peer_beta = MeshNodeAdvertisement::new("mesh-node-beta", "127.0.0.1:9202".parse().unwrap());
        engine.register_peer(peer_beta).await;
        assert_eq!(engine.list_peers().await.len(), 1);

        // 3. Register stale peer (timestamp 30 seconds ago)
        let mut peer_gamma = MeshNodeAdvertisement::new("mesh-node-gamma", "127.0.0.1:9203".parse().unwrap());
        peer_gamma.last_heartbeat = Utc::now() - chrono::Duration::seconds(30);
        engine.register_peer(peer_gamma).await;
        assert_eq!(engine.list_peers().await.len(), 2);

        // 4. Prune stale nodes with 15s timeout
        let pruned = engine.prune_stale_nodes(15).await;
        assert_eq!(pruned, 1);

        let peers = engine.list_peers().await;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].node_id, "mesh-node-beta");

        // 5. Heartbeat update refreshes load score and telemetry
        let mut refreshed_beta = peers[0].clone();
        refreshed_beta.refresh_telemetry(3);
        assert!(refreshed_beta.active_wasm_instances == 3);
        assert!(refreshed_beta.load_score > 0.30);
        engine.register_peer(refreshed_beta).await;

        let updated_peers = engine.list_peers().await;
        assert_eq!(updated_peers[0].active_wasm_instances, 3);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_wasm_fragment_serialization_blake3_checksum_and_tamper_detection() {
        let wat_code = r#"
        (module
            (func (export "run") (result i32)
                i32.const 42
            )
        )
        "#;

        let fragment = WasmExecutionFragment::new("frag-001", wat_code.as_bytes(), "run")
            .with_args(serde_json::json!({ "multiplier": 2 }))
            .with_fuel_limit(500_000);

        // 1. Checksum generation
        let checksum = fragment.compute_checksum();
        assert_eq!(checksum.len(), 32);
        assert!(fragment.verify_checksum(&checksum).is_ok());

        // 2. Bincode serialization round-trip
        let serialized = bincode::serialize(&fragment).expect("serialize fragment");
        let deserialized: WasmExecutionFragment = bincode::deserialize(&serialized).expect("deserialize");
        assert_eq!(fragment, deserialized);
        assert!(deserialized.verify_checksum(&checksum).is_ok());

        // 3. Tamper detection: modifying bytecode invalidates checksum
        let mut tampered = fragment.clone();
        tampered.wasm_module_bytes = wat_code.replace("42", "99").into_bytes();
        let verify_res = tampered.verify_checksum(&checksum);
        assert!(verify_res.is_err());
        let err_msg = verify_res.unwrap_err().to_string();
        assert!(err_msg.contains("BLAKE3 checksum verification failed"));
    }

    #[tokio::test]
    async fn test_sharding_router_dispatch_to_optimal_peers_and_failover() {
        let tmp = std::env::temp_dir().join(format!("wasm_mesh_dispatch_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        // Spin up Peer Worker Node 1 on port 9301
        let peer1 = Arc::new(
            WasmEdgeMeshEngine::new("peer-node-1", "127.0.0.1:9301".parse().unwrap(), &tmp)
                .expect("Peer 1 init"),
        );
        let _peer1_handle = peer1.start_tcp_listener().await.expect("start peer1 listener");

        // Primary node on port 9300
        let primary = Arc::new(
            WasmEdgeMeshEngine::new("primary-node", "127.0.0.1:9300".parse().unwrap(), &tmp)
                .expect("Primary init"),
        );

        // Register Peer 1 and an offline Peer 2
        let ad1 = peer1.local_advertisement().await;
        primary.register_peer(ad1).await;

        let offline_ad = MeshNodeAdvertisement::new("offline-node", "127.0.0.1:9302".parse().unwrap());
        primary.register_peer(offline_ad).await;

        let wat_code = r#"
        (module
            (func (export "compute") (result i32)
                i32.const 1337
            )
        )
        "#;

        let fragment = WasmExecutionFragment::new("frag-compute", wat_code.as_bytes(), "compute")
            .with_fuel_limit(1_000_000);

        // 1. Direct dispatch to active Peer 1
        let outcome = primary
            .dispatch_fragment_to_peer("127.0.0.1:9301".parse().unwrap(), &fragment)
            .await
            .expect("dispatch to peer1");

        assert!(outcome.success);
        assert_eq!(outcome.executing_node_id, "peer-node-1");
        assert_eq!(outcome.result["exit_code"], 1337);

        // 2. Failover: dispatching to offline peer triggers automatic fallback
        let workflow_report = primary
            .execute_sharded_workflow("failover-batch", vec![fragment.clone()])
            .await
            .expect("sharded workflow with failover");

        assert!(workflow_report.success);
        assert_eq!(workflow_report.succeeded_fragments, 1);
        assert_eq!(workflow_report.failed_fragments, 0);

        peer1.stop();
        primary.stop();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_concurrent_fragment_result_aggregation() {
        let tmp = std::env::temp_dir().join(format!("wasm_mesh_agg_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        let engine = Arc::new(
            WasmEdgeMeshEngine::new("mesh-agg-node", "127.0.0.1:9401".parse().unwrap(), &tmp)
                .expect("Engine init"),
        );

        let wat_1 = r#"(module (func (export "run") (result i32) i32.const 10))"#;
        let wat_2 = r#"(module (func (export "run") (result i32) i32.const 20))"#;
        let wat_3 = r#"(module (func (export "run") (result i32) i32.const 30))"#;
        let wat_4 = r#"(module (func (export "run") (result i32) i32.const 40))"#;

        let fragments = vec![
            WasmExecutionFragment::new("frag-1", wat_1.as_bytes(), "run"),
            WasmExecutionFragment::new("frag-2", wat_2.as_bytes(), "run"),
            WasmExecutionFragment::new("frag-3", wat_3.as_bytes(), "run"),
            WasmExecutionFragment::new("frag-4", wat_4.as_bytes(), "run"),
        ];

        let report = engine
            .execute_sharded_workflow("concurrent-aggregation-test", fragments)
            .await
            .expect("execute sharded workflow");

        assert_eq!(report.total_fragments, 4);
        assert_eq!(report.succeeded_fragments, 4);
        assert_eq!(report.failed_fragments, 0);
        assert!(report.success);
        assert!(report.total_fuel_consumed > 0);
        assert_eq!(report.outcomes.len(), 4);

        let exit_codes: Vec<i64> = report
            .outcomes
            .iter()
            .map(|o| o.result["exit_code"].as_i64().unwrap())
            .collect();

        assert!(exit_codes.contains(&10));
        assert!(exit_codes.contains(&20));
        assert!(exit_codes.contains(&30));
        assert!(exit_codes.contains(&40));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_computation_graph_dependency_staging() {
        let mut graph = WasmComputationGraph::new("staged-pipeline");

        let wat_step1 = r#"(module (func (export "run") (result i32) i32.const 1))"#;
        let wat_step2 = r#"(module (func (export "run") (result i32) i32.const 2))"#;
        let wat_step3 = r#"(module (func (export "run") (result i32) i32.const 3))"#;

        graph.add_node(WasmGraphNode {
            node_id: "step-1".to_string(),
            wasm_module_bytes: wat_step1.as_bytes().to_vec(),
            entry_point: "run".to_string(),
            args: serde_json::Value::Null,
            fuel_limit: 500_000,
            dependencies: vec![],
        });

        graph.add_node(WasmGraphNode {
            node_id: "step-2".to_string(),
            wasm_module_bytes: wat_step2.as_bytes().to_vec(),
            entry_point: "run".to_string(),
            args: serde_json::Value::Null,
            fuel_limit: 500_000,
            dependencies: vec![],
        });

        graph.add_node(WasmGraphNode {
            node_id: "step-3".to_string(),
            wasm_module_bytes: wat_step3.as_bytes().to_vec(),
            entry_point: "run".to_string(),
            args: serde_json::Value::Null,
            fuel_limit: 500_000,
            dependencies: vec!["step-1".to_string(), "step-2".to_string()],
        });

        let stages = WasmMeshShardRouter::decompose_graph(&graph).expect("decompose");
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].len(), 2); // step-1 and step-2 in stage 0
        assert_eq!(stages[1].len(), 1); // step-3 in stage 1

        let tmp = std::env::temp_dir().join(format!("wasm_mesh_graph_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        let engine = Arc::new(
            WasmEdgeMeshEngine::new("mesh-graph-node", "127.0.0.1:9501".parse().unwrap(), &tmp)
                .expect("Engine init"),
        );

        let report = engine.execute_computation_graph(&graph).await.expect("execute graph");
        assert_eq!(report.total_fragments, 3);
        assert_eq!(report.succeeded_fragments, 3);
        assert!(report.success);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
