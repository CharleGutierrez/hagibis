//! # Agentic Git Bisect Engine
//!
//! Autonomous binary search engine over Git commit history:
//! 1. Conducts automated binary search between known `good` and `bad` commit revisions.
//! 2. Executes custom verification commands (e.g. `cargo test`, lint, or build) at each step.
//! 3. Pinpoints the exact culprit commit introducing a regression.
//! 4. Extracts commit diffs and performs automated root-cause analysis.
//! 5. Synthesizes a structured remedy recommendation.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// Configuration for agentic bisect execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectConfig {
    pub good_commit: String,
    pub bad_commit: String,
    pub test_command: String,
    pub max_steps: usize,
    pub timeout_sec: u64,
}

impl BisectConfig {
    pub fn new(
        good_commit: impl Into<String>,
        bad_commit: impl Into<String>,
        test_command: impl Into<String>,
    ) -> Self {
        Self {
            good_commit: good_commit.into(),
            bad_commit: bad_commit.into(),
            test_command: test_command.into(),
            max_steps: 25,
            timeout_sec: 120,
        }
    }
}

/// A recorded step during bisect search
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BisectStep {
    pub step_number: usize,
    pub commit_hash: String,
    pub test_passed: bool,
    pub output_summary: String,
    pub duration_ms: u64,
}

/// In-depth root-cause analysis of the identified culprit commit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CulpritAnalysis {
    pub commit_hash: String,
    pub author: String,
    pub commit_message: String,
    pub files_changed: Vec<String>,
    pub diff_snippet: String,
    pub suspected_cause: String,
    pub recommended_remedy: String,
}

/// Final outcome of the bisect procedure
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BisectOutcome {
    pub culprit_commit: Option<String>,
    pub steps_taken: Vec<BisectStep>,
    pub total_steps: usize,
    pub analysis: Option<CulpritAnalysis>,
    pub success: bool,
}

/// The Agentic Git Bisect Engine
pub struct AgenticBisectEngine {
    repo_root: PathBuf,
}

impl AgenticBisectEngine {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }

    /// Retrieve commit list in revision range `good..bad` (oldest to newest)
    pub fn get_commit_range(&self, good: &str, bad: &str) -> Result<Vec<String>> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["rev-list", "--reverse", &format!("{}..{}", good, bad)])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to run git rev-list: {}", e)))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(HgbError::Execution(format!("git rev-list error: {}", err)));
        }

        let commits: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        Ok(commits)
    }

    /// Check out a commit
    pub fn git_checkout(&self, commit: &str) -> Result<()> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["checkout", "-f", commit])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to checkout commit {}: {}", commit, e)))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(HgbError::Execution(format!(
                "git checkout {} failed: {}",
                commit, err
            )));
        }
        Ok(())
    }

    /// Run the verification test command at current revision
    pub fn run_verification_test(&self, test_cmd: &str) -> (bool, String, u64) {
        let start = Instant::now();
        let output_res = Command::new("sh")
            .current_dir(&self.repo_root)
            .arg("-c")
            .arg(test_cmd)
            .output();

        let duration_ms = start.elapsed().as_millis() as u64;

        match output_res {
            Ok(out) => {
                let success = out.status.success();
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let summary: String = combined.lines().rev().take(10).collect::<Vec<_>>().join("\n");
                (success, summary, duration_ms)
            }
            Err(e) => (false, format!("Execution failed: {}", e), duration_ms),
        }
    }

    /// Core binary search algorithm over an indexed commit slice
    pub fn binary_search_commits<F>(commits: &[String], mut test_runner: F) -> (Option<String>, Vec<BisectStep>)
    where
        F: FnMut(&str, usize) -> (bool, String, u64),
    {
        if commits.is_empty() {
            return (None, Vec::new());
        }

        let mut low = 0usize;
        let mut high = commits.len() - 1;
        let mut culprit_idx = None;
        let mut steps = Vec::new();
        let mut step_num = 1;

        while low <= high {
            let mid = low + (high - low) / 2;
            let commit = &commits[mid];

            let (passed, summary, dur) = test_runner(commit, step_num);
            steps.push(BisectStep {
                step_number: step_num,
                commit_hash: commit.clone(),
                test_passed: passed,
                output_summary: summary,
                duration_ms: dur,
            });

            step_num += 1;

            if passed {
                // Mid is good, culprit must be after mid
                low = mid + 1;
            } else {
                // Mid is bad, culprit is at mid or earlier
                culprit_idx = Some(mid);
                if mid == 0 {
                    break;
                }
                high = mid - 1;
            }
        }

        let culprit = culprit_idx.map(|idx| commits[idx].clone());
        (culprit, steps)
    }

    /// Perform full autonomous git bisect
    pub fn run_bisect(&self, config: &BisectConfig) -> Result<BisectOutcome> {
        let commits = self.get_commit_range(&config.good_commit, &config.bad_commit)?;

        if commits.is_empty() {
            return Ok(BisectOutcome {
                culprit_commit: None,
                steps_taken: Vec::new(),
                total_steps: 0,
                analysis: None,
                success: false,
            });
        }

        let (culprit, steps) = Self::binary_search_commits(&commits, |commit, _step| {
            if let Err(e) = self.git_checkout(commit) {
                return (false, format!("Checkout error: {}", e), 0);
            }
            self.run_verification_test(&config.test_command)
        });

        // Restore back to bad_commit or initial
        let _ = self.git_checkout(&config.bad_commit);

        let analysis = if let Some(ref c) = culprit {
            self.analyze_culprit(c).ok()
        } else {
            None
        };

        let total_steps = steps.len();
        let success = culprit.is_some();

        Ok(BisectOutcome {
            culprit_commit: culprit,
            steps_taken: steps,
            total_steps,
            analysis,
            success,
        })
    }

    /// Extract commit details and perform automated root-cause analysis
    pub fn analyze_culprit(&self, commit: &str) -> Result<CulpritAnalysis> {
        // Extract commit metadata (author, message)
        let show_meta = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["show", "-s", "--format=%an%n%s", commit])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to run git show meta: {}", e)))?;

        let meta_str = String::from_utf8_lossy(&show_meta.stdout);
        let mut meta_lines = meta_str.lines();
        let author = meta_lines.next().unwrap_or("Unknown").to_string();
        let message = meta_lines.next().unwrap_or("No message").to_string();

        // Extract changed files
        let diff_names = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["diff-tree", "--no-commit-id", "--name-only", "-r", commit])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to run git diff-tree: {}", e)))?;

        let files: Vec<String> = String::from_utf8_lossy(&diff_names.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        // Extract diff snippet
        let diff_out = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["show", "--stat", "-p", commit])
            .output()
            .map_err(|e| HgbError::Execution(format!("Failed to run git show diff: {}", e)))?;

        let full_diff = String::from_utf8_lossy(&diff_out.stdout);
        let diff_snippet: String = full_diff.lines().take(60).collect::<Vec<_>>().join("\n");

        // Heuristic root cause & remedy synthesis
        let (suspected_cause, recommended_remedy) = Self::synthesize_cause_and_remedy(&full_diff, &message);

        Ok(CulpritAnalysis {
            commit_hash: commit.to_string(),
            author,
            commit_message: message,
            files_changed: files,
            diff_snippet,
            suspected_cause,
            recommended_remedy,
        })
    }

    /// Analyze diff and commit message to synthesize root cause and remedy
    pub fn synthesize_cause_and_remedy(diff: &str, message: &str) -> (String, String) {
        if diff.contains(".unwrap()") {
            (
                "Commit introduced an unchecked `.unwrap()` call that causes panic on None or Err".to_string(),
                "Replace `.unwrap()` with error propagation (`?`) or safe default handling".to_string(),
            )
        } else if diff.contains("panic!(") || diff.contains("todo!(") || diff.contains("unimplemented!(") {
            (
                "Commit introduced explicit panic/todo/unimplemented assertion".to_string(),
                "Implement the incomplete pathway and return standard Result<T, E>".to_string(),
            )
        } else if diff.contains("pub fn") || diff.contains("fn ") {
            (
                format!("Signature or behavioral modification in commit: '{}'", message),
                "Review downstream call sites and adjust argument types or return invariants".to_string(),
            )
        } else {
            (
                format!("Regression introduced by commit: '{}'", message),
                "Inspect the git diff of affected files and revert offending change or add regression test".to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_search_commits_algorithm() {
        // Commits: 0 (good), 1 (good), 2 (good), 3 (bad - culprit), 4 (bad)
        let commits: Vec<String> = (0..5).map(|i| format!("commit_{}", i)).collect();

        let (culprit, steps) = AgenticBisectEngine::binary_search_commits(&commits, |c, _| {
            let idx: usize = c.strip_prefix("commit_").unwrap().parse().unwrap();
            let passed = idx < 3; // 3 and 4 fail
            (passed, format!("Result for {}", c), 5)
        });

        assert_eq!(culprit, Some("commit_3".to_string()));
        assert!(!steps.is_empty());
        assert!(steps.len() <= 4); // log2(5) is <= 3-4 steps
    }

    #[test]
    fn test_synthesize_cause_and_remedy_unwrap() {
        let diff = "+ let val = data.get(key).unwrap();";
        let (cause, remedy) = AgenticBisectEngine::synthesize_cause_and_remedy(diff, "fix cache");
        assert!(cause.contains("unwrap()"));
        assert!(remedy.contains("error propagation"));
    }

    #[test]
    fn test_synthesize_cause_and_remedy_signature() {
        let diff = "+ pub fn calculate_total(a: i32, b: i32) -> i32 { a + b }";
        let (cause, remedy) = AgenticBisectEngine::synthesize_cause_and_remedy(diff, "update total fn");
        assert!(cause.contains("Signature or behavioral modification"));
        assert!(remedy.contains("downstream call sites"));
    }
}
