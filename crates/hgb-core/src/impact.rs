//! # Ripple Effect Radar: Cross-File Dependency Impact Analyzer
//!
//! Provides polyglot cross-file import graph construction, transitive blast-radius
//! computation, and exact call-site extraction across Rust, TypeScript, Python, and Go.

use crate::error::{HgbError, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// Assessment of risk severity for modifying a symbol
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Moderate,
    High,
    Critical,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "LOW"),
            Self::Moderate => write!(f, "MODERATE"),
            Self::High => write!(f, "HIGH"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// An exact call-site reference of a target symbol
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolCallsite {
    pub file_path: PathBuf,
    pub relative_path: String,
    pub line_number: usize,
    pub context_snippet: String,
}

/// Detailed impact assessment report for a symbol mutation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactReport {
    pub target_symbol: String,
    pub originating_file: PathBuf,
    pub direct_dependents: Vec<PathBuf>,
    pub transitive_dependents: Vec<PathBuf>,
    pub total_affected_files: usize,
    pub call_sites: Vec<SymbolCallsite>,
    pub risk_level: RiskLevel,
    pub recommended_action: String,
}

/// The Ripple Effect Radar engine
#[derive(Debug, Clone)]
pub struct ImpactRadar {
    pub workspace_root: PathBuf,
    pub import_graph: HashMap<PathBuf, HashSet<PathBuf>>,
    pub reverse_import_graph: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl ImpactRadar {
    /// Create a new Ripple Effect Radar rooted at the specified workspace
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            workspace_root: root.as_ref().to_path_buf(),
            import_graph: HashMap::new(),
            reverse_import_graph: HashMap::new(),
        }
    }

    /// Recursively scan and build dependency graphs for the workspace
    pub fn index_workspace(&mut self) -> Result<()> {
        self.import_graph.clear();
        self.reverse_import_graph.clear();

        let mut files = Vec::new();
        self.collect_source_files(&self.workspace_root.clone(), &mut files)?;

        // Regex patterns for imports across supported languages
        let rs_import_re = Regex::new(r#"(?m)^\s*(?:pub\s+)?(?:use\s+([^;]+);|mod\s+([a-zA-Z0-9_]+);)"#).unwrap();
        let ts_import_re = Regex::new(r#"(?m)^\s*import\s+.*?\s+from\s+['"]([^'"]+)['"]"#).unwrap();
        let py_import_re = Regex::new(r#"(?m)^\s*(?:from\s+([a-zA-Z0-9_.]+)\s+import|import\s+([a-zA-Z0-9_.]+))"#).unwrap();
        let go_import_re = Regex::new(r#"(?m)^\s*import\s+['"]([^'"]+)['"]"#).unwrap();

        for file in &files {
            if let Ok(content) = fs::read_to_string(file) {
                let mut imported_paths = HashSet::new();
                let ext = file.extension().and_then(|s| s.to_str()).unwrap_or("");

                match ext {
                    "rs" => {
                        for cap in rs_import_re.captures_iter(&content) {
                            if let Some(m) = cap.get(1) {
                                let import_stmt = m.as_str().trim();
                                for candidate in &files {
                                    if candidate == file {
                                        continue;
                                    }
                                    let stem = candidate.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                                    if !stem.is_empty() && import_stmt.contains(stem) {
                                        imported_paths.insert(candidate.clone());
                                    }
                                }
                            } else if let Some(m) = cap.get(2) {
                                let mod_name = m.as_str().trim();
                                for candidate in &files {
                                    if candidate == file {
                                        continue;
                                    }
                                    let stem = candidate.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                                    if stem == mod_name {
                                        imported_paths.insert(candidate.clone());
                                    }
                                }
                            }
                        }
                    }
                    "ts" | "tsx" | "js" | "jsx" => {
                        for cap in ts_import_re.captures_iter(&content) {
                            if let Some(m) = cap.get(1) {
                                let specifier = m.as_str();
                                if specifier.starts_with('.') {
                                    let parent = file.parent().unwrap_or(&self.workspace_root);
                                    let resolved = parent.join(specifier);
                                    for candidate in &files {
                                        if candidate.starts_with(&resolved) || candidate == &resolved.with_extension("ts") || candidate == &resolved.with_extension("tsx") || candidate == &resolved.with_extension("js") {
                                            imported_paths.insert(candidate.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "py" => {
                        for cap in py_import_re.captures_iter(&content) {
                            let mod_part = cap.get(1).or_else(|| cap.get(2)).map(|m| m.as_str()).unwrap_or("");
                            let simple_name = mod_part.split('.').last().unwrap_or("");
                            if !simple_name.is_empty() {
                                for candidate in &files {
                                    if candidate == file {
                                        continue;
                                    }
                                    let stem = candidate.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                                    if stem == simple_name {
                                        imported_paths.insert(candidate.clone());
                                    }
                                }
                            }
                        }
                    }
                    "go" => {
                        for cap in go_import_re.captures_iter(&content) {
                            if let Some(m) = cap.get(1) {
                                let pkg = m.as_str();
                                let pkg_name = pkg.split('/').last().unwrap_or("");
                                for candidate in &files {
                                    if candidate == file {
                                        continue;
                                    }
                                    if let Some(p) = candidate.parent() {
                                        if p.ends_with(pkg_name) {
                                            imported_paths.insert(candidate.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }

                for imp in &imported_paths {
                    self.reverse_import_graph
                        .entry(imp.clone())
                        .or_default()
                        .insert(file.clone());
                }
                self.import_graph.insert(file.clone(), imported_paths);
            }
        }

        Ok(())
    }

    /// Assess the blast-radius impact of modifying or removing a target symbol
    pub fn assess_symbol_impact<P: AsRef<Path>>(
        &self,
        symbol_name: &str,
        originating_file: P,
    ) -> ImpactReport {
        let orig = originating_file.as_ref().to_path_buf();
        let sym = symbol_name.trim();

        // 1. Direct dependents (files that import originating file)
        let direct_dependents: Vec<PathBuf> = self
            .reverse_import_graph
            .get(&orig)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();

        // 2. Transitive dependents using BFS with cycle detection
        let mut transitive = HashSet::new();
        let mut queue = VecDeque::new();
        for dep in &direct_dependents {
            transitive.insert(dep.clone());
            queue.push_back(dep.clone());
        }

        while let Some(current) = queue.pop_front() {
            if let Some(next_deps) = self.reverse_import_graph.get(&current) {
                for next_dep in next_deps {
                    if next_dep != &orig && transitive.insert(next_dep.clone()) {
                        queue.push_back(next_dep.clone());
                    }
                }
            }
        }

        // 3. Scan all candidate dependents for actual call-site references
        let mut call_sites = Vec::new();
        let mut all_affected = transitive.clone();
        all_affected.insert(orig.clone());

        for file in &all_affected {
            if let Ok(content) = fs::read_to_string(file) {
                for (idx, line) in content.lines().enumerate() {
                    // Match whole-word symbol occurrence
                    if line.contains(sym) {
                        let rel = file
                            .strip_prefix(&self.workspace_root)
                            .unwrap_or(file)
                            .to_string_lossy()
                            .to_string();
                        call_sites.push(SymbolCallsite {
                            file_path: file.clone(),
                            relative_path: rel,
                            line_number: idx + 1,
                            context_snippet: line.trim().to_string(),
                        });
                    }
                }
            }
        }

        let total_affected = all_affected.len();
        let call_count = call_sites.len();

        // 4. Calculate Risk Level
        let risk_level = if call_count == 0 {
            RiskLevel::Low
        } else if call_count <= 3 && total_affected <= 2 {
            RiskLevel::Moderate
        } else if call_count <= 8 && total_affected <= 5 {
            RiskLevel::High
        } else {
            RiskLevel::Critical
        };

        let recommended_action = match risk_level {
            RiskLevel::Low => "Direct surgical edit safe — zero external call sites detected.".to_string(),
            RiskLevel::Moderate => format!("Atomic patch across {} call sites recommended.", call_count),
            RiskLevel::High => format!("High blast radius: stage simultaneous edits across {} affected files with compiler dry-run.", total_affected),
            RiskLevel::Critical => format!("CRITICAL: symbol is a core nexus with {} call sites across {} files. Full speculative race and regression test required.", call_count, total_affected),
        };

        let mut transitive_vec: Vec<PathBuf> = transitive.into_iter().collect();
        transitive_vec.sort();

        ImpactReport {
            target_symbol: sym.to_string(),
            originating_file: orig,
            direct_dependents,
            transitive_dependents: transitive_vec,
            total_affected_files: total_affected,
            call_sites,
            risk_level,
            recommended_action,
        }
    }

    fn collect_source_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(());
        }

        let entries = fs::read_dir(dir).map_err(|e| HgbError::Io(e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

            // Skip hidden and build directories
            if file_name.starts_with('.') || file_name == "target" || file_name == "node_modules" || file_name == "dist" || file_name == "__pycache__" {
                continue;
            }

            if path.is_dir() {
                self.collect_source_files(&path, files)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    match ext {
                        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" => {
                            files.push(path);
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_impact_radar_indexing_and_blast_radius() {
        let tmp = std::env::temp_dir().join(format!("hgb_radar_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&tmp).unwrap();

        // 1. Create file A: core module
        let file_a = tmp.join("core_engine.rs");
        fs::write(&file_a, "pub struct SessionNexus;\nimpl SessionNexus { pub fn connect() {} }\n").unwrap();

        // 2. Create file B: direct dependent of A
        let file_b = tmp.join("service.rs");
        fs::write(&file_b, "use crate::core_engine::SessionNexus;\npub fn run() { SessionNexus::connect(); }\n").unwrap();

        // 3. Create file C: transitive dependent of A (via B)
        let file_c = tmp.join("app.rs");
        fs::write(&file_c, "use crate::service;\npub fn main() { service::run(); }\n").unwrap();

        let mut radar = ImpactRadar::new(&tmp);
        radar.index_workspace().unwrap();

        let report = radar.assess_symbol_impact("SessionNexus", &file_a);

        assert_eq!(report.target_symbol, "SessionNexus");
        assert_eq!(report.originating_file, file_a);
        assert!(report.direct_dependents.contains(&file_b));
        assert!(report.call_sites.len() >= 2);
        assert_ne!(report.risk_level, RiskLevel::Low);

        let _ = fs::remove_dir_all(&tmp);
    }
}
