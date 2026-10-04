//! # Zero-Knowledge Airgap Cloak & PII Sanitizer
//!
//! Masks API secrets, database credentials, internal IPs, and PII before dispatching prompts
//! to cloud models, seamlessly rehydrating the original sensitive entities upon response arrival.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use regex::Regex;

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

    /// Masks sensitive entities in outbound prompts using real Regex
    pub fn cloak(&mut self, text: &str) -> CloakAuditReport {
        let mut cloaked = text.to_string();
        let mut entities = Vec::new();

        // 1. Regex patterns for actual secrets and PII
        // Using common robust regexes for Stripe, AWS, GitHub, Slack tokens, Email, and IPv4
        let secret_re = Regex::new(r"(?i)(sk_[0-9a-zA-Z_]+|AKIA[0-9A-Z]{16}|gh[pousr]_[0-9a-zA-Z]{36}|xox[baprs]-[0-9]{10,13}-[0-9a-zA-Z]{24})").unwrap();
        let email_re = Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").unwrap();
        let ip_re = Regex::new(r"\b(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\b").unwrap();

        let mut secret_idx = 1;
        for cap in secret_re.captures_iter(text) {
            let secret = &cap[0];
            if !self.token_map.values().any(|v| v == secret) {
                let placeholder = format!("<CLOAK_SECRET_{}>", secret_idx);
                secret_idx += 1;
                self.token_map.insert(placeholder.clone(), secret.to_string());
                cloaked = cloaked.replace(secret, &placeholder);
                entities.push(CloakedEntity {
                    placeholder,
                    original_masked: format!("{}***", &secret[..secret.len().min(7)]),
                    entity_type: "API_SECRET".to_string(),
                });
            }
        }

        let mut email_idx = 1;
        for cap in email_re.captures_iter(text) {
            let email = &cap[0];
            if !self.token_map.values().any(|v| v == email) {
                let placeholder = format!("<CLOAK_EMAIL_{}>", email_idx);
                email_idx += 1;
                self.token_map.insert(placeholder.clone(), email.to_string());
                cloaked = cloaked.replace(email, &placeholder);
                entities.push(CloakedEntity {
                    placeholder,
                    original_masked: "user@***.com".to_string(),
                    entity_type: "PII_EMAIL".to_string(),
                });
            }
        }

        let mut ip_idx = 1;
        for cap in ip_re.captures_iter(text) {
            let ip = &cap[0];
            if !self.token_map.values().any(|v| v == ip) {
                let placeholder = format!("<CLOAK_IP_{}>", ip_idx);
                ip_idx += 1;
                self.token_map.insert(placeholder.clone(), ip.to_string());
                cloaked = cloaked.replace(ip, &placeholder);
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
        let prompt = "Connect to 192.168.1.50 using key sk_dummy_981723491823981723491823 for admin@corp.internal";

        let report = engine.cloak(prompt);
        assert!(!report.clean);
        assert_eq!(report.entities_masked.len(), 3);
        assert!(report.cloaked_text.contains("<CLOAK_SECRET_1>"));
        assert!(report.cloaked_text.contains("<CLOAK_EMAIL_1>"));
        assert!(report.cloaked_text.contains("<CLOAK_IP_1>"));
        assert!(!report.cloaked_text.contains("sk_dummy_981723491823981723491823"));

        let ai_response = "const client = init({ host: '<CLOAK_IP_1>', apiKey: '<CLOAK_SECRET_1>', user: '<CLOAK_EMAIL_1>' });";
        let rehydrated = engine.rehydrate(ai_response);
        assert!(rehydrated.contains("192.168.1.50"));
        assert!(rehydrated.contains("sk_dummy_981723491823981723491823"));
        assert!(rehydrated.contains("admin@corp.internal"));
    }
}
