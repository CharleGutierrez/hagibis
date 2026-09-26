use crate::error::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Result containing pruned code and token savings metrics
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrunedAstResult {
    pub pruned_code: String,
    pub original_lines: usize,
    pub pruned_lines: usize,
    pub folded_functions: usize,
    pub reduction_percentage: f64,
}

/// Adaptive KV-Cache & AST Pruner
pub struct AstPruner;

impl AstPruner {
    /// Prune source code by folding non-target function and method bodies
    pub fn prune_source(
        source: &str,
        file_ext: &str,
        focus_line: Option<usize>,
        target_fn: Option<&str>,
    ) -> PrunedAstResult {
        let lines: Vec<&str> = source.lines().collect();
        let original_lines = lines.len();
        if original_lines == 0 {
            return PrunedAstResult {
                pruned_code: String::new(),
                original_lines: 0,
                pruned_lines: 0,
                folded_functions: 0,
                reduction_percentage: 0.0,
            };
        }

        let mut output_lines = Vec::new();
        let mut folded_count = 0;
        let mut i = 0;

        let fn_start_re = match file_ext {
            "rs" => Regex::new(r"^(.*?)(?:pub(?:\([^\)]+\))?\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)").unwrap(),
            "go" => Regex::new(r"^(.*?)func\s+(?:\([^)]+\)\s+)?([A-Za-z0-9_]+)").unwrap(),
            "py" => Regex::new(r"^(\s*)def\s+([A-Za-z0-9_]+)").unwrap(),
            _ => Regex::new(r"^(.*?)(?:export\s+)?(?:async\s+)?(?:function\s+([A-Za-z0-9_]+)|([A-Za-z0-9_]+)\s*\([^)]*\)\s*\{)").unwrap(),
        };

        if file_ext == "py" {
            // Python indentation-based folding
            while i < lines.len() {
                let line = lines[i];
                let line_no = i + 1;
                if let Some(caps) = fn_start_re.captures(line) {
                    let indent_len = caps.get(1).map(|m| m.as_str().len()).unwrap_or(0);
                    let fn_name = caps.get(2).map(|m| m.as_str()).unwrap_or("");

                    // Find end of python function
                    let mut end_line = line_no;
                    for j in (i + 1)..lines.len() {
                        let l = lines[j];
                        let trimmed = l.trim();
                        if trimmed.is_empty() || trimmed.starts_with('#') {
                            continue;
                        }
                        let cur_indent = l.chars().take_while(|c| c.is_whitespace()).count();
                        if cur_indent <= indent_len {
                            break;
                        }
                        end_line = j + 1;
                    }

                    let is_focus = match (focus_line, target_fn) {
                        (Some(fl), _) if fl >= line_no && fl <= end_line => true,
                        (_, Some(tf)) if tf == fn_name => true,
                        _ => false,
                    };

                    let body_lines = end_line.saturating_sub(line_no);
                    if !is_focus && body_lines > 1 {
                        output_lines.push(line.to_string());
                        let indent = " ".repeat(indent_len + 4);
                        output_lines.push(format!("{}# [folded {} lines]", indent, body_lines));
                        folded_count += 1;
                        i = end_line;
                        continue;
                    }
                }
                output_lines.push(line.to_string());
                i += 1;
            }
        } else {
            // Brace-based folding (Rust, TS, JS, Go, etc.)
            while i < lines.len() {
                let line = lines[i];
                let line_no = i + 1;
                let trimmed = line.trim();

                // Skip comments
                if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                    output_lines.push(line.to_string());
                    i += 1;
                    continue;
                }

                if let Some(caps) = fn_start_re.captures(line) {
                    let fn_name = caps.get(2).or_else(|| caps.get(3)).map(|m| m.as_str()).unwrap_or("");

                    // Look ahead to find opening '{' and matching '}'
                    let mut brace_depth: i32 = 0;
                    let mut found_open = false;
                    let mut open_line_idx = i;
                    let mut end_line_idx = i;

                    for j in i..lines.len() {
                        let l = lines[j];
                        for ch in l.chars() {
                            if ch == '{' {
                                if !found_open {
                                    open_line_idx = j;
                                    found_open = true;
                                }
                                brace_depth += 1;
                            } else if ch == '}' {
                                brace_depth -= 1;
                            }
                        }
                        if found_open && brace_depth <= 0 {
                            end_line_idx = j;
                            break;
                        }
                    }

                    let start_no = line_no;
                    let end_no = end_line_idx + 1;

                    let is_focus = match (focus_line, target_fn) {
                        (Some(fl), _) if fl >= start_no && fl <= end_no => true,
                        (_, Some(tf)) if tf == fn_name => true,
                        _ => false,
                    };

                    let body_lines_count = end_no.saturating_sub(start_no);

                    if !is_focus && found_open && body_lines_count > 1 {
                        // Extract signature up to open '{'
                        let mut sig_parts = Vec::new();
                        for k in i..=open_line_idx {
                            sig_parts.push(lines[k]);
                        }
                        let joined_sig = sig_parts.join(" ");
                        let sig_before_brace = if let Some(pos) = joined_sig.find('{') {
                            joined_sig[..pos].trim_end()
                        } else {
                            joined_sig.trim_end()
                        };

                        output_lines.push(format!("{} {{ /* [folded {} lines] */ }}", sig_before_brace, body_lines_count));
                        folded_count += 1;
                        i = end_line_idx + 1;
                        continue;
                    }
                }

                output_lines.push(line.to_string());
                i += 1;
            }
        }

        let pruned_lines = output_lines.len();
        let reduction_percentage = if original_lines > 0 {
            (original_lines.saturating_sub(pruned_lines) as f64 / original_lines as f64) * 100.0
        } else {
            0.0
        };

        PrunedAstResult {
            pruned_code: output_lines.join("\n"),
            original_lines,
            pruned_lines,
            folded_functions: folded_count,
            reduction_percentage,
        }
    }

    /// Prune file on disk
    pub fn prune_file(
        path: &Path,
        focus_line: Option<usize>,
        target_fn: Option<&str>,
        max_tokens: Option<usize>,
    ) -> Result<PrunedAstResult> {
        let content = fs::read_to_string(path)?;
        let file_ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let mut result = Self::prune_source(&content, file_ext, focus_line, target_fn);

        if let Some(max_tok) = max_tokens {
            let max_chars = max_tok * 4;
            if result.pruned_code.len() > max_chars {
                result.pruned_code.truncate(max_chars);
                result.pruned_code.push_str("\n// ... [remaining tokens truncated to fit cache budget]");
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prune_rust_folds_non_target_functions() {
        let source = r#"pub struct Config {
    pub name: String,
}

pub fn helper_one() {
    let mut a = 1;
    a += 2;
    println!("{}", a);
}

pub fn focus_target() {
    let focus = "keep me intact";
    println!("{}", focus);
}

pub fn helper_two() {
    let b = 2;
    let c = b * 3;
    println!("{}", c);
}
"#;

        let result = AstPruner::prune_source(source, "rs", None, Some("focus_target"));
        assert_eq!(result.folded_functions, 2);
        assert!(result.pruned_code.contains("pub fn helper_one() { /* [folded"));
        assert!(result.pruned_code.contains("pub fn helper_two() { /* [folded"));
        assert!(result.pruned_code.contains("keep me intact"));
        assert!(result.reduction_percentage > 25.0);
    }
}
