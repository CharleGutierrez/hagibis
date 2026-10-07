//! # LiveGraphWatcher - Continuous In-Memory Codebase Index & AST Watcher
//!
//! Elevates Augment Code's real-time whole-codebase context graph.
//! Maintains an incremental, sub-millisecond in-memory symbol and dependency
//! graph that updates dynamically upon filesystem changes.

use crate::error::{HgbError, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Discovered symbol record in the live index
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedSymbol {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line_number: usize,
    pub is_exported: bool,
}

/// Metadata tracked per indexed file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIndexMeta {
    pub relative_path: String,
    pub last_modified_epoch: u64,
    pub symbols: Vec<IndexedSymbol>,
    pub imported_symbols: HashSet<String>,
}

/// Summary report of the live index state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveGraphSummary {
    pub total_files_tracked: usize,
    pub total_symbols_indexed: usize,
    pub total_dependency_links: usize,
    pub sync_duration_us: u64,
    pub hot_symbols: Vec<(String, usize)>,
}

pub struct LiveGraphWatcher {
    pub workspace_root: PathBuf,
    pub files: HashMap<String, FileIndexMeta>,
    pub symbol_to_files: HashMap<String, HashSet<String>>,
    pub incoming_references: HashMap<String, usize>,
}

impl LiveGraphWatcher {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            files: HashMap::new(),
            symbol_to_files: HashMap::new(),
            incoming_references: HashMap::new(),
        }
    }

    /// Incrementally synchronize workspace: re-scans only touched files based on mtime
    pub fn sync_workspace(&mut self, extensions: &[&str]) -> Result<LiveGraphSummary> {
        let start = SystemTime::now();
        let mut discovered_files = Vec::new();
        self.collect_files(&self.workspace_root, extensions, &mut discovered_files)?;

        let mut _touched_files = 0;
        let mut discovered_paths = HashSet::new();

        for file_path in discovered_files {
            let rel_path = file_path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&file_path)
                .to_string_lossy()
                .to_string();

            discovered_paths.insert(rel_path.clone());

            let mtime = fs::metadata(&file_path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::now())
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let needs_reindex = match self.files.get(&rel_path) {
                Some(meta) => meta.last_modified_epoch < mtime,
                None => true,
            };

            if needs_reindex {
                _touched_files += 1;
                self.index_file(&rel_path, &file_path, mtime)?;
            }
        }

        // Clean up deleted files
        let deleted: Vec<String> = self
            .files
            .keys()
            .filter(|k| !discovered_paths.contains(*k))
            .cloned()
            .collect();

        for del in deleted {
            self.unindex_file(&del);
        }

        // Recompute incoming reference counts
        self.recalculate_references();

        let elapsed_us = start.elapsed().unwrap_or_default().as_micros() as u64;

        let total_symbols = self.files.values().map(|f| f.symbols.len()).sum();
        let total_links = self.incoming_references.values().sum();

        let mut hot_symbols: Vec<(String, usize)> = self
            .incoming_references
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        hot_symbols.sort_by(|a, b| b.1.cmp(&a.1));
        hot_symbols.truncate(10);

        Ok(LiveGraphSummary {
            total_files_tracked: self.files.len(),
            total_symbols_indexed: total_symbols,
            total_dependency_links: total_links,
            sync_duration_us: elapsed_us,
            hot_symbols,
        })
    }

    /// Query which files depend on or import from a specific file
    pub fn find_dependents(&self, file_path: &str) -> Vec<String> {
        let mut dependents = Vec::new();
        if let Some(target_meta) = self.files.get(file_path) {
            let exported: HashSet<&str> = target_meta
                .symbols
                .iter()
                .filter(|s| s.is_exported)
                .map(|s| s.name.as_str())
                .collect();

            for (other_path, meta) in &self.files {
                if other_path == file_path {
                    continue;
                }
                if meta.imported_symbols.iter().any(|imp| exported.contains(imp.as_str())) {
                    dependents.push(other_path.clone());
                }
            }
        }
        dependents.sort();
        dependents
    }

    /// Query which files reference a specific symbol name
    pub fn find_callers(&self, symbol_name: &str) -> Vec<String> {
        let mut callers = Vec::new();
        for (f_path, meta) in &self.files {
            if meta.imported_symbols.contains(symbol_name) {
                callers.push(f_path.clone());
            }
        }
        callers.sort();
        callers
    }

    fn index_file(&mut self, rel_path: &str, full_path: &Path, mtime: u64) -> Result<()> {
        let content = match fs::read_to_string(full_path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

        // Remove old occurrences of symbols from this file
        self.unindex_file(rel_path);

        let _ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mut symbols = Vec::new();
        let mut imported_symbols = HashSet::new();

        // Regex patterns for declarations
        let fn_re = Regex::new(r"(?m)^\s*(?:pub\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)").unwrap();
        let struct_re = Regex::new(r"(?m)^\s*(?:pub\s+)?(?:struct|enum|trait|class|interface)\s+([a-zA-Z0-9_]+)").unwrap();

        let newline_offsets = crate::zig_accelerate::simd_find_newlines(content.as_bytes());
        let mut lines = Vec::with_capacity(newline_offsets.len() + 1);
        let mut prev = 0;
        for &nl_idx in &newline_offsets {
            lines.push(&content[prev..nl_idx]);
            prev = nl_idx + 1;
        }
        if prev <= content.len() {
            lines.push(&content[prev..]);
        }

        for (idx, line) in lines.iter().enumerate() {
            let line_no = idx + 1;
            let trimmed = line.trim();

            if let Some(caps) = fn_re.captures(trimmed) {
                let name = caps[1].to_string();
                let is_pub = trimmed.starts_with("pub ");
                symbols.push(IndexedSymbol {
                    name: name.clone(),
                    kind: "fn".to_string(),
                    file_path: rel_path.to_string(),
                    line_number: line_no,
                    is_exported: is_pub,
                });
                self.symbol_to_files.entry(name).or_default().insert(rel_path.to_string());
            } else if let Some(caps) = struct_re.captures(trimmed) {
                let name = caps[1].to_string();
                let is_pub = trimmed.starts_with("pub ");
                symbols.push(IndexedSymbol {
                    name: name.clone(),
                    kind: "type".to_string(),
                    file_path: rel_path.to_string(),
                    line_number: line_no,
                    is_exported: is_pub,
                });
                self.symbol_to_files.entry(name).or_default().insert(rel_path.to_string());
            }

            let is_use = trimmed.starts_with("use ") || trimmed.starts_with("pub use ");
            let is_import = trimmed.starts_with("import ");
            if is_use || is_import {
                for word in trimmed.split(|c: char| !c.is_alphanumeric() && c != '_') {
                    if !word.is_empty()
                        && word != "use"
                        && word != "import"
                        && word != "crate"
                        && word != "from"
                        && word != "as"
                        && word != "pub"
                        && word != "self"
                        && word != "super"
                        && word != "std"
                    {
                        imported_symbols.insert(word.to_string());
                    }
                }
            }
        }

        self.files.insert(
            rel_path.to_string(),
            FileIndexMeta {
                relative_path: rel_path.to_string(),
                last_modified_epoch: mtime,
                symbols,
                imported_symbols,
            },
        );

        Ok(())
    }

    fn unindex_file(&mut self, rel_path: &str) {
        if let Some(meta) = self.files.remove(rel_path) {
            for sym in meta.symbols {
                if let Some(set) = self.symbol_to_files.get_mut(&sym.name) {
                    set.remove(rel_path);
                    if set.is_empty() {
                        self.symbol_to_files.remove(&sym.name);
                    }
                }
            }
        }
    }

    fn recalculate_references(&mut self) {
        self.incoming_references.clear();
        for meta in self.files.values() {
            for imp in &meta.imported_symbols {
                *self.incoming_references.entry(imp.clone()).or_insert(0) += 1;
            }
        }
    }

    fn collect_files(&self, dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(dir).map_err(|e| HgbError::Io(e))? {
            let entry = entry.map_err(|e| HgbError::Io(e))?;
            let path = entry.path();
            let file_name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

            if file_name.starts_with('.') || file_name == "target" || file_name == "node_modules" {
                continue;
            }

            if path.is_dir() {
                self.collect_files(&path, extensions, out)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.contains(&ext) {
                        out.push(path);
                    }
                }
            }
        }

        Ok(())
    }

    /// Converts current in-memory symbol index into a Compressed Sparse Row (CSR) graph
    /// accelerated via Zig cache-line traversal.
    pub fn to_csr_graph(&self) -> CsrSymbolGraph {
        let mut symbol_names = Vec::new();
        let mut symbol_to_id = HashMap::new();

        for name in self.symbol_to_files.keys() {
            symbol_to_id.insert(name.clone(), symbol_names.len() as u32);
            symbol_names.push(name.clone());
        }

        let mut row_offsets = Vec::with_capacity(symbol_names.len() + 1);
        let mut col_indices = Vec::new();

        row_offsets.push(0);

        for sym in &symbol_names {
            if let Some(files) = self.symbol_to_files.get(sym) {
                let mut dep_ids = HashSet::new();
                for f in files {
                    if let Some(meta) = self.files.get(f) {
                        for imp in &meta.imported_symbols {
                            if let Some(&target_id) = symbol_to_id.get(imp) {
                                if target_id != *symbol_to_id.get(sym).unwrap() {
                                    dep_ids.insert(target_id);
                                }
                            }
                        }
                    }
                }
                let mut sorted_deps: Vec<u32> = dep_ids.into_iter().collect();
                sorted_deps.sort();
                col_indices.extend(sorted_deps);
            }
            row_offsets.push(col_indices.len() as u32);
        }

        CsrSymbolGraph {
            symbol_names,
            symbol_to_id,
            row_offsets,
            col_indices,
        }
    }
}

/// Compressed Sparse Row (CSR) Representation of the Symbol Dependency Graph
///
/// Compresses the codebase dependency structure into two flat contiguous arrays:
/// `row_offsets` and `col_indices`, traversed in-place via Zig SIMD cache-line kernels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsrSymbolGraph {
    pub symbol_names: Vec<String>,
    pub symbol_to_id: HashMap<String, u32>,
    pub row_offsets: Vec<u32>,
    pub col_indices: Vec<u32>,
}

impl CsrSymbolGraph {
    pub fn new() -> Self {
        Self {
            symbol_names: Vec::new(),
            symbol_to_id: HashMap::new(),
            row_offsets: vec![0],
            col_indices: Vec::new(),
        }
    }

    /// Retrieve neighbor symbol IDs accelerated via Zig CSR kernel
    pub fn get_neighbor_ids(&self, node_id: u32) -> Vec<u32> {
        crate::zig_accelerate::csr_graph_neighbors(&self.row_offsets, &self.col_indices, node_id)
    }

    /// Retrieve neighbor symbol names directly
    pub fn get_neighbors(&self, symbol: &str) -> Vec<String> {
        if let Some(&node_id) = self.symbol_to_id.get(symbol) {
            let neighbor_ids = self.get_neighbor_ids(node_id);
            neighbor_ids
                .into_iter()
                .filter_map(|id| self.symbol_names.get(id as usize).cloned())
                .collect()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csr_symbol_graph_zig_acceleration() {
        let names = vec!["App".to_string(), "Config".to_string(), "Database".to_string()];
        let mut symbol_to_id = HashMap::new();
        for (i, name) in names.iter().enumerate() {
            symbol_to_id.insert(name.clone(), i as u32);
        }

        // App (0) -> Config (1), Database (2)
        // Config (1) -> Database (2)
        // Database (2) -> none
        let row_offsets = vec![0, 2, 3, 3];
        let col_indices = vec![1, 2, 2];

        let graph = CsrSymbolGraph {
            symbol_names: names,
            symbol_to_id,
            row_offsets,
            col_indices,
        };

        let app_neighbors = graph.get_neighbors("App");
        assert_eq!(app_neighbors, vec!["Config", "Database"]);

        let config_neighbors = graph.get_neighbors("Config");
        assert_eq!(config_neighbors, vec!["Database"]);

        let db_neighbors = graph.get_neighbors("Database");
        assert!(db_neighbors.is_empty());
    }
}

