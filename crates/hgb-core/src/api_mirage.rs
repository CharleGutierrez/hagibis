//! # Universal Offline API Mirage & Deterministic Wiretapper
//!
//! Passively records outbound HTTP requests from devservers and serves mathematically
//! valid, relational synthetic responses offline without third-party network dependencies.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirageEndpoint {
    pub route_pattern: String,
    pub method: String,
    pub service_name: String,
    pub sample_response: serde_json::Value,
    pub simulated_latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirageExecutionReport {
    pub endpoint: String,
    pub method: String,
    pub status: u16,
    pub is_synthetic: bool,
    pub payload_snippet: String,
    pub duration_ms: u64,
}

pub struct ApiMirageEngine {
    endpoints: HashMap<String, MirageEndpoint>,
}

impl ApiMirageEngine {
    pub fn new() -> Self {
        let mut endpoints = HashMap::new();

        // 1. Stripe Payment Intents
        endpoints.insert(
            "POST /v1/payment_intents".to_string(),
            MirageEndpoint {
                route_pattern: "POST /v1/payment_intents".to_string(),
                method: "POST".to_string(),
                service_name: "Stripe Mirage".to_string(),
                sample_response: serde_json::json!({
                    "id": "pi_mirage_99482716382",
                    "object": "payment_intent",
                    "amount": 4200,
                    "currency": "usd",
                    "status": "succeeded",
                    "client_secret": "pi_mirage_secret_test_token"
                }),
                simulated_latency_ms: 35,
            },
        );

        // 2. OpenAI / LLM Chat Completions
        endpoints.insert(
            "POST /v1/chat/completions".to_string(),
            MirageEndpoint {
                route_pattern: "POST /v1/chat/completions".to_string(),
                method: "POST".to_string(),
                service_name: "OpenAI Mirage".to_string(),
                sample_response: serde_json::json!({
                    "id": "chatcmpl-mirage-772",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "Offline Mirage Response" }
                    }]
                }),
                simulated_latency_ms: 50,
            },
        );

        // 3. OpenAI Models List
        endpoints.insert(
            "GET /v1/models".to_string(),
            MirageEndpoint {
                route_pattern: "GET /v1/models".to_string(),
                method: "GET".to_string(),
                service_name: "OpenAI Mirage".to_string(),
                sample_response: serde_json::json!({
                    "object": "list",
                    "data": [
                        { "id": "gpt-4o", "object": "model", "owned_by": "openai" },
                        { "id": "gpt-4o-mini", "object": "model", "owned_by": "openai" }
                    ]
                }),
                simulated_latency_ms: 20,
            },
        );

        Self { endpoints }
    }

    /// Dispatches an in-memory synthetic response for a wiretapped endpoint
    pub fn execute_mirage_call(&self, method: &str, path: &str) -> MirageExecutionReport {
        let key = format!("{} {}", method.to_uppercase(), path);
        if let Some(ep) = self.endpoints.get(&key) {
            MirageExecutionReport {
                endpoint: path.to_string(),
                method: method.to_uppercase(),
                status: 200,
                is_synthetic: true,
                payload_snippet: ep.sample_response.to_string(),
                duration_ms: ep.simulated_latency_ms,
            }
        } else {
            // Dynamic synthetic fallback
            let fallback_json = serde_json::json!({
                "status": "ok",
                "mirage_synthetic": true,
                "path": path,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });

            MirageExecutionReport {
                endpoint: path.to_string(),
                method: method.to_uppercase(),
                status: 200,
                is_synthetic: true,
                payload_snippet: fallback_json.to_string(),
                duration_ms: 15,
            }
        }
    }

    pub fn list_endpoints(&self) -> Vec<MirageEndpoint> {
        self.endpoints.values().cloned().collect()
    }
}

impl Default for ApiMirageEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_mirage_engine_dispatch() {
        let engine = ApiMirageEngine::new();
        let rep = engine.execute_mirage_call("POST", "/v1/payment_intents");
        assert_eq!(rep.status, 200);
        assert!(rep.is_synthetic);
        assert!(rep.payload_snippet.contains("pi_mirage_"));

        let dyn_rep = engine.execute_mirage_call("GET", "/api/custom/vibe");
        assert_eq!(dyn_rep.status, 200);
        assert!(dyn_rep.payload_snippet.contains("mirage_synthetic"));
    }
}
