use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VulnSeverity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VulnerabilityFinding {
    pub file_path: String,
    pub line_number: usize,
    pub severity: VulnSeverity,
    pub category: String,
    pub message: String,
    pub offending_code: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSecurityReport {
    pub scanned_files_count: usize,
    pub total_findings: usize,
    pub critical_count: usize,
    pub high_count: usize,
    pub findings: Vec<VulnerabilityFinding>,
    pub passed_audit: bool,
    pub audit_summary: String,
}

pub struct AiPrSecurityAudit;

impl AiPrSecurityAudit {
    pub fn new() -> Self {
        Self
    }

    /// Scans a collection of file paths and their contents for LLM-specific code vulnerabilities
    pub fn scan_files(files: &[(&str, &str)]) -> PrSecurityReport {
        let mut findings = Vec::new();

        for (file_path, content) in files {
            for (idx, line) in content.lines().enumerate() {
                let line_num = idx + 1;
                let trimmed = line.trim();

                if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("/*") {
                    continue;
                }

                // 1. Prompt Injection in LLM string interpolations
                if (trimmed.contains("messages:") || trimmed.contains("prompt:") || trimmed.contains("input:"))
                    && (trimmed.contains("${") || trimmed.contains("format!(") || trimmed.contains("#{"))
                    && !trimmed.contains("sanitize")
                    && !trimmed.contains("escape")
                {
                    findings.push(VulnerabilityFinding {
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        severity: VulnSeverity::High,
                        category: "PROMPT_INJECTION_RISK".to_string(),
                        message: "Direct user input interpolated into LLM prompt without sanitization boundary.".to_string(),
                        offending_code: trimmed.to_string(),
                        remediation: "Use structured delimiters (e.g. <user_input></user_input>) and validate input against injection attacks.".to_string(),
                    });
                }

                // 2. Insecure Deserialization / Dynamic Eval
                if trimmed.contains("eval(")
                    || trimmed.contains("pickle.loads(")
                    || trimmed.contains("Marshal.load(")
                    || trimmed.contains("new Function(")
                    || trimmed.contains("dangerouslySetInnerHTML")
                {
                    findings.push(VulnerabilityFinding {
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        severity: VulnSeverity::Critical,
                        category: "DYNAMIC_CODE_EXECUTION".to_string(),
                        message: "Dynamic code evaluation or insecure deserialization detected.".to_string(),
                        offending_code: trimmed.to_string(),
                        remediation: "Replace eval/pickle with safe JSON parsing or strictly typed data structures.".to_string(),
                    });
                }

                // 3. IDOR / Missing Tenant Scope
                if (trimmed.contains(".find(params[:id])") || trimmed.contains(".findById(req.params.id)"))
                    && !trimmed.contains("current_user")
                    && !trimmed.contains("tenant")
                {
                    findings.push(VulnerabilityFinding {
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        severity: VulnSeverity::High,
                        category: "TENANT_IDOR_VIOLATION".to_string(),
                        message: "Direct model lookup by ID without tenancy or current_user ownership scope (Insecure Direct Object Reference).".to_string(),
                        offending_code: trimmed.to_string(),
                        remediation: "Scope query through user: current_user.resources.find(params[:id]) or add authorization guard.".to_string(),
                    });
                }

                // 4. Raw SQL / Shell Injection
                if (trimmed.contains("execute(\"") || trimmed.contains("execSync(\"") || trimmed.contains("system(\"") || trimmed.contains("Command::new(\"sh\")"))
                    && (trimmed.contains("${") || trimmed.contains("#{") || trimmed.contains("format!("))
                {
                    findings.push(VulnerabilityFinding {
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        severity: VulnSeverity::Critical,
                        category: "RAW_INJECTION_SURFACE".to_string(),
                        message: "Unsanitized dynamic parameter in SQL or shell command execution.".to_string(),
                        offending_code: trimmed.to_string(),
                        remediation: "Use parameterized SQL queries (? or $1) and safe array arguments for process execution.".to_string(),
                    });
                }

                // 5. Hardcoded Mock / Live Secrets
                if (trimmed.contains("sk_live_") || trimmed.contains("sk_dummy_") || trimmed.contains("AKIA") || trimmed.contains("ghp_"))
                    && !file_path.contains("test")
                    && !file_path.contains("spec")
                {
                    findings.push(VulnerabilityFinding {
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        severity: VulnSeverity::Critical,
                        category: "HARDCODED_SECRET_LEAK".to_string(),
                        message: "Secret or API key token hardcoded in production source file.".to_string(),
                        offending_code: trimmed.to_string(),
                        remediation: "Move secrets to hgb vault or environment variables.".to_string(),
                    });
                }
            }
        }

        let critical_count = findings.iter().filter(|f| f.severity == VulnSeverity::Critical).count();
        let high_count = findings.iter().filter(|f| f.severity == VulnSeverity::High).count();
        let passed = critical_count == 0 && high_count == 0;

        let summary = if passed {
            format!("Security audit passed: {} files scanned with zero critical or high vulnerabilities.", files.len())
        } else {
            format!(
                "SECURITY AUDIT BLOCKED: Found {} critical and {} high severity vulnerabilities. Remediate before merging.",
                critical_count, high_count
            )
        };

        PrSecurityReport {
            scanned_files_count: files.len(),
            total_findings: findings.len(),
            critical_count,
            high_count,
            findings,
            passed_audit: passed,
            audit_summary: summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ai_pr_security_scan_catches_flaws() {
        let code_vuln = r#"
const prompt = `Translate this: ${user_input}`;
const doc = eval(payload);
const order = Order.find(params[:id]);
const res = execute("SELECT * FROM users WHERE id = " + user_id);
"#;
        let report = AiPrSecurityAudit::scan_files(&[("app/services/ai.js", code_vuln)]);
        assert!(!report.passed_audit);
        assert!(report.critical_count >= 1);
        assert!(report.high_count >= 1);
        assert_eq!(report.scanned_files_count, 1);
    }

    #[test]
    fn test_ai_pr_security_scan_clean_code() {
        let clean = r#"
const prompt = `<context>${sanitize(user_input)}</context>`;
const order = current_user.orders.find(params[:id]);
const res = db.query("SELECT * FROM users WHERE id = $1", [user_id]);
"#;
        let report = AiPrSecurityAudit::scan_files(&[("app/services/clean.js", clean)]);
        assert!(report.passed_audit);
        assert_eq!(report.critical_count, 0);
        assert_eq!(report.high_count, 0);
    }
}
