use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BudgetTier {
    FlagshipTier, // Full Opus / GPT-5 / Gemini 2.5 Pro
    BalancedTier, // Sonnet / Gemini 2.5 Flash
    EcoTier,      // Haiku / Local Ollama (Zero Cost)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetCallRecord {
    pub timestamp_epoch_ms: u64,
    pub model_used: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatusReport {
    pub budget_limit_usd: f64,
    pub total_spent_usd: f64,
    pub remaining_usd: f64,
    pub percent_consumed: f32,
    pub current_tier: BudgetTier,
    pub is_exhausted: bool,
    pub total_calls: usize,
    pub advisory_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetGovernorState {
    pub budget_limit_usd: f64,
    pub total_spent_usd: f64,
    pub calls: Vec<BudgetCallRecord>,
}

pub struct SessionBudgetEnvelope {
    state: Mutex<BudgetGovernorState>,
}

static GLOBAL_BUDGET: OnceLock<Arc<SessionBudgetEnvelope>> = OnceLock::new();

impl SessionBudgetEnvelope {
    pub fn global() -> Arc<Self> {
        GLOBAL_BUDGET
            .get_or_init(|| Arc::new(SessionBudgetEnvelope::new(5.00)))
            .clone()
    }

    pub fn new(budget_limit_usd: f64) -> Self {
        Self {
            state: Mutex::new(BudgetGovernorState {
                budget_limit_usd,
                total_spent_usd: 0.0,
                calls: Vec::new(),
            }),
        }
    }

    pub fn set_limit(&self, limit_usd: f64) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.budget_limit_usd = limit_usd;
    }

    pub fn record_call(&self, model: &str, prompt_tokens: usize, completion_tokens: usize) -> Result<BudgetStatusReport, String> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());

        if guard.total_spent_usd >= guard.budget_limit_usd {
            return Err(format!(
                "BUDGET EXHAUSTED: Session spending limit of ${:.2} reached (current: ${:.2}). Expand budget via 'hgb budget --set <amount>' or switch to local Ollama.",
                guard.budget_limit_usd, guard.total_spent_usd
            ));
        }

        let cost_per_1k_input = if model.contains("pro") || model.contains("opus") { 0.003 } else { 0.0003 };
        let cost_per_1k_output = if model.contains("pro") || model.contains("opus") { 0.015 } else { 0.0015 };

        let cost = ((prompt_tokens as f64 / 1000.0) * cost_per_1k_input)
            + ((completion_tokens as f64 / 1000.0) * cost_per_1k_output);

        guard.total_spent_usd += cost;
        guard.calls.push(BudgetCallRecord {
            timestamp_epoch_ms: chrono::Utc::now().timestamp_millis() as u64,
            model_used: model.to_string(),
            prompt_tokens,
            completion_tokens,
            cost_usd: cost,
        });

        let total = guard.total_spent_usd;
        let limit = guard.budget_limit_usd;
        let remaining = (limit - total).max(0.0);
        let pct = if limit > 0.0 { ((total / limit) * 100.0) as f32 } else { 100.0 };

        let (tier, advisory) = if pct >= 100.0 {
            (BudgetTier::EcoTier, "Hard limit reached: All subsequent requests must route through zero-cost local Ollama.".to_string())
        } else if pct >= 80.0 {
            (BudgetTier::BalancedTier, format!("Warning: {:.1}% of budget consumed. Degraded model routing to Balanced Tier (Flash/Sonnet).", pct))
        } else {
            (BudgetTier::FlagshipTier, format!("Budget healthy: ${:.2} remaining ({:.1}% used).", remaining, pct))
        };

        Ok(BudgetStatusReport {
            budget_limit_usd: limit,
            total_spent_usd: total,
            remaining_usd: remaining,
            percent_consumed: pct,
            current_tier: tier,
            is_exhausted: pct >= 100.0,
            total_calls: guard.calls.len(),
            advisory_message: advisory,
        })
    }

    pub fn status(&self) -> BudgetStatusReport {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let total = guard.total_spent_usd;
        let limit = guard.budget_limit_usd;
        let remaining = (limit - total).max(0.0);
        let pct = if limit > 0.0 { ((total / limit) * 100.0) as f32 } else { 100.0 };

        let (tier, advisory) = if pct >= 100.0 {
            (BudgetTier::EcoTier, "Hard limit reached. Expand budget to continue cloud API calls.".to_string())
        } else if pct >= 80.0 {
            (BudgetTier::BalancedTier, format!("Warning: {:.1}% of budget consumed. Operating in Balanced Tier.", pct))
        } else {
            (BudgetTier::FlagshipTier, format!("Budget healthy: ${:.2} remaining.", remaining))
        };

        BudgetStatusReport {
            budget_limit_usd: limit,
            total_spent_usd: total,
            remaining_usd: remaining,
            percent_consumed: pct,
            current_tier: tier,
            is_exhausted: pct >= 100.0,
            total_calls: guard.calls.len(),
            advisory_message: advisory,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_envelope_tracking_and_exhaustion() {
        let env = SessionBudgetEnvelope::new(0.01);
        let s1 = env.record_call("gemini-2.5-flash", 1000, 500).unwrap();
        assert!(s1.total_spent_usd > 0.0);
        assert!(!s1.is_exhausted);

        let _ = env.record_call("opus-4", 10000, 5000);
        let status = env.status();
        assert!(status.is_exhausted);
        assert_eq!(status.current_tier, BudgetTier::EcoTier);

        let rejected = env.record_call("opus-4", 1000, 1000);
        assert!(rejected.is_err());
    }
}
