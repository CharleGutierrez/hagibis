use hgb_core::drift_lock::{
    ArchitecturalDna, ComplianceAuditReport, DnaPillars, DriftSeverity, DriftViolation,
};
use hgb_core::Result;
use hgb_storage::ArchitecturalDnaStore;
use std::fs;
use std::path::Path;

pub struct DriftLockEngine;

impl DriftLockEngine {
    /// Extract or infer repository Architectural DNA from workspace files
    pub fn extract_dna<P: AsRef<Path>>(workspace_root: P) -> Result<ArchitecturalDna> {
        let root = workspace_root.as_ref();
        let cargo_toml = root.join("Cargo.toml");
        let package_json = root.join("package.json");

        if cargo_toml.exists() {
            let content = fs::read_to_string(cargo_toml)?;
            let preferred_http = if content.contains("reqwest") { "reqwest" } else { "hyper" };
            let preferred_err = if content.contains("thiserror") { "thiserror::Result" } else { "anyhow::Result" };

            let dna = ArchitecturalDna {
                version: 1,
                ecosystem: "rust".to_string(),
                pillars: DnaPillars {
                    preferred_http_client: preferred_http.to_string(),
                    preferred_styling: "ratatui".to_string(),
                    preferred_state_mgr: "RwLock/Arc".to_string(),
                    error_handling_policy: preferred_err.to_string(),
                    async_runtime: "tokio".to_string(),
                },
                forbidden_import_patterns: vec![
                    "hyper".to_string(),
                    "actix".to_string(),
                ],
                forbidden_syntax_patterns: vec![
                    ".unwrap()".to_string(),
                    "panic!".to_string(),
                    "unsafe ".to_string(),
                ],
                created_at_rfc3339: chrono::Utc::now().to_rfc3339(),
            };
            let _ = ArchitecturalDnaStore::save(workspace_root, &dna);
            return Ok(dna);
        }

        if package_json.exists() {
            let content = fs::read_to_string(package_json)?;
            let preferred_http = if content.contains("axios") { "axios" } else { "fetch" };
            let preferred_style = if content.contains("tailwindcss") { "TailwindCSS" } else { "CssModules" };
            let preferred_state = if content.contains("zustand") { "Zustand" } else { "Redux" };

            let dna = ArchitecturalDna {
                version: 1,
                ecosystem: "javascript".to_string(),
                pillars: DnaPillars {
                    preferred_http_client: preferred_http.to_string(),
                    preferred_styling: preferred_style.to_string(),
                    preferred_state_mgr: preferred_state.to_string(),
                    error_handling_policy: "try-catch".to_string(),
                    async_runtime: "nodejs".to_string(),
                },
                forbidden_import_patterns: vec![
                    "lodash".to_string(),
                    "moment".to_string(),
                ],
                forbidden_syntax_patterns: vec![
                    "as any".to_string(),
                    "@ts-ignore".to_string(),
                ],
                created_at_rfc3339: chrono::Utc::now().to_rfc3339(),
            };
            let _ = ArchitecturalDnaStore::save(workspace_root, &dna);
            return Ok(dna);
        }

        ArchitecturalDnaStore::load_or_init(workspace_root)
    }

    /// Audit a proposed code diff/patch against the Architectural DNA
    pub fn audit_patch(dna: &ArchitecturalDna, proposed_patch: &str) -> ComplianceAuditReport {
        let mut violations = Vec::new();

        for line in proposed_patch.lines() {
            if !line.starts_with('+') || line.starts_with("+++") {
                continue;
            }
            let added_content = &line[1..].trim();

            // 1. Check forbidden import patterns
            for forbidden_import in &dna.forbidden_import_patterns {
                if added_content.contains(forbidden_import) && (added_content.contains("import ") || added_content.contains("use ") || added_content.contains("require(")) {
                    violations.push(DriftViolation {
                        rule_name: format!("ForbiddenImport:{}", forbidden_import),
                        severity: DriftSeverity::BlockingError,
                        offending_snippet: added_content.to_string(),
                        reason: format!("Dependency '{}' violates repository Architectural DNA", forbidden_import),
                        remediation_hint: format!("Replace with preferred library: '{}'", dna.pillars.preferred_http_client),
                    });
                }
            }

            // 2. Check forbidden syntax patterns
            for forbidden_syntax in &dna.forbidden_syntax_patterns {
                if added_content.contains(forbidden_syntax) {
                    let severity = if forbidden_syntax == ".unwrap()" || forbidden_syntax == "unsafe " {
                        DriftSeverity::BlockingError
                    } else {
                        DriftSeverity::Warning
                    };

                    violations.push(DriftViolation {
                        rule_name: format!("ForbiddenSyntax:{}", forbidden_syntax),
                        severity,
                        offending_snippet: added_content.to_string(),
                        reason: format!("Syntax '{}' is disallowed by codebase policy", forbidden_syntax),
                        remediation_hint: format!("Use '{}' pattern instead of panicking or unsafe code", dna.pillars.error_handling_policy),
                    });
                }
            }
        }

        let blocking_violations = violations
            .iter()
            .filter(|v| v.severity == DriftSeverity::BlockingError)
            .count();
        let total_violations = violations.len();
        let passed = blocking_violations == 0;

        ComplianceAuditReport {
            passed,
            total_violations,
            blocking_violations,
            violations,
        }
    }
}
