//! # Interactive Blast-Radius Architecture Radar
//!
//! Analyzes AST symbol definitions, imports, and cross-file references across a workspace.
//! Given a set of modified files or symbols, computes:
//! - Exact symbols modified (structs, functions, enums, traits).
//! - Direct call sites and downstream dependent files.
//! - Quantitative blast score and severity level (`Low`, `Medium`, `High`, `Critical`).
//! - Regression test suite recommendations.
//! - Formatted TrueColor/ANSI terminal radar visualizer.

use crate::error::{HgbError, Result};
use colored::Colorize;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Severity rating of the blast radius
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BlastSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl BlastSeverity {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Low => "LOW (Localized Impact)",
            Self::Medium => "MEDIUM (Downstream Consumers)",
            Self::High => "HIGH (Core Architectural Shift)",
            Self::Critical => "CRITICAL (System-Wide Blast Radius)",
        }
    }

    pub fn colorize(&self) -> String {
        match self {
            Self::Low => self.name().green().bold().to_string(),
            Self::Medium => self.name().yellow().bold().to_string(),
            Self::High => self.name().bright_red().bold().to_string(),
            Self::Critical => self.name().red().bold().on_black().to_string(),
        }
    }
}

/// A code symbol declared or modified in a target file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AffectedSymbol {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line_number: usize,
    pub references_count: usize,
}

/// A specific location where a modified symbol is referenced
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallsiteReference {
    pub symbol_name: String,
    pub file_path: String,
    pub line_number: usize,
    pub line_snippet: String,
    pub is_test: bool,
}

/// The complete architectural impact report
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArchitecturalBlastRadiusReport {
    pub target_files: Vec<String>,
    pub modified_symbols: Vec<AffectedSymbol>,
    pub call_sites: Vec<CallsiteReference>,
    pub dependent_files: Vec<String>,
    pub recommended_tests: Vec<String>,
    pub total_references: usize,
    pub blast_score: f32,
    pub severity: BlastSeverity,
}

pub struct BlastRadiusRadar {
    workspace_root: PathBuf,
}

impl BlastRadiusRadar {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Extract declared symbols (functions, structs, traits, enums) from a source file
    pub fn extract_symbols_from_content(content: &str, file_path: &str) -> Vec<AffectedSymbol> {
        let mut symbols = Vec::new();
        let fn_re = Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
        let struct_re = Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
        let enum_re = Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?enum\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
        let trait_re = Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?trait\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
        let type_re = Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?type\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

        for (idx, line) in content.lines().enumerate() {
            let line_num = idx + 1;
            if let Some(caps) = fn_re.captures(line) {
                if let Some(m) = caps.get(1) {
                    symbols.push(AffectedSymbol {
                        name: m.as_str().to_string(),
                        kind: "fn".to_string(),
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        references_count: 0,
                    });
                }
            } else if let Some(caps) = struct_re.captures(line) {
                if let Some(m) = caps.get(1) {
                    symbols.push(AffectedSymbol {
                        name: m.as_str().to_string(),
                        kind: "struct".to_string(),
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        references_count: 0,
                    });
                }
            } else if let Some(caps) = enum_re.captures(line) {
                if let Some(m) = caps.get(1) {
                    symbols.push(AffectedSymbol {
                        name: m.as_str().to_string(),
                        kind: "enum".to_string(),
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        references_count: 0,
                    });
                }
            } else if let Some(caps) = trait_re.captures(line) {
                if let Some(m) = caps.get(1) {
                    symbols.push(AffectedSymbol {
                        name: m.as_str().to_string(),
                        kind: "trait".to_string(),
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        references_count: 0,
                    });
                }
            } else if let Some(caps) = type_re.captures(line) {
                if let Some(m) = caps.get(1) {
                    symbols.push(AffectedSymbol {
                        name: m.as_str().to_string(),
                        kind: "type".to_string(),
                        file_path: file_path.to_string(),
                        line_number: line_num,
                        references_count: 0,
                    });
                }
            }
        }

        symbols
    }

    /// Recursively find all source code files in a directory
    fn collect_source_files(&self, dir: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with('.') || file_name == "target" || file_name == "node_modules" {
                    continue;
                }

                if path.is_dir() {
                    self.collect_source_files(&path, files);
                } else if path.is_file() {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if ["rs", "ts", "js", "py", "go", "cpp", "c", "h"].contains(&ext) {
                        files.push(path);
                    }
                }
            }
        }
    }

    /// Analyze blast radius for a list of modified files relative to workspace root
    pub fn analyze(&self, modified_files: &[impl AsRef<Path>]) -> Result<ArchitecturalBlastRadiusReport> {
        let mut target_file_strings = Vec::new();
        let mut symbols = Vec::new();

        // 1. Extract symbols from all modified files
        for rel_path in modified_files {
            let rel = rel_path.as_ref();
            let abs_path = if rel.is_absolute() {
                rel.to_path_buf()
            } else {
                self.workspace_root.join(rel)
            };

            let rel_str = rel.to_string_lossy().to_string();
            target_file_strings.push(rel_str.clone());

            if abs_path.exists() {
                if let Ok(content) = fs::read_to_string(&abs_path) {
                    let file_syms = Self::extract_symbols_from_content(&content, &rel_str);
                    symbols.extend(file_syms);
                }
            }
        }

        // 2. Discover all source files across the workspace
        let mut all_sources = Vec::new();
        self.collect_source_files(&self.workspace_root, &mut all_sources);

        let mut call_sites = Vec::new();
        let mut dependent_files_set = HashSet::new();
        let mut recommended_tests_set = HashSet::new();
        let mut symbol_ref_counts: HashMap<String, usize> = HashMap::new();

        // Build word-boundary regexes for each symbol
        let symbol_matchers: Vec<(&AffectedSymbol, Regex)> = symbols
            .iter()
            .filter_map(|sym| {
                let pat = format!(r"\b{}\b", regex::escape(&sym.name));
                Regex::new(&pat).ok().map(|re| (sym, re))
            })
            .collect();

        // 3. Scan all source files for occurrences
        for source_path in all_sources {
            let rel_source = source_path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&source_path)
                .to_string_lossy()
                .to_string();

            // Skip checking the source file against its own declaration
            let is_target_file = target_file_strings.iter().any(|t| t == &rel_source);

            if let Ok(content) = fs::read_to_string(&source_path) {
                let is_test_file = rel_source.contains("test") || rel_source.starts_with("tests/");

                for (idx, line) in content.lines().enumerate() {
                    let line_num = idx + 1;
                    let trimmed = line.trim();

                    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                        continue;
                    }

                    for (sym, re) in &symbol_matchers {
                        // Skip declaration line in the same file
                        if is_target_file && line_num == sym.line_number {
                            continue;
                        }

                        if re.is_match(line) {
                            *symbol_ref_counts.entry(sym.name.clone()).or_insert(0) += 1;

                            if !is_target_file {
                                dependent_files_set.insert(rel_source.clone());
                            }

                            if is_test_file {
                                recommended_tests_set.insert(rel_source.clone());
                            }

                            call_sites.push(CallsiteReference {
                                symbol_name: sym.name.clone(),
                                file_path: rel_source.clone(),
                                line_number: line_num,
                                line_snippet: if trimmed.len() > 100 {
                                    format!("{}...", &trimmed[..97])
                                } else {
                                    trimmed.to_string()
                                },
                                is_test: is_test_file,
                            });
                        }
                    }
                }
            }
        }

        // Update reference counts on symbols
        for sym in &mut symbols {
            sym.references_count = *symbol_ref_counts.get(&sym.name).unwrap_or(&0);
        }

        let total_references = call_sites.len();
        let dep_count = dependent_files_set.len();

        // Compute Blast Score ($score = references \times 1.5 + dependents \times 4.0$)
        let blast_score = (total_references as f32 * 1.5) + (dep_count as f32 * 4.0);

        let severity = if blast_score == 0.0 && symbols.is_empty() {
            BlastSeverity::Low
        } else if blast_score < 15.0 && dep_count <= 2 {
            BlastSeverity::Low
        } else if blast_score < 45.0 && dep_count <= 6 {
            BlastSeverity::Medium
        } else if blast_score < 100.0 {
            BlastSeverity::High
        } else {
            BlastSeverity::Critical
        };

        let mut dependent_files: Vec<String> = dependent_files_set.into_iter().collect();
        dependent_files.sort();

        let mut recommended_tests: Vec<String> = recommended_tests_set.into_iter().collect();
        recommended_tests.sort();

        Ok(ArchitecturalBlastRadiusReport {
            target_files: target_file_strings,
            modified_symbols: symbols,
            call_sites,
            dependent_files,
            recommended_tests,
            total_references,
            blast_score,
            severity,
        })
    }

    /// Render a rich terminal visualization box of the blast radius
    pub fn format_terminal(&self, report: &ArchitecturalBlastRadiusReport) -> String {
        let width = 78;
        let mut out = String::new();

        out.push_str(&format!("{}\n", "═".repeat(width).cyan()));
        out.push_str(&format!(
            "  📡  {}  📡\n",
            "HAGIBIS ARCHITECTURAL BLAST-RADIUS RADAR".cyan().bold()
        ));
        out.push_str(&format!("{}\n\n", "═".repeat(width).cyan()));

        out.push_str(&format!(
            "  Target Modified Files ({}):\n",
            report.target_files.len()
        ));
        for f in &report.target_files {
            out.push_str(&format!("    • {}\n", f.white().bold()));
        }

        out.push_str(&format!(
            "\n  Severity Rating:      {}\n",
            report.severity.colorize()
        ));
        out.push_str(&format!(
            "  Blast Score:          {:.1}\n",
            report.blast_score
        ));
        out.push_str(&format!(
            "  Symbols Tracked:      {}\n",
            report.modified_symbols.len()
        ));
        out.push_str(&format!(
            "  Total References:     {}\n",
            report.total_references
        ));
        out.push_str(&format!(
            "  Downstream Files ({}):\n",
            report.dependent_files.len()
        ));

        if report.dependent_files.is_empty() {
            out.push_str(&format!("    {}\n", "None (Zero downstream impact)".dimmed()));
        } else {
            for dep in &report.dependent_files {
                out.push_str(&format!("    ↳ {}\n", dep.yellow()));
            }
        }

        out.push_str(&format!(
            "\n  🎯 Recommended Regression Test Suites ({}):\n",
            report.recommended_tests.len()
        ));
        if report.recommended_tests.is_empty() {
            out.push_str(&format!("    {}\n", "No direct test files impacted".dimmed()));
        } else {
            for t in &report.recommended_tests {
                out.push_str(&format!("    ✔ {}\n", t.green().bold()));
            }
        }

        if !report.call_sites.is_empty() {
            out.push_str(&format!(
                "\n  🔍 Sample Callsites (Top {}):\n",
                report.call_sites.len().min(5)
            ));
            for cs in report.call_sites.iter().take(5) {
                out.push_str(&format!(
                    "    {}:{} [{}]\n      {}\n",
                    cs.file_path.cyan(),
                    cs.line_number,
                    cs.symbol_name.magenta(),
                    cs.line_snippet.dimmed()
                ));
            }
        }

        out.push_str(&format!("\n{}\n", "═".repeat(width).cyan()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_symbols_from_content() {
        let code = r#"
            pub struct UserSession {
                pub id: String,
            }

            pub enum AuthStatus {
                Active,
                Expired,
            }

            pub trait TokenValidator {
                fn validate(&self) -> bool;
            }

            pub async fn authenticate_user(token: &str) -> bool {
                true
            }
        "#;

        let symbols = BlastRadiusRadar::extract_symbols_from_content(code, "auth.rs");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"UserSession"));
        assert!(names.contains(&"AuthStatus"));
        assert!(names.contains(&"TokenValidator"));
        assert!(names.contains(&"authenticate_user"));
    }

    #[test]
    fn test_blast_radius_cross_file_impact() {
        let temp_dir = std::env::temp_dir().join(format!("blast_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let mod_a = temp_dir.join("src/models.rs");
        fs::create_dir_all(mod_a.parent().unwrap()).unwrap();
        fs::write(&mod_a, "pub struct CustomerAccount { pub balance: f64 }\npub fn calculate_tax() -> f64 { 0.15 }").unwrap();

        let mod_b = temp_dir.join("src/billing.rs");
        fs::write(&mod_b, "use crate::models::CustomerAccount;\npub fn charge(acc: &CustomerAccount) {\n let tax = crate::models::calculate_tax();\n}").unwrap();

        let mod_c = temp_dir.join("tests/billing_test.rs");
        fs::create_dir_all(mod_c.parent().unwrap()).unwrap();
        fs::write(&mod_c, "use crate::models::CustomerAccount;\n#[test]\nfn test_billing() {\n let acc = CustomerAccount { balance: 100.0 };\n}").unwrap();

        let radar = BlastRadiusRadar::new(&temp_dir);
        let report = radar.analyze(&[PathBuf::from("src/models.rs")]).unwrap();

        assert_eq!(report.target_files, vec!["src/models.rs"]);
        assert!(report.modified_symbols.iter().any(|s| s.name == "CustomerAccount"));
        assert!(report.modified_symbols.iter().any(|s| s.name == "calculate_tax"));

        assert!(report.dependent_files.contains(&"src/billing.rs".to_string()));
        assert!(report.recommended_tests.contains(&"tests/billing_test.rs".to_string()));

        assert!(report.total_references >= 3);
        assert!(report.blast_score > 0.0);

        let output = radar.format_terminal(&report);
        assert!(output.contains("HAGIBIS ARCHITECTURAL BLAST-RADIUS RADAR"));
        assert!(output.contains("CustomerAccount"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
