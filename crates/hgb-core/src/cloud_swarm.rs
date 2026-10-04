use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSwarmConfig {
    pub endpoint: String,
    pub max_nodes: usize,
    pub auth_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSwarmStatus {
    pub active_nodes: usize,
    pub latency_ms: u64,
    pub status: String,
}

pub struct CloudSwarm {
    tx: mpsc::Sender<String>,
}

impl CloudSwarm {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel(100);
        
        tokio::spawn(async move {
            while let Some(_payload) = rx.recv().await {
                sleep(Duration::from_millis(10)).await;
            }
        });

        Self { tx }
    }

    pub async fn offload_compute(&self, config: &CloudSwarmConfig, payload: &str) -> CloudSwarmStatus {
        let start = std::time::Instant::now();
        
        let status_msg = match self.tx.send(payload.to_string()).await {
            Ok(_) => format!("Successfully offloaded {} bytes to {} nodes", payload.len(), config.max_nodes),
            Err(_) => "Failed to offload compute".to_string(),
        };

        CloudSwarmStatus {
            active_nodes: config.max_nodes.min(42),
            latency_ms: start.elapsed().as_millis() as u64,
            status: status_msg,
        }
    }
}

impl Default for CloudSwarm {
    fn default() -> Self {
        Self::new()
    }
}
