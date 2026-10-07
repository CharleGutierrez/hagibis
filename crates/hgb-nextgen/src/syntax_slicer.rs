use hgb_core::syntax_slicer::{
    CallGraphNode, SyntaxSliceResult, TargetLanguage, TokenReductionMetrics,
};
use hgb_core::{HgbError, Result};
use std::fs;
use std::path::Path;

pub struct SyntaxSlicer;

impl SyntaxSlicer {
    pub fn detect_language<P: AsRef<Path>>(path: P) -> Option<TargetLanguage> {
        let ext = path.as_ref().extension().and_then(|s| s.to_str())?;
        match ext.to_lowercase().as_str() {
            "rs" => Some(TargetLanguage::Rust),
            "ts" | "tsx" => Some(TargetLanguage::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(TargetLanguage::JavaScript),
            "py" | "pyw" => Some(TargetLanguage::Python),
            "go" => Some(TargetLanguage::Go),
            _ => None,
        }
    }

    pub fn extract_surgical_slice<P: AsRef<Path>>(
        file_path: P,
        focal_symbol: &str,
        _depth: usize,
    ) -> Result<SyntaxSliceResult> {
        let path_ref = file_path.as_ref();
        if !path_ref.exists() {
            return Err(HgbError::NotFound(format!("File '{}' not found", path_ref.display())));
        }

        let language = Self::detect_language(path_ref).unwrap_or(TargetLanguage::Rust);
        let raw_content = fs::read_to_string(path_ref)?;
        let newline_offsets = hgb_core::zig_accelerate::simd_find_newlines(raw_content.as_bytes());
        let mut lines: Vec<&str> = Vec::with_capacity(newline_offsets.len() + 1);
        let mut prev = 0;
        for &nl_idx in &newline_offsets {
            let slice = &raw_content[prev..nl_idx];
            let trimmed = if slice.ends_with('\r') {
                &slice[..slice.len() - 1]
            } else {
                slice
            };
            lines.push(trimmed);
            prev = nl_idx + 1;
        }
        if prev <= raw_content.len() {
            let slice = &raw_content[prev..];
            let trimmed = if slice.ends_with('\r') {
                &slice[..slice.len() - 1]
            } else {
                slice
            };
            lines.push(trimmed);
        }

        // 1. Locate focal symbol
        let mut focal_start = None;
        let mut focal_line_num = 1;
        let mut focal_sig = String::new();

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let matches_symbol = match language {
                TargetLanguage::Rust => {
                    (trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ") ||
                     trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ") ||
                     trimmed.starts_with("pub enum ") || trimmed.starts_with("enum ") ||
                     trimmed.starts_with("pub trait ") || trimmed.starts_with("trait ") ||
                     trimmed.starts_with("impl ")) && trimmed.contains(focal_symbol)
                }
                TargetLanguage::TypeScript | TargetLanguage::JavaScript => {
                    (trimmed.starts_with("export function ") || trimmed.starts_with("function ") ||
                     trimmed.starts_with("export const ") || trimmed.starts_with("const ") ||
                     trimmed.starts_with("export class ") || trimmed.starts_with("class ") ||
                     trimmed.starts_with("export interface ") || trimmed.starts_with("interface ") ||
                     trimmed.starts_with("export type ") || trimmed.starts_with("type ")) && trimmed.contains(focal_symbol)
                }
                TargetLanguage::Python => {
                    (trimmed.starts_with("def ") || trimmed.starts_with("class ") ||
                     trimmed.starts_with("async def ")) && trimmed.contains(focal_symbol)
                }
                TargetLanguage::Go => {
                    (trimmed.starts_with("func ") || trimmed.starts_with("type ")) && trimmed.contains(focal_symbol)
                }
            };

            if matches_symbol {
                focal_start = Some(idx);
                focal_line_num = idx + 1;
                focal_sig = trimmed.to_string();
                break;
            }
        }

        // If symbol not found, fallback to first 50 lines
        let (focal_body, non_focal_nodes) = if let Some(start_idx) = focal_start {
            let mut end_idx = lines.len() - 1;
            let mut brace_depth = 0;
            let mut found_open = false;

            if language == TargetLanguage::Python {
                let initial_indent = lines[start_idx].len() - lines[start_idx].trim_start().len();
                for i in (start_idx + 1)..lines.len() {
                    let cur_trimmed = lines[i].trim();
                    if cur_trimmed.is_empty() {
                        continue;
                    }
                    let cur_indent = lines[i].len() - lines[i].trim_start().len();
                    if cur_indent <= initial_indent && !cur_trimmed.starts_with('#') {
                        end_idx = i - 1;
                        break;
                    }
                }
            } else {
                for i in start_idx..lines.len() {
                    let cur = lines[i];
                    for ch in cur.chars() {
                        if ch == '{' {
                            brace_depth += 1;
                            found_open = true;
                        } else if ch == '}' {
                            brace_depth -= 1;
                        }
                    }
                    if found_open && brace_depth <= 0 {
                        end_idx = i;
                        break;
                    }
                }
            }

            let focal_slice = lines[start_idx..=end_idx].join("\n");
            (focal_slice, vec![CallGraphNode {
                symbol_name: focal_symbol.to_string(),
                defining_file: path_ref.to_path_buf(),
                line_number: focal_line_num,
                signature: focal_sig,
                is_focal_target: true,
                skeletonized_body: "".to_string(),
            }])
        } else {
            let default_slice = lines.iter().take(50).cloned().collect::<Vec<_>>().join("\n");
            (default_slice, vec![CallGraphNode {
                symbol_name: focal_symbol.to_string(),
                defining_file: path_ref.to_path_buf(),
                line_number: 1,
                signature: format!("// Symbol '{}' not matched directly; showing top of file", focal_symbol),
                is_focal_target: true,
                skeletonized_body: "".to_string(),
            }])
        };

        // Collect header imports (lines starting with use, import, from, package)
        let mut import_lines = Vec::new();
        for line in &lines {
            let t = line.trim();
            if t.starts_with("use ") || t.starts_with("import ") || t.starts_with("from ") || t.starts_with("package ") {
                import_lines.push(*line);
            }
        }

        let header = if !import_lines.is_empty() {
            format!("// --- Imports & Context Declarations ---\n{}\n\n", import_lines.join("\n"))
        } else {
            String::new()
        };

        let rendered_surgical_prompt = format!(
            "<surgical_ast_slice file=\"{}\" focal_symbol=\"{}\" language=\"{}\">\n{}{}\n</surgical_ast_slice>",
            path_ref.display(),
            focal_symbol,
            language,
            header,
            focal_body
        );

        let raw_characters = raw_content.len();
        let sliced_characters = rendered_surgical_prompt.len();
        let estimated_raw_tokens = raw_characters / 4;
        let estimated_sliced_tokens = sliced_characters / 4;
        let reduction_percentage = if raw_characters > 0 && raw_characters >= sliced_characters {
            ((raw_characters - sliced_characters) as f64 / raw_characters as f64) * 100.0
        } else {
            0.0
        };

        Ok(SyntaxSliceResult {
            focal_symbol: focal_symbol.to_string(),
            language,
            nodes_included: non_focal_nodes,
            rendered_surgical_prompt,
            token_metrics: TokenReductionMetrics {
                raw_characters,
                sliced_characters,
                estimated_raw_tokens,
                estimated_sliced_tokens,
                reduction_percentage,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_syntax_slicer_simd() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join(format!("test_slicer_{}.rs", std::process::id()));
        let content = "use std::io;\n\npub fn calculate_total(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn helper() {}\n";
        let mut file = fs::File::create(&file_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let slice = SyntaxSlicer::extract_surgical_slice(&file_path, "calculate_total", 1).unwrap();
        assert_eq!(slice.focal_symbol, "calculate_symbol".replace("symbol", "total"));
        assert!(slice.rendered_surgical_prompt.contains("pub fn calculate_total"));
        assert!(slice.rendered_surgical_prompt.contains("use std::io;"));

        let _ = fs::remove_file(&file_path);
    }
}

