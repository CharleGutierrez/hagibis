use hgb_core::{HgbError, HgbRequest, HgbResponse, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024; // 64 MB

fn find_hgbd_binary() -> PathBuf {
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join("hgbd");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("hgbd")
}

#[derive(Clone)]
pub struct HgbClient {
    socket_path: PathBuf,
}

impl Default for HgbClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HgbClient {
    pub fn new() -> Self {
        let socket_path = std::env::var("HGB_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(runtime_dir).join("hgb.sock")
            });
        Self { socket_path }
    }

    #[allow(dead_code)]
    pub fn with_socket<P: AsRef<Path>>(socket_path: P) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Establishes a connection to the daemon, auto-spawning `hgbd` in the background if
    /// the socket does not exist or connection is refused.
    pub async fn connect(&self) -> Result<UnixStream> {
        match UnixStream::connect(&self.socket_path).await {
            Ok(stream) => Ok(stream),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::ConnectionRefused => {
                let hgbd_bin = find_hgbd_binary();
                let spawn_res = std::process::Command::new(&hgbd_bin)
                    .env("HGB_SOCKET", &self.socket_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();

                match spawn_res {
                    Ok(_child) => {
                        let start = std::time::Instant::now();
                        let max_wait = std::time::Duration::from_millis(300);
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
                            match UnixStream::connect(&self.socket_path).await {
                                Ok(stream) => return Ok(stream),
                                Err(_) => {
                                    if start.elapsed() >= max_wait {
                                        break;
                                    }
                                }
                            }
                        }
                        Err(HgbError::Ipc(format!(
                            "Spawned hgbd ({:?}) but socket {:?} was not ready within 300ms. Run 'hgbd' manually to inspect logs.",
                            hgbd_bin, self.socket_path
                        )))
                    }
                    Err(spawn_err) => {
                        Err(HgbError::Ipc(format!(
                            "Failed to connect to hgbd daemon at {:?} ({}) and daemon auto-spawn failed ({}). Please run 'hgbd' to start the daemon.",
                            self.socket_path, e, spawn_err
                        )))
                    }
                }
            }
            Err(e) => Err(HgbError::Ipc(format!(
                "Failed to connect to hgbd daemon at {:?}: {}. Run 'hgbd' to start the daemon.",
                self.socket_path, e
            ))),
        }
    }

    /// Fetches speculative diff session directly from active workspace or sandbox
    #[allow(dead_code)]
    pub async fn get_speculative_diff(&self) -> Result<hgb_core::speculative_diff_viewer::SpeculativeDiffSession> {
        let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let sandbox_dir = workspace.join(".hgb").join("sandbox");
        if sandbox_dir.exists() {
            let sb = hgb_core::overlayfs_sandbox::OverlaySandbox::new(&workspace)?;
            hgb_core::speculative_diff_viewer::SpeculativeDiffSession::from_overlay_sandbox(&sb)
        } else {
            Ok(hgb_core::speculative_diff_viewer::SpeculativeDiffSession::from_contents("workspace", "", ""))
        }
    }

    pub async fn send(&self, req: HgbRequest) -> Result<HgbResponse> {
        let req = match req {
            HgbRequest::Enveloped { .. } => req,
            other => other.enveloped(),
        };
        let mut stream = self.connect().await?;

        let encoded = bincode::serialize(&req).map_err(|e| HgbError::Serialization(e.to_string()))?;
        let len = (encoded.len() as u32).to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(&encoded).await?;
        stream.flush().await?;

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await
            .map_err(|e| HgbError::Ipc(format!("Daemon closed connection or failed to send length prefix: {}", e)))?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;

        if resp_len > MAX_PAYLOAD_SIZE {
            return Err(HgbError::Ipc(format!(
                "Daemon response payload exceeds maximum limit of 64MB: {} bytes",
                resp_len
            )));
        }

        let mut buf = vec![0u8; resp_len];
        stream.read_exact(&mut buf).await
            .map_err(|e| HgbError::Ipc(format!("Failed to read full daemon response (expected {} bytes): {}", resp_len, e)))?;

        let resp: HgbResponse = bincode::deserialize(&buf)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        Ok(resp)
    }

    /// Negotiate a zero-copy POSIX shared-memory ring buffer with the daemon
    pub async fn negotiate_shm_ring(
        &self,
        capacity: Option<usize>,
    ) -> Result<hgb_core::shm_ring_channel::ShmRingChannel> {
        let client_pid = std::process::id();
        let resp = self
            .send(HgbRequest::NegotiateShmRingBuffer {
                client_pid,
                buffer_capacity: capacity,
            })
            .await?;

        match resp {
            HgbResponse::ShmRingNegotiated {
                ring_name,
                ring_path,
                capacity: cap,
            } => {
                let path = Path::new(&ring_path);
                hgb_core::shm_ring_channel::ShmRingChannel::open_with_path(path, &ring_name)
                    .or_else(|_| hgb_core::shm_ring_channel::ShmRingChannel::create_with_path(path, &ring_name, cap))
            }
            HgbResponse::Error(e) => Err(HgbError::Ipc(format!("SHM ring negotiation failed: {e}"))),
            other => Err(HgbError::Ipc(format!("Unexpected response for SHM negotiation: {other:?}"))),
        }
    }

    /// Stream LLM tokens from daemon over zero-copy SHM ring buffer
    pub async fn stream_tokens_shm<F>(
        &self,
        prompt: &str,
        model: Option<&str>,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str, bool, u32),
    {
        let mut channel = self.negotiate_shm_ring(None).await?;
        let client_pid = std::process::id();

        let this = self.clone();
        let prompt_str = prompt.to_string();
        let model_opt = model.map(|s| s.to_string());
        let send_handle = tokio::spawn(async move {
            this.send(HgbRequest::StreamTokensToShm {
                client_pid,
                prompt: prompt_str,
                model: model_opt,
            })
            .await
        });

        let mut full_text = String::new();
        let poll_timeout = std::time::Duration::from_millis(1500);

        loop {
            match channel.recv_frame_blocking(poll_timeout)? {
                Some(hgb_core::shm_ring_channel::ShmRingFrame::TokenChunk { text, is_final, index }) => {
                    full_text.push_str(&text);
                    on_token(&text, is_final, index);
                    if is_final {
                        break;
                    }
                }
                Some(_) => {}
                None => {
                    if send_handle.is_finished() {
                        break;
                    }
                }
            }
        }

        let resp = send_handle.await.map_err(|e| HgbError::Ipc(e.to_string()))??;
        match resp {
            HgbResponse::ShmStreamCompleted { .. } => Ok(full_text),
            HgbResponse::Error(e) => Err(HgbError::Ipc(format!("Stream tokens failed: {e}"))),
            _ => Ok(full_text),
        }
    }

    /// Fetch hardware & daemon telemetry over zero-copy SHM ring buffer
    pub async fn stream_telemetry_shm(&self) -> Result<serde_json::Value> {
        let mut channel = self.negotiate_shm_ring(None).await?;
        let client_pid = std::process::id();

        let resp = self
            .send(HgbRequest::StreamTelemetryToShm { client_pid })
            .await?;

        match resp {
            HgbResponse::ShmStreamCompleted { .. } => {
                let frame = channel.recv_frame_blocking(std::time::Duration::from_millis(1000))?;
                match frame {
                    Some(hgb_core::shm_ring_channel::ShmRingFrame::Telemetry(val)) => Ok(val),
                    other => Err(HgbError::Ipc(format!("Expected Telemetry frame, got {other:?}"))),
                }
            }
            HgbResponse::Error(e) => Err(HgbError::Ipc(format!("Stream telemetry failed: {e}"))),
            other => Err(HgbError::Ipc(format!("Unexpected response: {other:?}"))),
        }
    }

    /// Fetch swarm federation status
    pub async fn swarm_status(&self) -> Result<hgb_core::p2p_swarm_federation::SwarmFederationStatusReport> {
        let resp = self.send(HgbRequest::SwarmStatus).await?;
        match resp {
            HgbResponse::SwarmStatus(rep) => Ok(rep),
            HgbResponse::Error(e) => Err(HgbError::Ipc(e)),
            other => Err(HgbError::Ipc(format!("Unexpected response for SwarmStatus: {other:?}"))),
        }
    }

    /// List active peers discovered by swarm federation
    pub async fn swarm_peers(&self) -> Result<Vec<hgb_core::p2p_swarm_federation::WorkerNodeAdvertisement>> {
        let resp = self.send(HgbRequest::SwarmListPeers).await?;
        match resp {
            HgbResponse::SwarmPeers(peers) => Ok(peers),
            HgbResponse::Error(e) => Err(HgbError::Ipc(e)),
            other => Err(HgbError::Ipc(format!("Unexpected response for SwarmListPeers: {other:?}"))),
        }
    }

    /// Dispatch task to the federated swarm
    pub async fn swarm_dispatch(
        &self,
        title: &str,
        kind: &str,
        code: &str,
    ) -> Result<hgb_core::p2p_swarm_federation::SwarmFederationResult> {
        let resp = self
            .send(HgbRequest::SwarmDispatchTask {
                title: title.to_string(),
                task_kind: kind.to_string(),
                code: code.to_string(),
            })
            .await?;
        match resp {
            HgbResponse::SwarmTaskResult(res) => Ok(res),
            HgbResponse::Error(e) => Err(HgbError::Ipc(e)),
            other => Err(HgbError::Ipc(format!("Unexpected response for SwarmDispatchTask: {other:?}"))),
        }
    }

    /// Fetch Autonomous Watchdog status from daemon
    pub async fn watchdog_status(&self) -> Result<hgb_core::autonomous_watchdog::WatchdogStatusReport> {
        let resp = self.send(HgbRequest::WatchdogStatus).await?;
        match resp {
            HgbResponse::WatchdogStatus(rep) => Ok(rep),
            HgbResponse::Error(e) => Err(HgbError::Ipc(e)),
            other => Err(HgbError::Ipc(format!("Unexpected response for WatchdogStatus: {other:?}"))),
        }
    }

    /// Fetch agent snapshots from SQLite WAL
    pub async fn watchdog_snapshots(&self, cycle_id: Option<&str>) -> Result<Vec<hgb_core::autonomous_watchdog::AgentContextSnapshot>> {
        let resp = self.send(HgbRequest::WatchdogSnapshots {
            cycle_id: cycle_id.map(|s| s.to_string()),
        }).await?;
        match resp {
            HgbResponse::WatchdogSnapshots(snaps) => Ok(snaps),
            HgbResponse::Error(e) => Err(HgbError::Ipc(e)),
            other => Err(HgbError::Ipc(format!("Unexpected response for WatchdogSnapshots: {other:?}"))),
        }
    }

    /// Trigger autonomous healing cycle via daemon
    pub async fn watchdog_heal(&self, target_crate: Option<&str>, max_retries: Option<usize>) -> Result<serde_json::Value> {
        let resp = self.send(HgbRequest::AutonomousHealTrigger {
            target_crate: target_crate.map(|s| s.to_string()),
            max_retries,
        }).await?;
        match resp {
            HgbResponse::AutonomousHealResult(val) => Ok(val),
            HgbResponse::Error(e) => Err(HgbError::Ipc(e)),
            other => Err(HgbError::Ipc(format!("Unexpected response for AutonomousHealTrigger: {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_client_negotiate_and_stream_shm() {
        let temp_dir = tempdir().expect("tempdir");
        let sock_path = temp_dir.path().join("test_hgb.sock");
        let listener = UnixListener::bind(&sock_path).expect("bind unix socket");

        let pid = std::process::id();
        let ring_name = format!("test_ring_cli_{}", pid);
        let ring_path = hgb_core::shm_ring_buffer::resolve_shm_path(&ring_name);

        let cap = 512 * 1024;
        let mut producer = hgb_core::shm_ring_buffer::ShmRingProducer::create_with_path(&ring_path, &ring_name, cap)
            .expect("create ring producer");
        producer.set_unlink_on_drop(true);

        let ring_path_str = ring_path.to_string_lossy().to_string();
        let ring_name_clone = ring_name.clone();

        let srv_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.expect("read len");
            let req_len = u32::from_be_bytes(len_buf) as usize;
            let mut req_buf = vec![0u8; req_len];
            stream.read_exact(&mut req_buf).await.expect("read req");

            let req: HgbRequest = bincode::deserialize(&req_buf).expect("deserialize req");
            let unwrapped = match req {
                HgbRequest::Enveloped { inner, .. } => *inner,
                other => other,
            };

            let resp = match unwrapped {
                HgbRequest::NegotiateShmRingBuffer { .. } => HgbResponse::ShmRingNegotiated {
                    ring_name: ring_name_clone,
                    ring_path: ring_path_str,
                    capacity: cap,
                },
                _ => HgbResponse::Error("unexpected request".to_string()),
            };

            let resp_bytes = bincode::serialize(&resp).expect("serialize resp");
            let rlen = (resp_bytes.len() as u32).to_be_bytes();
            stream.write_all(&rlen).await.expect("write rlen");
            stream.write_all(&resp_bytes).await.expect("write resp");
            stream.flush().await.expect("flush");
        });

        let client = HgbClient::with_socket(&sock_path);
        let mut channel = client.negotiate_shm_ring(Some(cap)).await.expect("negotiate shm");

        let mut chan_prod = hgb_core::shm_ring_channel::ShmRingChannelProducer::new(producer);
        chan_prod.send_token("Hello from daemon zero-copy!", true, 0).expect("send token");

        let token = channel.recv_token().expect("recv token");
        assert_eq!(token, Some(("Hello from daemon zero-copy!".to_string(), true, 0)));

        srv_handle.await.expect("server handle");
    }
}
