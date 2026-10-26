//! # High-Performance Linux io_uring & Zero-Copy SHM IPC Engine
//!
//! Sub-microsecond asynchronous IPC engine leveraging Linux io_uring submission
//! and completion queues with automatic kernel probing and graceful fallback to
//! non-blocking Unix Domain Socket event polling.
//!
//! Seamlessly integrates with `ShmRingBuffer` for zero-copy message frame notifications
//! without kernel context switch overhead:
//! - Streaming context chunks (megabyte-scale prompt windows)
//! - LLM token streams (streaming token deltas)
//! - Daemon RPC commands and responses

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::shm_ring_buffer::{
    resolve_shm_path, ShmRingBuffer, ShmRingConsumer, ShmRingProducer, DEFAULT_SHM_CAPACITY,
};
use crate::{HgbError, Result};

/// Magic identifier for Hagibis io_uring IPC message frames ("HGB_IOUR")
pub const IPC_FRAME_MAGIC: u64 = 0x4847425F494F5552;

/// Linux io_uring syscall numbers
#[cfg(target_os = "linux")]
const SYS_IO_URING_SETUP: libc::c_long = libc::SYS_io_uring_setup;
#[cfg(target_os = "linux")]
const SYS_IO_URING_ENTER: libc::c_long = libc::SYS_io_uring_enter;

/// Linux io_uring kernel submission queue flags & offsets
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct io_sqring_offsets {
    pub head: u32,
    pub tail: u32,
    pub ring_mask: u32,
    pub ring_entries: u32,
    pub flags: u32,
    pub dropped: u32,
    pub array: u32,
    pub resv1: u32,
    pub user_addr: u64,
}

/// Linux io_uring kernel completion queue offsets
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct io_cqring_offsets {
    pub head: u32,
    pub tail: u32,
    pub ring_mask: u32,
    pub ring_entries: u32,
    pub overflow: u32,
    pub cqes: u32,
    pub flags: u32,
    pub resv1: u32,
    pub user_addr: u64,
}

/// Linux io_uring initialization parameters
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct io_uring_params {
    pub sq_entries: u32,
    pub cq_entries: u32,
    pub flags: u32,
    pub sq_thread_cpu: u32,
    pub sq_thread_idle: u32,
    pub features: u32,
    pub wq_fd: u32,
    pub resv: [u32; 3],
    pub sq_off: io_sqring_offsets,
    pub cq_off: io_cqring_offsets,
}

/// Active transport backend used by the IPC engine
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcBackendKind {
    /// Native Linux io_uring kernel submission & completion queues
    IoUring,
    /// High-performance non-blocking Unix Domain Socket event polling fallback
    UnixSocketFallback,
}

impl Default for IpcBackendKind {
    fn default() -> Self {
        if probe_kernel_io_uring() {
            Self::IoUring
        } else {
            Self::UnixSocketFallback
        }
    }
}

/// Safely probe whether the Linux kernel supports `io_uring_setup`
pub fn probe_kernel_io_uring() -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut params: io_uring_params = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::syscall(
                SYS_IO_URING_SETUP,
                8 as libc::c_uint,
                &mut params as *mut io_uring_params,
            )
        };
        if ret >= 0 {
            unsafe { libc::close(ret as libc::c_int) };
            true
        } else {
            false
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Streaming context window chunk
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextChunk {
    pub chunk_id: u32,
    pub total_chunks: Option<u32>,
    pub is_final: bool,
    pub data: Vec<u8>,
}

/// Streaming LLM token chunk
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenStreamChunk {
    pub seq: u32,
    pub token: String,
    pub is_done: bool,
    pub timestamp_ns: u64,
}

/// Remote procedure call command from client to daemon
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpcRpcCommand {
    pub call_id: String,
    pub method: String,
    pub params: String,
}

impl IpcRpcCommand {
    pub fn params_value(&self) -> serde_json::Value {
        serde_json::from_str(&self.params).unwrap_or(serde_json::Value::Null)
    }
}

/// Remote procedure call response from daemon to client
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpcRpcResponse {
    pub call_id: String,
    pub success: bool,
    pub result: String,
    pub error: Option<String>,
}

impl IpcRpcResponse {
    pub fn result_value(&self) -> serde_json::Value {
        serde_json::from_str(&self.result).unwrap_or(serde_json::Value::Null)
    }
}

/// Typed payloads transferred through the zero-copy ring buffer
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IpcMessageKind {
    ContextChunk(ContextChunk),
    TokenStream(TokenStreamChunk),
    RpcCommand(IpcRpcCommand),
    RpcResponse(IpcRpcResponse),
    RawBytes(Vec<u8>),
    HeartbeatTick { pid: u32, timestamp_ns: u64 },
}

/// Wire envelope for IPC message frames
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpcMessageFrame {
    pub magic: u64,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub kind: IpcMessageKind,
}

impl IpcMessageFrame {
    pub fn new(sequence: u64, kind: IpcMessageKind) -> Self {
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        Self {
            magic: IPC_FRAME_MAGIC,
            sequence,
            timestamp_ns,
            kind,
        }
    }
}

/// Telemetry and transfer statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEngineStats {
    pub backend: IpcBackendKind,
    pub frames_sent: u64,
    pub frames_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub notifications_sent: u64,
    pub notifications_received: u64,
    pub ring_capacity: usize,
    pub ring_available_read: usize,
    pub ring_available_write: usize,
}

/// Low-level signaling channel backing message notifications
pub struct NotificationSignaler {
    backend: IpcBackendKind,
    event_fd: Option<RawFd>,
    io_uring_fd: Option<RawFd>,
    writer_sock: Option<UnixStream>,
    reader_sock: Option<UnixStream>,
}

impl NotificationSignaler {
    /// Create connected notification signaler pair
    pub fn create_pair(backend: IpcBackendKind) -> Result<(Self, Self)> {
        match backend {
            IpcBackendKind::IoUring => {
                #[cfg(target_os = "linux")]
                {
                    let ev_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
                    if ev_fd < 0 {
                        return Self::create_unix_pair();
                    }

                    let mut params: io_uring_params = unsafe { std::mem::zeroed() };
                    let uring_fd = unsafe {
                        libc::syscall(
                            SYS_IO_URING_SETUP,
                            16 as libc::c_uint,
                            &mut params as *mut io_uring_params,
                        )
                    };

                    let uring_raw_fd = if uring_fd >= 0 {
                        Some(uring_fd as RawFd)
                    } else {
                        None
                    };

                    let ev_fd_dup = unsafe { libc::dup(ev_fd) };
                    if ev_fd_dup < 0 {
                        unsafe { libc::close(ev_fd) };
                        if let Some(ufd) = uring_raw_fd {
                            unsafe { libc::close(ufd) };
                        }
                        return Self::create_unix_pair();
                    }

                    let producer = Self {
                        backend: IpcBackendKind::IoUring,
                        event_fd: Some(ev_fd),
                        io_uring_fd: None,
                        writer_sock: None,
                        reader_sock: None,
                    };

                    let consumer = Self {
                        backend: IpcBackendKind::IoUring,
                        event_fd: Some(ev_fd_dup),
                        io_uring_fd: uring_raw_fd,
                        writer_sock: None,
                        reader_sock: None,
                    };

                    Ok((producer, consumer))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Self::create_unix_pair()
                }
            }
            IpcBackendKind::UnixSocketFallback => Self::create_unix_pair(),
        }
    }

    fn create_unix_pair() -> Result<(Self, Self)> {
        let (s1, s2) = UnixStream::pair()?;
        s1.set_nonblocking(true)?;
        s2.set_nonblocking(true)?;

        let producer = Self {
            backend: IpcBackendKind::UnixSocketFallback,
            event_fd: None,
            io_uring_fd: None,
            writer_sock: Some(s1),
            reader_sock: None,
        };

        let consumer = Self {
            backend: IpcBackendKind::UnixSocketFallback,
            event_fd: None,
            io_uring_fd: None,
            writer_sock: None,
            reader_sock: Some(s2),
        };

        Ok((producer, consumer))
    }

    /// Send notification signal to the reader
    pub fn notify(&self) {
        if let Some(efd) = self.event_fd {
            let val: u64 = 1;
            unsafe {
                libc::write(
                    efd,
                    &val as *const u64 as *const libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        } else if let Some(ref sock) = self.writer_sock {
            use std::io::Write;
            let _ = (&*sock).write(&[1u8]);
        }
    }

    /// Drain pending notification signals without blocking
    pub fn drain(&self) {
        if let Some(efd) = self.event_fd {
            let mut val: u64 = 0;
            unsafe {
                libc::read(
                    efd,
                    &mut val as *mut u64 as *mut libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        } else if let Some(ref sock) = self.reader_sock {
            use std::io::Read;
            let mut buf = [0u8; 64];
            let _ = (&*sock).read(&mut buf);
        }
    }

    /// Wait for a notification signal up to timeout
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let poll_fd = if let Some(efd) = self.event_fd {
            efd
        } else if let Some(ref sock) = self.reader_sock {
            sock.as_raw_fd()
        } else {
            return false;
        };

        let mut pfd = libc::pollfd {
            fd: poll_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        let ret = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
        if ret > 0 && (pfd.revents & libc::POLLIN != 0) {
            self.drain();
            true
        } else {
            false
        }
    }

    pub fn backend(&self) -> IpcBackendKind {
        self.backend
    }
}

impl Drop for NotificationSignaler {
    fn drop(&mut self) {
        if let Some(efd) = self.event_fd {
            unsafe { libc::close(efd) };
        }
        if let Some(ufd) = self.io_uring_fd {
            unsafe { libc::close(ufd) };
        }
    }
}

/// Producer half of the io_uring asynchronous IPC channel
pub struct IoUringIpcProducer {
    name: String,
    producer: ShmRingProducer,
    signaler: NotificationSignaler,
    sequence: AtomicU64,
    frames_sent: AtomicU64,
    bytes_sent: AtomicU64,
    notifications_sent: AtomicU64,
}

impl IoUringIpcProducer {
    pub fn backend(&self) -> IpcBackendKind {
        self.signaler.backend()
    }

    pub fn send_frame(&mut self, kind: IpcMessageKind) -> Result<bool> {
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let frame = IpcMessageFrame::new(seq, kind);
        let encoded = bincode::serialize(&frame)
            .map_err(|e| HgbError::Storage(format!("IPC bincode serialization error: {}", e)))?;

        let written = self.producer.try_write(&encoded)?;
        if written {
            self.frames_sent.fetch_add(1, Ordering::Relaxed);
            self.bytes_sent.fetch_add(encoded.len() as u64, Ordering::Relaxed);
            self.signaler.notify();
            self.notifications_sent.fetch_add(1, Ordering::Relaxed);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn send_context_chunk(
        &mut self,
        chunk_id: u32,
        total_chunks: Option<u32>,
        is_final: bool,
        data: Vec<u8>,
    ) -> Result<bool> {
        self.send_frame(IpcMessageKind::ContextChunk(ContextChunk {
            chunk_id,
            total_chunks,
            is_final,
            data,
        }))
    }

    pub fn send_token(&mut self, seq: u32, token: &str, is_done: bool) -> Result<bool> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        self.send_frame(IpcMessageKind::TokenStream(TokenStreamChunk {
            seq,
            token: token.to_string(),
            is_done,
            timestamp_ns: now_ns,
        }))
    }

    pub fn send_rpc_command(
        &mut self,
        call_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<bool> {
        self.send_frame(IpcMessageKind::RpcCommand(IpcRpcCommand {
            call_id: call_id.to_string(),
            method: method.to_string(),
            params: params.to_string(),
        }))
    }

    pub fn send_rpc_response(
        &mut self,
        call_id: &str,
        success: bool,
        result: serde_json::Value,
        error: Option<String>,
    ) -> Result<bool> {
        self.send_frame(IpcMessageKind::RpcResponse(IpcRpcResponse {
            call_id: call_id.to_string(),
            success,
            result: result.to_string(),
            error,
        }))
    }

    pub fn send_raw(&mut self, data: &[u8]) -> Result<bool> {
        self.send_frame(IpcMessageKind::RawBytes(data.to_vec()))
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.producer.available_write()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.producer.capacity()
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.producer.set_unlink_on_drop(unlink);
    }

    pub fn stats(&self) -> IpcEngineStats {
        IpcEngineStats {
            backend: self.backend(),
            frames_sent: self.frames_sent.load(Ordering::Relaxed),
            frames_received: 0,
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: 0,
            notifications_sent: self.notifications_sent.load(Ordering::Relaxed),
            notifications_received: 0,
            ring_capacity: self.producer.capacity(),
            ring_available_read: self.producer.available_read(),
            ring_available_write: self.producer.available_write(),
        }
    }
}

/// Consumer half of the io_uring asynchronous IPC channel
pub struct IoUringIpcConsumer {
    name: String,
    consumer: ShmRingConsumer,
    signaler: NotificationSignaler,
    frames_received: AtomicU64,
    bytes_received: AtomicU64,
    notifications_received: AtomicU64,
}

impl IoUringIpcConsumer {
    pub fn backend(&self) -> IpcBackendKind {
        self.signaler.backend()
    }

    pub fn try_recv_frame(&mut self) -> Result<Option<IpcMessageFrame>> {
        self.signaler.drain();

        match self.consumer.try_read()? {
            Some(raw) => {
                let frame: IpcMessageFrame = bincode::deserialize(&raw).map_err(|e| {
                    HgbError::Storage(format!("IPC bincode deserialization error: {}", e))
                })?;
                self.frames_received.fetch_add(1, Ordering::Relaxed);
                self.bytes_received.fetch_add(raw.len() as u64, Ordering::Relaxed);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }

    pub fn recv_frame_timeout(&mut self, timeout: Duration) -> Result<Option<IpcMessageFrame>> {
        if let Some(frame) = self.try_recv_frame()? {
            return Ok(Some(frame));
        }

        let start = Instant::now();
        while start.elapsed() < timeout {
            let remaining = timeout.saturating_sub(start.elapsed());
            if self.signaler.wait_timeout(remaining) {
                self.notifications_received.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(frame) = self.try_recv_frame()? {
                return Ok(Some(frame));
            }
        }

        Ok(None)
    }

    pub fn recv_token_timeout(&mut self, timeout: Duration) -> Result<Option<TokenStreamChunk>> {
        match self.recv_frame_timeout(timeout)? {
            Some(frame) => match frame.kind {
                IpcMessageKind::TokenStream(t) => Ok(Some(t)),
                other => Err(HgbError::Storage(format!(
                    "Expected TokenStream frame, found {:?}",
                    other
                ))),
            },
            None => Ok(None),
        }
    }

    pub fn recv_context_chunk_timeout(&mut self, timeout: Duration) -> Result<Option<ContextChunk>> {
        match self.recv_frame_timeout(timeout)? {
            Some(frame) => match frame.kind {
                IpcMessageKind::ContextChunk(c) => Ok(Some(c)),
                other => Err(HgbError::Storage(format!(
                    "Expected ContextChunk frame, found {:?}",
                    other
                ))),
            },
            None => Ok(None),
        }
    }

    pub fn recv_rpc_command_timeout(&mut self, timeout: Duration) -> Result<Option<IpcRpcCommand>> {
        match self.recv_frame_timeout(timeout)? {
            Some(frame) => match frame.kind {
                IpcMessageKind::RpcCommand(cmd) => Ok(Some(cmd)),
                other => Err(HgbError::Storage(format!(
                    "Expected RpcCommand frame, found {:?}",
                    other
                ))),
            },
            None => Ok(None),
        }
    }

    pub fn recv_rpc_response_timeout(&mut self, timeout: Duration) -> Result<Option<IpcRpcResponse>> {
        match self.recv_frame_timeout(timeout)? {
            Some(frame) => match frame.kind {
                IpcMessageKind::RpcResponse(resp) => Ok(Some(resp)),
                other => Err(HgbError::Storage(format!(
                    "Expected RpcResponse frame, found {:?}",
                    other
                ))),
            },
            None => Ok(None),
        }
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        self.consumer.available_read()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.consumer.capacity()
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.consumer.set_unlink_on_drop(unlink);
    }

    pub fn unlink(&self) -> Result<()> {
        self.consumer.unlink()
    }

    pub fn stats(&self) -> IpcEngineStats {
        IpcEngineStats {
            backend: self.backend(),
            frames_sent: 0,
            frames_received: self.frames_received.load(Ordering::Relaxed),
            bytes_sent: 0,
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            notifications_sent: 0,
            notifications_received: self.notifications_received.load(Ordering::Relaxed),
            ring_capacity: self.consumer.capacity(),
            ring_available_read: self.consumer.available_read(),
            ring_available_write: self.consumer.available_write(),
        }
    }
}

/// Unified Linux io_uring asynchronous IPC Engine
pub struct IoUringIpcEngine {
    name: String,
    producer: IoUringIpcProducer,
    consumer: IoUringIpcConsumer,
}

impl IoUringIpcEngine {
    pub fn create(name: &str, capacity: usize) -> Result<Self> {
        let backend = if probe_kernel_io_uring() {
            IpcBackendKind::IoUring
        } else {
            IpcBackendKind::UnixSocketFallback
        };

        let ring_buffer = ShmRingBuffer::create(name, capacity)?;
        let (shm_prod, shm_cons) = ring_buffer.split()?;
        let (sig_prod, sig_cons) = NotificationSignaler::create_pair(backend)?;

        let producer = IoUringIpcProducer {
            name: name.to_string(),
            producer: shm_prod,
            signaler: sig_prod,
            sequence: AtomicU64::new(0),
            frames_sent: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            notifications_sent: AtomicU64::new(0),
        };

        let consumer = IoUringIpcConsumer {
            name: name.to_string(),
            consumer: shm_cons,
            signaler: sig_cons,
            frames_received: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            notifications_received: AtomicU64::new(0),
        };

        Ok(Self {
            name: name.to_string(),
            producer,
            consumer,
        })
    }

    pub fn split(self) -> (IoUringIpcProducer, IoUringIpcConsumer) {
        (self.producer, self.consumer)
    }

    pub fn backend(&self) -> IpcBackendKind {
        self.producer.backend()
    }

    pub fn is_io_uring(&self) -> bool {
        self.backend() == IpcBackendKind::IoUring
    }

    pub fn send_frame(&mut self, kind: IpcMessageKind) -> Result<bool> {
        self.producer.send_frame(kind)
    }

    pub fn try_recv_frame(&mut self) -> Result<Option<IpcMessageFrame>> {
        self.consumer.try_recv_frame()
    }

    pub fn recv_frame_timeout(&mut self, timeout: Duration) -> Result<Option<IpcMessageFrame>> {
        self.consumer.recv_frame_timeout(timeout)
    }

    pub fn send_context_chunk(
        &mut self,
        chunk_id: u32,
        total_chunks: Option<u32>,
        is_final: bool,
        data: Vec<u8>,
    ) -> Result<bool> {
        self.producer
            .send_context_chunk(chunk_id, total_chunks, is_final, data)
    }

    pub fn recv_context_chunk_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ContextChunk>> {
        self.consumer.recv_context_chunk_timeout(timeout)
    }

    pub fn send_token(&mut self, seq: u32, token: &str, is_done: bool) -> Result<bool> {
        self.producer.send_token(seq, token, is_done)
    }

    pub fn recv_token_timeout(&mut self, timeout: Duration) -> Result<Option<TokenStreamChunk>> {
        self.consumer.recv_token_timeout(timeout)
    }

    pub fn send_rpc_command(
        &mut self,
        call_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<bool> {
        self.producer.send_rpc_command(call_id, method, params)
    }

    pub fn recv_rpc_command_timeout(&mut self, timeout: Duration) -> Result<Option<IpcRpcCommand>> {
        self.consumer.recv_rpc_command_timeout(timeout)
    }

    pub fn send_rpc_response(
        &mut self,
        call_id: &str,
        success: bool,
        result: serde_json::Value,
        error: Option<String>,
    ) -> Result<bool> {
        self.producer
            .send_rpc_response(call_id, success, result, error)
    }

    pub fn recv_rpc_response_timeout(&mut self, timeout: Duration) -> Result<Option<IpcRpcResponse>> {
        self.consumer.recv_rpc_response_timeout(timeout)
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        self.consumer.available_read()
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.producer.available_write()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.producer.capacity()
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.producer.set_unlink_on_drop(unlink);
        self.consumer.set_unlink_on_drop(unlink);
    }

    pub fn unlink(&self) -> Result<()> {
        self.consumer.unlink()
    }

    pub fn stats(&self) -> IpcEngineStats {
        IpcEngineStats {
            backend: self.backend(),
            frames_sent: self.producer.frames_sent.load(Ordering::Relaxed),
            frames_received: self.consumer.frames_received.load(Ordering::Relaxed),
            bytes_sent: self.producer.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.consumer.bytes_received.load(Ordering::Relaxed),
            notifications_sent: self.producer.notifications_sent.load(Ordering::Relaxed),
            notifications_received: self.consumer.notifications_received.load(Ordering::Relaxed),
            ring_capacity: self.producer.capacity(),
            ring_available_read: self.consumer.available_read(),
            ring_available_write: self.producer.available_write(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_io_uring_probe() {
        let _supported = probe_kernel_io_uring();
        let name = format!("test_probe_{}", uuid::Uuid::new_v4().simple());
        let engine = IoUringIpcEngine::create(&name, 1024 * 64).expect("create engine");
        assert!(matches!(
            engine.backend(),
            IpcBackendKind::IoUring | IpcBackendKind::UnixSocketFallback
        ));
    }

    #[test]
    fn test_io_uring_ipc_engine_context_chunk() {
        let name = format!("test_io_uring_ctx_{}", uuid::Uuid::new_v4().simple());
        let mut engine = IoUringIpcEngine::create(&name, 1024 * 64).expect("create engine");
        engine.set_unlink_on_drop(true);

        let payload = b"fn main() { println!(\"Hagibis\"); }".to_vec();
        let sent = engine
            .send_context_chunk(0, Some(1), true, payload.clone())
            .expect("send context");
        assert!(sent);

        let received = engine
            .recv_context_chunk_timeout(Duration::from_millis(50))
            .expect("recv context")
            .expect("chunk present");

        assert_eq!(received.chunk_id, 0);
        assert_eq!(received.total_chunks, Some(1));
        assert!(received.is_final);
        assert_eq!(received.data, payload);
    }

    #[test]
    fn test_io_uring_ipc_token_stream() {
        let name = format!("test_io_uring_tokens_{}", uuid::Uuid::new_v4().simple());
        let mut engine = IoUringIpcEngine::create(&name, 1024 * 64).expect("create engine");
        engine.set_unlink_on_drop(true);

        let tokens = ["Hello", " ", "World", "!"];
        for (i, t) in tokens.iter().enumerate() {
            let is_final = i == tokens.len() - 1;
            let sent = engine.send_token(i as u32, t, is_final).expect("send token");
            assert!(sent);
        }

        for (i, expected) in tokens.iter().enumerate() {
            let chunk = engine
                .recv_token_timeout(Duration::from_millis(50))
                .expect("recv token")
                .expect("token present");
            assert_eq!(chunk.token, *expected);
            assert_eq!(chunk.seq, i as u32);
            if i == tokens.len() - 1 {
                assert!(chunk.is_done);
            }
        }
    }

    #[test]
    fn test_io_uring_ipc_rpc_roundtrip() {
        let name = format!("test_io_uring_rpc_{}", uuid::Uuid::new_v4().simple());
        let mut engine = IoUringIpcEngine::create(&name, 1024 * 64).expect("create engine");
        engine.set_unlink_on_drop(true);

        let call_id = "rpc-call-42";
        let params = serde_json::json!({ "model": "qwen2.5-coder", "max_tokens": 128 });
        let sent = engine.send_rpc_command(call_id, "generate_patch", params.clone()).expect("send rpc");
        assert!(sent);

        let cmd = engine
            .recv_rpc_command_timeout(Duration::from_millis(50))
            .expect("recv rpc")
            .expect("cmd present");
        assert_eq!(cmd.call_id, call_id);
        assert_eq!(cmd.method, "generate_patch");
        assert_eq!(cmd.params_value(), params);

        let result_json = serde_json::json!({ "diff": "+ fn healed() {}" });
        let sent_res = engine.send_rpc_response(call_id, true, result_json.clone(), None).expect("send res");
        assert!(sent_res);

        let resp = engine
            .recv_rpc_response_timeout(Duration::from_millis(50))
            .expect("recv res")
            .expect("resp present");
        assert_eq!(resp.call_id, call_id);
        assert!(resp.success);
        assert_eq!(resp.result_value(), result_json);
        assert!(resp.error.is_none());
    }
}
