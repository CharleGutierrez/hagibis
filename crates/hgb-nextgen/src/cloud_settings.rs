use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageItem {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSettings {
    pub active_provider: String,
    pub active_model: String,
    pub temperature: f32,
    pub anthropic_api_key: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub gemini_api_key: Option<String>,
    pub gemini_base_url: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub deepseek_base_url: Option<String>,
    pub ollama_base_url: Option<String>,
}

impl Default for CloudSettings {
    fn default() -> Self {
        Self {
            active_provider: "ollama".to_string(),
            active_model: "qwen2.5-coder:1.5b".to_string(),
            temperature: 0.2,
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            anthropic_base_url: Some("https://api.anthropic.com".to_string()),
            openai_api_key: std::env::var("OPENAI_API_KEY").ok(),
            openai_base_url: Some("https://api.openai.com/v1".to_string()),
            gemini_api_key: std::env::var("GEMINI_API_KEY").ok(),
            gemini_base_url: Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            deepseek_api_key: std::env::var("DEEPSEEK_API_KEY").ok(),
            deepseek_base_url: Some("https://api.deepseek.com".to_string()),
            ollama_base_url: Some("http://127.0.0.1:11434".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSettingsMasked {
    pub active_provider: String,
    pub active_model: String,
    pub temperature: f32,
    pub anthropic_api_key: Option<String>,
    pub anthropic_configured: bool,
    pub openai_api_key: Option<String>,
    pub openai_configured: bool,
    pub gemini_api_key: Option<String>,
    pub gemini_configured: bool,
    pub deepseek_api_key: Option<String>,
    pub deepseek_configured: bool,
    pub ollama_base_url: Option<String>,
}

fn mask_key(key: Option<&String>) -> Option<String> {
    key.map(|k| {
        let trimmed = k.trim();
        if trimmed.len() <= 8 {
            "****".to_string()
        } else {
            format!("{}...{}", &trimmed[..4], &trimmed[trimmed.len() - 4..])
        }
    })
}

impl CloudSettings {
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".hgb").join("credentials.json")
    }

    pub fn load_from_disk(path: Option<&Path>) -> Self {
        let file_path = path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(Self::default_path);

        if let Ok(content) = std::fs::read_to_string(&file_path) {
            if let Ok(mut settings) = serde_json::from_str::<CloudSettings>(&content) {
                // Overlay env vars if not set in config
                if settings.anthropic_api_key.is_none() {
                    settings.anthropic_api_key = std::env::var("ANTHROPIC_API_KEY").ok();
                }
                if settings.openai_api_key.is_none() {
                    settings.openai_api_key = std::env::var("OPENAI_API_KEY").ok();
                }
                if settings.gemini_api_key.is_none() {
                    settings.gemini_api_key = std::env::var("GEMINI_API_KEY").ok();
                }
                if settings.deepseek_api_key.is_none() {
                    settings.deepseek_api_key = std::env::var("DEEPSEEK_API_KEY").ok();
                }
                return settings;
            }
        }

        Self::default()
    }

    pub fn save_to_disk(&self, path: Option<&Path>) -> Result<(), String> {
        let file_path = path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(Self::default_path);

        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;
        std::fs::write(&file_path, json)
            .map_err(|e| format!("Write error to {}: {}", file_path.display(), e))?;
        Ok(())
    }

    pub fn to_masked(&self) -> CloudSettingsMasked {
        CloudSettingsMasked {
            active_provider: self.active_provider.clone(),
            active_model: self.active_model.clone(),
            temperature: self.temperature,
            anthropic_configured: self.anthropic_api_key.is_some(),
            anthropic_api_key: mask_key(self.anthropic_api_key.as_ref()),
            openai_configured: self.openai_api_key.is_some(),
            openai_api_key: mask_key(self.openai_api_key.as_ref()),
            gemini_configured: self.gemini_api_key.is_some(),
            gemini_api_key: mask_key(self.gemini_api_key.as_ref()),
            deepseek_configured: self.deepseek_api_key.is_some(),
            deepseek_api_key: mask_key(self.deepseek_api_key.as_ref()),
            ollama_base_url: self.ollama_base_url.clone(),
        }
    }

    /// Dispatches prompt to configured cloud provider or Ollama
    pub async fn dispatch_chat(
        &self,
        messages: &[ChatMessageItem],
        system_instruction: Option<&str>,
    ) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| e.to_string())?;

        match self.active_provider.as_str() {
            "anthropic" => {
                if let Some(ref key) = self.anthropic_api_key {
                    let base = self.anthropic_base_url.as_deref().unwrap_or("https://api.anthropic.com");
                    let url = format!("{}/v1/messages", base);
                    let body = serde_json::json!({
                        "model": self.active_model,
                        "max_tokens": 4096,
                        "temperature": self.temperature,
                        "system": system_instruction.unwrap_or("You are an expert pair-programming AI assistant."),
                        "messages": messages.iter().map(|m| serde_json::json!({
                            "role": if m.role == "assistant" { "assistant" } else { "user" },
                            "content": m.content
                        })).collect::<Vec<_>>()
                    });

                    let resp = client.post(&url)
                        .header("x-api-key", key.trim())
                        .header("anthropic-version", "2023-06-01")
                        .header("content-type", "application/json")
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| format!("Anthropic request failed: {}", e))?;

                    if !resp.status().is_success() {
                        let err_text = resp.text().await.unwrap_or_default();
                        return Err(format!("Anthropic API error: {}", err_text));
                    }

                    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                    if let Some(content_arr) = json.get("content").and_then(|v| v.as_array()) {
                        for block in content_arr {
                            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                    return Ok(t.to_string());
                                }
                            }
                        }
                    }
                }
            }
            "openai" => {
                if let Some(ref key) = self.openai_api_key {
                    let base = self.openai_base_url.as_deref().unwrap_or("https://api.openai.com/v1");
                    let url = format!("{}/chat/completions", base);
                    let mut msgs_json = Vec::new();
                    if let Some(sys) = system_instruction {
                        msgs_json.push(serde_json::json!({ "role": "system", "content": sys }));
                    }
                    for m in messages {
                        msgs_json.push(serde_json::json!({ "role": m.role, "content": m.content }));
                    }

                    let body = serde_json::json!({
                        "model": self.active_model,
                        "temperature": self.temperature,
                        "messages": msgs_json
                    });

                    let resp = client.post(&url)
                        .header("Authorization", format!("Bearer {}", key.trim()))
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| format!("OpenAI request failed: {}", e))?;

                    if !resp.status().is_success() {
                        let err_text = resp.text().await.unwrap_or_default();
                        return Err(format!("OpenAI API error: {}", err_text));
                    }

                    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                    if let Some(choice) = json.get("choices").and_then(|v| v.get(0)) {
                        if let Some(text) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                            return Ok(text.to_string());
                        }
                    }
                }
            }
            "gemini" => {
                if let Some(ref key) = self.gemini_api_key {
                    let base = self.gemini_base_url.as_deref().unwrap_or("https://generativelanguage.googleapis.com/v1beta");
                    let url = format!("{}/models/{}:generateContent?key={}", base, self.active_model, key.trim());

                    let contents: Vec<_> = messages.iter().map(|m| {
                        let role = if m.role == "assistant" { "model" } else { "user" };
                        serde_json::json!({
                            "role": role,
                            "parts": [{ "text": m.content }]
                        })
                    }).collect();

                    let body = serde_json::json!({
                        "contents": contents,
                        "generationConfig": { "temperature": self.temperature }
                    });

                    let resp = client.post(&url)
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| format!("Gemini request failed: {}", e))?;

                    if !resp.status().is_success() {
                        let err_text = resp.text().await.unwrap_or_default();
                        return Err(format!("Gemini API error: {}", err_text));
                    }

                    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                    if let Some(cand) = json.get("candidates").and_then(|v| v.get(0)) {
                        if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.get(0)) {
                            if let Some(text) = parts.get("text").and_then(|t| t.as_str()) {
                                return Ok(text.to_string());
                            }
                        }
                    }
                }
            }
            "deepseek" => {
                if let Some(ref key) = self.deepseek_api_key {
                    let base = self.deepseek_base_url.as_deref().unwrap_or("https://api.deepseek.com");
                    let url = format!("{}/chat/completions", base);
                    let mut msgs_json = Vec::new();
                    if let Some(sys) = system_instruction {
                        msgs_json.push(serde_json::json!({ "role": "system", "content": sys }));
                    }
                    for m in messages {
                        msgs_json.push(serde_json::json!({ "role": m.role, "content": m.content }));
                    }

                    let body = serde_json::json!({
                        "model": self.active_model,
                        "temperature": self.temperature,
                        "messages": msgs_json
                    });

                    let resp = client.post(&url)
                        .header("Authorization", format!("Bearer {}", key.trim()))
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| format!("DeepSeek request failed: {}", e))?;

                    if !resp.status().is_success() {
                        let err_text = resp.text().await.unwrap_or_default();
                        return Err(format!("DeepSeek API error: {}", err_text));
                    }

                    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                    if let Some(choice) = json.get("choices").and_then(|v| v.get(0)) {
                        if let Some(text) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                            return Ok(text.to_string());
                        }
                    }
                }
            }
            _ => {}
        }

        // Local Ollama fallback
        let ollama_url = self.ollama_base_url.as_deref().unwrap_or("http://127.0.0.1:11434");
        let last_prompt = messages.last().map(|m| m.content.as_str()).unwrap_or("");
        let prompt_with_sys = if let Some(sys) = system_instruction {
            format!("{}\n\n{}", sys, last_prompt)
        } else {
            last_prompt.to_string()
        };

        let req = serde_json::json!({
            "model": if self.active_provider == "ollama" { &self.active_model } else { "qwen2.5-coder:1.5b" },
            "prompt": prompt_with_sys,
            "stream": false
        });

        let resp = client.post(format!("{}/api/generate", ollama_url))
            .json(&req)
            .send()
            .await
            .map_err(|e| format!("Ollama local fallback error: {}", e))?;

        let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(text) = json.get("response").and_then(|v| v.as_str()) {
            Ok(text.to_string())
        } else {
            Err("No response generated from model".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_settings_save_and_mask() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let cred_file = temp_dir.path().join("credentials.json");

        let mut settings = CloudSettings::default();
        settings.anthropic_api_key = Some("sk-ant-api03-abcdefghijklmnop12345678".to_string());
        settings.active_provider = "anthropic".to_string();
        settings.active_model = "claude-3-5-sonnet-20241022".to_string();

        assert!(settings.save_to_disk(Some(&cred_file)).is_ok());

        let loaded = CloudSettings::load_from_disk(Some(&cred_file));
        assert_eq!(loaded.active_provider, "anthropic");
        assert_eq!(loaded.active_model, "claude-3-5-sonnet-20241022");

        let masked = loaded.to_masked();
        assert!(masked.anthropic_configured);
        assert!(masked.anthropic_api_key.unwrap().contains("..."));
    }
}
