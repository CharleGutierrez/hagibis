//! # Zero-Overhead SHM-gRPC / Streaming IPC Bridge
//!
//! Hosts the framed streaming UDS listener at `/run/user/1000/hgbd_shm.sock` or `/tmp/hgb_shm_ipc.sock`,
//! coordinating zero-copy buffer transfers through `/dev/shm/hgb_ring_daemon_shared_ring`.
//! Bridges ultra-high throughput client streams (tensor embedding pipelines, large AST graphs)
//! directly into Hagibis daemon workers without disk I/O or kernel buffer thrashing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use serde::{Deserialize, Serialize};

use hgb_core::shm_ipc_channel::{
    run_shm_ipc_benchmark, ShmIpcBenchReport, ShmIpcChannel,
    ShmMessageType, ShmRingPool, DEFAULT_SHM_FILE_NAME, DEFAULT_SHM_POOL_CAPACITY,
};
use hgb_core::{HgbError, Result};

pub const DEFAULT_SHM_SOCKET_PATH: &str = "/tmp/hgb_shm_ipc.sock";

/// Telemetry metrics for the active SHM-IPC bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShmBridgeStatus {
    pub socket_path: String,
    pub is_listening: bool,
    pub active_clients: usize,
    pub total_messages: u64,
    pub inline_messages: u64,
    pub shm_messages: u64,
    pub inline_bytes: u64,
    pub shm_bytes: u64,
    pub zero_copy_ratio_pct: f64,
    pub shm_ring_path: String,
    pub shm_capacity_bytes: usize,
}

pub struct ShmGrpcBridge {
    socket_path: PathBuf,
    shm_pool: Arc<Mutex<ShmRingPool>>,
    is_running: Arc<AtomicBool>,
    active_clients: Arc<AtomicUsize>,
    total_messages: Arc<AtomicU64>,
    inline_messages: Arc<AtomicU64>,
    shm_messages: Arc<AtomicU64>,
    inline_bytes: Arc<AtomicU64>,
    shm_bytes: Arc<AtomicU64>,
    listener_handle: Mutex<Option<JoinHandle<()>>>,
}

impl ShmGrpcBridge {
    pub fn new<P: AsRef<Path>>(socket_path: P) -> Result<Self> {
        let pool = ShmRingPool::open_or_create(
            DEFAULT_SHM_FILE_NAME,
            DEFAULT_SHM_POOL_CAPACITY,
        )?;

        Ok(Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            shm_pool: Arc::new(Mutex::new(pool)),
            is_running: Arc::new(AtomicBool::new(false)),
            active_clients: Arc::new(AtomicUsize::new(0)),
            total_messages: Arc::new(AtomicU64::new(0)),
            inline_messages: Arc::new(AtomicU64::new(0)),
            shm_messages: Arc::new(AtomicU64::new(0)),
            inline_bytes: Arc::new(AtomicU64::new(0)),
            shm_bytes: Arc::new(AtomicU64::new(0)),
            listener_handle: Mutex::new(None),
        })
    }

    pub fn default_bridge() -> Result<Self> {
        let sock_path = if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("hgbd_shm.sock")
        } else {
            PathBuf::from(DEFAULT_SHM_SOCKET_PATH)
        };
        Self::new(sock_path)
    }

    pub async fn start(&self) -> Result<()> {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return Ok(()); // Already running
        }

        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(|e| {
            HgbError::Ipc(format!(
                "Failed to bind SHM UDS listener on {}: {}",
                self.socket_path.display(),
                e
            ))
        })?;

        let is_running = self.is_running.clone();
        let active_clients = self.active_clients.clone();
        let total_messages = self.total_messages.clone();
        let inline_messages = self.inline_messages.clone();
        let shm_messages = self.shm_messages.clone();
        let inline_bytes = self.inline_bytes.clone();
        let shm_bytes = self.shm_bytes.clone();
        let pool = self.shm_pool.clone();

        let handle = tokio::spawn(async move {
            while is_running.load(Ordering::Relaxed) {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        active_clients.fetch_add(1, Ordering::Relaxed);
                        let client_active = active_clients.clone();
                        let total_msg = total_messages.clone();
                        let in_msg = inline_messages.clone();
                        let shm_msg = shm_messages.clone();
                        let in_bytes = inline_bytes.clone();
                        let shm_b = shm_bytes.clone();
                        let shm_pool = pool.clone();

                        tokio::spawn(async move {
                            let mut channel = ShmIpcChannel::new(stream, shm_pool);
                            while let Ok(msg) = channel.receive_message().await {
                                total_msg.fetch_add(1, Ordering::Relaxed);

                                if msg.header.shm_len > 0 {
                                    shm_msg.fetch_add(1, Ordering::Relaxed);
                                    shm_b.fetch_add(msg.header.shm_len, Ordering::Relaxed);
                                } else {
                                    in_msg.fetch_add(1, Ordering::Relaxed);
                                    in_bytes.fetch_add(msg.header.payload_len, Ordering::Relaxed);
                                }

                                // Process message and formulate framed response
                                let response_payload = match msg.header.message_type {
                                    ShmMessageType::Heartbeat => b"PONG".to_vec(),
                                    ShmMessageType::BenchPayload => msg.payload.clone(),
                                    _ => {
                                        let mut echo = b"ACK: ".to_vec();
                                        echo.extend_from_slice(&msg.payload[..msg.payload.len().min(64)]);
                                        echo
                                    }
                                };

                                let _ = channel
                                    .send_message(ShmMessageType::Response, &response_payload)
                                    .await;
                            }
                            client_active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(_) => {
                        if !is_running.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
            }
        });

        *self.listener_handle.lock().await = Some(handle);
        Ok(())
    }

    pub async fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.listener_handle.lock().await.take() {
            handle.abort();
        }
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    pub fn status(&self) -> ShmBridgeStatus {
        let (ring_path, capacity) = {
            if let Ok(pool) = self.shm_pool.try_lock() {
                (pool.path().to_string_lossy().to_string(), pool.capacity())
            } else {
                ("busy".to_string(), DEFAULT_SHM_POOL_CAPACITY)
            }
        };

        let in_b = self.inline_bytes.load(Ordering::Relaxed);
        let shm_b = self.shm_bytes.load(Ordering::Relaxed);
        let total_b = in_b + shm_b;
        let zero_copy_pct = if total_b > 0 {
            (shm_b as f64 / total_b as f64) * 100.0
        } else {
            0.0
        };

        ShmBridgeStatus {
            socket_path: self.socket_path.to_string_lossy().to_string(),
            is_listening: self.is_running.load(Ordering::Relaxed),
            active_clients: self.active_clients.load(Ordering::Relaxed),
            total_messages: self.total_messages.load(Ordering::Relaxed),
            inline_messages: self.inline_messages.load(Ordering::Relaxed),
            shm_messages: self.shm_messages.load(Ordering::Relaxed),
            inline_bytes: in_b,
            shm_bytes: shm_b,
            zero_copy_ratio_pct: zero_copy_pct,
            shm_ring_path: ring_path,
            shm_capacity_bytes: capacity,
        }
    }

    pub async fn run_benchmark(&self, chunk_size: usize, iterations: usize) -> Result<ShmIpcBenchReport> {
        run_shm_ipc_benchmark(chunk_size, iterations).await
    }
}

impl Drop for ShmGrpcBridge {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shm_bridge_lifecycle() {
        let sock_path = std::env::temp_dir().join(format!("hgb_test_bridge_{}.sock", std::process::id()));
        let bridge = ShmGrpcBridge::new(&sock_path).expect("create bridge");
        bridge.start().await.expect("start bridge");

        let status = bridge.status();
        assert!(status.is_listening);
        assert_eq!(status.active_clients, 0);

        bridge.stop().await;
        let status_stopped = bridge.status();
        assert!(!status_stopped.is_listening);
    }
}
