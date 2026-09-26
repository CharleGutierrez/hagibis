use crate::error::{HgbError, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Historical revision snapshot of a specific function or component symbol
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolRevision {
    pub symbol_name: String,
    pub file_path: String,
    pub timestamp: u64,
    pub signature: String,
    pub body_content: String,
}

/// Granular AST Rewind Timeline tracking per-function revision histories
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AstRewindTimeline {
    // Key: "file_path::symbol_name" -> history of revisions
    revisions: HashMap<String, Vec<SymbolRevision>>,
}

impl AstRewindTimeline {
    pub fn new() -> Self {
        Self {
            revisions: HashMap::new(),
        }
    }

    fn key(file: &str, symbol: &str) -> String {
        format!("{}::{}", file, symbol)
    }

    /// Record a snapshot of a specific function or component
    pub fn record_symbol_snapshot(
        &mut self,
        file: &str,
        symbol: &str,
        signature: &str,
        body: &str,
        timestamp: u64,
    ) {
        let k = Self::key(file, symbol);
        let list = self.revisions.entry(k).or_default();

        // Avoid duplicate identical snapshots
        if let Some(last) = list.last() {
            if last.signature == signature && last.body_content == body {
                return;
            }
        }

        list.push(SymbolRevision {
            symbol_name: symbol.to_string(),
            file_path: file.to_string(),
            timestamp,
            signature: signature.to_string(),
            body_content: body.to_string(),
        });
    }

    /// Get all recorded revisions for a symbol
    pub fn get_history(&self, file: &str, symbol: &str) -> Vec<&SymbolRevision> {
        let k = Self::key(file, symbol);
        self.revisions.get(&k).map(|v| v.iter().collect()).unwrap_or_default()
    }

    /// Surgically rollback only the target function/component in current_source
    pub fn rewind_symbol(
        &self,
        current_source: &str,
        file: &str,
        symbol: &str,
        revision_idx: usize,
    ) -> Result<String> {
        let history = self.get_history(file, symbol);
        if history.is_empty() {
            return Err(HgbError::validation(format!(
                "No revision history recorded for symbol '{}' in '{}'",
                symbol, file
            )));
        }

        let target_rev = history.get(revision_idx).ok_or_else(|| {
            HgbError::validation(format!(
                "Revision index {} out of bounds for '{}' (max: {})",
                revision_idx,
                symbol,
                history.len().saturating_sub(1)
            ))
        })?;

        // Locate symbol in current_source using brace/boundary scanner
        let lines: Vec<&str> = current_source.lines().collect();
        let fn_start_pattern = format!(r"(?:fn|def|function)\s+{}\b", regex::escape(symbol));
        let re = Regex::new(&fn_start_pattern)
            .map_err(|e| HgbError::syntax(format!("Regex error: {}", e)))?;

        let mut start_idx = None;
        for (idx, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                start_idx = Some(idx);
                break;
            }
        }

        let start_line = start_idx.ok_or_else(|| {
            HgbError::validation(format!("Could not locate symbol '{}' in current source code", symbol))
        })?;

        // Find end line using brace depth
        let mut brace_depth: i32 = 0;
        let mut found_open = false;
        let mut end_line = start_line;

        for (j, line) in lines.iter().enumerate().skip(start_line) {
            for ch in line.chars() {
                if ch == '{' {
                    brace_depth += 1;
                    found_open = true;
                } else if ch == '}' {
                    brace_depth -= 1;
                }
            }
            if found_open && brace_depth <= 0 {
                end_line = j;
                break;
            }
        }

        // Splice in target revision replacement
        let mut output = Vec::new();
        for (idx, line) in lines.iter().enumerate() {
            if idx == start_line {
                // Insert historical function signature and body
                output.push(target_rev.signature.clone());
                output.push(target_rev.body_content.clone());
            } else if idx > start_line && idx <= end_line {
                // Skip lines of current function
                continue;
            } else {
                output.push(line.to_string());
            }
        }

        Ok(output.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_rewind_replaces_only_target_function() {
        let mut timeline = AstRewindTimeline::new();

        // 1. Record historical version of calculate_tax
        timeline.record_symbol_snapshot(
            "src/tax.rs",
            "calculate_tax",
            "pub fn calculate_tax(amount: f64) -> f64 {",
            "    amount * 0.08\n}",
            1000,
        );

        // 2. Current source with broken calculate_tax but brand-new helper functions we WANT to keep
        let current_source = r#"use std::sync::Arc;

pub fn brand_new_ui_theme() -> &'static str {
    "cyberpunk-neon"
}

pub fn calculate_tax(amount: f64) -> f64 {
    // BROKEN EXPERIMENTAL LOGIC
    panic!("tax service failed");
}

pub fn brand_new_currency_formatter(cents: u64) -> String {
    format!("${:.2}", cents as f64 / 100.0)
}
"#;

        // 3. Rewind ONLY calculate_tax to revision 0
        let restored = timeline.rewind_symbol(current_source, "src/tax.rs", "calculate_tax", 0)
            .expect("rewind calculate_tax");

        // Verify restored calculate_tax has the old working logic
        assert!(restored.contains("amount * 0.08"));
        assert!(!restored.contains("panic!(\"tax service failed\")"));

        // Verify other brand-new functions and imports are 100% INTACT
        assert!(restored.contains("brand_new_ui_theme"));
        assert!(restored.contains("cyberpunk-neon"));
        assert!(restored.contains("brand_new_currency_formatter"));
        assert!(restored.contains("use std::sync::Arc;"));
    }
}
