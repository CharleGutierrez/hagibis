//! OpenAI-Compatible Provider for Hagibis (hgb)
//!
//! Provides a production-grade HTTP client implementation for:
//! - OpenAI (`https://api.openai.com/v1/chat/completions`)
//! - DeepSeek (`https://api.deepseek.com/chat/completions`)
//! - OpenRouter (`https://openrouter.ai/api/v1/chat/completions`)
//! - Any custom OpenAI-compatible endpoint (vLLM, Ollama OpenAI compatibility, LiteLLM, Groq, etc.)
//!
//! Complies with the `HgbProvider` trait.

use crate::error::{HgbError, Result};
use crate::traits::HgbProvider;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::time::Duration;

/// Flavor of the OpenAI-compatible endpoint
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenAiCompatibleFlavor {
    OpenAi,
    DeepSeek,
    OpenRouter,
    Custom(String),
}

impl OpenAiCompatibleFlavor {
    pub fn default_endpoint(&self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1/chat/completions",
            Self::DeepSeek => "https://api.deepseek.com/chat/completions",
            Self::OpenRouter => "https://openrouter.ai/api/v1/chat/completions",
            Self::Custom(_) => "http://127.0.0.1:8000/v1/chat/completions",
        }
    }

    pub fn provider_name(&self) -> &str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Custom(name) => name.as_str(),
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-4o",
            Self::DeepSeek => "deepseek-chat",
            Self::OpenRouter => "deepseek/deepseek-chat",
            Self::Custom(_) => "default",
        }
    }
}

/// A single message in a chat completion request
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
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

#[derive(Serialize)]
struct ChatCompletionRequestBody<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct ChatCompletionResponseBody {
    #[serde(default)]
    choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    error: Option<ApiErrorPayload>,
}

#[derive(Deserialize, Debug)]
struct ChatCompletionChoice {
    #[serde(default)]
    message: Option<ChatCompletionMessageContent>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ChatCompletionMessageContent {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ApiErrorPayload {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    code: Option<serde_json::Value>,
}

/// Real HTTP client for OpenAI-compatible LLM endpoints
#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    flavor: OpenAiCompatibleFlavor,
    api_key: String,
    endpoint_url: String,
    provider_name: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    /// Create a new provider with the specified flavor and API key using standard endpoint
    pub fn new(flavor: OpenAiCompatibleFlavor, api_key: impl Into<String>) -> Self {
        let endpoint = flavor.default_endpoint().to_string();
        let name = flavor.provider_name().to_string();
        Self::new_with_url(flavor, api_key, endpoint, name)
    }

    /// Create a new provider with an explicit endpoint URL and provider identifier
    pub fn new_with_url(
        flavor: OpenAiCompatibleFlavor,
        api_key: impl Into<String>,
        endpoint_url: impl Into<String>,
        provider_name: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            flavor,
            api_key: api_key.into(),
            endpoint_url: endpoint_url.into(),
            provider_name: provider_name.into(),
            client,
        }
    }

    /// Construct provider configured for official OpenAI API
    pub fn openai(api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        let endpoint = if let Ok(base) = std::env::var("OPENAI_BASE_URL") {
            Self::normalize_chat_endpoint(&base)
        } else {
            OpenAiCompatibleFlavor::OpenAi.default_endpoint().to_string()
        };
        Self::new_with_url(OpenAiCompatibleFlavor::OpenAi, key, endpoint, "openai")
    }

    /// Construct provider configured for DeepSeek API
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        let endpoint = if let Ok(base) = std::env::var("DEEPSEEK_BASE_URL") {
            Self::normalize_chat_endpoint(&base)
        } else {
            OpenAiCompatibleFlavor::DeepSeek.default_endpoint().to_string()
        };
        Self::new_with_url(OpenAiCompatibleFlavor::DeepSeek, key, endpoint, "deepseek")
    }

    /// Construct provider configured for OpenRouter API
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        let endpoint = if let Ok(base) = std::env::var("OPENROUTER_BASE_URL") {
            Self::normalize_chat_endpoint(&base)
        } else {
            OpenAiCompatibleFlavor::OpenRouter.default_endpoint().to_string()
        };
        Self::new_with_url(OpenAiCompatibleFlavor::OpenRouter, key, endpoint, "openrouter")
    }

    /// Construct provider configured for a custom endpoint
    pub fn custom(api_key: impl Into<String>, endpoint_url: impl Into<String>, name: impl Into<String>) -> Self {
        let n = name.into();
        Self::new_with_url(
            OpenAiCompatibleFlavor::Custom(n.clone()),
            api_key,
            endpoint_url,
            n,
        )
    }

    /// Auto-discover OpenAI credentials from `OPENAI_API_KEY`
    pub fn discover_openai() -> Option<Self> {
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return Some(Self::openai(trimmed));
            }
        }
        None
    }

    /// Auto-discover DeepSeek credentials from `DEEPSEEK_API_KEY`
    pub fn discover_deepseek() -> Option<Self> {
        if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return Some(Self::deepseek(trimmed));
            }
        }
        None
    }

    /// Auto-discover OpenRouter credentials from `OPENROUTER_API_KEY`
    pub fn discover_openrouter() -> Option<Self> {
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return Some(Self::openrouter(trimmed));
            }
        }
        None
    }

    /// Automatically discover any available OpenAI-compatible credentials (OpenAI -> DeepSeek -> OpenRouter)
    pub fn auto_discover() -> Option<Self> {
        if let Some(prov) = Self::discover_openai() {
            return Some(prov);
        }
        if let Some(prov) = Self::discover_deepseek() {
            return Some(prov);
        }
        if let Some(prov) = Self::discover_openrouter() {
            return Some(prov);
        }
        None
    }

    /// Check if any OpenAI-compatible provider credentials are ready in the environment
    pub fn is_available() -> bool {
        Self::discover_openai().is_some()
            || Self::discover_deepseek().is_some()
            || Self::discover_openrouter().is_some()
    }

    /// Returns a human-friendly string of active credentials
    pub fn credential_status() -> String {
        let mut active = Vec::new();
        if std::env::var("OPENAI_API_KEY").map(|k| !k.trim().is_empty()).unwrap_or(false) {
            active.push("OpenAI (OPENAI_API_KEY)");
        }
        if std::env::var("DEEPSEEK_API_KEY").map(|k| !k.trim().is_empty()).unwrap_or(false) {
            active.push("DeepSeek (DEEPSEEK_API_KEY)");
        }
        if std::env::var("OPENROUTER_API_KEY").map(|k| !k.trim().is_empty()).unwrap_or(false) {
            active.push("OpenRouter (OPENROUTER_API_KEY)");
        }

        if active.is_empty() {
            "No OpenAI-compatible credentials configured (export OPENAI_API_KEY, DEEPSEEK_API_KEY, or OPENROUTER_API_KEY)".to_string()
        } else {
            active.join(", ")
        }
    }

    /// Access flavor
    pub fn flavor(&self) -> &OpenAiCompatibleFlavor {
        &self.flavor
    }

    /// Access active endpoint URL
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// Access API key
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Normalize an arbitrary base URL to chat completions endpoint
    fn normalize_chat_endpoint(base: &str) -> String {
        let trimmed = base.trim().trim_end_matches('/');
        if trimmed.ends_with("/chat/completions") {
            trimmed.to_string()
        } else if trimmed.ends_with("/v1") {
            format!("{}/chat/completions", trimmed)
        } else {
            format!("{}/v1/chat/completions", trimmed)
        }
    }

    /// Resolve and sanitize model name
    pub fn sanitize_model<'a>(&self, model: Option<&'a str>) -> Cow<'a, str> {
        let raw = model.unwrap_or("auto").trim();
        let lower = raw.to_lowercase();
        match lower.as_str() {
            "" | "auto" | "default" => Cow::Borrowed(self.flavor.default_model()),
            _ => {
                // If it's already an explicit model ID, use it directly
                Cow::Borrowed(raw)
            }
        }
    }

    /// Send a full chat messages array to the completions endpoint
    pub async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        model: Option<&str>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        let model_name = self.sanitize_model(model);
        let payload = ChatCompletionRequestBody {
            model: &model_name,
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
            HeaderValue::from_static("hagibis/0.1.0 (OpenAI-Compatible Engine)"),
        );

        let auth_val = format!("Bearer {}", self.api_key);
        let auth_header = HeaderValue::from_str(&auth_val)
            .map_err(|e| HgbError::Authentication(format!("Invalid auth token format: {e}")))?;
        headers.insert(AUTHORIZATION, auth_header);

        // OpenRouter optional identification headers
        if matches!(self.flavor, OpenAiCompatibleFlavor::OpenRouter) {
            headers.insert(
                "HTTP-Referer",
                HeaderValue::from_static("https://github.com/CharleGutierrez/hagibis"),
            );
            headers.insert("X-Title", HeaderValue::from_static("Hagibis"));
        }

        let response = self
            .client
            .post(&self.endpoint_url)
            .headers(headers)
            .body(body_json)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to connect to {}: {e}", self.provider_name)))?;

        let status = response.status();
        if !status.is_success() {
            let err_text = response.text().await.unwrap_or_default();
            // Try to extract structured message
            if let Ok(parsed) = serde_json::from_str::<ChatCompletionResponseBody>(&err_text) {
                if let Some(err) = parsed.error {
                    if let Some(msg) = err.message {
                        return Err(HgbError::Provider(format!(
                            "{} API error (HTTP {}): {}",
                            self.provider_name, status, msg
                        )));
                    }
                }
            }
            return Err(HgbError::Network(format!(
                "{} API returned HTTP {}: {}",
                self.provider_name, status, err_text
            )));
        }

        let resp_json: ChatCompletionResponseBody = response
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Failed to parse {} response: {e}", self.provider_name)))?;

        // 1. Check choices[0].message.content
        if let Some(choice) = resp_json.choices.into_iter().next() {
            if let Some(msg) = choice.message {
                if let Some(cnt) = msg.content {
                    if !cnt.trim().is_empty() {
                        return Ok(cnt);
                    }
                }
            }
            // 2. Fallback to choice.text if any legacy completion response
            if let Some(txt) = choice.text {
                if !txt.trim().is_empty() {
                    return Ok(txt);
                }
            }
        }

        Err(HgbError::Execution(format!(
            "No output content returned by {} chat completions API",
            self.provider_name
        )))
    }

    /// Quick ping/health check by requesting a 1-token dummy prompt
    pub async fn ping(&self) -> Result<bool> {
        let messages = vec![ChatMessage::user("ping")];
        match self.chat_completion(&messages, None, Some(0.0), Some(1)).await {
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl HgbProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let messages = vec![ChatMessage::user(prompt)];
        self.chat_completion(&messages, model, None, None).await
    }
}

pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-3-5-sonnet-20241022";
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A conversation turn message for the Anthropic Messages API
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: String,
}

impl AnthropicMessage {
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
}

#[derive(Serialize)]
struct AnthropicRequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: &'a [AnthropicMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize, Debug)]
struct AnthropicResponseBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    error: Option<AnthropicErrorDetails>,
}

#[derive(Deserialize, Debug)]
struct AnthropicContentBlock {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AnthropicErrorDetails {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Real HTTP client for Anthropic Messages API
#[derive(Clone)]
pub struct AnthropicProvider {
    api_key: String,
    endpoint_url: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create provider with given API key using default endpoint URL
    pub fn new(api_key: impl Into<String>) -> Self {
        let endpoint = if let Ok(base) = std::env::var("ANTHROPIC_BASE_URL") {
            Self::normalize_messages_endpoint(&base)
        } else {
            "https://api.anthropic.com/v1/messages".to_string()
        };
        Self::new_with_url(api_key, endpoint)
    }

    /// Create provider with given API key and custom endpoint URL
    pub fn new_with_url(api_key: impl Into<String>, endpoint_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            api_key: api_key.into(),
            endpoint_url: endpoint_url.into(),
            client,
        }
    }

    /// Auto-discover credentials from `ANTHROPIC_API_KEY`
    pub fn auto_discover() -> Option<Self> {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("your_") {
                return Some(Self::new(trimmed));
            }
        }
        None
    }

    /// Check if Anthropic credentials are ready in the environment
    pub fn is_available() -> bool {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            let trimmed = key.trim();
            !trimmed.is_empty() && !trimmed.starts_with("your_")
        } else {
            false
        }
    }

    /// Current credential description
    pub fn credential_status() -> String {
        if Self::is_available() {
            "Anthropic API Key (ANTHROPIC_API_KEY)".to_string()
        } else {
            "Anthropic unconfigured (export ANTHROPIC_API_KEY)".to_string()
        }
    }

    /// Access active endpoint URL
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// Access active API key
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Normalize an arbitrary base URL to messages endpoint
    fn normalize_messages_endpoint(base: &str) -> String {
        let trimmed = base.trim().trim_end_matches('/');
        if trimmed.ends_with("/messages") {
            trimmed.to_string()
        } else if trimmed.ends_with("/v1") {
            format!("{}/messages", trimmed)
        } else {
            format!("{}/v1/messages", trimmed)
        }
    }

    /// Sanitize and resolve model aliases to standard Anthropic model IDs
    pub fn sanitize_model<'a>(model: Option<&'a str>) -> Cow<'a, str> {
        let raw = model.unwrap_or("auto").trim();
        let lower = raw.to_lowercase();
        match lower.as_str() {
            "" | "auto" | "default" | "claude" | "sonnet" | "claude-3-5-sonnet" | "claude-3.5-sonnet" => {
                Cow::Borrowed(DEFAULT_ANTHROPIC_MODEL)
            }
            "haiku" | "claude-haiku" | "claude-3-5-haiku" | "claude-3.5-haiku" => {
                Cow::Borrowed("claude-3-5-haiku-20241022")
            }
            "opus" | "claude-opus" | "claude-3-opus" => Cow::Borrowed("claude-3-opus-20240229"),
            "3.7-sonnet" | "claude-3-7-sonnet" | "claude-3.7-sonnet" => {
                Cow::Borrowed("claude-3-7-sonnet-20250219")
            }
            _ => {
                if raw.starts_with("claude-") {
                    Cow::Borrowed(raw)
                } else {
                    Cow::Borrowed(DEFAULT_ANTHROPIC_MODEL)
                }
            }
        }
    }

    /// Send conversation history to the Anthropic Messages API
    pub async fn messages(
        &self,
        messages: &[AnthropicMessage],
        model: Option<&str>,
        max_tokens: Option<u32>,
        system: Option<&str>,
        temperature: Option<f32>,
    ) -> Result<String> {
        let model_name = Self::sanitize_model(model);
        let tokens_limit = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

        let payload = AnthropicRequestBody {
            model: &model_name,
            max_tokens: tokens_limit,
            messages,
            system,
            temperature,
        };

        let body_json = serde_json::to_string(&payload)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("hagibis/0.1.0 (Anthropic Messages API)"),
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_API_VERSION),
        );

        let key_header = HeaderValue::from_str(&self.api_key)
            .map_err(|e| HgbError::Authentication(format!("Invalid Anthropic API key format: {e}")))?;
        headers.insert("x-api-key", key_header);

        let response = self
            .client
            .post(&self.endpoint_url)
            .headers(headers)
            .body(body_json)
            .send()
            .await
            .map_err(|e| HgbError::Network(format!("Failed to connect to Anthropic API: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let err_text = response.text().await.unwrap_or_default();
            if let Ok(parsed) = serde_json::from_str::<AnthropicResponseBody>(&err_text) {
                if let Some(err) = parsed.error {
                    if let Some(msg) = err.message {
                        return Err(HgbError::Provider(format!(
                            "Anthropic API error (HTTP {}): {}",
                            status, msg
                        )));
                    }
                }
            }
            return Err(HgbError::Network(format!(
                "Anthropic API returned HTTP {}: {}",
                status, err_text
            )));
        }

        let resp_json: AnthropicResponseBody = response
            .json()
            .await
            .map_err(|e| HgbError::Serialization(format!("Failed to parse Anthropic response: {e}")))?;

        let collected_text: String = resp_json
            .content
            .into_iter()
            .filter(|b| b.r#type == "text")
            .filter_map(|b| b.text)
            .collect::<Vec<String>>()
            .join("\n");

        if !collected_text.trim().is_empty() {
            Ok(collected_text)
        } else {
            Err(HgbError::Execution(
                "No output content returned by Anthropic Messages API".to_string(),
            ))
        }
    }

    /// Quick ping/health check by requesting a 1-token dummy prompt
    pub async fn ping(&self) -> Result<bool> {
        let messages = vec![AnthropicMessage::user("ping")];
        match self.messages(&messages, None, Some(1), None, Some(0.0)).await {
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl HgbProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let messages = vec![AnthropicMessage::user(prompt)];
        self.messages(&messages, model, None, None, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flavor_endpoints_and_defaults() {
        let openai = OpenAiCompatibleFlavor::OpenAi;
        assert_eq!(openai.default_endpoint(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(openai.provider_name(), "openai");
        assert_eq!(openai.default_model(), "gpt-4o");

        let deepseek = OpenAiCompatibleFlavor::DeepSeek;
        assert_eq!(deepseek.default_endpoint(), "https://api.deepseek.com/chat/completions");
        assert_eq!(deepseek.provider_name(), "deepseek");
        assert_eq!(deepseek.default_model(), "deepseek-chat");

        let openrouter = OpenAiCompatibleFlavor::OpenRouter;
        assert_eq!(openrouter.default_endpoint(), "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(openrouter.provider_name(), "openrouter");
        assert_eq!(openrouter.default_model(), "deepseek/deepseek-chat");

        let custom = OpenAiCompatibleFlavor::Custom("local_vllm".to_string());
        assert_eq!(custom.provider_name(), "local_vllm");
    }

    #[test]
    fn test_normalize_chat_endpoint() {
        assert_eq!(
            OpenAiCompatibleProvider::normalize_chat_endpoint("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            OpenAiCompatibleProvider::normalize_chat_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            OpenAiCompatibleProvider::normalize_chat_endpoint("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            OpenAiCompatibleProvider::normalize_chat_endpoint("http://localhost:8080/"),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[test]
    fn test_provider_constructors_and_models() {
        let p_oa = OpenAiCompatibleProvider::openai("sk-test12345");
        assert_eq!(p_oa.name(), "openai");
        assert_eq!(p_oa.api_key(), "sk-test12345");
        assert_eq!(p_oa.sanitize_model(None), "gpt-4o");
        assert_eq!(p_oa.sanitize_model(Some("gpt-4o-mini")), "gpt-4o-mini");

        let p_ds = OpenAiCompatibleProvider::deepseek("sk-ds12345");
        assert_eq!(p_ds.name(), "deepseek");
        assert_eq!(p_ds.sanitize_model(None), "deepseek-chat");

        let p_or = OpenAiCompatibleProvider::openrouter("sk-or12345");
        assert_eq!(p_or.name(), "openrouter");
        assert_eq!(p_or.sanitize_model(None), "deepseek/deepseek-chat");
    }

    #[test]
    fn test_parse_success_response() {
        let raw = r#"{
            "id": "chatcmpl-abc123xyz",
            "object": "chat.completion",
            "created": 1720000000,
            "model": "gpt-4o",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello from production model!"
                    },
                    "finish_reason": "stop"
                }
            ]
        }"#;

        let parsed: ChatCompletionResponseBody = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.choices.len(), 1);
        let content = parsed.choices[0].message.as_ref().unwrap().content.as_deref().unwrap();
        assert_eq!(content, "Hello from production model!");
    }

    #[test]
    fn test_parse_error_response() {
        let raw = r#"{
            "error": {
                "message": "Incorrect API key provided: sk-test****",
                "type": "invalid_request_error",
                "param": null,
                "code": "invalid_api_key"
            }
        }"#;

        let parsed: ChatCompletionResponseBody = serde_json::from_str(raw).unwrap();
        assert!(parsed.choices.is_empty());
        let err = parsed.error.unwrap();
        assert_eq!(err.message.unwrap(), "Incorrect API key provided: sk-test****");
        assert_eq!(err.r#type.unwrap(), "invalid_request_error");
    }

    #[test]
    fn test_anthropic_sanitize_model() {
        assert_eq!(AnthropicProvider::sanitize_model(None), DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(AnthropicProvider::sanitize_model(Some("auto")), DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(AnthropicProvider::sanitize_model(Some("sonnet")), DEFAULT_ANTHROPIC_MODEL);
        assert_eq!(AnthropicProvider::sanitize_model(Some("haiku")), "claude-3-5-haiku-20241022");
        assert_eq!(AnthropicProvider::sanitize_model(Some("opus")), "claude-3-opus-20240229");
        assert_eq!(AnthropicProvider::sanitize_model(Some("3.7-sonnet")), "claude-3-7-sonnet-20250219");
        assert_eq!(AnthropicProvider::sanitize_model(Some("claude-custom-tier")), "claude-custom-tier");
    }

    #[test]
    fn test_anthropic_normalize_messages_endpoint() {
        assert_eq!(
            AnthropicProvider::normalize_messages_endpoint("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            AnthropicProvider::normalize_messages_endpoint("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            AnthropicProvider::normalize_messages_endpoint("https://api.anthropic.com/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_parse_responses() {
        let raw_ok = r#"{
            "id": "msg_01X9z8Y7w6V5u4T3",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Hello, this is Claude."
                }
            ],
            "model": "claude-3-5-sonnet-20241022",
            "stop_reason": "end_turn"
        }"#;

        let parsed_ok: AnthropicResponseBody = serde_json::from_str(raw_ok).unwrap();
        assert_eq!(parsed_ok.content.len(), 1);
        assert_eq!(parsed_ok.content[0].text.as_deref(), Some("Hello, this is Claude."));

        let raw_err = r#"{
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": "invalid x-api-key"
            }
        }"#;

        let parsed_err: AnthropicResponseBody = serde_json::from_str(raw_err).unwrap();
        let err = parsed_err.error.unwrap();
        assert_eq!(err.r#type.as_deref(), Some("authentication_error"));
        assert_eq!(err.message.as_deref(), Some("invalid x-api-key"));
    }
}

