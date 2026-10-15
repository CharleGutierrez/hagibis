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
}
