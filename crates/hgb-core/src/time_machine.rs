//! # ChronoWarp Time-Machine TUI & Ledger (`time_machine`)
//!
//! Persistent timeline snapshot ledger stored in `.hgb/timelines/`.
//! Records every speculative branch, compiler attempt, and edit with timestamp,
//! BLAKE3 checksum, modified file list, diff content, test status, and outcome.
//! Implements snapshot recording, listing, restoring, and terminal timeline graph scrubber.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::Digest;

/// Outcome status of an edit or speculative branch
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotOutcome {
    Accepted,
    Discarded,
    RolledBack,
}

impl std::fmt::Display for SnapshotOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accepted => write!(f, "Accepted"),
            Self::Discarded => write!(f, "Discarded"),
            Self::RolledBack => write!(f, "RolledBack"),
        }
    }
}

/// A recorded timeline snapshot with cryptographic verification and outcome tracking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineSnapshot {
    pub snapshot_id: String,
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub blake3_hash: String,
    pub branch_name: String,
    pub modified_files: Vec<String>,
    pub diff_content: String,
    pub test_passed: bool,
    pub outcome: SnapshotOutcome,
    pub metadata: HashMap<String, String>,
}

impl TimelineSnapshot {
    pub fn new(
        session_id: &str,
        branch_name: &str,
        modified_files: Vec<String>,
        diff_content: &str,
        test_passed: bool,
        outcome: SnapshotOutcome,
    ) -> Self {
        let timestamp = Utc::now();
        let mut hasher = blake3::Hasher::new();
        hasher.update(session_id.as_bytes());
        hasher.update(branch_name.as_bytes());
        for f in &modified_files {
            hasher.update(f.as_bytes());
        }
        hasher.update(diff_content.as_bytes());
        hasher.update(timestamp.to_rfc3339().as_bytes());
        let hash = hasher.finalize().to_hex().to_string();

        let snapshot_id = format!("chrono-{}-{}", timestamp.timestamp(), &hash[0..8]);

        Self {
            snapshot_id,
            timestamp,
            session_id: session_id.to_string(),
            blake3_hash: hash,
            branch_name: branch_name.to_string(),
            modified_files,
            diff_content: diff_content.to_string(),
            test_passed,
            outcome,
            metadata: HashMap::new(),
        }
    }
}

/// Structured diff line representation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Add(String),    // In original forward diff: '+'
    Remove(String), // In original forward diff: '-'
}

/// Git-compatible unified patch hunk
#[derive(Debug, Clone)]
pub struct GitPatchHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

/// ChronoWarp Time-Machine Storage & Scrubber Engine
pub struct TimeMachineEngine {
    pub timeline_dir: PathBuf,
}

impl TimeMachineEngine {
    pub fn new(workspace_root: &Path) -> Self {
        let timeline_dir = workspace_root.join(".hgb").join("timelines");
        Self { timeline_dir }
    }

    pub fn with_custom_dir(timeline_dir: PathBuf) -> Self {
        Self { timeline_dir }
    }

    /// Ensure storage directory exists
    fn ensure_storage(&self) -> std::io::Result<()> {
        if !self.timeline_dir.exists() {
            fs::create_dir_all(&self.timeline_dir)?;
        }
        Ok(())
    }

    /// Record snapshot into timeline ledger
    pub fn record_snapshot(&self, snapshot: &TimelineSnapshot) -> Result<(), String> {
        self.ensure_storage().map_err(|e| format!("Failed to create timeline dir: {}", e))?;

        let file_path = self.timeline_dir.join(format!("{}.json", snapshot.snapshot_id));
        let serialized = serde_json::to_string_pretty(snapshot)
            .map_err(|e| format!("Serialization error: {}", e))?;

        fs::write(&file_path, serialized)
            .map_err(|e| format!("Failed to write snapshot {}: {}", file_path.display(), e))?;

        // Update index ledger
        let index_path = self.timeline_dir.join("ledger.json");
        let mut ids: Vec<String> = if index_path.exists() {
            let data = fs::read_to_string(&index_path).unwrap_or_else(|_| "[]".to_string());
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Vec::new()
        };

        if !ids.contains(&snapshot.snapshot_id) {
            ids.push(snapshot.snapshot_id.clone());
            let _ = fs::write(&index_path, serde_json::to_string_pretty(&ids).unwrap_or_default());
        }

        Ok(())
    }

    /// Retrieve a specific snapshot by ID
    pub fn get_snapshot(&self, id: &str) -> Result<TimelineSnapshot, String> {
        let clean_id = if id.ends_with(".json") {
            id.trim_end_matches(".json").to_string()
        } else {
            id.to_string()
        };

        let file_path = self.timeline_dir.join(format!("{}.json", clean_id));
        if !file_path.exists() {
            return Err(format!("Snapshot '{}' not found at {}", clean_id, file_path.display()));
        }

        let content = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to read snapshot file: {}", e))?;

        serde_json::from_str(&content)
            .map_err(|e| format!("Corrupted snapshot JSON: {}", e))
    }

    /// List all snapshots in chronological order
    pub fn list_snapshots(&self) -> Result<Vec<TimelineSnapshot>, String> {
        if !self.timeline_dir.exists() {
            return Ok(Vec::new());
        }

        let mut snapshots = Vec::new();
        let entries = fs::read_dir(&self.timeline_dir)
            .map_err(|e| format!("Failed to read timeline dir: {}", e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if file_name != "ledger.json" {
                        if let Ok(content) = fs::read_to_string(&path) {
                            if let Ok(snap) = serde_json::from_str::<TimelineSnapshot>(&content) {
                                snapshots.push(snap);
                            }
                        }
                    }
                }
            }
        }

        // Sort chronologically ascending
        snapshots.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(snapshots)
    }

    /// Restore modified files from snapshot diff/record back into workspace
    pub fn restore_snapshot(&self, id: &str, workspace_root: &Path) -> Result<Vec<String>, String> {
        let snapshot = self.get_snapshot(id)?;
        let mut restored_files = Vec::new();

        // If file content backup exists in .hgb/timelines/data/<snapshot_id>/
        let backup_dir = self.timeline_dir.join("data").join(&snapshot.snapshot_id);
        if backup_dir.exists() {
            for file_rel in &snapshot.modified_files {
                let backup_file = backup_dir.join(file_rel);
                let target_file = workspace_root.join(file_rel);

                if backup_file.exists() {
                    if let Some(parent) = target_file.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if fs::copy(&backup_file, &target_file).is_ok() {
                        restored_files.push(file_rel.clone());
                    }
                }
            }
        }

        // Fallback: Parse unified diff content and reconstruct files if no raw backup
        if restored_files.is_empty() && !snapshot.diff_content.is_empty() {
            match Self::apply_reverse_unified_diff(&snapshot.diff_content, workspace_root, &snapshot.modified_files) {
                Ok(files) => {
                    restored_files.extend(files);
                }
                Err(e) => {
                    return Err(format!("Failed to reverse diff for snapshot {}: {}", id, e));
                }
            }
        }

        if restored_files.is_empty() {
            return Err(format!("No files could be restored for snapshot {}: diff could not be applied and no raw backup exists", id));
        }

        // Record a rollback snapshot to preserve undo capability
        let rollback_snap = TimelineSnapshot::new(
            &snapshot.session_id,
            &format!("rollback-to-{}", snapshot.snapshot_id),
            snapshot.modified_files.clone(),
            &format!("Reverted to snapshot {}", snapshot.snapshot_id),
            snapshot.test_passed,
            SnapshotOutcome::RolledBack,
        );
        let _ = self.record_snapshot(&rollback_snap);

        Ok(restored_files)
    }

    /// Parse and apply reverse diff to files in workspace_root with Git-compatible hunk validation
    pub fn apply_reverse_unified_diff(
        diff: &str,
        workspace_root: &Path,
        modified_files: &[String],
    ) -> Result<Vec<String>, String> {
        let mut restored = Vec::new();

        if diff.trim().is_empty() {
            return Ok(restored);
        }

        // Parse diff into per-file hunks
        let file_hunks = Self::parse_unified_diff_hunks(diff, modified_files);
        if file_hunks.is_empty() && !modified_files.is_empty() {
            return Err("Failed to parse any valid Git unified diff hunks from diff content".to_string());
        }

        // Transactional staging: path -> Some(content) or None (delete)
        let mut staged_writes: HashMap<PathBuf, Option<String>> = HashMap::new();

        for file_rel in modified_files {
            let target_file = workspace_root.join(file_rel);
            let hunks = match file_hunks.get(file_rel) {
                Some(h) if !h.is_empty() => h,
                _ => {
                    // If diff has only one file's hunks without matching filename header, use those if single file
                    if modified_files.len() == 1 && file_hunks.len() == 1 {
                        file_hunks.values().next().unwrap()
                    } else {
                        return Err(format!("No diff hunks found for modified file '{}'", file_rel));
                    }
                }
            };

            let current_content = if target_file.exists() {
                fs::read_to_string(&target_file)
                    .map_err(|e| format!("Failed to read {}: {}", target_file.display(), e))?
            } else {
                String::new()
            };

            // Compute pre-patch SHA-256
            let sha_before = {
                let mut hasher = sha2::Sha256::new();
                sha2::Digest::update(&mut hasher, current_content.as_bytes());
                hex::encode(sha2::Digest::finalize(hasher))
            };

            let reversed_content = Self::apply_hunks_reverse(
                &current_content,
                hunks,
                target_file.exists(),
                file_rel,
            )?;

            // If reversed content is Some, verify post-patch state
            if let Some(ref content) = reversed_content {
                let mut hasher = sha2::Sha256::new();
                sha2::Digest::update(&mut hasher, content.as_bytes());
                let sha_after = hex::encode(sha2::Digest::finalize(hasher));

                if target_file.exists() && sha_before == sha_after && !hunks.is_empty() {
                    let has_effective_changes = hunks.iter().any(|h| {
                        h.lines.iter().any(|l| matches!(l, DiffLine::Add(_) | DiffLine::Remove(_)))
                    });
                    if has_effective_changes {
                        return Err(format!(
                            "Reverse diff validation failed: file '{}' was unchanged after applying hunks (SHA-256: {})",
                            file_rel, sha_before
                        ));
                    }
                }
            }

            staged_writes.insert(target_file, reversed_content);
            restored.push(file_rel.clone());
        }

        // If all files validated and applied cleanly in-memory, commit transaction to disk
        for (file_path, content_opt) in staged_writes {
            match content_opt {
                Some(content) => {
                    if let Some(parent) = file_path.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("Failed to create parent dir for {}: {}", file_path.display(), e))?;
                    }
                    fs::write(&file_path, content)
                        .map_err(|e| format!("Failed to write {}: {}", file_path.display(), e))?;
                }
                None => {
                    if file_path.exists() {
                        fs::remove_file(&file_path)
                            .map_err(|e| format!("Failed to remove rolled-back file {}: {}", file_path.display(), e))?;
                    }
                }
            }
        }

        Ok(restored)
    }

    /// Parses unified diff into structured per-file hunks
    pub fn parse_unified_diff_hunks(diff: &str, modified_files: &[String]) -> HashMap<String, Vec<GitPatchHunk>> {
        let mut result: HashMap<String, Vec<GitPatchHunk>> = HashMap::new();
        let lines: Vec<&str> = diff.lines().collect();
        let mut idx = 0;
        let mut current_file: Option<String> = if modified_files.len() == 1 {
            Some(modified_files[0].clone())
        } else {
            None
        };

        while idx < lines.len() {
            let line = lines[idx];

            // Match diff header: diff --git a/path b/path
            if line.starts_with("diff --git ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let b_path = parts[3].trim_start_matches("b/").trim_start_matches("a/");
                    current_file = Some(b_path.to_string());
                }
                idx += 1;
                continue;
            }

            // Match --- a/path or +++ b/path
            if line.starts_with("--- ") || line.starts_with("+++ ") {
                let raw_path = line[4..].trim().split('\t').next().unwrap_or("").trim();
                let clean = raw_path.trim_start_matches("a/").trim_start_matches("b/");
                if clean != "/dev/null" && !clean.is_empty() {
                    for mf in modified_files {
                        if clean == mf || clean.ends_with(mf) || mf.ends_with(clean) {
                            current_file = Some(mf.clone());
                            break;
                        }
                    }
                }
                idx += 1;
                continue;
            }

            // Match hunk header @@ -old_start,old_count +new_start,new_count @@
            if line.starts_with("@@ ") {
                if let Some((old_start, old_count, new_start, new_count)) = Self::parse_hunk_header(line) {
                    let file_key = current_file.clone().unwrap_or_else(|| {
                        modified_files.first().cloned().unwrap_or_else(|| "unknown".to_string())
                    });

                    idx += 1;
                    let mut hunk_lines = Vec::new();

                    while idx < lines.len() {
                        let h_line = lines[idx];
                        if h_line.starts_with("@@ ") || h_line.starts_with("diff --git ") || h_line.starts_with("--- ") {
                            break;
                        }
                        if let Some(ch) = h_line.chars().next() {
                            match ch {
                                ' ' => hunk_lines.push(DiffLine::Context(h_line[1..].to_string())),
                                '+' => hunk_lines.push(DiffLine::Add(h_line[1..].to_string())),
                                '-' => hunk_lines.push(DiffLine::Remove(h_line[1..].to_string())),
                                '\\' => {} // "\ No newline at end of file"
                                _ => {
                                    // Treat as context line if unadorned
                                    hunk_lines.push(DiffLine::Context(h_line.to_string()));
                                }
                            }
                        } else {
                            // Empty line treated as empty context line
                            hunk_lines.push(DiffLine::Context(String::new()));
                        }
                        idx += 1;
                    }

                    let hunk = GitPatchHunk {
                        old_start,
                        old_count,
                        new_start,
                        new_count,
                        lines: hunk_lines,
                    };

                    result.entry(file_key).or_default().push(hunk);
                    continue;
                }
            }

            idx += 1;
        }

        result
    }

    /// Parse header @@ -old_start,old_count +new_start,new_count @@
    fn parse_hunk_header(header: &str) -> Option<(usize, usize, usize, usize)> {
        let rest = header.strip_prefix("@@ -")?;
        let at_pos = rest.find(" @@")?;
        let spec = &rest[..at_pos];

        let mut parts = spec.split(' ');
        let old_part = parts.next()?;
        let new_part = parts.next()?.strip_prefix('+')?;

        let (old_start, old_count) = if let Some((s, c)) = old_part.split_once(',') {
            (s.parse::<usize>().ok()?, c.parse::<usize>().ok()?)
        } else {
            (old_part.parse::<usize>().ok()?, 1)
        };

        let (new_start, new_count) = if let Some((s, c)) = new_part.split_once(',') {
            (s.parse::<usize>().ok()?, c.parse::<usize>().ok()?)
        } else {
            (new_part.parse::<usize>().ok()?, 1)
        };

        Some((old_start, old_count, new_start, new_count))
    }

    /// Apply hunks in reverse to a single file's content
    fn apply_hunks_reverse(
        current_content: &str,
        hunks: &[GitPatchHunk],
        file_exists_on_disk: bool,
        _file_rel: &str,
    ) -> Result<Option<String>, String> {
        // Check for file creation in forward diff (all added lines, old_start == 0, old_count == 0)
        let is_pure_addition = hunks.iter().all(|h| {
            h.old_count == 0 && h.lines.iter().all(|l| matches!(l, DiffLine::Add(_)))
        });

        if is_pure_addition && file_exists_on_disk {
            return Ok(None);
        }

        // Check for file deletion in forward diff (all removed lines, new_count == 0)
        let is_pure_deletion = hunks.iter().all(|h| {
            h.new_count == 0 && h.lines.iter().all(|l| matches!(l, DiffLine::Remove(_)))
        });

        if is_pure_deletion && !file_exists_on_disk {
            let mut recreated = Vec::new();
            for h in hunks {
                for l in &h.lines {
                    if let DiffLine::Remove(s) = l {
                        recreated.push(s.as_str());
                    }
                }
            }
            let mut out = recreated.join("\n");
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            return Ok(Some(out));
        }

        let mut lines: Vec<String> = current_content.lines().map(|s| s.to_string()).collect();

        // Sort hunks by new_start descending so applying lower hunks doesn't shift line numbers of upper hunks
        let mut sorted_hunks = hunks.to_vec();
        sorted_hunks.sort_by(|a, b| b.new_start.cmp(&a.new_start));

        for hunk in &sorted_hunks {
            let mut expected_post: Vec<String> = Vec::new();
            let mut replacement_pre: Vec<String> = Vec::new();

            for dl in &hunk.lines {
                match dl {
                    DiffLine::Context(c) => {
                        expected_post.push(c.clone());
                        replacement_pre.push(c.clone());
                    }
                    DiffLine::Add(a) => {
                        expected_post.push(a.clone());
                    }
                    DiffLine::Remove(r) => {
                        replacement_pre.push(r.clone());
                    }
                }
            }

            if expected_post.is_empty() && replacement_pre.is_empty() {
                continue;
            }

            let target_line_idx = hunk.new_start.saturating_sub(1);
            let match_idx = Self::locate_hunk_match(&lines, &expected_post, target_line_idx)?;

            lines.splice(match_idx..match_idx + expected_post.len(), replacement_pre);
        }

        let mut reconstructed = lines.join("\n");
        if current_content.ends_with('\n') || (lines.is_empty() && current_content.is_empty()) {
            if !reconstructed.ends_with('\n') && !reconstructed.is_empty() {
                reconstructed.push('\n');
            }
        }

        Ok(Some(reconstructed))
    }

    /// Locates exact line index for a hunk match, testing nominal line first, then searching fuzz radius
    fn locate_hunk_match(
        file_lines: &[String],
        expected: &[String],
        target_idx: usize,
    ) -> Result<usize, String> {
        if expected.is_empty() {
            return Ok(target_idx.min(file_lines.len()));
        }

        let exp_len = expected.len();

        // 1. Check exact nominal index
        if target_idx + exp_len <= file_lines.len() {
            if &file_lines[target_idx..target_idx + exp_len] == expected {
                return Ok(target_idx);
            }
        }

        // 2. Search fuzz radius: ±1, ±2, ±3, ±4, ±5
        for offset in 1..=5 {
            if let Some(cand) = target_idx.checked_add(offset) {
                if cand + exp_len <= file_lines.len() && &file_lines[cand..cand + exp_len] == expected {
                    return Ok(cand);
                }
            }
            if let Some(cand) = target_idx.checked_sub(offset) {
                if cand + exp_len <= file_lines.len() && &file_lines[cand..cand + exp_len] == expected {
                    return Ok(cand);
                }
            }
        }

        // 3. Fallback: full file search with unambiguous check
        let mut candidates = Vec::new();
        for i in 0..=file_lines.len().saturating_sub(exp_len) {
            if &file_lines[i..i + exp_len] == expected {
                candidates.push(i);
            }
        }

        if candidates.len() == 1 {
            return Ok(candidates[0]);
        }

        if candidates.len() > 1 {
            return Err(format!(
                "Hunk context matches multiple ({}) ambiguous locations in file; cannot safely apply patch without corruption",
                candidates.len()
            ));
        }

        Err(format!(
            "Hunk failed to apply: expected context not found around line {} (expected {} lines: '{}')",
            target_idx + 1,
            exp_len,
            expected.first().map(|s| s.as_str()).unwrap_or("")
        ))
    }

    /// Render interactive / terminal timeline scrubber graph
    pub fn render_timeline_scrubber(&self, current_selection_idx: Option<usize>) -> Result<String, String> {
        let snapshots = self.list_snapshots()?;
        let total = snapshots.len();

        let mut out = String::new();
        let border = "═".repeat(78);
        out.push_str(&format!("╔{}╗\n", border));
        out.push_str(&format!(
            "║  {}  ─  {} snapshots recorded  ║\n",
            "CHRONOWARP TIME-MACHINE LEDGER".bold().cyan(),
            total.to_string().yellow().bold()
        ));
        out.push_str(&format!("╠{}╣\n", border));

        if snapshots.is_empty() {
            out.push_str("║  No snapshots recorded yet in .hgb/timelines/                               ║\n");
            out.push_str(&format!("╚{}╝\n", border));
            return Ok(out);
        }

        for (idx, snap) in snapshots.iter().enumerate() {
            let is_selected = current_selection_idx == Some(idx) || (current_selection_idx.is_none() && idx + 1 == total);
            let selector = if is_selected {
                "▶".green().bold().to_string()
            } else {
                " ".to_string()
            };

            let node_symbol = match snap.outcome {
                SnapshotOutcome::Accepted => "●".green().bold(),
                SnapshotOutcome::Discarded => "✖".red().bold(),
                SnapshotOutcome::RolledBack => "⮑".yellow().bold(),
            };

            let test_status = if snap.test_passed {
                "✔ PASS".green()
            } else {
                "✘ FAIL".red()
            };

            let outcome_tag = match snap.outcome {
                SnapshotOutcome::Accepted => "[ACCEPTED]".green().bold(),
                SnapshotOutcome::Discarded => "[DISCARDED]".red().bold(),
                SnapshotOutcome::RolledBack => "[ROLLEDBACK]".yellow().bold(),
            };

            let time_str = snap.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
            let branch_str = format!("[{}]", snap.branch_name);
            let files_summary = if snap.modified_files.is_empty() {
                "no files".dimmed().to_string()
            } else {
                format!("{} files ({})", snap.modified_files.len(), snap.modified_files.join(", "))
            };

            let pointer_str = if is_selected {
                "  ◄── ACTIVE SCRUBBER HEAD".cyan().bold().to_string()
            } else {
                String::new()
            };

            let connector = if idx + 1 == total { "└──" } else { "├──" };

            out.push_str(&format!(
                "║ {} {} {} {} {} {}  {} {}\n",
                selector,
                connector.dimmed(),
                node_symbol,
                snap.snapshot_id.bold().white(),
                test_status,
                outcome_tag,
                branch_str.magenta(),
                pointer_str
            ));

            out.push_str(&format!(
                "║      │   {} • hash: {} • {}\n",
                time_str.dimmed(),
                &snap.blake3_hash[0..12].dimmed(),
                files_summary
            ));

            if idx + 1 < total {
                out.push_str("║      │\n");
            }
        }

        out.push_str(&format!("╚{}╝\n", border));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeline_snapshot_creation_and_hashing() {
        let snap = TimelineSnapshot::new(
            "session-alpha",
            "feature/audio",
            vec!["src/voice.rs".to_string(), "Cargo.toml".to_string()],
            "+ pub fn record() {}",
            true,
            SnapshotOutcome::Accepted,
        );

        assert!(snap.snapshot_id.starts_with("chrono-"));
        assert_eq!(snap.blake3_hash.len(), 64);
        assert_eq!(snap.outcome, SnapshotOutcome::Accepted);
        assert!(snap.test_passed);
        assert_eq!(snap.modified_files.len(), 2);
    }

    #[test]
    fn test_timeline_engine_record_list_restore() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_tm_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let engine = TimeMachineEngine::with_custom_dir(temp_dir.clone());

        let snap1 = TimelineSnapshot::new(
            "sess-1",
            "main",
            vec!["src/lib.rs".to_string()],
            "+ pub mod test;",
            true,
            SnapshotOutcome::Accepted,
        );

        let snap2 = TimelineSnapshot::new(
            "sess-1",
            "speculative-branch",
            vec!["src/lib.rs".to_string()],
            "+ pub mod broken;",
            false,
            SnapshotOutcome::Discarded,
        );

        engine.record_snapshot(&snap1).expect("Failed to record snap1");
        engine.record_snapshot(&snap2).expect("Failed to record snap2");

        let snapshots = engine.list_snapshots().expect("Failed to list snapshots");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].snapshot_id, snap1.snapshot_id);
        assert_eq!(snapshots[1].snapshot_id, snap2.snapshot_id);

        let retrieved = engine.get_snapshot(&snap1.snapshot_id).expect("Failed to retrieve snap1");
        assert_eq!(retrieved.snapshot_id, snap1.snapshot_id);
        assert_eq!(retrieved.outcome, SnapshotOutcome::Accepted);

        let rendered = engine.render_timeline_scrubber(Some(0)).expect("Failed to render scrubber");
        assert!(rendered.contains("CHRONOWARP TIME-MACHINE LEDGER"));
        assert!(rendered.contains(&snap1.snapshot_id));
        assert!(rendered.contains(&snap2.snapshot_id));
        assert!(rendered.contains("ACTIVE SCRUBBER HEAD"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_apply_reverse_unified_diff_multi_hunk() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_tm_diff_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("src/service.rs");
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();

        // Target file in modified state:
        let modified_file_content = "\
// Service Header
fn init() {
    println!(\"modified_init\");
}

fn worker() {
    let x = 10;
    println!(\"modified_worker\");
}

fn cleanup() {
    println!(\"cleanup\");
}
";
        fs::write(&file_path, modified_file_content).unwrap();

        // Forward diff that changed init and worker:
        let diff = "\
--- a/src/service.rs
+++ b/src/service.rs
@@ -2,3 +2,3 @@
 fn init() {
-    println!(\"original_init\");
+    println!(\"modified_init\");
 }
@@ -7,4 +7,4 @@
 fn worker() {
     let x = 10;
-    println!(\"original_worker\");
+    println!(\"modified_worker\");
 }
";

        let res = TimeMachineEngine::apply_reverse_unified_diff(
            diff,
            &temp_dir,
            &["src/service.rs".to_string()],
        ).expect("Multi-hunk reverse diff should apply cleanly");

        assert_eq!(res, vec!["src/service.rs"]);

        let restored_content = fs::read_to_string(&file_path).unwrap();
        assert!(restored_content.contains("original_init"));
        assert!(restored_content.contains("original_worker"));
        assert!(!restored_content.contains("modified_init"));
        assert!(!restored_content.contains("modified_worker"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_apply_reverse_repeated_lines_handling() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_tm_rep_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("repeated.txt");
        // File has identical repeated lines
        let modified_file_content = "\
line_item
line_item
TARGET_MODIFIED
line_item
line_item
";
        fs::write(&file_path, modified_file_content).unwrap();

        // Diff targeting line 3 specifically with surrounding context
        let diff = "\
--- a/repeated.txt
+++ b/repeated.txt
@@ -2,3 +2,3 @@
 line_item
-TARGET_ORIGINAL
+TARGET_MODIFIED
 line_item
";

        let res = TimeMachineEngine::apply_reverse_unified_diff(
            diff,
            &temp_dir,
            &["repeated.txt".to_string()],
        ).expect("Repeated lines should resolve correctly by line position");

        assert_eq!(res, vec!["repeated.txt"]);

        let restored_content = fs::read_to_string(&file_path).unwrap();
        assert!(restored_content.contains("TARGET_ORIGINAL"));
        assert!(!restored_content.contains("TARGET_MODIFIED"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_apply_reverse_clean_rejection_on_mismatch() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_tm_reject_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("file.rs");
        let initial_content = "fn main() {\n    println!(\"different code\");\n}\n";
        fs::write(&file_path, initial_content).unwrap();

        // Diff expecting context that doesn't match
        let diff = "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"alpha\");
+    println!(\"beta\");
 }
";

        let res = TimeMachineEngine::apply_reverse_unified_diff(
            diff,
            &temp_dir,
            &["file.rs".to_string()],
        );

        assert!(res.is_err(), "Patch must fail cleanly on context mismatch");
        // Verify working file remains 100% untouched
        let content_after = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content_after, initial_content);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
