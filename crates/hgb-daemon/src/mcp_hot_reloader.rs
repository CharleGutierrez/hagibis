//! # Universal Model Context Protocol (MCP) Hot-Reloading Engine
//!
//! Live, zero-downtime MCP plugin hot-reloader and dynamic discovery daemon:
//! 1. Monitors `.hgb/mcp_plugins/` for new WebAssembly tools (`*.wasm`) and JSON manifests (`*.json`).
//! 2. Dynamically registers and unregisters tools, prompts, and resources into the live running
//!    MCP catalog (`mcp_sse_server.rs` and active sessions) without restarting `hgbd`.
//! 3. Self-extending OS loop: Autonomous agents write a tool implementation (in Rust, Zig, or C),
//!    compile it to Wasm (`hgb-wasm-kernel`), drop the artifact into `.hgb/mcp_plugins/`, and
//!    `hgb-daemon` hot-registers it immediately with SSE event broadcast `tools/list_changed`.

use axum::response::sse::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::mcp_sse_server::McpSseState;
use hgb_core::error::Result;

/// Metadata for a dynamically registered MCP tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotLoadedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub wasm_path: Option<String>,
    pub registered_at: u64,
    pub is_active: bool,
}

/// JSON manifest schema for external MCP plugin descriptors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPluginManifest {
    pub name: String,
    pub version: Option<String>,
    pub description: String,
    pub entrypoint_wasm: Option<String>,
    pub input_schema: Option<Value>,
}

/// Summary report returned upon scanning and hot-reloading MCP plugins
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHotReloaderReport {
    pub plugins_directory: String,
    pub total_plugins: usize,
    pub active_tools: usize,
    pub discovered_wasm: Vec<String>,
    pub discovered_manifests: Vec<String>,
    pub broadcast_notifications_sent: usize,
    pub last_scan_utc: String,
}

/// Universal MCP Hot-Reloading Subsystem
pub struct McpHotReloader {
    pub plugins_dir: PathBuf,
    catalog: Arc<RwLock<HashMap<String, HotLoadedTool>>>,
    sse_state: Option<Arc<McpSseState>>,
}

impl McpHotReloader {
    /// Create new hot reloader monitoring `<workspace>/.hgb/mcp_plugins/`
    pub fn new<P: AsRef<Path>>(workspace_root: P, sse_state: Option<Arc<McpSseState>>) -> Self {
        let plugins_dir = workspace_root.as_ref().join(".hgb").join("mcp_plugins");
        let _ = fs::create_dir_all(&plugins_dir);

        Self {
            plugins_dir,
            catalog: Arc::new(RwLock::new(HashMap::new())),
            sse_state,
        }
    }

    /// Scan plugins directory, register newly dropped WASM and JSON manifests, and broadcast SSE notification
    pub async fn scan_and_hot_reload(&self) -> Result<McpHotReloaderReport> {
        let _ = fs::create_dir_all(&self.plugins_dir);
        let mut discovered_wasm = Vec::new();
        let mut discovered_manifests = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.plugins_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if ext == "wasm" {
                        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("wasm_tool");
                        discovered_wasm.push(path.display().to_string());
                        self.register_wasm_plugin(name, &path, "Hot-loaded WebAssembly tool").await?;
                    } else if ext == "json" {
                        discovered_manifests.push(path.display().to_string());
                        if let Ok(content) = fs::read_to_string(&path) {
                            if let Ok(manifest) = serde_json::from_str::<McpPluginManifest>(&content) {
                                self.register_manifest_plugin(&manifest, &path).await?;
                            }
                        }
                    }
                }
            }
        }

        // Broadcast notifications to all active SSE subscribers
        let broadcasts = self.broadcast_tools_list_changed().await;

        let catalog_lock = self.catalog.read().await;
        let active_tools = catalog_lock.values().filter(|t| t.is_active).count();
        let total_plugins = catalog_lock.len();

        Ok(McpHotReloaderReport {
            plugins_directory: self.plugins_dir.display().to_string(),
            total_plugins,
            active_tools,
            discovered_wasm,
            discovered_manifests,
            broadcast_notifications_sent: broadcasts,
            last_scan_utc: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Register a standalone WebAssembly plugin
    pub async fn register_wasm_plugin(&self, name: &str, wasm_path: &Path, description: &str) -> Result<()> {
        let default_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "Input payload for WASM module" }
            }
        });

        let tool = HotLoadedTool {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: default_schema,
            wasm_path: Some(wasm_path.display().to_string()),
            registered_at: now_epoch_ms(),
            is_active: true,
        };

        let mut lock = self.catalog.write().await;
        lock.insert(name.to_string(), tool);
        Ok(())
    }

    /// Register a plugin described by a JSON manifest
    pub async fn register_manifest_plugin(&self, manifest: &McpPluginManifest, manifest_path: &Path) -> Result<()> {
        let wasm_path = if let Some(ref rel_wasm) = manifest.entrypoint_wasm {
            let parent = manifest_path.parent().unwrap_or_else(|| Path::new("."));
            Some(parent.join(rel_wasm).display().to_string())
        } else {
            None
        };

        let schema = manifest.input_schema.clone().unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "Input parameters" }
                }
            })
        });

        let tool = HotLoadedTool {
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            input_schema: schema,
            wasm_path,
            registered_at: now_epoch_ms(),
            is_active: true,
        };

        let mut lock = self.catalog.write().await;
        lock.insert(manifest.name.clone(), tool);
        Ok(())
    }

    /// Unregister or disable a dynamic tool
    pub async fn unregister_plugin(&self, name: &str) -> Result<bool> {
        let mut lock = self.catalog.write().await;
        if let Some(tool) = lock.get_mut(name) {
            tool.is_active = false;
            let _ = self.broadcast_tools_list_changed().await;
            return Ok(true);
        }
        Ok(false)
    }

    /// Retrieve all active registered tools
    pub async fn list_tools(&self) -> Vec<HotLoadedTool> {
        let lock = self.catalog.read().await;
        lock.values().cloned().collect()
    }

    /// Broadcast MCP `notifications/tools/list_changed` JSON-RPC event over active SSE sessions
    pub async fn broadcast_tools_list_changed(&self) -> usize {
        if let Some(ref sse) = self.sse_state {
            let notification = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/tools/list_changed",
                "params": {}
            });

            let event = Event::default()
                .event("tools/list_changed")
                .data(notification.to_string());

            let sessions = sse.sessions.read().await;
            let mut sent = 0;
            for tx in sessions.values() {
                if tx.send(event.clone()).is_ok() {
                    sent += 1;
                }
            }
            sent
        } else {
            0
        }
    }

    /// Current status of the hot reloader
    pub async fn get_status(&self) -> McpHotReloaderReport {
        let lock = self.catalog.read().await;
        let active_tools = lock.values().filter(|t| t.is_active).count();
        let total_plugins = lock.len();

        McpHotReloaderReport {
            plugins_directory: self.plugins_dir.display().to_string(),
            total_plugins,
            active_tools,
            discovered_wasm: Vec::new(),
            discovered_manifests: Vec::new(),
            broadcast_notifications_sent: 0,
            last_scan_utc: chrono::Utc::now().to_rfc3339(),
        }
    }
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
    async fn test_mcp_hot_reloader_lifecycle() {
        let tmp = std::env::temp_dir().join(format!("hgb_test_mcp_hot_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);

        let reloader = McpHotReloader::new(&tmp, None);

        // 1. Drop a synthetic plugin manifest
        let manifest_path = reloader.plugins_dir.join("test_tool.json");
        let manifest_content = r#"{
            "name": "calc_sqrt",
            "description": "Calculates square root in Wasm",
            "entrypoint_wasm": "calc_sqrt.wasm"
        }"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        // 2. Scan and verify registration
        let report = reloader.scan_and_hot_reload().await.unwrap();
        assert_eq!(report.total_plugins, 1);
        assert_eq!(report.active_tools, 1);

        let tools = reloader.list_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "calc_sqrt");

        // 3. Unregister plugin
        let unreg = reloader.unregister_plugin("calc_sqrt").await.unwrap();
        assert!(unreg);

        let tools2 = reloader.list_tools().await;
        assert_eq!(tools2[0].is_active, false);

        let _ = fs::remove_dir_all(&tmp);
    }
}
