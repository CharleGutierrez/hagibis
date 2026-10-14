//! # Azure OpenAI Service Provider for Hagibis
//!
//! Provides a production-grade enterprise LLM client targeting Microsoft Azure OpenAI Service:
//! `https://{resource}.openai.azure.com/openai/deployments/{deployment_id}/chat/completions?api-version={api_version}`
//!
//! Supports both standard Azure API keys (`api-key` header) and Microsoft Entra ID Bearer tokens
//! (`Authorization: Bearer <token>`).
//!
//! Complies with the `HgbProvider` trait.

use crate::error::{HgbError, Result};
use crate::traits::HgbProvider;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_AZURE_API_VERSION: &str = "2024-08-01-preview";

/// Authentication mechanism for Azure OpenAI
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AzureAuth {
    ApiKey(String),
    EntraBearer(String),
}

/// Token usage metadata returned by Azure OpenAI
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AzureTokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Serialize)]
struct AzureChatRequestBody<'a> {
    messages: Vec<AzureChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct AzureChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize, Debug)]
struct AzureChatResponseBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    choices: Vec<AzureChatChoice>,
    #[serde(default)]
    usage: Option<AzureTokenUsageResponse>,
    #[serde(default)]
    error: Option<AzureErrorPayload>,
}

#[derive(Deserialize, Debug)]
struct AzureChatChoice {
    #[serde(default)]
    message: Option<AzureMessageContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AzureMessageContent {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AzureTokenUsageResponse {
    #[serde(default)]
    prompt_tokens: usize,
    #[serde(default)]
    completion_tokens: usize,
    #[serde(default)]
    total_tokens: usize,
}

#[derive(Deserialize, Debug)]
struct AzureErrorPayload {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Production client for Azure OpenAI Service
#[derive(Clone)]
pub struct AzureOpenAiProvider {
    resource_name: String,
    deployment_id: String,
    api_version: String,
    auth: AzureAuth,
    custom_endpoint: Option<String>,
    client: reqwest::Client,
}

impl AzureOpenAiProvider {
    /// Create provider with standard Azure API key
    pub fn new(
        resource_name: impl Into<String>,
        deployment_id: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::new_with_auth(
            resource_name,
            deployment_id,
            AzureAuth::ApiKey(api_key.into()),
            DEFAULT_AZURE_API_VERSION,
        )
    }

    /// Create provider with Microsoft Entra ID Bearer token
    pub fn new_with_bearer(
        resource_name: impl Into<String>,
        deployment_id: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Self {
        Self::new_with_auth(
            resource_name,
            deployment_id,
            AzureAuth::EntraBearer(bearer_token.into()),
            DEFAULT_AZURE_API_VERSION,
        )
    }

    /// Full constructor specifying auth mode and api-version
    pub fn new_with_auth(
        resource_name: impl Into<String>,
        deployment_id: impl Into<String>,
        auth: AzureAuth,
        api_version: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            resource_name: resource_name.into(),
            deployment_id: deployment_id.into(),
            api_version: api_version.into(),
            auth,
            custom_endpoint: None,
            client,
        }
    }

    /// Set a custom endpoint override (e.g. for enterprise API gateways or proxies)
    pub fn with_custom_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.custom_endpoint = Some(endpoint.into());
        self
    }

    /// Auto-discover Azure OpenAI credentials from environment variables:
    /// - `AZURE_OPENAI_RESOURCE`
    /// - `AZURE_OPENAI_DEPLOYMENT`
    /// - `AZURE_OPENAI_API_KEY` or `AZURE_OPENAI_BEARER_TOKEN`
    /// - `AZURE_OPENAI_API_VERSION` (optional)
    pub fn auto_discover() -> Option<Self> {
        let resource = std::env::var("AZURE_OPENAI_RESOURCE").ok()?;
        let deployment = std::env::var("AZURE_OPENAI_DEPLOYMENT").unwrap_or_else(|_| "gpt-4o".to_string());
        let version = std::env::var("AZURE_OPENAI_API_VERSION").unwrap_or_else(|_| DEFAULT_AZURE_API_VERSION.to_string());

        let auth = if let Ok(key) = std::env::var("AZURE_OPENAI_API_KEY") {
            if !key.trim().is_empty() {
                AzureAuth::ApiKey(key.trim().to_string())
            } else {
                return None;
            }
        } else if let Ok(token) = std::env::var("AZURE_OPENAI_BEARER_TOKEN") {
            if !token.trim().is_empty() {
                AzureAuth::EntraBearer(token.trim().to_string())
            } else {
                return None;
            }
        } else {
            return None;
        };

        let mut prov = Self::new_with_auth(resource, deployment, auth, version);
        if let Ok(custom) = std::env::var("AZURE_OPENAI_ENDPOINT") {
            prov = prov.with_custom_endpoint(custom);
        }
        Some(prov)
    }

    pub fn is_available() -> bool {
        Self::auto_discover().is_some()
    }

    pub fn credential_status(&self) -> String {
        match &self.auth {
            AzureAuth::ApiKey(k) => {
                let masked = if k.len() > 8 {
                    format!("{}...{}", &k[..4], &k[k.len() - 4..])
                } else {
                    "configured".to_string()
                };
                format!("Azure API Key ({}, deployment: {})", masked, self.deployment_id)
            }
            AzureAuth::EntraBearer(_) => {
                format!("Microsoft Entra ID Bearer Token (deployment: {})", self.deployment_id)
            }
        }
    }

    /// Build the full Azure OpenAI REST endpoint URL
    pub fn endpoint_url(&self) -> String {
        if let Some(ref custom) = self.custom_endpoint {
            if custom.contains("?") {
                format!("{}&api-version={}", custom, self.api_version)
            } else {
                format!("{}?api-version={}", custom, self.api_version)
            }
        } else {
            format!(
                "https://{}.openai.azure.com/openai/deployments/{}/chat/completions?api-version={}",
                self.resource_name.trim(),
                self.deployment_id.trim(),
                self.api_version.trim()
            )
        }
    }

    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    pub fn auth(&self) -> &AzureAuth {
        &self.auth
    }

    /// Perform a full chat completion and return the output text alongside token accounting
    pub async fn complete_with_usage(
        &self,
        prompt: &str,
        system_instruction: Option<&str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<(String, AzureTokenUsage)> {
        let mut messages = Vec::new();
        if let Some(sys) = system_instruction {
            messages.push(AzureChatMessage {
                role: "system",
                content: sys,
            });
        }
        messages.push(AzureChatMessage {
            role: "user",
            content: prompt,
        });

        let payload = AzureChatRequestBody {
            messages,
            temperature,
            max_tokens,
        };

        let body_json = serde_json::to_string(&payload)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("hagibis/0.1.0 (Azure-OpenAI-Engine)"),
        );

        match &self.auth {
            AzureAuth::ApiKey(key) => {
                let hval = HeaderValue::from_str(key)
                    .map_err(|e| HgbError::Authentication(format!("Invalid Azure API key header: {e}")))?;
                headers.insert("api-key", hval);
            }
            AzureAuth::EntraBearer(token) => {
                let hval = HeaderValue::from_str(&format!("Bearer {}", token))
                    .map_err(|e| HgbError::Authentication(format!("Invalid Entra Bearer token: {e}")))?;
                headers.insert(AUTHORIZATION, hval);
            }
        }

        let url = self.endpoint_url();
        let response = self.client.post(&url)
            .headers(headers)
            .body(body_json)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to connect to Azure OpenAI at {}: {e}", url)))?;

        let status = response.status();
        if !status.is_success() {
            let err_text = response.text().await.unwrap_or_default();
            if let Ok(parsed) = serde_json::from_str::<AzureChatResponseBody>(&err_text) {
                if let Some(err) = parsed.error {
                    return Err(HgbError::Provider(format!(
                        "Azure OpenAI error [{}] (HTTP {}): {}",
                        err.code.unwrap_or_else(|| "Unknown".to_string()),
                        status,
                        err.message.unwrap_or_default()
                    )));
                }
            }
            return Err(HgbError::Network(format!(
                "Azure OpenAI returned HTTP {}: {}",
                status, err_text
            )));
        }

        let resp_json: AzureChatResponseBody = response
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Failed to parse Azure OpenAI response: {e}")))?;

        let usage = resp_json.usage.map(|u| AzureTokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }).unwrap_or_default();

        if let Some(first_choice) = resp_json.choices.into_iter().next() {
            if let Some(msg) = first_choice.message {
                if let Some(cnt) = msg.content {
                    if !cnt.trim().is_empty() {
                        return Ok((cnt, usage));
                    }
                }
            }
        }

        Err(HgbError::Execution("No content generated by Azure OpenAI model".to_string()))
    }
}

#[async_trait]
impl HgbProvider for AzureOpenAiProvider {
    fn name(&self) -> &str {
        "azure-openai"
    }

    async fn complete(&self, prompt: &str, _model: Option<&str>) -> Result<String> {
        let (output, _) = self.complete_with_usage(prompt, None, None, None).await?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_azure_endpoint_url_standard() {
        let provider = AzureOpenAiProvider::new("contoso-eastus", "gpt-4o", "secret_key_123");
        assert_eq!(
            provider.endpoint_url(),
            "https://contoso-eastus.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2024-08-01-preview"
        );
        assert_eq!(provider.name(), "azure-openai");
    }

    #[test]
    fn test_azure_custom_endpoint() {
        let provider = AzureOpenAiProvider::new("contoso-eastus", "gpt-4o", "secret_key_123")
            .with_custom_endpoint("https://api-gateway.contoso.com/v1/chat");
        assert_eq!(
            provider.endpoint_url(),
            "https://api-gateway.contoso.com/v1/chat?api-version=2024-08-01-preview"
        );
    }

    #[test]
    fn test_azure_parse_success_response() {
        let raw = r#"{
            "id": "chatcmpl-azure-98213",
            "choices": [
                {
                    "message": {
                        "content": "Contoso internal architecture approved."
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 42,
                "completion_tokens": 12,
                "total_tokens": 54
            }
        }"#;

        let parsed: AzureChatResponseBody = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.choices.len(), 1);
        let content = parsed.choices[0].message.as_ref().unwrap().content.as_deref().unwrap();
        assert_eq!(content, "Contoso internal architecture approved.");
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.total_tokens, 54);
    }

    #[test]
    fn test_azure_parse_error_response() {
        let raw = r#"{
            "error": {
                "code": "content_filter",
                "message": "The response was filtered due to the prompt triggering Azure OpenAI content management policy."
            }
        }"#;

        let parsed: AzureChatResponseBody = serde_json::from_str(raw).unwrap();
        let err = parsed.error.unwrap();
        assert_eq!(err.code.as_deref(), Some("content_filter"));
        assert!(err.message.unwrap().contains("Azure OpenAI content management policy"));
    }

    #[test]
    fn test_azure_auth_bearer_vs_api_key() {
        let p1 = AzureOpenAiProvider::new("res", "dep", "key123");
        assert!(matches!(p1.auth(), AzureAuth::ApiKey(k) if k == "key123"));

        let p2 = AzureOpenAiProvider::new_with_bearer("res", "dep", "token_xyz");
        assert!(matches!(p2.auth(), AzureAuth::EntraBearer(t) if t == "token_xyz"));
    }
}
