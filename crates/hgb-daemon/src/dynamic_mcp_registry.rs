//! # Dynamic MCP Plugin Registry (`hgb-daemon`)
//!
//! Provides a thread-safe, hot-reloading registry for WebAssembly-backed Model Context
//! Protocol (MCP) tools:
//!
//! 1. **Thread-Safe Tool Registry**: Stores dynamically synthesized and compiled WASM plugins.
//! 2. **Hot-Reloading into active `McpSseServer`**: Injects new tools directly into running
//!    Server-Sent Events server without restarting the daemon process.
//! 3. **Automatic Prompt-Driven Tool Instantiation**:
//!    - Intercepts `hgb_mcp_request_tool(tool_name, intent, schema)` requests or unrecognized tool calls.
//!    - Generates and compiles WASM tool bytecode via `AutonomousWasmPluginHealer` and `ZigWasmAgentCompiler`.
//!    - Synchronizes declarative entries into `hagibis.mcp.json`.
//!    - Exposes tools immediately in `/mcp/sse` `tools/list` and handles immediate invocation.
//! 4. **Live SSE Broadcasting**: Emits `notifications/tools/list_changed` to connected MCP clients
//!    (Cursor, Claude Desktop, Windsurf, Cline, Roo Code).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::sse::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::RwLock as StdRwLock;
use tokio::sync::RwLock;

use crate::mcp_sse_server::McpSseState;
use hgb_core::autonomous_wasm_plugin_healer::AutonomousWasmPluginHealer;
use hgb_core::error::{HgbError, Result};
use hgb_core::zig_wasm_agent_compiler::ZigWasmAgentCompiler;

/// Metadata for a dynamically registered WebAssembly MCP tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicMcpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub wasm_path: PathBuf,
    pub wasm_size_bytes: usize,
    pub language: String,
    pub created_at_epoch_ms: u64,
    pub is_active: bool,
    pub invocations_count: u64,
    pub last_invoked_epoch_ms: u64,
}

/// Comprehensive performance benchmark telemetry for an MCP tool against hgb-core SLA (<5ms)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolBenchmarkMetrics {
    pub tool_name: String,
    pub iterations: usize,
    pub p50_latency_ms: f64,
    pub p90_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub min_latency_ms: f64,
    pub max_latency_ms: f64,
    pub avg_latency_ms: f64,
    pub memory_kb: f64,
    pub fuel_consumed: u64,
    pub meets_sla: bool,
    pub sla_threshold_ms: f64,
}

/// Status telemetry report for DynamicMcpRegistry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicMcpRegistryReport {
    pub total_tools: usize,
    pub active_tools: usize,
    pub total_instantiations: u64,
    pub total_invocations: u64,
    pub mcp_json_path: String,
    pub tools: Vec<DynamicMcpTool>,
}

/// Dynamic Model Context Protocol (MCP) Plugin Registry
pub struct DynamicMcpRegistry {
    pub workspace_root: PathBuf,
    pub mcp_json_path: PathBuf,
    pub tools: Arc<RwLock<HashMap<String, DynamicMcpTool>>>,
    pub wasm_healer: Arc<AutonomousWasmPluginHealer>,
    pub zig_compiler: Arc<ZigWasmAgentCompiler>,
    pub sse_state: Arc<StdRwLock<Option<Arc<McpSseState>>>>,
    pub total_instantiations: AtomicU64,
    pub total_invocations: AtomicU64,
}

impl DynamicMcpRegistry {
    /// Create new DynamicMcpRegistry bound to workspace root and WASM healer
    pub fn new<P: AsRef<Path>>(
        workspace_root: P,
        wasm_healer: Arc<AutonomousWasmPluginHealer>,
    ) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        let mcp_json_path = ws.join("hagibis.mcp.json");
        let zig_compiler = Arc::new(ZigWasmAgentCompiler::new());

        Self {
            workspace_root: ws,
            mcp_json_path,
            tools: Arc::new(RwLock::new(HashMap::new())),
            wasm_healer,
            zig_compiler,
            sse_state: Arc::new(StdRwLock::new(None)),
            total_instantiations: AtomicU64::new(0),
            total_invocations: AtomicU64::new(0),
        }
    }

    /// Attach custom ZigWasmAgentCompiler
    pub fn with_compiler(mut self, compiler: Arc<ZigWasmAgentCompiler>) -> Self {
        self.zig_compiler = compiler;
        self
    }

    /// Attach custom path to `hagibis.mcp.json` (useful for unit testing)
    pub fn with_mcp_json_path<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.mcp_json_path = path.as_ref().to_path_buf();
        self
    }

    /// Attach McpSseState for live SSE broadcasting
    pub fn with_sse_state(self, sse_state: Arc<McpSseState>) -> Self {
        {
            let mut guard = self.sse_state.write().unwrap();
            *guard = Some(sse_state);
        }
        self
    }

    /// Asynchronously attach active McpSseState
    pub async fn attach_sse_state(&self, sse_state: Arc<McpSseState>) {
        let mut guard = self.sse_state.write().unwrap();
        *guard = Some(sse_state);
    }

    /// Synchronously attach active McpSseState
    pub fn attach_sse_state_sync(&self, sse_state: Arc<McpSseState>) {
        let mut guard = self.sse_state.write().unwrap();
        *guard = Some(sse_state);
    }

    /// Check if a specific dynamic tool is registered
    pub async fn has_tool(&self, name: &str) -> bool {
        let tools = self.tools.read().await;
        tools.contains_key(name)
    }

    /// Dynamically unload an MCP tool at runtime without restarting hgbd (Pillar 4.1)
    pub async fn unload_tool(&self, name: &str, remove_from_mcp_json: bool) -> Result<bool> {
        let removed = {
            let mut tools = self.tools.write().await;
            tools.remove(name).is_some()
        };

        if removed {
            if remove_from_mcp_json && self.mcp_json_path.exists() {
                if let Ok(content) = fs::read_to_string(&self.mcp_json_path) {
                    if let Ok(mut val) = serde_json::from_str::<Value>(&content) {
                        if let Some(tools_obj) = val.get_mut("tools").and_then(|t| t.as_object_mut()) {
                            tools_obj.remove(name);
                            if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                                let _ = fs::write(&self.mcp_json_path, pretty);
                            }
                        }
                    }
                }
            }
            self.broadcast_tools_changed().await;
        }

        Ok(removed)
    }

    /// Retrieve a cloned copy of a dynamic tool's metadata
    pub async fn get_tool(&self, name: &str) -> Option<DynamicMcpTool> {
        let tools = self.tools.read().await;
        tools.get(name).cloned()
    }

    /// List all dynamically registered tools
    pub async fn list_tools(&self) -> Vec<DynamicMcpTool> {
        let tools = self.tools.read().await;
        tools.values().cloned().collect()
    }

    /// List dynamic tools formatted for MCP `tools/list` protocol specification
    pub async fn list_tools_mcp_format(&self) -> Vec<Value> {
        let tools = self.tools.read().await;
        tools
            .values()
            .filter(|t| t.is_active)
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema
                })
            })
            .collect()
    }

    /// Manually register a new DynamicMcpTool
    pub async fn register_tool(&self, tool: DynamicMcpTool) -> Result<()> {
        let name = tool.name.clone();
        {
            let mut tools = self.tools.write().await;
            tools.insert(name.clone(), tool);
        }
        self.broadcast_tools_changed().await;
        Ok(())
    }

    /// Synchronize existing tools from `hagibis.mcp.json` manifest into registry
    pub async fn sync_with_mcp_json(&self) -> Result<usize> {
        if !self.mcp_json_path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&self.mcp_json_path).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to read {}: {}", self.mcp_json_path.display(), e),
            ))
        })?;

        let parsed: Value = serde_json::from_str(&content).map_err(|e| {
            HgbError::Serialization(format!("Invalid JSON in {}: {}", self.mcp_json_path.display(), e))
        })?;

        let mut loaded = 0;
        if let Some(tools_obj) = parsed.get("tools").and_then(|t| t.as_object()) {
            let mut tools_guard = self.tools.write().await;
            for (tool_name, tool_val) in tools_obj {
                if !tools_guard.contains_key(tool_name) {
                    let desc = tool_val
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Declarative MCP tool from hagibis.mcp.json")
                        .to_string();

                    let schema = if let Some(params) = tool_val.get("parameters") {
                        if params.get("type").is_some() {
                            params.clone()
                        } else {
                            serde_json::json!({
                                "type": "object",
                                "properties": params
                            })
                        }
                    } else if let Some(schema) = tool_val.get("inputSchema") {
                        schema.clone()
                    } else {
                        serde_json::json!({ "type": "object" })
                    };

                    let wasm_path = self
                        .workspace_root
                        .join(".hgb")
                        .join("plugins")
                        .join("wasm")
                        .join(format!("{}.wasm", tool_name));

                    let size = if wasm_path.exists() {
                        fs::metadata(&wasm_path).map(|m| m.len() as usize).unwrap_or(0)
                    } else {
                        0
                    };

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    tools_guard.insert(
                        tool_name.clone(),
                        DynamicMcpTool {
                            name: tool_name.clone(),
                            description: desc,
                            input_schema: schema,
                            wasm_path,
                            wasm_size_bytes: size,
                            language: "wasm".to_string(),
                            created_at_epoch_ms: now,
                            is_active: true,
                            invocations_count: 0,
                            last_invoked_epoch_ms: 0,
                        },
                    );
                    loaded += 1;
                }
            }
        }

        Ok(loaded)
    }

    /// Discover federated tools dynamically on startup from local plugins (.hgb/plugins/*.wasm),
    /// custom scripts (.hgb/scripts/), and system utilities, merging into active tool sets.
    pub async fn discover_federated_tools(&self) -> Result<usize> {
        let mut discovered = 0usize;

        // 1. First sync with declarative hagibis.mcp.json
        if let Ok(c) = self.sync_with_mcp_json().await {
            discovered += c;
        }

        // 2. Scan .hgb/plugins and .hgb/plugins/wasm for .wasm binaries
        let search_dirs = [
            self.workspace_root.join(".hgb").join("plugins"),
            self.workspace_root.join(".hgb").join("plugins").join("wasm"),
            self.workspace_root.join(".hgb").join("wasm_tools"),
        ];

        for dir in search_dirs {
            if !dir.is_dir() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("wasm") {
                        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        if stem.is_empty() {
                            continue;
                        }
                        let tool_name = format!("wasm_{}", stem.replace('-', "_"));
                        let mut tools_guard = self.tools.write().await;
                        if !tools_guard.contains_key(&tool_name) {
                            let size = fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
                            let tool = DynamicMcpTool {
                                name: tool_name.clone(),
                                description: format!("Federated WebAssembly plugin tool dynamically discovered at {}", path.display()),
                                input_schema: serde_json::json!({
                                    "type": "object",
                                    "properties": {
                                        "input": { "type": "string", "description": "Invocation payload" }
                                    }
                                }),
                                wasm_path: path.clone(),
                                wasm_size_bytes: size,
                                language: "wasm".to_string(),
                                created_at_epoch_ms: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                is_active: true,
                                invocations_count: 0,
                                last_invoked_epoch_ms: 0,
                            };
                            tools_guard.insert(tool_name.clone(), tool);
                            discovered += 1;
                        }
                    }
                }
            }
        }

        // 3. Scan .hgb/scripts for custom shell scripts
        let script_dir = self.workspace_root.join(".hgb").join("scripts");
        if script_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&script_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && (path.extension().and_then(|e| e.to_str()) == Some("sh") || path.extension().and_then(|e| e.to_str()) == Some("bash")) {
                        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        let tool_name = format!("script_{}", stem.replace('-', "_"));
                        let mut tools_guard = self.tools.write().await;
                        if !tools_guard.contains_key(&tool_name) {
                            let size = fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
                            let tool = DynamicMcpTool {
                                name: tool_name.clone(),
                                description: format!("Federated custom script tool discovered at {}", path.display()),
                                input_schema: serde_json::json!({
                                    "type": "object",
                                    "properties": {
                                        "args": { "type": "string", "description": "Script arguments" }
                                    }
                                }),
                                wasm_path: path.clone(),
                                wasm_size_bytes: size,
                                language: "bash".to_string(),
                                created_at_epoch_ms: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                is_active: true,
                                invocations_count: 0,
                                last_invoked_epoch_ms: 0,
                            };
                            tools_guard.insert(tool_name.clone(), tool);
                            discovered += 1;
                        }
                    }
                }
            }
        }

        if discovered > 0 {
            self.broadcast_tools_changed().await;
        }

        Ok(discovered)
    }

    /// Persist or update an MCP tool entry in `hagibis.mcp.json`
    pub async fn write_tool_to_mcp_json(
        &self,
        name: &str,
        description: &str,
        schema: &Value,
    ) -> Result<()> {
        let mut manifest_val = if self.mcp_json_path.exists() {
            let data = fs::read_to_string(&self.mcp_json_path).unwrap_or_else(|_| "{}".to_string());
            serde_json::from_str(&data).unwrap_or_else(|_| serde_json::json!({ "tools": {} }))
        } else {
            serde_json::json!({ "tools": {} })
        };

        if manifest_val.get("tools").is_none() {
            manifest_val["tools"] = serde_json::json!({});
        }

        let parameters = if let Some(props) = schema.get("properties") {
            props.clone()
        } else {
            schema.clone()
        };

        manifest_val["tools"][name] = serde_json::json!({
            "description": description,
            "parameters": parameters
        });

        if let Some(parent) = self.mcp_json_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let json_bytes = serde_json::to_string_pretty(&manifest_val)
            .map_err(|e| HgbError::Serialization(format!("Serialization error: {}", e)))?;

        fs::write(&self.mcp_json_path, json_bytes).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to write {}: {}", self.mcp_json_path.display(), e),
            ))
        })?;

        Ok(())
    }

    /// Automatic Prompt-Driven Tool Instantiation:
    /// 1. Synthesizes tool source code based on name and natural language intent.
    /// 2. Compiles source code to WASM bytecode via `AutonomousWasmPluginHealer` / `ZigWasmAgentCompiler`.
    /// 3. Validates and signs WASM module, injecting into running `WasmHotPool`.
    /// 4. Synchronizes declarative entry into `hagibis.mcp.json`.
    /// 5. Registers tool into thread-safe registry and broadcasts SSE tool list update.
    pub async fn instantiate_and_register_tool(
        &self,
        tool_name: &str,
        intent: &str,
        custom_schema: Option<Value>,
    ) -> Result<DynamicMcpTool> {
        let sanitized_name = sanitize_tool_name(tool_name);
        if sanitized_name.is_empty() {
            return Err(HgbError::InvalidInput("Tool name cannot be empty".into()));
        }

        // 1. Synthesize code on the fly
        let synthesized = self
            .wasm_healer
            .synthesize_micro_tool_code(&sanitized_name, intent);

        // 2. Compile and inject into WasmHotPool
        let (_cert, wasm_path, _compile_us) = self.wasm_healer.compile_and_inject(
            &sanitized_name,
            &synthesized.source_code,
            &synthesized.description,
        )?;

        let wasm_size_bytes = fs::metadata(&wasm_path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);

        // 3. Resolve input schema
        let input_schema = custom_schema.unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Primary argument for the synthesized tool"
                    }
                }
            })
        });

        // 4. Record to hagibis.mcp.json
        let _ = self
            .write_tool_to_mcp_json(&sanitized_name, &synthesized.description, &input_schema)
            .await;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let tool = DynamicMcpTool {
            name: sanitized_name.clone(),
            description: synthesized.description,
            input_schema,
            wasm_path,
            wasm_size_bytes,
            language: synthesized.language,
            created_at_epoch_ms: now,
            is_active: true,
            invocations_count: 0,
            last_invoked_epoch_ms: 0,
        };

        // 5. Store in thread-safe registry
        {
            let mut tools = self.tools.write().await;
            tools.insert(sanitized_name.clone(), tool.clone());
        }

        self.total_instantiations.fetch_add(1, Ordering::Relaxed);

        // 6. Broadcast SSE notifications/tools/list_changed to connected clients
        self.broadcast_tools_changed().await;

        Ok(tool)
    }

    /// Execute a dynamically registered WebAssembly tool by name
    pub async fn invoke_tool(&self, name: &str, args: &Value) -> Result<Value> {
        let (wasm_path, _) = {
            let tools = self.tools.read().await;
            let tool = tools.get(name).ok_or_else(|| {
                HgbError::Execution(format!("Dynamic tool '{}' not registered", name))
            })?;
            (tool.wasm_path.clone(), tool.name.clone())
        };

        let hot_pool = self.wasm_healer.hot_pool();
        let exec_report = hot_pool.execute(&wasm_path, args)?;

        // Update invocation counters
        {
            let mut tools = self.tools.write().await;
            if let Some(tool) = tools.get_mut(name) {
                tool.invocations_count += 1;
                tool.last_invoked_epoch_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
            }
        }
        self.total_invocations.fetch_add(1, Ordering::Relaxed);

        Ok(exec_report.output)
    }

    /// Intercept `hgb_mcp_request_tool` tool call or handle unrecognized tools
    pub async fn handle_mcp_request_tool_call(&self, args: &Value) -> Result<Value> {
        let tool_name = args
            .get("tool_name")
            .or_else(|| args.get("name"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing 'tool_name' in request".into()))?;

        let intent = args
            .get("intent")
            .or_else(|| args.get("task_intent"))
            .or_else(|| args.get("prompt"))
            .and_then(|v| v.as_str())
            .unwrap_or("Dynamic on-demand tool requested by AI agent");

        let custom_schema = args
            .get("schema")
            .or_else(|| args.get("input_schema"))
            .cloned();

        let tool = self
            .instantiate_and_register_tool(tool_name, intent, custom_schema)
            .await?;

        Ok(serde_json::json!({
            "status": "instantiated",
            "tool_name": tool.name,
            "description": tool.description,
            "wasm_path": tool.wasm_path.to_string_lossy(),
            "wasm_size_bytes": tool.wasm_size_bytes,
            "language": tool.language,
            "is_active": tool.is_active,
            "message": format!("Tool '{}' synthesized and dynamically hot-reloaded into MCP registry.", tool.name)
        }))
    }

    /// Dispatch tool call:
    /// - If `name == "hgb_mcp_request_tool"`, synthesizes and registers tool.
    /// - If tool exists in registry, executes it.
    /// - If unrecognized tool call, automatically instantiates on the fly, registers, and executes!
    pub async fn intercept_or_dispatch(&self, name: &str, args: &Value) -> Result<Value> {
        if name == "hgb_mcp_request_tool" {
            return self.handle_mcp_request_tool_call(args).await;
        }

        if self.has_tool(name).await {
            return self.invoke_tool(name, args).await;
        }

        // Automatic prompt-driven instantiation for unrecognized tool call
        let intent = format!(
            "Auto-synthesized tool for unrecognized tool invocation '{}' with arguments: {}",
            name, args
        );

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "input": { "type": "string" }
            }
        });

        let tool = self
            .instantiate_and_register_tool(name, &intent, Some(schema))
            .await?;

        // Immediately invoke newly registered tool
        self.invoke_tool(&tool.name, args).await
    }

    /// Emit Server-Sent Events (SSE) `notifications/tools/list_changed` to all connected clients
    pub async fn broadcast_tools_changed(&self) {
        let sse_guard = self.sse_state.read().unwrap().clone();
        if let Some(ref sse_state) = sse_guard {
            let event = Event::default().event("message").data(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/tools/list_changed",
                    "params": {}
                })
                .to_string(),
            );

            let sessions = sse_state.sessions.read().await;
            for (session_id, tx) in sessions.iter() {
                if tx.send(event.clone()).is_ok() {
                    sse_state
                        .total_streamed_events
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    let _ = session_id;
                }
            }
        }
    }

    /// Generate telemetry status report for the registry
    pub async fn report(&self) -> DynamicMcpRegistryReport {
        let tools = self.tools.read().await;
        let tool_list: Vec<DynamicMcpTool> = tools.values().cloned().collect();
        let active_count = tool_list.iter().filter(|t| t.is_active).count();

        DynamicMcpRegistryReport {
            total_tools: tool_list.len(),
            active_tools: active_count,
            total_instantiations: self.total_instantiations.load(Ordering::Relaxed),
            total_invocations: self.total_invocations.load(Ordering::Relaxed),
            mcp_json_path: self.mcp_json_path.to_string_lossy().to_string(),
            tools: tool_list,
        }
    }
}

/// Helper to sanitize tool names for filesystem and MCP compatibility
fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hgb_core::wasm_hot_pool::WasmHotPool;

    fn create_test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hgb_dyn_reg_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn test_dynamic_mcp_registry_instantiation_and_sync() {
        let ws_root = create_test_dir();
        let plugin_dir = ws_root.join(".hgb/plugins/wasm");
        let mcp_json = ws_root.join("hagibis.mcp.json");

        let hot_pool = Arc::new(WasmHotPool::new(4).expect("hot pool"));
        let healer = Arc::new(
            AutonomousWasmPluginHealer::new(hot_pool, &plugin_dir).expect("healer"),
        );

        let registry = DynamicMcpRegistry::new(&ws_root, healer)
            .with_mcp_json_path(&mcp_json);

        // 1. Instantiate on-demand tool: fibonacci
        let tool = registry
            .instantiate_and_register_tool(
                "hgb_dynamic_fib",
                "compute fibonacci sequence in WebAssembly",
                Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "n": { "type": "integer" }
                    }
                })),
            )
            .await
            .expect("instantiate tool");

        assert_eq!(tool.name, "hgb_dynamic_fib");
        assert!(tool.wasm_path.exists());
        assert!(tool.wasm_size_bytes > 0);
        assert!(registry.has_tool("hgb_dynamic_fib").await);

        // 2. Verify hagibis.mcp.json was written and contains the tool
        assert!(mcp_json.exists());
        let mcp_content = fs::read_to_string(&mcp_json).expect("read mcp json");
        assert!(mcp_content.contains("hgb_dynamic_fib"));
        assert!(mcp_content.contains("fibonacci"));

        // 3. Verify MCP tools list format includes the tool
        let mcp_tools = registry.list_tools_mcp_format().await;
        assert_eq!(mcp_tools.len(), 1);
        assert_eq!(mcp_tools[0]["name"], "hgb_dynamic_fib");

        // 4. Test immediate invocation of synthesized tool
        let invoke_res = registry
            .invoke_tool(
                "hgb_dynamic_fib",
                &serde_json::json!({ "n": 10 }),
            )
            .await;
        assert!(invoke_res.is_ok(), "Invocation must succeed: {:?}", invoke_res);

        // 5. Test prompt-driven interception: hgb_mcp_request_tool
        let req_args = serde_json::json!({
            "tool_name": "hgb_quick_calc",
            "intent": "evaluate math expression and return numerical result",
            "schema": {
                "type": "object",
                "properties": {
                    "expr": { "type": "string" }
                }
            }
        });
        let req_resp = registry
            .intercept_or_dispatch("hgb_mcp_request_tool", &req_args)
            .await
            .expect("request tool dispatch");

        assert_eq!(req_resp["status"], "instantiated");
        assert_eq!(req_resp["tool_name"], "hgb_quick_calc");
        assert!(registry.has_tool("hgb_quick_calc").await);

        // 6. Test sync from hagibis.mcp.json into a new registry instance
        let registry2 = DynamicMcpRegistry::new(&ws_root, registry.wasm_healer.clone())
            .with_mcp_json_path(&mcp_json);
        let synced = registry2.sync_with_mcp_json().await.expect("sync");
        assert!(synced >= 2, "Synced at least 2 tools, got {}", synced);
        assert!(registry2.has_tool("hgb_dynamic_fib").await);
        assert!(registry2.has_tool("hgb_quick_calc").await);

        // 7. Verify status report
        let report = registry2.report().await;
        assert_eq!(report.total_tools, 2);

        let _ = fs::remove_dir_all(&ws_root);
    }

    #[tokio::test]
    async fn test_unrecognized_tool_auto_instantiation() {
        let ws_root = create_test_dir();
        let plugin_dir = ws_root.join(".hgb/plugins/wasm");
        let mcp_json = ws_root.join("hagibis.mcp.json");

        let hot_pool = Arc::new(WasmHotPool::new(4).expect("hot pool"));
        let healer = Arc::new(
            AutonomousWasmPluginHealer::new(hot_pool, &plugin_dir).expect("healer"),
        );

        let registry = DynamicMcpRegistry::new(&ws_root, healer)
            .with_mcp_json_path(&mcp_json);

        // Call unrecognized tool: should auto-synthesize, compile, register and invoke!
        let result = registry
            .intercept_or_dispatch(
                "hgb_unrecognized_hasher",
                &serde_json::json!({ "input": "test_hash" }),
            )
            .await;

        assert!(result.is_ok(), "Auto-instantiation must succeed: {:?}", result);
        assert!(registry.has_tool("hgb_unrecognized_hasher").await);

        let _ = fs::remove_dir_all(&ws_root);
    }

    #[tokio::test]
    async fn test_federated_tool_discovery() {
        let ws_root = create_test_dir();
        let plugin_dir = ws_root.join(".hgb/plugins/wasm");
        let script_dir = ws_root.join(".hgb/scripts");
        let _ = fs::create_dir_all(&plugin_dir);
        let _ = fs::create_dir_all(&script_dir);

        // Create a dummy discovered WASM plugin and script
        let dummy_wasm = plugin_dir.join("image_scaler.wasm");
        let _ = fs::write(&dummy_wasm, b"\0asm\x01\0\0\0");

        let dummy_script = script_dir.join("clean_cache.sh");
        let _ = fs::write(&dummy_script, b"#!/bin/bash\necho ok");

        let hot_pool = Arc::new(WasmHotPool::new(2).expect("hot pool"));
        let healer = Arc::new(
            AutonomousWasmPluginHealer::new(hot_pool, &plugin_dir).expect("healer"),
        );

        let registry = DynamicMcpRegistry::new(&ws_root, healer);
        let discovered = registry.discover_federated_tools().await.expect("discover");
        assert!(discovered >= 2, "Expected at least 2 discovered tools, got {}", discovered);
        assert!(registry.has_tool("wasm_image_scaler").await);
        assert!(registry.has_tool("script_clean_cache").await);

        let _ = fs::remove_dir_all(&ws_root);
    }
}
