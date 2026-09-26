use crate::error::Result;
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Source classification for discovered workspace rules
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSourceType {
    HgbRulesDirectory, // Priority 1: .hgb/rules/*.md
    HgbRulesFile,      // Priority 1: .hgb/rules
    HgbMarkdown,       // Priority 2: HGB.md
    CursorRules,       // Priority 3: .cursorrules
    AgentsMarkdown,    // Priority 4: AGENTS.md
}

/// An individual parsed workspace rule document
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleDocument {
    pub file_path: PathBuf,
    pub relative_path: String,
    pub source_type: RuleSourceType,
    pub content: String,
    pub blake3_hash: String,
    pub priority: u8,
}

/// Workspace Rules Manager: scans and anchors workspace conventions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRulesManager {
    pub workspace_root: PathBuf,
    pub rules: Vec<RuleDocument>,
    pub aggregate_hash: String,
}

impl WorkspaceRulesManager {
    /// Discover rules in workspace hierarchy in strict precedence order
    pub fn discover<P: AsRef<Path>>(workspace_root: P) -> Self {
        let root = workspace_root.as_ref().to_path_buf();
        let mut rules = Vec::new();

        // 1. Check `.hgb/rules` directory
        let rules_dir = root.join(".hgb").join("rules");
        if rules_dir.is_dir() {
            if let Ok(entries) = fs::read_dir(&rules_dir) {
                let mut dir_files = Vec::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        let fname = path.file_name().unwrap_or_default().to_string_lossy();
                        if fname.ends_with(".md") || fname.ends_with(".txt") || fname.ends_with(".rules") {
                            dir_files.push(path);
                        }
                    }
                }
                dir_files.sort();
                for path in dir_files {
                    if let Ok(content) = fs::read_to_string(&path) {
                        let hash = Self::hash_content(content.as_bytes());
                        let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().to_string();
                        rules.push(RuleDocument {
                            file_path: path,
                            relative_path: rel,
                            source_type: RuleSourceType::HgbRulesDirectory,
                            content,
                            blake3_hash: hash,
                            priority: 1,
                        });
                    }
                }
            }
        } else {
            // Check `.hgb/rules` as single file
            let rules_file = root.join(".hgb").join("rules");
            if rules_file.is_file() {
                if let Ok(content) = fs::read_to_string(&rules_file) {
                    let hash = Self::hash_content(content.as_bytes());
                    let rel = rules_file.strip_prefix(&root).unwrap_or(&rules_file).to_string_lossy().to_string();
                    rules.push(RuleDocument {
                        file_path: rules_file,
                        relative_path: rel,
                        source_type: RuleSourceType::HgbRulesFile,
                        content,
                        blake3_hash: hash,
                        priority: 1,
                    });
                }
            }
        }

        // 2. Check `HGB.md`
        let hgb_md = root.join("HGB.md");
        if hgb_md.is_file() {
            if let Ok(content) = fs::read_to_string(&hgb_md) {
                let hash = Self::hash_content(content.as_bytes());
                let rel = hgb_md.strip_prefix(&root).unwrap_or(&hgb_md).to_string_lossy().to_string();
                rules.push(RuleDocument {
                    file_path: hgb_md,
                    relative_path: rel,
                    source_type: RuleSourceType::HgbMarkdown,
                    content,
                    blake3_hash: hash,
                    priority: 2,
                });
            }
        }

        // 3. Check `.cursorrules`
        let cursorrules = root.join(".cursorrules");
        if cursorrules.is_file() {
            if let Ok(content) = fs::read_to_string(&cursorrules) {
                let hash = Self::hash_content(content.as_bytes());
                let rel = cursorrules.strip_prefix(&root).unwrap_or(&cursorrules).to_string_lossy().to_string();
                rules.push(RuleDocument {
                    file_path: cursorrules,
                    relative_path: rel,
                    source_type: RuleSourceType::CursorRules,
                    content,
                    blake3_hash: hash,
                    priority: 3,
                });
            }
        }

        // 4. Check `AGENTS.md`
        let agents_md = root.join("AGENTS.md");
        if agents_md.is_file() {
            if let Ok(content) = fs::read_to_string(&agents_md) {
                let hash = Self::hash_content(content.as_bytes());
                let rel = agents_md.strip_prefix(&root).unwrap_or(&agents_md).to_string_lossy().to_string();
                rules.push(RuleDocument {
                    file_path: agents_md,
                    relative_path: rel,
                    source_type: RuleSourceType::AgentsMarkdown,
                    content,
                    blake3_hash: hash,
                    priority: 4,
                });
            }
        }

        let mut hasher = Hasher::new();
        for r in &rules {
            hasher.update(r.blake3_hash.as_bytes());
        }
        let aggregate_hash = hasher.finalize().to_hex().to_string();

        Self {
            workspace_root: root,
            rules,
            aggregate_hash,
        }
    }

    /// Whether any workspace rules were discovered
    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    /// Format combined rules for system prompt injection
    pub fn aggregate_rules(&self) -> String {
        if self.rules.is_empty() {
            return String::new();
        }

        let mut out = format!(
            "<workspace_rules aggregate_hash=\"{}\">\n",
            &self.aggregate_hash[..8.min(self.aggregate_hash.len())]
        );
        for rule in &self.rules {
            out.push_str(&format!(
                "<!-- Rule File: {} (Priority: {}) -->\n{}\n\n",
                rule.relative_path, rule.priority, rule.content.trim()
            ));
        }
        out.push_str("</workspace_rules>\n");
        out
    }

    /// Initialize default `.hgb/rules/general.md` in workspace
    pub fn init_default_rules<P: AsRef<Path>>(workspace_root: P) -> Result<PathBuf> {
        let rules_dir = workspace_root.as_ref().join(".hgb").join("rules");
        fs::create_dir_all(&rules_dir)?;
        let target_file = rules_dir.join("general.md");
        if !target_file.exists() {
            let default_content = "# Hagibis (`hgb`) Workspace Rules\n\n\
                - Always write clean, idiomatic, panic-free Rust code.\n\
                - Prefer explicit error handling over unwrap().\n\
                - Maintain unit test coverage for all new public APIs.\n\
                - Follow Conventional Commits format (feat, fix, refactor, test, docs).\n";
            let mut f = File::create(&target_file)?;
            f.write_all(default_content.as_bytes())?;
        }
        Ok(target_file)
    }

    fn hash_content(data: &[u8]) -> String {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rules_manager_discovery_and_hashing() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_rules_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let rules_path = WorkspaceRulesManager::init_default_rules(&temp_dir).unwrap();
        assert!(rules_path.exists());

        let mgr = WorkspaceRulesManager::discover(&temp_dir);
        assert!(mgr.has_rules());
        assert_eq!(mgr.rules.len(), 1);
        assert_eq!(mgr.rules[0].priority, 1);
        assert!(!mgr.aggregate_hash.is_empty());

        let aggregated = mgr.aggregate_rules();
        assert!(aggregated.contains("<workspace_rules"));
        assert!(aggregated.contains("Hagibis (`hgb`) Workspace Rules"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
