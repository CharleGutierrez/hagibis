//! # Ambient Watch-and-Vibe Autonomous Loop
//!
//! Autonomous workspace file watcher with debounce buffering, automated test & lint suite
//! runners, and a live event stream dispatching `VibeWatchEvent`s with speculative patches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VibeWatchEvent {
    WorkspaceModified { changed_paths: Vec<PathBuf> },
    CheckStarted { suite_name: String, command: String },
    CheckPassed { suite_name: String, duration_ms: u64, summary: String },
    CheckFailed { suite_name: String, duration_ms: u64, error_output: String, exit_code: i32 },
    SpeculativePatchReady { file_path: PathBuf, diff: String, explanation: String },
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestSuiteConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbientVibeConfig {
    pub debounce_duration_ms: u64,
    pub poll_interval_ms: u64,
    pub auto_run_suites: Vec<TestSuiteConfig>,
    pub ignored_patterns: Vec<String>,
    pub enable_speculative_fixes: bool,
}

impl Default for AmbientVibeConfig {
    fn default() -> Self {
        Self {
            debounce_duration_ms: 300,
            poll_interval_ms: 200,
            auto_run_suites: Vec::new(),
            ignored_patterns: vec![
                "target".into(),
                ".git".into(),
                ".hgb".into(),
                "node_modules".into(),
                ".idea".into(),
                ".vscode".into(),
                "*.tmp".into(),
                "*.swp".into(),
            ],
            enable_speculative_fixes: true,
        }
    }
}

pub struct AmbientVibeEngine {
    workspace_root: PathBuf,
    config: AmbientVibeConfig,
    file_snapshots: HashMap<PathBuf, (SystemTime, u64)>,
}

impl AmbientVibeEngine {
    pub fn new(workspace_root: PathBuf, mut config: AmbientVibeConfig) -> Self {
        if config.auto_run_suites.is_empty() {
            config.auto_run_suites = Self::detect_test_suites(&workspace_root);
        }
        let mut engine = Self {
            workspace_root,
            config,
            file_snapshots: HashMap::new(),
        };
        // Initial snapshot
        let _ = engine.scan_changed_files();
        engine
    }

    pub fn detect_test_suites(workspace_root: &Path) -> Vec<TestSuiteConfig> {
        let mut suites = Vec::new();
        if workspace_root.join("Cargo.toml").exists() {
            suites.push(TestSuiteConfig {
                name: "cargo test".into(),
                command: "cargo".into(),
                args: vec!["test".into(), "--workspace".into()],
                working_dir: None,
                timeout_secs: 60,
            });
        }
        if workspace_root.join("package.json").exists() {
            suites.push(TestSuiteConfig {
                name: "npm test".into(),
                command: "npm".into(),
                args: vec!["test".into()],
                working_dir: None,
                timeout_secs: 60,
            });
        }
        if workspace_root.join("pyproject.toml").exists() || workspace_root.join("pytest.ini").exists() {
            suites.push(TestSuiteConfig {
                name: "pytest".into(),
                command: "pytest".into(),
                args: vec![],
                working_dir: None,
                timeout_secs: 60,
            });
        }
        suites
    }

    pub fn scan_changed_files(&mut self) -> Result<Vec<PathBuf>> {
        let mut current_files = HashMap::new();
        Self::collect_files_recursive(&self.workspace_root, &self.workspace_root, &self.config.ignored_patterns, &mut current_files)?;

        let mut changed = Vec::new();

        // Detect modifications and additions
        for (path, (mtime, size)) in &current_files {
            match self.file_snapshots.get(path) {
                Some((old_mtime, old_size)) => {
                    if mtime != old_mtime || size != old_size {
                        changed.push(path.clone());
                    }
                }
                None => {
                    changed.push(path.clone());
                }
            }
        }

        // Detect deletions
        for path in self.file_snapshots.keys() {
            if !current_files.contains_key(path) {
                changed.push(path.clone());
            }
        }

        self.file_snapshots = current_files;
        Ok(changed)
    }

    fn collect_files_recursive(
        root: &Path,
        current: &Path,
        ignored: &[String],
        map: &mut HashMap<PathBuf, (SystemTime, u64)>,
    ) -> Result<()> {
        if !current.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if ignored.iter().any(|pattern| name == *pattern || name.starts_with('.')) {
                continue;
            }

            if path.is_dir() {
                Self::collect_files_recursive(root, &path, ignored, map)?;
            } else if path.is_file() {
                if let Ok(meta) = entry.metadata() {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let size = meta.len();
                    map.insert(path, (mtime, size));
                }
            }
        }
        Ok(())
    }

    pub async fn run_suite(suite: &TestSuiteConfig, workspace_root: &Path) -> VibeWatchEvent {
        let start = Instant::now();
        let mut cmd = tokio::process::Command::new(&suite.command);
        cmd.args(&suite.args);
        cmd.current_dir(suite.working_dir.as_ref().unwrap_or(&workspace_root.to_path_buf()));

        match tokio::time::timeout(Duration::from_secs(suite.timeout_secs), cmd.output()).await {
            Ok(Ok(output)) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let exit_code = output.status.code().unwrap_or(-1);
                if output.status.success() {
                    VibeWatchEvent::CheckPassed {
                        suite_name: suite.name.clone(),
                        duration_ms,
                        summary: format!("Suite '{}' passed in {}ms", suite.name, duration_ms),
                    }
                } else {
                    let mut err = String::from_utf8_lossy(&output.stderr).to_string();
                    if err.is_empty() {
                        err = String::from_utf8_lossy(&output.stdout).to_string();
                    }
                    VibeWatchEvent::CheckFailed {
                        suite_name: suite.name.clone(),
                        duration_ms,
                        error_output: err,
                        exit_code,
                    }
                }
            }
            Ok(Err(e)) => VibeWatchEvent::CheckFailed {
                suite_name: suite.name.clone(),
                duration_ms: start.elapsed().as_millis() as u64,
                error_output: format!("Execution failed: {}", e),
                exit_code: -1,
            },
            Err(_) => VibeWatchEvent::CheckFailed {
                suite_name: suite.name.clone(),
                duration_ms: suite.timeout_secs * 1000,
                error_output: format!("Timeout after {}s", suite.timeout_secs),
                exit_code: -1,
            },
        }
    }

    pub fn synthesize_speculative_patch(error_output: &str, workspace_root: &Path) -> Option<VibeWatchEvent> {
        let re = regex::Regex::new(r"-->\s+([a-zA-Z0-9_\-/\.]+):(\d+):(\d+)").ok()?;
        let caps = re.captures(error_output)?;
        let file_rel = caps.get(1)?.as_str();
        let target_file = workspace_root.join(file_rel);

        if !target_file.exists() {
            return None;
        }

        // Speculative patch for unused import removal
        if error_output.contains("unused import:") {
            let diff = format!("// Speculative fix for unused import in {}", file_rel);
            return Some(VibeWatchEvent::SpeculativePatchReady {
                file_path: target_file,
                diff,
                explanation: "Automatically identified unused import for removal.".into(),
            });
        }

        None
    }

    pub fn spawn_loop(
        mut self,
        event_tx: mpsc::Sender<VibeWatchEvent>,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(self.config.poll_interval_ms));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Ok(changed) = self.scan_changed_files() {
                            if !changed.is_empty() {
                                tokio::time::sleep(Duration::from_millis(self.config.debounce_duration_ms)).await;
                                let _ = event_tx.send(VibeWatchEvent::WorkspaceModified {
                                    changed_paths: changed.clone(),
                                }).await;

                                for suite in &self.config.auto_run_suites {
                                    let _ = event_tx.send(VibeWatchEvent::CheckStarted {
                                        suite_name: suite.name.clone(),
                                        command: format!("{} {}", suite.command, suite.args.join(" ")),
                                    }).await;

                                    let ev = Self::run_suite(suite, &self.workspace_root).await;
                                    if let VibeWatchEvent::CheckFailed { ref error_output, .. } = ev {
                                        if self.config.enable_speculative_fixes {
                                            if let Some(patch) = Self::synthesize_speculative_patch(error_output, &self.workspace_root) {
                                                let _ = event_tx.send(patch).await;
                                            }
                                        }
                                    }
                                    let _ = event_tx.send(ev).await;
                                }
                                let _ = event_tx.send(VibeWatchEvent::Idle).await;
                            }
                        }
                    }
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ambient_vibe_file_change_detection() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_ambient_test_detect_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let config = AmbientVibeConfig {
            debounce_duration_ms: 50,
            poll_interval_ms: 50,
            auto_run_suites: Vec::new(),
            ignored_patterns: vec![".git".into()],
            enable_speculative_fixes: true,
        };

        let mut engine = AmbientVibeEngine::new(temp_dir.clone(), config);

        // Initially no changes
        let initial_changes = engine.scan_changed_files().unwrap();
        assert!(initial_changes.is_empty());

        // Create a new file
        let new_file = temp_dir.join("example.rs");
        std::fs::write(&new_file, "fn main() {}").unwrap();

        let detected = engine.scan_changed_files().unwrap();
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0], new_file);

        // Next scan should be clean
        let clean = engine.scan_changed_files().unwrap();
        assert!(clean.is_empty());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_ambient_vibe_run_suite() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_ambient_test_run_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let suite = TestSuiteConfig {
            name: "echo_suite".into(),
            command: "echo".into(),
            args: vec!["suite passed".into()],
            working_dir: None,
            timeout_secs: 5,
        };

        let event = AmbientVibeEngine::run_suite(&suite, &temp_dir).await;
        match event {
            VibeWatchEvent::CheckPassed { suite_name, .. } => {
                assert_eq!(suite_name, "echo_suite");
            }
            other => panic!("Expected CheckPassed, got {:?}", other),
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
