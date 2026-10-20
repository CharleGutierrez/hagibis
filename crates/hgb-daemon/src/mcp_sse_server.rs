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

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};

use hgb_core::HagibisMcpServer;

/// Default port for the Hagibis MCP SSE server
pub const DEFAULT_MCP_SSE_PORT: u16 = 8083;

/// Query parameters for POST /message
#[derive(Debug, Deserialize)]
pub struct MessageQuery {
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
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
}

impl McpSseState {
    pub fn new<P: AsRef<Path>>(workspace_root: P, port: u16) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        Self {
            workspace_root: ws.clone(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            total_connections: AtomicU64::new(0),
            total_messages: AtomicU64::new(0),
            total_streamed_events: AtomicU64::new(0),
            mcp_server: Arc::new(HagibisMcpServer::new(ws)),
            port,
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

    pub fn state(&self) -> Arc<McpSseState> {
        Arc::clone(&self.state)
    }

    /// Construct Axum router for MCP SSE
    pub fn create_router(state: Arc<McpSseState>) -> Router {
        Router::new()
            .route("/sse", get(handle_sse_connect))
            .route("/message", post(handle_client_message))
            .route("/status", get(handle_status))
            .with_state(state)
    }

    /// Launch the background MCP SSE HTTP server
    pub async fn spawn_background(workspace_root: PathBuf, port: u16) -> tokio::task::JoinHandle<()> {
        let server = Self::new(workspace_root, port);
        let router = Self::create_router(server.state());
        let addr = SocketAddr::from(([0, 0, 0, 0], port));

        tokio::spawn(async move {
            if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                let _ = axum::serve(listener, router).await;
            }
        })
    }
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
async fn handle_client_message(
    State(state): State<Arc<McpSseState>>,
    Query(query): Query<MessageQuery>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
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
            let tools = HagibisMcpServer::list_tools();
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

            // Stream progress notification over active SSE connection
            if !session_id.is_empty() {
                let progress_evt = Event::default().event("message").data(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": 1,
                            "progress": 50,
                            "total": 100,
                            "message": format!("Executing sovereign Hagibis tool: {}", tool_name)
                        }
                    })
                    .to_string(),
                );
                state.emit_to_session(&session_id, progress_evt).await;
            }

            // Execute the tool
            let result_str = match state.mcp_server.execute_tool(tool_name, &arguments).await {
                Ok(res) => res,
                Err(e) => format!("Error executing {}: {}", tool_name, e),
            };

            // Stream completion notice over SSE
            if !session_id.is_empty() {
                let complete_evt = Event::default().event("message").data(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": 1,
                            "progress": 100,
                            "total": 100,
                            "message": format!("Tool {} execution completed", tool_name)
                        }
                    })
                    .to_string(),
                );
                state.emit_to_session(&session_id, complete_evt).await;
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
}
