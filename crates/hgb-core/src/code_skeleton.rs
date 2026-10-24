//! # AST Code Skeletonizer & Context Token Optimizer (`code_skeleton`)
//!
//! Extracts struct definitions, trait definitions, and function signatures while
//! replacing function bodies with `{ ... }`, cutting prompt context token usage by 70%+.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkeletonReport {
    pub original_chars: usize,
    pub skeleton_chars: usize,
    pub original_lines: usize,
    pub skeleton_lines: usize,
    pub token_reduction_pct: f32,
    pub structs_count: usize,
    pub traits_count: usize,
    pub functions_count: usize,
    pub skeleton_code: String,
}

pub struct CodeSkeletonizer;

impl CodeSkeletonizer {
    pub fn skeletonize(source_code: &str, file_ext: &str) -> SkeletonReport {
        let ext = file_ext.trim_start_matches('.').to_lowercase();
        match ext.as_str() {
            "rs" => Self::skeletonize_rust(source_code),
            "py" => Self::skeletonize_python(source_code),
            "ts" | "tsx" | "js" | "jsx" => Self::skeletonize_js_ts(source_code),
            "zig" => Self::skeletonize_zig(source_code),
            "go" => Self::skeletonize_go(source_code),
            _ => Self::skeletonize_generic(source_code),
        }
    }

    pub fn skeletonize_rust(source_code: &str) -> SkeletonReport {
        if let Ok(file) = syn::parse_file(source_code) {
            let mut structs_count = 0;
            let mut traits_count = 0;
            let mut functions_count = 0;

            let mut out = String::new();

            for item in &file.items {
                match item {
                    syn::Item::Struct(s) => {
                        structs_count += 1;
                        out.push_str(&quote::quote!(#s).to_string());
                        out.push_str("\n\n");
                    }
                    syn::Item::Trait(t) => {
                        traits_count += 1;
                        out.push_str(&quote::quote!(#t).to_string());
                        out.push_str("\n\n");
                    }
                    syn::Item::Enum(e) => {
                        structs_count += 1;
                        out.push_str(&quote::quote!(#e).to_string());
                        out.push_str("\n\n");
                    }
                    syn::Item::Type(t) => {
                        out.push_str(&quote::quote!(#t).to_string());
                        out.push_str("\n\n");
                    }
                    syn::Item::Use(u) => {
                        out.push_str(&quote::quote!(#u).to_string());
                        out.push_str("\n");
                    }
                    syn::Item::Fn(f) => {
                        functions_count += 1;
                        let sig = &f.sig;
                        let vis = &f.vis;
                        let attrs = &f.attrs;
                        let attr_str = if attrs.is_empty() {
                            String::new()
                        } else {
                            format!("{}\n", quote::quote!(#(#attrs)*))
                        };
                        let fn_header = quote::quote!(#vis #sig).to_string();
                        out.push_str(&format!("{}{} {{ ... }}\n\n", attr_str, fn_header));
                    }
                    syn::Item::Impl(imp) => {
                        let mut impl_lines = Vec::new();
                        for impl_item in &imp.items {
                            match impl_item {
                                syn::ImplItem::Fn(m) => {
                                    functions_count += 1;
                                    let sig = &m.sig;
                                    let vis = &m.vis;
                                    let attrs = &m.attrs;
                                    let attr_str = if attrs.is_empty() {
                                        String::new()
                                    } else {
                                        format!("    {}\n", quote::quote!(#(#attrs)*))
                                    };
                                    let method_header = quote::quote!(#vis #sig).to_string();
                                    impl_lines.push(format!("{}    {} {{ ... }}", attr_str, method_header));
                                }
                                syn::ImplItem::Type(t) => {
                                    impl_lines.push(format!("    {}", quote::quote!(#t)));
                                }
                                syn::ImplItem::Const(c) => {
                                    impl_lines.push(format!("    {}", quote::quote!(#c)));
                                }
                                other => {
                                    impl_lines.push(format!("    {}", quote::quote!(#other)));
                                }
                            }
                        }

                        let attrs = &imp.attrs;
                        let defaultness = &imp.defaultness;
                        let unsafety = &imp.unsafety;
                        let generics = &imp.generics;
                        let trait_ref = imp.trait_.as_ref().map(|(_, tr, _)| quote::quote!(#tr for ));
                        let self_ty = &imp.self_ty;

                        let attr_str = if attrs.is_empty() {
                            String::new()
                        } else {
                            format!("{}\n", quote::quote!(#(#attrs)*))
                        };
                        let impl_header = format!("{}{}impl {} {}{}", defaultness.as_ref().map(|_| "default ").unwrap_or(""), unsafety.as_ref().map(|_| "unsafe ").unwrap_or(""), quote::quote!(#generics), trait_ref.unwrap_or_default(), quote::quote!(#self_ty));
                        out.push_str(&format!("{}{}{} {{\n{}\n}}\n\n", attr_str, impl_header.trim(), if imp.generics.where_clause.is_some() { format!(" {}", quote::quote!(#imp.generics.where_clause)) } else { String::new() }, impl_lines.join("\n")));
                    }
                    syn::Item::Mod(m) => {
                        if m.content.is_none() {
                            out.push_str(&quote::quote!(#m).to_string());
                            out.push_str("\n");
                        } else {
                            let vis = &m.vis;
                            let ident = &m.ident;
                            out.push_str(&format!("{} mod {} {{ ... }}\n\n", quote::quote!(#vis), ident));
                        }
                    }
                    syn::Item::Const(c) => {
                        out.push_str(&quote::quote!(#c).to_string());
                        out.push_str("\n");
                    }
                    syn::Item::Static(s) => {
                        out.push_str(&quote::quote!(#s).to_string());
                        out.push_str("\n");
                    }
                    other => {
                        out.push_str(&quote::quote!(#other).to_string());
                        out.push_str("\n");
                    }
                }
            }

            let skeleton_code = out.trim().to_string();
            let original_chars = source_code.len();
            let skeleton_chars = skeleton_code.len();
            let original_lines = source_code.lines().count();
            let skeleton_lines = skeleton_code.lines().count();

            let reduction = if original_chars > 0 {
                ((original_chars.saturating_sub(skeleton_chars)) as f32 / original_chars as f32) * 100.0
            } else {
                0.0
            };

            return SkeletonReport {
                original_chars,
                skeleton_chars,
                original_lines,
                skeleton_lines,
                token_reduction_pct: reduction,
                structs_count,
                traits_count,
                functions_count,
                skeleton_code,
            };
        }

        Self::skeletonize_brace_language(source_code, "rs")
    }

    fn skeletonize_brace_language(source_code: &str, lang: &str) -> SkeletonReport {
        let lines: Vec<&str> = source_code.lines().collect();
        let original_lines = lines.len();
        let original_chars = source_code.len();

        let mut output_lines = Vec::new();
        let mut in_fn_body = false;
        let mut brace_depth: i32 = 0;
        let mut fn_brace_start_depth = 0;

        let mut structs_count = 0;
        let mut traits_count = 0;
        let mut functions_count = 0;

        let mut current_fn_sig = String::new();
        let mut collecting_fn_sig = false;

        for line in lines {
            let trimmed = line.trim();

            if !in_fn_body && !collecting_fn_sig {
                if trimmed.starts_with("struct ") || trimmed.starts_with("pub struct ") || trimmed.contains("type ") && trimmed.contains("struct {") {
                    structs_count += 1;
                } else if trimmed.starts_with("trait ") || trimmed.starts_with("pub trait ") || trimmed.starts_with("interface ") || trimmed.starts_with("export interface ") {
                    traits_count += 1;
                }

                let is_fn_start = match lang {
                    "rs" => trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ") || trimmed.starts_with("pub(crate) fn ") || trimmed.starts_with("async fn ") || trimmed.starts_with("pub async fn ") || trimmed.starts_with("const fn ") || trimmed.starts_with("unsafe fn "),
                    "ts" | "js" => trimmed.starts_with("function ") || trimmed.starts_with("export function ") || trimmed.starts_with("export async function ") || trimmed.starts_with("async function ") || (trimmed.contains('(') && trimmed.ends_with('{') && !trimmed.starts_with("if ") && !trimmed.starts_with("for ") && !trimmed.starts_with("while ")),
                    "zig" => trimmed.starts_with("fn ") || trimmed.starts_with("pub fn "),
                    "go" => trimmed.starts_with("func "),
                    _ => trimmed.starts_with("fn ") || trimmed.starts_with("function ") || trimmed.starts_with("func "),
                };

                if is_fn_start {
                    functions_count += 1;
                    if let Some(brace_idx) = line.find('{') {
                        let sig = line[..brace_idx].trim_end();
                        output_lines.push(format!("{} {{ ... }}", sig));
                        let open_cnt = line.chars().filter(|&c| c == '{').count() as i32;
                        let close_cnt = line.chars().filter(|&c| c == '}').count() as i32;
                        brace_depth += open_cnt - close_cnt;
                        if brace_depth > 0 {
                            in_fn_body = true;
                            fn_brace_start_depth = brace_depth - (open_cnt - close_cnt);
                        }
                        continue;
                    } else if trimmed.ends_with(';') {
                        output_lines.push(line.to_string());
                        continue;
                    } else {
                        collecting_fn_sig = true;
                        current_fn_sig = line.to_string();
                        continue;
                    }
                }
            }

            if collecting_fn_sig {
                current_fn_sig.push(' ');
                current_fn_sig.push_str(trimmed);
                if let Some(brace_idx) = current_fn_sig.find('{') {
                    let sig = current_fn_sig[..brace_idx].trim_end();
                    output_lines.push(format!("{} {{ ... }}", sig));
                    collecting_fn_sig = false;
                    current_fn_sig.clear();

                    let open_cnt = line.chars().filter(|&c| c == '{').count() as i32;
                    let close_cnt = line.chars().filter(|&c| c == '}').count() as i32;
                    brace_depth += open_cnt - close_cnt;
                    if brace_depth > 0 {
                        in_fn_body = true;
                        fn_brace_start_depth = brace_depth - (open_cnt - close_cnt);
                    }
                } else if trimmed.ends_with(';') {
                    output_lines.push(current_fn_sig.clone());
                    collecting_fn_sig = false;
                    current_fn_sig.clear();
                }
                continue;
            }

            if in_fn_body {
                let open_cnt = line.chars().filter(|&c| c == '{').count() as i32;
                let close_cnt = line.chars().filter(|&c| c == '}').count() as i32;
                brace_depth += open_cnt - close_cnt;
                if brace_depth <= fn_brace_start_depth {
                    in_fn_body = false;
                }
                continue;
            }

            output_lines.push(line.to_string());
        }

        let skeleton_code = output_lines.join("\n");
        let skeleton_chars = skeleton_code.len();
        let skeleton_lines = output_lines.len();

        let reduction = if original_chars > 0 {
            ((original_chars.saturating_sub(skeleton_chars)) as f32 / original_chars as f32) * 100.0
        } else {
            0.0
        };

        SkeletonReport {
            original_chars,
            skeleton_chars,
            original_lines,
            skeleton_lines,
            token_reduction_pct: reduction,
            structs_count,
            traits_count,
            functions_count,
            skeleton_code,
        }
    }

    pub fn skeletonize_python(source_code: &str) -> SkeletonReport {
        let lines: Vec<&str> = source_code.lines().collect();
        let original_lines = lines.len();
        let original_chars = source_code.len();

        let mut output_lines = Vec::new();
        let mut in_fn_body = false;
        let mut fn_base_indent = 0;

        let mut structs_count = 0;
        let mut traits_count = 0;
        let mut functions_count = 0;

        for line in lines {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();

            if in_fn_body {
                if !trimmed.is_empty() && indent <= fn_base_indent {
                    in_fn_body = false;
                } else {
                    continue;
                }
            }

            if trimmed.starts_with("class ") {
                if trimmed.contains("(Protocol)") || trimmed.contains("(ABC)") {
                    traits_count += 1;
                } else {
                    structs_count += 1;
                }
                output_lines.push(line.to_string());
                continue;
            }

            if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
                functions_count += 1;
                let colon_pos = line.rfind(':').unwrap_or(line.len());
                let sig = line[..colon_pos].trim_end();
                output_lines.push(format!("{}: ...", sig));
                in_fn_body = true;
                fn_base_indent = indent;
                continue;
            }

            output_lines.push(line.to_string());
        }

        let skeleton_code = output_lines.join("\n");
        let skeleton_chars = skeleton_code.len();
        let skeleton_lines = output_lines.len();

        let reduction = if original_chars > 0 {
            ((original_chars.saturating_sub(skeleton_chars)) as f32 / original_chars as f32) * 100.0
        } else {
            0.0
        };

        SkeletonReport {
            original_chars,
            skeleton_chars,
            original_lines,
            skeleton_lines,
            token_reduction_pct: reduction,
            structs_count,
            traits_count,
            functions_count,
            skeleton_code,
        }
    }

    pub fn skeletonize_js_ts(source_code: &str) -> SkeletonReport {
        Self::skeletonize_brace_language(source_code, "ts")
    }

    pub fn skeletonize_zig(source_code: &str) -> SkeletonReport {
        Self::skeletonize_brace_language(source_code, "zig")
    }

    pub fn skeletonize_go(source_code: &str) -> SkeletonReport {
        Self::skeletonize_brace_language(source_code, "go")
    }

    pub fn skeletonize_generic(source_code: &str) -> SkeletonReport {
        Self::skeletonize_brace_language(source_code, "generic")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skeletonize_rust_ast() {
        let rust_code = r#"
pub struct UserConfig {
    pub name: String,
    pub timeout: u64,
}

pub trait Worker {
    fn run(&self) -> bool;
}

impl Worker for UserConfig {
    fn run(&self) -> bool {
        let heavy_calculation = 42 * 100;
        println!("Calculating: {}", heavy_calculation);
        for i in 0..1000 {
            let _ = i * 2;
        }
        true
    }
}

pub fn standalone_func(x: i32, y: i32) -> i32 {
    let result = x + y;
    println!("Sum: {}", result);
    result
}
"#;

        let report = CodeSkeletonizer::skeletonize(rust_code, "rs");
        assert!(report.structs_count >= 1, "Expected struct count >= 1");
        assert!(report.traits_count >= 1, "Expected trait count >= 1");
        assert!(report.functions_count >= 2, "Expected function count >= 2");
        assert!(report.token_reduction_pct > 30.0, "Expected >30% token reduction, got: {}", report.token_reduction_pct);
        assert!(report.skeleton_code.contains("pub struct UserConfig"));
        assert!(report.skeleton_code.contains("pub trait Worker"));
        assert!(report.skeleton_code.contains("standalone_func"));
        assert!(!report.skeleton_code.contains("heavy_calculation"));
    }

    #[test]
    fn test_skeletonize_python() {
        let py_code = r#"
class DataProcessor:
    def __init__(self, path: str):
        self.path = path
        self.cache = {}
        for i in range(100):
            self.cache[i] = i * 2

    def process(self, data: list) -> dict:
        result = {}
        for item in data:
            result[item] = item.upper()
        return result
"#;

        let report = CodeSkeletonizer::skeletonize(py_code, "py");
        assert_eq!(report.structs_count, 1);
        assert_eq!(report.functions_count, 2);
        assert!(report.token_reduction_pct > 20.0);
        assert!(report.skeleton_code.contains("class DataProcessor:"));
        assert!(report.skeleton_code.contains("def __init__(self, path: str): ..."));
        assert!(report.skeleton_code.contains("def process(self, data: list) -> dict: ..."));
        assert!(!report.skeleton_code.contains("self.cache[i] = i * 2"));
    }

    #[test]
    fn test_skeletonize_typescript() {
        let ts_code = r#"
interface ApiResponse<T> {
    data: T;
    status: number;
}

export function fetchData(url: string): Promise<string> {
    console.log("Fetching: " + url);
    const complex_operation = 123 + 456;
    return Promise.resolve("done");
}
"#;

        let report = CodeSkeletonizer::skeletonize(ts_code, "ts");
        assert!(report.skeleton_code.contains("fetchData"));
        assert!(!report.skeleton_code.contains("complex_operation"));
        assert!(report.token_reduction_pct > 15.0);
    }
}

