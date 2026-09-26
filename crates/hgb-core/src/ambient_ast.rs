use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Ambient AST context representation for model prompt injection and TUI canvas display
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmbientContext {
    pub file_path: String,
    pub cursor_line: usize,
    pub enclosing_symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub context_snippet: String,
    pub imports: Vec<String>,
}

/// Discovered symbol boundary within a source file
#[derive(Debug, Clone)]
struct SymbolBoundary {
    name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
}

/// Ambient AST Follower tracking developer cursor and enclosing syntactic structures
#[derive(Debug, Clone)]
pub struct AmbientAstFollower {
    workspace_root: PathBuf,
    active_file: Option<PathBuf>,
    cursor_line: usize,
}

impl AmbientAstFollower {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            active_file: None,
            cursor_line: 1,
        }
    }

    /// Set or update the active file and cursor line
    pub fn set_focus(&mut self, file_path: impl Into<PathBuf>, line: usize) {
        let p = file_path.into();
        self.active_file = Some(p);
        self.cursor_line = line.max(1);
    }

    pub fn active_file(&self) -> Option<&Path> {
        self.active_file.as_deref()
    }

    pub fn cursor_line(&self) -> usize {
        self.cursor_line
    }

    /// Query the ambient context for the current focus
    pub fn get_ambient_context(&self) -> AmbientContext {
        let active = match &self.active_file {
            Some(p) => p,
            None => {
                return AmbientContext {
                    file_path: String::new(),
                    cursor_line: self.cursor_line,
                    enclosing_symbol: None,
                    symbol_kind: None,
                    context_snippet: String::new(),
                    imports: Vec::new(),
                };
            }
        };

        let resolved_path = if active.is_absolute() {
            active.clone()
        } else {
            self.workspace_root.join(active)
        };

        let content = fs::read_to_string(&resolved_path).unwrap_or_default();
        Self::analyze_content(active, &content, self.cursor_line)
    }

    /// Render ambient anchor tag for prompt injection: `<ambient_context>...</ambient_context>`
    pub fn render_ambient_anchor(&self, max_tokens: usize) -> String {
        let ctx = self.get_ambient_context();
        if ctx.file_path.is_empty() && ctx.context_snippet.is_empty() {
            return String::new();
        }

        let max_chars = (max_tokens * 4).max(200);
        let mut out = String::with_capacity(1024);
        out.push_str("<ambient_context>\n");
        out.push_str(&format!("Active File: {} (Line {})\n", ctx.file_path, ctx.cursor_line));

        if let (Some(sym), Some(kind)) = (&ctx.enclosing_symbol, &ctx.symbol_kind) {
            out.push_str(&format!("Enclosing Symbol: {} ({})\n", sym, kind));
        }

        if !ctx.imports.is_empty() {
            out.push_str("Key Imports:\n");
            for imp in ctx.imports.iter().take(8) {
                out.push_str(&format!("  {}\n", imp));
            }
        }

        out.push_str("Context Snippet:\n");
        let snippet = if ctx.context_snippet.len() > max_chars {
            let truncated = &ctx.context_snippet[..max_chars];
            format!("{}\n... [truncated]", truncated)
        } else {
            ctx.context_snippet.clone()
        };
        out.push_str(&snippet);
        out.push_str("\n</ambient_context>\n");

        out
    }

    /// Analyze arbitrary file content to extract imports and enclosing symbol at line
    pub fn analyze_content(file_path: &Path, content: &str, cursor_line: usize) -> AmbientContext {
        let file_ext = file_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        let lines: Vec<&str> = content.lines().collect();
        let imports = Self::extract_imports(&lines, &file_ext);
        let symbols = Self::extract_symbols(&lines, &file_ext);

        // Find the most specific (innermost) symbol enclosing cursor_line
        let mut enclosing: Option<SymbolBoundary> = None;
        for sym in symbols {
            if cursor_line >= sym.start_line && cursor_line <= sym.end_line {
                match &enclosing {
                    Some(prev) => {
                        let prev_span = prev.end_line.saturating_sub(prev.start_line);
                        let cur_span = sym.end_line.saturating_sub(sym.start_line);
                        if cur_span <= prev_span {
                            enclosing = Some(sym);
                        }
                    }
                    None => {
                        enclosing = Some(sym);
                    }
                }
            }
        }

        let (enclosing_symbol, symbol_kind, context_snippet) = if let Some(sym) = enclosing {
            let start_idx = sym.start_line.saturating_sub(1);
            let end_idx = sym.end_line.min(lines.len());
            let snippet = lines[start_idx..end_idx].join("\n");
            (Some(sym.name), Some(sym.kind), snippet)
        } else {
            // Fallback: window around cursor line
            let start = cursor_line.saturating_sub(6).max(1);
            let end = (cursor_line + 6).min(lines.len());
            let snippet = if !lines.is_empty() {
                lines[start.saturating_sub(1)..end].join("\n")
            } else {
                String::new()
            };
            (None, None, snippet)
        };

        AmbientContext {
            file_path: file_path.to_string_lossy().to_string(),
            cursor_line,
            enclosing_symbol,
            symbol_kind,
            context_snippet,
            imports,
        }
    }

    fn extract_imports(lines: &[&str], ext: &str) -> Vec<String> {
        let mut imports = Vec::new();
        match ext {
            "rs" => {
                let use_re = Regex::new(r"^\s*pub(?:\([^\)]+\))?\s+use\s+(.+);|^\s*use\s+(.+);").unwrap();
                for line in lines.iter().take(150) {
                    let trimmed = line.trim();
                    if let Some(caps) = use_re.captures(trimmed) {
                        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
                            imports.push(format!("use {};", m.as_str()));
                        }
                    }
                }
            }
            "ts" | "tsx" | "js" | "jsx" => {
                for line in lines.iter().take(150) {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ") {
                        imports.push(trimmed.to_string());
                    }
                }
            }
            "py" => {
                for line in lines.iter().take(150) {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                        imports.push(trimmed.to_string());
                    }
                }
            }
            "go" => {
                let mut in_import_block = false;
                for line in lines.iter().take(150) {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import (") {
                        in_import_block = true;
                    } else if in_import_block {
                        if trimmed == ")" {
                            in_import_block = false;
                        } else if !trimmed.is_empty() {
                            imports.push(trimmed.to_string());
                        }
                    } else if trimmed.starts_with("import ") {
                        imports.push(trimmed.to_string());
                    }
                }
            }
            _ => {}
        }
        imports
    }

    fn extract_symbols(lines: &[&str], ext: &str) -> Vec<SymbolBoundary> {
        let mut symbols = Vec::new();
        if ext == "py" {
            Self::extract_python_symbols(lines, &mut symbols);
        } else {
            Self::extract_brace_symbols(lines, ext, &mut symbols);
        }
        symbols
    }

    fn extract_brace_symbols(lines: &[&str], ext: &str, symbols: &mut Vec<SymbolBoundary>) {
        let fn_re = match ext {
            "rs" => Regex::new(r"^\s*(?:pub(?:\([^\)]+\))?\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)").unwrap(),
            "go" => Regex::new(r"^\s*func\s+(?:\([^)]+\)\s+)?([A-Za-z0-9_]+)").unwrap(),
            _ => Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z0-9_]+)|^\s*(?:pub(?:\([^\)]+\))?\s+)?([A-Za-z0-9_]+)\s*\([^)]*\)\s*\{").unwrap(),
        };

        let struct_re = match ext {
            "rs" => Regex::new(r"^\s*(?:pub(?:\([^\)]+\))?\s+)?struct\s+([A-Za-z0-9_]+)").unwrap(),
            "go" => Regex::new(r"^\s*type\s+([A-Za-z0-9_]+)\s+struct").unwrap(),
            _ => Regex::new(r"^\s*(?:export\s+)?class\s+([A-Za-z0-9_]+)|^\s*(?:export\s+)?interface\s+([A-Za-z0-9_]+)").unwrap(),
        };

        let impl_re = Regex::new(r"^\s*impl(?:<[^>]+>)?\s+(?:[A-Za-z0-9_:]+\s+for\s+)?([A-Za-z0-9_:]+)").unwrap();
        let enum_re = Regex::new(r"^\s*(?:pub(?:\([^\)]+\))?\s+)?enum\s+([A-Za-z0-9_]+)").unwrap();
        let trait_re = Regex::new(r"^\s*(?:pub(?:\([^\)]+\))?\s+)?trait\s+([A-Za-z0-9_]+)").unwrap();

        for i in 0..lines.len() {
            let line = lines[i];
            let line_no = i + 1;
            let trimmed = line.trim();

            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }

            let mut matched: Option<(String, String)> = None;

            if let Some(caps) = fn_re.captures(trimmed) {
                let name = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str().to_string()).unwrap_or_default();
                matched = Some((name, "function".to_string()));
            } else if let Some(caps) = struct_re.captures(trimmed) {
                let name = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str().to_string()).unwrap_or_default();
                matched = Some((name, "struct".to_string()));
            } else if ext == "rs" {
                if let Some(caps) = impl_re.captures(trimmed) {
                    let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                    matched = Some((name, "impl".to_string()));
                } else if let Some(caps) = enum_re.captures(trimmed) {
                    let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                    matched = Some((name, "enum".to_string()));
                } else if let Some(caps) = trait_re.captures(trimmed) {
                    let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                    matched = Some((name, "trait".to_string()));
                }
            }

            if let Some((name, kind)) = matched {
                // Find matching closing brace
                let mut brace_depth: i32 = 0;
                let mut found_open = false;
                let mut end_line = line_no;

                for j in i..lines.len() {
                    let l = lines[j];
                    for ch in l.chars() {
                        if ch == '{' {
                            brace_depth += 1;
                            found_open = true;
                        } else if ch == '}' {
                            brace_depth -= 1;
                        }
                    }
                    if found_open && brace_depth <= 0 {
                        end_line = j + 1;
                        break;
                    }
                }

                if !found_open {
                    end_line = (line_no + 10).min(lines.len());
                }

                symbols.push(SymbolBoundary {
                    name,
                    kind,
                    start_line: line_no,
                    end_line,
                });
            }
        }
    }

    fn extract_python_symbols(lines: &[&str], symbols: &mut Vec<SymbolBoundary>) {
        let fn_re = Regex::new(r"^(\s*)def\s+([A-Za-z0-9_]+)\s*\(").unwrap();
        let class_re = Regex::new(r"^(\s*)class\s+([A-Za-z0-9_]+)").unwrap();

        for i in 0..lines.len() {
            let line = lines[i];
            let line_no = i + 1;

            let mut matched: Option<(String, String, usize)> = None;

            if let Some(caps) = fn_re.captures(line) {
                let indent = caps.get(1).map(|m| m.as_str().len()).unwrap_or(0);
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                matched = Some((name, "function".to_string(), indent));
            } else if let Some(caps) = class_re.captures(line) {
                let indent = caps.get(1).map(|m| m.as_str().len()).unwrap_or(0);
                let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                matched = Some((name, "class".to_string(), indent));
            }

            if let Some((name, kind, indent)) = matched {
                let mut end_line = line_no;
                for j in (i + 1)..lines.len() {
                    let l = lines[j];
                    let trimmed = l.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    let cur_indent = l.chars().take_while(|c| c.is_whitespace()).count();
                    if cur_indent <= indent {
                        break;
                    }
                    end_line = j + 1;
                }

                symbols.push(SymbolBoundary {
                    name,
                    kind,
                    start_line: line_no,
                    end_line,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_ambient_ast_rust_function_analysis() {
        let code = r#"use std::collections::HashMap;
use std::sync::Arc;

pub struct Engine {
    pub id: String,
}

impl Engine {
    pub fn process_event(&mut self, ev: u32) -> bool {
        let x = ev * 2;
        x > 10
    }
}
"#;
        let ctx = AmbientAstFollower::analyze_content(Path::new("src/engine.rs"), code, 11);
        assert_eq!(ctx.file_path, "src/engine.rs");
        assert_eq!(ctx.cursor_line, 11);
        assert_eq!(ctx.enclosing_symbol.as_deref(), Some("process_event"));
        assert_eq!(ctx.symbol_kind.as_deref(), Some("function"));
        assert!(ctx.context_snippet.contains("process_event"));
        assert_eq!(ctx.imports.len(), 2);
        assert!(ctx.imports[0].contains("use std::collections::HashMap;"));
    }

    #[test]
    fn test_render_ambient_anchor() {
        let mut follower = AmbientAstFollower::new(PathBuf::from("/tmp"));
        let code = "fn run() {\n    let val = 42;\n}\n";
        let ctx = AmbientAstFollower::analyze_content(Path::new("src/main.rs"), code, 2);
        follower.set_focus("src/main.rs", 2);

        let _anchor = follower.render_ambient_anchor(500);
        assert!(ctx.context_snippet.contains("let val = 42"));
    }
}
