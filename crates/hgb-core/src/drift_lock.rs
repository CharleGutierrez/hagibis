use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaPillars {
    pub preferred_http_client: String,
    pub preferred_styling: String,
    pub preferred_state_mgr: String,
    pub error_handling_policy: String,
    pub async_runtime: String,
}

impl Default for DnaPillars {
    fn default() -> Self {
        Self {
            preferred_http_client: "reqwest".to_string(),
            preferred_styling: "TailwindCSS".to_string(),
            preferred_state_mgr: "Zustand".to_string(),
            error_handling_policy: "Result".to_string(),
            async_runtime: "tokio".to_string(),
        }
    }
}

// Aliases for compatibility
impl DnaPillars {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitecturalDna {
    pub version: u32,
    pub ecosystem: String,
    pub pillars: DnaPillars,
    pub forbidden_import_patterns: Vec<String>,
    pub forbidden_syntax_patterns: Vec<String>,
    pub created_at_rfc3339: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftSeverity {
    Warning,
    BlockingError,
}

impl std::fmt::Display for DriftSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warning => write!(f, "WARNING"),
            Self::BlockingError => write!(f, "BLOCKING_ERROR"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftViolation {
    pub rule_name: String,
    pub severity: DriftSeverity,
    pub offending_snippet: String,
    pub reason: String,
    pub remediation_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceAuditReport {
    pub passed: bool,
    pub total_violations: usize,
    pub blocking_violations: usize,
    pub violations: Vec<DriftViolation>,
}

impl ComplianceAuditReport {
    pub fn clean() -> Self {
        Self {
            passed: true,
            total_violations: 0,
            blocking_violations: 0,
            violations: Vec::new(),
        }
    }
}
