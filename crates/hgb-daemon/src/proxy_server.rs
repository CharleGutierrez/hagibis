//! # High-Concurrency Multi-Protocol Axum Proxy Server
//!
//! Drop-in HTTP reverse proxy server implementing:
//! 1. Anthropic protocol (`POST /v1/messages`) for Claude Code CLI (`claude`), Claude Desktop, and JetBrains.
//!    Supports full Anthropic SSE streaming (`message_start`, `content_block_start`, `content_block_delta`, `message_delta`, `message_stop`).
//! 2. OpenAI protocol (`POST /v1/chat/completions`) for Cursor, Cline, Codex, Aider, OpenCode.
//! 3. OpenAI model catalog (`GET /v1/models`).
//! 4. Superpower injections: passes incoming context through `TokenShredder` for up to 90% token reduction.

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::stream::{self, Stream};
use hgb_core::cloud_fabric::{FabricChatRequest, FabricMessage, UniversalCloudFabric};
use hgb_core::token_shredder::TokenShredder;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Configuration for the Hagibis Proxy Server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyServerConfig {
    pub host: String,
    pub port: u16,
    pub default_provider: Option<String>,
    pub shred_tokens: bool,
}

impl Default for ProxyServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8082,
            default_provider: None,
            shred_tokens: true,
        }
    }
}

/// Shared proxy application state
#[derive(Clone)]
pub struct ProxyState {
    pub config: ProxyServerConfig,
    pub fabric: UniversalCloudFabric,
    pub shredder: TokenShredder,
}

// ---------------------------------------------------------------------------
// Anthropic Protocol Structures (`/v1/messages`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicBlock {
    pub r#type: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicIncomingMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicIncomingMessage>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicResponseMessage {
    pub id: String,
    pub r#type: String,
    pub role: String,
    pub model: String,
    pub content: Vec<AnthropicBlock>,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: AnthropicUsage,
}

// ---------------------------------------------------------------------------
// OpenAI Protocol Structures (`/v1/chat/completions` & `/v1/models`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiIncomingMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub messages: Vec<OpenAiIncomingMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChoiceMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChoice {
    pub index: usize,
    pub message: OpenAiChoiceMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAiChoice>,
    pub usage: OpenAiUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelItem {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelItem>,
}

// ---------------------------------------------------------------------------
// Router & Handlers
// ---------------------------------------------------------------------------

pub fn build_proxy_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/v1/messages", post(handle_anthropic_messages))
        .route("/v1/chat/completions", post(handle_openai_chat_completions))
        .route("/v1/models", get(handle_list_models))
        .route("/health", get(handle_health))
        .with_state(state)
}

/// GET /health
async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "online",
        "service": "hagibis-proxy",
        "version": "0.1.0"
    }))
}

/// GET /v1/models
async fn handle_list_models(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut models = vec![
        ModelItem {
            id: "claude-3-5-sonnet-20241022".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "anthropic".to_string(),
        },
        ModelItem {
            id: "claude-3.5-sonnet".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "anthropic".to_string(),
        },
        ModelItem {
            id: "gpt-4o".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "openai".to_string(),
        },
        ModelItem {
            id: "o1-preview".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "openai".to_string(),
        },
        ModelItem {
            id: "gemini-2.5-flash".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "google".to_string(),
        },
        ModelItem {
            id: "llama-3.3-70b-versatile".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "groq".to_string(),
        },
        ModelItem {
            id: "meta/llama-3.3-70b-instruct".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "nvidia".to_string(),
        },
        ModelItem {
            id: "qwen2.5-coder:7b".to_string(),
            object: "model".to_string(),
            created: now,
            owned_by: "ollama".to_string(),
        },
    ];

    // Append all models from fabric catalog
    for desc in state.fabric.catalog() {
        for m in desc.available_models {
            if !models.iter().any(|x| x.id == m) {
                models.push(ModelItem {
                    id: m,
                    object: "model".to_string(),
                    created: now,
                    owned_by: desc.id.as_str().to_string(),
                });
            }
        }
    }

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

/// POST /v1/messages (Anthropic Protocol)
async fn handle_anthropic_messages(
    State(state): State<Arc<ProxyState>>,
    Json(payload): Json<AnthropicMessagesRequest>,
) -> Response {
    // 1. Extract messages & apply TokenShredder superpower
    let mut fabric_messages = Vec::new();

    if let Some(sys) = payload.system {
        let shredded_sys = if state.config.shred_tokens {
            state.shredder.shred(&sys).content
        } else {
            sys
        };
        fabric_messages.push(FabricMessage::system(shredded_sys));
    }

    for msg in payload.messages {
        let raw_text = match msg.content {
            AnthropicContent::Text(t) => t,
            AnthropicContent::Blocks(blocks) => blocks
                .into_iter()
                .filter_map(|b| b.text)
                .collect::<Vec<_>>()
                .join("\n"),
        };

        let processed_text = if state.config.shred_tokens {
            state.shredder.shred(&raw_text).content
        } else {
            raw_text
        };

        fabric_messages.push(FabricMessage {
            role: msg.role,
            content: processed_text,
        });
    }

    // 2. Build cloud fabric request
    let fabric_req = FabricChatRequest {
        messages: fabric_messages,
        model: Some(payload.model.clone()),
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
        stream: payload.stream,
    };

    let priority_chain = state.config.default_provider.as_ref().map(|p| vec![p.as_str()]);
    let chain_slice = priority_chain.as_deref();

    let completion = match state.fabric.complete_with_failover(&fabric_req, chain_slice).await {
        Ok(c) => c,
        Err(e) => {
            let err_json = serde_json::json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": format!("Hagibis Proxy upstream failure: {e}")
                }
            });
            return (StatusCode::BAD_GATEWAY, Json(err_json)).into_response();
        }
    };

    let msg_id = format!("msg_{}", &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]);

    // 3. Streaming response: Anthropic SSE events
    if payload.stream.unwrap_or(false) {
        let stream_events = create_anthropic_sse_stream(
            msg_id,
            payload.model,
            completion.content,
            completion.prompt_tokens,
            completion.completion_tokens,
        );
        return Sse::new(stream_events)
            .keep_alive(KeepAlive::default())
            .into_response();
    }

    // 4. Non-streaming response: Anthropic JSON
    let resp = AnthropicResponseMessage {
        id: msg_id,
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        model: payload.model,
        content: vec![AnthropicBlock {
            r#type: "text".to_string(),
            text: Some(completion.content),
        }],
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: completion.prompt_tokens,
            output_tokens: completion.completion_tokens,
        },
    };

    Json(resp).into_response()
}

/// Creates Anthropic SSE event sequence
fn create_anthropic_sse_stream(
    msg_id: String,
    model: String,
    full_text: String,
    prompt_tokens: usize,
    completion_tokens: usize,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let mut events = Vec::new();

    // 1. message_start
    let msg_start_data = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": msg_id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": prompt_tokens,
                "output_tokens": 1
            }
        }
    });
    events.push(Event::default().event("message_start").data(msg_start_data.to_string()));

    // 2. content_block_start
    let block_start_data = serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "text",
            "text": ""
        }
    });
    events.push(Event::default().event("content_block_start").data(block_start_data.to_string()));

    // 3. content_block_delta (stream in chunks)
    let chunk_size = 40.max(full_text.len() / 8 + 1);
    let chars: Vec<char> = full_text.chars().collect();
    for chunk in chars.chunks(chunk_size) {
        let chunk_str: String = chunk.iter().collect();
        let delta_data = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": chunk_str
            }
        });
        events.push(Event::default().event("content_block_delta").data(delta_data.to_string()));
    }

    // 4. content_block_stop
    let block_stop_data = serde_json::json!({
        "type": "content_block_stop",
        "index": 0
    });
    events.push(Event::default().event("content_block_stop").data(block_stop_data.to_string()));

    // 5. message_delta
    let msg_delta_data = serde_json::json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": "end_turn",
            "stop_sequence": null
        },
        "usage": {
            "output_tokens": completion_tokens
        }
    });
    events.push(Event::default().event("message_delta").data(msg_delta_data.to_string()));

    // 6. message_stop
    let msg_stop_data = serde_json::json!({
        "type": "message_stop"
    });
    events.push(Event::default().event("message_stop").data(msg_stop_data.to_string()));

    stream::iter(events.into_iter().map(Ok))
}

/// POST /v1/chat/completions (OpenAI Protocol)
async fn handle_openai_chat_completions(
    State(state): State<Arc<ProxyState>>,
    Json(payload): Json<OpenAiChatRequest>,
) -> Response {
    let mut fabric_messages = Vec::new();

    for msg in payload.messages {
        let content = if state.config.shred_tokens {
            state.shredder.shred(&msg.content).content
        } else {
            msg.content
        };
        fabric_messages.push(FabricMessage {
            role: msg.role,
            content,
        });
    }

    let fabric_req = FabricChatRequest {
        messages: fabric_messages,
        model: Some(payload.model.clone()),
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
        stream: payload.stream,
    };

    let priority_chain = state.config.default_provider.as_ref().map(|p| vec![p.as_str()]);
    let completion = match state.fabric.complete_with_failover(&fabric_req, priority_chain.as_deref()).await {
        Ok(c) => c,
        Err(e) => {
            let err_json = serde_json::json!({
                "error": {
                    "message": format!("Hagibis OpenAI Proxy error: {e}"),
                    "type": "server_error"
                }
            });
            return (StatusCode::BAD_GATEWAY, Json(err_json)).into_response();
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());

    if payload.stream.unwrap_or(false) {
        let stream_events = create_openai_sse_stream(
            completion_id,
            payload.model,
            completion.content,
            now,
        );
        return Sse::new(stream_events)
            .keep_alive(KeepAlive::default())
            .into_response();
    }

    let resp = OpenAiChatResponse {
        id: completion_id,
        object: "chat.completion".to_string(),
        created: now,
        model: payload.model,
        choices: vec![OpenAiChoice {
            index: 0,
            message: OpenAiChoiceMessage {
                role: "assistant".to_string(),
                content: completion.content,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: OpenAiUsage {
            prompt_tokens: completion.prompt_tokens,
            completion_tokens: completion.completion_tokens,
            total_tokens: completion.prompt_tokens + completion.completion_tokens,
        },
    };

    Json(resp).into_response()
}

/// Creates OpenAI SSE event sequence
fn create_openai_sse_stream(
    id: String,
    model: String,
    full_text: String,
    created: u64,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let mut events = Vec::new();

    let chunk_size = 35.max(full_text.len() / 8 + 1);
    let chars: Vec<char> = full_text.chars().collect();

    for chunk in chars.chunks(chunk_size) {
        let chunk_str: String = chunk.iter().collect();
        let chunk_data = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "content": chunk_str
                    },
                    "finish_reason": null
                }
            ]
        });
        events.push(Event::default().data(chunk_data.to_string()));
    }

    // Final finish stop frame
    let stop_data = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }
        ]
    });
    events.push(Event::default().data(stop_data.to_string()));
    events.push(Event::default().data("[DONE]"));

    stream::iter(events.into_iter().map(Ok))
}

/// Launch the Proxy Server in background or current task
pub async fn start_proxy_server(config: ProxyServerConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let state = Arc::new(ProxyState {
        config: config.clone(),
        fabric: UniversalCloudFabric::new(),
        shredder: TokenShredder::default(),
    });

    let app = build_proxy_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_request_deserialization() {
        let json = r#"{
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "Hello from Claude Code!"}
            ],
            "max_tokens": 4096,
            "stream": true
        }"#;

        let parsed: AnthropicMessagesRequest = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.model, "claude-3-5-sonnet-20241022");
        assert_eq!(parsed.stream, Some(true));
        assert_eq!(parsed.messages.len(), 1);
    }

    #[test]
    fn test_openai_chat_request_deserialization() {
        let json = r#"{
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "Refactor this struct."}
            ],
            "temperature": 0.5
        }"#;

        let parsed: OpenAiChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.model, "gpt-4o");
        assert_eq!(parsed.temperature, Some(0.5));
    }
}
