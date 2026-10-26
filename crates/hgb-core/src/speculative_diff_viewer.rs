//! # Speculative Dry-Run Diff Viewer (`hgb diff --speculative`)
//!
//! Visual inspection and selective cherry-picking engine for speculative patches:
//! 1. Reads speculative changes directly from `OverlaySandbox` or active workspace session.
//! 2. Computes Myers-style diff hunks with fine-grained line context.
//! 3. Renders unified colored diffs or side-by-side comparative layouts.
//! 4. Supports selective hunk acceptance/rejection (`accept_hunk`, `reject_hunk`).
//! 5. Commits cherry-picked accepted changes to disk while preserving rejected baselines.

use crate::error::{HgbError, Result};
use crate::overlayfs_sandbox::OverlaySandbox;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Status of an individual diff hunk during cherry-picking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HunkStatus {
    Pending,
    Accepted,
    Rejected,
}

impl HunkStatus {
    pub fn badge(&self) -> String {
        match self {
            Self::Pending => "[?] PENDING".yellow().to_string(),
            Self::Accepted => "[✓] ACCEPTED".green().bold().to_string(),
            Self::Rejected => "[✗] REJECTED".red().bold().to_string(),
        }
    }
}

/// Kind of line in a diff hunk
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

/// Individual line within a diff hunk
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub content: String,
}

/// Contiguous diff hunk with fine-grained context and line-level changes
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub id: usize,
    pub file_path: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub header: String,
    pub lines: Vec<DiffLine>,
    pub status: HunkStatus,
}

impl DiffHunk {
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.kind == DiffLineKind::Context)
    }

    pub fn additions_count(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == DiffLineKind::Addition).count()
    }

    pub fn deletions_count(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == DiffLineKind::Deletion).count()
    }
}

/// Status of a file in the diff
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileDiffStatus {
    Modified,
    Created,
    Deleted,
}

/// Diff details for a single file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeculativeFileDiff {
    pub file_path: String,
    pub status: FileDiffStatus,
    pub original_content: String,
    pub speculative_content: String,
    pub hunks: Vec<DiffHunk>,
}

/// Summary scorecard of a speculative diff session
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpeculativeDiffSummary {
    pub total_files: usize,
    pub total_hunks: usize,
    pub accepted_hunks: usize,
    pub rejected_hunks: usize,
    pub pending_hunks: usize,
    pub total_additions: usize,
    pub total_deletions: usize,
}

/// Report after committing cherry-picked hunks to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CherryPickCommitReport {
    pub files_committed: usize,
    pub hunks_accepted: usize,
    pub hunks_rejected: usize,
    pub affected_paths: Vec<String>,
}

/// Interactive session managing speculative diffs, hunk navigation, and cherry-picking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeculativeDiffSession {
    pub files: Vec<SpeculativeFileDiff>,
    pub current_file_idx: usize,
    pub current_hunk_idx: usize,
}

impl SpeculativeDiffSession {
    /// Constructs a diff session directly from an active `OverlaySandbox`
    pub fn from_overlay_sandbox(sandbox: &OverlaySandbox) -> Result<Self> {
        let diff = sandbox.inspect_diff()?;
        let mut session_files = Vec::new();

        // 1. Process modified files
        for rel_path in &diff.modified_files {
            let orig_path = sandbox.lowerdir().join(rel_path);
            let spec_path = sandbox.upperdir().join(rel_path);

            let orig_content = fs::read_to_string(&orig_path).unwrap_or_default();
            let spec_content = fs::read_to_string(&spec_path).unwrap_or_default();

            let hunks = Self::compute_hunks(rel_path, &orig_content, &spec_content);
            session_files.push(SpeculativeFileDiff {
                file_path: rel_path.clone(),
                status: FileDiffStatus::Modified,
                original_content: orig_content,
                speculative_content: spec_content,
                hunks,
            });
        }

        // 2. Process added files
        for rel_path in &diff.added_files {
            let spec_path = sandbox.upperdir().join(rel_path);
            let spec_content = fs::read_to_string(&spec_path).unwrap_or_default();
            let hunks = Self::compute_hunks(rel_path, "", &spec_content);

            session_files.push(SpeculativeFileDiff {
                file_path: rel_path.clone(),
                status: FileDiffStatus::Created,
                original_content: String::new(),
                speculative_content: spec_content,
                hunks,
            });
        }

        // 3. Process deleted files
        for rel_path in &diff.deleted_files {
            let orig_path = sandbox.lowerdir().join(rel_path);
            let orig_content = fs::read_to_string(&orig_path).unwrap_or_default();
            let hunks = Self::compute_hunks(rel_path, &orig_content, "");

            session_files.push(SpeculativeFileDiff {
                file_path: rel_path.clone(),
                status: FileDiffStatus::Deleted,
                original_content: orig_content,
                speculative_content: String::new(),
                hunks,
            });
        }

        Ok(Self {
            files: session_files,
            current_file_idx: 0,
            current_hunk_idx: 0,
        })
    }

    /// Constructs a diff session for a single file given original and speculative contents
    pub fn from_contents(file_path: &str, original: &str, speculative: &str) -> Self {
        let hunks = Self::compute_hunks(file_path, original, speculative);
        let status = if original.is_empty() && !speculative.is_empty() {
            FileDiffStatus::Created
        } else if !original.is_empty() && speculative.is_empty() {
            FileDiffStatus::Deleted
        } else {
            FileDiffStatus::Modified
        };

        Self {
            files: vec![SpeculativeFileDiff {
                file_path: file_path.to_string(),
                status,
                original_content: original.to_string(),
                speculative_content: speculative.to_string(),
                hunks,
            }],
            current_file_idx: 0,
            current_hunk_idx: 0,
        }
    }

    /// Computes diff hunks between old and new text using line-by-line Myers diff
    pub fn compute_hunks(file_path: &str, old_text: &str, new_text: &str) -> Vec<DiffHunk> {
        let old_lines: Vec<&str> = if old_text.is_empty() {
            Vec::new()
        } else {
            old_text.lines().collect()
        };

        let new_lines: Vec<&str> = if new_text.is_empty() {
            Vec::new()
        } else {
            new_text.lines().collect()
        };

        let diff_ops = compute_line_diff(&old_lines, &new_lines);
        group_diff_ops_into_hunks(file_path, &diff_ops, 3)
    }

    /// Total hunks across all files
    pub fn total_hunks(&self) -> usize {
        self.files.iter().map(|f| f.hunks.len()).sum()
    }

    /// Get current active hunk
    pub fn current_hunk(&self) -> Option<&DiffHunk> {
        self.files
            .get(self.current_file_idx)
            .and_then(|f| f.hunks.get(self.current_hunk_idx))
    }

    /// Advance to next hunk across files
    pub fn next_hunk(&mut self) -> bool {
        if self.files.is_empty() {
            return false;
        }

        if let Some(f) = self.files.get(self.current_file_idx) {
            if self.current_hunk_idx + 1 < f.hunks.len() {
                self.current_hunk_idx += 1;
                return true;
            }
        }

        if self.current_file_idx + 1 < self.files.len() {
            self.current_file_idx += 1;
            self.current_hunk_idx = 0;
            return true;
        }

        false
    }

    /// Move to previous hunk across files
    pub fn prev_hunk(&mut self) -> bool {
        if self.current_hunk_idx > 0 {
            self.current_hunk_idx -= 1;
            return true;
        }

        if self.current_file_idx > 0 {
            self.current_file_idx -= 1;
            self.current_hunk_idx = self.files[self.current_file_idx].hunks.len().saturating_sub(1);
            return true;
        }

        false
    }

    /// Accept a specific hunk by ID in a file
    pub fn accept_hunk(&mut self, file_path: &str, hunk_id: usize) -> Result<()> {
        let file = self
            .files
            .iter_mut()
            .find(|f| f.file_path == file_path)
            .ok_or_else(|| HgbError::NotFound(format!("File {} not in diff session", file_path)))?;

        let hunk = file
            .hunks
            .iter_mut()
            .find(|h| h.id == hunk_id)
            .ok_or_else(|| HgbError::NotFound(format!("Hunk {} not found in {}", hunk_id, file_path)))?;

        hunk.status = HunkStatus::Accepted;
        Ok(())
    }

    /// Reject a specific hunk by ID in a file
    pub fn reject_hunk(&mut self, file_path: &str, hunk_id: usize) -> Result<()> {
        let file = self
            .files
            .iter_mut()
            .find(|f| f.file_path == file_path)
            .ok_or_else(|| HgbError::NotFound(format!("File {} not in diff session", file_path)))?;

        let hunk = file
            .hunks
            .iter_mut()
            .find(|h| h.id == hunk_id)
            .ok_or_else(|| HgbError::NotFound(format!("Hunk {} not found in {}", hunk_id, file_path)))?;

        hunk.status = HunkStatus::Rejected;
        Ok(())
    }

    /// Accept all hunks across all files
    pub fn accept_all(&mut self) {
        for file in &mut self.files {
            for hunk in &mut file.hunks {
                hunk.status = HunkStatus::Accepted;
            }
        }
    }

    /// Reject all hunks across all files
    pub fn reject_all(&mut self) {
        for file in &mut self.files {
            for hunk in &mut file.hunks {
                hunk.status = HunkStatus::Rejected;
            }
        }
    }

    /// Produces summary metrics of the diff session
    pub fn summary(&self) -> SpeculativeDiffSummary {
        let mut s = SpeculativeDiffSummary {
            total_files: self.files.len(),
            ..Default::default()
        };

        for file in &self.files {
            for hunk in &file.hunks {
                s.total_hunks += 1;
                match hunk.status {
                    HunkStatus::Accepted => s.accepted_hunks += 1,
                    HunkStatus::Rejected => s.rejected_hunks += 1,
                    HunkStatus::Pending => s.pending_hunks += 1,
                }
                s.total_additions += hunk.additions_count();
                s.total_deletions += hunk.deletions_count();
            }
        }

        s
    }

    /// Renders a unified diff view of the entire session
    pub fn render_unified(&self, colored: bool) -> String {
        let mut out = String::new();

        if self.files.is_empty() {
            return if colored {
                "No speculative diffs detected. Workspace matches baseline.".green().to_string()
            } else {
                "No speculative diffs detected. Workspace matches baseline.".to_string()
            };
        }

        for (f_idx, file) in self.files.iter().enumerate() {
            let status_badge = match file.status {
                FileDiffStatus::Modified => "modified",
                FileDiffStatus::Created => "new file",
                FileDiffStatus::Deleted => "deleted",
            };

            let header = format!("diff --speculative a/{} b/{} ({})\n", file.file_path, file.file_path, status_badge);
            let file_a = format!("--- a/{}\n", file.file_path);
            let file_b = format!("+++ b/{}\n", file.file_path);

            if colored {
                out.push_str(&header.bold().to_string());
                out.push_str(&file_a.red().to_string());
                out.push_str(&file_b.green().to_string());
            } else {
                out.push_str(&header);
                out.push_str(&file_a);
                out.push_str(&file_b);
            }

            for (h_idx, hunk) in file.hunks.iter().enumerate() {
                let is_current = f_idx == self.current_file_idx && h_idx == self.current_hunk_idx;
                let marker = if is_current { "▶ " } else { "  " };

                let hunk_hdr = format!(
                    "{}{} {} {}\n",
                    marker,
                    hunk.header,
                    hunk.status.badge(),
                    format!("(Hunk #{})", hunk.id).dimmed()
                );

                if colored {
                    out.push_str(&hunk_hdr);
                } else {
                    out.push_str(&format!(
                        "{}{} [{:?}] (Hunk #{})\n",
                        marker, hunk.header, hunk.status, hunk.id
                    ));
                }

                for line in &hunk.lines {
                    match line.kind {
                        DiffLineKind::Context => {
                            out.push_str(&format!("  {}\n", line.content));
                        }
                        DiffLineKind::Addition => {
                            let line_str = format!("+ {}\n", line.content);
                            if colored {
                                out.push_str(&line_str.green().to_string());
                            } else {
                                out.push_str(&line_str);
                            }
                        }
                        DiffLineKind::Deletion => {
                            let line_str = format!("- {}\n", line.content);
                            if colored {
                                out.push_str(&line_str.red().to_string());
                            } else {
                                out.push_str(&line_str);
                            }
                        }
                    }
                }
            }
            out.push('\n');
        }

        out
    }

    /// Renders a side-by-side split layout comparing original vs speculative
    pub fn render_side_by_side(&self, col_width: usize, colored: bool) -> String {
        let mut out = String::new();
        let width = col_width.max(30);

        for file in &self.files {
            let title = format!("=== {} (Side-by-Side Speculative Diff) ===", file.file_path);
            let sep = "=".repeat(width * 2 + 3);

            if colored {
                out.push_str(&title.cyan().bold().to_string());
                out.push('\n');
                out.push_str(&format!("{:<width$} | {:<width$}\n", "ORIGINAL (Baseline)", "SPECULATIVE (Proposal)", width = width).bold().to_string());
                out.push_str(&sep.dimmed().to_string());
                out.push('\n');
            } else {
                out.push_str(&title);
                out.push('\n');
                out.push_str(&format!("{:<width$} | {:<width$}\n", "ORIGINAL (Baseline)", "SPECULATIVE (Proposal)", width = width));
                out.push_str(&sep);
                out.push('\n');
            }

            for hunk in &file.hunks {
                let badge = hunk.status.badge();
                out.push_str(&format!("--- Hunk #{} ({}) {} ---\n", hunk.id, hunk.header, badge));

                let old_lines: Vec<&DiffLine> = hunk.lines.iter().filter(|l| l.kind != DiffLineKind::Addition).collect();
                let new_lines: Vec<&DiffLine> = hunk.lines.iter().filter(|l| l.kind != DiffLineKind::Deletion).collect();

                let max_lines = old_lines.len().max(new_lines.len());

                for i in 0..max_lines {
                    let left = old_lines.get(i).map(|l| truncate_str(&l.content, width)).unwrap_or_default();
                    let right = new_lines.get(i).map(|l| truncate_str(&l.content, width)).unwrap_or_default();

                    let is_old_del = old_lines.get(i).map(|l| l.kind == DiffLineKind::Deletion).unwrap_or(false);
                    let is_new_add = new_lines.get(i).map(|l| l.kind == DiffLineKind::Addition).unwrap_or(false);

                    let left_formatted = if colored && is_old_del {
                        format!("{:<width$}", left).red().to_string()
                    } else {
                        format!("{:<width$}", left)
                    };

                    let right_formatted = if colored && is_new_add {
                        format!("{:<width$}", right).green().to_string()
                    } else {
                        format!("{:<width$}", right)
                    };

                    out.push_str(&format!("{} | {}\n", left_formatted, right_formatted));
                }
            }
            out.push('\n');
        }

        out
    }

    /// Reconstructs content of a file applying only accepted hunks
    pub fn reconstruct_cherry_picked_content(&self, file_idx: usize) -> Result<String> {
        let file = self
            .files
            .get(file_idx)
            .ok_or_else(|| HgbError::NotFound(format!("File index {} out of range", file_idx)))?;

        if file.hunks.is_empty() {
            return Ok(file.original_content.clone());
        }

        let mut output_lines = Vec::new();
        let orig_lines: Vec<&str> = file.original_content.lines().collect();
        let mut orig_cursor = 1; // 1-indexed

        for hunk in &file.hunks {
            // Copy unchanged lines preceding this hunk
            while orig_cursor < hunk.old_start && (orig_cursor - 1) < orig_lines.len() {
                output_lines.push(orig_lines[orig_cursor - 1].to_string());
                orig_cursor += 1;
            }

            match hunk.status {
                HunkStatus::Accepted => {
                    // Apply additions, discard deletions
                    for line in &hunk.lines {
                        match line.kind {
                            DiffLineKind::Context => {
                                output_lines.push(line.content.clone());
                                orig_cursor += 1;
                            }
                            DiffLineKind::Addition => {
                                output_lines.push(line.content.clone());
                            }
                            DiffLineKind::Deletion => {
                                orig_cursor += 1; // skip deleted line in original
                            }
                        }
                    }
                }
                HunkStatus::Rejected | HunkStatus::Pending => {
                    // Keep original baseline: keep context and deletions, ignore additions
                    for line in &hunk.lines {
                        match line.kind {
                            DiffLineKind::Context | DiffLineKind::Deletion => {
                                output_lines.push(line.content.clone());
                                orig_cursor += 1;
                            }
                            DiffLineKind::Addition => {
                                // Skip rejected addition
                            }
                        }
                    }
                }
            }
        }

        // Copy remaining lines after last hunk
        while (orig_cursor - 1) < orig_lines.len() {
            output_lines.push(orig_lines[orig_cursor - 1].to_string());
            orig_cursor += 1;
        }

        let mut result = output_lines.join("\n");
        if !result.is_empty() && (file.original_content.ends_with('\n') || file.speculative_content.ends_with('\n')) {
            result.push('\n');
        }

        Ok(result)
    }

    /// Commits selective cherry-picked hunks directly to real workspace disk
    pub fn commit_to_disk(&self, workspace_root: &Path) -> Result<CherryPickCommitReport> {
        let mut files_committed = 0;
        let mut hunks_accepted = 0;
        let mut hunks_rejected = 0;
        let mut affected_paths = Vec::new();

        for (idx, file) in self.files.iter().enumerate() {
            let has_accepted = file.hunks.iter().any(|h| h.status == HunkStatus::Accepted);
            let accepted_count = file.hunks.iter().filter(|h| h.status == HunkStatus::Accepted).count();
            let rejected_count = file.hunks.iter().filter(|h| h.status == HunkStatus::Rejected).count();

            hunks_accepted += accepted_count;
            hunks_rejected += rejected_count;

            if !has_accepted && file.status != FileDiffStatus::Deleted {
                continue;
            }

            let target_path = workspace_root.join(&file.file_path);

            if file.status == FileDiffStatus::Deleted && file.hunks.iter().all(|h| h.status == HunkStatus::Accepted) {
                if target_path.exists() {
                    fs::remove_file(&target_path)?;
                    files_committed += 1;
                    affected_paths.push(file.file_path.clone());
                }
                continue;
            }

            let resolved_content = self.reconstruct_cherry_picked_content(idx)?;

            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::write(&target_path, resolved_content.as_bytes())?;
            files_committed += 1;
            affected_paths.push(file.file_path.clone());
        }

        Ok(CherryPickCommitReport {
            files_committed,
            hunks_accepted,
            hunks_rejected,
            affected_paths,
        })
    }
}

// ---------------------------------------------------------------------------
// Myers-style line diff algorithm helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditOp {
    Equal(String),
    Insert(String),
    Delete(String),
}

fn compute_line_diff(a: &[&str], b: &[&str]) -> Vec<EditOp> {
    let n = a.len();
    let m = b.len();

    // Fast path: identical
    if a == b {
        return a.iter().map(|&s| EditOp::Equal(s.to_string())).collect();
    }

    // Fast path: empty old
    if n == 0 {
        return b.iter().map(|&s| EditOp::Insert(s.to_string())).collect();
    }

    // Fast path: empty new
    if m == 0 {
        return a.iter().map(|&s| EditOp::Delete(s.to_string())).collect();
    }

    // Standard LCS dynamic programming table for line diffs
    let mut dp = vec![vec![0usize; m + 1]; n + 1];

    for i in 0..n {
        for j in 0..m {
            if a[i] == b[j] {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    // Backtrack to find edits
    let mut ops = Vec::new();
    let mut i = n;
    let mut j = m;

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] {
            ops.push(EditOp::Equal(a[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push(EditOp::Insert(b[j - 1].to_string()));
            j -= 1;
        } else if i > 0 {
            ops.push(EditOp::Delete(a[i - 1].to_string()));
            i -= 1;
        }
    }

    ops.reverse();
    ops
}

fn group_diff_ops_into_hunks(file_path: &str, ops: &[EditOp], context_lines: usize) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();

    // Find indices of edits (Insert or Delete)
    let edit_indices: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| match op {
            EditOp::Equal(_) => None,
            _ => Some(idx),
        })
        .collect();

    if edit_indices.is_empty() {
        return hunks;
    }

    // Group adjacent edits that are within 2 * context_lines of each other
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut current_start = edit_indices[0].saturating_sub(context_lines);
    let mut current_end = (edit_indices[0] + context_lines).min(ops.len().saturating_sub(1));

    for &edit_idx in &edit_indices[1..] {
        let edit_start = edit_idx.saturating_sub(context_lines);
        let edit_end = (edit_idx + context_lines).min(ops.len().saturating_sub(1));

        if edit_start <= current_end + 1 {
            current_end = current_end.max(edit_end);
        } else {
            groups.push((current_start, current_end));
            current_start = edit_start;
            current_end = edit_end;
        }
    }
    groups.push((current_start, current_end));

    let mut hunk_id = 1;
    let mut old_lineno_acc = 1;
    let mut new_lineno_acc = 1;
    let mut op_cursor = 0;

    for (start, end) in groups {
        // Catch up line numbers to start
        while op_cursor < start {
            match &ops[op_cursor] {
                EditOp::Equal(_) => {
                    old_lineno_acc += 1;
                    new_lineno_acc += 1;
                }
                EditOp::Delete(_) => {
                    old_lineno_acc += 1;
                }
                EditOp::Insert(_) => {
                    new_lineno_acc += 1;
                }
            }
            op_cursor += 1;
        }

        let old_start = old_lineno_acc;
        let new_start = new_lineno_acc;
        let mut old_count = 0;
        let mut new_count = 0;
        let mut lines = Vec::new();

        while op_cursor <= end && op_cursor < ops.len() {
            match &ops[op_cursor] {
                EditOp::Equal(text) => {
                    lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        old_lineno: Some(old_lineno_acc),
                        new_lineno: Some(new_lineno_acc),
                        content: text.clone(),
                    });
                    old_lineno_acc += 1;
                    new_lineno_acc += 1;
                    old_count += 1;
                    new_count += 1;
                }
                EditOp::Insert(text) => {
                    lines.push(DiffLine {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(new_lineno_acc),
                        content: text.clone(),
                    });
                    new_lineno_acc += 1;
                    new_count += 1;
                }
                EditOp::Delete(text) => {
                    lines.push(DiffLine {
                        kind: DiffLineKind::Deletion,
                        old_lineno: Some(old_lineno_acc),
                        new_lineno: None,
                        content: text.clone(),
                    });
                    old_lineno_acc += 1;
                    old_count += 1;
                }
            }
            op_cursor += 1;
        }

        let header = format!("@@ -{},{} +{},{} @@", old_start, old_count, new_start, new_count);

        hunks.push(DiffHunk {
            id: hunk_id,
            file_path: file_path.to_string(),
            old_start,
            old_count,
            new_start,
            new_count,
            header,
            lines,
            status: HunkStatus::Pending,
        });

        hunk_id += 1;
    }

    hunks
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hunks_simple_edit() {
        let old = "fn main() {\n    println!(\"hello\");\n}\n";
        let new = "fn main() {\n    println!(\"hello world!\");\n}\n";

        let hunks = SpeculativeDiffSession::compute_hunks("src/main.rs", old, new);
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert_eq!(h.file_path, "src/main.rs");
        assert_eq!(h.additions_count(), 1);
        assert_eq!(h.deletions_count(), 1);
    }

    #[test]
    fn test_cherry_pick_acceptance_and_rejection() {
        let old = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let new = "line 1\nline 2 MODIFIED\nline 3\nline 4 MODIFIED\nline 5\n";

        let mut session = SpeculativeDiffSession::from_contents("test.txt", old, new);
        assert!(!session.files.is_empty());
        let file = &session.files[0];
        assert!(!file.hunks.is_empty());

        // Accept all hunks
        session.accept_all();
        let reconstructed = session.reconstruct_cherry_picked_content(0).unwrap();
        assert!(reconstructed.contains("MODIFIED"));

        // Reject all hunks
        session.reject_all();
        let rejected_recon = session.reconstruct_cherry_picked_content(0).unwrap();
        assert_eq!(rejected_recon, old);
    }

    #[test]
    fn test_render_unified_and_side_by_side() {
        let old = "alpha\nbeta\ngamma\n";
        let new = "alpha\nbeta updated\ngamma\n";

        let session = SpeculativeDiffSession::from_contents("demo.txt", old, new);
        let unified = session.render_unified(false);
        assert!(unified.contains("diff --speculative"));
        assert!(unified.contains("+ beta updated"));
        assert!(unified.contains("- beta"));

        let side_by_side = session.render_side_by_side(40, false);
        assert!(side_by_side.contains("ORIGINAL (Baseline)"));
        assert!(side_by_side.contains("SPECULATIVE (Proposal)"));
    }

    #[test]
    fn test_commit_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("code.rs");
        fs::write(&file_path, "fn old() {}\n").unwrap();

        let mut session = SpeculativeDiffSession::from_contents("code.rs", "fn old() {}\n", "fn new() {}\n");
        session.accept_all();

        let report = session.commit_to_disk(temp.path()).unwrap();
        assert_eq!(report.files_committed, 1);
        assert_eq!(report.hunks_accepted, 1);

        let content_on_disk = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content_on_disk, "fn new() {}\n");
    }
}
