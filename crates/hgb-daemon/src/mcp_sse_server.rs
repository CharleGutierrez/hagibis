//! # Native Model Context Protocol (MCP) Server-Sent Events (SSE) Server
//!
//! Exposes a fully compliant Model Context Protocol (MCP) HTTP/SSE transport server.
//! Allows any MCP-compatible IDE (Cursor, Windsurf, Claude Desktop, Cline, Roo Code) to stream
//! tool outputs, vector searches, and WASM executions live directly into the coding environment.
//!
//! Specification:
//! - `GET /sse`: Connects client to Server-Sent Events stream, generates session UUID, emits
//!   `event: endpoint\ndata: /message?sessionId=<uuid>`, and holds open bi-directional channel.
//! - `POST /message?sessionId=<uuid>`: Handles JSON-RPC 2.0 messages (`initialize`, `tools/list`,
//!   `tools/call`), streaming progress chunks and intermediate telemetry over the active SSE stream.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::RwLock;

use axum::extract::{Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};

use hgb_core::HagibisMcpServer;

/// Default port for the Hagibis MCP SSE server
pub const DEFAULT_MCP_SSE_PORT: u16 = 8083;

/// Resolve the path for the MCP auth token file (~/.hgb/mcp_auth_token)
pub fn auth_token_path() -> PathBuf {
    if let Ok(path) = std::env::var("HGB_MCP_AUTH_TOKEN_PATH") {
        PathBuf::from(path)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".hgb").join("mcp_auth_token")
    } else {
        PathBuf::from(".hgb").join("mcp_auth_token")
    }
}

/// Generate a 256-bit cryptographically secure hex token (32 bytes = 64 hex characters)
pub fn generate_256bit_hex_token() -> String {
    let mut bytes = [0u8; 32];
    let mut filled = false;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut bytes).is_ok() {
            filled = true;
        }
    }
    if !filled {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let h = blake3::hash(&now.to_le_bytes());
        bytes.copy_from_slice(h.as_bytes());
    }
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Load existing 256-bit hex token or generate and save with 0600 permissions
pub fn get_or_create_auth_token() -> std::io::Result<String> {
    if let Ok(env_token) = std::env::var("HGB_MCP_AUTH_TOKEN") {
        let trimmed = env_token.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }

    let path = auth_token_path();
    get_or_create_auth_token_at(&path)
}

/// Explicit-path token loader/creator (thread-safe and deterministic for testing)
pub fn get_or_create_auth_token_at(path: &Path) -> std::io::Result<String> {
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(path) {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }
    }

    let token = generate_256bit_hex_token();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(token.as_bytes())?;

        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, &token)?;
    }

    Ok(token)
}

/// Query parameters for POST /message
#[derive(Debug, Deserialize)]
pub struct MessageQuery {
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub profile: Option<String>,
}

/// Status telemetry for the MCP SSE server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSseServerStatus {
    pub bind_addr: String,
    pub active_sse_sessions: usize,
    pub total_connections: u64,
    pub total_messages_received: u64,
    pub total_events_streamed: u64,
    pub tools_count: usize,
}

/// Shared internal state for the MCP SSE server
pub struct McpSseState {
    pub workspace_root: PathBuf,
    pub sessions: Arc<RwLock<HashMap<String, UnboundedSender<Event>>>>,
    pub total_connections: AtomicU64,
    pub total_messages: AtomicU64,
    pub total_streamed_events: AtomicU64,
    pub mcp_server: Arc<HagibisMcpServer>,
    pub port: u16,
    pub auth_token: String,
    pub dynamic_registry: Arc<std::sync::RwLock<Option<Arc<crate::dynamic_mcp_registry::DynamicMcpRegistry>>>>,
    pub semantic_cache: Arc<hgb_storage::semantic_response_cache::SemanticResponseCache>,
    pub ipc_governor: Arc<crate::dynatune_ipc_governor::DynatuneIpcGovernor>,
    pub tenant_manager: Arc<hgb_core::multitenant_wasm_sandbox::TenantSessionManager>,
    pub swarm_orchestrator: Arc<std::sync::RwLock<Option<Arc<crate::distributed_swarm_orchestrator::DistributedSwarmOrchestrator>>>>,
    pub unified_hardware_loop: Arc<crate::unified_neural_hardware_loop::UnifiedNeuralHardwareLoop>,
    pub vector_memory_mesh: Arc<hgb_storage::vector_mcp_memory_mesh::VectorMcpMemoryMesh>,
}

impl McpSseState {
    pub fn new<P: AsRef<Path>>(workspace_root: P, port: u16) -> Self {
        let auth_token = get_or_create_auth_token().unwrap_or_else(|_| generate_256bit_hex_token());
        Self::with_auth_token(workspace_root, port, auth_token)
    }

    pub fn with_auth_token<P: AsRef<Path>>(workspace_root: P, port: u16, auth_token: impl Into<String>) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        let cache_db = ws.join(".hgb").join("semantic_cache.db");
        let semantic_cache = Arc::new(
            hgb_storage::semantic_response_cache::SemanticResponseCache::open(&cache_db, 10000, 0.92, 3600)
                .unwrap_or_else(|_| hgb_storage::semantic_response_cache::SemanticResponseCache::in_memory(10000, 0.92, 3600))
        );
        let mem_db = ws.join(".hgb").join("vector_memory.db");
        let vector_memory_mesh = Arc::new(
            hgb_storage::vector_mcp_memory_mesh::VectorMcpMemoryMesh::open_or_create(&mem_db)
                .unwrap_or_else(|_| hgb_storage::vector_mcp_memory_mesh::VectorMcpMemoryMesh::in_memory().expect("in-memory vector mesh"))
        );

        Self {
            workspace_root: ws.clone(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            total_connections: AtomicU64::new(0),
            total_messages: AtomicU64::new(0),
            total_streamed_events: AtomicU64::new(0),
            mcp_server: Arc::new(HagibisMcpServer::new(ws.clone())),
            port,
            auth_token: auth_token.into(),
            dynamic_registry: Arc::new(std::sync::RwLock::new(None)),
            semantic_cache,
            ipc_governor: Arc::new(crate::dynatune_ipc_governor::DynatuneIpcGovernor::new()),
            tenant_manager: Arc::new(hgb_core::multitenant_wasm_sandbox::TenantSessionManager::new().unwrap_or_else(|_| {
                hgb_core::multitenant_wasm_sandbox::TenantSessionManager::with_base_root(ws.join(".hgb").join("tenants")).expect("tenant manager")
            })),
            swarm_orchestrator: Arc::new(std::sync::RwLock::new(None)),
            unified_hardware_loop: crate::unified_neural_hardware_loop::UnifiedNeuralHardwareLoop::new(),
            vector_memory_mesh,
        }
    }

    pub async fn set_dynamic_registry(&self, reg: Arc<crate::dynamic_mcp_registry::DynamicMcpRegistry>) {
        let mut guard = self.dynamic_registry.write().unwrap();
        *guard = Some(reg);
    }

    pub fn set_dynamic_registry_sync(&self, reg: Arc<crate::dynamic_mcp_registry::DynamicMcpRegistry>) {
        let mut guard = self.dynamic_registry.write().unwrap();
        *guard = Some(reg);
    }

    /// Emit an SSE event to a specific session
    pub async fn emit_to_session(&self, session_id: &str, event: Event) -> bool {
        let sessions = self.sessions.read().await;
        if let Some(tx) = sessions.get(session_id) {
            if tx.send(event).is_ok() {
                self.total_streamed_events.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
        false
    }
}

/// The MCP SSE Server instance
pub struct McpSseServer {
    state: Arc<McpSseState>,
}

impl McpSseServer {
    pub fn new<P: AsRef<Path>>(workspace_root: P, port: u16) -> Self {
        Self {
            state: Arc::new(McpSseState::new(workspace_root, port)),
        }
    }

    pub fn with_auth_token<P: AsRef<Path>>(workspace_root: P, port: u16, auth_token: impl Into<String>) -> Self {
        Self {
            state: Arc::new(McpSseState::with_auth_token(workspace_root, port, auth_token)),
        }
    }

    pub fn state(&self) -> Arc<McpSseState> {
        Arc::clone(&self.state)
    }

    /// Construct Axum router for MCP SSE with Bearer auth middleware for /sse and /message
    pub fn create_router(state: Arc<McpSseState>) -> Router {
        let protected_routes = Router::new()
            .route("/sse", get(handle_sse_connect))
            .route("/message", post(handle_client_message))
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                require_bearer_auth,
            ));

        Router::new()
            .merge(protected_routes)
            .route("/status", get(handle_status))
            .with_state(state)
    }

    /// Launch the background MCP SSE HTTP server on localhost (127.0.0.1)
    pub async fn spawn_background(workspace_root: PathBuf, port: u16) -> tokio::task::JoinHandle<()> {
        let server = Self::new(workspace_root, port);
        let router = Self::create_router(server.state());
        let addr = SocketAddr::from(([127, 0, 0, 1], port));

        tokio::spawn(async move {
            if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                let _ = axum::serve(listener, router).await;
            }
        })
    }

    /// Construct MCP SSE server pre-wired with DynamicMcpRegistry
    pub fn with_dynamic_registry<P: AsRef<Path>>(
        workspace_root: P,
        port: u16,
        dynamic_registry: Arc<crate::dynamic_mcp_registry::DynamicMcpRegistry>,
    ) -> Self {
        let state = Arc::new(McpSseState::new(workspace_root, port));
        state.set_dynamic_registry_sync(Arc::clone(&dynamic_registry));
        dynamic_registry.attach_sse_state_sync(Arc::clone(&state));
        Self { state }
    }

    /// Launch background MCP SSE HTTP server pre-wired with DynamicMcpRegistry
    pub async fn spawn_background_with_registry(
        workspace_root: PathBuf,
        port: u16,
        dynamic_registry: Arc<crate::dynamic_mcp_registry::DynamicMcpRegistry>,
    ) -> tokio::task::JoinHandle<()> {
        let server = Self::with_dynamic_registry(workspace_root, port, dynamic_registry);
        let router = Self::create_router(server.state());
        let addr = SocketAddr::from(([127, 0, 0, 1], port));

        tokio::spawn(async move {
            if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                let _ = axum::serve(listener, router).await;
            }
        })
    }
}

/// Axum middleware requiring Bearer token authorization on /sse and /message endpoints
pub async fn require_bearer_auth(
    State(state): State<Arc<McpSseState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(auth_val) = req.headers().get(AUTHORIZATION) {
        if let Ok(auth_str) = auth_val.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                if token.trim() == state.auth_token.as_str() {
                    return Ok(next.run(req).await);
                }
            }
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}

/// GET /sse - Establishes Server-Sent Events connection
async fn handle_sse_connect(
    State(state): State<Arc<McpSseState>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::unbounded_channel::<Event>();

    // Initial endpoint announcement per MCP SSE spec
    let endpoint_data = format!("/message?sessionId={}", session_id);
    let initial_event = Event::default().event("endpoint").data(endpoint_data);
    let _ = tx.send(initial_event);

    state.total_connections.fetch_add(1, Ordering::Relaxed);
    state.total_streamed_events.fetch_add(1, Ordering::Relaxed);

    // Register session
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id.clone(), tx);
    }

    // Cleanup session on disconnect stream drop
    let sessions_map = Arc::clone(&state.sessions);
    let s_id = session_id.clone();

    let stream = stream::unfold((rx, sessions_map, s_id), |(mut rx, sessions, id)| async move {
        match rx.recv().await {
            Some(event) => Some((Ok(event), (rx, sessions, id))),
            None => {
                let mut sess = sessions.write().await;
                sess.remove(&id);
                None
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// POST /message?sessionId=<uuid> - Receives JSON-RPC 2.0 requests from MCP clients
pub async fn handle_client_message(
    State(state): State<Arc<McpSseState>>,
    Query(query): Query<MessageQuery>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    state.total_messages.fetch_add(1, Ordering::Relaxed);

    let id = body.get("id").cloned();
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(serde_json::json!({}));

    let session_id = query.session_id.unwrap_or_default();

    match method {
        "initialize" => {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        },
                        "logging": {}
                    },
                    "serverInfo": {
                        "name": "hagibis-mcp-sse",
                        "version": "0.2.0"
                    }
                }
            });
            (StatusCode::OK, Json(resp))
        }

        "notifications/initialized" => {
            // Client acknowledgement
            (StatusCode::OK, Json(serde_json::json!({"status": "acknowledged"})))
        }

        "ping" => {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            });
            (StatusCode::OK, Json(resp))
        }

        "tools/list" => {
            let profile_str = query.profile.as_deref().or_else(|| params.get("profile").and_then(|p| p.as_str()));
            let profile = hgb_core::HgbMcpProfile::from_str_opt(profile_str);
            let mut tools_val = HagibisMcpServer::list_tools_for_profile(profile);

            // Dynamically inject dynamic registry tools & prompt-driven tool creation interface
            if let Some(arr) = tools_val.as_array_mut() {
                arr.push(crate::multitenant_mcp_tool::MultitenantMcpTool::tool_definition());
                for swarm_tool in crate::distributed_swarm_orchestrator::SwarmMcpTools::tool_definitions() {
                    arr.push(swarm_tool);
                }
                for auto_tool in crate::dynamic_mcp_registry::autonomous_mcp_self_healing_loop::AutonomousMcpSelfHealingLoop::tool_definitions() {
                    arr.push(auto_tool);
                }
                arr.push(serde_json::json!({
                    "name": "index_codebase_memory",
                    "description": "Recursively scans, extracts symbols/functions, chunks, computes IEEE-754 semantic embeddings, and stores codebase chunks into persistent Vector MCP Memory Mesh",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "target_dir": { "type": "string", "description": "Optional target directory or crate to index (defaults to workspace root)" },
                            "file_extensions": { "type": "array", "items": { "type": "string" }, "description": "File extensions to index (defaults to ['rs', 'py', 'toml', 'json', 'md', 'c', 'cpp', 'zig', 'ts', 'js'])" },
                            "max_files": { "type": "integer", "description": "Maximum number of files to process", "default": 500 },
                            "force_reindex": { "type": "boolean", "description": "Force reindexing of all matching files", "default": false },
                            "namespace": { "type": "string", "description": "Memory namespace to isolate indexed codebase", "default": "codebase" }
                        }
                    }
                }));
                arr.push(serde_json::json!({
                    "name": "semantic_memory_search",
                    "description": "Queries the persistent Vector MCP Memory Mesh using hybrid semantic vector similarity (HNSW / SIMD cosine) and lexical SQLite FTS5 matching",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Natural language or technical query to match semantically against memory mesh" },
                            "top_k": { "type": "integer", "description": "Maximum number of nearest neighbor results to return", "default": 10 },
                            "category": { "type": "string", "description": "Optional memory category filter: interaction, command_outcome, user_preference, code_context, past_fix_pattern, code_chunk" },
                            "namespace": { "type": "string", "description": "Optional namespace filter (e.g. codebase, developer_habits, default)" },
                            "min_score": { "type": "number", "description": "Minimum similarity score threshold (0.0 to 1.0)", "default": 0.0 },
                            "query_vector": { "type": "array", "items": { "type": "number" }, "description": "Optional dense f32 embedding vector for direct vector query" }
                        },
                        "required": ["query"]
                    }
                }));
                arr.push(serde_json::json!({
                    "name": "recall_developer_context",
                    "description": "Retrieves long-term developer context, learned habits, architectural rules, past fix patterns, and debugging sessions from persistent memory",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "topic": { "type": "string", "description": "Optional topic or concept to recall context for" },
                            "category": { "type": "string", "description": "Optional category filter: user_preference, past_fix_pattern, code_context, interaction, command_outcome" },
                            "limit": { "type": "integer", "description": "Maximum number of context memories to recall", "default": 5 }
                        }
                    }
                }));
                arr.push(serde_json::json!({
                    "name": "update_long_term_memory",
                    "description": "Records a new persistent long-term memory insight, developer habit, command outcome, or fix pattern into the Vector MCP Memory Mesh",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Category: user_preference, past_fix_pattern, code_context, interaction, command_outcome" },
                            "title": { "type": "string", "description": "Descriptive title or headline for the memory insight" },
                            "content": { "type": "string", "description": "Detailed body content, rule, error trace, or fix diff" },
                            "metadata": { "type": "object", "description": "Optional arbitrary JSON metadata key-value mapping" },
                            "symbols": { "type": "array", "items": { "type": "string" }, "description": "Associated code symbols (functions, structs, traits, classes)" },
                            "namespace": { "type": "string", "description": "Target memory namespace", "default": "default" }
                        },
                        "required": ["category", "title", "content"]
                    }
                }));
                arr.push(serde_json::json!({
                    "name": "hgb_mcp_request_tool",
                    "description": "On-demand prompt-driven WASM tool generation and dynamic hot-swapping into MCP registry",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "tool_name": { "type": "string", "description": "Unique identifier for the dynamic tool" },
                            "intent": { "type": "string", "description": "Natural language prompt or intent specifying tool behavior" },
                            "schema": { "type": "object", "description": "Optional JSON schema describing expected input arguments" }
                        },
                        "required": ["tool_name", "intent"]
                    }
                }));

                let reg_opt = state.dynamic_registry.read().unwrap().clone();
                if let Some(reg) = reg_opt {
                    let dynamic_tools = reg.list_tools_mcp_format().await;
                    for dt in dynamic_tools {
                        arr.push(dt);
                    }
                }
            }

            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": tools_val
                }
            });
            (StatusCode::OK, Json(resp))
        }

        "tools/call" => {
            let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
            let args_str = arguments.to_string();

            // Intercept multi-tenant WASM sandbox tool execution
            if tool_name == crate::multitenant_mcp_tool::MULTITENANT_WASM_TOOL_NAME {
                let (res_val, _is_error) = crate::multitenant_mcp_tool::MultitenantMcpTool::handle_tool_call(
                    &state,
                    &arguments,
                    &session_id,
                ).await;
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": res_val
                });
                return (StatusCode::OK, Json(resp));
            }

            // Intercept Distributed Swarm MicroVM tool calls
            if tool_name == crate::distributed_swarm_orchestrator::SWARM_DISPATCH_TOOL_NAME {
                let orch_opt = state.swarm_orchestrator.read().unwrap().clone();
                let orch = match orch_opt {
                    Some(o) => o,
                    None => Arc::new(crate::distributed_swarm_orchestrator::DistributedSwarmOrchestrator::new(&state.workspace_root).unwrap()),
                };
                let (res_val, _is_err) = crate::distributed_swarm_orchestrator::SwarmMcpTools::handle_dispatch_tool_call(&orch, &arguments).await;
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": res_val
                });
                return (StatusCode::OK, Json(resp));
            } else if tool_name == crate::distributed_swarm_orchestrator::SWARM_TELEMETRY_TOOL_NAME {
                let orch_opt = state.swarm_orchestrator.read().unwrap().clone();
                let orch = match orch_opt {
                    Some(o) => o,
                    None => Arc::new(crate::distributed_swarm_orchestrator::DistributedSwarmOrchestrator::new(&state.workspace_root).unwrap()),
                };
                let (res_val, _is_err) = crate::distributed_swarm_orchestrator::SwarmMcpTools::handle_telemetry_tool_call(&orch, &arguments).await;
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": res_val
                });
                return (StatusCode::OK, Json(resp));
            }

            // Intercept Autonomous Self-Healing & Profiling Loop tools
            if tool_name == crate::dynamic_mcp_registry::autonomous_mcp_self_healing_loop::MCP_TOOL_SELF_HEALING_DIAGNOSE
                || tool_name == crate::dynamic_mcp_registry::autonomous_mcp_self_healing_loop::MCP_TOOL_PGO_TUNE_PASS
                || tool_name == crate::dynamic_mcp_registry::autonomous_mcp_self_healing_loop::MCP_TOOL_AUTO_PATCH_BOTTLENECK
            {
                let auto_loop = match crate::dynamic_mcp_registry::autonomous_mcp_self_healing_loop::AutonomousMcpSelfHealingLoop::new(&state.workspace_root) {
                    Ok(l) => l,
                    Err(e) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("Failed to initialize AutonomousMcpSelfHealingLoop: {}", e) }],
                                "isError": true
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                };

                match auto_loop.dispatch_tool(tool_name, &arguments).await {
                    Ok(res_val) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&res_val).unwrap_or_else(|_| res_val.to_string()) }],
                                "isError": false
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                    Err(e) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("Error executing {}: {}", tool_name, e) }],
                                "isError": true
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                }
            }

            // Intercept Vector MCP Memory Mesh tools
            if tool_name == "index_codebase_memory" || tool_name == "hgb_index_codebase_memory" {
                let target_dir_str = arguments.get("target_dir").and_then(|v| v.as_str()).unwrap_or(".");
                let target_path = if Path::new(target_dir_str).is_absolute() {
                    PathBuf::from(target_dir_str)
                } else {
                    state.workspace_root.join(target_dir_str)
                };

                let extensions = arguments.get("file_extensions")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_else(|| hgb_storage::vector_mcp_memory_mesh::IndexOptions::default().extensions);

                let max_files = arguments.get("max_files").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
                let force_reindex = arguments.get("force_reindex").and_then(|v| v.as_bool()).unwrap_or(false);
                let namespace = arguments.get("namespace").and_then(|v| v.as_str()).unwrap_or("codebase").to_string();

                let opts = hgb_storage::vector_mcp_memory_mesh::IndexOptions {
                    extensions,
                    max_files,
                    chunk_size_lines: 40,
                    overlap_lines: 8,
                    namespace,
                    force_reindex,
                };

                match state.vector_memory_mesh.index_codebase(&target_path, &opts) {
                    Ok(report) => {
                        let resp_val = serde_json::json!({
                            "status": "success",
                            "indexed_files": report.indexed_files,
                            "total_chunks": report.total_chunks,
                            "extracted_symbols": report.extracted_symbols,
                            "duration_ms": report.duration_ms,
                            "errors": report.errors
                        });
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp_val).unwrap_or_default() }],
                                "isError": false
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                    Err(e) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("Error indexing codebase: {}", e) }],
                                "isError": true
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                }
            } else if tool_name == "semantic_memory_search" || tool_name == "hgb_semantic_memory_search" {
                let query_str = arguments.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let top_k = arguments.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let category = arguments.get("category").and_then(|v| v.as_str());
                let namespace = arguments.get("namespace").and_then(|v| v.as_str());
                let min_score = arguments.get("min_score").and_then(|v| v.as_f64()).map(|s| s as f32);

                let filter = hgb_storage::vector_mcp_memory_mesh::MemoryFilter {
                    namespace: namespace.map(|n| n.to_string()),
                    category: category.map(|c| c.to_string()),
                    min_score,
                    ..Default::default()
                };

                let search_res = if let Some(q_vec) = arguments.get("query_vector").and_then(|v| v.as_array()) {
                    let f32_vec: Vec<f32> = q_vec.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
                    if !f32_vec.is_empty() {
                        state.vector_memory_mesh.search_semantic_memory(&f32_vec, top_k, Some(&filter))
                    } else {
                        state.vector_memory_mesh.search_by_text(query_str, top_k, Some(&filter))
                    }
                } else {
                    state.vector_memory_mesh.search_by_text(query_str, top_k, Some(&filter))
                };

                match search_res {
                    Ok(results) => {
                        let formatted_hits: Vec<serde_json::Value> = results.into_iter().map(|hit| {
                            serde_json::json!({
                                "id": hit.item.id,
                                "title": hit.item.title,
                                "category": hit.item.category,
                                "namespace": hit.item.namespace,
                                "content": hit.item.content,
                                "symbols": hit.item.symbols,
                                "similarity_score": hit.similarity_score,
                                "combined_score": hit.combined_score,
                                "lexical_score": hit.lexical_score,
                                "snippet": hit.snippet,
                                "metadata": hit.item.metadata,
                                "access_count": hit.item.access_count
                            })
                        }).collect();

                        let resp_val = serde_json::json!({
                            "query": query_str,
                            "count": formatted_hits.len(),
                            "results": formatted_hits
                        });
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp_val).unwrap_or_default() }],
                                "isError": false
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                    Err(e) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("Semantic memory search error: {}", e) }],
                                "isError": true
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                }
            } else if tool_name == "recall_developer_context" || tool_name == "hgb_recall_developer_context" {
                let topic = arguments.get("topic").and_then(|v| v.as_str());
                let category = arguments.get("category").and_then(|v| v.as_str());
                let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

                match state.vector_memory_mesh.recall_developer_context(topic, category, limit) {
                    Ok(items) => {
                        let formatted: Vec<serde_json::Value> = items.into_iter().map(|item| {
                            serde_json::json!({
                                "id": item.id,
                                "title": item.title,
                                "category": item.category,
                                "namespace": item.namespace,
                                "content": item.content,
                                "symbols": item.symbols,
                                "metadata": item.metadata,
                                "access_count": item.access_count,
                                "updated_at": item.updated_at
                            })
                        }).collect();

                        let resp_val = serde_json::json!({
                            "topic": topic,
                            "category": category,
                            "recalled_items": formatted
                        });
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp_val).unwrap_or_default() }],
                                "isError": false
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                    Err(e) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("Recall developer context error: {}", e) }],
                                "isError": true
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                }
            } else if tool_name == "update_long_term_memory" || tool_name == "hgb_update_long_term_memory" {
                let category = arguments.get("category").and_then(|v| v.as_str()).unwrap_or("general");
                let title = arguments.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled Insight");
                let content = arguments.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let metadata = arguments.get("metadata").cloned().unwrap_or(serde_json::json!({}));
                let symbols: Vec<String> = arguments.get("symbols")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let namespace = arguments.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");

                let vector = hgb_core::deterministic_semantic_embedding(content, hgb_storage::vector_mcp_memory_mesh::DEFAULT_VECTOR_DIM);
                let mut item = hgb_storage::vector_mcp_memory_mesh::MemoryItem::new(category, title, content, vector)
                    .with_namespace(namespace)
                    .with_metadata(metadata)
                    .with_symbols(symbols);

                match state.vector_memory_mesh.insert_item(item.clone()) {
                    Ok(()) => {
                        let resp_val = serde_json::json!({
                            "status": "stored",
                            "id": item.id,
                            "category": item.category,
                            "namespace": item.namespace,
                            "title": item.title,
                            "updated_at": item.updated_at
                        });
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&resp_val).unwrap_or_default() }],
                                "isError": false
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                    Err(e) => {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("Error storing long-term memory: {}", e) }],
                                "isError": true
                            }
                        });
                        return (StatusCode::OK, Json(resp));
                    }
                }
            }

            // Intercept semantic cache inspection and purge tools directly
            if tool_name == "hgb_semantic_cache_stats" {
                let stats = state.semantic_cache.stats();
                let resp_str = serde_json::to_string_pretty(&stats).unwrap_or_default();
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": resp_str }],
                        "isError": false
                    }
                });
                return (StatusCode::OK, Json(resp));
            } else if tool_name == "hgb_semantic_cache_purge" {
                let purge_all = arguments.get("purge_all").and_then(|v| v.as_bool()).unwrap_or(false);
                let target_tool = arguments.get("tool_name").and_then(|v| v.as_str());

                let purged = if purge_all {
                    state.semantic_cache.purge_all()
                } else if let Some(t) = target_tool {
                    state.semantic_cache.purge_tool(t)
                } else {
                    state.semantic_cache.prune_expired()
                };

                let report = serde_json::json!({
                    "status": "purged",
                    "purged_entries": purged,
                    "purge_all": purge_all,
                    "target_tool": target_tool
                });
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&report).unwrap_or_default() }],
                        "isError": false
                    }
                });
                return (StatusCode::OK, Json(resp));
            }

            // 1. Semantic cache lookup before dispatching
            let query_emb = hgb_core::deterministic_semantic_embedding(&args_str, 768);
            if let Some(cached) = state.semantic_cache.lookup_semantic(tool_name, &args_str, &query_emb, 0.92) {
                let sim = cached.similarity_score.unwrap_or(1.0);
                let saved_ms = cached.execution_duration_ms;

                // Emit SSE chunk with metadata tag
                if !session_id.is_empty() {
                    let meta_evt = Event::default().event("message").data(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/message",
                            "params": {
                                "level": "info",
                                "logger": "semantic_cache",
                                "data": {
                                    "cached": true,
                                    "semantic_similarity": sim,
                                    "saved_ms": saved_ms
                                }
                            }
                        })
                        .to_string(),
                    );
                    state.emit_to_session(&session_id, meta_evt).await;
                }

                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": cached.response_text
                            }
                        ],
                        "_meta": {
                            "cached": true,
                            "semantic_similarity": sim,
                            "saved_ms": saved_ms
                        },
                        "isError": false
                    }
                });
                return (StatusCode::OK, Json(resp));
            }

            let _ipc_lease = state.ipc_governor.acquire_lease(tool_name);
            let _governor_guard = hgb_core::dynatune_subsystem_fuse::DynatuneWorkloadGuard::acquire_for_workspace(
                &state.workspace_root,
                tool_name,
            );

            let exec_start = std::time::Instant::now();

            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<(u32, String)>();
            let state_clone = Arc::clone(&state);
            let s_id = session_id.clone();

            // Background task to pump granular progress notifications over SSE stream
            let progress_pump = tokio::spawn(async move {
                while let Some((pct, msg)) = progress_rx.recv().await {
                    if !s_id.is_empty() {
                        let evt = Event::default().event("message").data(
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "notifications/progress",
                                "params": {
                                    "progressToken": 1,
                                    "progress": pct,
                                    "total": 100,
                                    "message": msg
                                }
                            })
                            .to_string(),
                        );
                        state_clone.emit_to_session(&s_id, evt).await;
                    }
                }
            });

            let tx_clone = progress_tx.clone();
            let progress_cb = move |pct: u32, msg: &str| {
                let _ = tx_clone.send((pct, msg.to_string()));
            };

            let dyn_reg_opt = state.dynamic_registry.read().unwrap().clone();
            let is_dynamic = tool_name == "hgb_mcp_request_tool"
                || match dyn_reg_opt.as_ref() {
                    Some(reg) => reg.has_tool(tool_name).await,
                    None => false,
                };

            let (result_str, is_err) = if is_dynamic {
                if let Some(ref reg) = dyn_reg_opt {
                    progress_cb(50, &format!("Dispatching dynamic tool '{}'...", tool_name));
                    match reg.intercept_or_dispatch(tool_name, &arguments).await {
                        Ok(val) => (serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string()), false),
                        Err(e) => (format!("Error executing dynamic tool {}: {}", tool_name, e), true),
                    }
                } else {
                    ("DynamicMcpRegistry not available".to_string(), true)
                }
            } else {
                // Execute standard tool with fallback to prompt-driven synthesis
                match state.mcp_server.execute_tool_with_progress(tool_name, &arguments, progress_cb).await {
                    Ok(res) => {
                        if res.contains("recognized by Hagibis MCP dispatcher") && dyn_reg_opt.is_some() {
                            let reg = dyn_reg_opt.as_ref().unwrap();
                            match reg.intercept_or_dispatch(tool_name, &arguments).await {
                                Ok(val) => (serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string()), false),
                                Err(_) => (res, false),
                            }
                        } else {
                            (res, false)
                        }
                    }
                    Err(e) => {
                        if let Some(ref reg) = dyn_reg_opt {
                            match reg.intercept_or_dispatch(tool_name, &arguments).await {
                                Ok(val) => (serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string()), false),
                                Err(_) => (format!("Error executing {}: {}", tool_name, e), true),
                            }
                        } else {
                            (format!("Error executing {}: {}", tool_name, e), true)
                        }
                    }
                }
            };

            // Close progress channel and wait for pump to flush
            drop(progress_tx);
            let _ = progress_pump.await;

            let execution_duration_ms = exec_start.elapsed().as_secs_f64() * 1000.0;
            if !is_err && tool_name != "hgb_semantic_cache_stats" && tool_name != "hgb_semantic_cache_purge" {
                state.semantic_cache.insert(
                    tool_name,
                    &args_str,
                    query_emb,
                    result_str.clone(),
                    execution_duration_ms,
                    Some(3600),
                );
            }

            // Return standardized MCP tool call content result
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": result_str
                        }
                    ],
                    "isError": is_err
                }
            });

            (StatusCode::OK, Json(resp))
        }

        _ => {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {}", method)
                }
            });
            (StatusCode::OK, Json(resp))
        }
    }
}

/// GET /status - Read server telemetry
async fn handle_status(State(state): State<Arc<McpSseState>>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let tools_count = HagibisMcpServer::list_tools().as_array().map(|a| a.len()).unwrap_or(0);
    let status = McpSseServerStatus {
        bind_addr: format!("http://127.0.0.1:{}", state.port),
        active_sse_sessions: sessions.len(),
        total_connections: state.total_connections.load(Ordering::Relaxed),
        total_messages_received: state.total_messages.load(Ordering::Relaxed),
        total_events_streamed: state.total_streamed_events.load(Ordering::Relaxed),
        tools_count,
    };
    Json(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_sse_server_routes() {
        let state = Arc::new(McpSseState::new(".", 8083));
        let _router = McpSseServer::create_router(state.clone());
        assert_eq!(state.port, 8083);
        assert_eq!(state.total_connections.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_mcp_sse_tools_list_discovery() {
        let state = Arc::new(McpSseState::new(".", 8083));
        let query = MessageQuery { session_id: None, profile: Some("full".to_string()) };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        });

        let resp = handle_client_message(State(state), Query(query), Json(req)).await;
        let body = serde_json::to_string(&resp.1 .0).expect("Serialize");
        assert!(body.contains("hgb_mcp_run_tests"));
        assert!(body.contains("hgb_mcp_read_patch"));
        assert!(body.contains("hgb_mcp_apply_patch"));
        assert!(body.contains("hgb_mcp_sandbox_exec"));
        assert!(body.contains("hgb_daemon_telemetry"));
        assert!(body.contains("hgb_filesystem_tree"));
    }

    #[tokio::test]
    async fn test_mcp_sse_tool_call_with_progress_streaming() {
        let state = Arc::new(McpSseState::new(".", 8083));
        let session_id = "test-session-uuid".to_string();

        // Register session channel
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        {
            let mut sess = state.sessions.write().await;
            sess.insert(session_id.clone(), tx);
        }

        let query = MessageQuery { session_id: Some(session_id), profile: None };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "hgb_daemon_telemetry",
                "arguments": {}
            }
        });

        let resp = handle_client_message(State(state.clone()), Query(query), Json(req)).await;
        assert_eq!(resp.0, StatusCode::OK);

        // Verify that intermediate progress events were streamed into session
        let mut progress_events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            progress_events.push(format!("{:?}", evt));
        }
        assert!(!progress_events.is_empty(), "Must stream granular SSE progress events");

        // Verify response content
        let body = serde_json::to_string(&resp.1 .0).expect("Serialize");
        assert!(body.contains("healthy"));
        assert!(body.contains("daemon_health"));
    }

    #[test]
    fn test_mcp_auth_token_generation_and_permissions() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_auth_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let token_file = temp_dir.join("mcp_auth_token");

        let token = get_or_create_auth_token_at(&token_file).expect("get or create token");
        assert_eq!(token.len(), 64, "256-bit hex token must be 64 hex chars");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

        // File must exist
        assert!(token_file.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&token_file).expect("metadata");
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "Token file mode must be strictly 0600");
        }

        // Second call should return the exact same token
        let token2 = get_or_create_auth_token_at(&token_file).expect("load existing token");
        assert_eq!(token, token2);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_mcp_bearer_auth_enforcement_on_routes() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let token = "test_secret_token_1234567890abcdef";
        let state = Arc::new(McpSseState::with_auth_token(&temp_dir, 8083, token));
        let app = McpSseServer::create_router(state.clone());

        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        // 1. Request to /status without auth (public health/telemetry endpoint) -> OK
        let status_req = Request::builder()
            .uri("/status")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(status_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 2. Request to /message without auth -> 401 UNAUTHORIZED
        let unauth_req = Request::builder()
            .uri("/message")
            .method("POST")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"ping","id":1}"#))
            .unwrap();
        let resp = app.clone().oneshot(unauth_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 3. Request to /message with invalid auth -> 401 UNAUTHORIZED
        let bad_auth_req = Request::builder()
            .uri("/message")
            .method("POST")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer wrong_token")
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"ping","id":1}"#))
            .unwrap();
        let resp = app.clone().oneshot(bad_auth_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 4. Request to /message with valid Bearer token -> 200 OK
        let valid_req = Request::builder()
            .uri("/message")
            .method("POST")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::from(r#"{"jsonrpc":"2.0","method":"ping","id":1}"#))
            .unwrap();
        let resp = app.clone().oneshot(valid_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 5. Request to /sse without auth -> 401 UNAUTHORIZED
        let unauth_sse = Request::builder()
            .uri("/sse")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(unauth_sse).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_mcp_sse_semantic_cache_hit_and_telemetry() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_cache_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let token = "test_cache_token_999";
        let state = Arc::new(McpSseState::with_auth_token(&temp_dir, 8083, token));

        // Pre-populate semantic cache with a response
        let tool = "hgb_system_inspect";
        let args_str = r#"{"component":"kernel","mode":"fast"}"#;
        let response_text = r#"{"kernel":"Linux","status":"optimal"}"#;
        let query_emb = hgb_core::deterministic_semantic_embedding(args_str, 768);

        state.semantic_cache.insert(
            tool,
            args_str,
            query_emb,
            response_text.to_string(),
            35.5,
            Some(3600),
        );

        // Register session channel
        let session_id = "test-session-cache-uuid".to_string();
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        {
            let mut sess = state.sessions.write().await;
            sess.insert(session_id.clone(), tx);
        }

        // Call tools/call with matching arguments
        let query = MessageQuery { session_id: Some(session_id), profile: None };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 101,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": {
                    "mode": "fast",
                    "component": "kernel"
                }
            }
        });

        let resp = handle_client_message(State(state.clone()), Query(query), Json(req)).await;
        assert_eq!(resp.0, StatusCode::OK);

        let body_val = &resp.1 .0;
        let result = body_val.get("result").expect("result field");
        assert_eq!(result["_meta"]["cached"], true);
        assert_eq!(result["_meta"]["saved_ms"], 35.5);
        assert_eq!(result["_meta"]["semantic_similarity"], 1.0);
        assert_eq!(result["content"][0]["text"], response_text);

        // Verify that SSE session received cache telemetry metadata notification
        let mut cache_event_received = false;
        while let Ok(evt) = rx.try_recv() {
            let s = format!("{:?}", evt);
            if s.contains("semantic_cache") && s.contains("cached") {
                cache_event_received = true;
            }
        }
        assert!(cache_event_received, "Expected SSE notification with cache hit metadata");

        // Verify stats tool inspection
        let stats_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 102,
            "method": "tools/call",
            "params": {
                "name": "hgb_semantic_cache_stats",
                "arguments": {}
            }
        });
        let stats_resp = handle_client_message(State(state.clone()), Query(MessageQuery { session_id: None, profile: None }), Json(stats_req)).await;
        assert_eq!(stats_resp.0, StatusCode::OK);
        let stats_text = stats_resp.1 .0["result"]["content"][0]["text"].as_str().unwrap();
        assert!(stats_text.contains("\"hit_count\": 1"));

        // Verify purge tool
        let purge_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 103,
            "method": "tools/call",
            "params": {
                "name": "hgb_semantic_cache_purge",
                "arguments": { "purge_all": true }
            }
        });
        let purge_resp = handle_client_message(State(state.clone()), Query(MessageQuery { session_id: None, profile: None }), Json(purge_req)).await;
        assert_eq!(purge_resp.0, StatusCode::OK);
        let purge_text = purge_resp.1 .0["result"]["content"][0]["text"].as_str().unwrap();
        assert!(purge_text.contains("\"status\": \"purged\""));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
