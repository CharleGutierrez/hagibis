//! # Universal Model Context Protocol (MCP) Client
//!
//! Systems-grade JSON-RPC 2.0 stdio client for communicating with external MCP tool servers.
//! Fully compliant with the 2024-11-05 Model Context Protocol specification.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use crate::error::{HgbError, Result};
use crate::traits::HgbTool;

/// Configuration for launching an external MCP server process
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Command executable (e.g. "npx", "uvx", "python", "node")
    pub command: String,
    /// Command-line arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to pass to the process
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Whether this server is disabled
    #[serde(default)]
    pub disabled: bool,
}

impl McpServerConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
            disabled: false,
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

/// MCP Configuration file format (`hagibis.mcp.json` or `.hgb/mcp.json`)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers", alias = "mcp_servers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl McpConfigFile {
    pub fn new() -> Self {
        Self {
            mcp_servers: HashMap::new(),
        }
    }

    /// Load MCP config from known locations in workspace root:
    /// 1. `hagibis.mcp.json`
    /// 2. `.hgb/mcp.json`
    /// 3. `.mcp.json`
    pub fn load_from_dir<P: AsRef<Path>>(workspace_root: P) -> Result<Option<Self>> {
        let root = workspace_root.as_ref();
        let candidates = [
            root.join("hagibis.mcp.json"),
            root.join(".hgb").join("mcp.json"),
            root.join(".mcp.json"),
        ];

        for path in &candidates {
            if path.exists() {
                let data = std::fs::read_to_string(path)?;
                let cfg: McpConfigFile = serde_json::from_str(&data).map_err(|e| {
                    HgbError::Serialization(format!(
                        "Failed to parse MCP config at {}: {}",
                        path.display(),
                        e
                    ))
                })?;
                return Ok(Some(cfg));
            }
        }
        Ok(None)
    }

    /// Persist MCP configuration to specified path
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        std::fs::write(path, data)?;
        Ok(())
    }
}

/// Descriptor of an MCP tool advertised by a server
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", alias = "input_schema", default)]
    pub input_schema: serde_json::Value,
}

/// JSON-RPC 2.0 Request envelope
#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<u64>,
    method: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    params: serde_json::Value,
}

/// JSON-RPC 2.0 Response envelope
#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<u64>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

type ResponseMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value>>>>>;

/// Live MCP client session communicating with a subprocess over stdio
pub struct McpClient {
    server_name: String,
    stdin_tx: mpsc::Sender<String>,
    pending_responses: ResponseMap,
    next_id: AtomicU64,
    child_handle: Arc<Mutex<Option<Child>>>,
}

impl McpClient {
    /// Spawn external MCP server process and perform standard initialize handshake
    pub async fn spawn_and_handshake(
        server_name: impl Into<String>,
        config: &McpServerConfig,
        working_dir: Option<PathBuf>,
    ) -> Result<Arc<Self>> {
        let server_name = server_name.into();
        if config.disabled {
            return Err(HgbError::Execution(format!(
                "MCP server '{}' is disabled in configuration",
                server_name
            )));
        }

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        if let Some(ref cwd) = working_dir {
            cmd.current_dir(cwd);
        }
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            HgbError::Execution(format!(
                "Failed to spawn MCP server '{}' ({}): {}",
                server_name, config.command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            HgbError::Execution(format!(
                "Failed to capture stdin for MCP server '{}'",
                server_name
            ))
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            HgbError::Execution(format!(
                "Failed to capture stdout for MCP server '{}'",
                server_name
            ))
        })?;

        let pending_responses: ResponseMap = Arc::new(Mutex::new(HashMap::new()));
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(128);

        // Stdin pump worker
        tokio::spawn(async move {
            let mut writer = stdin;
            while let Some(msg) = stdin_rx.recv().await {
                if writer.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // Stdout reader worker
        let pending_clone = Arc::clone(&pending_responses);
        let s_name = server_name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                    if let Some(id) = resp.id {
                        let mut map = pending_clone.lock().await;
                        if let Some(sender) = map.remove(&id) {
                            if let Some(err) = resp.error {
                                let _ = sender.send(Err(HgbError::Execution(format!(
                                    "MCP error ({}) from '{}': {}",
                                    err.code, s_name, err.message
                                ))));
                            } else if let Some(res) = resp.result {
                                let _ = sender.send(Ok(res));
                            } else {
                                let _ = sender.send(Ok(serde_json::Value::Null));
                            }
                        }
                    }
                }
            }
        });

        let client = Arc::new(Self {
            server_name,
            stdin_tx,
            pending_responses,
            next_id: AtomicU64::new(1),
            child_handle: Arc::new(Mutex::new(Some(child))),
        });

        // Perform MCP Handshake
        client.handshake().await?;

        Ok(client)
    }

    /// Internal handshake: initialize request -> wait response -> initialized notification
    async fn handshake(&self) -> Result<()> {
        let init_params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "clientInfo": {
                "name": "hagibis",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let _init_result = self.send_request("initialize", init_params).await?;

        // Send initialized notification (no id)
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let line = serde_json::to_string(&notif)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        self.stdin_tx
            .send(line)
            .await
            .map_err(|e| HgbError::Execution(format!("Failed to send MCP notification: {}", e)))?;

        Ok(())
    }

    /// Send a JSON-RPC request and wait for matching response
    pub async fn send_request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: method.to_string(),
            params,
        };

        let line = serde_json::to_string(&req)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending_responses.lock().await;
            map.insert(id, tx);
        }

        self.stdin_tx
            .send(line)
            .await
            .map_err(|e| HgbError::Execution(format!("Failed to write to MCP stdin: {}", e)))?;

        // Await response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(HgbError::Execution(format!(
                "MCP server '{}' closed connection unexpectedly",
                self.server_name
            ))),
            Err(_) => {
                let mut map = self.pending_responses.lock().await;
                map.remove(&id);
                Err(HgbError::Execution(format!(
                    "MCP request '{}' timed out after 30s",
                    method
                )))
            }
        }
    }

    /// Query server for advertised tools (`tools/list`)
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let res = self.send_request("tools/list", serde_json::json!({})).await?;
        let tools_val = res.get("tools").ok_or_else(|| {
            HgbError::Execution(format!(
                "MCP 'tools/list' response missing 'tools' key: {}",
                res
            ))
        })?;

        let tools: Vec<McpTool> = serde_json::from_value(tools_val.clone()).map_err(|e| {
            HgbError::Serialization(format!("Failed to parse MCP tools list: {}", e))
        })?;

        Ok(tools)
    }

    /// Invoke a specific MCP tool (`tools/call`)
    pub async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });

        let res = self.send_request("tools/call", params).await?;
        Ok(res)
    }

    /// Name of this server
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Convert all tools from this MCP client into standard `HgbTool` implementations
    pub async fn create_hgb_tools(
        client: &Arc<McpClient>,
        prefix: Option<&str>,
    ) -> Result<Vec<Arc<dyn HgbTool>>> {
        let tools = client.list_tools().await?;
        let mut list: Vec<Arc<dyn HgbTool>> = Vec::new();
        for tool in tools {
            let adapter = McpToolAdapter::new(Arc::clone(client), tool, prefix);
            list.push(Arc::new(adapter));
        }
        Ok(list)
    }

    /// Gracefully shutdown MCP child process
    pub async fn close(&self) {
        let mut guard = self.child_handle.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child_handle.try_lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
    }
}

/// Dynamic adapter wrapping an MCP tool as an `HgbTool`
pub struct McpToolAdapter {
    client: Arc<McpClient>,
    tool: McpTool,
    registered_name: String,
    description: String,
}

impl McpToolAdapter {
    pub fn new(client: Arc<McpClient>, tool: McpTool, prefix: Option<&str>) -> Self {
        let registered_name = match prefix {
            Some(p) => format!("{}_{}", p, tool.name),
            None => tool.name.clone(),
        };

        let desc_base = tool
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool '{}'", tool.name));
        let schema_str = serde_json::to_string(&tool.input_schema).unwrap_or_default();
        let description = format!("{} (Schema: {})", desc_base, schema_str);

        Self {
            client,
            tool,
            registered_name,
            description,
        }
    }

    pub fn tool(&self) -> &McpTool {
        &self.tool
    }
}

#[async_trait]
impl HgbTool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        self.client.call_tool(&self.tool.name, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_config_serialization_roundtrip() {
        let mut cfg = McpConfigFile::new();
        let mut server = McpServerConfig::new("npx");
        server.args = vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into()];
        server.env.insert("DEBUG".into(), "1".into());
        cfg.mcp_servers.insert("filesystem".into(), server);

        let json = serde_json::to_string_pretty(&cfg).unwrap();
        assert!(json.contains("mcpServers"));
        assert!(json.contains("filesystem"));
        assert!(json.contains("@modelcontextprotocol/server-filesystem"));

        let deserialized: McpConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, deserialized);
    }

    #[test]
    fn test_mcp_config_load_and_save() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let mut cfg = McpConfigFile::new();
        cfg.mcp_servers.insert(
            "test_server".into(),
            McpServerConfig::new("echo").with_args(["hello"]),
        );

        let file_path = temp_dir.join("hagibis.mcp.json");
        cfg.save_to_file(&file_path).unwrap();

        let loaded = McpConfigFile::load_from_dir(&temp_dir).unwrap().unwrap();
        assert_eq!(loaded.mcp_servers.len(), 1);
        assert!(loaded.mcp_servers.contains_key("test_server"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_mcp_client_mock_stdio_protocol() {
        // Implement an inline bash script speaking standard JSON-RPC 2.0 stdio
        let script = r#"
while read -r line; do
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d':' -f2)
  if [ "$method" = "initialize" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}"
  elif [ "$method" = "tools/list" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo_tool\",\"description\":\"Echoes input\",\"inputSchema\":{\"type\":\"object\"}}]}}"
  elif [ "$method" = "tools/call" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello from mcp\"}],\"isError\":false}}"
  fi
done
"#;

        let server_cfg = McpServerConfig::new("bash").with_args(["-c", script]);
        let client = McpClient::spawn_and_handshake("mock_mcp", &server_cfg, None)
            .await
            .expect("MCP spawn and handshake must succeed");

        let tools = client.list_tools().await.expect("list_tools must succeed");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo_tool");

        let call_res = client
            .call_tool("echo_tool", serde_json::json!({"msg": "hi"}))
            .await
            .expect("call_tool must succeed");

        assert_eq!(call_res["content"][0]["text"], "hello from mcp");

        // Test HgbTool dynamic adapter
        let hgb_tools = McpClient::create_hgb_tools(&client, Some("mock"))
            .await
            .expect("create_hgb_tools must succeed");
        assert_eq!(hgb_tools.len(), 1);
        assert_eq!(hgb_tools[0].name(), "mock_echo_tool");

        let tool_exec = hgb_tools[0]
            .execute(serde_json::json!({}))
            .await
            .expect("adapter execution must succeed");
        assert_eq!(tool_exec["content"][0]["text"], "hello from mcp");

        client.close().await;
    }
}
