use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use hgb_core::protocol::DevServerEndpointInfo;

/// Known dev server framework detected on a port
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevServerFramework {
    NextJs,
    Vite,
    ReactScripts,
    WarpActixAxum,
    FastApiFlask,
    OllamaLocal,
    GenericHttp,
}

/// Information about an active dev server endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevServerEndpoint {
    pub port: u16,
    pub url: String,
    pub framework: DevServerFramework,
    pub http_status: Option<u16>,
    pub response_time_ms: u64,
    pub server_header: Option<String>,
    pub is_healthy: bool,
}

impl From<DevServerEndpoint> for DevServerEndpointInfo {
    fn from(ep: DevServerEndpoint) -> Self {
        Self {
            port: ep.port,
            url: ep.url,
            framework: format!("{:?}", ep.framework),
            http_status: ep.http_status,
            response_time_ms: ep.response_time_ms,
            is_healthy: ep.is_healthy,
        }
    }
}

/// Runtime exception or panic captured from devserver output or error response
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeDiagnostic {
    pub port: u16,
    pub error_type: String,
    pub message: String,
    pub source_file: Option<PathBuf>,
    pub line_number: Option<usize>,
    pub stack_trace: Vec<String>,
    pub raw_payload: String,
}

pub struct DevServerSentinel {
    candidate_ports: Vec<u16>,
    client: reqwest::Client,
}

impl Default for DevServerSentinel {
    fn default() -> Self {
        Self::new(vec![3000, 5173, 8080, 8000, 4000, 4200, 11434, 8081, 9000])
    }
}

impl DevServerSentinel {
    pub fn new(candidate_ports: Vec<u16>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(800))
            .connect_timeout(Duration::from_millis(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { candidate_ports, client }
    }

    /// Probe all candidate ports via TCP handshake in parallel
    pub async fn scan_active_endpoints(&self) -> Vec<DevServerEndpoint> {
        let mut endpoints = Vec::new();
        let mut join_set = tokio::task::JoinSet::new();

        for &port in &self.candidate_ports {
            let client = self.client.clone();
            join_set.spawn(async move {
                Self::probe_single_port(port, client).await
            });
        }

        while let Some(res) = join_set.join_next().await {
            if let Ok(Some(ep)) = res {
                endpoints.push(ep);
            }
        }

        endpoints.sort_by_key(|e| e.port);
        endpoints
    }

    async fn probe_single_port(port: u16, client: reqwest::Client) -> Option<DevServerEndpoint> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        // 1. Fast TCP connection check (<60ms)
        match tokio::time::timeout(Duration::from_millis(60), TcpStream::connect(addr)).await {
            Ok(Ok(_stream)) => {}
            _ => return None,
        }

        // 2. HTTP Probe
        let url = format!("http://127.0.0.1:{}", port);
        let start = std::time::Instant::now();
        match client.get(&url).send().await {
            Ok(resp) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let status = resp.status().as_u16();
                let server_header = resp.headers().get("server")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let body = resp.text().await.unwrap_or_default();
                let framework = Self::detect_framework(port, &server_header, &body);
                let is_healthy = status < 500 && !body.contains("500 Internal Server Error");

                Some(DevServerEndpoint {
                    port,
                    url,
                    framework,
                    http_status: Some(status),
                    response_time_ms: duration_ms,
                    server_header,
                    is_healthy,
                })
            }
            Err(_) => {
                Some(DevServerEndpoint {
                    port,
                    url: format!("tcp://127.0.0.1:{}", port),
                    framework: DevServerFramework::GenericHttp,
                    http_status: None,
                    response_time_ms: 1,
                    server_header: None,
                    is_healthy: false,
                })
            }
        }
    }

    pub fn detect_framework(port: u16, server_header: &Option<String>, body: &str) -> DevServerFramework {
        if port == 11434 || body.contains("Ollama is running") {
            return DevServerFramework::OllamaLocal;
        }
        if body.contains("/@vite/client") || body.contains("__vite_plugin_react_preamble_installed__") {
            return DevServerFramework::Vite;
        }
        if body.contains("__NEXT_DATA__") || body.contains("/_next/") {
            return DevServerFramework::NextJs;
        }
        if let Some(ref s) = server_header {
            let lower = s.to_lowercase();
            if lower.contains("actix") || lower.contains("axum") || lower.contains("warp") {
                return DevServerFramework::WarpActixAxum;
            }
            if lower.contains("uvicorn") || lower.contains("werkzeug") {
                return DevServerFramework::FastApiFlask;
            }
        }
        DevServerFramework::GenericHttp
    }

    /// Extract runtime diagnostics from server response bodies or server logs
    pub fn parse_runtime_error(port: u16, raw_log: &str) -> Option<RuntimeDiagnostic> {
        // 1. Rust Panic: `thread 'main' panicked at src/main.rs:120:9:\nassertion failed: x > 0`
        let rust_panic_re = Regex::new(r"thread '.*?' panicked at (.*?):(\d+):(\d+):\n(.*?)(?:\nstack backtrace:|\z)").ok()?;
        if let Some(cap) = rust_panic_re.captures(raw_log) {
            let file = cap.get(1).map(|m| PathBuf::from(m.as_str()));
            let line = cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok());
            let msg = cap.get(4).map(|m| m.as_str().trim().to_string()).unwrap_or_else(|| "Rust panic".into());

            return Some(RuntimeDiagnostic {
                port,
                error_type: "RustPanic".into(),
                message: msg,
                source_file: file,
                line_number: line,
                stack_trace: vec![raw_log.to_string()],
                raw_payload: raw_log.to_string(),
            });
        }

        // 2. Node / Next.js / Vite unhandled exception
        let node_err_re = Regex::new(r"(?:Error|TypeError|ReferenceError|SyntaxError): (.*?)\n\s+at (?:.*?\((.*?):(\d+):(\d+)\)|(.*?):(\d+):(\d+))").ok()?;
        if let Some(cap) = node_err_re.captures(raw_log) {
            let msg = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let file = cap.get(2).or_else(|| cap.get(5)).map(|m| PathBuf::from(m.as_str()));
            let line = cap.get(3).or_else(|| cap.get(6)).and_then(|m| m.as_str().parse::<usize>().ok());

            return Some(RuntimeDiagnostic {
                port,
                error_type: "NodeException".into(),
                message: msg,
                source_file: file,
                line_number: line,
                stack_trace: vec![raw_log.to_string()],
                raw_payload: raw_log.to_string(),
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_frameworks() {
        assert_eq!(
            DevServerSentinel::detect_framework(11434, &None, ""),
            DevServerFramework::OllamaLocal
        );
        assert_eq!(
            DevServerSentinel::detect_framework(3000, &None, "<html><head><script type=\"module\" src=\"/@vite/client\"></script></head></html>"),
            DevServerFramework::Vite
        );
        assert_eq!(
            DevServerSentinel::detect_framework(3000, &None, "<script id=\"__NEXT_DATA__\" type=\"application/json\"></script>"),
            DevServerFramework::NextJs
        );
        assert_eq!(
            DevServerSentinel::detect_framework(8080, &Some("actix-web/4.0".into()), ""),
            DevServerFramework::WarpActixAxum
        );
        assert_eq!(
            DevServerSentinel::detect_framework(8000, &Some("uvicorn".into()), ""),
            DevServerFramework::FastApiFlask
        );
        assert_eq!(
            DevServerSentinel::detect_framework(9999, &None, "Hello world"),
            DevServerFramework::GenericHttp
        );
    }

    #[test]
    fn test_parse_rust_panic() {
        let panic_log = "thread 'main' panicked at src/server.rs:42:10:\nattempt to divide by zero\nstack backtrace:\n   0: rust_begin_unwind";
        let diag = DevServerSentinel::parse_runtime_error(8080, panic_log).expect("should parse panic");
        assert_eq!(diag.port, 8080);
        assert_eq!(diag.error_type, "RustPanic");
        assert_eq!(diag.message, "attempt to divide by zero");
        assert_eq!(diag.source_file, Some(PathBuf::from("src/server.rs")));
        assert_eq!(diag.line_number, Some(42));
    }

    #[test]
    fn test_parse_node_exception() {
        let node_log = "TypeError: Cannot read properties of undefined (reading 'map')\n    at renderList (/app/src/List.tsx:15:20)\n    at Component";
        let diag = DevServerSentinel::parse_runtime_error(3000, node_log).expect("should parse node exception");
        assert_eq!(diag.port, 3000);
        assert_eq!(diag.error_type, "NodeException");
        assert_eq!(diag.message, "Cannot read properties of undefined (reading 'map')");
        assert_eq!(diag.source_file, Some(PathBuf::from("/app/src/List.tsx")));
        assert_eq!(diag.line_number, Some(15));
    }

    #[tokio::test]
    async fn test_scan_active_endpoints_empty() {
        let sentinel = DevServerSentinel::new(vec![59998, 59999]);
        let endpoints = sentinel.scan_active_endpoints().await;
        assert!(endpoints.is_empty());
    }

    #[tokio::test]
    async fn test_scan_active_endpoint_live() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response = "HTTP/1.1 200 OK\r\nContent-Length: 17\r\nServer: actix-web\r\nConnection: close\r\n\r\nOllama is running";
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        let sentinel = DevServerSentinel::new(vec![port]);
        let endpoints = sentinel.scan_active_endpoints().await;
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].port, port);
        assert!(endpoints[0].is_healthy);
    }
}
