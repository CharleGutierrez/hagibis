//! # Ambient Daemon Service (`hgbd --ambient`)
//!
//! Low-power Linux background service that monitors `/proc/stat` and `/sys/class/power_supply`
//! to autonomously execute sleep-state distillation and self-tuning during developer downtime.
//! Manages `.hgb/dream_journal.json` and Linux systemd user service integration.

use chrono::{DateTime, Utc};
use hgb_core::{AmbientDreamEngine, DreamCycleReport};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

/// Individual entry in the persistent dream journal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamJournalEntry {
    pub cycle_id: String,
    pub timestamp: DateTime<Utc>,
    pub commits_ingested: usize,
    pub dpo_pairs_synthesized: usize,
    pub learning_velocity: f32,
    pub style_alignment_score: f32,
    pub cpu_idle_pct: f32,
    pub is_on_ac: bool,
    pub dataset_path: String,
}

/// Persistent dream journal stored in `.hgb/dream_journal.json`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DreamJournal {
    pub total_cycles: usize,
    pub total_dpo_pairs: usize,
    pub last_cycle_timestamp: Option<DateTime<Utc>>,
    pub entries: Vec<DreamJournalEntry>,
}

impl DreamJournal {
    pub fn load(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        if !p.exists() {
            return Self::default();
        }
        if let Ok(content) = fs::read_to_string(p) {
            if let Ok(journal) = serde_json::from_str(&content) {
                return journal;
            }
        }
        Self::default()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        fs::write(p, json)
    }

    pub fn append_report(&mut self, report: &DreamCycleReport) {
        self.total_cycles += 1;
        self.total_dpo_pairs += report.total_dpo_pairs;
        self.last_cycle_timestamp = Some(report.timestamp);

        self.entries.push(DreamJournalEntry {
            cycle_id: format!("dream_{}", report.timestamp.timestamp_millis()),
            timestamp: report.timestamp,
            commits_ingested: report.commits_ingested,
            dpo_pairs_synthesized: report.total_dpo_pairs,
            learning_velocity: report.learning_velocity,
            style_alignment_score: report.style_alignment_score,
            cpu_idle_pct: report.headroom.cpu_idle_pct,
            is_on_ac: report.headroom.is_on_ac,
            dataset_path: report.dataset_path.to_string_lossy().to_string(),
        });

        // Retain latest 100 entries
        if self.entries.len() > 100 {
            self.entries.remove(0);
        }
    }

    /// Produce a morning greeting summary of overnight ambient learning
    pub fn morning_greeting(&self) -> String {
        let recent_entries: Vec<&DreamJournalEntry> = self
            .entries
            .iter()
            .filter(|e| {
                if let Some(last) = self.last_cycle_timestamp {
                    last.signed_duration_since(e.timestamp).num_hours() <= 24
                } else {
                    false
                }
            })
            .collect();

        if recent_entries.is_empty() {
            return "🪽 Hagibis was standing by overnight. No new dream cycles executed.".to_string();
        }

        let overnight_pairs: usize = recent_entries.iter().map(|e| e.dpo_pairs_synthesized).sum();
        let avg_style: f32 = recent_entries.iter().map(|e| e.style_alignment_score).sum::<f32>()
            / recent_entries.len() as f32;

        format!(
            "🪽 Good morning! Overnight, Hagibis completed {} ambient dream cycles:\n   • Synthesized {} DPO preference pairs from your git commits\n   • Style alignment score: {:.1}%\n   • Quantized LoRA runbook updated at .hgb/distillation/train_lora.sh",
            recent_entries.len(),
            overnight_pairs,
            avg_style
        )
    }
}

/// Ambient background daemon service
pub struct AmbientDaemonService {
    workspace_root: PathBuf,
    journal_path: PathBuf,
    poll_interval: Duration,
    min_headroom_ticks: usize,
    running: Arc<AtomicBool>,
}

impl AmbientDaemonService {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        let journal_path = ws.join(".hgb").join("dream_journal.json");
        Self {
            workspace_root: ws,
            journal_path,
            poll_interval: Duration::from_secs(30),
            min_headroom_ticks: 3, // Requires 3 consecutive idle checks (90 seconds)
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn with_min_headroom_ticks(mut self, ticks: usize) -> Self {
        self.min_headroom_ticks = ticks;
        self
    }

    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }

    pub fn journal(&self) -> DreamJournal {
        DreamJournal::load(&self.journal_path)
    }

    /// Run single headless evaluation tick
    pub fn tick(&self, consecutive_idle: &mut usize) -> Option<DreamCycleReport> {
        let engine = AmbientDreamEngine::new(&self.workspace_root);
        let headroom = engine.check_headroom();

        if headroom.has_headroom {
            *consecutive_idle += 1;
        } else {
            *consecutive_idle = 0;
        }

        if *consecutive_idle >= self.min_headroom_ticks {
            // Sufficient sustained idle detected — execute dream cycle!
            if let Ok(report) = engine.run_dream_cycle(15, false) {
                let mut journal = DreamJournal::load(&self.journal_path);
                journal.append_report(&report);
                let _ = journal.save(&self.journal_path);
                *consecutive_idle = 0; // Reset counter for next window
                return Some(report);
            }
        }

        None
    }

    /// Generate systemd user unit configuration content
    pub fn generate_systemd_unit(hgbd_bin_path: Option<&str>) -> String {
        let bin = hgbd_bin_path.unwrap_or("%h/.cargo/bin/hgbd");
        format!(
            r#"[Unit]
Description=Hagibis Resident Microkernel & Ambient Swarm Daemon
Documentation=https://github.com/CharleGutierrez/hagibis
After=network.target

[Service]
Type=simple
ExecStart={} --ambient
Restart=always
RestartSec=10s
Nice=15
StandardOutput=journal
StandardError=journal
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"#,
            bin
        )
    }

    /// Install systemd user service unit into ~/.config/systemd/user/hgbd.service
    pub fn install_systemd_service(hgbd_bin_path: Option<&str>) -> std::io::Result<PathBuf> {
        let home = std::env::var("HOME").map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
        let unit_dir = PathBuf::from(home).join(".config").join("systemd").join("user");
        fs::create_dir_all(&unit_dir)?;

        let unit_path = unit_dir.join("hgbd.service");
        let content = Self::generate_systemd_unit(hgbd_bin_path);
        fs::write(&unit_path, content)?;
        Ok(unit_path)
    }

    /// Check systemd user service installation status
    pub fn check_systemd_service_installed() -> bool {
        if let Ok(home) = std::env::var("HOME") {
            let unit_path = PathBuf::from(home)
                .join(".config")
                .join("systemd")
                .join("user")
                .join("hgbd.service");
            unit_path.exists()
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hgb_core::SystemHeadroom;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("daemon_test_{}_{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
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
    fn test_dream_journal_lifecycle() {
        let tmp = TestDir::new();
        let journal_path = tmp.0.join("dream_journal.json");

        let mut journal = DreamJournal::load(&journal_path);
        assert_eq!(journal.total_cycles, 0);

        let report = DreamCycleReport {
            learning_velocity: 1.2,
            style_alignment_score: 97.4,
            total_dpo_pairs: 6,
            commits_ingested: 5,
            headroom: SystemHeadroom {
                cpu_idle_pct: 82.0,
                power_state: hgb_core::PowerState::AcPower,
                is_on_ac: true,
                has_headroom: true,
                timestamp: Utc::now(),
            },
            dataset_path: tmp.0.join("dataset.jsonl"),
            lora_config_path: tmp.0.join("lora.json"),
            runbook_path: tmp.0.join("train.sh"),
            timestamp: Utc::now(),
        };

        journal.append_report(&report);
        assert_eq!(journal.total_cycles, 1);
        assert_eq!(journal.total_dpo_pairs, 6);

        journal.save(&journal_path).unwrap();
        assert!(journal_path.exists());

        let loaded = DreamJournal::load(&journal_path);
        assert_eq!(loaded.total_cycles, 1);
        assert_eq!(loaded.entries.len(), 1);

        let greeting = loaded.morning_greeting();
        assert!(greeting.contains("Good morning"));
        assert!(greeting.contains("97.4%"));
    }

    #[test]
    fn test_systemd_unit_generation() {
        let unit = AmbientDaemonService::generate_systemd_unit(Some("/usr/local/bin/hgbd"));
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("ExecStart=/usr/local/bin/hgbd --ambient"));
        assert!(unit.contains("WantedBy=default.target"));
    }
}
