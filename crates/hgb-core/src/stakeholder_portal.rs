use serde::{Deserialize, Serialize};
use std::thread;
use axum::{routing::get, Router, response::Html};

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
        let title = config.title.clone();
        
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
                
            rt.block_on(async {
                let app = Router::new().route("/", get(move || {
                    let title = title.clone();
                    async move {
                        Html(format!("<h1>{}</h1><p>Hagibis Real Portal</p>", title))
                    }
                }));
                
                if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                    let _ = axum::serve(listener, app).await;
                }
            });
        });

        PortalStatus {
            is_running: true,
            url: format!("http://localhost:{}", config.port),
            active_viewers: 1,
        }
    }
}
