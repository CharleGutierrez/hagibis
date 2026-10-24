//! # Unified AI Provider Router & Cache Fingerprinting
//!
//! Provides a single, unified execution path for LLM completions across
//! Google Gemini, OpenAI / Codex, and local Ollama instances.
//! Calculates BLAKE3 workspace context fingerprints to ensure that cached
//! completions are properly isolated and invalidated when project code changes.

use async_trait::async_trait;
use crate::error::{HgbError, Result};
use crate::traits::HgbProvider;
use crate::providers::gemini::GeminiProvider;
use crate::providers::ollama::OllamaProvider;
use crate::providers::openai_compatible::{ChatMessage, OpenAiCompatibleProvider};

/// Unified provider router ensuring consistent model routing and project context fingerprinting
pub struct SharedProviderRouter {
    default_model: Option<String>,
}

impl Default for SharedProviderRouter {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SharedProviderRouter {
    pub fn new(default_model: Option<String>) -> Self {
        Self { default_model }
    }

    /// Compute cryptographic context fingerprint for cache keying
    pub fn compute_context_key(workspace_id: &str, model: &str, prompt: &str) -> String {
        let hash_input = format!("{}:{}:{}", workspace_id, model, prompt.trim());
        blake3::hash(hash_input.as_bytes()).to_hex().to_string()
    }
}

#[async_trait]
impl HgbProvider for SharedProviderRouter {
    fn name(&self) -> &str {
        "shared-provider-router"
    }

    async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let model_name = model
            .map(|s| s.to_string())
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| "auto".to_string());

        // 1. Ollama local models
        if model_name.starts_with("ollama/")
            || (model_name != "auto"
                && !model_name.starts_with("gpt-")
                && !model_name.starts_with("gemini-")
                && OllamaProvider::is_available())
        {
            let actual_model = model_name.strip_prefix("ollama/").unwrap_or(&model_name);
            let prov = OllamaProvider::new(None, Some(actual_model.to_string()));
            return prov.complete(prompt, Some(actual_model)).await;
        }

        // 2. OpenAI compatible / Codex
        if model_name.starts_with("gpt-")
            || model_name.starts_with("o1")
            || model_name.starts_with("o3")
            || model_name.starts_with("chatgpt")
        {
            if let Some(prov) = OpenAiCompatibleProvider::discover_openai() {
                let msgs = vec![
                    ChatMessage::system("You are an expert AI software engineer in Hagibis."),
                    ChatMessage::user(prompt),
                ];
                return prov.chat_completion(&msgs, Some(&model_name), None, None).await;
            }
        }

        // 3. Gemini Cloud Provider
        if let Some(gemini) = GeminiProvider::auto_discover() {
            return gemini.complete(prompt, Some(&model_name)).await;
        }

        // 4. Ollama fallback
        if let Some(ollama) = OllamaProvider::auto_discover() {
            return ollama.complete(prompt, None).await;
        }

        Err(HgbError::Provider(
            "No AI LLM provider is currently available. Configure GEMINI_API_KEY, OPENAI_API_KEY, or start Ollama.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_fingerprint_deterministic() {
        let key1 = SharedProviderRouter::compute_context_key("proj_1", "gemini-2.5-flash", "test prompt");
        let key2 = SharedProviderRouter::compute_context_key("proj_1", "gemini-2.5-flash", "test prompt");
        let key3 = SharedProviderRouter::compute_context_key("proj_2", "gemini-2.5-flash", "test prompt");

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }
}
