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
        } else if diff.contains(".expect(") {
            (
                "Commit introduced `.expect(...)` call that panics when precondition is violated".to_string(),
                "Handle the failure branch gracefully or return standard Result<T, E>".to_string(),
            )
        } else if diff.contains("panic!(") || diff.contains("todo!(") || diff.contains("unimplemented!(") {
            (
                "Commit introduced explicit panic/todo/unimplemented assertion".to_string(),
                "Implement the incomplete pathway and return standard Result<T, E>".to_string(),
            )
        } else if diff.contains("Cargo.toml") && (diff.contains("[dependencies") || diff.contains("version =")) {
            (
                "Dependency modification or version bump in Cargo.toml broke compatibility".to_string(),
                "Pin to known-compatible dependency version or resolve breaking API changes".to_string(),
            )
        } else if diff.contains("unsafe {") {
            (
                "Commit introduced unsafe block with potential memory safety or invariant violation".to_string(),
                "Audit unsafe pointer arithmetic/casting and enforce memory safety invariants".to_string(),
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

    /// Synthesize an automated patch recommendation or git command for the culprit commit
    pub fn recommend_patch(&self, analysis: &CulpritAnalysis) -> String {
        format!(
            "// Recommended Remedy for Culprit Commit {}:\n// Root Cause: {}\n// Remedy: {}\n// 1-Key Revert Command: git revert --no-edit {}",
            analysis.commit_hash,
            analysis.suspected_cause,
            analysis.recommended_remedy,
            analysis.commit_hash
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
    fn test_binary_search_edge_cases() {
        // Single commit that is bad
        let single: Vec<String> = vec!["only_commit".to_string()];
        let (culprit, steps) = AgenticBisectEngine::binary_search_commits(&single, |_, _| (false, "fail".into(), 1));
        assert_eq!(culprit, Some("only_commit".to_string()));
        assert_eq!(steps.len(), 1);

        // Empty commits
        let empty: Vec<String> = Vec::new();
        let (culprit_empty, steps_empty) = AgenticBisectEngine::binary_search_commits(&empty, |_, _| (true, "ok".into(), 1));
        assert_eq!(culprit_empty, None);
        assert!(steps_empty.is_empty());

        // First commit is culprit
        let commits: Vec<String> = (0..4).map(|i| format!("c_{}", i)).collect();
        let (c_first, _) = AgenticBisectEngine::binary_search_commits(&commits, |_, _| (false, "fail".into(), 1));
        assert_eq!(c_first, Some("c_0".to_string()));
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

    #[test]
    fn test_synthesize_cause_and_remedy_cargo_and_expect() {
        let diff_expect = "+ let user = db.find_user().expect(\"user not found\");";
        let (cause_expect, remedy_expect) = AgenticBisectEngine::synthesize_cause_and_remedy(diff_expect, "find user");
        assert!(cause_expect.contains("expect"));
        assert!(remedy_expect.contains("gracefully"));

        let diff_cargo = "+++ b/Cargo.toml\n+ tokio = { version = \"2.0\" }";
        let (cause_cargo, remedy_cargo) = AgenticBisectEngine::synthesize_cause_and_remedy(diff_cargo, "bump tokio");
        assert!(cause_cargo.contains("Cargo.toml"));
        assert!(cause_cargo.contains("compatibility"));
        assert!(remedy_cargo.contains("compatible"));

        let engine = AgenticBisectEngine::new(std::path::PathBuf::from("."));
        let analysis = CulpritAnalysis {
            commit_hash: "abcd1234ef".to_string(),
            author: "Tester".to_string(),
            commit_message: "bump tokio".to_string(),
            files_changed: vec!["Cargo.toml".to_string()],
            diff_snippet: diff_cargo.to_string(),
            suspected_cause: cause_cargo,
            recommended_remedy: remedy_cargo,
        };
        let recommendation = engine.recommend_patch(&analysis);
        assert!(recommendation.contains("git revert --no-edit abcd1234ef"));
    }

    #[test]
    fn test_agentic_bisect_real_git_workflow() {
        let temp = tempdir().unwrap();
        let repo = temp.path();

        // 1. Initialize git repo
        let _ = Command::new("git").args(["init"]).current_dir(repo).output().unwrap();
        let _ = Command::new("git").args(["config", "user.email", "bisect@hagibis.ai"]).current_dir(repo).output().unwrap();
        let _ = Command::new("git").args(["config", "user.name", "BisectBot"]).current_dir(repo).output().unwrap();

        // Commit 0 (good commit)
        let f = repo.join("test.txt");
        std::fs::write(&f, "pass\n").unwrap();
        let _ = Command::new("git").args(["add", "test.txt"]).current_dir(repo).output().unwrap();
        let _ = Command::new("git").args(["commit", "-m", "commit 0: clean pass"]).current_dir(repo).output().unwrap();
        let c0 = String::from_utf8_lossy(&Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap().stdout).trim().to_string();

        // Commit 1 (good commit)
        std::fs::write(&f, "pass\nstep 1\n").unwrap();
        let _ = Command::new("git").args(["add", "test.txt"]).current_dir(repo).output().unwrap();
        let _ = Command::new("git").args(["commit", "-m", "commit 1: still passing"]).current_dir(repo).output().unwrap();

        // Commit 2 (culprit commit introducing failure)
        std::fs::write(&f, "FAIL_REGRESSION\n").unwrap();
        let _ = Command::new("git").args(["add", "test.txt"]).current_dir(repo).output().unwrap();
        let _ = Command::new("git").args(["commit", "-m", "commit 2: broke feature with unwrap()"]).current_dir(repo).output().unwrap();
        let c2 = String::from_utf8_lossy(&Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap().stdout).trim().to_string();

        // Commit 3 (bad commit)
        std::fs::write(&f, "FAIL_REGRESSION\nstep 3\n").unwrap();
        let _ = Command::new("git").args(["add", "test.txt"]).current_dir(repo).output().unwrap();
        let _ = Command::new("git").args(["commit", "-m", "commit 3: latest bad"]).current_dir(repo).output().unwrap();
        let c3 = String::from_utf8_lossy(&Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap().stdout).trim().to_string();

        // Run bisect with a test script checking that test.txt does not contain FAIL_REGRESSION
        let engine = AgenticBisectEngine::new(repo);
        let config = BisectConfig::new(c0, c3, "grep -v 'FAIL_REGRESSION' test.txt");

        let outcome = engine.run_bisect(&config).expect("Bisect should execute successfully");
        assert!(outcome.success);
        assert_eq!(outcome.culprit_commit, Some(c2.clone()));
        assert!(outcome.analysis.is_some());

        let analysis = outcome.analysis.unwrap();
        assert_eq!(analysis.commit_hash, c2);
        assert!(analysis.commit_message.contains("commit 2"));
    }
}
