use crate::error::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Classification of discovered code symbol
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Struct,
    Enum,
    Trait,
    Function,
    TypeAlias,
    Interface,
    Class,
    Constant,
}

impl SymbolKind {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Function => "fn",
            Self::TypeAlias => "type",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Constant => "const",
        }
    }
}

/// An individual parsed code symbol
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line_number: usize,
    pub signature: String,
    pub is_public: bool,
}

/// Collection of symbols in a file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSymbols {
    pub relative_path: String,
    pub symbols: Vec<CodeSymbol>,
}

/// Syntactic RepoMap Outline Report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMapReport {
    pub content: String,
    pub symbol_count: usize,
    pub file_count: usize,
}

/// High-Speed AST & Syntactic RepoMap Generator
pub struct RepoMap;

impl RepoMap {
    /// Parse symbols from content based on file extension
    pub fn parse_file_symbols(content: &str, file_ext: &str) -> Vec<CodeSymbol> {
        let mut symbols = Vec::new();

        match file_ext {
            "rs" => Self::parse_rust_symbols(content, &mut symbols),
            "ts" | "tsx" | "js" | "jsx" => Self::parse_ts_symbols(content, &mut symbols),
            "py" => Self::parse_python_symbols(content, &mut symbols),
            "go" => Self::parse_go_symbols(content, &mut symbols),
            _ => {}
        }

        symbols
    }

    fn parse_rust_symbols(content: &str, symbols: &mut Vec<CodeSymbol>) {
        // Fast regex matchers for Rust syntax
        let struct_re = Regex::new(r"^\s*(pub(?:\([^\)]+\))?\s+)?struct\s+([A-Za-z0-9_]+)").unwrap();
        let enum_re = Regex::new(r"^\s*(pub(?:\([^\)]+\))?\s+)?enum\s+([A-Za-z0-9_]+)").unwrap();
        let trait_re = Regex::new(r"^\s*(pub(?:\([^\)]+\))?\s+)?trait\s+([A-Za-z0-9_]+)").unwrap();
        let fn_re = Regex::new(r"^\s*(pub(?:\([^\)]+\))?\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)\s*(<[^>]+>)?\s*\((.*?)\)").unwrap();
        let type_re = Regex::new(r"^\s*(pub(?:\([^\)]+\))?\s+)?type\s+([A-Za-z0-9_]+)").unwrap();

        for (idx, line) in content.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();

            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }

            if let Some(caps) = struct_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Struct,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = enum_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Enum,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = trait_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Trait,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = fn_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Function,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = type_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::TypeAlias,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            }
        }
    }

    fn parse_ts_symbols(content: &str, symbols: &mut Vec<CodeSymbol>) {
        let iface_re = Regex::new(r"^\s*(export\s+)?interface\s+([A-Za-z0-9_]+)").unwrap();
        let class_re = Regex::new(r"^\s*(export\s+)?class\s+([A-Za-z0-9_]+)").unwrap();
        let type_re = Regex::new(r"^\s*(export\s+)?type\s+([A-Za-z0-9_]+)").unwrap();
        let fn_re = Regex::new(r"^\s*(export\s+)?(?:async\s+)?function\s+([A-Za-z0-9_]+)").unwrap();

        for (idx, line) in content.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();

            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }

            if let Some(caps) = iface_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Interface,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = class_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Class,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = type_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::TypeAlias,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            } else if let Some(caps) = fn_re.captures(line) {
                let is_pub = caps.get(1).is_some();
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Function,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: is_pub,
                });
            }
        }
    }

    fn parse_python_symbols(content: &str, symbols: &mut Vec<CodeSymbol>) {
        let class_re = Regex::new(r"^\s*class\s+([A-Za-z0-9_]+)").unwrap();
        let def_re = Regex::new(r"^\s*(?:async\s+)?def\s+([A-Za-z0-9_]+)").unwrap();

        for (idx, line) in content.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();

            if trimmed.starts_with('#') {
                continue;
            }

            if let Some(caps) = class_re.captures(line) {
                let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Class,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public: true,
                });
            } else if let Some(caps) = def_re.captures(line) {
                let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                let is_public = !name.starts_with('_');
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Function,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public,
                });
            }
        }
    }

    fn parse_go_symbols(content: &str, symbols: &mut Vec<CodeSymbol>) {
        let type_struct_re = Regex::new(r"^\s*type\s+([A-Za-z0-9_]+)\s+struct").unwrap();
        let type_iface_re = Regex::new(r"^\s*type\s+([A-Za-z0-9_]+)\s+interface").unwrap();
        let func_re = Regex::new(r"^\s*func\s+(?:\([^\)]+\)\s+)?([A-Za-z0-9_]+)\s*\(").unwrap();

        for (idx, line) in content.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();

            if trimmed.starts_with("//") {
                continue;
            }

            if let Some(caps) = type_struct_re.captures(line) {
                let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                let is_public = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Struct,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public,
                });
            } else if let Some(caps) = type_iface_re.captures(line) {
                let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                let is_public = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Interface,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public,
                });
            } else if let Some(caps) = func_re.captures(line) {
                let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                let is_public = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Function,
                    line_number,
                    signature: trimmed.to_string(),
                    is_public,
                });
            }
        }
    }

    /// Walk workspace and generate concise, token-efficient RepoMap
    pub fn generate_map<P: AsRef<Path>>(workspace_root: P, max_files: Option<usize>) -> Result<RepoMapReport> {
        let root = workspace_root.as_ref();
        let mut file_symbols_list: Vec<FileSymbols> = Vec::new();
        let limit = max_files.unwrap_or(150);

        Self::collect_files(root, root, &mut file_symbols_list, limit)?;

        file_symbols_list.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        let mut out = String::new();
        let mut total_symbols = 0;
        let file_count = file_symbols_list.len();

        out.push_str("<repomap>\n");
        for file in &file_symbols_list {
            if file.symbols.is_empty() {
                continue;
            }
            out.push_str(&format!("{}:\n", file.relative_path));
            for sym in &file.symbols {
                total_symbols += 1;
                let pub_marker = if sym.is_public { "pub " } else { "" };
                out.push_str(&format!("  L{:<4} {}{} {}\n", sym.line_number, pub_marker, sym.kind.badge(), sym.name));
            }
            out.push('\n');
        }
        out.push_str("</repomap>\n");

        Ok(RepoMapReport {
            content: out,
            symbol_count: total_symbols,
            file_count,
        })
    }

    fn collect_files(
        root: &Path,
        current_dir: &Path,
        results: &mut Vec<FileSymbols>,
        max_files: usize,
    ) -> Result<()> {
        if results.len() >= max_files {
            return Ok(());
        }

        if let Ok(entries) = fs::read_dir(current_dir) {
            let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();

            for path in paths {
                if results.len() >= max_files {
                    break;
                }

                let fname = path.file_name().unwrap_or_default().to_string_lossy();

                // Skip common large/vendor directories
                if fname.starts_with('.')
                    || fname == "target"
                    || fname == "node_modules"
                    || fname == "dist"
                    || fname == "build"
                    || fname == "vendor"
                {
                    continue;
                }

                if path.is_dir() {
                    Self::collect_files(root, &path, results, max_files)?;
                } else if path.is_file() {
                    let ext = path.extension().unwrap_or_default().to_string_lossy().to_string();
                    if matches!(ext.as_str(), "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go") {
                        if let Ok(content) = fs::read_to_string(&path) {
                            let symbols = Self::parse_file_symbols(&content, &ext);
                            if !symbols.is_empty() {
                                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
                                results.push(FileSymbols {
                                    relative_path: rel,
                                    symbols,
                                });
                            }
                        }
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
    fn test_rust_symbol_extraction() {
        let code = r#"
            pub struct EngineConfig {
                pub timeout: u64,
            }

            pub enum Status {
                Active,
                Inactive,
            }

            pub trait Runner {
                fn run(&self);
            }

            pub async fn start_server(port: u16) -> Result<()> {
                Ok(())
            }

            fn internal_helper() {}
        "#;

        let symbols = RepoMap::parse_file_symbols(code, "rs");
        assert_eq!(symbols.len(), 6);
        assert_eq!(symbols[0].name, "EngineConfig");
        assert_eq!(symbols[0].kind, SymbolKind::Struct);
        assert!(symbols[0].is_public);

        assert_eq!(symbols[1].name, "Status");
        assert_eq!(symbols[1].kind, SymbolKind::Enum);

        assert_eq!(symbols[2].name, "Runner");
        assert_eq!(symbols[2].kind, SymbolKind::Trait);

        assert_eq!(symbols[3].name, "run");
        assert_eq!(symbols[3].kind, SymbolKind::Function);

        assert_eq!(symbols[4].name, "start_server");
        assert_eq!(symbols[4].kind, SymbolKind::Function);
        assert!(symbols[4].is_public);

        assert_eq!(symbols[5].name, "internal_helper");
        assert_eq!(symbols[5].kind, SymbolKind::Function);
        assert!(!symbols[5].is_public);
    }
}
