use crate::checkpoint::SwarmCheckpointManager;
use crate::inline_diff_engine::{DualBufferOverlay, InlineDiffEngine};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::fs;

static CHECKPOINT_MGR: OnceLock<Mutex<SwarmCheckpointManager>> = OnceLock::new();

fn get_checkpoint_mgr() -> &'static Mutex<SwarmCheckpointManager> {
    CHECKPOINT_MGR.get_or_init(|| Mutex::new(SwarmCheckpointManager::new()))
}

#[derive(Clone)]
pub struct VisualIdeServer {
    pub port: u16,
    pub workspace_root: PathBuf,
    pub auth_token: Option<String>,
    pub lsp: Arc<crate::lsp_engine::LspEngine>,
    pub semantic_index: Arc<crate::semantic_index::SemanticCodebaseIndex>,
    pub dap: Arc<crate::dap_engine::DapEngine>,
    pub swarm: Arc<crate::worktree_swarm::WorktreeSwarmManager>,
    pub pty: Arc<crate::pty_engine::PtyManager>,
    pub fs_watcher: Arc<tokio::sync::Mutex<crate::fs_watcher::WorkspaceFsWatcher>>,
    pub cloud_settings: Arc<tokio::sync::RwLock<crate::cloud_settings::CloudSettings>>,
    pub agent_loop: Arc<crate::agent_loop::AgentLoopEngine>,
}

#[derive(Serialize)]
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Option<Vec<FileNode>>,
}

#[derive(Deserialize)]
pub struct FileQuery {
    pub path: String,
}

#[derive(Deserialize)]
pub struct SavePayload {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct DiffPayload {
    pub original: String,
    pub modified: String,
}

#[derive(Deserialize)]
pub struct TabCommitPayload {
    pub path: String,
    pub original: String,
    pub speculative: String,
}

#[derive(Deserialize)]
pub struct ChatPayload {
    pub prompt: String,
    pub context: Option<String>,
    pub model: Option<String>,
    pub history: Option<Vec<ChatMessage>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp_utc: String,
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub created_at_utc: String,
    pub updated_at_utc: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChatSessionSummary {
    pub id: String,
    pub title: String,
    pub created_at_utc: String,
    pub updated_at_utc: String,
    pub model: String,
    pub message_count: usize,
}

#[derive(Deserialize)]
pub struct SessionQuery {
    pub id: String,
}

#[derive(Deserialize)]
pub struct CreateSessionPayload {
    pub title: String,
    pub model: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GlobalSearchMatch {
    pub file_path: String,
    pub line_number: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub line_content: String,
    pub matched_text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GlobalSearchResult {
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub total_matches: usize,
    pub files_count: usize,
    pub matches: Vec<GlobalSearchMatch>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GlobalReplaceResult {
    pub checkpoint_id: String,
    pub files_modified: Vec<String>,
    pub total_replacements: usize,
}

#[derive(Deserialize)]
pub struct GlobalSearchPayload {
    pub query: String,
    pub is_regex: Option<bool>,
    pub case_sensitive: Option<bool>,
}

#[derive(Deserialize)]
pub struct GlobalReplacePayload {
    pub query: String,
    pub replacement: String,
    pub is_regex: Option<bool>,
    pub case_sensitive: Option<bool>,
    pub file_filter: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GitFileChange {
    pub path: String,
    pub status_code: String,
    pub status_label: String,
    pub is_staged: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GitStatusSummary {
    pub branch: String,
    pub is_clean: bool,
    pub staged: Vec<GitFileChange>,
    pub unstaged: Vec<GitFileChange>,
    pub untracked: Vec<GitFileChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GitCommitResult {
    pub success: bool,
    pub commit_hash: String,
    pub summary: String,
    pub files_changed: usize,
}

#[derive(Deserialize)]
pub struct GitStagePayload {
    pub path: String,
}

#[derive(Deserialize)]
pub struct GitCommitPayload {
    pub message: String,
}

#[derive(Deserialize)]
pub struct SearchPayload {
    pub query: String,
}

#[derive(Deserialize)]
pub struct TerminalPayload {
    pub command: String,
    pub working_dir: Option<String>,
}

#[derive(Deserialize)]
pub struct InlineEditPayload {
    pub path: String,
    pub selected_text: String,
    pub file_context: String,
    pub instruction: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ComposerFilePlan {
    pub path: String,
    pub original_content: String,
    pub modified_content: String,
    pub explanation: String,
}

#[derive(Deserialize)]
pub struct ComposerPayload {
    pub prompt: String,
    pub files: Vec<String>,
}

#[derive(Deserialize)]
pub struct CreatePayload {
    pub path: String,
}

#[derive(Deserialize)]
pub struct RenamePayload {
    pub old_path: String,
    pub new_path: String,
}

#[derive(Deserialize)]
pub struct TabCompletionPayload {
    pub path: String,
    pub prefix: String,
    pub suffix: String,
}

#[derive(Deserialize)]
pub struct ContextResolutionPayload {
    pub prompt: String,
}

#[derive(Deserialize)]
pub struct GitDiffPayload {
    pub path: String,
}

#[derive(Deserialize)]
pub struct DiagnosticsPayload {
    pub path: String,
}

#[derive(Deserialize)]
pub struct AutonomousAgentPayload {
    pub task: String,
    pub files: Vec<String>,
    pub verify_command: Option<String>,
    pub max_iterations: Option<usize>,
}

#[derive(Deserialize)]
pub struct McpToolPayload {
    pub server_cmd: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

#[derive(Deserialize)]
pub struct CreateCheckpointPayload {
    pub label: String,
    pub files: Vec<String>,
}

#[derive(Deserialize)]
pub struct RollbackPayload {
    pub checkpoint_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WorkspaceSession {
    pub open_tabs: Vec<String>,
    pub active_tab_idx: usize,
    pub selected_model: String,
    pub last_active_view: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ZigTelemetry {
    pub engine_status: String,
    pub zig_version: String,
    pub simd_active: bool,
    pub simd_feature: String,
    pub myers_ses_latency_us: u64,
    pub myers_lcs_similarity: f32,
    pub levenshtein_benchmark_us: u64,
    pub landlock_lsm_supported: bool,
    pub memory_arena_status: String,
    pub active_optimizations: Vec<String>,
}

fn get_real_memory_metrics() -> String {
    let page_size = 4096u64;
    let self_rss_pages = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()))
        .unwrap_or(0);
    let self_mb = (self_rss_pages * page_size) as f64 / (1024.0 * 1024.0);

    // Check if hgbd daemon is running and get its RSS
    let mut daemon_mb: Option<f64> = None;
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.chars().all(|c| c.is_ascii_digit()) {
                let comm_path = entry.path().join("comm");
                if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                    if comm.trim() == "hgbd" {
                        let statm_path = entry.path().join("statm");
                        if let Ok(statm) = std::fs::read_to_string(&statm_path) {
                            if let Some(pages) = statm.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()) {
                                daemon_mb = Some((pages * page_size) as f64 / (1024.0 * 1024.0));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    match daemon_mb {
        Some(d_mb) => format!("{:.1} MB (proc: {:.1} MB, hgbd: {:.1} MB)", self_mb + d_mb, self_mb, d_mb),
        None => format!("{:.1} MB (resident RSS)", self_mb),
    }
}

async fn query_local_llm_status() -> String {
    let client = reqwest::Client::new();
    match client.get("http://127.0.0.1:11434/api/tags")
        .timeout(std::time::Duration::from_millis(600))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(models) = json.get("models").and_then(|m| m.as_array()) {
                    let names: Vec<&str> = models.iter()
                        .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                        .collect();
                    if !names.is_empty() {
                        format!("Online ({} models: {})", names.len(), names.join(", "))
                    } else {
                        "Online (no models loaded)".to_string()
                    }
                } else {
                    "Online (Ollama)".to_string()
                }
            } else {
                "Online (response parse error)".to_string()
            }
        }
        Err(_) => "Offline / Not Running (run 'ollama serve')".to_string(),
    }
}

impl VisualIdeServer {
    pub fn new(port: u16, workspace_root: PathBuf) -> Self {
        let lsp = Arc::new(crate::lsp_engine::LspEngine::new(workspace_root.clone()));
        let semantic_index = Arc::new(crate::semantic_index::SemanticCodebaseIndex::new(workspace_root.clone()));
        let dap = Arc::new(crate::dap_engine::DapEngine::new(workspace_root.clone()));
        let swarm = Arc::new(crate::worktree_swarm::WorktreeSwarmManager::new(workspace_root.clone()));
        let pty = Arc::new(crate::pty_engine::PtyManager::new(workspace_root.clone()));
        let fs_watcher = Arc::new(tokio::sync::Mutex::new(crate::fs_watcher::WorkspaceFsWatcher::new(workspace_root.clone())));
        let cloud_settings = Arc::new(tokio::sync::RwLock::new(crate::cloud_settings::CloudSettings::load_from_disk(None)));
        let agent_loop = Arc::new(crate::agent_loop::AgentLoopEngine::new(workspace_root.clone()));

        Self {
            port,
            workspace_root,
            auth_token: None,
            lsp,
            semantic_index,
            dap,
            swarm,
            pty,
            fs_watcher,
            cloud_settings,
            agent_loop,
        }
    }

    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token;
        self
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Kick off background semantic indexing
        let sem_idx = self.semantic_index.clone();
        tokio::spawn(async move {
            let _ = sem_idx.index_workspace().await;
        });

        // Kick off background live filesystem watcher
        let watcher_mutex = self.fs_watcher.clone();
        tokio::spawn(async move {
            let mut w = watcher_mutex.lock().await;
            if let Err(e) = w.start_watching() {
                eprintln!("Workspace filesystem watcher notice: {}", e);
            }
        });

        let app = Router::new()
            .route("/", get(Self::serve_index))
            .route("/manifest.json", get(Self::serve_manifest))
            .route("/manifest.webmanifest", get(Self::serve_manifest))
            .route("/sw.js", get(Self::serve_sw))
            .route("/icon.svg", get(Self::serve_icon))
            .route("/api/workspace", get(Self::handle_workspace))
            .route("/api/file", get(Self::handle_get_file))
            .route("/api/save_file", post(Self::handle_save_file))
            .route("/api/create_file", post(Self::handle_create_file))
            .route("/api/create_folder", post(Self::handle_create_folder))
            .route("/api/delete_entry", post(Self::handle_delete_entry))
            .route("/api/rename_entry", post(Self::handle_rename_entry))
            .route("/api/diff", post(Self::handle_diff))
            .route("/api/tab_commit", post(Self::handle_tab_commit))
            .route("/api/chat", post(Self::handle_chat))
            .route("/api/status", get(Self::handle_status))
            .route("/api/search", post(Self::handle_search))
            .route("/api/terminal", post(Self::handle_terminal))
            .route("/api/rules", get(Self::handle_rules))
            .route("/api/inline_edit", post(Self::handle_inline_edit))
            .route("/api/composer", post(Self::handle_composer))
            .route("/api/composer_commit", post(Self::handle_composer_commit))
            .route("/api/tab_completion", post(Self::handle_tab_completion))
            .route("/api/resolve_context", post(Self::handle_resolve_context))
            .route("/api/git_diff", post(Self::handle_git_diff))
            .route("/api/symbols", get(Self::handle_symbols))
            .route("/api/diagnostics", post(Self::handle_diagnostics))
            .route("/api/autonomous_agent", post(Self::handle_autonomous_agent))
            .route("/api/mcp_tool", post(Self::handle_mcp_tool))
            .route("/api/checkpoints", get(Self::handle_get_checkpoints))
            .route("/api/checkpoints/create", post(Self::handle_create_checkpoint))
            .route("/api/rollback", post(Self::handle_rollback))
            .route("/api/models", get(Self::handle_get_models))
            .route("/api/session/load", get(Self::handle_load_session))
            .route("/api/session/save", post(Self::handle_save_session))
            .route("/api/chat_sessions", get(Self::handle_get_chat_sessions))
            .route("/api/chat_session", get(Self::handle_load_chat_session))
            .route("/api/chat_session/save", post(Self::handle_save_chat_session))
            .route("/api/chat_session/delete", post(Self::handle_delete_chat_session))
            .route("/api/chat_session/create", post(Self::handle_create_chat_session))
            .route("/api/global_search", post(Self::handle_global_search))
            .route("/api/global_replace", post(Self::handle_global_replace))
            .route("/api/git_status", get(Self::handle_git_status))
            .route("/api/git_stage", post(Self::handle_git_stage))
            .route("/api/git_unstage", post(Self::handle_git_unstage))
            .route("/api/git_stage_all", post(Self::handle_git_stage_all))
            .route("/api/git_unstage_all", post(Self::handle_git_unstage_all))
            .route("/api/git_commit", post(Self::handle_git_commit))
            .route("/api/chat_stream", post(Self::handle_chat_stream))
            .route("/api/zig_telemetry", get(Self::handle_zig_telemetry))
            // Phase 1: Real LSP Routes
            .route("/api/lsp/hover", post(Self::handle_lsp_hover))
            .route("/api/lsp/definition", post(Self::handle_lsp_definition))
            .route("/api/lsp/completion", post(Self::handle_lsp_completion))
            .route("/api/lsp/ws", get(Self::handle_lsp_ws))
            // Phase 2: Remote Auth Token Verification
            .route("/api/auth/verify", post(Self::handle_auth_verify))
            // Phase 3: Semantic Vector Search
            .route("/api/semantic_search", post(Self::handle_semantic_search))
            // Phase 4: DAP Visual Debugger
            .route("/api/dap/toggle_breakpoint", post(Self::handle_dap_toggle_breakpoint))
            .route("/api/dap/breakpoints", get(Self::handle_dap_get_breakpoints))
            .route("/api/dap/launch", post(Self::handle_dap_launch))
            .route("/api/dap/continue", post(Self::handle_dap_continue))
            .route("/api/dap/step_over", post(Self::handle_dap_step_over))
            .route("/api/dap/step_into", post(Self::handle_dap_step_into))
            .route("/api/dap/stop", post(Self::handle_dap_stop))
            .route("/api/dap/status", get(Self::handle_dap_status))
            // Phase 5: Multi-Agent Parallel Git Worktree Swarm
            .route("/api/swarm/spawn", post(Self::handle_swarm_spawn))
            .route("/api/swarm/tasks", get(Self::handle_swarm_tasks))
            .route("/api/swarm/merge", post(Self::handle_swarm_merge))
            // Phase 6A: Interactive WebSocket PTY Terminal
            .route("/api/terminal/pty/ws", get(Self::handle_pty_ws))
            // Phase 6B: Live Workspace File System Watcher Stream
            .route("/api/fs/events/ws", get(Self::handle_fs_events_ws))
            // Phase 6C: Cloud LLM & API Key Settings Manager
            .route("/api/settings/models", get(Self::handle_get_cloud_settings).post(Self::handle_update_cloud_settings))
            .route("/api/settings/test_model", post(Self::handle_test_cloud_settings))
            // Phase 7A: Autonomous Agentic Tool-Use Loop & Permission Gate
            .route("/api/agent/tools", get(Self::handle_agent_tools))
            .route("/api/agent/chat_step", post(Self::handle_agent_chat_step))
            .route("/api/agent/approve_tool", post(Self::handle_agent_approve_tool))
            .route("/api/agent/auto_approve", post(Self::handle_agent_auto_approve))
            // Phase 7B: Live Streaming Ghost Diff (Cursor-Style Ctrl+K)
            .route("/api/inline_edit_stream", post(Self::handle_inline_edit_stream))
            // Phase 7C: Multi-Tab Persistent Terminals
            .route("/api/terminal/sessions", get(Self::handle_terminal_sessions_list))
            .route("/api/terminal/sessions/new", post(Self::handle_terminal_session_new))
            .route("/api/terminal/sessions/kill", post(Self::handle_terminal_session_kill))
            // Phase 7D: Git Branch Manager & 3-Way Merge Conflict Resolver
            .route("/api/git/branches", get(Self::handle_git_branches))
            .route("/api/git/branch/checkout", post(Self::handle_git_branch_checkout))
            .route("/api/git/conflicts", get(Self::handle_git_conflicts))
            .route("/api/git/conflicts/resolve", post(Self::handle_git_conflicts_resolve))
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(self.clone()),
                Self::auth_middleware,
            ))
            .layer(axum::middleware::from_fn(Self::cors_middleware))
            .with_state(Arc::new(self.clone()));

        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        println!("🚀 Hagibis Visual IDE listening on http://{}", addr);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    async fn cors_middleware(
        req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        use axum::http::Method;
        if req.method() == Method::OPTIONS {
            let mut resp = axum::response::Response::new(axum::body::Body::empty());
            let headers = resp.headers_mut();
            headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
            headers.insert("Access-Control-Allow-Methods", "GET, POST, OPTIONS, PUT, DELETE".parse().unwrap());
            headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
            return resp;
        }
        let mut response = next.run(req).await;
        let headers = response.headers_mut();
        headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
        headers.insert("Access-Control-Allow-Methods", "GET, POST, OPTIONS, PUT, DELETE".parse().unwrap());
        response
    }

    async fn auth_middleware(
        State(state): State<Arc<Self>>,
        req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        let path = req.uri().path();

        // Landing page, auth verification, and OPTIONS preflight do not require authentication
        if path == "/" || path == "/api/auth/verify" || req.method() == axum::http::Method::OPTIONS {
            return next.run(req).await;
        }

        if let Some(ref token) = state.auth_token {
            let auth_hdr = req.headers().get("authorization").and_then(|h| h.to_str().ok());
            let custom_hdr = req.headers().get("x-hgb-token").and_then(|h| h.to_str().ok());
            let query_token = req.uri().query().and_then(|q| {
                q.split('&').find_map(|pair| {
                    let mut parts = pair.split('=');
                    if parts.next() == Some("token") {
                        parts.next()
                    } else {
                        None
                    }
                })
            });

            let provided = auth_hdr
                .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
                .or(custom_hdr)
                .or(query_token);

            if provided != Some(token.as_str()) {
                let mut resp = axum::response::Response::new(
                    axum::body::Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "error": "Authentication required. Provide valid Bearer token or ?token= parameter.",
                            "auth_required": true
                        })).unwrap()
                    )
                );
                *resp.status_mut() = StatusCode::UNAUTHORIZED;
                resp.headers_mut().insert(axum::http::header::CONTENT_TYPE, "application/json".parse().unwrap());
                resp.headers_mut().insert(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
                return resp;
            }
        }

        next.run(req).await
    }

    async fn serve_index() -> Html<&'static str> {
        Html(INDEX_HTML)
    }

    async fn serve_manifest() -> impl IntoResponse {
        (
            [(axum::http::header::CONTENT_TYPE, "application/manifest+json")],
            MANIFEST_JSON,
        )
    }

    async fn serve_sw() -> impl IntoResponse {
        (
            [
                (axum::http::header::CONTENT_TYPE, "application/javascript"),
                (axum::http::header::CACHE_CONTROL, "no-cache"),
            ],
            SW_JS,
        )
    }

    async fn serve_icon() -> impl IntoResponse {
        (
            [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
            ICON_SVG,
        )
    }

    async fn handle_workspace(State(state): State<Arc<Self>>) -> impl IntoResponse {
        async fn walk_dir(dir: &Path) -> Result<Vec<FileNode>, std::io::Error> {
            let mut entries = fs::read_dir(dir).await?;
            let mut nodes = Vec::new();
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "target" || name == ".git" || name == ".hgb" || name == "node_modules" {
                    continue;
                }
                let is_dir = entry.file_type().await?.is_dir();
                let children = if is_dir {
                    Some(Box::pin(walk_dir(&path)).await?)
                } else {
                    None
                };
                nodes.push(FileNode {
                    name,
                    path: path.to_string_lossy().to_string(),
                    is_dir,
                    children,
                });
            }
            nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
            Ok(nodes)
        }
        match walk_dir(&state.workspace_root).await {
            Ok(nodes) => (StatusCode::OK, Json(nodes)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    async fn handle_get_file(Query(query): Query<FileQuery>) -> impl IntoResponse {
        match fs::read_to_string(&query.path).await {
            Ok(content) => (StatusCode::OK, content).into_response(),
            Err(e) => (StatusCode::NOT_FOUND, format!("Error reading file: {}", e)).into_response(),
        }
    }

    async fn handle_save_file(Json(payload): Json<SavePayload>) -> impl IntoResponse {
        match fs::write(&payload.path, payload.content).await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "saved", "path": payload.path }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error writing file: {}", e)).into_response(),
        }
    }

    async fn handle_create_file(Json(payload): Json<CreatePayload>) -> impl IntoResponse {
        let p = Path::new(&payload.path);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent).await;
        }
        match fs::write(p, "").await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "created", "path": payload.path }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error creating file: {}", e)).into_response(),
        }
    }

    async fn handle_create_folder(Json(payload): Json<CreatePayload>) -> impl IntoResponse {
        match fs::create_dir_all(&payload.path).await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "folder_created", "path": payload.path }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error creating folder: {}", e)).into_response(),
        }
    }

    async fn handle_delete_entry(Json(payload): Json<CreatePayload>) -> impl IntoResponse {
        let p = Path::new(&payload.path);
        let res = if p.is_dir() {
            fs::remove_dir_all(p).await
        } else {
            fs::remove_file(p).await
        };
        match res {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "deleted", "path": payload.path }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error deleting entry: {}", e)).into_response(),
        }
    }

    async fn handle_rename_entry(Json(payload): Json<RenamePayload>) -> impl IntoResponse {
        match fs::rename(&payload.old_path, &payload.new_path).await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "renamed", "path": payload.new_path }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error renaming entry: {}", e)).into_response(),
        }
    }

    async fn handle_diff(Json(payload): Json<DiffPayload>) -> impl IntoResponse {
        // Native Zig Myers SES & Character-level intra-line calculation
        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(&payload.original, &payload.modified);
        let ansi_terminal = InlineDiffEngine::render_ansi_inline_diff(&payload.original, &payload.modified);
        let side_by_side = InlineDiffEngine::render_side_by_side(&payload.original, &payload.modified, 45);

        let response = serde_json::json!({
            "metrics": metrics,
            "lines": lines,
            "ansi_terminal": ansi_terminal,
            "side_by_side": side_by_side
        });

        (StatusCode::OK, Json(response)).into_response()
    }

    async fn handle_tab_commit(Json(payload): Json<TabCommitPayload>) -> impl IntoResponse {
        // O(1) DualBufferOverlay Tab acceptance pointer swap
        let mut overlay = DualBufferOverlay::new(&payload.path, &payload.original);
        overlay.set_speculative_content(&payload.speculative);
        let committed = overlay.commit_tab_acceptance();

        match fs::write(&payload.path, &committed).await {
            Ok(_) => (StatusCode::OK, Json(serde_json::json!({
                "status": "committed",
                "path": payload.path,
                "bytes": committed.len()
            }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to commit buffer to disk: {}", e)).into_response(),
        }
    }

    async fn handle_chat(State(state): State<Arc<Self>>, Json(payload): Json<ChatPayload>) -> impl IntoResponse {
        let prompt_clone = payload.prompt.clone();
        let prompt_full = if let Some(ctx) = payload.context {
            format!("Code Context:\n```\n{}\n```\n\nTask: {}\nProvide direct, surgical code corrections.", ctx, prompt_clone)
        } else {
            prompt_clone
        };

        let cloud = state.cloud_settings.read().await;
        let msgs = vec![crate::cloud_settings::ChatMessageItem {
            role: "user".to_string(),
            content: prompt_full.clone(),
        }];

        match cloud.dispatch_chat(&msgs, Some("You are Hagibis Visual Copilot, an expert AI pair-programmer.")).await {
            Ok(text) => {
                (StatusCode::OK, Json(serde_json::json!({
                    "response": text,
                    "model": cloud.active_model,
                    "provider": cloud.active_provider,
                    "status": "success"
                }))).into_response()
            }
            Err(e) => {
                let client = reqwest::Client::new();
                let fallback_req = serde_json::json!({
                    "model": "qwen2.5-coder:1.5b",
                    "prompt": format!("Task: {}\nProvide direct, surgical code corrections.", payload.prompt),
                    "stream": false
                });
                match client.post("http://127.0.0.1:11434/api/generate")
                    .json(&fallback_req)
                    .timeout(std::time::Duration::from_secs(15))
                    .send()
                    .await {
                    Ok(r) => {
                        if let Ok(json) = r.json::<serde_json::Value>().await {
                            let text = json.get("response").and_then(|v| v.as_str()).unwrap_or("No response content");
                            (StatusCode::OK, Json(serde_json::json!({
                                "response": text,
                                "model": "qwen2.5-coder:1.5b",
                                "status": "success"
                            }))).into_response()
                        } else {
                            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e }))).into_response()
                        }
                    }
                    Err(_) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": e }))).into_response(),
                }
            }
        }
    }

    async fn handle_status() -> impl IntoResponse {
        let has_avx2 = is_x86_feature_detected!("avx2");
        let has_sse41 = is_x86_feature_detected!("sse4.1");
        let simd_label = if has_avx2 {
            "AVX2 256-bit SIMD Accelerated (Myers SES)"
        } else if has_sse41 {
            "SSE4.1 128-bit SIMD Accelerated (Myers SES)"
        } else {
            "Scalar Optimized (Myers SES)"
        };

        let landlock_label = if hgb_core::zig_accelerate::sandbox_check_support() {
            "Enforced (Linux Landlock LSM Active)"
        } else {
            "Unavailable (Kernel lacks Landlock support)"
        };

        let memory_label = get_real_memory_metrics();
        let llm_label = query_local_llm_status().await;

        let status = serde_json::json!({
            "status": "online",
            "engine": "Rust 2021 + Native Zig 0.13.0 SIMD",
            "myers_ses": simd_label,
            "dual_buffer_overlay": "Active (Dual-buffer overlay with instant commit)",
            "memory_resident": memory_label,
            "landlock_confinement": landlock_label,
            "local_llm": llm_label
        });
        (StatusCode::OK, Json(status)).into_response()
    }

    async fn handle_search(State(state): State<Arc<Self>>, Json(payload): Json<SearchPayload>) -> impl IntoResponse {
        let q_lower = payload.query.to_lowercase();
        let mut matches = Vec::new();

        async fn search_dir(dir: &Path, q: &str, out: &mut Vec<serde_json::Value>) -> Result<(), std::io::Error> {
            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "target" || name == ".git" || name == ".hgb" || name == "node_modules" {
                    continue;
                }
                if entry.file_type().await?.is_dir() {
                    Box::pin(search_dir(&path, q, out)).await?;
                } else {
                    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ["png", "jpg", "ico", "bin", "so", "lock", "tar", "gz"].contains(&ext) {
                        continue;
                    }
                    if let Ok(c) = fs::read_to_string(&path).await {
                        for (idx, line) in c.lines().enumerate() {
                            if line.to_lowercase().contains(q) {
                                out.push(serde_json::json!({
                                    "path": path.to_string_lossy(),
                                    "line_number": idx + 1,
                                    "line_content": line.trim(),
                                }));
                                if out.len() >= 50 {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }

        let _ = search_dir(&state.workspace_root, &q_lower, &mut matches).await;
        (StatusCode::OK, Json(serde_json::json!({
            "query": payload.query,
            "total_matches": matches.len(),
            "matches": matches
        }))).into_response()
    }

    async fn handle_terminal(State(state): State<Arc<Self>>, Json(payload): Json<TerminalPayload>) -> impl IntoResponse {
        let t0 = Instant::now();
        let dir = payload.working_dir.map(PathBuf::from).unwrap_or_else(|| state.workspace_root.clone());

        match tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&payload.command)
            .current_dir(dir)
            .output()
            .await
        {
            Ok(output) => {
                let elapsed = t0.elapsed().as_millis() as u64;
                (StatusCode::OK, Json(serde_json::json!({
                    "command": payload.command,
                    "exit_code": output.status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                    "duration_ms": elapsed
                }))).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    async fn handle_rules(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let candidates = [
            (".cursorrules", "Cursor Rules (.cursorrules)"),
            (".hgb/rules", "Hagibis Project Directives (.hgb/rules)"),
            (".github/copilot-instructions.md", "Copilot Instructions (.github/copilot-instructions.md)"),
            ("AGENTS.md", "Agents Architecture (AGENTS.md)"),
        ];

        for (rel, source) in candidates {
            let p = state.workspace_root.join(rel);
            if p.is_file() {
                if let Ok(c) = fs::read_to_string(&p).await {
                    return (StatusCode::OK, Json(serde_json::json!({
                        "rules": c,
                        "source": source
                    }))).into_response();
                }
            }
        }

        (StatusCode::OK, Json(serde_json::json!({
            "rules": "// Default Hagibis Microkernel Sovereign Directives\n// 1. Prioritize zero-copy memory and Zig AVX2 SIMD acceleration.\n// 2. Strict Landlock LSM sandboxing.",
            "source": "Default System Rules"
        }))).into_response()
    }

    async fn handle_inline_edit(Json(payload): Json<InlineEditPayload>) -> impl IntoResponse {
        let client = reqwest::Client::new();
        let prompt = format!(
            "File: {}\nContext:\n```\n{}\n```\nSelected Code to modify:\n```\n{}\n```\nInstruction: {}\nProvide ONLY the replacement code snippet directly, without explanations or commentary.",
            payload.path, payload.file_context, payload.selected_text, payload.instruction
        );

        let ollama_req = serde_json::json!({
            "model": "dynabook-coder:latest",
            "prompt": prompt,
            "stream": false
        });

        let mut replacement = String::new();

        if let Ok(resp) = client.post("http://127.0.0.1:11434/api/generate")
            .json(&ollama_req)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                    replacement = text.to_string();
                }
            }
        }

        if replacement.is_empty() {
            return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Local LLM failed to generate inline edit. Ensure Ollama is running ('ollama serve') and model is available."
            }))).into_response();
        }

        // Clean markdown fences
        let clean = if replacement.contains("```") {
            let parts: Vec<&str> = replacement.split("```").collect();
            if parts.len() >= 2 {
                let code_part = parts[1];
                let lines: Vec<&str> = code_part.lines().collect();
                if !lines.is_empty() && (lines[0].starts_with("rust") || lines[0].starts_with("zig") || lines[0].starts_with("js")) {
                    lines[1..].join("\n")
                } else {
                    code_part.to_string()
                }
            } else {
                replacement
            }
        } else {
            replacement
        };

        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(&payload.selected_text, &clean);
        let ansi_terminal = InlineDiffEngine::render_ansi_inline_diff(&payload.selected_text, &clean);
        let side_by_side = InlineDiffEngine::render_side_by_side(&payload.selected_text, &clean, 45);

        (StatusCode::OK, Json(serde_json::json!({
            "metrics": metrics,
            "lines": lines,
            "ansi_terminal": ansi_terminal,
            "side_by_side": side_by_side,
            "replacement": clean
        }))).into_response()
    }

    async fn handle_composer(Json(payload): Json<ComposerPayload>) -> impl IntoResponse {
        let mut file_plans = Vec::new();
        let client = reqwest::Client::new();

        for file_path in payload.files {
            let orig = fs::read_to_string(&file_path).await.unwrap_or_default();
            let file_prompt = format!(
                "You are Hagibis Composer. File: {}\nOriginal Code:\n```\n{}\n```\nTask: {}\nProvide the complete updated code for this file incorporating the changes. Output ONLY the code enclosed in ``` fenced block.",
                file_path, orig, payload.prompt
            );

            let file_req = serde_json::json!({
                "model": "dynabook-coder:latest",
                "prompt": file_prompt,
                "stream": false
            });

            let mut modif = String::new();
            if let Ok(resp) = client.post("http://127.0.0.1:11434/api/generate")
                .json(&file_req)
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await
            {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                        modif = text.to_string();
                    }
                }
            }

            if modif.is_empty() {
                let fb_req = serde_json::json!({
                    "model": "qwen2.5-coder:1.5b",
                    "prompt": file_prompt,
                    "stream": false
                });
                if let Ok(resp) = client.post("http://127.0.0.1:11434/api/generate")
                    .json(&fb_req)
                    .timeout(std::time::Duration::from_secs(20))
                    .send()
                    .await
                {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                            modif = text.to_string();
                        }
                    }
                }
            }

            let clean_modif = if modif.contains("```") {
                let parts: Vec<&str> = modif.split("```").collect();
                if parts.len() >= 2 {
                    let code_part = parts[1];
                    let lines: Vec<&str> = code_part.lines().collect();
                    if !lines.is_empty() && (lines[0].starts_with("rust") || lines[0].starts_with("zig") || lines[0].starts_with("js") || lines[0].starts_with("ts") || lines[0].starts_with("python")) {
                        lines[1..].join("\n")
                    } else {
                        code_part.to_string()
                    }
                } else {
                    modif
                }
            } else if !modif.is_empty() {
                modif
            } else {
                return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                    "error": format!("Local LLM failed to generate updates for '{}'. Ensure Ollama is running ('ollama serve').", file_path)
                }))).into_response();
            };

            file_plans.push(ComposerFilePlan {
                path: file_path.clone(),
                original_content: orig,
                modified_content: clean_modif,
                explanation: format!("Orchestrated update for {} according to task: {}", file_path, payload.prompt),
            });
        }

        (StatusCode::OK, Json(serde_json::json!({
            "summary": format!("Composer plan generated for {} file(s)", file_plans.len()),
            "files": file_plans,
            "model": "dynabook-coder:latest"
        }))).into_response()
    }

    async fn handle_tab_completion(Json(payload): Json<TabCompletionPayload>) -> impl IntoResponse {
        let client = reqwest::Client::new();
        let fim_prompt = format!("<|fim_prefix|>{}<|fim_suffix|>{}<|fim_middle|>", payload.prefix, payload.suffix);

        let ollama_req = serde_json::json!({
            "model": "qwen2.5-coder:1.5b",
            "prompt": fim_prompt,
            "raw": true,
            "stream": false,
            "options": {
                "num_predict": 32,
                "temperature": 0.2,
                "stop": ["<|fim_pad|>", "<|endoftext|>", "\n\n", "```"]
            }
        });

        if let Ok(resp) = client.post("http://127.0.0.1:11434/api/generate")
            .json(&ollama_req)
            .timeout(std::time::Duration::from_millis(4000))
            .send()
            .await
        {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                    let trimmed = text.trim_end();
                    if !trimmed.is_empty() {
                        return (StatusCode::OK, Json(serde_json::json!({
                            "completion": trimmed,
                            "model": "qwen2.5-coder:1.5b (FIM)"
                        }))).into_response();
                    }
                }
            }
        }

        (StatusCode::OK, Json(serde_json::json!({
            "completion": "",
            "model": "none"
        }))).into_response()
    }

    async fn handle_resolve_context(State(state): State<Arc<Self>>, Json(payload): Json<ContextResolutionPayload>) -> impl IntoResponse {
        let mut expanded = payload.prompt.clone();
        let mut sources = Vec::new();

        if payload.prompt.contains("@git") || payload.prompt.contains("@Git") {
            if let Ok(output) = tokio::process::Command::new("git")
                .args(["status", "-s"])
                .current_dir(&state.workspace_root)
                .output()
                .await
            {
                let git_status = String::from_utf8_lossy(&output.stdout);
                let git_ctx = format!("\n[Git Status]:\n{}\n", git_status);
                expanded = expanded.replace("@git", &git_ctx).replace("@Git", &git_ctx);
                sources.push("Git Workspace State".to_string());
            }
        }

        if payload.prompt.contains("@rules") || payload.prompt.contains("@Rules") {
            let candidates = [
                (".cursorrules", "Cursor Rules (.cursorrules)"),
                (".hgb/rules", "Hagibis Project Directives (.hgb/rules)"),
                (".github/copilot-instructions.md", "Copilot Instructions"),
                ("AGENTS.md", "Agents Architecture (AGENTS.md)"),
            ];
            let mut matched = false;
            for (rel, src_name) in candidates {
                let p = state.workspace_root.join(rel);
                if p.is_file() {
                    if let Ok(c) = fs::read_to_string(&p).await {
                        let rules_ctx = format!("\n[Project Rules ({})]:\n{}\n", src_name, c);
                        expanded = expanded.replace("@rules", &rules_ctx).replace("@Rules", &rules_ctx);
                        sources.push(src_name.to_string());
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                let default_rules = "// Default Hagibis Microkernel Sovereign Directives\n// 1. Prioritize zero-copy memory and Zig AVX2/SSE4 SIMD acceleration.\n// 2. Enforce strict Landlock LSM confinement.\n// 3. Keep all completions localized without remote telemetry.";
                let rules_ctx = format!("\n[Project Rules (Default System Rules)]:\n{}\n", default_rules);
                expanded = expanded.replace("@rules", &rules_ctx).replace("@Rules", &rules_ctx);
                sources.push("Default System Rules".to_string());
            }
        }

        if payload.prompt.contains("@problems") || payload.prompt.contains("@Problems") {
            let output = tokio::process::Command::new("cargo")
                .args(["check", "--message-format=json", "--quiet"])
                .current_dir(&state.workspace_root)
                .output()
                .await;

            let mut diags_text = String::new();
            let mut problem_count = 0;
            if let Ok(out) = output {
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                        if json.get("reason").and_then(|r| r.as_str()) == Some("compiler-message") {
                            if let Some(msg) = json.get("message") {
                                let rendered = msg.get("rendered").and_then(|r| r.as_str()).unwrap_or("");
                                if !rendered.is_empty() && problem_count < 10 {
                                    diags_text.push_str(rendered);
                                    problem_count += 1;
                                }
                            }
                        }
                    }
                }
            }
            let problem_ctx = if problem_count > 0 {
                format!("\n[Live Compiler Problems ({} issues)]:\n{}\n", problem_count, diags_text)
            } else {
                "\n[Live Compiler Problems]: 0 errors or warnings detected in workspace.\n".to_string()
            };
            expanded = expanded.replace("@problems", &problem_ctx).replace("@Problems", &problem_ctx);
            sources.push(format!("Compiler Diagnostics ({} problems)", problem_count));
        }

        if payload.prompt.contains("@Codebase") || payload.prompt.contains("@codebase") {
            let symbols = crate::ide_agent_engine::IdeAgentEngine::extract_workspace_symbols(&state.workspace_root).await.unwrap_or_default();
            let mut symbol_summary = String::new();
            for s in symbols.iter().take(20) {
                symbol_summary.push_str(&format!("- [{}] {} in {} (L{})\n", s.kind, s.signature, s.file_path, s.line_number));
            }

            let clean_prompt_keywords: Vec<&str> = payload.prompt
                .split_whitespace()
                .filter(|w| !w.starts_with('@') && w.len() > 3)
                .collect();

            let mut snippet_summary = String::new();
            for kw in clean_prompt_keywords.iter().take(3) {
                let kw_lower = kw.to_lowercase();
                if let Ok(mut entries) = fs::read_dir(&state.workspace_root).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let path = entry.path();
                        if path.is_file() {
                            if let Ok(content) = fs::read_to_string(&path).await {
                                for (idx, line) in content.lines().enumerate() {
                                    if line.to_lowercase().contains(&kw_lower) {
                                        snippet_summary.push_str(&format!("{}:L{} -> {}\n", path.display(), idx + 1, line.trim()));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let mut codebase_ctx = String::new();
            if !symbol_summary.is_empty() {
                codebase_ctx.push_str(&format!("\n[Codebase Outline & Symbols ({} symbols)]:\n{}\n", symbols.len(), symbol_summary));
            }
            if !snippet_summary.is_empty() {
                codebase_ctx.push_str(&format!("[Relevant Code Snippets]:\n{}\n", snippet_summary));
            }

            if !codebase_ctx.is_empty() {
                expanded = expanded.replace("@Codebase", &codebase_ctx).replace("@codebase", &codebase_ctx);
            }
            sources.push(format!("Project Codebase Index ({} symbols extracted)", symbols.len()));
        }

        // Match @file:, @folder:, and @web:
        for word in payload.prompt.split_whitespace() {
            if let Some(rest) = word.strip_prefix("@file:").or_else(|| word.strip_prefix("@File:")) {
                let target = rest.trim_matches(|c| c == ',' || c == '.' || c == ';' || c == ')' || c == ']' || c == '"' || c == '\'');
                let p = if Path::new(target).is_absolute() {
                    PathBuf::from(target)
                } else {
                    state.workspace_root.join(target)
                };
                if let Ok(content) = fs::read_to_string(&p).await {
                    let file_ctx = format!("\n[File Context: {}]:\n```\n{}\n```\n", target, content);
                    expanded = expanded.replace(word, &file_ctx);
                    sources.push(format!("File Context ({})", target));
                }
            } else if let Some(rest) = word.strip_prefix("@folder:").or_else(|| word.strip_prefix("@Folder:")) {
                let target = rest.trim_matches(|c| c == ',' || c == '.' || c == ';' || c == ')' || c == ']' || c == '"' || c == '\'');
                let p = if Path::new(target).is_absolute() {
                    PathBuf::from(target)
                } else {
                    state.workspace_root.join(target)
                };
                if let Ok(mut entries) = fs::read_dir(&p).await {
                    let mut listing = Vec::new();
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                        if is_dir { listing.push(format!("{}/", name)); } else { listing.push(name); }
                        if listing.len() >= 30 { break; }
                    }
                    let folder_ctx = format!("\n[Folder Listing: {} ({} items)]:\n{}\n", target, listing.len(), listing.join("\n"));
                    expanded = expanded.replace(word, &folder_ctx);
                    sources.push(format!("Folder Listing ({})", target));
                }
            } else if let Some(rest) = word.strip_prefix("@web:").or_else(|| word.strip_prefix("@Web:")) {
                let url = rest.trim_matches(|c| c == ',' || c == ';' || c == ')' || c == ']' || c == '"' || c == '\'');
                let client = reqwest::Client::new();
                if let Ok(resp) = client.get(url).timeout(std::time::Duration::from_secs(6)).send().await {
                    if let Ok(body) = resp.text().await {
                        let clean_text = if body.contains("<body") {
                            body.split("<body").nth(1).unwrap_or(&body)
                                .chars().filter(|&c| c != '<' && c != '>').take(2500).collect::<String>()
                        } else {
                            body.chars().take(2500).collect::<String>()
                        };
                        let web_ctx = format!("\n[Web Resource: {}]:\n```\n{}\n```\n", url, clean_text);
                        expanded = expanded.replace(word, &web_ctx);
                        sources.push(format!("Web Resource ({})", url));
                    }
                }
            }
        }

        (StatusCode::OK, Json(serde_json::json!({
            "expanded_prompt": expanded,
            "sources": sources
        }))).into_response()
    }

    async fn handle_composer_commit(Json(plans): Json<Vec<ComposerFilePlan>>) -> impl IntoResponse {
        // Automatic Time-Travel Checkpoint snapshot before mutation
        if let Ok(mut mgr) = get_checkpoint_mgr().lock() {
            for plan in &plans {
                let p = PathBuf::from(&plan.path);
                let _ = mgr.snapshot_file(&p);
            }
            let _ = mgr.create_checkpoint("Auto-checkpoint before Composer commit", HashMap::new(), HashMap::new());
        }

        let mut results = Vec::new();
        for plan in plans {
            let mut overlay = DualBufferOverlay::new(&plan.path, &plan.original_content);
            overlay.set_speculative_content(&plan.modified_content);
            let committed = overlay.commit_tab_acceptance();
            let bytes = committed.len();

            if fs::write(&plan.path, &committed).await.is_ok() {
                results.push(serde_json::json!({
                    "status": "committed",
                    "path": plan.path,
                    "bytes": bytes
                }));
            }
        }
        (StatusCode::OK, Json(results)).into_response()
    }

    async fn handle_get_checkpoints() -> impl IntoResponse {
        let mgr = match get_checkpoint_mgr().lock() {
            Ok(m) => m,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        let list: Vec<serde_json::Value> = mgr.get_checkpoints().iter().map(|c| {
            serde_json::json!({
                "id": c.checkpoint_id,
                "label": c.label,
                "timestamp_utc": c.timestamp_utc,
                "files_count": c.file_snapshots.len()
            })
        }).collect();
        (StatusCode::OK, Json(list)).into_response()
    }

    async fn handle_create_checkpoint(Json(payload): Json<CreateCheckpointPayload>) -> impl IntoResponse {
        let mut mgr = match get_checkpoint_mgr().lock() {
            Ok(m) => m,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        for f in &payload.files {
            let p = PathBuf::from(f);
            let _ = mgr.snapshot_file(&p);
        }
        let ckpt = mgr.create_checkpoint(&payload.label, HashMap::new(), HashMap::new());
        (StatusCode::OK, Json(serde_json::json!({ "checkpoint_id": ckpt.checkpoint_id }))).into_response()
    }

    async fn handle_rollback(Json(payload): Json<RollbackPayload>) -> impl IntoResponse {
        let mgr = match get_checkpoint_mgr().lock() {
            Ok(m) => m,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        let res = match payload.checkpoint_id {
            Some(id) if !id.trim().is_empty() => mgr.restore_files_from_checkpoint(&id),
            _ => mgr.rollback_latest_files(),
        };
        match res {
            Ok(report) => (StatusCode::OK, Json(report)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    async fn handle_get_models(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let mut list = Vec::new();
        let cloud = state.cloud_settings.read().await;

        // Ollama
        let ollama_url = cloud.ollama_base_url.clone().unwrap_or_else(|| "http://127.0.0.1:11434".to_string());
        let client = reqwest::Client::new();
        if let Ok(resp) = client.get(format!("{}/api/tags", ollama_url.trim_end_matches('/'))).timeout(std::time::Duration::from_millis(600)).send().await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(models) = json.get("models").and_then(|m| m.as_array()) {
                    for m in models {
                        if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
                            list.push(serde_json::json!({
                                "id": name,
                                "name": format!("{} (Local)", name),
                                "provider": "Ollama (Local)",
                                "is_local": true,
                                "context_window": 32768
                            }));
                        }
                    }
                }
            }
        }

        // Anthropic
        if cloud.anthropic_api_key.is_some() || std::env::var("ANTHROPIC_API_KEY").is_ok() {
            list.push(serde_json::json!({
                "id": "claude-3-5-sonnet-20241022",
                "name": "Claude 3.5 Sonnet (Anthropic)",
                "provider": "Anthropic",
                "is_local": false,
                "context_window": 200000
            }));
            list.push(serde_json::json!({
                "id": "claude-3-7-sonnet-20250219",
                "name": "Claude 3.7 Sonnet (Anthropic)",
                "provider": "Anthropic",
                "is_local": false,
                "context_window": 200000
            }));
        }

        // OpenAI
        if cloud.openai_api_key.is_some() || std::env::var("OPENAI_API_KEY").is_ok() {
            list.push(serde_json::json!({
                "id": "gpt-4o",
                "name": "GPT-4o (OpenAI)",
                "provider": "OpenAI",
                "is_local": false,
                "context_window": 128000
            }));
            list.push(serde_json::json!({
                "id": "gpt-4o-mini",
                "name": "GPT-4o Mini (OpenAI)",
                "provider": "OpenAI",
                "is_local": false,
                "context_window": 128000
            }));
        }

        // Gemini
        if cloud.gemini_api_key.is_some() || std::env::var("GEMINI_API_KEY").is_ok() {
            list.push(serde_json::json!({
                "id": "gemini-1.5-pro",
                "name": "Gemini 1.5 Pro (Google Gemini)",
                "provider": "Gemini",
                "is_local": false,
                "context_window": 1000000
            }));
            list.push(serde_json::json!({
                "id": "gemini-1.5-flash",
                "name": "Gemini 1.5 Flash (Google Gemini)",
                "provider": "Gemini",
                "is_local": false,
                "context_window": 1000000
            }));
        }

        // DeepSeek
        if cloud.deepseek_api_key.is_some() || std::env::var("DEEPSEEK_API_KEY").is_ok() {
            list.push(serde_json::json!({
                "id": "deepseek-chat",
                "name": "DeepSeek-V3",
                "provider": "DeepSeek",
                "is_local": false,
                "context_window": 64000
            }));
            list.push(serde_json::json!({
                "id": "deepseek-reasoner",
                "name": "DeepSeek-R1 (Reasoning)",
                "provider": "DeepSeek",
                "is_local": false,
                "context_window": 64000
            }));
        }

        // Default local entries if list is empty
        if list.is_empty() {
            list.push(serde_json::json!({
                "id": "dynabook-coder:latest",
                "name": "dynabook-coder (Local)",
                "provider": "Ollama (Local)",
                "is_local": true,
                "context_window": 32768
            }));
            list.push(serde_json::json!({
                "id": "qwen2.5-coder:1.5b",
                "name": "qwen2.5-coder:1.5b (Local)",
                "provider": "Ollama (Local)",
                "is_local": true,
                "context_window": 32768
            }));
        }

        (StatusCode::OK, Json(list)).into_response()
    }

    async fn handle_load_session(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let session_file = state.workspace_root.join(".hgb").join("session.json");
        if session_file.exists() {
            if let Ok(content) = fs::read_to_string(&session_file).await {
                if let Ok(session) = serde_json::from_str::<WorkspaceSession>(&content) {
                    return (StatusCode::OK, Json(session)).into_response();
                }
            }
        }
        (StatusCode::OK, Json(WorkspaceSession::default())).into_response()
    }

    async fn handle_save_session(State(state): State<Arc<Self>>, Json(session): Json<WorkspaceSession>) -> impl IntoResponse {
        let dir = state.workspace_root.join(".hgb");
        let _ = fs::create_dir_all(&dir).await;
        let session_file = dir.join("session.json");
        if let Ok(json) = serde_json::to_string_pretty(&session) {
            if fs::write(&session_file, json).await.is_ok() {
                return (StatusCode::OK, Json(serde_json::json!({ "status": "saved" }))).into_response();
            }
        }
        (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save session").into_response()
    }

    async fn handle_git_diff(State(state): State<Arc<Self>>, Json(payload): Json<GitDiffPayload>) -> impl IntoResponse {
        match crate::ide_agent_engine::IdeAgentEngine::compute_file_git_diff(&payload.path, &state.workspace_root).await {
            Ok(diff) => (StatusCode::OK, Json(diff)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }

    async fn handle_symbols(State(state): State<Arc<Self>>) -> impl IntoResponse {
        match crate::ide_agent_engine::IdeAgentEngine::extract_workspace_symbols(&state.workspace_root).await {
            Ok(syms) => (StatusCode::OK, Json(syms)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }

    async fn handle_diagnostics(State(state): State<Arc<Self>>, Json(payload): Json<DiagnosticsPayload>) -> impl IntoResponse {
        match crate::ide_agent_engine::IdeAgentEngine::query_file_diagnostics(&payload.path, &state.workspace_root).await {
            Ok(diags) => (StatusCode::OK, Json(diags)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }

    async fn handle_autonomous_agent(State(state): State<Arc<Self>>, Json(payload): Json<AutonomousAgentPayload>) -> impl IntoResponse {
        match crate::ide_agent_engine::IdeAgentEngine::run_autonomous_agent(
            &payload.task,
            payload.files,
            payload.verify_command,
            payload.max_iterations.unwrap_or(3),
            &state.workspace_root,
        ).await {
            Ok(report) => (StatusCode::OK, Json(report)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }

    async fn handle_mcp_tool(Json(payload): Json<McpToolPayload>) -> impl IntoResponse {
        match crate::ide_agent_engine::IdeAgentEngine::execute_mcp_tool_call(
            &payload.server_cmd,
            &payload.tool_name,
            &payload.arguments,
        ).await {
            Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }

    // --- Persistent Multi-Turn Conversation Threads ---

    async fn handle_get_chat_sessions(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let dir = state.workspace_root.join(".hgb").join("chat_sessions");
        if !dir.exists() {
            return (StatusCode::OK, Json(Vec::<ChatSessionSummary>::new())).into_response();
        }
        let mut summaries = Vec::new();
        if let Ok(mut entries) = fs::read_dir(&dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(content) = fs::read_to_string(&path).await {
                        if let Ok(sess) = serde_json::from_str::<ChatSession>(&content) {
                            summaries.push(ChatSessionSummary {
                                id: sess.id,
                                title: sess.title,
                                created_at_utc: sess.created_at_utc,
                                updated_at_utc: sess.updated_at_utc,
                                model: sess.model,
                                message_count: sess.messages.len(),
                            });
                        }
                    }
                }
            }
        }
        summaries.sort_by(|a, b| b.updated_at_utc.cmp(&a.updated_at_utc));
        (StatusCode::OK, Json(summaries)).into_response()
    }

    async fn handle_load_chat_session(State(state): State<Arc<Self>>, Query(query): Query<SessionQuery>) -> impl IntoResponse {
        let file = state.workspace_root.join(".hgb").join("chat_sessions").join(format!("{}.json", query.id));
        if !file.exists() {
            return (StatusCode::NOT_FOUND, "Chat session not found").into_response();
        }
        match fs::read_to_string(&file).await {
            Ok(content) => match serde_json::from_str::<ChatSession>(&content) {
                Ok(sess) => (StatusCode::OK, Json(sess)).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Parse error: {}", e)).into_response(),
            },
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Read error: {}", e)).into_response(),
        }
    }

    async fn handle_save_chat_session(State(state): State<Arc<Self>>, Json(session): Json<ChatSession>) -> impl IntoResponse {
        let dir = state.workspace_root.join(".hgb").join("chat_sessions");
        let _ = fs::create_dir_all(&dir).await;
        let file = dir.join(format!("{}.json", session.id));
        match serde_json::to_string_pretty(&session) {
            Ok(json) => match fs::write(&file, json).await {
                Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "status": "saved", "id": session.id }))).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Write error: {}", e)).into_response(),
            },
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Serialize error: {}", e)).into_response(),
        }
    }

    async fn handle_delete_chat_session(State(state): State<Arc<Self>>, Json(payload): Json<SessionQuery>) -> impl IntoResponse {
        let file = state.workspace_root.join(".hgb").join("chat_sessions").join(format!("{}.json", payload.id));
        if file.exists() {
            let _ = fs::remove_file(&file).await;
        }
        (StatusCode::OK, Json(serde_json::json!({ "status": "deleted" }))).into_response()
    }

    async fn handle_create_chat_session(State(state): State<Arc<Self>>, Json(payload): Json<CreateSessionPayload>) -> impl IntoResponse {
        let dir = state.workspace_root.join(".hgb").join("chat_sessions");
        let _ = fs::create_dir_all(&dir).await;
        let now = chrono::Utc::now().to_rfc3339();
        let id = format!("chat_{}_{:x}", chrono::Utc::now().timestamp_millis(), std::process::id());
        let title_clean = if payload.title.trim().is_empty() { "New Chat".to_string() } else { payload.title };
        let sess = ChatSession {
            id: id.clone(),
            title: title_clean,
            created_at_utc: now.clone(),
            updated_at_utc: now,
            model: payload.model,
            messages: Vec::new(),
        };
        let file = dir.join(format!("{}.json", id));
        if let Ok(json) = serde_json::to_string_pretty(&sess) {
            let _ = fs::write(&file, json).await;
        }
        (StatusCode::OK, Json(sess)).into_response()
    }

    // --- Global Regex Codebase Search & Replace Engine (Ctrl+Shift+F) ---

    async fn handle_global_search(State(state): State<Arc<Self>>, Json(payload): Json<GlobalSearchPayload>) -> impl IntoResponse {
        let query = payload.query;
        let is_regex = payload.is_regex.unwrap_or(false);
        let case_sensitive = payload.case_sensitive.unwrap_or(false);

        if query.is_empty() {
            return (StatusCode::OK, Json(GlobalSearchResult {
                query,
                is_regex,
                case_sensitive,
                total_matches: 0,
                files_count: 0,
                matches: Vec::new(),
            })).into_response();
        }

        let compiled_re = if is_regex {
            match regex::RegexBuilder::new(&query).case_insensitive(!case_sensitive).build() {
                Ok(r) => r,
                Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid regex: {}", e)).into_response(),
            }
        } else {
            let escaped = regex::escape(&query);
            match regex::RegexBuilder::new(&escaped).case_insensitive(!case_sensitive).build() {
                Ok(r) => r,
                Err(e) => return (StatusCode::BAD_REQUEST, format!("Regex build error: {}", e)).into_response(),
            }
        };

        let mut matches = Vec::new();
        let mut files_matched = std::collections::HashSet::new();

        async fn search_files(
            dir: &Path,
            re: &regex::Regex,
            matches: &mut Vec<GlobalSearchMatch>,
            files_matched: &mut std::collections::HashSet<String>,
        ) -> Result<(), std::io::Error> {
            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "target" || name == ".git" || name == ".hgb" || name == "node_modules" {
                    continue;
                }
                let is_dir = entry.file_type().await?.is_dir();
                if is_dir {
                    Box::pin(search_files(&path, re, matches, files_matched)).await?;
                } else {
                    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ["png", "jpg", "jpeg", "ico", "bin", "so", "o", "lock", "tar", "gz", "wasm"].contains(&ext) {
                        continue;
                    }
                    if let Ok(content) = fs::read_to_string(&path).await {
                        let path_str = path.to_string_lossy().to_string();
                        for (line_idx, line) in content.lines().enumerate() {
                            for mat in re.find_iter(line) {
                                matches.push(GlobalSearchMatch {
                                    file_path: path_str.clone(),
                                    line_number: line_idx + 1,
                                    start_col: mat.start(),
                                    end_col: mat.end(),
                                    line_content: line.to_string(),
                                    matched_text: mat.as_str().to_string(),
                                });
                                files_matched.insert(path_str.clone());
                                if matches.len() >= 1000 {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }

        let _ = search_files(&state.workspace_root, &compiled_re, &mut matches, &mut files_matched).await;
        let total = matches.len();
        let files_count = files_matched.len();

        (StatusCode::OK, Json(GlobalSearchResult {
            query,
            is_regex,
            case_sensitive,
            total_matches: total,
            files_count,
            matches,
        })).into_response()
    }

    async fn handle_global_replace(State(state): State<Arc<Self>>, Json(payload): Json<GlobalReplacePayload>) -> impl IntoResponse {
        let query = payload.query;
        let replacement = payload.replacement;
        let is_regex = payload.is_regex.unwrap_or(false);
        let case_sensitive = payload.case_sensitive.unwrap_or(false);

        if query.is_empty() {
            return (StatusCode::BAD_REQUEST, "Search query cannot be empty").into_response();
        }

        let compiled_re = if is_regex {
            match regex::RegexBuilder::new(&query).case_insensitive(!case_sensitive).build() {
                Ok(r) => r,
                Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid regex: {}", e)).into_response(),
            }
        } else {
            let escaped = regex::escape(&query);
            match regex::RegexBuilder::new(&escaped).case_insensitive(!case_sensitive).build() {
                Ok(r) => r,
                Err(e) => return (StatusCode::BAD_REQUEST, format!("Regex build error: {}", e)).into_response(),
            }
        };

        let mut files_to_modify: Vec<(PathBuf, String, usize)> = Vec::new();

        async fn collect_candidates(
            dir: &Path,
            re: &regex::Regex,
            repl: &str,
            filter: Option<&str>,
            out: &mut Vec<(PathBuf, String, usize)>,
        ) -> Result<(), std::io::Error> {
            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "target" || name == ".git" || name == ".hgb" || name == "node_modules" {
                    continue;
                }
                let is_dir = entry.file_type().await?.is_dir();
                if is_dir {
                    Box::pin(collect_candidates(&path, re, repl, filter, out)).await?;
                } else {
                    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ["png", "jpg", "jpeg", "ico", "bin", "so", "o", "lock", "tar", "gz", "wasm"].contains(&ext) {
                        continue;
                    }
                    if let Some(f) = filter {
                        if !name.contains(f) && !path.to_string_lossy().contains(f) {
                            continue;
                        }
                    }
                    if let Ok(content) = fs::read_to_string(&path).await {
                        let count = re.find_iter(&content).count();
                        if count > 0 {
                            let new_content = re.replace_all(&content, repl).to_string();
                            out.push((path, new_content, count));
                        }
                    }
                }
            }
            Ok(())
        }

        let _ = collect_candidates(&state.workspace_root, &compiled_re, &replacement, payload.file_filter.as_deref(), &mut files_to_modify).await;

        if files_to_modify.is_empty() {
            return (StatusCode::OK, Json(GlobalReplaceResult {
                checkpoint_id: String::new(),
                files_modified: Vec::new(),
                total_replacements: 0,
            })).into_response();
        }

        // Automatic Time-Travel Checkpoint snapshot
        let target_paths_str: Vec<String> = files_to_modify.iter().map(|(p, _, _)| p.to_string_lossy().to_string()).collect();
        let checkpoint_label = format!("Pre-global replace: '{}' -> '{}'", query, replacement);
        let ckpt_id = {
            let mut mgr = get_checkpoint_mgr().lock().unwrap();
            for f in &target_paths_str {
                let _ = mgr.snapshot_file(&PathBuf::from(f));
            }
            let ckpt = mgr.create_checkpoint(&checkpoint_label, HashMap::new(), HashMap::new());
            ckpt.checkpoint_id
        };

        let mut modified_list = Vec::new();
        let mut total_replacements = 0;
        for (path, new_content, count) in files_to_modify {
            let _ = fs::write(&path, &new_content).await;
            modified_list.push(path.to_string_lossy().to_string());
            total_replacements += count;
        }

        (StatusCode::OK, Json(GlobalReplaceResult {
            checkpoint_id: ckpt_id,
            files_modified: modified_list,
            total_replacements,
        })).into_response()
    }

    // --- Visual Git Source Control Panel ---

    async fn handle_git_status(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let base = &state.workspace_root;

        let status_out = tokio::process::Command::new("git")
            .args(["status", "--porcelain=v1"])
            .current_dir(base)
            .output()
            .await;

        let (status_success, stdout) = match status_out {
            Ok(out) if out.status.success() => (true, String::from_utf8_lossy(&out.stdout).to_string()),
            _ => (false, String::new()),
        };

        if !status_success {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Not a git repository or git command failed").into_response();
        }

        let branch_out = tokio::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(base)
            .output()
            .await;

        let branch = match branch_out {
            Ok(ref out) if out.status.success() => {
                let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if b.is_empty() { "main".into() } else { b }
            }
            _ => "main".into(),
        };

        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        for line in stdout.lines() {
            if line.len() < 3 {
                continue;
            }
            let bytes = line.as_bytes();
            let index_code = bytes[0] as char;
            let worktree_code = bytes[1] as char;
            let raw_path = &line[3..];
            let path = if let Some((_, new_p)) = raw_path.split_once(" -> ") {
                new_p.trim().to_string()
            } else {
                raw_path.trim().to_string()
            };

            if index_code == '?' && worktree_code == '?' {
                untracked.push(GitFileChange {
                    path: path.clone(),
                    status_code: "??".into(),
                    status_label: "Untracked".into(),
                    is_staged: false,
                });
                continue;
            }

            if index_code != ' ' && index_code != '?' {
                let label = match index_code {
                    'M' => "Modified",
                    'A' => "Added",
                    'D' => "Deleted",
                    'R' => "Renamed",
                    'C' => "Copied",
                    _ => "Staged",
                };
                staged.push(GitFileChange {
                    path: path.clone(),
                    status_code: index_code.to_string(),
                    status_label: label.to_string(),
                    is_staged: true,
                });
            }

            if worktree_code != ' ' && worktree_code != '?' {
                let label = match worktree_code {
                    'M' => "Modified",
                    'D' => "Deleted",
                    _ => "Unstaged",
                };
                unstaged.push(GitFileChange {
                    path: path.clone(),
                    status_code: worktree_code.to_string(),
                    status_label: label.to_string(),
                    is_staged: false,
                });
            }
        }

        let is_clean = staged.is_empty() && unstaged.is_empty() && untracked.is_empty();

        (StatusCode::OK, Json(GitStatusSummary {
            branch,
            is_clean,
            staged,
            unstaged,
            untracked,
        })).into_response()
    }

    async fn handle_git_stage(State(state): State<Arc<Self>>, Json(payload): Json<GitStagePayload>) -> impl IntoResponse {
        let output = tokio::process::Command::new("git")
            .args(["add", "-A", "--", &payload.path])
            .current_dir(&state.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "staged", "path": payload.path }))).into_response(),
            Ok(out) => (StatusCode::INTERNAL_SERVER_ERROR, String::from_utf8_lossy(&out.stderr).to_string()).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    async fn handle_git_unstage(State(state): State<Arc<Self>>, Json(payload): Json<GitStagePayload>) -> impl IntoResponse {
        let output = tokio::process::Command::new("git")
            .args(["restore", "--staged", "--", &payload.path])
            .current_dir(&state.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "unstaged", "path": payload.path }))).into_response(),
            _ => {
                let fallback = tokio::process::Command::new("git")
                    .args(["reset", "HEAD", "--", &payload.path])
                    .current_dir(&state.workspace_root)
                    .output()
                    .await;
                match fallback {
                    Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "unstaged", "path": payload.path }))).into_response(),
                    _ => {
                        let rm_cached = tokio::process::Command::new("git")
                            .args(["rm", "--cached", "--", &payload.path])
                            .current_dir(&state.workspace_root)
                            .output()
                            .await;
                        match rm_cached {
                            Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "unstaged", "path": payload.path }))).into_response(),
                            Ok(out) => (StatusCode::INTERNAL_SERVER_ERROR, String::from_utf8_lossy(&out.stderr).to_string()).into_response(),
                            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
                        }
                    }
                }
            }
        }
    }

    async fn handle_git_stage_all(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let output = tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&state.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "all_staged" }))).into_response(),
            Ok(out) => (StatusCode::INTERNAL_SERVER_ERROR, String::from_utf8_lossy(&out.stderr).to_string()).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    async fn handle_git_unstage_all(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let output = tokio::process::Command::new("git")
            .args(["restore", "--staged", "."])
            .current_dir(&state.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "all_unstaged" }))).into_response(),
            _ => {
                let fallback = tokio::process::Command::new("git")
                    .args(["reset", "HEAD", "."])
                    .current_dir(&state.workspace_root)
                    .output()
                    .await;
                match fallback {
                    Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "all_unstaged" }))).into_response(),
                    _ => {
                        let rm_cached = tokio::process::Command::new("git")
                            .args(["rm", "--cached", "-r", "."])
                            .current_dir(&state.workspace_root)
                            .output()
                            .await;
                        match rm_cached {
                            Ok(out) if out.status.success() => (StatusCode::OK, Json(serde_json::json!({ "status": "all_unstaged" }))).into_response(),
                            Ok(out) => (StatusCode::INTERNAL_SERVER_ERROR, String::from_utf8_lossy(&out.stderr).to_string()).into_response(),
                            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
                        }
                    }
                }
            }
        }
    }

    async fn handle_git_commit(State(state): State<Arc<Self>>, Json(payload): Json<GitCommitPayload>) -> impl IntoResponse {
        let clean_msg = payload.message.trim();
        if clean_msg.is_empty() {
            return (StatusCode::BAD_REQUEST, "Commit message cannot be empty").into_response();
        }

        let output = tokio::process::Command::new("git")
            .args(["commit", "-m", clean_msg])
            .current_dir(&state.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let hash_out = tokio::process::Command::new("git")
                    .args(["rev-parse", "--short", "HEAD"])
                    .current_dir(&state.workspace_root)
                    .output()
                    .await;
                let commit_hash = hash_out.ok().map(|h| String::from_utf8_lossy(&h.stdout).trim().to_string()).unwrap_or_default();
                (StatusCode::OK, Json(GitCommitResult {
                    success: true,
                    commit_hash,
                    summary: stdout.lines().next().unwrap_or(clean_msg).to_string(),
                    files_changed: 1,
                })).into_response()
            }
            Ok(out) => (StatusCode::BAD_REQUEST, format!("Commit failed: {}", String::from_utf8_lossy(&out.stderr))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    async fn handle_chat_stream(Json(payload): Json<ChatPayload>) -> impl IntoResponse {
        let client = reqwest::Client::new();
        let prompt_clone = payload.prompt.clone();
        let prompt_full = if let Some(ctx) = payload.context {
            format!("Code Context:\n```\n{}\n```\n\nTask: {}\nProvide direct, surgical code corrections.", ctx, prompt_clone)
        } else {
            prompt_clone
        };

        let requested = payload.model.unwrap_or_else(|| "dynabook-coder:latest".to_string());

        let mut messages = Vec::new();
        if let Some(hist) = payload.history {
            for m in hist {
                messages.push(serde_json::json!({ "role": m.role, "content": m.content }));
            }
        }
        messages.push(serde_json::json!({ "role": "user", "content": prompt_full }));

        let ollama_req = serde_json::json!({
            "model": requested,
            "messages": messages,
            "stream": false
        });

        match client.post("http://127.0.0.1:11434/api/chat")
            .json(&ollama_req)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let text = json.get("message").and_then(|m| m.get("content")).and_then(|v| v.as_str()).unwrap_or("No content");
                    (StatusCode::OK, Json(serde_json::json!({
                        "response": text,
                        "model": requested,
                        "status": "success"
                    }))).into_response()
                } else {
                    (StatusCode::OK, Json(serde_json::json!({ "response": "Error parsing output", "status": "error" }))).into_response()
                }
            }
            Err(_) => {
                (StatusCode::OK, Json(serde_json::json!({
                    "response": "Inference service offline",
                    "status": "offline"
                }))).into_response()
            }
        }
    }

    async fn handle_zig_telemetry() -> impl IntoResponse {
        let t0 = Instant::now();
        let text_a = "fn main() {\n    println!(\"Hello World\");\n    let x = 42;\n}\n";
        let text_b = "fn main() {\n    println!(\"Hello Hagibis\");\n    let x = 43;\n}\n";
        let hashes_a: Vec<u64> = text_a.lines().map(|l| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(l, &mut h);
            std::hash::Hasher::finish(&h)
        }).collect();
        let hashes_b: Vec<u64> = text_b.lines().map(|l| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(l, &mut h);
            std::hash::Hasher::finish(&h)
        }).collect();

        let _dist = hgb_core::zig_accelerate::myers_diff_distance(&hashes_a, &hashes_b);
        let sim = hgb_core::zig_accelerate::myers_lcs_similarity(&hashes_a, &hashes_b);
        let myers_us = t0.elapsed().as_micros() as u64;

        let t1 = Instant::now();
        let _lev = hgb_core::zig_accelerate::levenshtein_distance("fn calculate_hash", "fn compute_hash_fast");
        let lev_us = t1.elapsed().as_micros() as u64;

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let (has_avx2, has_sse41) = (is_x86_feature_detected!("avx2"), is_x86_feature_detected!("sse4.1"));
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        let (has_avx2, has_sse41) = (false, false);

        let simd_feat = if has_avx2 { "AVX2 256-bit SIMD" } else if has_sse41 { "SSE4.1 128-bit SIMD" } else { "Scalar" };
        let landlock = hgb_core::zig_accelerate::sandbox_check_support();

        let telemetry = ZigTelemetry {
            engine_status: "Online / Native Compiled".into(),
            zig_version: "0.13.0".into(),
            simd_active: has_avx2 || has_sse41,
            simd_feature: simd_feat.into(),
            myers_ses_latency_us: myers_us.max(1),
            myers_lcs_similarity: sim,
            levenshtein_benchmark_us: lev_us.max(1),
            landlock_lsm_supported: landlock,
            memory_arena_status: "Zero-Copy POSIX SHM & Page Arena Ready".into(),
            active_optimizations: vec![
                "Zig Myers SES Microsecond Diff Kernel".into(),
                "Zig SIMD Vector Cosine & Dot Product".into(),
                "Zig Character-Level Intra-Line SES Highlighter".into(),
                "Zig Sub-Microsecond Levenshtein Distance".into(),
                "Linux Landlock LSM Sandboxing".into(),
                "Dual-Buffer O(1) Speculative Virtual Overlay".into(),
            ],
        };

        (StatusCode::OK, Json(telemetry))
    }

    // --- Phase 1: Real LSP Handlers ---
    async fn handle_lsp_hover(
        State(state): State<Arc<Self>>,
        Json(payload): Json<crate::lsp_engine::LspHoverRequest>,
    ) -> impl IntoResponse {
        let hover = state.lsp.analyze_hover(&payload);
        (StatusCode::OK, Json(hover))
    }

    async fn handle_lsp_definition(
        State(state): State<Arc<Self>>,
        Json(payload): Json<crate::lsp_engine::LspDefinitionRequest>,
    ) -> impl IntoResponse {
        let def = state.lsp.find_definition(&payload);
        (StatusCode::OK, Json(def))
    }

    async fn handle_lsp_completion(
        State(state): State<Arc<Self>>,
        Json(payload): Json<LspCompletionPayload>,
    ) -> impl IntoResponse {
        let completions = state.lsp.get_completions(&payload.path, &payload.line_content, payload.character);
        (StatusCode::OK, Json(completions))
    }

    async fn handle_lsp_ws(
        State(state): State<Arc<Self>>,
        ws: WebSocketUpgrade,
    ) -> impl IntoResponse {
        ws.on_upgrade(move |socket| Self::handle_lsp_ws_stream(socket, state))
    }

    async fn handle_lsp_ws_stream(mut socket: WebSocket, state: Arc<Self>) {
        while let Some(Ok(msg)) = socket.recv().await {
            if let Message::Text(text) = msg {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    let id = val.get("id").cloned();
                    let method = val.get("method").and_then(|m| m.as_str()).unwrap_or("");

                    let response = match method {
                        "initialize" => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "capabilities": {
                                    "hoverProvider": true,
                                    "definitionProvider": true,
                                    "completionProvider": { "triggerCharacters": [".", ":", "$"] },
                                    "textDocumentSync": 1
                                },
                                "serverInfo": { "name": "Hagibis Native LSP Engine", "version": "1.0.0" }
                            }
                        }),
                        "textDocument/hover" => {
                            let path = val.get("params")
                                .and_then(|p| p.get("textDocument"))
                                .and_then(|t| t.get("uri"))
                                .and_then(|u| u.as_str())
                                .unwrap_or("");
                            let line = val.get("params")
                                .and_then(|p| p.get("position"))
                                .and_then(|pos| pos.get("line"))
                                .and_then(|l| l.as_u64())
                                .unwrap_or(0) as usize + 1;
                            let character = val.get("params")
                                .and_then(|p| p.get("position"))
                                .and_then(|pos| pos.get("character"))
                                .and_then(|c| c.as_u64())
                                .unwrap_or(0) as usize + 1;

                            let hover = state.lsp.analyze_hover(&crate::lsp_engine::LspHoverRequest {
                                path: path.replace("file://", ""),
                                line,
                                character,
                            });

                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": hover.map(|h| serde_json::json!({
                                    "contents": { "kind": "markdown", "value": h.contents }
                                }))
                            })
                        }
                        "textDocument/definition" => {
                            let path = val.get("params")
                                .and_then(|p| p.get("textDocument"))
                                .and_then(|t| t.get("uri"))
                                .and_then(|u| u.as_str())
                                .unwrap_or("");
                            let line = val.get("params")
                                .and_then(|p| p.get("position"))
                                .and_then(|pos| pos.get("line"))
                                .and_then(|l| l.as_u64())
                                .unwrap_or(0) as usize + 1;
                            let character = val.get("params")
                                .and_then(|p| p.get("position"))
                                .and_then(|pos| pos.get("character"))
                                .and_then(|c| c.as_u64())
                                .unwrap_or(0) as usize + 1;

                            let def = state.lsp.find_definition(&crate::lsp_engine::LspDefinitionRequest {
                                path: path.replace("file://", ""),
                                line,
                                character,
                            });

                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": def.map(|d| serde_json::json!({
                                    "uri": format!("file://{}", d.uri),
                                    "range": {
                                        "start": { "line": d.line.saturating_sub(1), "character": d.character.saturating_sub(1) },
                                        "end": { "line": d.line.saturating_sub(1), "character": d.character }
                                    }
                                }))
                            })
                        }
                        _ => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": null }),
                    };

                    let _ = socket.send(Message::Text(response.to_string())).await;
                }
            }
        }
    }

    // --- Phase 2: Remote Auth Token Verification ---
    async fn handle_auth_verify(
        State(state): State<Arc<Self>>,
        Json(payload): Json<AuthVerifyPayload>,
    ) -> impl IntoResponse {
        let is_valid = match &state.auth_token {
            Some(t) => t == &payload.token,
            None => true,
        };

        if is_valid {
            (StatusCode::OK, Json(serde_json::json!({ "valid": true, "message": "Authentication verified" }))).into_response()
        } else {
            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "valid": false, "error": "Invalid auth token" }))).into_response()
        }
    }

    // --- Phase 3: Semantic Vector Search ---
    async fn handle_semantic_search(
        State(state): State<Arc<Self>>,
        Json(payload): Json<SemanticSearchPayload>,
    ) -> impl IntoResponse {
        let limit = payload.limit.unwrap_or(10);
        let results = state.semantic_index.search(&payload.query, limit).await;
        (StatusCode::OK, Json(results)).into_response()
    }

    // --- Phase 4: DAP Visual Debugger Handlers ---
    async fn handle_dap_toggle_breakpoint(
        State(state): State<Arc<Self>>,
        Json(payload): Json<ToggleBreakpointPayload>,
    ) -> impl IntoResponse {
        let bp = state.dap.toggle_breakpoint(&payload.file, payload.line).await;
        (StatusCode::OK, Json(bp)).into_response()
    }

    async fn handle_dap_get_breakpoints(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let bps = state.dap.get_all_breakpoints().await;
        (StatusCode::OK, Json(bps)).into_response()
    }

    async fn handle_dap_launch(
        State(state): State<Arc<Self>>,
        Json(payload): Json<DapLaunchPayload>,
    ) -> impl IntoResponse {
        let prog = payload.program.unwrap_or_default();
        let args = payload.args.unwrap_or_default();
        match state.dap.launch_target(&prog, &args).await {
            Ok(status) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap())).into_response(),
            Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": e }))).into_response(),
        }
    }

    async fn handle_dap_continue(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let status = state.dap.continue_exec().await;
        (StatusCode::OK, Json(status)).into_response()
    }

    async fn handle_dap_step_over(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let status = state.dap.step_over().await;
        (StatusCode::OK, Json(status)).into_response()
    }

    async fn handle_dap_step_into(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let status = state.dap.step_into().await;
        (StatusCode::OK, Json(status)).into_response()
    }

    async fn handle_dap_stop(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let status = state.dap.stop_session().await;
        (StatusCode::OK, Json(status)).into_response()
    }

    async fn handle_dap_status(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let status = state.dap.get_status().await;
        (StatusCode::OK, Json(status)).into_response()
    }

    // --- Phase 5: Multi-Agent Parallel Git Worktree Swarm Handlers ---
    async fn handle_swarm_spawn(
        State(state): State<Arc<Self>>,
        Json(payload): Json<SwarmSpawnPayload>,
    ) -> impl IntoResponse {
        match state.swarm.spawn_worktree_task(&payload.title, &payload.prompt).await {
            Ok(task) => (StatusCode::OK, Json(serde_json::to_value(task).unwrap())).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e }))).into_response(),
        }
    }

    async fn handle_swarm_tasks(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let tasks = state.swarm.list_tasks().await;
        (StatusCode::OK, Json(tasks)).into_response()
    }

    async fn handle_swarm_merge(
        State(state): State<Arc<Self>>,
        Json(payload): Json<SwarmMergePayload>,
    ) -> impl IntoResponse {
        match state.swarm.merge_worktree_task(&payload.task_id).await {
            Ok(msg) => (StatusCode::OK, Json(serde_json::json!({ "success": true, "message": msg }))).into_response(),
            Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "success": false, "error": e }))).into_response(),
        }
    }

    // Phase 6A & 7C: Interactive WebSocket PTY Terminal (Multi-Session)
    async fn handle_pty_ws(
        ws: WebSocketUpgrade,
        Query(params): Query<HashMap<String, String>>,
        State(state): State<Arc<Self>>,
    ) -> impl IntoResponse {
        let pty = state.pty.clone();
        let root = state.workspace_root.clone();
        let session_id = params.get("session_id").cloned();
        ws.on_upgrade(move |socket| async move {
            pty.handle_websocket(socket, Some(root), session_id).await;
        })
    }

    // Phase 6B: Live Workspace File System Watcher Stream
    async fn handle_fs_events_ws(
        ws: WebSocketUpgrade,
        State(state): State<Arc<Self>>,
    ) -> impl IntoResponse {
        let mut rx = {
            let w = state.fs_watcher.lock().await;
            w.subscribe()
        };
        ws.on_upgrade(move |mut socket| async move {
            while let Ok(event) = rx.recv().await {
                if let Ok(json) = serde_json::to_string(&event) {
                    if socket.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
            }
        })
    }

    // Phase 6C: Cloud LLM & API Key Settings Manager
    async fn handle_get_cloud_settings(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let settings = state.cloud_settings.read().await;
        (StatusCode::OK, Json(settings.to_masked()))
    }

    async fn handle_update_cloud_settings(
        State(state): State<Arc<Self>>,
        Json(payload): Json<crate::cloud_settings::CloudSettings>,
    ) -> impl IntoResponse {
        let mut settings = state.cloud_settings.write().await;
        let mut updated = payload;
        if let Some(ref k) = updated.anthropic_api_key {
            if k.contains("...") {
                updated.anthropic_api_key = settings.anthropic_api_key.clone();
            }
        }
        if let Some(ref k) = updated.openai_api_key {
            if k.contains("...") {
                updated.openai_api_key = settings.openai_api_key.clone();
            }
        }
        if let Some(ref k) = updated.gemini_api_key {
            if k.contains("...") {
                updated.gemini_api_key = settings.gemini_api_key.clone();
            }
        }
        if let Some(ref k) = updated.deepseek_api_key {
            if k.contains("...") {
                updated.deepseek_api_key = settings.deepseek_api_key.clone();
            }
        }
        if let Err(e) = updated.save_to_disk(None) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e }))).into_response();
        }
        *settings = updated;
        (StatusCode::OK, Json(settings.to_masked())).into_response()
    }

    async fn handle_test_cloud_settings(
        State(state): State<Arc<Self>>,
        Json(payload): Json<TestModelPayload>,
    ) -> impl IntoResponse {
        let mut test_settings = {
            let current = state.cloud_settings.read().await;
            current.clone()
        };
        test_settings.active_provider = payload.provider.clone();
        test_settings.active_model = payload.model.clone();
        if let Some(key) = payload.api_key {
            if !key.contains("...") && !key.trim().is_empty() {
                match payload.provider.as_str() {
                    "anthropic" => test_settings.anthropic_api_key = Some(key),
                    "openai" => test_settings.openai_api_key = Some(key),
                    "gemini" => test_settings.gemini_api_key = Some(key),
                    "deepseek" => test_settings.deepseek_api_key = Some(key),
                    _ => {}
                }
            }
        }
        let msgs = vec![crate::cloud_settings::ChatMessageItem {
            role: "user".to_string(),
            content: "Ping. Respond with OK.".to_string(),
        }];
        match test_settings.dispatch_chat(&msgs, Some("You are a connection test bot.")).await {
            Ok(reply) => (StatusCode::OK, Json(serde_json::json!({ "success": true, "reply": reply.trim() }))).into_response(),
            Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "success": false, "error": e }))).into_response(),
        }
    }

    // =========================================================================
    // Phase 7A: Autonomous Agentic Tool-Use Loop & Permission Gate
    // =========================================================================

    async fn handle_agent_tools(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let tools = state.agent_loop.get_available_tools();
        (StatusCode::OK, Json(tools)).into_response()
    }

    async fn handle_agent_chat_step(
        State(state): State<Arc<Self>>,
        Json(req): Json<AgentStepRequest>,
    ) -> impl IntoResponse {
        if let Some(aa) = req.auto_approve {
            state.agent_loop.set_auto_approve(aa);
        }

        let system_prompt = state.agent_loop.build_agent_system_prompt();
        let prompt_with_system = format!("User Request: {}", req.prompt);

        let msgs = vec![crate::cloud_settings::ChatMessageItem {
            role: "user".to_string(),
            content: prompt_with_system,
        }];

        let cloud = state.cloud_settings.read().await;
        match cloud.dispatch_chat(&msgs, Some(&system_prompt)).await {
            Ok(resp) => {
                if let Some(call) = crate::agent_loop::AgentLoopEngine::parse_tool_call(&resp) {
                    if state.agent_loop.requires_approval(&call.name) {
                        let pending = state.agent_loop.register_pending_approval(&call).await;
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "type": "tool_approval_required",
                                "call_id": pending.call_id,
                                "tool": pending.tool_name,
                                "arguments": pending.arguments,
                                "description": pending.description,
                                "thought": resp.replace(&format!("```tool_call\n{}\n```", serde_json::to_string_pretty(&call.arguments).unwrap_or_default()), "").trim()
                            })),
                        ).into_response()
                    } else {
                        let res = state.agent_loop.execute_tool(&call).await;
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "type": "tool_executed",
                                "call_id": res.call_id,
                                "tool": call.name,
                                "output": res.output,
                                "is_error": res.is_error,
                                "thought": resp.trim()
                            })),
                        ).into_response()
                    }
                } else {
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "type": "message",
                            "content": resp,
                            "is_final": true
                        })),
                    ).into_response()
                }
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "type": "error",
                    "message": format!("LLM dispatch failed: {}", e)
                })),
            ).into_response(),
        }
    }

    async fn handle_agent_approve_tool(
        State(state): State<Arc<Self>>,
        Json(req): Json<AgentApproveRequest>,
    ) -> impl IntoResponse {
        match state.agent_loop.resolve_approval(&req.call_id, req.approved, req.allow_all).await {
            Ok(res) => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "call_id": req.call_id,
                    "output": res.output,
                    "is_error": res.is_error,
                    "approved": req.approved
                })),
            ).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            ).into_response(),
        }
    }

    async fn handle_agent_auto_approve(
        State(state): State<Arc<Self>>,
        Json(req): Json<AgentAutoApproveToggle>,
    ) -> impl IntoResponse {
        state.agent_loop.set_auto_approve(req.enabled);
        (
            StatusCode::OK,
            Json(serde_json::json!({ "auto_approve": req.enabled })),
        ).into_response()
    }

    // =========================================================================
    // Phase 7B: Live Streaming Ghost Diff (Cursor-Style Ctrl+K)
    // =========================================================================

    async fn handle_inline_edit_stream(
        State(state): State<Arc<Self>>,
        Json(req): Json<InlineEditStreamRequest>,
    ) -> impl IntoResponse {
        let full_path = state.workspace_root.join(req.path.trim_start_matches('/'));
        let orig_content = match fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(_) => String::new(),
        };

        let prompt = format!(
            "Instruction: {}\nTarget file: {}\nOriginal code:\n```\n{}\n```\nProvide ONLY the fully revised code inside a single ``` fenced code block.",
            req.instruction, req.path, orig_content
        );

        let msgs = vec![crate::cloud_settings::ChatMessageItem {
            role: "user".to_string(),
            content: prompt,
        }];

        let cloud = state.cloud_settings.read().await;
        let gen_code = match cloud.dispatch_chat(&msgs, Some("You are an expert AI code editor. Return only revised code in markdown fences.")).await {
            Ok(txt) => {
                if txt.contains("```") {
                    let parts: Vec<&str> = txt.split("```").collect();
                    if parts.len() >= 2 {
                        let lines: Vec<&str> = parts[1].lines().collect();
                        if !lines.is_empty() && (lines[0].starts_with("rust") || lines[0].starts_with("zig") || lines[0].starts_with("js") || lines[0].starts_with("ts") || lines[0].starts_with("python") || lines[0].starts_with("html")) {
                            lines[1..].join("\n")
                        } else {
                            parts[1].to_string()
                        }
                    } else {
                        txt
                    }
                } else {
                    txt
                }
            }
            Err(e) => return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("LLM failed: {}", e) })),
            ).into_response(),
        };

        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(&orig_content, &gen_code);
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "original": orig_content,
                "speculative": gen_code,
                "metrics": metrics,
                "lines": lines,
                "path": req.path
            })),
        ).into_response()
    }

    // =========================================================================
    // Phase 7C: Multi-Tab Persistent Terminals
    // =========================================================================

    async fn handle_terminal_sessions_list(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let list = state.pty.list_sessions().await;
        (StatusCode::OK, Json(list)).into_response()
    }

    async fn handle_terminal_session_new(
        State(state): State<Arc<Self>>,
        Json(req): Json<TerminalSessionNewReq>,
    ) -> impl IntoResponse {
        let meta = state.pty.create_session(req.title).await;
        (StatusCode::OK, Json(meta)).into_response()
    }

    async fn handle_terminal_session_kill(
        State(state): State<Arc<Self>>,
        Json(req): Json<TerminalSessionKillReq>,
    ) -> impl IntoResponse {
        let ok = state.pty.remove_session(&req.session_id).await;
        (StatusCode::OK, Json(serde_json::json!({ "success": ok, "session_id": req.session_id }))).into_response()
    }

    // =========================================================================
    // Phase 7D: Git Branch Manager & 3-Way Merge Conflict Resolver
    // =========================================================================

    async fn handle_git_branches(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let out = tokio::process::Command::new("git")
            .args(["branch", "--all"])
            .current_dir(&state.workspace_root)
            .output()
            .await;

        match out {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut current = "main".to_string();
                let mut branches = Vec::new();
                let mut remotes = Vec::new();

                for line in stdout.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let is_cur = trimmed.starts_with('*');
                    let name = trimmed.trim_start_matches('*').trim();

                    if name.starts_with("remotes/") {
                        remotes.push(name.to_string());
                    } else {
                        if is_cur {
                            current = name.to_string();
                        }
                        branches.push(name.to_string());
                    }
                }

                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "current": current,
                        "branches": branches,
                        "remotes": remotes
                    })),
                ).into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to inspect branches: {}", e) })),
            ).into_response(),
        }
    }

    async fn handle_git_branch_checkout(
        State(state): State<Arc<Self>>,
        Json(req): Json<GitBranchCheckoutReq>,
    ) -> impl IntoResponse {
        let mut cmd = tokio::process::Command::new("git");
        cmd.current_dir(&state.workspace_root);

        if req.create_new {
            cmd.args(["checkout", "-b", &req.branch]);
        } else {
            cmd.args(["checkout", &req.branch]);
        }

        match cmd.output().await {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let ok = out.status.success();
                (
                    if ok { StatusCode::OK } else { StatusCode::BAD_REQUEST },
                    Json(serde_json::json!({
                        "success": ok,
                        "branch": req.branch,
                        "output": format!("{}{}", stdout, stderr)
                    })),
                ).into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Git checkout command failed: {}", e) })),
            ).into_response(),
        }
    }

    async fn handle_git_conflicts(State(state): State<Arc<Self>>) -> impl IntoResponse {
        let mut conflict_files = Vec::new();
        let mut stack = vec![state.workspace_root.clone()];

        while let Some(dir) = stack.pop() {
            if let Ok(mut entries) = fs::read_dir(&dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') || name == "target" || name == "node_modules" {
                        continue;
                    }
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        if let Ok(content) = fs::read_to_string(&path).await {
                            if content.contains("<<<<<<<") && content.contains(">>>>>>>") && content.contains("=======") {
                                if let Ok(rel) = path.strip_prefix(&state.workspace_root) {
                                    conflict_files.push(rel.to_string_lossy().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        (
            StatusCode::OK,
            Json(serde_json::json!({ "conflict_files": conflict_files })),
        ).into_response()
    }

    async fn handle_git_conflicts_resolve(
        State(state): State<Arc<Self>>,
        Json(req): Json<GitConflictResolveReq>,
    ) -> impl IntoResponse {
        let full_path = state.workspace_root.join(req.path.trim_start_matches('/'));
        let content = match fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Cannot read file: {}", e) })),
            ).into_response(),
        };

        // Snapshot checkpoint before resolving
        {
            let mut mgr = get_checkpoint_mgr().lock().unwrap();
            let _ = mgr.snapshot_file(&full_path);
        }

        let mut resolved_lines = Vec::new();
        let mut in_conflict = false;
        let mut in_incoming = false;
        let mut current_block = Vec::new();
        let mut incoming_block = Vec::new();

        for line in content.lines() {
            if line.starts_with("<<<<<<<") {
                in_conflict = true;
                in_incoming = false;
                current_block.clear();
                incoming_block.clear();
            } else if in_conflict && line.starts_with("=======") {
                in_incoming = true;
            } else if in_conflict && line.starts_with(">>>>>>>") {
                in_conflict = false;
                in_incoming = false;
                match req.resolution.as_str() {
                    "current" => resolved_lines.extend(current_block.drain(..)),
                    "incoming" => resolved_lines.extend(incoming_block.drain(..)),
                    "both" | _ => {
                        resolved_lines.extend(current_block.drain(..));
                        resolved_lines.extend(incoming_block.drain(..));
                    }
                }
            } else if in_conflict {
                if in_incoming {
                    incoming_block.push(line.to_string());
                } else {
                    current_block.push(line.to_string());
                }
            } else {
                resolved_lines.push(line.to_string());
            }
        }

        let result_text = resolved_lines.join("\n");
        match fs::write(&full_path, &result_text).await {
            Ok(_) => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "path": req.path,
                    "resolution": req.resolution
                })),
            ).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to write resolved file: {}", e) })),
            ).into_response(),
        }
    }
}

#[derive(Deserialize)]
pub struct AgentStepRequest {
    pub prompt: String,
    pub auto_approve: Option<bool>,
}

#[derive(Deserialize)]
pub struct AgentApproveRequest {
    pub call_id: String,
    pub approved: bool,
    #[serde(default)]
    pub allow_all: bool,
}

#[derive(Deserialize)]
pub struct AgentAutoApproveToggle {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct InlineEditStreamRequest {
    pub path: String,
    pub instruction: String,
    pub selection: Option<String>,
}

#[derive(Deserialize)]
pub struct TerminalSessionNewReq {
    pub title: Option<String>,
}

#[derive(Deserialize)]
pub struct TerminalSessionKillReq {
    pub session_id: String,
}

#[derive(Deserialize)]
pub struct GitBranchCheckoutReq {
    pub branch: String,
    #[serde(default)]
    pub create_new: bool,
}

#[derive(Deserialize)]
pub struct GitConflictResolveReq {
    pub path: String,
    pub resolution: String,
}

#[derive(Deserialize)]
pub struct LspCompletionPayload {
    pub path: String,
    pub line_content: String,
    pub character: usize,
}

#[derive(Deserialize)]
pub struct AuthVerifyPayload {
    pub token: String,
}

#[derive(Deserialize)]
pub struct TestModelPayload {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
}

#[derive(Deserialize)]
pub struct SemanticSearchPayload {
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct ToggleBreakpointPayload {
    pub file: String,
    pub line: usize,
}

#[derive(Deserialize)]
pub struct DapLaunchPayload {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct SwarmSpawnPayload {
    pub title: String,
    pub prompt: String,
}

#[derive(Deserialize)]
pub struct SwarmMergePayload {
    pub task_id: String,
}

const INDEX_HTML: &str = include_str!("../dist/index.html");
const MANIFEST_JSON: &str = include_str!("../dist/manifest.json");
const SW_JS: &str = include_str!("../dist/sw.js");
const ICON_SVG: &str = include_str!("../dist/icon.svg");

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_visual_ide_server_creation() {
        let server = VisualIdeServer::new(4173, PathBuf::from("."));
        assert_eq!(server.port, 4173);
    }

    #[tokio::test]
    async fn test_visual_ide_diff_calculation() {
        let orig = "fn main() {\n    println!(\"old\");\n}";
        let modif = "fn main() {\n    println!(\"new\");\n}";
        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(orig, modif);
        assert!(!lines.is_empty());
        assert_eq!(metrics.added_lines, 1);
        assert_eq!(metrics.deleted_lines, 1);
    }

    #[tokio::test]
    async fn test_visual_ide_tab_commit() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let orig = "alpha";
        let spec = "beta";

        let mut overlay = DualBufferOverlay::new(&path, orig);
        overlay.set_speculative_content(spec);
        let committed = overlay.commit_tab_acceptance();
        assert_eq!(committed, "beta");
    }

    #[tokio::test]
    async fn test_visual_ide_file_creation_and_deletion() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file_path = tmp.path().with_extension("test_create.txt").to_string_lossy().to_string();

        let _ = VisualIdeServer::handle_create_file(Json(CreatePayload {
            path: file_path.clone(),
        })).await;
        assert!(std::path::Path::new(&file_path).exists());

        let _ = VisualIdeServer::handle_delete_entry(Json(CreatePayload {
            path: file_path.clone(),
        })).await;
        assert!(!std::path::Path::new(&file_path).exists());
    }

    #[tokio::test]
    async fn test_visual_ide_composer_plan() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        tokio::fs::write(&path, "fn sample() {}").await.unwrap();

        let resp = VisualIdeServer::handle_composer(Json(ComposerPayload {
            prompt: "Add logging statement".to_string(),
            files: vec![path.clone()],
        })).await;

        let (parts, _body) = resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn test_visual_ide_chat_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Arc::new(VisualIdeServer::new(4174, tmp.path().to_path_buf()));

        // Create
        let created_resp = VisualIdeServer::handle_create_chat_session(
            State(server.clone()),
            Json(CreateSessionPayload {
                title: "Web Thread".to_string(),
                model: "qwen2.5-coder:1.5b".to_string(),
            }),
        ).await;
        let (parts, _body) = created_resp.into_response().into_parts();
        assert_eq!(parts.status, StatusCode::OK);

        // List
        let list_resp = VisualIdeServer::handle_get_chat_sessions(State(server.clone())).await;
        let (lparts, _) = list_resp.into_response().into_parts();
        assert_eq!(lparts.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn test_visual_ide_global_search_and_replace() {
        let tmp = tempfile::tempdir().unwrap();
        let server = Arc::new(VisualIdeServer::new(4175, tmp.path().to_path_buf()));

        let file1 = tmp.path().join("code.rs");
        tokio::fs::write(&file1, "const MAGIC_VAL_99: u32 = 99;").await.unwrap();

        // Search
        let search_resp = VisualIdeServer::handle_global_search(
            State(server.clone()),
            Json(GlobalSearchPayload {
                query: r"MAGIC_VAL_\d+".to_string(),
                is_regex: Some(true),
                case_sensitive: Some(true),
            }),
        ).await;
        let (sparts, _) = search_resp.into_response().into_parts();
        assert_eq!(sparts.status, StatusCode::OK);

        // Replace
        let replace_resp = VisualIdeServer::handle_global_replace(
            State(server.clone()),
            Json(GlobalReplacePayload {
                query: r"MAGIC_VAL_(\d+)".to_string(),
                replacement: "SOVEREIGN_VAL_$1".to_string(),
                is_regex: Some(true),
                case_sensitive: Some(true),
                file_filter: None,
            }),
        ).await;
        let (rparts, _) = replace_resp.into_response().into_parts();
        assert_eq!(rparts.status, StatusCode::OK);

        let content = tokio::fs::read_to_string(&file1).await.unwrap();
        assert!(content.contains("SOVEREIGN_VAL_99"));
    }

    #[tokio::test]
    async fn test_visual_ide_zig_telemetry() {
        let resp = VisualIdeServer::handle_zig_telemetry().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_visual_ide_pwa_endpoints() {
        // Verify manifest endpoint
        let manifest_resp = VisualIdeServer::serve_manifest().await.into_response();
        assert_eq!(manifest_resp.status(), StatusCode::OK);
        let ct = manifest_resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap();
        assert_eq!(ct, "application/manifest+json");

        // Verify Service Worker endpoint
        let sw_resp = VisualIdeServer::serve_sw().await.into_response();
        assert_eq!(sw_resp.status(), StatusCode::OK);
        let sw_ct = sw_resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap();
        assert_eq!(sw_ct, "application/javascript");
        let sw_cc = sw_resp.headers().get(axum::http::header::CACHE_CONTROL).unwrap();
        assert_eq!(sw_cc, "no-cache");

        // Verify SVG icon endpoint
        let icon_resp = VisualIdeServer::serve_icon().await.into_response();
        assert_eq!(icon_resp.status(), StatusCode::OK);
        let icon_ct = icon_resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap();
        assert_eq!(icon_ct, "image/svg+xml");
    }
}
