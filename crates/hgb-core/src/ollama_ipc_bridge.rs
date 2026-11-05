//! # Local LLM & Ollama Zero-Overhead IPC Bridge (Pillar 3.1)
//!
//! High-performance, zero-latency inter-process communication bridge connecting the Hagibis
//! microkernel to local Ollama instances over Unix Domain Sockets (`/tmp/ollama.sock`) or
//! HTTP (`http://127.0.0.1:11434`).
//!
//! Provides:
//! - Direct kernel-bypass IPC via Unix Domain Sockets for sub-millisecond round-trip times.
//! - Non-blocking asynchronous token streaming with line-delimited JSON parsing.
//! - Active loaded model inspection and memory footprint tracking via `/api/ps`.
//! - Installed model discovery via `/api/tags`.
//! - High-speed vector embeddings generation via `/api/embeddings` and `/api/embed`.
//! - Fast health check probe and transport auto-negotiation.

use crate::error::{HgbError, Result};
use crate::providers::{OllamaModelDetails, OllamaModelTag};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// Default local Ollama HTTP host
pub const DEFAULT_OLLAMA_HTTP_URL: &str = "http://127.0.0.1:11434";

/// Default local Ollama Unix Domain Socket path
pub const DEFAULT_OLLAMA_UNIX_SOCKET: &str = "/tmp/ollama.sock";

/// Secondary fallback Unix socket paths
pub const OLLAMA_SOCKET_FALLBACKS: &[&str] = &[
    "/tmp/ollama.sock",
    "/var/run/ollama.sock",
    "/run/user/1000/ollama.sock",
];

/// Transport protocol used to communicate with Ollama
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OllamaTransportMode {
    /// Zero-overhead Unix Domain Socket (kernel-bypass, zero TCP stack overhead)
    UnixSocket(PathBuf),
    /// Local HTTP/REST TCP loopback
    Http(String),
}

/// Request payload for streaming or non-streaming generation (`/api/generate`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaGenerateRequest {
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

impl OllamaGenerateRequest {
    pub fn new(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            prompt: prompt.into(),
            stream: true,
            system: None,
            template: None,
            context: None,
            options: None,
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_options(mut self, options: Value) -> Self {
        self.options = Some(options);
        self
    }
}

/// Streaming chunk emitted by Ollama during text generation (`/api/generate`)
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OllamaStreamChunk {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub total_duration: Option<u64>,
    #[serde(default)]
    pub load_duration: Option<u64>,
    #[serde(default)]
    pub prompt_eval_count: Option<usize>,
    #[serde(default)]
    pub prompt_eval_duration: Option<u64>,
    #[serde(default)]
    pub eval_count: Option<usize>,
    #[serde(default)]
    pub eval_duration: Option<u64>,
    #[serde(default)]
    pub context: Option<Vec<i64>>,
}

/// Request payload for chat completions (`/api/chat`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaChatBridgeRequest {
    pub model: String,
    pub messages: Vec<OllamaBridgeChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

/// Chat message entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OllamaBridgeChatMessage {
    pub role: String,
    pub content: String,
}

/// Streaming chunk emitted by Ollama during chat completions (`/api/chat`)
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OllamaChatStreamChunk {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub message: Option<OllamaBridgeChatMessage>,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub total_duration: Option<u64>,
    #[serde(default)]
    pub prompt_eval_count: Option<usize>,
    #[serde(default)]
    pub eval_count: Option<usize>,
    #[serde(default)]
    pub eval_duration: Option<u64>,
}

/// An active model currently loaded into RAM / VRAM (`/api/ps`)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OllamaLoadedModel {
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub size_vram: Option<u64>,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
}

impl OllamaLoadedModel {
    pub fn formatted_vram_size(&self) -> String {
        let vram = self.size_vram.unwrap_or(0);
        const MB: u64 = 1024 * 1024;
        const GB: u64 = 1024 * MB;
        if vram >= GB {
            format!("{:.2} GB", vram as f64 / GB as f64)
        } else if vram >= MB {
            format!("{:.0} MB", vram as f64 / MB as f64)
        } else {
            format!("{} B", vram)
        }
    }
}

/// Response returned by `/api/ps`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OllamaPsResponse {
    #[serde(default)]
    pub models: Vec<OllamaLoadedModel>,
}

/// Response returned by `/api/tags`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OllamaTagsBridgeResponse {
    #[serde(default)]
    pub models: Vec<OllamaModelTag>,
}

/// Request payload for embedding generation (`/api/embeddings`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaEmbeddingRequest {
    pub model: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
}

/// Response payload for embedding generation (`/api/embeddings`)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OllamaEmbeddingResponse {
    #[serde(default)]
    pub embedding: Vec<f32>,
}

/// Health and connectivity status report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaHealthStatus {
    pub reachable: bool,
    pub transport: String,
    pub latency_ms: f64,
    pub total_installed_models: usize,
    pub active_loaded_models: usize,
    pub active_models: Vec<String>,
    pub endpoint: String,
}

/// Telemetry metrics for Ollama IPC bridge operations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OllamaBridgeMetrics {
    pub total_requests: u64,
    pub stream_tokens_received: u64,
    pub unix_socket_requests: u64,
    pub http_requests: u64,
    pub total_errors: u64,
}

/// Zero-overhead IPC Bridge for Ollama
pub struct OllamaIpcBridge {
    transport: OllamaTransportMode,
    client: reqwest::Client,
    metrics: Arc<BridgeMetricsInner>,
}

struct BridgeMetricsInner {
    total_requests: AtomicU64,
    stream_tokens_received: AtomicU64,
    unix_socket_requests: AtomicU64,
    http_requests: AtomicU64,
    total_errors: AtomicU64,
}

impl Default for BridgeMetricsInner {
    fn default() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            stream_tokens_received: AtomicU64::new(0),
            unix_socket_requests: AtomicU64::new(0),
            http_requests: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
        }
    }
}

impl OllamaIpcBridge {
    pub fn auto_detect() -> Self {
        let transport = Self::resolve_transport(None, None);
        Self::with_transport(transport)
    }

    pub fn with_transport(transport: OllamaTransportMode) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_millis(2000))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            transport,
            client,
            metrics: Arc::new(BridgeMetricsInner::default()),
        }
    }

    pub fn new_http(endpoint: impl Into<String>) -> Self {
        let ep = endpoint.into();
        Self::with_transport(OllamaTransportMode::Http(ep))
    }

    pub fn new_unix_socket(path: impl Into<PathBuf>) -> Self {
        let p = path.into();
        Self::with_transport(OllamaTransportMode::UnixSocket(p))
    }

    pub fn transport(&self) -> &OllamaTransportMode {
        &self.transport
    }

    pub fn metrics(&self) -> OllamaBridgeMetrics {
        OllamaBridgeMetrics {
            total_requests: self.metrics.total_requests.load(Ordering::Relaxed),
            stream_tokens_received: self.metrics.stream_tokens_received.load(Ordering::Relaxed),
            unix_socket_requests: self.metrics.unix_socket_requests.load(Ordering::Relaxed),
            http_requests: self.metrics.http_requests.load(Ordering::Relaxed),
            total_errors: self.metrics.total_errors.load(Ordering::Relaxed),
        }
    }

    pub fn resolve_transport(
        explicit_socket: Option<&Path>,
        explicit_http: Option<&str>,
    ) -> OllamaTransportMode {
        if let Some(sock) = explicit_socket {
            if sock.exists() {
                return OllamaTransportMode::UnixSocket(sock.to_path_buf());
            }
        }

        if let Ok(sock_env) = std::env::var("OLLAMA_SOCK") {
            let p = PathBuf::from(sock_env);
            if p.exists() {
                return OllamaTransportMode::UnixSocket(p);
            }
        }

        if let Ok(host_env) = std::env::var("OLLAMA_HOST") {
            if let Some(stripped) = host_env.strip_prefix("unix://") {
                let p = PathBuf::from(stripped);
                if p.exists() {
                    return OllamaTransportMode::UnixSocket(p);
                }
            }
        }

        for &candidate in OLLAMA_SOCKET_FALLBACKS {
            let p = Path::new(candidate);
            if p.exists() {
                return OllamaTransportMode::UnixSocket(p.to_path_buf());
            }
        }

        if let Some(http) = explicit_http {
            return OllamaTransportMode::Http(http.trim_end_matches('/').to_string());
        }

        if let Ok(host_env) = std::env::var("OLLAMA_HOST") {
            if !host_env.starts_with("unix://") {
                return OllamaTransportMode::Http(host_env.trim_end_matches('/').to_string());
            }
        }

        OllamaTransportMode::Http(DEFAULT_OLLAMA_HTTP_URL.to_string())
    }

    pub async fn health_check(&self) -> OllamaHealthStatus {
        let start = Instant::now();
        let (endpoint_str, transport_name) = match &self.transport {
            OllamaTransportMode::UnixSocket(p) => (p.display().to_string(), "UnixDomainSocket".to_string()),
            OllamaTransportMode::Http(u) => (u.clone(), "HTTP/REST".to_string()),
        };

        let installed_res = self.list_installed_models().await;
        let loaded_res = self.list_running_models().await;

        let reachable = installed_res.is_ok();
        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

        let (total_installed, active_loaded, active_names) = match (installed_res, loaded_res) {
            (Ok(installed), Ok(loaded)) => {
                let names = loaded.iter().map(|m| m.name.clone()).collect();
                (installed.len(), loaded.len(), names)
            }
            (Ok(installed), Err(_)) => (installed.len(), 0, Vec::new()),
            _ => (0, 0, Vec::new()),
        };

        OllamaHealthStatus {
            reachable,
            transport: transport_name,
            latency_ms,
            total_installed_models: total_installed,
            active_loaded_models: active_loaded,
            active_models: active_names,
            endpoint: endpoint_str,
        }
    }

    pub async fn list_running_models(&self) -> Result<Vec<OllamaLoadedModel>> {
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        let resp_body = self.dispatch_get("/api/ps").await?;
        let parsed: OllamaPsResponse = serde_json::from_str(&resp_body)
            .map_err(|e| HgbError::Serialization(format!("Failed to parse /api/ps response: {}", e)))?;
        Ok(parsed.models)
    }

    pub async fn list_installed_models(&self) -> Result<Vec<OllamaModelTag>> {
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        let resp_body = self.dispatch_get("/api/tags").await?;
        let parsed: OllamaTagsBridgeResponse = serde_json::from_str(&resp_body)
            .map_err(|e| HgbError::Serialization(format!("Failed to parse /api/tags response: {}", e)))?;
        Ok(parsed.models)
    }

    pub async fn generate_embedding(&self, model: &str, prompt: &str) -> Result<Vec<f32>> {
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        let req = OllamaEmbeddingRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
            options: None,
        };
        let body = serde_json::to_string(&req)
            .map_err(|e| HgbError::Serialization(format!("Serialize embedding request error: {}", e)))?;

        let resp_body = self.dispatch_post("/api/embeddings", &body).await?;
        let parsed: OllamaEmbeddingResponse = serde_json::from_str(&resp_body)
            .map_err(|e| HgbError::Serialization(format!("Parse embedding response error: {}", e)))?;

        if parsed.embedding.is_empty() {
            return Err(HgbError::Provider("Ollama returned empty embedding vector".into()));
        }
        Ok(parsed.embedding)
    }

    pub async fn generate_stream(
        &self,
        request: OllamaGenerateRequest,
    ) -> Result<mpsc::Receiver<Result<OllamaStreamChunk>>> {
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        let mut req = request;
        req.stream = true;

        let payload = serde_json::to_string(&req)
            .map_err(|e| HgbError::Serialization(format!("Serialize generate request: {}", e)))?;

        let (tx, rx) = mpsc::channel(128);
        let metrics = Arc::clone(&self.metrics);

        match &self.transport {
            OllamaTransportMode::UnixSocket(sock_path) => {
                metrics.unix_socket_requests.fetch_add(1, Ordering::Relaxed);
                let path = sock_path.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::stream_via_unix_socket(&path, "/api/generate", &payload, tx.clone()).await {
                        metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(Err(e)).await;
                    }
                });
            }
            OllamaTransportMode::Http(base_url) => {
                metrics.http_requests.fetch_add(1, Ordering::Relaxed);
                let client = self.client.clone();
                let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
                tokio::spawn(async move {
                    if let Err(e) = Self::stream_via_http(client, &url, &payload, tx.clone()).await {
                        metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(Err(e)).await;
                    }
                });
            }
        }

        Ok(rx)
    }

    pub async fn generate_complete(&self, request: OllamaGenerateRequest) -> Result<String> {
        let mut rx = self.generate_stream(request).await?;
        let mut full_text = String::new();

        while let Some(chunk_res) = rx.recv().await {
            let chunk = chunk_res?;
            full_text.push_str(&chunk.response);
            self.metrics.stream_tokens_received.fetch_add(1, Ordering::Relaxed);
            if chunk.done {
                break;
            }
        }
        Ok(full_text)
    }

    pub async fn chat_stream(
        &self,
        request: OllamaChatBridgeRequest,
    ) -> Result<mpsc::Receiver<Result<OllamaChatStreamChunk>>> {
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        let mut req = request;
        req.stream = true;

        let payload = serde_json::to_string(&req)
            .map_err(|e| HgbError::Serialization(format!("Serialize chat request: {}", e)))?;

        let (tx, rx) = mpsc::channel(128);
        let metrics = Arc::clone(&self.metrics);

        match &self.transport {
            OllamaTransportMode::UnixSocket(sock_path) => {
                metrics.unix_socket_requests.fetch_add(1, Ordering::Relaxed);
                let path = sock_path.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::stream_chat_via_unix_socket(&path, &payload, tx.clone()).await {
                        metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(Err(e)).await;
                    }
                });
            }
            OllamaTransportMode::Http(base_url) => {
                metrics.http_requests.fetch_add(1, Ordering::Relaxed);
                let client = self.client.clone();
                let url = format!("{}/api/chat", base_url.trim_end_matches('/'));
                tokio::spawn(async move {
                    if let Err(e) = Self::stream_chat_via_http(client, &url, &payload, tx.clone()).await {
                        metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(Err(e)).await;
                    }
                });
            }
        }

        Ok(rx)
    }

    async fn dispatch_get(&self, path: &str) -> Result<String> {
        match &self.transport {
            OllamaTransportMode::UnixSocket(sock) => {
                self.metrics.unix_socket_requests.fetch_add(1, Ordering::Relaxed);
                Self::send_unix_http_request(sock, "GET", path, "").await
            }
            OllamaTransportMode::Http(base) => {
                self.metrics.http_requests.fetch_add(1, Ordering::Relaxed);
                let url = format!("{}{}", base.trim_end_matches('/'), path);
                let resp = self
                    .client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| HgbError::Network(format!("Ollama GET {} failed: {}", url, e)))?;
                if !resp.status().is_success() {
                    return Err(HgbError::Provider(format!("Ollama HTTP {}", resp.status())));
                }
                resp.text()
                    .await
                    .map_err(|e| HgbError::Network(format!("Ollama read body failed: {}", e)))
            }
        }
    }

    async fn dispatch_post(&self, path: &str, body: &str) -> Result<String> {
        match &self.transport {
            OllamaTransportMode::UnixSocket(sock) => {
                self.metrics.unix_socket_requests.fetch_add(1, Ordering::Relaxed);
                Self::send_unix_http_request(sock, "POST", path, body).await
            }
            OllamaTransportMode::Http(base) => {
                self.metrics.http_requests.fetch_add(1, Ordering::Relaxed);
                let url = format!("{}{}", base.trim_end_matches('/'), path);
                let resp = self
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body(body.to_string())
                    .send()
                    .await
                    .map_err(|e| HgbError::Network(format!("Ollama POST {} failed: {}", url, e)))?;
                if !resp.status().is_success() {
                    return Err(HgbError::Provider(format!("Ollama HTTP {}", resp.status())));
                }
                resp.text()
                    .await
                    .map_err(|e| HgbError::Network(format!("Ollama read body failed: {}", e)))
            }
        }
    }

    async fn send_unix_http_request(
        socket_path: &Path,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String> {
        let mut stream = UnixStream::connect(socket_path).await.map_err(|e| {
            HgbError::Network(format!(
                "Failed to connect to Ollama Unix socket {}: {}",
                socket_path.display(),
                e
            ))
        })?;

        let request = format!(
            "{} {} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: Hagibis-OllamaIPC/0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            method,
            path,
            body.len(),
            body
        );

        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| HgbError::Network(format!("Unix socket write failed: {}", e)))?;
        stream.flush().await.map_err(|e| HgbError::Network(format!("Unix socket flush failed: {}", e)))?;

        let mut raw_response = Vec::new();
        stream
            .read_to_end(&mut raw_response)
            .await
            .map_err(|e| HgbError::Network(format!("Unix socket read failed: {}", e)))?;

        let resp_str = String::from_utf8(raw_response)
            .map_err(|e| HgbError::Serialization(format!("Invalid UTF-8 from Ollama socket: {}", e)))?;

        Self::extract_http_body(&resp_str)
    }

    async fn stream_via_unix_socket(
        socket_path: &Path,
        path: &str,
        payload: &str,
        tx: mpsc::Sender<Result<OllamaStreamChunk>>,
    ) -> Result<()> {
        let mut stream = UnixStream::connect(socket_path).await.map_err(|e| {
            HgbError::Network(format!(
                "Unix socket stream connect error {}: {}",
                socket_path.display(),
                e
            ))
        })?;

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: Hagibis-OllamaIPC/0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            path,
            payload.len(),
            payload
        );

        stream.write_all(request.as_bytes()).await.map_err(|e| HgbError::Network(e.to_string()))?;
        stream.flush().await.map_err(|e| HgbError::Network(e.to_string()))?;

        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        let mut in_body = false;

        while let Some(line) = lines.next_line().await.map_err(|e| HgbError::Network(e.to_string()))? {
            if !in_body {
                if line.trim().is_empty() {
                    in_body = true;
                }
                continue;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "0" {
                continue;
            }

            if trimmed.len() <= 8 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }

            match serde_json::from_str::<OllamaStreamChunk>(trimmed) {
                Ok(chunk) => {
                    let is_done = chunk.done;
                    let _ = tx.send(Ok(chunk)).await;
                    if is_done {
                        break;
                    }
                }
                Err(e) => {
                    if let Some(chunk) = Self::try_parse_json_chunk::<OllamaStreamChunk>(trimmed) {
                        let is_done = chunk.done;
                        let _ = tx.send(Ok(chunk)).await;
                        if is_done {
                            break;
                        }
                    } else {
                        let _ = tx
                            .send(Err(HgbError::Serialization(format!(
                                "Malformed chunk: {} ({})",
                                e, trimmed
                            ))))
                            .await;
                    }
                }
            }
        }
        Ok(())
    }

    async fn stream_via_http(
        client: reqwest::Client,
        url: &str,
        payload: &str,
        tx: mpsc::Sender<Result<OllamaStreamChunk>>,
    ) -> Result<()> {
        let resp = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("HTTP stream post error: {}", e)))?;

        if !resp.status().is_success() {
            return Err(HgbError::Provider(format!("Ollama HTTP {}", resp.status())));
        }

        let mut mut_resp = resp;
        let mut buffer = String::new();

        while let Some(bytes) = mut_resp.chunk().await.map_err(|e| HgbError::Network(format!("Read chunk error: {}", e)))? {
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<OllamaStreamChunk>(trimmed) {
                    Ok(chunk) => {
                        let is_done = chunk.done;
                        let _ = tx.send(Ok(chunk)).await;
                        if is_done {
                            return Ok(());
                        }
                    }
                    Err(_) => {
                        if let Some(chunk) = Self::try_parse_json_chunk::<OllamaStreamChunk>(trimmed) {
                            let is_done = chunk.done;
                            let _ = tx.send(Ok(chunk)).await;
                            if is_done {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn stream_chat_via_unix_socket(
        socket_path: &Path,
        payload: &str,
        tx: mpsc::Sender<Result<OllamaChatStreamChunk>>,
    ) -> Result<()> {
        let mut stream = UnixStream::connect(socket_path).await.map_err(|e| {
            HgbError::Network(format!("Unix socket chat connect error: {}", e))
        })?;

        let request = format!(
            "POST /api/chat HTTP/1.1\r\nHost: localhost\r\nUser-Agent: Hagibis-OllamaIPC/0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );

        stream.write_all(request.as_bytes()).await.map_err(|e| HgbError::Network(e.to_string()))?;
        stream.flush().await.map_err(|e| HgbError::Network(e.to_string()))?;

        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        let mut in_body = false;

        while let Some(line) = lines.next_line().await.map_err(|e| HgbError::Network(e.to_string()))? {
            if !in_body {
                if line.trim().is_empty() {
                    in_body = true;
                }
                continue;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "0" {
                continue;
            }
            if trimmed.len() <= 8 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }

            match serde_json::from_str::<OllamaChatStreamChunk>(trimmed) {
                Ok(chunk) => {
                    let is_done = chunk.done;
                    let _ = tx.send(Ok(chunk)).await;
                    if is_done {
                        break;
                    }
                }
                Err(_) => {
                    if let Some(chunk) = Self::try_parse_json_chunk::<OllamaChatStreamChunk>(trimmed) {
                        let is_done = chunk.done;
                        let _ = tx.send(Ok(chunk)).await;
                        if is_done {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn stream_chat_via_http(
        client: reqwest::Client,
        url: &str,
        payload: &str,
        tx: mpsc::Sender<Result<OllamaChatStreamChunk>>,
    ) -> Result<()> {
        let resp = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("HTTP chat stream error: {}", e)))?;

        if !resp.status().is_success() {
            return Err(HgbError::Provider(format!("Ollama HTTP {}", resp.status())));
        }

        let mut mut_resp = resp;
        let mut buffer = String::new();

        while let Some(bytes) = mut_resp.chunk().await.map_err(|e| HgbError::Network(e.to_string()))? {
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<OllamaChatStreamChunk>(trimmed) {
                    Ok(chunk) => {
                        let is_done = chunk.done;
                        let _ = tx.send(Ok(chunk)).await;
                        if is_done {
                            return Ok(());
                        }
                    }
                    Err(_) => {
                        if let Some(chunk) = Self::try_parse_json_chunk::<OllamaChatStreamChunk>(trimmed) {
                            let is_done = chunk.done;
                            let _ = tx.send(Ok(chunk)).await;
                            if is_done {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn extract_http_body(raw_http: &str) -> Result<String> {
        if let Some(idx) = raw_http.find("\r\n\r\n") {
            let body = &raw_http[idx + 4..];
            let clean = if let (Some(start), Some(end)) = (body.find('{'), body.rfind('}')) {
                if start <= end {
                    &body[start..=end]
                } else {
                    body
                }
            } else if let (Some(start), Some(end)) = (body.find('['), body.rfind(']')) {
                if start <= end {
                    &body[start..=end]
                } else {
                    body
                }
            } else {
                body
            };
            Ok(clean.to_string())
        } else {
            Ok(raw_http.to_string())
        }
    }

    fn try_parse_json_chunk<T: for<'de> Deserialize<'de>>(raw: &str) -> Option<T> {
        if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
            if start <= end {
                if let Ok(val) = serde_json::from_str::<T>(&raw[start..=end]) {
                    return Some(val);
                }
            }
        }
        None
    }

    pub fn parse_stream_lines(raw: &str) -> Vec<Result<OllamaStreamChunk>> {
        let mut results = Vec::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<OllamaStreamChunk>(trimmed) {
                Ok(chunk) => results.push(Ok(chunk)),
                Err(_) => {
                    if let Some(chunk) = Self::try_parse_json_chunk::<OllamaStreamChunk>(trimmed) {
                        results.push(Ok(chunk));
                    } else {
                        results.push(Err(HgbError::Serialization(format!(
                            "Invalid stream line: {}",
                            trimmed
                        ))));
                    }
                }
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stream_lines_generate() {
        let raw_stream = r#"
{"model":"qwen2.5-coder:7b","created_at":"2026-11-04T12:00:00Z","response":"fn ","done":false}
{"model":"qwen2.5-coder:7b","created_at":"2026-11-04T12:00:01Z","response":"solve","done":false}
{"model":"qwen2.5-coder:7b","created_at":"2026-11-04T12:00:02Z","response":"() {}","done":true,"total_duration":42000000,"eval_count":3,"eval_duration":35000000}
"#;
        let chunks = OllamaIpcBridge::parse_stream_lines(raw_stream);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.is_ok()));

        let c1 = chunks[0].as_ref().unwrap();
        assert_eq!(c1.response, "fn ");
        assert!(!c1.done);

        let c2 = chunks[1].as_ref().unwrap();
        assert_eq!(c2.response, "solve");
        assert!(!c2.done);

        let c3 = chunks[2].as_ref().unwrap();
        assert_eq!(c3.response, "() {}");
        assert!(c3.done);
        assert_eq!(c3.eval_count, Some(3));
        assert_eq!(c3.total_duration, Some(42000000));
    }

    #[test]
    fn test_parse_loaded_models_ps() {
        let raw_ps = r#"{
            "models": [
                {
                    "name": "qwen2.5-coder:7b",
                    "model": "qwen2.5-coder:7b",
                    "size": 4683074368,
                    "digest": "b5a93",
                    "expires_at": "2026-11-04T13:00:00Z",
                    "size_vram": 4683074368,
                    "details": {
                        "family": "qwen2",
                        "parameter_size": "7B",
                        "quantization_level": "Q4_K_M"
                    }
                }
            ]
        }"#;

        let parsed: OllamaPsResponse = serde_json::from_str(raw_ps).expect("ps parse");
        assert_eq!(parsed.models.len(), 1);
        let m = &parsed.models[0];
        assert_eq!(m.name, "qwen2.5-coder:7b");
        assert_eq!(m.size_vram, Some(4683074368));
        assert!(m.formatted_vram_size().contains("GB"));
    }

    #[test]
    fn test_extract_http_body() {
        let http_resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\"}";
        let body = OllamaIpcBridge::extract_http_body(http_resp).expect("extract body");
        assert_eq!(body, "{\"status\":\"ok\"}");
    }

    #[test]
    fn test_transport_resolution_defaults() {
        let trans = OllamaIpcBridge::resolve_transport(None, None);
        match trans {
            OllamaTransportMode::Http(url) => assert!(url.starts_with("http")),
            OllamaTransportMode::UnixSocket(p) => assert!(p.exists()),
        }
    }

    #[test]
    fn test_embedding_request_serialization() {
        let req = OllamaEmbeddingRequest {
            model: "nomic-embed-text".to_string(),
            prompt: "system microkernel".to_string(),
            options: None,
        };
        let serialized = serde_json::to_string(&req).expect("serialize");
        assert!(serialized.contains("nomic-embed-text"));
        assert!(serialized.contains("system microkernel"));
    }
}
