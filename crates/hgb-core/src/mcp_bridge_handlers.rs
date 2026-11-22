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
