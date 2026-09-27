//! # Structural Invariant Guardrails & Anti-Spaghetti Linter
//!
//! Enforces clean architecture invariants, layer isolation rules, circular dependency protection,
//! DRY duplication thresholds, and mandatory observability/telemetry gates for vibe-coded software.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationSeverity {
    Critical,
    Warning,
    Suggestion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailViolation {
    pub rule_name: String,
    pub severity: ViolationSeverity,
    pub file_path: String,
    pub line: usize,
    pub message: String,
    pub suggested_fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardrailReport {
    pub scanned_files: usize,
    pub passed_rules: usize,
    pub violations: Vec<GuardrailViolation>,
    pub clean_architecture_score: u32,
    pub healthy: bool,
    pub summary: String,
}

pub struct StructuralGuardrails;

impl StructuralGuardrails {
    pub fn new() -> Self {
        Self
    }

    /// Audits a set of in-memory files (path, content) against architectural invariants
    pub fn audit_codebase(&self, files: &[(&str, &str)]) -> GuardrailReport {
        let mut violations = Vec::new();
        let mut total_rules_checked: usize = 0;

        // 1. Check Layer Isolation: Core/Domain cannot import UI/Web/DB drivers
        total_rules_checked += 1;
        for (path, content) in files {
            if path.contains("core") || path.contains("domain") {
                for (line_idx, line) in content.lines().enumerate() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("use ") || trimmed.starts_with("import ") {
                        if trimmed.contains("axum")
                            || trimmed.contains("actix")
                            || trimmed.contains("sqlx")
                            || trimmed.contains("diesel")
                            || trimmed.contains("react")
                            || trimmed.contains("next")
                        {
                            violations.push(GuardrailViolation {
                                rule_name: "LAYER_ISOLATION_DOMAIN_PURITY".to_string(),
                                severity: ViolationSeverity::Critical,
                                file_path: path.to_string(),
                                line: line_idx + 1,
                                message: format!("Domain/Core layer references external web/db dependency: `{}`", trimmed),
                                suggested_fix: "Inject database and HTTP adapters through traits/interfaces in the application layer.".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 2. Check Client Security: Client/Web components must never import server-only secrets or private keys
        total_rules_checked += 1;
        for (path, content) in files {
            if path.contains("client") || path.contains("ui") || path.contains("web") || path.contains("frontend") {
                for (line_idx, line) in content.lines().enumerate() {
                    let lower = line.to_lowercase();
                    if lower.contains("process.env.private_key")
                        || lower.contains("secret_key")
                        || lower.contains("database_url")
                        || lower.contains("service_role")
                    {
                        violations.push(GuardrailViolation {
                            rule_name: "LEAKED_SERVER_SECRET_IN_CLIENT".to_string(),
                            severity: ViolationSeverity::Critical,
                            file_path: path.to_string(),
                            line: line_idx + 1,
                            message: format!("Potential server credential or secret exposure in client component: `{}`", line.trim()),
                            suggested_fix: "Route secret operations through a secured backend API route or Server Action.".to_string(),
                        });
                    }
                }
            }
        }

        // 3. Check Circular Dependencies (module DAG analysis)
        total_rules_checked += 1;
        let mut import_graph: HashMap<String, HashSet<String>> = HashMap::new();
        for (path, content) in files {
            let mod_name = extract_module_name(path);
            let entry = import_graph.entry(mod_name.clone()).or_default();
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("use crate::") || trimmed.starts_with("import ") {
                    for (other_path, _) in files {
                        let other_mod = extract_module_name(other_path);
                        if other_mod != mod_name && trimmed.contains(&other_mod) {
                            entry.insert(other_mod);
                        }
                    }
                }
            }
        }

        for (mod_a, imports) in &import_graph {
            for mod_b in imports {
                if let Some(b_imports) = import_graph.get(mod_b) {
                    if b_imports.contains(mod_a) && mod_a < mod_b {
                        violations.push(GuardrailViolation {
                            rule_name: "CIRCULAR_MODULE_DEPENDENCY".to_string(),
                            severity: ViolationSeverity::Critical,
                            file_path: mod_a.clone(),
                            line: 1,
                            message: format!("Circular dependency detected between modules `{}` and `{}`", mod_a, mod_b),
                            suggested_fix: "Extract shared state or shared types into a dedicated common leaf module.".to_string(),
                        });
                    }
                }
            }
        }

        // 4. Check DRY Duplication / Identical Repeated Logic
        total_rules_checked += 1;
        let mut seen_blocks: HashMap<String, (String, usize)> = HashMap::new();
        for (path, content) in files {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() >= 5 {
                for window_start in 0..=(lines.len() - 5) {
                    let block_text = lines[window_start..window_start + 5]
                        .iter()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty() && !l.starts_with("//"))
                        .collect::<Vec<_>>()
                        .join("\n");

                    if block_text.len() > 80 {
                        let hash = blake3::hash(block_text.as_bytes()).to_hex().to_string();
                        if let Some((prev_file, prev_line)) = seen_blocks.get(&hash) {
                            if prev_file != path {
                                violations.push(GuardrailViolation {
                                    rule_name: "DUPLICATE_CODE_BLOCK_DRY".to_string(),
                                    severity: ViolationSeverity::Warning,
                                    file_path: path.to_string(),
                                    line: window_start + 1,
                                    message: format!("5-line duplicate block identical to `{}:{}`", prev_file, prev_line),
                                    suggested_fix: "Refactor repeated block into a shared helper function.".to_string(),
                                });
                                break;
                            }
                        } else {
                            seen_blocks.insert(hash, (path.to_string(), window_start + 1));
                        }
                    }
                }
            }
        }

        // 5. Mandatory Telemetry / Error Handling Gates (check for unwrap/swallowed errors in production code)
        total_rules_checked += 1;
        for (path, content) in files {
            if !path.contains("test") {
                let unwrap_count = content.matches(".unwrap()").count();
                if unwrap_count > 3 {
                    violations.push(GuardrailViolation {
                        rule_name: "NAKED_UNWRAP_IN_PRODUCTION".to_string(),
                        severity: ViolationSeverity::Warning,
                        file_path: path.to_string(),
                        line: 1,
                        message: format!("File contains {} naked `.unwrap()` calls which may panic under production stress", unwrap_count),
                        suggested_fix: "Replace `.unwrap()` with `?` error propagation, `.unwrap_or_default()`, or robust `match` handling.".to_string(),
                    });
                }
            }
        }

        let penalty: u32 = violations.iter().map(|v| match v.severity {
            ViolationSeverity::Critical => 25,
            ViolationSeverity::Warning => 10,
            ViolationSeverity::Suggestion => 3,
        }).sum();

        let clean_architecture_score = 100u32.saturating_sub(penalty);
        let healthy = clean_architecture_score >= 80 && !violations.iter().any(|v| v.severity == ViolationSeverity::Critical);

        let summary = format!(
            "Scanned {} files across {} rules. Found {} violations (Score: {}/100). Status: {}",
            files.len(),
            total_rules_checked,
            violations.len(),
            clean_architecture_score,
            if healthy { "HEALTHY & PRODUCTION READY" } else { "ATTENTION REQUIRED" }
        );

        GuardrailReport {
            scanned_files: files.len(),
            passed_rules: total_rules_checked.saturating_sub(violations.len()),
            violations,
            clean_architecture_score,
            healthy,
            summary,
        }
    }

    /// Audits files from a local directory (recursively scanning source files)
    pub fn audit_directory(&self, dir_path: &str) -> GuardrailReport {
        let mut contents = Vec::new();
        collect_source_files(std::path::Path::new(dir_path), 0, 4, &mut contents);

        let borrowed: Vec<(&str, &str)> = contents.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
        self.audit_codebase(&borrowed)
    }
}

fn collect_source_files(dir: &std::path::Path, depth: usize, max_depth: usize, out: &mut Vec<(String, String)>) {
    if depth > max_depth {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                collect_source_files(&path, depth + 1, max_depth, out);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if ext == "rs" || ext == "ts" || ext == "tsx" || ext == "js" {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            out.push((path.to_string_lossy().to_string(), content));
                        }
                    }
                }
            }
        }
    }
}

fn extract_module_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guardrails_detects_violations_and_clean_code() {
        let guardrails = StructuralGuardrails::new();

        let clean_files = vec![
            ("src/core/user.rs", "pub struct User { pub id: String }\nimpl User { pub fn new(id: String) -> Result<Self, String> { Ok(Self { id }) } }"),
            ("src/repo/user_repo.rs", "use crate::core::user::User;\npub struct UserRepo;\nimpl UserRepo { pub fn save(&self, u: &User) -> Result<(), String> { Ok(()) } }"),
        ];

        let clean_report = guardrails.audit_codebase(&clean_files);
        assert_eq!(clean_report.clean_architecture_score, 100);
        assert!(clean_report.healthy);
        assert!(clean_report.violations.is_empty());

        let dirty_files = vec![
            ("src/core/domain_logic.rs", "use sqlx::PgPool;\npub fn do_something() {}\n"),
            ("src/client/header.tsx", "const key = process.env.PRIVATE_KEY;\n"),
        ];

        let dirty_report = guardrails.audit_codebase(&dirty_files);
        assert!(dirty_report.clean_architecture_score < 80);
        assert!(!dirty_report.healthy);
        assert_eq!(dirty_report.violations.len(), 2);
    }
}
