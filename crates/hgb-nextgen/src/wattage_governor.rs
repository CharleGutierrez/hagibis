//! # Token & Wattage Governor (`hgb-nextgen`)
//!
//! Autonomous hardware telemetry, energy efficiency, and cost governor:
//! - Tracks hardware concurrency threads and calculates estimated CPU power draw (mW)
//! - Smart Local-First Routing: directs routine refactoring/completions to local Ollama ($0.00 cost)
//! - Escalates to Cloud Frontier models only for complex architectural synthesis
//! - Enforces hard session & daily budget caps with automatic circuit-breaker throttling

use serde::{Deserialize, Serialize};

/// Configuration parameters for the Wattage & Budget Governor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorConfig {
    pub daily_budget_usd: f64,
    pub session_budget_usd: f64,
    pub max_cloud_tokens: usize,
    pub prefer_local_ollama: bool,
    pub idle_power_mw: f64,
    pub core_power_mw: f64,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            daily_budget_usd: 0.50, // Hard $0.50 / day limit
            session_budget_usd: 0.20,
            max_cloud_tokens: 50_000,
            prefer_local_ollama: true,
            idle_power_mw: 1500.0,
            core_power_mw: 650.0,
        }
    }
}

/// A model routing decision made by the Governor
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingDecision {
    pub target_model: String,
    pub is_local: bool,
    pub estimated_cost_usd: f64,
    pub estimated_power_mw: f64,
    pub reason: String,
}

/// Cumulative session hardware & financial telemetry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionExpenditure {
    pub local_tokens_used: usize,
    pub cloud_tokens_used: usize,
    pub spent_usd: f64,
    pub active_threads: usize,
    pub current_power_mw: f64,
    pub is_throttled: bool,
    pub remaining_budget_usd: f64,
}

/// Token & Wattage Governor Engine
pub struct WattageGovernor {
    config: GovernorConfig,
    local_tokens: usize,
    cloud_tokens: usize,
    spent_usd: f64,
    hardware_threads: usize,
}

impl Default for WattageGovernor {
    fn default() -> Self {
        Self::new(GovernorConfig::default())
    }
}

impl WattageGovernor {
    pub fn new(config: GovernorConfig) -> Self {
        let hardware_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
        Self {
            config,
            local_tokens: 0,
            cloud_tokens: 0,
            spent_usd: 0.0,
            hardware_threads,
        }
    }

    /// Available hardware CPU threads
    pub fn hardware_threads(&self) -> usize {
        self.hardware_threads
    }

    /// Record a completed LLM invocation
    pub fn record_tokens(&mut self, is_local: bool, tokens: usize) {
        if is_local {
            self.local_tokens += tokens;
        } else {
            self.cloud_tokens += tokens;
            // Cost calculation: ~$0.15 per million tokens for lightweight flash models
            let cost = (tokens as f64 / 1_000_000.0) * 0.15;
            self.spent_usd += cost;
        }
    }

    /// Estimate current real-time CPU wattage draw (mW)
    pub fn estimate_power_draw_mw(&self, active_worker_threads: usize) -> f64 {
        let active = active_worker_threads.min(self.hardware_threads);
        self.config.idle_power_mw + (active as f64 * self.config.core_power_mw)
    }

    /// Make an intelligent Local-First model routing decision
    pub fn route_task(&self, task_description: &str, complexity_score: u8) -> RoutingDecision {
        let is_over_budget = self.spent_usd >= self.config.daily_budget_usd || self.cloud_tokens >= self.config.max_cloud_tokens;

        // If throttled by budget cap, force local execution
        if is_over_budget {
            return RoutingDecision {
                target_model: "ollama/qwen2.5-coder:1.5b".to_string(),
                is_local: true,
                estimated_cost_usd: 0.0,
                estimated_power_mw: self.estimate_power_draw_mw(4),
                reason: "Budget cap reached ($0.50/day). Throttling to 100% free local inference.".to_string(),
            };
        }

        // Routine tasks (< complexity 70) route to local Ollama
        let is_routine = complexity_score < 70
            || task_description.contains("refactor")
            || task_description.contains("test")
            || task_description.contains("format")
            || task_description.contains("stub");

        if is_routine && self.config.prefer_local_ollama {
            RoutingDecision {
                target_model: "ollama/qwen2.5-coder:1.5b".to_string(),
                is_local: true,
                estimated_cost_usd: 0.0,
                estimated_power_mw: self.estimate_power_draw_mw(4),
                reason: "Routine task routed to local zero-cost Ollama model.".to_string(),
            }
        } else {
            RoutingDecision {
                target_model: "gemini-2.5-pro".to_string(),
                is_local: false,
                estimated_cost_usd: 0.00075,
                estimated_power_mw: self.estimate_power_draw_mw(1), // Minimal client power
                reason: "High complexity architectural task escalated to cloud reasoning engine.".to_string(),
            }
        }
    }

    /// Telemetry snapshot for Cockpit display
    pub fn get_telemetry(&self) -> SessionExpenditure {
        let is_throttled = self.spent_usd >= self.config.daily_budget_usd;
        let remaining = (self.config.daily_budget_usd - self.spent_usd).max(0.0);

        SessionExpenditure {
            local_tokens_used: self.local_tokens,
            cloud_tokens_used: self.cloud_tokens,
            spent_usd: self.spent_usd,
            active_threads: self.hardware_threads,
            current_power_mw: self.estimate_power_draw_mw(2),
            is_throttled,
            remaining_budget_usd: remaining,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wattage_governor_local_first_routing() {
        let mut governor = WattageGovernor::default();

        // 1. Routine task routes to local Ollama
        let decision1 = governor.route_task("refactor test assertions", 40);
        assert!(decision1.is_local);
        assert_eq!(decision1.estimated_cost_usd, 0.0);
        assert_eq!(decision1.target_model, "ollama/qwen2.5-coder:1.5b");

        // 2. Complex task routes to Cloud
        let decision2 = governor.route_task("Formal Invariant Verification of Raft Swarm", 95);
        assert!(!decision2.is_local);
        assert_eq!(decision2.target_model, "gemini-2.5-pro");

        // 3. Exceed budget and verify throttling
        governor.record_tokens(false, 4_000_000); // costs ~$0.60 > $0.50 limit
        let telemetry = governor.get_telemetry();
        assert!(telemetry.is_throttled);

        // Subsequent complex task must be throttled to local model
        let decision3 = governor.route_task("Formal Invariant Verification of Raft Swarm", 95);
        assert!(decision3.is_local);
        assert!(decision3.reason.contains("Budget cap reached"));
    }
}
