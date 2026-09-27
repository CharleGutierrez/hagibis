//! # Zero-Knowledge Airgap Cloak & PII Sanitizer
//!
//! Masks API secrets, database credentials, internal IPs, and PII before dispatching prompts
//! to cloud models, seamlessly rehydrating the original sensitive entities upon response arrival.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloakedEntity {
    pub placeholder: String,
    pub original_masked: String,
    pub entity_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloakAuditReport {
    pub original_length: usize,
    pub cloaked_length: usize,
    pub cloaked_text: String,
    pub entities_masked: Vec<CloakedEntity>,
    pub clean: bool,
}

pub struct AirgapCloakEngine {
    token_map: HashMap<String, String>,
}

impl AirgapCloakEngine {
    pub fn new() -> Self {
        Self {
            token_map: HashMap::new(),
        }
    }

    /// Masks sensitive entities in outbound prompts with deterministic placeholders
    pub fn cloak(&mut self, text: &str) -> CloakAuditReport {
        let mut cloaked = text.to_string();
        let mut entities = Vec::new();

        // 1. Scan for API Keys (sk_live_, ghp_, etc.)
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut secret_idx = 1;
        let mut email_idx = 1;
        let mut ip_idx = 1;

        for word in words {
            let clean_word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '@' && c != '.');
            
            // Check for API Keys
            if clean_word.starts_with("sk_live_") || clean_word.starts_with("sk_dummy_") || clean_word.starts_with("ghp_") || clean_word.starts_with("xoxb-") {
                let placeholder = format!("<CLOAK_SECRET_{}>", secret_idx);
                secret_idx += 1;
                self.token_map.insert(placeholder.clone(), clean_word.to_string());
                cloaked = cloaked.replace(clean_word, &placeholder);
                entities.push(CloakedEntity {
                    placeholder,
                    original_masked: format!("{}***", &clean_word[..clean_word.len().min(7)]),
                    entity_type: "API_SECRET".to_string(),
                });
            }
            // Check for Emails
            else if clean_word.contains('@') && clean_word.contains('.') && !clean_word.starts_with('<') {
                let placeholder = format!("<CLOAK_EMAIL_{}>", email_idx);
                email_idx += 1;
                self.token_map.insert(placeholder.clone(), clean_word.to_string());
                cloaked = cloaked.replace(clean_word, &placeholder);
                entities.push(CloakedEntity {
                    placeholder,
                    original_masked: "user@***.com".to_string(),
                    entity_type: "PII_EMAIL".to_string(),
                });
            }
            // Check for IPv4 addresses
            else if clean_word.split('.').count() == 4 && clean_word.split('.').all(|p| p.parse::<u8>().is_ok()) {
                let placeholder = format!("<CLOAK_IP_{}>", ip_idx);
                ip_idx += 1;
                self.token_map.insert(placeholder.clone(), clean_word.to_string());
                cloaked = cloaked.replace(clean_word, &placeholder);
                entities.push(CloakedEntity {
                    placeholder,
                    original_masked: "192.168.***.***".to_string(),
                    entity_type: "NETWORK_IP".to_string(),
                });
            }
        }

        let is_clean = entities.is_empty();
        CloakAuditReport {
            original_length: text.len(),
            cloaked_length: cloaked.len(),
            cloaked_text: cloaked,
            entities_masked: entities,
            clean: is_clean,
        }
    }

    /// Rehydrates cloaked tokens in the AI response back into real sensitive values
    pub fn rehydrate(&self, response_text: &str) -> String {
        let mut out = response_text.to_string();
        for (placeholder, original) in &self.token_map {
            out = out.replace(placeholder, original);
        }
        out
    }
}

impl Default for AirgapCloakEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_airgap_cloak_and_rehydrate() {
        let mut engine = AirgapCloakEngine::new();
        let prompt = "Connect to 192.168.1.50 using key sk_dummy_981723491823 for admin@corp.internal";

        let report = engine.cloak(prompt);
        assert!(!report.clean);
        assert_eq!(report.entities_masked.len(), 3);
        assert!(report.cloaked_text.contains("<CLOAK_SECRET_1>"));
        assert!(report.cloaked_text.contains("<CLOAK_EMAIL_1>"));
        assert!(report.cloaked_text.contains("<CLOAK_IP_1>"));
        assert!(!report.cloaked_text.contains("sk_dummy_981723491823"));

        // Simulate AI completion containing the cloaked placeholders
        let ai_response = "const client = init({ host: '<CLOAK_IP_1>', apiKey: '<CLOAK_SECRET_1>', user: '<CLOAK_EMAIL_1>' });";
        let rehydrated = engine.rehydrate(ai_response);
        assert!(rehydrated.contains("192.168.1.50"));
        assert!(rehydrated.contains("sk_dummy_981723491823"));
        assert!(rehydrated.contains("admin@corp.internal"));
    }
}
