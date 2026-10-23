//! # Dynamic WebAssembly Tool Hot-Swapping & Native Multi-Modal MCP Streaming
//!
//! Live, zero-downtime tool compilation, validation, and instantaneous MCP hot-swapping:
//! 1. Autonomous AI agents dynamically generate Rust or Zig tool code at runtime.
//! 2. Compiles code directly to WebAssembly (`wasm32-wasip1` / `wasm32-wasi`).
//! 3. Validates binary integrity using Wasmtime sandboxed module verification.
//! 4. Synthesizes declarative MCP JSON manifests with input schemas into `.hgb/mcp_plugins/`.
//! 5. Registers tool into live running daemon MCP catalog without restarting the process.
//! 6. Emits Server-Sent Events (SSE) `notifications/tools/list_changed` to connected IDE clients.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use wasmtime::{Config, Engine, Module};

use crate::mcp_hot_reloader::{McpHotReloader, McpPluginManifest};
use crate::mcp_sse_server::McpSseState;
use hgb_core::error::{HgbError, Result};
use hgb_core::wasm_plugin_kernel::WasmPluginInstance;

/// Supported languages for dynamic tool synthesis
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DynamicSourceLanguage {
    Rust,
    Zig,
}

impl DynamicSourceLanguage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Zig => "zig",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "zig" => Self::Zig,
            _ => Self::Rust,
        }
    }
}

/// Request payload for generating and hot-swapping a dynamic tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolRequest {
    pub name: String,
    pub description: String,
    pub language: DynamicSourceLanguage,
    pub source_code: String,
    pub input_schema: Option<Value>,
    pub author: Option<String>,
}

/// Result returned after successful dynamic compilation and hot-swap
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicCompilationResult {
    pub name: String,
    pub wasm_path: String,
    pub manifest_path: String,
    pub wasm_size_bytes: usize,
    pub compile_duration_ms: u64,
    pub compiler_used: String,
    pub target_triple: String,
    pub validated_with_wasmtime: bool,
    pub sse_notifications_sent: usize,
    pub is_active: bool,
    pub registered_at_epoch_ms: u64,
}

/// Summary metadata for a dynamically hot-swapped tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolSummary {
    pub name: String,
    pub description: String,
    pub language: String,
    pub wasm_size_bytes: usize,
    pub manifest_path: String,
    pub is_active: bool,
    pub compiled_at_epoch_ms: u64,
}

/// Status telemetry report for the dynamic tool hot-swapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicHotSwapperReport {
    pub plugins_directory: String,
    pub total_tools: usize,
    pub active_tools: usize,
    pub tools: Vec<DynamicToolSummary>,
    pub rustc_available: bool,
    pub zig_available: bool,
    pub default_target: String,
}

/// Dynamic Tool Hot-Swapping Engine
pub struct DynamicToolHotSwapper {
    pub workspace_root: PathBuf,
    pub plugins_dir: PathBuf,
    pub cache_build_dir: PathBuf,
    pub hot_reloader: Arc<McpHotReloader>,
    pub sse_state: Option<Arc<McpSseState>>,
    wasm_engine: Engine,
    dynamic_registry: Arc<RwLock<HashMap<String, DynamicCompilationResult>>>,
}

impl DynamicToolHotSwapper {
    /// Initialize with workspace root and optional SSE broadcasting state
    pub fn new<P: AsRef<Path>>(workspace_root: P, sse_state: Option<Arc<McpSseState>>) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        let hot_reloader = Arc::new(McpHotReloader::new(&ws, sse_state.clone()));
        Self::with_reloader(ws, hot_reloader, sse_state)
    }

    /// Initialize with shared MCP hot-reloader instance
    pub fn with_reloader(
        workspace_root: PathBuf,
        hot_reloader: Arc<McpHotReloader>,
        sse_state: Option<Arc<McpSseState>>,
    ) -> Self {
        let plugins_dir = workspace_root.join(".hgb").join("mcp_plugins");
        let cache_build_dir = workspace_root.join(".hgb").join("cache").join("tool_build");
        let _ = fs::create_dir_all(&plugins_dir);
        let _ = fs::create_dir_all(&cache_build_dir);

        let mut config = Config::new();
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        config.consume_fuel(true);
        let wasm_engine = Engine::new(&config).unwrap_or_else(|_| Engine::default());

        Self {
            workspace_root,
            plugins_dir,
            cache_build_dir,
            hot_reloader,
            sse_state,
            wasm_engine,
            dynamic_registry: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Compile generated source code on-the-fly, validate Wasm binary, generate manifest,
    /// hot-register into daemon MCP catalog, and emit SSE list change event.
    pub async fn compile_and_hotswap(
        &self,
        request: &DynamicToolRequest,
    ) -> Result<DynamicCompilationResult> {
        let start_time = Instant::now();
        let sanitized_name = sanitize_tool_name(&request.name);

        if sanitized_name.is_empty() {
            return Err(HgbError::InvalidInput("Tool name cannot be empty".into()));
        }

        let build_scratch = self.cache_build_dir.join(&sanitized_name);
        let _ = fs::create_dir_all(&build_scratch);

        // 1. Compile source code to WASM binary
        let (wasm_temp_path, compiler_used, target_triple) = match request.language {
            DynamicSourceLanguage::Rust => {
                self.compile_rust_to_wasm(&sanitized_name, &request.source_code, &build_scratch)?
            }
            DynamicSourceLanguage::Zig => {
                self.compile_zig_to_wasm(&sanitized_name, &request.source_code, &build_scratch)?
            }
        };

        // 2. Read compiled WASM bytes and validate with Wasmtime
        let wasm_bytes = fs::read(&wasm_temp_path).map_err(|e| {
            HgbError::Execution(format!(
                "Failed to read compiled WASM artifact at {}: {}",
                wasm_temp_path.display(),
                e
            ))
        })?;

        let _module = Module::new(&self.wasm_engine, &wasm_bytes).map_err(|e| {
            HgbError::Execution(format!(
                "Wasmtime module validation failed for dynamic tool '{}': {}",
                sanitized_name, e
            ))
        })?;

        // 3. Move WASM artifact into active plugins directory (.hgb/mcp_plugins/)
        let dest_wasm_path = self.plugins_dir.join(format!("{}.wasm", sanitized_name));
        fs::copy(&wasm_temp_path, &dest_wasm_path).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to copy WASM binary to {}: {}",
                    dest_wasm_path.display(),
                    e
                ),
            ))
        })?;

        // 4. Generate declarative MCP JSON manifest
        let input_schema = request.input_schema.clone().unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": format!("Input payload for dynamically hot-swapped tool '{}'", sanitized_name)
                    }
                }
            })
        });

        let manifest = McpPluginManifest {
            name: sanitized_name.clone(),
            version: Some("1.0.0".to_string()),
            description: request.description.clone(),
            entrypoint_wasm: Some(format!("{}.wasm", sanitized_name)),
            input_schema: Some(input_schema),
        };

        let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
            HgbError::Serialization(format!("Failed to serialize MCP manifest: {}", e))
        })?;

        let dest_manifest_path = self.plugins_dir.join(format!("{}.json", sanitized_name));
        fs::write(&dest_manifest_path, manifest_json).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to write MCP manifest to {}: {}",
                    dest_manifest_path.display(),
                    e
                ),
            ))
        })?;

        // 5. Instantly register tool into live running daemon MCP catalog
        self.hot_reloader
            .register_manifest_plugin(&manifest, &dest_manifest_path)
            .await?;

        // 6. Broadcast SSE event `notifications/tools/list_changed` to connected clients
        let sse_sent = self.hot_reloader.broadcast_tools_list_changed().await;

        let elapsed_ms = start_time.elapsed().as_millis() as u64;
        let wasm_size_bytes = wasm_bytes.len();
        let now_epoch = now_epoch_ms();

        let result = DynamicCompilationResult {
            name: sanitized_name.clone(),
            wasm_path: dest_wasm_path.display().to_string(),
            manifest_path: dest_manifest_path.display().to_string(),
            wasm_size_bytes,
            compile_duration_ms: elapsed_ms,
            compiler_used,
            target_triple,
            validated_with_wasmtime: true,
            sse_notifications_sent: sse_sent,
            is_active: true,
            registered_at_epoch_ms: now_epoch,
        };

        // Cache in dynamic registry
        let mut reg = self.dynamic_registry.write().await;
        reg.insert(sanitized_name, result.clone());

        Ok(result)
    }

    /// Compile Rust source to WASM using `rustc --target wasm32-wasip1 -O`
    fn compile_rust_to_wasm(
        &self,
        name: &str,
        source_code: &str,
        build_dir: &Path,
    ) -> Result<(PathBuf, String, String)> {
        let src_file = build_dir.join(format!("{}.rs", name));
        let out_wasm = build_dir.join(format!("{}.wasm", name));

        fs::write(&src_file, source_code).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to write source file: {}", e),
            ))
        })?;

        // Detect available targets: wasm32-wasip1 or wasm32-unknown-unknown
        let target_triple = "wasm32-wasip1";
        let output = Command::new("rustc")
            .arg("--target")
            .arg(target_triple)
            .arg("--crate-type")
            .arg("cdylib")
            .arg("-O")
            .arg(&src_file)
            .arg("-o")
            .arg(&out_wasm)
            .output();

        match output {
            Ok(res) if res.status.success() && out_wasm.exists() => {
                Ok((out_wasm, "rustc".to_string(), target_triple.to_string()))
            }
            Ok(res) => {
                // Try fallback to wasm32-unknown-unknown
                let fallback_target = "wasm32-unknown-unknown";
                let fb_output = Command::new("rustc")
                    .arg("--target")
                    .arg(fallback_target)
                    .arg("--crate-type")
                    .arg("cdylib")
                    .arg("-O")
                    .arg(&src_file)
                    .arg("-o")
                    .arg(&out_wasm)
                    .output();

                match fb_output {
                    Ok(fb_res) if fb_res.status.success() && out_wasm.exists() => {
                        Ok((out_wasm, "rustc".to_string(), fallback_target.to_string()))
                    }
                    _ => {
                        let stderr = String::from_utf8_lossy(&res.stderr);
                        Err(HgbError::Execution(format!(
                            "Rust tool compilation failed for '{}': {}",
                            name, stderr
                        )))
                    }
                }
            }
            Err(e) => Err(HgbError::Execution(format!(
                "Failed to execute rustc compiler: {}",
                e
            ))),
        }
    }

    /// Compile Zig source to WASM using `zig build-exe -target wasm32-wasi`
    fn compile_zig_to_wasm(
        &self,
        name: &str,
        source_code: &str,
        build_dir: &Path,
    ) -> Result<(PathBuf, String, String)> {
        let src_file = build_dir.join(format!("{}.zig", name));
        let out_wasm = build_dir.join(format!("{}.wasm", name));

        fs::write(&src_file, source_code).map_err(|e| {
            HgbError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to write Zig source: {}", e),
            ))
        })?;

        let zig_bin = find_zig_binary();
        let target_triple = "wasm32-wasi";

        let output = Command::new(&zig_bin)
            .arg("build-exe")
            .arg("-target")
            .arg(target_triple)
            .arg("-O")
            .arg("ReleaseFast")
            .arg("-fno-entry")
            .arg("-rdynamic")
            .arg(&src_file)
            .arg(format!("-femit-bin={}", out_wasm.display()))
            .output();

        match output {
            Ok(res) if res.status.success() && out_wasm.exists() => {
                Ok((out_wasm, zig_bin, target_triple.to_string()))
            }
            Ok(res) => {
                // Try fallback to wasm32-freestanding
                let fb_target = "wasm32-freestanding";
                let fb_output = Command::new(&zig_bin)
                    .arg("build-exe")
                    .arg("-target")
                    .arg(fb_target)
                    .arg("-O")
                    .arg("ReleaseFast")
                    .arg("-fno-entry")
                    .arg("-rdynamic")
                    .arg(&src_file)
                    .arg(format!("-femit-bin={}", out_wasm.display()))
                    .output();

                match fb_output {
                    Ok(fb_res) if fb_res.status.success() && out_wasm.exists() => {
                        Ok((out_wasm, zig_bin, fb_target.to_string()))
                    }
                    _ => {
                        let stderr = String::from_utf8_lossy(&res.stderr);
                        Err(HgbError::Execution(format!(
                            "Zig tool compilation failed for '{}': {}",
                            name, stderr
                        )))
                    }
                }
            }
            Err(e) => Err(HgbError::Execution(format!(
                "Failed to execute zig compiler ({}): {}",
                zig_bin, e
            ))),
        }
    }

    /// Execute a hot-swapped tool directly using WasmPluginInstance
    pub fn execute_tool(&self, name: &str, input: &Value) -> Result<Value> {
        let sanitized = sanitize_tool_name(name);
        let wasm_path = self.plugins_dir.join(format!("{}.wasm", sanitized));
        if !wasm_path.exists() {
            return Err(HgbError::NotFound(format!(
                "Hot-swapped WASM tool '{}' not found at {}",
                sanitized,
                wasm_path.display()
            )));
        }

        let instance = WasmPluginInstance::load(&wasm_path)?;
        instance.execute(input)
    }

    /// Retrieve detailed telemetry and tool inventory report
    pub async fn get_status(&self) -> DynamicHotSwapperReport {
        let reg = self.dynamic_registry.read().await;
        let mut tools = Vec::new();

        for item in reg.values() {
            tools.push(DynamicToolSummary {
                name: item.name.clone(),
                description: format!("Dynamically compiled with {}", item.compiler_used),
                language: item.target_triple.clone(),
                wasm_size_bytes: item.wasm_size_bytes,
                manifest_path: item.manifest_path.clone(),
                is_active: item.is_active,
                compiled_at_epoch_ms: item.registered_at_epoch_ms,
            });
        }

        let rustc_available = Command::new("rustc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let zig_bin = find_zig_binary();
        let zig_available = Command::new(&zig_bin)
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let active_tools = tools.iter().filter(|t| t.is_active).count();
        let total_tools = tools.len();

        DynamicHotSwapperReport {
            plugins_directory: self.plugins_dir.display().to_string(),
            total_tools,
            active_tools,
            tools,
            rustc_available,
            zig_available,
            default_target: "wasm32-wasip1".to_string(),
        }
    }

    /// Scaffolding helper: Rust tool template
    pub fn scaffold_rust_tool(name: &str, logic_body: &str) -> String {
        format!(
            r#"// Dynamically generated Rust tool: {name}
#![allow(dead_code)]

#[no_mangle]
pub extern "C" fn run() -> i32 {{
    {logic_body}
}}
"#
        )
    }

    /// Scaffolding helper: Zig tool template
    pub fn scaffold_zig_tool(name: &str, exit_code: i32) -> String {
        format!(
            r#"// Dynamically generated Zig tool: {name}
export fn run() i32 {{
    return {exit_code};
}}
"#
        )
    }
}

fn sanitize_tool_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

fn find_zig_binary() -> String {
    let local_zig = Path::new("/home/toshiba/.local/bin/zig");
    if local_zig.exists() {
        return local_zig.display().to_string();
    }
    "zig".to_string()
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dynamic_rust_tool_hotswap_lifecycle() {
        let tmp = std::env::temp_dir().join(format!("hgb_test_dyn_tool_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);

        let swapper = DynamicToolHotSwapper::new(&tmp, None);

        // 1. Rust Tool Source
        let rust_code = r#"
#[no_mangle]
pub extern "C" fn run() -> i32 {
    1337
}
"#;
        let request = DynamicToolRequest {
            name: "math_fast_eval".to_string(),
            description: "Evaluates high-speed mathematical operations in sandboxed Wasm".to_string(),
            language: DynamicSourceLanguage::Rust,
            source_code: rust_code.to_string(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string" }
                }
            })),
            author: Some("Autonomous AI Agent".to_string()),
        };

        // 2. Compile and Hot-Swap
        let res = swapper.compile_and_hotswap(&request).await.expect("compile and hotswap");
        assert_eq!(res.name, "math_fast_eval");
        assert!(res.wasm_size_bytes > 0);
        assert!(res.validated_with_wasmtime);
        assert!(res.is_active);

        // 3. Verify manifest dropped in .hgb/mcp_plugins/
        let manifest_path = swapper.plugins_dir.join("math_fast_eval.json");
        assert!(manifest_path.exists());
        let manifest_content = fs::read_to_string(&manifest_path).unwrap();
        assert!(manifest_content.contains("math_fast_eval"));
        assert!(manifest_content.contains("math_fast_eval.wasm"));

        // 4. Verify catalog registration in hot_reloader
        let tools = swapper.hot_reloader.list_tools().await;
        let found = tools.iter().find(|t| t.name == "math_fast_eval");
        assert!(found.is_some());
        assert!(found.unwrap().is_active);

        // 5. Execute hot-swapped tool
        let exec_result = swapper
            .execute_tool("math_fast_eval", &serde_json::json!({"expression": "42"}))
            .expect("execute tool");
        assert_eq!(exec_result["status"], "success");
        assert_eq!(exec_result["exit_code"], 1337);

        // 6. Inspect telemetry status
        let status = swapper.get_status().await;
        assert_eq!(status.total_tools, 1);
        assert_eq!(status.active_tools, 1);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_dynamic_zig_tool_hotswap() {
        let tmp = std::env::temp_dir().join(format!("hgb_test_dyn_zig_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);

        let swapper = DynamicToolHotSwapper::new(&tmp, None);

        let zig_code = r#"
export fn run() i32 {
    return 777;
}
"#;
        let request = DynamicToolRequest {
            name: "zig_fast_accel".to_string(),
            description: "High performance Zig WASM acceleration kernel".to_string(),
            language: DynamicSourceLanguage::Zig,
            source_code: zig_code.to_string(),
            input_schema: None,
            author: Some("Agent".to_string()),
        };

        let res = swapper.compile_and_hotswap(&request).await.expect("zig hotswap");
        assert_eq!(res.name, "zig_fast_accel");
        assert!(res.wasm_size_bytes > 0);
        assert!(res.validated_with_wasmtime);

        let exec_result = swapper
            .execute_tool("zig_fast_accel", &serde_json::json!({}))
            .expect("execute zig tool");
        assert_eq!(exec_result["status"], "success");
        assert_eq!(exec_result["exit_code"], 777);

        let _ = fs::remove_dir_all(&tmp);
    }
}
