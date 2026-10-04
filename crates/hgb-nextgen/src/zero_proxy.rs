use serde::{Deserialize, Serialize};
use serde_json::json;
use hgb_core::providers::ollama::OllamaProvider;
use hgb_core::traits::HgbProvider;
use uuid::Uuid;
use chrono::Utc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterceptedCall {
    pub method: String,
    pub url_path: String,
    pub status: u16,
    pub body_snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub json_body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZeroProxyReport {
    pub path_pattern: String,
    pub proxyed_response: ProxyResponse,
    pub schema_inferred: String,
    pub is_dynamic_seed: bool,
}

pub struct ZeroLocalProxyFabric;

impl ZeroLocalProxyFabric {
    pub async fn synthesize_proxy_for_call(call: &InterceptedCall) -> ZeroProxyReport {
        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));
        let prompt = format!("Generate a realistic JSON proxy response body for the following API request path: '{}'. Only output valid JSON without any markdown formatting.", call.url_path);
        
        let mut proxy_body = json!({});
        let mut schema_name = "GenericResponse".to_string();

        if let Ok(response) = provider.complete(&prompt, None).await {
            let clean_response = response.replace("```json", "").replace("```", "").trim().to_string();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&clean_response) {
                proxy_body = parsed;
                schema_name = "DynamicLlmSchema".to_string();
            }
        }

        if proxy_body.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            // Real programmable dynamic fallback
            let mut body = serde_json::Map::new();
            body.insert("id".to_string(), json!(Uuid::new_v4().to_string()));
            body.insert("status".to_string(), json!("success"));
            body.insert("timestamp".to_string(), json!(Utc::now().to_rfc3339()));
            body.insert("path_echo".to_string(), json!(call.url_path.clone()));
            body.insert("method".to_string(), json!(call.method.clone()));
            proxy_body = serde_json::Value::Object(body);
        }

        let response = ProxyResponse {
            status: 200,
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Hgb-ZeroProxy".to_string(), "true".to_string()),
            ],
            json_body: proxy_body,
        };

        ZeroProxyReport {
            path_pattern: call.url_path.clone(),
            proxyed_response: response,
            schema_inferred: schema_name,
            is_dynamic_seed: true,
        }
    }
}
