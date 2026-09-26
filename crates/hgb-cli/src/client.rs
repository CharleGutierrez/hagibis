use hgb_core::{HgbError, HgbRequest, HgbResponse, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct HgbClient {
    socket_path: PathBuf,
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

    pub async fn send(&self, req: HgbRequest) -> Result<HgbResponse> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| HgbError::Ipc(format!("Failed to connect to hgbd daemon at {:?}: {}. Run 'hgbd' to start the daemon.", self.socket_path, e)))?;

        let encoded = bincode::serialize(&req).map_err(|e| HgbError::Serialization(e.to_string()))?;
        stream.write_all(&encoded).await?;

        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(HgbError::Ipc("Daemon closed connection without response".to_string()));
        }

        let resp: HgbResponse = bincode::deserialize(&buf[..n])
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        Ok(resp)
    }
}
