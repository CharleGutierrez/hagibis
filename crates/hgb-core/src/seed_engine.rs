use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    // Curated edge-case first names (international, multi-byte UTF-8, accented)
    const FIRST_NAMES: &'static [&'static str] = &[
        "Alex", "René", "佐々木", "José", "Fatima", "Aaliyah", "Björn",
        "Chloé", "Dmitri", "Esperanza", "Liam", "Kavita", "Sora", "Noor",
        "🚀 Sparky", "O'Connor", "Null_Pointer", "Dr. Jane"
    ];

    const LAST_NAMES: &'static [&'static str] = &[
        "Vance", "Descartes", "希", "María", "Al-Mansoor", "Smith", "Lindqvist",
        "Dubois", "Ivanov", "Rodriguez", "Chen", "Patel", "Takahashi", "Khan",
        "DropTable;", "van der Beek", "St. John", "Test-User"
    ];

    const DOMAINS: &'static [&'static str] = &[
        "gmail.com", "outlook.com", "vibe.dev", "corp.internal", "proton.me", "startup.io"
    ];

    const CITIES: &'static [&'static str] = &[
        "San Francisco, CA", "Tokyo, Japan", "Stockholm, Sweden", "Paris, France",
        "Berlin, Germany", "Manila, Philippines", "London, UK", "Austin, TX"
    ];

    const STATUSES: &'static [&'static str] = &[
        "active", "pending_verification", "suspended", "trialing", "premium"
    ];

    /// Generate a deterministic pseudo-random sequence
    fn next_rand(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state
    }

    /// Generate an authentic batch of synthetic records for an entity
    pub fn generate_batch(entity_str: &str, count: usize, seed: Option<u64>) -> Result<SeedBatch> {
        let kind = EntityKind::parse(entity_str)
            .ok_or_else(|| HgbError::validation(format!("Unsupported entity kind: '{}'. Valid: users, customers, orders, payments, auth_sessions", entity_str)))?;

        let mut rng = seed.unwrap_or(0xCAFE_BABE_1337_0001);
        let mut records = Vec::with_capacity(count);

        for i in 1..=count {
            let record = match kind {
                EntityKind::User | EntityKind::Customer => {
                    let fn_idx = (Self::next_rand(&mut rng) as usize) % Self::FIRST_NAMES.len();
                    let ln_idx = (Self::next_rand(&mut rng) as usize) % Self::LAST_NAMES.len();
                    let dom_idx = (Self::next_rand(&mut rng) as usize) % Self::DOMAINS.len();
                    let city_idx = (Self::next_rand(&mut rng) as usize) % Self::CITIES.len();
                    let st_idx = (Self::next_rand(&mut rng) as usize) % Self::STATUSES.len();

                    let first = Self::FIRST_NAMES[fn_idx];
                    let last = Self::LAST_NAMES[ln_idx];
                    let email = format!("{}.{}{}@{}", first.to_lowercase().replace(' ', "_"), last.to_lowercase().replace(' ', "_"), i, Self::DOMAINS[dom_idx]);
                    let prefix = if kind == EntityKind::User { "usr" } else { "cus" };
                    let id = format!("{}_{:06x}", prefix, (Self::next_rand(&mut rng) & 0xFFFFFF));

                    let mut fields = HashMap::new();
                    fields.insert("id".to_string(), serde_json::Value::String(id.clone()));
                    fields.insert("name".to_string(), serde_json::Value::String(format!("{} {}", first, last)));
                    fields.insert("email".to_string(), serde_json::Value::String(email));
                    fields.insert("city".to_string(), serde_json::Value::String(Self::CITIES[city_idx].to_string()));
                    fields.insert("status".to_string(), serde_json::Value::String(Self::STATUSES[st_idx].to_string()));
                    fields.insert("tier".to_string(), serde_json::Value::String(if i % 3 == 0 { "enterprise" } else if i % 2 == 0 { "pro" } else { "free" }.to_string()));
                    fields.insert("login_count".to_string(), serde_json::Value::Number(serde_json::Number::from((Self::next_rand(&mut rng) % 150) as u64)));
                    fields.insert("created_at".to_string(), serde_json::Value::String(format!("2026-09-{:02}T12:00:00Z", (i % 25) + 1)));

                    SeedRecord { id, entity: kind.table_name().to_string(), fields }
                }
                EntityKind::Order => {
                    let id = format!("ord_{:06x}", (Self::next_rand(&mut rng) & 0xFFFFFF));
                    let customer_id = format!("cus_{:06x}", (Self::next_rand(&mut rng) & 0xFFFFFF));
                    let cents = (Self::next_rand(&mut rng) % 50000) + 999;
                    let amount_usd = (cents as f64) / 100.0;

                    let mut fields = HashMap::new();
                    fields.insert("id".to_string(), serde_json::Value::String(id.clone()));
                    fields.insert("customer_id".to_string(), serde_json::Value::String(customer_id));
                    fields.insert("total_amount".to_string(), serde_json::json!(amount_usd));
                    fields.insert("currency".to_string(), serde_json::Value::String("USD".to_string()));
                    fields.insert("status".to_string(), serde_json::Value::String(if i % 4 == 0 { "refunded" } else if i % 3 == 0 { "pending" } else { "paid" }.to_string()));
                    fields.insert("item_count".to_string(), serde_json::Value::Number(serde_json::Number::from((Self::next_rand(&mut rng) % 5) + 1)));
                    fields.insert("created_at".to_string(), serde_json::Value::String(format!("2026-09-{:02}T14:30:00Z", (i % 25) + 1)));

                    SeedRecord { id, entity: kind.table_name().to_string(), fields }
                }
                EntityKind::Payment => {
                    let id = format!("ch_3M{:016x}", Self::next_rand(&mut rng));
                    let order_id = format!("ord_{:06x}", (Self::next_rand(&mut rng) & 0xFFFFFF));
                    let cents = (Self::next_rand(&mut rng) % 40000) + 1200;

                    let mut fields = HashMap::new();
                    fields.insert("id".to_string(), serde_json::Value::String(id.clone()));
                    fields.insert("order_id".to_string(), serde_json::Value::String(order_id));
                    fields.insert("amount_cents".to_string(), serde_json::Value::Number(serde_json::Number::from(cents)));
                    fields.insert("provider".to_string(), serde_json::Value::String("stripe".to_string()));
                    fields.insert("card_last4".to_string(), serde_json::Value::String(format!("{:04}", Self::next_rand(&mut rng) % 10000)));
                    fields.insert("status".to_string(), serde_json::Value::String("succeeded".to_string()));

                    SeedRecord { id, entity: kind.table_name().to_string(), fields }
                }
                EntityKind::AuthSession => {
                    let token = format!("jwt_vibe_{:032x}", Self::next_rand(&mut rng));
                    let user_id = format!("usr_{:06x}", (Self::next_rand(&mut rng) & 0xFFFFFF));

                    let mut fields = HashMap::new();
                    fields.insert("token".to_string(), serde_json::Value::String(token.clone()));
                    fields.insert("user_id".to_string(), serde_json::Value::String(user_id));
                    fields.insert("ip_address".to_string(), serde_json::Value::String(format!("192.0.2.{}", (Self::next_rand(&mut rng) % 250) + 1)));
                    fields.insert("user_agent".to_string(), serde_json::Value::String("Mozilla/5.0 (Vibe Developer Browser)".to_string()));
                    fields.insert("expires_in_sec".to_string(), serde_json::Value::Number(serde_json::Number::from(86400u64)));

                    SeedRecord { id: token, entity: kind.table_name().to_string(), fields }
                }
            };
            records.push(record);
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
        lines.push(format!("-- Synthetic Seed Data generated by Hagibis PersonaSeedEngine"));
        lines.push(format!("-- Entity: {} | Records: {}", table_name, records.len()));

        // Determine column keys from the first record
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
        let mut rng = 999912345u64;

        for cust in &customers.records {
            for j in 1..=orders_per_customer {
                let ord_id = format!("ord_{:06x}", (Self::next_rand(&mut rng) & 0xFFFFFF));
                let amount = ((Self::next_rand(&mut rng) % 25000) + 500) as f64 / 100.0;

                let mut fields = HashMap::new();
                fields.insert("id".to_string(), serde_json::Value::String(ord_id.clone()));
                fields.insert("customer_id".to_string(), serde_json::Value::String(cust.id.clone()));
                fields.insert("total_amount".to_string(), serde_json::json!(amount));
                fields.insert("currency".to_string(), serde_json::Value::String("USD".to_string()));
                fields.insert("status".to_string(), serde_json::Value::String("paid".to_string()));
                fields.insert("order_seq".to_string(), serde_json::json!(j));

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
        assert!(batch.json_export.contains("usr_"));

        // Check edge cases: international names / emails
        assert!(batch.records.iter().any(|r| {
            let email = r.fields.get("email").and_then(|e| e.as_str()).unwrap_or("");
            email.contains('@')
        }));
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
