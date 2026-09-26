use serde::{Deserialize, Serialize};
use serde_json::json;

/// Information about a failed or unconfigured outgoing third-party network call
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterceptedCall {
    pub method: String,
    pub url_path: String,
    pub status: u16,
    pub body_snippet: Option<String>,
}

/// Synthesized mock HTTP response
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub json_body: serde_json::Value,
}

/// Report summarizing the generated zero-mock route
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZeroMockReport {
    pub path_pattern: String,
    pub mocked_response: MockResponse,
    pub schema_inferred: String,
    pub is_dynamic_seed: bool,
}

/// Autonomous "Stub-Anything" Zero-Mock Fabric
pub struct ZeroMockFabric;

impl ZeroMockFabric {
    /// Analyze an unconfigured/failed HTTP call and synthesize a realistic JSON mock response
    pub fn synthesize_mock_for_call(call: &InterceptedCall) -> ZeroMockReport {
        let path = call.url_path.to_lowercase();

        let (mock_body, schema_name) = if path.contains("charge") || path.contains("payment") || path.contains("stripe") {
            (
                json!({
                    "id": format!("ch_{}", chrono::Utc::now().timestamp_millis()),
                    "object": "charge",
                    "amount": 4900,
                    "currency": "usd",
                    "status": "succeeded",
                    "paid": true,
                    "created": chrono::Utc::now().timestamp(),
                }),
                "PaymentIntent"
            )
        } else if path.contains("user") || path.contains("profile") || path.contains("account") {
            (
                json!({
                    "id": "usr_vibe_999",
                    "email": "developer@vibe.local",
                    "name": "Sovereign Vibe Coder",
                    "role": "admin",
                    "active": true,
                    "created_at": chrono::Utc::now().to_rfc3339()
                }),
                "UserProfile"
            )
        } else if path.contains("notify") || path.contains("sms") || path.contains("email") {
            (
                json!({
                    "status": "queued",
                    "message_id": format!("msg_{}", chrono::Utc::now().timestamp_millis()),
                    "recipients": 1,
                    "delivered": true
                }),
                "NotificationReceipt"
            )
        } else {
            (
                json!({
                    "data": [
                        { "id": 1, "title": "Vibe Scaffolding Mock #1", "active": true },
                        { "id": 2, "title": "Vibe Scaffolding Mock #2", "active": false }
                    ],
                    "total": 2,
                    "status": "ok"
                }),
                "GenericCollection"
            )
        };

        let response = MockResponse {
            status: 200,
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Hgb-ZeroMock".to_string(), "true".to_string()),
            ],
            json_body: mock_body,
        };

        ZeroMockReport {
            path_pattern: call.url_path.clone(),
            mocked_response: response,
            schema_inferred: schema_name.to_string(),
            is_dynamic_seed: true,
        }
    }
}
