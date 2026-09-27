//! Ollama & Local LLM Provider for Hagibis (`hgb`)
//!
//! Connects to local Ollama server (default `http://127.0.0.1:11434` or `OLLAMA_HOST`),
//! dynamically discovers installed local GGUF models via `/api/tags`,
//! and executes zero-latency, zero-cloud-cost local inference.

use crate::error::{HgbError, Result};
use crate::traits::HgbProvider;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::ToSocketAddrs;
use std::time::Duration;

/// Default local Ollama host endpoint
pub const DEFAULT_OLLAMA_HOST: &str = "http://127.0.0.1:11434";

/// Provider for local models running via Ollama
use std::sync::RwLock;

/// Cache for discovered Ollama models to avoid redundant network round-trips
static OLLAMA_MODELS_CACHE: RwLock<Option<(std::time::Instant, Vec<OllamaModelTag>)>> = RwLock::new(None);
const CACHE_TTL_SECS: u64 = 4;

fn update_models_cache(models: Vec<OllamaModelTag>) {
    if let Ok(mut lock) = OLLAMA_MODELS_CACHE.write() {
        *lock = Some((std::time::Instant::now(), models));
    }
}

/// Parse host, port, and URL path from an OLLAMA_HOST string
fn parse_host_port_path(raw: &str) -> (String, u16, String) {
    let without_proto = if let Some(stripped) = raw.strip_prefix("http://") {
        stripped
    } else if let Some(stripped) = raw.strip_prefix("https://") {
        stripped
    } else {
        raw
    };

    let (host_port, path) = if let Some(slash_pos) = without_proto.find('/') {
        (&without_proto[..slash_pos], &without_proto[slash_pos..])
    } else {
        (without_proto, "")
    };

    let (host, port) = if let Some(colon_pos) = host_port.find(':') {
        let h = &host_port[..colon_pos];
        let p = host_port[colon_pos + 1..].parse::<u16>().unwrap_or(11434);
        (h.to_string(), p)
    } else {
        (host_port.to_string(), 11434)
    };

    (if host.is_empty() { "127.0.0.1".to_string() } else { host }, port, path.to_string())
}

/// Provider for local models running via Ollama
#[derive(Clone, Debug)]
pub struct OllamaProvider {
    base_url: String,
    default_model: String,
    client: reqwest::Client,
}

/// Detailed specifications of an installed Ollama model
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OllamaModelDetails {
    #[serde(default)]
    pub parent_model: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub embedding_length: Option<u64>,
}

/// Metadata tag returned for an installed Ollama model
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OllamaModelTag {
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl OllamaModelTag {
    /// Formatted human-readable file size (e.g. "986 MB", "7.37 GB")
    pub fn formatted_size(&self) -> String {
        const KB: u64 = 1024;
        const MB: u64 = 1024 * KB;
        const GB: u64 = 1024 * MB;

        if self.size >= GB {
            format!("{:.2} GB", self.size as f64 / GB as f64)
        } else if self.size >= MB {
            format!("{:.0} MB", self.size as f64 / MB as f64)
        } else if self.size >= KB {
            format!("{:.0} KB", self.size as f64 / KB as f64)
        } else {
            format!("{} B", self.size)
        }
    }

    /// Parameter summary (e.g. "13B [Q4_0]", "1.5B [Q4_K_M]")
    pub fn param_summary(&self) -> String {
        if let Some(details) = &self.details {
            let p_size = details.parameter_size.as_deref().unwrap_or("unknown");
            let quant = details.quantization_level.as_deref().unwrap_or("raw");
            format!("{p_size} [{quant}]")
        } else {
            "GGUF".to_string()
        }
    }

    /// Short model name without ':latest' if ':latest' is present
    pub fn short_name(&self) -> &str {
        self.name.strip_suffix(":latest").unwrap_or(&self.name)
    }
}

/// Response returned by Ollama /api/tags
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OllamaTagsResponse {
    #[serde(default)]
    pub models: Vec<OllamaModelTag>,
}

#[derive(Clone, Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct OllamaChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    #[serde(default)]
    message: Option<OllamaChatResponseMessage>,
    #[serde(default)]
    response: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponseMessage {
    content: String,
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl OllamaProvider {
    /// Fast synchronous probe to fetch all installed models from local Ollama.
    /// Uses raw TCP socket HTTP GET with a 350ms timeout, running cleanly in any thread or runtime.
    pub fn fetch_installed_models_sync() -> Vec<OllamaModelTag> {
        // 1. Fast cache check
        if let Ok(lock) = OLLAMA_MODELS_CACHE.read() {
            if let Some((instant, ref models)) = *lock {
                if instant.elapsed() < Duration::from_secs(CACHE_TTL_SECS) {
                    return models.clone();
                }
            }
        }

        let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
        let (host_str, port, path) = parse_host_port_path(&host);
        let addr = format!("{}:{}", host_str, port);

        if let Ok(socket_addrs) = addr.to_socket_addrs() {
            for sa in socket_addrs {
                if let Ok(mut stream) = std::net::TcpStream::connect_timeout(&sa, Duration::from_millis(300)) {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));

                    use std::io::{Read, Write};
                    let req_path = if path.is_empty() { "/api/tags" } else { &path };
                    let request = format!(
                        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: Hagibis-OllamaProbe/0.1\r\nConnection: close\r\n\r\n",
                        req_path, host_str, port
                    );

                    if stream.write_all(request.as_bytes()).is_ok() {
                        let mut response = Vec::new();
                        let _ = stream.read_to_end(&mut response);
                        if let Ok(resp_str) = String::from_utf8(response) {
                            if let Some(body_idx) = resp_str.find("\r\n\r\n") {
                                let body = &resp_str[body_idx + 4..];
                                // Handle HTTP 1.1 chunked transfer encoding by extracting JSON object { ... }
                                let json_slice = if let (Some(start), Some(end)) = (body.find('{'), body.rfind('}')) {
                                    if start <= end {
                                        &body[start..=end]
                                    } else {
                                        body
                                    }
                                } else {
                                    body
                                };
                                if let Ok(tags_resp) = serde_json::from_str::<OllamaTagsResponse>(json_slice) {
                                    update_models_cache(tags_resp.models.clone());
                                    return tags_resp.models;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fallback to cached entries if probe failed
        if let Ok(lock) = OLLAMA_MODELS_CACHE.read() {
            if let Some((_, ref models)) = *lock {
                return models.clone();
            }
        }
        Vec::new()
    }

    /// Retrieve all installed model metadata tags currently held by Ollama
    pub fn installed_models() -> Vec<OllamaModelTag> {
        Self::fetch_installed_models_sync()
    }

    /// Retrieve the names/tags of all models currently held by Ollama
    pub fn installed_model_names() -> Vec<String> {
        Self::installed_models().into_iter().map(|m| m.name).collect()
    }

    /// Check if a model name is physically installed in Ollama
    pub fn is_installed_model(model_name: &str) -> bool {
        let trimmed = model_name.trim();
        let clean = trimmed
            .strip_prefix("ollama/")
            .or_else(|| trimmed.strip_prefix("local/"))
            .unwrap_or(trimmed);

        let installed = Self::installed_model_names();
        installed.iter().any(|m| {
            m.eq_ignore_ascii_case(clean)
                || m.strip_suffix(":latest").unwrap_or(m).eq_ignore_ascii_case(clean)
                || clean.strip_suffix(":latest").unwrap_or(clean).eq_ignore_ascii_case(m.strip_suffix(":latest").unwrap_or(m))
        })
    }

    /// Create new OllamaProvider instance
    pub fn new(base_url: Option<String>, default_model: Option<String>) -> Self {
        let base = base_url
            .or_else(|| std::env::var("OLLAMA_HOST").ok())
            .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.to_string());

        let clean_base = base.trim_end_matches('/').to_string();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(4))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let model = if let Some(ref explicit) = default_model {
            explicit
                .strip_prefix("ollama/")
                .or_else(|| explicit.strip_prefix("local/"))
                .unwrap_or(explicit)
                .to_string()
        } else if let Ok(env_m) = std::env::var("OLLAMA_MODEL") {
            env_m
        } else {
            let installed = Self::installed_model_names();
            if let Some(coder) = installed.iter().find(|m| m.contains("coder")) {
                coder.clone()
            } else if let Some(first) = installed.first() {
                first.clone()
            } else {
                "qwen2.5-coder:7b".to_string()
            }
        };

        Self {
            base_url: clean_base,
            default_model: model,
            client,
        }
    }

    /// Base URL for Ollama HTTP API
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Configured default model
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Check if local Ollama daemon is reachable
    pub fn is_available() -> bool {
        let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
        let (host_str, port, _) = parse_host_port_path(&host);
        let addr = format!("{}:{}", host_str, port);

        if let Ok(socket_addrs) = addr.to_socket_addrs() {
            for sa in socket_addrs {
                if std::net::TcpStream::connect_timeout(&sa, Duration::from_millis(300)).is_ok() {
                    return true;
                }
            }
        }
        false
    }

    /// Auto-discover local Ollama server and installed models dynamically
    pub fn auto_discover() -> Option<Self> {
        if Self::is_available() {
            let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
            let default_model = if let Ok(explicit) = std::env::var("OLLAMA_MODEL") {
                explicit
            } else {
                let models = Self::installed_model_names();
                if let Some(coder) = models.iter().find(|m| m.contains("coder")) {
                    coder.clone()
                } else if let Some(first) = models.first() {
                    first.clone()
                } else {
                    "qwen2.5-coder:7b".to_string()
                }
            };
            Some(Self::new(Some(host), Some(default_model)))
        } else {
            None
        }
    }

    /// Async auto-discovery of local Ollama server, dynamically querying installed models
    pub async fn auto_discover_async() -> Option<Self> {
        if !Self::is_available() {
            return None;
        }

        let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
        let mut prov = Self::new(Some(host), None);

        let default_model = if let Ok(explicit) = std::env::var("OLLAMA_MODEL") {
            explicit
        } else if let Ok(models) = prov.list_models().await {
            if let Some(coder) = models.iter().find(|m| m.contains("coder")) {
                coder.clone()
            } else if let Some(first) = models.first() {
                first.clone()
            } else {
                "qwen2.5-coder:7b".to_string()
            }
        } else {
            "qwen2.5-coder:7b".to_string()
        };

        prov.default_model = default_model;
        Some(prov)
    }

    /// Status string for doctor and telemetry
    pub fn status_string() -> String {
        if Self::is_available() {
            let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
            let count = Self::installed_model_names().len();
            if count > 0 {
                format!("Connected ({}, {} models available)", host, count)
            } else {
                format!("Connected ({})", host)
            }
        } else {
            "Offline (run 'ollama serve' to enable local models)".to_string()
        }
    }

    /// Retrieve full details of all locally installed models
    pub async fn list_model_details(&self) -> Result<Vec<OllamaModelTag>> {
        let endpoint = format!("{}/api/tags", self.base_url);
        let resp = self
            .client
            .get(&endpoint)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to connect to Ollama at {}: {}", self.base_url, e)))?;

        if !resp.status().is_success() {
            return Err(HgbError::Provider(format!(
                "Ollama tags endpoint returned HTTP {}",
                resp.status()
            )));
        }

        let tags: OllamaTagsResponse = resp
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Failed to parse Ollama model list: {}", e)))?;

        update_models_cache(tags.models.clone());
        Ok(tags.models)
    }

    /// Retrieve the list of all locally installed model tags
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let tags = self.list_model_details().await?;
        Ok(tags.into_iter().map(|m| m.name).collect())
    }

    /// Determine if a given model identifier targets local Ollama.
    /// Determine if a given model identifier targets local Ollama.
    /// Automatically checks against the live installed models currently held by Ollama!
    pub fn is_ollama_model(model: &str) -> bool {
        let m = model.trim().to_lowercase();
        let clean = m
            .strip_prefix("ollama/")
            .or_else(|| m.strip_prefix("local/"))
            .or_else(|| m.strip_prefix("gguf/"))
            .or_else(|| m.strip_prefix("hf/"))
            .or_else(|| m.strip_prefix("huggingface/"))
            .or_else(|| m.strip_prefix("ollama:"))
            .or_else(|| m.strip_prefix("local:"))
            .unwrap_or(&m);

        // 1. Check if model is held by local Ollama!
        if Self::is_installed_model(clean) || Self::is_installed_model(&m) {
            return true;
        }

        // 2. Generic local aliases
        if m == "ollama" || m == "local" || m == "in-process-gguf" || m.starts_with("ollama/") || m.starts_with("local/") || m.starts_with("gguf/") {
            return true;
        }

        // 3. Fallback well-known open-source local model architectures
        if m.contains("qwen")
            || m.contains("llama")
            || m.contains("smollm")
            || m.contains("phi")
            || m.contains("mistral")
            || m.contains("gemma")
            || m.contains("deepseek")
            || m.contains("codellama")
            || m.contains("starcoder")
            || m.contains("yi")
            || m.contains("vicuna")
            || m.contains("tinyllama")
            || m.contains("wizardlm")
            || m.contains("hermes")
            || m.contains("falcon")
            || m.contains("granite")
            || m.contains("command-r")
        {
            return true;
        }

        // 4. Quantization / tag signatures common to Ollama & GGUF
        if m.contains(":q") || m.contains(":latest") || m.contains(":instruct") || m.contains(":chat") || m.contains(".gguf") {
            return true;
        }

        // 5. Emerging, next-generation, and future local architectures
        m.contains("deepseek-r1")
            || m.contains("deepseek-v3")
            || m.contains("qwen3")
            || m.contains("llama4")
            || m.contains("nemotron")
            || m.contains("phi4")
            || m.contains("ministral")
            || m.contains("devstral")
            || m.contains("solar")
            || m.contains("olmo")
            || m.contains("exaone")
            || m.contains("internlm")
    }

    /// Clean model name (strip "ollama/", "local/", "gguf/", etc. prefixes)
    pub fn sanitize_model_name<'a>(&'a self, requested: Option<&'a str>) -> &'a str {
        if let Some(m) = requested {
            let trimmed = m.trim();
            if trimmed.is_empty() || trimmed == "ollama" || trimmed == "local" || trimmed == "in-process-gguf" || trimmed == "auto" || trimmed == "default" {
                &self.default_model
            } else if let Some(stripped) = trimmed.strip_prefix("ollama/") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("local/") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("gguf/") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("hf/") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("huggingface/") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("ollama:") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("local:") {
                stripped
            } else {
                trimmed
            }
        } else {
            &self.default_model
        }
    }

    /// Resolve requested model name into an actually installed local model or clean requested name.
    /// If the requested model is not installed in Ollama, automatically finds the closest installed
    /// local model (e.g. `qwen2.5-coder:7b` when `qwen2.5-coder:1.5b` was requested) to prevent 404 crashes!
    pub fn resolve_target_model(&self, requested: Option<&str>) -> String {
        let requested_raw = requested.unwrap_or(&self.default_model);
        let requested_clean = self.sanitize_model_name(Some(requested_raw)).to_string();

        if Self::is_installed_model(&requested_clean) || Self::is_installed_model(requested_raw) {
            return requested_clean;
        }

        let installed = Self::installed_model_names();
        if installed.is_empty() {
            return requested_clean;
        }

        // Try to find closest match in same family (e.g. qwen2.5-coder)
        let family_prefix = requested_clean.split(':').next().unwrap_or(&requested_clean);
        if let Some(m) = installed.iter().find(|m| m.contains(family_prefix)) {
            return m.clone();
        }

        // Try to find a coder model
        if let Some(m) = installed.iter().find(|m| m.contains("coder")) {
            return m.clone();
        }

        // Fallback to default_model or first installed
        if Self::is_installed_model(&self.default_model) {
            self.default_model.clone()
        } else {
            installed[0].clone()
        }
    }

    /// Dynamically construct Ollama inference options from environment variables or host defaults:
    /// - `OLLAMA_NUM_CTX`: Context window length (default: 4096+ or model default)
    /// - `OLLAMA_NUM_THREAD`: Hardware threads for CPU inference (default: all available physical threads)
    /// - `OLLAMA_TEMPERATURE`: Sampling temperature (e.g. 0.6)
    /// - `OLLAMA_NUM_PREDICT`: Max generated tokens (e.g. 4096 or -1)
    pub fn build_inference_options() -> Option<Value> {
        let mut opts = serde_json::Map::new();

        // 1. Threading: Auto-detect CPU cores if not explicitly set
        let threads = std::env::var("OLLAMA_NUM_THREAD")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get() as u64)
                    .unwrap_or(4)
            });
        opts.insert("num_thread".to_string(), serde_json::json!(threads));

        // 2. Context Window: override via OLLAMA_NUM_CTX
        if let Ok(ctx) = std::env::var("OLLAMA_NUM_CTX") {
            if let Ok(v) = ctx.parse::<u64>() {
                opts.insert("num_ctx".to_string(), serde_json::json!(v));
            }
        }

        // 3. Temperature: override via OLLAMA_TEMPERATURE
        if let Ok(temp) = std::env::var("OLLAMA_TEMPERATURE") {
            if let Ok(v) = temp.parse::<f64>() {
                opts.insert("temperature".to_string(), serde_json::json!(v));
            }
        }

        // 4. Max output tokens: override via OLLAMA_NUM_PREDICT
        if let Ok(predict) = std::env::var("OLLAMA_NUM_PREDICT") {
            if let Ok(v) = predict.parse::<i64>() {
                opts.insert("num_predict".to_string(), serde_json::json!(v));
            }
        }

        Some(Value::Object(opts))
    }

    /// Default system capability prompt ensuring present and future local LLMs
    /// are 100% aware of their live web browsing and persistent memory capabilities.
    pub fn default_system_prompt() -> &'static str {
        "You are Hagibis (hgb), an elite Systems Rust & Autonomous AI Agent runtime.\n\
        You have full, active live internet browsing and persistent cross-session memory via your built-in tools.\n\
        Never state that you cannot access the internet, browse the web, or remember past decisions.\n\
        You are equipped with:\n\
        - `search_web(query: string, max_results?: number)`: Run live web searches and retrieve ranked, sanitized organic results.\n\
        - `browse_web(url: string, max_tokens?: number)`: Fetch and read live web pages, sanitized markdown, and documentation.\n\
        - `record_memory(title: string, decision: string, ...)`: Persist architectural decisions and technical debt to disk.\n\
        - `search_memory(query: string)`: Query project memory across sessions.\n\
        When you need live documentation, current library versions, or fresh web information, invoke your tools via a ```tool_call block or state the query.\n\
        Always reason step-by-step."
    }

    /// Parse an incoming prompt string into structured Ollama chat messages.
    /// Handles both raw single prompts and ReAct-style XML delimited conversation histories
    /// (<system>, <user>, <assistant>, <tool_results>), while unconditionally guaranteeing
    /// that every local LLM receives the universal Hagibis web browsing and memory capability awareness anchor!
    pub fn parse_chat_messages(prompt: &str) -> Vec<OllamaChatMessage> {
        let trimmed = prompt.trim();
        let mut messages = Vec::new();

        let has_system = trimmed.contains("<system>") && trimmed.contains("</system>");
        let has_user = trimmed.contains("<user>") && trimmed.contains("</user>");
        let has_assistant = trimmed.contains("<assistant>") && trimmed.contains("</assistant>");

        if has_system || has_user || has_assistant {
            let mut remaining = trimmed;
            while !remaining.is_empty() {
                let next_tag = [
                    remaining.find("<system>").map(|idx| (idx, "<system>", "</system>", "system")),
                    remaining.find("<user>").map(|idx| (idx, "<user>", "</user>", "user")),
                    remaining.find("<assistant>").map(|idx| (idx, "<assistant>", "</assistant>", "assistant")),
                    remaining.find("<tool_results>").map(|idx| (idx, "<tool_results>", "</tool_results>", "user")),
                ].into_iter().flatten().min_by_key(|(idx, _, _, _)| *idx);

                if let Some((idx, open_tag, close_tag, role)) = next_tag {
                    let before = remaining[..idx].trim();
                    if !before.is_empty() {
                        messages.push(OllamaChatMessage {
                            role: "user".to_string(),
                            content: before.to_string(),
                        });
                    }
                    let after_open = &remaining[idx + open_tag.len()..];
                    if let Some(close_idx) = after_open.find(close_tag) {
                        let content = after_open[..close_idx].trim();
                        let final_content = if open_tag == "<tool_results>" {
                            format!("[Tool Results]:\n{}", content)
                        } else {
                            content.to_string()
                        };
                        messages.push(OllamaChatMessage {
                            role: role.to_string(),
                            content: final_content,
                        });
                        remaining = after_open[close_idx + close_tag.len()..].trim();
                    } else {
                        messages.push(OllamaChatMessage {
                            role: role.to_string(),
                            content: after_open.trim().to_string(),
                        });
                        break;
                    }
                } else {
                    if !remaining.is_empty() {
                        messages.push(OllamaChatMessage {
                            role: "user".to_string(),
                            content: remaining.to_string(),
                        });
                    }
                    break;
                }
            }
        } else {
            messages.push(OllamaChatMessage {
                role: "system".to_string(),
                content: Self::default_system_prompt().to_string(),
            });
            messages.push(OllamaChatMessage {
                role: "user".to_string(),
                content: trimmed.to_string(),
            });
        }

        // Guarantee that a system message exists so the model is 100% aware of browsing!
        if !messages.iter().any(|m| m.role == "system") {
            messages.insert(0, OllamaChatMessage {
                role: "system".to_string(),
                content: Self::default_system_prompt().to_string(),
            });
        }

        messages
    }

    /// Execute completion query against Ollama /api/chat (with /api/generate fallback)
    pub async fn complete_prompt(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let resolved_model_str = self.resolve_target_model(model);
        let effective_model = resolved_model_str.as_str();
        let endpoint = format!("{}/api/chat", self.base_url);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("Hagibis-Microkernel/0.1.0"));

        let messages = Self::parse_chat_messages(prompt);

        let payload = OllamaChatRequest {
            model: effective_model,
            messages,
            stream: false,
            options: Self::build_inference_options(),
        };

        let resp_result = self
            .client
            .post(&endpoint)
            .headers(headers.clone())
            .json(&payload)
            .send()
            .await;

        match resp_result {
            Ok(resp) => {
                if resp.status().is_success() {
                    let chat_resp: OllamaChatResponse = resp
                        .json()
                        .await
                        .map_err(|e| HgbError::Serialization(format!("Ollama response parse error: {}", e)))?;

                    if let Some(msg) = chat_resp.message {
                        return Ok(msg.content);
                    } else if let Some(txt) = chat_resp.response {
                        return Ok(txt);
                    } else {
                        return Err(HgbError::Provider("Empty response returned by Ollama".into()));
                    }
                }

                let status_code = resp.status();
                let error_text = resp.text().await.unwrap_or_default();

                // If model not found (404), trigger smart auto-recovery with installed models
                if status_code.as_u16() == 404 {
                    if error_text.contains("not found") {
                        let installed = Self::installed_model_names();
                        if let Some(fallback_model) = installed.iter().find(|m| m.as_str() != effective_model) {
                            let mut retry_payload = payload.clone();
                            retry_payload.model = fallback_model.as_str();
                            if let Ok(retry_resp) = self.client.post(&endpoint).headers(headers.clone()).json(&retry_payload).send().await {
                                if retry_resp.status().is_success() {
                                    if let Ok(chat_resp) = retry_resp.json::<OllamaChatResponse>().await {
                                        if let Some(msg) = chat_resp.message {
                                            let _ = crate::persist_active_model(fallback_model);
                                            return Ok(msg.content);
                                        }
                                    }
                                }
                            }
                        }
                        return Err(HgbError::Provider(format!(
                            "Local Ollama model '{}' is not installed.\nInstalled models: {:?}\nRun 'ollama pull {}' in your terminal or switch models with '/model <name>'.",
                            effective_model, installed, effective_model
                        )));
                    }

                    // If /api/chat not supported, fallback to /api/generate
                    return self.generate_fallback(prompt, effective_model).await;
                }

                Err(HgbError::Provider(format!(
                    "Ollama API error (HTTP {}): {}",
                    status_code, error_text
                )))
            }
            Err(e) => {
                // Try fallback to /api/generate
                self.generate_fallback(prompt, effective_model).await.map_err(|_| {
                    HgbError::Network(format!(
                        "Failed to communicate with local Ollama server at {}: {}. Ensure 'ollama serve' is running.",
                        self.base_url, e
                    ))
                })
            }
        }
    }

    async fn generate_fallback(&self, prompt: &str, model: &str) -> Result<String> {
        let endpoint = format!("{}/api/generate", self.base_url);
        let mut payload = serde_json::json!({
            "model": model,
            "system": Self::default_system_prompt(),
            "prompt": prompt,
            "stream": false,
        });
        if let Some(opts) = Self::build_inference_options() {
            payload["options"] = opts;
        }

        let resp = self
            .client
            .post(&endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Ollama generate endpoint failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 && err.contains("not found") {
                let installed = Self::installed_model_names();
                return Err(HgbError::Provider(format!(
                    "Local Ollama model '{}' is not installed.\nInstalled models: {:?}\nRun 'ollama pull {}' in your terminal or switch models with '/model <name>'.",
                    model, installed, model
                )));
            }
            return Err(HgbError::Provider(format!("Ollama generate error {}: {}", status, err)));
        }

        let val: Value = resp
            .json()
            .await
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        if let Some(resp_txt) = val.get("response").and_then(|v| v.as_str()) {
            Ok(resp_txt.to_string())
        } else {
            Err(HgbError::Provider("Invalid response schema from Ollama generate endpoint".into()))
        }
    }
}

#[async_trait]
impl HgbProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        self.complete_prompt(prompt, model).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ollama_model_resolution() {
        assert!(OllamaProvider::is_ollama_model("ollama"));
        assert!(OllamaProvider::is_ollama_model("local"));
        assert!(OllamaProvider::is_ollama_model("in-process-gguf"));
        assert!(OllamaProvider::is_ollama_model("qwen2.5-coder:1.5b"));
        assert!(OllamaProvider::is_ollama_model("llama3.2:1b"));
        assert!(OllamaProvider::is_ollama_model("smollm2:1.7b"));
        assert!(OllamaProvider::is_ollama_model("phi3:mini"));
        assert!(OllamaProvider::is_ollama_model("ollama/deepseek-coder:6.7b"));
        assert!(OllamaProvider::is_ollama_model("wizardlm-uncensored:latest"));
        assert!(!OllamaProvider::is_ollama_model("gemini-2.5-flash"));
        assert!(!OllamaProvider::is_ollama_model("gemini-2.5-pro"));
    }

    #[test]
    fn test_sanitize_model_name() {
        let prov = OllamaProvider::new(None, Some("qwen2.5-coder:1.5b".to_string()));
        assert_eq!(prov.sanitize_model_name(None), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("ollama")), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("local")), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("auto")), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("ollama/llama3.2:1b")), "llama3.2:1b");
        assert_eq!(prov.sanitize_model_name(Some("phi3:mini")), "phi3:mini");
        assert_eq!(prov.sanitize_model_name(Some("wizardlm-uncensored:latest")), "wizardlm-uncensored:latest");
    }

    #[test]
    fn test_ollama_model_tag_formatting() {
        let tag = OllamaModelTag {
            name: "wizardlm-uncensored:latest".to_string(),
            model: "wizardlm-uncensored:latest".to_string(),
            modified_at: Some("2026-09-25T19:56:09.721951957+08:00".to_string()),
            size: 7365835187,
            digest: Some("886a369d74fc".to_string()),
            details: Some(OllamaModelDetails {
                parent_model: None,
                format: Some("gguf".to_string()),
                family: Some("llama".to_string()),
                parameter_size: Some("13B".to_string()),
                quantization_level: Some("Q4_0".to_string()),
                context_length: Some(4096),
                embedding_length: Some(5120),
            }),
            capabilities: vec!["completion".to_string()],
        };

        assert_eq!(tag.short_name(), "wizardlm-uncensored");
        assert_eq!(tag.formatted_size(), "6.86 GB");
        assert_eq!(tag.param_summary(), "13B [Q4_0]");
    }

    #[test]
    fn test_parse_host_port_path() {
        let (host, port, path) = parse_host_port_path("http://127.0.0.1:11434");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 11434);
        assert_eq!(path, "");

        let (host2, port2, path2) = parse_host_port_path("192.168.1.50:8080/prefix");
        assert_eq!(host2, "192.168.1.50");
        assert_eq!(port2, 8080);
        assert_eq!(path2, "/prefix");
    }

    #[tokio::test]
    async fn test_ollama_live_tags_or_fallback() {
        if OllamaProvider::is_available() {
            let prov = OllamaProvider::new(None, None);
            let models = prov.list_models().await.expect("Failed to list models from running Ollama");
            assert!(!models.is_empty(), "Ollama models list should not be empty");
            let details = prov.list_model_details().await.expect("Failed to list model details");
            assert_eq!(models.len(), details.len());
        }
    }

    #[test]
    fn test_fetch_installed_models_sync_probe() {
        if OllamaProvider::is_available() {
            let models = OllamaProvider::installed_models();
            assert!(!models.is_empty(), "OllamaProvider::installed_models() should return live models");
            let names = OllamaProvider::installed_model_names();
            assert!(!names.is_empty(), "OllamaProvider::installed_model_names() should return model names");
            // Check that at least one known model is recognized as installed
            assert!(OllamaProvider::is_installed_model(&names[0]));
        }
    }

    #[test]
    fn test_parse_chat_messages_auto_injects_system_and_tools() {
        // 1. Raw prompt: must automatically inject Hagibis system prompt with browsing & memory capabilities
        let raw_prompt = "What is the newest version of tokio on crates.io?";
        let messages = OllamaProvider::parse_chat_messages(raw_prompt);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert!(messages[0].content.contains("Hagibis (hgb)"));
        assert!(messages[0].content.contains("search_web"));
        assert!(messages[0].content.contains("browse_web"));
        assert!(messages[0].content.contains("Never state that you cannot access the internet"));
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, raw_prompt);

        // 2. Structured ReAct prompt: must parse into distinct system, user, assistant, and tool turns
        let react_prompt = "<system>\nYou are Hagibis.\n</system>\n\n<user>\nCheck the docs.\n</user>\n\n<assistant>\n```tool_call\n{\"call_id\":\"c1\",\"tool_name\":\"search_web\",\"arguments\":{\"query\":\"rust\"}}\n```\n</assistant>\n\n<tool_results>\nRust 1.85 released\n</tool_results>";
        let parsed = OllamaProvider::parse_chat_messages(react_prompt);
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].role, "system");
        assert_eq!(parsed[0].content, "You are Hagibis.");
        assert_eq!(parsed[1].role, "user");
        assert_eq!(parsed[1].content, "Check the docs.");
        assert_eq!(parsed[2].role, "assistant");
        assert!(parsed[2].content.contains("tool_call"));
        assert_eq!(parsed[3].role, "user");
        assert!(parsed[3].content.contains("[Tool Results]"));
        assert!(parsed[3].content.contains("Rust 1.85 released"));
    }

    #[test]
    fn test_is_ollama_model_future_and_quant_architectures() {
        // Present models
        assert!(OllamaProvider::is_ollama_model("qwen2.5-coder:1.5b"));
        assert!(OllamaProvider::is_ollama_model("deepseek-coder:6.7b"));
        assert!(OllamaProvider::is_ollama_model("llama3.2:3b"));
        assert!(OllamaProvider::is_ollama_model("mistral:7b"));

        // Emerging and future models
        assert!(OllamaProvider::is_ollama_model("qwen3:8b"));
        assert!(OllamaProvider::is_ollama_model("deepseek-r1:8b"));
        assert!(OllamaProvider::is_ollama_model("deepseek-v3:671b"));
        assert!(OllamaProvider::is_ollama_model("llama4:8b"));
        assert!(OllamaProvider::is_ollama_model("nemotron:70b"));
        assert!(OllamaProvider::is_ollama_model("phi4:14b"));
        assert!(OllamaProvider::is_ollama_model("devstral:24b"));

        // Common quant suffixes and custom tags
        assert!(OllamaProvider::is_ollama_model("custom-agent-model:q4_k_m"));
        assert!(OllamaProvider::is_ollama_model("my-future-llm:latest"));
        assert!(OllamaProvider::is_ollama_model("gguf/finetuned.gguf"));
        assert!(OllamaProvider::is_ollama_model("hf/meta-llama/Llama-3.2-1B-Instruct"));
    }

    #[test]
    fn test_resolve_target_model_with_uninstalled_model() {
        if OllamaProvider::is_available() {
            let prov = OllamaProvider::new(None, None);
            let installed = OllamaProvider::installed_model_names();
            if !installed.is_empty() {
                // Requesting an uninstalled model must resolve to an installed model!
                let resolved = prov.resolve_target_model(Some("qwen2.5-coder:1.5b"));
                assert!(installed.contains(&resolved), "Resolved model '{}' must be one of installed {:?}", resolved, installed);
            }
        }
    }

    #[tokio::test]
    async fn test_uninstalled_model_query_auto_recovery() {
        if OllamaProvider::is_available() {
            let prov = OllamaProvider::new(None, None);
            let installed = OllamaProvider::installed_model_names();
            if !installed.is_empty() {
                // Explicitly send query with the model from the user's error screenshot!
                let result = prov.complete_prompt("respond with exactly PONG", Some("qwen2.5-coder:1.5b")).await;
                assert!(result.is_ok(), "Expected query with uninstalled model to auto-resolve or recover, but got: {:?}", result.err());
                let resp = result.unwrap();
                assert!(!resp.trim().is_empty());
            }
        }
    }
}

