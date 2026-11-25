use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::Mutex;

pub const MAX_CONSUMED_TOKENS: usize = 5000;
pub const CONSUMED_TOKEN_TTL_SECS: u64 = 86400; // 24 hours

/// Bounded FIFO and TTL-evicting tracker for consumed approval tokens to prevent unbounded memory growth
#[derive(Debug, Clone)]
pub struct ConsumedTokenTracker {
    tokens: HashMap<String, Instant>,
    order: VecDeque<String>,
    max_capacity: usize,
    ttl: Duration,
}

impl ConsumedTokenTracker {
    pub fn new(max_capacity: usize, ttl: Duration) -> Self {
        Self {
            tokens: HashMap::new(),
            order: VecDeque::new(),
            max_capacity,
            ttl,
        }
    }

    /// Prune tokens exceeding TTL from the front of the FIFO queue
    pub fn prune_expired(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.order.front() {
            if let Some(inserted_at) = self.tokens.get(front) {
                if now.duration_since(*inserted_at) >= self.ttl {
                    let old_token = self.order.pop_front().unwrap();
                    self.tokens.remove(&old_token);
                    continue;
                }
            } else {
                self.order.pop_front();
                continue;
            }
            break;
        }
    }

    pub fn contains(&mut self, token: &str) -> bool {
        self.prune_expired();
        self.tokens.contains_key(token)
    }

    pub fn insert(&mut self, token: String) {
        self.prune_expired();
        if self.tokens.contains_key(&token) {
            return;
        }
        while self.tokens.len() >= self.max_capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.tokens.remove(&oldest);
            } else {
                break;
            }
        }
        let now = Instant::now();
        self.tokens.insert(token.clone(), now);
        self.order.push_back(token);
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

/// Immutable approval token representation bound to exact execution context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalToken {
    pub token: String,
    pub call_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub payload_hash: String,
    pub target_path_hash: Option<String>,
    pub created_at: u64,
}

/// Pending tool approval with server-generated UUIDv4 token and cryptographic payload binding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolApproval {
    pub approval_token: String,
    pub call_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub description: String,
    pub payload_hash: String,
    pub target_path_hash: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentStepResponse {
    #[serde(rename = "message")]
    Message { content: String, is_final: bool },
    #[serde(rename = "tool_approval_required")]
    ToolApprovalRequired {
        call_id: String,
        #[serde(default)]
        approval_token: String,
        tool: String,
        arguments: serde_json::Value,
        description: String,
    },
    #[serde(rename = "tool_executed")]
    ToolExecuted {
        call_id: String,
        tool: String,
        output: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageItem {
    pub role: String, // "user", "assistant", "tool", "system"
    pub content: String,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

pub struct AgentLoopEngine {
    workspace_root: PathBuf,
    pending_approvals: Arc<Mutex<HashMap<String, PendingToolApproval>>>,
    consumed_tokens: Arc<Mutex<ConsumedTokenTracker>>,
    auto_approve_session: Arc<AtomicBool>,
}

impl AgentLoopEngine {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            consumed_tokens: Arc::new(Mutex::new(ConsumedTokenTracker::new(
                MAX_CONSUMED_TOKENS,
                Duration::from_secs(CONSUMED_TOKEN_TTL_SECS),
            ))),
            auto_approve_session: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_auto_approve(&self, enabled: bool) {
        self.auto_approve_session.store(enabled, Ordering::SeqCst);
    }

    pub fn is_auto_approve(&self) -> bool {
        self.auto_approve_session.load(Ordering::SeqCst)
    }

    pub fn get_available_tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "bash_run".to_string(),
                description: "Execute an arbitrary command in the workspace shell and capture stdout/stderr".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to run (e.g. 'cargo check', 'git status', 'ls -la')"
                        }
                    },
                    "required": ["command"]
                }),
            },
            ToolDefinition {
                name: "file_read".to_string(),
                description: "Read the full or line-bounded content of a file in the workspace".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path" },
                        "start_line": { "type": "integer", "description": "Optional 1-indexed start line" },
                        "end_line": { "type": "integer", "description": "Optional 1-indexed end line" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "file_write".to_string(),
                description: "Write complete content to a file in the workspace (creates parent directories if needed)".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path" },
                        "content": { "type": "string", "description": "New file content to write" }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolDefinition {
                name: "file_patch".to_string(),
                description: "Surgically patch a targeted section of code in a file by replacing old_snippet with new_snippet".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path" },
                        "old_snippet": { "type": "string", "description": "Existing text block to replace" },
                        "new_snippet": { "type": "string", "description": "Replacement text block" }
                    },
                    "required": ["path", "old_snippet", "new_snippet"]
                }),
            },
            ToolDefinition {
                name: "codebase_search".to_string(),
                description: "Search across files in the workspace for a keyword or regex pattern".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search pattern or token" }
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "list_dir".to_string(),
                description: "List directory contents in the workspace".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative directory path (e.g. '.' or 'src')" }
                    },
                    "required": ["path"]
                }),
            },
        ]
    }

    /// Execute a tool call safely in the workspace
    pub async fn execute_tool(&self, call: &ToolCall) -> ToolResult {
        match call.name.as_str() {
            "bash_run" => {
                let cmd_str = match call.arguments.get("command").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'command' argument".to_string(),
                            is_error: true,
                        }
                    }
                };

                let output = tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(cmd_str)
                    .current_dir(&self.workspace_root)
                    .output()
                    .await;

                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let exit_code = out.status.code().unwrap_or(-1);
                        let full = format!(
                            "Exit Code: {}\n--- STDOUT ---\n{}\n--- STDERR ---\n{}",
                            exit_code, stdout, stderr
                        );
                        ToolResult {
                            call_id: call.id.clone(),
                            output: full,
                            is_error: exit_code != 0,
                        }
                    }
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        output: format!("Execution failed: {}", e),
                        is_error: true,
                    },
                }
            }

            "file_read" => {
                let path_str = match call.arguments.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'path' argument".to_string(),
                            is_error: true,
                        }
                    }
                };

                let full_path = match self.resolve_path(path_str) {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: format!("Path resolution error: {}", e),
                            is_error: true,
                        };
                    }
                };
                match fs::read_to_string(&full_path).await {
                    Ok(content) => {
                        let start = call.arguments.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
                        let end = call.arguments.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);

                        let lines: Vec<&str> = content.lines().collect();
                        let result_text = if let Some(s) = start {
                            let s_idx = s.saturating_sub(1);
                            let e_idx = end.unwrap_or(lines.len()).min(lines.len());
                            if s_idx < lines.len() {
                                lines[s_idx..e_idx].join("\n")
                            } else {
                                String::new()
                            }
                        } else {
                            content
                        };

                        ToolResult {
                            call_id: call.id.clone(),
                            output: result_text,
                            is_error: false,
                        }
                    }
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        output: format!("Failed to read {}: {}", path_str, e),
                        is_error: true,
                    },
                }
            }

            "file_write" => {
                let path_str = match call.arguments.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'path' argument".to_string(),
                            is_error: true,
                        }
                    }
                };
                let content = match call.arguments.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'content' argument".to_string(),
                            is_error: true,
                        }
                    }
                };

                let full_path = match self.resolve_path(path_str) {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: format!("Path resolution error: {}", e),
                            is_error: true,
                        };
                    }
                };
                if let Some(parent) = full_path.parent() {
                    let _ = fs::create_dir_all(parent).await;
                }

                match fs::write(&full_path, content).await {
                    Ok(_) => ToolResult {
                        call_id: call.id.clone(),
                        output: format!("Successfully wrote {} bytes to {}", content.len(), path_str),
                        is_error: false,
                    },
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        output: format!("Failed to write {}: {}", path_str, e),
                        is_error: true,
                    },
                }
            }

            "file_patch" => {
                let path_str = match call.arguments.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'path' argument".to_string(),
                            is_error: true,
                        }
                    }
                };
                let old_snippet = match call.arguments.get("old_snippet").and_then(|v| v.as_str()) {
                    Some(o) => o,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'old_snippet' argument".to_string(),
                            is_error: true,
                        }
                    }
                };
                let new_snippet = match call.arguments.get("new_snippet").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'new_snippet' argument".to_string(),
                            is_error: true,
                        }
                    }
                };

                let full_path = match self.resolve_path(path_str) {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: format!("Path resolution error: {}", e),
                            is_error: true,
                        };
                    }
                };
                match fs::read_to_string(&full_path).await {
                    Ok(content) => {
                        if !content.contains(old_snippet) {
                            return ToolResult {
                                call_id: call.id.clone(),
                                output: format!("Target snippet not found in {}", path_str),
                                is_error: true,
                            };
                        }

                        let patched = content.replacen(old_snippet, new_snippet, 1);
                        match fs::write(&full_path, &patched).await {
                            Ok(_) => ToolResult {
                                call_id: call.id.clone(),
                                output: format!("Successfully patched {}", path_str),
                                is_error: false,
                            },
                            Err(e) => ToolResult {
                                call_id: call.id.clone(),
                                output: format!("Failed to write patched file: {}", e),
                                is_error: true,
                            },
                        }
                    }
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        output: format!("Cannot read file to patch: {}", e),
                        is_error: true,
                    },
                }
            }

            "codebase_search" => {
                let query = match call.arguments.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: "Error: Missing 'query' argument".to_string(),
                            is_error: true,
                        }
                    }
                };

                let mut matches = Vec::new();
                let mut stack = vec![self.workspace_root.clone()];
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
                                if let Ok(text) = fs::read_to_string(&path).await {
                                    for (idx, line) in text.lines().enumerate() {
                                        if line.contains(query) {
                                            let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                                            matches.push(format!("{}:{}: {}", rel.display(), idx + 1, line.trim()));
                                            if matches.len() >= 50 {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if matches.len() >= 50 {
                        break;
                    }
                }

                ToolResult {
                    call_id: call.id.clone(),
                    output: if matches.is_empty() {
                        format!("No matches found for '{}'", query)
                    } else {
                        matches.join("\n")
                    },
                    is_error: false,
                }
            }

            "list_dir" => {
                let rel_path = call.arguments.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let full = match self.resolve_path(rel_path) {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolResult {
                            call_id: call.id.clone(),
                            output: format!("Path resolution error: {}", e),
                            is_error: true,
                        };
                    }
                };

                match fs::read_dir(&full).await {
                    Ok(mut entries) => {
                        let mut names = Vec::new();
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let ft = entry.file_type().await.ok();
                            let is_dir = ft.map(|f| f.is_dir()).unwrap_or(false);
                            let n = entry.file_name().to_string_lossy().to_string();
                            names.push(format!("{}{}", n, if is_dir { "/" } else { "" }));
                        }
                        names.sort();
                        ToolResult {
                            call_id: call.id.clone(),
                            output: names.join("\n"),
                            is_error: false,
                        }
                    }
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        output: format!("Failed to list {}: {}", rel_path, e),
                        is_error: true,
                    },
                }
            }

            _ => ToolResult {
                call_id: call.id.clone(),
                output: format!("Unknown tool '{}'", call.name),
                is_error: true,
            },
        }
    }

    /// Checks if a tool call requires explicit user approval
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        if self.is_auto_approve() {
            return false;
        }
        matches!(tool_name, "bash_run" | "file_write" | "file_patch")
    }

    /// Registers a tool call requiring user approval with server-generated UUIDv4 token and payload binding
    pub async fn register_pending_approval(&self, call: &ToolCall) -> PendingToolApproval {
        self.register_pending_approval_with_session(call, "default_session").await
    }

    /// Registers a tool call requiring user approval bound to a specific session ID
    pub async fn register_pending_approval_with_session(&self, call: &ToolCall, session_id: &str) -> PendingToolApproval {
        let desc = match call.name.as_str() {
            "bash_run" => {
                let cmd = call.arguments.get("command").and_then(|v| v.as_str()).unwrap_or("");
                format!("Execute command: $ {}", cmd)
            }
            "file_write" => {
                let p = call.arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
                format!("Overwrite file: {}", p)
            }
            "file_patch" => {
                let p = call.arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
                format!("Apply surgical patch to: {}", p)
            }
            _ => format!("Invoke tool: {}", call.name),
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Server-generated cryptographically unique single-use token
        let approval_token = format!("appr_{}", uuid::Uuid::new_v4());

        // Cryptographic payload hash (SHA-256 of canonical JSON arguments)
        let arg_bytes = serde_json::to_vec(&call.arguments).unwrap_or_default();
        let payload_hash = {
            let mut hasher = sha2::Sha256::new();
            sha2::Digest::update(&mut hasher, &arg_bytes);
            hex::encode(sha2::Digest::finalize(hasher))
        };

        // Target path hash if the tool call touches a file
        let target_path_hash = call.arguments.get("path")
            .or_else(|| call.arguments.get("target_dir"))
            .or_else(|| call.arguments.get("target_file"))
            .and_then(|v| v.as_str())
            .map(|p| {
                let full = match self.resolve_path(p) {
                    Ok(path) => path,
                    Err(_) => self.workspace_root.join(p),
                };
                let path_bytes = full.to_string_lossy().as_bytes().to_vec();
                let mut hasher = sha2::Sha256::new();
                sha2::Digest::update(&mut hasher, &path_bytes);
                hex::encode(sha2::Digest::finalize(hasher))
            });

        let pending = PendingToolApproval {
            approval_token: approval_token.clone(),
            call_id: call.id.clone(),
            session_id: session_id.to_string(),
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
            description: desc,
            payload_hash,
            target_path_hash,
            created_at: now,
        };

        let mut lock = self.pending_approvals.lock().await;
        // Index strictly by server-generated cryptographically random approval token
        lock.insert(approval_token.clone(), pending.clone());

        pending
    }

    /// Handles a user response to an approval request with high-entropy token requirement and single-consumption enforcement
    pub async fn resolve_approval(&self, token: &str, approved: bool, allow_all: bool) -> Result<ToolResult, String> {
        // Enforce high-entropy token requirement: reject predictable model call.ids
        if !token.starts_with("appr_") {
            return Err(format!(
                "Security violation: Approval requires a high-entropy server-generated token ('appr_<uuid>'), got '{}'",
                token
            ));
        }

        // Enforce single-use: reject if token was already consumed
        {
            let mut consumed = self.consumed_tokens.lock().await;
            if consumed.contains(token) {
                return Err(format!(
                    "Approval token '{}' has already been consumed (single-use enforcement violation)",
                    token
                ));
            }
        }

        let pending = {
            let mut lock = self.pending_approvals.lock().await;
            lock.remove(token)
                .ok_or_else(|| format!("Approval token '{}' not found or invalid", token))?
        };

        // Mark high-entropy token as consumed immediately
        {
            let mut consumed = self.consumed_tokens.lock().await;
            consumed.insert(pending.approval_token.clone());
        }

        // Verify payload immutability against original registered hash
        let current_arg_bytes = serde_json::to_vec(&pending.arguments).unwrap_or_default();
        let current_hash = {
            let mut hasher = sha2::Sha256::new();
            sha2::Digest::update(&mut hasher, &current_arg_bytes);
            hex::encode(sha2::Digest::finalize(hasher))
        };

        if current_hash != pending.payload_hash {
            return Err(format!(
                "Security violation: Tool payload hash mismatch for token '{}'. Registered: {}, Actual: {}",
                pending.approval_token, pending.payload_hash, current_hash
            ));
        }

        if allow_all {
            self.set_auto_approve(true);
        }

        if !approved {
            return Ok(ToolResult {
                call_id: pending.call_id,
                output: "Action rejected by user.".to_string(),
                is_error: true,
            });
        }

        let call = ToolCall {
            id: pending.call_id,
            name: pending.tool_name,
            arguments: pending.arguments,
        };

        Ok(self.execute_tool(&call).await)
    }

    /// Build system prompt with tool calling instructions for general LLMs
    pub fn build_agent_system_prompt(&self) -> String {
        let tools = self.get_available_tools();
        let tools_json = serde_json::to_string_pretty(&tools).unwrap_or_default();
        format!(
            r#"You are Hagibis Autonomous Agent, an expert AI software engineer operating inside a real sovereign codebase.
You have access to the following real tools:
{}

To invoke a tool, output a single JSON block wrapped in ```tool_call``` fences:
```tool_call
{{
  "id": "call_<unique_id>",
  "name": "<tool_name>",
  "arguments": {{ ... }}
}}
```

Guidelines:
1. Always explore and read relevant files before modifying them.
2. When running tests or build commands, inspect stdout/stderr carefully.
3. If errors occur, diagnose root causes and patch files to self-heal.
4. When done, explain your solution clearly to the user without calling further tools.
"#,
            tools_json
        )
    }

    /// Parse any ```tool_call blocks from LLM output
    pub fn parse_tool_call(content: &str) -> Option<ToolCall> {
        if let Some(start) = content.find("```tool_call") {
            let after = &content[start + 12..];
            if let Some(end) = after.find("```") {
                let json_str = after[..end].trim();
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let id = val.get("id").and_then(|v| v.as_str()).unwrap_or("call_auto").to_string();
                    let name = val.get("name").and_then(|v| v.as_str())?.to_string();
                    let arguments = val.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                    return Some(ToolCall { id, name, arguments });
                }
            }
        }
        None
    }

    /// Resolve and canonicalize relative path safely within workspace root.
    /// Strictly blocks `../` path traversal, absolute path escape attempts, and symlink escapes.
    /// If target does not exist yet (e.g. for write), verifies lowest existing ancestor resolves within workspace root.
    pub fn resolve_path(&self, rel: &str) -> Result<PathBuf, String> {
        let canonical_root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());

        if rel.is_empty() || rel == "." || rel == "/" {
            return Ok(self.workspace_root.clone());
        }

        let path = Path::new(rel);

        // Reject Windows drive prefix / volume prefixes
        for comp in path.components() {
            if let Component::Prefix(_) = comp {
                return Err(format!("Prefix components not allowed in path: '{}'", rel));
            }
        }

        // If path is absolute, check if it is within workspace_root
        let rel_path = if path.is_absolute() {
            if let Ok(inside) = path.strip_prefix(&canonical_root) {
                inside
            } else if let Ok(inside) = path.strip_prefix(&self.workspace_root) {
                inside
            } else {
                return Err(format!(
                    "Path traversal attempt detected: absolute path '{}' is outside workspace root",
                    rel
                ));
            }
        } else {
            path
        };

        // Lexical normalization of components to strictly block `../` escapes
        let mut normalized = PathBuf::new();
        for component in rel_path.components() {
            match component {
                Component::Prefix(_) => {
                    return Err(format!("Prefix components not allowed in path: '{}'", rel));
                }
                Component::RootDir => {
                    continue;
                }
                Component::CurDir => {
                    continue;
                }
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(format!(
                            "Path traversal attempt detected: path '{}' escapes workspace root",
                            rel
                        ));
                    }
                }
                Component::Normal(c) => {
                    normalized.push(c);
                }
            }
        }

        let candidate = self.workspace_root.join(&normalized);

        // Filesystem canonicalization check: verify target or existing ancestor resolves within workspace root
        if candidate.exists() {
            let canonical_cand = candidate
                .canonicalize()
                .map_err(|e| format!("Failed to canonicalize path '{}': {}", candidate.display(), e))?;
            if !canonical_cand.starts_with(&canonical_root) {
                return Err(format!(
                    "Path traversal attempt detected: path '{}' escapes workspace root via symlink",
                    rel
                ));
            }
            Ok(candidate)
        } else {
            // Target file does not exist yet; verify lowest existing ancestor directory
            let mut ancestor = candidate.as_path();
            while !ancestor.exists() {
                match ancestor.parent() {
                    Some(p) => ancestor = p,
                    None => break,
                }
            }
            if ancestor.exists() {
                let canonical_ancestor = ancestor
                    .canonicalize()
                    .map_err(|e| format!("Failed to canonicalize ancestor '{}': {}", ancestor.display(), e))?;
                if !canonical_ancestor.starts_with(&canonical_root) {
                    return Err(format!(
                        "Path traversal attempt detected: ancestor of '{}' escapes workspace root",
                        rel
                    ));
                }
            } else {
                return Err(format!(
                    "Path traversal attempt detected: no existing ancestor found within workspace root for '{}'",
                    rel
                ));
            }
            Ok(candidate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_agent_tool_execution() {
        let dir = tempdir().unwrap();
        let engine = AgentLoopEngine::new(dir.path().to_path_buf());

        // 1. file_write
        let write_call = ToolCall {
            id: "1".to_string(),
            name: "file_write".to_string(),
            arguments: serde_json::json!({
                "path": "hello.txt",
                "content": "Hello World\nLine 2\nLine 3"
            }),
        };
        let res_write = engine.execute_tool(&write_call).await;
        assert!(!res_write.is_error);

        // 2. file_read
        let read_call = ToolCall {
            id: "2".to_string(),
            name: "file_read".to_string(),
            arguments: serde_json::json!({
                "path": "hello.txt",
                "start_line": 2,
                "end_line": 2
            }),
        };
        let res_read = engine.execute_tool(&read_call).await;
        assert_eq!(res_read.output.trim(), "Line 2");

        // 3. file_patch
        let patch_call = ToolCall {
            id: "3".to_string(),
            name: "file_patch".to_string(),
            arguments: serde_json::json!({
                "path": "hello.txt",
                "old_snippet": "Line 2",
                "new_snippet": "Patched Line"
            }),
        };
        let res_patch = engine.execute_tool(&patch_call).await;
        assert!(!res_patch.is_error);

        // Verify patched content
        let read_all = engine.execute_tool(&ToolCall {
            id: "4".to_string(),
            name: "file_read".to_string(),
            arguments: serde_json::json!({ "path": "hello.txt" }),
        }).await;
        assert!(read_all.output.contains("Patched Line"));

        // 4. bash_run
        let bash_call = ToolCall {
            id: "5".to_string(),
            name: "bash_run".to_string(),
            arguments: serde_json::json!({ "command": "echo 'Sovereign Hagibis'" }),
        };
        let res_bash = engine.execute_tool(&bash_call).await;
        assert!(!res_bash.is_error);
        assert!(res_bash.output.contains("Sovereign Hagibis"));
    }

    #[tokio::test]
    async fn test_immutable_approval_token_lifecycle_and_single_use() {
        let dir = tempdir().unwrap();
        let engine = AgentLoopEngine::new(dir.path().to_path_buf());

        let call = ToolCall {
            id: "client_call_99".to_string(),
            name: "bash_run".to_string(),
            arguments: serde_json::json!({ "command": "echo 'approval test'" }),
        };

        // 1. Register approval
        let pending = engine.register_pending_approval_with_session(&call, "session-xyz").await;

        // Verify server-generated UUIDv4 token format
        assert!(pending.approval_token.starts_with("appr_"));
        assert_ne!(pending.approval_token, call.id);
        assert_eq!(pending.session_id, "session-xyz");
        assert_eq!(pending.payload_hash.len(), 64);

        // Verify predictable model call_id CANNOT resolve approval
        let predict_result = engine.resolve_approval(&call.id, true, false).await;
        assert!(predict_result.is_err(), "Predictable model call.id must not resolve approval");
        let predict_err = predict_result.unwrap_err();
        assert!(predict_err.contains("Security violation") || predict_err.contains("high-entropy"));

        // 2. Resolve with server-generated token
        let result = engine.resolve_approval(&pending.approval_token, true, false).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("approval test"));

        // 3. Second resolution of the same token MUST be strictly rejected
        let replay_result = engine.resolve_approval(&pending.approval_token, true, false).await;
        assert!(replay_result.is_err(), "Consumed approval token must be strictly rejected");
        let err_msg = replay_result.unwrap_err();
        assert!(err_msg.contains("already been consumed"));

        // 4. Arbitrary unapproved or forged token must fail
        let fake_result = engine.resolve_approval("appr_fake_unregistered_token", true, false).await;
        assert!(fake_result.is_err());
    }

    #[tokio::test]
    async fn test_agent_resolve_path_traversal_blocked() {
        let dir = tempdir().unwrap();
        let engine = AgentLoopEngine::new(dir.path().to_path_buf());

        // 1. Direct resolve_path tests
        assert!(engine.resolve_path("../../etc/passwd").is_err());
        assert!(engine.resolve_path("../../../shadow").is_err());
        assert!(engine.resolve_path("sub/../../escaped").is_err());
        assert!(engine.resolve_path("/etc/passwd").is_err());
        assert!(engine.resolve_path("a/b/../../../../outside").is_err());

        // Valid relative paths
        assert!(engine.resolve_path("valid/sub/file.txt").is_ok());
        assert!(engine.resolve_path("./valid/../valid/file.txt").is_ok());
        assert!(engine.resolve_path("").is_ok());
        assert!(engine.resolve_path(".").is_ok());

        // 2. Tool calls blocked
        let read_call = ToolCall {
            id: "t1".to_string(),
            name: "file_read".to_string(),
            arguments: serde_json::json!({ "path": "../../etc/passwd" }),
        };
        let res_read = engine.execute_tool(&read_call).await;
        assert!(res_read.is_error);
        assert!(res_read.output.contains("Path resolution error"));

        let write_call = ToolCall {
            id: "t2".to_string(),
            name: "file_write".to_string(),
            arguments: serde_json::json!({
                "path": "../../../escaped_write.txt",
                "content": "malicious"
            }),
        };
        let res_write = engine.execute_tool(&write_call).await;
        assert!(res_write.is_error);
        assert!(res_write.output.contains("Path resolution error"));

        let patch_call = ToolCall {
            id: "t3".to_string(),
            name: "file_patch".to_string(),
            arguments: serde_json::json!({
                "path": "../../escaped_patch.txt",
                "old_snippet": "a",
                "new_snippet": "b"
            }),
        };
        let res_patch = engine.execute_tool(&patch_call).await;
        assert!(res_patch.is_error);
        assert!(res_patch.output.contains("Path resolution error"));

        let list_call = ToolCall {
            id: "t4".to_string(),
            name: "list_dir".to_string(),
            arguments: serde_json::json!({ "path": "../../" }),
        };
        let res_list = engine.execute_tool(&list_call).await;
        assert!(res_list.is_error);
        assert!(res_list.output.contains("Path resolution error"));
    }

    #[tokio::test]
    async fn test_consumed_tokens_tracker_bounding_and_ttl() {
        let mut tracker = ConsumedTokenTracker::new(5, Duration::from_millis(50));

        // Insert 5 tokens
        for i in 0..5 {
            tracker.insert(format!("appr_token_{}", i));
        }
        assert_eq!(tracker.len(), 5);
        assert!(tracker.contains("appr_token_0"));

        // Insert 6th token -> FIFO evicts oldest ("appr_token_0")
        tracker.insert("appr_token_5".to_string());
        assert_eq!(tracker.len(), 5);
        assert!(!tracker.contains("appr_token_0"));
        assert!(tracker.contains("appr_token_5"));

        // Test TTL eviction
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!tracker.contains("appr_token_1"));
        assert_eq!(tracker.len(), 0);
    }
}
