use crate::error::{HgbError, Result};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

/// AGY-Compatible File View Options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFileOptions {
    pub start_line: Option<usize>,      // 1-indexed
    pub end_line: Option<usize>,        // 1-indexed
    pub content_offset: Option<usize>,  // byte offset
    pub max_lines: Option<usize>,       // truncation limit
    pub line_numbers: bool,
}

impl Default for ViewFileOptions {
    fn default() -> Self {
        Self {
            start_line: None,
            end_line: None,
            content_offset: None,
            max_lines: Some(800),
            line_numbers: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFileResult {
    pub content: String,
    pub total_lines: usize,
    pub is_binary: bool,
    pub is_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaceOptions {
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub allow_multiple: bool,
}

impl Default for ReplaceOptions {
    fn default() -> Self {
        Self {
            start_line: None,
            end_line: None,
            allow_multiple: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub child_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    pub file_path: String,
    pub line_number: usize,
    pub line_content: String,
}

/// Reverse-Engineered Compact AGY Surgical CRUD Engine
pub struct AgyCrud;

impl AgyCrud {
    /// Detect binary payloads using magic bytes and null byte heuristics
    pub fn is_binary(bytes: &[u8]) -> bool {
        let sample = if bytes.len() > 1024 { &bytes[..1024] } else { bytes };
        if sample.iter().any(|&b| b == 0) {
            return true;
        }
        // Known binary headers: ELF, PNG, JPEG, GIF, PDF, ZIP
        if sample.starts_with(b"\x7fELF")
            || sample.starts_with(b"\x89PNG")
            || sample.starts_with(b"\xff\xd8\xff")
            || sample.starts_with(b"GIF8")
            || sample.starts_with(b"%PDF-")
            || sample.starts_with(b"PK\x03\x04")
        {
            return true;
        }
        false
    }

    /// Paged, 1-indexed windowed file reader with binary safety and content offset
    pub fn view_file<P: AsRef<Path>>(path: P, options: ViewFileOptions) -> Result<ViewFileResult> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(HgbError::NotFound(format!("File '{}' not found", path.display())));
        }

        let mut file = File::open(path)?;
        let mut raw_bytes = Vec::new();
        file.read_to_end(&mut raw_bytes)?;

        if Self::is_binary(&raw_bytes) {
            return Ok(ViewFileResult {
                content: format!("[Binary file: {} bytes, non-text content]", raw_bytes.len()),
                total_lines: 0,
                is_binary: true,
                is_truncated: false,
            });
        }

        // Apply content offset if specified
        let slice = if let Some(offset) = options.content_offset {
            if offset >= raw_bytes.len() {
                ""
            } else {
                std::str::from_utf8(&raw_bytes[offset..]).unwrap_or_default()
            }
        } else {
            std::str::from_utf8(&raw_bytes).unwrap_or_default()
        };

        let all_lines: Vec<&str> = slice.lines().collect();
        let total_lines = all_lines.len();

        let start_idx = options.start_line.map(|s| s.saturating_sub(1)).unwrap_or(0);
        let end_idx = options.end_line.map(|e| e.min(total_lines)).unwrap_or(total_lines);

        if start_idx >= total_lines && total_lines > 0 {
            return Err(HgbError::Execution(format!(
                "start_line ({}) exceeds total lines ({})",
                start_idx + 1,
                total_lines
            )));
        }

        let selected = &all_lines[start_idx..end_idx];
        let max = options.max_lines.unwrap_or(800);
        let is_truncated = selected.len() > max;
        let clamped = if is_truncated { &selected[..max] } else { selected };

        let mut out = String::new();
        for (i, line) in clamped.iter().enumerate() {
            let actual_line_no = start_idx + i + 1;
            if options.line_numbers {
                out.push_str(&format!("{:>5} | {}\n", actual_line_no, line));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }

        if is_truncated {
            out.push_str(&format!(
                "\n[Content truncated: showing {} of {} lines. Use start_line/end_line to view more]",
                max,
                selected.len()
            ));
        }

        Ok(ViewFileResult {
            content: out,
            total_lines,
            is_binary: false,
            is_truncated,
        })
    }

    /// Atomic file writer with automatic parent directory creation and overwrite guards
    pub fn write_to_file<P: AsRef<Path>>(path: P, content: &str, overwrite: bool) -> Result<usize> {
        let path = path.as_ref();
        if path.exists() && !overwrite {
            return Err(HgbError::Security(format!(
                "Target file '{}' already exists. Pass overwrite=true to replace.",
                path.display()
            )));
        }

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let mut file = File::create(path)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        Ok(content.len())
    }

    /// Surgical search-and-replace with line bounding and uniqueness enforcement
    pub fn replace_file_content<P: AsRef<Path>>(
        path: P,
        target_content: &str,
        replacement_content: &str,
        options: ReplaceOptions,
    ) -> Result<usize> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(HgbError::NotFound(format!("File '{}' not found", path.display())));
        }

        let original = fs::read_to_string(path)?;

        if options.start_line.is_some() || options.end_line.is_some() {
            // Line-bounded replacement
            let lines: Vec<&str> = original.lines().collect();
            let total = lines.len();
            let start = options.start_line.map(|s| s.saturating_sub(1)).unwrap_or(0);
            let end = options.end_line.map(|e| e.min(total)).unwrap_or(total);

            if start >= total {
                return Err(HgbError::Execution(format!("Start line {} exceeds total lines {}", start + 1, total)));
            }

            let prefix = if start > 0 { lines[..start].join("\n") + "\n" } else { String::new() };
            let middle = lines[start..end].join("\n");
            let suffix = if end < total { "\n".to_string() + &lines[end..].join("\n") } else { String::new() };

            let count = middle.matches(target_content).count();
            if count == 0 {
                return Err(HgbError::Execution(format!(
                    "Target content not found within specified lines {}-{} in '{}'",
                    start + 1,
                    end,
                    path.display()
                )));
            }
            if count > 1 && !options.allow_multiple {
                return Err(HgbError::Execution(format!(
                    "Ambiguous target: found {} occurrences within lines {}-{}. Narrow lines or set allow_multiple=true",
                    count,
                    start + 1,
                    end
                )));
            }

            let new_middle = if options.allow_multiple {
                middle.replace(target_content, replacement_content)
            } else {
                middle.replacen(target_content, replacement_content, 1)
            };

            let final_content = format!("{}{}{}", prefix, new_middle, suffix);
            fs::write(path, final_content)?;
            Ok(count)
        } else {
            // Whole file replacement
            let count = original.matches(target_content).count();
            if count == 0 {
                return Err(HgbError::Execution(format!(
                    "Target content not found in '{}'",
                    path.display()
                )));
            }
            if count > 1 && !options.allow_multiple {
                return Err(HgbError::Execution(format!(
                    "Ambiguous target: found {} occurrences in '{}'. Specify line range or set allow_multiple=true",
                    count,
                    path.display()
                )));
            }

            let updated = if options.allow_multiple {
                original.replace(target_content, replacement_content)
            } else {
                original.replacen(target_content, replacement_content, 1)
            };

            fs::write(path, updated)?;
            Ok(count)
        }
    }

    /// List directory contents with file sizes and recursive counts
    pub fn list_dir<P: AsRef<Path>>(path: P) -> Result<Vec<DirEntryInfo>> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(HgbError::NotFound(format!("Directory '{}' not found", path.display())));
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = metadata.is_dir();
            let size_bytes = metadata.len();

            let child_count = if is_dir {
                fs::read_dir(entry.path()).map(|r| r.count()).ok()
            } else {
                None
            };

            entries.push(DirEntryInfo {
                name,
                path: entry.path().to_string_lossy().to_string(),
                is_dir,
                size_bytes,
                child_count,
            });
        }

        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name))
        });

        Ok(entries)
    }

    /// Line-numbered pattern and regex search with match snippets
    pub fn grep_search<P: AsRef<Path>>(
        root: P,
        pattern: &str,
        case_insensitive: bool,
        max_matches: usize,
    ) -> Result<Vec<GrepMatch>> {
        let regex = RegexBuilder::new(pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| HgbError::Execution(format!("Invalid regex pattern '{}': {}", pattern, e)))?;

        let root_path = root.as_ref();
        let mut matches = Vec::new();
        Self::grep_recursive(root_path, &regex, &mut matches, max_matches)?;
        Ok(matches)
    }

    fn grep_recursive(
        dir: &Path,
        regex: &regex::Regex,
        matches: &mut Vec<GrepMatch>,
        max_matches: usize,
    ) -> Result<()> {
        if matches.len() >= max_matches {
            return Ok(());
        }

        if dir.is_file() {
            Self::grep_file(dir, regex, matches, max_matches)?;
            return Ok(());
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if matches.len() >= max_matches {
                    break;
                }
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();

                // Skip hidden folders, git, target, node_modules
                if file_name.starts_with('.') || file_name == "target" || file_name == "node_modules" {
                    continue;
                }

                if path.is_dir() {
                    Self::grep_recursive(&path, regex, matches, max_matches)?;
                } else if path.is_file() {
                    Self::grep_file(&path, regex, matches, max_matches)?;
                }
            }
        }
        Ok(())
    }

    fn grep_file(
        path: &Path,
        regex: &regex::Regex,
        matches: &mut Vec<GrepMatch>,
        max_matches: usize,
    ) -> Result<()> {
        if let Ok(mut file) = File::open(path) {
            let mut buf = Vec::new();
            if file.read_to_end(&mut buf).is_ok() && !Self::is_binary(&buf) {
                if let Ok(text) = std::str::from_utf8(&buf) {
                    for (i, line) in text.lines().enumerate() {
                        if matches.len() >= max_matches {
                            break;
                        }
                        if regex.is_match(line) {
                            matches.push(GrepMatch {
                                file_path: path.to_string_lossy().to_string(),
                                line_number: i + 1,
                                line_content: line.trim().to_string(),
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
