use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use chrono::{Utc, Duration};
use crate::providers::ollama::OllamaProvider;
use crate::traits::HgbProvider;

/// Entity types supported by the synthetic persona generator
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    User,
    Customer,
    Order,
    Payment,
    AuthSession,
}

impl EntityKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "user" | "users" => Some(Self::User),
            "customer" | "customers" => Some(Self::Customer),
            "order" | "orders" => Some(Self::Order),
            "payment" | "payments" | "transaction" | "transactions" => Some(Self::Payment),
            "session" | "sessions" | "auth" | "auth_session" => Some(Self::AuthSession),
            _ => None,
        }
    }

    pub fn table_name(&self) -> &'static str {
        match self {
            Self::User => "users",
            Self::Customer => "customers",
            Self::Order => "orders",
            Self::Payment => "payments",
            Self::AuthSession => "auth_sessions",
        }
    }
}

/// A synthetic data record with typed attributes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SeedRecord {
    pub id: String,
    pub entity: String,
    pub fields: HashMap<String, serde_json::Value>,
}

/// Complete generated batch with SQL insert script and JSON representations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SeedBatch {
    pub entity: String,
    pub records: Vec<SeedRecord>,
    pub sql_script: String,
    pub json_export: String,
}

/// Instant Persona & Synthetic Data Seed Engine
pub struct PersonaSeedEngine;

impl PersonaSeedEngine {
    /// Generate an authentic batch of synthetic records for an entity using real LLMs or dynamic generation
    pub fn generate_batch(entity_str: &str, count: usize, _seed: Option<u64>) -> Result<SeedBatch> {
        let kind = EntityKind::parse(entity_str)
            .ok_or_else(|| HgbError::validation(format!("Unsupported entity kind: '{}'. Valid: users, customers, orders, payments, auth_sessions", entity_str)))?;

        let mut records = Vec::with_capacity(count);

        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));
        let prompt = format!("Generate {} realistic JSON records for entity '{}' as a JSON array of objects. Only valid JSON array.", count, kind.table_name());
        
        let rt = tokio::runtime::Runtime::new().unwrap();
        let llm_res = rt.block_on(async {
            provider.complete(&prompt, None).await
        });

        let mut parsed_records: Vec<serde_json::Value> = vec![];
        if let Ok(resp) = llm_res {
            let clean = resp.replace("```json", "").replace("```", "").trim().to_string();
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&clean) {
                parsed_records = arr;
            }
        }

        for i in 0..count {
            let mut fields = HashMap::new();
            let id = Uuid::new_v4().to_string();
            let now = Utc::now();
            let created_at = now - Duration::days((i as i64) % 30);

            if i < parsed_records.len() {
                if let Some(obj) = parsed_records[i].as_object() {
                    for (k, v) in obj {
                        fields.insert(k.clone(), v.clone());
                    }
                }
            }

            // Ensure basic schema is always present if LLM missed it or failed
            fields.insert("id".to_string(), serde_json::Value::String(id.clone()));
            fields.entry("created_at".to_string()).or_insert(serde_json::Value::String(created_at.to_rfc3339()));

            match kind {
                EntityKind::User | EntityKind::Customer => {
                    fields.entry("email".to_string()).or_insert(serde_json::Value::String(format!("user_{}@example.com", Uuid::new_v4().simple())));
                    fields.entry("name".to_string()).or_insert(serde_json::Value::String(format!("User {}", i)));
                }
                EntityKind::Order => {
                    fields.entry("customer_id".to_string()).or_insert(serde_json::Value::String(Uuid::new_v4().to_string()));
                    fields.entry("total_amount".to_string()).or_insert(serde_json::json!(99.99));
                }
                EntityKind::Payment => {
                    fields.entry("order_id".to_string()).or_insert(serde_json::Value::String(Uuid::new_v4().to_string()));
                    fields.entry("amount_cents".to_string()).or_insert(serde_json::json!(9999));
                    fields.entry("status".to_string()).or_insert(serde_json::Value::String("succeeded".to_string()));
                }
                EntityKind::AuthSession => {
                    fields.entry("user_id".to_string()).or_insert(serde_json::Value::String(Uuid::new_v4().to_string()));
                    fields.entry("token".to_string()).or_insert(serde_json::Value::String(Uuid::new_v4().to_string()));
                }
            }

            records.push(SeedRecord { id, entity: kind.table_name().to_string(), fields });
        }

        let sql_script = Self::generate_sql_insert(&records, kind.table_name());
        let json_export = serde_json::to_string_pretty(&records)
            .map_err(|e| HgbError::serialization(format!("JSON serialization failed: {}", e)))?;

        Ok(SeedBatch {
            entity: kind.table_name().to_string(),
            records,
            sql_script,
            json_export,
        })
    }

    /// Generate clean SQL INSERT INTO statement script
    pub fn generate_sql_insert(records: &[SeedRecord], table_name: &str) -> String {
        if records.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push(format!("-- Real Synthetic Seed Data generated by Hagibis PersonaSeedEngine"));
        lines.push(format!("-- Entity: {} | Records: {}", table_name, records.len()));

        let first = &records[0];
        let mut keys: Vec<&String> = first.fields.keys().collect();
        keys.sort();

        let cols = keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ");

        for r in records {
            let values: Vec<String> = keys.iter().map(|k| {
                match r.fields.get(*k) {
                    Some(serde_json::Value::String(s)) => format!("'{}'", s.replace('\'', "''")),
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    Some(serde_json::Value::Bool(b)) => if *b { "1".to_string() } else { "0".to_string() },
                    _ => "NULL".to_string(),
                }
            }).collect();

            lines.push(format!("INSERT INTO {} ({}) VALUES ({});", table_name, cols, values.join(", ")));
        }

        lines.join("\n")
    }

    /// Generate relational suite where Orders reference real Customer IDs
    pub fn generate_relational_suite(customer_count: usize, orders_per_customer: usize) -> Result<(SeedBatch, SeedBatch)> {
        let customers = Self::generate_batch("customers", customer_count, Some(101))?;
        let mut order_records = Vec::new();

        for cust in &customers.records {
            for j in 1..=orders_per_customer {
                let ord_id = Uuid::new_v4().to_string();

                let mut fields = HashMap::new();
                fields.insert("id".to_string(), serde_json::Value::String(ord_id.clone()));
                fields.insert("customer_id".to_string(), serde_json::Value::String(cust.id.clone()));
                fields.insert("total_amount".to_string(), serde_json::json!(42.0));
                fields.insert("currency".to_string(), serde_json::Value::String("USD".to_string()));
                fields.insert("status".to_string(), serde_json::Value::String("paid".to_string()));
                fields.insert("order_seq".to_string(), serde_json::json!(j));
                fields.insert("created_at".to_string(), serde_json::Value::String(Utc::now().to_rfc3339()));

                order_records.push(SeedRecord {
                    id: ord_id,
                    entity: "orders".to_string(),
                    fields,
                });
            }
        }

        let orders_sql = Self::generate_sql_insert(&order_records, "orders");
        let orders_json = serde_json::to_string_pretty(&order_records).unwrap_or_default();

        let orders_batch = SeedBatch {
            entity: "orders".to_string(),
            records: order_records,
            sql_script: orders_sql,
            json_export: orders_json,
        };

        Ok((customers, orders_batch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_user_seed_batch() {
        let batch = PersonaSeedEngine::generate_batch("users", 25, Some(42)).expect("generate users");
        assert_eq!(batch.records.len(), 25);
        assert_eq!(batch.entity, "users");
        assert!(batch.sql_script.contains("INSERT INTO users"));
    }

    #[test]
    fn test_relational_suite_linking() {
        let (customers, orders) = PersonaSeedEngine::generate_relational_suite(5, 3).expect("generate suite");
        assert_eq!(customers.records.len(), 5);
        assert_eq!(orders.records.len(), 15);

        for ord in &orders.records {
            let cust_id = ord.fields.get("customer_id").and_then(|c| c.as_str()).unwrap();
            assert!(customers.records.iter().any(|c| c.id == cust_id));
        }
    }
}
