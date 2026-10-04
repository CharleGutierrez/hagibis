//! # AppSecSentinel - Autonomous Pre-Apply Security Gate
//!
//! Audits AI-synthesized routes, database queries, and handlers before writing to disk.
//! Intercepts critical vulnerabilities including Broken Object-Level Authorization (BOLA),
//! hardcoded secrets, raw string SQL interpolation, and permissive security configurations.

use serde::{Deserialize, Serialize};

/// Classification of the detected security vulnerability
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VulnerabilityCategory {
    BrokenObjectLevelAuth, // BOLA / IDOR on mutating routes
    HardcodedSecret,       // Leaked private keys, API tokens, AWS keys
    SqlInjectionHazard,    // Raw string concatenation in database queries
    InsecureCorsConfig,    // Wildcard origins with credentials
    UnsafeDeserialization, // Insecure deserialization or eval-like hazard
}

/// Severity level of the finding
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecuritySeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl SecuritySeverity {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::High => "HIGH",
            Self::Medium => "MEDIUM",
            Self::Low => "LOW",
        }
    }
}

/// Specific security vulnerability finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFinding {
    pub category: VulnerabilityCategory,
    pub severity: SecuritySeverity,
    pub file_path: String,
    pub line_number: usize,
    pub code_snippet: String,
    pub cwe_id: String,
    pub description: String,
    pub remediation_patch: String,
}

/// Complete security audit report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSecReport {
    pub target_file: String,
    pub findings: Vec<SecurityFinding>,
    pub critical_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
    pub is_safe_to_apply: bool,
    pub security_score_pct: f32,
    pub summary: String,
}

pub struct AppSecSentinel;

impl AppSecSentinel {
    /// Audit a file's content against pre-commit security heuristics
    pub fn audit_content(file_path: &str, content: &str) -> AppSecReport {
        let mut physical_scan_output = String::new();
        // Cargo tree shell out removed to prevent test deadlocks
        if physical_scan_output.is_empty() {
            physical_scan_output = "No dependencies parsed".to_string();
        }

        let mut findings = Vec::new();
        let lines: Vec<&str> = content.lines().collect();

        for (idx, line) in lines.iter().enumerate() {
            let line_no = idx + 1;
            let trimmed = line.trim();

            // 1. HARDCODED SECRET DETECTION
            if trimmed.contains("AKIA") && trimmed.len() >= 20 {
                findings.push(SecurityFinding {
                    category: VulnerabilityCategory::HardcodedSecret,
                    severity: SecuritySeverity::Critical,
                    file_path: file_path.to_string(),
                    line_number: line_no,
                    code_snippet: trimmed.to_string(),
                    cwe_id: "CWE-798".to_string(),
                    description: "Hardcoded AWS Access Key ID detected.".to_string(),
                    remediation_patch: "Replace hardcoded credential with std::env::var(\"AWS_ACCESS_KEY_ID\")".to_string(),
                });
            } else if trimmed.contains("ghp_") || trimmed.contains("gho_") {
                findings.push(SecurityFinding {
                    category: VulnerabilityCategory::HardcodedSecret,
                    severity: SecuritySeverity::Critical,
                    file_path: file_path.to_string(),
                    line_number: line_no,
                    code_snippet: trimmed.to_string(),
                    cwe_id: "CWE-798".to_string(),
                    description: "Hardcoded GitHub Personal Access Token detected.".to_string(),
                    remediation_patch: "Load token securely from environment variable or secret vault".to_string(),
                });
            } else if (trimmed.contains("api_key = \"") || trimmed.contains("secret_key = \"") || trimmed.contains("password = \"") || trimmed.contains("apiKey: \""))
                && !trimmed.contains("env")
                && !trimmed.contains("test")
                && !trimmed.contains("\"\"")
            {
                findings.push(SecurityFinding {
                    category: VulnerabilityCategory::HardcodedSecret,
                    severity: SecuritySeverity::High,
                    file_path: file_path.to_string(),
                    line_number: line_no,
                    code_snippet: trimmed.to_string(),
                    cwe_id: "CWE-798".to_string(),
                    description: "Hardcoded secret or API credential detected in source code.".to_string(),
                    remediation_patch: "Use std::env::var or process.env to retrieve credentials dynamically".to_string(),
                });
            } else if trimmed.contains("-----BEGIN PRIVATE KEY-----") || trimmed.contains("-----BEGIN RSA PRIVATE KEY-----") {
                findings.push(SecurityFinding {
                    category: VulnerabilityCategory::HardcodedSecret,
                    severity: SecuritySeverity::Critical,
                    file_path: file_path.to_string(),
                    line_number: line_no,
                    code_snippet: trimmed.to_string(),
                    cwe_id: "CWE-321".to_string(),
                    description: "Raw cryptographic private key embedded in repository source.".to_string(),
                    remediation_patch: "Store private key in an external key management service or encrypted vault".to_string(),
                });
            }

            // 2. SQL INJECTION HAZARD
            let is_sql_line = trimmed.to_uppercase().contains("SELECT ")
                || trimmed.to_uppercase().contains("INSERT INTO ")
                || trimmed.to_uppercase().contains("UPDATE ")
                || trimmed.to_uppercase().contains("DELETE FROM ");

            if is_sql_line {
                let has_raw_concat = trimmed.contains(" + ")
                    || trimmed.contains("format!(\"")
                    || trimmed.contains("f\"")
                    || trimmed.contains("${");

                if has_raw_concat && !trimmed.contains("?") && !trimmed.contains("$1") {
                    findings.push(SecurityFinding {
                        category: VulnerabilityCategory::SqlInjectionHazard,
                        severity: SecuritySeverity::Critical,
                        file_path: file_path.to_string(),
                        line_number: line_no,
                        code_snippet: trimmed.to_string(),
                        cwe_id: "CWE-89".to_string(),
                        description: "Unparameterized raw string interpolation in SQL statement.".to_string(),
                        remediation_patch: "Use parameterized query placeholders (?1, $1) and bind variables securely".to_string(),
                    });
                }
            }

            // 3. BROKEN OBJECT-LEVEL AUTHORIZATION (BOLA / IDOR)
            let is_mutating_route = (trimmed.contains("delete_") || trimmed.contains("update_") || trimmed.contains(".post(") || trimmed.contains(".delete("))
                && (trimmed.contains("id: ") || trimmed.contains("id,") || trimmed.contains("user_id") || trimmed.contains(":id"));

            if is_mutating_route {
                // Check if the surrounding context lacks auth verification
                let surrounding_window = lines
                    .iter()
                    .skip(idx.saturating_sub(2))
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");

                let has_auth_guard = surrounding_window.contains("auth")
                    || surrounding_window.contains("jwt")
                    || surrounding_window.contains("session")
                    || surrounding_window.contains("verify")
                    || surrounding_window.contains("claims")
                    || surrounding_window.contains("permission");

                if !has_auth_guard {
                    findings.push(SecurityFinding {
                        category: VulnerabilityCategory::BrokenObjectLevelAuth,
                        severity: SecuritySeverity::High,
                        file_path: file_path.to_string(),
                        line_number: line_no,
                        code_snippet: trimmed.to_string(),
                        cwe_id: "CWE-639".to_string(),
                        description: "Mutating route / handler operates on entity ID without verifying session ownership or authentication token.".to_string(),
                        remediation_patch: "Inject AuthUser/JwtClaims extractor and verify user_id matches requesting identity".to_string(),
                    });
                }
            }

            // 4. PERMISSIVE CORS HAZARD
            if trimmed.contains("Access-Control-Allow-Origin") && trimmed.contains("*") && content.contains("credentials") {
                findings.push(SecurityFinding {
                    category: VulnerabilityCategory::InsecureCorsConfig,
                    severity: SecuritySeverity::Medium,
                    file_path: file_path.to_string(),
                    line_number: line_no,
                    code_snippet: trimmed.to_string(),
                    cwe_id: "CWE-942".to_string(),
                    description: "Permissive wildcard CORS origin with credential support enabled.".to_string(),
                    remediation_patch: "Explicitly allow trusted origin domains rather than wildcard '*'".to_string(),
                });
            }
        }

        let critical = findings.iter().filter(|f| f.severity == SecuritySeverity::Critical).count();
        let high = findings.iter().filter(|f| f.severity == SecuritySeverity::High).count();
        let medium = findings.iter().filter(|f| f.severity == SecuritySeverity::Medium).count();

        let is_safe = critical == 0 && high == 0;
        let score = if is_safe && medium == 0 {
            100.0
        } else if is_safe {
            85.0
        } else {
            (100.0 - (critical as f32 * 35.0) - (high as f32 * 20.0)).max(0.0)
        };

        let mut summary = if is_safe {
            format!("Clean: 0 critical/high security hazards identified in '{}'", file_path)
        } else {
            format!(
                "SECURITY GATE BLOCKED: Found {} critical and {} high severity vulnerabilities in '{}'",
                critical, high, file_path
            )
        };

        if !physical_scan_output.is_empty() && physical_scan_output != "No dependencies parsed" {
            summary.push_str(&format!("\n[Top Dependencies]:\n{}", physical_scan_output));
        }

        AppSecReport {
            target_file: file_path.to_string(),
            findings,
            critical_count: critical,
            high_count: high,
            medium_count: medium,
            is_safe_to_apply: is_safe,
            security_score_pct: score,
            summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_appsec_sentinel_hardcoded_secret() {
        let content = "let api_key = \"AKIAIOSFODNN7EXAMPLE\";";
        let report = AppSecSentinel::audit_content("src/main.rs", content);
        assert_eq!(report.critical_count, 1);
        assert_eq!(report.findings[0].category, VulnerabilityCategory::HardcodedSecret);
    }

    #[test]
    fn test_appsec_sentinel_sql_injection() {
        let content = "let query = format!(\"SELECT * FROM users WHERE id = {}\", user_input);";
        let report = AppSecSentinel::audit_content("src/db.rs", content);
        assert_eq!(report.critical_count, 1);
        assert_eq!(report.findings[0].category, VulnerabilityCategory::SqlInjectionHazard);
    }
    
    #[test]
    fn test_appsec_sentinel_bola() {
        let content = "async fn update_user(id: String, req: Request) { \n // doing update without checking \n }";
        let report = AppSecSentinel::audit_content("src/api.rs", content);
        assert_eq!(report.high_count, 1);
        assert_eq!(report.findings[0].category, VulnerabilityCategory::BrokenObjectLevelAuth);
    }
}
