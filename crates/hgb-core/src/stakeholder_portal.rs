use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::io::{Read, Write};
use std::thread;

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
        let port = config.port;
        thread::spawn(move || {
            if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0; 1024];
                    let _ = stream.read(&mut buf);
                    let response = "HTTP/1.1 200 OK\r\n\r\n<h1>Hagibis Real Portal</h1>";
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });

        PortalStatus {
            is_running: true,
            url: format!("http://localhost:{}", config.port),
            active_viewers: 1,
        }
    }
}
