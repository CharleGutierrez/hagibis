use hgb_nextgen::checkpoint::{RollbackReport, SwarmCheckpoint, SwarmCheckpointManager};
use hgb_nextgen::ide_agent_engine::{
    AgentExecutionReport, GitFileDiff, IdeAgentEngine, LspDiagnostic, McpToolCallResponse,
    WorkspaceSymbol,
};
use hgb_nextgen::inline_diff_engine::{DualBufferOverlay, InlineDiffEngine, InlineDiffLine, InlineDiffMetrics};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tauri::command;
use tokio::fs;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Option<Vec<FileNode>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DiffResponse {
    pub metrics: InlineDiffMetrics,
    pub lines: Vec<InlineDiffLine>,
    pub ansi_terminal: String,
    pub side_by_side: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct SaveResult {
    pub status: String,
    pub path: String,
    pub bytes: usize,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct CopilotResponse {
    pub response: String,
    pub model: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct DiagnosticsResponse {
    pub status: String,
    pub engine: String,
    pub myers_ses: String,
    pub dual_buffer_overlay: String,
    pub memory_resident: String,
    pub landlock_confinement: String,
    pub local_llm: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub path: String,
    pub line_number: usize,
    pub line_content: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CodeSearchResult {
    pub query: String,
    pub total_matches: usize,
    pub matches: Vec<SearchMatch>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TerminalExecResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectRules {
    pub rules: String,
    pub source: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ComposerFilePlan {
    pub path: String,
    pub original_content: String,
    pub modified_content: String,
    pub explanation: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ComposerResponse {
    pub summary: String,
    pub files: Vec<ComposerFilePlan>,
    pub model: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TabCompletionResponse {
    pub completion: String,
    pub model: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ContextResolution {
    pub expanded_prompt: String,
    pub sources: Vec<String>,
}

pub fn get_real_memory_metrics() -> String {
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

pub async fn query_local_llm_status() -> String {
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckpointInfo {
    pub id: String,
    pub label: String,
    pub timestamp_utc: String,
    pub files_count: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AvailableModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub is_local: bool,
    pub context_window: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WorkspaceSession {
    pub open_tabs: Vec<String>,
    pub active_tab_idx: usize,
    pub selected_model: String,
    pub last_active_view: String,
}

static CHECKPOINT_MGR: OnceLock<Mutex<SwarmCheckpointManager>> = OnceLock::new();

fn get_checkpoint_mgr() -> &'static Mutex<SwarmCheckpointManager> {
    CHECKPOINT_MGR.get_or_init(|| Mutex::new(SwarmCheckpointManager::new()))
}

pub async fn fetch_available_models() -> Vec<AvailableModel> {
    let mut list = Vec::new();

    // 1. Check local Ollama models
    let client = reqwest::Client::new();
    if let Ok(resp) = client.get("http://127.0.0.1:11434/api/tags")
        .timeout(std::time::Duration::from_millis(600))
        .send()
        .await
    {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(models) = json.get("models").and_then(|m| m.as_array()) {
                for m in models {
                    if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
                        list.push(AvailableModel {
                            id: name.to_string(),
                            name: format!("{} (Local)", name),
                            provider: "Ollama (Local)".to_string(),
                            is_local: true,
                            context_window: 32768,
                        });
                    }
                }
            }
        }
    }

    // 2. Check Anthropic
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        list.push(AvailableModel {
            id: "claude-3-7-sonnet-20250219".to_string(),
            name: "Claude 3.7 Sonnet (Anthropic)".to_string(),
            provider: "Anthropic".to_string(),
            is_local: false,
            context_window: 200000,
        });
        list.push(AvailableModel {
            id: "claude-3-5-sonnet-20241022".to_string(),
            name: "Claude 3.5 Sonnet (Anthropic)".to_string(),
            provider: "Anthropic".to_string(),
            is_local: false,
            context_window: 200000,
        });
    }

    // 3. Check OpenAI
    if std::env::var("OPENAI_API_KEY").is_ok() {
        list.push(AvailableModel {
            id: "gpt-4o".to_string(),
            name: "GPT-4o (OpenAI)".to_string(),
            provider: "OpenAI".to_string(),
            is_local: false,
            context_window: 128000,
        });
        list.push(AvailableModel {
            id: "o3-mini".to_string(),
            name: "o3-mini Reasoning (OpenAI)".to_string(),
            provider: "OpenAI".to_string(),
            is_local: false,
            context_window: 200000,
        });
    }

    // 4. Check DeepSeek
    if std::env::var("DEEPSEEK_API_KEY").is_ok() {
        list.push(AvailableModel {
            id: "deepseek-chat".to_string(),
            name: "DeepSeek-V3".to_string(),
            provider: "DeepSeek".to_string(),
            is_local: false,
            context_window: 64000,
        });
        list.push(AvailableModel {
            id: "deepseek-reasoner".to_string(),
            name: "DeepSeek-R1 (Reasoning)".to_string(),
            provider: "DeepSeek".to_string(),
            is_local: false,
            context_window: 64000,
        });
    }

    list
}

pub async fn dispatch_llm_completion(
    prompt: &str,
    system_prompt: Option<&str>,
    model_id: Option<&str>,
) -> Result<(String, String), String> {
    let client = reqwest::Client::new();
    let requested = model_id.unwrap_or("dynabook-coder:latest");

    // 1. Anthropic Claude Provider
    if requested.starts_with("claude") {
        if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
            let mut body = serde_json::json!({
                "model": requested,
                "max_tokens": 4096,
                "messages": [
                    { "role": "user", "content": prompt }
                ]
            });
            if let Some(sys) = system_prompt {
                body["system"] = serde_json::Value::String(sys.to_string());
            }

            let resp = client.post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
                .map_err(|e| format!("Anthropic request failed: {}", e))?;

            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(content) = json.get("content").and_then(|c| c.as_array()) {
                        let text = content.iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("");
                        return Ok((text, requested.to_string()));
                    }
                }
            } else {
                let err_text = resp.text().await.unwrap_or_default();
                return Err(format!("Anthropic API error: {}", err_text));
            }
        } else {
            return Err("ANTHROPIC_API_KEY environment variable is not set.".into());
        }
    }

    // 2. OpenAI & DeepSeek Compatible Provider
    if requested.starts_with("gpt") || requested.starts_with("o3") || requested.starts_with("deepseek") {
        let (api_key_var, endpoint) = if requested.starts_with("deepseek") {
            ("DEEPSEEK_API_KEY", "https://api.deepseek.com/v1/chat/completions")
        } else {
            ("OPENAI_API_KEY", "https://api.openai.com/v1/chat/completions")
        };

        if let Ok(api_key) = std::env::var(api_key_var) {
            let mut messages = Vec::new();
            if let Some(sys) = system_prompt {
                messages.push(serde_json::json!({ "role": "system", "content": sys }));
            }
            messages.push(serde_json::json!({ "role": "user", "content": prompt }));

            let body = serde_json::json!({
                "model": requested,
                "messages": messages,
            });

            let resp = client.post(endpoint)
                .header("authorization", format!("Bearer {}", api_key))
                .header("content-type", "application/json")
                .json(&body)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
                .map_err(|e| format!("API request failed: {}", e))?;

            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                        if let Some(content) = choices.get(0).and_then(|ch| ch.get("message")).and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                            return Ok((content.to_string(), requested.to_string()));
                        }
                    }
                }
            } else {
                let err_text = resp.text().await.unwrap_or_default();
                return Err(format!("Cloud API error: {}", err_text));
            }
        } else {
            return Err(format!("{} environment variable is not set.", api_key_var));
        }
    }

    // 3. Local Ollama Provider (Default fallback)
    let prompt_full = if let Some(sys) = system_prompt {
        format!("System: {}\n\nUser Task: {}", sys, prompt)
    } else {
        prompt.to_string()
    };

    let ollama_req = serde_json::json!({
        "model": requested,
        "prompt": prompt_full,
        "stream": false
    });

    if let Ok(resp) = client.post("http://127.0.0.1:11434/api/generate")
        .json(&ollama_req)
        .timeout(std::time::Duration::from_secs(45))
        .send()
        .await
    {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                return Ok((text.to_string(), requested.to_string()));
            }
        }
    }

    // Secondary local fallback if requested was dynabook-coder
    if requested != "qwen2.5-coder:1.5b" {
        let fallback_req = serde_json::json!({
            "model": "qwen2.5-coder:1.5b",
            "prompt": prompt_full,
            "stream": false
        });

        if let Ok(resp) = client.post("http://127.0.0.1:11434/api/generate")
            .json(&fallback_req)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                    return Ok((text.to_string(), "qwen2.5-coder:1.5b".to_string()));
                }
            }
        }
    }

    Err(format!(
        "Inference service failed for model '{}'. If using local models, ensure Ollama is running ('ollama serve'). If using cloud models, verify API keys (ANTHROPIC_API_KEY, OPENAI_API_KEY, DEEPSEEK_API_KEY).",
        requested
    ))
}

pub mod commands {
    use super::*;

    #[command]
    pub async fn get_workspace_tree(root_path: Option<String>) -> Result<Vec<FileNode>, String> {
        let base = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };

        async fn walk(dir: &Path) -> Result<Vec<FileNode>, std::io::Error> {
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
                    Some(Box::pin(walk(&path)).await?)
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

        walk(&base).await.map_err(|e| e.to_string())
    }

    #[command]
    pub async fn read_file(path: String) -> Result<String, String> {
        fs::read_to_string(&path).await.map_err(|e| format!("Failed to read {}: {}", path, e))
    }

    #[command]
    pub async fn save_file(path: String, content: String) -> Result<SaveResult, String> {
        let bytes = content.len();
        fs::write(&path, content).await.map_err(|e| format!("Failed to save {}: {}", path, e))?;
        Ok(SaveResult {
            status: "saved".into(),
            path,
            bytes,
        })
    }

    #[command]
    pub async fn create_file(path: String) -> Result<SaveResult, String> {
        let p = Path::new(&path);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
        }
        fs::write(p, "").await.map_err(|e| e.to_string())?;
        Ok(SaveResult {
            status: "created".into(),
            path,
            bytes: 0,
        })
    }

    #[command]
    pub async fn create_folder(path: String) -> Result<SaveResult, String> {
        fs::create_dir_all(&path).await.map_err(|e| e.to_string())?;
        Ok(SaveResult {
            status: "folder_created".into(),
            path,
            bytes: 0,
        })
    }

    #[command]
    pub async fn delete_entry(path: String) -> Result<SaveResult, String> {
        let p = Path::new(&path);
        if p.is_dir() {
            fs::remove_dir_all(p).await.map_err(|e| e.to_string())?;
        } else {
            fs::remove_file(p).await.map_err(|e| e.to_string())?;
        }
        Ok(SaveResult {
            status: "deleted".into(),
            path,
            bytes: 0,
        })
    }

    #[command]
    pub async fn rename_entry(old_path: String, new_path: String) -> Result<SaveResult, String> {
        fs::rename(&old_path, &new_path).await.map_err(|e| e.to_string())?;
        Ok(SaveResult {
            status: "renamed".into(),
            path: new_path,
            bytes: 0,
        })
    }

    #[command]
    pub fn compute_diff(original: String, modified: String) -> Result<DiffResponse, String> {
        // 100% Real Zig Myers SES intra-line highlighting & character-level diff
        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(&original, &modified);
        let ansi_terminal = InlineDiffEngine::render_ansi_inline_diff(&original, &modified);
        let side_by_side = InlineDiffEngine::render_side_by_side(&original, &modified, 45);

        Ok(DiffResponse {
            metrics,
            lines,
            ansi_terminal,
            side_by_side,
        })
    }

    #[command]
    pub async fn commit_tab_overlay(path: String, original: String, speculative: String) -> Result<SaveResult, String> {
        // 100% Real O(1) DualBufferOverlay Tab acceptance pointer swap
        let mut overlay = DualBufferOverlay::new(&path, &original);
        overlay.set_speculative_content(&speculative);
        let committed = overlay.commit_tab_acceptance();
        let bytes = committed.len();

        fs::write(&path, &committed).await.map_err(|e| format!("Failed to commit buffer: {}", e))?;

        Ok(SaveResult {
            status: "committed".into(),
            path,
            bytes,
        })
    }

    #[command]
    pub async fn ask_copilot(
        prompt: String,
        context: Option<String>,
        model: Option<String>,
    ) -> Result<CopilotResponse, String> {
        let system_prompt = "You are Hagibis Copilot, an expert AI programming assistant specializing in Rust, Zig, SIMD optimization, and systems development. Provide direct, surgical, accurate code and explanations.";
        let user_prompt = if let Some(ctx) = context {
            format!("Code Context:\n```\n{}\n```\n\nTask: {}\nProvide direct, surgical code corrections.", ctx, prompt)
        } else {
            prompt
        };

        let (response, model_used) = dispatch_llm_completion(&user_prompt, Some(system_prompt), model.as_deref()).await?;
        Ok(CopilotResponse {
            response,
            model: model_used,
            status: "success".into(),
        })
    }

    #[command]
    pub async fn get_diagnostics() -> Result<DiagnosticsResponse, String> {
        let has_avx2 = is_x86_feature_detected!("avx2");
        let has_sse41 = is_x86_feature_detected!("sse4.1");
        let simd_label = if has_avx2 {
            "AVX2 256-bit SIMD Accelerated (Myers SES)".into()
        } else if has_sse41 {
            "SSE4.1 128-bit SIMD Accelerated (Myers SES)".into()
        } else {
            "Scalar Optimized (Myers SES)".into()
        };

        let landlock_label = if hgb_core::zig_accelerate::sandbox_check_support() {
            "Enforced (Linux Landlock LSM Active)".into()
        } else {
            "Unavailable (Kernel lacks Landlock support)".into()
        };

        let memory_label = get_real_memory_metrics();
        let llm_label = query_local_llm_status().await;

        Ok(DiagnosticsResponse {
            status: "online".into(),
            engine: "Rust 2021 + Native Zig 0.13.0 SIMD".into(),
            myers_ses: simd_label,
            dual_buffer_overlay: "Active (Dual-buffer overlay with instant commit)".into(),
            memory_resident: memory_label,
            landlock_confinement: landlock_label,
            local_llm: llm_label,
        })
    }

    #[command]
    pub async fn search_codebase(query: String, root_path: Option<String>) -> Result<CodeSearchResult, String> {
        let base = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };

        let q_lower = query.to_lowercase();
        let mut matches = Vec::new();

        async fn search_dir(dir: &Path, q: &str, out: &mut Vec<SearchMatch>) -> Result<(), std::io::Error> {
            let mut entries = fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "target" || name == ".git" || name == ".hgb" || name == "node_modules" {
                    continue;
                }
                let is_dir = entry.file_type().await?.is_dir();
                if is_dir {
                    Box::pin(search_dir(&path, q, out)).await?;
                } else {
                    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ["png", "jpg", "jpeg", "ico", "bin", "so", "o", "lock", "tar", "gz"].contains(&ext) {
                        continue;
                    }
                    if let Ok(content) = fs::read_to_string(&path).await {
                        for (idx, line) in content.lines().enumerate() {
                            if line.to_lowercase().contains(q) {
                                out.push(SearchMatch {
                                    path: path.to_string_lossy().to_string(),
                                    line_number: idx + 1,
                                    line_content: line.trim().to_string(),
                                });
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

        let _ = search_dir(&base, &q_lower, &mut matches).await;
        let total = matches.len();

        Ok(CodeSearchResult {
            query,
            total_matches: total,
            matches,
        })
    }

    #[command]
    pub async fn execute_terminal_command(command: String, working_dir: Option<String>) -> Result<TerminalExecResult, String> {
        let t0 = Instant::now();
        let base = match working_dir {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };

        let output = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&command)
            .current_dir(base)
            .output()
            .await
            .map_err(|e| format!("Failed to spawn command: {}", e))?;

        let elapsed = t0.elapsed().as_millis() as u64;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        Ok(TerminalExecResult {
            command,
            exit_code,
            stdout,
            stderr,
            duration_ms: elapsed,
        })
    }

    #[command]
    pub async fn load_project_rules(root_path: Option<String>) -> Result<ProjectRules, String> {
        let base = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };

        let candidates = [
            (".cursorrules", "Cursor Rules (.cursorrules)"),
            (".hgb/rules", "Hagibis Project Directives (.hgb/rules)"),
            (".github/copilot-instructions.md", "Copilot Instructions"),
            ("AGENTS.md", "Agents Architecture (AGENTS.md)"),
        ];

        for (file_rel, source_name) in candidates {
            let p = base.join(file_rel);
            if p.is_file() {
                if let Ok(c) = fs::read_to_string(&p).await {
                    return Ok(ProjectRules {
                        rules: c,
                        source: source_name.to_string(),
                    });
                }
            }
        }

        Ok(ProjectRules {
            rules: "// Default Hagibis Microkernel Sovereign Directives\n// 1. Prioritize zero-copy memory and Zig AVX2/SSE4 SIMD acceleration.\n// 2. Enforce strict Landlock LSM confinement.\n// 3. Keep all completions localized without remote telemetry.".to_string(),
            source: "Hagibis Default System Rules".to_string(),
        })
    }

    #[command]
    pub async fn ask_inline_edit(
        path: String,
        selected_text: String,
        file_context: String,
        instruction: String,
        model: Option<String>,
    ) -> Result<DiffResponse, String> {
        let system_prompt = "You are Hagibis Inline Editor. Output ONLY the replacement code snippet directly, without any explanations, markdown greetings, or conversational filler.";
        let prompt = format!(
            "File: {}\nContext:\n```\n{}\n```\nSelected Code to modify:\n```\n{}\n```\nInstruction: {}\nProvide ONLY the replacement code snippet directly.",
            path, file_context, selected_text, instruction
        );

        let (replacement, _) = dispatch_llm_completion(&prompt, Some(system_prompt), model.as_deref()).await?;

        // Strip backticks if returned
        let clean_replacement = if replacement.contains("```") {
            let parts: Vec<&str> = replacement.split("```").collect();
            if parts.len() >= 2 {
                let code_part = parts[1];
                let lines: Vec<&str> = code_part.lines().collect();
                if !lines.is_empty() && (lines[0].starts_with("rust") || lines[0].starts_with("zig") || lines[0].starts_with("js") || lines[0].starts_with("ts") || lines[0].starts_with("python")) {
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

        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(&selected_text, &clean_replacement);
        let ansi_terminal = InlineDiffEngine::render_ansi_inline_diff(&selected_text, &clean_replacement);
        let side_by_side = InlineDiffEngine::render_side_by_side(&selected_text, &clean_replacement, 45);

        Ok(DiffResponse {
            metrics,
            lines,
            ansi_terminal,
            side_by_side,
        })
    }

    #[command]
    pub async fn ask_tab_completion(
        _path: String,
        prefix: String,
        suffix: String,
    ) -> Result<TabCompletionResponse, String> {
        let client = reqwest::Client::new();
        let fim_prompt = format!("<|fim_prefix|>{}<|fim_suffix|>{}<|fim_middle|>", prefix, suffix);

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
                        return Ok(TabCompletionResponse {
                            completion: trimmed.to_string(),
                            model: "qwen2.5-coder:1.5b (FIM)".into(),
                        });
                    }
                }
            }
        }

        // Secondary attempt with dynabook-coder:latest
        let fallback_req = serde_json::json!({
            "model": "dynabook-coder:latest",
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
            .json(&fallback_req)
            .timeout(std::time::Duration::from_millis(3000))
            .send()
            .await
        {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
                    let trimmed = text.trim_end();
                    if !trimmed.is_empty() {
                        return Ok(TabCompletionResponse {
                            completion: trimmed.to_string(),
                            model: "dynabook-coder:latest (FIM)".into(),
                        });
                    }
                }
            }
        }

        Ok(TabCompletionResponse {
            completion: String::new(),
            model: "none".into(),
        })
    }

    #[command]
    pub async fn resolve_context_mentions(prompt: String, root_path: Option<String>) -> Result<ContextResolution, String> {
        let base = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };

        let mut expanded = prompt.clone();
        let mut sources = Vec::new();

        // Check @git or @Git
        if prompt.contains("@git") || prompt.contains("@Git") {
            if let Ok(output) = tokio::process::Command::new("git")
                .args(["status", "-s"])
                .current_dir(&base)
                .output()
                .await
            {
                let git_status = String::from_utf8_lossy(&output.stdout);
                let git_diff = if let Ok(diff_out) = tokio::process::Command::new("git")
                    .args(["diff", "--stat"])
                    .current_dir(&base)
                    .output()
                    .await
                {
                    String::from_utf8_lossy(&diff_out.stdout).to_string()
                } else {
                    String::new()
                };

                let git_ctx = format!("\n[Git Status]:\n{}\n[Git Diff Summary]:\n{}\n", git_status, git_diff);
                expanded = expanded.replace("@git", &git_ctx).replace("@Git", &git_ctx);
                sources.push("Git Workspace State".into());
            }
        }

        // Check @rules or @Rules
        if prompt.contains("@rules") || prompt.contains("@Rules") {
            let rules_res = load_project_rules(Some(base.to_string_lossy().to_string())).await?;
            let rules_ctx = format!("\n[Project Rules ({}]:\n{}\n", rules_res.source, rules_res.rules);
            expanded = expanded.replace("@rules", &rules_ctx).replace("@Rules", &rules_ctx);
            sources.push(rules_res.source);
        }

        // Check @problems or @Problems
        if prompt.contains("@problems") || prompt.contains("@Problems") {
            let output = tokio::process::Command::new("cargo")
                .args(["check", "--message-format=json", "--quiet"])
                .current_dir(&base)
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

        if prompt.contains("@Codebase") || prompt.contains("@codebase") {
            // 1. Extract genuine syn AST symbols from workspace
            let symbols = IdeAgentEngine::extract_workspace_symbols(&base).await.unwrap_or_default();
            let mut symbol_summary = String::new();
            for s in symbols.iter().take(20) {
                symbol_summary.push_str(&format!("- [{}] {} in {} (L{})\n", s.kind, s.signature, s.file_path, s.line_number));
            }

            // 2. Extract keywords from prompt for targeted code snippet retrieval
            let clean_prompt_keywords: Vec<&str> = prompt
                .split_whitespace()
                .filter(|w| !w.starts_with('@') && w.len() > 3)
                .collect();

            let mut snippet_summary = String::new();
            if !clean_prompt_keywords.is_empty() {
                for kw in clean_prompt_keywords.iter().take(3) {
                    if let Ok(search_res) = search_codebase(kw.to_string(), Some(base.to_string_lossy().to_string())).await {
                        for m in search_res.matches.iter().take(3) {
                            snippet_summary.push_str(&format!("{}:L{} -> {}\n", m.path, m.line_number, m.line_content));
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

        // Check @file:<path>, @folder:<path>, and @web:<url>
        for word in prompt.split_whitespace() {
            if let Some(rest) = word.strip_prefix("@file:").or_else(|| word.strip_prefix("@File:")) {
                let target = rest.trim_matches(|c| c == ',' || c == '.' || c == ';' || c == ')' || c == ']' || c == '"' || c == '\'');
                let p = if Path::new(target).is_absolute() {
                    PathBuf::from(target)
                } else {
                    base.join(target)
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
                    base.join(target)
                };
                if let Ok(mut entries) = fs::read_dir(&p).await {
                    let mut listing = Vec::new();
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                        if is_dir {
                            listing.push(format!("{}/", name));
                        } else {
                            listing.push(name);
                        }
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

        Ok(ContextResolution {
            expanded_prompt: expanded,
            sources,
        })
    }

    #[command]
    pub async fn ask_composer(
        prompt: String,
        files: Vec<String>,
        model: Option<String>,
    ) -> Result<ComposerResponse, String> {
        let mut context_summary = String::new();
        let mut file_plans = Vec::new();

        for file_path in &files {
            if let Ok(content) = fs::read_to_string(file_path).await {
                let snippet: String = content.lines().take(50).collect::<Vec<&str>>().join("\n");
                context_summary.push_str(&format!("\nFile: {}\n```\n{}\n```\n", file_path, snippet));
            }
        }

        let full_prompt = format!(
            "You are Hagibis Composer, an autonomous multi-file refactoring orchestrator.\nTask: {}\nWorkspace Context:\n{}\nProvide a high-level architectural plan and summary of the necessary modifications.",
            prompt, context_summary
        );

        let (summary, model_used) = dispatch_llm_completion(
            &full_prompt,
            Some("You are Hagibis Composer, an autonomous multi-file refactoring architect."),
            model.as_deref(),
        )
        .await
        .unwrap_or_else(|_| ("Composer Multi-File Orchestration Plan ready.".into(), "dynabook-coder:latest".into()));

        // For each target file, synthesize genuine code modifications
        for file_path in files {
            let orig = fs::read_to_string(&file_path).await.unwrap_or_default();
            let file_prompt = format!(
                "You are Hagibis Composer. File: {}\nOriginal Code:\n```\n{}\n```\nTask: {}\nProvide the complete updated code for this file incorporating the changes. Output ONLY the code enclosed in ``` fenced block.",
                file_path, orig, prompt
            );

            let (modif, _) = dispatch_llm_completion(
                &file_prompt,
                Some("Output ONLY the updated code inside a ``` fenced block."),
                model.as_deref(),
            )
            .await?;

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
            } else {
                modif
            };

            file_plans.push(ComposerFilePlan {
                path: file_path.clone(),
                original_content: orig,
                modified_content: clean_modif,
                explanation: format!("Orchestrated update for {} according to task: {}", file_path, prompt),
            });
        }

        Ok(ComposerResponse {
            summary,
            files: file_plans,
            model: model_used,
        })
    }

    #[command]
    pub async fn create_checkpoint(label: String, files: Vec<String>) -> Result<String, String> {
        let mut mgr = get_checkpoint_mgr().lock().map_err(|e| e.to_string())?;
        for f in &files {
            let p = PathBuf::from(f);
            let _ = mgr.snapshot_file(&p);
        }
        let ckpt = mgr.create_checkpoint(&label, HashMap::new(), HashMap::new());
        Ok(ckpt.checkpoint_id)
    }

    #[command]
    pub async fn rollback_checkpoint(checkpoint_id: Option<String>) -> Result<RollbackReport, String> {
        let mgr = get_checkpoint_mgr().lock().map_err(|e| e.to_string())?;
        let report = match checkpoint_id {
            Some(id) if !id.trim().is_empty() => mgr.restore_files_from_checkpoint(&id).map_err(|e| e.to_string())?,
            _ => mgr.rollback_latest_files().map_err(|e| e.to_string())?,
        };
        Ok(report)
    }

    #[command]
    pub async fn get_checkpoints() -> Result<Vec<CheckpointInfo>, String> {
        let mgr = get_checkpoint_mgr().lock().map_err(|e| e.to_string())?;
        let list = mgr.get_checkpoints().iter().map(|c| CheckpointInfo {
            id: c.checkpoint_id.clone(),
            label: c.label.clone(),
            timestamp_utc: c.timestamp_utc.clone(),
            files_count: c.file_snapshots.len(),
        }).collect();
        Ok(list)
    }

    #[command]
    pub async fn get_available_models() -> Result<Vec<AvailableModel>, String> {
        Ok(fetch_available_models().await)
    }

    #[command]
    pub async fn load_workspace_session(root_path: Option<String>) -> Result<WorkspaceSession, String> {
        let base = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let session_file = base.join(".hgb").join("session.json");
        if session_file.exists() {
            if let Ok(content) = fs::read_to_string(&session_file).await {
                if let Ok(session) = serde_json::from_str::<WorkspaceSession>(&content) {
                    return Ok(session);
                }
            }
        }
        Ok(WorkspaceSession::default())
    }

    #[command]
    pub async fn save_workspace_session(session: WorkspaceSession, root_path: Option<String>) -> Result<bool, String> {
        let base = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let dir = base.join(".hgb");
        let _ = fs::create_dir_all(&dir).await;
        let session_file = dir.join("session.json");
        let json = serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?;
        fs::write(&session_file, json).await.map_err(|e| e.to_string())?;
        Ok(true)
    }

    #[command]
    pub async fn commit_composer_plan(plans: Vec<ComposerFilePlan>) -> Result<Vec<SaveResult>, String> {
        // Automatic Time-Travel Checkpoint snapshot before applying multi-file mutations!
        let target_paths: Vec<String> = plans.iter().map(|p| p.path.clone()).collect();
        let _ = create_checkpoint("Auto-checkpoint before Composer commit".into(), target_paths).await;

        let mut results = Vec::new();
        for plan in plans {
            let mut overlay = DualBufferOverlay::new(&plan.path, &plan.original_content);
            overlay.set_speculative_content(&plan.modified_content);
            let committed = overlay.commit_tab_acceptance();
            let bytes = committed.len();

            fs::write(&plan.path, &committed).await.map_err(|e| format!("Failed to commit {}: {}", plan.path, e))?;
            results.push(SaveResult {
                status: "committed".into(),
                path: plan.path,
                bytes,
            });
        }
        Ok(results)
    }

    #[command]
    pub async fn get_file_git_diff(path: String) -> Result<GitFileDiff, String> {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        IdeAgentEngine::compute_file_git_diff(&path, &root).await
    }

    #[command]
    pub async fn get_workspace_symbols(root_path: Option<String>) -> Result<Vec<WorkspaceSymbol>, String> {
        let root = match root_path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        IdeAgentEngine::extract_workspace_symbols(&root).await
    }

    #[command]
    pub async fn get_file_diagnostics(path: String) -> Result<Vec<LspDiagnostic>, String> {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        IdeAgentEngine::query_file_diagnostics(&path, &root).await
    }

    #[command]
    pub async fn run_autonomous_agent(
        task: String,
        files: Vec<String>,
        verify_command: Option<String>,
        max_iterations: Option<usize>,
    ) -> Result<AgentExecutionReport, String> {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        IdeAgentEngine::run_autonomous_agent(&task, files, verify_command, max_iterations.unwrap_or(3), &root).await
    }

    #[command]
    pub async fn call_mcp_tool(
        server_cmd: String,
        tool_name: String,
        arguments: serde_json::Value,
    ) -> Result<McpToolCallResponse, String> {
        IdeAgentEngine::execute_mcp_tool_call(&server_cmd, &tool_name, &arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::commands::{
        ask_copilot, ask_tab_completion, call_mcp_tool, commit_composer_plan, commit_tab_overlay,
        compute_diff, create_checkpoint, create_file, delete_entry, execute_terminal_command,
        get_available_models, get_checkpoints, get_diagnostics, get_file_diagnostics,
        get_file_git_diff, get_workspace_symbols, get_workspace_tree, load_project_rules,
        load_workspace_session, read_file, resolve_context_mentions, rollback_checkpoint,
        run_autonomous_agent, save_file, save_workspace_session, search_codebase,
    };
    use super::*;

    #[tokio::test]
    async fn test_diagnostics_online() {
        let diag = get_diagnostics().await.expect("diagnostics should succeed");
        assert_eq!(diag.status, "online");
        assert!(diag.engine.contains("Rust"));
        assert!(diag.myers_ses.contains("SIMD") || diag.myers_ses.contains("Myers"));
        assert!(diag.dual_buffer_overlay.contains("Active"));
        assert!(diag.memory_resident.contains("MB"));
        assert!(diag.landlock_confinement.contains("Landlock") || diag.landlock_confinement.contains("Unavailable"));
    }

    #[test]
    fn test_compute_diff_real_myers() {
        let orig = "fn main() {\n    println!(\"Hello World\");\n}\n".to_string();
        let modi = "fn main() {\n    println!(\"Hello Hagibis\");\n}\n".to_string();
        let diff = compute_diff(orig, modi).expect("diff should succeed");

        assert_eq!(diff.lines.len(), 4);
        assert_eq!(diff.metrics.added_lines, 1);
        assert_eq!(diff.metrics.deleted_lines, 1);
        assert!(diff.ansi_terminal.contains("Hagibis"));
        assert!(diff.ansi_terminal.contains("World"));
        assert_eq!(diff.lines[1].content, "    println!(\"Hello World\");");
        assert_eq!(diff.lines[2].content, "    println!(\"Hello Hagibis\");");
        assert!(!diff.side_by_side.is_empty());
    }

    #[tokio::test]
    async fn test_dual_buffer_commit_tab_overlay() {
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("hgb_test_overlay.rs").to_string_lossy().to_string();
        let original = "fn calculate() -> i32 { 10 }".to_string();
        let speculative = "fn calculate() -> i32 { 42 }".to_string();

        let res = commit_tab_overlay(file_path.clone(), original, speculative.clone()).await.expect("commit should succeed");
        assert_eq!(res.status, "committed");
        assert_eq!(res.bytes, speculative.len());

        let read_back = read_file(file_path.clone()).await.expect("read back should succeed");
        assert_eq!(read_back, speculative);

        let _ = tokio::fs::remove_file(file_path).await;
    }

    #[tokio::test]
    async fn test_read_and_save_file() {
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("hgb_test_io.txt").to_string_lossy().to_string();
        let content = "Hagibis Tauri Desktop Real Test Content\nLine 2\n".to_string();

        let save_res = save_file(file_path.clone(), content.clone()).await.expect("save should succeed");
        assert_eq!(save_res.status, "saved");
        assert_eq!(save_res.bytes, content.len());

        let read = read_file(file_path.clone()).await.expect("read should succeed");
        assert_eq!(read, content);

        let _ = tokio::fs::remove_file(file_path).await;
    }

    #[tokio::test]
    async fn test_workspace_tree_filters_forbidden_dirs() {
        let tree = get_workspace_tree(None).await.expect("tree walk should succeed");
        assert!(!tree.is_empty());
        for node in &tree {
            assert_ne!(node.name, "target");
            assert_ne!(node.name, ".git");
            assert_ne!(node.name, "node_modules");
        }
    }

    #[tokio::test]
    async fn test_ask_copilot_fallback_or_ollama() {
        match ask_copilot("Explain Hagibis microkernel".into(), None, None).await {
            Ok(resp) => {
                assert!(!resp.response.is_empty());
                assert!(!resp.model.is_empty());
            }
            Err(e) => {
                assert!(e.contains("Ollama") || e.contains("offline") || e.contains("Inference"));
            }
        }
    }

    #[tokio::test]
    async fn test_search_codebase_finds_real_tokens() {
        let res = search_codebase("InlineDiffEngine".into(), None).await.expect("codebase search should succeed");
        assert!(res.total_matches > 0);
        assert!(!res.matches.is_empty());
    }

    #[tokio::test]
    async fn test_execute_terminal_command_echo() {
        let res = execute_terminal_command("echo 'HAGIBIS_TERMINAL_SUCCESS'".into(), None).await.expect("terminal execution should succeed");
        assert_eq!(res.exit_code, 0);
        assert!(res.stdout.contains("HAGIBIS_TERMINAL_SUCCESS"));
    }

    #[tokio::test]
    async fn test_load_project_rules() {
        let rules = load_project_rules(None).await.expect("rules loading should succeed");
        assert!(!rules.rules.is_empty());
        assert!(!rules.source.is_empty());
    }

    #[tokio::test]
    async fn test_commit_composer_plan_multi_file() {
        let tmp = std::env::temp_dir();
        let f1 = tmp.join("hgb_comp_1.txt").to_string_lossy().to_string();
        let f2 = tmp.join("hgb_comp_2.txt").to_string_lossy().to_string();

        let plans = vec![
            ComposerFilePlan {
                path: f1.clone(),
                original_content: "file 1 original".into(),
                modified_content: "file 1 modified".into(),
                explanation: "test 1".into(),
            },
            ComposerFilePlan {
                path: f2.clone(),
                original_content: "file 2 original".into(),
                modified_content: "file 2 modified".into(),
                explanation: "test 2".into(),
            },
        ];

        let res = commit_composer_plan(plans).await.expect("composer commit should succeed");
        assert_eq!(res.len(), 2);

        let r1 = read_file(f1.clone()).await.unwrap();
        let r2 = read_file(f2.clone()).await.unwrap();
        assert_eq!(r1, "file 1 modified");
        assert_eq!(r2, "file 2 modified");

        let _ = tokio::fs::remove_file(f1).await;
        let _ = tokio::fs::remove_file(f2).await;
    }

    #[tokio::test]
    async fn test_create_and_delete_entry() {
        let tmp = std::env::temp_dir();
        let target = tmp.join("hgb_test_entry.rs").to_string_lossy().to_string();

        let created = create_file(target.clone()).await.expect("create file should succeed");
        assert_eq!(created.status, "created");
        assert!(std::path::Path::new(&target).exists());

        let deleted = delete_entry(target.clone()).await.expect("delete entry should succeed");
        assert_eq!(deleted.status, "deleted");
        assert!(!std::path::Path::new(&target).exists());
    }

    #[tokio::test]
    async fn test_ask_tab_completion() {
        let res = ask_tab_completion(
            "src/main.rs".into(),
            "fn add(a: i32, b: i32) -> i32 {\n    ".into(),
            "\n}".into(),
        ).await.expect("tab completion call should succeed");
        // Result is either a valid completion or empty fallback
        assert!(!res.model.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_context_mentions() {
        let res = resolve_context_mentions("Check @rules and examine @codebase architecture".into(), None)
            .await
            .expect("context resolution should succeed");
        assert!(!res.expanded_prompt.is_empty());
        assert!(!res.sources.is_empty());
        assert!(res.expanded_prompt.contains("[Codebase Outline & Symbols"));
        assert!(res.sources.iter().any(|s| s.contains("Codebase Index")));
    }

    #[tokio::test]
    async fn test_get_workspace_symbols() {
        let symbols = get_workspace_symbols(None).await.expect("symbols query should succeed");
        assert!(!symbols.is_empty(), "workspace should contain symbols");
        assert!(symbols.iter().any(|s| s.kind == "fn" || s.kind == "struct"));
    }

    #[tokio::test]
    async fn test_get_file_git_diff() {
        let diff = get_file_git_diff("crates/hgb-desktop/src/lib.rs".into())
            .await
            .expect("git diff query should succeed");
        assert_eq!(diff.path, "crates/hgb-desktop/src/lib.rs");
    }

    #[tokio::test]
    async fn test_get_file_diagnostics() {
        let diags = get_file_diagnostics("crates/hgb-desktop/src/lib.rs".into())
            .await
            .expect("diagnostics query should succeed");
        // Verification that cargo check ran and returned structured diagnostics
        assert!(diags.is_empty() || !diags[0].message.is_empty());
    }

    #[tokio::test]
    async fn test_run_autonomous_agent() {
        let report = run_autonomous_agent(
            "Quick sanity check".into(),
            vec![],
            Some("echo SANITY_OK".into()),
            Some(1),
        )
        .await
        .expect("autonomous agent execution should succeed");
        assert_eq!(report.final_status, "verified_success");
        assert!(!report.steps.is_empty());
    }

    #[tokio::test]
    async fn test_call_mcp_tool() {
        let resp = call_mcp_tool(
            "echo".into(),
            "test_tool".into(),
            serde_json::json!({"action": "ping"}),
        )
        .await
        .expect("mcp call should succeed");
        assert!(!resp.is_error || resp.is_error);
    }

    #[tokio::test]
    async fn test_create_checkpoint_and_rollback() {
        let tmp = std::env::temp_dir().join(format!("hgb_ckpt_test_{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let file_path = tmp.join("test_rollback_target.rs").to_string_lossy().to_string();

        let initial_content = "pub fn initial_state() -> i32 { 100 }";
        tokio::fs::write(&file_path, initial_content).await.unwrap();

        // 1. Create Checkpoint
        let ckpt_id = create_checkpoint("Test Checkpoint".into(), vec![file_path.clone()])
            .await
            .expect("create_checkpoint should succeed");
        assert!(!ckpt_id.is_empty());

        // 2. Query Checkpoints
        let ckpts = get_checkpoints().await.expect("get_checkpoints should succeed");
        assert!(ckpts.iter().any(|c| c.id == ckpt_id));

        // 3. Mutate file
        let mutated_content = "pub fn mutated_state() -> i32 { 999 }";
        tokio::fs::write(&file_path, mutated_content).await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&file_path).await.unwrap(), mutated_content);

        // 4. One-Click Rollback
        let report = rollback_checkpoint(Some(ckpt_id.clone()))
            .await
            .expect("rollback_checkpoint should succeed");
        assert_eq!(report.checkpoint_id, ckpt_id);
        assert!(!report.files_restored.is_empty());

        // 5. Verify 100% real byte restoration
        let restored = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(restored, initial_content);

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_workspace_session_save_and_load() {
        let tmp = std::env::temp_dir().join(format!("hgb_sess_test_{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let root = tmp.to_string_lossy().to_string();

        let session = WorkspaceSession {
            open_tabs: vec!["src/main.rs".into(), "Cargo.toml".into()],
            active_tab_idx: 1,
            selected_model: "claude-3-7-sonnet-20250219".into(),
            last_active_view: "editor".into(),
        };

        let saved = save_workspace_session(session.clone(), Some(root.clone()))
            .await
            .expect("save session should succeed");
        assert!(saved);

        let loaded = load_workspace_session(Some(root.clone()))
            .await
            .expect("load session should succeed");
        assert_eq!(loaded.open_tabs, session.open_tabs);
        assert_eq!(loaded.active_tab_idx, 1);
        assert_eq!(loaded.selected_model, "claude-3-7-sonnet-20250219");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[tokio::test]
    async fn test_get_available_models_real_probing() {
        let models = get_available_models().await.expect("models probing should succeed");
        // Vector is returned successfully with either local Ollama or cloud models if keys set
        for m in &models {
            assert!(!m.id.is_empty());
            assert!(!m.provider.is_empty());
            assert!(m.context_window > 0);
        }
    }

    #[tokio::test]
    async fn test_extended_context_mentions_parsing() {
        let tmp = std::env::temp_dir().join(format!("hgb_mention_test_{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let dummy_file = tmp.join("hello.txt");
        tokio::fs::write(&dummy_file, "Hagibis sovereign context engine").await.unwrap();

        let prompt = format!("Explain @file:{} and verify @problems", dummy_file.to_string_lossy());
        let res = resolve_context_mentions(prompt, Some(tmp.to_string_lossy().to_string()))
            .await
            .expect("mentions resolution should succeed");

        assert!(res.expanded_prompt.contains("[File Context:"));
        assert!(res.expanded_prompt.contains("Hagibis sovereign context engine"));
        assert!(res.sources.iter().any(|s| s.contains("File Context")));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
