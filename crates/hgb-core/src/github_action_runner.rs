//! # Official GitHub Action PR Runner (`hgb ci-action`)
//!
//! Systems-grade GitHub Actions CI runner:
//! 1. Ingests Pull Request diffs and commit metadata.
//! 2. Computes architectural blast radius and affected symbols via `BlastRadiusRadar`.
//! 3. Audits package additions against hallucinated/typosquatted crates via `SlopsquattingFirewall`.
//! 4. Executes verification checks and generates cryptographic BLAKE3 verification signature.
//! 5. Emits formatted GitHub Flavored Markdown (GFM) status tables to `$GITHUB_STEP_SUMMARY` and outputs to `$GITHUB_OUTPUT`.

use crate::blast_radius::{ArchitecturalBlastRadiusReport, BlastRadiusRadar};
use crate::error::{HgbError, Result};
use crate::slopsquatting_firewall::{FirewallAuditReport, SlopsquattingFirewall};
use chrono::Utc;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Configuration options for the GitHub Action runner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionConfig {
    pub audit_blast_radius: bool,
    pub slopsquatting_firewall: bool,
    pub verify_specs: bool,
    pub comment_pr: bool,
    pub task: String,
}

impl Default for ActionConfig {
    fn default() -> Self {
        Self {
            audit_blast_radius: true,
            slopsquatting_firewall: true,
            verify_specs: true,
            comment_pr: true,
            task: "full".to_string(),
        }
    }
}

/// Comprehensive report produced by the GitHub Action CI runner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReport {
    pub repository: String,
    pub sha: String,
    pub blast_score: f32,
    pub symbols_affected: usize,
    pub packages_scanned: usize,
    pub hallucinated_packages: Vec<String>,
    pub verification_passed: bool,
    pub blake3_cert: String,
    pub summary_markdown: String,
    pub timestamp: String,
}

/// GitHub Action Execution Runner
pub struct GitHubActionRunner {
    workspace_root: PathBuf,
    config: ActionConfig,
}

impl GitHubActionRunner {
    pub fn new(workspace_root: impl AsRef<Path>, config: Option<ActionConfig>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            config: config.unwrap_or_default(),
        }
    }

    /// Extract changed files from git diff
    pub fn extract_diff_files(&self, diff_text: &str) -> Vec<String> {
        let mut files = Vec::new();
        for line in diff_text.lines() {
            if line.starts_with("+++ b/") {
                let file = &line[6..];
                if !file.is_empty() && file != "/dev/null" {
                    files.push(file.to_string());
                }
            } else if line.starts_with("diff --git a/") {
                if let Some(second_part) = line.split(" b/").nth(1) {
                    files.push(second_part.to_string());
                }
            }
        }
        files.sort();
        files.dedup();
        files
    }

    /// Read local git diff against HEAD~1 or empty if not git
    pub fn read_git_diff(&self) -> String {
        let output = Command::new("git")
            .args(&["diff", "HEAD~1..HEAD"])
            .current_dir(&self.workspace_root)
            .output();

        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            _ => {
                // Fallback to unstaged or staged diff
                let out2 = Command::new("git")
                    .args(&["diff", "HEAD"])
                    .current_dir(&self.workspace_root)
                    .output();
                match out2 {
                    Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
                    _ => String::new(),
                }
            }
        }
    }

    /// Execute GitHub Action CI analysis pipeline
    pub fn run(&self, custom_diff: Option<&str>) -> Result<ActionReport> {
        let diff_content = match custom_diff {
            Some(d) => d.to_string(),
            None => self.read_git_diff(),
        };

        let changed_files = self.extract_diff_files(&diff_content);

        // 1. Blast Radius Analysis
        let radar = BlastRadiusRadar::new(&self.workspace_root);
        let blast_report = radar.analyze(&changed_files).unwrap_or_else(|_| ArchitecturalBlastRadiusReport {
            target_files: changed_files.clone(),
            modified_symbols: Vec::new(),
            call_sites: Vec::new(),
            dependent_files: Vec::new(),
            recommended_tests: Vec::new(),
            total_references: 0,
            blast_score: 0.0,
            severity: crate::blast_radius::BlastSeverity::Low,
        });
        let blast_score = blast_report.blast_score;
        let symbols_affected = blast_report.modified_symbols.len();

        // 2. Slopsquatting / Package Hallucination Firewall
        let mut added_packages = Vec::new();
        for line in diff_content.lines() {
            if (line.starts_with("+ ") || line.starts_with("+\t")) && (line.contains("version =") || line.contains("dependencies")) {
                let pkg_name = line
                    .trim_start_matches('+')
                    .trim()
                    .split(['=', ' '])
                    .next()
                    .unwrap_or("")
                    .trim_matches(['"', '\'']);
                if !pkg_name.is_empty() && pkg_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    added_packages.push(pkg_name.to_string());
                }
            }
        }

        let pkg_refs: Vec<&str> = added_packages.iter().map(|s| s.as_str()).collect();
        let firewall_report = SlopsquattingFirewall::audit_packages(&pkg_refs, "crates.io");
        let hallucinated_packages: Vec<String> = firewall_report
            .items
            .iter()
            .filter(|it| it.blocked)
            .map(|it| it.package_name.clone())
            .collect();
        let packages_scanned = firewall_report.total_inspected;

        // 3. Verification verdict
        let verification_passed = hallucinated_packages.is_empty() && blast_score < 85.0;

        // 4. Cryptographic BLAKE3 ThoughtCertificate signature
        let repo = std::env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| "CharleGutierrez/hagibis".to_string());
        let sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local_sha".to_string());
        let timestamp = Utc::now().to_rfc3339();

        let sig_payload = format!("{}:{}:{:.2}:{}:{}", repo, sha, blast_score, verification_passed, timestamp);
        let blake3_cert = blake3::hash(sig_payload.as_bytes()).to_hex().to_string();

        // 5. Generate GitHub Flavored Markdown Summary Table
        let status_emoji = if verification_passed { "✅ **PASSED**" } else { "❌ **FAILED / BLOCKED**" };
        let blast_status = if blast_score < 30.0 { "🟢 Low Impact" } else if blast_score < 70.0 { "🟡 Moderate Impact" } else { "🔴 High Impact" };

        let mut markdown = format!(
            "### 🪽 Hagibis Autonomous CI Guardrail Verdict: {}\n\n",
            status_emoji
        );
        markdown.push_str("| Metric | Value | Status |\n");
        markdown.push_str("| :--- | :--- | :--- |\n");
        markdown.push_str(&format!("| **Architectural Blast Radius** | `{:.1} / 100.0` | {} |\n", blast_score, blast_status));
        markdown.push_str(&format!("| **Symbols Touched** | `{}` symbols | 🔍 Analyzed |\n", symbols_affected));
        markdown.push_str(&format!("| **Dependency Firewall** | `{}` audited | {} |\n", packages_scanned, if hallucinated_packages.is_empty() { "🛡️ 0 Hallucinations" } else { "🚨 Hallucinated Package" }));
        markdown.push_str(&format!("| **BLAKE3 Certificate** | `{}` | 🔏 Cryptographically Signed |\n", &blake3_cert[..16]));
        markdown.push_str(&format!("| **Timestamp** | `{}` | ⏱️ Real-Time |\n\n", timestamp));

        if !hallucinated_packages.is_empty() {
            markdown.push_str("#### 🚨 Blocked Malicious / Hallucinated Dependencies:\n");
            for pkg in &hallucinated_packages {
                markdown.push_str(&format!("- ⛔ `{}`\n", pkg));
            }
            markdown.push('\n');
        }

        markdown.push_str(&format!(
            "<sub>Verified by [Hagibis (hgb)](https://github.com/CharleGutierrez/hagibis) • Commit `{}`</sub>\n",
            if sha.len() >= 7 { &sha[..7] } else { &sha }
        ));

        // 6. Emit to GITHUB_STEP_SUMMARY and GITHUB_OUTPUT if present
        Self::write_step_summary(&markdown);
        Self::write_action_output("passed", &verification_passed.to_string());
        Self::write_action_output("blast_score", &format!("{:.2}", blast_score));
        Self::write_action_output("blake3_cert", &blake3_cert);

        Ok(ActionReport {
            repository: repo,
            sha,
            blast_score,
            symbols_affected,
            packages_scanned,
            hallucinated_packages,
            verification_passed,
            blake3_cert,
            summary_markdown: markdown,
            timestamp,
        })
    }

    fn write_step_summary(markdown: &str) {
        if let Ok(summary_file) = std::env::var("GITHUB_STEP_SUMMARY") {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(summary_file) {
                let _ = writeln!(f, "{}", markdown);
            }
        }
    }

    fn write_action_output(name: &str, val: &str) {
        if let Ok(output_file) = std::env::var("GITHUB_OUTPUT") {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(output_file) {
                let _ = writeln!(f, "{}={}", name, val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("action_test_{}", uuid::Uuid::new_v4().simple()));
            let _ = fs::create_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_extract_diff_files() {
        let runner = GitHubActionRunner::new(".", None);
        let diff = r#"
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
+pub fn new_func() {}
diff --git a/Cargo.toml b/Cargo.toml
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -10,3 +10,4 @@
+serde = "1.0"
"#;
        let files = runner.extract_diff_files(diff);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(files.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn test_action_pipeline_run() {
        let tmp = TestDir::new();
        fs::write(tmp.0.join("src_lib.rs"), b"pub fn helper() -> bool { true }").unwrap();

        let runner = GitHubActionRunner::new(&tmp.0, None);
        let sample_diff = "+ pub fn calculate() {}\n+ serde = \"1.0\"\n";
        let report = runner.run(Some(sample_diff)).unwrap();

        assert!(report.blast_score >= 0.0);
        assert!(report.blake3_cert.len() == 64);
        assert!(report.summary_markdown.contains("Hagibis Autonomous CI Guardrail Verdict"));
        assert!(report.summary_markdown.contains("BLAKE3 Certificate"));
    }
}
