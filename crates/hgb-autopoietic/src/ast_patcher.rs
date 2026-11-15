//! # AST Isolation & Codebase Untangling (`ast_patcher`)
//!
//! Pinpoints target function AST via `syn`, extracts signatures and invariants,
//! enforces delimiter and grammar invariants, performs deterministic function
//! replacement, and bridges with `skills/untangle-codebase` to enforce strict
//! 5-tier downward DAG hierarchy and call hierarchy mapping.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::visit::Visit;
use syn::{Item, ItemFn, ReturnType, Visibility};

use hgb_core::error::{HgbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AstArgument {
    pub name: String,
    pub arg_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AstFunctionTarget {
    pub fn_name: String,
    pub file_path: PathBuf,
    pub is_async: bool,
    pub visibility: String,
    pub signature: String,
    pub inputs: Vec<AstArgument>,
    pub output_type: Option<String>,
    pub attributes: Vec<String>,
    pub full_fn_code: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchitecturalTier {
    Tier1Core,      // hgb-core, hgb-wasm-kernel, wat
    Tier2Storage,   // hgb-storage
    Tier3Nextgen,   // hgb-nextgen
    Tier4Daemon,    // hgb-daemon
    Tier5Cli,       // hgb-cli
}

impl ArchitecturalTier {
    pub fn level(&self) -> u32 {
        match self {
            Self::Tier1Core => 1,
            Self::Tier2Storage => 2,
            Self::Tier3Nextgen => 3,
            Self::Tier4Daemon => 4,
            Self::Tier5Cli => 5,
        }
    }

    pub fn from_crate_name(name: &str) -> Self {
        if name.contains("core") || name.contains("wasm-kernel") || name == "wat" {
            Self::Tier1Core
        } else if name.contains("storage") {
            Self::Tier2Storage
        } else if name.contains("nextgen") {
            Self::Tier3Nextgen
        } else if name.contains("daemon") {
            Self::Tier4Daemon
        } else {
            Self::Tier5Cli
        }
    }
}

pub struct SyntaxInvariantValidator;

impl SyntaxInvariantValidator {
    pub fn verify_delimiter_balance(source: &str) -> Result<()> {
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
                    if c == '\n' { in_line_comment = false; }
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

            if in_string || in_char || in_line_comment || in_block_comment {
                prev_char = c;
                i += 1;
                continue;
            }

            match c {
                '{' | '[' | '(' => stack.push(c),
                '}' => {
                    if stack.pop() != Some('{') {
                        return Err(HgbError::InvalidInput(format!("Unbalanced '}}' at char {}", i)));
                    }
                }
                ']' => {
                    if stack.pop() != Some('[') {
                        return Err(HgbError::InvalidInput(format!("Unbalanced ']' at char {}", i)));
                    }
                }
                ')' => {
                    if stack.pop() != Some('(') {
                        return Err(HgbError::InvalidInput(format!("Unbalanced ')' at char {}", i)));
                    }
                }
                _ => {}
            }

            prev_char = c;
            i += 1;
        }

        if !stack.is_empty() {
            let unclosed: String = stack.into_iter().collect();
            return Err(HgbError::InvalidInput(format!("Unclosed delimiters detected: {}", unclosed)));
        }

        Ok(())
    }

    pub fn verify_syn_parse(source: &str) -> Result<()> {
        syn::parse_file(source).map_err(|e| HgbError::InvalidInput(format!("Syn parse failure: {}", e)))?;
        Ok(())
    }

    pub fn verify_all(source: &str) -> Result<()> {
        Self::verify_delimiter_balance(source)?;
        Self::verify_syn_parse(source)?;
        Ok(())
    }
}

pub struct AstPatcher;

impl AstPatcher {
    pub fn isolate_function(source: &str, fn_name: &str, file_path: &Path) -> Result<AstFunctionTarget> {
        let file = syn::parse_file(source)
            .map_err(|e| HgbError::InvalidInput(format!("Failed to parse source file '{}': {}", file_path.display(), e)))?;

        for item in &file.items {
            if let Item::Fn(ref item_fn) = item {
                if item_fn.sig.ident == fn_name {
                    return Ok(Self::extract_target_from_item_fn(item_fn, file_path));
                }
            } else if let Item::Impl(ref item_impl) = item {
                for impl_item in &item_impl.items {
                    if let syn::ImplItem::Fn(ref method) = impl_item {
                        if method.sig.ident == fn_name {
                            return Ok(Self::extract_target_from_impl_fn(method, file_path));
                        }
                    }
                }
            }
        }

        Err(HgbError::NotFound(format!(
            "Target function '{}' not found in AST of '{}'",
            fn_name,
            file_path.display()
        )))
    }

    fn extract_target_from_item_fn(item_fn: &ItemFn, file_path: &Path) -> AstFunctionTarget {
        let name = item_fn.sig.ident.to_string();
        let is_async = item_fn.sig.asyncness.is_some();
        let visibility = match &item_fn.vis {
            Visibility::Public(_) => "pub".to_string(),
            Visibility::Restricted(r) => format!("pub({})", r.path.to_token_stream()),
            Visibility::Inherited => "private".to_string(),
        };

        let inputs = item_fn.sig.inputs.iter().filter_map(|arg| {
            if let syn::FnArg::Typed(pat_type) = arg {
                let pat_name = pat_type.pat.to_token_stream().to_string();
                let ty_name = pat_type.ty.to_token_stream().to_string();
                Some(AstArgument { name: pat_name, arg_type: ty_name })
            } else {
                Some(AstArgument { name: "self".to_string(), arg_type: "&self".to_string() })
            }
        }).collect();

        let output_type = match &item_fn.sig.output {
            ReturnType::Default => None,
            ReturnType::Type(_, ty) => Some(ty.to_token_stream().to_string()),
        };

        let attributes = item_fn.attrs.iter().map(|a| a.to_token_stream().to_string()).collect();
        let signature = item_fn.sig.to_token_stream().to_string();
        let full_fn_code = item_fn.to_token_stream().to_string();

        AstFunctionTarget {
            fn_name: name,
            file_path: file_path.to_path_buf(),
            is_async,
            visibility,
            signature,
            inputs,
            output_type,
            attributes,
            full_fn_code,
            start_line: 0,
            end_line: 0,
        }
    }

    fn extract_target_from_impl_fn(method: &syn::ImplItemFn, file_path: &Path) -> AstFunctionTarget {
        let name = method.sig.ident.to_string();
        let is_async = method.sig.asyncness.is_some();
        let visibility = match &method.vis {
            Visibility::Public(_) => "pub".to_string(),
            Visibility::Restricted(r) => format!("pub({})", r.path.to_token_stream()),
            Visibility::Inherited => "private".to_string(),
        };

        let inputs = method.sig.inputs.iter().filter_map(|arg| {
            if let syn::FnArg::Typed(pat_type) = arg {
                let pat_name = pat_type.pat.to_token_stream().to_string();
                let ty_name = pat_type.ty.to_token_stream().to_string();
                Some(AstArgument { name: pat_name, arg_type: ty_name })
            } else {
                Some(AstArgument { name: "self".to_string(), arg_type: "&self".to_string() })
            }
        }).collect();

        let output_type = match &method.sig.output {
            ReturnType::Default => None,
            ReturnType::Type(_, ty) => Some(ty.to_token_stream().to_string()),
        };

        let attributes = method.attrs.iter().map(|a| a.to_token_stream().to_string()).collect();
        let signature = method.sig.to_token_stream().to_string();
        let full_fn_code = method.to_token_stream().to_string();

        AstFunctionTarget {
            fn_name: name,
            file_path: file_path.to_path_buf(),
            is_async,
            visibility,
            signature,
            inputs,
            output_type,
            attributes,
            full_fn_code,
            start_line: 0,
            end_line: 0,
        }
    }

    pub fn replace_function_in_source(
        source: &str,
        fn_name: &str,
        patched_fn_str: &str,
    ) -> Result<String> {
        let replacement_fn: ItemFn = syn::parse_str(patched_fn_str)
            .map_err(|e| HgbError::InvalidInput(format!("Patched code is not a valid syn function: {}", e)))?;

        let mut file = syn::parse_file(source)
            .map_err(|e| HgbError::InvalidInput(format!("Failed to parse source file: {}", e)))?;

        let mut replaced = false;

        for item in &mut file.items {
            if let Item::Fn(ref mut func) = item {
                if func.sig.ident == fn_name {
                    *func = replacement_fn.clone();
                    replaced = true;
                    break;
                }
            } else if let Item::Impl(ref mut impl_item) = item {
                for i in &mut impl_item.items {
                    if let syn::ImplItem::Fn(ref mut method) = i {
                        if method.sig.ident == fn_name {
                            method.sig = replacement_fn.sig.clone();
                            method.block = *replacement_fn.block.clone();
                            method.vis = replacement_fn.vis.clone();
                            method.attrs = replacement_fn.attrs.clone();
                            replaced = true;
                            break;
                        }
                    }
                }
                if replaced { break; }
            }
        }

        if !replaced {
            return Err(HgbError::NotFound(format!("Function '{}' not found in source AST to replace", fn_name)));
        }

        let new_code = file.into_token_stream().to_string();
        SyntaxInvariantValidator::verify_all(&new_code)?;
        Ok(new_code)
    }

    pub fn audit_codebase_dag(
        workspace_root: &Path,
        crate_dependencies: &HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        fn has_cycle(
            node: &str,
            adj: &HashMap<String, Vec<String>>,
            visited: &mut HashSet<String>,
            rec_stack: &mut HashSet<String>,
        ) -> bool {
            visited.insert(node.to_string());
            rec_stack.insert(node.to_string());

            if let Some(neighbors) = adj.get(node) {
                for n in neighbors {
                    if !visited.contains(n) {
                        if has_cycle(n, adj, visited, rec_stack) { return true; }
                    } else if rec_stack.contains(n) {
                        return true;
                    }
                }
            }

            rec_stack.remove(node);
            false
        }

        for k in crate_dependencies.keys() {
            if !visited.contains(k) {
                if has_cycle(k, crate_dependencies, &mut visited, &mut rec_stack) {
                    return Err(HgbError::Execution(format!(
                        "ARCH-002 Violation: Circular dependency cycle detected involving crate '{}'",
                        k
                    )));
                }
            }
        }

        for (from, deps) in crate_dependencies {
            let from_tier = ArchitecturalTier::from_crate_name(from).level();
            for dep in deps {
                let dep_tier = ArchitecturalTier::from_crate_name(dep).level();
                if dep_tier > from_tier {
                    return Err(HgbError::Execution(format!(
                        "ARCH-001 Violation: Layer ordering broken. Crate '{}' (Tier {}) cannot depend upward on '{}' (Tier {})",
                        from, from_tier, dep, dep_tier
                    )));
                }
            }
        }

        Ok(())
    }
}