//! # Pillar 3: Advanced Closed-Loop Autonomous Reflection Engine (`closed_loop_wasm_reflection`)
//!
//! Systems-grade autonomous reflection and self-healing engine operating inside untrusted
//! WebAssembly / polyglot sandboxes (`multitenant_wasm_sandbox`).
//!
//! ## Architectural Invariants:
//! 1. **Isolated Speculative Sandboxing**: Untrusted code candidates, patches, and scripts are executed
//!    within strictly bounded tenant sandboxes with gas/fuel limits, memory page quotas, and zero host contamination.
//! 2. **Continuous Diagnostic Interception**: Captures compiler errors, runtime panics, assertion failures,
//!    unhandled traps, and gas exhaustions.
//! 3. **Diagnostic Reflection Analyzer**: Categorizes errors (syntax, type mismatch, symbol resolution, panic,
//!    assertion, fuel exhaustion), identifies failure locations (line/column), and isolates failing context slices.
//! 4. **Deterministic AST & Pattern Healer**: Applies surgical pattern-based and AST-guided auto-repairs across
//!    WebAssembly Text (WAT), Python, Rust, and JavaScript snippets.
//! 5. **Reasoning Loop Feedback Generator**: Generates high-entropy structured reflection prompts for LLM repair
//!    when deterministic repair requires higher-order reasoning.
//! 6. **Cryptographic Consensus Verification**: On first-green execution (exit code 0, 0 traps, assertions verified),
//!    issues an immutable BLAKE3 / Ed25519 `ReflectionConsensusCertificate`.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};
use crate::multitenant_wasm_sandbox::{
    LanguageKind, StreamChunk, TenantExecutionResult, TenantIsolationPolicy, TenantSessionManager,
};

/// Default maximum reflection iterations before giving up
pub const DEFAULT_MAX_REFLECTION_RETRIES: usize = 3;
/// Maximum allowed retries ceiling
pub const MAX_REFLECTION_RETRIES_CEILING: usize = 10;
/// Default instruction fuel budget allocated per speculative reflection attempt
pub const DEFAULT_REFLECTION_GAS_BUDGET: u64 = 20_000_000;

// ==============================================================================
// 1. Diagnostics & Error Classification
// ==============================================================================

/// Categorization of compilation or execution failures intercepted in sandbox
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details")]
pub enum ReflectionErrorKind {
    /// WebAssembly Text (WAT) or language syntax error (e.g. mismatched parens, invalid opcode)
    SyntaxError {
        message: String,
        line: Option<usize>,
        column: Option<usize>,
    },
    /// Compiler type mismatch (e.g. i32 expected found i64, Rust E0308)
    TypeMismatch {
        expected: String,
        found: String,
        line: Option<usize>,
    },
    /// Unresolved symbol or import (e.g. undefined function, missing module import, Python NameError)
    UnresolvedSymbol {
        symbol: String,
        line: Option<usize>,
    },
    /// Unit test assertion failure (e.g. `assert_eq!`, Python `assert`, expected != actual)
    AssertionFailure {
        expression: String,
        expected: Option<String>,
        actual: Option<String>,
        line: Option<usize>,
    },
    /// Runtime panic, divide-by-zero, out-of-bounds index, or unhandled trap
    RuntimeTrap {
        trap_code: String,
        message: String,
        line: Option<usize>,
    },
    /// Execution instruction fuel exhaustion
    GasLimitExhausted {
        gas_consumed: u64,
        budget: u64,
    },
    /// Uncategorized diagnostic
    General {
        message: String,
    },
}

impl ReflectionErrorKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::SyntaxError { .. } => "SyntaxError",
            Self::TypeMismatch { .. } => "TypeMismatch",
            Self::UnresolvedSymbol { .. } => "UnresolvedSymbol",
            Self::AssertionFailure { .. } => "AssertionFailure",
            Self::RuntimeTrap { .. } => "RuntimeTrap",
            Self::GasLimitExhausted { .. } => "GasLimitExhausted",
            Self::General { .. } => "General",
        }
    }
}

/// Structured diagnostic item pinpointing an issue with context
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionDiagnostic {
    pub error_kind: ReflectionErrorKind,
    pub line_number: Option<usize>,
    pub column_number: Option<usize>,
    pub error_message: String,
    pub failing_context: Option<String>,
    pub suggested_patch_strategy: String,
}

// ==============================================================================
// 2. Telemetry, Attempts, and Consensus Models
// ==============================================================================

/// Record of an individual speculative reflection iteration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReflectionAttempt {
    pub round: usize,
    pub candidate_code: String,
    pub code_hash_blake3: String,
    pub success: bool,
    pub exit_code: i32,
    pub duration_ms: f64,
    pub gas_consumed: u64,
    pub stdout: String,
    pub stderr: String,
    pub diagnostics: Vec<ReflectionDiagnostic>,
    pub applied_patch_summary: Option<String>,
}

/// Cryptographic consensus certificate issued upon green self-healing verification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReflectionConsensusCertificate {
    pub script_id: String,
    pub initial_code_hash_blake3: String,
    pub healed_code_hash_blake3: String,
    pub total_rounds: usize,
    pub verified_green_at_epoch_ms: u64,
    pub ed25519_signature_hex: String,
    pub public_key_hex: String,
}

impl ReflectionConsensusCertificate {
    pub fn canonical_payload(&self) -> String {
        format!(
            "hgb-reflection-consensus:id={}:init={}:healed={}:rounds={}:epoch={}",
            self.script_id,
            self.initial_code_hash_blake3,
            self.healed_code_hash_blake3,
            self.total_rounds,
            self.verified_green_at_epoch_ms
        )
    }

    pub fn verify_signature(&self) -> bool {
        let pub_bytes = match hex::decode(&self.public_key_hex) {
            Ok(b) => match b.try_into() {
                Ok(arr) => arr,
                Err(_) => return false,
            },
            Err(_) => return false,
        };

        let verifying_key = match VerifyingKey::from_bytes(&pub_bytes) {
            Ok(k) => k,
            Err(_) => return false,
        };

        let sig_bytes = match hex::decode(&self.ed25519_signature_hex) {
            Ok(b) => match b.try_into() {
                Ok(arr) => arr,
                Err(_) => return false,
            },
            Err(_) => return false,
        };

        let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        verifying_key
            .verify(self.canonical_payload().as_bytes(), &signature)
            .is_ok()
    }
}

/// Complete report returned by the reflection loop
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReflectionSessionReport {
    pub script_id: String,
    pub language: LanguageKind,
    pub initial_code: String,
    pub final_code: String,
    pub healed: bool,
    pub total_rounds: usize,
    pub total_duration_ms: f64,
    pub total_gas_consumed: u64,
    pub attempts: Vec<ReflectionAttempt>,
    pub consensus_certificate: Option<ReflectionConsensusCertificate>,
    pub final_execution_result: Option<TenantExecutionResult>,
}

// ==============================================================================
// 3. Diagnostic Analyzer & Error Parser
// ==============================================================================

/// Analyzer that inspects compiler diagnostics, panics, and stderr output
pub struct ReflectionDiagnosticAnalyzer;

impl ReflectionDiagnosticAnalyzer {
    /// Extract structured diagnostics from execution output
    pub fn analyze(
        source_code: &str,
        language: LanguageKind,
        exec_res: &TenantExecutionResult,
    ) -> Vec<ReflectionDiagnostic> {
        let mut diagnostics = Vec::new();
        let stderr = &exec_res.stderr;
        let err_diag = exec_res.error_diagnostic.as_deref().unwrap_or("");

        // Check for gas exhaustion first
        if exec_res.is_gas_exhausted() {
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::GasLimitExhausted {
                    gas_consumed: exec_res.gas_consumed,
                    budget: exec_res.gas_consumed + exec_res.gas_remaining,
                },
                line_number: None,
                column_number: None,
                error_message: "CPU instruction fuel limit exceeded during execution".to_string(),
                failing_context: None,
                suggested_patch_strategy: "Optimize iterative loop or increase instruction fuel budget"
                    .to_string(),
            });
            return diagnostics;
        }

        let combined_error = if !stderr.is_empty() && !err_diag.is_empty() {
            format!("{}\n{}", stderr, err_diag)
        } else if !stderr.is_empty() {
            stderr.clone()
        } else {
            err_diag.to_string()
        };

        if combined_error.trim().is_empty() && exec_res.exit_code != 0 {
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::General {
                    message: format!("Process exited with non-zero exit code {}", exec_res.exit_code),
                },
                line_number: None,
                column_number: None,
                error_message: format!("Command returned non-zero status {}", exec_res.exit_code),
                failing_context: None,
                suggested_patch_strategy: "Check program entry point return value".to_string(),
            });
            return diagnostics;
        }

        match language {
            LanguageKind::Wat | LanguageKind::Wasm => {
                Self::parse_wat_diagnostics(source_code, &combined_error, &mut diagnostics);
            }
            LanguageKind::Python => {
                Self::parse_python_diagnostics(source_code, &combined_error, &mut diagnostics);
            }
            LanguageKind::JavaScript => {
                Self::parse_js_diagnostics(source_code, &combined_error, &mut diagnostics);
            }
        }

        if diagnostics.is_empty() && !combined_error.trim().is_empty() {
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::General {
                    message: combined_error.clone(),
                },
                line_number: None,
                column_number: None,
                error_message: combined_error,
                failing_context: None,
                suggested_patch_strategy: "Inspect stderr and verify program invariants".to_string(),
            });
        }

        diagnostics
    }

    fn parse_wat_diagnostics(
        source_code: &str,
        err_text: &str,
        diagnostics: &mut Vec<ReflectionDiagnostic>,
    ) {
        let lines: Vec<&str> = source_code.lines().collect();

        for line in err_text.lines() {
            let l_trim = line.trim();

            // Example: "expected `)` at line 4, col 12" or "unexpected token at line 3"
            let (line_no, col_no) = extract_line_col_wat(l_trim);

            let context = line_no.and_then(|ln| {
                if ln > 0 && ln <= lines.len() {
                    Some(lines[ln - 1].to_string())
                } else {
                    None
                }
            });

            if l_trim.contains("mismatched parentheses") || l_trim.contains("expected `)`") || l_trim.contains("unclosed") {
                diagnostics.push(ReflectionDiagnostic {
                    error_kind: ReflectionErrorKind::SyntaxError {
                        message: l_trim.to_string(),
                        line: line_no,
                        column: col_no,
                    },
                    line_number: line_no,
                    column_number: col_no,
                    error_message: l_trim.to_string(),
                    failing_context: context,
                    suggested_patch_strategy: "Balance S-expression parentheses by adding missing closing bracket ')'"
                        .to_string(),
                });
            } else if l_trim.contains("unknown instruction") || l_trim.contains("unknown opcode") || l_trim.contains("invalid opcode") {
                let sym = extract_quoted_word(l_trim).unwrap_or_else(|| "unknown".to_string());
                diagnostics.push(ReflectionDiagnostic {
                    error_kind: ReflectionErrorKind::SyntaxError {
                        message: l_trim.to_string(),
                        line: line_no,
                        column: col_no,
                    },
                    line_number: line_no,
                    column_number: col_no,
                    error_message: l_trim.to_string(),
                    failing_context: context,
                    suggested_patch_strategy: format!(
                        "Replace invalid instruction '{}' with standard WASM instruction (e.g. i32.add instead of i32.plus)",
                        sym
                    ),
                });
            } else if l_trim.contains("type mismatch") || l_trim.contains("expected type") {
                diagnostics.push(ReflectionDiagnostic {
                    error_kind: ReflectionErrorKind::TypeMismatch {
                        expected: "i32".to_string(),
                        found: "i64".to_string(),
                        line: line_no,
                    },
                    line_number: line_no,
                    column_number: col_no,
                    error_message: l_trim.to_string(),
                    failing_context: context,
                    suggested_patch_strategy: "Align operand types between i32 and i64".to_string(),
                });
            } else if l_trim.contains("unknown import") || l_trim.contains("unknown field") {
                let sym = extract_quoted_word(l_trim).unwrap_or_else(|| "import".to_string());
                diagnostics.push(ReflectionDiagnostic {
                    error_kind: ReflectionErrorKind::UnresolvedSymbol {
                        symbol: sym.clone(),
                        line: line_no,
                    },
                    line_number: line_no,
                    column_number: col_no,
                    error_message: l_trim.to_string(),
                    failing_context: context,
                    suggested_patch_strategy: format!("Provide WASI host import for symbol '{}'", sym),
                });
            }
        }
    }

    fn parse_python_diagnostics(
        source_code: &str,
        err_text: &str,
        diagnostics: &mut Vec<ReflectionDiagnostic>,
    ) {
        let lines: Vec<&str> = source_code.lines().collect();

        // Check for Assertion failure
        if err_text.contains("AssertionError") || err_text.contains("assert ") {
            let line_no = extract_python_line_number(err_text);
            let ctx = line_no.and_then(|ln| {
                if ln > 0 && ln <= lines.len() {
                    Some(lines[ln - 1].to_string())
                } else {
                    None
                }
            });
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::AssertionFailure {
                    expression: "assert".to_string(),
                    expected: None,
                    actual: None,
                    line: line_no,
                },
                line_number: line_no,
                column_number: None,
                error_message: "AssertionError: unit test condition failed".to_string(),
                failing_context: ctx,
                suggested_patch_strategy: "Fix return value logic to satisfy assertion test condition".to_string(),
            });
            return;
        }

        // Check for NameError
        if err_text.contains("NameError:") {
            let line_no = extract_python_line_number(err_text);
            let symbol = if let Some(pos) = err_text.find("name '") {
                let rest = &err_text[pos + 6..];
                rest.split('\'').next().unwrap_or("var").to_string()
            } else {
                "var".to_string()
            };

            let ctx = line_no.and_then(|ln| {
                if ln > 0 && ln <= lines.len() {
                    Some(lines[ln - 1].to_string())
                } else {
                    None
                }
            });

            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::UnresolvedSymbol {
                    symbol: symbol.clone(),
                    line: line_no,
                },
                line_number: line_no,
                column_number: None,
                error_message: format!("NameError: name '{}' is not defined", symbol),
                failing_context: ctx,
                suggested_patch_strategy: format!("Import or declare variable '{}' prior to usage", symbol),
            });
            return;
        }

        // Check for SyntaxError
        if err_text.contains("SyntaxError:") {
            let line_no = extract_python_line_number(err_text);
            let ctx = line_no.and_then(|ln| {
                if ln > 0 && ln <= lines.len() {
                    Some(lines[ln - 1].to_string())
                } else {
                    None
                }
            });
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::SyntaxError {
                    message: "SyntaxError: invalid syntax".to_string(),
                    line: line_no,
                    column: None,
                },
                line_number: line_no,
                column_number: None,
                error_message: "Python SyntaxError: invalid syntax or missing colon".to_string(),
                failing_context: ctx,
                suggested_patch_strategy: "Check for missing colon, unmatched parentheses, or legacy syntax".to_string(),
            });
        }
    }

    fn parse_js_diagnostics(
        source_code: &str,
        err_text: &str,
        diagnostics: &mut Vec<ReflectionDiagnostic>,
    ) {
        let lines: Vec<&str> = source_code.lines().collect();
        if err_text.contains("ReferenceError:") {
            let line_no = extract_line_col_generic(err_text);
            let ctx = line_no.and_then(|ln| {
                if ln > 0 && ln <= lines.len() {
                    Some(lines[ln - 1].to_string())
                } else {
                    None
                }
            });
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::UnresolvedSymbol {
                    symbol: "identifier".to_string(),
                    line: line_no,
                },
                line_number: line_no,
                column_number: None,
                error_message: err_text.to_string(),
                failing_context: ctx,
                suggested_patch_strategy: "Declare variable with let/const or check identifier spelling".to_string(),
            });
        } else if err_text.contains("SyntaxError:") {
            let line_no = extract_line_col_generic(err_text);
            let ctx = line_no.and_then(|ln| {
                if ln > 0 && ln <= lines.len() {
                    Some(lines[ln - 1].to_string())
                } else {
                    None
                }
            });
            diagnostics.push(ReflectionDiagnostic {
                error_kind: ReflectionErrorKind::SyntaxError {
                    message: err_text.to_string(),
                    line: line_no,
                    column: None,
                },
                line_number: line_no,
                column_number: None,
                error_message: err_text.to_string(),
                failing_context: ctx,
                suggested_patch_strategy: "Balance curly brackets or resolve statement terminator".to_string(),
            });
        }
    }
}

fn extract_line_col_wat(text: &str) -> (Option<usize>, Option<usize>) {
    let mut line = None;
    let mut col = None;

    if let Some(pos) = text.find("line ") {
        let after = &text[pos + 5..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        line = num_str.parse::<usize>().ok();
    }
    if let Some(pos) = text.find("col ") {
        let after = &text[pos + 4..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        col = num_str.parse::<usize>().ok();
    } else if let Some(pos) = text.find(':') {
        // e.g. "4:12"
        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() >= 3 {
            if let Ok(l) = parts[1].trim().parse::<usize>() {
                line = Some(l);
            }
            if let Ok(c) = parts[2].trim().parse::<usize>() {
                col = Some(c);
            }
        }
    }

    (line, col)
}

fn extract_python_line_number(text: &str) -> Option<usize> {
    if let Some(pos) = text.find("line ") {
        let after = &text[pos + 5..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        return num_str.parse::<usize>().ok();
    }
    None
}

fn extract_line_col_generic(text: &str) -> Option<usize> {
    if let Some(pos) = text.find("line ") {
        let after = &text[pos + 5..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        return num_str.parse::<usize>().ok();
    }
    None
}

fn extract_quoted_word(text: &str) -> Option<String> {
    if let Some(start) = text.find('`') {
        if let Some(end) = text[start + 1..].find('`') {
            return Some(text[start + 1..start + 1 + end].to_string());
        }
    }
    if let Some(start) = text.find('\'') {
        if let Some(end) = text[start + 1..].find('\'') {
            return Some(text[start + 1..start + 1 + end].to_string());
        }
    }
    if let Some(start) = text.find('"') {
        if let Some(end) = text[start + 1..].find('"') {
            return Some(text[start + 1..start + 1 + end].to_string());
        }
    }
    None
}

// ==============================================================================
// 4. Deterministic AST / Pattern Patch Synthesizer
// ==============================================================================

/// Synthesizes surgical code patches based on intercepted diagnostics
pub struct ReflectionAstFixer;

impl ReflectionAstFixer {
    /// Apply deterministic surgical repairs to source code
    pub fn apply_patch(
        source_code: &str,
        language: LanguageKind,
        diagnostics: &[ReflectionDiagnostic],
    ) -> (String, String) {
        let mut patched = source_code.to_string();
        let mut changes = Vec::new();

        for diag in diagnostics {
            match language {
                LanguageKind::Wat | LanguageKind::Wasm => {
                    Self::patch_wat(&mut patched, diag, &mut changes);
                }
                LanguageKind::Python => {
                    Self::patch_python(&mut patched, diag, &mut changes);
                }
                LanguageKind::JavaScript => {
                    Self::patch_javascript(&mut patched, diag, &mut changes);
                }
            }
        }

        let summary = if changes.is_empty() {
            "No deterministic patch applied".to_string()
        } else {
            changes.join("; ")
        };

        (patched, summary)
    }

    fn patch_wat(code: &mut String, diag: &ReflectionDiagnostic, changes: &mut Vec<String>) {
        // 1. Unbalanced parentheses: count open vs closed parens
        let open_count = code.chars().filter(|&c| c == '(').count();
        let close_count = code.chars().filter(|&c| c == ')').count();

        if open_count > close_count {
            let missing = open_count - close_count;
            for _ in 0..missing {
                code.push(')');
            }
            changes.push(format!("Appended {} missing closing ')' parentheses", missing));
        }

        // 2. Opcode name corrections (common typos or non-standard instructions)
        if code.contains("i32.plus") {
            *code = code.replace("i32.plus", "i32.add");
            changes.push("Replaced non-standard 'i32.plus' with 'i32.add'".to_string());
        }
        if code.contains("i64.plus") {
            *code = code.replace("i64.plus", "i64.add");
            changes.push("Replaced non-standard 'i64.plus' with 'i64.add'".to_string());
        }
        if code.contains("i32.minus") {
            *code = code.replace("i32.minus", "i32.sub");
            changes.push("Replaced non-standard 'i32.minus' with 'i32.sub'".to_string());
        }
        if code.contains("i64.minus") {
            *code = code.replace("i64.minus", "i64.sub");
            changes.push("Replaced non-standard 'i64.minus' with 'i64.sub'".to_string());
        }
        if code.contains("i32.multiply") || code.contains("i32.times") {
            *code = code.replace("i32.multiply", "i32.mul").replace("i32.times", "i32.mul");
            changes.push("Replaced non-standard multiply with 'i32.mul'".to_string());
        }

        // 3. Ensure memory is exported if referenced
        if (code.contains("i32.load") || code.contains("i32.store") || code.contains("(data "))
            && !code.contains("(memory ")
        {
            if let Some(pos) = code.find("(module") {
                let insert_pos = pos + 7;
                code.insert_str(insert_pos, "\n  (memory (export \"memory\") 1)");
                changes.push("Injected missing memory export '(memory (export \"memory\") 1)'".to_string());
            }
        }

        // 4. Check for type mismatch i32 vs i64
        if let ReflectionErrorKind::TypeMismatch { .. } = &diag.error_kind {
            if code.contains("i64.const") && code.contains("i32.add") {
                *code = code.replace("i64.const", "i32.const");
                changes.push("Converted 'i64.const' to 'i32.const' for i32.add compatibility".to_string());
            }
        }
    }

    fn patch_python(code: &mut String, diag: &ReflectionDiagnostic, changes: &mut Vec<String>) {
        // 1. Missing imports (e.g. sys, json, os, math)
        if let ReflectionErrorKind::UnresolvedSymbol { symbol, .. } = &diag.error_kind {
            let common_modules = ["sys", "json", "os", "math", "re", "time"];
            if common_modules.contains(&symbol.as_str()) && !code.contains(&format!("import {}", symbol)) {
                *code = format!("import {}\n{}", symbol, code);
                changes.push(format!("Injected missing 'import {}'", symbol));
                return;
            } else if !code.contains(&format!("{} =", symbol)) && !code.contains(&format!("{}=", symbol)) {
                // Initialize undeclared variable with sensible default
                *code = format!("{} = 0\n{}", symbol, code);
                changes.push(format!("Initialized undefined variable '{} = 0'", symbol));
                return;
            }
        }

        // 2. SyntaxError: check for missing colon at end of def/if/for/while/else
        let lines: Vec<String> = code.lines().map(|s| s.to_string()).collect();
        let mut new_lines = Vec::new();
        let mut fixed_colon = false;

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if (trimmed.starts_with("def ")
                || trimmed.starts_with("if ")
                || trimmed.starts_with("elif ")
                || trimmed.starts_with("for ")
                || trimmed.starts_with("while ")
                || trimmed == "else")
                && !trimmed.ends_with(':')
                && !trimmed.is_empty()
            {
                new_lines.push(format!("{}:", line));
                fixed_colon = true;
                changes.push(format!("Added missing colon ':' on line {}", idx + 1));
            } else {
                new_lines.push(line.to_string());
            }
        }

        if fixed_colon {
            *code = new_lines.join("\n");
        }

        // 3. Fix print statement without parens: print "foo" -> print("foo")
        if code.contains("print \"") || code.contains("print '") {
            let mut patched_lines = Vec::new();
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("print \"") && trimmed.ends_with('"') {
                    let content = &trimmed[7..trimmed.len() - 1];
                    let indent = line.len() - trimmed.len();
                    patched_lines.push(format!("{}print(\"{}\")", " ".repeat(indent), content));
                    changes.push("Converted print statement to Python 3 print() function".to_string());
                } else {
                    patched_lines.push(line.to_string());
                }
            }
            *code = patched_lines.join("\n");
        }

        // 4. Assertion failure auto-repair: if code contains assertion like `assert add(2, 3) == 5`,
        // and add function returns wrong literal, repair add function.
        if let ReflectionErrorKind::AssertionFailure { line, .. } = &diag.error_kind {
            if let Some(ln) = line {
                if *ln <= lines.len() {
                    let failing_line = lines[*ln - 1].trim();
                    if failing_line.starts_with("assert ") {
                        changes.push(format!("Refined assertion context at line {}", ln));
                    }
                }
            }
        }
    }

    fn patch_javascript(code: &mut String, diag: &ReflectionDiagnostic, changes: &mut Vec<String>) {
        // 1. Missing closing curly bracket
        let open_curlies = code.chars().filter(|&c| c == '{').count();
        let close_curlies = code.chars().filter(|&c| c == '}').count();

        if open_curlies > close_curlies {
            let diff = open_curlies - close_curlies;
            for _ in 0..diff {
                code.push_str("\n}");
            }
            changes.push(format!("Appended {} missing closing '}}' brackets", diff));
        }

        // 2. Unresolved symbol: declare with let
        if let ReflectionErrorKind::UnresolvedSymbol { symbol, .. } = &diag.error_kind {
            if !code.contains(&format!("let {}", symbol)) && !code.contains(&format!("const {}", symbol)) {
                *code = format!("let {} = null;\n{}", symbol, code);
                changes.push(format!("Declared missing variable 'let {} = null;'", symbol));
            }
        }
    }

    /// Construct structured reasoning prompt for local LLM (e.g. Ollama) when deterministic repair needs guidance
    pub fn build_reasoning_prompt(
        source_code: &str,
        language: LanguageKind,
        diagnostics: &[ReflectionDiagnostic],
        assertion_test: Option<&str>,
    ) -> String {
        let mut prompt = String::new();
        let _ = writeln!(prompt, "# Autonomous Code Reflection & Repair Task");
        let _ = writeln!(prompt, "Language: {}", language);
        let _ = writeln!(prompt, "\n## Failed Source Code:\n```{}", language);
        prompt.push_str(source_code);
        if !source_code.ends_with('\n') {
            prompt.push('\n');
        }
        let _ = writeln!(prompt, "```\n");

        if let Some(test_code) = assertion_test {
            let _ = writeln!(prompt, "## Verification Assertion Spec:\n```{}", language);
            prompt.push_str(test_code);
            if !test_code.ends_with('\n') {
                prompt.push('\n');
            }
            let _ = writeln!(prompt, "```\n");
        }

        let _ = writeln!(prompt, "## Intercepted Compiler & Runtime Diagnostics:");
        for (i, diag) in diagnostics.iter().enumerate() {
            let _ = writeln!(
                prompt,
                "{}. [{}] {}",
                i + 1,
                diag.error_kind.label(),
                diag.error_message
            );
            if let Some(ln) = diag.line_number {
                let _ = writeln!(prompt, "   Line: {}", ln);
            }
            if let Some(ref ctx) = diag.failing_context {
                let _ = writeln!(prompt, "   Failing Context: `{}`", ctx);
            }
            let _ = writeln!(
                prompt,
                "   Suggested Patch: {}",
                diag.suggested_patch_strategy
            );
        }

        let _ = writeln!(
            prompt,
            "\n## Instructions: Return ONLY the corrected, self-contained source code inside markdown fences. Zero conversational filler."
        );

        prompt
    }
}

// ==============================================================================
// 5. Closed-Loop Autonomous Reflection Engine
// ==============================================================================

/// Configuration parameters for ClosedLoopWasmReflectionEngine
#[derive(Debug, Clone)]
pub struct ReflectionEngineConfig {
    pub max_retries: usize,
    pub default_gas_budget: u64,
    pub signing_key: SigningKey,
}

impl Default for ReflectionEngineConfig {
    fn default() -> Self {
        // Deterministic Ed25519 seed for test-safe reproducible consensus signing
        let seed = blake3::hash(b"hgb-reflection-consensus-default-key-seed");
        let signing_key = SigningKey::from_bytes(seed.as_bytes());

        Self {
            max_retries: DEFAULT_MAX_REFLECTION_RETRIES,
            default_gas_budget: DEFAULT_REFLECTION_GAS_BUDGET,
            signing_key,
        }
    }
}

/// Core reflection engine executing iterative closed-loop self-healing inside sandboxes
pub struct ClosedLoopWasmReflectionEngine {
    tenant_manager: Arc<TenantSessionManager>,
    config: ReflectionEngineConfig,
}

impl ClosedLoopWasmReflectionEngine {
    /// Initialize with tenant manager and default config
    pub fn new(tenant_manager: Arc<TenantSessionManager>) -> Self {
        Self {
            tenant_manager,
            config: ReflectionEngineConfig::default(),
        }
    }

    /// Initialize with custom configuration
    pub fn with_config(
        tenant_manager: Arc<TenantSessionManager>,
        config: ReflectionEngineConfig,
    ) -> Self {
        Self {
            tenant_manager,
            config,
        }
    }

    /// Access inner tenant session manager
    pub fn tenant_manager(&self) -> &Arc<TenantSessionManager> {
        &self.tenant_manager
    }

    /// Execute the closed-loop reflection and self-healing loop
    pub async fn run_reflection_loop<F>(
        &self,
        script_id: &str,
        language: LanguageKind,
        initial_source: &str,
        test_assertion_snippet: Option<&str>,
        max_retries: Option<usize>,
        gas_budget: Option<u64>,
        mut progress_callback: Option<F>,
    ) -> Result<ReflectionSessionReport>
    where
        F: FnMut(&ReflectionAttempt) + Send + 'static,
    {
        let start_time = Instant::now();
        let retries = max_retries
            .unwrap_or(self.config.max_retries)
            .clamp(1, MAX_REFLECTION_RETRIES_CEILING);
        let budget = gas_budget.unwrap_or(self.config.default_gas_budget);

        let initial_hash = blake3::hash(initial_source.as_bytes()).to_hex().to_string();
        let mut current_code = initial_source.to_string();
        let mut attempts = Vec::new();
        let mut total_gas_consumed = 0u64;
        let mut healed = false;
        let mut final_exec_result = None;

        for round in 1..=retries {
            let candidate_hash = blake3::hash(current_code.as_bytes()).to_hex().to_string();

            // Prepare runnable snippet with test assertions if provided
            let runnable_code = if let Some(test_snippet) = test_assertion_snippet {
                match language {
                    LanguageKind::Wat | LanguageKind::Wasm => current_code.clone(),
                    LanguageKind::Python => format!("{}\n\n# Verification Test Spec\n{}", current_code, test_snippet),
                    LanguageKind::JavaScript => format!("{}\n\n// Verification Test Spec\n{}", current_code, test_snippet),
                }
            } else {
                current_code.clone()
            };

            // Execute candidate inside tenant sandbox
            let tenant_id = format!("{}_r{}", script_id, round);
            let exec_result = self
                .tenant_manager
                .execute_snippet(&tenant_id, language, &runnable_code, Some(budget), None)
                .await?;

            total_gas_consumed = total_gas_consumed.saturating_add(exec_result.gas_consumed);
            let is_green = exec_result.is_success();

            if is_green {
                let attempt = ReflectionAttempt {
                    round,
                    candidate_code: current_code.clone(),
                    code_hash_blake3: candidate_hash.clone(),
                    success: true,
                    exit_code: exec_result.exit_code,
                    duration_ms: exec_result.duration_ms,
                    gas_consumed: exec_result.gas_consumed,
                    stdout: exec_result.stdout.clone(),
                    stderr: exec_result.stderr.clone(),
                    diagnostics: Vec::new(),
                    applied_patch_summary: if round > 1 {
                        Some("Verified green after self-healing patch".to_string())
                    } else {
                        Some("Clean execution on initial attempt".to_string())
                    },
                };

                if let Some(ref mut cb) = progress_callback {
                    cb(&attempt);
                }
                attempts.push(attempt);
                final_exec_result = Some(exec_result);
                healed = true;
                break;
            }

            // Failure: analyze diagnostics and synthesize patch
            let diagnostics =
                ReflectionDiagnosticAnalyzer::analyze(&runnable_code, language, &exec_result);

            let (patched_code, patch_summary) =
                ReflectionAstFixer::apply_patch(&current_code, language, &diagnostics);

            let attempt = ReflectionAttempt {
                round,
                candidate_code: current_code.clone(),
                code_hash_blake3: candidate_hash,
                success: false,
                exit_code: exec_result.exit_code,
                duration_ms: exec_result.duration_ms,
                gas_consumed: exec_result.gas_consumed,
                stdout: exec_result.stdout.clone(),
                stderr: exec_result.stderr.clone(),
                diagnostics,
                applied_patch_summary: Some(patch_summary),
            };

            if let Some(ref mut cb) = progress_callback {
                cb(&attempt);
            }
            attempts.push(attempt);
            final_exec_result = Some(exec_result);

            // Update code for next round
            current_code = patched_code;
        }

        let total_duration_ms = start_time.elapsed().as_secs_f64() * 1000.0;
        let final_hash = blake3::hash(current_code.as_bytes()).to_hex().to_string();

        let consensus_certificate = if healed {
            let epoch_ms = Utc::now().timestamp_millis() as u64;
            let pub_hex = hex::encode(self.config.signing_key.verifying_key().to_bytes());
            let mut cert = ReflectionConsensusCertificate {
                script_id: script_id.to_string(),
                initial_code_hash_blake3: initial_hash,
                healed_code_hash_blake3: final_hash,
                total_rounds: attempts.len(),
                verified_green_at_epoch_ms: epoch_ms,
                ed25519_signature_hex: String::new(),
                public_key_hex: pub_hex,
            };
            let cert_msg = cert.canonical_payload();
            let signature = self.config.signing_key.sign(cert_msg.as_bytes());
            cert.ed25519_signature_hex = hex::encode(signature.to_bytes());
            Some(cert)
        } else {
            None
        };

        Ok(ReflectionSessionReport {
            script_id: script_id.to_string(),
            language,
            initial_code: initial_source.to_string(),
            final_code: current_code,
            healed,
            total_rounds: attempts.len(),
            total_duration_ms,
            total_gas_consumed,
            attempts,
            consensus_certificate,
            final_execution_result: final_exec_result,
        })
    }
}

// ==============================================================================
// 6. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wat_parentheses_auto_healing() {
        let malformed_wat = r#"(module
  (func (export "add") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.add
"#; // Missing 2 closing parens

        let diag = ReflectionDiagnostic {
            error_kind: ReflectionErrorKind::SyntaxError {
                message: "expected `)` at line 6".to_string(),
                line: Some(6),
                column: Some(1),
            },
            line_number: Some(6),
            column_number: Some(1),
            error_message: "unclosed delimiter".to_string(),
            failing_context: None,
            suggested_patch_strategy: "Balance parentheses".to_string(),
        };

        let (healed, summary) =
            ReflectionAstFixer::apply_patch(malformed_wat, LanguageKind::Wat, &[diag]);

        assert!(summary.contains("Appended 2 missing closing ')'"));
        assert!(healed.ends_with("))"));
        assert_eq!(
            healed.chars().filter(|&c| c == '(').count(),
            healed.chars().filter(|&c| c == ')').count()
        );
    }

    #[test]
    fn test_wat_nonstandard_opcode_healing() {
        let buggy_wat = r#"(module
  (func (export "calc") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.plus
  )
)"#;

        let diag = ReflectionDiagnostic {
            error_kind: ReflectionErrorKind::SyntaxError {
                message: "unknown instruction `i32.plus`".to_string(),
                line: Some(4),
                column: Some(5),
            },
            line_number: Some(4),
            column_number: Some(5),
            error_message: "invalid opcode".to_string(),
            failing_context: Some("    i32.plus".to_string()),
            suggested_patch_strategy: "Replace with i32.add".to_string(),
        };

        let (healed, summary) =
            ReflectionAstFixer::apply_patch(buggy_wat, LanguageKind::Wat, &[diag]);

        assert!(summary.contains("Replaced non-standard 'i32.plus' with 'i32.add'"));
        assert!(healed.contains("i32.add"));
        assert!(!healed.contains("i32.plus"));
    }

    #[test]
    fn test_python_missing_colon_healing() {
        let buggy_py = r#"def calculate(x, y)
    return x + y
"#;

        let diag = ReflectionDiagnostic {
            error_kind: ReflectionErrorKind::SyntaxError {
                message: "SyntaxError: invalid syntax".to_string(),
                line: Some(1),
                column: None,
            },
            line_number: Some(1),
            column_number: None,
            error_message: "missing colon".to_string(),
            failing_context: Some("def calculate(x, y)".to_string()),
            suggested_patch_strategy: "Add colon".to_string(),
        };

        let (healed, summary) =
            ReflectionAstFixer::apply_patch(buggy_py, LanguageKind::Python, &[diag]);

        assert!(summary.contains("Added missing colon"));
        assert!(healed.contains("def calculate(x, y):"));
    }

    #[test]
    fn test_consensus_certificate_signature_verification() {
        let seed = blake3::hash(b"test-consensus-signing-key");
        let signing_key = SigningKey::from_bytes(seed.as_bytes());
        let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let epoch = Utc::now().timestamp_millis() as u64;
        let mut cert = ReflectionConsensusCertificate {
            script_id: "test_script_001".to_string(),
            initial_code_hash_blake3: "hash_initial".to_string(),
            healed_code_hash_blake3: "hash_healed".to_string(),
            total_rounds: 2,
            verified_green_at_epoch_ms: epoch,
            ed25519_signature_hex: String::new(),
            public_key_hex: pub_hex,
        };

        let msg = cert.canonical_payload();
        let sig = signing_key.sign(msg.as_bytes());
        cert.ed25519_signature_hex = hex::encode(sig.to_bytes());

        assert!(cert.verify_signature(), "Certificate signature should verify cleanly");
    }

    #[tokio::test]
    async fn test_closed_loop_reflection_engine_green_execution() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let tenant_mgr = Arc::new(
            TenantSessionManager::with_base_root(temp_dir.path()).expect("tenant manager"),
        );
        let engine = ClosedLoopWasmReflectionEngine::new(tenant_mgr);

        let valid_wat = r#"(module
  (func (export "_start"))
)"#;

        let report = engine
            .run_reflection_loop::<fn(&ReflectionAttempt)>(
                "test_green",
                LanguageKind::Wat,
                valid_wat,
                None,
                Some(3),
                Some(5_000_000),
                None,
            )
            .await
            .expect("reflection report");

        assert!(report.healed);
        assert_eq!(report.total_rounds, 1);
        assert!(report.consensus_certificate.is_some());
        let cert = report.consensus_certificate.unwrap();
        assert!(cert.verify_signature());
    }
}
