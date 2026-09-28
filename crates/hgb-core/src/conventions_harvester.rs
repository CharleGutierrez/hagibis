//! Superpower 108: Style Guide & Architectural DNA Harvester
//!
//! Scans repository configuration files (STYLE_GUIDE.md, .editorconfig, CONTRIBUTING.md,
//! rustfmt.toml, etc.) and compiles them into a token-compressed conventions DNA
//! system prompt block.

use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::error::HgbError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConventionRule {
    pub category: String,
    pub source_file: String,
    pub rule_summary: String,
    pub enforcement_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConventionsDna {
    pub blake3_content_hash: String,
    pub discovered_files: Vec<String>,
    pub rules: Vec<ConventionRule>,
    pub compact_system_prompt: String,
    pub token_count: usize,
}

pub struct ConventionsHarvester;

impl ConventionsHarvester {
    /// Scans the repository for conventions and compiles architectural DNA
    pub fn harvest(repo_root: &str, _force_refresh: bool) -> Result<ConventionsDna, HgbError> {
        let base = if repo_root.trim().is_empty() { "." } else { repo_root.trim() };
        let mut discovered = Vec::new();
        let mut rules = Vec::new();
        let mut combined_content = String::new();

        let candidate_files = [
            ("STYLE_GUIDE.md", "Architecture", "Strict"),
            ("CONTRIBUTING.md", "Workflow", "Advisory"),
            (".editorconfig", "Formatting", "Strict"),
            ("rustfmt.toml", "Formatting", "Strict"),
            (".eslintrc.json", "Linting", "Strict"),
            ("tsconfig.json", "TypeSafety", "Strict"),
            ("README.md", "Overview", "Advisory"),
        ];

        for (filename, category, enforcement) in candidate_files {
            let path = Path::new(base).join(filename);
            if path.exists() {
                discovered.push(filename.to_string());
                if let Ok(content) = std::fs::read_to_string(&path) {
                    combined_content.push_str(&content);

                    // Extract high-level rules from lines
                    for line in content.lines().take(15) {
                        let l = line.trim();
                        if l.starts_with('-') || l.starts_with('*') || l.starts_with("indent") || l.starts_with("max_width") {
                            rules.push(ConventionRule {
                                category: category.to_string(),
                                source_file: filename.to_string(),
                                rule_summary: l.trim_start_matches(|c| c == '-' || c == '*' || c == ' ').chars().take(80).collect(),
                                enforcement_level: enforcement.to_string(),
                            });
                        }
                    }
                }
            }
        }

        if rules.is_empty() {
            rules.push(ConventionRule {
                category: "Baseline".to_string(),
                source_file: "system-defaults".to_string(),
                rule_summary: "Sub-millisecond execution, zero-panic on hot paths, length-delimited Bincode IPC".to_string(),
                enforcement_level: "Strict".to_string(),
            });
        }

        let hash = blake3::hash(combined_content.as_bytes()).to_hex().to_string();

        let mut prompt_lines = Vec::new();
        prompt_lines.push(format!("[CONVENTIONS DNA: {} rules from {} files]", rules.len(), discovered.len()));
        for r in &rules {
            prompt_lines.push(format!("* [{}] {}: {}", r.enforcement_level, r.category, r.rule_summary));
        }

        let compact_system_prompt = prompt_lines.join("\n");
        let token_count = compact_system_prompt.split_whitespace().count() * 4 / 3;

        Ok(ConventionsDna {
            blake3_content_hash: hash,
            discovered_files: discovered,
            rules,
            compact_system_prompt,
            token_count,
        })
    }
}
