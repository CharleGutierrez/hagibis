use serde::{Deserialize, Serialize};
use crate::providers::GeminiProvider;
use crate::traits::HgbProvider;
use std::error::Error;

/// Expanded design tokens inferred from the user's vibe
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesignTokens {
    pub theme_name: String,
    pub background: String,
    pub surface: String,
    pub primary_accent: String,
    pub secondary_accent: String,
    pub text_primary: String,
    pub text_muted: String,
    pub border_style: String,
    pub blur_effect: String,
}

/// Motion and micro-interaction specifications
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MotionTokens {
    pub transition_curve: String,
    pub hover_scale: String,
    pub celebratory_effect: Option<String>,
    pub enter_animation: String,
}

/// Complete expanded specification for code generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandedVibeSpec {
    pub raw_prompt: String,
    pub design: DesignTokens,
    pub motion: MotionTokens,
    pub inferred_components: Vec<String>,
    pub recommended_packages: Vec<String>,
    pub enriched_system_prompt: String,
}

pub struct VibeIntentExpander;

impl VibeIntentExpander {
    /// Expand a vague vibe prompt into structured technical tokens using a real LLM
    pub async fn expand(prompt: &str) -> Result<ExpandedVibeSpec, Box<dyn Error + Send + Sync>> {
        let provider = GeminiProvider::auto_discover()
            .ok_or("No Gemini API key available")?;

        let system_prompt = r#"You are an AI vibe-coder intent expander. 
Given a short vibe prompt from the user, you must expand it into a detailed design and technical specification.
Respond EXACTLY with a raw JSON object matching this schema:
{
    "raw_prompt": "the original prompt",
    "design": {
        "theme_name": "string",
        "background": "string",
        "surface": "string",
        "primary_accent": "string",
        "secondary_accent": "string",
        "text_primary": "string",
        "text_muted": "string",
        "border_style": "string",
        "blur_effect": "string"
    },
    "motion": {
        "transition_curve": "string",
        "hover_scale": "string",
        "celebratory_effect": "string or null",
        "enter_animation": "string"
    },
    "inferred_components": ["string"],
    "recommended_packages": ["string"],
    "enriched_system_prompt": "string"
}
Return ONLY valid JSON without Markdown block formatting."#;

        let full_prompt = format!("{}\n\nUser Prompt: {}", system_prompt, prompt);
        let response = provider.complete(&full_prompt, Some("gemini-2.5-flash")).await?;

        let clean = response.trim();
        let clean = if clean.starts_with("```json") {
            clean.trim_start_matches("```json").trim_end_matches("```").trim()
        } else if clean.starts_with("```") {
            clean.trim_start_matches("```").trim_end_matches("```").trim()
        } else {
            clean
        };

        let spec: ExpandedVibeSpec = serde_json::from_str(clean)?;
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[tokio::test]
    async fn test_vibe_intent_expander_real() {
        // Only run if we actually have a GEMINI_API_KEY for testing
        if env::var("GEMINI_API_KEY").is_err() {
            println!("Skipping real vibe_intent_expander test because GEMINI_API_KEY is not set");
            return;
        }

        let res = VibeIntentExpander::expand("dark mode habit tracker with confetti").await;
        assert!(res.is_ok(), "Failed to expand vibe intent: {:?}", res.err());
        let spec = res.unwrap();
        assert!(spec.design.theme_name.len() > 0);
        assert!(spec.inferred_components.len() > 0);
    }
}
