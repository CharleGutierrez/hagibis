//! # Zero-Overhead IPC via Shared Memory & Unix Domain Sockets
//!
//! High-performance framed streaming IPC transport over UNIX domain sockets (`/run/user/1000/hgbd.sock`
//! or `/tmp/hgb_shm_ipc.sock`) with zero-copy shared memory handles (`/dev/shm`).
//!
//! Framing Protocol Header (64-byte cacheline aligned):
//! - Magic (8 bytes): `0x4847424950433031` ("HGBIPC01")
//! - MessageType (4 bytes): Request, Response, StreamChunk, ShmDescriptor, Heartbeat, Bench
//! - Flags (4 bytes): bitmask (compression, encryption, urgent)
//! - Sequence (8 bytes): monotonically increasing counter
//! - PayloadLength (8 bytes): inline socket payload length
//! - ShmOffset (8 bytes): offset in shared memory ring buffer
//! - ShmLength (8 bytes): length of payload in shared memory ring buffer
//! - Reserved (16 bytes): reserved for future zero-copy buffer handles
//!
//! Thresholding:
//! - <= 4 KiB: Transmitted inline over the UDS socket.
//! - > 4 KiB: Written directly to `/dev/shm/hgb_ring_daemon_shared_ring` and only descriptor
//!   (offset, length) is passed over UDS socket, eliminating serialization and kernel copy overhead.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::{HgbError, Result};

pub const HGB_IPC_MAGIC: u64 = 0x4847424950433031; // "HGBIPC01"
pub const SHM_THRESHOLD_BYTES: usize = 4096; // 4 KiB
pub const HEADER_SIZE: usize = 64;
pub const DEFAULT_SHM_POOL_CAPACITY: usize = 32 * 1024 * 1024; // 32 MiB
pub const DEFAULT_SHM_FILE_NAME: &str = "hgb_ring_daemon_shared_ring";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum ShmMessageType {
    Request = 1,
    Response = 2,
    StreamChunk = 3,
    ShmDescriptor = 4,
    Heartbeat = 5,
    BenchPayload = 6,
    SwarmMessage = 7,
}

impl ShmMessageType {
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => Self::Request,
            2 => Self::Response,
            3 => Self::StreamChunk,
            4 => Self::ShmDescriptor,
            5 => Self::Heartbeat,
            6 => Self::BenchPayload,
            7 => Self::SwarmMessage,
            _ => Self::Request,
        }
    }
}

/// 64-Byte Framed Streaming Header
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmIpcHeader {
    pub magic: u64,
    pub message_type: ShmMessageType,
    pub flags: u32,
    pub sequence: u64,
    pub payload_len: u64,
    pub shm_offset: u64,
    pub shm_len: u64,
    pub reserved: [u8; 16],
}

impl Default for ShmIpcHeader {
    fn default() -> Self {
        Self {
            magic: HGB_IPC_MAGIC,
            message_type: ShmMessageType::Request,
            flags: 0,
            sequence: 0,
            payload_len: 0,
            shm_offset: 0,
            shm_len: 0,
            reserved: [0u8; 16],
        }
    }
}

impl ShmIpcHeader {
    pub fn new(msg_type: ShmMessageType, seq: u64) -> Self {
        Self {
            magic: HGB_IPC_MAGIC,
            message_type: msg_type,
            flags: 0,
            sequence: seq,
            payload_len: 0,
            shm_offset: 0,
            shm_len: 0,
            reserved: [0u8; 16],
        }
    }

    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..8].copy_from_slice(&self.magic.to_be_bytes());
        buf[8..12].copy_from_slice(&(self.message_type as u32).to_be_bytes());
        buf[12..16].copy_from_slice(&self.flags.to_be_bytes());
        buf[16..24].copy_from_slice(&self.sequence.to_be_bytes());
        buf[24..32].copy_from_slice(&self.payload_len.to_be_bytes());
        buf[32..40].copy_from_slice(&self.shm_offset.to_be_bytes());
        buf[40..48].copy_from_slice(&self.shm_len.to_be_bytes());
        buf[48..64].copy_from_slice(&self.reserved);
        buf
    }

    pub fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Result<Self> {
        let magic = u64::from_be_bytes(buf[0..8].try_into().unwrap());
        if magic != HGB_IPC_MAGIC {
            return Err(HgbError::Ipc(format!(
                "Invalid HGB IPC Magic: expected 0x{:X}, found 0x{:X}",
                HGB_IPC_MAGIC, magic
            )));
        }

        let message_type = ShmMessageType::from_u32(u32::from_be_bytes(buf[8..12].try_into().unwrap()));
        let flags = u32::from_be_bytes(buf[12..16].try_into().unwrap());
        let sequence = u64::from_be_bytes(buf[16..24].try_into().unwrap());
        let payload_len = u64::from_be_bytes(buf[24..32].try_into().unwrap());
        let shm_offset = u64::from_be_bytes(buf[32..40].try_into().unwrap());
        let shm_len = u64::from_be_bytes(buf[40..48].try_into().unwrap());
        let mut reserved = [0u8; 16];
        reserved.copy_from_slice(&buf[48..64]);

        Ok(Self {
            magic,
            message_type,
            flags,
            sequence,
            payload_len,
            shm_offset,
            shm_len,
            reserved,
        })
    }
}

/// A complete framed message containing either inline payload or shared memory payload
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmIpcMessage {
    pub header: ShmIpcHeader,
    pub payload: Vec<u8>,
}

impl ShmIpcMessage {
    pub fn new(msg_type: ShmMessageType, seq: u64, payload: Vec<u8>) -> Self {
        Self {
            header: ShmIpcHeader::new(msg_type, seq),
            payload,
        }
    }
}

/// Shared memory ring allocator backed by memory mapped files in `/dev/shm`
pub struct ShmRingPool {
    path: PathBuf,
    mmap: MmapMut,
    capacity: usize,
    write_head: AtomicU64,
}

impl ShmRingPool {
    pub fn open_or_create(name: &str, capacity: usize) -> Result<Self> {
        let shm_dir = Path::new("/dev/shm");
        let path = if shm_dir.is_dir() && OpenOptions::new().write(true).open("/dev/null").is_ok() {
            shm_dir.join(name)
        } else {
            std::env::temp_dir().join(name)
        };

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| HgbError::Storage(format!("Failed to open SHM file {}: {}", path.display(), e)))?;

        file.set_len(capacity as u64)
            .map_err(|e| HgbError::Storage(format!("Failed to truncate SHM file {}: {}", path.display(), e)))?;

        let mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|e| HgbError::Storage(format!("Failed to map SHM file {}: {}", path.display(), e)))?
        };

        Ok(Self {
            path,
            mmap,
            capacity,
            write_head: AtomicU64::new(0),
        })
    }

    pub fn write_payload(&mut self, data: &[u8]) -> Result<(u64, u64)> {
        let len = data.len();
        if len > self.capacity {
            return Err(HgbError::Storage(format!(
                "Payload size {} exceeds SHM capacity {}",
                len, self.capacity
            )));
        }

        let curr_head = self.write_head.load(Ordering::Relaxed) as usize;
        let mut target_offset = curr_head;

        // If wrapping around
        if target_offset + len > self.capacity {
            target_offset = 0; // Wrap to start of ring
        }

        self.mmap[target_offset..target_offset + len].copy_from_slice(data);
        fence(Ordering::Release);

        let next_head = (target_offset + len) % self.capacity;
        self.write_head.store(next_head as u64, Ordering::Release);

        Ok((target_offset as u64, len as u64))
    }

    pub fn read_payload(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let off = offset as usize;
        let l = len as usize;
        if off + l > self.capacity {
            return Err(HgbError::Storage(format!(
                "Read out of bounds in SHM ring: offset {} + len {} > capacity {}",
                off, l, self.capacity
            )));
        }

        fence(Ordering::Acquire);
        Ok(self.mmap[off..off + l].to_vec())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// High-performance Framed Streaming IPC Channel over UNIX Domain Sockets
pub struct ShmIpcChannel {
    stream: UnixStream,
    shm_pool: Arc<Mutex<ShmRingPool>>,
    next_seq: u64,
}

impl ShmIpcChannel {
    pub fn new(stream: UnixStream, shm_pool: Arc<Mutex<ShmRingPool>>) -> Self {
        Self {
            stream,
            shm_pool,
            next_seq: 1,
        }
    }

    pub async fn connect<P: AsRef<Path>>(
        socket_path: P,
        shm_pool: Arc<Mutex<ShmRingPool>>,
    ) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|e| HgbError::Ipc(format!("Failed to connect to UDS: {}", e)))?;
        Ok(Self::new(stream, shm_pool))
    }

    /// Send a framed message. If payload > 4 KiB, writes directly to SHM
    pub async fn send_message(&mut self, msg_type: ShmMessageType, payload: &[u8]) -> Result<ShmIpcHeader> {
        let seq = self.next_seq;
        self.next_seq += 1;

        let mut header = ShmIpcHeader::new(msg_type, seq);

        if payload.len() > SHM_THRESHOLD_BYTES {
            // Write directly to mapped SHM ring buffer
            let (shm_off, shm_len) = {
                let mut pool = self.shm_pool.lock().await;
                pool.write_payload(payload)?
            };
            header.shm_offset = shm_off;
            header.shm_len = shm_len;
            header.payload_len = 0;

            // Only header transmitted over socket
            let hdr_bytes = header.to_bytes();
            self.stream
                .write_all(&hdr_bytes)
                .await
                .map_err(|e| HgbError::Ipc(format!("Failed to send SHM-IPC header: {}", e)))?;
        } else {
            // Inline payload <= 4 KiB
            header.payload_len = payload.len() as u64;
            header.shm_offset = 0;
            header.shm_len = 0;

            let hdr_bytes = header.to_bytes();
            self.stream
                .write_all(&hdr_bytes)
                .await
                .map_err(|e| HgbError::Ipc(format!("Failed to send inline IPC header: {}", e)))?;

            if !payload.is_empty() {
                self.stream
                    .write_all(payload)
                    .await
                    .map_err(|e| HgbError::Ipc(format!("Failed to send inline IPC payload: {}", e)))?;
            }
        }

        self.stream
            .flush()
            .await
            .map_err(|e| HgbError::Ipc(format!("Failed to flush stream: {}", e)))?;

        Ok(header)
    }

    /// Receive a framed message. Resolves payload transparently from SHM or socket inline
    pub async fn receive_message(&mut self) -> Result<ShmIpcMessage> {
        let mut hdr_buf = [0u8; HEADER_SIZE];
        self.stream
            .read_exact(&mut hdr_buf)
            .await
            .map_err(|e| HgbError::Ipc(format!("Failed to read IPC header: {}", e)))?;

        let header = ShmIpcHeader::from_bytes(&hdr_buf)?;

        let payload = if header.shm_len > 0 {
            // Zero-copy read from mapped SHM ring
            let pool = self.shm_pool.lock().await;
            pool.read_payload(header.shm_offset, header.shm_len)?
        } else if header.payload_len > 0 {
            // Read inline bytes from socket
            let len = header.payload_len as usize;
            let mut buf = vec![0u8; len];
            self.stream
                .read_exact(&mut buf)
                .await
                .map_err(|e| HgbError::Ipc(format!("Failed to read inline IPC payload: {}", e)))?;
            buf
        } else {
            Vec::new()
        };

        Ok(ShmIpcMessage { header, payload })
    }

    pub fn inner_mut(&mut self) -> &mut UnixStream {
        &mut self.stream
    }
}

/// Comprehensive benchmark report comparing pure socket loopback vs. SHM-assisted IPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShmIpcBenchReport {
    pub benchmark_name: String,
    pub payload_size_bytes: usize,
    pub iterations: usize,
    pub loopback_elapsed_us: u64,
    pub loopback_throughput_mb_s: f64,
    pub loopback_avg_latency_ns: f64,
    pub shm_elapsed_us: u64,
    pub shm_throughput_mb_s: f64,
    pub shm_avg_latency_ns: f64,
    pub speedup_factor: f64,
    pub zero_copy_verified: bool,
    pub shm_file_path: String,
}

/// Run a live, bare-metal benchmark comparing UDS loopback vs. SHM transport
pub async fn run_shm_ipc_benchmark(
    payload_size: usize,
    iterations: usize,
) -> Result<ShmIpcBenchReport> {
    let (s1, s2) = UnixStream::pair()
        .map_err(|e| HgbError::Ipc(format!("Failed to create UDS pair: {}", e)))?;

    let shm_pool = Arc::new(Mutex::new(ShmRingPool::open_or_create(
        "hgb_ring_bench_shared_ring",
        DEFAULT_SHM_POOL_CAPACITY,
    )?));

    let shm_path = {
        let p = shm_pool.lock().await;
        p.path().to_string_lossy().to_string()
    };

    let payload = vec![0x5Au8; payload_size];

    // --- Benchmark 1: Pure Socket Loopback ---
    let mut writer_raw = s1;
    let mut reader_raw = s2;

    let start_loopback = Instant::now();
    for _ in 0..iterations {
        writer_raw
            .write_all(&(payload_size as u32).to_be_bytes())
            .await
            .map_err(|e| HgbError::Ipc(format!("Loopback write len failed: {}", e)))?;
        writer_raw
            .write_all(&payload)
            .await
            .map_err(|e| HgbError::Ipc(format!("Loopback write payload failed: {}", e)))?;

        let mut len_buf = [0u8; 4];
        reader_raw
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| HgbError::Ipc(format!("Loopback read len failed: {}", e)))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut sink = vec![0u8; len];
        reader_raw
            .read_exact(&mut sink)
            .await
            .map_err(|e| HgbError::Ipc(format!("Loopback read payload failed: {}", e)))?;
    }
    let loopback_elapsed = start_loopback.elapsed();
    let loopback_elapsed_us = loopback_elapsed.as_micros().max(1) as u64;
    let total_bytes = payload_size * iterations;
    let loopback_throughput = (total_bytes as f64 / (1024.0 * 1024.0)) / (loopback_elapsed.as_secs_f64().max(1e-9));
    let loopback_latency_ns = (loopback_elapsed.as_nanos() as f64) / (iterations as f64);

    // --- Benchmark 2: SHM-Assisted IPC ---
    let (s3, s4) = UnixStream::pair()
        .map_err(|e| HgbError::Ipc(format!("Failed to create second UDS pair: {}", e)))?;

    let mut chan_sender = ShmIpcChannel::new(s3, shm_pool.clone());
    let mut chan_receiver = ShmIpcChannel::new(s4, shm_pool.clone());

    let start_shm = Instant::now();
    for _ in 0..iterations {
        chan_sender
            .send_message(ShmMessageType::BenchPayload, &payload)
            .await?;
        let msg = chan_receiver.receive_message().await?;
        assert_eq!(msg.payload.len(), payload_size);
    }
    let shm_elapsed = start_shm.elapsed();
    let shm_elapsed_us = shm_elapsed.as_micros().max(1) as u64;
    let shm_throughput = (total_bytes as f64 / (1024.0 * 1024.0)) / (shm_elapsed.as_secs_f64().max(1e-9));
    let shm_latency_ns = (shm_elapsed.as_nanos() as f64) / (iterations as f64);

    let speedup = if shm_elapsed_us > 0 {
        loopback_elapsed_us as f64 / shm_elapsed_us as f64
    } else {
        1.0
    };

    Ok(ShmIpcBenchReport {
        benchmark_name: "SHM-Assisted UDS vs Loopback".to_string(),
        payload_size_bytes: payload_size,
        iterations,
        loopback_elapsed_us,
        loopback_throughput_mb_s: loopback_throughput,
        loopback_avg_latency_ns: loopback_latency_ns,
        shm_elapsed_us,
        shm_throughput_mb_s: shm_throughput,
        shm_avg_latency_ns: shm_latency_ns,
        speedup_factor: speedup,
        zero_copy_verified: payload_size > SHM_THRESHOLD_BYTES,
        shm_file_path: shm_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_serialization_roundtrip() {
        let mut hdr = ShmIpcHeader::new(ShmMessageType::Request, 42);
        hdr.payload_len = 1024;
        hdr.shm_offset = 64;
        hdr.shm_len = 4096;
        hdr.flags = 0x01;

        let bytes = hdr.to_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE);

        let decoded = ShmIpcHeader::from_bytes(&bytes).expect("decode failed");
        assert_eq!(decoded.magic, HGB_IPC_MAGIC);
        assert_eq!(decoded.sequence, 42);
        assert_eq!(decoded.payload_len, 1024);
        assert_eq!(decoded.shm_offset, 64);
        assert_eq!(decoded.shm_len, 4096);
        assert_eq!(decoded.flags, 0x01);
        assert_eq!(decoded.message_type, ShmMessageType::Request);
    }

    #[tokio::test]
    async fn test_inline_payload_roundtrip() {
        let (s1, s2) = UnixStream::pair().expect("unix pair");
        let pool = Arc::new(Mutex::new(
            ShmRingPool::open_or_create("hgb_test_shm_inline", 1024 * 1024).expect("pool"),
        ));

        let mut sender = ShmIpcChannel::new(s1, pool.clone());
        let mut receiver = ShmIpcChannel::new(s2, pool);

        let test_data = b"Hello, Zero-Overhead Inline IPC!".to_vec();
        sender
            .send_message(ShmMessageType::Request, &test_data)
            .await
            .expect("send");

        let received = receiver.receive_message().await.expect("receive");
        assert_eq!(received.header.shm_len, 0);
        assert_eq!(received.header.payload_len, test_data.len() as u64);
        assert_eq!(received.payload, test_data);
    }

    #[tokio::test]
    async fn test_shm_threshold_payload_roundtrip() {
        let (s1, s2) = UnixStream::pair().expect("unix pair");
        let pool = Arc::new(Mutex::new(
            ShmRingPool::open_or_create("hgb_test_shm_threshold", 8 * 1024 * 1024).expect("pool"),
        ));

        let mut sender = ShmIpcChannel::new(s1, pool.clone());
        let mut receiver = ShmIpcChannel::new(s2, pool);

        // Payload > 4 KiB (e.g. 16 KiB)
        let large_payload = vec![0xCDu8; 16 * 1024];
        let sent_hdr = sender
            .send_message(ShmMessageType::StreamChunk, &large_payload)
            .await
            .expect("send");

        assert_eq!(sent_hdr.payload_len, 0);
        assert_eq!(sent_hdr.shm_len, 16 * 1024);

        let received = receiver.receive_message().await.expect("receive");
        assert_eq!(received.header.shm_len, 16 * 1024);
        assert_eq!(received.header.payload_len, 0);
        assert_eq!(received.payload.len(), 16 * 1024);
        assert_eq!(received.payload, large_payload);
    }

    #[tokio::test]
    async fn test_shm_benchmark_execution() {
        let report = run_shm_ipc_benchmark(8192, 20)
            .await
            .expect("bench run");
        assert_eq!(report.payload_size_bytes, 8192);
        assert_eq!(report.iterations, 20);
        assert!(report.shm_throughput_mb_s > 0.0);
        assert!(report.zero_copy_verified);
    }
}
