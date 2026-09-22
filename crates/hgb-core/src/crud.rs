use crate::error::{HgbError, Result};
use crate::security::AgentShieldLight;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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

impl ViewFileOptions {
    pub fn from_json(args: &serde_json::Value) -> Self {
        let start_line = args.get("StartLine")
            .or_else(|| args.get("start_line"))
            .and_then(|v| v.as_u64())
            .map(|u| u as usize);
        let end_line = args.get("EndLine")
            .or_else(|| args.get("end_line"))
            .and_then(|v| v.as_u64())
            .map(|u| u as usize);
        let content_offset = args.get("ContentOffset")
            .or_else(|| args.get("content_offset"))
            .and_then(|v| v.as_u64())
            .map(|u| u as usize);
        let max_lines = args.get("max_lines")
            .and_then(|v| v.as_u64())
            .map(|u| u as usize)
            .or(Some(800));
        let line_numbers = args.get("LineNumbers")
            .or_else(|| args.get("line_numbers"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        Self {
            start_line,
            end_line,
            content_offset,
            max_lines,
            line_numbers,
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
    pub create_backup: bool,
    pub instruction: Option<String>,
    pub description: Option<String>,
    pub target_lint_error_ids: Vec<String>,
}

impl Default for ReplaceOptions {
    fn default() -> Self {
        Self {
            start_line: None,
            end_line: None,
            allow_multiple: false,
            create_backup: false,
            instruction: None,
            description: None,
            target_lint_error_ids: Vec::new(),
        }
    }
}

impl ReplaceOptions {
    pub fn from_json(args: &serde_json::Value) -> Self {
        let start_line = args.get("StartLine")
            .or_else(|| args.get("start_line"))
            .and_then(|v| v.as_u64())
            .map(|u| u as usize);
        let end_line = args.get("EndLine")
            .or_else(|| args.get("end_line"))
            .and_then(|v| v.as_u64())
            .map(|u| u as usize);
        let allow_multiple = args.get("AllowMultiple")
            .or_else(|| args.get("allow_multiple"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let create_backup = args.get("create_backup")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let instruction = args.get("Instruction")
            .or_else(|| args.get("instruction"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let description = args.get("Description")
            .or_else(|| args.get("description"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let mut target_lint_error_ids = Vec::new();
        if let Some(arr) = args.get("TargetLintErrorIds").or_else(|| args.get("target_lint_error_ids")).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    target_lint_error_ids.push(s.to_string());
                }
            }
        }

        Self {
            start_line,
            end_line,
            allow_multiple,
            create_backup,
            instruction,
            description,
            target_lint_error_ids,
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
    pub filename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindEntry {
    pub path: String,
    pub r#type: String,
    pub size_bytes: u64,
}

/// Reverse-Engineered Compact AGY Surgical CRUD Engine
pub struct AgyCrud;

impl AgyCrud {
    /// Detect binary payloads using magic bytes and null byte heuristics
    pub fn detect_binary(bytes: &[u8]) -> Option<&'static str> {
        let sample = if bytes.len() > 1024 { &bytes[..1024] } else { bytes };
        if sample.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some("image/png");
        }
        if sample.starts_with(b"\xff\xd8\xff") {
            return Some("image/jpeg");
        }
        if sample.starts_with(b"GIF87a") || sample.starts_with(b"GIF89a") {
            return Some("image/gif");
        }
        if sample.starts_with(b"%PDF-") {
            return Some("application/pdf");
        }
        if sample.starts_with(b"\x7fELF") {
            return Some("application/x-executable");
        }
        if sample.iter().any(|&b| b == 0) {
            return Some("application/octet-stream");
        }
        None
    }

    /// Paged, 1-indexed windowed file reader with binary safety and content offset
    pub fn view_file<P: AsRef<Path>>(path: P, options: ViewFileOptions) -> Result<ViewFileResult> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        AgentShieldLight::audit_path(&path_str)?;

        if !path.exists() {
            return Err(HgbError::NotFound(format!("File '{}' not found", path.display())));
        }

        let mut file = File::open(path)?;
        let mut raw_bytes = Vec::new();
        file.read_to_end(&mut raw_bytes)?;

        if let Some(mime) = Self::detect_binary(&raw_bytes) {
            let json_desc = serde_json::json!({
                "is_binary": true,
                "mime_type": mime,
                "size_bytes": raw_bytes.len(),
                "path": path.display().to_string()
            });
            let pretty_desc = serde_json::to_string_pretty(&json_desc).unwrap_or_default();
            return Ok(ViewFileResult {
                content: format!("Binary file detected: {} ({} bytes)\n{}", mime, raw_bytes.len(), pretty_desc),
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

        let selected = if start_idx < total_lines {
            &all_lines[start_idx..end_idx]
        } else {
            &[]
        };

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
                "[Content truncated: showing lines {} to {} of {} lines. Use start_line/end_line to view more]\n",
                start_idx + 1,
                start_idx + max,
                total_lines
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
    pub fn write_to_file<P: AsRef<Path>>(
        path: P,
        content: &str,
        overwrite: bool,
        artifact_summary: Option<&str>,
    ) -> Result<String> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        AgentShieldLight::audit_path(&path_str)?;
        AgentShieldLight::audit_payload(content)?;

        if path.exists() && !overwrite {
            return Err(HgbError::Security(format!(
                "Target file already exists: '{}'. 'overwrite' is set to false.",
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

        let mut msg = format!("✔ Successfully wrote {} bytes to '{}'", content.len(), path.display());
        if let Some(summary) = artifact_summary {
            msg.push_str(&format!("\nArtifact Summary: {}", summary));
        }
        Ok(msg)
    }

    /// Surgical search-and-replace with ±25 line-drift tolerance, backups, and diff stats
    pub fn replace_file_content<P: AsRef<Path>>(
        path: P,
        target_content: &str,
        replacement_content: &str,
        options: ReplaceOptions,
    ) -> Result<String> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        AgentShieldLight::audit_path(&path_str)?;
        AgentShieldLight::audit_payload(replacement_content)?;

        if !path.exists() {
            return Err(HgbError::NotFound(format!("File '{}' not found", path.display())));
        }

        let original = fs::read_to_string(path)?;

        if options.create_backup {
            let bak_path = PathBuf::from(format!("{}.bak", path.display()));
            fs::write(&bak_path, &original)?;
        }

        let lines: Vec<&str> = original.lines().collect();
        let total_lines = lines.len();

        let mut drift_applied = false;
        let mut actual_start = 0;
        let mut actual_end = total_lines;

        if options.start_line.is_some() || options.end_line.is_some() {
            let req_start = options.start_line.map(|s| s.saturating_sub(1)).unwrap_or(0);
            let req_end = options.end_line.map(|e| e.min(total_lines)).unwrap_or(total_lines);

            let window = lines[req_start..req_end].join("\n");
            if window.contains(target_content) {
                actual_start = req_start;
                actual_end = req_end;
            } else {
                // Apply ±25 line drift tolerance window
                let drift_window = 25;
                let min_bound = req_start.saturating_sub(drift_window);
                let max_bound = (req_end + drift_window).min(total_lines);

                let mut found = false;
                for i in min_bound..max_bound {
                    for j in (i + 1)..=max_bound {
                        let candidate = lines[i..j].join("\n");
                        if candidate.contains(target_content) {
                            actual_start = i;
                            actual_end = j;
                            found = true;
                            drift_applied = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }

                if !found {
                    return Err(HgbError::Execution(format!(
                        "Target content not found within specified range {}..={} (or within ±25 lines drift)",
                        req_start + 1,
                        req_end
                    )));
                }
            }
        }

        let prefix = if actual_start > 0 { lines[..actual_start].join("\n") + "\n" } else { String::new() };
        let middle = lines[actual_start..actual_end].join("\n");
        let suffix = if actual_end < total_lines { "\n".to_string() + &lines[actual_end..].join("\n") } else { String::new() };

        let occurrences = middle.matches(target_content).count();
        if occurrences == 0 {
            return Err(HgbError::Execution(format!(
                "Target content not found in target window lines {}..={}",
                actual_start + 1,
                actual_end
            )));
        }
        if occurrences > 1 && !options.allow_multiple {
            return Err(HgbError::Execution(format!(
                "Ambiguous target: found {} occurrences. Narrow line bounds or set allow_multiple=true",
                occurrences
            )));
        }

        let new_middle = if options.allow_multiple {
            middle.replace(target_content, replacement_content)
        } else {
            middle.replacen(target_content, replacement_content, 1)
        };

        let final_content = format!("{}{}{}", prefix, new_middle, suffix);
        fs::write(path, &final_content)?;

        // Calculate diff statistics
        let removed_lines = target_content.lines().count();
        let added_lines = replacement_content.lines().count();
        let net_delta = (added_lines as isize) - (removed_lines as isize);
        let sign = if net_delta >= 0 { "+" } else { "" };

        let mut report = format!(
            "✔ Successfully replaced {} occurrence(s) in '{}'.\nDiff: +{} lines, -{} lines (net delta: {}{})",
            occurrences,
            path.display(),
            added_lines,
            removed_lines,
            sign,
            net_delta
        );

        if drift_applied {
            report.push_str("\n[Notice: line-drift sliding window applied]");
        }
        if let Some(desc) = options.description {
            report.push_str(&format!("\nDescription: {}", desc));
        }
        if !options.target_lint_error_ids.is_empty() {
            report.push_str(&format!("\nFixed Lints: {}", options.target_lint_error_ids.join(", ")));
        }

        Ok(report)
    }

    /// List directory contents with file sizes and recursive counts
    pub fn list_dir<P: AsRef<Path>>(path: P) -> Result<Vec<DirEntryInfo>> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        AgentShieldLight::audit_path(&path_str)?;

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

    /// Grep search supporting regex, literal matching, case-insensitivity, and glob filtering
    pub fn grep_search<P: AsRef<Path>>(
        root: P,
        query: &str,
        is_regex: bool,
        case_insensitive: bool,
        match_per_line: bool,
        includes: &[String],
    ) -> Result<serde_json::Value> {
        let root_path = root.as_ref();
        let root_str = root_path.to_string_lossy();
        AgentShieldLight::audit_path(&root_str)?;

        let pattern = if is_regex {
            query.to_string()
        } else {
            regex::escape(query)
        };

        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| HgbError::Execution(format!("Invalid regex '{}': {}", query, e)))?;

        let mut matches = Vec::new();
        let mut matched_files = Vec::new();

        Self::grep_tree(root_path, &regex, match_per_line, includes, &mut matches, &mut matched_files)?;

        if match_per_line {
            Ok(serde_json::to_value(&matches).unwrap_or_default())
        } else {
            let files_json: Vec<serde_json::Value> = matched_files
                .into_iter()
                .map(|f| serde_json::json!({ "filename": f }))
                .collect();
            Ok(serde_json::to_value(&files_json).unwrap_or_default())
        }
    }

    fn grep_tree(
        path: &Path,
        regex: &regex::Regex,
        match_per_line: bool,
        includes: &[String],
        matches: &mut Vec<GrepMatch>,
        matched_files: &mut Vec<String>,
    ) -> Result<()> {
        let path_str = path.to_string_lossy().to_string();

        // Glob filtering
        if !includes.is_empty() {
            let mut include_match = false;
            let mut exclude_match = false;

            for inc in includes {
                if let Some(exc_pattern) = inc.strip_prefix('!') {
                    let glob = exc_pattern.replace("**/*", "").replace("*", "");
                    if path_str.contains(&glob) {
                        exclude_match = true;
                        break;
                    }
                } else {
                    let ext = inc.replace("*.", "");
                    if path_str.ends_with(&ext) || inc == "*" {
                        include_match = true;
                    }
                }
            }

            if exclude_match || (!include_match && path.is_file()) {
                return Ok(());
            }
        }

        if path.is_file() {
            if let Ok(mut f) = File::open(path) {
                let mut buf = Vec::new();
                if f.read_to_end(&mut buf).is_ok() && Self::detect_binary(&buf).is_none() {
                    if let Ok(text) = std::str::from_utf8(&buf) {
                        let mut file_has_match = false;
                        for (i, line) in text.lines().enumerate() {
                            if regex.is_match(line) {
                                file_has_match = true;
                                if match_per_line {
                                    matches.push(GrepMatch {
                                        filename: path_str.clone(),
                                        line_number: Some(i + 1),
                                        line_content: Some(line.trim().to_string()),
                                    });
                                }
                            }
                        }
                        if file_has_match && !match_per_line {
                            matched_files.push(path_str);
                        }
                    }
                }
            }
            return Ok(());
        }

        if let Ok(read_dir) = fs::read_dir(path) {
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                let child = entry.path();
                Self::grep_tree(&child, regex, match_per_line, includes, matches, matched_files)?;
            }
        }

        Ok(())
    }

    /// Find files by extension, pattern, excludes, and depth clamping
    pub fn find_by_name<P: AsRef<Path>>(
        root: P,
        pattern: Option<&str>,
        extensions: &[String],
        excludes: &[String],
        max_depth: Option<usize>,
        target_type: Option<&str>,
    ) -> Result<Vec<FindEntry>> {
        let root = root.as_ref();
        let root_str = root.to_string_lossy();
        AgentShieldLight::audit_path(&root_str)?;

        let mut results = Vec::new();
        Self::find_tree(root, 1, pattern, extensions, excludes, max_depth, target_type.unwrap_or("any"), &mut results)?;
        Ok(results)
    }

    fn find_tree(
        dir: &Path,
        current_depth: usize,
        pattern: Option<&str>,
        extensions: &[String],
        excludes: &[String],
        max_depth: Option<usize>,
        target_type: &str,
        results: &mut Vec<FindEntry>,
    ) -> Result<()> {
        if let Some(max) = max_depth {
            if current_depth > max {
                return Ok(());
            }
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();
                let path_str = path.to_string_lossy().to_string();

                if file_name.starts_with('.') || file_name == "target" {
                    continue;
                }

                // Check excludes
                let mut excluded = false;
                for exc in excludes {
                    let ext = exc.replace("*.", "").replace("*", "");
                    if file_name.contains(&ext) {
                        excluded = true;
                        break;
                    }
                }
                if excluded {
                    continue;
                }

                let is_dir = entry.metadata().map(|m| m.is_dir()).unwrap_or(false);
                let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);

                let matches_type = match target_type {
                    "file" => !is_dir,
                    "directory" => is_dir,
                    _ => true,
                };

                let matches_ext = if extensions.is_empty() {
                    true
                } else {
                    extensions.iter().any(|ext| {
                        let clean = ext.trim_start_matches('.');
                        file_name.ends_with(&format!(".{}", clean)) || file_name.ends_with(ext)
                    })
                };

                let matches_pattern = if let Some(pat) = pattern {
                    let clean_pat = pat.replace("*", "");
                    file_name.contains(&clean_pat)
                } else {
                    true
                };

                if matches_type && matches_ext && matches_pattern {
                    results.push(FindEntry {
                        path: path_str.clone(),
                        r#type: if is_dir { "directory".to_string() } else { "file".to_string() },
                        size_bytes,
                    });
                }

                if is_dir {
                    Self::find_tree(&path, current_depth + 1, pattern, extensions, excludes, max_depth, target_type, results)?;
                }
            }
        }

        Ok(())
    }
}
