//! # Smart FinOps & Token Budget Envelope (`finops_budget`)
//!
//! Real financial and token accounting ledger persisted at `.hgb/budget.json`.
//! Model pricing tables, session/daily USD budget caps, and dynamic routing
//! arbiter that automatically routes background tasks to local Ollama when budget
//! thresholds are reached.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

/// Pricing rates per 1,000,000 tokens (USD)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub prompt_price_per_1m: f64,
    pub completion_price_per_1m: f64,
}

impl ModelPricing {
    pub const fn new(prompt_price_per_1m: f64, completion_price_per_1m: f64) -> Self {
        Self {
            prompt_price_per_1m,
            completion_price_per_1m,
        }
    }

    /// Calculate exact cost for given input prompt and output completion token counts
    pub fn calculate_cost(&self, prompt_tokens: usize, completion_tokens: usize) -> f64 {
        let prompt_cost = (prompt_tokens as f64) * (self.prompt_price_per_1m / 1_000_000.0);
        let completion_cost = (completion_tokens as f64) * (self.completion_price_per_1m / 1_000_000.0);
        prompt_cost + completion_cost
    }
}

/// Catalog of model pricing
pub struct ModelPricingCatalog;

impl ModelPricingCatalog {
    pub fn lookup(model_name: &str) -> ModelPricing {
        let lower = model_name.to_lowercase();
        if lower.contains("claude-3-5-sonnet") || lower.contains("claude-3.5-sonnet") {
            ModelPricing::new(3.00, 15.00)
        } else if lower.contains("gpt-4o") {
            ModelPricing::new(2.50, 10.00)
        } else if lower.contains("groq") || lower.contains("llama-3.3-70b") {
            ModelPricing::new(0.59, 0.79)
        } else if lower.contains("gemini-2.5-flash") || lower.contains("gemini-flash") {
            ModelPricing::new(0.075, 0.30)
        } else if lower.contains("gemini-2.5-pro") || lower.contains("gemini-pro") {
            ModelPricing::new(1.25, 5.00)
        } else if lower.contains("nvidia") || lower.contains("deepseek") {
            ModelPricing::new(0.20, 0.40)
        } else if lower.contains("ollama") || lower.contains("local") || lower.contains("qwen2.5-coder") {
            // Local offline model: Zero dollar API cost
            ModelPricing::new(0.0, 0.0)
        } else {
            // Default baseline estimation
            ModelPricing::new(1.00, 3.00)
        }
    }
}

/// Usage Record Entry in Budget Ledger
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cost_usd: f64,
    pub task_category: String, // "core", "test_generation", "linting", "docs", "background"
}

/// Daily Budget Envelope Tracker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetEnvelope {
    pub daily_cap_usd: f64,
    pub monthly_cap_usd: f64,
    pub total_spend_usd: f64,
    pub today_spend_usd: f64,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub last_reset_day: u32,
    pub records: Vec<UsageRecord>,
}

impl Default for BudgetEnvelope {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            daily_cap_usd: 5.00,
            monthly_cap_usd: 50.00,
            total_spend_usd: 0.0,
            today_spend_usd: 0.0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            last_reset_day: now.day(),
            records: Vec::new(),
        }
    }
}

impl BudgetEnvelope {
    pub fn new(daily_cap_usd: f64, monthly_cap_usd: f64) -> Self {
        let mut env = Self::default();
        env.daily_cap_usd = daily_cap_usd;
        env.monthly_cap_usd = monthly_cap_usd;
        env
    }

    /// Refresh day cycle if UTC date rolled over
    pub fn maybe_reset_day(&mut self) {
        let current_day = Utc::now().day();
        if self.last_reset_day != current_day {
            self.today_spend_usd = 0.0;
            self.last_reset_day = current_day;
        }
    }

    /// Utilization percentage of daily cap [0.0 - 100.0+]
    pub fn daily_utilization_pct(&self) -> f64 {
        if self.daily_cap_usd <= 0.0 {
            return 100.0;
        }
        (self.today_spend_usd / self.daily_cap_usd) * 100.0
    }

    /// Remaining daily budget in USD
    pub fn remaining_daily_usd(&self) -> f64 {
        (self.daily_cap_usd - self.today_spend_usd).max(0.0)
    }

    /// Record a call and update totals
    pub fn record_usage(
        &mut self,
        model: &str,
        prompt_tokens: usize,
        completion_tokens: usize,
        task_category: &str,
    ) -> f64 {
        self.maybe_reset_day();

        let pricing = ModelPricingCatalog::lookup(model);
        let cost = pricing.calculate_cost(prompt_tokens, completion_tokens);

        self.today_spend_usd += cost;
        self.total_spend_usd += cost;
        self.total_prompt_tokens += prompt_tokens;
        self.total_completion_tokens += completion_tokens;

        self.records.push(UsageRecord {
            timestamp: Utc::now(),
            model: model.to_string(),
            prompt_tokens,
            completion_tokens,
            cost_usd: cost,
            task_category: task_category.to_string(),
        });

        cost
    }
}

/// Routing Arbiter Decision
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RoutingDecision {
    /// Normal routing to requested model
    Proceed {
        model: String,
        estimated_cost: f64,
        utilization_pct: f64,
    },
    /// Route to requested model with threshold warning (utilization >= 75%)
    WarnAndProceed {
        model: String,
        estimated_cost: f64,
        utilization_pct: f64,
        warning: String,
    },
    /// Cap reached: Reroute background/secondary tasks to Local Ollama ($0.00 cost)
    RerouteToLocalOllama {
        requested_model: String,
        fallback_model: String,
        reason: String,
    },
}

/// Smart FinOps Engine managing ledger persistence and dynamic routing
pub struct FinOpsEngine {
    pub ledger_path: PathBuf,
}

impl FinOpsEngine {
    pub fn new(workspace_root: &Path) -> Self {
        let ledger_path = workspace_root.join(".hgb").join("budget.json");
        Self { ledger_path }
    }

    pub fn with_custom_path(ledger_path: PathBuf) -> Self {
        Self { ledger_path }
    }

    /// Load or initialize budget envelope from disk
    pub fn load_envelope(&self) -> BudgetEnvelope {
        if self.ledger_path.exists() {
            if let Ok(content) = fs::read_to_string(&self.ledger_path) {
                if let Ok(mut env) = serde_json::from_str::<BudgetEnvelope>(&content) {
                    env.maybe_reset_day();
                    return env;
                }
            }
        }
        BudgetEnvelope::default()
    }

    /// Save envelope to disk
    pub fn save_envelope(&self, envelope: &BudgetEnvelope) -> Result<(), String> {
        if let Some(parent) = self.ledger_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let json = serde_json::to_string_pretty(envelope)
            .map_err(|e| format!("Serialization error: {}", e))?;

        fs::write(&self.ledger_path, json)
            .map_err(|e| format!("Failed to write budget ledger: {}", e))
    }

    /// Dynamic routing arbiter evaluating task category and current budget utilization
    pub fn arbitrate_request(
        &self,
        envelope: &BudgetEnvelope,
        requested_model: &str,
        estimated_prompt_tokens: usize,
        task_category: &str,
    ) -> RoutingDecision {
        let pricing = ModelPricingCatalog::lookup(requested_model);
        let estimated_cost = pricing.calculate_cost(estimated_prompt_tokens, 256);
        let utilization = envelope.daily_utilization_pct();

        let is_background_task = matches!(
            task_category,
            "test_generation" | "linting" | "docs" | "background" | "indexer"
        );

        // 1. Budget Cap Exceeded (>= 100%)
        if utilization >= 100.0 {
            if is_background_task {
                return RoutingDecision::RerouteToLocalOllama {
                    requested_model: requested_model.to_string(),
                    fallback_model: "qwen2.5-coder:7b".to_string(),
                    reason: format!(
                        "Daily budget cap of ${:.2} reached ({:.1}% utilization). Automatically rerouted background task '{}' to Local Ollama ($0.00).",
                        envelope.daily_cap_usd, utilization, task_category
                    ),
                };
            } else {
                return RoutingDecision::WarnAndProceed {
                    model: requested_model.to_string(),
                    estimated_cost,
                    utilization_pct: utilization,
                    warning: format!(
                        "CRITICAL: Daily budget limit (${:.2}) exceeded! User core prompt permitted with spend tracking.",
                        envelope.daily_cap_usd
                    ),
                };
            }
        }

        // 2. Warning Threshold (75% - 99.9%)
        if utilization >= 75.0 {
            return RoutingDecision::WarnAndProceed {
                model: requested_model.to_string(),
                estimated_cost,
                utilization_pct: utilization,
                warning: format!(
                    "Budget Alert: Daily envelope at {:.1}% (${:.2} / ${:.2}). Consider local Ollama fallback for background tasks.",
                    utilization, envelope.today_spend_usd, envelope.daily_cap_usd
                ),
            };
        }

        // 3. Normal Routing (< 75%)
        RoutingDecision::Proceed {
            model: requested_model.to_string(),
            estimated_cost,
            utilization_pct: utilization,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_calculations() {
        let sonnet = ModelPricingCatalog::lookup("claude-3-5-sonnet");
        // 1,000,000 prompt tokens = $3.00, 1,000,000 completion = $15.00
        let cost = sonnet.calculate_cost(100_000, 20_000);
        // (100k / 1M) * 3 = 0.30; (20k / 1M) * 15 = 0.30; total = 0.60
        assert!((cost - 0.60).abs() < 0.0001);

        let ollama = ModelPricingCatalog::lookup("qwen2.5-coder:7b");
        assert_eq!(ollama.calculate_cost(500_000, 500_000), 0.0);
    }

    #[test]
    fn test_envelope_tracking_and_utilization() {
        let mut env = BudgetEnvelope::new(10.00, 100.00);
        assert_eq!(env.daily_utilization_pct(), 0.0);
        assert_eq!(env.remaining_daily_usd(), 10.00);

        // Record $2.50 spend
        env.record_usage("gpt-4o", 500_000, 125_000, "core");
        assert!((env.today_spend_usd - 2.50).abs() < 0.01);
        assert!((env.daily_utilization_pct() - 25.0).abs() < 0.1);
        assert!((env.remaining_daily_usd() - 7.50).abs() < 0.01);
    }

    #[test]
    fn test_dynamic_routing_arbiter() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_budget_test_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        let engine = FinOpsEngine::with_custom_path(temp_dir.join("budget.json"));

        let mut env = BudgetEnvelope::new(4.00, 40.00);

        // Phase 1: Under 75% -> Proceed
        env.today_spend_usd = 2.00; // 50%
        let dec1 = engine.arbitrate_request(&env, "claude-3-5-sonnet", 1000, "core");
        assert!(matches!(dec1, RoutingDecision::Proceed { .. }));

        // Phase 2: >= 75% -> WarnAndProceed
        env.today_spend_usd = 3.20; // 80%
        let dec2 = engine.arbitrate_request(&env, "claude-3-5-sonnet", 1000, "core");
        assert!(matches!(dec2, RoutingDecision::WarnAndProceed { .. }));

        // Phase 3: >= 100% Background Task -> RerouteToLocalOllama
        env.today_spend_usd = 4.50; // 112.5%
        let dec3 = engine.arbitrate_request(&env, "claude-3-5-sonnet", 1000, "test_generation");
        match dec3 {
            RoutingDecision::RerouteToLocalOllama { requested_model, fallback_model, .. } => {
                assert_eq!(requested_model, "claude-3-5-sonnet");
                assert_eq!(fallback_model, "qwen2.5-coder:7b");
            }
            _ => panic!("Expected RerouteToLocalOllama for background task when cap exceeded"),
        }

        // Save & load test
        engine.save_envelope(&env).expect("Save failed");
        let loaded = engine.load_envelope();
        assert_eq!(loaded.daily_cap_usd, 4.00);
        assert!((loaded.today_spend_usd - 4.50).abs() < 0.001);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
