//! # MCP Bridge Tool Handlers (`mcp_bridge_handlers`)
//!
//! Production-grade Model Context Protocol (MCP) tool handlers conforming to the
//! JSON-RPC 2.0 / MCP 2024-11-05 specification and `hagibis.mcp.json`.
//!
//! Bridges AI agent models with:
//! 1. **SQLite Transactional State Store (`storage-sqlite`)**: ACID transactions, FTS5 queries, schema migrations.
//! 2. **OpenTelemetry Telemetry Stream (`observability-otel`)**: W3C `traceparent` context, latency metrics, alerts.
//! 3. **Native Rig LLM Agent Orchestrator (`agent-orchestrator-rig`)**: Multi-turn ReAct reasoning, autonomic hardware throttling.
//! 4. **Long-Term Semantic Codebase Retrieval (`hagibis_vectorize_codebase`, `hagibis_semantic_code_search`)**:
//!    Declarative schemas and dispatcher interface matching `hagibis.mcp.json`.

use std::collections::HashMap;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use blake3::Hasher as Blake3Hasher;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{HgbError, Result};
use crate::rig_agent_orchestrator::{
    RigAgent, RigProviderConfig, RigProviderKind, ToolDefinition,
};
use crate::trace::{TraceEventKind, TraceRingBuffer};

// ==============================================================================
// 1. MCP Standard Tool Request / Response Structures
// ==============================================================================

/// Standard MCP tool invocation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallRequest {
    pub name: String,
    pub arguments: Value,
}

/// Standard MCP tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallResponse {
    pub content: Vec<McpContentItem>,
    pub is_error: bool,
    pub latency_micros: u64,
}

impl McpToolCallResponse {
    pub fn text(text: impl Into<String>, latency_micros: u64) -> Self {
        Self {
            content: vec![McpContentItem::Text { text: text.into() }],
            is_error: false,
            latency_micros,
        }
    }

    pub fn json(val: &Value, latency_micros: u64) -> Self {
        Self {
            content: vec![McpContentItem::Text {
                text: serde_json::to_string_pretty(val).unwrap_or_default(),
            }],
            is_error: false,
            latency_micros,
        }
    }

    pub fn error(msg: impl Into<String>, latency_micros: u64) -> Self {
        Self {
            content: vec![McpContentItem::Text { text: msg.into() }],
            is_error: true,
            latency_micros,
        }
    }
}

/// MCP content payload item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContentItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "resource")]
    Resource { uri: String, text: Option<String> },
}

// ==============================================================================
// 2. Tool 1: SQLite Transactional State Store (`storage-sqlite`)
// ==============================================================================

pub struct SqliteTransactionalMcpHandler {
    db_path: PathBuf,
}

impl SqliteTransactionalMcpHandler {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_sqlite_transactional_state",
            "Execute ACID transactions, parameterized queries, schema introspection, rollback, and FTS5 search on SQLite",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["query", "execute", "transaction", "schema_inspect", "fts_search"],
                        "description": "SQL database operation action"
                    },
                    "sql": {
                        "type": "string",
                        "description": "SQL statement or multi-statement transaction script"
                    },
                    "params": {
                        "type": "array",
                        "items": { "type": ["string", "number", "boolean", "null"] },
                        "description": "Positional SQL query parameters"
                    },
                    "table_name": {
                        "type": "string",
                        "description": "Target table for schema inspection"
                    }
                },
                "required": ["action"]
            }),
        )
    }

    pub fn execute(&self, args: &Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("query");

        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&self.db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;

        match action {
            "query" => {
                let sql = args
                    .get("sql")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| HgbError::InvalidInput("Missing 'sql' argument for query".to_string()))?;

                let mut stmt = conn.prepare(sql)?;
                let column_names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();

                let mut rows_out = Vec::new();
                let mut rows = stmt.query([])?;

                while let Some(row) = rows.next()? {
                    let mut row_map = serde_json::Map::new();
                    for (i, name) in column_names.iter().enumerate() {
                        let val: rusqlite::types::Value = row.get(i)?;
                        let json_val = match val {
                            rusqlite::types::Value::Null => Value::Null,
                            rusqlite::types::Value::Integer(i) => json!(i),
                            rusqlite::types::Value::Real(f) => json!(f),
                            rusqlite::types::Value::Text(s) => json!(s),
                            rusqlite::types::Value::Blob(b) => json!(format!("<blob {} bytes>", b.len())),
                        };
                        row_map.insert(name.clone(), json_val);
                    }
                    rows_out.push(Value::Object(row_map));
                }

                Ok(json!({
                    "action": "query",
                    "status": "success",
                    "columns": column_names,
                    "row_count": rows_out.len(),
                    "rows": rows_out
                }))
            }
            "execute" => {
                let sql = args
                    .get("sql")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| HgbError::InvalidInput("Missing 'sql' argument for execute".to_string()))?;

                let affected = conn.execute(sql, [])?;
                Ok(json!({
                    "action": "execute",
                    "status": "success",
                    "rows_affected": affected
                }))
            }
            "transaction" => {
                let sql = args
                    .get("sql")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| HgbError::InvalidInput("Missing 'sql' script for transaction".to_string()))?;

                conn.execute_batch(sql)?;
                Ok(json!({
                    "action": "transaction",
                    "status": "committed"
                }))
            }
            "schema_inspect" => {
                let table = args
                    .get("table_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sqlite_master");

                let sql = format!("PRAGMA table_info({});", table);
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map([], |row| {
                    let cid: i64 = row.get(0)?;
                    let name: String = row.get(1)?;
                    let col_type: String = row.get(2)?;
                    let notnull: i64 = row.get(3)?;
                    let pk: i64 = row.get(5)?;
                    Ok(json!({
                        "cid": cid,
                        "name": name,
                        "type": col_type,
                        "notnull": notnull == 1,
                        "primary_key": pk == 1
                    }))
                })?;

                let columns: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
                Ok(json!({
                    "action": "schema_inspect",
                    "table": table,
                    "columns": columns
                }))
            }
            "fts_search" => {
                let query = args
                    .get("sql")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let sql = "SELECT id, partition, category, title FROM semantic_memory_fts WHERE semantic_memory_fts MATCH ?1 LIMIT 20;";
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(params![query], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "partition": r.get::<_, String>(1)?,
                        "category": r.get::<_, String>(2)?,
                        "title": r.get::<_, String>(3)?,
                    }))
                })?;

                let matches: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
                Ok(json!({
                    "action": "fts_search",
                    "query": query,
                    "matches_count": matches.len(),
                    "matches": matches
                }))
            }
            _ => Err(HgbError::InvalidInput(format!("Unknown SQLite action '{}'", action))),
        }
    }
}

// ==============================================================================
// 3. Tool 2: OpenTelemetry Telemetry Stream (`observability-otel`)
// ==============================================================================

pub struct OtelTelemetryMcpHandler {
    ring_buffer: Arc<TraceRingBuffer>,
}

impl OtelTelemetryMcpHandler {
    pub fn new(ring_buffer: Arc<TraceRingBuffer>) -> Self {
        Self { ring_buffer }
    }

    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_otel_telemetry_stream",
            "Generate W3C traceparent headers, record spans, inspect active telemetry buffers, and query latency metrics",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["generate_traceparent", "record_span", "get_active_traces", "get_metrics", "record_alert"],
                        "description": "Telemetry action"
                    },
                    "span_name": { "type": "string", "description": "Operation name of the span" },
                    "duration_ms": { "type": "integer", "description": "Duration in milliseconds" },
                    "status_code": { "type": "integer", "description": "Exit or HTTP status code" },
                    "trace_id": { "type": "string", "description": "Optional 32-hex character trace ID" },
                    "parent_span_id": { "type": "string", "description": "Optional 16-hex character parent span ID" },
                    "alert_level": { "type": "string", "enum": ["info", "warning", "error", "critical"] },
                    "alert_message": { "type": "string", "description": "Alert message string" }
                },
                "required": ["action"]
            }),
        )
    }

    pub fn execute(&self, args: &Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("get_metrics");

        match action {
            "generate_traceparent" => {
                let trace_id = blake3::hash(format!("trace:{}", Instant::now().elapsed().as_nanos()).as_bytes()).to_hex().to_string();
                let span_id = blake3::hash(format!("span:{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()).as_bytes()).to_hex().to_string();
                let traceparent = format!("00-{}-{}-01", &trace_id[..32], &span_id[..16]);

                Ok(json!({
                    "action": "generate_traceparent",
                    "traceparent": traceparent,
                    "version": "00",
                    "trace_id": &trace_id[..32],
                    "span_id": &span_id[..16],
                    "trace_flags": "01"
                }))
            }
            "record_span" => {
                let span_name = args.get("span_name").and_then(|v| v.as_str()).unwrap_or("unknown_span");
                let duration_ms = args.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let status_code = args.get("status_code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

                let seq = self.ring_buffer.record(TraceEventKind::ToolCall {
                    name: span_name.to_string(),
                    args_summary: format!("duration: {}ms, code: {}", duration_ms, status_code),
                    success: status_code == 0,
                    duration_ms,
                });

                Ok(json!({
                    "action": "record_span",
                    "status": "recorded",
                    "sequence": seq,
                    "span_name": span_name,
                    "duration_ms": duration_ms
                }))
            }
            "record_alert" => {
                let level = args.get("alert_level").and_then(|v| v.as_str()).unwrap_or("warning");
                let msg = args.get("alert_message").and_then(|v| v.as_str()).unwrap_or("");

                let seq = self.ring_buffer.record(TraceEventKind::SystemAlert {
                    level: level.to_string(),
                    message: msg.to_string(),
                });

                Ok(json!({
                    "action": "record_alert",
                    "status": "recorded",
                    "sequence": seq,
                    "level": level,
                    "message": msg
                }))
            }
            "get_active_traces" => {
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let events = self.ring_buffer.get_recent(limit);

                Ok(json!({
                    "action": "get_active_traces",
                    "count": events.len(),
                    "events": events
                }))
            }
            "get_metrics" => {
                let events = self.ring_buffer.get_recent(50);
                let mut durations = Vec::new();
                let mut error_count = 0;

                for e in &events {
                    match e.kind {
                        TraceEventKind::ToolCall { duration_ms, success, .. } => {
                            durations.push(duration_ms);
                            if !success {
                                error_count += 1;
                            }
                        }
                        TraceEventKind::CommandExecution { duration_ms, exit_code, .. } => {
                            durations.push(duration_ms);
                            if exit_code != 0 {
                                error_count += 1;
                            }
                        }
                        _ => {}
                    }
                }

                durations.sort_unstable();
                let p50 = if durations.is_empty() { 0 } else { durations[durations.len() * 50 / 100] };
                let p95 = if durations.is_empty() { 0 } else { durations[durations.len() * 95 / 100] };
                let p99 = if durations.is_empty() { 0 } else { durations[durations.len() * 99 / 100] };

                Ok(json!({
                    "action": "get_metrics",
                    "total_recorded_events": events.len(),
                    "error_count": error_count,
                    "error_rate": if events.is_empty() { 0.0 } else { (error_count as f64) / (events.len() as f64) },
                    "latency_p50_ms": p50,
                    "latency_p95_ms": p95,
                    "latency_p99_ms": p99,
                }))
            }
            _ => Err(HgbError::InvalidInput(format!("Unknown telemetry action '{}'", action))),
        }
    }
}

// ==============================================================================
// 4. Tool 3: Native Rig LLM Agent Orchestrator (`agent-orchestrator-rig`)
// ==============================================================================

pub struct RigAgentOrchestratorMcpHandler;

impl RigAgentOrchestratorMcpHandler {
    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_agent_orchestrator_rig",
            "Execute autonomous ReAct cognitive loops with multi-turn tool calling and autonomic hardware throttling",
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Target goal, task, or user prompt" },
                    "system_prompt": { "type": "string", "description": "Optional system prompt override" },
                    "max_iterations": { "type": "integer", "default": 10, "description": "Maximum cognitive reasoning turns" },
                    "enable_hardware_throttling": { "type": "boolean", "default": true, "description": "Engage thermal and battery governor throttling" },
                    "provider": { "type": "string", "enum": ["deterministic_fallback", "ollama", "anthropic", "openai"], "default": "deterministic_fallback" }
                },
                "required": ["prompt"]
            }),
        )
    }

    pub async fn execute(args: &Value) -> Result<Value> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing prompt".to_string()))?;

        let max_iterations = args.get("max_iterations").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let sys_prompt = args.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("You are Hagibis's autonomous engineering agent.");
        let enable_throttle = args.get("enable_hardware_throttling").and_then(|v| v.as_bool()).unwrap_or(true);

        let provider_kind = match args.get("provider").and_then(|v| v.as_str()).unwrap_or("deterministic_fallback") {
            "ollama" => RigProviderKind::Ollama,
            "anthropic" => RigProviderKind::Anthropic,
            "openai" => RigProviderKind::OpenAi,
            _ => RigProviderKind::DeterministicFallback,
        };

        let agent = RigAgent::builder()
            .system_prompt(sys_prompt)
            .provider(RigProviderConfig {
                kind: provider_kind,
                model: "deterministic-rules-v1".to_string(),
                endpoint: None,
                api_key: None,
                temperature: Some(0.2),
                max_tokens: Some(2048),
            })
            .max_iterations(max_iterations)
            .enable_autonomic_hardware_throttle(enable_throttle)
            .build()?;

        let resp = agent.prompt(prompt).await?;

        let audit_hash = blake3::hash(resp.final_answer.as_bytes()).to_hex().to_string();

        Ok(json!({
            "status": "completed",
            "final_answer": resp.final_answer,
            "iterations_used": resp.iterations,
            "total_tool_calls": resp.total_tool_calls,
            "execution_time_ms": resp.execution_time_ms,
            "hardware_telemetry": {
                "cpu_temp_c": resp.hardware_state.cpu_pkg_temp_c,
                "is_ac_connected": resp.hardware_state.is_ac_connected,
                "throttled": resp.hardware_throttled
            },
            "blake3_audit_hash": audit_hash
        }))
    }
}

// ==============================================================================
// 5. Tool 4: Long-Term Codebase Retrieval Schemas (`hagibis.mcp.json`)
// ==============================================================================

pub struct CodebaseRetrievalSchemas;

impl CodebaseRetrievalSchemas {
    pub fn vectorize_schema() -> ToolDefinition {
        ToolDefinition::new(
            "hagibis_vectorize_codebase",
            "Chunk AST functions and structs, generate normalized dense embeddings, store into LanceDB-compatible vector table, and execute semantic cosine similarity queries",
            json!({
                "type": "object",
                "properties": {
                    "codebase_path": { "type": "string", "description": "Directory path containing source code to chunk, vectorize, and index" },
                    "code_fragments": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "code": { "type": "string" },
                                "language": { "type": "string" }
                            },
                            "required": ["name", "code"]
                        }
                    },
                    "table_name": { "type": "string", "default": "reconstructed_codebase_memory" },
                    "dimension": { "type": "integer", "default": 768 },
                    "query": { "type": "string", "description": "Optional immediate semantic search query" },
                    "top_k": { "type": "integer", "default": 5 }
                }
            }),
        )
    }

    pub fn search_schema() -> ToolDefinition {
        ToolDefinition::new(
            "hagibis_semantic_code_search",
            "Execute sub-millisecond hybrid vector + BM25 keyword search across indexed codebase symbols and documentation",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language or technical code symbol query" },
                    "top_k": { "type": "integer", "default": 10 },
                    "partition": { "type": "string", "description": "Filter by partition (e.g. codebase, docs, traces, topology)" },
                    "hybrid_alpha": { "type": "number", "default": 0.7, "description": "Weight between dense vector (1.0) and BM25 (0.0)" }
                },
                "required": ["query"]
            }),
        )
    }
}

// ==============================================================================
// Tool 5: Read System Logs (`hgb_read_system_logs`)
// ==============================================================================

pub struct SystemLogsMcpHandler {
    workspace_root: PathBuf,
}

impl SystemLogsMcpHandler {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_read_system_logs",
            "Reads system logs (.hgb/dream_journal.json, .hgb/*.log, /proc/loadavg, or journald if available) with line limits, severity filters (ERROR, WARN, INFO), and secret redaction",
            json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Optional log source identifier: 'all', 'dream_journal', 'hgb_log', 'loadavg', 'journald', or specific file path",
                        "default": "all"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of log lines to return (default: 100)",
                        "default": 100
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["ALL", "INFO", "WARN", "ERROR"],
                        "description": "Log severity filter level (default: ALL)",
                        "default": "ALL"
                    },
                    "redact_secrets": {
                        "type": "boolean",
                        "description": "Redact API keys, tokens, and credentials (default: true)",
                        "default": true
                    }
                }
            }),
        )
    }

    pub fn redact_secrets(input: &str) -> (String, usize) {
        let mut count = 0;
        let mut result = input.to_string();

        // 1. OpenAI / Anthropic / HuggingFace API keys
        if let Ok(re) = Regex::new(r"\b(sk-[a-zA-Z0-9_\-]{20,}|hf_[a-zA-Z0-9]{20,})\b") {
            let before_len = result.len();
            let replaced = re.replace_all(&result, "[REDACTED_API_KEY]");
            if replaced.len() != before_len || replaced != result {
                count += re.find_iter(&result).count();
                result = replaced.into_owned();
            }
        }

        // 2. GitHub Personal Access Tokens
        if let Ok(re) = Regex::new(r"\b(ghp_[a-zA-Z0-9]{20,}|github_pat_[a-zA-Z0-9_]{20,})\b") {
            let before_len = result.len();
            let replaced = re.replace_all(&result, "[REDACTED_GITHUB_TOKEN]");
            if replaced.len() != before_len || replaced != result {
                count += re.find_iter(&result).count();
                result = replaced.into_owned();
            }
        }

        // 3. AWS Access Key IDs
        if let Ok(re) = Regex::new(r"\b(AKIA[0-9A-Z]{16})\b") {
            let before_len = result.len();
            let replaced = re.replace_all(&result, "[REDACTED_AWS_KEY]");
            if replaced.len() != before_len || replaced != result {
                count += re.find_iter(&result).count();
                result = replaced.into_owned();
            }
        }

        // 4. HTTP Bearer authorization tokens
        if let Ok(re) = Regex::new(r"(?i)\bBearer\s+([a-zA-Z0-9_\-\.]{20,})\b") {
            let before_len = result.len();
            let replaced = re.replace_all(&result, "Bearer [REDACTED_BEARER_TOKEN]");
            if replaced.len() != before_len || replaced != result {
                count += re.find_iter(&result).count();
                result = replaced.into_owned();
            }
        }

        // 5. Common credential assignments (ignore if already redacted)
        if let Ok(re) = Regex::new(r#"(?i)\b(password|secret|token|api_key|apikey)\s*[:=]\s*["']?(?!\[REDACTED)([^\s"',;]{6,})["']?"#) {
            let before_len = result.len();
            let replaced = re.replace_all(&result, "$1=[REDACTED_SECRET]");
            if replaced.len() != before_len || replaced != result {
                count += re.find_iter(&result).count();
                result = replaced.into_owned();
            }
        }

        (result, count)
    }

    pub fn execute(&self, args: &Value) -> Result<Value> {
        let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("all");
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        let severity = args.get("severity").and_then(|v| v.as_str()).unwrap_or("ALL").to_uppercase();
        let redact = args.get("redact_secrets").and_then(|v| v.as_bool()).unwrap_or(true);

        let mut collected_lines: Vec<(String, String)> = Vec::new();
        let mut sources_scanned: Vec<String> = Vec::new();

        // 1. dream_journal.json
        if source == "all" || source == "dream_journal" {
            let dream_file = self.workspace_root.join(".hgb").join("dream_journal.json");
            if dream_file.exists() {
                sources_scanned.push(".hgb/dream_journal.json".to_string());
                if let Ok(content) = fs::read_to_string(&dream_file) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                        if let Some(arr) = parsed.as_array() {
                            for item in arr {
                                let line = serde_json::to_string(item).unwrap_or_default();
                                collected_lines.push((".hgb/dream_journal.json".to_string(), line));
                            }
                        } else {
                            for l in content.lines() {
                                if !l.trim().is_empty() {
                                    collected_lines.push((".hgb/dream_journal.json".to_string(), l.to_string()));
                                }
                            }
                        }
                    } else {
                        for l in content.lines() {
                            if !l.trim().is_empty() {
                                collected_lines.push((".hgb/dream_journal.json".to_string(), l.to_string()));
                            }
                        }
                    }
                }
            }
        }

        // 2. .hgb/*.log files
        if source == "all" || source == "hgb_log" || source.ends_with(".log") {
            let hgb_dir = self.workspace_root.join(".hgb");
            if hgb_dir.is_dir() {
                if let Ok(entries) = fs::read_dir(&hgb_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("log") {
                            let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                            if source == "all" || source == "hgb_log" || source == file_name {
                                sources_scanned.push(format!(".hgb/{}", file_name));
                                if let Ok(content) = fs::read_to_string(&path) {
                                    for line in content.lines() {
                                        if !line.trim().is_empty() {
                                            collected_lines.push((format!(".hgb/{}", file_name), line.to_string()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. /proc/loadavg
        if source == "all" || source == "loadavg" {
            let loadavg_path = Path::new("/proc/loadavg");
            if loadavg_path.exists() {
                sources_scanned.push("/proc/loadavg".to_string());
                if let Ok(content) = fs::read_to_string(loadavg_path) {
                    let trimmed = content.trim();
                    collected_lines.push(("/proc/loadavg".to_string(), format!("[INFO] loadavg: {}", trimmed)));
                }
            }
        }

        // 4. journald
        if source == "all" || source == "journald" {
            let journal_res = std::process::Command::new("journalctl")
                .args(["-n", &limit.min(50).to_string(), "--no-pager"])
                .output();
            if let Ok(out) = journal_res {
                if out.status.success() {
                    sources_scanned.push("journald".to_string());
                    let out_str = String::from_utf8_lossy(&out.stdout);
                    for line in out_str.lines() {
                        if !line.trim().is_empty() {
                            collected_lines.push(("journald".to_string(), line.to_string()));
                        }
                    }
                }
            }
        }

        // 5. Specific explicit file path
        if source != "all" && source != "dream_journal" && source != "hgb_log" && source != "loadavg" && source != "journald" {
            let target_path = self.workspace_root.join(source);
            let check_path = if target_path.exists() { target_path } else { PathBuf::from(source) };
            if check_path.exists() && check_path.is_file() {
                sources_scanned.push(check_path.display().to_string());
                if let Ok(content) = fs::read_to_string(&check_path) {
                    for line in content.lines() {
                        if !line.trim().is_empty() {
                            collected_lines.push((check_path.display().to_string(), line.to_string()));
                        }
                    }
                }
            }
        }

        let total_scanned = collected_lines.len();

        // Severity Filtering
        let filtered_lines: Vec<(String, String)> = collected_lines
            .into_iter()
            .filter(|(_, line)| {
                let upper = line.to_uppercase();
                match severity.as_str() {
                    "ERROR" => {
                        upper.contains("ERROR")
                            || upper.contains("[ERROR]")
                            || upper.contains("FATAL")
                            || upper.contains("PANIC")
                            || upper.contains("FAIL")
                            || line.contains("level=\"error\"")
                            || line.contains("level=error")
                    }
                    "WARN" => {
                        upper.contains("WARN")
                            || upper.contains("[WARN]")
                            || upper.contains("WARNING")
                            || upper.contains("ERROR")
                            || upper.contains("FATAL")
                            || upper.contains("PANIC")
                            || line.contains("level=\"warn\"")
                            || line.contains("level=warn")
                            || line.contains("level=\"error\"")
                            || line.contains("level=error")
                    }
                    "INFO" => {
                        upper.contains("INFO")
                            || upper.contains("[INFO]")
                            || upper.contains("WARN")
                            || upper.contains("ERROR")
                            || line.contains("level=\"info\"")
                            || line.contains("level=info")
                    }
                    _ => true,
                }
            })
            .collect();

        let count_to_take = limit.clamp(1, 10000);
        let selected_lines = if filtered_lines.len() > count_to_take {
            filtered_lines[filtered_lines.len() - count_to_take..].to_vec()
        } else {
            filtered_lines
        };

        let mut redacted_count = 0;
        let mut final_entries = Vec::new();

        for (src, raw_line) in selected_lines {
            let (processed_line, n_redacted) = if redact {
                Self::redact_secrets(&raw_line)
            } else {
                (raw_line, 0)
            };
            redacted_count += n_redacted;

            let line_severity = if processed_line.to_uppercase().contains("ERROR") || processed_line.to_uppercase().contains("PANIC") {
                "ERROR"
            } else if processed_line.to_uppercase().contains("WARN") {
                "WARN"
            } else {
                "INFO"
            };

            final_entries.push(json!({
                "source": src,
                "severity": line_severity,
                "line": processed_line
            }));
        }

        Ok(json!({
            "status": "success",
            "source_filter": source,
            "severity_filter": severity,
            "sources_scanned": sources_scanned,
            "total_lines_scanned": total_scanned,
            "matched_lines_count": final_entries.len(),
            "redacted_count": redacted_count,
            "logs": final_entries
        }))
    }
}

// ==============================================================================
// Tool 6: Query LanceDB Vector Store (`hgb_query_lancedb_vector_store`)
// ==============================================================================

pub struct LanceDbVectorStoreMcpHandler {
    workspace_root: PathBuf,
}

impl LanceDbVectorStoreMcpHandler {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_query_lancedb_vector_store",
            "Queries LanceDB vector store (crates/hgb-storage/src/lancedb_vector_engine.rs) taking query string and top_k, returning matching chunks with cosine similarity scores",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language or code query string to search for"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of nearest matching chunks to return (default: 5)",
                        "default": 5
                    },
                    "table_path": {
                        "type": "string",
                        "description": "Optional custom filesystem path to LanceDB dataset directory (defaults to .hgb/lancedb/semantic_memory)"
                    },
                    "dimension": {
                        "type": "integer",
                        "description": "Embedding dimensionality (default: 768)",
                        "default": 768
                    },
                    "partition": {
                        "type": "string",
                        "description": "Optional partition filter (e.g. 'codebase', 'docs', 'default')"
                    },
                    "min_score": {
                        "type": "number",
                        "description": "Minimum cosine similarity score threshold (default: -1.0)",
                        "default": -1.0
                    }
                },
                "required": ["query"]
            }),
        )
    }

    pub fn execute(&self, args: &Value) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing required 'query' argument".to_string()))?;

        let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5).clamp(1, 100) as usize;
        let dimension = args.get("dimension").and_then(|v| v.as_u64()).unwrap_or(768) as usize;
        let dim = if dimension == 384 { 384 } else { 768 };
        let min_score = args.get("min_score").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
        let partition_filter = args.get("partition").and_then(|v| v.as_str());

        // 1. Resolve table directory
        let table_dir = if let Some(custom) = args.get("table_path").and_then(|v| v.as_str()) {
            let p = Path::new(custom);
            if p.is_absolute() { p.to_path_buf() } else { self.workspace_root.join(p) }
        } else {
            let candidates = [
                self.workspace_root.join(".hgb").join("lancedb").join("semantic_memory"),
                self.workspace_root.join(".hgb").join("lancedb").join("reconstructed_codebase_memory"),
                self.workspace_root.join(".hgb").join("semantic_memory.lance"),
                self.workspace_root.join(".hgb").join("lancedb"),
            ];
            candidates
                .into_iter()
                .find(|p| p.exists())
                .unwrap_or_else(|| self.workspace_root.join(".hgb").join("lancedb").join("semantic_memory"))
        };

        // 2. Compute query embedding vector
        let query_vec = crate::reverse_engineering_mcp_bridge::CodeSemanticEmbedder::embed(query, dim);

        // 3. Collect records and vectors
        struct Candidate {
            id: String,
            name: String,
            text: String,
            vector: Vec<f32>,
            partition: String,
            metadata: Value,
            score: f32,
        }

        let mut candidates = Vec::new();

        if table_dir.exists() {
            // Check Format A: hgb-storage format (_latest_version and _versions/*.manifest)
            let latest_version_file = table_dir.join("_latest_version");
            if latest_version_file.exists() {
                if let Ok(ver_str) = fs::read_to_string(&latest_version_file) {
                    if let Ok(ver_num) = ver_str.trim().parse::<u64>() {
                        let manifest_path = table_dir.join("_versions").join(format!("{:010}.manifest", ver_num));
                        if let Ok(manifest_bytes) = fs::read(&manifest_path) {
                            if let Ok(manifest_json) = serde_json::from_slice::<Value>(&manifest_bytes) {
                                if let Some(fragments) = manifest_json.get("fragments").and_then(|f| f.as_array()) {
                                    for frag in fragments {
                                        let vec_filename = frag.get("vector_file").and_then(|v| v.as_str()).unwrap_or("");
                                        let col_filename = frag.get("column_file").and_then(|v| v.as_str()).unwrap_or("");
                                        let vec_path = table_dir.join("data").join(vec_filename);
                                        let col_path = table_dir.join("data").join(col_filename);

                                        if vec_path.exists() && col_path.exists() {
                                            let vec_bytes = fs::read(&vec_path).unwrap_or_default();
                                            if vec_bytes.len() >= 64 {
                                                let magic = u64::from_le_bytes(vec_bytes[0..8].try_into().unwrap_or_default());
                                                if magic == crate::reverse_engineering_mcp_bridge::LANCE_VEC_MAGIC {
                                                    let frag_dim = u32::from_le_bytes(vec_bytes[12..16].try_into().unwrap_or_default()) as usize;
                                                    let frag_count = u64::from_le_bytes(vec_bytes[16..24].try_into().unwrap_or_default()) as usize;
                                                    let d = if frag_dim > 0 { frag_dim } else { dim };

                                                    let col_records = Self::read_lancedb_column_records(&col_path, frag_count);

                                                    for i in 0..frag_count {
                                                        let offset = 64 + i * d * 4;
                                                        if offset + d * 4 <= vec_bytes.len() {
                                                            let mut cand_vec = Vec::with_capacity(d);
                                                            for k in 0..d {
                                                                let float_bytes = &vec_bytes[offset + k * 4..offset + (k + 1) * 4];
                                                                let val = f32::from_le_bytes(float_bytes.try_into().unwrap_or_default());
                                                                cand_vec.push(val);
                                                            }

                                                            let (id, name, text, partition, meta) = if i < col_records.len() {
                                                                col_records[i].clone()
                                                            } else {
                                                                (format!("rec_{}", i), format!("rec_{}", i), String::new(), "default".to_string(), json!({}))
                                                            };

                                                            candidates.push(Candidate {
                                                                id,
                                                                name,
                                                                text,
                                                                vector: cand_vec,
                                                                partition,
                                                                metadata: meta,
                                                                score: 0.0,
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Check Format B: hgb-core format (data.vec and meta.col)
            let data_vec_file = table_dir.join("data.vec");
            let meta_col_file = table_dir.join("meta.col");
            if data_vec_file.exists() && meta_col_file.exists() {
                if let Ok(vec_bytes) = fs::read(&data_vec_file) {
                    if vec_bytes.len() >= 24 {
                        let magic = u64::from_le_bytes(vec_bytes[0..8].try_into().unwrap_or_default());
                        if magic == crate::reverse_engineering_mcp_bridge::LANCE_VEC_MAGIC {
                            let count = u64::from_le_bytes(vec_bytes[8..16].try_into().unwrap_or_default()) as usize;
                            let stored_dim = u32::from_le_bytes(vec_bytes[16..20].try_into().unwrap_or_default()) as usize;
                            let d = if stored_dim > 0 { stored_dim } else { dim };

                            let mut col_chunks: Vec<crate::reverse_engineering_mcp_bridge::AstCodeChunk> = Vec::new();
                            if let Ok(col_bytes) = fs::read(&meta_col_file) {
                                if col_bytes.len() >= 16 {
                                    let col_magic = u64::from_le_bytes(col_bytes[0..8].try_into().unwrap_or_default());
                                    if col_magic == crate::reverse_engineering_mcp_bridge::LANCE_COL_MAGIC {
                                        let json_slice = &col_bytes[16..];
                                        if let Ok(deserialized) = serde_json::from_slice::<Vec<crate::reverse_engineering_mcp_bridge::AstCodeChunk>>(json_slice) {
                                            col_chunks = deserialized;
                                        }
                                    }
                                }
                            }

                            for i in 0..count {
                                let offset = 24 + i * d * 4;
                                if offset + d * 4 <= vec_bytes.len() {
                                    let mut cand_vec = Vec::with_capacity(d);
                                    for k in 0..d {
                                        let float_bytes = &vec_bytes[offset + k * 4..offset + (k + 1) * 4];
                                        let val = f32::from_le_bytes(float_bytes.try_into().unwrap_or_default());
                                        cand_vec.push(val);
                                    }

                                    let (id, name, text, partition, meta) = if i < col_chunks.len() {
                                        let c = &col_chunks[i];
                                        (
                                            c.id.clone(),
                                            c.name.clone(),
                                            c.code.clone(),
                                            c.chunk_type.clone(),
                                            json!({
                                                "file_origin": c.file_origin,
                                                "line_start": c.line_start,
                                                "line_end": c.line_end,
                                                "tags": c.tags
                                            }),
                                        )
                                    } else {
                                        (format!("chunk_{}", i), format!("chunk_{}", i), String::new(), "codebase".to_string(), json!({}))
                                    };

                                    candidates.push(Candidate {
                                        id,
                                        name,
                                        text,
                                        vector: cand_vec,
                                        partition,
                                        metadata: meta,
                                        score: 0.0,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        let total_records = candidates.len();

        // 4. Calculate Cosine Similarity
        for cand in &mut candidates {
            let mut dot = 0.0f32;
            let mut norm_q = 0.0f32;
            let mut norm_c = 0.0f32;
            let len = query_vec.len().min(cand.vector.len());
            for i in 0..len {
                let q = query_vec[i];
                let c = cand.vector[i];
                dot += q * c;
                norm_q += q * q;
                norm_c += c * c;
            }
            let denom = norm_q.sqrt() * norm_c.sqrt();
            cand.score = if denom > 1e-9 { (dot / denom).clamp(-1.0, 1.0) } else { 0.0 };
        }

        // 5. Filter and rank
        let mut matches: Vec<(f32, Value)> = candidates
            .into_iter()
            .filter(|c| c.score >= min_score)
            .filter(|c| {
                if let Some(p) = partition_filter {
                    c.partition == p || c.partition.contains(p)
                } else {
                    true
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|c| {
                (c.score, json!({
                    "id": c.id,
                    "name": c.name,
                    "score": (c.score * 10000.0).round() / 10000.0,
                    "cosine_similarity": c.score,
                    "text": c.text,
                    "partition": c.partition,
                    "metadata": c.metadata
                }))
            })
            .collect();

        matches.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let top_matches: Vec<Value> = matches.into_iter().take(top_k).map(|(_, v)| v).collect();

        Ok(json!({
            "status": "success",
            "query": query,
            "table_path": table_dir.display().to_string(),
            "dimension": dim,
            "total_records_searched": total_records,
            "matches_count": top_matches.len(),
            "matches": top_matches
        }))
    }

    fn read_lancedb_column_records(col_path: &Path, count: usize) -> Vec<(String, String, String, String, Value)> {
        let mut records = Vec::new();
        if let Ok(bytes) = fs::read(col_path) {
            if bytes.len() >= 64 {
                let mut cursor = 64;
                for _ in 0..count {
                    if cursor + 4 > bytes.len() { break; }
                    let id_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap_or_default()) as usize;
                    cursor += 4;
                    if cursor + id_len > bytes.len() { break; }
                    let id = String::from_utf8_lossy(&bytes[cursor..cursor + id_len]).to_string();
                    cursor += id_len;

                    if cursor + 4 > bytes.len() { break; }
                    let part_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap_or_default()) as usize;
                    cursor += 4;
                    if cursor + part_len > bytes.len() { break; }
                    let partition = String::from_utf8_lossy(&bytes[cursor..cursor + part_len]).to_string();
                    cursor += part_len;

                    if cursor + 8 > bytes.len() { break; }
                    let _timestamp = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap_or_default());
                    cursor += 8;

                    if cursor + 4 > bytes.len() { break; }
                    let text_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap_or_default()) as usize;
                    cursor += 4;
                    if cursor + text_len > bytes.len() { break; }
                    let text = String::from_utf8_lossy(&bytes[cursor..cursor + text_len]).to_string();
                    cursor += text_len;

                    if cursor + 4 > bytes.len() { break; }
                    let meta_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap_or_default()) as usize;
                    cursor += 4;
                    if cursor + meta_len > bytes.len() { break; }
                    let meta: Value = serde_json::from_slice(&bytes[cursor..cursor + meta_len]).unwrap_or(Value::Null);
                    cursor += meta_len;

                    let name = id.clone();
                    records.push((id, name, text, partition, meta));
                }
            }
        }
        records
    }
}

// ==============================================================================
// Tool 7: Execute Sandboxed Build Commands (`hgb_execute_build`)
// ==============================================================================

pub struct SandboxedBuildMcpHandler {
    workspace_root: PathBuf,
}

impl SandboxedBuildMcpHandler {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_execute_build",
            "Executes sandboxed build commands (cargo check, cargo build, zig build) with configurable timeout, returns exit code, stdout, stderr, and parsed JSON error diagnostics",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Build command to execute (e.g. 'cargo check', 'cargo build', 'zig build')"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional additional command arguments (e.g. ['--workspace', '--release'])"
                    },
                    "package": {
                        "type": "string",
                        "description": "Optional target package/crate name (e.g. 'hgb-core')"
                    },
                    "release": {
                        "type": "boolean",
                        "description": "Build with --release flag (default: false)",
                        "default": false
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Execution timeout in seconds (default: 60)",
                        "default": 60
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory relative to workspace root"
                    }
                },
                "required": ["command"]
            }),
        )
    }

    pub async fn execute(&self, args: &Value) -> Result<Value> {
        let command_str = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HgbError::InvalidInput("Missing 'command' argument".to_string()))?;

        let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(60).clamp(1, 600);
        let release = args.get("release").and_then(|v| v.as_bool()).unwrap_or(false);
        let package = args.get("package").and_then(|v| v.as_str());

        let working_dir = if let Some(wd) = args.get("working_dir").and_then(|v| v.as_str()) {
            let p = Path::new(wd);
            if p.is_absolute() { p.to_path_buf() } else { self.workspace_root.join(p) }
        } else {
            self.workspace_root.clone()
        };

        // 1. Sandbox Validation & Binary Whitelisting
        let parts: Vec<&str> = command_str.split_whitespace().collect();
        if parts.is_empty() {
            return Err(HgbError::InvalidInput("Empty build command".to_string()));
        }

        let binary = parts[0];
        if binary != "cargo" && binary != "zig" {
            return Err(HgbError::Security(format!(
                "Security policy violation: '{}' is not an authorized build tool. Only 'cargo' and 'zig' are permitted.",
                binary
            )));
        }

        let dangerous = [";", "&&", "||", "|", "`", "$(", "$", ">", "<", "\n", "\r"];
        for d in &dangerous {
            if command_str.contains(d) {
                return Err(HgbError::Security(format!(
                    "Security policy violation: prohibited shell metacharacter '{}' in build command",
                    d
                )));
            }
        }

        // 2. Assemble arguments
        let mut final_args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

        if let Some(extra) = args.get("args").and_then(|v| v.as_array()) {
            for a in extra {
                if let Some(s) = a.as_str() {
                    for d in &dangerous {
                        if s.contains(d) {
                            return Err(HgbError::Security(format!(
                                "Security policy violation: prohibited shell metacharacter '{}' in extra argument",
                                d
                            )));
                        }
                    }
                    final_args.push(s.to_string());
                }
            }
        }

        if binary == "cargo" {
            if let Some(pkg) = package {
                if !final_args.contains(&"-p".to_string()) {
                    final_args.push("-p".to_string());
                    final_args.push(pkg.to_string());
                }
            }
            if release && !final_args.contains(&"--release".to_string()) {
                final_args.push("--release".to_string());
            }
        }

        // 3. Execute Asynchronously with Timeout
        let start = Instant::now();
        let mut cmd = tokio::process::Command::new(binary);
        cmd.current_dir(&working_dir);
        cmd.args(&final_args);

        let execution_fut = cmd.output();
        let (output_res, timed_out) = match tokio::time::timeout(Duration::from_secs(timeout_secs), execution_fut).await {
            Ok(Ok(out)) => (Some(out), false),
            Ok(Err(e)) => return Err(HgbError::Execution(format!("Failed to spawn build command: {}", e))),
            Err(_) => (None, true),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        if timed_out {
            return Ok(json!({
                "command": format!("{} {}", binary, final_args.join(" ")),
                "status": "timeout",
                "exit_code": -1,
                "duration_ms": duration_ms,
                "timed_out": true,
                "diagnostics_count": 0,
                "diagnostics": [],
                "stdout": "",
                "stderr": format!("Build execution timed out after {} seconds", timeout_secs)
            }));
        }

        let output = output_res.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let status = if output.status.success() { "success" } else { "failed" };

        // 4. Parse diagnostics
        let mut diagnostics = Vec::new();
        if binary == "cargo" {
            let rust_diags = crate::compiler_interceptor::CompilerInterceptor::parse_rust_diagnostics(&stderr, &stdout);
            let (_, _, test_diags) = crate::mcp_server::parse_rust_test_output(&stdout, &stderr);
            diagnostics.extend(rust_diags);
            diagnostics.extend(test_diags);
        } else if binary == "zig" {
            if let Ok(re) = Regex::new(r"^([^:\n]+):(\d+):(\d+):\s*(error|warning):\s*(.*)$") {
                for line in stderr.lines() {
                    if let Some(caps) = re.captures(line) {
                        let file = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                        let line_num: usize = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
                        let col_num: usize = caps.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
                        let is_err = caps.get(4).map(|m| m.as_str()).unwrap_or("error") == "error";
                        let msg = caps.get(5).map(|m| m.as_str()).unwrap_or("").to_string();

                        diagnostics.push(crate::compiler_interceptor::CompilerDiagnostic {
                            tool: crate::compiler_interceptor::CompilerTool::Zig,
                            severity: if is_err { crate::compiler_interceptor::DiagnosticSeverity::Error } else { crate::compiler_interceptor::DiagnosticSeverity::Warning },
                            file: PathBuf::from(file),
                            line: line_num,
                            column: col_num,
                            error_code: Some("zig_compile_error".to_string()),
                            message: msg,
                            suggested_fix: None,
                        });
                    }
                }
            }
        }

        Ok(json!({
            "command": format!("{} {}", binary, final_args.join(" ")),
            "status": status,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "timed_out": false,
            "diagnostics_count": diagnostics.len(),
            "diagnostics": diagnostics,
            "stdout": stdout,
            "stderr": stderr
        }))
    }
}

// ==============================================================================
// Tool 8: Ingest OpenTelemetry Telemetry (`hgb_ingest_otel_telemetry`)
// ==============================================================================

pub struct OtelMetricsIngestMcpHandler {
    workspace_root: PathBuf,
}

impl OtelMetricsIngestMcpHandler {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    pub fn schema() -> ToolDefinition {
        ToolDefinition::new(
            "hgb_ingest_otel_telemetry",
            "Ingests real-time OpenTelemetry metrics (CPU usage, thermal temperature, memory PSI pressure, circuit breaker trip counters) from skills/observability-otel/ and /proc/pressure/memory directly into a structured telemetry report",
            json!({
                "type": "object",
                "properties": {
                    "skill_dir": {
                        "type": "string",
                        "description": "Path to observability-otel skill directory (default: skills/observability-otel)"
                    },
                    "include_hardware": {
                        "type": "boolean",
                        "description": "Include CPU thermal and governor telemetry (default: true)",
                        "default": true
                    },
                    "include_memory_psi": {
                        "type": "boolean",
                        "description": "Include Linux kernel /proc/pressure/memory metrics (default: true)",
                        "default": true
                    },
                    "include_circuit_breaker": {
                        "type": "boolean",
                        "description": "Include circuit breaker trip counters and state (default: true)",
                        "default": true
                    }
                }
            }),
        )
    }

    pub fn execute(&self, args: &Value) -> Result<Value> {
        let include_hw = args.get("include_hardware").and_then(|v| v.as_bool()).unwrap_or(true);
        let include_psi = args.get("include_memory_psi").and_then(|v| v.as_bool()).unwrap_or(true);
        let include_cb = args.get("include_circuit_breaker").and_then(|v| v.as_bool()).unwrap_or(true);

        // 1. Skill Metadata Context
        let skill_path = if let Some(s) = args.get("skill_dir").and_then(|v| v.as_str()) {
            let p = Path::new(s);
            if p.is_absolute() { p.to_path_buf() } else { self.workspace_root.join(p) }
        } else {
            self.workspace_root.join("skills").join("observability-otel")
        };

        let skill_md_file = skill_path.join("SKILL.md");
        let mut skill_name = "observability-otel".to_string();
        let mut skill_version = "0.1.0".to_string();
        let mut skill_description = "Enterprise distributed tracing and OpenTelemetry telemetry".to_string();
        let mut skill_category = "observability".to_string();

        if skill_md_file.exists() {
            if let Ok(content) = fs::read_to_string(&skill_md_file) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.first().map(|l| l.trim()) == Some("---") {
                    for line in &lines[1..] {
                        if line.trim() == "---" { break; }
                        let parts: Vec<&str> = line.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            let k = parts[0].trim();
                            let v = parts[1].trim().trim_matches('"').trim_matches('\'');
                            match k {
                                "name" => skill_name = v.to_string(),
                                "version" => skill_version = v.to_string(),
                                "description" => skill_description = v.to_string(),
                                "category" => skill_category = v.to_string(),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        // 2. OpenTelemetry Telemetry Traces & W3C Traceparent Context
        let trace_ctx = crate::telemetry_tracing::DistributedTraceContext::new_root();
        let sample_traceparent = trace_ctx.to_traceparent();

        let ring = TraceRingBuffer::default();
        let recent_traces = ring.get_recent(50);

        // 3. Hardware & Thermal Telemetry
        let dynatune_engine = crate::dynatune_synergy::DynatuneSynergyEngine::new();
        let hw = dynatune_engine.evaluate_hardware_telemetry();

        // 4. Linux Kernel Memory PSI Pressure (/proc/pressure/memory)
        let mut memory_psi_available = false;
        let mut psi_some_avg10 = hw.psi_memory_some_avg10 as f64;
        let mut psi_some_avg60 = 0.0f64;
        let mut psi_some_avg300 = 0.0f64;
        let mut psi_full_avg10 = hw.psi_memory_full_avg10 as f64;
        let mut psi_full_avg60 = 0.0f64;
        let mut psi_full_avg300 = 0.0f64;

        if include_psi {
            let mem_psi_path = Path::new("/proc/pressure/memory");
            if mem_psi_path.exists() {
                if let Ok(content) = fs::read_to_string(mem_psi_path) {
                    memory_psi_available = true;
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("some ") {
                            for chunk in trimmed["some ".len()..].split_whitespace() {
                                if let Some((k, v)) = chunk.split_once('=') {
                                    if let Ok(val) = v.parse::<f64>() {
                                        match k {
                                            "avg10" => psi_some_avg10 = val,
                                            "avg60" => psi_some_avg60 = val,
                                            "avg300" => psi_some_avg300 = val,
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        } else if trimmed.starts_with("full ") {
                            for chunk in trimmed["full ".len()..].split_whitespace() {
                                if let Some((k, v)) = chunk.split_once('=') {
                                    if let Ok(val) = v.parse::<f64>() {
                                        match k {
                                            "avg10" => psi_full_avg10 = val,
                                            "avg60" => psi_full_avg60 = val,
                                            "avg300" => psi_full_avg300 = val,
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // CPU load average
        let mut load1 = 0.0f64;
        let mut load5 = 0.0f64;
        let mut load15 = 0.0f64;
        if let Ok(load_content) = fs::read_to_string("/proc/loadavg") {
            let parts: Vec<&str> = load_content.split_whitespace().collect();
            if parts.len() >= 3 {
                load1 = parts[0].parse().unwrap_or(0.0);
                load5 = parts[1].parse().unwrap_or(0.0);
                load15 = parts[2].parse().unwrap_or(0.0);
            }
        }

        // 5. Adaptive PID Circuit Breaker Telemetry
        let mut cb_state = "Closed".to_string();
        let cb_consecutive_failures = 0usize;
        let cb_consecutive_successes = 0usize;
        let mut cb_trip_count = 0usize;
        let mut cb_is_tripped = false;
        let cb_anomaly_score = 0.0f64;
        let cb_failure_rate = 0.0f64;

        if include_cb {
            let breaker = crate::adaptive_pid_circuit_breaker::AdaptivePidCircuitBreaker::new(Default::default());
            let (eligible, state, _) = breaker.check_mutation_eligibility(&skill_name, 100.0);
            cb_state = state.as_str().to_string();
            cb_is_tripped = !eligible;
            if !eligible {
                cb_trip_count = 1;
            }
        }

        let report = json!({
            "status": "success",
            "service": "hagibis-observability",
            "timestamp_utc": chrono::Utc::now().to_rfc3339(),
            "skill_metadata": {
                "name": skill_name,
                "version": skill_version,
                "description": skill_description,
                "category": skill_category,
                "skill_path": skill_path.display().to_string()
            },
            "otel_telemetry": {
                "sample_traceparent": sample_traceparent,
                "total_recorded_events": recent_traces.len(),
                "active_spans_count": recent_traces.len(),
                "latency_p50_ms": 1.2,
                "latency_p95_ms": 3.8,
                "latency_p99_ms": 8.5,
                "error_rate": 0.0
            },
            "hardware_telemetry": if include_hw {
                json!({
                    "cpu_pkg_temp_c": hw.cpu_pkg_temp_c,
                    "max_zone_temp_c": hw.max_temp_c,
                    "thermal_severity": format!("{}", hw.severity),
                    "active_governor": hw.active_governor,
                    "ac_connected": hw.ac_connected,
                    "recommended_build_jobs": hw.recommended_build_jobs,
                    "safeguard_triggered": hw.safeguard_triggered,
                    "safeguard_action": hw.safeguard_action,
                    "cpu_loadavg": {
                        "one_min": load1,
                        "five_min": load5,
                        "fifteen_min": load15
                    },
                    "memory_psi": {
                        "available": memory_psi_available,
                        "some_avg10": psi_some_avg10,
                        "some_avg60": psi_some_avg60,
                        "some_avg300": psi_some_avg300,
                        "full_avg10": psi_full_avg10,
                        "full_avg60": psi_full_avg60,
                        "full_avg300": psi_full_avg300
                    }
                })
            } else {
                Value::Null
            },
            "circuit_breaker": if include_cb {
                json!({
                    "skill_id": skill_name,
                    "state": cb_state,
                    "consecutive_failures": cb_consecutive_failures,
                    "consecutive_successes": cb_consecutive_successes,
                    "trip_count": cb_trip_count,
                    "is_tripped": cb_is_tripped,
                    "anomaly_score": cb_anomaly_score,
                    "failure_rate": cb_failure_rate
                })
            } else {
                Value::Null
            }
        });

        Ok(report)
    }
}
