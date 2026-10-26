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
}
