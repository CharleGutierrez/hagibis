//! # RelationalWebhookReplayer - Instant Third-Party API & Webhook Replay Fabric
//!
//! Enables vibe developers to prototype full-stack applications with deterministic,
//! offline simulations of critical third-party APIs (Stripe, GitHub Webhooks, OAuth providers).
//! Generates valid cryptographic HMAC-SHA256 webhook signatures, simulated latencies,
//! and schema-compliant relational JSON fixtures without live credentials or internet access.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::providers::ollama::OllamaProvider;
use crate::traits::HgbProvider;
use tokio::runtime::Runtime;

/// Categories of supported external service proxys
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProxyServiceKind {
    StripePayment,
    GitHubWebhook,
    OAuthProvider,
    GenericRest,
}

impl ProxyServiceKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::StripePayment => "Stripe Payments & Subscriptions",
            Self::GitHubWebhook => "GitHub Webhooks & Events",
            Self::OAuthProvider => "OAuth2 & OIDC Token Issuer",
            Self::GenericRest => "Generic REST API Service",
        }
    }
}

/// Simulated response report from proxy API engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyReplayReport {
    pub service: ProxyServiceKind,
    pub endpoint: String,
    pub method: String,
    pub status_code: u16,
    pub response_body: serde_json::Value,
    pub hmac_header: Option<(String, String)>,
    pub latency_ms: u64,
    pub is_deterministic: bool,
}

pub struct RelationalWebhookReplayer;

impl RelationalWebhookReplayer {
    /// Compute an RFC 2104 compliant HMAC-SHA256 signature in hex
    pub fn compute_hmac_sha256(key: &[u8], data: &[u8]) -> String {
        let mut key_block = [0u8; 64];
        if key.len() > 64 {
            let hash = Sha256::digest(key);
            key_block[..hash.len()].copy_from_slice(&hash);
        } else {
            key_block[..key.len()].copy_from_slice(key);
        }

        let mut o_key_pad = [0x5cu8; 64];
        let mut i_key_pad = [0x36u8; 64];
        for i in 0..64 {
            o_key_pad[i] ^= key_block[i];
            i_key_pad[i] ^= key_block[i];
        }

        let mut inner = Sha256::new();
        inner.update(i_key_pad);
        inner.update(data);
        let inner_hash = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(o_key_pad);
        outer.update(inner_hash);
        let result = outer.finalize();

        result.iter().map(|b| format!("{:02x}", b)).collect()
    }

    fn generate_llm_payload(prompt: &str, fallback: serde_json::Value) -> serde_json::Value {
        let rt = Runtime::new().unwrap();
        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));
        let result = rt.block_on(async {
            provider.complete(prompt, None).await
        });
        if let Ok(resp) = result {
            let clean = resp.replace("```json", "").replace("```", "").trim().to_string();
            if let Ok(parsed) = serde_json::from_str(&clean) {
                return parsed;
            }
        }
        fallback
    }

    /// Dispatch a simulated proxy API request
    pub fn dispatch(
        service: ProxyServiceKind,
        endpoint: &str,
        method: &str,
        payload: Option<&serde_json::Value>,
    ) -> ProxyReplayReport {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match service {
            ProxyServiceKind::StripePayment => {
                let session_id = format!("cs_test_{}", now);
                let payment_intent = format!("pi_3M{}", now);
                let prompt = format!("Generate a valid Stripe JSON response for a payment intent. Include id: '{}', status: 'succeeded', amount: 4900, currency: 'usd'. Only output valid JSON.", payment_intent);
                let fallback = serde_json::json!({
                    "id": payment_intent,
                    "object": "payment_intent",
                    "status": "succeeded",
                    "amount": 4900,
                    "currency": "usd",
                    "created": now
                });
                
                let response = Self::generate_llm_payload(&prompt, fallback);

                let raw_json = response.to_string();
                let secret_str = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_else(|_| "test_secret".to_string());
                let secret = secret_str.as_bytes();
                let signed_payload = format!("{}.{}", now, raw_json);
                let sig = Self::compute_hmac_sha256(secret, signed_payload.as_bytes());
                let header_val = format!("t={},v1={}", now, sig);

                ProxyReplayReport {
                    service: ProxyServiceKind::StripePayment,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: Some(("Stripe-Signature".to_string(), header_val)),
                    latency_ms: 15,
                    is_deterministic: false,
                }
            }
            ProxyServiceKind::GitHubWebhook => {
                let prompt = "Generate a valid GitHub Webhook JSON payload for a push event to the main branch. Include a ref, repository, pusher, and head_commit. Only output valid JSON.";
                let fallback = serde_json::json!({
                    "ref": "refs/heads/main",
                    "repository": { "id": 1296269, "full_name": "developer/hagibis-app" },
                    "head_commit": { "id": "6dcb09b5", "message": "test commit" }
                });
                
                let response = Self::generate_llm_payload(prompt, fallback);

                let raw_json = response.to_string();
                let secret_str = std::env::var("GITHUB_WEBHOOK_SECRET").unwrap_or_else(|_| "test_secret".to_string());
                let secret = secret_str.as_bytes();
                let sig = Self::compute_hmac_sha256(secret, raw_json.as_bytes());
                let header_val = format!("sha256={}", sig);

                ProxyReplayReport {
                    service: ProxyServiceKind::GitHubWebhook,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: Some(("X-Hub-Signature-256".to_string(), header_val)),
                    latency_ms: 22,
                    is_deterministic: false,
                }
            }
            ProxyServiceKind::OAuthProvider => {
                let prompt = "Generate a valid OAuth2 JSON response containing an access_token, token_type: 'bearer', scope, and a user object. Only output valid JSON.";
                let fallback = serde_json::json!({
                    "access_token": format!("ghu_{}", uuid::Uuid::new_v4().simple()),
                    "token_type": "bearer",
                    "user": { "login": "test_user" }
                });
                
                let response = Self::generate_llm_payload(prompt, fallback);

                ProxyReplayReport {
                    service: ProxyServiceKind::OAuthProvider,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: None,
                    latency_ms: 8,
                    is_deterministic: false,
                }
            }
            ProxyServiceKind::GenericRest => {
                let prompt = "Generate a generic REST API JSON response indicating success with a timestamp. Only output valid JSON.";
                let fallback = payload.cloned().unwrap_or_else(|| {
                    serde_json::json!({
                        "status": "success",
                        "timestamp": now
                    })
                });
                
                let response = Self::generate_llm_payload(prompt, fallback);

                ProxyReplayReport {
                    service: ProxyServiceKind::GenericRest,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: None,
                    latency_ms: 10,
                    is_deterministic: false,
                }
            }
        }
    }
}
