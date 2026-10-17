//! # OpenAI Codex CLI Web Authentication Engine for Hagibis (hgb)
//!
//! Provides production-grade integration with Codex CLI's ChatGPT Web Authentication:
//! - Auto-discovery & 1-click import from `~/.codex/auth.json` (or `$CODEX_HOME/auth.json`)
//! - Persistent credentials cache in `~/.hgb/codex_auth.json` and `~/.hgb/credentials.json`
//! - Automated token refresh using `https://auth.openai.com/oauth/token` (RFC 6749)
//! - Model catalog retrieval from `https://chatgpt.com/backend-api/codex/models`
//! - Direct prompt & streaming completion execution on `https://chatgpt.com/backend-api/codex/responses`
//! - Seamless dual-stack failover and mapping for `gpt-6-astra`, `gpt-6.1-sol`, `gpt-5.6-sol`, and `gpt-4o`

use crate::error::{HgbError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ORIGIN, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models?client_version=0.163.0";
pub const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const CODEX_USER_AGENT: &str = "codex-cli/0.163.0";
pub const CODEX_ORIGIN: &str = "https://chatgpt.com";

/// Persisted Codex CLI credentials
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub id_token: Option<String>,
    pub email: Option<String>,
    pub expires_at: Option<i64>,
    pub last_refreshed: Option<String>,
    pub client_id: String,
}

impl CodexCredentials {
    /// Check if the access token is expired or close to expiring (within 120 seconds)
    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            let now = Utc::now().timestamp();
            exp <= (now + 120)
        } else {
            false
        }
    }
}

/// Model descriptor returned by Codex model catalog
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexModel {
    pub slug: String,
    pub title: Option<String>,
    pub visibility: Option<String>,
}

/// Token refresh response from https://auth.openai.com/oauth/token
#[derive(Debug, Deserialize)]
struct OpenAIAuthTokenResponse {
    access_token: String,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

/// Production-grade Codex CLI Authentication & Inference Engine
#[derive(Clone)]
pub struct CodexAuthEngine {
    client: reqwest::Client,
}

impl Default for CodexAuthEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAuthEngine {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    /// Resolve the default CODEX_HOME directory
    pub fn codex_home_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("CODEX_HOME") {
            let p = PathBuf::from(dir);
            if !p.as_os_str().is_empty() {
                return p;
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".codex")
    }

    /// Resolve Hagibis persistent credentials directory
    pub fn hgb_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".hgb")
    }

    /// Extract email and exp claims from a JWT token
    pub fn extract_jwt_claims(token: &str) -> (Option<String>, Option<i64>) {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() >= 2 {
            let payload_b64 = parts[1];
            let pad_len = (4 - (payload_b64.len() % 4)) % 4;
            let padded = format!("{}{}", payload_b64, "=".repeat(pad_len));
            if let Ok(bytes) = URL_SAFE_NO_PAD.decode(padded.as_bytes()) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    let email = v.get("email").and_then(|e| e.as_str()).map(String::from);
                    let exp = v.get("exp").and_then(|x| x.as_i64());
                    return (email, exp);
                }
            }
        }
        (None, None)
    }

    /// Read credentials directly from Codex CLI (`~/.codex/auth.json`)
    pub fn load_from_codex_cli() -> Option<CodexCredentials> {
        let auth_file = Self::codex_home_dir().join("auth.json");
        if !auth_file.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&auth_file).ok()?;
        let v: serde_json::Value = serde_json::from_str(&content).ok()?;
        let tokens = v.get("tokens")?;

        let access_token = tokens.get("access_token")?.as_str()?.to_string();
        if access_token.is_empty() {
            return None;
        }

        let refresh_token = tokens.get("refresh_token").and_then(|x| x.as_str()).map(String::from);
        let account_id = tokens.get("account_id").and_then(|x| x.as_str()).map(String::from);
        let id_token = tokens.get("id_token").and_then(|x| x.as_str()).map(String::from);

        let (mut email, mut expires_at) = (None, None);
        if let Some(id_tok) = &id_token {
            let (e, exp) = Self::extract_jwt_claims(id_tok);
            email = e;
            expires_at = exp;
        }
        if email.is_none() || expires_at.is_none() {
            let (e, exp) = Self::extract_jwt_claims(&access_token);
            if email.is_none() {
                email = e;
            }
            if expires_at.is_none() {
                expires_at = exp;
            }
        }

        let last_refreshed = v.get("last_refresh")
            .and_then(|s| s.as_str())
            .map(String::from)
            .or_else(|| Some(Utc::now().to_rfc3339()));

        Some(CodexCredentials {
            access_token,
            refresh_token,
            account_id,
            id_token,
            email,
            expires_at,
            last_refreshed,
            client_id: CODEX_CLIENT_ID.to_string(),
        })
    }

    /// Save credentials into Hagibis persistent storage (`~/.hgb/codex_auth.json` and `~/.hgb/credentials.json`)
    pub fn save_credentials(creds: &CodexCredentials) -> Result<()> {
        let hgb_dir = Self::hgb_dir();
        std::fs::create_dir_all(&hgb_dir).map_err(HgbError::Io)?;

        // 1. Save dedicated codex_auth.json
        let dedicated_path = hgb_dir.join("codex_auth.json");
        let json_str = serde_json::to_string_pretty(creds)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        std::fs::write(&dedicated_path, json_str).map_err(HgbError::Io)?;

        // 2. Also update ~/.hgb/credentials.json for unified discoverability
        let creds_file = hgb_dir.join("credentials.json");
        let mut map: serde_json::Map<String, serde_json::Value> = if creds_file.exists() {
            std::fs::read_to_string(&creds_file)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };

        map.insert("codex_access_token".to_string(), serde_json::Value::String(creds.access_token.clone()));
        if let Some(ref r) = creds.refresh_token {
            map.insert("codex_refresh_token".to_string(), serde_json::Value::String(r.clone()));
        }
        if let Some(ref a) = creds.account_id {
            map.insert("codex_account_id".to_string(), serde_json::Value::String(a.clone()));
        }
        if let Some(ref e) = creds.email {
            map.insert("codex_email".to_string(), serde_json::Value::String(e.clone()));
        }

        let updated = serde_json::to_string_pretty(&map)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        std::fs::write(&creds_file, updated).map_err(HgbError::Io)?;

        Ok(())
    }

    /// Load credentials from all known persistent locations (hgb storage -> codex CLI auto-import)
    pub fn load_credentials() -> Option<CodexCredentials> {
        // 1. Check ~/.hgb/codex_auth.json
        let dedicated = Self::hgb_dir().join("codex_auth.json");
        if dedicated.exists() {
            if let Ok(c) = std::fs::read_to_string(&dedicated) {
                if let Ok(creds) = serde_json::from_str::<CodexCredentials>(&c) {
                    if !creds.access_token.trim().is_empty() {
                        return Some(creds);
                    }
                }
            }
        }

        // 2. Auto-import from ~/.codex/auth.json
        if let Some(creds) = Self::load_from_codex_cli() {
            let _ = Self::save_credentials(&creds);
            return Some(creds);
        }

        // 3. Fallback to ~/.hgb/credentials.json
        let creds_file = Self::hgb_dir().join("credentials.json");
        if creds_file.exists() {
            if let Ok(c) = std::fs::read_to_string(&creds_file) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&c) {
                    if let Some(tok) = v.get("codex_access_token").and_then(|t| t.as_str()) {
                        if !tok.trim().is_empty() {
                            let account_id = v.get("codex_account_id").and_then(|a| a.as_str()).map(String::from);
                            let refresh_token = v.get("codex_refresh_token").and_then(|r| r.as_str()).map(String::from);
                            let email = v.get("codex_email").and_then(|e| e.as_str()).map(String::from);
                            let (e2, exp) = Self::extract_jwt_claims(tok);
                            return Some(CodexCredentials {
                                access_token: tok.to_string(),
                                refresh_token,
                                account_id,
                                id_token: None,
                                email: email.or(e2),
                                expires_at: exp,
                                last_refreshed: None,
                                client_id: CODEX_CLIENT_ID.to_string(),
                            });
                        }
                    }
                }
            }
        }

        None
    }

    /// Check if Codex Web Authentication is available and ready
    pub fn is_available() -> bool {
        Self::load_credentials().is_some()
    }

    /// Refresh token using auth.openai.com
    pub async fn refresh_tokens(&self, creds: &mut CodexCredentials) -> Result<bool> {
        let refresh_token = match &creds.refresh_token {
            Some(r) if !r.trim().is_empty() => r.clone(),
            _ => return Ok(false),
        };

        let body = serde_json::json!({
            "client_id": &creds.client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        });

        let resp = self.client.post(CODEX_OAUTH_TOKEN_URL)
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to contact OpenAI auth endpoint: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(HgbError::Provider(format!("OpenAI token refresh failed ({}): {}", status, err_text)));
        }

        let parsed: OpenAIAuthTokenResponse = resp.json().await
            .map_err(|e| HgbError::Serialization(format!("Invalid refresh token response: {}", e)))?;

        creds.access_token = parsed.access_token.clone();
        if let Some(r) = parsed.refresh_token {
            creds.refresh_token = Some(r);
        }
        if let Some(id) = parsed.id_token {
            creds.id_token = Some(id);
        }
        if let Some(exp_in) = parsed.expires_in {
            creds.expires_at = Some(Utc::now().timestamp() + exp_in);
        }
        creds.last_refreshed = Some(Utc::now().to_rfc3339());

        // Update persistence in ~/.hgb and ~/.codex/auth.json
        let _ = Self::save_credentials(creds);
        let _ = Self::sync_back_to_codex_cli(creds);

        Ok(true)
    }

    /// Synchronize refreshed tokens back into ~/.codex/auth.json so Codex CLI also stays synchronized
    pub fn sync_back_to_codex_cli(creds: &CodexCredentials) -> Result<()> {
        let auth_file = Self::codex_home_dir().join("auth.json");
        if !auth_file.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&auth_file).map_err(HgbError::Io)?;
        let mut v: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        if let Some(tokens) = v.get_mut("tokens").and_then(|t| t.as_object_mut()) {
            tokens.insert("access_token".to_string(), serde_json::Value::String(creds.access_token.clone()));
            if let Some(ref r) = creds.refresh_token {
                tokens.insert("refresh_token".to_string(), serde_json::Value::String(r.clone()));
            }
            if let Some(ref id) = creds.id_token {
                tokens.insert("id_token".to_string(), serde_json::Value::String(id.clone()));
            }
            if let Some(ref a) = creds.account_id {
                tokens.insert("account_id".to_string(), serde_json::Value::String(a.clone()));
            }
        }
        if let Some(ref lr) = creds.last_refreshed {
            v["last_refresh"] = serde_json::Value::String(lr.clone());
        }

        let updated = serde_json::to_string_pretty(&v)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        std::fs::write(&auth_file, updated).map_err(HgbError::Io)?;
        Ok(())
    }

    /// Fetch list of available models from Codex backend
    pub async fn fetch_models(&self) -> Result<Vec<CodexModel>> {
        let mut creds = Self::load_credentials()
            .ok_or_else(|| HgbError::Provider("No Codex credentials available".to_string()))?;

        let mut req = self.client.get(CODEX_MODELS_URL)
            .header(AUTHORIZATION, format!("Bearer {}", creds.access_token))
            .header(USER_AGENT, CODEX_USER_AGENT)
            .header(ORIGIN, CODEX_ORIGIN);

        if let Some(ref acc_id) = creds.account_id {
            req = req.header("ChatGPT-Account-Id", acc_id);
        }

        let mut resp = req.send().await
            .map_err(|e| HgbError::Network(format!("Failed to connect to Codex models endpoint: {}", e)))?;

        // If 401 Unauthorized, attempt refresh once
        if resp.status().as_u16() == 401 {
            if let Ok(true) = self.refresh_tokens(&mut creds).await {
                let mut retry_req = self.client.get(CODEX_MODELS_URL)
                    .header(AUTHORIZATION, format!("Bearer {}", creds.access_token))
                    .header(USER_AGENT, CODEX_USER_AGENT)
                    .header(ORIGIN, CODEX_ORIGIN);
                if let Some(ref acc_id) = creds.account_id {
                    retry_req = retry_req.header("ChatGPT-Account-Id", acc_id);
                }
                resp = retry_req.send().await
                    .map_err(|e| HgbError::Network(format!("Retry failed: {}", e)))?;
            }
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(HgbError::Provider(format!("Codex models error ({}): {}", status, err)));
        }

        #[derive(Deserialize)]
        struct ModelsEnvelope {
            #[serde(default)]
            models: Vec<CodexModel>,
        }

        let envelope: ModelsEnvelope = resp.json().await
            .map_err(|e| HgbError::Serialization(format!("Failed to decode models json: {}", e)))?;

        Ok(envelope.models)
    }

    /// Map requested model slug to an active Codex backend model
    pub fn map_model_slug(requested: &str) -> &'static str {
        let lower = requested.trim().to_lowercase();
        match lower.as_str() {
            "gpt-6-astra" => "gpt-6-astra",
            "gpt-6.1-sol" | "gpt-6.1" => "gpt-6.1-sol",
            "gpt-6-sol" => "gpt-6-sol",
            "gpt-6-luna" => "gpt-6-luna",
            "gpt-5.6-sol" => "gpt-5.6-sol",
            "gpt-5.6-terra" => "gpt-5.6-terra",
            "gpt-5.6-luna" => "gpt-5.6-luna",
            // Standard OpenAI aliases map cleanly to the top tier coding model on Codex
            "gpt-4o" | "gpt-4o-mini" | "o3-mini" | "o1" | "o1-mini" | "gpt-4-turbo" | "chatgpt-4o-latest" | "auto" | "default" => "gpt-6-astra",
            _ => "gpt-6-astra",
        }
    }

    /// Execute a chat/prompt completion using Codex Responses API (`https://chatgpt.com/backend-api/codex/responses`)
    pub async fn chat_completion(&self, model: &str, prompt: &str) -> Result<String> {
        let mut creds = Self::load_credentials()
            .ok_or_else(|| HgbError::Provider("No Codex web authentication credentials configured. Run 'hgb auth-codex'".to_string()))?;

        let effective_model = Self::map_model_slug(model);

        let payload = serde_json::json!({
            "model": effective_model,
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": prompt}]
                }
            ],
            "store": false,
            "stream": true
        });

        let mut req = self.client.post(CODEX_RESPONSES_URL)
            .header(AUTHORIZATION, format!("Bearer {}", creds.access_token))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "text/event-stream")
            .header(USER_AGENT, CODEX_USER_AGENT)
            .header(ORIGIN, CODEX_ORIGIN)
            .json(&payload);

        if let Some(ref acc_id) = creds.account_id {
            req = req.header("ChatGPT-Account-Id", acc_id);
        }

        let mut resp = req.send().await
            .map_err(|e| HgbError::Network(format!("Failed to connect to Codex responses backend: {}", e)))?;

        // Handle token expiration & retry
        if resp.status().as_u16() == 401 {
            if let Ok(true) = self.refresh_tokens(&mut creds).await {
                let mut retry_req = self.client.post(CODEX_RESPONSES_URL)
                    .header(AUTHORIZATION, format!("Bearer {}", creds.access_token))
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "text/event-stream")
                    .header(USER_AGENT, CODEX_USER_AGENT)
                    .header(ORIGIN, CODEX_ORIGIN)
                    .json(&payload);

                if let Some(ref acc_id) = creds.account_id {
                    retry_req = retry_req.header("ChatGPT-Account-Id", acc_id);
                }

                resp = retry_req.send().await
                    .map_err(|e| HgbError::Network(format!("Retry failed: {}", e)))?;
            }
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(HgbError::Provider(format!("Codex API returned error ({}): {}", status, err_text)));
        }

        // Process Server-Sent Events (SSE)
        let mut full_text = String::new();
        let bytes = resp.bytes().await
            .map_err(|e| HgbError::Network(format!("Failed to read response stream: {}", e)))?;

        let stream_str = String::from_utf8_lossy(&bytes);
        for line in stream_str.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("data: ") {
                let data_part = &trimmed[6..];
                if data_part == "[DONE]" {
                    break;
                }
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data_part) {
                    let ev_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match ev_type {
                        "response.output_item.delta" => {
                            if let Some(txt) = val.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                                full_text.push_str(txt);
                            }
                        }
                        "response.output_item.done" => {
                            if let Some(item) = val.get("item") {
                                if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                                    if let Some(contents) = item.get("content").and_then(|c| c.as_array()) {
                                        for c in contents {
                                            if c.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                                                if let Some(txt) = c.get("text").and_then(|t| t.as_str()) {
                                                    if full_text.is_empty() {
                                                        full_text.push_str(txt);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if full_text.trim().is_empty() {
            Ok("Received empty response from Codex backend.".to_string())
        } else {
            Ok(full_text)
        }
    }

    /// One-shot import, test, and diagnostics
    pub async fn import_and_verify() -> Result<String> {
        let creds = Self::load_from_codex_cli().ok_or_else(|| {
            HgbError::Provider("No ~/.codex/auth.json file found. Please run 'codex login' or login with ChatGPT.".to_string())
        })?;

        Self::save_credentials(&creds)?;

        let engine = Self::new();
        let models = match engine.fetch_models().await {
            Ok(m) => m,
            Err(e) => {
                return Ok(format!(
                    "Codex authentication imported, but model verification returned notice: {}\nAccount ID: {}\nUser Email: {}",
                    e,
                    creds.account_id.as_deref().unwrap_or("none"),
                    creds.email.as_deref().unwrap_or("unknown")
                ));
            }
        };

        let visible_models: Vec<String> = models
            .into_iter()
            .filter(|m| m.visibility.as_deref() != Some("hide"))
            .map(|m| m.slug)
            .collect();

        Ok(format!(
            "Successfully connected OpenAI Codex Web Authentication!\n\
             • User Email:  {}\n\
             • Account ID:  {}\n\
             • Active Models: {}\n\
             • Primary Model: gpt-6-astra\n\
             • Status: Ready for prompts & Cockpit routing",
            creds.email.as_deref().unwrap_or("Authenticated"),
            creds.account_id.as_deref().unwrap_or("Personal Account"),
            visible_models.join(", ")
        ))
    }
}
