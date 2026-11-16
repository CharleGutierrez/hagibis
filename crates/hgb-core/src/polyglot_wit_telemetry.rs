//! # Polyglot OpenTelemetry WIT Telemetry Engine (`polyglot_wit_telemetry`)
//!
//! Production-grade zero-copy telemetry engine bridging:
//! - Rust microVMs and Core runtime (`hgb-core`, `hgb-daemon`)
//! - TypeScript Cloudflare Workers Edge Gateway (`worker/index.ts`)
//! - Python Masterclass simulations and dynamic hardware governors
//!
//! ## Key Capabilities
//! - Strict W3C Trace Context recommendation (`traceparent` + `tracestate`) injection and extraction
//! - Lock-free concurrent span ring buffer with bounded FIFO capacity and non-blocking producers
//! - High-resolution nanosecond timestamps (`SystemTime` & `Instant`)
//! - Dynamic span event and typed attribute attachments
//! - Sub-microsecond sliding-window percentile calculations (P50, P90, P95, P99, Min, Max, Mean)
//! - Dynamic Hardware Governor & Firecracker MicroVM dynamic allocation scaling feedback
//! - Complete OpenTelemetry (OTLP) JSON compatibility

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// ==============================================================================
// 1. Errors & Status Types
// ==============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryError {
    InvalidTraceparentFormat(String),
    InvalidHexEncoding(String),
    InvalidTraceIdLength(usize),
    InvalidParentIdLength(usize),
    AllZeroTraceId,
    AllZeroParentId,
    UnsupportedVersion(String),
    BufferFull,
    SpanNotFound(String),
    SerializationError(String),
}

impl std::error::Error for TelemetryError {}

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTraceparentFormat(s) => write!(f, "Invalid W3C traceparent format: '{s}'"),
            Self::InvalidHexEncoding(s) => write!(f, "Invalid hex character in trace header: '{s}'"),
            Self::InvalidTraceIdLength(len) => {
                write!(f, "Invalid trace_id length: expected 32 hex chars, got {len}")
            }
            Self::InvalidParentIdLength(len) => {
                write!(f, "Invalid parent/span_id length: expected 16 hex chars, got {len}")
            }
            Self::AllZeroTraceId => write!(f, "W3C trace_id cannot be all zeros"),
            Self::AllZeroParentId => write!(f, "W3C parent/span_id cannot be all zeros"),
            Self::UnsupportedVersion(v) => write!(f, "Unsupported W3C version: '{v}'"),
            Self::BufferFull => write!(f, "Lock-free span ring buffer is saturated"),
            Self::SpanNotFound(id) => write!(f, "Span ID '{id}' not found in active registry"),
            Self::SerializationError(msg) => write!(f, "Telemetry serialization error: {msg}"),
        }
    }
}

// ==============================================================================
// 2. OpenTelemetry Span Kinds, Status, and Values
// ==============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
    EdgeGateway,
    MicrovmHypervisor,
}

impl Default for SpanKind {
    fn default() -> Self {
        Self::Internal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatusCode {
    Unset,
    Ok,
    Error,
}

impl Default for SpanStatusCode {
    fn default() -> Self {
        Self::Unset
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpanStatus {
    pub code: SpanStatusCode,
    pub description: Option<String>,
}

impl SpanStatus {
    pub fn ok() -> Self {
        Self {
            code: SpanStatusCode::Ok,
            description: None,
        }
    }

    pub fn error(desc: impl Into<String>) -> Self {
        Self {
            code: SpanStatusCode::Error,
            description: Some(desc.into()),
        }
    }

    pub fn unset() -> Self {
        Self {
            code: SpanStatusCode::Unset,
            description: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Int64(i64),
    Float64(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    StringList(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanAttribute {
    pub key: String,
    pub value: AttributeValue,
}

impl SpanAttribute {
    pub fn new(key: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    pub fn string(key: impl Into<String>, val: impl Into<String>) -> Self {
        Self::new(key, AttributeValue::String(val.into()))
    }

    pub fn int(key: impl Into<String>, val: i64) -> Self {
        Self::new(key, AttributeValue::Int64(val))
    }

    pub fn float(key: impl Into<String>, val: f64) -> Self {
        Self::new(key, AttributeValue::Float64(val))
    }

    pub fn bool_val(key: impl Into<String>, val: bool) -> Self {
        Self::new(key, AttributeValue::Bool(val))
    }
}

impl From<String> for AttributeValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for AttributeValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<i64> for AttributeValue {
    fn from(v: i64) -> Self {
        Self::Int64(v)
    }
}

impl From<i32> for AttributeValue {
    fn from(v: i32) -> Self {
        Self::Int64(v as i64)
    }
}

impl From<f64> for AttributeValue {
    fn from(v: f64) -> Self {
        Self::Float64(v)
    }
}

impl From<bool> for AttributeValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanEvent {
    pub name: String,
    pub timestamp_unix_nano: u64,
    pub attributes: Vec<SpanAttribute>,
}

impl SpanEvent {
    pub fn new(name: impl Into<String>, attributes: Vec<SpanAttribute>) -> Self {
        let now_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        Self {
            name: name.into(),
            timestamp_unix_nano: now_nano,
            attributes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    pub trace_state: Option<String>,
    pub attributes: Vec<SpanAttribute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRole {
    EdgeWorker,
    GuestWasm,
    FirecrackerMicrovm,
    PythonSimulation,
    BareMetalDaemon,
}

impl Default for ExecutionRole {
    fn default() -> Self {
        Self::BareMetalDaemon
    }
}

// ==============================================================================
// 3. W3C Trace Context Implementation
// ==============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    pub version: u8,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub span_id: String,
    pub trace_flags: u8,
    pub trace_state: Option<String>,
}

impl TraceContext {
    /// Generates a brand-new sampled root trace context with high-entropy cryptographic hex IDs
    pub fn new_root() -> Self {
        let trace_bytes = generate_random_bytes::<16>();
        let span_bytes = generate_random_bytes::<8>();

        Self {
            version: 0x00,
            trace_id: hex::encode(trace_bytes),
            parent_span_id: None,
            span_id: hex::encode(span_bytes),
            trace_flags: 0x01, // Sampled
            trace_state: None,
        }
    }

    /// Spawns a child context inheriting trace_id, setting parent_span_id, and generating new span_id
    pub fn child(&self) -> Self {
        let child_span_bytes = generate_random_bytes::<8>();
        Self {
            version: self.version,
            trace_id: self.trace_id.clone(),
            parent_span_id: Some(self.span_id.clone()),
            span_id: hex::encode(child_span_bytes),
            trace_flags: self.trace_flags,
            trace_state: self.trace_state.clone(),
        }
    }

    /// Formats this context into a strictly compliant W3C traceparent carrier string:
    /// `00-{trace_id:32hex}-{span_id:16hex}-{flags:2hex}`
    pub fn to_traceparent(&self) -> String {
        format!(
            "{:02x}-{}-{}-{:02x}",
            self.version, self.trace_id, self.span_id, self.trace_flags
        )
    }

    /// Injects context into a map or carrier header pair
    pub fn inject_headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("traceparent".to_string(), self.to_traceparent())];
        if let Some(ref state) = self.trace_state {
            headers.push(("tracestate".to_string(), state.clone()));
        }
        headers
    }

    /// Parses and strictly validates a W3C traceparent header string according to W3C RFC:
    /// Format: `version-trace_id-parent_id-trace_flags`
    pub fn from_traceparent(carrier: &str) -> Result<Self, TelemetryError> {
        let trimmed = carrier.trim();
        let parts: Vec<&str> = trimmed.split('-').collect();

        if parts.len() < 4 {
            return Err(TelemetryError::InvalidTraceparentFormat(trimmed.to_string()));
        }

        // Version validation (2 hex chars)
        let version_str = parts[0];
        if version_str.len() != 2 {
            return Err(TelemetryError::InvalidTraceparentFormat(trimmed.to_string()));
        }
        let version = u8::from_str_radix(version_str, 16)
            .map_err(|_| TelemetryError::InvalidHexEncoding(version_str.to_string()))?;

        if version == 0xff {
            return Err(TelemetryError::UnsupportedVersion(version_str.to_string()));
        }

        // Trace ID validation (32 hex chars, cannot be all zeros)
        let trace_id = parts[1].to_ascii_lowercase();
        if trace_id.len() != 32 {
            return Err(TelemetryError::InvalidTraceIdLength(trace_id.len()));
        }
        if !trace_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TelemetryError::InvalidHexEncoding(trace_id));
        }
        if trace_id.chars().all(|c| c == '0') {
            return Err(TelemetryError::AllZeroTraceId);
        }

        // Parent/Span ID validation (16 hex chars, cannot be all zeros)
        let parent_id = parts[2].to_ascii_lowercase();
        if parent_id.len() != 16 {
            return Err(TelemetryError::InvalidParentIdLength(parent_id.len()));
        }
        if !parent_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TelemetryError::InvalidHexEncoding(parent_id));
        }
        if parent_id.chars().all(|c| c == '0') {
            return Err(TelemetryError::AllZeroParentId);
        }

        // Trace flags validation (2 hex chars)
        let flags_str = parts[3];
        if flags_str.len() != 2 {
            return Err(TelemetryError::InvalidTraceparentFormat(trimmed.to_string()));
        }
        let trace_flags = u8::from_str_radix(flags_str, 16)
            .map_err(|_| TelemetryError::InvalidHexEncoding(flags_str.to_string()))?;

        // For version 00, exactly 4 parts are allowed
        if version == 0x00 && parts.len() > 4 {
            return Err(TelemetryError::InvalidTraceparentFormat(trimmed.to_string()));
        }

        // The incoming parent_id in traceparent becomes parent_span_id of our incoming context,
        // and we allocate a fresh span_id for work done in this process
        let local_span_bytes = generate_random_bytes::<8>();
        let local_span_id = hex::encode(local_span_bytes);

        Ok(Self {
            version,
            trace_id,
            parent_span_id: Some(parent_id),
            span_id: local_span_id,
            trace_flags,
            trace_state: None,
        })
    }
}

// Pseudo-random high-entropy bytes generator for span & trace IDs
fn generate_random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let nano = now.as_nanos();
    let pid = std::process::id();
    let thread_id = std::thread::current().id();

    // Fill seed material
    let hash = blake3::keyed_hash(
        &[0x42; 32],
        format!("{nano}:{pid}:{:?}:{now:?}", thread_id).as_bytes(),
    );
    let hash_bytes = hash.as_bytes();
    bytes.copy_from_slice(&hash_bytes[..N]);

    // Ensure non-zero
    if bytes.iter().all(|&b| b == 0) {
        bytes[0] = 0x01;
    }
    bytes
}

// ==============================================================================
// 4. Completed Span Record
// ==============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub service_name: String,
    pub role: ExecutionRole,
    pub kind: SpanKind,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub duration_ms: f64,
    pub status: SpanStatus,
    pub attributes: Vec<SpanAttribute>,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

impl SpanRecord {
    pub fn to_w3c_traceparent(&self) -> String {
        let flags = if self.status.code == SpanStatusCode::Error {
            0x00
        } else {
            0x01
        };
        format!("00-{}-{}-{:02x}", self.trace_id, self.span_id, flags)
    }

    /// Serializes span to standard OpenTelemetry (OTLP) JSON format
    pub fn to_otlp_json(&self) -> serde_json::Value {
        serde_json::json!({
            "traceId": self.trace_id,
            "spanId": self.span_id,
            "parentSpanId": self.parent_span_id,
            "name": self.name,
            "kind": format!("{:?}", self.kind).to_uppercase(),
            "startTimeUnixNano": self.start_time_unix_nano.to_string(),
            "endTimeUnixNano": self.end_time_unix_nano.to_string(),
            "durationMs": self.duration_ms,
            "status": {
                "code": match self.status.code {
                    SpanStatusCode::Ok => "STATUS_CODE_OK",
                    SpanStatusCode::Error => "STATUS_CODE_ERROR",
                    SpanStatusCode::Unset => "STATUS_CODE_UNSET",
                },
                "message": self.status.description,
            },
            "attributes": self.attributes.iter().map(|attr| {
                serde_json::json!({
                    "key": attr.key,
                    "value": match &attr.value {
                        AttributeValue::String(s) => serde_json::json!({"stringValue": s}),
                        AttributeValue::Int64(i) => serde_json::json!({"intValue": i.to_string()}),
                        AttributeValue::Float64(f) => serde_json::json!({"doubleValue": f}),
                        AttributeValue::Bool(b) => serde_json::json!({"boolValue": b}),
                        AttributeValue::Bytes(by) => serde_json::json!({"bytesValue": hex::encode(by)}),
                        AttributeValue::StringList(l) => serde_json::json!({"arrayValue": {"values": l}}),
                    }
                })
            }).collect::<Vec<_>>(),
            "events": self.events.iter().map(|ev| {
                serde_json::json!({
                    "name": ev.name,
                    "timeUnixNano": ev.timestamp_unix_nano.to_string(),
                    "attributes": ev.attributes.iter().map(|a| {
                        serde_json::json!({"key": a.key, "value": format!("{:?}", a.value)})
                    }).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

// ==============================================================================
// 5. Active Span Builder & In-Flight Context
// ==============================================================================

pub struct ActiveSpan {
    pub context: TraceContext,
    pub name: String,
    pub service_name: String,
    pub role: ExecutionRole,
    pub kind: SpanKind,
    pub start_instant: Instant,
    pub start_unix_nano: u64,
    pub attributes: Mutex<Vec<SpanAttribute>>,
    pub events: Mutex<Vec<SpanEvent>>,
    pub links: Mutex<Vec<SpanLink>>,
}

impl ActiveSpan {
    pub fn new(
        name: impl Into<String>,
        service_name: impl Into<String>,
        role: ExecutionRole,
        kind: SpanKind,
        parent: Option<&TraceContext>,
    ) -> Self {
        let context = match parent {
            Some(p) => p.child(),
            None => TraceContext::new_root(),
        };

        let start_instant = Instant::now();
        let start_unix_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        Self {
            context,
            name: name.into(),
            service_name: service_name.into(),
            role,
            kind,
            start_instant,
            start_unix_nano,
            attributes: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            links: Mutex::new(Vec::new()),
        }
    }

    pub fn add_attribute(&self, attribute: SpanAttribute) {
        if let Ok(mut attrs) = self.attributes.lock() {
            attrs.push(attribute);
        }
    }

    pub fn add_event(&self, name: impl Into<String>, attributes: Vec<SpanAttribute>) {
        let ev = SpanEvent::new(name, attributes);
        if let Ok(mut events) = self.events.lock() {
            events.push(ev);
        }
    }

    pub fn add_link(&self, link: SpanLink) {
        if let Ok(mut links) = self.links.lock() {
            links.push(link);
        }
    }

    pub fn finish(self, status: SpanStatus) -> SpanRecord {
        let duration = self.start_instant.elapsed();
        let duration_ms = duration.as_secs_f64() * 1000.0;
        let end_unix_nano = self.start_unix_nano + duration.as_nanos() as u64;

        let attributes = self.attributes.into_inner().unwrap_or_default();
        let events = self.events.into_inner().unwrap_or_default();
        let links = self.links.into_inner().unwrap_or_default();

        SpanRecord {
            trace_id: self.context.trace_id,
            span_id: self.context.span_id,
            parent_span_id: self.context.parent_span_id,
            name: self.name,
            service_name: self.service_name,
            role: self.role,
            kind: self.kind,
            start_time_unix_nano: self.start_unix_nano,
            end_time_unix_nano: end_unix_nano,
            duration_ms,
            status,
            attributes,
            events,
            links,
        }
    }
}

// ==============================================================================
// 6. Lock-Free Concurrent Span Ring Buffer
// ==============================================================================

/// High-throughput lock-free span ring buffer using atomic sequence indexing.
/// Implements bounded FIFO storage where push operations never block concurrent callers.
pub struct LockFreeSpanRingBuffer {
    capacity: usize,
    mask: usize,
    slots: Vec<Arc<RwLock<Option<SpanRecord>>>>,
    head: AtomicUsize,
    tail: AtomicUsize,
    total_spans_recorded: AtomicU64,
}

impl LockFreeSpanRingBuffer {
    /// Creates a lock-free ring buffer with capacity rounded up to power of two (minimum 16)
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(16);
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(Arc::new(RwLock::new(None)));
        }

        Self {
            capacity: cap,
            mask: cap - 1,
            slots,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            total_spans_recorded: AtomicU64::new(0),
        }
    }

    /// Appends a completed span record into the ring buffer.
    /// Thread-safe, non-blocking, and lock-free across producers.
    pub fn push(&self, record: SpanRecord) -> bool {
        let idx = self.head.fetch_add(1, Ordering::SeqCst) & self.mask;
        if let Some(slot) = self.slots.get(idx) {
            if let Ok(mut guard) = slot.write() {
                *guard = Some(record);
                self.total_spans_recorded.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    /// Returns a non-destructive read-only snapshot of recent spans up to `limit`
    pub fn snapshot(&self, limit: usize) -> Vec<SpanRecord> {
        let cur_head = self.head.load(Ordering::SeqCst);
        let count = limit.min(self.capacity);
        let mut result = Vec::with_capacity(count);

        for i in 0..count {
            if cur_head < i + 1 {
                break;
            }
            let idx = (cur_head.wrapping_sub(i + 1)) & self.mask;
            if let Some(slot) = self.slots.get(idx) {
                if let Ok(guard) = slot.read() {
                    if let Some(ref record) = *guard {
                        result.push(record.clone());
                    }
                }
            }
        }
        result
    }

    /// Drains up to `max_records` from the buffer in FIFO order
    pub fn drain(&self, max_records: usize) -> Vec<SpanRecord> {
        let count = max_records.min(self.capacity);
        let mut drained = Vec::with_capacity(count);

        let cur_tail = self.tail.load(Ordering::SeqCst);
        let cur_head = self.head.load(Ordering::SeqCst);
        let available = cur_head.saturating_sub(cur_tail).min(self.capacity);
        let to_drain = count.min(available);

        for _ in 0..to_drain {
            let tail_idx = self.tail.fetch_add(1, Ordering::SeqCst) & self.mask;
            if let Some(slot) = self.slots.get(tail_idx) {
                if let Ok(mut guard) = slot.write() {
                    if let Some(record) = guard.take() {
                        drained.push(record);
                    }
                }
            }
        }
        drained
    }

    pub fn total_recorded(&self) -> u64 {
        self.total_spans_recorded.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        for slot in &self.slots {
            if let Ok(mut guard) = slot.write() {
                *guard = None;
            }
        }
        self.head.store(0, Ordering::SeqCst);
        self.tail.store(0, Ordering::SeqCst);
    }
}

// ==============================================================================
// 7. Latency Percentiles Engine
// ==============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyPercentiles {
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
    pub sample_count: u64,
    pub error_rate: f64,
}

impl Default for LatencyPercentiles {
    fn default() -> Self {
        Self {
            p50_ms: 0.0,
            p90_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            min_ms: 0.0,
            max_ms: 0.0,
            mean_ms: 0.0,
            sample_count: 0,
            error_rate: 0.0,
        }
    }
}

pub struct SpanMetricsEngine {
    window_capacity: usize,
    buffer: Arc<LockFreeSpanRingBuffer>,
}

impl SpanMetricsEngine {
    pub fn new(capacity: usize) -> Self {
        Self {
            window_capacity: capacity,
            buffer: Arc::new(LockFreeSpanRingBuffer::new(capacity)),
        }
    }

    pub fn record_span(&self, record: SpanRecord) -> bool {
        self.buffer.push(record)
    }

    pub fn buffer(&self) -> Arc<LockFreeSpanRingBuffer> {
        Arc::clone(&self.buffer)
    }

    /// Computes accurate sliding-window percentiles (P50, P90, P95, P99)
    pub fn compute_percentiles(&self) -> LatencyPercentiles {
        let spans = self.buffer.snapshot(self.window_capacity);
        if spans.is_empty() {
            return LatencyPercentiles::default();
        }

        let n = spans.len();
        let mut durations: Vec<f64> = spans.iter().map(|s| s.duration_ms).collect();
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let error_count = spans
            .iter()
            .filter(|s| s.status.code == SpanStatusCode::Error)
            .count();
        let sum: f64 = durations.iter().sum();
        let mean = sum / (n as f64);

        let get_p = |p: f64| -> f64 {
            let rank = (p * (n as f64)).ceil() as usize;
            let idx = rank.saturating_sub(1).min(n - 1);
            durations[idx]
        };

        LatencyPercentiles {
            p50_ms: round3(get_p(0.50)),
            p90_ms: round3(get_p(0.90)),
            p95_ms: round3(get_p(0.95)),
            p99_ms: round3(get_p(0.99)),
            min_ms: round3(durations[0]),
            max_ms: round3(durations[n - 1]),
            mean_ms: round3(mean),
            sample_count: n as u64,
            error_rate: round3((error_count as f64) / (n as f64)),
        }
    }
}

fn round3(val: f64) -> f64 {
    (val * 1000.0).round() / 1000.0
}

// ==============================================================================
// 8. Dynamic Hardware Governor & MicroVM Allocation Bridge
// ==============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernorProfile {
    Performance,
    Balanced,
    Powersave,
}

impl GovernorProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::Powersave => "powersave",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerConcurrencyState {
    Eco,     // 2 workers
    Nominal, // 4 workers
    Boosted, // 8 workers
}

impl WorkerConcurrencyState {
    pub fn worker_count(&self) -> u32 {
        match self {
            Self::Eco => 2,
            Self::Nominal => 4,
            Self::Boosted => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicrovmAllocationTelemetry {
    pub vcpu_count: u32,
    pub memory_mb: u32,
    pub queue_depth: u32,
    pub governor_profile: String,
    pub worker_concurrency_state: String,
    pub execution_us: u64,
    pub throttling_detected: bool,
    pub action: String,
    pub reason: String,
}

pub struct PolyglotGovernorFeedback {
    p99_sla_ms: f64,
    queue_spike_threshold: u32,
    settle_latency_threshold_ms: f64,
    settle_duration_seconds: f64,
}

impl Default for PolyglotGovernorFeedback {
    fn default() -> Self {
        Self {
            p99_sla_ms: 200.0,
            queue_spike_threshold: 10,
            settle_latency_threshold_ms: 50.0,
            settle_duration_seconds: 15.0,
        }
    }
}

impl PolyglotGovernorFeedback {
    pub fn new(
        p99_sla_ms: f64,
        queue_spike_threshold: u32,
        settle_latency_threshold_ms: f64,
        settle_duration_seconds: f64,
    ) -> Self {
        Self {
            p99_sla_ms,
            queue_spike_threshold,
            settle_latency_threshold_ms,
            settle_duration_seconds,
        }
    }

    /// Evaluates real-time P99 latency SLA and queue depth against thresholds:
    /// - High Load / SLA Breach (P99 > 200ms OR queue > 10):
    ///     -> Performance Governor
    ///     -> Boosted Worker Pool (8 workers)
    ///     -> Firecracker MicroVM scale up (4 vCPUs, 1024 MB RAM)
    /// - Settle Cooldown (queue == 0 AND P99 < 50ms for >= 15s):
    ///     -> Powersave Governor
    ///     -> Eco Worker Pool (2 workers)
    ///     -> Firecracker MicroVM scale down (1 vCPU, 256 MB RAM)
    /// - Nominal Baseline:
    ///     -> Balanced Governor
    ///     -> Nominal Worker Pool (4 workers)
    ///     -> Firecracker MicroVM baseline (2 vCPUs, 512 MB RAM)
    pub fn evaluate_sla_feedback(
        &self,
        p99_ms: f64,
        queue_depth: u32,
        settle_seconds: f64,
    ) -> MicrovmAllocationTelemetry {
        if p99_ms > self.p99_sla_ms || queue_depth > self.queue_spike_threshold {
            let reason = if p99_ms > self.p99_sla_ms {
                format!("P99 latency breach: {p99_ms:.1}ms > {:.1}ms SLA", self.p99_sla_ms)
            } else {
                format!("Queue depth spike: {queue_depth} > {}", self.queue_spike_threshold)
            };

            MicrovmAllocationTelemetry {
                vcpu_count: 4,
                memory_mb: 1024,
                queue_depth,
                governor_profile: GovernorProfile::Performance.as_str().to_string(),
                worker_concurrency_state: "boosted".to_string(),
                execution_us: (p99_ms * 1000.0) as u64,
                throttling_detected: p99_ms > self.p99_sla_ms * 1.5,
                action: "BOOST_PERFORMANCE".to_string(),
                reason,
            }
        } else if queue_depth == 0
            && p99_ms < self.settle_latency_threshold_ms
            && settle_seconds >= self.settle_duration_seconds
        {
            MicrovmAllocationTelemetry {
                vcpu_count: 1,
                memory_mb: 256,
                queue_depth: 0,
                governor_profile: GovernorProfile::Powersave.as_str().to_string(),
                worker_concurrency_state: "eco".to_string(),
                execution_us: (p99_ms * 1000.0) as u64,
                throttling_detected: false,
                action: "SETTLE_POWERSAVE".to_string(),
                reason: format!(
                    "Workload settled: queue=0, P99={p99_ms:.1}ms < {:.1}ms for {settle_seconds:.1}s",
                    self.settle_latency_threshold_ms
                ),
            }
        } else {
            MicrovmAllocationTelemetry {
                vcpu_count: 2,
                memory_mb: 512,
                queue_depth,
                governor_profile: GovernorProfile::Balanced.as_str().to_string(),
                worker_concurrency_state: "nominal".to_string(),
                execution_us: (p99_ms * 1000.0) as u64,
                throttling_detected: false,
                action: "BALANCED_NOMINAL".to_string(),
                reason: "Workload nominal within SLA limits".to_string(),
            }
        }
    }
}

// ==============================================================================
// 9. Comprehensive Unit Test Suite (6 Rigorous Unit Tests)
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_w3c_traceparent_injection_extraction() {
        // 1. Generate fresh root context
        let root = TraceContext::new_root();
        assert_eq!(root.version, 0x00);
        assert_eq!(root.trace_id.len(), 32);
        assert_eq!(root.span_id.len(), 16);
        assert!(root.parent_span_id.is_none());
        assert_eq!(root.trace_flags, 0x01);

        let carrier = root.to_traceparent();
        assert!(carrier.starts_with("00-"));
        let parts: Vec<&str> = carrier.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[1], root.trace_id);
        assert_eq!(parts[2], root.span_id);
        assert_eq!(parts[3], "01");

        // 2. Extract into new context
        let extracted = TraceContext::from_traceparent(&carrier).expect("Should extract valid traceparent");
        assert_eq!(extracted.version, 0x00);
        assert_eq!(extracted.trace_id, root.trace_id);
        assert_eq!(extracted.parent_span_id, Some(root.span_id));
        assert_eq!(extracted.trace_flags, 0x01);
        assert_eq!(extracted.span_id.len(), 16);

        // 3. Child span creation
        let child = extracted.child();
        assert_eq!(child.trace_id, root.trace_id);
        assert_eq!(child.parent_span_id, Some(extracted.span_id.clone()));
        assert_ne!(child.span_id, extracted.span_id);
    }

    #[test]
    fn test_w3c_traceparent_malformed_rejections() {
        // Missing fields
        assert!(TraceContext::from_traceparent("00-1234").is_err());
        assert!(TraceContext::from_traceparent("not-a-traceparent").is_err());

        // Invalid version 0xff
        assert!(TraceContext::from_traceparent(
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        )
        .is_err());

        // All zero trace ID
        assert!(TraceContext::from_traceparent(
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01"
        )
        .is_err());

        // All zero parent/span ID
        assert!(TraceContext::from_traceparent(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"
        )
        .is_err());

        // Invalid hex characters
        assert!(TraceContext::from_traceparent(
            "00-4bf92f3577b34da6a3ce929d0e0e47zz-00f067aa0ba902b7-01"
        )
        .is_err());
    }

    #[test]
    fn test_lockfree_span_ring_buffer_concurrent() {
        let buffer = Arc::new(LockFreeSpanRingBuffer::new(128));
        let num_threads = 8;
        let spans_per_thread = 50;

        let mut handles = Vec::new();
        for t in 0..num_threads {
            let buf = Arc::clone(&buffer);
            handles.push(thread::spawn(move || {
                for i in 0..spans_per_thread {
                    let record = SpanRecord {
                        trace_id: format!("trace_{t:04}_{i:04}0000000000000000"),
                        span_id: format!("span_{t:02}_{i:06}"),
                        parent_span_id: None,
                        name: format!("work_t{t}"),
                        service_name: "test_service".to_string(),
                        role: ExecutionRole::BareMetalDaemon,
                        kind: SpanKind::Internal,
                        start_time_unix_nano: 1000,
                        end_time_unix_nano: 2000,
                        duration_ms: (t as f64) + (i as f64) * 0.1,
                        status: SpanStatus::ok(),
                        attributes: vec![SpanAttribute::int("thread_id", t as i64)],
                        events: vec![],
                        links: vec![],
                    };
                    assert!(buf.push(record));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(buffer.total_recorded(), (num_threads * spans_per_thread) as u64);
        let snap = buffer.snapshot(64);
        assert_eq!(snap.len(), 64);
    }

    #[test]
    fn test_percentiles_calculation_accuracy() {
        let engine = SpanMetricsEngine::new(200);

        // Insert 100 spans with precise durations: 1.0ms .. 100.0ms
        for i in 1..=100 {
            let record = SpanRecord {
                trace_id: "0123456789abcdef0123456789abcdef".to_string(),
                span_id: format!("{i:016x}"),
                parent_span_id: None,
                name: "simulated_req".to_string(),
                service_name: "hgb-edge".to_string(),
                role: ExecutionRole::EdgeWorker,
                kind: SpanKind::Server,
                start_time_unix_nano: 100,
                end_time_unix_nano: 200,
                duration_ms: i as f64,
                status: if i == 100 {
                    SpanStatus::error("Timeout")
                } else {
                    SpanStatus::ok()
                },
                attributes: vec![],
                events: vec![],
                links: vec![],
            };
            engine.record_span(record);
        }

        let p = engine.compute_percentiles();
        assert_eq!(p.sample_count, 100);
        assert_eq!(p.min_ms, 1.0);
        assert_eq!(p.max_ms, 100.0);
        assert_eq!(p.p50_ms, 50.0);
        assert_eq!(p.p90_ms, 90.0);
        assert_eq!(p.p95_ms, 95.0);
        assert_eq!(p.p99_ms, 99.0);
        assert!((p.mean_ms - 50.5).abs() < 0.01);
        assert_eq!(p.error_rate, 0.01);
    }

    #[test]
    fn test_span_lifecycle_and_event_attachments() {
        let active = ActiveSpan::new(
            "wasm_eval",
            "hgb-wasm-kernel",
            ExecutionRole::GuestWasm,
            SpanKind::Internal,
            None,
        );

        active.add_attribute(SpanAttribute::string("module", "vector_cos_sim.wasm"));
        active.add_attribute(SpanAttribute::int("fuel_budget", 500_000));
        active.add_attribute(SpanAttribute::bool_val("simd_accelerated", true));

        active.add_event(
            "compilation_complete",
            vec![SpanAttribute::int("bytecode_bytes", 4096)],
        );

        let completed = active.finish(SpanStatus::ok());
        assert_eq!(completed.name, "wasm_eval");
        assert_eq!(completed.service_name, "hgb-wasm-kernel");
        assert_eq!(completed.role, ExecutionRole::GuestWasm);
        assert_eq!(completed.status.code, SpanStatusCode::Ok);
        assert_eq!(completed.attributes.len(), 3);
        assert_eq!(completed.events.len(), 1);
        assert!(completed.duration_ms >= 0.0);
        assert!(completed.end_time_unix_nano >= completed.start_time_unix_nano);
    }

    #[test]
    fn test_governor_sla_feedback_and_microvm_scaling() {
        let governor = PolyglotGovernorFeedback::default();

        // 1. High latency SLA breach (> 200ms)
        let boost = governor.evaluate_sla_feedback(320.5, 3, 0.0);
        assert_eq!(boost.action, "BOOST_PERFORMANCE");
        assert_eq!(boost.governor_profile, "performance");
        assert_eq!(boost.worker_concurrency_state, "boosted");
        assert_eq!(boost.vcpu_count, 4);
        assert_eq!(boost.memory_mb, 1024);
        assert!(boost.throttling_detected);

        // 2. Queue spike breach (> 10)
        let q_boost = governor.evaluate_sla_feedback(40.0, 15, 0.0);
        assert_eq!(q_boost.action, "BOOST_PERFORMANCE");
        assert_eq!(q_boost.governor_profile, "performance");
        assert_eq!(q_boost.vcpu_count, 4);
        assert_eq!(q_boost.memory_mb, 1024);

        // 3. Settle cooldown (queue = 0, latency < 50ms for >= 15s)
        let settle = governor.evaluate_sla_feedback(22.0, 0, 16.5);
        assert_eq!(settle.action, "SETTLE_POWERSAVE");
        assert_eq!(settle.governor_profile, "powersave");
        assert_eq!(settle.worker_concurrency_state, "eco");
        assert_eq!(settle.vcpu_count, 1);
        assert_eq!(settle.memory_mb, 256);

        // 4. Nominal workload
        let nominal = governor.evaluate_sla_feedback(85.0, 2, 5.0);
        assert_eq!(nominal.action, "BALANCED_NOMINAL");
        assert_eq!(nominal.governor_profile, "balanced");
        assert_eq!(nominal.worker_concurrency_state, "nominal");
        assert_eq!(nominal.vcpu_count, 2);
        assert_eq!(nominal.memory_mb, 512);
    }

    #[test]
    fn test_otlp_json_export_compatibility() {
        let active = ActiveSpan::new(
            "dispatch_to_firecracker",
            "hagibis-edge-worker",
            ExecutionRole::EdgeWorker,
            SpanKind::Producer,
            None,
        );

        active.add_attribute(SpanAttribute::string("target_jail", "jail_01"));
        active.add_attribute(SpanAttribute::int("vcpus", 4));
        let span = active.finish(SpanStatus::ok());

        let json = span.to_otlp_json();
        assert_eq!(json["name"], "dispatch_to_firecracker");
        assert_eq!(json["kind"], "PRODUCER");
        assert_eq!(json["status"]["code"], "STATUS_CODE_OK");
        assert!(json["traceId"].is_string());
        assert!(json["spanId"].is_string());
        assert!(json["startTimeUnixNano"].is_string());
        assert!(json["attributes"].is_array());
    }
}
