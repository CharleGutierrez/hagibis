//! # Relational Time-Warp Data Synthesizer
//!
//! Synthesizes complex relational test data across interdependent tables with strict foreign key integrity,
//! realistic temporal progression, and simulated clock-skew events (timezone jumps, leap seconds, expired tokens).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeWarpConfig {
    pub seed: u64,
    pub months: u32,
    pub base_timestamp: u64,
    pub include_skew: bool,
    pub record_scale: usize,
}

impl Default for TimeWarpConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            months: 6,
            base_timestamp: 1740000000, // Approximate baseline epoch
            include_skew: true,
            record_scale: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticEntity {
    pub table: String,
    pub id: String,
    pub parent_fk: Option<String>,
    pub created_at_iso: String,
    pub attributes: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSummary {
    pub table_name: String,
    pub count: usize,
    pub fk_column: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeWarpReport {
    pub seed: u64,
    pub timespan_months: u32,
    pub total_records: usize,
    pub table_summaries: Vec<TableSummary>,
    pub fk_integrity_verified: bool,
    pub clock_skew_events_simulated: usize,
    pub sql_fixture_preview: String,
    pub json_fixture_bytes: usize,
    pub temporal_range: (String, String),
}

pub struct TimeWarpDataEngine;

impl TimeWarpDataEngine {
    pub fn new() -> Self {
        Self
    }

    /// Generates multi-table relational data with strict FK dependencies and temporal progression
    pub fn synthesize_dataset(&self, config: &TimeWarpConfig) -> TimeWarpReport {
        let mut rng = config.seed;
        let mut pseudo_rand = || -> u64 {
            // Xorshift64
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        let seconds_per_month = 30 * 86400u64;
        let total_seconds = config.months as u64 * seconds_per_month;
        let start_ts = config.base_timestamp.saturating_sub(total_seconds);

        let mut organizations = Vec::new();
        let mut users = Vec::new();
        let mut invoices = Vec::new();
        let mut audit_events = Vec::new();

        let num_orgs = (config.record_scale.max(1)).min(50);
        let num_users_per_org = 3;
        let num_invoices_per_org = 4;

        // 1. Generate Organizations
        for o_idx in 0..num_orgs {
            let org_id = format!("org_{:04x}", (pseudo_rand() % 0xFFFF));
            let offset_sec = (pseudo_rand() % (total_seconds / 4).max(1)) + 10;
            let org_ts = start_ts + offset_sec;

            let mut attrs = HashMap::new();
            attrs.insert("name".to_string(), serde_json::json!(format!("Nexus Corp {}", o_idx + 1)));
            attrs.insert("tier".to_string(), serde_json::json!(if o_idx % 2 == 0 { "enterprise" } else { "pro" }));

            organizations.push(SyntheticEntity {
                table: "organizations".to_string(),
                id: org_id,
                parent_fk: None,
                created_at_iso: format_epoch_iso(org_ts),
                attributes: attrs,
            });
        }

        // 2. Generate Users (belong to orgs)
        for org in &organizations {
            for u_idx in 0..num_users_per_org {
                let user_id = format!("usr_{:04x}", (pseudo_rand() % 0xFFFF));
                let user_ts = start_ts + (pseudo_rand() % (total_seconds / 2).max(1)) + 500;

                let mut attrs = HashMap::new();
                attrs.insert("org_id".to_string(), serde_json::json!(org.id));
                attrs.insert("email".to_string(), serde_json::json!(format!("dev_{}@{}", u_idx, org.id)));
                attrs.insert("role".to_string(), serde_json::json!(if u_idx == 0 { "admin" } else { "member" }));

                users.push(SyntheticEntity {
                    table: "users".to_string(),
                    id: user_id,
                    parent_fk: Some(org.id.clone()),
                    created_at_iso: format_epoch_iso(user_ts),
                    attributes: attrs,
                });
            }
        }

        // 3. Generate Invoices (belong to orgs)
        for org in &organizations {
            for inv_idx in 0..num_invoices_per_org {
                let invoice_id = format!("inv_{:04x}", (pseudo_rand() % 0xFFFF));
                let inv_ts = start_ts + (pseudo_rand() % total_seconds.max(1));

                let mut attrs = HashMap::new();
                attrs.insert("org_id".to_string(), serde_json::json!(org.id));
                let amount = (pseudo_rand() % 50000 + 1000) as f64 / 100.0;
                attrs.insert("amount_usd".to_string(), serde_json::json!(amount));
                attrs.insert("status".to_string(), serde_json::json!(if inv_idx % 3 == 0 { "paid" } else { "pending" }));

                invoices.push(SyntheticEntity {
                    table: "invoices".to_string(),
                    id: invoice_id,
                    parent_fk: Some(org.id.clone()),
                    created_at_iso: format_epoch_iso(inv_ts),
                    attributes: attrs,
                });
            }
        }

        // 4. Generate Audit Events (linked to users and orgs)
        let mut clock_skew_events = 0;
        for user in &users {
            let audit_id = format!("evt_{:04x}", (pseudo_rand() % 0xFFFF));
            let mut evt_ts = start_ts + (pseudo_rand() % total_seconds.max(1));

            // Inject intentional clock-skew test events if requested (e.g. leap second, leap backward)
            let mut skew_note = None;
            if config.include_skew && (pseudo_rand() % 5 == 0) {
                clock_skew_events += 1;
                evt_ts = evt_ts.saturating_sub(3600); // 1 hr leap back / DST transition
                skew_note = Some("DST_FALLBACK_ANOMALY");
            }

            let mut attrs = HashMap::new();
            attrs.insert("user_id".to_string(), serde_json::json!(user.id));
            if let Some(org_id) = &user.parent_fk {
                attrs.insert("org_id".to_string(), serde_json::json!(org_id));
            }
            attrs.insert("action".to_string(), serde_json::json!("API_MUTATION"));
            if let Some(note) = skew_note {
                attrs.insert("clock_skew_marker".to_string(), serde_json::json!(note));
            }

            audit_events.push(SyntheticEntity {
                table: "audit_events".to_string(),
                id: audit_id,
                parent_fk: Some(user.id.clone()),
                created_at_iso: format_epoch_iso(evt_ts),
                attributes: attrs,
            });
        }

        // Verify FK Integrity
        let org_ids: std::collections::HashSet<_> = organizations.iter().map(|o| &o.id).collect();
        let user_ids: std::collections::HashSet<_> = users.iter().map(|u| &u.id).collect();

        let mut fk_intact = true;
        for u in &users {
            if let Some(fk) = &u.parent_fk {
                if !org_ids.contains(fk) {
                    fk_intact = false;
                }
            }
        }
        for inv in &invoices {
            if let Some(fk) = &inv.parent_fk {
                if !org_ids.contains(fk) {
                    fk_intact = false;
                }
            }
        }
        for evt in &audit_events {
            if let Some(fk) = &evt.parent_fk {
                if !user_ids.contains(fk) {
                    fk_intact = false;
                }
            }
        }

        let total_records = organizations.len() + users.len() + invoices.len() + audit_events.len();

        let table_summaries = vec![
            TableSummary { table_name: "organizations".to_string(), count: organizations.len(), fk_column: None },
            TableSummary { table_name: "users".to_string(), count: users.len(), fk_column: Some("org_id -> organizations.id".to_string()) },
            TableSummary { table_name: "invoices".to_string(), count: invoices.len(), fk_column: Some("org_id -> organizations.id".to_string()) },
            TableSummary { table_name: "audit_events".to_string(), count: audit_events.len(), fk_column: Some("user_id -> users.id".to_string()) },
        ];

        // Format SQL fixture preview
        let mut sql = String::new();
        sql.push_str("-- Auto-generated Time-Warp Fixture by Hagibis (hgb)\n");
        sql.push_str("BEGIN TRANSACTION;\n");
        if let Some(first_org) = organizations.first() {
            sql.push_str(&format!(
                "INSERT INTO organizations (id, name, created_at) VALUES ('{}', '{}', '{}');\n",
                first_org.id,
                first_org.attributes.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                first_org.created_at_iso
            ));
        }
        if let Some(first_usr) = users.first() {
            sql.push_str(&format!(
                "INSERT INTO users (id, org_id, email, created_at) VALUES ('{}', '{}', '{}', '{}');\n",
                first_usr.id,
                first_usr.parent_fk.as_deref().unwrap_or(""),
                first_usr.attributes.get("email").and_then(|v| v.as_str()).unwrap_or(""),
                first_usr.created_at_iso
            ));
        }
        if let Some(first_inv) = invoices.first() {
            sql.push_str(&format!(
                "INSERT INTO invoices (id, org_id, amount_usd, status, created_at) VALUES ('{}', '{}', {}, '{}', '{}');\n",
                first_inv.id,
                first_inv.parent_fk.as_deref().unwrap_or(""),
                first_inv.attributes.get("amount_usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
                first_inv.attributes.get("status").and_then(|v| v.as_str()).unwrap_or("pending"),
                first_inv.created_at_iso
            ));
        }
        sql.push_str("-- ... [additional records omitted for preview]\nCOMMIT;\n");

        let json_data = serde_json::json!({
            "organizations": organizations,
            "users": users,
            "invoices": invoices,
            "audit_events": audit_events,
        });
        let json_bytes = serde_json::to_vec(&json_data).unwrap_or_default().len();

        let min_iso = format_epoch_iso(start_ts);
        let max_iso = format_epoch_iso(config.base_timestamp);

        TimeWarpReport {
            seed: config.seed,
            timespan_months: config.months,
            total_records,
            table_summaries,
            fk_integrity_verified: fk_intact,
            clock_skew_events_simulated: clock_skew_events,
            sql_fixture_preview: sql,
            json_fixture_bytes: json_bytes,
            temporal_range: (min_iso, max_iso),
        }
    }
}

fn format_epoch_iso(epoch_secs: u64) -> String {
    // Simple reproducible timestamp formatter
    let days = epoch_secs / 86400;
    let rem_secs = epoch_secs % 86400;
    let hours = rem_secs / 3600;
    let minutes = (rem_secs % 3600) / 60;
    let seconds = rem_secs % 60;

    // Approximate calendar conversion from year 1970
    let year = 1970 + days / 365;
    let day_of_year = (days % 365) + 1;
    let month = (day_of_year / 30).min(11) + 1;
    let day = (day_of_year % 30).max(1);

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month, day, hours, minutes, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_warp_data_synthesis_and_fk_integrity() {
        let engine = TimeWarpDataEngine::new();
        let config = TimeWarpConfig {
            seed: 1337,
            months: 6,
            base_timestamp: 1740000000,
            include_skew: true,
            record_scale: 5,
        };

        let report = engine.synthesize_dataset(&config);
        assert_eq!(report.seed, 1337);
        assert_eq!(report.timespan_months, 6);
        assert!(report.total_records > 0);
        assert!(report.fk_integrity_verified);
        assert!(report.json_fixture_bytes > 0);
        assert!(report.sql_fixture_preview.contains("BEGIN TRANSACTION;"));
    }
}
