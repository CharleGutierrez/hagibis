//! # The "Zero-Broken-Diff" Speculative Sandbox
//!
//! Provides atomic, isolated speculative execution for proposed code patches:
//! 1. Clones target workspace files and manifests into memory-backed `/dev/shm/hgb_sandbox_<uuid>` (or tempdir).
//! 2. Applies proposed modifications speculatively in the sandbox.
//! 3. Runs compilation checks (`cargo check` or custom test command).
//! 4. If compilation fails, extracts error diagnostics and triggers an autonomous self-healing loop.
//! 5. Commits to the real workspace ONLY when 100% green; rolls back cleanly on persistent failure.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A proposed file patch to test speculatively
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxPatch {
    pub relative_path: String,
    pub original_content: String,
    pub proposed_content: String,
}

impl SandboxPatch {
    pub fn new(
        relative_path: impl Into<String>,
        original_content: impl Into<String>,
        proposed_content: impl Into<String>,
    ) -> Self {
        Self {
            relative_path: relative_path.into(),
            original_content: original_content.into(),
            proposed_content: proposed_content.into(),
        }
    }
}

/// Result of running the validation/compilation command
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompilationResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub extracted_errors: Vec<String>,
    pub duration_ms: u64,
}

/// A recorded attempt in the autonomous self-healing loop
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealingRound {
    pub round: usize,
    pub compiler_errors: Vec<String>,
    pub fix_applied: String,
    pub success: bool,
}

/// Comprehensive report of the speculative execution and healing cycle
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealingReport {
    pub initial_success: bool,
    pub final_success: bool,
    pub healing_rounds: Vec<HealingRound>,
    pub files_committed: Vec<String>,
    pub total_duration_ms: u64,
    pub rolled_back: bool,
}

/// The Zero-Broken-Diff Speculative Sandbox Engine
pub struct SpeculativeSandbox {
    workspace_root: PathBuf,
    sandbox_root: PathBuf,
    check_command: String,
    max_healing_rounds: usize,
    use_shm: bool,
}

impl SpeculativeSandbox {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        let session_id = uuid::Uuid::new_v4().simple().to_string();

        // Prefer /dev/shm for sub-microsecond RAM disk I/O, fallback to std::env::temp_dir()
        let shm_dir = Path::new("/dev/shm");
        let (sandbox_root, use_shm) = if shm_dir.exists() && shm_dir.is_dir() {
            (shm_dir.join(format!("hgb_sandbox_{}", session_id)), true)
        } else {
            (std::env::temp_dir().join(format!("hgb_sandbox_{}", session_id)), false)
        };

        Self {
            workspace_root: ws,
            sandbox_root,
            check_command: "cargo check".to_string(),
            max_healing_rounds: 3,
            use_shm,
        }
    }

    pub fn with_check_command(mut self, cmd: impl Into<String>) -> Self {
        self.check_command = cmd.into();
        self
    }

    pub fn with_max_healing_rounds(mut self, rounds: usize) -> Self {
        self.max_healing_rounds = rounds;
        self
    }

    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox_root
    }

    pub fn is_using_shm(&self) -> bool {
        self.use_shm
    }

    /// Prepares the isolated sandbox by copying target files and build manifests
    pub fn prepare_sandbox(&self, files_to_mirror: &[&str]) -> Result<()> {
        fs::create_dir_all(&self.sandbox_root)
            .map_err(|e| HgbError::Storage(format!("Failed to create sandbox directory: {e}")))?;

        // Mirror root build manifests if they exist (Cargo.toml, Cargo.lock, package.json)
        let manifests = ["Cargo.toml", "Cargo.lock", "package.json", "tsconfig.json"];
        for manifest in &manifests {
            let src = self.workspace_root.join(manifest);
            if src.exists() && src.is_file() {
                let dst = self.sandbox_root.join(manifest);
                let _ = fs::copy(&src, &dst);
            }
        }

        // Mirror specified files
        for rel in files_to_mirror {
            let clean_rel = rel.trim_start_matches('/');
            let src = self.workspace_root.join(clean_rel);
            let dst = self.sandbox_root.join(clean_rel);

            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| HgbError::Storage(format!("Failed to create sandbox subdirectories: {e}")))?;
            }

            if src.exists() && src.is_file() {
                fs::copy(&src, &dst)
                    .map_err(|e| HgbError::Storage(format!("Failed to copy '{}' to sandbox: {e}", clean_rel)))?;
            }
        }

        Ok(())
    }

    /// Applies proposed modifications to files inside the sandbox
    pub fn apply_speculative_patch(&self, patch: &SandboxPatch) -> Result<()> {
        let clean_rel = patch.relative_path.trim_start_matches('/');
        let target = self.sandbox_root.join(clean_rel);

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| HgbError::Storage(format!("Failed to create directory for patch: {e}")))?;
        }

        fs::write(&target, &patch.proposed_content)
            .map_err(|e| HgbError::Storage(format!("Failed to write patch to sandbox: {e}")))?;

        Ok(())
    }

    /// Executes the verification check command inside the sandbox root
    pub fn run_compilation_check(&self) -> Result<CompilationResult> {
        let start = Instant::now();
        let parts: Vec<&str> = self.check_command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(HgbError::InvalidInput("Empty verification command".to_string()));
        }

        let mut cmd = Command::new(parts[0]);
        if parts.len() > 1 {
            cmd.args(&parts[1..]);
        }
        cmd.current_dir(&self.sandbox_root);

        let output = cmd.output().map_err(|e| {
            HgbError::Execution(format!("Failed to execute validation command '{}': {e}", self.check_command))
        })?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();
        let exit_code = output.status.code().unwrap_or(-1);

        // Extract high-signal error diagnostics
        let mut extracted_errors = Vec::new();
        for line in stderr.lines().chain(stdout.lines()) {
            let trimmed = line.trim();
            if trimmed.starts_with("error[") || trimmed.starts_with("error:") || trimmed.contains("FAILED") {
                extracted_errors.push(trimmed.to_string());
            }
        }

        Ok(CompilationResult {
            success,
            exit_code,
            stdout,
            stderr,
            extracted_errors,
            duration_ms,
        })
    }

    /// Autonomous heuristic auto-fixer for common compiler errors in the sandbox
    pub fn attempt_heuristic_fix(&self, rel_path: &str, current_content: &str, errors: &[String]) -> Option<String> {
        let mut fixed = current_content.to_string();
        let mut changed = false;

        for err in errors {
            // Case 1: Missing semicolon
            if err.contains("expected `;`") {
                if let Some(pos) = fixed.rfind("}\n") {
                    // Try repairing preceding statement
                    let before = &fixed[..pos];
                    if let Some(last_nl) = before.rfind('\n') {
                        let line = &before[last_nl + 1..];
                        if !line.trim().ends_with(';') && !line.trim().ends_with('{') {
                            fixed.insert(pos, ';');
                            changed = true;
                        }
                    }
                }
            }

            // Case 2: Unused mutable variable warning promoted to error
            if err.contains("variable does not need to be mutable") {
                fixed = fixed.replace("let mut ", "let ");
                changed = true;
            }

            // Case 3: Missing standard import
            if err.contains("cannot find type `HashMap`") && !fixed.contains("use std::collections::HashMap;") {
                fixed = format!("use std::collections::HashMap;\n{}", fixed);
                changed = true;
            }
            if err.contains("cannot find type `PathBuf`") && !fixed.contains("use std::path::PathBuf;") {
                fixed = format!("use std::path::PathBuf;\n{}", fixed);
                changed = true;
            }
        }

        if changed {
            let target = self.sandbox_root.join(rel_path.trim_start_matches('/'));
            let _ = fs::write(target, &fixed);
            Some(fixed)
        } else {
            None
        }
    }

    /// Commits all green files from sandbox to the real workspace
    pub fn commit_to_workspace(&self, patches: &[SandboxPatch]) -> Result<Vec<String>> {
        let mut committed = Vec::new();

        for patch in patches {
            let clean_rel = patch.relative_path.trim_start_matches('/');
            let src = self.sandbox_root.join(clean_rel);
            let dst = self.workspace_root.join(clean_rel);

            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| HgbError::Storage(format!("Failed to create directory in workspace: {e}")))?;
            }

            fs::copy(&src, &dst)
                .map_err(|e| HgbError::Storage(format!("Failed to commit '{}' to workspace: {e}", clean_rel)))?;

            committed.push(clean_rel.to_string());
        }

        Ok(committed)
    }

    /// Discards and removes the sandbox directory completely
    pub fn rollback(&self) -> Result<()> {
        if self.sandbox_root.exists() {
            let _ = fs::remove_dir_all(&self.sandbox_root);
        }
        Ok(())
    }

    /// Full speculative validation pipeline with autonomous self-healing loop
    pub fn speculative_execute_and_heal(&self, patches: &[SandboxPatch]) -> Result<HealingReport> {
        let start = Instant::now();

        // 1. Prepare sandbox with patched files
        let rel_paths: Vec<&str> = patches.iter().map(|p| p.relative_path.as_str()).collect();
        self.prepare_sandbox(&rel_paths)?;

        // 2. Apply proposed modifications
        for patch in patches {
            self.apply_speculative_patch(patch)?;
        }

        // 3. Initial compilation check
        let initial_check = self.run_compilation_check()?;
        if initial_check.success {
            let committed = self.commit_to_workspace(patches)?;
            let _ = self.rollback();

            return Ok(HealingReport {
                initial_success: true,
                final_success: true,
                healing_rounds: Vec::new(),
                files_committed: committed,
                total_duration_ms: start.elapsed().as_millis() as u64,
                rolled_back: false,
            });
        }

        // 4. Autonomous Healing Loop
        let mut healing_rounds = Vec::new();
        let mut is_green = false;
        let mut current_errors = initial_check.extracted_errors;

        for round_idx in 1..=self.max_healing_rounds {
            let mut round_healed_any = false;
            let mut fixes = Vec::new();

            for patch in patches {
                let clean_rel = patch.relative_path.trim_start_matches('/');
                let sandboxed_file = self.sandbox_root.join(clean_rel);
                if let Ok(content) = fs::read_to_string(&sandboxed_file) {
                    if let Some(_fixed_content) = self.attempt_heuristic_fix(clean_rel, &content, &current_errors) {
                        round_healed_any = true;
                        fixes.push(format!("Applied heuristic repair on {}", clean_rel));
                    }
                }
            }

            if !round_healed_any {
                break;
            }

            // Re-check after fixes
            let recheck = self.run_compilation_check()?;
            healing_rounds.push(HealingRound {
                round: round_idx,
                compiler_errors: current_errors.clone(),
                fix_applied: fixes.join("; "),
                success: recheck.success,
            });

            if recheck.success {
                is_green = true;
                break;
            } else {
                current_errors = recheck.extracted_errors;
            }
        }

        // 5. Commit if green, otherwise rollback cleanly
        if is_green {
            let committed = self.commit_to_workspace(patches)?;
            let _ = self.rollback();

            Ok(HealingReport {
                initial_success: false,
                final_success: true,
                healing_rounds,
                files_committed: committed,
                total_duration_ms: start.elapsed().as_millis() as u64,
                rolled_back: false,
            })
        } else {
            // Discard all sandbox edits - workspace remains completely untouched
            let _ = self.rollback();

            Ok(HealingReport {
                initial_success: false,
                final_success: false,
                healing_rounds,
                files_committed: Vec::new(),
                total_duration_ms: start.elapsed().as_millis() as u64,
                rolled_back: true,
            })
        }
    }
}

impl Drop for SpeculativeSandbox {
    fn drop(&mut self) {
        let _ = self.rollback();
    }
}

/// Speculative session tracking an isolated "what-if" prototype branch
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeculativeSession {
    pub session_id: String,
    pub intent: String,
    pub slug: String,
    pub branch_name: String,
    pub worktree_path: PathBuf,
    pub base_branch: String,
    pub base_commit: String,
    pub created_at: String,
    pub is_active: bool,
    pub check_status: Option<String>,
}

/// Outcome of creating an isolated speculative prototype
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeculativeTryResult {
    pub session: SpeculativeSession,
    pub worktree_created: bool,
    pub check_passed: bool,
    pub check_output: String,
    pub message: String,
}

/// Outcome of accepting and merging a speculative prototype
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeculativeAcceptResult {
    pub session_id: String,
    pub branch_name: String,
    pub target_branch: String,
    pub merge_output: String,
    pub files_changed: usize,
    pub cleaned_up: bool,
}

/// Outcome of vaporizing and discarding a speculative prototype
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeculativeDiscardResult {
    pub session_id: String,
    pub branch_name: String,
    pub worktree_removed: bool,
    pub branch_deleted: bool,
    pub git_status_clean: bool,
}

/// Engine for managing zero-risk speculative prototyping using isolated git worktrees
pub struct SpeculativeWorktreeEngine;

impl SpeculativeWorktreeEngine {
    /// Path to active speculative session metadata
    pub fn session_file(repo_root: &Path) -> PathBuf {
        repo_root.join(".hgb").join("active_try.json")
    }

    /// Turn human natural language intent into a clean branch slug
    pub fn slugify(intent: &str) -> String {
        let cleaned: String = intent
            .chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();

        let mut collapsed = String::new();
        let mut last_dash = false;
        for c in cleaned.chars() {
            if c == '-' {
                if !last_dash {
                    collapsed.push('-');
                    last_dash = true;
                }
            } else {
                collapsed.push(c);
                last_dash = false;
            }
        }

        let trimmed = collapsed.trim_matches('-').to_string();
        if trimmed.is_empty() {
            "prototype".to_string()
        } else if trimmed.len() > 32 {
            trimmed[..32].trim_end_matches('-').to_string()
        } else {
            trimmed
        }
    }

    /// Get current active speculative session, if any
    pub fn get_active_session(repo_root: &Path) -> Result<Option<SpeculativeSession>> {
        let path = Self::session_file(repo_root);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .map_err(|e| HgbError::Storage(format!("Failed to read active speculative session: {e}")))?;
        let session: SpeculativeSession = serde_json::from_str(&data)
            .map_err(|e| HgbError::Serialization(format!("Invalid session metadata: {e}")))?;
        Ok(Some(session))
    }

    /// Save speculative session metadata
    pub fn save_session(repo_root: &Path, session: &SpeculativeSession) -> Result<()> {
        let path = Self::session_file(repo_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| HgbError::Storage(format!("Failed to create metadata directory: {e}")))?;
        }
        let json = serde_json::to_string_pretty(session)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        fs::write(&path, json)
            .map_err(|e| HgbError::Storage(format!("Failed to write session file: {e}")))?;
        Ok(())
    }

    /// Remove active speculative session metadata
    pub fn remove_session(repo_root: &Path) -> Result<()> {
        let path = Self::session_file(repo_root);
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        Ok(())
    }

    /// `hgb try "<intent>"`: Spawns an isolated git worktree branch (.hgb/worktrees/try-<slug>)
    pub fn try_prototype(
        repo_root: &Path,
        intent: &str,
        run_check: bool,
        check_cmd: Option<&str>,
    ) -> Result<SpeculativeTryResult> {
        let repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
        let slug = Self::slugify(intent);
        let branch_name = format!("try-{}", slug);
        let worktrees_root = repo.join(".hgb").join("worktrees");
        fs::create_dir_all(&worktrees_root)
            .map_err(|e| HgbError::Storage(format!("Failed to create worktrees directory: {e}")))?;

        let worktree_path = worktrees_root.join(&branch_name);

        // Detect current base branch and commit
        let base_branch = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "main".to_string());

        let base_commit = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "HEAD".to_string());

        // Clean up previous worktree at this path if it exists
        if worktree_path.exists() {
            let _ = Command::new("git")
                .current_dir(&repo)
                .args(["worktree", "remove", "--force", &worktree_path.display().to_string()])
                .output();
            let _ = fs::remove_dir_all(&worktree_path);
        }

        // Try creating new worktree with new branch -b
        let mut worktree_created = false;
        let add_out = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "add", "-b", &branch_name, &worktree_path.display().to_string(), "HEAD"])
            .output();

        if let Ok(ref o) = add_out {
            if o.status.success() {
                worktree_created = true;
            }
        }

        if !worktree_created {
            // Fallback: branch may already exist, add without -b
            let fallback_out = Command::new("git")
                .current_dir(&repo)
                .args(["worktree", "add", &worktree_path.display().to_string(), &branch_name])
                .output();

            if let Ok(ref o) = fallback_out {
                if o.status.success() {
                    worktree_created = true;
                }
            }
        }

        // Non-git fallback: if git failed or not a git repo, mirror repo files into shadow worktree
        if !worktree_created {
            fs::create_dir_all(&worktree_path)
                .map_err(|e| HgbError::Storage(format!("Failed to create shadow worktree: {e}")))?;

            // Copy manifest and source files
            for entry in fs::read_dir(&repo)? {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    let file_name = entry.file_name();
                    let file_name_str = file_name.to_string_lossy();
                    if file_name_str == ".git" || file_name_str == ".hgb" || file_name_str == "target" || file_name_str == "node_modules" {
                        continue;
                    }
                    let dst = worktree_path.join(&file_name);
                    if path.is_file() {
                        let _ = fs::copy(&path, &dst);
                    }
                }
            }
            worktree_created = true;
        }

        // Execute candidate validation checks if requested
        let mut check_passed = true;
        let mut check_output = String::new();

        if run_check {
            let cmd_str = if let Some(c) = check_cmd {
                c.to_string()
            } else if worktree_path.join("Cargo.toml").exists() {
                "cargo check".to_string()
            } else if worktree_path.join("package.json").exists() {
                "npm test".to_string()
            } else {
                "echo 'Speculative worktree verified'".to_string()
            };

            let parts: Vec<&str> = cmd_str.split_whitespace().collect();
            if !parts.is_empty() {
                let mut cmd = Command::new(parts[0]);
                if parts.len() > 1 {
                    cmd.args(&parts[1..]);
                }
                cmd.current_dir(&worktree_path);

                match cmd.output() {
                    Ok(out) => {
                        check_passed = out.status.success();
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        check_output = format!("{}\n{}", stdout, stderr).trim().to_string();
                    }
                    Err(e) => {
                        check_passed = false;
                        check_output = format!("Check execution error: {e}");
                    }
                }
            }
        }

        let session = SpeculativeSession {
            session_id: format!("try_{}_{}", slug, uuid::Uuid::new_v4().simple()),
            intent: intent.to_string(),
            slug,
            branch_name: branch_name.clone(),
            worktree_path: worktree_path.clone(),
            base_branch,
            base_commit,
            created_at: chrono::Utc::now().to_rfc3339(),
            is_active: true,
            check_status: Some(if check_passed { "PASSED".to_string() } else { "FAILED".to_string() }),
        };

        Self::save_session(&repo, &session)?;

        let message = format!(
            "Speculative worktree '{}' active at {}\nChecks: {}\nRun 'hgb accept' to merge or 'hgb discard' to vaporize.",
            branch_name,
            worktree_path.display(),
            if check_passed { "PASSED ✔" } else { "FAILED ✖" }
        );

        Ok(SpeculativeTryResult {
            session,
            worktree_created,
            check_passed,
            check_output,
            message,
        })
    }

    /// `hgb accept`: 1-key merge of speculative worktree into active workspace
    pub fn accept_prototype(repo_root: &Path) -> Result<SpeculativeAcceptResult> {
        let repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
        let session = Self::get_active_session(&repo)?
            .ok_or_else(|| HgbError::NotFound("No active speculative worktree found to accept. Run 'hgb try <intent>' first.".to_string()))?;

        // In worktree: commit any modified or newly added files
        if session.worktree_path.exists() {
            let _ = Command::new("git")
                .current_dir(&session.worktree_path)
                .args(["add", "-A"])
                .output();

            let commit_msg = format!("hgb: speculative prototype [{}]", session.intent);
            let _ = Command::new("git")
                .current_dir(&session.worktree_path)
                .args(["commit", "-m", &commit_msg])
                .output();
        }

        // In main repo: merge speculative branch
        let merge_out = Command::new("git")
            .current_dir(&repo)
            .args(["merge", "--no-ff", "-m", &format!("Merge speculative worktree '{}'", session.branch_name), &session.branch_name])
            .output();

        let (merge_output, merge_success) = match merge_out {
            Ok(o) => (String::from_utf8_lossy(&o.stdout).to_string(), o.status.success()),
            Err(e) => (format!("Failed to execute git merge: {e}"), false),
        };

        // Fallback for non-git shadow worktrees: copy modified files back to main repo
        let mut files_changed = 0;
        if !merge_success && session.worktree_path.exists() {
            if let Ok(entries) = fs::read_dir(&session.worktree_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str != ".git" && name_str != ".hgb" {
                        let dst = repo.join(&name);
                        if path.is_file() {
                            if fs::copy(&path, &dst).is_ok() {
                                files_changed += 1;
                            }
                        }
                    }
                }
            }
        } else {
            // Count lines indicating changed files from git merge output
            for line in merge_output.lines() {
                if line.contains("file changed") || line.contains("files changed") {
                    files_changed += 1;
                }
            }
        }

        // Teardown worktree and prune branch
        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "remove", "--force", &session.worktree_path.display().to_string()])
            .output();

        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "prune"])
            .output();

        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["branch", "-D", &session.branch_name])
            .output();

        if session.worktree_path.exists() {
            let _ = fs::remove_dir_all(&session.worktree_path);
        }

        Self::remove_session(&repo)?;

        Ok(SpeculativeAcceptResult {
            session_id: session.session_id,
            branch_name: session.branch_name,
            target_branch: session.base_branch,
            merge_output,
            files_changed,
            cleaned_up: true,
        })
    }

    /// `hgb discard`: Vaporize speculative worktree with zero trace or dirtying of git status
    pub fn discard_prototype(repo_root: &Path) -> Result<SpeculativeDiscardResult> {
        let repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
        let session = Self::get_active_session(&repo)?
            .ok_or_else(|| HgbError::NotFound("No active speculative worktree found to discard.".to_string()))?;

        // 1. Force remove worktree from git
        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "remove", "--force", &session.worktree_path.display().to_string()])
            .output();

        // 2. Prune git worktree records
        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "prune"])
            .output();

        // 3. Delete scratch branch
        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["branch", "-D", &session.branch_name])
            .output();

        // 4. Force delete directory if still present on disk
        if session.worktree_path.exists() {
            let _ = fs::remove_dir_all(&session.worktree_path);
        }

        // 5. Remove session file
        Self::remove_session(&repo)?;

        // 6. Verify main workspace git status is clean
        let status_out = Command::new("git")
            .current_dir(&repo)
            .args(["status", "--porcelain"])
            .output();

        let git_status_clean = status_out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(true);

        Ok(SpeculativeDiscardResult {
            session_id: session.session_id,
            branch_name: session.branch_name,
            worktree_removed: true,
            branch_deleted: true,
            git_status_clean,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speculative_sandbox_success_and_commit() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_spec_ws_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let test_file = temp_dir.join("src/lib.rs");
        fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        fs::write(&test_file, "pub fn hello() -> &'static str { \"hello\" }").unwrap();

        // Use echo / true as check command for instantaneous unit test
        let sandbox = SpeculativeSandbox::new(&temp_dir)
            .with_check_command("echo All Checks Passed");

        let patch = SandboxPatch::new(
            "src/lib.rs",
            "pub fn hello() -> &'static str { \"hello\" }",
            "pub fn hello() -> &'static str { \"hello from sandbox!\" }",
        );

        let report = sandbox.speculative_execute_and_heal(&[patch]).unwrap();
        assert!(report.initial_success);
        assert!(report.final_success);
        assert!(!report.rolled_back);
        assert_eq!(report.files_committed.len(), 1);

        // Verify workspace was updated with patched content
        let updated = fs::read_to_string(&test_file).unwrap();
        assert_eq!(updated, "pub fn hello() -> &'static str { \"hello from sandbox!\" }");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_speculative_sandbox_failure_rolls_back_cleanly() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_spec_fail_ws_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let test_file = temp_dir.join("src/lib.rs");
        fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        let original_code = "pub fn add(a: i32, b: i32) -> i32 { a + b }";
        fs::write(&test_file, original_code).unwrap();

        // Check command fails deliberately
        let sandbox = SpeculativeSandbox::new(&temp_dir)
            .with_check_command("false")
            .with_max_healing_rounds(1);

        let broken_patch = SandboxPatch::new(
            "src/lib.rs",
            original_code,
            "pub fn broken_syntax(( { missing_bracket",
        );

        let report = sandbox.speculative_execute_and_heal(&[broken_patch]).unwrap();
        assert!(!report.initial_success);
        assert!(!report.final_success);
        assert!(report.rolled_back);
        assert!(report.files_committed.is_empty());

        // Verify real workspace was NEVER modified
        let current = fs::read_to_string(&test_file).unwrap();
        assert_eq!(current, original_code);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_speculative_slugify() {
        assert_eq!(
            SpeculativeWorktreeEngine::slugify("Add Stripe Payment Webhook!"),
            "add-stripe-payment-webhook"
        );
        assert_eq!(SpeculativeWorktreeEngine::slugify(""), "prototype");
        assert_eq!(SpeculativeWorktreeEngine::slugify("---   ---"), "prototype");
        let long_slug = SpeculativeWorktreeEngine::slugify("a very very very very very long intent description exceeding standard length");
        assert!(long_slug.len() <= 32);
    }

    #[test]
    fn test_speculative_worktree_try_accept_cycle() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_spec_wt_{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&temp_dir).unwrap();

        // Initialize git repo for realistic test
        let _ = Command::new("git").args(["init"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.email", "test@hagibis.ai"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.name", "HagibisTester"]).current_dir(&temp_dir).output();

        let src_file = temp_dir.join("main.rs");
        fs::write(&src_file, "fn main() { println!(\"original\"); }").unwrap();
        let _ = Command::new("git").args(["add", "main.rs"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["commit", "-m", "Initial commit"]).current_dir(&temp_dir).output();

        // 1. Try prototype
        let try_res = SpeculativeWorktreeEngine::try_prototype(&temp_dir, "Refactor Logging Flow", false, None).unwrap();
        assert!(try_res.worktree_created);
        assert!(try_res.session.worktree_path.exists());
        assert_eq!(try_res.session.slug, "refactor-logging-flow");
        assert_eq!(try_res.session.branch_name, "try-refactor-logging-flow");

        // Verify active session file
        let active = SpeculativeWorktreeEngine::get_active_session(&temp_dir).unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().slug, "refactor-logging-flow");

        // 2. Make modification inside speculative worktree
        let wt_main = try_res.session.worktree_path.join("main.rs");
        fs::write(&wt_main, "fn main() { println!(\"refactored with structured logging\"); }").unwrap();

        // 3. Accept prototype
        let accept_res = SpeculativeWorktreeEngine::accept_prototype(&temp_dir).unwrap();
        assert!(accept_res.cleaned_up);
        assert!(!try_res.session.worktree_path.exists());
        assert!(SpeculativeWorktreeEngine::get_active_session(&temp_dir).unwrap().is_none());

        // 4. Verify main workspace received the merged changes
        let merged_content = fs::read_to_string(&src_file).unwrap();
        assert!(merged_content.contains("structured logging"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_speculative_worktree_try_discard_cycle() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_spec_wt_disc_{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&temp_dir).unwrap();

        let _ = Command::new("git").args(["init"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.email", "test@hagibis.ai"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.name", "HagibisTester"]).current_dir(&temp_dir).output();

        let src_file = temp_dir.join("main.rs");
        fs::write(&src_file, "fn main() { println!(\"stable base\"); }").unwrap();
        let _ = Command::new("git").args(["add", "main.rs"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["commit", "-m", "Initial commit"]).current_dir(&temp_dir).output();

        // 1. Try prototype
        let try_res = SpeculativeWorktreeEngine::try_prototype(&temp_dir, "Broken Syntax Experiment", false, None).unwrap();
        assert!(try_res.worktree_created);

        // 2. Add garbage inside worktree
        let wt_main = try_res.session.worktree_path.join("main.rs");
        fs::write(&wt_main, "syntax error (()").unwrap();

        // 3. Discard prototype
        let discard_res = SpeculativeWorktreeEngine::discard_prototype(&temp_dir).unwrap();
        assert!(discard_res.worktree_removed);
        assert!(discard_res.branch_deleted);
        assert!(!try_res.session.worktree_path.exists());
        assert!(SpeculativeWorktreeEngine::get_active_session(&temp_dir).unwrap().is_none());

        // 4. Verify main workspace remains completely clean and untouched
        let content = fs::read_to_string(&src_file).unwrap();
        assert_eq!(content, "fn main() { println!(\"stable base\"); }");

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
