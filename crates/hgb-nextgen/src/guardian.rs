//! # Continuous Guardian Mode: Passive File Watcher & Ghost-Healing Engine
//!
//! Passively detects workspace file modifications with debouncing, executes
//! incremental background verification, and pre-computes memory-staged Ghost-Fixes
//! without interrupting developer flow.

use crate::heal::HealEngine;
use hgb_core::{
    AgyCrud, CommandOptions, HgbError, HgbProvider, ReplaceOptions, Result, ViewFileOptions,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::{mpsc, RwLock};

/// Current lifecycle status of a staged ghost-fix
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GhostFixStatus {
    ReadyToApply,
    Verifying,
    Applied,
    Dismissed,
}

impl std::fmt::Display for GhostFixStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadyToApply => write!(f, "READY"),
            Self::Verifying => write!(f, "VERIFYING"),
            Self::Applied => write!(f, "APPLIED"),
            Self::Dismissed => write!(f, "DISMISSED"),
        }
    }
}

/// A pre-computed surgical repair staged in daemon memory
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GhostFix {
    pub id: String,
    pub file_path: PathBuf,
    pub line_number: usize,
    pub error_code: Option<String>,
    pub diagnostic_message: String,
    pub target_content: String,
    pub replacement_content: String,
    pub status: GhostFixStatus,
    pub created_at: String,
    pub confidence_score: f32,
}

/// Real-time events dispatched by Guardian Engine
#[derive(Debug, Clone)]
pub enum GuardianEvent {
    WatchingStarted { root: PathBuf, extensions: Vec<String> },
    FilesChanged { paths: Vec<PathBuf> },
    VerificationStarted { command: String },
    VerificationPassed { command: String, duration_ms: u64 },
    VerificationFailed { command: String, error_count: usize, duration_ms: u64 },
    GhostFixStaged(GhostFix),
    GhostFixApplied { fix_id: String, file_path: PathBuf },
    GhostFixDismissed { fix_id: String },
    WatchingStopped,
}

/// Configuration settings for Guardian Engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardianConfig {
    pub workspace_root: PathBuf,
    pub check_command: String,
    pub debounce_ms: u64,
    pub extensions: Vec<String>,
    pub auto_heal: bool,
}

impl Default for GuardianConfig {
    fn default() -> Self {
        Self {
            workspace_root: PathBuf::from("."),
            check_command: "cargo check".to_string(),
            debounce_ms: 350,
            extensions: vec![
                "rs".into(), "ts".into(), "tsx".into(), "js".into(), "jsx".into(),
                "py".into(), "go".into(),
            ],
            auto_heal: false,
        }
    }
}

/// The Continuous Guardian Engine
pub struct GuardianEngine {
    pub config: GuardianConfig,
    provider: Arc<dyn HgbProvider>,
    ghost_fixes: Arc<RwLock<HashMap<String, GhostFix>>>,
    file_mtimes: Arc<RwLock<HashMap<PathBuf, SystemTime>>>,
    event_tx: mpsc::UnboundedSender<GuardianEvent>,
    event_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<GuardianEvent>>>>,
    pub is_running: Arc<RwLock<bool>>,
}

impl GuardianEngine {
    /// Initialize a new Continuous Guardian Engine
    pub fn new(config: GuardianConfig, provider: Arc<dyn HgbProvider>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            config,
            provider,
            ghost_fixes: Arc::new(RwLock::new(HashMap::new())),
            file_mtimes: Arc::new(RwLock::new(HashMap::new())),
            event_tx: tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(Some(rx))),
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Take event receiver for streaming to Cockpit / TUI
    pub async fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<GuardianEvent>> {
        let mut guard = self.event_rx.lock().await;
        guard.take()
    }

    /// Scan directory and detect any modified or new files based on mtime
    pub async fn detect_modified_files(&self) -> Result<Vec<PathBuf>> {
        let mut modified = Vec::new();
        let mut all_files = Vec::new();
        self.collect_files(&self.config.workspace_root, &mut all_files)?;

        let mut mtimes = self.file_mtimes.write().await;
        for path in all_files {
            if let Ok(meta) = fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    match mtimes.get(&path) {
                        Some(&prev) if prev != mtime => {
                            mtimes.insert(path.clone(), mtime);
                            modified.push(path);
                        }
                        None => {
                            mtimes.insert(path.clone(), mtime);
                            // Initial seeding
                        }
                        _ => {}
                    }
                }
            }
        }

        if !modified.is_empty() {
            let _ = self.event_tx.send(GuardianEvent::FilesChanged { paths: modified.clone() });
        }

        Ok(modified)
    }

    /// Run passive verification check and pre-compute ghost fix if errors occur
    pub async fn run_passive_check(&self) -> Result<Option<GhostFix>> {
        let t0 = Instant::now();
        let cmd = self.config.check_command.clone();
        let _ = self.event_tx.send(GuardianEvent::VerificationStarted { command: cmd.clone() });

        let check_res = AgyCrud::run_command(
            &cmd,
            Some(&self.config.workspace_root),
            CommandOptions::default(),
        ).await?;
        let duration_ms = t0.elapsed().as_millis() as u64;

        if check_res.exit_code == 0 {
            let _ = self.event_tx.send(GuardianEvent::VerificationPassed { command: cmd, duration_ms });
            return Ok(None);
        }

        // Diagnostics extraction
        let diags = HealEngine::parse_diagnostics(&check_res.combined_output);
        let _ = self.event_tx.send(GuardianEvent::VerificationFailed {
            command: cmd.clone(),
            error_count: diags.len(),
            duration_ms,
        });

        if let Some(first_diag) = diags.first() {
            let target_abs = if first_diag.file_path.is_absolute() {
                first_diag.file_path.clone()
            } else {
                self.config.workspace_root.join(&first_diag.file_path)
            };

            if !target_abs.exists() {
                return Ok(None);
            }

            let start_line = first_diag.line_number.saturating_sub(4).max(1);
            let end_line = first_diag.line_number + 4;

            let view_res = AgyCrud::view_file(
                &target_abs,
                ViewFileOptions {
                    start_line: Some(start_line),
                    end_line: Some(end_line),
                    content_offset: None,
                    max_lines: Some(20),
                    line_numbers: false,
                },
            )?;

            let prompt = format!(
                "You are an automated compiler ghost-healing assistant.\n\
                Command: {}\n\
                Error: {}\nCode: {:?}\nFile: {}:{}\n\n\
                Snippet:\n```\n{}\n```\n\n\
                Return ONLY a JSON block with exact target and replacement strings:\n\
                ```json\n\
                {{\n\
                  \"target\": \"<exact original code to replace>\",\n\
                  \"replacement\": \"<corrected code>\"\n\
                }}\n\
                ```",
                cmd, first_diag.message, first_diag.error_code, target_abs.display(), first_diag.line_number, view_res.content
            );

            let model_resp = self.provider.complete(&prompt, None).await?;
            if let Some((target, replacement)) = HealEngine::extract_repair_json(&model_resp) {
                let fix_id = format!("GF-{:03}", self.ghost_fixes.read().await.len() + 1);
                let ghost_fix = GhostFix {
                    id: fix_id.clone(),
                    file_path: target_abs,
                    line_number: first_diag.line_number,
                    error_code: first_diag.error_code.clone(),
                    diagnostic_message: first_diag.message.clone(),
                    target_content: target,
                    replacement_content: replacement,
                    status: GhostFixStatus::ReadyToApply,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    confidence_score: 0.95,
                };

                let mut fixes = self.ghost_fixes.write().await;
                fixes.insert(fix_id, ghost_fix.clone());
                let _ = self.event_tx.send(GuardianEvent::GhostFixStaged(ghost_fix.clone()));

                if self.config.auto_heal {
                    self.apply_ghost_fix(&ghost_fix.id).await?;
                }

                return Ok(Some(ghost_fix));
            }
        }

        Ok(None)
    }

    /// Explicitly stage a ghost fix in memory
    pub async fn stage_ghost_fix(&self, fix: GhostFix) {
        let mut fixes = self.ghost_fixes.write().await;
        let id = fix.id.clone();
        fixes.insert(id, fix.clone());
        let _ = self.event_tx.send(GuardianEvent::GhostFixStaged(fix));
    }

    /// Apply a staged ghost fix to disk
    pub async fn apply_ghost_fix(&self, fix_id: &str) -> Result<String> {
        let mut fixes = self.ghost_fixes.write().await;
        let fix = fixes.get_mut(fix_id).ok_or_else(|| {
            HgbError::NotFound(format!("Ghost fix '{}' not found", fix_id))
        })?;

        let res = AgyCrud::replace_file_content(
            &fix.file_path,
            &fix.target_content,
            &fix.replacement_content,
            ReplaceOptions {
                start_line: Some(fix.line_number.saturating_sub(10).max(1)),
                end_line: Some(fix.line_number + 10),
                allow_multiple: false,
                create_backup: false,
                instruction: Some("Guardian ghost-fix applied".to_string()),
                description: Some(fix.diagnostic_message.clone()),
                target_lint_error_ids: fix.error_code.clone().into_iter().collect(),
            },
        )?;

        fix.status = GhostFixStatus::Applied;
        let _ = self.event_tx.send(GuardianEvent::GhostFixApplied {
            fix_id: fix_id.to_string(),
            file_path: fix.file_path.clone(),
        });

        Ok(res)
    }

    /// Retrieve all active staged ghost fixes
    pub async fn get_ghost_fixes(&self) -> Vec<GhostFix> {
        let fixes = self.ghost_fixes.read().await;
        fixes.values().cloned().collect()
    }

    /// Dismiss a staged ghost fix
    pub async fn dismiss_ghost_fix(&self, fix_id: &str) -> Result<()> {
        let mut fixes = self.ghost_fixes.write().await;
        if let Some(fix) = fixes.get_mut(fix_id) {
            fix.status = GhostFixStatus::Dismissed;
            let _ = self.event_tx.send(GuardianEvent::GhostFixDismissed { fix_id: fix_id.to_string() });
            Ok(())
        } else {
            Err(HgbError::NotFound(format!("Ghost fix '{}' not found", fix_id)))
        }
    }

    fn collect_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(());
        }

        let entries = fs::read_dir(dir).map_err(HgbError::Io)?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" {
                continue;
            }

            if path.is_dir() {
                self.collect_files(&path, files)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if self.config.extensions.iter().any(|e| e == ext) {
                        files.push(path);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct MockGuardianProvider {
        response: String,
    }

    #[async_trait]
    impl HgbProvider for MockGuardianProvider {
        fn name(&self) -> &str {
            "mock_guardian"
        }
        async fn complete(&self, _prompt: &str, _model: Option<&str>) -> Result<String> {
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_guardian_file_modification_detection() {
        let tmp = std::env::temp_dir().join(format!("hgb_guardian_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&tmp).unwrap();

        let file = tmp.join("test.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        let config = GuardianConfig {
            workspace_root: tmp.clone(),
            check_command: "true".to_string(),
            ..Default::default()
        };
        let provider = Arc::new(MockGuardianProvider { response: "".to_string() });
        let guardian = GuardianEngine::new(config, provider);

        // Initial scan: seeds mtimes
        let initial = guardian.detect_modified_files().await.unwrap();
        assert!(initial.is_empty());

        // Sleep briefly and mutate file
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        fs::write(&file, "fn main() { println!(\"mutated\"); }\n").unwrap();

        let modified = guardian.detect_modified_files().await.unwrap();
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0], file);

        let _ = fs::remove_dir_all(&tmp);
    }
}
