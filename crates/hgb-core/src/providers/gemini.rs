//! Google Gemini Provider for Hagibis (hgb)
//!
//! Supports both standard Google AI Studio API Keys (`GEMINI_API_KEY`)
//! and Google AntiGravity OAuth 2.0 Web Authentication tokens.

use crate::auth::GeminiOAuthManager;
use crate::error::{HgbError, Result};
use crate::traits::HgbProvider;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub enum GeminiAuth {
    ApiKey(String),
    OAuth(GeminiOAuthManager),
}

pub struct GeminiProvider {
    auth: GeminiAuth,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new_with_api_key(api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            auth: GeminiAuth::ApiKey(api_key.into()),
            client,
        }
    }

    pub fn new_with_oauth(oauth_mgr: GeminiOAuthManager) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            auth: GeminiAuth::OAuth(oauth_mgr),
            client,
        }
    }

    /// Automatically discover available credentials (API key first, then Google OAuth session)
    pub fn auto_discover() -> Option<Self> {
        // 1. Check GEMINI_API_KEY environment variable
        if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return Some(Self::new_with_api_key(trimmed));
            }
        }

        // 2. Check stored Google OAuth session
        if GeminiOAuthManager::is_authenticated() {
            let oauth_mgr = GeminiOAuthManager::new();
            return Some(Self::new_with_oauth(oauth_mgr));
        }

        None
    }

    /// Check if either API key or OAuth credentials are ready
    pub fn is_available() -> bool {
        if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return true;
            }
        }
        GeminiOAuthManager::is_authenticated()
    }

    /// Get current credential description (email or API key indicator)
    pub fn credential_status() -> String {
        if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return "Google API Key (GEMINI_API_KEY)".to_string();
            }
        }
        if let Some(email) = GeminiOAuthManager::get_account_email() {
            return format!("Google OAuth ({email})");
        }
        if GeminiOAuthManager::is_authenticated() {
            return "Google OAuth (Authenticated)".to_string();
        }
        "Unauthenticated (run 'hgb login' or export GEMINI_API_KEY)".to_string()
    }

    /// Normalize model aliases
    pub fn sanitize_model(model: Option<&str>) -> &'static str {
        match model.unwrap_or("auto").trim().to_lowercase().as_str() {
            "" | "auto" | "default" | "gemini" | "flash" | "gemini-flash" | "gemini-2.5-flash" => "gemini-2.5-flash",
            "pro" | "gemini-pro" | "gemini-2.5-pro" => "gemini-2.5-pro",
            "lite" | "flash-lite" | "gemini-flash-lite" | "gemini-2.5-flash-lite" => "gemini-2.5-flash-lite",
            "gemini-1.5-pro" => "gemini-1.5-pro",
            "gemini-1.5-flash" => "gemini-1.5-flash",
            _ => "gemini-2.5-flash",
        }
    }
}

#[derive(Serialize)]
struct ContentPart {
    text: String,
}

#[derive(Serialize)]
struct ContentMessage {
    role: String,
    parts: Vec<ContentPart>,
}

#[derive(Serialize)]
struct StandardGeminiPayload {
    contents: Vec<ContentMessage>,
}

#[derive(Serialize)]
struct CloudCodeGeminiPayload<'a> {
    project: &'static str,
    model: &'a str,
    request: StandardGeminiPayload,
}

#[derive(Deserialize, Debug)]
struct GeminiPartResponse {
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GeminiContentResponse {
    parts: Option<Vec<GeminiPartResponse>>,
}

#[derive(Deserialize, Debug)]
struct GeminiCandidateResponse {
    content: Option<GeminiContentResponse>,
}

#[derive(Deserialize, Debug)]
struct GeminiOuterResponse {
    candidates: Option<Vec<GeminiCandidateResponse>>,
    #[serde(default)]
    response: Option<Box<GeminiOuterResponse>>,
}

#[async_trait]
impl HgbProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let model_name = Self::sanitize_model(model);
        let contents = vec![ContentMessage {
            role: "user".to_string(),
            parts: vec![ContentPart {
                text: prompt.to_string(),
            }],
        }];

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let (url, body_json) = match &self.auth {
            GeminiAuth::ApiKey(key) => {
                let u = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                    model_name, key
                );
                let payload = StandardGeminiPayload { contents };
                let body = serde_json::to_string(&payload)
                    .map_err(|e| HgbError::Serialization(e.to_string()))?;
                (u, body)
            }
            GeminiAuth::OAuth(mgr) => {
                let mut mgr_clone = mgr.clone();
                let access_token = mgr_clone.get_valid_access_token().await?;
                let auth_header = HeaderValue::from_str(&format!("Bearer {}", access_token))
                    .map_err(|e| HgbError::Authentication(format!("Invalid auth header: {e}")))?;
                headers.insert(AUTHORIZATION, auth_header);
                headers.insert(USER_AGENT, HeaderValue::from_static("antigravity/2.0.0"));

                let u = "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent".to_string();
                let payload = CloudCodeGeminiPayload {
                    project: "",
                    model: model_name,
                    request: StandardGeminiPayload { contents },
                };
                let body = serde_json::to_string(&payload)
                    .map_err(|e| HgbError::Serialization(e.to_string()))?;
                (u, body)
            }
        };

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .body(body_json)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to connect to Gemini API: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let err_text = response.text().await.unwrap_or_default();
            return Err(HgbError::Network(format!(
                "Gemini API returned HTTP {}: {}",
                status, err_text
            )));
        }

        let resp_json: GeminiOuterResponse = response
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Failed to parse Gemini response: {e}")))?;

        // Check top-level candidates or nested response.candidates
        let candidates = resp_json
            .candidates
            .or_else(|| resp_json.response.and_then(|r| r.candidates));

        if let Some(cands) = candidates {
            if let Some(first_cand) = cands.into_iter().next() {
                if let Some(cnt) = first_cand.content {
                    if let Some(parts) = cnt.parts {
                        let text: String = parts
                            .into_iter()
                            .filter_map(|p| p.text)
                            .collect::<Vec<String>>()
                            .join("\n");
                        if !text.is_empty() {
                            return Ok(text);
                        }
                    }
                }
            }
        }

        Err(HgbError::Execution("No output content returned by Gemini".to_string()))
    }
}
