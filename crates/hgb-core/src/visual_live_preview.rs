//! # Visual Live-Preview Sidecar & Web Canvas
//!
//! Provides an ultra-lightweight, zero-bloat HTTP preview server and live telemetry bridge
//! for Vibe Code Developers. Injects live DOM click-to-code telemetry, hot-reload hooks,
//! and HUD overlays into frontend web apps so developers see visual changes in real-time.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot, RwLock};

/// Configuration for the Visual Live-Preview Sidecar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivePreviewConfig {
    pub workspace_root: PathBuf,
    pub preferred_port: Option<u16>,
    pub proxy_devserver_port: Option<u16>,
    pub enable_dom_teleport: bool,
    pub inject_vibe_hud: bool,
}

impl Default for LivePreviewConfig {
    fn default() -> Self {
        Self {
            workspace_root: PathBuf::from("."),
            preferred_port: None,
            proxy_devserver_port: None,
            enable_dom_teleport: true,
            inject_vibe_hud: true,
        }
    }
}

/// Telemetry event emitted when developer clicks an element in the live preview
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewDomClickEvent {
    pub tag: String,
    pub element_id: Option<String>,
    pub classes: Vec<String>,
    pub text_snippet: Option<String>,
    pub source_hint: Option<String>, // e.g. "src/App.tsx:42"
    pub client_x: f64,
    pub client_y: f64,
    pub timestamp_ms: u64,
}

/// Active handle to the running Visual Live-Preview Sidecar
pub struct LivePreviewHandle {
    port: u16,
    base_url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    reload_tx: broadcast::Sender<String>,
    click_history: Arc<RwLock<Vec<PreviewDomClickEvent>>>,
}

impl LivePreviewHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Trigger hot-reload in all open preview browser tabs
    pub fn trigger_reload(&self, reason: &str) {
        let _ = self.reload_tx.send(reason.to_string());
    }

    /// Retrieve recorded visual DOM click events
    pub async fn get_click_events(&self) -> Vec<PreviewDomClickEvent> {
        self.click_history.read().await.clone()
    }

    /// Stop the preview server
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for LivePreviewHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Embedded Visual Live-Preview Sidecar Engine
pub struct VisualLivePreview;

impl VisualLivePreview {
    /// Injects the Hagibis Vibe HUD, click-to-code listener, and SSE live reload into HTML
    pub fn inject_preview_harness(html: &str, preview_port: u16) -> String {
        let harness_script = format!(
            r#"<script id="hgb-preview-harness">
(() => {{
    // 🪽 Hagibis Vibe Live-Preview Harness
    console.log("%c🪽 Hagibis Live-Preview Active (Port: {})", "color: #00f0ff; font-weight: bold;");

    // 1. Live Reload EventSource
    try {{
        const es = new EventSource('/__hgb_events');
        es.onmessage = (e) => {{
            console.log("🪽 [hgb] Reload trigger:", e.data);
            window.location.reload();
        }};
    }} catch(e) {{}}

    // 2. Click-to-Code DOM Teleportation Wiretap
    document.addEventListener('click', (e) => {{
        if (e.altKey || e.metaKey || e.ctrlKey) {{
            e.preventDefault();
            e.stopPropagation();
            const el = e.target;
            const payload = {{
                tag: el.tagName.toLowerCase(),
                element_id: el.id || null,
                classes: Array.from(el.classList),
                text_snippet: (el.innerText || "").slice(0, 80),
                source_hint: el.getAttribute('data-hgb-source') || null,
                client_x: e.clientX,
                client_y: e.clientY,
                timestamp_ms: Date.now()
            }};
            fetch('/__hgb_click', {{
                method: 'POST',
                headers: {{ 'Content-Type': 'application/json' }},
                body: JSON.stringify(payload)
            }}).catch(() => {{}});
        }}
    }}, true);
}})();
</script>"#,
            preview_port
        );

        if html.contains("</body>") {
            html.replace("</body>", &format!("{}\n</body>", harness_script))
        } else {
            format!("{}\n{}", html, harness_script)
        }
    }

    /// Launch the live preview sidecar server
    pub async fn start(config: LivePreviewConfig) -> Result<LivePreviewHandle> {
        let port_to_bind = config.preferred_port.unwrap_or(0);
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port_to_bind))
            .await
            .map_err(|e| HgbError::Network(format!("Failed to bind live preview listener: {}", e)))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| HgbError::Network(format!("Failed to get local address: {}", e)))?;
        let port = local_addr.port();
        let base_url = format!("http://127.0.0.1:{}", port);

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let (reload_tx, _) = broadcast::channel(32);
        let click_history = Arc::new(RwLock::new(Vec::new()));

        let ws_root = config.workspace_root.clone();
        let reload_tx_cloned = reload_tx.clone();
        let click_history_cloned = click_history.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        if let Ok((mut stream, _)) = accept_res {
                            let ws_root = ws_root.clone();
                            let mut rx_reload = reload_tx_cloned.subscribe();
                            let clicks = click_history_cloned.clone();

                            tokio::spawn(async move {
                                let mut buf = [0u8; 8192];
                                let n = match stream.read(&mut buf).await {
                                    Ok(n) if n > 0 => n,
                                    _ => return,
                                };

                                let req_str = String::from_utf8_lossy(&buf[..n]);
                                let first_line = req_str.lines().next().unwrap_or("");
                                let parts: Vec<&str> = first_line.split_whitespace().collect();

                                if parts.len() < 2 {
                                    return;
                                }

                                let method = parts[0];
                                let path = parts[1];

                                if path == "/__hgb_events" {
                                    // SSE Hot-reload stream
                                    let sse_headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
                                    let _ = stream.write_all(sse_headers.as_bytes()).await;
                                    let _ = stream.write_all(b": connected\n\n").await;

                                    while let Ok(msg) = rx_reload.recv().await {
                                        let data = format!("data: {}\n\n", msg);
                                        if stream.write_all(data.as_bytes()).await.is_err() {
                                            break;
                                        }
                                    }
                                } else if path == "/__hgb_click" && method == "POST" {
                                    // Parse clicked DOM element telemetry
                                    if let Some(body_start) = req_str.find("\r\n\r\n") {
                                        let body = &req_str[body_start + 4..];
                                        if let Ok(click_event) = serde_json::from_str::<PreviewDomClickEvent>(body) {
                                            clicks.write().await.push(click_event);
                                        }
                                    }
                                    let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}";
                                    let _ = stream.write_all(resp.as_bytes()).await;
                                } else {
                                    // Serve static file or fallback preview page
                                    let rel_path = if path == "/" || path.is_empty() {
                                        "index.html"
                                    } else {
                                        path.trim_start_matches('/')
                                    };

                                    let target_file = ws_root.join(rel_path);
                                    let (status_line, content_type, body_bytes) = if target_file.exists() && target_file.is_file() {
                                        let mime = if rel_path.ends_with(".html") {
                                            "text/html; charset=utf-8"
                                        } else if rel_path.ends_with(".js") || rel_path.ends_with(".jsx") || rel_path.ends_with(".ts") || rel_path.ends_with(".tsx") {
                                            "application/javascript"
                                        } else if rel_path.ends_with(".css") {
                                            "text/css"
                                        } else if rel_path.ends_with(".json") {
                                            "application/json"
                                        } else {
                                            "text/plain"
                                        };

                                        let raw = std::fs::read_to_string(&target_file).unwrap_or_default();
                                        let enriched = if rel_path.ends_with(".html") {
                                            VisualLivePreview::inject_preview_harness(&raw, port)
                                        } else {
                                            raw
                                        };
                                        ("HTTP/1.1 200 OK", mime, enriched.into_bytes())
                                    } else {
                                        // Default Vibe Canvas Dashboard fallback
                                        let fallback = format!(
                                            r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>🪽 Hagibis Vibe Canvas</title>
  <style>
    body {{ background: #090d16; color: #e0f2fe; font-family: ui-sans-serif, system-ui, sans-serif; display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; }}
    .card {{ background: rgba(15, 23, 42, 0.85); border: 1px solid rgba(0, 240, 255, 0.3); border-radius: 16px; padding: 32px 48px; text-align: center; box-shadow: 0 8px 32px rgba(0,0,0,0.5); backdrop-filter: blur(12px); max-width: 520px; }}
    h1 {{ color: #00f0ff; margin-bottom: 8px; font-size: 28px; letter-spacing: -0.5px; }}
    p {{ color: #94a3b8; font-size: 15px; line-height: 1.6; }}
    .badge {{ display: inline-block; padding: 4px 12px; background: rgba(0, 240, 255, 0.15); color: #38bdf8; border-radius: 9999px; font-size: 13px; font-weight: 600; margin-bottom: 16px; }}
    .cmd {{ background: #020617; border: 1px solid #1e293b; padding: 8px 16px; border-radius: 8px; font-family: monospace; color: #f43f5e; font-size: 14px; margin-top: 16px; }}
  </style>
</head>
<body>
  <div class="card">
    <div class="badge">🪽 Hagibis Visual Sidecar Live</div>
    <h1>Ready to Vibe</h1>
    <p>Your instant visual canvas is running on port <strong>{}</strong>. Place an <code>index.html</code> in your project or launch your devserver to start visual live-coding.</p>
    <div class="cmd">Alt + Click any element to teleport to code</div>
  </div>
</body>
</html>"#,
                                            port
                                        );
                                        let enriched = VisualLivePreview::inject_preview_harness(&fallback, port);
                                        ("HTTP/1.1 200 OK", "text/html; charset=utf-8", enriched.into_bytes())
                                    };

                                    let resp_header = format!(
                                        "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
                                        status_line,
                                        content_type,
                                        body_bytes.len()
                                    );
                                    let _ = stream.write_all(resp_header.as_bytes()).await;
                                    let _ = stream.write_all(&body_bytes).await;
                                }
                            });
                        }
                    }
                }
            }
        });

        Ok(LivePreviewHandle {
            port,
            base_url,
            shutdown_tx: Some(shutdown_tx),
            reload_tx,
            click_history,
        })
    }
}
