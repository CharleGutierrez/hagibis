use serde::{Deserialize, Serialize};
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

        // 1. Stripe Topology
        endpoints.push(MirageEndpoint {
            route_pattern: r"^/v1/payment_intents(/\w+)?$".to_string(),
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
            simulated_latency_ms: 15,
        });
        endpoints.push(MirageEndpoint {
            route_pattern: r"^/v1/customers(/\w+)?$".to_string(),
            method: "POST".to_string(),
            service_name: "Stripe Mirage".to_string(),
            sample_response: serde_json::json!({
                "id": "cus_mirage_883719284",
                "object": "customer",
                "email": "dev@hagibis.ai",
                "name": "Hagibis Builder"
            }),
            simulated_latency_ms: 12,
        });

        // 2. GitHub Topology
        endpoints.push(MirageEndpoint {
            route_pattern: r"^/user$".to_string(),
            method: "GET".to_string(),
            service_name: "GitHub Mirage".to_string(),
            sample_response: serde_json::json!({
                "login": "hagibis-dev",
                "id": 10293847,
                "type": "User",
                "site_admin": false
            }),
            simulated_latency_ms: 8,
        });
        endpoints.push(MirageEndpoint {
            route_pattern: r"^/repos/[^/]+/[^/]+/pulls$".to_string(),
            method: "POST".to_string(),
            service_name: "GitHub Mirage".to_string(),
            sample_response: serde_json::json!({
                "id": 8920194,
                "number": 42,
                "state": "open",
                "title": "feat: autonomous autopilot agent",
                "html_url": "https://github.com/hagibis/repo/pull/42"
            }),
            simulated_latency_ms: 20,
        });

        // 3. Supabase Topology
        endpoints.push(MirageEndpoint {
            route_pattern: r"^/auth/v1/token".to_string(),
            method: "POST".to_string(),
            service_name: "Supabase Mirage".to_string(),
            sample_response: serde_json::json!({
                "access_token": "supabase_sbp_mock_token_jwt",
                "token_type": "bearer",
                "expires_in": 3600,
                "user": {
                    "id": "usr_9988112233",
                    "email": "user@example.com"
                }
            }),
            simulated_latency_ms: 10,
        });

        // 4. OpenAI / Anthropic Topology
        endpoints.push(MirageEndpoint {
            route_pattern: r"^/v1/chat/completions$".to_string(),
            method: "POST".to_string(),
            service_name: "OpenAI Mirage".to_string(),
            sample_response: serde_json::json!({
                "id": "chatcmpl_mirage_776655",
                "object": "chat.completion",
                "created": 1727700000,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Mirage response: verified code generation."
                    },
                    "finish_reason": "stop"
                }]
            }),
            simulated_latency_ms: 25,
        });

        Self {
            endpoints: Arc::new(RwLock::new(endpoints)),
        }
    }

    /// Dynamically loads an OpenAPI v3 / Swagger JSON specification into the Mirage engine
    pub async fn load_openapi_spec(&self, spec_json: &str) -> Result<usize, String> {
        let parsed: serde_json::Value = serde_json::from_str(spec_json)
            .map_err(|e| format!("Invalid OpenAPI JSON: {}", e))?;

        let paths = match parsed.get("paths").and_then(|p| p.as_object()) {
            Some(p) => p,
            None => return Err("Missing 'paths' object in OpenAPI specification".to_string()),
        };

        let mut count = 0;
        let mut endpoints = self.endpoints.write().await;

        for (path, methods_val) in paths {
            if let Some(methods) = methods_val.as_object() {
                for (method, op_val) in methods {
                    let method_upper = method.to_uppercase();
                    if ["GET", "POST", "PUT", "DELETE", "PATCH"].contains(&method_upper.as_str()) {
                        let regex_path = format!("^{}$", path.replace("{", "(?P<").replace("}", ">[^/]+)"));
                        let service_name = op_val.get("summary")
                            .and_then(|s| s.as_str())
                            .unwrap_or("OpenAPI Mirage Endpoint")
                            .to_string();

                        let sample_resp = op_val.get("responses")
                            .and_then(|r| r.get("200").or_else(|| r.get("201")).or_else(|| r.get("default")))
                            .and_then(|r| r.get("content"))
                            .and_then(|c| c.get("application/json"))
                            .and_then(|j| j.get("example").or_else(|| j.get("schema")))
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({"status": "ok", "mocked_by": "hgb_openapi_mirage"}));

                        endpoints.push(MirageEndpoint {
                            route_pattern: regex_path,
                            method: method_upper,
                            service_name,
                            sample_response: sample_resp,
                            simulated_latency_ms: 10,
                        });
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
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
        
        let req_builder = match method {
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

    #[tokio::test]
    async fn test_api_mirage_openapi_and_topology() {
        let engine = ApiMirageEngine::new();
        // Verify GitHub topology
        let gh_rep = engine.execute_mirage_call("GET", "/user").await;
        assert_eq!(gh_rep.status, 200);
        assert!(gh_rep.is_synthetic);
        assert!(gh_rep.payload_snippet.contains("hagibis-dev"));

        // Load dynamic OpenAPI spec
        let openapi_json = r#"{
            "openapi": "3.0.0",
            "paths": {
                "/api/v2/analytics": {
                    "get": {
                        "summary": "Analytics Stream",
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "example": { "active_sessions": 420 }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }"#;

        let loaded = engine.load_openapi_spec(openapi_json).await.unwrap();
        assert_eq!(loaded, 1);

        let dynamic_call = engine.execute_mirage_call("GET", "/api/v2/analytics").await;
        assert_eq!(dynamic_call.status, 200);
        assert!(dynamic_call.is_synthetic);
        assert!(dynamic_call.payload_snippet.contains("420"));
    }
}
