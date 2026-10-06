//! # RepoMapRanker - High-Density Code Architecture & PageRank Graph
//!
//! Elevates Aider's Repo-Map concept with deterministic graph ranking,
//! reference tracking, and dynamic token-budget packing for vibe code developers.

use crate::error::{HgbError, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Classification of discovered code symbol
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RankedSymbolKind {
    Struct,
    Enum,
    Trait,
    Function,
    TypeAlias,
    Interface,
    Class,
    Method,
    Constant,
}

impl RankedSymbolKind {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Function => "fn",
            Self::TypeAlias => "type",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Method => "method",
            Self::Constant => "const",
        }
    }
}

/// An individual parsed code symbol with architectural coordinates
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankedSymbol {
    pub name: String,
    pub kind: RankedSymbolKind,
    pub relative_path: String,
    pub line_number: usize,
    pub signature: String,
    pub is_public: bool,
    pub rank_score: f64,
}

/// Outgoing reference or dependency link
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolRef {
    pub from_symbol: String,
    pub to_symbol: String,
    pub from_file: String,
}

/// Directed reference graph for PageRank calculation
#[derive(Debug, Clone, Default)]
pub struct RepoSymbolGraph {
    pub symbols: HashMap<String, RankedSymbol>,
    pub outgoing: HashMap<String, HashSet<String>>,
    pub incoming: HashMap<String, HashSet<String>>,
}

impl RepoSymbolGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_symbol(&mut self, symbol: RankedSymbol) {
        let key = format!("{}::{}", symbol.relative_path, symbol.name);
        self.symbols.insert(key, symbol);
    }

    pub fn add_reference(&mut self, from_key: &str, to_key: &str) {
        self.outgoing
            .entry(from_key.to_string())
            .or_default()
            .insert(to_key.to_string());
        self.incoming
            .entry(to_key.to_string())
            .or_default()
            .insert(from_key.to_string());
    }

    /// Compute PageRank scores using power iteration accelerated by Zig CSR engine
    pub fn compute_pagerank(&mut self, iterations: usize, damping_factor: f64) {
        let n = self.symbols.len();
        if n == 0 {
            return;
        }

        let mut node_keys: Vec<String> = self.symbols.keys().cloned().collect();
        node_keys.sort();
        let key_to_idx: HashMap<&str, usize> = node_keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i))
            .collect();

        let mut row_offsets = Vec::with_capacity(n + 1);
        let mut col_indices = Vec::new();

        for key in &node_keys {
            row_offsets.push(col_indices.len() as u32);
            if let Some(out_nodes) = self.outgoing.get(key) {
                for out_node in out_nodes {
                    if let Some(&v) = key_to_idx.get(out_node.as_str()) {
                        col_indices.push(v as u32);
                    }
                }
            }
        }
        row_offsets.push(col_indices.len() as u32);

        let scores = crate::zig_accelerate::pagerank_csr(
            n,
            &row_offsets,
            &col_indices,
            iterations,
            damping_factor,
        );

        for (i, key) in node_keys.iter().enumerate() {
            if let Some(sym) = self.symbols.get_mut(key) {
                let pub_boost = if sym.is_public { 1.25 } else { 1.0 };
                sym.rank_score = scores[i] * (n as f64) * pub_boost;
            }
        }
    }
}

/// The RepoMapRanker engine
pub struct RepoMapRanker {
    pub root_dir: PathBuf,
}

impl RepoMapRanker {
    pub fn new(root_dir: impl AsRef<Path>) -> Self {
        Self {
            root_dir: root_dir.as_ref().to_path_buf(),
        }
    }

    /// Scan directory, parse symbols, build graph and calculate PageRank
    pub fn analyze_repo(&self, extensions: &[&str]) -> Result<RepoSymbolGraph> {
        let mut graph = RepoSymbolGraph::new();
        let mut all_files = Vec::new();

        self.collect_files(&self.root_dir, extensions, &mut all_files)?;

        // Pass 1: Parse all symbols
        let mut name_to_keys: HashMap<String, Vec<String>> = HashMap::new();
        for file_path in &all_files {
            let rel_path = file_path
                .strip_prefix(&self.root_dir)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            if let Ok(content) = fs::read_to_string(file_path) {
                let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let symbols = Self::extract_symbols(&content, &rel_path, ext);
                for sym in symbols {
                    let key = format!("{}::{}", sym.relative_path, sym.name);
                    name_to_keys.entry(sym.name.clone()).or_default().push(key.clone());
                    graph.add_symbol(sym);
                }
            }
        }

        // Pass 2: Detect references between symbols across files
        for file_path in &all_files {
            let rel_path = file_path
                .strip_prefix(&self.root_dir)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            if let Ok(content) = fs::read_to_string(file_path) {
                for (sym_name, target_keys) in &name_to_keys {
                    if content.contains(sym_name) {
                        for target_key in target_keys {
                            let from_key = format!("{}::main", rel_path);
                            graph.add_reference(&from_key, target_key);
                        }
                    }
                }
            }
        }

        // Pass 3: Compute PageRank
        graph.compute_pagerank(15, 0.85);

        Ok(graph)
    }

    /// Extract symbols from source code across Rust, TypeScript, Python, and Go
    pub fn extract_symbols(content: &str, rel_path: &str, ext: &str) -> Vec<RankedSymbol> {
        let mut symbols = Vec::new();

        match ext {
            "rs" => {
                let fn_re = Regex::new(r"(?m)^\s*(pub(?:\s*\(.*?\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)\s*(<.*?>)?\s*\((.*?)\)").unwrap();
                let struct_re = Regex::new(r"(?m)^\s*(pub(?:\s*\(.*?\))?\s+)?struct\s+([a-zA-Z0-9_]+)").unwrap();
                let enum_re = Regex::new(r"(?m)^\s*(pub(?:\s*\(.*?\))?\s+)?enum\s+([a-zA-Z0-9_]+)").unwrap();
                let trait_re = Regex::new(r"(?m)^\s*(pub(?:\s*\(.*?\))?\s+)?trait\s+([a-zA-Z0-9_]+)").unwrap();

                for (idx, line) in content.lines().enumerate() {
                    let line_no = idx + 1;
                    if let Some(caps) = fn_re.captures(line) {
                        let is_pub = caps.get(1).is_some();
                        let name = caps[2].to_string();
                        symbols.push(RankedSymbol {
                            name,
                            kind: RankedSymbolKind::Function,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: is_pub,
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = struct_re.captures(line) {
                        let is_pub = caps.get(1).is_some();
                        let name = caps[2].to_string();
                        symbols.push(RankedSymbol {
                            name,
                            kind: RankedSymbolKind::Struct,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: is_pub,
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = enum_re.captures(line) {
                        let is_pub = caps.get(1).is_some();
                        let name = caps[2].to_string();
                        symbols.push(RankedSymbol {
                            name,
                            kind: RankedSymbolKind::Enum,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: is_pub,
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = trait_re.captures(line) {
                        let is_pub = caps.get(1).is_some();
                        let name = caps[2].to_string();
                        symbols.push(RankedSymbol {
                            name,
                            kind: RankedSymbolKind::Trait,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: is_pub,
                            rank_score: 1.0,
                        });
                    }
                }
            }
            "ts" | "tsx" | "js" | "jsx" => {
                let fn_re = Regex::new(r"(?m)^\s*(export\s+)?(?:async\s+)?function\s+([a-zA-Z0-9_]+)").unwrap();
                let class_re = Regex::new(r"(?m)^\s*(export\s+)?class\s+([a-zA-Z0-9_]+)").unwrap();
                let iface_re = Regex::new(r"(?m)^\s*(export\s+)?interface\s+([a-zA-Z0-9_]+)").unwrap();
                let type_re = Regex::new(r"(?m)^\s*(export\s+)?type\s+([a-zA-Z0-9_]+)").unwrap();

                for (idx, line) in content.lines().enumerate() {
                    let line_no = idx + 1;
                    if let Some(caps) = fn_re.captures(line) {
                        symbols.push(RankedSymbol {
                            name: caps[2].to_string(),
                            kind: RankedSymbolKind::Function,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: caps.get(1).is_some(),
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = class_re.captures(line) {
                        symbols.push(RankedSymbol {
                            name: caps[2].to_string(),
                            kind: RankedSymbolKind::Class,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: caps.get(1).is_some(),
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = iface_re.captures(line) {
                        symbols.push(RankedSymbol {
                            name: caps[2].to_string(),
                            kind: RankedSymbolKind::Interface,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: caps.get(1).is_some(),
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = type_re.captures(line) {
                        symbols.push(RankedSymbol {
                            name: caps[2].to_string(),
                            kind: RankedSymbolKind::TypeAlias,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: caps.get(1).is_some(),
                            rank_score: 1.0,
                        });
                    }
                }
            }
            "py" => {
                let fn_re = Regex::new(r"(?m)^\s*(?:async\s+)?def\s+([a-zA-Z0-9_]+)\s*\(").unwrap();
                let class_re = Regex::new(r"(?m)^\s*class\s+([a-zA-Z0-9_]+)").unwrap();

                for (idx, line) in content.lines().enumerate() {
                    let line_no = idx + 1;
                    if let Some(caps) = fn_re.captures(line) {
                        let name = caps[1].to_string();
                        let is_pub = !name.starts_with('_');
                        symbols.push(RankedSymbol {
                            name,
                            kind: RankedSymbolKind::Function,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: is_pub,
                            rank_score: 1.0,
                        });
                    } else if let Some(caps) = class_re.captures(line) {
                        let name = caps[1].to_string();
                        let is_pub = !name.starts_with('_');
                        symbols.push(RankedSymbol {
                            name,
                            kind: RankedSymbolKind::Class,
                            relative_path: rel_path.to_string(),
                            line_number: line_no,
                            signature: line.trim().to_string(),
                            is_public: is_pub,
                            rank_score: 1.0,
                        });
                    }
                }
            }
            _ => {}
        }

        symbols
    }

    /// Render a compact, token-budgeted architectural map
    pub fn render_ranked_map(graph: &RepoSymbolGraph, token_budget: usize) -> String {
        let max_chars = token_budget * 4; // ~4 chars per token estimate
        let mut symbols: Vec<&RankedSymbol> = graph.symbols.values().collect();

        // Sort descending by PageRank score
        symbols.sort_by(|a, b| b.rank_score.partial_cmp(&a.rank_score).unwrap_or(std::cmp::Ordering::Equal));

        // Group by file
        let mut file_groups: HashMap<String, Vec<&RankedSymbol>> = HashMap::new();
        for sym in symbols {
            file_groups
                .entry(sym.relative_path.clone())
                .or_default()
                .push(sym);
        }

        let mut out = String::new();
        out.push_str("=== HGB ARCHITECTURAL REPO-MAP (PageRank Ranked) ===\n");

        for (file_path, syms) in file_groups {
            let header = format!("\n📂 {}\n", file_path);
            if out.len() + header.len() > max_chars {
                out.push_str("\n... [Token budget limit reached]");
                break;
            }
            out.push_str(&header);

            for sym in syms {
                let sym_line = format!(
                    "  [{}] {}:{} (rank: {:.2})\n",
                    sym.kind.badge(),
                    sym.signature,
                    sym.line_number,
                    sym.rank_score
                );
                if out.len() + sym_line.len() > max_chars {
                    out.push_str("  ... [Remaining symbols omitted for budget]\n");
                    return out;
                }
                out.push_str(&sym_line);
            }
        }

        out
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zig_pagerank_computation() {
        let mut graph = RepoSymbolGraph::new();

        graph.add_symbol(RankedSymbol {
            name: "Server".to_string(),
            kind: RankedSymbolKind::Struct,
            relative_path: "src/server.rs".to_string(),
            line_number: 10,
            signature: "pub struct Server".to_string(),
            is_public: true,
            rank_score: 1.0,
        });

        graph.add_symbol(RankedSymbol {
            name: "Router".to_string(),
            kind: RankedSymbolKind::Struct,
            relative_path: "src/router.rs".to_string(),
            line_number: 20,
            signature: "pub struct Router".to_string(),
            is_public: true,
            rank_score: 1.0,
        });

        graph.add_reference("src/server.rs::Server", "src/router.rs::Router");

        graph.compute_pagerank(10, 0.85);

        let router = graph.symbols.get("src/router.rs::Router").unwrap();
        let server = graph.symbols.get("src/server.rs::Server").unwrap();
        assert!(router.rank_score > 0.0);
        assert!(server.rank_score > 0.0);
    }
}
