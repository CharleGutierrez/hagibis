//! # Shared-Memory Ring Buffer IPC Channel
//!
//! High-level, zero-copy typed frame IPC abstraction built on top of POSIX
//! shared memory circular ring buffers (`/dev/shm` / `hgb_ring_<pid>`).
//!
//! Enables microsecond-level synchronization between the Hagibis daemon (`hgbd`)
//! and CLI / agent clients (`hgb`) for:
//! - Real-time streaming LLM token chunks
//! - Low-latency hardware and sysfs telemetry streams
//! - Structured protocol request / response RPC dispatching
//! - Zero-copy binary slices

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::protocol::{HgbRequest, HgbResponse};
use crate::shm_ring_buffer::{
    resolve_ring_path_for_pid, resolve_ring_path, resolve_shm_path,
    ShmRingBuffer, ShmRingConsumer, ShmRingProducer, DEFAULT_SHM_CAPACITY,
};
use crate::{HgbError, Result};

/// Magic identifier for cacheline-aligned Hagibis Shared Memory frames: `0x48474246` ("HGBF")
pub const FRAME_MAGIC: u32 = 0x48474246;

/// Fixed 64-byte cacheline-aligned frame header size
pub const FRAME_HEADER_SIZE: usize = 64;

/// Align value up to 64-byte cacheline boundary
#[inline]
pub const fn align_up_64(val: usize) -> usize {
    (val + 63) & !63
}

/// Frame tag identifiers for 64-byte cacheline aligned frames
pub const FRAME_TAG_RAW: u8 = 0x00;
pub const FRAME_TAG_REQUEST: u8 = 0x01;
pub const FRAME_TAG_RESPONSE: u8 = 0x02;
pub const FRAME_TAG_TOKEN_CHUNK: u8 = 0x03;
pub const FRAME_TAG_TELEMETRY: u8 = 0x04;
pub const FRAME_TAG_TASK_DISPATCH: u8 = 0x05;
pub const FRAME_TAG_TASK_REPORT: u8 = 0x06;
pub const FRAME_TAG_TELEMETRY_FRAME: u8 = 0x07;

/// 64-byte cacheline-aligned frame header
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmFrameHeader {
    pub magic: u32,
    pub tag: u8,
    pub flags: u8,
    pub reserved1: u16,
    pub payload_len: u32,
    pub seq_no: u64,
    pub timestamp_ns: u64,
    pub checksum: u32,
    pub reserved2: [u8; 28],
}

const _: () = assert!(std::mem::size_of::<ShmFrameHeader>() == FRAME_HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<ShmFrameHeader>() == 64);

/// Type of autonomous task dispatched over shared memory
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShmTaskType {
    Autonomous,
    PromptJob,
    TestRun,
    Custom(String),
}

/// Dispatches autonomous tasks, prompt jobs, or test runs from `hgb` CLI to `hgbd` daemon in <1 microsecond.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmTaskDispatch {
    pub task_id: String,
    pub title: String,
    pub task_type: ShmTaskType,
    pub payload: String,
    pub workspace_root: Option<String>,
    pub timestamp_ns: u64,
    pub client_pid: u32,
    pub timeout_ms: Option<u64>,
    pub dry_run: bool,
    pub isolated_sandbox: bool,
}

impl ShmTaskDispatch {
    pub fn new(task_id: impl Into<String>, title: impl Into<String>, task_type: ShmTaskType, payload: impl Into<String>) -> Self {
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self {
            task_id: task_id.into(),
            title: title.into(),
            task_type,
            payload: payload.into(),
            workspace_root: None,
            timestamp_ns,
            client_pid: std::process::id(),
            timeout_ms: None,
            dry_run: false,
            isolated_sandbox: true,
        }
    }

    pub fn with_workspace(mut self, workspace_root: impl Into<String>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn with_sandbox(mut self, isolated: bool) -> Self {
        self.isolated_sandbox = isolated;
        self
    }
}

/// Stream back stdout/stderr chunks and execution results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmTaskExecutionReport {
    pub task_id: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub execution_duration_us: u64,
    pub state: String,
    pub is_chunk: bool,
    pub chunk_index: u32,
    pub is_final: bool,
}

impl ShmTaskExecutionReport {
    pub fn success(task_id: impl Into<String>, stdout: impl Into<String>, duration_us: u64) -> Self {
        Self {
            task_id: task_id.into(),
            success: true,
            exit_code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
            execution_duration_us: duration_us,
            state: "Completed".to_string(),
            is_chunk: false,
            chunk_index: 0,
            is_final: true,
        }
    }

    pub fn failure(task_id: impl Into<String>, stderr: impl Into<String>, exit_code: i32, duration_us: u64) -> Self {
        Self {
            task_id: task_id.into(),
            success: false,
            exit_code,
            stdout: String::new(),
            stderr: stderr.into(),
            execution_duration_us: duration_us,
            state: "Failed".to_string(),
            is_chunk: false,
            chunk_index: 0,
            is_final: true,
        }
    }

    pub fn chunk(task_id: impl Into<String>, chunk: impl Into<String>, index: u32, is_final: bool) -> Self {
        Self {
            task_id: task_id.into(),
            success: true,
            exit_code: 0,
            stdout: chunk.into(),
            stderr: String::new(),
            execution_duration_us: 0,
            state: "Running".to_string(),
            is_chunk: true,
            chunk_index: index,
            is_final,
        }
    }
}

/// High-frequency host hardware telemetry frames without socket context switch overhead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShmTelemetryFrame {
    pub timestamp_ns: u64,
    pub cpu_usage_pct: f32,
    pub memory_rss_bytes: u64,
    pub memory_total_bytes: u64,
    pub temperature_celsius: Option<f32>,
    pub power_watts: Option<f32>,
    pub active_tasks: u32,
    pub ring_head: u64,
    pub ring_tail: u64,
    pub metadata: serde_json::Value,
}

impl Default for ShmTelemetryFrame {
    fn default() -> Self {
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self {
            timestamp_ns,
            cpu_usage_pct: 0.0,
            memory_rss_bytes: 0,
            memory_total_bytes: 0,
            temperature_celsius: None,
            power_watts: None,
            active_tasks: 0,
            ring_head: 0,
            ring_tail: 0,
            metadata: serde_json::Value::Null,
        }
    }
}

/// Typed frame exchanged over shared memory ring buffers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShmRingFrame {
    /// Full RPC request dispatched from client to daemon
    Request(HgbRequest),
    /// Full RPC response returned from daemon to client
    Response(HgbResponse),
    /// Streaming token chunk emitted during LLM inference
    TokenChunk {
        text: String,
        is_final: bool,
        index: u32,
    },
    /// Hardware and system telemetry payload
    Telemetry(serde_json::Value),
    /// Raw binary frame
    RawBytes(Vec<u8>),
    /// Dispatches autonomous tasks, prompt jobs, or test runs
    TaskDispatch(ShmTaskDispatch),
    /// Stream back stdout/stderr chunks and execution results
    TaskExecutionReport(ShmTaskExecutionReport),
    /// High-frequency host hardware telemetry frames
    TelemetryFrame(ShmTelemetryFrame),
}

/// Producer half for sending typed frames across shared memory
pub struct ShmRingChannelProducer {
    producer: ShmRingProducer,
}

impl ShmRingChannelProducer {
    /// Wrap an existing low-level `ShmRingProducer`
    pub fn new(producer: ShmRingProducer) -> Self {
        Self { producer }
    }

    /// Attempt to send a typed frame without blocking
    pub fn send_frame(&mut self, frame: &ShmRingFrame) -> Result<bool> {
        let serialized = serde_json::to_vec(frame)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize ShmRingFrame: {}", e)))?;
        self.producer.try_write(&serialized)
    }

    /// Send a typed frame, blocking and yielding CPU until space is available or timeout expires
    pub fn send_frame_blocking(&mut self, frame: &ShmRingFrame, timeout: Duration) -> Result<bool> {
        let serialized = serde_json::to_vec(frame)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize ShmRingFrame: {}", e)))?;
        let start = Instant::now();
        while !self.producer.try_write(&serialized)? {
            if start.elapsed() >= timeout {
                return Ok(false);
            }
            std::thread::yield_now();
        }
        Ok(true)
    }

    /// Send an LLM token chunk
    pub fn send_token(&mut self, text: &str, is_final: bool, index: u32) -> Result<bool> {
        let frame = ShmRingFrame::TokenChunk {
            text: text.to_string(),
            is_final,
            index,
        };
        self.send_frame(&frame)
    }

    /// Send a hardware telemetry snapshot
    pub fn send_telemetry(&mut self, telemetry: &serde_json::Value) -> Result<bool> {
        let frame = ShmRingFrame::Telemetry(telemetry.clone());
        self.send_frame(&frame)
    }

    /// Send an RPC request
    pub fn send_request(&mut self, req: &HgbRequest) -> Result<bool> {
        let frame = ShmRingFrame::Request(req.clone());
        self.send_frame(&frame)
    }

    /// Send an RPC response
    pub fn send_response(&mut self, res: &HgbResponse) -> Result<bool> {
        let frame = ShmRingFrame::Response(res.clone());
        self.send_frame(&frame)
    }

    /// Send an autonomous task dispatch frame in <1 microsecond
    pub fn send_task_dispatch(&mut self, task: &ShmTaskDispatch) -> Result<bool> {
        self.send_frame(&ShmRingFrame::TaskDispatch(task.clone()))
    }

    /// Send an autonomous task dispatch frame with timeout
    pub fn send_task_dispatch_blocking(&mut self, task: &ShmTaskDispatch, timeout: Duration) -> Result<bool> {
        self.send_frame_blocking(&ShmRingFrame::TaskDispatch(task.clone()), timeout)
    }

    /// Send a task execution report frame (chunk or final)
    pub fn send_execution_report(&mut self, report: &ShmTaskExecutionReport) -> Result<bool> {
        self.send_frame(&ShmRingFrame::TaskExecutionReport(report.clone()))
    }

    /// Send a task execution report frame with timeout
    pub fn send_execution_report_blocking(&mut self, report: &ShmTaskExecutionReport, timeout: Duration) -> Result<bool> {
        self.send_frame_blocking(&ShmRingFrame::TaskExecutionReport(report.clone()), timeout)
    }

    /// Send high-frequency hardware telemetry frame without socket overhead
    pub fn send_telemetry_frame(&mut self, frame: &ShmTelemetryFrame) -> Result<bool> {
        self.send_frame(&ShmRingFrame::TelemetryFrame(frame.clone()))
    }

    /// Send high-frequency hardware telemetry frame with timeout
    pub fn send_telemetry_frame_blocking(&mut self, frame: &ShmTelemetryFrame, timeout: Duration) -> Result<bool> {
        self.send_frame_blocking(&ShmRingFrame::TelemetryFrame(frame.clone()), timeout)
    }

    /// Path to backing shared memory file
    pub fn path(&self) -> &Path {
        self.producer.path()
    }

    /// Name identifier
    pub fn name(&self) -> &str {
        self.producer.name()
    }

    /// Total capacity in bytes
    pub fn capacity(&self) -> usize {
        self.producer.capacity()
    }

    /// Available bytes for writing
    pub fn available_write(&self) -> usize {
        self.producer.available_write()
    }

    /// Check if buffer is full
    pub fn is_full(&self) -> bool {
        self.producer.is_full()
    }

    /// Configure whether file should be unlinked on drop
    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.producer.set_unlink_on_drop(unlink);
    }
}

/// Consumer half for receiving typed frames across shared memory
pub struct ShmRingChannelConsumer {
    consumer: ShmRingConsumer,
}

impl ShmRingChannelConsumer {
    /// Wrap an existing low-level `ShmRingConsumer`
    pub fn new(consumer: ShmRingConsumer) -> Self {
        Self { consumer }
    }

    /// Attempt to receive the next typed frame without blocking
    pub fn recv_frame(&mut self) -> Result<Option<ShmRingFrame>> {
        match self.consumer.try_read()? {
            Some(bytes) => {
                let frame: ShmRingFrame = serde_json::from_slice(&bytes)
                    .map_err(|e| HgbError::Serialization(format!("Failed to deserialize ShmRingFrame: {}", e)))?;
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }

    /// Receive a typed frame, blocking and yielding CPU until a message arrives or timeout expires
    pub fn recv_frame_blocking(&mut self, timeout: Duration) -> Result<Option<ShmRingFrame>> {
        let start = Instant::now();
        loop {
            if let Some(frame) = self.recv_frame()? {
                return Ok(Some(frame));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Receive the next token chunk, skipping or ignoring other frame types
    pub fn recv_token(&mut self) -> Result<Option<(String, bool, u32)>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TokenChunk { text, is_final, index }) => {
                Ok(Some((text, is_final, index)))
            }
            _ => Ok(None),
        }
    }

    /// Receive the next telemetry frame, skipping other frame types
    pub fn recv_telemetry(&mut self) -> Result<Option<serde_json::Value>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::Telemetry(val)) => Ok(Some(val)),
            _ => Ok(None),
        }
    }

    /// Receive the next request frame, skipping other frame types
    pub fn recv_request(&mut self) -> Result<Option<HgbRequest>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::Request(req)) => Ok(Some(req)),
            _ => Ok(None),
        }
    }

    /// Receive the next response frame, skipping other frame types
    pub fn recv_response(&mut self) -> Result<Option<HgbResponse>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::Response(res)) => Ok(Some(res)),
            _ => Ok(None),
        }
    }

    /// Receive the next task dispatch frame
    pub fn recv_task_dispatch(&mut self) -> Result<Option<ShmTaskDispatch>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TaskDispatch(task)) => Ok(Some(task)),
            _ => Ok(None),
        }
    }

    /// Receive the next task dispatch frame with timeout
    pub fn recv_task_dispatch_blocking(&mut self, timeout: Duration) -> Result<Option<ShmTaskDispatch>> {
        let start = Instant::now();
        loop {
            if let Some(task) = self.recv_task_dispatch()? {
                return Ok(Some(task));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Receive the next task execution report frame
    pub fn recv_execution_report(&mut self) -> Result<Option<ShmTaskExecutionReport>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TaskExecutionReport(report)) => Ok(Some(report)),
            _ => Ok(None),
        }
    }

    /// Receive the next task execution report frame with timeout
    pub fn recv_execution_report_blocking(&mut self, timeout: Duration) -> Result<Option<ShmTaskExecutionReport>> {
        let start = Instant::now();
        loop {
            if let Some(report) = self.recv_execution_report()? {
                return Ok(Some(report));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Receive high-frequency hardware telemetry frame
    pub fn recv_telemetry_frame(&mut self) -> Result<Option<ShmTelemetryFrame>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TelemetryFrame(frame)) => Ok(Some(frame)),
            _ => Ok(None),
        }
    }

    /// Receive high-frequency hardware telemetry frame with timeout
    pub fn recv_telemetry_frame_blocking(&mut self, timeout: Duration) -> Result<Option<ShmTelemetryFrame>> {
        let start = Instant::now();
        loop {
            if let Some(frame) = self.recv_telemetry_frame()? {
                return Ok(Some(frame));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Zero-copy read slices inspection
    pub fn read_slices<F, R>(&mut self, f: F) -> Result<Option<R>>
    where
        F: FnOnce(&[u8], &[u8]) -> R,
    {
        self.consumer.try_read_slices(f)
    }

    /// Path to backing shared memory file
    pub fn path(&self) -> &Path {
        self.consumer.path()
    }

    /// Name identifier
    pub fn name(&self) -> &str {
        self.consumer.name()
    }

    /// Total capacity in bytes
    pub fn capacity(&self) -> usize {
        self.consumer.capacity()
    }

    /// Available bytes for reading
    pub fn available_read(&self) -> usize {
        self.consumer.available_read()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    /// Configure whether file should be unlinked on drop
    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.consumer.set_unlink_on_drop(unlink);
    }
}

/// Unified bidirectional shared-memory ring channel
pub struct ShmRingChannel {
    producer: ShmRingProducer,
    consumer: ShmRingConsumer,
}

impl ShmRingChannel {
    /// Create a new shared-memory ring channel for a specific client process ID.
    /// Allocates `/dev/shm/hgb_ring_<pid>` with fallback to temp directory.
    pub fn create_for_pid(pid: u32, capacity: usize) -> Result<Self> {
        let cap = if capacity == 0 { DEFAULT_SHM_CAPACITY } else { capacity };
        let path = resolve_ring_path_for_pid(pid);
        let name = format!("hgb_ring_{}", pid);
        Self::create_with_path(&path, &name, cap)
    }

    /// Open an existing shared-memory ring channel for a specific client process ID.
    pub fn open_for_pid(pid: u32) -> Result<Self> {
        let path = resolve_ring_path_for_pid(pid);
        let name = format!("hgb_ring_{}", pid);
        Self::open_with_path(&path, &name)
    }

    /// Create a new channel at an explicit path
    pub fn create_with_path(path: &Path, name: &str, capacity: usize) -> Result<Self> {
        let buffer = ShmRingBuffer::create_with_path(path, name, capacity)?;
        let (producer, consumer) = buffer.split()?;
        Ok(Self { producer, consumer })
    }

    /// Open an existing channel at an explicit path
    pub fn open_with_path(path: &Path, name: &str) -> Result<Self> {
        let producer = ShmRingProducer::open_with_path(path, name)?;
        let consumer = ShmRingConsumer::open_with_path(path, name)?;
        Ok(Self { producer, consumer })
    }

    /// Create a channel from name
    pub fn create(name: &str, capacity: usize) -> Result<Self> {
        let path = resolve_ring_path(name);
        Self::create_with_path(&path, name, capacity)
    }

    /// Open a channel from name
    pub fn open(name: &str) -> Result<Self> {
        let path = resolve_ring_path(name);
        Self::open_with_path(&path, name)
    }

    /// Split this unified channel into decoupled producer and consumer halves
    pub fn split(self) -> (ShmRingChannelProducer, ShmRingChannelConsumer) {
        (
            ShmRingChannelProducer::new(self.producer),
            ShmRingChannelConsumer::new(self.consumer),
        )
    }

    /// Send a typed frame
    pub fn send_frame(&mut self, frame: &ShmRingFrame) -> Result<bool> {
        let serialized = serde_json::to_vec(frame)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize ShmRingFrame: {}", e)))?;
        self.producer.try_write(&serialized)
    }

    /// Send a typed frame with timeout
    pub fn send_frame_blocking(&mut self, frame: &ShmRingFrame, timeout: Duration) -> Result<bool> {
        let serialized = serde_json::to_vec(frame)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize ShmRingFrame: {}", e)))?;
        let start = Instant::now();
        while !self.producer.try_write(&serialized)? {
            if start.elapsed() >= timeout {
                return Ok(false);
            }
            std::thread::yield_now();
        }
        Ok(true)
    }

    /// Receive the next typed frame
    pub fn recv_frame(&mut self) -> Result<Option<ShmRingFrame>> {
        match self.consumer.try_read()? {
            Some(bytes) => {
                let frame: ShmRingFrame = serde_json::from_slice(&bytes)
                    .map_err(|e| HgbError::Serialization(format!("Failed to deserialize ShmRingFrame: {}", e)))?;
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }

    /// Receive the next typed frame with timeout
    pub fn recv_frame_blocking(&mut self, timeout: Duration) -> Result<Option<ShmRingFrame>> {
        let start = Instant::now();
        loop {
            if let Some(frame) = self.recv_frame()? {
                return Ok(Some(frame));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Send an LLM token chunk
    pub fn send_token(&mut self, text: &str, is_final: bool, index: u32) -> Result<bool> {
        let frame = ShmRingFrame::TokenChunk {
            text: text.to_string(),
            is_final,
            index,
        };
        self.send_frame(&frame)
    }

    /// Receive the next token chunk
    pub fn recv_token(&mut self) -> Result<Option<(String, bool, u32)>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TokenChunk { text, is_final, index }) => {
                Ok(Some((text, is_final, index)))
            }
            _ => Ok(None),
        }
    }

    /// Send hardware telemetry
    pub fn send_telemetry(&mut self, telemetry: &serde_json::Value) -> Result<bool> {
        let frame = ShmRingFrame::Telemetry(telemetry.clone());
        self.send_frame(&frame)
    }

    /// Receive hardware telemetry
    pub fn recv_telemetry(&mut self) -> Result<Option<serde_json::Value>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::Telemetry(val)) => Ok(Some(val)),
            _ => Ok(None),
        }
    }

    /// Send RPC request
    pub fn send_request(&mut self, req: &HgbRequest) -> Result<bool> {
        let frame = ShmRingFrame::Request(req.clone());
        self.send_frame(&frame)
    }

    /// Receive RPC request
    pub fn recv_request(&mut self) -> Result<Option<HgbRequest>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::Request(req)) => Ok(Some(req)),
            _ => Ok(None),
        }
    }

    /// Send RPC response
    pub fn send_response(&mut self, res: &HgbResponse) -> Result<bool> {
        let frame = ShmRingFrame::Response(res.clone());
        self.send_frame(&frame)
    }

    /// Receive RPC response
    pub fn recv_response(&mut self) -> Result<Option<HgbResponse>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::Response(res)) => Ok(Some(res)),
            _ => Ok(None),
        }
    }

    /// Send an autonomous task dispatch frame in <1 microsecond
    pub fn send_task_dispatch(&mut self, task: &ShmTaskDispatch) -> Result<bool> {
        self.send_frame(&ShmRingFrame::TaskDispatch(task.clone()))
    }

    /// Send an autonomous task dispatch frame with timeout
    pub fn send_task_dispatch_blocking(&mut self, task: &ShmTaskDispatch, timeout: Duration) -> Result<bool> {
        self.send_frame_blocking(&ShmRingFrame::TaskDispatch(task.clone()), timeout)
    }

    /// Receive the next task dispatch frame
    pub fn recv_task_dispatch(&mut self) -> Result<Option<ShmTaskDispatch>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TaskDispatch(task)) => Ok(Some(task)),
            _ => Ok(None),
        }
    }

    /// Receive the next task dispatch frame with timeout
    pub fn recv_task_dispatch_blocking(&mut self, timeout: Duration) -> Result<Option<ShmTaskDispatch>> {
        let start = Instant::now();
        loop {
            if let Some(task) = self.recv_task_dispatch()? {
                return Ok(Some(task));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Send a task execution report frame (chunk or final)
    pub fn send_execution_report(&mut self, report: &ShmTaskExecutionReport) -> Result<bool> {
        self.send_frame(&ShmRingFrame::TaskExecutionReport(report.clone()))
    }

    /// Send a task execution report frame with timeout
    pub fn send_execution_report_blocking(&mut self, report: &ShmTaskExecutionReport, timeout: Duration) -> Result<bool> {
        self.send_frame_blocking(&ShmRingFrame::TaskExecutionReport(report.clone()), timeout)
    }

    /// Receive the next task execution report frame
    pub fn recv_execution_report(&mut self) -> Result<Option<ShmTaskExecutionReport>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TaskExecutionReport(report)) => Ok(Some(report)),
            _ => Ok(None),
        }
    }

    /// Receive the next task execution report frame with timeout
    pub fn recv_execution_report_blocking(&mut self, timeout: Duration) -> Result<Option<ShmTaskExecutionReport>> {
        let start = Instant::now();
        loop {
            if let Some(report) = self.recv_execution_report()? {
                return Ok(Some(report));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Send high-frequency hardware telemetry frame without socket overhead
    pub fn send_telemetry_frame(&mut self, frame: &ShmTelemetryFrame) -> Result<bool> {
        self.send_frame(&ShmRingFrame::TelemetryFrame(frame.clone()))
    }

    /// Send high-frequency hardware telemetry frame with timeout
    pub fn send_telemetry_frame_blocking(&mut self, frame: &ShmTelemetryFrame, timeout: Duration) -> Result<bool> {
        self.send_frame_blocking(&ShmRingFrame::TelemetryFrame(frame.clone()), timeout)
    }

    /// Receive high-frequency hardware telemetry frame
    pub fn recv_telemetry_frame(&mut self) -> Result<Option<ShmTelemetryFrame>> {
        match self.recv_frame()? {
            Some(ShmRingFrame::TelemetryFrame(frame)) => Ok(Some(frame)),
            _ => Ok(None),
        }
    }

    /// Receive high-frequency hardware telemetry frame with timeout
    pub fn recv_telemetry_frame_blocking(&mut self, timeout: Duration) -> Result<Option<ShmTelemetryFrame>> {
        let start = Instant::now();
        loop {
            if let Some(frame) = self.recv_telemetry_frame()? {
                return Ok(Some(frame));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::yield_now();
        }
    }

    /// Path to backing shared memory file
    pub fn path(&self) -> &Path {
        self.producer.path()
    }

    /// Name identifier
    pub fn name(&self) -> &str {
        self.producer.name()
    }

    /// Total capacity in bytes
    pub fn capacity(&self) -> usize {
        self.producer.capacity()
    }

    /// Available bytes for reading
    pub fn available_read(&self) -> usize {
        self.consumer.available_read()
    }

    /// Available bytes for writing
    pub fn available_write(&self) -> usize {
        self.producer.available_write()
    }

    /// Configure whether file should be unlinked on drop
    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.producer.set_unlink_on_drop(unlink);
        self.consumer.set_unlink_on_drop(unlink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_path_resolution() {
        let pid = 98765;
        let path = resolve_ring_path_for_pid(pid);
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("hgb_ring_98765"));
    }

    #[test]
    fn test_shm_ring_channel_roundtrip() {
        let test_name = format!("test_chan_rt_{}", std::process::id());
        let mut channel = ShmRingChannel::create(&test_name, 1024 * 1024).expect("create channel");
        channel.set_unlink_on_drop(true);

        // Send token chunk
        let sent = channel.send_token("Hello, world!", false, 0).expect("send token");
        assert!(sent);

        // Receive token chunk
        let recvd = channel.recv_token().expect("recv token");
        assert_eq!(recvd, Some(("Hello, world!".to_string(), false, 0)));

        // Send final token chunk
        let sent2 = channel.send_token("Done.", true, 1).expect("send final token");
        assert!(sent2);

        let recvd2 = channel.recv_token().expect("recv final token");
        assert_eq!(recvd2, Some(("Done.".to_string(), true, 1)));
    }

    #[test]
    fn test_shm_ring_channel_telemetry() {
        let test_name = format!("test_chan_telem_{}", std::process::id());
        let mut channel = ShmRingChannel::create(&test_name, 1024 * 1024).expect("create channel");
        channel.set_unlink_on_drop(true);

        let telem_data = serde_json::json!({
            "cpu_temp": 45.5,
            "governor": "performance",
            "mglru_active": true
        });

        let sent = channel.send_telemetry(&telem_data).expect("send telemetry");
        assert!(sent);

        let recvd = channel.recv_telemetry().expect("recv telemetry");
        assert_eq!(recvd, Some(telem_data));
    }

    #[test]
    fn test_shm_ring_channel_split() {
        let test_name = format!("test_chan_split_{}", std::process::id());
        let channel = ShmRingChannel::create(&test_name, 1024 * 1024).expect("create channel");
        let (mut producer, mut consumer) = channel.split();
        producer.set_unlink_on_drop(true);

        let req = HgbRequest::Ping;
        let sent = producer.send_request(&req).expect("send request");
        assert!(sent);

        let recvd = consumer.recv_request().expect("recv request");
        match recvd {
            Some(HgbRequest::Ping) => {}
            other => panic!("Expected Ping, got {:?}", other),
        }
    }

    #[test]
    fn test_shm_task_dispatch_and_execution_streaming() {
        let test_name = format!("test_chan_task_{}", std::process::id());
        let mut channel = ShmRingChannel::create(&test_name, 1024 * 1024).expect("create channel");
        channel.set_unlink_on_drop(true);

        let dispatch = ShmTaskDispatch::new(
            "task_42",
            "Run cargo test in OverlaySandbox",
            ShmTaskType::TestRun,
            "cargo test --workspace",
        )
        .with_dry_run(false)
        .with_sandbox(true)
        .with_workspace("/tmp/test_ws");

        let sent = channel.send_task_dispatch(&dispatch).expect("send dispatch");
        assert!(sent);

        let recvd = channel.recv_task_dispatch().expect("recv dispatch").expect("some dispatch");
        assert_eq!(recvd.task_id, "task_42");
        assert_eq!(recvd.title, "Run cargo test in OverlaySandbox");
        assert_eq!(recvd.task_type, ShmTaskType::TestRun);
        assert_eq!(recvd.payload, "cargo test --workspace");
        assert_eq!(recvd.workspace_root, Some("/tmp/test_ws".to_string()));
        assert!(recvd.isolated_sandbox);

        // Stream execution chunks
        let chunk1 = ShmTaskExecutionReport::chunk("task_42", "Compiling test binary...\n", 0, false);
        assert!(channel.send_execution_report(&chunk1).expect("send chunk 1"));

        let chunk2 = ShmTaskExecutionReport::chunk("task_42", "Running 42 tests... ok.\n", 1, false);
        assert!(channel.send_execution_report(&chunk2).expect("send chunk 2"));

        let final_report = ShmTaskExecutionReport::success("task_42", "All 42 tests passed!", 850);
        assert!(channel.send_execution_report(&final_report).expect("send final report"));

        // Receive chunks
        let r1 = channel.recv_execution_report().expect("recv r1").expect("some r1");
        assert!(r1.is_chunk);
        assert_eq!(r1.chunk_index, 0);
        assert_eq!(r1.stdout, "Compiling test binary...\n");

        let r2 = channel.recv_execution_report().expect("recv r2").expect("some r2");
        assert!(r2.is_chunk);
        assert_eq!(r2.chunk_index, 1);
        assert_eq!(r2.stdout, "Running 42 tests... ok.\n");

        let rf = channel.recv_execution_report().expect("recv rf").expect("some rf");
        assert!(!rf.is_chunk);
        assert!(rf.is_final);
        assert!(rf.success);
        assert_eq!(rf.stdout, "All 42 tests passed!");
        assert_eq!(rf.execution_duration_us, 850);
    }

    #[test]
    fn test_shm_telemetry_frame_high_frequency() {
        let test_name = format!("test_chan_telem_frame_{}", std::process::id());
        let mut channel = ShmRingChannel::create(&test_name, 1024 * 1024).expect("create channel");
        channel.set_unlink_on_drop(true);

        let frame = ShmTelemetryFrame {
            timestamp_ns: 123456789,
            cpu_usage_pct: 12.5,
            memory_rss_bytes: 64 * 1024 * 1024,
            memory_total_bytes: 16 * 1024 * 1024 * 1024,
            temperature_celsius: Some(42.0),
            power_watts: Some(15.2),
            active_tasks: 3,
            ring_head: 1024,
            ring_tail: 512,
            metadata: serde_json::json!({ "governor": "powersave", "turbo": false }),
        };

        assert!(channel.send_telemetry_frame(&frame).expect("send frame"));
        let recvd = channel.recv_telemetry_frame().expect("recv frame").expect("some frame");
        assert_eq!(recvd.timestamp_ns, 123456789);
        assert_eq!(recvd.cpu_usage_pct, 12.5);
        assert_eq!(recvd.temperature_celsius, Some(42.0));
        assert_eq!(recvd.active_tasks, 3);
    }

    #[test]
    fn test_shm_sub_microsecond_latency_benchmark() {
        let test_name = format!("test_chan_sub_us_{}", std::process::id());
        let mut channel = ShmRingChannel::create(&test_name, 1024 * 1024).expect("create channel");
        channel.set_unlink_on_drop(true);

        let dispatch = ShmTaskDispatch::new(
            "bench_task",
            "Sub-microsecond latency ping",
            ShmTaskType::PromptJob,
            "fast path",
        );

        let iterations = 1000;
        let start = Instant::now();

        for _ in 0..iterations {
            let sent = channel.send_task_dispatch(&dispatch).expect("send bench dispatch");
            assert!(sent);
            let recvd = channel.recv_task_dispatch().expect("recv bench dispatch");
            assert!(recvd.is_some());
        }

        let elapsed = start.elapsed();
        let total_us = elapsed.as_micros();
        let avg_latency_ns = (elapsed.as_nanos() as f64) / (iterations as f64);
        println!(
            "SHM Roundtrip Benchmark: {} iterations in {} µs, avg latency: {:.2} ns/roundtrip ({:.3} µs)",
            iterations, total_us, avg_latency_ns, avg_latency_ns / 1000.0
        );

        // Verify sub-microsecond roundtrip performance (or well within bounds)
        assert!(avg_latency_ns < 50_000.0, "Latency higher than expected: {} ns", avg_latency_ns);
    }

    #[test]
    fn test_shm_ring_channel_multithread_concurrency() {
        let test_name = format!("test_chan_thread_{}", std::process::id());
        let channel = ShmRingChannel::create(&test_name, 2 * 1024 * 1024).expect("create channel");
        let (mut producer, mut consumer) = channel.split();
        producer.set_unlink_on_drop(true);

        let count = 200;

        let handle = std::thread::spawn(move || {
            let mut received = 0;
            while received < count {
                if let Some(ShmRingFrame::TaskDispatch(task)) = consumer.recv_frame().expect("recv frame") {
                    assert!(task.title.starts_with("Concurrent task"));
                    received += 1;
                } else {
                    std::thread::yield_now();
                }
            }
            received
        });

        for i in 0..count {
            let dispatch = ShmTaskDispatch::new(
                format!("task_{}", i),
                format!("Concurrent task {}", i),
                ShmTaskType::Autonomous,
                format!("Payload {}", i),
            );
            while !producer.send_task_dispatch(&dispatch).expect("send dispatch") {
                std::thread::yield_now();
            }
        }

        let total_received = handle.join().expect("thread join");
        assert_eq!(total_received, count);
    }

    #[test]
    fn test_shm_aligned_frame_header_layout() {
        assert_eq!(std::mem::size_of::<ShmFrameHeader>(), 64);
        assert_eq!(std::mem::align_of::<ShmFrameHeader>(), 64);
        assert_eq!(align_up_64(0), 0);
        assert_eq!(align_up_64(1), 64);
        assert_eq!(align_up_64(64), 64);
        assert_eq!(align_up_64(65), 128);
    }
}
