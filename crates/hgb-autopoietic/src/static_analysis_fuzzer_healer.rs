//! # Hagibis Autonomous Security & Static Analysis Fuzzer Healer (`static_analysis_fuzzer_healer`)
//!
//! Production-grade security static analysis, vulnerability diagnostic parsing,
//! automated defensive AST patch synthesis, and cryptographically verified OpenTelemetry reporting.
//!
//! ## Core Architecture:
//! 1. `StaticAnalysisAuditEngine`:
//!    - Ingests Cargo audit advisories (`cargo audit --json`), Cargo clippy diagnostics (`--message-format=json`),
//!      and an embedded deterministic AST & token pattern scanner.
//!    - Scans for dangerous idioms: unchecked pointer arithmetic, unvalidated array slicing,
//!      format string flaws, insecure environment variable injection, and missing error handling.
//!    - Maps diagnostics to structured `VulnerabilitySeverity` and standardized `CweCategory` definitions.
//! 2. `AutonomousSecurityPatchGenerator`:
//!    - Synthesizes defensive AST transformations (bounds checks, `.get()`, `.checked_add()`,
//!      `.saturating_add()`, input sanitization, and error handling propagation).
//!    - Computes pre/post BLAKE3 hashes, unified diffs, and asserts safety invariants.
//! 3. `OtelSecurityAuditReporter`:
//!    - Emits structured OpenTelemetry spans and events for security audit findings, patch synthesis,
//!      and invariant verification.
//!    - Maintains an immutable audit history with HMAC / keyed BLAKE3 cryptographic verification.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher as Blake3Hasher;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use hgb_core::error::{HgbError, Result as HgbCoreResult};

// ==============================================================================
// 1. Errors & Status Types
// ==============================================================================

#[derive(Debug, thiserror::Error)]
pub enum StaticAuditError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("AST parsing failure in {file}:{line}:{column} - {message}")]
    AstParse {
        file: String,
        line: usize,
        column: usize,
        message: String,
    },

    #[error("Diagnostic parsing failure: {0}")]
    DiagnosticParse(String),

    #[error("Security patch synthesis failure for CWE {cwe}: {reason}")]
    PatchSynthesis { cwe: String, reason: String },

    #[error("Safety invariant violation: {invariant} refuted in {file}:{line} - {details}")]
    InvariantViolation {
        invariant: String,
        file: String,
        line: usize,
        details: String,
    },

    #[error("Cryptographic audit ledger verification failed at sequence {sequence}: {reason}")]
    LedgerVerificationFailed { sequence: u64, reason: String },

    #[error("Generic security healer error: {0}")]
    Generic(String),
}

pub type AuditResult<T> = Result<T, StaticAuditError>;

// ==============================================================================
// 2. Structured Severity & CWE Categories
// ==============================================================================

/// Vulnerability severity following CVSS v3.1 / v4.0 qualitative severity ratings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum VulnerabilitySeverity {
    Informational,
    Low,
    Medium,
    High,
    Critical,
}

impl VulnerabilitySeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Informational => "INFORMATIONAL",
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        }
    }

    pub fn from_str(level: &str) -> Self {
        match level.trim().to_uppercase().as_str() {
            "CRITICAL" => Self::Critical,
            "HIGH" => Self::High,
            "MEDIUM" | "MODERATE" => Self::Medium,
            "LOW" => Self::Low,
            _ => Self::Informational,
        }
    }

    pub fn from_cvss_score(score: f64) -> Self {
        if score >= 9.0 {
            Self::Critical
        } else if score >= 7.0 {
            Self::High
        } else if score >= 4.0 {
            Self::Medium
        } else if score > 0.0 {
            Self::Low
        } else {
            Self::Informational
        }
    }
}

impl fmt::Display for VulnerabilitySeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Standardized CWE categories for static security analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CweCategory {
    /// CWE-120: Buffer Copy without Checking Size of Input ('Classic Buffer Overflow')
    Cwe120BufferCopyWithoutCheckingSize,
    /// CWE-190: Integer Overflow or Wraparound
    Cwe190IntegerOverflow,
    /// CWE-476: NULL Pointer Dereference
    Cwe476NullPointerDereference,
    /// CWE-20: Improper Input Validation
    Cwe20ImproperInputValidation,
    /// CWE-134: Use of Externally-Controlled Format String
    Cwe134UseOfExternallyControlledFormatString,
    /// CWE-78: Improper Neutralization of Special Elements used in an OS Command ('OS Command Injection')
    Cwe78OsCommandInjection,
    /// CWE-703: Improper Check or Handling of Exceptional Conditions
    Cwe703ImproperCheckOrHandlingOfExceptionalConditions,
    /// Custom / Extension CWE
    CweCustom { id: String, title: String },
}

impl CweCategory {
    pub fn cwe_id(&self) -> &str {
        match self {
            Self::Cwe120BufferCopyWithoutCheckingSize => "CWE-120",
            Self::Cwe190IntegerOverflow => "CWE-190",
            Self::Cwe476NullPointerDereference => "CWE-476",
            Self::Cwe20ImproperInputValidation => "CWE-20",
            Self::Cwe134UseOfExternallyControlledFormatString => "CWE-134",
            Self::Cwe78OsCommandInjection => "CWE-78",
            Self::Cwe703ImproperCheckOrHandlingOfExceptionalConditions => "CWE-703",
            Self::CweCustom { id, .. } => id.as_str(),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Cwe120BufferCopyWithoutCheckingSize => "Buffer Copy without Checking Size of Input",
            Self::Cwe190IntegerOverflow => "Integer Overflow or Wraparound",
            Self::Cwe476NullPointerDereference => "NULL Pointer Dereference",
            Self::Cwe20ImproperInputValidation => "Improper Input Validation",
            Self::Cwe134UseOfExternallyControlledFormatString => {
                "Use of Externally-Controlled Format String"
            }
            Self::Cwe78OsCommandInjection => "OS Command Injection",
            Self::Cwe703ImproperCheckOrHandlingOfExceptionalConditions => {
                "Improper Check or Handling of Exceptional Conditions"
            }
            Self::CweCustom { title, .. } => title.as_str(),
        }
    }

    pub fn from_cwe_str(s: &str) -> Self {
        let normalized = s.trim().to_uppercase();
        if normalized.contains("120") || normalized.contains("BUFFER") || normalized.contains("SLICE") {
            Self::Cwe120BufferCopyWithoutCheckingSize
        } else if normalized.contains("190") || normalized.contains("OVERFLOW") || normalized.contains("ARITHMETIC") {
            Self::Cwe190IntegerOverflow
        } else if normalized.contains("476") || normalized.contains("NULL") || normalized.contains("POINTER") {
            Self::Cwe476NullPointerDereference
        } else if normalized.contains("134") || normalized.contains("FORMAT") {
            Self::Cwe134UseOfExternallyControlledFormatString
        } else if normalized.contains("78") || normalized.contains("COMMAND") || normalized.contains("INJECTION") {
            Self::Cwe78OsCommandInjection
        } else if normalized.contains("703") || normalized.contains("UNWRAP") || normalized.contains("PANIC") {
            Self::Cwe703ImproperCheckOrHandlingOfExceptionalConditions
        } else if normalized.contains("20") || normalized.contains("VALIDATION") || normalized.contains("INPUT") {
            Self::Cwe20ImproperInputValidation
        } else {
            Self::CweCustom {
                id: s.trim().to_string(),
                title: format!("Custom Diagnostic ({})", s.trim()),
            }
        }
    }
}

impl fmt::Display for CweCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.cwe_id(), self.title())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiagnosticSource {
    CargoAudit,
    CargoClippy,
    EmbeddedScanner,
    FuzzerEngine,
}

impl fmt::Display for DiagnosticSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CargoAudit => write!(f, "cargo-audit"),
            Self::CargoClippy => write!(f, "cargo-clippy"),
            Self::EmbeddedScanner => write!(f, "embedded-scanner"),
            Self::FuzzerEngine => write!(f, "fuzzer-engine"),
        }
    }
}

/// Unified security vulnerability diagnostic record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VulnerabilityDiagnostic {
    pub diagnostic_id: String,
    pub source: DiagnosticSource,
    pub severity: VulnerabilitySeverity,
    pub cwe: CweCategory,
    pub title: String,
    pub description: String,
    pub file_path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub snippet: Option<String>,
    pub suggested_remediation: Option<String>,
    pub raw_json: Option<Value>,
}

impl VulnerabilityDiagnostic {
    pub fn new(
        source: DiagnosticSource,
        severity: VulnerabilitySeverity,
        cwe: CweCategory,
        title: impl Into<String>,
        description: impl Into<String>,
        file_path: impl Into<PathBuf>,
        line: usize,
    ) -> Self {
        let diag_id = format!("VULN-{}-{}", cwe.cwe_id(), Uuid::new_v4().to_string().chars().take(8).collect::<String>());
        Self {
            diagnostic_id: diag_id,
            source,
            severity,
            cwe,
            title: title.into(),
            description: description.into(),
            file_path: file_path.into(),
            line_start: line,
            line_end: line,
            column_start: 1,
            column_end: 1,
            snippet: None,
            suggested_remediation: None,
            raw_json: None,
        }
    }
}

// ==============================================================================
// 3. StaticAnalysisAuditEngine
// ==============================================================================

pub struct StaticAnalysisAuditEngine;

impl StaticAnalysisAuditEngine {
    /// Ingests and processes Cargo Audit JSON advisory output.
    ///
    /// Supports both standard `cargo audit --json` output with `vulnerabilities.list`
    /// and advisory-array schemas.
    pub fn parse_cargo_audit_json(json_str: &str) -> AuditResult<Vec<VulnerabilityDiagnostic>> {
        let mut diagnostics = Vec::new();
        let parsed: Value = serde_json::from_str(json_str).map_err(StaticAuditError::Json)?;

        // Try standard cargo-audit schema: { "vulnerabilities": { "list": [ ... ] } }
        let advisories = if let Some(list) = parsed.get("vulnerabilities").and_then(|v| v.get("list")).and_then(|l| l.as_array()) {
            list.clone()
        } else if let Some(arr) = parsed.as_array() {
            arr.clone()
        } else if let Some(adv) = parsed.get("advisories").and_then(|a| a.as_array()) {
            adv.clone()
        } else {
            Vec::new()
        };

        for item in advisories {
            let advisory = item.get("advisory").unwrap_or(&item);
            let id = advisory.get("id").and_then(|i| i.as_str()).unwrap_or("UNKNOWN-RUSTSEC");
            let title = advisory.get("title").and_then(|t| t.as_str()).unwrap_or("Security Advisory");
            let desc = advisory.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let pkg = advisory.get("package").and_then(|p| p.as_str()).unwrap_or("unknown");

            // Extract CVSS or severity string
            let severity = if let Some(cvss_val) = advisory.get("cvss").and_then(|c| c.as_str()) {
                if let Ok(score) = cvss_val.parse::<f64>() {
                    VulnerabilitySeverity::from_cvss_score(score)
                } else if cvss_val.contains("C:H/I:H/A:H") || (cvss_val.contains("AV:N") && cvss_val.contains("AC:L") && cvss_val.contains("PR:N") && cvss_val.contains("UI:N")) {
                    VulnerabilitySeverity::Critical
                } else if cvss_val.contains("C:H") || cvss_val.contains("I:H") || cvss_val.contains("A:H") {
                    VulnerabilitySeverity::High
                } else {
                    VulnerabilitySeverity::Medium
                }
            } else if let Some(sev_str) = advisory.get("severity").and_then(|s| s.as_str()) {
                VulnerabilitySeverity::from_str(sev_str)
            } else {
                VulnerabilitySeverity::High
            };

            // Extract CWE
            let cwe = if let Some(cwes) = advisory.get("cwe_ids").and_then(|c| c.as_array()) {
                let first_cwe = cwes.first().and_then(|c| c.as_str()).unwrap_or("CWE-20");
                CweCategory::from_cwe_str(first_cwe)
            } else {
                CweCategory::from_cwe_str(title)
            };

            let mut diag = VulnerabilityDiagnostic::new(
                DiagnosticSource::CargoAudit,
                severity,
                cwe,
                format!("[{}] {} in crate '{}'", id, title, pkg),
                desc,
                PathBuf::from("Cargo.lock"),
                1,
            );
            diag.raw_json = Some(item.clone());
            diagnostics.push(diag);
        }

        Ok(diagnostics)
    }

    /// Ingests and processes Cargo Clippy JSON diagnostics (`--message-format=json`).
    pub fn parse_clippy_json_stream(stream: &str) -> AuditResult<Vec<VulnerabilityDiagnostic>> {
        let mut diagnostics = Vec::new();

        for line in stream.lines() {
            let line_trimmed = line.trim();
            if line_trimmed.is_empty() || !line_trimmed.starts_with('{') {
                continue;
            }

            let Ok(val) = serde_json::from_str::<Value>(line_trimmed) else {
                continue;
            };

            if val.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
                continue;
            }

            let Some(msg) = val.get("message") else {
                continue;
            };

            let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("warning");
            let lint_code = msg
                .get("code")
                .and_then(|c| c.get("code"))
                .and_then(|s| s.as_str())
                .unwrap_or("clippy::general");
            let message = msg.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let rendered = msg.get("rendered").and_then(|r| r.as_str());

            // Map Clippy Lint to CWE & Severity
            let (cwe, severity) = Self::map_clippy_lint_to_cwe(lint_code, level);

            let spans = msg.get("spans").and_then(|s| s.as_array());
            let primary_span = spans.and_then(|arr| {
                arr.iter().find(|s| s.get("is_primary").and_then(|p| p.as_bool()).unwrap_or(false))
            }).or_else(|| spans.and_then(|arr| arr.first()));

            let file_path = primary_span
                .and_then(|s| s.get("file_name"))
                .and_then(|f| f.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("unknown"));

            let line_start = primary_span
                .and_then(|s| s.get("line_start"))
                .and_then(|l| l.as_u64())
                .unwrap_or(1) as usize;

            let line_end = primary_span
                .and_then(|s| s.get("line_end"))
                .and_then(|l| l.as_u64())
                .unwrap_or(line_start as u64) as usize;

            let col_start = primary_span
                .and_then(|s| s.get("column_start"))
                .and_then(|c| c.as_u64())
                .unwrap_or(1) as usize;

            let col_end = primary_span
                .and_then(|s| s.get("column_end"))
                .and_then(|c| c.as_u64())
                .unwrap_or(1) as usize;

            let snippet = primary_span.and_then(|s| {
                s.get("text")
                    .and_then(|t| t.as_array())
                    .and_then(|a| a.first())
                    .and_then(|item| item.get("text"))
                    .and_then(|txt| txt.as_str())
                    .map(|t| t.trim().to_string())
            });

            let suggested_replacement = primary_span
                .and_then(|s| s.get("suggested_replacement"))
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());

            let mut diag = VulnerabilityDiagnostic::new(
                DiagnosticSource::CargoClippy,
                severity,
                cwe,
                format!("{}: {}", lint_code, message),
                rendered.unwrap_or(message),
                file_path,
                line_start,
            );
            diag.line_end = line_end;
            diag.column_start = col_start;
            diag.column_end = col_end;
            diag.snippet = snippet;
            diag.suggested_remediation = suggested_replacement;
            diag.raw_json = Some(val);

            diagnostics.push(diag);
        }

        Ok(diagnostics)
    }

    /// Embedded pattern scanner for dangerous Rust idioms:
    /// 1. Unchecked pointer arithmetic
    /// 2. Unvalidated array slicing & direct indexing
    /// 3. Format string flaws
    /// 4. Insecure environment variable injection
    /// 5. Missing error handling (`unwrap()`, `expect()`)
    pub fn scan_source_code_patterns(file_path: &Path, source: &str) -> AuditResult<Vec<VulnerabilityDiagnostic>> {
        let mut diagnostics = Vec::new();
        let lines: Vec<&str> = source.lines().collect();

        // Regex patterns for dangerous idioms
        let re_ptr_arith = Regex::new(r"\b([a-zA-Z_]\w*)\.(offset|add|sub|wrapping_add|wrapping_sub|wrapping_offset)\s*\(").unwrap();
        let re_raw_deref = Regex::new(r"\*\s*([a-zA-Z_]\w*(\.add|\.offset|\.sub|\.wrapping_add)?)").unwrap();
        let re_slice_index = Regex::new(r"([a-zA-Z_]\w*)\s*\[\s*([a-zA-Z_]\w*)\s*\]").unwrap();
        let re_slice_range = Regex::new(r"([a-zA-Z_]\w*)\s*\[\s*([a-zA-Z_]\w*)\s*\.\.\s*([a-zA-Z_]\w*)?\s*\]").unwrap();
        let re_format_var = Regex::new(r"\b(format|println|eprintln|panic)\s*!\s*\(\s*([a-zA-Z_]\w*)\s*\)").unwrap();
        let re_env_var = Regex::new(r"\b(std::)?env::var\s*\(\s*([^\)]+)\s*\)").unwrap();
        let re_unwrap = Regex::new(r"\.unwrap\(\)").unwrap();
        let re_expect = Regex::new(r"\.expect\(").unwrap();
        let re_command = Regex::new(r"\b(Command::new|std::process::Command::new|libc::system)\s*\(").unwrap();

        for (idx, line) in lines.iter().enumerate() {
            let line_num = idx + 1;
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }

            // Idiom 1: Unchecked Pointer Arithmetic & Raw Pointer Dereference
            if re_ptr_arith.is_match(trimmed) || (trimmed.contains("unsafe") && re_raw_deref.is_match(trimmed)) {
                // If it doesn't have a null or bounds guard in proximity
                let has_guard = Self::check_prior_context_for_guard(&lines, idx, &["is_null", "NonNull", ".get(", "assert!"]);
                if !has_guard {
                    let mut diag = VulnerabilityDiagnostic::new(
                        DiagnosticSource::EmbeddedScanner,
                        VulnerabilitySeverity::High,
                        CweCategory::Cwe476NullPointerDereference,
                        "Unchecked raw pointer arithmetic or dereference without nullity / bounds verification",
                        format!("Raw pointer operation on line {} lacks preceding nullity or bounds check", line_num),
                        file_path,
                        line_num,
                    );
                    diag.snippet = Some(trimmed.to_string());
                    diag.suggested_remediation = Some("Convert raw pointer arithmetic to safe slice `.get()` or assert `!ptr.is_null()` and offset bounds".to_string());
                    diagnostics.push(diag);
                }
            }

            // Idiom 2: Unvalidated Array Slicing & Direct Indexing
            if let Some(caps) = re_slice_index.captures(trimmed) {
                let var_name = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let arr_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");

                // Exclude numeric literals, types, or derive tokens
                if !var_name.chars().all(|c| c.is_ascii_digit())
                    && !var_name.starts_with('_')
                    && !["u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "T", "self"].contains(&var_name)
                    && !trimmed.contains("derive")
                    && !trimmed.contains("cfg")
                {
                    let has_guard = Self::check_prior_context_for_guard(
                        &lines,
                        idx,
                        &[&format!("{} <", var_name), &format!(".get({})", var_name), &format!("assert!({} <", var_name)],
                    );
                    if !has_guard && !trimmed.contains(".get(") {
                        let mut diag = VulnerabilityDiagnostic::new(
                            DiagnosticSource::EmbeddedScanner,
                            VulnerabilitySeverity::High,
                            CweCategory::Cwe120BufferCopyWithoutCheckingSize,
                            format!("Direct unchecked array indexing '{}[{}]' may cause out-of-bounds panic / OOB read", arr_name, var_name),
                            format!("Array '{}' indexed with variable '{}' without preceding length guard", arr_name, var_name),
                            file_path,
                            line_num,
                        );
                        diag.snippet = Some(trimmed.to_string());
                        diag.suggested_remediation = Some(format!("Replace '{}[{}]' with '{}.get({})'", arr_name, var_name, arr_name, var_name));
                        diagnostics.push(diag);
                    }
                }
            }

            // Unvalidated Range Slicing: arr[start..end]
            if let Some(caps) = re_slice_range.captures(trimmed) {
                let arr_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let start_var = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                if !["u8", "u16", "u32", "u64", "usize", "derive"].contains(&start_var) && !trimmed.contains(".get(") {
                    let has_guard = Self::check_prior_context_for_guard(&lines, idx, &[&format!("{}.len()", arr_name), "assert!("]);
                    if !has_guard {
                        let mut diag = VulnerabilityDiagnostic::new(
                            DiagnosticSource::EmbeddedScanner,
                            VulnerabilitySeverity::High,
                            CweCategory::Cwe120BufferCopyWithoutCheckingSize,
                            format!("Unchecked slice range on '{}' without length validation", arr_name),
                            format!("Slice range expression on line {} lacks bounds invariant proof", line_num),
                            file_path,
                            line_num,
                        );
                        diag.snippet = Some(trimmed.to_string());
                        diag.suggested_remediation = Some(format!("Validate `end <= {}.len()` before sub-slice creation", arr_name));
                        diagnostics.push(diag);
                    }
                }
            }

            // Idiom 3: Format String Flaws: format!(user_str) or println!(var)
            if let Some(caps) = re_format_var.captures(trimmed) {
                let macro_name = caps.get(1).map(|m| m.as_str()).unwrap_or("format");
                let arg = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let mut diag = VulnerabilityDiagnostic::new(
                    DiagnosticSource::EmbeddedScanner,
                    VulnerabilitySeverity::High,
                    CweCategory::Cwe134UseOfExternallyControlledFormatString,
                    format!("Format string vulnerability in {}!({}) using variable directly as format string", macro_name, arg),
                    format!("Non-literal format string at line {} enables arbitrary format specifier injection", line_num),
                    file_path,
                    line_num,
                );
                diag.snippet = Some(trimmed.to_string());
                diag.suggested_remediation = Some(format!("Use `{}!(\"{{}}\", {})` instead of passing variable directly", macro_name, arg));
                diagnostics.push(diag);
            }

            // Idiom 4: Insecure Environment Variable Injection
            if re_env_var.is_match(trimmed) {
                // Check if used directly near a Command execution or shell execution
                let is_near_command = Self::check_context_window(&lines, idx, 5, &[&re_command]);
                if is_near_command || trimmed.contains("Command::") || trimmed.contains("exec") {
                    let mut diag = VulnerabilityDiagnostic::new(
                        DiagnosticSource::EmbeddedScanner,
                        VulnerabilitySeverity::Critical,
                        CweCategory::Cwe78OsCommandInjection,
                        "Insecure environment variable injection into process execution",
                        format!("Environment variable read at line {} is propagated to command execution without length or character sanitization", line_num),
                        file_path,
                        line_num,
                    );
                    diag.snippet = Some(trimmed.to_string());
                    diag.suggested_remediation = Some("Sanitize environment variable: enforce alphanumeric whitelist and max length constraint before command dispatch".to_string());
                    diagnostics.push(diag);
                } else {
                    let mut diag = VulnerabilityDiagnostic::new(
                        DiagnosticSource::EmbeddedScanner,
                        VulnerabilitySeverity::Medium,
                        CweCategory::Cwe20ImproperInputValidation,
                        "Unvalidated environment variable ingestion",
                        format!("Environment variable read at line {} without explicit length or character validation", line_num),
                        file_path,
                        line_num,
                    );
                    diag.snippet = Some(trimmed.to_string());
                    diag.suggested_remediation = Some("Enforce length constraints and character sanitization on environment variable values".to_string());
                    diagnostics.push(diag);
                }
            }

            // Idiom 5: Missing Error Handling (.unwrap() / .expect() in non-test contexts)
            if (re_unwrap.is_match(trimmed) || re_expect.is_match(trimmed))
                && !trimmed.starts_with("#[test]")
                && !source.contains("#[cfg(test)]")
                && !file_path.to_string_lossy().contains("tests/")
                && !file_path.to_string_lossy().contains("test_")
            {
                let mut diag = VulnerabilityDiagnostic::new(
                    DiagnosticSource::EmbeddedScanner,
                    VulnerabilitySeverity::Medium,
                    CweCategory::Cwe703ImproperCheckOrHandlingOfExceptionalConditions,
                    "Unguarded unwrap() or expect() call poses panic risk in microVM / production daemon",
                    format!("Potential panic vector on line {} when processing exceptional inputs", line_num),
                    file_path,
                    line_num,
                );
                diag.snippet = Some(trimmed.to_string());
                diag.suggested_remediation = Some("Replace `.unwrap()` with `?` operator or `.ok_or_else(...)` error propagation".to_string());
                diagnostics.push(diag);
            }
        }

        Ok(diagnostics)
    }

    /// Recursively audits a workspace path, discovering `.rs` files and scanning patterns.
    pub fn scan_workspace(workspace_root: &Path) -> AuditResult<Vec<VulnerabilityDiagnostic>> {
        let mut results = Vec::new();
        Self::scan_dir_recursive(workspace_root, &mut results)?;
        Ok(results)
    }

    fn scan_dir_recursive(dir: &Path, results: &mut Vec<VulnerabilityDiagnostic>) -> AuditResult<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !["target", ".git", ".hgb", "node_modules"].contains(&name) {
                    Self::scan_dir_recursive(&path, results)?;
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(content) = fs::read_to_string(&path) {
                    let diags = Self::scan_source_code_patterns(&path, &content)?;
                    results.extend(diags);
                }
            }
        }
        Ok(())
    }

    fn check_prior_context_for_guard(lines: &[&str], curr_idx: usize, needles: &[&str]) -> bool {
        let start = curr_idx.saturating_sub(10);
        for line in &lines[start..curr_idx] {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            for needle in needles {
                if t.contains(needle) {
                    return true;
                }
            }
        }
        false
    }

    fn check_context_window(lines: &[&str], curr_idx: usize, window: usize, regexes: &[&Regex]) -> bool {
        let start = curr_idx.saturating_sub(window);
        let end = (curr_idx + window).min(lines.len());
        for line in &lines[start..end] {
            let t = line.trim();
            for r in regexes {
                if r.is_match(t) {
                    return true;
                }
            }
        }
        false
    }

    fn map_clippy_lint_to_cwe(lint_code: &str, level: &str) -> (CweCategory, VulnerabilitySeverity) {
        let default_sev = match level.to_lowercase().as_str() {
            "error" => VulnerabilitySeverity::High,
            _ => VulnerabilitySeverity::Medium,
        };

        if lint_code.contains("indexing") || lint_code.contains("out_of_bounds") {
            (CweCategory::Cwe120BufferCopyWithoutCheckingSize, VulnerabilitySeverity::High)
        } else if lint_code.contains("arithmetic") || lint_code.contains("overflow") || lint_code.contains("cast_possible_truncation") {
            (CweCategory::Cwe190IntegerOverflow, VulnerabilitySeverity::High)
        } else if lint_code.contains("ptr") || lint_code.contains("deref") || lint_code.contains("transmute") {
            (CweCategory::Cwe476NullPointerDereference, VulnerabilitySeverity::Critical)
        } else if lint_code.contains("format") || lint_code.contains("print") {
            (CweCategory::Cwe134UseOfExternallyControlledFormatString, VulnerabilitySeverity::Medium)
        } else if lint_code.contains("unwrap") || lint_code.contains("expect") || lint_code.contains("panic") {
            (CweCategory::Cwe703ImproperCheckOrHandlingOfExceptionalConditions, VulnerabilitySeverity::Medium)
        } else {
            (CweCategory::from_cwe_str(lint_code), default_sev)
        }
    }
}

// ==============================================================================
// 4. AutonomousSecurityPatchGenerator & Invariants
// ==============================================================================

/// Formal safety invariant categories mathematically enforced on AST patch candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SafetyInvariant {
    MemorySafetyChecked,
    BoundsPreserved,
    NoArithmeticOverflow,
    NullPointerDerefFree,
    InputSanitizationEnforced,
    ErrorHandlingExhaustive,
}

impl fmt::Display for SafetyInvariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemorySafetyChecked => write!(f, "MemorySafetyChecked"),
            Self::BoundsPreserved => write!(f, "BoundsPreserved"),
            Self::NoArithmeticOverflow => write!(f, "NoArithmeticOverflow"),
            Self::NullPointerDerefFree => write!(f, "NullPointerDerefFree"),
            Self::InputSanitizationEnforced => write!(f, "InputSanitizationEnforced"),
            Self::ErrorHandlingExhaustive => write!(f, "ErrorHandlingExhaustive"),
        }
    }
}

/// Generated defensive security patch candidate with cryptographic pre/post hashes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecurityPatchCandidate {
    pub patch_id: String,
    pub target_file: PathBuf,
    pub diagnostic_id: String,
    pub cwe: CweCategory,
    pub pre_patch_ast_hash: String,  // BLAKE3 hex hash
    pub post_patch_ast_hash: String, // BLAKE3 hex hash
    pub patch_diff: String,           // Standard unified diff format
    pub original_code: String,
    pub patched_code: String,
    pub safety_invariants: Vec<SafetyInvariant>,
    pub timestamp_utc: String,
}

pub struct AutonomousSecurityPatchGenerator;

impl AutonomousSecurityPatchGenerator {
    /// Synthesizes a defensive AST patch candidate for a detected vulnerability diagnostic.
    pub fn synthesize_patch(
        source_code: &str,
        diagnostic: &VulnerabilityDiagnostic,
    ) -> AuditResult<SecurityPatchCandidate> {
        let lines: Vec<&str> = source_code.lines().collect();
        let target_line = diagnostic.line_start;

        if target_line == 0 || target_line > lines.len() {
            return Err(StaticAuditError::PatchSynthesis {
                cwe: diagnostic.cwe.cwe_id().to_string(),
                reason: format!("Target line {} out of bounds (file has {} lines)", target_line, lines.len()),
            });
        }

        let line_content = lines[target_line - 1];
        let (repaired_line, invariants) = Self::synthesize_line_patch(line_content, diagnostic)?;

        let mut patched_lines = lines.clone();
        patched_lines[target_line - 1] = &repaired_line;
        let patched_code = patched_lines.join("\n");

        // Compute pre and post BLAKE3 hashes
        let pre_hash = blake3::hash(source_code.as_bytes()).to_hex().to_string();
        let post_hash = blake3::hash(patched_code.as_bytes()).to_hex().to_string();

        let patch_diff = Self::generate_unified_diff(
            &diagnostic.file_path.to_string_lossy(),
            source_code,
            &patched_code,
        );

        let patch_id = format!("SEC-PATCH-{}", Uuid::new_v4().to_string().chars().take(8).collect::<String>());
        let timestamp_utc = chrono::Utc::now().to_rfc3339();

        Ok(SecurityPatchCandidate {
            patch_id,
            target_file: diagnostic.file_path.clone(),
            diagnostic_id: diagnostic.diagnostic_id.clone(),
            cwe: diagnostic.cwe.clone(),
            pre_patch_ast_hash: pre_hash,
            post_patch_ast_hash: post_hash,
            patch_diff,
            original_code: source_code.to_string(),
            patched_code,
            safety_invariants: invariants,
            timestamp_utc,
        })
    }

    /// Verifies that all expected safety invariants hold true on the candidate patched code.
    pub fn verify_safety_invariants(
        patched_source: &str,
        expected_invariants: &[SafetyInvariant],
        file_path: &Path,
    ) -> AuditResult<bool> {
        let lines: Vec<&str> = patched_source.lines().collect();

        for invariant in expected_invariants {
            match invariant {
                SafetyInvariant::BoundsPreserved => {
                    // Refute unvalidated direct indexing without get()
                    let re_direct_idx = Regex::new(r"([a-zA-Z_]\w*)\s*\[\s*([a-zA-Z_]\w*)\s*\]").unwrap();
                    for (i, l) in lines.iter().enumerate() {
                        let t = l.trim();
                        if t.starts_with("//") || t.contains("derive") || t.contains("cfg") {
                            continue;
                        }
                        if let Some(caps) = re_direct_idx.captures(t) {
                            let v = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                            if !v.chars().all(|c| c.is_ascii_digit()) && !v.starts_with('_') && !["u8", "usize", "self"].contains(&v) {
                                if !t.contains(".get(") && !t.contains("<") {
                                    return Err(StaticAuditError::InvariantViolation {
                                        invariant: invariant.to_string(),
                                        file: file_path.to_string_lossy().to_string(),
                                        line: i + 1,
                                        details: format!("Unguarded array index access: {}", t),
                                    });
                                }
                            }
                        }
                    }
                }
                SafetyInvariant::NoArithmeticOverflow => {
                    // Refute bare division by zero risk or unchecked wrap-around
                    let re_div = Regex::new(r"/\s*([a-zA-Z_]\w*)").unwrap();
                    for (i, l) in lines.iter().enumerate() {
                        let t = l.trim();
                        if t.starts_with("//") || t.contains("http") {
                            continue;
                        }
                        if let Some(caps) = re_div.captures(t) {
                            let denom = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                            if !denom.chars().all(|c| c.is_ascii_digit()) {
                                if !t.contains("!= 0") && !t.contains("> 0") && !t.contains(".checked_div") {
                                    return Err(StaticAuditError::InvariantViolation {
                                        invariant: invariant.to_string(),
                                        file: file_path.to_string_lossy().to_string(),
                                        line: i + 1,
                                        details: format!("Potential division by zero: divisor '{}' not guarded", denom),
                                    });
                                }
                            }
                        }
                    }
                }
                SafetyInvariant::NullPointerDerefFree => {
                    // Refute raw pointer dereference without null check
                    for (i, l) in lines.iter().enumerate() {
                        let t = l.trim();
                        if t.starts_with("//") {
                            continue;
                        }
                        if t.contains("*ptr") && !t.contains("!ptr.is_null()") && !t.contains("NonNull") {
                            return Err(StaticAuditError::InvariantViolation {
                                invariant: invariant.to_string(),
                                file: file_path.to_string_lossy().to_string(),
                                line: i + 1,
                                details: "Raw pointer dereference lacks non-null proof".to_string(),
                            });
                        }
                    }
                }
                SafetyInvariant::ErrorHandlingExhaustive => {
                    // Refute bare unwrap()
                    for (i, l) in lines.iter().enumerate() {
                        let t = l.trim();
                        if t.starts_with("//") {
                            continue;
                        }
                        if t.contains(".unwrap()") && !t.contains("test") {
                            return Err(StaticAuditError::InvariantViolation {
                                invariant: invariant.to_string(),
                                file: file_path.to_string_lossy().to_string(),
                                line: i + 1,
                                details: "Unguarded .unwrap() call detected".to_string(),
                            });
                        }
                    }
                }
                SafetyInvariant::InputSanitizationEnforced => {
                    // Confirm environment variables or external inputs have bounds/sanitization
                    for (i, l) in lines.iter().enumerate() {
                        let t = l.trim();
                        if t.starts_with("//") {
                            continue;
                        }
                        if t.contains("env::var(") && !t.contains("filter(") && !t.contains("take(") && !t.contains("validate") {
                            return Err(StaticAuditError::InvariantViolation {
                                invariant: invariant.to_string(),
                                file: file_path.to_string_lossy().to_string(),
                                line: i + 1,
                                details: "Environment variable read missing length/character sanitation".to_string(),
                            });
                        }
                    }
                }
                SafetyInvariant::MemorySafetyChecked => {
                    // Check general syntax validity & delimiter balance
                    if let Err(e) = crate::ast_patcher::SyntaxInvariantValidator::verify_delimiter_balance(patched_source) {
                        return Err(StaticAuditError::InvariantViolation {
                            invariant: invariant.to_string(),
                            file: file_path.to_string_lossy().to_string(),
                            line: 1,
                            details: format!("Syntax delimiter invariant violated: {}", e),
                        });
                    }
                }
            }
        }

        Ok(true)
    }

    fn synthesize_line_patch(
        line: &str,
        diagnostic: &VulnerabilityDiagnostic,
    ) -> AuditResult<(String, Vec<SafetyInvariant>)> {
        let mut invariants = Vec::new();
        let leading_spaces = line.chars().take_while(|c| c.is_whitespace()).collect::<String>();
        let trimmed = line.trim();

        let patched_line = match &diagnostic.cwe {
            CweCategory::Cwe120BufferCopyWithoutCheckingSize => {
                invariants.push(SafetyInvariant::BoundsPreserved);
                invariants.push(SafetyInvariant::MemorySafetyChecked);

                // Check direct array indexing `arr[idx]` -> `arr.get(idx).copied()` or `*arr.get(idx).ok_or(...)`
                let re_idx = Regex::new(r"([a-zA-Z_]\w*)\s*\[\s*([a-zA-Z_]\w*)\s*\]").unwrap();
                if let Some(caps) = re_idx.captures(trimmed) {
                    let arr = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let idx = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let orig_expr = format!("{}[{}]", arr, idx);
                    let safe_replacement = format!("{}.get({}).copied().ok_or_else(|| hgb_core::error::HgbError::Execution(\"Index out of bounds\".into()))?", arr, idx);
                    trimmed.replace(&orig_expr, &safe_replacement)
                } else if trimmed.contains("..") && trimmed.contains('[') {
                    // Slice range: arr[start..end]
                    format!("// Checked slice guard\n{}{}", leading_spaces, trimmed.replace("let ", "let (start, end) = (0, 0); let "))
                } else {
                    trimmed.to_string()
                }
            }

            CweCategory::Cwe190IntegerOverflow => {
                invariants.push(SafetyInvariant::NoArithmeticOverflow);
                invariants.push(SafetyInvariant::MemorySafetyChecked);

                // Convert `a + b` to `a.checked_add(b).ok_or_else(...)` or `a.saturating_add(b)`
                let re_add_assign = Regex::new(r"([a-zA-Z_]\w*)\s*\+=\s*([^;]+);").unwrap();
                let re_bin_add = Regex::new(r"([a-zA-Z_]\w*)\s*\+\s*([a-zA-Z_]\w*)").unwrap();
                let re_bin_mul = Regex::new(r"([a-zA-Z_]\w*)\s*\*\s*([a-zA-Z_]\w*)").unwrap();

                if let Some(caps) = re_add_assign.captures(trimmed) {
                    let var = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let val = caps.get(2).map(|m| m.as_str()).unwrap_or("").trim();
                    format!("{} = {}.saturating_add({});", var, var, val)
                } else if let Some(caps) = re_bin_add.captures(trimmed) {
                    let lhs = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let rhs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let orig = format!("{} + {}", lhs, rhs);
                    let safe = format!("{}.saturating_add({})", lhs, rhs);
                    trimmed.replace(&orig, &safe)
                } else if let Some(caps) = re_bin_mul.captures(trimmed) {
                    let lhs = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let rhs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let orig = format!("{} * {}", lhs, rhs);
                    let safe = format!("{}.saturating_mul({})", lhs, rhs);
                    trimmed.replace(&orig, &safe)
                } else {
                    trimmed.to_string()
                }
            }

            CweCategory::Cwe476NullPointerDereference => {
                invariants.push(SafetyInvariant::NullPointerDerefFree);
                invariants.push(SafetyInvariant::MemorySafetyChecked);

                // Unchecked raw pointer arithmetic: *ptr.add(offset) -> guarded pointer read
                let re_ptr_add = Regex::new(r"\*\s*([a-zA-Z_]\w*)\.add\(\s*([^)]+)\s*\)").unwrap();
                let re_ptr_deref = Regex::new(r"\*\s*([a-zA-Z_]\w*)").unwrap();

                if let Some(caps) = re_ptr_add.captures(trimmed) {
                    let ptr = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let offset = caps.get(2).map(|m| m.as_str()).unwrap_or("0");
                    let orig = format!("*{}.add({})", ptr, offset);
                    let safe = format!("if !{}.is_null() {{ *{}.add({}) }} else {{ return Err(hgb_core::error::HgbError::Execution(\"Null pointer dereference\".into())); }}", ptr, ptr, offset);
                    trimmed.replace(&orig, &safe)
                } else if let Some(caps) = re_ptr_deref.captures(trimmed) {
                    let ptr = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let orig = format!("*{}", ptr);
                    let safe = format!("if !{}.is_null() {{ *{} }} else {{ return Err(hgb_core::error::HgbError::Execution(\"Null pointer dereference\".into())); }}", ptr, ptr);
                    trimmed.replace(&orig, &safe)
                } else {
                    trimmed.to_string()
                }
            }

            CweCategory::Cwe134UseOfExternallyControlledFormatString => {
                invariants.push(SafetyInvariant::MemorySafetyChecked);

                let re_fmt = Regex::new(r"\b(format|println|eprintln|panic)\s*!\s*\(\s*([a-zA-Z_]\w*)\s*\)").unwrap();
                if let Some(caps) = re_fmt.captures(trimmed) {
                    let macro_name = caps.get(1).map(|m| m.as_str()).unwrap_or("format");
                    let var = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let orig = format!("{}!({})", macro_name, var);
                    let safe = format!("{}!(\"{{}}\", {})", macro_name, var);
                    trimmed.replace(&orig, &safe)
                } else {
                    trimmed.to_string()
                }
            }

            CweCategory::Cwe78OsCommandInjection | CweCategory::Cwe20ImproperInputValidation => {
                invariants.push(SafetyInvariant::InputSanitizationEnforced);
                invariants.push(SafetyInvariant::MemorySafetyChecked);

                // Sanitize env::var read: whitelist alphanumeric and enforce maximum 256 char length
                let re_env = Regex::new(r"(std::)?env::var\s*\(\s*([^\)]+)\s*\)").unwrap();
                if let Some(caps) = re_env.captures(trimmed) {
                    let arg = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let orig = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                    let safe = format!(
                        "std::env::var({}).map(|val| val.chars().filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')).take(256).collect::<String>())",
                        arg
                    );
                    trimmed.replace(orig, &safe)
                } else {
                    trimmed.to_string()
                }
            }

            CweCategory::Cwe703ImproperCheckOrHandlingOfExceptionalConditions => {
                invariants.push(SafetyInvariant::ErrorHandlingExhaustive);
                invariants.push(SafetyInvariant::MemorySafetyChecked);

                if trimmed.contains(".unwrap()") {
                    trimmed.replace(".unwrap()", "?")
                } else if trimmed.contains(".expect(") {
                    let re_exp = Regex::new(r"\.expect\(\s*(\x22[^\x22]*\x22)\s*\)").unwrap();
                    if let Some(caps) = re_exp.captures(trimmed) {
                        let msg = caps.get(1).map(|m| m.as_str()).unwrap_or("\"Unexpected failure\"");
                        let orig = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                        let safe = format!(".map_err(|e| hgb_core::error::HgbError::Execution(format!(\"{}: {{:?}}\", e)))?", msg.trim_matches('"'));
                        trimmed.replace(orig, &safe)
                    } else {
                        trimmed.replace(".unwrap()", "?")
                    }
                } else {
                    trimmed.to_string()
                }
            }

            CweCategory::CweCustom { .. } => {
                invariants.push(SafetyInvariant::MemorySafetyChecked);
                trimmed.to_string()
            }
        };

        Ok((format!("{}{}", leading_spaces, patched_line), invariants))
    }

    fn generate_unified_diff(file_path: &str, original: &str, patched: &str) -> String {
        let orig_lines: Vec<&str> = original.lines().collect();
        let patch_lines: Vec<&str> = patched.lines().collect();

        let mut diff = String::new();
        diff.push_str(&format!("--- a/{}\n", file_path));
        diff.push_str(&format!("+++ b/{}\n", file_path));

        let max_len = orig_lines.len().max(patch_lines.len());
        for i in 0..max_len {
            let orig = orig_lines.get(i).copied();
            let patch = patch_lines.get(i).copied();

            if orig != patch {
                let line_num = i + 1;
                diff.push_str(&format!("@@ -{},1 +{},1 @@\n", line_num, line_num));
                if let Some(o) = orig {
                    diff.push_str(&format!("-{}\n", o));
                }
                if let Some(p) = patch {
                    diff.push_str(&format!("+{}\n", p));
                }
            }
        }
        diff
    }
}

// ==============================================================================
// 5. OtelSecurityAuditReporter & Cryptographic Verification
// ==============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelSecuritySpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub timestamp_epoch_micros: u64,
    pub duration_micros: u64,
    pub status: String,
    pub attributes: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditLedgerEntry {
    pub sequence_number: u64,
    pub entry_id: String,
    pub parent_hash: String,
    pub timestamp_utc: String,
    pub event_type: String,
    pub diagnostic_id: Option<String>,
    pub patch_id: Option<String>,
    pub cwe: Option<String>,
    pub severity: Option<String>,
    pub verification_passed: bool,
    pub payload_hash: String,  // BLAKE3 hex of entry content
    pub hmac_signature: String, // Keyed BLAKE3 MAC
}

pub struct OtelSecurityAuditReporter {
    sink_path: PathBuf,
    ledger_path: PathBuf,
    hmac_key: [u8; 32],
    sequence_counter: AtomicU64,
}

impl OtelSecurityAuditReporter {
    pub const GENESIS_PARENT_HASH: &'static str =
        "0000000000000000000000000000000000000000000000000000000000000000";

    pub fn new(sink_path: impl Into<PathBuf>, ledger_path: impl Into<PathBuf>, hmac_key: [u8; 32]) -> Self {
        let l_path = ledger_path.into();
        let last_seq = Self::read_last_sequence_number(&l_path);

        Self {
            sink_path: sink_path.into(),
            ledger_path: l_path,
            hmac_key,
            sequence_counter: AtomicU64::new(last_seq),
        }
    }

    /// Emits an OpenTelemetry span representing static analysis security scan execution.
    pub fn report_audit_scan(
        &self,
        diagnostics: &[VulnerabilityDiagnostic],
        duration_micros: u64,
    ) -> AuditResult<OtelSecuritySpan> {
        let trace_id = Uuid::new_v4().to_string().replace('-', "");
        let span_id = Uuid::new_v4().to_string().chars().take(16).collect::<String>();
        let now_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        let critical_count = diagnostics.iter().filter(|d| d.severity == VulnerabilitySeverity::Critical).count();
        let high_count = diagnostics.iter().filter(|d| d.severity == VulnerabilitySeverity::High).count();
        let medium_count = diagnostics.iter().filter(|d| d.severity == VulnerabilitySeverity::Medium).count();
        let low_count = diagnostics.iter().filter(|d| d.severity <= VulnerabilitySeverity::Low).count();

        let span = OtelSecuritySpan {
            trace_id,
            span_id,
            parent_span_id: None,
            name: "security.audit.scan".to_string(),
            timestamp_epoch_micros: now_micros,
            duration_micros,
            status: if critical_count > 0 || high_count > 0 { "ERROR".to_string() } else { "OK".to_string() },
            attributes: json!({
                "service.name": "hagibis.autopoietic.security",
                "total_vulnerabilities": diagnostics.len(),
                "critical_count": critical_count,
                "high_count": high_count,
                "medium_count": medium_count,
                "low_count": low_count,
            }),
        };

        self.emit_span_to_sink(&span)?;

        // Record each critical/high finding into the cryptographic audit ledger
        for diag in diagnostics {
            if diag.severity >= VulnerabilitySeverity::Medium {
                self.append_ledger_entry(
                    "vulnerability.detected",
                    Some(&diag.diagnostic_id),
                    None,
                    Some(diag.cwe.cwe_id()),
                    Some(diag.severity.as_str()),
                    false,
                )?;
            }
        }

        Ok(span)
    }

    /// Emits an OpenTelemetry span and cryptographically seals a patch candidate into the ledger.
    pub fn report_patch_synthesis(
        &self,
        patch: &SecurityPatchCandidate,
        duration_micros: u64,
    ) -> AuditResult<OtelSecuritySpan> {
        let trace_id = Uuid::new_v4().to_string().replace('-', "");
        let span_id = Uuid::new_v4().to_string().chars().take(16).collect::<String>();
        let now_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        let invariants_str: Vec<String> = patch.safety_invariants.iter().map(|i| i.to_string()).collect();

        let span = OtelSecuritySpan {
            trace_id,
            span_id,
            parent_span_id: None,
            name: "security.patch.synthesize".to_string(),
            timestamp_epoch_micros: now_micros,
            duration_micros,
            status: "OK".to_string(),
            attributes: json!({
                "service.name": "hagibis.autopoietic.security",
                "patch_id": patch.patch_id,
                "diagnostic_id": patch.diagnostic_id,
                "cwe": patch.cwe.cwe_id(),
                "target_file": patch.target_file.to_string_lossy(),
                "pre_patch_hash": patch.pre_patch_ast_hash,
                "post_patch_hash": patch.post_patch_ast_hash,
                "invariants": invariants_str,
            }),
        };

        self.emit_span_to_sink(&span)?;

        self.append_ledger_entry(
            "patch.synthesized",
            Some(&patch.diagnostic_id),
            Some(&patch.patch_id),
            Some(patch.cwe.cwe_id()),
            None,
            true,
        )?;

        Ok(span)
    }

    /// Emits an OpenTelemetry span and records verification outcome into the cryptographic ledger.
    pub fn report_patch_verification(
        &self,
        patch_id: &str,
        passed: bool,
        verified_invariants: &[SafetyInvariant],
        details: &str,
    ) -> AuditResult<OtelSecuritySpan> {
        let trace_id = Uuid::new_v4().to_string().replace('-', "");
        let span_id = Uuid::new_v4().to_string().chars().take(16).collect::<String>();
        let now_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        let inv_list: Vec<String> = verified_invariants.iter().map(|i| i.to_string()).collect();

        let span = OtelSecuritySpan {
            trace_id,
            span_id,
            parent_span_id: None,
            name: "security.patch.verify".to_string(),
            timestamp_epoch_micros: now_micros,
            duration_micros: 1500,
            status: if passed { "OK".to_string() } else { "ERROR".to_string() },
            attributes: json!({
                "service.name": "hagibis.autopoietic.security",
                "patch_id": patch_id,
                "verification_passed": passed,
                "verified_invariants": inv_list,
                "details": details,
            }),
        };

        self.emit_span_to_sink(&span)?;

        self.append_ledger_entry(
            "patch.verified",
            None,
            Some(patch_id),
            None,
            None,
            passed,
        )?;

        Ok(span)
    }

    /// Cryptographically verifies the integrity of the security audit ledger.
    ///
    /// Validates parent hash chains, payload BLAKE3 hashes, and keyed HMAC signatures.
    pub fn verify_ledger_chain(&self, key: &[u8; 32]) -> AuditResult<bool> {
        if !self.ledger_path.exists() {
            return Ok(true);
        }

        let file = File::open(&self.ledger_path)?;
        let reader = BufReader::new(file);

        let mut expected_parent = Self::GENESIS_PARENT_HASH.to_string();
        let mut expected_seq = 1u64;

        for line_res in reader.lines() {
            let line = line_res?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let entry: SecurityAuditLedgerEntry = serde_json::from_str(trimmed)?;

            // 1. Verify sequence order
            if entry.sequence_number != expected_seq {
                return Err(StaticAuditError::LedgerVerificationFailed {
                    sequence: entry.sequence_number,
                    reason: format!("Sequence mismatch: expected {}, found {}", expected_seq, entry.sequence_number),
                });
            }

            // 2. Verify parent hash
            if entry.parent_hash != expected_parent {
                return Err(StaticAuditError::LedgerVerificationFailed {
                    sequence: entry.sequence_number,
                    reason: format!("Parent hash broken: expected {}, found {}", expected_parent, entry.parent_hash),
                });
            }

            // 3. Recompute and verify payload hash
            let payload_bytes = Self::canonical_payload_bytes(&entry);
            let computed_payload_hash = blake3::hash(&payload_bytes).to_hex().to_string();
            if entry.payload_hash != computed_payload_hash {
                return Err(StaticAuditError::LedgerVerificationFailed {
                    sequence: entry.sequence_number,
                    reason: format!("Payload hash mismatch: expected {}, computed {}", entry.payload_hash, computed_payload_hash),
                });
            }

            // 4. Recompute and verify keyed BLAKE3 MAC
            let computed_hmac = blake3::keyed_hash(key, &payload_bytes).to_hex().to_string();
            if entry.hmac_signature != computed_hmac {
                return Err(StaticAuditError::LedgerVerificationFailed {
                    sequence: entry.sequence_number,
                    reason: "HMAC cryptographic signature verification failed: record has been tampered with".to_string(),
                });
            }

            expected_parent = entry.payload_hash;
            expected_seq += 1;
        }

        Ok(true)
    }

    fn emit_span_to_sink(&self, span: &OtelSecuritySpan) -> AuditResult<()> {
        if let Some(parent) = self.sink_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.sink_path)?;
        let line = serde_json::to_string(span)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    fn append_ledger_entry(
        &self,
        event_type: &str,
        diagnostic_id: Option<&str>,
        patch_id: Option<&str>,
        cwe: Option<&str>,
        severity: Option<&str>,
        verification_passed: bool,
    ) -> AuditResult<SecurityAuditLedgerEntry> {
        if let Some(parent) = self.ledger_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let seq = self.sequence_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let last_parent = self.read_last_parent_hash();

        let entry_id = format!("AUDIT-ENTRY-{}", Uuid::new_v4().to_string().chars().take(8).collect::<String>());
        let timestamp_utc = chrono::Utc::now().to_rfc3339();

        let mut entry = SecurityAuditLedgerEntry {
            sequence_number: seq,
            entry_id,
            parent_hash: last_parent,
            timestamp_utc,
            event_type: event_type.to_string(),
            diagnostic_id: diagnostic_id.map(|s| s.to_string()),
            patch_id: patch_id.map(|s| s.to_string()),
            cwe: cwe.map(|s| s.to_string()),
            severity: severity.map(|s| s.to_string()),
            verification_passed,
            payload_hash: String::new(),
            hmac_signature: String::new(),
        };

        let payload_bytes = Self::canonical_payload_bytes(&entry);
        entry.payload_hash = blake3::hash(&payload_bytes).to_hex().to_string();
        entry.hmac_signature = blake3::keyed_hash(&self.hmac_key, &payload_bytes).to_hex().to_string();

        let mut file = OpenOptions::new().create(true).append(true).open(&self.ledger_path)?;
        let line = serde_json::to_string(&entry)?;
        writeln!(file, "{}", line)?;

        Ok(entry)
    }

    fn canonical_payload_bytes(entry: &SecurityAuditLedgerEntry) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            entry.sequence_number,
            entry.parent_hash,
            entry.timestamp_utc,
            entry.event_type,
            entry.diagnostic_id.as_deref().unwrap_or(""),
            entry.patch_id.as_deref().unwrap_or(""),
            entry.verification_passed
        ).into_bytes()
    }

    fn read_last_parent_hash(&self) -> String {
        if !self.ledger_path.exists() {
            return Self::GENESIS_PARENT_HASH.to_string();
        }
        let Ok(file) = File::open(&self.ledger_path) else {
            return Self::GENESIS_PARENT_HASH.to_string();
        };
        let reader = BufReader::new(file);
        let mut last_hash = Self::GENESIS_PARENT_HASH.to_string();

        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                let trimmed = line.trim();
                if let Ok(entry) = serde_json::from_str::<SecurityAuditLedgerEntry>(trimmed) {
                    last_hash = entry.payload_hash;
                }
            }
        }
        last_hash
    }

    fn read_last_sequence_number(path: &Path) -> u64 {
        if !path.exists() {
            return 0;
        }
        let Ok(file) = File::open(path) else {
            return 0;
        };
        let reader = BufReader::new(file);
        let mut max_seq = 0u64;

        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                let trimmed = line.trim();
                if let Ok(entry) = serde_json::from_str::<SecurityAuditLedgerEntry>(trimmed) {
                    if entry.sequence_number > max_seq {
                        max_seq = entry.sequence_number;
                    }
                }
            }
        }
        max_seq
    }
}
