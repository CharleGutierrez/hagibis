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
}

impl McpSseState {
    pub fn new<P: AsRef<Path>>(workspace_root: P, port: u16) -> Self {
        let auth_token = get_or_create_auth_token().unwrap_or_else(|_| generate_256bit_hex_token());
        Self::with_auth_token(workspace_root, port, auth_token)
    }

    pub fn with_auth_token<P: AsRef<Path>>(workspace_root: P, port: u16, auth_token: impl Into<String>) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        Self {
            workspace_root: ws.clone(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            total_connections: AtomicU64::new(0),
            total_messages: AtomicU64::new(0),
            total_streamed_events: AtomicU64::new(0),
            mcp_server: Arc::new(HagibisMcpServer::new(ws)),
            port,
            auth_token: auth_token.into(),
        }
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
            let tools = HagibisMcpServer::list_tools_for_profile(profile);
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": tools
                }
            });
            (StatusCode::OK, Json(resp))
        }

        "tools/call" => {
            let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));

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

            // Execute the tool with streaming progress feedback
            let result_str = match state.mcp_server.execute_tool_with_progress(tool_name, &arguments, progress_cb).await {
                Ok(res) => res,
                Err(e) => format!("Error executing {}: {}", tool_name, e),
            };

            // Close progress channel and wait for pump to flush
            drop(progress_tx);
            let _ = progress_pump.await;

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
                    "isError": false
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
}
