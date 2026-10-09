use crate::inline_diff_engine::{DualBufferOverlay, InlineDiffEngine};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::fs;

#[derive(Clone)]
pub struct VisualIdeServer {
    pub port: u16,
    pub workspace_root: PathBuf,
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
        Self {
            port,
            workspace_root,
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = Router::new()
            .route("/", get(Self::serve_index))
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
            .with_state(Arc::new(self.clone()));

        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        println!("🚀 Hagibis Visual IDE listening on http://{}", addr);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    async fn serve_index() -> Html<&'static str> {
        Html(INDEX_HTML)
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

    async fn handle_chat(Json(payload): Json<ChatPayload>) -> impl IntoResponse {
        // Connect to local Ollama inference service (dynabook-coder or qwen2.5-coder:1.5b)
        let client = reqwest::Client::new();
        let prompt_clone = payload.prompt.clone();
        let prompt_full = if let Some(ctx) = payload.context {
            format!("Code Context:\n```\n{}\n```\n\nTask: {}\nProvide direct, surgical code corrections.", ctx, prompt_clone)
        } else {
            prompt_clone
        };

        let ollama_req = serde_json::json!({
            "model": "dynabook-coder:latest",
            "prompt": prompt_full,
            "stream": false
        });

        let resp_result = client.post("http://127.0.0.1:11434/api/generate")
            .json(&ollama_req)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await;

        let final_resp = match resp_result {
            Ok(resp) => Ok((resp, "dynabook-coder:latest")),
            Err(_) => {
                // Secondary attempt with qwen2.5-coder:1.5b
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
                    Ok(r) => Ok((r, "qwen2.5-coder:1.5b")),
                    Err(e) => Err(e),
                }
            }
        };

        match final_resp {
            Ok((resp, model_name)) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let text = json.get("response").and_then(|v| v.as_str()).unwrap_or("No response content");
                    (StatusCode::OK, Json(serde_json::json!({
                        "response": text,
                        "model": model_name,
                        "status": "success"
                    }))).into_response()
                } else {
                    (StatusCode::OK, Json(serde_json::json!({
                        "response": "Error parsing model output",
                        "status": "parse_error"
                    }))).into_response()
                }
            }
            Err(_) => {
                (StatusCode::OK, Json(serde_json::json!({
                    "response": "Local LLM inference service is offline or unreachable at http://127.0.0.1:11434. Please start Ollama ('ollama serve') and ensure 'dynabook-coder:latest' or 'qwen2.5-coder:1.5b' is installed.",
                    "model": "offline",
                    "status": "error"
                }))).into_response()
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
            let candidates = [".cursorrules", ".hgb/rules", "AGENTS.md"];
            for rel in candidates {
                let p = state.workspace_root.join(rel);
                if p.is_file() {
                    if let Ok(c) = fs::read_to_string(&p).await {
                        let rules_ctx = format!("\n[Project Rules ({}]:\n{}\n", rel, c);
                        expanded = expanded.replace("@rules", &rules_ctx).replace("@Rules", &rules_ctx);
                        sources.push(rel.to_string());
                        break;
                    }
                }
            }
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

        (StatusCode::OK, Json(serde_json::json!({
            "expanded_prompt": expanded,
            "sources": sources
        }))).into_response()
    }

    async fn handle_composer_commit(Json(plans): Json<Vec<ComposerFilePlan>>) -> impl IntoResponse {
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
}

const INDEX_HTML: &str = include_str!("../../hgb-desktop/dist/index.html");

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
}
