use chrono::Utc;
use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, RwLock};

/// Configuration options for the ephemeral mock server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockFabricConfig {
    pub resource_name: String,
    pub schema_template: Value,
    pub preferred_port: Option<u16>,
    pub seed_count: usize,
}

impl Default for MockFabricConfig {
    fn default() -> Self {
        Self {
            resource_name: "items".to_string(),
            schema_template: json!({
                "id": "item_1",
                "name": "Sample Item",
                "email": "user@example.com",
                "price": 29.99,
                "is_active": true,
                "status": "active"
            }),
            preferred_port: None,
            seed_count: 5,
        }
    }
}

/// Active handle to an ephemeral localhost Mock Fabric server
pub struct MockFabricServer {
    port: u16,
    base_url: String,
    resource_name: String,
    store: Arc<RwLock<HashMap<String, Value>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl MockFabricServer {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    pub async fn count(&self) -> usize {
        self.store.read().await.len()
    }

    pub async fn get_all(&self) -> Vec<Value> {
        self.store.read().await.values().cloned().collect()
    }

    /// Terminate and release the mock server port
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for MockFabricServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Ephemeral Mock Fabric with Dynamic Schema-to-Route Mapper & Synthetic Data Generator
pub struct MockFabric;

impl MockFabric {
    /// Start an in-memory ephemeral HTTP REST API server
    pub async fn start(config: MockFabricConfig) -> Result<MockFabricServer> {
        let port_to_bind = config.preferred_port.unwrap_or(0);
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port_to_bind))
            .await
            .map_err(|e| HgbError::Network(format!("Failed to bind mock server listener: {}", e)))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| HgbError::Network(format!("Failed to get local address: {}", e)))?;
        let port = local_addr.port();
        let base_url = format!("http://127.0.0.1:{}", port);
        let resource = config.resource_name.trim_start_matches('/').to_string();

        let store: Arc<RwLock<HashMap<String, Value>>> = Arc::new(RwLock::new(HashMap::new()));

        // Seed realistic synthetic data
        for i in 1..=config.seed_count {
            let record = Self::generate_synthetic_record(&config.schema_template, i);
            let id = record
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| i.to_string());
            store.write().await.insert(id, record);
        }

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let store_clone = Arc::clone(&store);
        let resource_clone = resource.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((mut socket, _)) => {
                                let store_ref = Arc::clone(&store_clone);
                                let res_name = resource_clone.clone();
                                tokio::spawn(async move {
                                    let mut buf = vec![0u8; 8192];
                                    if let Ok(n) = socket.read(&mut buf).await {
                                        if n > 0 {
                                            let req_text = String::from_utf8_lossy(&buf[..n]);
                                            let response_bytes = Self::dispatch_http_request(&req_text, &res_name, &store_ref).await;
                                            let _ = socket.write_all(&response_bytes).await;
                                        }
                                    }
                                });
                            }
                            Err(_) => break,
                        }
                    }
                    _ = &mut shutdown_rx => {
                        break;
                    }
                }
            }
        });

        Ok(MockFabricServer {
            port,
            base_url,
            resource_name: resource,
            store,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// Dispatch incoming raw HTTP/1.1 request to CRUD storage operations
    async fn dispatch_http_request(
        raw_req: &str,
        resource_name: &str,
        store: &Arc<RwLock<HashMap<String, Value>>>,
    ) -> Vec<u8> {
        let first_line = raw_req.lines().next().unwrap_or("");
        let mut parts = first_line.split_whitespace();
        let method = parts.next().unwrap_or("GET");
        let path = parts.next().unwrap_or("/");

        // Handle CORS Preflight
        if method == "OPTIONS" {
            return format!(
                "HTTP/1.1 200 OK\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\n\
                Access-Control-Allow-Headers: Content-Type, Authorization\r\n\
                Content-Length: 0\r\n\r\n"
            )
            .into_bytes();
        }

        let clean_path = path.split('?').next().unwrap_or(path);
        let path_segments: Vec<&str> = clean_path.trim_matches('/').split('/').collect();

        // Check if route matches /api/{resource} or /{resource}
        let (is_match, item_id) = if path_segments.len() == 2 && path_segments[0] == "api" && path_segments[1] == resource_name {
            (true, None)
        } else if path_segments.len() == 3 && path_segments[0] == "api" && path_segments[1] == resource_name {
            (true, Some(path_segments[2]))
        } else if path_segments.len() == 1 && path_segments[0] == resource_name {
            (true, None)
        } else if path_segments.len() == 2 && path_segments[0] == resource_name {
            (true, Some(path_segments[1]))
        } else {
            (false, None)
        };

        if !is_match {
            let err_body = json!({ "error": "Not Found", "requested_path": path }).to_string();
            return Self::build_http_response(404, "Not Found", &err_body);
        }

        match (method, item_id) {
            // GET /api/{resource}
            ("GET", None) => {
                let items: Vec<Value> = store.read().await.values().cloned().collect();
                let body = serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
                Self::build_http_response(200, "OK", &body)
            }
            // GET /api/{resource}/{id}
            ("GET", Some(id)) => {
                let guard = store.read().await;
                if let Some(item) = guard.get(id) {
                    Self::build_http_response(200, "OK", &item.to_string())
                } else {
                    let err = json!({ "error": "Entity not found", "id": id }).to_string();
                    Self::build_http_response(404, "Not Found", &err)
                }
            }
            // POST /api/{resource}
            ("POST", None) => {
                let body_str = extract_http_body(raw_req);
                let mut new_item: Value = serde_json::from_str(body_str).unwrap_or_else(|_| json!({}));
                let id = new_item.get("id").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| {
                    format!("{}_{}", resource_name, Utc::now().timestamp_millis())
                });

                if let Some(obj) = new_item.as_object_mut() {
                    obj.insert("id".to_string(), json!(id));
                    obj.insert("created_at".to_string(), json!(Utc::now().to_rfc3339()));
                }

                store.write().await.insert(id, new_item.clone());
                Self::build_http_response(201, "Created", &new_item.to_string())
            }
            // PUT /api/{resource}/{id}
            ("PUT", Some(id)) => {
                let body_str = extract_http_body(raw_req);
                let updated_fields: Value = serde_json::from_str(body_str).unwrap_or_else(|_| json!({}));

                let mut guard = store.write().await;
                if let Some(existing) = guard.get_mut(id) {
                    if let (Some(ex_obj), Some(up_obj)) = (existing.as_object_mut(), updated_fields.as_object()) {
                        for (k, v) in up_obj {
                            if k != "id" {
                                ex_obj.insert(k.clone(), v.clone());
                            }
                        }
                        ex_obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                    }
                    Self::build_http_response(200, "OK", &existing.to_string())
                } else {
                    let err = json!({ "error": "Entity not found", "id": id }).to_string();
                    Self::build_http_response(404, "Not Found", &err)
                }
            }
            // DELETE /api/{resource}/{id}
            ("DELETE", Some(id)) => {
                let mut guard = store.write().await;
                if guard.remove(id).is_some() {
                    let ok_body = json!({ "status": "deleted", "id": id }).to_string();
                    Self::build_http_response(200, "OK", &ok_body)
                } else {
                    let err = json!({ "error": "Entity not found", "id": id }).to_string();
                    Self::build_http_response(404, "Not Found", &err)
                }
            }
            _ => {
                let err = json!({ "error": "Method Not Allowed", "method": method }).to_string();
                Self::build_http_response(405, "Method Not Allowed", &err)
            }
        }
    }

    fn build_http_response(status: u16, reason: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {} {}\r\n\
            Content-Type: application/json\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Content-Length: {}\r\n\
            Connection: close\r\n\r\n{}",
            status,
            reason,
            body.len(),
            body
        )
        .into_bytes()
    }

    /// Generate synthetic records with realistic names, emails, prices, timestamps, and booleans
    pub fn generate_synthetic_record(schema: &Value, index: usize) -> Value {
        static NAMES: &[&str] = &[
            "Ada Lovelace",
            "Alan Turing",
            "Grace Hopper",
            "Margaret Hamilton",
            "Claude Shannon",
            "John von Neumann",
            "Barbara Liskov",
            "Linus Torvalds",
            "Dennis Ritchie",
            "Ken Thompson",
        ];

        let name = NAMES[(index - 1) % NAMES.len()];
        let name_slug = name.to_lowercase().replace(' ', ".");

        if let Some(obj) = schema.as_object() {
            let mut result = serde_json::Map::new();
            for (key, sample_val) in obj {
                let lower = key.to_lowercase();
                let synthetic_val = if lower == "id" || lower.ends_with("_id") {
                    json!(format!("id_{}", index))
                } else if lower.contains("name") {
                    json!(name)
                } else if lower.contains("email") {
                    json!(format!("{}@example.com", name_slug))
                } else if lower.contains("price") || lower.contains("amount") || lower.contains("cost") {
                    json!(19.99 + (index as f64 * 10.50))
                } else if lower.contains("timestamp") || lower.contains("created") || lower.contains("date") {
                    json!(Utc::now().to_rfc3339())
                } else if lower.contains("active") || lower.contains("enabled") || lower.contains("valid") {
                    json!(index % 2 == 1)
                } else if lower.contains("status") {
                    let statuses = ["active", "pending", "completed", "archived"];
                    json!(statuses[(index - 1) % statuses.len()])
                } else {
                    sample_val.clone()
                };
                result.insert(key.clone(), synthetic_val);
            }
            Value::Object(result)
        } else {
            json!({
                "id": format!("id_{}", index),
                "name": name,
                "email": format!("{}@example.com", name_slug),
                "price": 29.99,
                "is_active": true,
                "created_at": Utc::now().to_rfc3339()
            })
        }
    }
}

fn extract_http_body(raw: &str) -> &str {
    if let Some(pos) = raw.find("\r\n\r\n") {
        &raw[pos + 4..]
    } else if let Some(pos) = raw.find("\n\n") {
        &raw[pos + 2..]
    } else {
        ""
    }
}
