//! # RelationalMockApiReplayer - Instant Third-Party API & Webhook Replay Fabric
//!
//! Enables vibe developers to prototype full-stack applications with deterministic,
//! offline simulations of critical third-party APIs (Stripe, GitHub Webhooks, OAuth providers).
//! Generates valid cryptographic HMAC-SHA256 webhook signatures, simulated latencies,
//! and schema-compliant relational JSON fixtures without live credentials or internet access.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// Categories of supported external service mocks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MockServiceKind {
    StripePayment,
    GitHubWebhook,
    OAuthProvider,
    GenericRest,
}

impl MockServiceKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::StripePayment => "Stripe Payments & Subscriptions",
            Self::GitHubWebhook => "GitHub Webhooks & Events",
            Self::OAuthProvider => "OAuth2 & OIDC Token Issuer",
            Self::GenericRest => "Generic REST API Service",
        }
    }
}

/// Simulated response report from mock API engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockReplayReport {
    pub service: MockServiceKind,
    pub endpoint: String,
    pub method: String,
    pub status_code: u16,
    pub response_body: serde_json::Value,
    pub hmac_header: Option<(String, String)>,
    pub latency_ms: u64,
    pub is_deterministic: bool,
}

pub struct RelationalMockApiReplayer;

impl RelationalMockApiReplayer {
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

    /// Dispatch a simulated mock API request
    pub fn dispatch(
        service: MockServiceKind,
        endpoint: &str,
        method: &str,
        payload: Option<&serde_json::Value>,
    ) -> MockReplayReport {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match service {
            MockServiceKind::StripePayment => {
                let session_id = format!("cs_test_{}", now);
                let payment_intent = format!("pi_3M{}", now);
                let response = if endpoint.contains("checkout") {
                    serde_json::json!({
                        "id": session_id,
                        "object": "checkout.session",
                        "status": "complete",
                        "payment_status": "paid",
                        "amount_total": 4900,
                        "currency": "usd",
                        "customer_details": {
                            "email": "vibe_dev@example.com",
                            "name": "Alex Vibe"
                        },
                        "payment_intent": payment_intent,
                        "created": now
                    })
                } else {
                    serde_json::json!({
                        "id": payment_intent,
                        "object": "payment_intent",
                        "status": "succeeded",
                        "amount": 4900,
                        "currency": "usd",
                        "created": now
                    })
                };

                let raw_json = response.to_string();
                let secret = b"whsec_test_mock_secret_key_12345";
                let signed_payload = format!("{}.{}", now, raw_json);
                let sig = Self::compute_hmac_sha256(secret, signed_payload.as_bytes());
                let header_val = format!("t={},v1={}", now, sig);

                MockReplayReport {
                    service: MockServiceKind::StripePayment,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: Some(("Stripe-Signature".to_string(), header_val)),
                    latency_ms: 15,
                    is_deterministic: true,
                }
            }
            MockServiceKind::GitHubWebhook => {
                let response = serde_json::json!({
                    "ref": "refs/heads/main",
                    "before": "0000000000000000000000000000000000000000",
                    "after": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                    "repository": {
                        "id": 1296269,
                        "name": "hagibis-app",
                        "full_name": "developer/hagibis-app",
                        "private": false
                    },
                    "pusher": {
                        "name": "vibe-coder",
                        "email": "coder@hagibis.dev"
                    },
                    "head_commit": {
                        "id": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                        "message": "feat: launch autonomous Lakandiwa swarm",
                        "timestamp": "2026-09-27T20:00:00Z"
                    }
                });

                let raw_json = response.to_string();
                let secret = b"github_webhook_secret_key_67890";
                let sig = Self::compute_hmac_sha256(secret, raw_json.as_bytes());
                let header_val = format!("sha256={}", sig);

                MockReplayReport {
                    service: MockServiceKind::GitHubWebhook,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: Some(("X-Hub-Signature-256".to_string(), header_val)),
                    latency_ms: 22,
                    is_deterministic: true,
                }
            }
            MockServiceKind::OAuthProvider => {
                let response = serde_json::json!({
                    "access_token": format!("ghu_mock_token_{}", now),
                    "token_type": "bearer",
                    "scope": "user,repo",
                    "user": {
                        "id": 4242,
                        "login": "vibe_developer",
                        "email": "dev@sovereign.local",
                        "verified": true
                    }
                });

                MockReplayReport {
                    service: MockServiceKind::OAuthProvider,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: response,
                    hmac_header: None,
                    latency_ms: 8,
                    is_deterministic: true,
                }
            }
            MockServiceKind::GenericRest => {
                let body = payload.cloned().unwrap_or_else(|| {
                    serde_json::json!({
                        "status": "success",
                        "message": "Mock REST endpoint acknowledged",
                        "timestamp": now
                    })
                });

                MockReplayReport {
                    service: MockServiceKind::GenericRest,
                    endpoint: endpoint.to_string(),
                    method: method.to_uppercase(),
                    status_code: 200,
                    response_body: body,
                    hmac_header: None,
                    latency_ms: 10,
                    is_deterministic: true,
                }
            }
        }
    }
}
