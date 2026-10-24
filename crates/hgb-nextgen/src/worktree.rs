use chrono::Utc;
use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Handle to an isolated, atmospheric Git worktree
pub struct AtmosphericWorktreeHandle {
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub is_ephemeral: bool,
    pub cleaned: bool,
}

impl Drop for AtmosphericWorktreeHandle {
    fn drop(&mut self) {
        if self.is_ephemeral && !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

impl AtmosphericWorktreeHandle {
    /// Create a scratch git worktree isolated in `.hagibis/worktrees/<branch>`
    pub fn create(
        repo_root: &Path,
        branch_name: &str,
        base_commit: Option<&str>,
        ephemeral: bool,
    ) -> Result<Self> {
        let clean_branch = branch_name
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
            .trim_matches('-')
            .to_string();

        let hagibis_worktrees = if repo_root.join(".hagibis").join("worktrees").exists() {
            repo_root.join(".hagibis").join("worktrees")
        } else {
            repo_root.join(".hgb").join("worktrees")
        };
        fs::create_dir_all(&hagibis_worktrees)?;

        let worktree_path = hagibis_worktrees.join(&clean_branch);

        // If path already exists, clean it up first
        if worktree_path.exists() {
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force", &worktree_path.display().to_string()])
                .current_dir(repo_root)
                .output();
            let _ = fs::remove_dir_all(&worktree_path);
        }

        let mut cmd = Command::new("git");
        cmd.current_dir(repo_root);
        cmd.args(["worktree", "add"]);

        let base = base_commit.unwrap_or("HEAD");
        // Try creating new branch first
        let add_out = cmd
            .args(["-b", &clean_branch, &worktree_path.display().to_string(), base])
            .output();

        let success = match add_out {
            Ok(ref o) if o.status.success() => true,
            _ => {
                // If branch already existed, add without -b
                let mut fallback_cmd = Command::new("git");
                fallback_cmd
                    .current_dir(repo_root)
                    .args(["worktree", "add", &worktree_path.display().to_string(), &clean_branch]);
                fallback_cmd.output().map(|o| o.status.success()).unwrap_or(false)
            }
        };

        if !success {
            return Err(HgbError::Execution(format!(
                "Failed to create git worktree for branch '{}' at '{}'",
                clean_branch,
                worktree_path.display()
            )));
        }

        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            worktree_path,
            branch_name: clean_branch,
            is_ephemeral: ephemeral,
            cleaned: false,
        })
    }

    /// Tear down worktree and prune scratch branches
    pub fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }

        let _ = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["worktree", "remove", "--force", &self.worktree_path.display().to_string()])
            .output();

        let _ = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["worktree", "prune"])
            .output();

        if self.worktree_path.exists() {
            let _ = fs::remove_dir_all(&self.worktree_path);
        }

        if self.is_ephemeral {
            let _ = Command::new("git")
                .current_dir(&self.repo_root)
                .args(["branch", "-D", &self.branch_name])
                .output();
        }

        self.cleaned = true;
        Ok(())
    }

    /// Merge worktree modifications back to target branch in the main repo
    pub fn merge_back(&mut self, target_branch: &str) -> Result<String> {
        // 1. In worktree: commit any pending changes
        let _ = Command::new("git")
            .current_dir(&self.worktree_path)
            .args(["add", "-A"])
            .output();

        let commit_msg = format!("hgb: atmospheric auto-checkpoint [{}]", self.branch_name);
        let _ = Command::new("git")
            .current_dir(&self.worktree_path)
            .args(["commit", "-m", &commit_msg])
            .output();

        // 2. In main repo: merge branch
        let out = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["merge", "--no-ff", "-m", &format!("Merge worktree branch '{}'", self.branch_name), &self.branch_name])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to execute git merge: {}", e)))?;

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();

        if !out.status.success() {
            return Err(HgbError::Execution(format!(
                "Failed to merge worktree branch '{}' into '{}': {}",
                self.branch_name, target_branch, stderr
            )));
        }

        self.cleanup()?;
        Ok(stdout)
    }
}

/// Tagged semantic stash document stored in `.hagibis/stashes/<tag>.json`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticStash {
    pub id: String,
    pub tag: String,
    pub description: String,
    pub created_at_rfc3339: String,
    pub diff: String,
    pub affected_files: Vec<String>,
    pub base_commit: String,
}

pub struct SemanticStashManager;

impl SemanticStashManager {
    pub fn stash_dir(repo_root: &Path) -> PathBuf {
        repo_root.join(".hagibis").join("stashes")
    }

    /// Capture all uncommitted changes into a tagged semantic stash
    pub fn create_stash(repo_root: &Path, tag: &str, description: Option<&str>) -> Result<SemanticStash> {
        let dir = Self::stash_dir(repo_root);
        fs::create_dir_all(&dir)?;

        // 1. Get HEAD commit hash
        let head_out = Command::new("git")
            .current_dir(repo_root)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to get HEAD: {}", e)))?;
        let base_commit = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

        // 2. Capture git diff
        let diff_out = Command::new("git")
            .current_dir(repo_root)
            .args(["diff", "HEAD"])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to capture git diff: {}", e)))?;
        let diff = String::from_utf8_lossy(&diff_out.stdout).to_string();

        // 3. Extract affected files
        let mut affected = Vec::new();
        for line in diff.lines() {
            if line.starts_with("+++ b/") {
                affected.push(line[6..].to_string());
            }
        }
        affected.sort();
        affected.dedup();

        let clean_tag = tag
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
            .trim_matches('-')
            .to_string();

        let stash = SemanticStash {
            id: format!("stash_{}_{}", clean_tag, Utc::now().timestamp_millis()),
            tag: clean_tag.clone(),
            description: description.unwrap_or("Semantic stash").to_string(),
            created_at_rfc3339: Utc::now().to_rfc3339(),
            diff,
            affected_files: affected,
            base_commit,
        };

        let json_path = dir.join(format!("{}.json", clean_tag));
        let json_bytes = serde_json::to_vec_pretty(&stash)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        fs::write(&json_path, json_bytes)?;

        Ok(stash)
    }

    /// Apply a tagged semantic stash back into the working tree
    pub fn apply_stash(repo_root: &Path, tag: &str) -> Result<String> {
        let dir = Self::stash_dir(repo_root);
        let clean_tag = tag
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
            .trim_matches('-')
            .to_string();
        let json_path = dir.join(format!("{}.json", clean_tag));

        if !json_path.exists() {
            return Err(HgbError::NotFound(format!("Semantic stash tag '{}' not found", tag)));
        }

        let bytes = fs::read(&json_path)?;
        let stash: SemanticStash = serde_json::from_slice(&bytes)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        if stash.diff.trim().is_empty() {
            return Ok("Stash is empty; zero changes applied.".to_string());
        }

        // Apply diff via temporary patch file
        let tmp_patch = dir.join(format!("{}.patch", clean_tag));
        fs::write(&tmp_patch, &stash.diff)?;

        let apply_out = Command::new("git")
            .current_dir(repo_root)
            .args(["apply", "--whitespace=nowarn", &tmp_patch.display().to_string()])
            .output();

        let _ = fs::remove_file(tmp_patch);

        match apply_out {
            Ok(o) if o.status.success() => {
                Ok(format!("Applied semantic stash '{}' affecting {} files.", tag, stash.affected_files.len()))
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                Err(HgbError::Execution(format!("git apply failed for stash '{}': {}", tag, err)))
            }
            Err(e) => Err(HgbError::Execution(format!("Failed to execute git apply: {}", e))),
        }
    }

    /// List all available tagged semantic stashes
    pub fn list_stashes(repo_root: &Path) -> Result<Vec<SemanticStash>> {
        let dir = Self::stash_dir(repo_root);
        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut stashes = Vec::new();
        let entries = fs::read_dir(dir)?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(bytes) = fs::read(&p) {
                    if let Ok(stash) = serde_json::from_slice::<SemanticStash>(&bytes) {
                        stashes.push(stash);
                    }
                }
            }
        }
        stashes.sort_by(|a, b| b.created_at_rfc3339.cmp(&a.created_at_rfc3339));
        Ok(stashes)
    }

    /// Delete a tagged semantic stash
    pub fn drop_stash(repo_root: &Path, tag: &str) -> Result<()> {
        let dir = Self::stash_dir(repo_root);
        let clean_tag = tag
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
            .trim_matches('-')
            .to_string();
        let json_path = dir.join(format!("{}.json", clean_tag));

        if json_path.exists() {
            fs::remove_file(json_path)?;
            Ok(())
        } else {
            Err(HgbError::NotFound(format!("Semantic stash tag '{}' not found", tag)))
        }
    }
}

pub use hgb_core::speculative_sandbox::{
    SpeculativeAcceptResult, SpeculativeDiscardResult, SpeculativeSession, SpeculativeTryResult,
    SpeculativeWorktreeEngine,
};

/// High-level ergonomic handle to a speculative "what-if" prototype worktree
pub struct SpeculativeWorktree;

impl SpeculativeWorktree {
    /// Create an isolated speculative worktree for an intent (e.g. `.hgb/worktrees/try-<slug>`)
    pub fn try_intent(
        repo_root: &Path,
        intent: &str,
        run_check: bool,
        check_cmd: Option<&str>,
    ) -> Result<SpeculativeTryResult> {
        SpeculativeWorktreeEngine::try_prototype(repo_root, intent, run_check, check_cmd)
    }

    /// 1-key merge into active workspace
    pub fn accept(repo_root: &Path) -> Result<SpeculativeAcceptResult> {
        SpeculativeWorktreeEngine::accept_prototype(repo_root)
    }

    /// Vaporize speculative worktree cleanly without dirtying git status
    pub fn discard(repo_root: &Path) -> Result<SpeculativeDiscardResult> {
        SpeculativeWorktreeEngine::discard_prototype(repo_root)
    }

    /// Check active speculative session
    pub fn active_session(repo_root: &Path) -> Result<Option<SpeculativeSession>> {
        SpeculativeWorktreeEngine::get_active_session(repo_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stash_dir_resolution() {
        let root = Path::new("/tmp/test-repo");
        let dir = SemanticStashManager::stash_dir(root);
        assert_eq!(dir, PathBuf::from("/tmp/test-repo/.hagibis/stashes"));
    }

    #[test]
    fn test_speculative_worktree_try_accept_wrapper() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_wt_wrap_{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&temp_dir).unwrap();

        let _ = Command::new("git").args(["init"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.email", "test@hagibis.ai"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.name", "HagibisTester"]).current_dir(&temp_dir).output();

        let test_file = temp_dir.join("lib.rs");
        fs::write(&test_file, "pub fn alpha() -> i32 { 1 }").unwrap();
        let _ = Command::new("git").args(["add", "lib.rs"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["commit", "-m", "init"]).current_dir(&temp_dir).output();

        let try_res = SpeculativeWorktree::try_intent(&temp_dir, "Add Beta Function", false, None).unwrap();
        assert!(try_res.worktree_created);
        assert!(try_res.session.worktree_path.exists());

        // Modify file in worktree
        let wt_file = try_res.session.worktree_path.join("lib.rs");
        fs::write(&wt_file, "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }").unwrap();

        // Accept
        let accept_res = SpeculativeWorktree::accept(&temp_dir).unwrap();
        assert!(accept_res.cleaned_up);

        let merged = fs::read_to_string(&test_file).unwrap();
        assert!(merged.contains("pub fn beta"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_speculative_worktree_discard_wrapper() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_wt_wrap_disc_{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&temp_dir).unwrap();

        let _ = Command::new("git").args(["init"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.email", "test@hagibis.ai"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["config", "user.name", "HagibisTester"]).current_dir(&temp_dir).output();

        let test_file = temp_dir.join("lib.rs");
        fs::write(&test_file, "pub fn stable() -> bool { true }").unwrap();
        let _ = Command::new("git").args(["add", "lib.rs"]).current_dir(&temp_dir).output();
        let _ = Command::new("git").args(["commit", "-m", "init"]).current_dir(&temp_dir).output();

        let try_res = SpeculativeWorktree::try_intent(&temp_dir, "Experimental Spike", false, None).unwrap();
        assert!(try_res.worktree_created);

        let discard_res = SpeculativeWorktree::discard(&temp_dir).unwrap();
        assert!(discard_res.worktree_removed);
        assert!(discard_res.branch_deleted);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
