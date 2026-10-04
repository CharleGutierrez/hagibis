use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeleportTarget {
    pub selector: String,
    pub source_file: String,
    pub line_number: usize,
    pub column_number: usize,
    pub symbol_name: String,
    pub component_type: String,
    pub code_snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeleportTargetReport {
    pub query_selector: String,
    pub matched: bool,
    pub target: Option<TeleportTarget>,
    pub alternatives: Vec<TeleportTarget>,
    pub confidence: f32,
    pub ghost_patch_hint: String,
}

pub struct CdpTeleportEngine {
    workspace_root: String,
}

impl CdpTeleportEngine {
    pub fn new() -> Self {
        Self {
            workspace_root: ".".to_string(),
        }
    }

    pub fn with_workspace(root: &str) -> Self {
        Self {
            workspace_root: root.to_string(),
        }
    }

    pub fn resolve_teleport(&self, selector: &str) -> TeleportTargetReport {
        let clean = selector.trim();
        
        let mut alternatives = Vec::new();
        let target_name = clean.split(|c: char| !c.is_alphanumeric() && c != '-').last().unwrap_or(clean);

        // Simple recursive search in src/ directory (or workspace_root)
        let mut to_visit = vec![std::path::PathBuf::from(&self.workspace_root)];
        while let Some(path) = to_visit.pop() {
            if path.is_dir() {
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        to_visit.push(entry.path());
                    }
                }
            } else if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "tsx" || ext == "ts" || ext == "jsx" || ext == "js" {
                        if let Ok(content) = fs::read_to_string(&path) {
                            for (i, line) in content.lines().enumerate() {
                                if line.contains(target_name) {
                                    alternatives.push(TeleportTarget {
                                        selector: clean.to_string(),
                                        source_file: path.to_string_lossy().into_owned(),
                                        line_number: i + 1,
                                        column_number: line.find(target_name).unwrap_or(0) + 1,
                                        symbol_name: target_name.to_string(),
                                        component_type: "React Component".to_string(),
                                        code_snippet: line.trim().to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(first) = alternatives.first().cloned() {
            TeleportTargetReport {
                query_selector: clean.to_string(),
                matched: true,
                target: Some(first.clone()),
                alternatives: alternatives.clone(),
                confidence: 0.95,
                ghost_patch_hint: format!("Found {} at {}:{}", first.symbol_name, first.source_file, first.line_number),
            }
        } else {
            TeleportTargetReport {
                query_selector: clean.to_string(),
                matched: false,
                target: None,
                alternatives: Vec::new(),
                confidence: 0.0,
                ghost_patch_hint: format!("Could not map {}", clean),
            }
        }
    }
}

impl Default for CdpTeleportEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_cdp_teleport_dynamic_search() {
        // Create a temporary test fixture file to search
        std::fs::create_dir_all("test_workspace").unwrap();
        let mut file = File::create("test_workspace/MyButton.tsx").unwrap();
        writeln!(file, "export const MyButton = () => <button id=\"my-test-btn\">Click</button>;").unwrap();

        let engine = CdpTeleportEngine::with_workspace("test_workspace");
        let rep = engine.resolve_teleport("button#my-test-btn");
        
        assert!(rep.matched);
        assert_eq!(rep.target.unwrap().source_file, "test_workspace/MyButton.tsx");

        std::fs::remove_dir_all("test_workspace").unwrap();
    }
}
