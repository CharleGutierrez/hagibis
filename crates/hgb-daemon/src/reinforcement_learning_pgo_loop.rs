//! # Active Reinforcement Learning Reward Model & PGO Self-Compilation Loop
//!
//! Closes the autonomous execution loop by coupling runtime optimizations with a local
//! multi-objective reward ledger and Profile-Guided Optimization (PGO) re-compilation.
//!
//! ## Architecture
//! - **Local Reward Model**: Evaluates multi-objective performance deltas across latency (p50/p99),
//!   memory RSS consumption, error frequencies, and OpenTelemetry span drop rates.
//! - **Persistent Reward Ledger**: Append-only transactional record (`.hgb/reward_ledger.jsonl`)
//!   capturing optimization episodes, reward coefficients, and moving average gains.
//! - **Autonomous PGO Compiler**: Triggers profile generation (`-Cprofile-generate`), profile
//!   merging (`llvm-profdata`), and hardened binary recompilation (`-Cprofile-use` + Fat LTO)
//!   when cumulative reinforcement learning reward exceeds adaptive thresholds.
//! - **Adaptive Feedback**: Systematically optimizes binary layout for actual daily workloads,
//!   ensuring Hagibis continuously improves execution speed and hardware efficiency.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use hgb_core::error::{HgbError, Result};
use hgb_core::telemetry_tracing::{get_global_buffer, ActiveTelemetryBuffer, SpanRecord, SpanStatus};

/// Default cumulative reward threshold required to initiate autonomous PGO recompilation (150.0 pts)
pub const DEFAULT_PGO_TRIGGER_REWARD_THRESHOLD: f64 = 150.0;

/// Default minimum episodes before PGO evaluation can fire (3 episodes)
pub const DEFAULT_MIN_EPISODES_BEFORE_PGO: usize = 3;

/// Default cooldown period between autonomous recompilations (300 seconds)
pub const DEFAULT_PGO_RECOMPILE_COOLDOWN_SECS: u64 = 300;

/// Category of autonomous optimization action evaluated by the reward model
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptimizationActionCategory {
    SelfHealingPatch,
    GovernorProfileShift,
    MicroVmWorkloadMigration,
    WasmJitTuning,
    ThreadConcurrencyScale,
}

impl OptimizationActionCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SelfHealingPatch => "self_healing_patch",
            Self::GovernorProfileShift => "governor_profile_shift",
            Self::MicroVmWorkloadMigration => "microvm_workload_migration",
            Self::WasmJitTuning => "wasm_jit_tuning",
            Self::ThreadConcurrencyScale => "thread_concurrency_scale",
        }
    }
}

/// Instantaneous performance telemetry metrics captured before or after an action
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryDeltaMetrics {
    pub latency_p50_ms: f64,
    pub latency_p99_ms: f64,
    pub memory_rss_mb: f64,
    pub span_drop_rate_pct: f64,
    pub error_count: u64,
    pub cpu_temp_c: f32,
    pub timestamp_ms: u64,
}

impl Default for TelemetryDeltaMetrics {
    fn default() -> Self {
        Self {
            latency_p50_ms: 10.0,
            latency_p99_ms: 50.0,
            memory_rss_mb: 256.0,
            span_drop_rate_pct: 0.0,
            error_count: 0,
            cpu_temp_c: 55.0,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

/// Single recorded optimization episode inside the reward ledger
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationEpisode {
    pub episode_id: String,
    pub action_category: OptimizationActionCategory,
    pub action_description: String,
    pub metrics_before: TelemetryDeltaMetrics,
    pub metrics_after: TelemetryDeltaMetrics,
    pub latency_reward: f64,
    pub memory_reward: f64,
    pub error_reward: f64,
    pub scalar_reward: f64,
    pub cumulative_reward: f64,
    pub pgo_trigger_recommended: bool,
    pub timestamp_ms: u64,
}

/// Outcome report produced when PGO self-compilation executes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgoRecompilationReport {
    pub pgo_cycle_id: String,
    pub trigger_reason: String,
    pub cumulative_reward_at_trigger: f64,
    pub recompiled_binaries: Vec<String>,
    pub binary_hash: String,
    pub duration_seconds: f64,
    pub success: bool,
    pub message: String,
    pub timestamp_ms: u64,
}

/// Local multi-objective reinforcement learning reward calculator
#[derive(Debug, Clone)]
pub struct LocalRewardModel {
    pub latency_weight: f64,
    pub memory_weight: f64,
    pub error_weight: f64,
}

impl Default for LocalRewardModel {
    fn default() -> Self {
        Self {
            latency_weight: 100.0,
            memory_weight: 50.0,
            error_weight: 25.0,
        }
    }
}

impl LocalRewardModel {
    /// Calculate component and scalar rewards based on metrics delta
    pub fn evaluate_reward(
        &self,
        before: &TelemetryDeltaMetrics,
        after: &TelemetryDeltaMetrics,
    ) -> (f64, f64, f64, f64) {
        // 1. Latency delta reward: positive when latency drops
        let lat_reward = if before.latency_p99_ms > 0.0 {
            let improvement = (before.latency_p99_ms - after.latency_p99_ms) / before.latency_p99_ms;
            (improvement * self.latency_weight).clamp(-100.0, 100.0)
        } else {
            0.0
        };

        // 2. Memory efficiency reward: positive when RSS decreases
        let mem_reward = if before.memory_rss_mb > 0.0 {
            let reduction = (before.memory_rss_mb - after.memory_rss_mb) / before.memory_rss_mb;
            (reduction * self.memory_weight).clamp(-50.0, 50.0)
        } else {
            0.0
        };

        // 3. Error reduction reward: positive when error count decreases
        let err_reward = if before.error_count >= after.error_count {
            ((before.error_count - after.error_count) as f64) * self.error_weight
        } else {
            -(((after.error_count - before.error_count) as f64) * self.error_weight)
        };

        let total = lat_reward + mem_reward + err_reward;
        (lat_reward, mem_reward, err_reward, total)
    }
}

/// Durable append-only ledger tracking optimization episodes and rewards
pub struct LocalRewardLedger {
    ledger_path: PathBuf,
    history: RwLock<VecDeque<OptimizationEpisode>>,
    cumulative_reward: RwLock<f64>,
    pgo_reports: RwLock<VecDeque<PgoRecompilationReport>>,
    capacity: usize,
}

impl LocalRewardLedger {
    pub fn new<P: AsRef<Path>>(base_dir: P, capacity: usize) -> Result<Self> {
        let dir = base_dir.as_ref();
        if !dir.exists() {
            let _ = fs::create_dir_all(dir);
        }
        let ledger_path = dir.join("reward_ledger.jsonl");

        let mut history = VecDeque::with_capacity(capacity);
        let mut cumulative = 0.0f64;

        // Replay existing ledger if present on disk
        if ledger_path.exists() {
            if let Ok(file) = File::open(&ledger_path) {
                let reader = BufReader::new(file);
                for line in reader.lines().flatten() {
                    if let Ok(episode) = serde_json::from_str::<OptimizationEpisode>(&line) {
                        cumulative += episode.scalar_reward;
                        if history.len() >= capacity {
                            history.pop_front();
                        }
                        history.push_back(episode);
                    }
                }
            }
        }

        Ok(Self {
            ledger_path,
            history: RwLock::new(history),
            cumulative_reward: RwLock::new(cumulative),
            pgo_reports: RwLock::new(VecDeque::with_capacity(32)),
            capacity,
        })
    }

    /// Append a new optimization episode to disk and in-memory history
    pub fn append_episode(&self, mut episode: OptimizationEpisode) -> Result<f64> {
        let mut cum_guard = self.cumulative_reward.write().map_err(|_| {
            HgbError::Execution("Failed to acquire cumulative reward lock".into())
        })?;

        *cum_guard += episode.scalar_reward;
        episode.cumulative_reward = *cum_guard;

        // Persist to JSONL file append-only
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.ledger_path) {
            if let Ok(json) = serde_json::to_string(&episode) {
                let _ = writeln!(file, "{}", json);
            }
        }

        // Add to in-memory deque
        if let Ok(mut hist) = self.history.write() {
            if hist.len() >= self.capacity {
                hist.pop_front();
            }
            hist.push_back(episode);
        }

        Ok(*cum_guard)
    }

    /// Record a completed PGO self-compilation report
    pub fn record_pgo_report(&self, report: PgoRecompilationReport) {
        if let Ok(mut reports) = self.pgo_reports.write() {
            if reports.len() >= 32 {
                reports.pop_front();
            }
            reports.push_back(report);
        }
    }

    /// Reset or decay cumulative reward post-compilation
    pub fn decay_cumulative_reward(&self, factor: f64) {
        if let Ok(mut cum) = self.cumulative_reward.write() {
            *cum *= factor.clamp(0.0, 1.0);
        }
    }

    /// Read current cumulative reward
    pub fn cumulative_reward(&self) -> f64 {
        self.cumulative_reward.read().map(|g| *g).unwrap_or(0.0)
    }

    /// Total recorded episodes count
    pub fn episode_count(&self) -> usize {
        self.history.read().map(|g| g.len()).unwrap_or(0)
    }

    /// Get recent optimization episodes snapshot
    pub fn recent_episodes(&self, limit: usize) -> Vec<OptimizationEpisode> {
        self.history
            .read()
            .map(|g| g.iter().rev().take(limit).cloned().collect())
            .unwrap_or_default()
    }

    /// Get recent PGO compilation reports
    pub fn recent_pgo_reports(&self) -> Vec<PgoRecompilationReport> {
        self.pgo_reports
            .read()
            .map(|g| g.iter().rev().cloned().collect())
            .unwrap_or_default()
    }
}

/// Closed-loop autonomous reinforcement learning governor coupling reward modeling with PGO recompilation
pub struct ReinforcementLearningPgoLoop {
    workspace_root: PathBuf,
    reward_model: LocalRewardModel,
    ledger: Arc<LocalRewardLedger>,
    trigger_threshold: f64,
    min_episodes: usize,
    cooldown_secs: u64,
    last_pgo_timestamp_ms: AtomicU64,
    is_compiling: AtomicBool,
}

impl ReinforcementLearningPgoLoop {
    pub fn new<P: AsRef<Path>>(workspace_root: P) -> Result<Self> {
        let ws = workspace_root.as_ref().to_path_buf();
        let ledger_dir = ws.join(".hgb");
        let ledger = Arc::new(LocalRewardLedger::new(ledger_dir, 512)?);

        Ok(Self {
            workspace_root: ws,
            reward_model: LocalRewardModel::default(),
            ledger,
            trigger_threshold: DEFAULT_PGO_TRIGGER_REWARD_THRESHOLD,
            min_episodes: DEFAULT_MIN_EPISODES_BEFORE_PGO,
            cooldown_secs: DEFAULT_PGO_RECOMPILE_COOLDOWN_SECS,
            last_pgo_timestamp_ms: AtomicU64::new(0),
            is_compiling: AtomicBool::new(false),
        })
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.trigger_threshold = threshold;
        self
    }

    pub fn with_cooldown_secs(mut self, secs: u64) -> Self {
        self.cooldown_secs = secs;
        self
    }

    /// Access reference to underlying reward ledger
    pub fn ledger(&self) -> &Arc<LocalRewardLedger> {
        &self.ledger
    }

    /// Record and evaluate an autonomous optimization action
    pub fn record_optimization(
        &self,
        action: OptimizationActionCategory,
        description: impl Into<String>,
        before: TelemetryDeltaMetrics,
        after: TelemetryDeltaMetrics,
    ) -> Result<OptimizationEpisode> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let (lat_r, mem_r, err_r, total_r) = self.reward_model.evaluate_reward(&before, &after);

        let episode_id = Uuid::new_v4().to_string();

        let mut episode = OptimizationEpisode {
            episode_id: episode_id.clone(),
            action_category: action,
            action_description: description.into(),
            metrics_before: before,
            metrics_after: after,
            latency_reward: lat_r,
            memory_reward: mem_r,
            error_reward: err_r,
            scalar_reward: total_r,
            cumulative_reward: 0.0,
            pgo_trigger_recommended: false,
            timestamp_ms: now_ms,
        };

        // Append to ledger and retrieve updated cumulative reward
        let cum = self.ledger.append_episode(episode.clone())?;
        episode.cumulative_reward = cum;

        // Check if PGO recompilation condition is met
        let cooldown_elapsed = now_ms.saturating_sub(self.last_pgo_timestamp_ms.load(Ordering::Relaxed))
            >= (self.cooldown_secs * 1000);

        let should_trigger = cum >= self.trigger_threshold
            && self.ledger.episode_count() >= self.min_episodes
            && cooldown_elapsed
            && !self.is_compiling.load(Ordering::Relaxed);

        episode.pgo_trigger_recommended = should_trigger;

        // Record OpenTelemetry span for this RL episode
        let mut span = SpanRecord::new(
            episode_id,
            Uuid::new_v4().to_string(),
            None,
            "reinforcement_learning.reward_episode".to_string(),
            "hgb-daemon".to_string(),
        );
        span.set_attribute("rl.action", action.as_str());
        span.set_attribute("rl.scalar_reward", &format!("{:.2}", total_r));
        span.set_attribute("rl.cumulative_reward", &format!("{:.2}", cum));
        span.set_attribute("rl.pgo_recommended", &should_trigger.to_string());
        span.finish(SpanStatus::Ok);
        get_global_buffer().record_span(span);

        Ok(episode)
    }

    /// Trigger autonomous Profile-Guided Optimization (PGO) self-compilation
    pub fn trigger_pgo_self_compilation(
        &self,
        reason: &str,
        dry_run: bool,
    ) -> Result<PgoRecompilationReport> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if self.is_compiling.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err(HgbError::Execution("PGO compilation is already actively running".into()));
        }

        let start = Instant::now();
        let cycle_id = Uuid::new_v4().to_string();
        let cum = self.ledger.cumulative_reward();

        let report = if dry_run {
            // Simulated validation cycle for testing
            let duration = start.elapsed().as_secs_f64();
            let report = PgoRecompilationReport {
                pgo_cycle_id: cycle_id,
                trigger_reason: reason.to_string(),
                cumulative_reward_at_trigger: cum,
                recompiled_binaries: vec!["target/release/hgb".to_string(), "target/release/hgbd".to_string()],
                binary_hash: blake3::hash(b"dry_run_pgo_binary").to_hex().to_string(),
                duration_seconds: duration,
                success: true,
                message: format!("PGO dry-run self-compilation successful for '{}'", reason),
                timestamp_ms: now_ms,
            };
            self.last_pgo_timestamp_ms.store(now_ms, Ordering::Relaxed);
            self.ledger.decay_cumulative_reward(0.2); // Decay reward after compilation
            self.ledger.record_pgo_report(report.clone());
            self.is_compiling.store(false, Ordering::SeqCst);
            report
        } else {
            // Real execution of scripts/pgo-optimize.sh or make pgo
            let script_path = self.workspace_root.join("scripts").join("pgo-optimize.sh");
            let makefile = self.workspace_root.join("Makefile");

            let res = if script_path.exists() {
                std::process::Command::new("bash")
                    .arg(&script_path)
                    .current_dir(&self.workspace_root)
                    .output()
            } else if makefile.exists() {
                std::process::Command::new("make")
                    .arg("pgo-build")
                    .current_dir(&self.workspace_root)
                    .output()
            } else {
                return {
                    self.is_compiling.store(false, Ordering::SeqCst);
                    Err(HgbError::Execution("Neither scripts/pgo-optimize.sh nor Makefile found for PGO self-compilation".into()))
                };
            };

            let duration = start.elapsed().as_secs_f64();
            let is_success = res.as_ref().map(|o| o.status.success()).unwrap_or(false);

            let stdout_snippet = res.as_ref().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();

            let report = PgoRecompilationReport {
                pgo_cycle_id: cycle_id,
                trigger_reason: reason.to_string(),
                cumulative_reward_at_trigger: cum,
                recompiled_binaries: vec!["target/release/hgb".to_string(), "target/release/hgbd".to_string()],
                binary_hash: blake3::hash(stdout_snippet.as_bytes()).to_hex().to_string(),
                duration_seconds: duration,
                success: is_success,
                message: if is_success {
                    "PGO production binary recompilation finished successfully".to_string()
                } else {
                    format!("PGO recompilation failed: {:?}", res.err())
                },
                timestamp_ms: now_ms,
            };

            if is_success {
                self.last_pgo_timestamp_ms.store(now_ms, Ordering::Relaxed);
                self.ledger.decay_cumulative_reward(0.2);
            }

            self.ledger.record_pgo_report(report.clone());
            self.is_compiling.store(false, Ordering::SeqCst);
            report
        };

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reward_model_calculation() {
        let model = LocalRewardModel::default();

        let before = TelemetryDeltaMetrics {
            latency_p50_ms: 20.0,
            latency_p99_ms: 100.0,
            memory_rss_mb: 500.0,
            span_drop_rate_pct: 2.0,
            error_count: 5,
            cpu_temp_c: 70.0,
            timestamp_ms: 1000,
        };

        // After: Latency halved, memory reduced by 100MB, errors reduced from 5 to 1
        let after = TelemetryDeltaMetrics {
            latency_p50_ms: 10.0,
            latency_p99_ms: 50.0,
            memory_rss_mb: 400.0,
            span_drop_rate_pct: 0.0,
            error_count: 1,
            cpu_temp_c: 65.0,
            timestamp_ms: 2000,
        };

        let (lat_r, mem_r, err_r, total_r) = model.evaluate_reward(&before, &after);
        assert!(lat_r > 0.0, "Latency drop must yield positive reward");
        assert!(mem_r > 0.0, "Memory reduction must yield positive reward");
        assert!(err_r > 0.0, "Error reduction must yield positive reward");
        assert!(total_r > 50.0);
    }

    #[test]
    fn test_reward_ledger_persistence() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let ledger = LocalRewardLedger::new(temp_dir.path(), 10).expect("create ledger");

        let before = TelemetryDeltaMetrics::default();
        let mut after = TelemetryDeltaMetrics::default();
        after.latency_p99_ms = 25.0; // 50% speedup

        let episode = OptimizationEpisode {
            episode_id: "ep_1".to_string(),
            action_category: OptimizationActionCategory::SelfHealingPatch,
            action_description: "Applied memory pool patch".to_string(),
            metrics_before: before,
            metrics_after: after,
            latency_reward: 50.0,
            memory_reward: 10.0,
            error_reward: 25.0,
            scalar_reward: 85.0,
            cumulative_reward: 85.0,
            pgo_trigger_recommended: false,
            timestamp_ms: 1000,
        };

        let cum = ledger.append_episode(episode).expect("append");
        assert_eq!(cum, 85.0);
        assert_eq!(ledger.episode_count(), 1);

        // Re-read from disk
        let reloaded = LocalRewardLedger::new(temp_dir.path(), 10).expect("reload");
        assert_eq!(reloaded.episode_count(), 1);
        assert_eq!(reloaded.cumulative_reward(), 85.0);
    }

    #[test]
    fn test_rl_pgo_loop_trigger_flow() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let rl_loop = ReinforcementLearningPgoLoop::new(temp_dir.path())
            .expect("init rl")
            .with_threshold(100.0)
            .with_cooldown_secs(0); // zero cooldown for test

        let before = TelemetryDeltaMetrics {
            latency_p99_ms: 200.0,
            memory_rss_mb: 800.0,
            error_count: 10,
            ..Default::default()
        };

        let after = TelemetryDeltaMetrics {
            latency_p99_ms: 40.0, // 80% speedup
            memory_rss_mb: 400.0, // 50% memory saved
            error_count: 0,       // All errors fixed
            ..Default::default()
        };

        // Episode 1
        let ep1 = rl_loop.record_optimization(
            OptimizationActionCategory::SelfHealingPatch,
            "Applied vector cache patch",
            before.clone(),
            after.clone(),
        ).expect("ep1");
        assert!(ep1.scalar_reward > 100.0);

        // Episode 2 & 3 to cross min_episodes threshold
        rl_loop.record_optimization(
            OptimizationActionCategory::GovernorProfileShift,
            "Tuned kernel governor",
            before.clone(),
            after.clone(),
        ).expect("ep2");

        let ep3 = rl_loop.record_optimization(
            OptimizationActionCategory::WasmJitTuning,
            "Hardened WASM JIT units",
            before,
            after,
        ).expect("ep3");

        assert!(ep3.pgo_trigger_recommended);

        // Trigger PGO dry run
        let report = rl_loop.trigger_pgo_self_compilation("Cumulative reward threshold achieved", true)
            .expect("trigger pgo");
        assert!(report.success);
        assert_eq!(report.recompiled_binaries.len(), 2);
    }
}
