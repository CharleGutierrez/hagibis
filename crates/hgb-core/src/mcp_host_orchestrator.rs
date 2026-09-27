//! # McpHostOrchestrator - Unified Multi-Server Model Context Protocol Host
//!
//! Elevates Goose & Claude Code's native MCP integration. Manages an active fleet
//! of external MCP servers (Postgres, GitHub, Slack, SQLite, Web search),
//! aggregates their tools under a unified namespace (`server::tool`), and
//! provides auto-discovery across `.hgb/mcp.json`, `.cursor/mcp.json`, and `.claude/mcp.json`.

use crate::error::{HgbError, Result};
use crate::mcp::{McpClient, McpConfigFile, McpServerConfig, McpTool};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Status of an individual managed MCP server
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpServerStatus {
    Registered,
    Running,
    Stopped,
    Error(String),
}

/// Aggregated tool with namespaced identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamespacedMcpTool {
    pub namespaced_name: String, // e.g. "postgres::execute_query"
    pub server_name: String,
    pub original_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Managed server record inside the host orchestrator
struct ManagedServer {
    config: McpServerConfig,
    status: McpServerStatus,
    client: Option<Arc<McpClient>>,
    discovered_tools: Vec<McpTool>,
}

pub struct McpHostOrchestrator {
    workspace_root: PathBuf,
    servers: Arc<RwLock<HashMap<String, ManagedServer>>>,
}

impl McpHostOrchestrator {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Auto-discover MCP configs from .hgb, .cursor, or .claude directories
    pub async fn auto_discover(&self) -> Result<usize> {
        let mut discovered_count = 0;
        let candidates = [
            self.workspace_root.join(".hgb").join("mcp.json"),
            self.workspace_root.join(".hgb_mcp.json"),
            self.workspace_root.join(".cursor").join("mcp.json"),
            self.workspace_root.join(".claude").join("mcp.json"),
        ];

        for path in &candidates {
            if path.exists() {
                if let Ok(config_file) = McpConfigFile::load_from_path(path) {
                    let mut lock = self.servers.write().await;
                    for (name, cfg) in config_file.mcp_servers {
                        if !lock.contains_key(&name) {
                            lock.insert(
                                name.clone(),
                                ManagedServer {
                                    config: cfg,
                                    status: McpServerStatus::Registered,
                                    client: None,
                                    discovered_tools: Vec::new(),
                                },
                            );
                            discovered_count += 1;
                        }
                    }
                }
            }
        }

        Ok(discovered_count)
    }

    /// Register a server dynamically
    pub async fn register_server(&self, name: impl Into<String>, config: McpServerConfig) {
        let mut lock = self.servers.write().await;
        lock.insert(
            name.into(),
            ManagedServer {
                config,
                status: McpServerStatus::Registered,
                client: None,
                discovered_tools: Vec::new(),
            },
        );
    }

    /// Start a specific server by name
    pub async fn start_server(&self, name: &str) -> Result<Vec<McpTool>> {
        let config = {
            let lock = self.servers.read().await;
            let s = lock.get(name).ok_or_else(|| {
                HgbError::Execution(format!("MCP server '{}' is not registered", name))
            })?;
            s.config.clone()
        };

        match McpClient::spawn_and_handshake(name, &config, Some(self.workspace_root.clone())).await {
            Ok(client) => match client.list_tools().await {
                Ok(tools) => {
                    let mut lock = self.servers.write().await;
                    if let Some(entry) = lock.get_mut(name) {
                        entry.status = McpServerStatus::Running;
                        entry.client = Some(client);
                        entry.discovered_tools = tools.clone();
                    }
                    Ok(tools)
                }
                Err(e) => {
                    let mut lock = self.servers.write().await;
                    if let Some(entry) = lock.get_mut(name) {
                        entry.status = McpServerStatus::Error(e.to_string());
                    }
                    Err(e)
                }
            },
            Err(e) => {
                let mut lock = self.servers.write().await;
                if let Some(entry) = lock.get_mut(name) {
                    entry.status = McpServerStatus::Error(e.to_string());
                }
                Err(e)
            }
        }
    }

    /// Stop an active server
    pub async fn stop_server(&self, name: &str) -> Result<()> {
        let mut lock = self.servers.write().await;
        if let Some(entry) = lock.get_mut(name) {
            entry.client = None;
            entry.status = McpServerStatus::Stopped;
        }
        Ok(())
    }

    /// List all tools aggregated across all currently active servers
    pub async fn list_aggregated_tools(&self) -> Vec<NamespacedMcpTool> {
        let lock = self.servers.read().await;
        let mut aggregated = Vec::new();

        for (s_name, server) in lock.iter() {
            if server.status == McpServerStatus::Running {
                for t in &server.discovered_tools {
                    aggregated.push(NamespacedMcpTool {
                        namespaced_name: format!("{}::{}", s_name, t.name),
                        server_name: s_name.clone(),
                        original_name: t.name.clone(),
                        description: t.description.clone().unwrap_or_default(),
                        input_schema: t.input_schema.clone(),
                    });
                }
            }
        }

        aggregated
    }

    /// Dispatch tool invocation to the appropriate MCP server
    pub async fn dispatch_tool(
        &self,
        namespaced_tool: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let (server_name, tool_name) = if let Some((s, t)) = namespaced_tool.split_once("::") {
            (s, t)
        } else {
            return Err(HgbError::Execution(format!(
                "Invalid namespaced MCP tool '{}'. Expected 'server::tool_name'",
                namespaced_tool
            )));
        };

        let client = {
            let lock = self.servers.read().await;
            let server = lock.get(server_name).ok_or_else(|| {
                HgbError::Execution(format!("MCP server '{}' is not registered", server_name))
            })?;

            if server.status != McpServerStatus::Running {
                return Err(HgbError::Execution(format!(
                    "MCP server '{}' is not running (status: {:?})",
                    server_name, server.status
                )));
            }

            server
                .client
                .clone()
                .ok_or_else(|| HgbError::Execution(format!("MCP client '{}' is not active", server_name)))?
        };

        client.call_tool(tool_name, arguments).await
    }

    /// Inspect health and status of all configured servers
    pub async fn health_summary(&self) -> HashMap<String, McpServerStatus> {
        let lock = self.servers.read().await;
        lock.iter()
            .map(|(k, v)| (k.clone(), v.status.clone()))
            .collect()
    }
}
