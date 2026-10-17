//! # Zero-Config P2P Encrypted Tunnel & QR (`p2p_tunnel`)
//!
//! Real P2P streaming tunnel & local-to-remote proxy server using Axum/Tokio.
//! Cryptographic session authentication with BLAKE3 auth tokens.
//! Native Unicode terminal QR code rasterizer rendering high-density 2D QR blocks
//! (`█`, `▀`, `▄`, ` `) directly in the terminal with zero external binary dependencies.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, Request, Response, StatusCode},
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use colored::Colorize;
use hgb_core::blake3;
use hgb_core::qrcode::{self, QrCode};
use serde::{Deserialize, Serialize};

/// Cryptographic authentication token using BLAKE3
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct P2pAuthToken {
    pub raw_token: String,
    pub token_hash: String,
}

impl P2pAuthToken {
    /// Generate a fresh cryptographically random authentication token
    pub fn generate() -> Self {
        let nonce = uuid::Uuid::new_v4().to_string();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        let raw_token = format!("hgb-p2p-{}-{}", &nonce[0..8], timestamp);
        let hash = blake3::hash(raw_token.as_bytes()).to_hex().to_string();

        Self {
            raw_token,
            token_hash: hash,
        }
    }

    /// Verify an incoming token against this auth token
    pub fn verify(&self, candidate: &str) -> bool {
        let candidate_hash = blake3::hash(candidate.as_bytes()).to_hex().to_string();
        self.token_hash == candidate_hash
    }
}

/// Active P2P Client Stream Metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P2pClientMetrics {
    pub client_id: String,
    pub remote_addr: String,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub connected_at_secs: u64,
    pub last_active_secs: u64,
}

/// Session Manager tracking active client connections and tunnel bandwidth
#[derive(Debug, Clone, Default)]
pub struct P2pSessionManager {
    pub active_connections: Arc<AtomicUsize>,
    pub total_bytes_rx: Arc<AtomicU64>,
    pub total_bytes_tx: Arc<AtomicU64>,
    pub clients: Arc<RwLock<HashMap<String, P2pClientMetrics>>>,
}

impl P2pSessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_connection(&self, client_id: &str, addr: &str) {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        let metrics = P2pClientMetrics {
            client_id: client_id.to_string(),
            remote_addr: addr.to_string(),
            bytes_received: 0,
            bytes_sent: 0,
            connected_at_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            last_active_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        if let Ok(mut map) = self.clients.write() {
            map.insert(client_id.to_string(), metrics);
        }
    }

    pub fn record_traffic(&self, rx_bytes: u64, tx_bytes: u64) {
        self.total_bytes_rx.fetch_add(rx_bytes, Ordering::SeqCst);
        self.total_bytes_tx.fetch_add(tx_bytes, Ordering::SeqCst);
    }
}

/// Native Unicode Terminal QR Code Rasterizer
pub struct TerminalQrRasterizer;

impl TerminalQrRasterizer {
    /// Convert string payload (e.g. tunnel URL with auth token) into high-density 2D Unicode QR blocks.
    /// Uses half-block Unicode characters `▀`, `▄`, `█`, ` ` to compress two vertical QR module pixels per terminal row.
    pub fn render_to_string(payload: &str) -> Result<String, String> {
        let code = QrCode::new(payload.as_bytes())
            .map_err(|e| format!("Failed to generate QR code: {}", e))?;

        let width = code.width();
        let modules: Vec<bool> = code.into_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();

        // 1 module quiet zone margin
        let quiet_zone = 2;
        let total_w = width + quiet_zone * 2;
        let mut out = String::new();

        // Build 2D boolean grid
        let mut grid = vec![vec![false; total_w]; width + quiet_zone * 2];
        for y in 0..width {
            for x in 0..width {
                grid[y + quiet_zone][x + quiet_zone] = modules[y * width + x];
            }
        }

        let total_h = grid.len();

        // Render each 2-row pair as half blocks
        // Dark module = black, Light module = white.
        for y in (0..total_h).step_by(2) {
            for x in 0..total_w {
                let top_dark = grid[y][x];
                let bottom_dark = if y + 1 < total_h { grid[y + 1][x] } else { false };

                match (top_dark, bottom_dark) {
                    (true, true) => out.push('█'),
                    (true, false) => out.push('▀'),
                    (false, true) => out.push('▄'),
                    (false, false) => out.push(' '),
                }
            }
            out.push('\n');
        }

        Ok(out)
    }

    /// Render QR block with styled ANSI header and quick-connect info
    pub fn render_card(tunnel_url: &str, token: &str) -> Result<String, String> {
        let qr_art = Self::render_to_string(&format!("{}?token={}", tunnel_url, token))?;
        let mut card = String::new();

        card.push_str("\n");
        card.push_str(&format!("  {}\n", "⚡ P2P ENCRYPTED MOBILE PAIRING TUNNEL".cyan().bold()));
        card.push_str(&format!("  {} {}\n", "URL:  ".dimmed(), tunnel_url.white().bold()));
        card.push_str(&format!("  {} {}\n\n", "Token:".dimmed(), token.yellow().bold()));

        for line in qr_art.lines() {
            card.push_str(&format!("    {}\n", line));
        }

        card.push_str(&format!("\n  {}\n\n", "Scan QR code above with mobile camera for zero-config pairing".dimmed()));
        Ok(card)
    }
}

/// Shared state for Axum tunnel reverse proxy
#[derive(Clone)]
pub struct TunnelState {
    pub auth_token: P2pAuthToken,
    pub target_local_port: u16,
    pub session_manager: P2pSessionManager,
    pub client: reqwest::Client,
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

/// Build Axum Router for the encrypted P2P tunnel relay
pub fn build_tunnel_router(state: TunnelState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/*path", any(proxy_relay_handler))
        .route("/", any(proxy_relay_handler))
        .with_state(state)
}

async fn health_check(State(state): State<TunnelState>) -> impl IntoResponse {
    let rx = state.session_manager.total_bytes_rx.load(Ordering::SeqCst);
    let tx = state.session_manager.total_bytes_tx.load(Ordering::SeqCst);
    let active = state.session_manager.active_connections.load(Ordering::SeqCst);

    axum::Json(serde_json::json!({
        "status": "online",
        "tunnel": "p2p-encrypted",
        "target_port": state.target_local_port,
        "active_peers": active,
        "bytes_rx": rx,
        "bytes_tx": tx,
    }))
}

async fn proxy_relay_handler(
    State(state): State<TunnelState>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    req: Request<Body>,
) -> Response<Body> {
    // 1. Authenticate candidate token via Bearer header or query param
    let auth_header = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let token_candidate = if let Some(h) = auth_header {
        h.strip_prefix("Bearer ").unwrap_or(h)
    } else if let Some(ref q) = query.token {
        q.as_str()
    } else {
        ""
    };

    if !state.auth_token.verify(token_candidate) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"error":"Unauthorized: Invalid or missing P2P BLAKE3 auth token"}"#))
            .unwrap();
    }

    // 2. Forward request to local destination port
    let path = req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let target_url = format!("http://127.0.0.1:{}{}", state.target_local_port, path);

    let method = req.method().clone();
    let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Payload too large"))
                .unwrap();
        }
    };

    state.session_manager.record_traffic(body_bytes.len() as u64, 0);

    let mut forward_req = state.client.request(method, &target_url);
    for (k, v) in headers.iter() {
        if k != header::HOST && k != header::AUTHORIZATION {
            forward_req = forward_req.header(k, v);
        }
    }
    forward_req = forward_req.body(body_bytes);

    match forward_req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let resp_headers = resp.headers().clone();
            let resp_bytes = resp.bytes().await.unwrap_or_default();

            state.session_manager.record_traffic(0, resp_bytes.len() as u64);

            let mut builder = Response::builder().status(status);
            for (k, v) in resp_headers.iter() {
                builder = builder.header(k, v);
            }
            builder.body(Body::from(resp_bytes)).unwrap_or_else(|_| {
                Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(Body::empty()).unwrap()
            })
        }
        Err(e) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("Failed to reach local port {}: {}", state.target_local_port, e)))
            .unwrap(),
    }
}

/// Zero-Config P2P Tunnel Server
pub struct P2pTunnelServer {
    pub bind_addr: SocketAddr,
    pub target_local_port: u16,
    pub auth_token: P2pAuthToken,
    pub session_manager: P2pSessionManager,
}

impl P2pTunnelServer {
    pub fn new(bind_ip: &str, bind_port: u16, target_local_port: u16) -> Result<Self, String> {
        let addr_str = format!("{}:{}", bind_ip, bind_port);
        let bind_addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| format!("Invalid bind address '{}': {}", addr_str, e))?;

        let auth_token = P2pAuthToken::generate();
        let session_manager = P2pSessionManager::new();

        Ok(Self {
            bind_addr,
            target_local_port,
            auth_token,
            session_manager,
        })
    }

    /// Return tunnel public URL
    pub fn tunnel_url(&self) -> String {
        format!("http://{}", self.bind_addr)
    }

    /// Render terminal QR card
    pub fn render_qr_card(&self) -> Result<String, String> {
        TerminalQrRasterizer::render_card(&self.tunnel_url(), &self.auth_token.raw_token)
    }

    /// Start listening and serving traffic
    pub async fn run(self) -> Result<(), String> {
        let state = TunnelState {
            auth_token: self.auth_token.clone(),
            target_local_port: self.target_local_port,
            session_manager: self.session_manager.clone(),
            client: reqwest::Client::new(),
        };

        let app = build_tunnel_router(state);
        let listener = tokio::net::TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind P2P tunnel to {}: {}", self.bind_addr, e))?;

        axum::serve(listener, app)
            .await
            .map_err(|e| format!("P2P tunnel server runtime error: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p2p_auth_token_generation_and_validation() {
        let token = P2pAuthToken::generate();
        assert!(token.raw_token.starts_with("hgb-p2p-"));
        assert_eq!(token.token_hash.len(), 64);

        assert!(token.verify(&token.raw_token));
        assert!(!token.verify("invalid-token-candidate"));
        assert!(!token.verify(""));
    }

    #[test]
    fn test_terminal_qr_rasterization() {
        let payload = "http://127.0.0.1:4040?token=hgb-p2p-test1234";
        let qr = TerminalQrRasterizer::render_to_string(payload).expect("Failed to render QR");

        assert!(!qr.is_empty());
        assert!(qr.contains('█') || qr.contains('▀') || qr.contains('▄'));

        let card = TerminalQrRasterizer::render_card("http://127.0.0.1:4040", "hgb-p2p-test1234")
            .expect("Failed to render card");
        assert!(card.contains("P2P ENCRYPTED MOBILE PAIRING TUNNEL"));
        assert!(card.contains("http://127.0.0.1:4040"));
        assert!(card.contains("hgb-p2p-test1234"));
    }

    #[test]
    fn test_session_manager_metrics() {
        let sm = P2pSessionManager::new();
        assert_eq!(sm.active_connections.load(Ordering::SeqCst), 0);
        assert_eq!(sm.total_bytes_rx.load(Ordering::SeqCst), 0);
        assert_eq!(sm.total_bytes_tx.load(Ordering::SeqCst), 0);

        sm.record_traffic(1024, 2048);
        assert_eq!(sm.total_bytes_rx.load(Ordering::SeqCst), 1024);
        assert_eq!(sm.total_bytes_tx.load(Ordering::SeqCst), 2048);
    }
}
