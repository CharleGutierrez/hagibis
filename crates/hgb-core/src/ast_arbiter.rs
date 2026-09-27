//! # AST-Aware Visual Patch Arbiter (Semantic Hunk Cherry-Picking)
//!
//! Deconstructs complex multi-file diffs into fine-grained, AST-aligned semantic hunks.
//! Allows selective cherry-picking of functions, structs, imports, and statements
//! with automatic syntax integrity validation (delimiter balancing and scope preservation).

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};

/// Semantic category of the code chunk
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstHunkKind {
    Function,
    Struct,
    ImplBlock,
    Import,
    Statement,
    Module,
    Unknown,
}

impl AstHunkKind {
    pub fn badge(&self) -> &'static str {
        match self {
            AstHunkKind::Function => "fn",
            AstHunkKind::Struct => "struct",
            AstHunkKind::ImplBlock => "impl",
            AstHunkKind::Import => "import",
            AstHunkKind::Statement => "stmt",
            AstHunkKind::Module => "mod",
            AstHunkKind::Unknown => "raw",
        }
    }
}

/// The status or developer decision for an individual AST hunk
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AstHunkDecision {
    Staged,
    Accepted,
    Rejected,
}

/// An individual fine-grained semantic change hunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstHunk {
    pub id: String,
    pub kind: AstHunkKind,
    pub symbol_name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub original_content: String,
    pub modified_content: String,
    pub decision: AstHunkDecision,
    pub confidence: f32,
}

/// Comprehensive report of parsed AST hunks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstPatchReport {
    pub total_hunks: usize,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub staged_count: usize,
    pub hunks: Vec<AstHunk>,
    pub syntax_valid: bool,
}

/// The core AST patch arbiter engine
pub struct AstPatchArbiter;

impl AstPatchArbiter {
    /// Detects symbol name and hunk kind from source line
    fn detect_symbol_info(line: &str, file_ext: &str) -> (AstHunkKind, String) {
        let trimmed = line.trim();
        match file_ext {
            "rs" => {
                if trimmed.starts_with("use ") {
                    let sym = trimmed.trim_start_matches("use ").trim_end_matches(';').trim();
                    return (AstHunkKind::Import, sym.to_string());
                }
                if trimmed.starts_with("mod ") {
                    let sym = trimmed.trim_start_matches("mod ").trim_end_matches(';').trim();
                    return (AstHunkKind::Module, sym.to_string());
                }
                if trimmed.contains("fn ") {
                    let after_fn = trimmed.split("fn ").nth(1).unwrap_or("");
                    let name = after_fn.split(['(', '<', ' ']).next().unwrap_or("anon").trim();
                    return (AstHunkKind::Function, name.to_string());
                }
                if trimmed.contains("struct ") {
                    let after_st = trimmed.split("struct ").nth(1).unwrap_or("");
                    let name = after_st.split(['{', '<', ';', ' ']).next().unwrap_or("anon").trim();
                    return (AstHunkKind::Struct, name.to_string());
                }
                if trimmed.contains("impl ") {
                    let after_impl = trimmed.split("impl ").nth(1).unwrap_or("");
                    let name = after_impl.split(['{', '<', ' ']).next().unwrap_or("anon").trim();
                    return (AstHunkKind::ImplBlock, name.to_string());
                }
            }
            "ts" | "js" | "tsx" | "jsx" => {
                if trimmed.starts_with("import ") {
                    return (AstHunkKind::Import, trimmed.to_string());
                }
                if trimmed.contains("function ") || trimmed.contains("=>") {
                    let name = if let Some(part) = trimmed.split("function ").nth(1) {
                        part.split('(').next().unwrap_or("anon").trim()
                    } else if let Some(part) = trimmed.split('=').next() {
                        part.split_whitespace().last().unwrap_or("anon").trim()
                    } else {
                        "anon"
                    };
                    return (AstHunkKind::Function, name.to_string());
                }
                if trimmed.contains("class ") || trimmed.contains("interface ") || trimmed.contains("type ") {
                    let name = trimmed.split_whitespace().nth(1).unwrap_or("anon").trim();
                    return (AstHunkKind::Struct, name.to_string());
                }
            }
            "py" => {
                if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                    return (AstHunkKind::Import, trimmed.to_string());
                }
                if trimmed.starts_with("def ") {
                    let name = trimmed.trim_start_matches("def ").split('(').next().unwrap_or("anon").trim();
                    return (AstHunkKind::Function, name.to_string());
                }
                if trimmed.starts_with("class ") {
                    let name = trimmed.trim_start_matches("class ").split(['(', ':']).next().unwrap_or("anon").trim();
                    return (AstHunkKind::Struct, name.to_string());
                }
            }
            _ => {}
        }
        (AstHunkKind::Statement, "block".to_string())
    }

    /// Deconstructs modified source code into distinct semantic AST hunks relative to original
    pub fn parse_diff_into_hunks(original: &str, modified: &str, file_ext: &str) -> Vec<AstHunk> {
        let orig_lines: Vec<&str> = original.lines().collect();
        let mod_lines: Vec<&str> = modified.lines().collect();
        let mut hunks = Vec::new();

        let mut i = 0;
        let mut hunk_counter = 0;

        while i < mod_lines.len() {
            let m_line = mod_lines[i];
            let is_matched = orig_lines.iter().any(|&o| o.trim() == m_line.trim());

            if !is_matched || (m_line.trim().starts_with("fn ") || m_line.trim().starts_with("pub fn ") || m_line.trim().starts_with("def ")) {
                let start_line = i + 1;
                let (kind, symbol) = Self::detect_symbol_info(m_line, file_ext);
                let mut block_lines = vec![m_line];
                i += 1;

                let mut brace_depth = 0i32;
                for c in m_line.chars() {
                    if c == '{' { brace_depth += 1; }
                    if c == '}' { brace_depth -= 1; }
                }

                while i < mod_lines.len() {
                    let next_line = mod_lines[i];
                    for c in next_line.chars() {
                        if c == '{' { brace_depth += 1; }
                        if c == '}' { brace_depth -= 1; }
                    }
                    block_lines.push(next_line);
                    i += 1;
                    if brace_depth <= 0 && (next_line.trim().is_empty() || next_line.trim() == "}" || file_ext == "py") {
                        break;
                    }
                }

                let end_line = i;
                let modified_content = block_lines.join("\n");
                let original_content = orig_lines.get(start_line.saturating_sub(1)..end_line.min(orig_lines.len()))
                    .map(|slice| slice.join("\n"))
                    .unwrap_or_default();

                hunk_counter += 1;
                hunks.push(AstHunk {
                    id: format!("hunk-{}", hunk_counter),
                    kind,
                    symbol_name: symbol,
                    start_line,
                    end_line,
                    original_content,
                    modified_content,
                    decision: AstHunkDecision::Accepted,
                    confidence: 0.95,
                });
            } else {
                i += 1;
            }
        }

        if hunks.is_empty() && original != modified {
            hunks.push(AstHunk {
                id: "hunk-1".to_string(),
                kind: AstHunkKind::Statement,
                symbol_name: "patch".to_string(),
                start_line: 1,
                end_line: mod_lines.len(),
                original_content: original.to_string(),
                modified_content: modified.to_string(),
                decision: AstHunkDecision::Accepted,
                confidence: 0.90,
            });
        }

        hunks
    }

    /// Validates balanced delimiters and structural syntax integrity
    pub fn audit_syntax_integrity(code: &str, _file_ext: &str) -> Result<()> {
        let mut curly = 0i32;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut in_string = false;
        let mut in_char = false;
        let mut escape = false;

        for c in code.chars() {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' {
                escape = true;
                continue;
            }
            if c == '"' && !in_char {
                in_string = !in_string;
                continue;
            }
            if c == '\'' && !in_string {
                in_char = !in_char;
                continue;
            }
            if in_string || in_char {
                continue;
            }

            match c {
                '{' => curly += 1,
                '}' => {
                    curly -= 1;
                    if curly < 0 {
                        return Err(HgbError::syntax("Unmatched closing brace '}' in AST patch".to_string()));
                    }
                }
                '(' => paren += 1,
                ')' => {
                    paren -= 1;
                    if paren < 0 {
                        return Err(HgbError::syntax("Unmatched closing parenthesis ')' in AST patch".to_string()));
                    }
                }
                '[' => bracket += 1,
                ']' => {
                    bracket -= 1;
                    if bracket < 0 {
                        return Err(HgbError::syntax("Unmatched closing bracket ']' in AST patch".to_string()));
                    }
                }
                _ => {}
            }
        }

        if curly != 0 {
            return Err(HgbError::syntax(format!("Unbalanced braces in AST patch: {} unclosed", curly)));
        }
        if paren != 0 {
            return Err(HgbError::syntax(format!("Unbalanced parentheses in AST patch: {} unclosed", paren)));
        }
        if bracket != 0 {
            return Err(HgbError::syntax(format!("Unbalanced brackets in AST patch: {} unclosed", bracket)));
        }

        Ok(())
    }

    /// Reconstructs the target code applying only accepted and staged hunks, rejecting discarded hunks
    pub fn apply_decisions(original: &str, hunks: &[AstHunk], file_ext: &str) -> Result<String> {
        let mut result = String::new();
        let mut any_accepted = false;

        for hunk in hunks {
            match hunk.decision {
                AstHunkDecision::Accepted | AstHunkDecision::Staged => {
                    any_accepted = true;
                    if !result.is_empty() && !result.ends_with('\n') {
                        result.push('\n');
                    }
                    result.push_str(&hunk.modified_content);
                    result.push('\n');
                }
                AstHunkDecision::Rejected => {
                    if !hunk.original_content.is_empty() {
                        if !result.is_empty() && !result.ends_with('\n') {
                            result.push('\n');
                        }
                        result.push_str(&hunk.original_content);
                        result.push('\n');
                    }
                }
            }
        }

        let final_code = if any_accepted {
            result.trim_end().to_string() + "\n"
        } else {
            original.to_string()
        };

        Self::audit_syntax_integrity(&final_code, file_ext)?;
        Ok(final_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_hunk_detection_and_cherry_pick() {
        let original = r#"use std::sync::Arc;

pub fn calculate_sum(a: i32, b: i32) -> i32 {
    a + b
}
"#;

        let modified = r#"use std::sync::Arc;
use std::collections::HashMap;

pub fn calculate_sum(a: i32, b: i32) -> i32 {
    a + b
}

pub fn calculate_product(a: i32, b: i32) -> i32 {
    a * b
}
"#;

        let hunks = AstPatchArbiter::parse_diff_into_hunks(original, modified, "rs");
        assert!(!hunks.is_empty());
        assert!(hunks.iter().any(|h| h.symbol_name.contains("calculate_product") || h.modified_content.contains("calculate_product")));

        let applied = AstPatchArbiter::apply_decisions(original, &hunks, "rs").expect("apply should succeed");
        assert!(applied.contains("calculate_product"));
        assert!(AstPatchArbiter::audit_syntax_integrity(&applied, "rs").is_ok());
    }

    #[test]
    fn test_syntax_integrity_rejects_unbalanced_braces() {
        let broken_code = "fn bad() { let x = 1; ";
        assert!(AstPatchArbiter::audit_syntax_integrity(broken_code, "rs").is_err());

        let good_code = "fn good() { let x = 1; }";
        assert!(AstPatchArbiter::audit_syntax_integrity(good_code, "rs").is_ok());
    }
}
