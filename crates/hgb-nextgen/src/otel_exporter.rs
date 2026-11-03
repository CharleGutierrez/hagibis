//! # OpenTelemetry Exporter & Telemetry Tracer (`hgb-nextgen`)
//!
//! Provides built-in OpenTelemetry (OTel) metrics and trace export/ingestion capabilities:
//! - `/v1/metrics`: Serves OpenTelemetry metrics in standard OTLP JSON or Prometheus text format:
//!   - Tokens consumed (prompt, completion, total, burn velocity).
//!   - Active Firecracker microVMs and snapshot cache state.
//!   - WebAssembly runtime memory RSS and linear heap pages.
//!   - Linux CPU frequency scaling, active governor, EPP, and thermal state.
//!   - Vector memory HNSW search latency and similarity scores.
//! - `/v1/traces`: Ingests and exports OpenTelemetry trace spans:
//!   - MCP tool calls (status, duration, fuel consumed, arguments hash).
//!   - Sub-agent reinforcement loop steps (agent ID, iteration, token count).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::cockpit_web_hud::{CockpitWebTelemetry, McpToolInvocationRecord};

/// Default capacity for trace store ring buffer
pub const DEFAULT_MAX_TRACE_CAPACITY: usize = 1000;

/// OpenTelemetry trace span representing an operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtelTraceSpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub duration_ms: f64,
    pub status_code: String,
    pub status_message: Option<String>,
    pub attributes: HashMap<String, serde_json::Value>,
}

impl OtelTraceSpan {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        duration_ms: f64,
        status_ok: bool,
    ) -> Self {
        let now_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let dur_nano = (duration_ms * 1_000_000.0) as u64;
        let start_nano = now_nano.saturating_sub(dur_nano);

        let trace_id = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let span_id = format!("{:016x}", (uuid::Uuid::new_v4().as_u128() & 0xFFFFFFFFFFFFFFFF) as u64);

        Self {
            trace_id,
            span_id,
            parent_span_id: None,
            name: name.into(),
            kind: kind.into(),
            start_time_unix_nano: start_nano,
            end_time_unix_nano: now_nano,
            duration_ms,
            status_code: if status_ok { "STATUS_CODE_OK".to_string() } else { "STATUS_CODE_ERROR".to_string() },
            status_message: None,
            attributes: HashMap::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, val: serde_json::Value) -> Self {
        self.attributes.insert(key.into(), val);
        self
    }
}

/// In-Memory Thread-Safe Trace Store Ring Buffer
#[derive(Debug)]
pub struct OtelTraceStore {
    spans: Arc<RwLock<VecDeque<OtelTraceSpan>>>,
    max_capacity: usize,
}

impl Default for OtelTraceStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TRACE_CAPACITY)
    }
}

impl OtelTraceStore {
    pub fn new(max_capacity: usize) -> Self {
        Self {
            spans: Arc::new(RwLock::new(VecDeque::with_capacity(max_capacity.max(10)))),
            max_capacity: max_capacity.max(10),
        }
    }

    /// Record a completed trace span into the ring buffer
    pub async fn record_span(&self, span: OtelTraceSpan) {
        let mut queue = self.spans.write().await;
        if queue.len() >= self.max_capacity {
            queue.pop_front();
        }
        queue.push_back(span);
    }

    /// Convenience helper: record an MCP tool invocation trace span
    pub async fn record_mcp_tool_span(
        &self,
        tool_name: &str,
        status: &str,
        duration_ms: f64,
        fuel_consumed: u64,
        trace_id: Option<String>,
        parent_span_id: Option<String>,
    ) -> OtelTraceSpan {
        let is_ok = status.eq_ignore_ascii_case("success") || status.eq_ignore_ascii_case("ok");
        let mut span = OtelTraceSpan::new(
            format!("mcp.tool_call.{}", tool_name),
            "SPAN_KIND_CLIENT",
            duration_ms,
            is_ok,
        );

        if let Some(t_id) = trace_id {
            span.trace_id = t_id;
        }
        span.parent_span_id = parent_span_id;

        span.attributes.insert("mcp.tool_name".into(), serde_json::json!(tool_name));
        span.attributes.insert("mcp.status".into(), serde_json::json!(status));
        span.attributes.insert("mcp.duration_ms".into(), serde_json::json!(duration_ms));
        span.attributes.insert("mcp.fuel_consumed".into(), serde_json::json!(fuel_consumed));

        self.record_span(span.clone()).await;
        span
    }

    /// Convenience helper: record a sub-agent reinforcement loop step trace span
    pub async fn record_subagent_loop_span(
        &self,
        step_name: &str,
        agent_id: &str,
        status: &str,
        duration_ms: f64,
        tokens: usize,
        trace_id: Option<String>,
        parent_span_id: Option<String>,
    ) -> OtelTraceSpan {
        let is_ok = status.eq_ignore_ascii_case("success") || status.eq_ignore_ascii_case("ok");
        let mut span = OtelTraceSpan::new(
            format!("agent.loop_step.{}", step_name),
            "SPAN_KIND_INTERNAL",
            duration_ms,
            is_ok,
        );

        if let Some(t_id) = trace_id {
            span.trace_id = t_id;
        }
        span.parent_span_id = parent_span_id;

        span.attributes.insert("agent.id".into(), serde_json::json!(agent_id));
        span.attributes.insert("agent.step".into(), serde_json::json!(step_name));
        span.attributes.insert("agent.status".into(), serde_json::json!(status));
        span.attributes.insert("agent.tokens".into(), serde_json::json!(tokens));
        span.attributes.insert("agent.duration_ms".into(), serde_json::json!(duration_ms));

        self.record_span(span.clone()).await;
        span
    }

    /// Retrieve all recorded trace spans
    pub async fn get_all_spans(&self) -> Vec<OtelTraceSpan> {
        let queue = self.spans.read().await;
        queue.iter().cloned().collect()
    }

    /// Convert stored MCP spans into `McpToolInvocationRecord`s for live HUD widget
    pub async fn get_recent_mcp_invocations(&self, limit: usize) -> Vec<McpToolInvocationRecord> {
        let queue = self.spans.read().await;
        let mut records = Vec::new();

        for span in queue.iter().rev() {
            if let Some(name_val) = span.attributes.get("mcp.tool_name") {
                if let Some(tool_name) = name_val.as_str() {
                    let status = span.attributes.get("mcp.status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("success")
                        .to_string();
                    let fuel = span.attributes.get("mcp.fuel_consumed")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(120);
                    let ts_ms = span.end_time_unix_nano / 1_000_000;

                    records.push(McpToolInvocationRecord {
                        tool_name: tool_name.to_string(),
                        status,
                        duration_ms: span.duration_ms,
                        fuel_consumed: fuel,
                        timestamp_epoch_ms: ts_ms,
                    });

                    if records.len() >= limit {
                        break;
                    }
                }
            }
        }

        records
    }

    /// Export stored spans in standard OTLP JSON trace format
    pub async fn export_otlp_json(&self) -> serde_json::Value {
        let queue = self.spans.read().await;
        let spans_json: Vec<serde_json::Value> = queue.iter().map(|s| {
            let attributes_list: Vec<serde_json::Value> = s.attributes.iter().map(|(k, v)| {
                let val_obj = if let Some(s) = v.as_str() {
                    serde_json::json!({ "stringValue": s })
                } else if let Some(n) = v.as_i64() {
                    serde_json::json!({ "intValue": n.to_string() })
                } else if let Some(n) = v.as_u64() {
                    serde_json::json!({ "intValue": n.to_string() })
                } else if let Some(f) = v.as_f64() {
                    serde_json::json!({ "doubleValue": f })
                } else if let Some(b) = v.as_bool() {
                    serde_json::json!({ "boolValue": b })
                } else {
                    serde_json::json!({ "stringValue": v.to_string() })
                };
                serde_json::json!({
                    "key": k,
                    "value": val_obj
                })
            }).collect();

            serde_json::json!({
                "traceId": s.trace_id,
                "spanId": s.span_id,
                "parentSpanId": s.parent_span_id.clone().unwrap_or_default(),
                "name": s.name,
                "kind": s.kind,
                "startTimeUnixNano": s.start_time_unix_nano.to_string(),
                "endTimeUnixNano": s.end_time_unix_nano.to_string(),
                "attributes": attributes_list,
                "status": {
                    "code": s.status_code,
                    "message": s.status_message.clone().unwrap_or_default()
                }
            })
        }).collect();

        serde_json::json!({
            "resourceSpans": [
                {
                    "resource": {
                        "attributes": [
                            { "key": "service.name", "value": { "stringValue": "hagibis" } },
                            { "key": "service.version", "value": { "stringValue": "0.1.0" } },
                            { "key": "deployment.environment", "value": { "stringValue": "sovereign" } }
                        ]
                    },
                    "scopeSpans": [
                        {
                            "scope": {
                                "name": "hgb-cockpit-otel-tracer",
                                "version": "0.1.0"
                            },
                            "spans": spans_json
                        }
                    ]
                }
            ]
        })
    }

    /// Ingest trace spans from an incoming OTLP JSON payload
    pub async fn ingest_otlp_spans(&self, val: &serde_json::Value) -> Result<usize, String> {
        let mut count = 0;

        // Try standard resourceSpans array
        if let Some(res_spans) = val.get("resourceSpans").and_then(|v| v.as_array()) {
            for rs in res_spans {
                if let Some(scope_spans) = rs.get("scopeSpans").and_then(|v| v.as_array()) {
                    for ss in scope_spans {
                        if let Some(spans) = ss.get("spans").and_then(|v| v.as_array()) {
                            for s in spans {
                                if let Some(parsed) = Self::parse_span_from_json(s) {
                                    self.record_span(parsed).await;
                                    count += 1;
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some(spans) = val.as_array() {
            // Raw list of spans
            for s in spans {
                if let Some(parsed) = Self::parse_span_from_json(s) {
                    self.record_span(parsed).await;
                    count += 1;
                }
            }
        } else if let Some(parsed) = Self::parse_span_from_json(val) {
            // Single span object
            self.record_span(parsed).await;
            count += 1;
        }

        Ok(count)
    }

    fn parse_span_from_json(s: &serde_json::Value) -> Option<OtelTraceSpan> {
        let name = s.get("name").and_then(|v| v.as_str())?.to_string();
        let trace_id = s.get("traceId").and_then(|v| v.as_str())
            .unwrap_or_else(|| "00000000000000000000000000000000").to_string();
        let span_id = s.get("spanId").and_then(|v| v.as_str())
            .unwrap_or_else(|| "0000000000000000").to_string();
        let parent_span_id = s.get("parentSpanId")
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string());
        let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("SPAN_KIND_INTERNAL").to_string();

        let start_time_unix_nano = s.get("startTimeUnixNano")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64()))
            .unwrap_or_default();
        let end_time_unix_nano = s.get("endTimeUnixNano")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64()))
            .unwrap_or_default();

        let duration_ms = if end_time_unix_nano > start_time_unix_nano {
            (end_time_unix_nano - start_time_unix_nano) as f64 / 1_000_000.0
        } else {
            1.0
        };

        let status_code = s.get("status")
            .and_then(|v| v.get("code"))
            .and_then(|c| c.as_str())
            .unwrap_or("STATUS_CODE_OK")
            .to_string();

        let mut attributes = HashMap::new();
        if let Some(attrs) = s.get("attributes").and_then(|v| v.as_array()) {
            for attr in attrs {
                if let (Some(k), Some(v)) = (attr.get("key").and_then(|v| v.as_str()), attr.get("value")) {
                    let extracted = if let Some(s) = v.get("stringValue").and_then(|x| x.as_str()) {
                        serde_json::json!(s)
                    } else if let Some(n) = v.get("intValue").and_then(|x| x.as_str().and_then(|val| val.parse::<i64>().ok())) {
                        serde_json::json!(n)
                    } else if let Some(d) = v.get("doubleValue").and_then(|x| x.as_f64()) {
                        serde_json::json!(d)
                    } else if let Some(b) = v.get("boolValue").and_then(|x| x.as_bool()) {
                        serde_json::json!(b)
                    } else {
                        v.clone()
                    };
                    attributes.insert(k.to_string(), extracted);
                }
            }
        }

        Some(OtelTraceSpan {
            trace_id,
            span_id,
            parent_span_id,
            name,
            kind,
            start_time_unix_nano,
            end_time_unix_nano,
            duration_ms,
            status_code,
            status_message: None,
            attributes,
        })
    }
}

/// OpenTelemetry Metrics Exporter (OTLP JSON & Prometheus text formats)
pub struct OtelMetricsExporter;

impl OtelMetricsExporter {
    /// Format telemetry into standard Prometheus text exposition format
    pub fn export_prometheus(telem: &CockpitWebTelemetry) -> String {
        let mut out = String::with_capacity(2048);

        // 1. Token metrics
        out.push_str("# HELP hgb_tokens_consumed_total Total tokens consumed by LLM agents\n");
        out.push_str("# TYPE hgb_tokens_consumed_total counter\n");
        out.push_str(&format!("hgb_tokens_consumed_total{{type=\"prompt\"}} {}\n", telem.prompt_tokens));
        out.push_str(&format!("hgb_tokens_consumed_total{{type=\"completion\"}} {}\n", telem.completion_tokens));
        out.push_str(&format!("hgb_tokens_consumed_total{{type=\"total\"}} {}\n", telem.total_tokens));

        out.push_str("# HELP hgb_token_burn_rate_tokens_per_sec Instantaneous token consumption rate\n");
        out.push_str("# TYPE hgb_token_burn_rate_tokens_per_sec gauge\n");
        out.push_str(&format!("hgb_token_burn_rate_tokens_per_sec {:.2}\n", telem.token_burn_rate_tokens_per_sec));

        // 2. MicroVM metrics
        out.push_str("# HELP hgb_active_microvms Active Firecracker microVM instances\n");
        out.push_str("# TYPE hgb_active_microvms gauge\n");
        out.push_str(&format!("hgb_active_microvms {}\n", telem.active_firecracker_vms));

        out.push_str("# HELP hgb_microvm_snapshot_cached Snapshot prewarm status (1=cached, 0=cold)\n");
        out.push_str("# TYPE hgb_microvm_snapshot_cached gauge\n");
        out.push_str(&format!("hgb_microvm_snapshot_cached {}\n", if telem.firecracker_snapshot_cached { 1 } else { 0 }));

        // 3. WASM metrics
        out.push_str("# HELP hgb_wasm_memory_rss_bytes WASM runtime resident set size in bytes\n");
        out.push_str("# TYPE hgb_wasm_memory_rss_bytes gauge\n");
        out.push_str(&format!("hgb_wasm_memory_rss_bytes {}\n", (telem.wasm_memory_rss_mb * 1024.0 * 1024.0) as u64));

        out.push_str("# HELP hgb_wasm_linear_heap_pages WASM engine active linear memory heap pages\n");
        out.push_str("# TYPE hgb_wasm_linear_heap_pages gauge\n");
        out.push_str(&format!("hgb_wasm_linear_heap_pages {}\n", telem.wasm_linear_heap_pages));

        out.push_str("# HELP hgb_wasm_hot_pool_modules Precompiled hot-pool WASM modules\n");
        out.push_str("# TYPE hgb_wasm_hot_pool_modules gauge\n");
        out.push_str(&format!("hgb_wasm_hot_pool_modules {}\n", telem.wasm_hot_pool_modules));

        // 4. Hardware & Thermal metrics
        out.push_str("# HELP hgb_cpu_temperature_celsius CPU package temperature in degrees Celsius\n");
        out.push_str("# TYPE hgb_cpu_temperature_celsius gauge\n");
        out.push_str(&format!("hgb_cpu_temperature_celsius {:.1}\n", telem.cpu_temperature_c));

        out.push_str("# HELP hgb_cpu_thermal_throttled Thermal throttling indicator (1=throttled, 0=optimal)\n");
        out.push_str("# TYPE hgb_cpu_thermal_throttled gauge\n");
        out.push_str(&format!("hgb_cpu_thermal_throttled {}\n", if telem.thermal_throttled { 1 } else { 0 }));

        out.push_str("# HELP hgb_battery_discharge_milliwatts Instantaneous battery draw in milliwatts\n");
        out.push_str("# TYPE hgb_battery_discharge_milliwatts gauge\n");
        out.push_str(&format!("hgb_battery_discharge_milliwatts {:.0}\n", telem.battery_discharge_mw));

        out.push_str("# HELP hgb_mglru_min_ttl_ms Linux Multi-Gen LRU min_ttl_ms active tuning\n");
        out.push_str("# TYPE hgb_mglru_min_ttl_ms gauge\n");
        out.push_str(&format!("hgb_mglru_min_ttl_ms {}\n", telem.mglru_min_ttl_ms));

        out.push_str("# HELP hgb_cpu_governor CPU governor state active\n");
        out.push_str("# TYPE hgb_cpu_governor gauge\n");
        out.push_str(&format!("hgb_cpu_governor{{governor=\"{}\",epp=\"{}\"}} 1\n", telem.cpu_governor, telem.cpu_epp));

        for (idx, freq) in telem.cpu_frequencies_mhz.iter().enumerate() {
            out.push_str(&format!("hgb_cpu_frequency_mhz{{core=\"{}\"}} {}\n", idx, freq));
        }

        // 5. Memory vector metrics
        out.push_str("# HELP hgb_hnsw_search_latency_ms Vector memory HNSW search latency in ms\n");
        out.push_str("# TYPE hgb_hnsw_search_latency_ms gauge\n");
        out.push_str(&format!("hgb_hnsw_search_latency_ms {:.2}\n", telem.hnsw_search_latency_ms));

        out.push_str("# HELP hgb_vector_index_size Total vectors stored in persistent memory index\n");
        out.push_str("# TYPE hgb_vector_index_size gauge\n");
        out.push_str(&format!("hgb_vector_index_size {}\n", telem.vector_index_size));

        // 6. MCP Requests
        out.push_str("# HELP hgb_mcp_requests_total Total MCP tool invocations handled\n");
        out.push_str("# TYPE hgb_mcp_requests_total counter\n");
        out.push_str(&format!("hgb_mcp_requests_total {}\n", telem.mcp_requests_total));

        out
    }

    /// Format telemetry into standard OpenTelemetry OTLP JSON metrics format
    pub fn export_otlp_json(telem: &CockpitWebTelemetry) -> serde_json::Value {
        let now_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let time_str = now_nano.to_string();

        let metrics = vec![
            serde_json::json!({
                "name": "tokens_consumed_total",
                "description": "Total tokens consumed by agents",
                "unit": "1",
                "sum": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asInt": telem.prompt_tokens.to_string(),
                            "attributes": [{ "key": "type", "value": { "stringValue": "prompt" } }]
                        },
                        {
                            "timeUnixNano": time_str,
                            "asInt": telem.completion_tokens.to_string(),
                            "attributes": [{ "key": "type", "value": { "stringValue": "completion" } }]
                        },
                        {
                            "timeUnixNano": time_str,
                            "asInt": telem.total_tokens.to_string(),
                            "attributes": [{ "key": "type", "value": { "stringValue": "total" } }]
                        }
                    ],
                    "aggregationTemporality": "AGGREGATION_TEMPORALITY_CUMULATIVE",
                    "isMonotonic": true
                }
            }),
            serde_json::json!({
                "name": "token_burn_velocity",
                "description": "Instantaneous token consumption velocity in tokens per second",
                "unit": "tokens/s",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asDouble": telem.token_burn_rate_tokens_per_sec
                        }
                    ]
                }
            }),
            serde_json::json!({
                "name": "active_firecracker_microvms",
                "description": "Number of active Firecracker MicroVM jail instances",
                "unit": "1",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asInt": telem.active_firecracker_vms.to_string()
                        }
                    ]
                }
            }),
            serde_json::json!({
                "name": "wasm_memory_rss_bytes",
                "description": "Resident set size of WASM execution engine in bytes",
                "unit": "By",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asInt": ((telem.wasm_memory_rss_mb * 1024.0 * 1024.0) as u64).to_string()
                        }
                    ]
                }
            }),
            serde_json::json!({
                "name": "cpu_temperature_celsius",
                "description": "Host CPU package temperature in degrees Celsius",
                "unit": "Cel",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asDouble": telem.cpu_temperature_c as f64
                        }
                    ]
                }
            }),
            serde_json::json!({
                "name": "battery_discharge_milliwatts",
                "description": "Instantaneous battery draw in milliwatts",
                "unit": "mW",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asDouble": telem.battery_discharge_mw as f64
                        }
                    ]
                }
            }),
            serde_json::json!({
                "name": "hnsw_search_latency_ms",
                "description": "Vector memory HNSW search latency in milliseconds",
                "unit": "ms",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asDouble": telem.hnsw_search_latency_ms
                        }
                    ]
                }
            }),
            serde_json::json!({
                "name": "cpu_governor_state",
                "description": "Active CPU governor and energy performance preference",
                "unit": "1",
                "gauge": {
                    "dataPoints": [
                        {
                            "timeUnixNano": time_str,
                            "asInt": "1",
                            "attributes": [
                                { "key": "governor", "value": { "stringValue": telem.cpu_governor.clone() } },
                                { "key": "epp", "value": { "stringValue": telem.cpu_epp.clone() } },
                                { "key": "thermal_throttled", "value": { "boolValue": telem.thermal_throttled } }
                            ]
                        }
                    ]
                }
            }),
        ];

        serde_json::json!({
            "resourceMetrics": [
                {
                    "resource": {
                        "attributes": [
                            { "key": "service.name", "value": { "stringValue": "hagibis" } },
                            { "key": "service.version", "value": { "stringValue": "0.1.0" } }
                        ]
                    },
                    "scopeMetrics": [
                        {
                            "scope": {
                                "name": "hgb-cockpit-otel-metrics",
                                "version": "0.1.0"
                            },
                            "metrics": metrics
                        }
                    ]
                }
            ]
        })
    }
}
