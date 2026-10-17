//! # Visual Multimodal Debugger via CDP (`visual_loop`)
//!
//! Real Chrome DevTools Protocol (CDP) WebSocket client for Chromium/Chrome remote debugging.
//! Implements JSON-RPC 2.0 messages: Page.navigate, Page.captureScreenshot, Runtime.evaluate, Console.enable.
//! Captures PNG screenshot bytes, extracts console errors & JS exceptions, formats multimodal vision payloads.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

static RPC_REQ_ID: AtomicU64 = AtomicU64::new(1);

/// JSON-RPC 2.0 Request Frame for CDP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

impl CdpRequest {
    pub fn new(method: &str, params: Value) -> Self {
        let id = RPC_REQ_ID.fetch_add(1, Ordering::SeqCst);
        Self {
            id,
            method: method.to_string(),
            params,
        }
    }

    pub fn navigate(url: &str) -> Self {
        Self::new("Page.navigate", json!({ "url": url }))
    }

    pub fn capture_screenshot(format: &str, quality: Option<u8>) -> Self {
        let mut params = json!({ "format": format });
        if let Some(q) = quality {
            params["quality"] = json!(q);
        }
        Self::new("Page.captureScreenshot", params)
    }

    pub fn evaluate(expression: &str) -> Self {
        Self::new("Runtime.evaluate", json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true
        }))
    }

    pub fn enable_console() -> Self {
        Self::new("Console.enable", json!({}))
    }

    pub fn enable_page() -> Self {
        Self::new("Page.enable", json!({}))
    }

    pub fn enable_runtime() -> Self {
        Self::new("Runtime.enable", json!({}))
    }
}

/// JSON-RPC 2.0 Response Frame from CDP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpResponse {
    pub id: Option<u64>,
    pub method: Option<String>,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub params: Option<Value>,
}

/// Console message level
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warning,
    Error,
    Debug,
}

/// Captured browser console error or message
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CdpConsoleMessage {
    pub level: ConsoleLevel,
    pub text: String,
    pub source: String,
    pub url: Option<String>,
    pub line_number: Option<u32>,
    pub timestamp: f64,
}

/// Visual Debugger Diagnostic Report ready for vision models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualDebuggerReport {
    pub target_url: String,
    pub http_status: u16,
    pub page_title: String,
    pub screenshot_base64: String,
    pub screenshot_format: String,
    pub console_errors: Vec<CdpConsoleMessage>,
    pub console_warnings: Vec<CdpConsoleMessage>,
    pub dom_error_count: usize,
    pub formatted_vision_prompt: String,
}

impl VisualDebuggerReport {
    /// Verify that the screenshot base64 string decodes to valid PNG magic bytes
    pub fn is_valid_png_screenshot(&self) -> bool {
        if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &self.screenshot_base64) {
            // PNG signature: 89 50 4E 47 0D 0A 1A 0A
            if bytes.len() >= 8 {
                return bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
            }
        }
        false
    }

    /// Construct prompt tailored for multimodal vision models (Claude 3.5 Sonnet / Gemini 2.5 Flash)
    pub fn generate_multimodal_prompt(
        target_url: &str,
        page_title: &str,
        errors: &[CdpConsoleMessage],
        warnings: &[CdpConsoleMessage],
        dom_error_count: usize,
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str("# MULTIMODAL VISUAL DEBUGGING INCIDENT\n\n");
        prompt.push_str(&format!("Target URL: {}\n", target_url));
        prompt.push_str(&format!("Page Title: {}\n", page_title));
        prompt.push_str(&format!("DOM Error Count: {}\n\n", dom_error_count));

        prompt.push_str("## BROWSER CONSOLE ERRORS:\n");
        if errors.is_empty() {
            prompt.push_str("None detected.\n");
        } else {
            for (idx, err) in errors.iter().enumerate() {
                prompt.push_str(&format!("{}. [{}] {}\n", idx + 1, err.source, err.text));
                if let Some(ref u) = err.url {
                    prompt.push_str(&format!("   Location: {}:{}\n", u, err.line_number.unwrap_or(0)));
                }
            }
        }
        prompt.push_str("\n");

        prompt.push_str("## BROWSER CONSOLE WARNINGS:\n");
        if warnings.is_empty() {
            prompt.push_str("None detected.\n");
        } else {
            for (idx, w) in warnings.iter().enumerate() {
                prompt.push_str(&format!("{}. {}\n", idx + 1, w.text));
            }
        }
        prompt.push_str("\n");

        prompt.push_str("## ACTION INSTRUCTIONS:\n");
        prompt.push_str("Analyze the attached screenshot alongside the JavaScript console exceptions above. ");
        prompt.push_str("Identify the root cause of layout breaking, failed network requests, or unhandled promise rejections, ");
        prompt.push_str("and provide a minimal, surgical code patch to fix the underlying frontend bug.");

        prompt
    }
}

/// Real Chrome DevTools Protocol Client
pub struct CdpClient {
    pub debug_port: u16,
    pub host: String,
}

impl Default for CdpClient {
    fn default() -> Self {
        Self {
            debug_port: 9222,
            host: "127.0.0.1".to_string(),
        }
    }
}

impl CdpClient {
    pub fn new(host: &str, debug_port: u16) -> Self {
        Self {
            host: host.to_string(),
            debug_port,
        }
    }

    /// Discover WebSocket Debugger URL from Chromium /json/version or /json/list HTTP endpoint
    pub async fn discover_debugger_url(&self) -> Result<String, String> {
        let http_url = format!("http://{}:{}/json/version", self.host, self.debug_port);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;

        let resp = client.get(&http_url).send().await
            .map_err(|e| format!("Failed to reach Chromium debugging port {}: {}", self.debug_port, e))?;

        if !resp.status().is_success() {
            return Err(format!("Chromium /json/version returned HTTP {}", resp.status()));
        }

        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(ws_url) = body.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
            return Ok(ws_url.to_string());
        }

        // Fallback: try /json/list
        let list_url = format!("http://{}:{}/json/list", self.host, self.debug_port);
        let list_resp = client.get(&list_url).send().await.map_err(|e| e.to_string())?;
        let pages: Vec<Value> = list_resp.json().await.map_err(|e| e.to_string())?;
        for p in pages {
            if let Some(ws) = p.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                return Ok(ws.to_string());
            }
        }

        Err("No webSocketDebuggerUrl found in Chromium endpoints".to_string())
    }

    /// Connect to remote debugging WebSocket and run end-to-end visual inspection
    pub async fn capture_visual_debug_report(&self, target_url: &str) -> Result<VisualDebuggerReport, String> {
        let ws_url = self.discover_debugger_url().await?;
        let (ws_stream, _) = connect_async(&ws_url).await
            .map_err(|e| format!("Failed to connect to CDP WebSocket {}: {}", ws_url, e))?;

        let (mut write, mut read) = ws_stream.split();

        // 1. Enable domains
        for req in [CdpRequest::enable_page(), CdpRequest::enable_runtime(), CdpRequest::enable_console()] {
            let msg = serde_json::to_string(&req).map_err(|e| e.to_string())?;
            write.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await
                .map_err(|e| e.to_string())?;
        }

        // 2. Navigate to target URL
        let nav_req = CdpRequest::navigate(target_url);
        let nav_id = nav_req.id;
        write.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&nav_req).unwrap())).await
            .map_err(|e| e.to_string())?;

        // 3. Listen for events and wait for load
        let mut console_errors = Vec::new();
        let mut console_warnings = Vec::new();
        let mut page_loaded = false;
        let mut title = String::from("Web Page");

        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = &mut timeout => {
                    break;
                }
                msg_opt = read.next() => {
                    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt))) = msg_opt {
                        if let Ok(resp) = serde_json::from_str::<CdpResponse>(&txt) {
                            if let Some(ref m) = resp.method {
                                match m.as_str() {
                                    "Page.loadEventFired" => {
                                        page_loaded = true;
                                    }
                                    "Console.messageAdded" => {
                                        if let Some(ref p) = resp.params {
                                            if let Some(msg_obj) = p.get("message") {
                                                let text = msg_obj.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                                let level_str = msg_obj.get("level").and_then(|l| l.as_str()).unwrap_or("log");
                                                let source = msg_obj.get("source").and_then(|s| s.as_str()).unwrap_or("console").to_string();
                                                let url = msg_obj.get("url").and_then(|u| u.as_str()).map(|s| s.to_string());
                                                let line_number = msg_obj.get("line").and_then(|l| l.as_u64()).map(|n| n as u32);

                                                let msg = CdpConsoleMessage {
                                                    level: match level_str {
                                                        "error" => ConsoleLevel::Error,
                                                        "warning" => ConsoleLevel::Warning,
                                                        "info" => ConsoleLevel::Info,
                                                        _ => ConsoleLevel::Log,
                                                    },
                                                    text,
                                                    source,
                                                    url,
                                                    line_number,
                                                    timestamp: 0.0,
                                                };

                                                if msg.level == ConsoleLevel::Error {
                                                    console_errors.push(msg);
                                                } else if msg.level == ConsoleLevel::Warning {
                                                    console_warnings.push(msg);
                                                }
                                            }
                                        }
                                    }
                                    "Runtime.exceptionThrown" => {
                                        if let Some(ref p) = resp.params {
                                            let desc = p.get("exceptionDetails")
                                                .and_then(|d| d.get("text"))
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("Unhandled JavaScript Exception")
                                                .to_string();

                                            console_errors.push(CdpConsoleMessage {
                                                level: ConsoleLevel::Error,
                                                text: desc,
                                                source: "exception".to_string(),
                                                url: None,
                                                line_number: None,
                                                timestamp: 0.0,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    } else {
                        break;
                    }
                }
            }

            if page_loaded {
                break;
            }
        }

        // 4. Capture screenshot
        let shot_req = CdpRequest::capture_screenshot("png", None);
        let shot_id = shot_req.id;
        write.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&shot_req).unwrap())).await
            .map_err(|e| e.to_string())?;

        let mut screenshot_base64 = String::new();
        let shot_timeout = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(shot_timeout);

        loop {
            tokio::select! {
                _ = &mut shot_timeout => break,
                msg_opt = read.next() => {
                    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt))) = msg_opt {
                        if let Ok(resp) = serde_json::from_str::<CdpResponse>(&txt) {
                            if resp.id == Some(shot_id) {
                                if let Some(ref res) = resp.result {
                                    if let Some(data) = res.get("data").and_then(|d| d.as_str()) {
                                        screenshot_base64 = data.to_string();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 5. Query page title via Runtime.evaluate
        let eval_req = CdpRequest::evaluate("document.title");
        let eval_id = eval_req.id;
        write.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&eval_req).unwrap())).await
            .map_err(|e| e.to_string())?;

        let eval_timeout = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(eval_timeout);

        loop {
            tokio::select! {
                _ = &mut eval_timeout => break,
                msg_opt = read.next() => {
                    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(txt))) = msg_opt {
                        if let Ok(resp) = serde_json::from_str::<CdpResponse>(&txt) {
                            if resp.id == Some(eval_id) {
                                if let Some(ref res) = resp.result {
                                    if let Some(val) = res.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str()) {
                                        title = val.to_string();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let dom_error_count = console_errors.len();
        let prompt = VisualDebuggerReport::generate_multimodal_prompt(
            target_url,
            &title,
            &console_errors,
            &console_warnings,
            dom_error_count,
        );

        Ok(VisualDebuggerReport {
            target_url: target_url.to_string(),
            http_status: 200,
            page_title: title,
            screenshot_base64,
            screenshot_format: "png".to_string(),
            console_errors,
            console_warnings,
            dom_error_count,
            formatted_vision_prompt: prompt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdp_message_framing() {
        let nav = CdpRequest::navigate("https://example.com");
        assert_eq!(nav.method, "Page.navigate");
        assert_eq!(nav.params["url"], "https://example.com");

        let shot = CdpRequest::capture_screenshot("png", Some(85));
        assert_eq!(shot.method, "Page.captureScreenshot");
        assert_eq!(shot.params["format"], "png");
        assert_eq!(shot.params["quality"], 85);

        let eval = CdpRequest::evaluate("1 + 1");
        assert_eq!(eval.method, "Runtime.evaluate");
        assert_eq!(eval.params["expression"], "1 + 1");
    }

    #[test]
    fn test_screenshot_png_magic_byte_validation() {
        // Construct valid 1x1 PNG image bytes:
        let valid_png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // Magic
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
            0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
            0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
            0x42, 0x60, 0x82,
        ];
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, valid_png_bytes);

        let report = VisualDebuggerReport {
            target_url: "http://localhost:3000".to_string(),
            http_status: 200,
            page_title: "Test App".to_string(),
            screenshot_base64: encoded,
            screenshot_format: "png".to_string(),
            console_errors: vec![],
            console_warnings: vec![],
            dom_error_count: 0,
            formatted_vision_prompt: String::new(),
        };

        assert!(report.is_valid_png_screenshot());

        // Invalid screenshot
        let invalid_report = VisualDebuggerReport {
            screenshot_base64: "bm90X2FfcG5n".to_string(),
            ..report
        };
        assert!(!invalid_report.is_valid_png_screenshot());
    }

    #[test]
    fn test_diagnostic_report_and_vision_prompt_formatting() {
        let errors = vec![
            CdpConsoleMessage {
                level: ConsoleLevel::Error,
                text: "Uncaught TypeError: Cannot read property 'map' of undefined".to_string(),
                source: "javascript".to_string(),
                url: Some("http://localhost:3000/app.js".to_string()),
                line_number: Some(42),
                timestamp: 12345.0,
            }
        ];
        let warnings = vec![
            CdpConsoleMessage {
                level: ConsoleLevel::Warning,
                text: "DevTools failed to load source map".to_string(),
                source: "network".to_string(),
                url: None,
                line_number: None,
                timestamp: 12344.0,
            }
        ];

        let prompt = VisualDebuggerReport::generate_multimodal_prompt(
            "http://localhost:3000/dashboard",
            "Dashboard - Hagibis",
            &errors,
            &warnings,
            1,
        );

        assert!(prompt.contains("Dashboard - Hagibis"));
        assert!(prompt.contains("Uncaught TypeError"));
        assert!(prompt.contains("Location: http://localhost:3000/app.js:42"));
        assert!(prompt.contains("DevTools failed to load source map"));
        assert!(prompt.contains("DOM Error Count: 1"));
    }
}
