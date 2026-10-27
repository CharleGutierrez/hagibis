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
}
