use hgb_core::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Classification of diff line
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

/// A parsed line in a diff hunk
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
}

/// A single hunk in a unified diff
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub file_path: PathBuf,
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub header: String,
    pub lines: Vec<DiffLine>,
    pub accepted: Option<bool>,
}

/// Selective Diff Patcher & Hunk Review Engine
pub struct SelectivePatcher;

impl SelectivePatcher {
    /// Parse diff with an optional fallback file path hint
    pub fn parse_diff(raw_diff: &str, file_hint: Option<&PathBuf>) -> Vec<DiffHunk> {
        let mut hunks = Self::parse_unified_diff(raw_diff);
        if let Some(hint) = file_hint {
            for h in &mut hunks {
                if h.file_path == PathBuf::from("unknown") || h.file_path.as_os_str().is_empty() {
                    h.file_path = hint.clone();
                }
            }
        }
        hunks
    }

    /// Parse raw unified git diff into structured hunks
    pub fn parse_unified_diff(raw_diff: &str) -> Vec<DiffHunk> {
        let mut hunks = Vec::new();
        let hunk_header_re = Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)").unwrap();

        let mut current_file = PathBuf::from("unknown");
        let mut current_hunk: Option<DiffHunk> = None;

        for line in raw_diff.lines() {
            if line.starts_with("+++ b/") || line.starts_with("+++ ") {
                let path_str = line.trim_start_matches("+++ b/").trim_start_matches("+++ ");
                current_file = PathBuf::from(path_str);
                continue;
            }

            if line.starts_with("--- ") || line.starts_with("diff --git ") || line.starts_with("index ") {
                continue;
            }

            if let Some(caps) = hunk_header_re.captures(line) {
                if let Some(h) = current_hunk.take() {
                    hunks.push(h);
                }

                let old_start = caps.get(1).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                let old_len = caps.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                let new_start = caps.get(3).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                let new_len = caps.get(4).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                let header = caps.get(5).map(|m| m.as_str().trim().to_string()).unwrap_or_default();

                current_hunk = Some(DiffHunk {
                    file_path: current_file.clone(),
                    old_start,
                    old_len,
                    new_start,
                    new_len,
                    header,
                    lines: Vec::new(),
                    accepted: None,
                });
                continue;
            }

            if let Some(ref mut hunk) = current_hunk {
                if line.starts_with('+') && !line.starts_with("+++") {
                    hunk.lines.push(DiffLine {
                        kind: DiffLineKind::Addition,
                        content: line[1..].to_string(),
                        old_line: None,
                        new_line: None,
                    });
                } else if line.starts_with('-') && !line.starts_with("---") {
                    hunk.lines.push(DiffLine {
                        kind: DiffLineKind::Deletion,
                        content: line[1..].to_string(),
                        old_line: None,
                        new_line: None,
                    });
                } else if line.starts_with(' ') || line.is_empty() {
                    let content = if line.starts_with(' ') { &line[1..] } else { line };
                    hunk.lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        content: content.to_string(),
                        old_line: None,
                        new_line: None,
                    });
                }
            }
        }

        if let Some(h) = current_hunk {
            hunks.push(h);
        }

        hunks
    }

    /// Apply only accepted hunks to original file content
    pub fn apply_accepted_hunks(original_content: &str, hunks: &[DiffHunk]) -> Result<String> {
        let orig_lines: Vec<&str> = original_content.lines().collect();
        let mut new_lines = Vec::new();
        let mut orig_idx = 0;

        for hunk in hunks {
            // If hunk explicitly rejected, skip and preserve original lines
            let is_accepted = hunk.accepted.unwrap_or(true);

            // Copy lines up to hunk start
            let target_start = hunk.old_start.saturating_sub(1);
            while orig_idx < target_start && orig_idx < orig_lines.len() {
                new_lines.push(orig_lines[orig_idx].to_string());
                orig_idx += 1;
            }

            if is_accepted {
                // Apply hunk lines
                for line in &hunk.lines {
                    match line.kind {
                        DiffLineKind::Context => {
                            new_lines.push(line.content.clone());
                            orig_idx += 1;
                        }
                        DiffLineKind::Addition => {
                            new_lines.push(line.content.clone());
                        }
                        DiffLineKind::Deletion => {
                            orig_idx += 1;
                        }
                    }
                }
            } else {
                // Keep original lines for this hunk range
                let target_end = (hunk.old_start.saturating_sub(1) + hunk.old_len).min(orig_lines.len());
                while orig_idx < target_end {
                    new_lines.push(orig_lines[orig_idx].to_string());
                    orig_idx += 1;
                }
            }
        }

        // Copy remaining lines
        while orig_idx < orig_lines.len() {
            new_lines.push(orig_lines[orig_idx].to_string());
            orig_idx += 1;
        }

        let mut res = new_lines.join("\n");
        if original_content.ends_with('\n') {
            res.push('\n');
        }
        Ok(res)
    }

    /// Format summary of hunks
    pub fn render_diff_summary(hunks: &[DiffHunk]) -> String {
        let mut additions = 0;
        let mut deletions = 0;

        for h in hunks {
            for l in &h.lines {
                match l.kind {
                    DiffLineKind::Addition => additions += 1,
                    DiffLineKind::Deletion => deletions += 1,
                    DiffLineKind::Context => {}
                }
            }
        }

        format!("Diff Summary: {} hunk(s), +{} line(s), -{} line(s)", hunks.len(), additions, deletions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_and_apply_hunks() {
        let diff = r#"
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn test() {
-    let a = 1;
+    let a = 2;
 }
"#;

        let hunks = SelectivePatcher::parse_unified_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, PathBuf::from("src/lib.rs"));
        assert_eq!(hunks[0].lines.len(), 4);

        let orig = "fn test() {\n    let a = 1;\n}\n";
        let patched = SelectivePatcher::apply_accepted_hunks(orig, &hunks).unwrap();
        assert_eq!(patched, "fn test() {\n    let a = 2;\n}\n");
    }

    #[test]
    fn test_selective_reject_hunk() {
        let diff = r#"
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn test() {
-    let a = 1;
+    let a = 2;
 }
"#;

        let mut hunks = SelectivePatcher::parse_unified_diff(diff);
        hunks[0].accepted = Some(false);

        let orig = "fn test() {\n    let a = 1;\n}\n";
        let patched = SelectivePatcher::apply_accepted_hunks(orig, &hunks).unwrap();
        assert_eq!(patched, orig);
    }
}
