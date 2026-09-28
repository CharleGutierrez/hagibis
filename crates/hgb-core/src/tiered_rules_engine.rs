use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleTier {
    AlwaysOn,
    AutoAttached,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredRule {
    pub name: String,
    pub tier: RuleTier,
    pub glob_patterns: Vec<String>,
    pub priority: u32,
    pub content: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatedRuleset {
    pub matched_rules: Vec<TieredRule>,
    pub total_rules_evaluated: usize,
    pub injected_prompt_context: String,
    pub triggered_globs: Vec<String>,
}

pub struct TieredRulesEngine {
    rules: HashMap<String, TieredRule>,
}

impl TieredRulesEngine {
    pub fn new() -> Self {
        let mut engine = Self {
            rules: HashMap::new(),
        };
        engine.load_builtin_defaults();
        engine
    }

    fn load_builtin_defaults(&mut self) {
        // Builtin Tier 1: AlwaysOn Safety
        self.register_rule(TieredRule {
            name: "core_safety_invariants".to_string(),
            tier: RuleTier::AlwaysOn,
            glob_patterns: vec!["*".to_string()],
            priority: 100,
            content: "Never expose plaintext production secrets. All fallible operations must return Result without panicking. Never drop database tables without explicit shadow verification.".to_string(),
            tags: vec!["safety".to_string(), "invariants".to_string()],
        });

        // Builtin Tier 2: AutoAttached Rust
        self.register_rule(TieredRule {
            name: "rust_microkernel_standards".to_string(),
            tier: RuleTier::AutoAttached,
            glob_patterns: vec!["*.rs".to_string(), "Cargo.toml".to_string(), "crates/**".to_string()],
            priority: 80,
            content: "Use idiomatic Rust 2021 edition. Prefer zero-allocation references (&str, &[u8]). Implement serde Serialize/Deserialize for IPC types. Enforce bounds checking and saturating arithmetic.".to_string(),
            tags: vec!["rust".to_string()],
        });

        // Builtin Tier 2: AutoAttached Rails
        self.register_rule(TieredRule {
            name: "rails_zero_downtime_rules".to_string(),
            tier: RuleTier::AutoAttached,
            glob_patterns: vec!["*.rb".to_string(), "Gemfile".to_string(), "config/routes.rb".to_string(), "db/migrate/**".to_string()],
            priority: 80,
            content: "Enforce zero-downtime migrations: add_index must specify algorithm: :concurrently on PostgreSQL with disable_ddl_transaction!. Preload relations to prevent N+1 queries. Use strong parameters.".to_string(),
            tags: vec!["rails".to_string(), "ruby".to_string()],
        });

        // Builtin Tier 2: AutoAttached React/Frontend
        self.register_rule(TieredRule {
            name: "frontend_vibe_rules".to_string(),
            tier: RuleTier::AutoAttached,
            glob_patterns: vec!["*.tsx".to_string(), "*.jsx".to_string(), "*.ts".to_string(), "*.vue".to_string()],
            priority: 70,
            content: "Use modern Tailwind CSS utility classes. Maintain responsive mobile-first views with iOS safe-area insets. Prefer immutable state updates.".to_string(),
            tags: vec!["frontend".to_string(), "react".to_string()],
        });

        // Builtin Tier 3: Manual Rule
        self.register_rule(TieredRule {
            name: "brutal_perf_audit".to_string(),
            tier: RuleTier::Manual,
            glob_patterns: Vec::new(),
            priority: 90,
            content: "Conduct brutal memory allocation and microsecond latency audit. Minimize heap allocations and thread lock contention.".to_string(),
            tags: vec!["performance".to_string()],
        });
    }

    pub fn register_rule(&mut self, rule: TieredRule) {
        self.rules.insert(rule.name.clone(), rule);
    }

    /// Evaluates which rules apply given a list of active modified file paths and optional manual summon tags
    pub fn evaluate(&self, active_files: &[&str], manual_mentions: &[&str]) -> EvaluatedRuleset {
        let mut matched: Vec<TieredRule> = Vec::new();
        let mut triggered_globs = Vec::new();

        for rule in self.rules.values() {
            match rule.tier {
                RuleTier::AlwaysOn => {
                    matched.push(rule.clone());
                }
                RuleTier::AutoAttached => {
                    let mut hits = false;
                    for pattern in &rule.glob_patterns {
                        for file in active_files {
                            if matches_glob(pattern, file) {
                                hits = true;
                                triggered_globs.push(format!("{} => {}", file, pattern));
                                break;
                            }
                        }
                        if hits {
                            break;
                        }
                    }
                    if hits {
                        matched.push(rule.clone());
                    }
                }
                RuleTier::Manual => {
                    if manual_mentions.iter().any(|m| *m == rule.name || rule.tags.contains(&m.to_string())) {
                        matched.push(rule.clone());
                    }
                }
            }
        }

        // Sort by priority descending
        matched.sort_by(|a, b| b.priority.cmp(&a.priority));

        // Build injected prompt context
        let mut prompt_lines = Vec::new();
        prompt_lines.push("### ACTIVE PROJECT RULES & ARCHITECTURAL INVARIANTS:".to_string());
        for rule in &matched {
            let tier_label = match rule.tier {
                RuleTier::AlwaysOn => "[ALWAYS-ON]",
                RuleTier::AutoAttached => "[AUTO-ATTACHED]",
                RuleTier::Manual => "[SUMMONED]",
            };
            prompt_lines.push(format!("- {} **{}**: {}", tier_label, rule.name, rule.content));
        }

        EvaluatedRuleset {
            matched_rules: matched,
            total_rules_evaluated: self.rules.len(),
            injected_prompt_context: prompt_lines.join("\n"),
            triggered_globs,
        }
    }
}

fn matches_glob(pattern: &str, path: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.starts_with("*.") {
        let ext = &pattern[2..];
        return path.ends_with(&format!(".{}", ext));
    }
    if pattern.ends_with("/**") {
        let prefix = &pattern[..pattern.len() - 3];
        return path.starts_with(prefix);
    }
    path.contains(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tiered_rules_evaluation() {
        let engine = TieredRulesEngine::new();
        
        // When touching a ruby file: should trigger AlwaysOn + Rails rule
        let res = engine.evaluate(&["db/migrate/20260928_create_users.rb"], &[]);
        assert!(res.matched_rules.iter().any(|r| r.name == "core_safety_invariants"));
        assert!(res.matched_rules.iter().any(|r| r.name == "rails_zero_downtime_rules"));
        assert!(!res.matched_rules.iter().any(|r| r.name == "rust_microkernel_standards"));

        // When touching a rust file: should trigger AlwaysOn + Rust rule
        let res_rust = engine.evaluate(&["crates/hgb-core/src/lib.rs"], &[]);
        assert!(res_rust.matched_rules.iter().any(|r| r.name == "rust_microkernel_standards"));

        // Manual summon
        let res_manual = engine.evaluate(&["main.rs"], &["performance"]);
        assert!(res_manual.matched_rules.iter().any(|r| r.name == "brutal_perf_audit"));
    }
}
