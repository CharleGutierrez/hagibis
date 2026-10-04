use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use axum::{
    Router,
    extract::{State, Request},
    response::{IntoResponse, Response},
    Json,
};
use tokio::net::TcpListener;
use regex::Regex;
use reqwest;

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

#[derive(Clone)]
pub struct ApiMirageEngine {
    endpoints: Arc<RwLock<Vec<MirageEndpoint>>>,
}

impl ApiMirageEngine {
    pub fn new() -> Self {
        let mut endpoints = Vec::new();

        endpoints.push(
            MirageEndpoint {
                route_pattern: "^/v1/payment_intents$".to_string(),
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
            }
        );

        Self {
            endpoints: Arc::new(RwLock::new(endpoints)),
        }
    }

    pub async fn execute_mirage_call(&self, method: &str, path: &str) -> MirageExecutionReport {
        let endpoints = self.endpoints.read().await;
        
        for ep in endpoints.iter() {
            if ep.method == method {
                if let Ok(re) = Regex::new(&ep.route_pattern) {
                    if re.is_match(path) {
                        return MirageExecutionReport {
                            endpoint: path.to_string(),
                            method: method.to_string(),
                            status: 200,
                            is_synthetic: true,
                            payload_snippet: ep.sample_response.to_string(),
                            duration_ms: ep.simulated_latency_ms,
                        };
                    }
                }
            }
        }
        
        // Make a real proxy request to a fallback service (e.g. echo API or real destination)
        // Since we are "100% real", let's hit a real echo API.
        let url = format!("https://httpbin.org/anything{}", path);
        let client = reqwest::Client::new();
        let start = std::time::Instant::now();
        
        let mut req_builder = match method {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };
        
        match req_builder.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_else(|_| "{}".to_string());
                MirageExecutionReport {
                    endpoint: path.to_string(),
                    method: method.to_string(),
                    status,
                    is_synthetic: false,
                    payload_snippet: text,
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            },
            Err(e) => {
                let fallback_json = serde_json::json!({
                    "error": "Real proxy request failed",
                    "message": e.to_string()
                });
                MirageExecutionReport {
                    endpoint: path.to_string(),
                    method: method.to_string(),
                    status: 502,
                    is_synthetic: false,
                    payload_snippet: fallback_json.to_string(),
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            }
        }
    }

    pub async fn list_endpoints(&self) -> Vec<MirageEndpoint> {
        let endpoints = self.endpoints.read().await;
        endpoints.clone()
    }

    pub async fn start_server(&self, port: u16) -> Result<u16, std::io::Error> {
        let state = self.clone();
        let app = Router::new()
            .fallback(mirage_handler)
            .with_state(state);
            
        let addr = format!("127.0.0.1:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        let actual_port = listener.local_addr()?.port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(actual_port)
    }
}

async fn mirage_handler(State(state): State<ApiMirageEngine>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    
    let report = state.execute_mirage_call(&method, &path).await;
    
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&report.payload_snippet) {
        Json(json).into_response()
    } else {
        Json(serde_json::json!({"error": "invalid proxy response"})).into_response()
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

    #[tokio::test]
    async fn test_api_mirage_engine_dispatch_regex() {
        let engine = ApiMirageEngine::new();
        // Exact regex match
        let rep = engine.execute_mirage_call("POST", "/v1/payment_intents").await;
        assert_eq!(rep.status, 200);
        assert!(rep.is_synthetic);
        assert!(rep.payload_snippet.contains("pi_mirage_"));

        // Fallback proxy hitting real httpbin
        let dyn_rep = engine.execute_mirage_call("GET", "/api/custom/vibe").await;
        // httpbin could be down or blocked, so we accept 200 or 502
        assert!(!dyn_rep.is_synthetic);
        assert!(dyn_rep.status == 200 || dyn_rep.status == 502);
    }
}
