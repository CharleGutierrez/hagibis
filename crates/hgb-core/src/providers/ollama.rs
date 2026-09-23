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
#[derive(Clone, Debug)]
pub struct OllamaProvider {
    base_url: String,
    default_model: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaModelTag>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelTag {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    model: String,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaChatMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaChatMessage<'a> {
    role: &'a str,
    content: &'a str,
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

        let model = default_model
            .or_else(|| std::env::var("OLLAMA_MODEL").ok())
            .unwrap_or_else(|| "qwen2.5-coder:1.5b".to_string());

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
        let addr = if host.contains("://") {
            let without_proto = host.split("://").nth(1).unwrap_or("127.0.0.1:11434");
            let clean = without_proto.split('/').next().unwrap_or("127.0.0.1:11434");
            if !clean.contains(':') {
                format!("{}:11434", clean)
            } else {
                clean.to_string()
            }
        } else if !host.contains(':') {
            format!("{}:11434", host)
        } else {
            host
        };

        if let Ok(socket_addrs) = addr.to_socket_addrs() {
            for sa in socket_addrs {
                if std::net::TcpStream::connect_timeout(&sa, Duration::from_millis(300)).is_ok() {
                    return true;
                }
            }
        }
        false
    }

    /// Auto-discover local Ollama server and installed models
    pub fn auto_discover() -> Option<Self> {
        if Self::is_available() {
            let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
            let default_model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5-coder:1.5b".to_string());
            Some(Self::new(Some(host), Some(default_model)))
        } else {
            None
        }
    }

    /// Status string for doctor and telemetry
    pub fn status_string() -> String {
        if Self::is_available() {
            let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_OLLAMA_HOST.to_string());
            format!("Connected ({})", host)
        } else {
            "Offline (run 'ollama serve' to enable local models)".to_string()
        }
    }

    /// Retrieve the list of all locally installed model tags
    pub async fn list_models(&self) -> Result<Vec<String>> {
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

        Ok(tags.models.into_iter().map(|m| m.name).collect())
    }

    /// Determine if a given model identifier targets local Ollama
    pub fn is_ollama_model(model: &str) -> bool {
        let m = model.to_lowercase();
        m == "ollama"
            || m == "local"
            || m == "in-process-gguf"
            || m.starts_with("ollama/")
            || m.starts_with("local/")
            || m.contains("qwen")
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
    }

    /// Clean model name (strip "ollama/" or "local/" prefixes)
    pub fn sanitize_model_name<'a>(&'a self, requested: Option<&'a str>) -> &'a str {
        if let Some(m) = requested {
            let trimmed = m.trim();
            if trimmed.is_empty() || trimmed == "ollama" || trimmed == "local" || trimmed == "in-process-gguf" {
                &self.default_model
            } else if let Some(stripped) = trimmed.strip_prefix("ollama/") {
                stripped
            } else if let Some(stripped) = trimmed.strip_prefix("local/") {
                stripped
            } else {
                trimmed
            }
        } else {
            &self.default_model
        }
    }

    /// Execute completion query against Ollama /api/chat (with /api/generate fallback)
    pub async fn complete_prompt(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let effective_model = self.sanitize_model_name(model);
        let endpoint = format!("{}/api/chat", self.base_url);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("Hagibis-Microkernel/0.1.0"));

        let payload = OllamaChatRequest {
            model: effective_model,
            messages: vec![OllamaChatMessage {
                role: "user",
                content: prompt,
            }],
            stream: false,
            options: None,
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

                // If chat returned 404 or bad request, fallback to /api/generate
                let status_code = resp.status();
                if status_code.as_u16() == 404 {
                    return self.generate_fallback(prompt, effective_model).await;
                }

                let error_text = resp.text().await.unwrap_or_default();
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
        let payload = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
        });

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
        assert!(!OllamaProvider::is_ollama_model("gemini-2.5-flash"));
        assert!(!OllamaProvider::is_ollama_model("gemini-2.5-pro"));
    }

    #[test]
    fn test_sanitize_model_name() {
        let prov = OllamaProvider::new(None, Some("qwen2.5-coder:1.5b".to_string()));
        assert_eq!(prov.sanitize_model_name(None), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("ollama")), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("local")), "qwen2.5-coder:1.5b");
        assert_eq!(prov.sanitize_model_name(Some("ollama/llama3.2:1b")), "llama3.2:1b");
        assert_eq!(prov.sanitize_model_name(Some("phi3:mini")), "phi3:mini");
    }

    #[tokio::test]
    async fn test_ollama_live_tags_or_fallback() {
        if OllamaProvider::is_available() {
            let prov = OllamaProvider::new(None, None);
            let models = prov.list_models().await.expect("Failed to list models from running Ollama");
            assert!(!models.is_empty(), "Ollama models list should not be empty");
        }
    }
}
