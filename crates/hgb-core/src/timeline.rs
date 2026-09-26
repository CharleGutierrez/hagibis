//! # Ephemeral Worktree "What-If" Timelines
//!
//! Systems-grade isolated timeline management allowing developers and agents
//! to fork experiments, run what-if mutations in parallel git worktrees,
//! and diff/merge/discard safely without touching working branches.
//! Falls back automatically to isolated filesystem snapshots when git is unavailable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use serde::{Deserialize, Serialize};
use crate::error::{HgbError, Result};

/// Metadata description of an active timeline
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineInfo {
    pub name: String,
    pub path: PathBuf,
    pub created_at: String,
    pub base_branch: Option<String>,
    pub is_git_worktree: bool,
    pub head_commit: Option<String>,
}

/// Diff report comparing a timeline branch/snapshot with base
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineDiff {
    pub timeline_name: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub diff_content: String,
    pub modified_files: Vec<String>,
}

/// Result of merging a timeline back into the main workspace
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineMergeReport {
    pub timeline_name: String,
    pub merged_files: Vec<String>,
    pub commit_hash: Option<String>,
    pub success: bool,
    pub message: String,
}

/// Manages parallel "what-if" timelines under `.hgb/timelines/`
pub struct TimelineManager {
    workspace_root: PathBuf,
    timelines_root: PathBuf,
}

impl TimelineManager {
    /// Create a new TimelineManager for the target workspace
    pub fn new<P: Into<PathBuf>>(workspace_root: P) -> Self {
        let ws = workspace_root.into();
        let timelines_root = ws.join(".hgb").join("timelines");
        Self {
            workspace_root: ws,
            timelines_root,
        }
    }

    /// Explicitly configure custom timelines directory
    pub fn with_timelines_root<P: Into<PathBuf>>(mut self, timelines_root: P) -> Self {
        self.timelines_root = timelines_root.into();
        self
    }

    /// Check if the workspace is a valid git repository
    pub fn is_git_repo(&self) -> bool {
        if !self.workspace_root.join(".git").exists() {
            return false;
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.workspace_root)
            .arg("rev-parse")
            .arg("--is-inside-work-tree")
            .output();

        match output {
            Ok(out) => out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true",
            Err(_) => false,
        }
    }

    /// Sanitize and validate timeline name
    fn validate_name(name: &str) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(HgbError::Execution("Timeline name cannot be empty".into()));
        }
        if !trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(HgbError::Execution(format!(
                "Invalid timeline name '{}': must only contain alphanumeric, dash, or underscore characters",
                name
            )));
        }
        Ok(())
    }

    /// Get target path for timeline
    pub fn timeline_path(&self, name: &str) -> PathBuf {
        self.timelines_root.join(name)
    }

    /// Ensure timelines root directory exists
    fn ensure_timelines_root(&self) -> Result<()> {
        if !self.timelines_root.exists() {
            std::fs::create_dir_all(&self.timelines_root)?;
        }
        Ok(())
    }

    /// Create a new ephemeral timeline. Attempts git worktree first; falls back to snapshot.
    pub fn create_timeline(&self, name: &str, base_branch: Option<&str>) -> Result<TimelineInfo> {
        Self::validate_name(name)?;
        self.ensure_timelines_root()?;

        let target_path = self.timeline_path(name);
        if target_path.exists() {
            return Err(HgbError::Execution(format!(
                "Timeline '{}' already exists at {}",
                name,
                target_path.display()
            )));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let branch_name = format!("hgb-timeline-{}", name);

        // Attempt Git Worktree first if in git repo
        if self.is_git_repo() {
            let base = match base_branch {
                Some(b) => b.to_string(),
                None => {
                    // Try getting current branch or HEAD
                    let rev = Command::new("git")
                        .arg("-C")
                        .arg(&self.workspace_root)
                        .arg("rev-parse")
                        .arg("--abbrev-ref")
                        .arg("HEAD")
                        .output();
                    match rev {
                        Ok(o) if o.status.success() => {
                            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if s == "HEAD" { "HEAD".to_string() } else { s }
                        }
                        _ => "HEAD".to_string(),
                    }
                }
            };

            // Command: git worktree add -b <branch_name> <path> <base>
            let mut worktree_cmd = Command::new("git");
            worktree_cmd
                .arg("-C")
                .arg(&self.workspace_root)
                .arg("worktree")
                .arg("add")
                .arg("-b")
                .arg(&branch_name)
                .arg(&target_path)
                .arg(&base);

            if let Ok(output) = worktree_cmd.output() {
                if output.status.success() {
                    let head_commit = Command::new("git")
                        .arg("-C")
                        .arg(&target_path)
                        .arg("rev-parse")
                        .arg("HEAD")
                        .output()
                        .ok()
                        .and_then(|o| {
                            if o.status.success() {
                                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                            } else {
                                None
                            }
                        });

                    let info = TimelineInfo {
                        name: name.to_string(),
                        path: target_path.clone(),
                        created_at: now,
                        base_branch: Some(base),
                        is_git_worktree: true,
                        head_commit,
                    };

                    self.save_timeline_metadata(&info)?;
                    return Ok(info);
                }
            }
        }

        // Fallback: Isolated filesystem snapshot
        self.create_snapshot_timeline(name, &target_path, base_branch, now)
    }

    /// Snapshot fallback implementation
    fn create_snapshot_timeline(
        &self,
        name: &str,
        target_path: &Path,
        base_branch: Option<&str>,
        created_at: String,
    ) -> Result<TimelineInfo> {
        std::fs::create_dir_all(target_path)?;

        // Copy files recursively, ignoring target, .git, .hgb, node_modules
        Self::copy_dir_recursive(&self.workspace_root, target_path)?;

        let info = TimelineInfo {
            name: name.to_string(),
            path: target_path.to_path_buf(),
            created_at,
            base_branch: base_branch.map(|s| s.to_string()),
            is_git_worktree: false,
            head_commit: None,
        };

        self.save_timeline_metadata(&info)?;
        Ok(info)
    }

    /// Copy directory recursively ignoring VCS / build artifacts
    fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
        if !dst.exists() {
            std::fs::create_dir_all(dst)?;
        }

        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // Exclude noise
            if name_str == ".git"
                || name_str == ".hgb"
                || name_str == "target"
                || name_str == "node_modules"
                || name_str == ".idea"
                || name_str == ".vscode"
            {
                continue;
            }

            let dest_child = dst.join(&file_name);
            if path.is_dir() {
                Self::copy_dir_recursive(&path, &dest_child)?;
            } else if path.is_file() {
                std::fs::copy(&path, &dest_child)?;
            }
        }
        Ok(())
    }

    /// Save metadata file inside timeline directory
    fn save_timeline_metadata(&self, info: &TimelineInfo) -> Result<()> {
        let meta_file = info.path.join(".hgb-timeline.json");
        let content = serde_json::to_string_pretty(info)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        std::fs::write(meta_file, content)?;
        Ok(())
    }

    /// Load metadata file from timeline directory
    fn load_timeline_metadata(timeline_dir: &Path) -> Result<TimelineInfo> {
        let meta_file = timeline_dir.join(".hgb-timeline.json");
        if !meta_file.exists() {
            let name = timeline_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            return Ok(TimelineInfo {
                name,
                path: timeline_dir.to_path_buf(),
                created_at: chrono::Utc::now().to_rfc3339(),
                base_branch: None,
                is_git_worktree: false,
                head_commit: None,
            });
        }

        let content = std::fs::read_to_string(&meta_file)?;
        let info: TimelineInfo = serde_json::from_str(&content)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        Ok(info)
    }

    /// List all existing active timelines
    pub fn list_timelines(&self) -> Result<Vec<TimelineInfo>> {
        if !self.timelines_root.exists() {
            return Ok(Vec::new());
        }

        let mut list = Vec::new();
        for entry in std::fs::read_dir(&self.timelines_root)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Ok(info) = Self::load_timeline_metadata(&path) {
                    list.push(info);
                }
            }
        }

        list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(list)
    }

    /// Diff a timeline against its base branch or the workspace root
    pub fn diff_timeline(&self, name: &str) -> Result<TimelineDiff> {
        let target_path = self.timeline_path(name);
        if !target_path.exists() {
            return Err(HgbError::NotFound(format!(
                "Timeline '{}' not found at {}",
                name,
                target_path.display()
            )));
        }

        let info = Self::load_timeline_metadata(&target_path)?;

        if info.is_git_worktree {
            let base = info.base_branch.as_deref().unwrap_or("HEAD");
            // Git diff --stat
            let diff_stat = Command::new("git")
                .arg("-C")
                .arg(&target_path)
                .arg("diff")
                .arg("--stat")
                .arg(base)
                .output();

            // Git full diff
            let full_diff = Command::new("git")
                .arg("-C")
                .arg(&target_path)
                .arg("diff")
                .arg(base)
                .output()?;

            let diff_content = String::from_utf8_lossy(&full_diff.stdout).to_string();

            // Parse changed files
            let name_only = Command::new("git")
                .arg("-C")
                .arg(&target_path)
                .arg("diff")
                .arg("--name-only")
                .arg(base)
                .output()
                .ok();

            let modified_files: Vec<String> = name_only
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty() && !s.ends_with(".hgb-timeline.json"))
                        .collect()
                })
                .unwrap_or_default();

            let (insertions, deletions) = if let Ok(stat_out) = diff_stat {
                Self::parse_git_stat(&String::from_utf8_lossy(&stat_out.stdout))
            } else {
                (0, 0)
            };

            let files_changed = modified_files.len();

            return Ok(TimelineDiff {
                timeline_name: name.to_string(),
                files_changed,
                insertions,
                deletions,
                diff_content,
                modified_files,
            });
        }

        // Snapshot directory diff fallback
        self.diff_snapshot(name, &target_path)
    }

    /// Parse insertions and deletions from git diff --stat summary line
    fn parse_git_stat(stat: &str) -> (usize, usize) {
        let mut ins = 0;
        let mut del = 0;
        for line in stat.lines() {
            if line.contains("insertion") || line.contains("deletion") {
                let parts: Vec<&str> = line.split(',').collect();
                for p in parts {
                    let p = p.trim();
                    if p.contains("insertion") {
                        if let Some(num) = p.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()) {
                            ins = num;
                        }
                    } else if p.contains("deletion") {
                        if let Some(num) = p.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()) {
                            del = num;
                        }
                    }
                }
            }
        }
        (ins, del)
    }

    /// Snapshot diff comparison against main workspace
    fn diff_snapshot(&self, name: &str, target_path: &Path) -> Result<TimelineDiff> {
        let mut modified_files = Vec::new();
        let mut diff_content = String::new();
        let mut insertions = 0;
        let mut deletions = 0;

        let snap_files = Self::collect_relative_files(target_path)?;
        let ws_files = Self::collect_relative_files(&self.workspace_root)?;

        let mut all_files: HashSet<String> = snap_files.into_iter().collect();
        for f in ws_files {
            all_files.insert(f);
        }

        let mut sorted_files: Vec<String> = all_files.into_iter().collect();
        sorted_files.sort();

        for rel in sorted_files {
            if rel == ".hgb-timeline.json" {
                continue;
            }

            let snap_file = target_path.join(&rel);
            let ws_file = self.workspace_root.join(&rel);

            match (ws_file.exists(), snap_file.exists()) {
                (true, true) => {
                    let ws_bytes = std::fs::read(&ws_file)?;
                    let snap_bytes = std::fs::read(&snap_file)?;
                    if ws_bytes != snap_bytes {
                        modified_files.push(rel.clone());
                        let ws_str = String::from_utf8_lossy(&ws_bytes);
                        let snap_str = String::from_utf8_lossy(&snap_bytes);
                        let (ins, del, file_diff) = Self::compute_text_diff(&rel, &ws_str, &snap_str);
                        insertions += ins;
                        deletions += del;
                        diff_content.push_str(&file_diff);
                    }
                }
                (false, true) => {
                    // Added in timeline
                    modified_files.push(rel.clone());
                    let snap_bytes = std::fs::read(&snap_file)?;
                    let snap_str = String::from_utf8_lossy(&snap_bytes);
                    let (ins, _, file_diff) = Self::compute_text_diff(&rel, "", &snap_str);
                    insertions += ins;
                    diff_content.push_str(&file_diff);
                }
                (true, false) => {
                    // Deleted in timeline
                    modified_files.push(rel.clone());
                    let ws_bytes = std::fs::read(&ws_file)?;
                    let ws_str = String::from_utf8_lossy(&ws_bytes);
                    let (_, del, file_diff) = Self::compute_text_diff(&rel, &ws_str, "");
                    deletions += del;
                    diff_content.push_str(&file_diff);
                }
                (false, false) => {}
            }
        }

        Ok(TimelineDiff {
            timeline_name: name.to_string(),
            files_changed: modified_files.len(),
            insertions,
            deletions,
            diff_content,
            modified_files,
        })
    }

    /// Collect relative paths of all regular files in a directory
    fn collect_relative_files(base: &Path) -> Result<Vec<String>> {
        let mut result = Vec::new();
        Self::collect_files_recursive(base, base, &mut result)?;
        Ok(result)
    }

    fn collect_files_recursive(root: &Path, current: &Path, list: &mut Vec<String>) -> Result<()> {
        if !current.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let name_str = entry.file_name().to_string_lossy().to_string();

            if name_str == ".git"
                || name_str == ".hgb"
                || name_str == "target"
                || name_str == "node_modules"
            {
                continue;
            }

            if path.is_dir() {
                Self::collect_files_recursive(root, &path, list)?;
            } else if path.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    list.push(rel.to_string_lossy().to_string());
                }
            }
        }
        Ok(())
    }

    /// Compute simple line-based unified diff snippet
    fn compute_text_diff(rel_path: &str, old_text: &str, new_text: &str) -> (usize, usize, String) {
        let mut diff = format!("--- a/{}\n+++ b/{}\n", rel_path, rel_path);
        let old_lines: Vec<&str> = old_text.lines().collect();
        let new_lines: Vec<&str> = new_text.lines().collect();

        let mut ins = 0;
        let mut del = 0;

        for line in &old_lines {
            if !new_lines.contains(line) {
                diff.push_str(&format!("-{}\n", line));
                del += 1;
            }
        }
        for line in &new_lines {
            if !old_lines.contains(line) {
                diff.push_str(&format!("+{}\n", line));
                ins += 1;
            }
        }

        (ins, del, diff)
    }

    /// Merge changes from a timeline into the main workspace
    pub fn merge_timeline(&self, name: &str) -> Result<TimelineMergeReport> {
        let target_path = self.timeline_path(name);
        if !target_path.exists() {
            return Err(HgbError::NotFound(format!(
                "Timeline '{}' not found at {}",
                name,
                target_path.display()
            )));
        }

        let info = Self::load_timeline_metadata(&target_path)?;

        if info.is_git_worktree {
            let branch_name = format!("hgb-timeline-{}", name);

            // In main workspace, commit or check clean
            let merge_out = Command::new("git")
                .arg("-C")
                .arg(&self.workspace_root)
                .arg("merge")
                .arg("--no-ff")
                .arg(&branch_name)
                .arg("-m")
                .arg(format!("Merge timeline '{}'", name))
                .output()?;

            if !merge_out.status.success() {
                let err = String::from_utf8_lossy(&merge_out.stderr).to_string();
                return Ok(TimelineMergeReport {
                    timeline_name: name.to_string(),
                    merged_files: Vec::new(),
                    commit_hash: None,
                    success: false,
                    message: format!("Git merge failed: {}", err),
                });
            }

            // Get merge commit hash
            let head = Command::new("git")
                .arg("-C")
                .arg(&self.workspace_root)
                .arg("rev-parse")
                .arg("HEAD")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

            // Collect files in merge commit
            let files_out = Command::new("git")
                .arg("-C")
                .arg(&self.workspace_root)
                .arg("diff-tree")
                .arg("--no-commit-id")
                .arg("--name-only")
                .arg("-r")
                .arg("HEAD")
                .output()
                .ok();

            let merged_files: Vec<String> = files_out
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();

            // Discard the worktree and branch
            let _ = self.discard_timeline(name);

            return Ok(TimelineMergeReport {
                timeline_name: name.to_string(),
                merged_files,
                commit_hash: head,
                success: true,
                message: format!("Successfully merged git timeline '{}'", name),
            });
        }

        // Snapshot merge: Copy modified files from target_path into workspace_root
        let diff = self.diff_snapshot(name, &target_path)?;
        for file in &diff.modified_files {
            let snap_src = target_path.join(file);
            let ws_dst = self.workspace_root.join(file);

            if snap_src.exists() {
                if let Some(parent) = ws_dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&snap_src, &ws_dst)?;
            } else if ws_dst.exists() {
                let _ = std::fs::remove_file(&ws_dst);
            }
        }

        let merged_files = diff.modified_files.clone();
        // Discard snapshot timeline after successful merge
        let _ = self.discard_timeline(name);

        Ok(TimelineMergeReport {
            timeline_name: name.to_string(),
            merged_files,
            commit_hash: None,
            success: true,
            message: format!("Successfully merged snapshot timeline '{}'", name),
        })
    }

    /// Discard and delete a timeline
    pub fn discard_timeline(&self, name: &str) -> Result<()> {
        let target_path = self.timeline_path(name);
        if !target_path.exists() {
            return Ok(());
        }

        let info = Self::load_timeline_metadata(&target_path).ok();
        let is_worktree = info.map(|i| i.is_git_worktree).unwrap_or(false);

        if is_worktree && self.is_git_repo() {
            let branch_name = format!("hgb-timeline-{}", name);
            // 1. Remove git worktree
            let _ = Command::new("git")
                .arg("-C")
                .arg(&self.workspace_root)
                .arg("worktree")
                .arg("remove")
                .arg("--force")
                .arg(&target_path)
                .output();

            // 2. Delete git branch
            let _ = Command::new("git")
                .arg("-C")
                .arg(&self.workspace_root)
                .arg("branch")
                .arg("-D")
                .arg(&branch_name)
                .output();
        }

        // Remove leftover directory if present
        if target_path.exists() {
            std::fs::remove_dir_all(&target_path)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeline_manager_snapshot_lifecycle() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_timeline_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create dummy workspace files
        let src_dir = temp_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "fn main() { println!(\"original\"); }\n").unwrap();

        let mgr = TimelineManager::new(&temp_dir);
        assert!(!mgr.is_git_repo());

        // 1. Create timeline (fallback to snapshot)
        let info = mgr.create_timeline("experiment-1", None).expect("Must create timeline");
        assert_eq!(info.name, "experiment-1");
        assert!(!info.is_git_worktree);
        assert!(info.path.exists());

        // 2. List timelines
        let list = mgr.list_timelines().expect("Must list timelines");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "experiment-1");

        // 3. Mutate file in timeline
        let timeline_main = info.path.join("src").join("main.rs");
        std::fs::write(&timeline_main, "fn main() { println!(\"vibe_mutated\"); }\n").unwrap();

        // Add a new file in timeline
        std::fs::write(info.path.join("src").join("helper.rs"), "pub fn help() {}\n").unwrap();

        // 4. Diff timeline
        let diff = mgr.diff_timeline("experiment-1").expect("Must diff timeline");
        assert_eq!(diff.files_changed, 2);
        assert!(diff.modified_files.contains(&"src/main.rs".to_string()));
        assert!(diff.modified_files.contains(&"src/helper.rs".to_string()));
        assert!(diff.diff_content.contains("vibe_mutated"));

        // 5. Merge timeline back to workspace
        let merge = mgr.merge_timeline("experiment-1").expect("Must merge timeline");
        assert!(merge.success);
        assert_eq!(merge.merged_files.len(), 2);

        // Verify workspace files updated
        let updated_main = std::fs::read_to_string(src_dir.join("main.rs")).unwrap();
        assert!(updated_main.contains("vibe_mutated"));
        assert!(src_dir.join("helper.rs").exists());

        // Verify timeline was cleaned up on merge
        let remaining = mgr.list_timelines().unwrap();
        assert_eq!(remaining.len(), 0);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_timeline_discard() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_timeline_discard_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mgr = TimelineManager::new(&temp_dir);
        let info = mgr.create_timeline("to-discard", None).unwrap();
        assert!(info.path.exists());

        mgr.discard_timeline("to-discard").unwrap();
        assert!(!info.path.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
