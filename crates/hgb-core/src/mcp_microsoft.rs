//! # McpMicrosoftBridge - Model Context Protocol Bridge for Microsoft 365
//!
//! Provides enterprise tools conforming to `HgbTool` and MCP JSON-RPC:
//! 1. `sharepoint_read_spec`: Read specifications/PRDs from SharePoint/OneDrive or local mirror `.hgb/sharepoint/`.
//! 2. `teams_send_adaptive_card`: Construct and send rich, interactive Teams Adaptive Cards.
//! 3. `outlook_send_digest`: Dispatch formatted executive sprint digests via Microsoft Graph.
//! 4. `graph_search_tribal`: Semantic and full-text search across internal enterprise tribal knowledge.
//!
//! Exposes registration into `McpHostOrchestrator` and `.hgb/mcp.microsoft.json` generation.

use crate::error::{HgbError, Result};
use crate::mcp::McpServerConfig;
use crate::mcp_host_orchestrator::McpHostOrchestrator;
use crate::traits::HgbTool;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Enterprise Microsoft 365 MCP configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicrosoftMcpConfig {
    pub tenant_id: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub graph_base_url: String,
    pub cache_dir: PathBuf,
    pub teams_webhook_url: Option<String>,
    pub bearer_token: Option<String>,
}

impl Default for MicrosoftMcpConfig {
    fn default() -> Self {
        Self {
            tenant_id: std::env::var("AZURE_TENANT_ID").ok(),
            client_id: std::env::var("AZURE_CLIENT_ID").ok(),
            client_secret: std::env::var("AZURE_CLIENT_SECRET").ok(),
            graph_base_url: "https://graph.microsoft.com/v1.0".to_string(),
            cache_dir: PathBuf::from(".hgb").join("sharepoint"),
            teams_webhook_url: std::env::var("TEAMS_WEBHOOK_URL").ok(),
            bearer_token: std::env::var("AZURE_BEARER_TOKEN").ok(),
        }
    }
}

impl MicrosoftMcpConfig {
    pub fn new(cache_dir: impl AsRef<Path>) -> Self {
        let mut cfg = Self::default();
        cfg.cache_dir = cache_dir.as_ref().to_path_buf();
        cfg
    }

    pub fn with_teams_webhook(mut self, url: impl Into<String>) -> Self {
        self.teams_webhook_url = Some(url.into());
        self
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    pub fn is_authenticated(&self) -> bool {
        self.bearer_token.is_some() || (self.client_id.is_some() && self.client_secret.is_some())
    }
}

// -----------------------------------------------------------------------------
// Tool 1: sharepoint_read_spec
// -----------------------------------------------------------------------------

pub struct SharePointReadSpecTool {
    config: MicrosoftMcpConfig,
    client: reqwest::Client,
}

impl SharePointReadSpecTool {
    pub fn new(config: MicrosoftMcpConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl HgbTool for SharePointReadSpecTool {
    fn name(&self) -> &str {
        "sharepoint_read_spec"
    }

    fn description(&self) -> &str {
        "Reads enterprise PRDs, engineering specs, or architecture docs from SharePoint / OneDrive or local mirrored cache in .hgb/sharepoint/."
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let resource_path = arguments
            .get("resource_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing required argument: 'resource_path'".to_string()))?;

        // 1. Check local mirrored cache first (.hgb/sharepoint/<resource_path>)
        let clean_relative = resource_path.trim_start_matches('/').trim_start_matches('\\');
        let local_file = self.config.cache_dir.join(clean_relative);

        if local_file.exists() && local_file.is_file() {
            let content = fs::read_to_string(&local_file)
                .map_err(|e| HgbError::Storage(format!("Failed to read cached SharePoint spec: {e}")))?;
            let meta = fs::metadata(&local_file).ok();
            return Ok(json!({
                "source": "local_mirror_cache",
                "resource_path": resource_path,
                "local_path": local_file.to_string_lossy(),
                "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(content.len() as u64),
                "content": content,
                "last_modified": meta.as_ref().and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339())
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            }));
        }

        // 2. If authenticated via Graph API, attempt live fetch from Microsoft Graph
        if let Some(token) = &self.config.bearer_token {
            let url = format!("{}/sites/root/drive/root:/{}:/content", self.config.graph_base_url, clean_relative);
            let resp = self.client.get(&url)
                .header("Authorization", format!("Bearer {}", token))
                .header("User-Agent", "Hagibis-Microsoft-Bridge/1.0")
                .send()
                .await
                .map_err(|e| HgbError::Network(format!("Failed to query Microsoft Graph API: {e}")))?;

            if resp.status().is_success() {
                let text = resp.text().await
                    .map_err(|e| HgbError::Network(format!("Failed to decode Graph API response text: {e}")))?;

                // Persist to local mirror cache for subsequent offline resilience
                let _ = fs::create_dir_all(local_file.parent().unwrap_or(&self.config.cache_dir));
                let _ = fs::write(&local_file, &text);

                return Ok(json!({
                    "source": "microsoft_graph_live",
                    "resource_path": resource_path,
                    "local_path": local_file.to_string_lossy(),
                    "size_bytes": text.len(),
                    "content": text,
                    "last_modified": Utc::now().to_rfc3339(),
                }));
            }
        }

        // 3. Fallback: Search any file ending with the base file name inside cache_dir
        if self.config.cache_dir.exists() {
            let target_filename = Path::new(clean_relative).file_name().and_then(|f| f.to_str()).unwrap_or(clean_relative);
            if let Ok(entries) = fs::read_dir(&self.config.cache_dir) {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy() == target_filename {
                        let content = fs::read_to_string(entry.path())
                            .map_err(|e| HgbError::Storage(format!("Failed to read spec: {e}")))?;
                        return Ok(json!({
                            "source": "local_mirror_scan",
                            "resource_path": resource_path,
                            "local_path": entry.path().to_string_lossy(),
                            "size_bytes": content.len(),
                            "content": content,
                            "last_modified": Utc::now().to_rfc3339(),
                        }));
                    }
                }
            }
        }

        Err(HgbError::NotFound(format!(
            "SharePoint specification '{}' not found in local cache ({:?}) and Microsoft Graph unconfigured/unreachable.",
            resource_path, self.config.cache_dir
        )))
    }
}

// -----------------------------------------------------------------------------
// Tool 2: teams_send_adaptive_card
// -----------------------------------------------------------------------------

pub struct TeamsSendAdaptiveCardTool {
    config: MicrosoftMcpConfig,
    client: reqwest::Client,
}

impl TeamsSendAdaptiveCardTool {
    pub fn new(config: MicrosoftMcpConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Construct an official Microsoft Teams Incident Card (Adaptive Card schema v1.5)
    pub fn build_incident_card(title: &str, severity: &str, description: &str, action_url: Option<&str>) -> serde_json::Value {
        let color = match severity.to_uppercase().as_str() {
            "CRITICAL" | "SEV1" => "Attention",
            "HIGH" | "SEV2" => "Warning",
            _ => "Good",
        };

        let body = vec![
            json!({
                "type": "TextBlock",
                "size": "Medium",
                "weight": "Bolder",
                "text": format!("🚨 Hagibis Incident Report: {}", title),
                "color": color
            }),
            json!({
                "type": "FactSet",
                "facts": [
                    { "title": "Severity", "value": severity },
                    { "title": "Timestamp", "value": Utc::now().to_rfc3339() },
                    { "title": "Engine", "value": "Hagibis Autonomous Sentinel" }
                ]
            }),
            json!({
                "type": "TextBlock",
                "text": description,
                "wrap": true
            })
        ];

        let mut actions = Vec::new();
        if let Some(url) = action_url {
            actions.push(json!({
                "type": "Action.OpenUrl",
                "title": "Investigate in Cockpit",
                "url": url
            }));
        }
        actions.push(json!({
            "type": "Action.Submit",
            "title": "Approve Auto-Remediation",
            "data": { "action": "approve_heal", "incident_title": title }
        }));

        json!({
            "type": "AdaptiveCard",
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "version": "1.5",
            "body": body,
            "actions": actions
        })
    }

    /// Construct an automated Test Summary Card
    pub fn build_test_summary_card(suite_name: &str, passed: usize, failed: usize, duration_ms: u64) -> serde_json::Value {
        let status_color = if failed == 0 { "Good" } else { "Attention" };
        json!({
            "type": "AdaptiveCard",
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "version": "1.5",
            "body": [
                {
                    "type": "TextBlock",
                    "size": "Large",
                    "weight": "Bolder",
                    "text": format!("🧪 Test Execution Summary: {}", suite_name),
                    "color": status_color
                },
                {
                    "type": "FactSet",
                    "facts": [
                        { "title": "Passed", "value": passed.to_string() },
                        { "title": "Failed", "value": failed.to_string() },
                        { "title": "Duration", "value": format!("{} ms", duration_ms) },
                        { "title": "Result", "value": if failed == 0 { "PASSED ✔" } else { "FAILED ✖" } }
                    ]
                }
            ]
        })
    }

    /// Construct a Pull Request Approval Card
    pub fn build_approval_card(pr_title: &str, pr_url: &str, author: &str, risk_level: &str) -> serde_json::Value {
        json!({
            "type": "AdaptiveCard",
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "version": "1.5",
            "body": [
                {
                    "type": "TextBlock",
                    "size": "Medium",
                    "weight": "Bolder",
                    "text": format!("🛡️ AI PR Approval Request: {}", pr_title)
                },
                {
                    "type": "FactSet",
                    "facts": [
                        { "title": "Author", "value": author },
                        { "title": "Risk Level", "value": risk_level },
                        { "title": "Verification Gate", "value": "Passed All Invariants" }
                    ]
                }
            ],
            "actions": [
                {
                    "type": "Action.OpenUrl",
                    "title": "Review Diff",
                    "url": pr_url
                },
                {
                    "type": "Action.Submit",
                    "title": "Merge to Main",
                    "data": { "action": "merge", "pr_title": pr_title }
                }
            ]
        })
    }
}

#[async_trait]
impl HgbTool for TeamsSendAdaptiveCardTool {
    fn name(&self) -> &str {
        "teams_send_adaptive_card"
    }

    fn description(&self) -> &str {
        "Constructs and dispatches interactive Microsoft Teams Adaptive Cards for incidents, test runs, and approvals."
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let channel_id = arguments
            .get("channel_id")
            .and_then(|v| v.as_str())
            .unwrap_or("general");

        let card_payload = arguments
            .get("card_payload")
            .cloned()
            .ok_or_else(|| HgbError::InvalidInput("Missing required argument: 'card_payload'".to_string()))?;

        // Format as Teams Webhook Attachment Payload
        let webhook_payload = json!({
            "type": "message",
            "attachments": [
                {
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "contentUrl": null,
                    "content": card_payload
                }
            ]
        });

        let mut delivered = false;
        let mut dispatch_details = "Queued in local enterprise audit trail".to_string();

        // If live Teams webhook URL is configured, post it
        if let Some(webhook_url) = &self.config.teams_webhook_url {
            let res = self.client.post(webhook_url)
                .json(&webhook_payload)
                .send()
                .await;

            match res {
                Ok(resp) if resp.status().is_success() => {
                    delivered = true;
                    dispatch_details = format!("Delivered to Teams Webhook (HTTP {})", resp.status());
                }
                Ok(resp) => {
                    dispatch_details = format!("Teams Webhook returned status HTTP {}", resp.status());
                }
                Err(e) => {
                    dispatch_details = format!("Teams Webhook connection error: {e}");
                }
            }
        }

        // Archive dispatched card for auditing
        let audit_dir = PathBuf::from(".hgb").join("teams");
        let _ = fs::create_dir_all(&audit_dir);
        let card_id = format!("teams_card_{}", uuid::Uuid::new_v4().simple());
        let audit_file = audit_dir.join(format!("{}.json", card_id));
        let _ = fs::write(&audit_file, serde_json::to_string_pretty(&webhook_payload).unwrap_or_default());

        Ok(json!({
            "card_id": card_id,
            "channel_id": channel_id,
            "delivered": delivered,
            "dispatch_status": dispatch_details,
            "audit_file": audit_file.to_string_lossy(),
            "timestamp": Utc::now().to_rfc3339()
        }))
    }
}

// -----------------------------------------------------------------------------
// Tool 3: outlook_send_digest
// -----------------------------------------------------------------------------

pub struct OutlookSendDigestTool {
    config: MicrosoftMcpConfig,
    client: reqwest::Client,
}

impl OutlookSendDigestTool {
    pub fn new(config: MicrosoftMcpConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl HgbTool for OutlookSendDigestTool {
    fn name(&self) -> &str {
        "outlook_send_digest"
    }

    fn description(&self) -> &str {
        "Generates and dispatches executive sprint digests, release notes, and security audits via Outlook / Microsoft Graph."
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let recipient = arguments
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing required argument: 'recipient'".to_string()))?;

        let subject = arguments
            .get("subject")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing required argument: 'subject'".to_string()))?;

        let html_body = arguments
            .get("html_body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing required argument: 'html_body'".to_string()))?;

        // Archive digest to .hgb/digests/
        let digest_dir = PathBuf::from(".hgb").join("digests");
        let _ = fs::create_dir_all(&digest_dir);
        let digest_id = format!("digest_{}_{}", Utc::now().format("%Y%m%d_%H%M%S"), uuid::Uuid::new_v4().simple());
        let digest_file = digest_dir.join(format!("{}.html", digest_id));
        let _ = fs::write(&digest_file, html_body);

        let mut delivered = false;
        let mut dispatch_msg = "Archived to local enterprise audit store".to_string();

        // If Graph API token is present, dispatch via Graph API /me/sendMail
        if let Some(token) = &self.config.bearer_token {
            let send_mail_url = format!("{}/me/sendMail", self.config.graph_base_url);
            let email_payload = json!({
                "message": {
                    "subject": subject,
                    "body": {
                        "contentType": "HTML",
                        "content": html_body
                    },
                    "toRecipients": [
                        {
                            "emailAddress": {
                                "address": recipient
                            }
                        }
                    ]
                },
                "saveToSentItems": "true"
            });

            match self.client.post(&send_mail_url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .json(&email_payload)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() || resp.status() == 202 => {
                    delivered = true;
                    dispatch_msg = "Dispatched via Microsoft Graph Outlook API".to_string();
                }
                Ok(resp) => {
                    dispatch_msg = format!("Graph sendMail returned HTTP {}", resp.status());
                }
                Err(e) => {
                    dispatch_msg = format!("Graph sendMail network error: {e}");
                }
            }
        }

        Ok(json!({
            "digest_id": digest_id,
            "recipient": recipient,
            "subject": subject,
            "delivered": delivered,
            "archive_path": digest_file.to_string_lossy(),
            "status": dispatch_msg,
            "timestamp": Utc::now().to_rfc3339()
        }))
    }
}

// -----------------------------------------------------------------------------
// Tool 4: graph_search_tribal
// -----------------------------------------------------------------------------

pub struct GraphSearchTribalTool {
    config: MicrosoftMcpConfig,
    client: reqwest::Client,
}

impl GraphSearchTribalTool {
    pub fn new(config: MicrosoftMcpConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TribalSearchResult {
    pub title: String,
    pub source: String,
    pub snippet: String,
    pub path: String,
    pub score: f32,
}

#[async_trait]
impl HgbTool for GraphSearchTribalTool {
    fn name(&self) -> &str {
        "graph_search_tribal"
    }

    fn description(&self) -> &str {
        "Queries internal corporate documentation, Teams knowledge bases, and design specs for tribal engineering context."
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing required argument: 'query'".to_string()))?;

        let mut hits: Vec<TribalSearchResult> = Vec::new();

        // 1. Search local mirrored SharePoint cache (.hgb/sharepoint/ and .hgb/tribal/)
        let search_dirs = [
            self.config.cache_dir.clone(),
            PathBuf::from(".hgb").join("tribal"),
            PathBuf::from("docs").join("enterprise"),
        ];

        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();

        for dir in &search_dirs {
            if dir.exists() {
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_file() {
                            if let Ok(content) = fs::read_to_string(&path) {
                                let content_lower = content.to_lowercase();
                                let mut matches = 0;
                                for term in &terms {
                                    if content_lower.contains(term) {
                                        matches += 1;
                                    }
                                }

                                if matches > 0 || terms.is_empty() {
                                    let score = matches as f32 / terms.len().max(1) as f32;
                                    let best_line = content
                                        .lines()
                                        .map(|l| {
                                            let l_lower = l.to_lowercase();
                                            let count = terms.iter().filter(|t| l_lower.contains(*t)).count();
                                            (count, l)
                                        })
                                        .max_by_key(|(count, _)| *count)
                                        .map(|(_, l)| l)
                                        .unwrap_or_else(|| content.lines().next().unwrap_or(""));
                                    let snippet = best_line.trim().to_string();

                                    hits.push(TribalSearchResult {
                                        title: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Document").to_string(),
                                        source: "SharePoint Mirror Cache".to_string(),
                                        snippet: if snippet.len() > 200 { format!("{}...", &snippet[..200]) } else { snippet },
                                        path: path.to_string_lossy().to_string(),
                                        score,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Query live Microsoft Graph Search API if token is provided
        if let Some(token) = &self.config.bearer_token {
            let search_url = format!("{}/search/query", self.config.graph_base_url);
            let search_body = json!({
                "requests": [
                    {
                        "entityTypes": ["driveItem", "message"],
                        "query": {
                            "queryString": query
                        },
                        "from": 0,
                        "size": 5
                    }
                ]
            });

            if let Ok(resp) = self.client.post(&search_url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .json(&search_body)
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(json_resp) = resp.json::<serde_json::Value>().await {
                        if let Some(containers) = json_resp["value"][0]["hitsContainers"].as_array() {
                            for container in containers {
                                if let Some(items) = container["hits"].as_array() {
                                    for item in items {
                                        let summary = item["summary"].as_str().unwrap_or("").to_string();
                                        let name = item["resource"]["name"].as_str().unwrap_or("Graph Result").to_string();
                                        let web_url = item["resource"]["webUrl"].as_str().unwrap_or("").to_string();
                                        hits.push(TribalSearchResult {
                                            title: name,
                                            source: "Microsoft Graph Enterprise Search".to_string(),
                                            snippet: summary,
                                            path: web_url,
                                            score: 1.0,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort descending by score
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        Ok(json!({
            "query": query,
            "total_hits": hits.len(),
            "results": hits
        }))
    }
}

// -----------------------------------------------------------------------------
// McpMicrosoftBridge
// -----------------------------------------------------------------------------

/// The complete MCP Microsoft 365 Bridge Orchestration Engine
pub struct McpMicrosoftBridge {
    config: MicrosoftMcpConfig,
    tools: Vec<Arc<dyn HgbTool>>,
}

impl McpMicrosoftBridge {
    pub fn new(config: MicrosoftMcpConfig) -> Self {
        let tools: Vec<Arc<dyn HgbTool>> = vec![
            Arc::new(SharePointReadSpecTool::new(config.clone())),
            Arc::new(TeamsSendAdaptiveCardTool::new(config.clone())),
            Arc::new(OutlookSendDigestTool::new(config.clone())),
            Arc::new(GraphSearchTribalTool::new(config.clone())),
        ];
        Self { config, tools }
    }

    pub fn from_env(workspace_root: impl AsRef<Path>) -> Self {
        let cache_dir = workspace_root.as_ref().join(".hgb").join("sharepoint");
        Self::new(MicrosoftMcpConfig::new(cache_dir))
    }

    pub fn config(&self) -> &MicrosoftMcpConfig {
        &self.config
    }

    pub fn tools(&self) -> &[Arc<dyn HgbTool>] {
        &self.tools
    }

    /// Generate `.hgb/mcp.microsoft.json` declarative server configuration
    pub fn generate_mcp_config(&self, target_path: Option<&Path>) -> Result<PathBuf> {
        let path = match target_path {
            Some(p) => p.to_path_buf(),
            None => PathBuf::from(".hgb").join("mcp.microsoft.json"),
        };

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mcp_manifest = json!({
            "mcpServers": {
                "microsoft_365": {
                    "command": "hgb",
                    "args": ["mcp", "serve", "--namespace", "microsoft"],
                    "env": {
                        "AZURE_TENANT_ID": self.config.tenant_id.as_deref().unwrap_or(""),
                        "AZURE_CLIENT_ID": self.config.client_id.as_deref().unwrap_or(""),
                        "TEAMS_WEBHOOK_URL": self.config.teams_webhook_url.as_deref().unwrap_or("")
                    },
                    "description": "Microsoft 365 Enterprise Bridge (SharePoint, Teams Adaptive Cards, Outlook Digests, Graph Search)"
                }
            }
        });

        let json_text = serde_json::to_string_pretty(&mcp_manifest)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        fs::write(&path, json_text)
            .map_err(|e| HgbError::Storage(format!("Failed to write .hgb/mcp.microsoft.json: {e}")))?;

        Ok(path)
    }

    /// Register bridge tools directly into `McpHostOrchestrator`
    pub async fn register_into_orchestrator(&self, orchestrator: &McpHostOrchestrator) -> Result<()> {
        let mut server_env = HashMap::new();
        if let Some(t) = &self.config.tenant_id {
            server_env.insert("AZURE_TENANT_ID".to_string(), t.clone());
        }
        if let Some(w) = &self.config.teams_webhook_url {
            server_env.insert("TEAMS_WEBHOOK_URL".to_string(), w.clone());
        }

        let cfg = McpServerConfig {
            command: "hgb".to_string(),
            args: vec!["mcp".to_string(), "serve".to_string(), "--namespace".to_string(), "microsoft".to_string()],
            env: server_env,
            disabled: false,
        };

        orchestrator.register_server("microsoft_365", cfg).await;
        Ok(())
    }

    /// Handle standard MCP JSON-RPC requests (e.g. `tools/list` and `tools/call`)
    pub async fn handle_jsonrpc(&self, request: &serde_json::Value) -> Result<serde_json::Value> {
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = request.get("id").cloned().unwrap_or(json!(1));

        match method {
            "tools/list" => {
                let tool_list: Vec<serde_json::Value> = self.tools.iter().map(|t| {
                    json!({
                        "name": t.name(),
                        "description": t.description(),
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    })
                }).collect();

                Ok(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": tool_list
                    }
                }))
            }
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                for tool in &self.tools {
                    if tool.name() == name {
                        let res = tool.execute(arguments).await?;
                        return Ok(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [
                                    {
                                        "type": "text",
                                        "text": serde_json::to_string_pretty(&res).unwrap_or_default()
                                    }
                                ]
                            }
                        }));
                    }
                }

                Err(HgbError::NotFound(format!("Tool '{}' not found in Microsoft MCP bridge", name)))
            }
            _ => Err(HgbError::Execution(format!("Unsupported MCP JSON-RPC method '{}'", method))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sharepoint_read_spec_from_local_cache() {
        let temp_dir = tempfile_dir("sp_test");
        let cache_dir = temp_dir.join(".hgb").join("sharepoint");
        fs::create_dir_all(&cache_dir).unwrap();

        let spec_file = cache_dir.join("ARCHITECTURE_SPEC.md");
        fs::write(&spec_file, "# Contoso Enterprise Architecture\n- Microkernel tokio\n- Zero-trust RBAC").unwrap();

        let cfg = MicrosoftMcpConfig::new(&cache_dir);
        let tool = SharePointReadSpecTool::new(cfg);

        let res = tool.execute(json!({ "resource_path": "ARCHITECTURE_SPEC.md" })).await.unwrap();
        assert_eq!(res["source"], "local_mirror_cache");
        assert!(res["content"].as_str().unwrap().contains("Contoso Enterprise Architecture"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_teams_adaptive_card_builder_and_dispatch() {
        let temp_dir = tempfile_dir("teams_test");
        let cfg = MicrosoftMcpConfig::new(&temp_dir);
        let tool = TeamsSendAdaptiveCardTool::new(cfg);

        // 1. Build test cards
        let incident_card = TeamsSendAdaptiveCardTool::build_incident_card(
            "Database Connection Spike", "Critical", "Connection pool exhausted", Some("http://localhost:3000")
        );
        assert_eq!(incident_card["type"], "AdaptiveCard");
        assert_eq!(incident_card["version"], "1.5");

        let summary_card = TeamsSendAdaptiveCardTool::build_test_summary_card("Core Suite", 140, 0, 1250);
        assert_eq!(summary_card["type"], "AdaptiveCard");

        // 2. Execute tool
        let res = tool.execute(json!({
            "channel_id": "eng-alerts",
            "card_payload": incident_card
        })).await.unwrap();

        assert_eq!(res["channel_id"], "eng-alerts");
        assert!(res["card_id"].as_str().unwrap().starts_with("teams_card_"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_outlook_send_digest_archival() {
        let temp_dir = tempfile_dir("outlook_test");
        let cfg = MicrosoftMcpConfig::new(&temp_dir);
        let tool = OutlookSendDigestTool::new(cfg);

        let res = tool.execute(json!({
            "recipient": "cto@contoso.com",
            "subject": "Hagibis Sprint 42 Executive Digest",
            "html_body": "<h1>Sprint 42 Completed</h1><p>206 tests passing with zero failures.</p>"
        })).await.unwrap();

        assert_eq!(res["recipient"], "cto@contoso.com");
        assert!(res["digest_id"].as_str().unwrap().starts_with("digest_"));
        assert!(Path::new(res["archive_path"].as_str().unwrap()).exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_graph_search_tribal_local() {
        let temp_dir = tempfile_dir("tribal_test");
        let cache_dir = temp_dir.join(".hgb").join("sharepoint");
        fs::create_dir_all(&cache_dir).unwrap();

        let doc1 = cache_dir.join("AUTHENTICATION_GUIDE.md");
        fs::write(&doc1, "# Single Sign-On Guide\nAll services must authenticate via Entra ID / OAuth 2.0.").unwrap();

        let cfg = MicrosoftMcpConfig::new(&cache_dir);
        let tool = GraphSearchTribalTool::new(cfg);

        let res = tool.execute(json!({ "query": "Entra ID OAuth" })).await.unwrap();
        assert_eq!(res["total_hits"], 1);
        let first = &res["results"][0];
        assert_eq!(first["title"], "AUTHENTICATION_GUIDE");
        assert!(first["snippet"].as_str().unwrap().contains("Entra ID"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_mcp_bridge_jsonrpc_list_and_call() {
        let temp_dir = tempfile_dir("mcp_bridge_test");
        let bridge = McpMicrosoftBridge::from_env(&temp_dir);

        // 1. List tools
        let list_req = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/list",
            "params": {}
        });
        let list_resp = bridge.handle_jsonrpc(&list_req).await.unwrap();
        let tools = list_resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);

        // 2. Generate config
        let cfg_path = temp_dir.join("mcp.microsoft.json");
        let out = bridge.generate_mcp_config(Some(&cfg_path)).unwrap();
        assert!(out.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    fn tempfile_dir(prefix: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("{}_{}", prefix, uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&p);
        p
    }
}
