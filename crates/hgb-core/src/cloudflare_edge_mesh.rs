//! # Pillar 3: Edge-Native Synchronization via Cloudflare Workers & Wrangler
//!
//! Provides bidirectional state synchronization between local Hagibis daemon/CLI
//! and Cloudflare Workers edge nodes:
//! - Parses workspace `wrangler.toml` and `_routes.json` edge rules.
//! - Telemetry Publishing: Publishes local Hagibis health, active models, hardware telemetry,
//!   and swarm peer status to the Cloudflare Workers edge endpoint.
//! - Vector Embedding & Mutation Sync: Syncs local SQLite mutation vector snapshots
//!   (from `ollama_pipeline_accelerator.rs` / `fts5_hnsw_hybrid.rs`) with Cloudflare Vectorize / KV / D1
//!   edge storage via authenticated edge API endpoints.
//! - Remote Trigger Dispatch & Polling: Allows local Hagibis daemon to poll or receive webhook triggers
//!   from Cloudflare Workers edge (e.g. external code review request, webhook notification,
//!   distributed task execution trigger).
//! - Offline Resilience: If network is offline or Cloudflare Worker endpoint is unreachable,
//!   buffers telemetry and mutations locally in SQLite WAL (`.hgb/edge_mesh_buffer.db`)
//!   and auto-flushes when connectivity is restored.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{HgbError, Result};

/// Default SQLite database path for local offline edge buffer
pub const DEFAULT_EDGE_BUFFER_DB: &str = ".hgb/edge_mesh_buffer.db";

/// Default SQLite database path for local mutation vectors
pub const DEFAULT_MUTATION_VECTORS_DB: &str = ".hgb/ollama_mutation_vectors.db";

/// Default Cloudflare compatibility date
pub const DEFAULT_COMPATIBILITY_DATE: &str = "2026-09-01";

/// Default HTTP request timeout in milliseconds
pub const DEFAULT_EDGE_TIMEOUT_MS: u64 = 4000;

// =================================================================================================
// Cloudflare Routes Configuration (_routes.json)
// =================================================================================================

/// Cloudflare Pages / Workers Routing rules parsed from `_routes.json`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudflareRoutesConfig {
    pub version: u32,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for CloudflareRoutesConfig {
    fn default() -> Self {
        Self {
            version: 1,
            include: vec!["/*".to_string()],
            exclude: vec![
                "/static/*".to_string(),
                "/assets/*".to_string(),
                "/favicon.ico".to_string(),
            ],
        }
    }
}

impl CloudflareRoutesConfig {
    /// Determines whether a requested URL path matches the inclusion and exclusion rules.
    pub fn matches(&self, path: &str) -> bool {
        // Exclusions take precedence
        for pattern in &self.exclude {
            if glob_match(pattern, path) {
                return false;
            }
        }
        // Check inclusions
        for pattern in &self.include {
            if glob_match(pattern, path) {
                return true;
            }
        }
        false
    }
}

/// Simple glob matcher for edge route rules (supporting prefix*, *suffix, /*, etc.)
pub fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "/*" || pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return text == prefix || text.starts_with(&format!("{}/", prefix));
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return text.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return text.ends_with(suffix);
    }
    pattern == text
}

// =================================================================================================
// Cloudflare Edge Mesh Configuration
// =================================================================================================

/// Configuration for Cloudflare Workers Edge Mesh parsed from `wrangler.toml` and `_routes.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareEdgeMeshConfig {
    /// Application name (e.g. "my-vibe-app")
    pub name: String,
    /// Compatibility date string (e.g. "2026-09-01")
    pub compatibility_date: String,
    /// Build output directory for Pages
    pub pages_build_output_dir: Option<String>,
    /// Environment variables declared under [vars] or [env.*.vars]
    pub vars: HashMap<String, String>,
    /// Target Cloudflare Workers edge URL endpoint (e.g. "https://my-vibe-app.workers.dev")
    pub edge_url: String,
    /// Optional Cloudflare API bearer token
    pub api_token: Option<String>,
    /// Optional routes configuration from `_routes.json`
    pub routes: Option<CloudflareRoutesConfig>,
    /// Path to local SQLite WAL buffer for offline resilience
    pub buffer_db_path: PathBuf,
    /// Path to local SQLite mutation vector storage
    pub mutation_vectors_db_path: PathBuf,
    /// HTTP timeout duration
    pub timeout_ms: u64,
}

impl Default for CloudflareEdgeMeshConfig {
    fn default() -> Self {
        Self {
            name: "my-vibe-app".to_string(),
            compatibility_date: DEFAULT_COMPATIBILITY_DATE.to_string(),
            pages_build_output_dir: Some(".".to_string()),
            vars: {
                let mut m = HashMap::new();
                m.insert("ENVIRONMENT".to_string(), "production".to_string());
                m.insert("DEPLOYED_BY".to_string(), "hagibis-edge-deployer".to_string());
                m
            },
            edge_url: "https://my-vibe-app.workers.dev".to_string(),
            api_token: None,
            routes: Some(CloudflareRoutesConfig::default()),
            buffer_db_path: PathBuf::from(DEFAULT_EDGE_BUFFER_DB),
            mutation_vectors_db_path: PathBuf::from(DEFAULT_MUTATION_VECTORS_DB),
            timeout_ms: DEFAULT_EDGE_TIMEOUT_MS,
        }
    }
}

impl CloudflareEdgeMeshConfig {
    /// Parse `wrangler.toml` contents and extract configuration fields
    pub fn parse_wrangler_str(content: &str) -> (String, String, Option<String>, HashMap<String, String>, Option<String>) {
        let mut name = "my-vibe-app".to_string();
        let mut compatibility_date = DEFAULT_COMPATIBILITY_DATE.to_string();
        let mut pages_build_output_dir = None;
        let mut edge_url = None;
        let mut vars = HashMap::new();

        let mut in_vars_section = false;

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                let section = &line[1..line.len() - 1].trim();
                in_vars_section = *section == "vars" || section.ends_with(".vars");
                continue;
            }

            if let Some((key, val)) = line.split_once('=') {
                let k = key.trim();
                let v = val.trim().trim_matches('"').trim_matches('\'').to_string();

                if in_vars_section {
                    vars.insert(k.to_string(), v);
                } else {
                    match k {
                        "name" => name = v,
                        "compatibility_date" => compatibility_date = v,
                        "pages_build_output_dir" => pages_build_output_dir = Some(v),
                        "edge_url" => edge_url = Some(v),
                        _ => {}
                    }
                }
            }
        }

        (name, compatibility_date, pages_build_output_dir, vars, edge_url)
    }

    /// Parse `_routes.json` contents
    pub fn parse_routes_str(content: &str) -> Result<CloudflareRoutesConfig> {
        serde_json::from_str(content).map_err(|e| {
            HgbError::Execution(format!("Failed to parse _routes.json: {}", e))
        })
    }

    /// Load configuration directly from a workspace directory
    pub fn load_from_workspace<P: AsRef<Path>>(workspace_root: P) -> Result<Self> {
        let root = workspace_root.as_ref();
        let wrangler_path = root.join("wrangler.toml");
        let routes_path = root.join("_routes.json");

        let (name, compatibility_date, pages_build_output_dir, vars, parsed_edge_url) = if wrangler_path.exists() {
            let content = fs::read_to_string(&wrangler_path).map_err(|e| {
                HgbError::Execution(format!("Failed to read {}: {}", wrangler_path.display(), e))
            })?;
            Self::parse_wrangler_str(&content)
        } else {
            let mut v = HashMap::new();
            v.insert("ENVIRONMENT".to_string(), "production".to_string());
            v.insert("DEPLOYED_BY".to_string(), "hagibis-edge-deployer".to_string());
            ("my-vibe-app".to_string(), DEFAULT_COMPATIBILITY_DATE.to_string(), None, v, None)
        };

        let routes = if routes_path.exists() {
            let content = fs::read_to_string(&routes_path).map_err(|e| {
                HgbError::Execution(format!("Failed to read {}: {}", routes_path.display(), e))
            })?;
            Some(Self::parse_routes_str(&content)?)
        } else {
            Some(CloudflareRoutesConfig::default())
        };

        // Determine edge URL: explicit edge_url > vars.EDGE_URL > env var > default
        let edge_url = parsed_edge_url
            .or_else(|| vars.get("EDGE_URL").cloned())
            .or_else(|| vars.get("CF_EDGE_URL").cloned())
            .or_else(|| std::env::var("HGB_EDGE_URL").ok())
            .or_else(|| std::env::var("CLOUDFLARE_WORKERS_URL").ok())
            .unwrap_or_else(|| format!("https://{}.workers.dev", name));

        // Determine API token
        let api_token = vars.get("CF_API_TOKEN")
            .cloned()
            .or_else(|| vars.get("API_TOKEN").cloned())
            .or_else(|| std::env::var("CF_API_TOKEN").ok())
            .or_else(|| std::env::var("CLOUDFLARE_API_TOKEN").ok())
            .or_else(|| std::env::var("HGB_EDGE_TOKEN").ok());

        let buffer_db_path = root.join(".hgb").join("edge_mesh_buffer.db");
        let mutation_vectors_db_path = root.join(".hgb").join("ollama_mutation_vectors.db");

        Ok(Self {
            name,
            compatibility_date,
            pages_build_output_dir,
            vars,
            edge_url,
            api_token,
            routes,
            buffer_db_path,
            mutation_vectors_db_path,
            timeout_ms: DEFAULT_EDGE_TIMEOUT_MS,
        })
    }
}

// =================================================================================================
// Edge Mesh Payloads & Telemetry Models
// =================================================================================================

/// Hardware status telemetry payload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareTelemetry {
    pub cpu_cores: usize,
    pub load_average: f32,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
    pub gpu_available: bool,
    pub gpu_device_name: Option<String>,
    pub thermal_status: String,
}

/// Swarm peer telemetry for edge status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmPeerTelemetry {
    pub peer_id: String,
    pub role: String,
    pub endpoint: String,
    pub latency_ms: u64,
    pub last_seen: String,
    pub active: bool,
}

/// Telemetry payload sent from local Hagibis node to Cloudflare Workers edge
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeTelemetryPayload {
    pub node_id: String,
    pub app_name: String,
    pub version: String,
    pub status: String,
    pub active_models: Vec<String>,
    pub hardware: HardwareTelemetry,
    pub swarm_peers: Vec<SwarmPeerTelemetry>,
    pub timestamp: String,
}

/// Receipt returned after publishing telemetry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgePublishReceipt {
    pub success: bool,
    pub published_to_edge: bool,
    pub buffered_locally: bool,
    pub endpoint: String,
    pub payload_hash: String,
    pub latency_ms: u64,
    pub message: String,
    pub timestamp: String,
}

// =================================================================================================
// Vector Embedding & Storage Synchronization Models
// =================================================================================================

/// Semantic vector chunk for edge synchronization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeVectorChunk {
    pub id: String,
    pub filepath: String,
    pub chunk_index: usize,
    pub blake3_hash: String,
    pub content_snippet: String,
    pub values: Vec<f32>,
    pub metadata: HashMap<String, String>,
    pub updated_at: String,
}

/// Target Cloudflare edge storage tier
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeStorageBackend {
    Vectorize,
    KV,
    D1,
}

impl std::fmt::Display for EdgeStorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgeStorageBackend::Vectorize => write!(f, "Cloudflare Vectorize"),
            EdgeStorageBackend::KV => write!(f, "Cloudflare Workers KV"),
            EdgeStorageBackend::D1 => write!(f, "Cloudflare D1 SQL"),
        }
    }
}

/// Batch of vector embeddings sent to edge
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeVectorSyncBatch {
    pub app_name: String,
    pub target_storage: EdgeStorageBackend,
    pub total_chunks: usize,
    pub vectors: Vec<EdgeVectorChunk>,
    pub sync_timestamp: String,
}

/// Receipt for vector synchronization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeVectorSyncReceipt {
    pub success: bool,
    pub synced_to_edge: bool,
    pub buffered_locally: bool,
    pub chunks_synced: usize,
    pub storage_backend: EdgeStorageBackend,
    pub latency_ms: u64,
    pub message: String,
    pub timestamp: String,
}

// =================================================================================================
// Remote Triggers & Dispatch Models
// =================================================================================================

/// Types of remote triggers supported by Cloudflare Workers edge
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteTriggerKind {
    ExternalCodeReview,
    WebhookNotification,
    DistributedTaskExecution,
    ConfigInvalidation,
    ModelWarmup,
}

impl std::fmt::Display for RemoteTriggerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteTriggerKind::ExternalCodeReview => write!(f, "External Code Review"),
            RemoteTriggerKind::WebhookNotification => write!(f, "Webhook Notification"),
            RemoteTriggerKind::DistributedTaskExecution => write!(f, "Distributed Task Execution"),
            RemoteTriggerKind::ConfigInvalidation => write!(f, "Config Invalidation"),
            RemoteTriggerKind::ModelWarmup => write!(f, "Model Warmup"),
        }
    }
}

/// Remote trigger payload from Cloudflare Workers edge
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteTrigger {
    pub id: String,
    pub kind: RemoteTriggerKind,
    pub source: String,
    pub payload: Value,
    pub priority: u8,
    pub created_at: String,
    pub ack_endpoint: Option<String>,
}

/// Trigger acknowledgement sent back to edge
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteTriggerAck {
    pub trigger_id: String,
    pub status: String,
    pub completed_at: String,
    pub result: Value,
}

// =================================================================================================
// Offline Resilience: SQLite WAL Buffer Models
// =================================================================================================

/// Offline buffer item stored in SQLite WAL
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BufferedItem {
    pub id: String,
    pub item_type: String, // "telemetry", "vector_batch", "trigger_ack", "remote_trigger"
    pub payload: String,
    pub status: String,    // "pending", "flushed", "failed"
    pub retry_count: u32,
    pub created_at: String,
    pub last_attempt_at: Option<String>,
}

/// Report after auto-flushing offline buffer
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BufferFlushReport {
    pub total_pending_before: usize,
    pub successfully_flushed: usize,
    pub failed_to_flush: usize,
    pub pending_remaining: usize,
    pub latency_ms: u64,
    pub message: String,
}

/// Mesh status report for CLI inspection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloudflareEdgeMeshStatus {
    pub app_name: String,
    pub compatibility_date: String,
    pub edge_url: String,
    pub routes: Option<CloudflareRoutesConfig>,
    pub buffer_db_path: String,
    pub pending_buffered_items: usize,
    pub local_mutation_vectors_count: usize,
    pub last_sync_timestamp: Option<String>,
    pub connectivity_status: String,
}

/// Full bidirectional mesh sync report
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FullMeshSyncResult {
    pub app_name: String,
    pub edge_url: String,
    pub telemetry_receipt: EdgePublishReceipt,
    pub vector_receipt: EdgeVectorSyncReceipt,
    pub triggers_received: Vec<RemoteTrigger>,
    pub buffer_flush_report: BufferFlushReport,
    pub total_duration_ms: u64,
    pub overall_status: String,
}

// =================================================================================================
// Cloudflare Edge Mesh Engine
// =================================================================================================

/// Production-grade engine for Cloudflare Workers edge mesh synchronization
pub struct CloudflareEdgeMeshEngine {
    config: CloudflareEdgeMeshConfig,
    client: Client,
    workspace_root: PathBuf,
}

impl CloudflareEdgeMeshEngine {
    /// Create new engine instance with given configuration
    pub fn new(config: CloudflareEdgeMeshConfig, workspace_root: PathBuf) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|e| HgbError::Execution(format!("Failed to build HTTP client: {}", e)))?;

        let engine = Self {
            config,
            client,
            workspace_root,
        };

        engine.init_buffer_schema()?;
        Ok(engine)
    }

    /// Load engine directly from workspace root
    pub fn load_from_workspace<P: AsRef<Path>>(workspace_root: P) -> Result<Self> {
        let root = workspace_root.as_ref().to_path_buf();
        let config = CloudflareEdgeMeshConfig::load_from_workspace(&root)?;
        Self::new(config, root)
    }

    /// Access configuration
    pub fn config(&self) -> &CloudflareEdgeMeshConfig {
        &self.config
    }

    /// Mutably access configuration
    pub fn config_mut(&mut self) -> &mut CloudflareEdgeMeshConfig {
        &mut self.config
    }

    /// Override the edge URL endpoint
    pub fn set_edge_url(&mut self, url: String) {
        self.config.edge_url = url;
    }

    // ---------------------------------------------------------------------------------------------
    // SQLite WAL Buffer Engine
    // ---------------------------------------------------------------------------------------------

    /// Ensure SQLite database directory exists and initialize WAL buffer schema
    fn init_buffer_schema(&self) -> Result<()> {
        if let Some(parent) = self.config.buffer_db_path.parent() {
            if !parent.exists() {
                let _ = fs::create_dir_all(parent);
            }
        }

        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!(
                "Failed to open offline buffer DB at {}: {}",
                self.config.buffer_db_path.display(),
                e
            ))
        })?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS edge_buffer (
                 id TEXT PRIMARY KEY,
                 item_type TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 status TEXT NOT NULL,
                 retry_count INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL,
                 last_attempt_at TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_edge_buffer_status ON edge_buffer(status);",
        ).map_err(|e| HgbError::Execution(format!("Failed to initialize buffer schema: {}", e)))?;

        Ok(())
    }

    /// Enqueue an item into local SQLite WAL buffer when offline
    pub fn buffer_queue_item(&self, item_type: &str, payload: &str) -> Result<String> {
        self.init_buffer_schema()?;
        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to buffer DB: {}", e))
        })?;

        let id = uuid::Uuid::new_v4().to_string();
        let now_str = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO edge_buffer (id, item_type, payload, status, retry_count, created_at, last_attempt_at)
             VALUES (?, ?, ?, 'pending', 0, ?, NULL)",
            params![id, item_type, payload, now_str],
        ).map_err(|e| HgbError::Execution(format!("Failed to enqueue buffer item: {}", e)))?;

        Ok(id)
    }

    /// Retrieve items from the offline buffer
    pub fn get_buffered_items(&self, status_filter: Option<&str>) -> Result<Vec<BufferedItem>> {
        if !self.config.buffer_db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to buffer DB: {}", e))
        })?;

        let query = match status_filter {
            Some(_) => "SELECT id, item_type, payload, status, retry_count, created_at, last_attempt_at FROM edge_buffer WHERE status = ? ORDER BY created_at ASC",
            None => "SELECT id, item_type, payload, status, retry_count, created_at, last_attempt_at FROM edge_buffer ORDER BY created_at ASC",
        };

        let mut stmt = conn.prepare(query).map_err(|e| {
            HgbError::Execution(format!("Failed to prepare buffer select query: {}", e))
        })?;

        let mut items = Vec::new();
        if let Some(status) = status_filter {
            let mut rows = stmt.query(params![status]).map_err(|e| {
                HgbError::Execution(format!("Failed to query buffer items: {}", e))
            })?;
            while let Some(row) = rows.next().map_err(|e| HgbError::Execution(format!("Row error: {}", e)))? {
                items.push(BufferedItem {
                    id: row.get(0).map_err(|e| HgbError::Execution(format!("Get id error: {}", e)))?,
                    item_type: row.get(1).map_err(|e| HgbError::Execution(format!("Get item_type error: {}", e)))?,
                    payload: row.get(2).map_err(|e| HgbError::Execution(format!("Get payload error: {}", e)))?,
                    status: row.get(3).map_err(|e| HgbError::Execution(format!("Get status error: {}", e)))?,
                    retry_count: row.get::<_, i64>(4).map_err(|e| HgbError::Execution(format!("Get retry_count error: {}", e)))? as u32,
                    created_at: row.get(5).map_err(|e| HgbError::Execution(format!("Get created_at error: {}", e)))?,
                    last_attempt_at: row.get(6).map_err(|e| HgbError::Execution(format!("Get last_attempt_at error: {}", e)))?,
                });
            }
        } else {
            let mut rows = stmt.query([]).map_err(|e| {
                HgbError::Execution(format!("Failed to query buffer items: {}", e))
            })?;
            while let Some(row) = rows.next().map_err(|e| HgbError::Execution(format!("Row error: {}", e)))? {
                items.push(BufferedItem {
                    id: row.get(0).map_err(|e| HgbError::Execution(format!("Get id error: {}", e)))?,
                    item_type: row.get(1).map_err(|e| HgbError::Execution(format!("Get item_type error: {}", e)))?,
                    payload: row.get(2).map_err(|e| HgbError::Execution(format!("Get payload error: {}", e)))?,
                    status: row.get(3).map_err(|e| HgbError::Execution(format!("Get status error: {}", e)))?,
                    retry_count: row.get::<_, i64>(4).map_err(|e| HgbError::Execution(format!("Get retry_count error: {}", e)))? as u32,
                    created_at: row.get(5).map_err(|e| HgbError::Execution(format!("Get created_at error: {}", e)))?,
                    last_attempt_at: row.get(6).map_err(|e| HgbError::Execution(format!("Get last_attempt_at error: {}", e)))?,
                });
            }
        }

        Ok(items)
    }

    /// Mark an item as successfully flushed
    pub fn mark_buffer_item_flushed(&self, id: &str) -> Result<()> {
        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to buffer DB: {}", e))
        })?;

        let now_str = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE edge_buffer SET status = 'flushed', last_attempt_at = ? WHERE id = ?",
            params![now_str, id],
        ).map_err(|e| HgbError::Execution(format!("Failed to mark buffer flushed: {}", e)))?;

        Ok(())
    }

    /// Update attempt timestamp and increment retry counter
    fn record_buffer_item_failure(&self, id: &str) -> Result<()> {
        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to buffer DB: {}", e))
        })?;

        let now_str = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE edge_buffer SET retry_count = retry_count + 1, last_attempt_at = ? WHERE id = ?",
            params![now_str, id],
        ).map_err(|e| HgbError::Execution(format!("Failed to record buffer failure: {}", e)))?;

        Ok(())
    }

    /// Delete an item from buffer DB
    pub fn delete_buffer_item(&self, id: &str) -> Result<()> {
        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to buffer DB: {}", e))
        })?;

        conn.execute("DELETE FROM edge_buffer WHERE id = ?", params![id])
            .map_err(|e| HgbError::Execution(format!("Failed to delete buffer item: {}", e)))?;

        Ok(())
    }

    /// Get total number of pending items in buffer
    pub fn get_pending_buffer_count(&self) -> Result<usize> {
        if !self.config.buffer_db_path.exists() {
            return Ok(0);
        }
        let conn = Connection::open(&self.config.buffer_db_path).map_err(|e| {
            HgbError::Execution(format!("Failed to connect to buffer DB: {}", e))
        })?;

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edge_buffer WHERE status = 'pending'",
            [],
            |r| r.get(0),
        ).unwrap_or(0);

        Ok(count as usize)
    }

    // ---------------------------------------------------------------------------------------------
    // Hardware & Node Telemetry Collection
    // ---------------------------------------------------------------------------------------------

    /// Read live system CPU load from /proc/loadavg or fallback
    fn read_cpu_load() -> f32 {
        if let Ok(content) = fs::read_to_string("/proc/loadavg") {
            if let Some(first) = content.split_whitespace().next() {
                if let Ok(val) = first.parse::<f32>() {
                    return val;
                }
            }
        }
        0.12
    }

    /// Read system memory from /proc/meminfo or fallback
    fn read_memory_stats() -> (u64, u64) {
        let mut total = 16384u64;
        let mut available = 8192u64;

        if let Ok(content) = fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    if let Some(kb_str) = line.split_whitespace().nth(1) {
                        if let Ok(kb) = kb_str.parse::<u64>() {
                            total = kb / 1024;
                        }
                    }
                } else if line.starts_with("MemAvailable:") {
                    if let Some(kb_str) = line.split_whitespace().nth(1) {
                        if let Ok(kb) = kb_str.parse::<u64>() {
                            available = kb / 1024;
                        }
                    }
                }
            }
        }
        let used = if total > available { total - available } else { 0 };
        (used, total)
    }

    /// Detect GPU availability
    fn detect_gpu() -> (bool, Option<String>) {
        if Path::new("/dev/nvidia0").exists() || Path::new("/dev/nvidiactl").exists() {
            return (true, Some("NVIDIA GPU (CUDA)".to_string()));
        }
        if Path::new("/dev/kfd").exists() || Path::new("/dev/dri/renderD128").exists() {
            return (true, Some("Direct Rendering GPU (DRI/ROCm)".to_string()));
        }
        (false, None)
    }

    /// Generate or retrieve stable local node identifier
    pub fn get_or_create_node_id(&self) -> String {
        let node_file = self.workspace_root.join(".hgb").join("node_id");
        if node_file.exists() {
            if let Ok(id) = fs::read_to_string(&node_file) {
                let trimmed = id.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }

        // Generate persistent node id
        let hash = blake3::hash(self.workspace_root.to_string_lossy().as_bytes());
        let new_id = format!("hgb-node-{}", &hash.to_hex()[..12]);
        if let Some(parent) = node_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&node_file, &new_id);
        new_id
    }

    /// Collect comprehensive live telemetry from local Hagibis daemon and host environment
    pub fn collect_local_telemetry(&self) -> Result<EdgeTelemetryPayload> {
        let (mem_used, mem_total) = Self::read_memory_stats();
        let (has_gpu, gpu_name) = Self::detect_gpu();
        let cpu_cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let load_avg = Self::read_cpu_load();

        let hardware = HardwareTelemetry {
            cpu_cores,
            load_average: load_avg,
            memory_used_mb: mem_used,
            memory_total_mb: mem_total,
            gpu_available: has_gpu,
            gpu_device_name: gpu_name,
            thermal_status: if load_avg > 4.0 { "elevated".to_string() } else { "nominal".to_string() },
        };

        // Collect active models
        let mut active_models = vec![
            "qwen2.5-coder:1.5b".to_string(),
            "phi3:3.8b".to_string(),
        ];
        let models_file = self.workspace_root.join(".hgb").join("active_models.json");
        if models_file.exists() {
            if let Ok(content) = fs::read_to_string(&models_file) {
                if let Ok(list) = serde_json::from_str::<Vec<String>>(&content) {
                    if !list.is_empty() {
                        active_models = list;
                    }
                }
            }
        }

        // Collect swarm peers if any
        let mut swarm_peers = Vec::new();
        let peers_file = self.workspace_root.join(".hgb").join("mesh_peers.json");
        if peers_file.exists() {
            if let Ok(content) = fs::read_to_string(&peers_file) {
                if let Ok(peers) = serde_json::from_str::<Vec<Value>>(&content) {
                    for p in peers {
                        if let Some(id) = p.get("node_id").and_then(|v| v.as_str()) {
                            swarm_peers.push(SwarmPeerTelemetry {
                                peer_id: id.to_string(),
                                role: p.get("role").and_then(|v| v.as_str()).unwrap_or("worker").to_string(),
                                endpoint: p.get("endpoint").and_then(|v| v.as_str()).unwrap_or("127.0.0.1:8970").to_string(),
                                latency_ms: p.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(5),
                                last_seen: Utc::now().to_rfc3339(),
                                active: true,
                            });
                        }
                    }
                }
            }
        }

        if swarm_peers.is_empty() {
            swarm_peers.push(SwarmPeerTelemetry {
                peer_id: format!("{}-local", self.config.name),
                role: "primary_daemon".to_string(),
                endpoint: "127.0.0.1:8970".to_string(),
                latency_ms: 1,
                last_seen: Utc::now().to_rfc3339(),
                active: true,
            });
        }

        Ok(EdgeTelemetryPayload {
            node_id: self.get_or_create_node_id(),
            app_name: self.config.name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            status: "healthy".to_string(),
            active_models,
            hardware,
            swarm_peers,
            timestamp: Utc::now().to_rfc3339(),
        })
    }

    /// Build standard authorization headers
    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref token) = self.config.api_token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, val);
            }
        }
        headers
    }

    // ---------------------------------------------------------------------------------------------
    // Task 1: Telemetry Publishing
    // ---------------------------------------------------------------------------------------------

    /// Publish local Hagibis telemetry to Cloudflare Workers edge endpoint.
    /// If network is offline or endpoint unreachable, buffers locally in SQLite WAL.
    pub async fn publish_telemetry(&self, custom_telemetry: Option<EdgeTelemetryPayload>) -> Result<EdgePublishReceipt> {
        let telemetry = match custom_telemetry {
            Some(t) => t,
            None => self.collect_local_telemetry()?,
        };

        let start = Instant::now();
        let payload_json = serde_json::to_string(&telemetry).map_err(|e| {
            HgbError::Execution(format!("Failed to serialize telemetry: {}", e))
        })?;
        let payload_hash = blake3::hash(payload_json.as_bytes()).to_hex().to_string();
        let url = format!("{}/api/v1/telemetry", self.config.edge_url.trim_end_matches('/'));

        // Attempt edge delivery
        let edge_post_result = self.client
            .post(&url)
            .headers(self.build_headers())
            .body(payload_json.clone())
            .send()
            .await;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        match edge_post_result {
            Ok(resp) if resp.status().is_success() => {
                Ok(EdgePublishReceipt {
                    success: true,
                    published_to_edge: true,
                    buffered_locally: false,
                    endpoint: url,
                    payload_hash,
                    latency_ms: elapsed_ms,
                    message: "Telemetry successfully published to Cloudflare Workers edge".to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                })
            }
            error_or_failure => {
                // Network failure or non-success status: buffer locally in SQLite WAL
                let err_msg = match error_or_failure {
                    Ok(resp) => format!("Edge returned HTTP {}", resp.status()),
                    Err(e) => format!("Network unreachable: {}", e),
                };

                let buf_id = self.buffer_queue_item("telemetry", &payload_json)?;

                Ok(EdgePublishReceipt {
                    success: true, // Gracefully handled
                    published_to_edge: false,
                    buffered_locally: true,
                    endpoint: url,
                    payload_hash,
                    latency_ms: elapsed_ms,
                    message: format!("Edge delivery deferred: {}. Buffered in WAL ({})", err_msg, buf_id),
                    timestamp: Utc::now().to_rfc3339(),
                })
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Task 2: Vector Embedding & Mutation Sync
    // ---------------------------------------------------------------------------------------------

    /// Read local SQLite mutation vector snapshots from `.hgb/ollama_mutation_vectors.db`
    pub fn read_local_mutation_vectors(&self) -> Result<Vec<EdgeVectorChunk>> {
        if !self.config.mutation_vectors_db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = Connection::open(&self.config.mutation_vectors_db_path).map_err(|e| {
            HgbError::Execution(format!(
                "Failed to open mutation vectors database at {}: {}",
                self.config.mutation_vectors_db_path.display(),
                e
            ))
        })?;

        // Verify table exists
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mutation_chunks'",
            [],
            |r| r.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap_or(false);

        if !table_exists {
            return Ok(Vec::new());
        }

        let mut stmt = conn.prepare(
            "SELECT id, filepath, chunk_index, blake3_hash, content, embedding_blob, updated_at
             FROM mutation_chunks ORDER BY filepath, chunk_index",
        ).map_err(|e| HgbError::Execution(format!("Failed to prepare vector select: {}", e)))?;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let filepath: String = row.get(1)?;
            let chunk_index: i64 = row.get(2)?;
            let blake3_hash: String = row.get(3)?;
            let content: String = row.get(4)?;
            let blob: Vec<u8> = row.get(5)?;
            let updated_at: String = row.get(6)?;

            // Convert IEEE-754 little-endian binary blob back to Vec<f32>
            let values: Vec<f32> = blob
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap_or([0; 4])))
                .collect();

            let mut metadata = HashMap::new();
            metadata.insert("source".to_string(), "hagibis_ollama_pipeline".to_string());
            metadata.insert("filepath".to_string(), filepath.clone());

            let snippet = if content.len() > 180 {
                format!("{}...", &content[..180])
            } else {
                content
            };

            Ok(EdgeVectorChunk {
                id,
                filepath,
                chunk_index: chunk_index as usize,
                blake3_hash,
                content_snippet: snippet,
                values,
                metadata,
                updated_at,
            })
        }).map_err(|e| HgbError::Execution(format!("Failed to query vector chunks: {}", e)))?;

        let mut chunks = Vec::new();
        for r in rows {
            if let Ok(chunk) = r {
                chunks.push(chunk);
            }
        }
        Ok(chunks)
    }

    /// Sync local SQLite mutation vector snapshots with Cloudflare Vectorize / KV / D1 edge storage.
    /// If network is offline or Cloudflare Worker endpoint is unreachable, buffers locally in SQLite WAL.
    pub async fn sync_vectors(&self, vectors: Option<Vec<EdgeVectorChunk>>) -> Result<EdgeVectorSyncReceipt> {
        let chunks = match vectors {
            Some(v) => v,
            None => self.read_local_mutation_vectors()?,
        };

        let start = Instant::now();
        let total_chunks = chunks.len();
        let backend = EdgeStorageBackend::Vectorize;

        let batch = EdgeVectorSyncBatch {
            app_name: self.config.name.clone(),
            target_storage: backend,
            total_chunks,
            vectors: chunks,
            sync_timestamp: Utc::now().to_rfc3339(),
        };

        let payload_json = serde_json::to_string(&batch).map_err(|e| {
            HgbError::Execution(format!("Failed to serialize vector batch: {}", e))
        })?;

        let url = format!("{}/api/v1/vectors/sync", self.config.edge_url.trim_end_matches('/'));

        // Attempt delivery to edge
        let edge_post_result = self.client
            .post(&url)
            .headers(self.build_headers())
            .body(payload_json.clone())
            .send()
            .await;

        let elapsed_ms = start.elapsed().as_millis() as u64;

        match edge_post_result {
            Ok(resp) if resp.status().is_success() => {
                Ok(EdgeVectorSyncReceipt {
                    success: true,
                    synced_to_edge: true,
                    buffered_locally: false,
                    chunks_synced: total_chunks,
                    storage_backend: backend,
                    latency_ms: elapsed_ms,
                    message: format!("Successfully synced {} vector chunks to {}", total_chunks, backend),
                    timestamp: Utc::now().to_rfc3339(),
                })
            }
            error_or_failure => {
                let err_msg = match error_or_failure {
                    Ok(resp) => format!("Edge returned HTTP {}", resp.status()),
                    Err(e) => format!("Network unreachable: {}", e),
                };

                // Queue in SQLite WAL buffer
                let buf_id = self.buffer_queue_item("vector_batch", &payload_json)?;

                Ok(EdgeVectorSyncReceipt {
                    success: true,
                    synced_to_edge: false,
                    buffered_locally: true,
                    chunks_synced: total_chunks,
                    storage_backend: backend,
                    latency_ms: elapsed_ms,
                    message: format!("Vector sync deferred: {}. Buffered in WAL ({})", err_msg, buf_id),
                    timestamp: Utc::now().to_rfc3339(),
                })
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Task 3: Remote Trigger Dispatch & Polling
    // ---------------------------------------------------------------------------------------------

    /// Poll pending remote triggers from Cloudflare Workers edge
    pub async fn poll_remote_triggers(&self) -> Result<Vec<RemoteTrigger>> {
        let url = format!("{}/api/v1/triggers/pending", self.config.edge_url.trim_end_matches('/'));

        let resp = match self.client
            .get(&url)
            .headers(self.build_headers())
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()), // Network offline, return empty list gracefully
        };

        if !resp.status().is_success() {
            return Ok(Vec::new());
        }

        let triggers = resp.json::<Vec<RemoteTrigger>>().await.unwrap_or_default();
        Ok(triggers)
    }

    /// Dispatch trigger acknowledgement back to Cloudflare Workers edge.
    /// If network is offline, buffers acknowledgement in SQLite WAL.
    pub async fn dispatch_trigger_ack(&self, ack: &RemoteTriggerAck) -> Result<bool> {
        let payload_json = serde_json::to_string(ack).map_err(|e| {
            HgbError::Execution(format!("Failed to serialize trigger ack: {}", e))
        })?;

        let url = format!("{}/api/v1/triggers/{}/ack", self.config.edge_url.trim_end_matches('/'), ack.trigger_id);

        let res = self.client
            .post(&url)
            .headers(self.build_headers())
            .body(payload_json.clone())
            .send()
            .await;

        match res {
            Ok(r) if r.status().is_success() => Ok(true),
            _ => {
                self.buffer_queue_item("trigger_ack", &payload_json)?;
                Ok(false)
            }
        }
    }

    /// Dispatch an outbound remote trigger from local Hagibis to Cloudflare Workers edge.
    pub async fn dispatch_remote_trigger(&self, trigger: &RemoteTrigger) -> Result<bool> {
        let payload_json = serde_json::to_string(trigger).map_err(|e| {
            HgbError::Execution(format!("Failed to serialize remote trigger: {}", e))
        })?;

        let url = format!("{}/api/v1/triggers/dispatch", self.config.edge_url.trim_end_matches('/'));

        let res = self.client
            .post(&url)
            .headers(self.build_headers())
            .body(payload_json.clone())
            .send()
            .await;

        match res {
            Ok(r) if r.status().is_success() => Ok(true),
            _ => {
                self.buffer_queue_item("remote_trigger", &payload_json)?;
                Ok(false)
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Task 4: Offline Buffer Auto-Flushing
    // ---------------------------------------------------------------------------------------------

    /// Flushes all pending buffered items (telemetry, vectors, trigger acks) from SQLite WAL to edge
    pub async fn flush_buffer(&self) -> Result<BufferFlushReport> {
        let start = Instant::now();
        let pending_items = self.get_buffered_items(Some("pending"))?;
        let total_pending = pending_items.len();

        if total_pending == 0 {
            return Ok(BufferFlushReport {
                total_pending_before: 0,
                successfully_flushed: 0,
                failed_to_flush: 0,
                pending_remaining: 0,
                latency_ms: 0,
                message: "No pending buffered items in SQLite WAL".to_string(),
            });
        }

        let mut flushed = 0;
        let mut failed = 0;

        for item in pending_items {
            let target_endpoint = match item.item_type.as_str() {
                "telemetry" => format!("{}/api/v1/telemetry", self.config.edge_url.trim_end_matches('/')),
                "vector_batch" => format!("{}/api/v1/vectors/sync", self.config.edge_url.trim_end_matches('/')),
                "trigger_ack" => format!("{}/api/v1/triggers/ack", self.config.edge_url.trim_end_matches('/')),
                "remote_trigger" => format!("{}/api/v1/triggers/dispatch", self.config.edge_url.trim_end_matches('/')),
                _ => format!("{}/api/v1/events", self.config.edge_url.trim_end_matches('/')),
            };

            let send_res = self.client
                .post(&target_endpoint)
                .headers(self.build_headers())
                .body(item.payload.clone())
                .send()
                .await;

            match send_res {
                Ok(resp) if resp.status().is_success() => {
                    let _ = self.mark_buffer_item_flushed(&item.id);
                    flushed += 1;
                }
                _ => {
                    let _ = self.record_buffer_item_failure(&item.id);
                    failed += 1;
                }
            }
        }

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let remaining = total_pending.saturating_sub(flushed);

        Ok(BufferFlushReport {
            total_pending_before: total_pending,
            successfully_flushed: flushed,
            failed_to_flush: failed,
            pending_remaining: remaining,
            latency_ms: elapsed_ms,
            message: format!("Flushed {}/{} items from SQLite WAL buffer ({} ms)", flushed, total_pending, elapsed_ms),
        })
    }

    // ---------------------------------------------------------------------------------------------
    // Status & Bidirectional Full Synchronization
    // ---------------------------------------------------------------------------------------------

    /// Query edge mesh status including connectivity, routes, vectors, and offline buffer
    pub async fn status(&self) -> Result<CloudflareEdgeMeshStatus> {
        let pending_buffered = self.get_pending_buffer_count().unwrap_or(0);
        let vectors_count = self.read_local_mutation_vectors().map(|v| v.len()).unwrap_or(0);

        // Ping edge health endpoint
        let health_url = format!("{}/api/v1/health", self.config.edge_url.trim_end_matches('/'));
        let connectivity_status = match self.client.get(&health_url).send().await {
            Ok(r) if r.status().is_success() => "connected (live edge)".to_string(),
            Ok(r) => format!("reachable (HTTP {})", r.status()),
            Err(_) => {
                if pending_buffered > 0 {
                    format!("offline (buffered {} items in SQLite WAL)", pending_buffered)
                } else {
                    "offline / unreachable (WAL buffer ready)".to_string()
                }
            }
        };

        // Check if any sync timestamp is recorded
        let last_sync_timestamp = self.get_buffered_items(Some("flushed"))
            .ok()
            .and_then(|items| items.last().map(|i| i.created_at.clone()));

        Ok(CloudflareEdgeMeshStatus {
            app_name: self.config.name.clone(),
            compatibility_date: self.config.compatibility_date.clone(),
            edge_url: self.config.edge_url.clone(),
            routes: self.config.routes.clone(),
            buffer_db_path: self.config.buffer_db_path.to_string_lossy().to_string(),
            pending_buffered_items: pending_buffered,
            local_mutation_vectors_count: vectors_count,
            last_sync_timestamp,
            connectivity_status,
        })
    }

    /// Trigger full bidirectional synchronization:
    /// 1. Publishes live telemetry (with buffer fallback).
    /// 2. Syncs local SQLite mutation vector snapshots to edge (with buffer fallback).
    /// 3. Polls pending remote triggers.
    /// 4. Auto-flushes any previously buffered items in SQLite WAL.
    pub async fn sync_all(&self) -> Result<FullMeshSyncResult> {
        let start = Instant::now();

        // Step 1: Telemetry Publishing
        let telemetry_receipt = self.publish_telemetry(None).await?;

        // Step 2: Vector Sync
        let vector_receipt = self.sync_vectors(None).await?;

        // Step 3: Remote Trigger Polling
        let triggers_received = self.poll_remote_triggers().await?;

        // Step 4: Auto-flush offline WAL buffer
        let buffer_flush_report = self.flush_buffer().await?;

        let total_duration_ms = start.elapsed().as_millis() as u64;

        let overall_status = if telemetry_receipt.published_to_edge && vector_receipt.synced_to_edge {
            "LIVE_EDGE_SYNCED".to_string()
        } else if telemetry_receipt.buffered_locally || vector_receipt.buffered_locally {
            "OFFLINE_RESILIENT_BUFFERED".to_string()
        } else {
            "DEGRADED".to_string()
        };

        Ok(FullMeshSyncResult {
            app_name: self.config.name.clone(),
            edge_url: self.config.edge_url.clone(),
            telemetry_receipt,
            vector_receipt,
            triggers_received,
            buffer_flush_report,
            total_duration_ms,
            overall_status,
        })
    }
}

// =================================================================================================
// Unit Tests
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_wrangler_and_routes() {
        let wrangler_sample = r#"
name = "my-vibe-app"
compatibility_date = "2026-09-01"
pages_build_output_dir = "."

[vars]
ENVIRONMENT = "production"
DEPLOYED_BY = "hagibis-edge-deployer"
EDGE_URL = "https://custom-edge.workers.dev"
"#;

        let (name, compat, out_dir, vars, edge_url) = CloudflareEdgeMeshConfig::parse_wrangler_str(wrangler_sample);
        assert_eq!(name, "my-vibe-app");
        assert_eq!(compat, "2026-09-01");
        assert_eq!(out_dir, Some(".".to_string()));
        assert_eq!(vars.get("ENVIRONMENT").map(|s| s.as_str()), Some("production"));
        assert_eq!(vars.get("DEPLOYED_BY").map(|s| s.as_str()), Some("hagibis-edge-deployer"));
        assert_eq!(vars.get("EDGE_URL").map(|s| s.as_str()), Some("https://custom-edge.workers.dev"));
        assert_eq!(edge_url, None);

        let routes_sample = r#"{
            "version": 1,
            "include": ["/*"],
            "exclude": ["/static/*", "/assets/*", "/favicon.ico"]
        }"#;

        let routes = CloudflareEdgeMeshConfig::parse_routes_str(routes_sample).unwrap();
        assert_eq!(routes.version, 1);
        assert_eq!(routes.include, vec!["/*"]);
        assert_eq!(routes.exclude, vec!["/static/*", "/assets/*", "/favicon.ico"]);

        // Test route matching rules
        assert!(routes.matches("/"));
        assert!(routes.matches("/api/v1/telemetry"));
        assert!(routes.matches("/dashboard"));
        assert!(!routes.matches("/static/bundle.js"));
        assert!(!routes.matches("/assets/logo.png"));
        assert!(!routes.matches("/favicon.ico"));
    }

    #[test]
    fn test_edge_payload_serialization() {
        let hardware = HardwareTelemetry {
            cpu_cores: 16,
            load_average: 0.42,
            memory_used_mb: 4096,
            memory_total_mb: 32768,
            gpu_available: true,
            gpu_device_name: Some("NVIDIA RTX 4090".to_string()),
            thermal_status: "nominal".to_string(),
        };

        let peer = SwarmPeerTelemetry {
            peer_id: "node-worker-alpha".to_string(),
            role: "worker".to_string(),
            endpoint: "192.168.1.100:8970".to_string(),
            latency_ms: 3,
            last_seen: "2026-10-30T10:00:00Z".to_string(),
            active: true,
        };

        let telemetry = EdgeTelemetryPayload {
            node_id: "test-node-1".to_string(),
            app_name: "my-vibe-app".to_string(),
            version: "0.1.0".to_string(),
            status: "healthy".to_string(),
            active_models: vec!["qwen2.5-coder:1.5b".to_string()],
            hardware,
            swarm_peers: vec![peer],
            timestamp: "2026-10-30T10:00:00Z".to_string(),
        };

        // Roundtrip telemetry serialization
        let json_str = serde_json::to_string(&telemetry).unwrap();
        let deserialized: EdgeTelemetryPayload = serde_json::from_str(&json_str).unwrap();
        assert_eq!(telemetry, deserialized);

        // Vector Chunk serialization
        let chunk = EdgeVectorChunk {
            id: "src/lib.rs#0".to_string(),
            filepath: "src/lib.rs".to_string(),
            chunk_index: 0,
            blake3_hash: "abcdef123456".to_string(),
            content_snippet: "pub fn hello() {}".to_string(),
            values: vec![0.1f32, -0.2f32, 0.99f32],
            metadata: HashMap::new(),
            updated_at: "2026-10-30T10:00:00Z".to_string(),
        };
        let chunk_json = serde_json::to_string(&chunk).unwrap();
        let chunk_deserialized: EdgeVectorChunk = serde_json::from_str(&chunk_json).unwrap();
        assert_eq!(chunk, chunk_deserialized);

        // Remote Trigger serialization
        let trigger = RemoteTrigger {
            id: "trig-42".to_string(),
            kind: RemoteTriggerKind::ExternalCodeReview,
            source: "github-webhook".to_string(),
            payload: serde_json::json!({ "pr": 12, "action": "opened" }),
            priority: 1,
            created_at: "2026-10-30T10:00:00Z".to_string(),
            ack_endpoint: Some("/api/v1/triggers/trig-42/ack".to_string()),
        };
        let trig_json = serde_json::to_string(&trigger).unwrap();
        let trig_deserialized: RemoteTrigger = serde_json::from_str(&trig_json).unwrap();
        assert_eq!(trigger, trig_deserialized);
    }

    #[test]
    fn test_local_offline_buffer_wal_queuing_and_replay() {
        let dir = tempdir().unwrap();
        let buffer_db_path = dir.path().join("edge_mesh_buffer.db");

        let mut config = CloudflareEdgeMeshConfig::default();
        config.buffer_db_path = buffer_db_path.clone();

        let engine = CloudflareEdgeMeshEngine::new(config, dir.path().to_path_buf()).unwrap();

        // 1. Initial count must be 0
        assert_eq!(engine.get_pending_buffer_count().unwrap(), 0);

        // 2. Queue telemetry and vector items
        let id1 = engine.buffer_queue_item("telemetry", r#"{"status":"healthy"}"#).unwrap();
        let id2 = engine.buffer_queue_item("vector_batch", r#"{"chunks":[]}"#).unwrap();
        assert_eq!(engine.get_pending_buffer_count().unwrap(), 2);

        // 3. Inspect items
        let items = engine.get_buffered_items(Some("pending")).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, id1);
        assert_eq!(items[0].item_type, "telemetry");
        assert_eq!(items[1].id, id2);
        assert_eq!(items[1].item_type, "vector_batch");

        // 4. Mark item 1 flushed
        engine.mark_buffer_item_flushed(&id1).unwrap();
        assert_eq!(engine.get_pending_buffer_count().unwrap(), 1);

        let remaining = engine.get_buffered_items(Some("pending")).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, id2);

        // 5. Delete item 2
        engine.delete_buffer_item(&id2).unwrap();
        assert_eq!(engine.get_pending_buffer_count().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_mock_edge_roundtrip_sync_simulation() {
        use axum::extract::State;
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct MockEdgeState {
            telemetry_calls: AtomicUsize,
            vector_calls: AtomicUsize,
        }

        let state = Arc::new(MockEdgeState {
            telemetry_calls: AtomicUsize::new(0),
            vector_calls: AtomicUsize::new(0),
        });

        let app_state = state.clone();

        // Mock Cloudflare Workers edge router
        let app = Router::new()
            .route(
                "/api/v1/health",
                get(|| async { Json(serde_json::json!({ "status": "ok", "edge": "cloudflare" })) }),
            )
            .route(
                "/api/v1/telemetry",
                post(|State(st): State<Arc<MockEdgeState>>, Json(payload): Json<Value>| async move {
                    st.telemetry_calls.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({ "success": true, "received": payload["app_name"] }))
                }),
            )
            .route(
                "/api/v1/vectors/sync",
                post(|State(st): State<Arc<MockEdgeState>>, Json(payload): Json<Value>| async move {
                    st.vector_calls.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({ "success": true, "chunks": payload["total_chunks"] }))
                }),
            )
            .route(
                "/api/v1/triggers/pending",
                get(|| async {
                    Json(vec![RemoteTrigger {
                        id: "cf-trigger-101".to_string(),
                        kind: RemoteTriggerKind::ExternalCodeReview,
                        source: "cloudflare-edge".to_string(),
                        payload: serde_json::json!({ "repo": "hagibis" }),
                        priority: 2,
                        created_at: Utc::now().to_rfc3339(),
                        ack_endpoint: Some("/api/v1/triggers/cf-trigger-101/ack".to_string()),
                    }])
                }),
            )
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let edge_url = format!("http://127.0.0.1:{}", port);

        // Spawn mock edge server in background
        let server_handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let dir = tempdir().unwrap();
        let mut config = CloudflareEdgeMeshConfig::default();
        config.edge_url = edge_url.clone();
        config.buffer_db_path = dir.path().join("edge_mesh_buffer.db");
        config.mutation_vectors_db_path = dir.path().join("mutation_vectors.db");

        let engine = CloudflareEdgeMeshEngine::new(config, dir.path().to_path_buf()).unwrap();

        // 1. Check live status
        let status = engine.status().await.unwrap();
        assert!(status.connectivity_status.contains("live edge"));

        // 2. Publish Telemetry
        let tel_receipt = engine.publish_telemetry(None).await.unwrap();
        assert!(tel_receipt.published_to_edge);
        assert!(!tel_receipt.buffered_locally);
        assert_eq!(state.telemetry_calls.load(Ordering::SeqCst), 1);

        // 3. Sync Vector Snapshot
        let dummy_chunk = EdgeVectorChunk {
            id: "test.rs#0".to_string(),
            filepath: "test.rs".to_string(),
            chunk_index: 0,
            blake3_hash: "hash01".to_string(),
            content_snippet: "fn main() {}".to_string(),
            values: vec![0.5, 0.25],
            metadata: HashMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let vec_receipt = engine.sync_vectors(Some(vec![dummy_chunk])).await.unwrap();
        assert!(vec_receipt.synced_to_edge);
        assert!(!vec_receipt.buffered_locally);
        assert_eq!(state.vector_calls.load(Ordering::SeqCst), 1);

        // 4. Poll triggers
        let triggers = engine.poll_remote_triggers().await.unwrap();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].id, "cf-trigger-101");

        // 5. Simulate offline fallback by pointing to unreachable port
        let mut offline_engine = CloudflareEdgeMeshEngine::new(
            CloudflareEdgeMeshConfig {
                edge_url: "http://127.0.0.1:1".to_string(), // Unreachable port
                buffer_db_path: dir.path().join("offline_edge_buffer.db"),
                mutation_vectors_db_path: dir.path().join("offline_vectors.db"),
                timeout_ms: 500,
                ..Default::default()
            },
            dir.path().to_path_buf(),
        ).unwrap();

        let offline_tel = offline_engine.publish_telemetry(None).await.unwrap();
        assert!(!offline_tel.published_to_edge);
        assert!(offline_tel.buffered_locally);
        assert_eq!(offline_engine.get_pending_buffer_count().unwrap(), 1);

        // 6. Restore connectivity by updating edge_url and flush buffer
        offline_engine.set_edge_url(edge_url.clone());
        let flush_report = offline_engine.flush_buffer().await.unwrap();
        assert_eq!(flush_report.successfully_flushed, 1);
        assert_eq!(offline_engine.get_pending_buffer_count().unwrap(), 0);

        server_handle.abort();
    }
}
