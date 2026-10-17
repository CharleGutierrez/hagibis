//! # GitHub Copilot RFC 8628 OAuth & Inference Engine
//!
//! Real production-grade RFC 8628 Device Flow implementation, token exchange,
//! local credential caching (`~/.hgb/copilot_auth.json`), and Copilot chat completion client.
//! Targets `https://api.individual.githubcopilot.com/chat/completions` with Claude 3.5 Sonnet & GPT-4o.

use crate::error::{HgbError, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe81";
pub const COPILOT_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const COPILOT_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const COPILOT_INTERNAL_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
pub const COPILOT_CHAT_API_DEFAULT: &str = "https://api.individual.githubcopilot.com/chat/completions";

pub const COPILOT_USER_AGENT: &str = "GithubCopilot/1.155.0";
pub const COPILOT_EDITOR_VERSION: &str = "vscode/1.95.0";
pub const COPILOT_EDITOR_PLUGIN: &str = "copilot-chat/0.22.0";
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";

/// Device authorization code response from GitHub (RFC 8628)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// OAuth access token response from GitHub
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubTokenResponse {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Copilot internal session token response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotSessionResponse {
    pub token: String,
    pub expires_at: i64,
    #[serde(default)]
    pub endpoints: Option<CopilotEndpoints>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CopilotEndpoints {
    pub api: Option<String>,
    pub telemetry: Option<String>,
}

/// Persisted Copilot credentials stored in `~/.hgb/copilot_auth.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotCredentials {
    pub github_token: String,
    pub copilot_token: String,
    pub expires_at: i64,
    pub api_endpoint: String,
    pub updated_at: i64,
}

/// A message in a Copilot chat request
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CopilotChatMessage {
    pub role: String,
    pub content: String,
}

impl CopilotChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }
}

/// Chat completion request for Copilot chat endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotChatRequest {
    pub model: String,
    pub messages: Vec<CopilotChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

/// Chat completion response choice from Copilot
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CopilotChoice {
    pub index: Option<usize>,
    pub message: Option<CopilotChatMessage>,
    pub finish_reason: Option<String>,
}

/// Chat completion response body from Copilot
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CopilotChatResponse {
    pub id: Option<String>,
    #[serde(default)]
    pub choices: Vec<CopilotChoice>,
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<CopilotUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CopilotUsage {
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
}

/// Production engine for GitHub Copilot OAuth Device Flow & completions.
#[derive(Debug, Clone)]
pub struct CopilotAuthEngine {
    client: reqwest::Client,
    cache_path: PathBuf,
}

impl Default for CopilotAuthEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotAuthEngine {
    /// Creates a new `CopilotAuthEngine` with the standard cache path `~/.hgb/copilot_auth.json`.
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let cache_path = PathBuf::from(home).join(".hgb").join("copilot_auth.json");
        Self::with_cache_path(cache_path)
    }

    /// Creates a new `CopilotAuthEngine` with a custom credentials cache path.
    pub fn with_cache_path(cache_path: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, cache_path }
    }

    /// Step 1: Initiate RFC 8628 OAuth Device Flow.
    /// Calls `POST https://github.com/login/device/code` with client ID `Iv1.b507a08c87ecfe81`.
    pub async fn request_device_code(&self) -> Result<DeviceCodeResponse> {
        let params = [
            ("client_id", COPILOT_CLIENT_ID),
            ("scope", "read:user"),
        ];

        let res = self
            .client
            .post(COPILOT_DEVICE_CODE_URL)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, COPILOT_USER_AGENT)
            .form(&params)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to connect to GitHub Device Flow: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(HgbError::Authentication(format!(
                "GitHub Device Code error (HTTP {}): {}",
                status, body
            )));
        }

        let device_code_resp: DeviceCodeResponse = res.json().await.map_err(|e| {
            HgbError::Serialization(format!("Failed to parse GitHub Device Code response: {e}"))
        })?;

        Ok(device_code_resp)
    }

    /// Step 2: Poll `https://github.com/login/oauth/access_token` until user authorizes.
    pub async fn poll_access_token(
        &self,
        device_code: &str,
        interval_secs: u64,
        expires_in_secs: u64,
    ) -> Result<String> {
        let start = SystemTime::now();
        let timeout = std::time::Duration::from_secs(expires_in_secs.min(900));
        let mut poll_interval = std::time::Duration::from_secs(interval_secs.max(5));

        while start.elapsed().unwrap_or_default() < timeout {
            tokio::time::sleep(poll_interval).await;

            let params = [
                ("client_id", COPILOT_CLIENT_ID),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ];

            let res = self
                .client
                .post(COPILOT_ACCESS_TOKEN_URL)
                .header(ACCEPT, "application/json")
                .header(USER_AGENT, COPILOT_USER_AGENT)
                .form(&params)
                .send()
                .await
                .map_err(|e| HgbError::Network(format!("Poll access token network error: {e}")))?;

            let token_resp: GitHubTokenResponse = res.json().await.map_err(|e| {
                HgbError::Serialization(format!("Failed to parse token response: {e}"))
            })?;

            if let Some(token) = token_resp.access_token {
                return Ok(token);
            }

            if let Some(ref err) = token_resp.error {
                match err.as_str() {
                    "authorization_pending" => {
                        // User has not finished logging in yet, continue polling
                        continue;
                    }
                    "slow_down" => {
                        // GitHub requests polling backoff
                        poll_interval += std::time::Duration::from_secs(5);
                        continue;
                    }
                    "expired_token" => {
                        return Err(HgbError::Authentication(
                            "Device authorization code expired. Please rerun 'hgb auth copilot'.".into(),
                        ));
                    }
                    "access_denied" => {
                        return Err(HgbError::Authentication(
                            "User denied Copilot device authorization.".into(),
                        ));
                    }
                    other => {
                        let desc = token_resp
                            .error_description
                            .unwrap_or_else(|| "Unknown error".to_string());
                        return Err(HgbError::Authentication(format!(
                            "OAuth error {}: {}",
                            other, desc
                        )));
                    }
                }
            }
        }

        Err(HgbError::Authentication("Timed out waiting for GitHub Copilot device authorization.".into()))
    }

    /// Step 3: Exchange GitHub access token for internal Copilot session token.
    /// Calls `GET https://api.github.com/copilot_internal/v2/token`.
    pub async fn exchange_copilot_token(&self, github_token: &str) -> Result<CopilotSessionResponse> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", github_token))
                .map_err(|e| HgbError::Authentication(format!("Invalid GitHub token format: {e}")))?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(COPILOT_USER_AGENT));
        headers.insert(
            "Editor-Version",
            HeaderValue::from_static(COPILOT_EDITOR_VERSION),
        );
        headers.insert(
            "Editor-Plugin-Version",
            HeaderValue::from_static(COPILOT_EDITOR_PLUGIN),
        );

        let res = self
            .client
            .get(COPILOT_INTERNAL_TOKEN_URL)
            .headers(headers)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to exchange Copilot internal token: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(HgbError::Authentication(format!(
                "Copilot internal token exchange failed (HTTP {}): {}",
                status, body
            )));
        }

        let session: CopilotSessionResponse = res.json().await.map_err(|e| {
            HgbError::Serialization(format!("Failed to parse Copilot session response: {e}"))
        })?;

        Ok(session)
    }

    /// Saves credentials to local cache file.
    pub fn save_credentials(&self, creds: &CopilotCredentials) -> Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(creds)
            .map_err(|e| HgbError::Serialization(format!("Failed to serialize credentials: {e}")))?;
        std::fs::write(&self.cache_path, json)?;
        Ok(())
    }

    /// Loads credentials from local cache file.
    pub fn load_credentials(&self) -> Result<Option<CopilotCredentials>> {
        if !self.cache_path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&self.cache_path)?;
        let creds: CopilotCredentials = serde_json::from_str(&data)
            .map_err(|e| HgbError::Serialization(format!("Failed to parse credentials cache: {e}")))?;
        Ok(Some(creds))
    }

    /// Returns true if credentials exist and token has not expired.
    pub fn is_token_valid(&self, creds: &CopilotCredentials) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        // Valid if at least 60 seconds remain before expiration
        creds.expires_at > (now + 60)
    }

    /// Gets active session token, automatically refreshing if expired using cached GitHub token.
    pub async fn get_valid_session_token(&self) -> Result<String> {
        let mut creds = match self.load_credentials()? {
            Some(c) => c,
            None => {
                return Err(HgbError::Authentication(
                    "No GitHub Copilot credentials found. Run 'hgb auth copilot' first.".into(),
                ))
            }
        };

        if self.is_token_valid(&creds) {
            return Ok(creds.copilot_token);
        }

        // Token expired: refresh via internal endpoint using github_token
        let fresh_session = self.exchange_copilot_token(&creds.github_token).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        creds.copilot_token = fresh_session.token.clone();
        creds.expires_at = fresh_session.expires_at;
        creds.updated_at = now;
        if let Some(endpoints) = fresh_session.endpoints {
            if let Some(api) = endpoints.api {
                creds.api_endpoint = api;
            }
        }

        let _ = self.save_credentials(&creds);
        Ok(creds.copilot_token)
    }

    /// Complete chat with Copilot backend (`https://api.individual.githubcopilot.com/chat/completions`).
    pub async fn chat_completion(&self, request: &CopilotChatRequest) -> Result<CopilotChatResponse> {
        let creds = self.load_credentials()?.ok_or_else(|| {
            HgbError::Authentication("GitHub Copilot not authenticated. Run 'hgb auth copilot'.".into())
        })?;

        let session_token = self.get_valid_session_token().await?;

        let endpoint = if creds.api_endpoint.is_empty() {
            COPILOT_CHAT_API_DEFAULT.to_string()
        } else {
            format!("{}/chat/completions", creds.api_endpoint.trim_end_matches('/'))
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", session_token))
                .map_err(|e| HgbError::Authentication(format!("Invalid Copilot token format: {e}")))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(COPILOT_USER_AGENT));
        headers.insert(
            "Editor-Version",
            HeaderValue::from_static(COPILOT_EDITOR_VERSION),
        );
        headers.insert(
            "Editor-Plugin-Version",
            HeaderValue::from_static(COPILOT_EDITOR_PLUGIN),
        );
        headers.insert(
            "Copilot-Integration-Id",
            HeaderValue::from_static(COPILOT_INTEGRATION_ID),
        );
        headers.insert("Openai-Organization", HeaderValue::from_static("github-copilot"));
        headers.insert("Openai-Intent", HeaderValue::from_static("conversation-panel"));

        let res = self
            .client
            .post(&endpoint)
            .headers(headers)
            .json(request)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Copilot chat completion failed to send: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(HgbError::Provider(format!(
                "Copilot API error (HTTP {}): {}",
                status, body
            )));
        }

        let resp: CopilotChatResponse = res.json().await.map_err(|e| {
            HgbError::Serialization(format!("Failed to parse Copilot chat response: {e}"))
        })?;

        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_code_response_deserialization() {
        let json = r#"{
            "device_code": "3584d83530557fdd1f46af8282488d80b3d7390f",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5
        }"#;

        let parsed: DeviceCodeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.user_code, "WDJB-MJHT");
        assert_eq!(parsed.interval, 5);
        assert_eq!(parsed.expires_in, 900);
    }

    #[test]
    fn test_token_validity_check() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let engine = CopilotAuthEngine::with_cache_path(PathBuf::from("/tmp/test_copilot_auth.json"));

        let valid_creds = CopilotCredentials {
            github_token: "ghu_123".to_string(),
            copilot_token: "tid_valid".to_string(),
            expires_at: now + 3600, // 1 hour future
            api_endpoint: COPILOT_CHAT_API_DEFAULT.to_string(),
            updated_at: now,
        };
        assert!(engine.is_token_valid(&valid_creds));

        let expired_creds = CopilotCredentials {
            github_token: "ghu_123".to_string(),
            copilot_token: "tid_expired".to_string(),
            expires_at: now - 100, // past
            api_endpoint: COPILOT_CHAT_API_DEFAULT.to_string(),
            updated_at: now,
        };
        assert!(!engine.is_token_valid(&expired_creds));
    }

    #[test]
    fn test_copilot_chat_request_serialization() {
        let req = CopilotChatRequest {
            model: "claude-3.5-sonnet".to_string(),
            messages: vec![
                CopilotChatMessage::system("You are an expert engineer."),
                CopilotChatMessage::user("Optimize this Rust function."),
            ],
            temperature: Some(0.2),
            max_tokens: Some(4096),
            stream: Some(false),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("claude-3.5-sonnet"));
        assert!(json.contains("Optimize this Rust function."));
        assert!(json.contains("temperature"));
    }

    #[test]
    fn test_save_and_load_credentials() {
        let temp_dir = std::env::temp_dir();
        let test_path = temp_dir.join(format!("copilot_test_{}.json", std::process::id()));
        let engine = CopilotAuthEngine::with_cache_path(test_path.clone());

        let creds = CopilotCredentials {
            github_token: "ghu_test_token".to_string(),
            copilot_token: "tid_test_token".to_string(),
            expires_at: 1999999999,
            api_endpoint: "https://api.individual.githubcopilot.com".to_string(),
            updated_at: 1729000000,
        };

        engine.save_credentials(&creds).unwrap();
        let loaded = engine.load_credentials().unwrap().expect("Loaded creds");
        assert_eq!(loaded.github_token, "ghu_test_token");
        assert_eq!(loaded.copilot_token, "tid_test_token");

        let _ = std::fs::remove_file(test_path);
    }
}
