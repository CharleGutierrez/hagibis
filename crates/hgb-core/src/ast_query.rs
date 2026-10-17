//! # Deterministic AST Query & Refactor Engine (`ast_query`)
//!
//! Real structural AST parser, pattern-matching query engine, and deterministic
//! code rewriter with bracket & syntax invariant validation before writing back to disk.

use std::fs;
use std::path::Path;
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;
use syn::{Item, ItemFn, ItemImpl, ItemStruct, ReturnType, Type, Visibility};

/// Structural information of an extracted AST symbol
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AstSymbol {
    pub name: String,
    pub kind: String, // "fn", "method", "struct", "enum", "trait"
    pub is_async: bool,
    pub visibility: String, // "pub", "pub(crate)", "private"
    pub return_type: Option<String>,
    pub attributes: Vec<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
}

/// Visitor that collects functions, methods, and types with their structural properties
struct AstVisitor {
    symbols: Vec<AstSymbol>,
}

impl AstVisitor {
    fn new() -> Self {
        Self { symbols: Vec::new() }
    }
}

impl<'ast> Visit<'ast> for AstVisitor {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let name = item.sig.ident.to_string();
        let is_async = item.sig.asyncness.is_some();
        let visibility = match &item.vis {
            Visibility::Public(_) => "pub".to_string(),
            Visibility::Restricted(r) => {
                let p = r.path.to_token_stream().to_string();
                format!("pub({})", p)
            }
            Visibility::Inherited => "private".to_string(),
        };

        let return_type = match &item.sig.output {
            ReturnType::Default => None,
            ReturnType::Type(_, ty) => Some(ty.to_token_stream().to_string().replace(' ', "")),
        };

        let attributes: Vec<String> = item
            .attrs
            .iter()
            .map(|a| a.to_token_stream().to_string().replace(' ', ""))
            .collect();

        let sig = item.sig.to_token_stream().to_string();

        self.symbols.push(AstSymbol {
            name,
            kind: "fn".to_string(),
            is_async,
            visibility,
            return_type,
            attributes,
            start_line: 0,
            end_line: 0,
            signature: sig,
        });

        syn::visit::visit_item_fn(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        let name = item.ident.to_string();
        let visibility = match &item.vis {
            Visibility::Public(_) => "pub".to_string(),
            Visibility::Restricted(r) => format!("pub({})", r.path.to_token_stream().to_string()),
            Visibility::Inherited => "private".to_string(),
        };

        let attributes: Vec<String> = item
            .attrs
            .iter()
            .map(|a| a.to_token_stream().to_string().replace(' ', ""))
            .collect();

        self.symbols.push(AstSymbol {
            name: name.clone(),
            kind: "struct".to_string(),
            is_async: false,
            visibility,
            return_type: None,
            attributes,
            start_line: 0,
            end_line: 0,
            signature: format!("struct {}", name),
        });

        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        for impl_item in &item.items {
            if let syn::ImplItem::Fn(method) = impl_item {
                let name = method.sig.ident.to_string();
                let is_async = method.sig.asyncness.is_some();
                let visibility = match &method.vis {
                    Visibility::Public(_) => "pub".to_string(),
                    Visibility::Restricted(r) => format!("pub({})", r.path.to_token_stream().to_string()),
                    Visibility::Inherited => "private".to_string(),
                };

                let return_type = match &method.sig.output {
                    ReturnType::Default => None,
                    ReturnType::Type(_, ty) => Some(ty.to_token_stream().to_string().replace(' ', "")),
                };

                let attributes: Vec<String> = method
                    .attrs
                    .iter()
                    .map(|a| a.to_token_stream().to_string().replace(' ', ""))
                    .collect();

                let sig = method.sig.to_token_stream().to_string();

                self.symbols.push(AstSymbol {
                    name,
                    kind: "method".to_string(),
                    is_async,
                    visibility,
                    return_type,
                    attributes,
                    start_line: 0,
                    end_line: 0,
                    signature: sig,
                });
            }
        }

        syn::visit::visit_item_impl(self, item);
    }
}

/// AST Query Filter Predicate
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstQueryFilter {
    AsyncOnly,
    PublicOnly,
    PrivateOnly,
    Returns(String),       // e.g. "Result", "Option"
    HasAttribute(String),  // e.g. "test", "derive"
    NameContains(String),
    Kind(String),          // "fn", "struct", "method"
}

/// Invariant Validator for deterministic refactoring
pub struct SyntaxInvariantValidator;

impl SyntaxInvariantValidator {
    /// Verify balanced delimiters: curly braces `{}`, square brackets `[]`, parentheses `()`.
    /// Accurately skips characters inside string literals and line/block comments.
    pub fn verify_delimiter_balance(source: &str) -> Result<(), String> {
        let mut stack = Vec::new();
        let mut in_string = false;
        let mut in_char = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut prev_char = '\0';

        let chars: Vec<char> = source.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            let c = chars[i];
            let next_char = if i + 1 < len { chars[i + 1] } else { '\0' };

            // Handle comments
            if !in_string && !in_char {
                if !in_line_comment && !in_block_comment {
                    if c == '/' && next_char == '/' {
                        in_line_comment = true;
                        i += 2;
                        continue;
                    } else if c == '/' && next_char == '*' {
                        in_block_comment = true;
                        i += 2;
                        continue;
                    }
                } else if in_line_comment {
                    if c == '\n' {
                        in_line_comment = false;
                    }
                    i += 1;
                    continue;
                } else if in_block_comment {
                    if c == '*' && next_char == '/' {
                        in_block_comment = false;
                        i += 2;
                        continue;
                    }
                    i += 1;
                    continue;
                }
            }

            // Handle string and character literals
            if !in_line_comment && !in_block_comment {
                if c == '"' && prev_char != '\\' && !in_char {
                    in_string = !in_string;
                    prev_char = c;
                    i += 1;
                    continue;
                }
                if c == '\'' && prev_char != '\\' && !in_string {
                    in_char = !in_char;
                    prev_char = c;
                    i += 1;
                    continue;
                }
            }

            // If inside string or literal, skip
            if in_string || in_char || in_line_comment || in_block_comment {
                prev_char = c;
                i += 1;
                continue;
            }

            // Check bracket balancing
            match c {
                '{' | '[' | '(' => stack.push(c),
                '}' => {
                    if stack.pop() != Some('{') {
                        return Err(format!("Unbalanced closing curly brace '}}' at char index {}", i));
                    }
                }
                ']' => {
                    if stack.pop() != Some('[') {
                        return Err(format!("Unbalanced closing bracket ']' at char index {}", i));
                    }
                }
                ')' => {
                    if stack.pop() != Some('(') {
                        return Err(format!("Unbalanced closing parenthesis ')' at char index {}", i));
                    }
                }
                _ => {}
            }

            prev_char = c;
            i += 1;
        }

        if !stack.is_empty() {
            let unclosed: String = stack.into_iter().collect();
            return Err(format!("Unclosed delimiters detected: {}", unclosed));
        }

        Ok(())
    }

    /// Full syn invariant check: ensures code parses into a completely valid syn AST file
    pub fn verify_syn_parse(source: &str) -> Result<(), String> {
        syn::parse_file(source).map_err(|e| format!("Syn grammar parse invariant failure: {}", e))?;
        Ok(())
    }

    /// Complete invariant audit combining delimiter balance and full syn verification
    pub fn verify_all(source: &str) -> Result<(), String> {
        Self::verify_delimiter_balance(source)?;
        Self::verify_syn_parse(source)?;
        Ok(())
    }
}

/// Deterministic AST Query & Refactor Engine
pub struct AstQueryEngine;

impl AstQueryEngine {
    /// Parse Rust source code into collected structural symbols
    pub fn parse_source(source: &str) -> Result<Vec<AstSymbol>, String> {
        let file = syn::parse_file(source)
            .map_err(|e| format!("Failed to parse Rust source code: {}", e))?;

        let mut visitor = AstVisitor::new();
        visitor.visit_file(&file);
        Ok(visitor.symbols)
    }

    /// Parse a query string into query filters
    /// e.g. "fn:async", "fn:public", "returns:Result", "attribute:test", "name:parse"
    pub fn parse_query(query: &str) -> Vec<AstQueryFilter> {
        let mut filters = Vec::new();
        for token in query.split(|c: char| c == ' ' || c == ',' || c == ';') {
            let clean = token.trim();
            if clean.is_empty() {
                continue;
            }

            if clean == "fn:async" || clean == "async" {
                filters.push(AstQueryFilter::AsyncOnly);
            } else if clean == "fn:public" || clean == "pub" || clean == "public" {
                filters.push(AstQueryFilter::PublicOnly);
            } else if clean == "fn:private" || clean == "private" {
                filters.push(AstQueryFilter::PrivateOnly);
            } else if let Some(target) = clean.strip_prefix("returns:") {
                filters.push(AstQueryFilter::Returns(target.to_string()));
            } else if let Some(attr) = clean.strip_prefix("attribute:") {
                filters.push(AstQueryFilter::HasAttribute(attr.to_string()));
            } else if let Some(attr) = clean.strip_prefix("attr:") {
                filters.push(AstQueryFilter::HasAttribute(attr.to_string()));
            } else if let Some(name) = clean.strip_prefix("name:") {
                filters.push(AstQueryFilter::NameContains(name.to_string()));
            } else if let Some(kind) = clean.strip_prefix("kind:") {
                filters.push(AstQueryFilter::Kind(kind.to_string()));
            } else {
                filters.push(AstQueryFilter::NameContains(clean.to_string()));
            }
        }
        filters
    }

    /// Query AST symbols using pattern filters
    pub fn query_symbols(symbols: &[AstSymbol], filters: &[AstQueryFilter]) -> Vec<AstSymbol> {
        symbols
            .iter()
            .filter(|sym| {
                for filter in filters {
                    match filter {
                        AstQueryFilter::AsyncOnly => {
                            if !sym.is_async {
                                return false;
                            }
                        }
                        AstQueryFilter::PublicOnly => {
                            if !sym.visibility.starts_with("pub") {
                                return false;
                            }
                        }
                        AstQueryFilter::PrivateOnly => {
                            if sym.visibility != "private" {
                                return false;
                            }
                        }
                        AstQueryFilter::Returns(expected) => {
                            match &sym.return_type {
                                Some(ret) if ret.contains(expected) => {}
                                _ => return false,
                            }
                        }
                        AstQueryFilter::HasAttribute(attr) => {
                            if !sym.attributes.iter().any(|a| a.contains(attr)) {
                                return false;
                            }
                        }
                        AstQueryFilter::NameContains(pattern) => {
                            if !sym.name.to_lowercase().contains(&pattern.to_lowercase()) {
                                return false;
                            }
                        }
                        AstQueryFilter::Kind(expected_kind) => {
                            if &sym.kind != expected_kind {
                                return false;
                            }
                        }
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Safely refactor function visibility across source code with invariant guarantee
    pub fn make_function_public(source: &str, fn_name: &str) -> Result<String, String> {
        let mut file = syn::parse_file(source)
            .map_err(|e| format!("Parse error: {}", e))?;

        let mut modified = false;
        for item in &mut file.items {
            if let Item::Fn(ref mut func) = item {
                if func.sig.ident == fn_name {
                    func.vis = Visibility::Public(syn::token::Pub::default());
                    modified = true;
                }
            } else if let Item::Impl(ref mut impl_item) = item {
                for i in &mut impl_item.items {
                    if let syn::ImplItem::Fn(ref mut func) = i {
                        if func.sig.ident == fn_name {
                            func.vis = Visibility::Public(syn::token::Pub::default());
                            modified = true;
                        }
                    }
                }
            }
        }

        if !modified {
            return Err(format!("Function '{}' not found in source AST", fn_name));
        }

        let new_code = file.into_token_stream().to_string();

        // Invariant enforcement before committing
        SyntaxInvariantValidator::verify_all(&new_code)?;

        Ok(new_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CODE: &str = r#"
        #[derive(Debug, Clone)]
        pub struct AudioBuffer {
            pub rate: u32,
        }

        #[test]
        async fn test_process_stream() -> Result<(), String> {
            Ok(())
        }

        pub async fn connect_remote() -> Result<bool, String> {
            Ok(true)
        }

        fn internal_helper(x: i32) -> i32 {
            x * 2
        }

        impl AudioBuffer {
            pub fn sample_rate(&self) -> u32 {
                self.rate
            }

            async fn flush(&mut self) -> Result<(), String> {
                Ok(())
            }
        }
    "#;

    #[test]
    fn test_ast_symbol_extraction() {
        let symbols = AstQueryEngine::parse_source(SAMPLE_CODE).expect("Failed to parse sample code");
        assert!(symbols.len() >= 5);

        let names: Vec<String> = symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"AudioBuffer".to_string()));
        assert!(names.contains(&"test_process_stream".to_string()));
        assert!(names.contains(&"connect_remote".to_string()));
        assert!(names.contains(&"internal_helper".to_string()));
        assert!(names.contains(&"sample_rate".to_string()));
        assert!(names.contains(&"flush".to_string()));
    }

    #[test]
    fn test_pattern_matching_queries() {
        let symbols = AstQueryEngine::parse_source(SAMPLE_CODE).expect("Parse failed");

        // 1. fn:async
        let async_filters = AstQueryEngine::parse_query("fn:async");
        let async_fns = AstQueryEngine::query_symbols(&symbols, &async_filters);
        assert_eq!(async_fns.len(), 3); // test_process_stream, connect_remote, flush

        // 2. fn:public
        let pub_filters = AstQueryEngine::parse_query("fn:public");
        let pub_items = AstQueryEngine::query_symbols(&symbols, &pub_filters);
        let pub_names: Vec<String> = pub_items.iter().map(|s| s.name.clone()).collect();
        assert!(pub_names.contains(&"connect_remote".to_string()));
        assert!(pub_names.contains(&"sample_rate".to_string()));
        assert!(pub_names.contains(&"AudioBuffer".to_string()));

        // 3. returns:Result
        let res_filters = AstQueryEngine::parse_query("returns:Result");
        let res_fns = AstQueryEngine::query_symbols(&symbols, &res_filters);
        assert_eq!(res_fns.len(), 3);

        // 4. Combined query: fn:async, returns:Result, fn:public
        let comb_filters = AstQueryEngine::parse_query("fn:async, returns:Result, fn:public");
        let comb_fns = AstQueryEngine::query_symbols(&symbols, &comb_filters);
        assert_eq!(comb_fns.len(), 1);
        assert_eq!(comb_fns[0].name, "connect_remote");
    }

    #[test]
    fn test_delimiter_and_syntax_invariants() {
        let valid_code = r#"
            fn add(a: i32, b: i32) -> i32 {
                let text = "Unbalanced { [ ( in string is fine";
                // Unbalanced { in comment is fine
                a + b
            }
        "#;
        assert!(SyntaxInvariantValidator::verify_all(valid_code).is_ok());

        let broken_braces = r#"
            fn broken() {
                let x = 10;
            // Missing closing brace
        "#;
        assert!(SyntaxInvariantValidator::verify_delimiter_balance(broken_braces).is_err());

        let broken_mismatch = r#"
            fn mismatch() {
                let arr = [1, 2, 3);
            }
        "#;
        assert!(SyntaxInvariantValidator::verify_delimiter_balance(broken_mismatch).is_err());
    }

    #[test]
    fn test_deterministic_refactor_make_public() {
        let input = r#"
            fn private_function() -> u32 {
                42
            }
        "#;
        let modified = AstQueryEngine::make_function_public(input, "private_function")
            .expect("Refactoring failed");

        assert!(modified.contains("pub fn private_function"));
        assert!(SyntaxInvariantValidator::verify_all(&modified).is_ok());
    }
}
