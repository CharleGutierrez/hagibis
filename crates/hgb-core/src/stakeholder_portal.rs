use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalConfig {
    pub port: u16,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalStatus {
    pub is_running: bool,
    pub url: String,
    pub active_viewers: usize,
}

pub struct StakeholderPortal;

impl StakeholderPortal {
    pub fn new() -> Self {
        Self
    }

    pub fn serve(&self, config: &PortalConfig) -> PortalStatus {
        PortalStatus {
            is_running: true,
            url: format!("http://localhost:{}", config.port),
            active_viewers: 1,
        }
    }
}
