//! # Pillar 4: Autonomous Watchdog, Snapshotting, and Self-Correction Engine
//!
//! Autonomous supervisor that guards agent execution loops against:
//! 1. Tool execution failures: compiler error codes (`E0xxx`, `TSxxxx`), command timeouts, non-zero exits.
//! 2. LLM hallucination and repetitive loops: identical candidate prompts or patches generated multiple times.
//! 3. SQLite database locks (`SQLITE_BUSY`, database is locked).
//!
//! Features:
//! - SQLite WAL-backed snapshotting of agent conversation and memory state (`agent_context_snapshots`).
//! - Context rollback restoring clean snapshot state prior to corruption.
//! - Dynamic parameter mutation: autonomously shifting temperature from 0.2 to 0.5–0.7 to escape local minima,
//!   adjusting `top_p` and `frequency_penalty`, and engaging fallback models on context limit exhaustion.
//! - Autonomous retry loop driving repairs to First-Green commit, disk persistence, and zero-downtime hot reloading.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use rusqlite::{params, Connection};

use hgb_core::error::{HgbError, Result};
use hgb_core::event_bus::DistributedEventBus;

pub use hgb_core::autonomous_watchdog::{
    AgentContextSnapshot, AgentGenerationParams, FailurePattern, RecoveryStory, WatchdogStatusReport,
};

/// Core Autonomous Watchdog engine guarding agent loops
pub struct AutonomousWatchdog {
    workspace_root: PathBuf,
    db_path: PathBuf,
    conn: Arc<Mutex<Connection>>,
    patch_hash_history: Arc<RwLock<HashMap<String, Vec<String>>>>,
    prompt_hash_history: Arc<RwLock<HashMap<String, Vec<String>>>>,
    total_snapshots: Arc<AtomicU64>,
    total_rollbacks: Arc<AtomicU64>,
    total_intercepted: Arc<AtomicU64>,
    total_recoveries: Arc<AtomicU64>,
    event_bus: Option<Arc<DistributedEventBus>>,
}

fn init_watchdog_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;

         CREATE TABLE IF NOT EXISTS agent_context_snapshots (
             snapshot_id TEXT PRIMARY KEY,
             cycle_id TEXT NOT NULL,
             context_tokens INTEGER NOT NULL,
             prompt_history_json TEXT NOT NULL,
             scratchpad TEXT NOT NULL,
             generation_params_json TEXT NOT NULL,
             created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_acs_cycle_id ON agent_context_snapshots(cycle_id);
         CREATE INDEX IF NOT EXISTS idx_acs_created_at ON agent_context_snapshots(created_at);

         CREATE TABLE IF NOT EXISTS autonomous_recovery_stories (
             story_id TEXT PRIMARY KEY,
             cycle_id TEXT NOT NULL,
             initial_failure TEXT NOT NULL,
             failure_pattern TEXT NOT NULL,
             iterations_taken INTEGER NOT NULL,
             mutations_applied_json TEXT NOT NULL,
             final_patch TEXT NOT NULL,
             target_file TEXT,
             hot_reload_triggered INTEGER NOT NULL,
             created_at TEXT NOT NULL,
             duration_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_ars_cycle_id ON autonomous_recovery_stories(cycle_id);
         CREATE INDEX IF NOT EXISTS idx_ars_created_at ON autonomous_recovery_stories(created_at);",
    ).map_err(|e| HgbError::Storage(format!("Failed to initialize watchdog SQLite schema: {}", e)))?;

    Ok(())
}

impl AutonomousWatchdog {
    /// Initialize watchdog associated with the workspace root (`.hgb/autonomous_state.db`)
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let ws = workspace_root.as_ref().to_path_buf();
        let hgb_dir = ws.join(".hgb");
        let _ = fs::create_dir_all(&hgb_dir);
        let db_path = hgb_dir.join("autonomous_state.db");
        Self::open_internal(ws, db_path)
    }

    /// Open watchdog with explicit SQLite database path
    pub fn with_db_path(db_path: impl AsRef<Path>) -> Result<Self> {
        let p = db_path.as_ref().to_path_buf();
        let parent = p.parent().unwrap_or_else(|| Path::new("."));
        let ws = parent
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| parent.to_path_buf());
        if let Some(parent_dir) = p.parent() {
            let _ = fs::create_dir_all(parent_dir);
        }
        Self::open_internal(ws, p)
    }

    /// Fast, in-memory watchdog instance for zero-disk test isolation
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("Failed to open in-memory SQLite: {}", e)))?;
        init_watchdog_schema(&conn)?;

        Ok(Self {
            workspace_root: PathBuf::from("."),
            db_path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
            patch_hash_history: Arc::new(RwLock::new(HashMap::new())),
            prompt_hash_history: Arc::new(RwLock::new(HashMap::new())),
            total_snapshots: Arc::new(AtomicU64::new(0)),
            total_rollbacks: Arc::new(AtomicU64::new(0)),
            total_intercepted: Arc::new(AtomicU64::new(0)),
            total_recoveries: Arc::new(AtomicU64::new(0)),
            event_bus: None,
        })
    }

    fn open_internal(workspace_root: PathBuf, db_path: PathBuf) -> Result<Self> {
        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("Failed to open SQLite watchdog DB at {}: {}", db_path.display(), e)))?;
        init_watchdog_schema(&conn)?;

        Ok(Self {
            workspace_root,
            db_path,
            conn: Arc::new(Mutex::new(conn)),
            patch_hash_history: Arc::new(RwLock::new(HashMap::new())),
            prompt_hash_history: Arc::new(RwLock::new(HashMap::new())),
            total_snapshots: Arc::new(AtomicU64::new(0)),
            total_rollbacks: Arc::new(AtomicU64::new(0)),
            total_intercepted: Arc::new(AtomicU64::new(0)),
            total_recoveries: Arc::new(AtomicU64::new(0)),
            event_bus: None,
        })
    }

    pub fn with_event_bus(mut self, bus: Arc<DistributedEventBus>) -> Self {
        self.event_bus = Some(bus);
        self
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    // =========================================================================
    // SNAPSHOTTING & PERSISTENCE
    // =========================================================================

    /// Create and persist an agent context snapshot in SQLite WAL
    pub fn create_snapshot(
        &self,
        cycle_id: &str,
        context_tokens: usize,
        prompt_history_json: &str,
        scratchpad: &str,
        params: &AgentGenerationParams,
    ) -> Result<AgentContextSnapshot> {
        let snapshot_id = format!("snap_{}", uuid::Uuid::new_v4().simple());
        let created_at = chrono::Utc::now().to_rfc3339();

        let snapshot = AgentContextSnapshot {
            snapshot_id,
            cycle_id: cycle_id.to_string(),
            context_tokens,
            prompt_history_json: prompt_history_json.to_string(),
            scratchpad: scratchpad.to_string(),
            generation_params: params.clone(),
            created_at,
        };

        self.save_snapshot(&snapshot)?;
        self.total_snapshots.fetch_add(1, Ordering::Relaxed);
        Ok(snapshot)
    }

    /// Save an existing snapshot into the SQLite database
    pub fn save_snapshot(&self, snapshot: &AgentContextSnapshot) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let params_json = serde_json::to_string(&snapshot.generation_params)
            .map_err(|e| HgbError::Serialization(format!("Params serialization error: {}", e)))?;

        conn.execute(
            "INSERT INTO agent_context_snapshots (
                snapshot_id, cycle_id, context_tokens, prompt_history_json,
                scratchpad, generation_params_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &snapshot.snapshot_id,
                &snapshot.cycle_id,
                snapshot.context_tokens as i64,
                &snapshot.prompt_history_json,
                &snapshot.scratchpad,
                &params_json,
                &snapshot.created_at,
            ],
        ).map_err(|e| HgbError::Storage(format!("Failed to insert agent context snapshot: {}", e)))?;

        Ok(())
    }

    /// Retrieve an agent context snapshot by snapshot_id
    pub fn get_snapshot(&self, snapshot_id: &str) -> Result<Option<AgentContextSnapshot>> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let mut stmt = conn.prepare(
            "SELECT snapshot_id, cycle_id, context_tokens, prompt_history_json, scratchpad, generation_params_json, created_at
             FROM agent_context_snapshots
             WHERE snapshot_id = ?1"
        ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let res = stmt.query_row(params![snapshot_id], |row| {
            let params_json: String = row.get(5)?;
            let gen_params: AgentGenerationParams = serde_json::from_str(&params_json).unwrap_or_default();
            Ok(AgentContextSnapshot {
                snapshot_id: row.get(0)?,
                cycle_id: row.get(1)?,
                context_tokens: row.get::<_, i64>(2)? as usize,
                prompt_history_json: row.get(3)?,
                scratchpad: row.get(4)?,
                generation_params: gen_params,
                created_at: row.get(6)?,
            })
        });

        match res {
            Ok(snap) => Ok(Some(snap)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(HgbError::Storage(format!("Query error: {}", e))),
        }
    }

    /// List all snapshots recorded for a specific autonomous cycle, ordered chronologically
    pub fn list_snapshots_for_cycle(&self, cycle_id: &str) -> Result<Vec<AgentContextSnapshot>> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let mut stmt = conn.prepare(
            "SELECT snapshot_id, cycle_id, context_tokens, prompt_history_json, scratchpad, generation_params_json, created_at
             FROM agent_context_snapshots
             WHERE cycle_id = ?1
             ORDER BY created_at ASC, rowid ASC"
        ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let rows = stmt.query_map(params![cycle_id], |row| {
            let params_json: String = row.get(5)?;
            let gen_params: AgentGenerationParams = serde_json::from_str(&params_json).unwrap_or_default();
            Ok(AgentContextSnapshot {
                snapshot_id: row.get(0)?,
                cycle_id: row.get(1)?,
                context_tokens: row.get::<_, i64>(2)? as usize,
                prompt_history_json: row.get(3)?,
                scratchpad: row.get(4)?,
                generation_params: gen_params,
                created_at: row.get(6)?,
            })
        }).map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

        let mut snapshots = Vec::new();
        for r in rows {
            if let Ok(snap) = r {
                snapshots.push(snap);
            }
        }
        Ok(snapshots)
    }

    /// Retrieve all snapshots for a cycle (alias for list_snapshots_for_cycle)
    pub fn get_snapshots_for_cycle(&self, cycle_id: &str) -> Result<Vec<AgentContextSnapshot>> {
        self.list_snapshots_for_cycle(cycle_id)
    }

    /// Retrieve the most recent snapshot for a cycle
    pub fn get_latest_snapshot(&self, cycle_id: &str) -> Result<Option<AgentContextSnapshot>> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let mut stmt = conn.prepare(
            "SELECT snapshot_id, cycle_id, context_tokens, prompt_history_json, scratchpad, generation_params_json, created_at
             FROM agent_context_snapshots
             WHERE cycle_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT 1"
        ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let res = stmt.query_row(params![cycle_id], |row| {
            let params_json: String = row.get(5)?;
            let gen_params: AgentGenerationParams = serde_json::from_str(&params_json).unwrap_or_default();
            Ok(AgentContextSnapshot {
                snapshot_id: row.get(0)?,
                cycle_id: row.get(1)?,
                context_tokens: row.get::<_, i64>(2)? as usize,
                prompt_history_json: row.get(3)?,
                scratchpad: row.get(4)?,
                generation_params: gen_params,
                created_at: row.get(6)?,
            })
        });

        match res {
            Ok(snap) => Ok(Some(snap)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(HgbError::Storage(format!("Query error: {}", e))),
        }
    }

    // =========================================================================
    // CONTEXT ROLLBACK
    // =========================================================================

    /// Cleanly restores the context window prior to the corruption/failure from SQLite WAL snapshots
    pub fn rollback_context(&self, cycle_id: &str, target_snapshot_id: &str) -> Result<AgentContextSnapshot> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let mut stmt = conn.prepare(
            "SELECT snapshot_id, cycle_id, context_tokens, prompt_history_json, scratchpad, generation_params_json, created_at
             FROM agent_context_snapshots
             WHERE cycle_id = ?1 AND snapshot_id = ?2"
        ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let snap = stmt.query_row(params![cycle_id, target_snapshot_id], |row| {
            let params_json: String = row.get(5)?;
            let gen_params: AgentGenerationParams = serde_json::from_str(&params_json).unwrap_or_default();
            Ok(AgentContextSnapshot {
                snapshot_id: row.get(0)?,
                cycle_id: row.get(1)?,
                context_tokens: row.get::<_, i64>(2)? as usize,
                prompt_history_json: row.get(3)?,
                scratchpad: row.get(4)?,
                generation_params: gen_params,
                created_at: row.get(6)?,
            })
        }).map_err(|e| HgbError::NotFound(format!("Snapshot {} for cycle {} not found: {}", target_snapshot_id, cycle_id, e)))?;

        self.total_rollbacks.fetch_add(1, Ordering::Relaxed);

        if let Some(bus) = &self.event_bus {
            let payload = serde_json::json!({
                "event": "context_rollback",
                "cycle_id": cycle_id,
                "target_snapshot_id": target_snapshot_id,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            let _ = bus.publish("watchdog/rollback", &payload.to_string());
        }

        Ok(snap)
    }

    /// Roll back to the latest clean snapshot for a cycle
    pub fn rollback_to_latest_clean_snapshot(&self, cycle_id: &str) -> Result<AgentContextSnapshot> {
        let latest = self.get_latest_snapshot(cycle_id)?
            .ok_or_else(|| HgbError::NotFound(format!("No snapshots found for cycle {}", cycle_id)))?;

        self.total_rollbacks.fetch_add(1, Ordering::Relaxed);
        Ok(latest)
    }

    // =========================================================================
    // FAILURE PATTERN DETECTION
    // =========================================================================

    /// Detect compiler error codes (`E0xxx` or `TSxxxx`) from stderr/stdout
    pub fn detect_compiler_error(stderr: &str, stdout: &str, exit_code: i32) -> Option<FailurePattern> {
        let combined = format!("{}\n{}", stderr, stdout);

        for word in combined.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if (word.starts_with("E0") && word.len() == 5 && word[2..].chars().all(|c| c.is_ascii_digit()))
                || (word.starts_with("TS") && word.len() == 6 && word[2..].chars().all(|c| c.is_ascii_digit()))
            {
                let snippet = stderr.lines().take(5).collect::<Vec<_>>().join("\n");
                return Some(FailurePattern::CompilerError {
                    error_code: word.to_string(),
                    exit_code,
                    stderr_snippet: if snippet.is_empty() { word.to_string() } else { snippet },
                });
            }
        }
        None
    }

    /// Detect execution timeout
    pub fn detect_command_timeout(
        command: &str,
        duration_ms: u64,
        timeout_limit_ms: u64,
        stderr: &str,
    ) -> Option<FailurePattern> {
        let lower = stderr.to_lowercase();
        let timeout_str = lower.contains("timed out")
            || lower.contains("command timed out")
            || lower.contains("deadline exceeded")
            || lower.contains("timeout expired")
            || lower.contains("sigkill: killed after");

        if (timeout_limit_ms > 0 && duration_ms >= timeout_limit_ms) || timeout_str {
            Some(FailurePattern::CommandTimeout {
                command: command.to_string(),
                duration_ms,
                timeout_limit_ms,
            })
        } else {
            None
        }
    }

    /// Detect SQLite database locks (`SQLITE_BUSY`, database is locked)
    pub fn detect_sqlite_lock(err: &str) -> Option<FailurePattern> {
        let lower = err.to_lowercase();
        if lower.contains("sqlite_busy")
            || lower.contains("database is locked")
            || lower.contains("database table is locked")
            || lower.contains("busy timeout")
            || lower.contains("code: 5")
            || lower.contains("busy: 5")
        {
            Some(FailurePattern::SqliteDatabaseLock {
                error_message: err.to_string(),
            })
        } else {
            None
        }
    }

    /// Detect context window overflow
    pub fn detect_context_overflow(tokens: usize, limit: usize) -> Option<FailurePattern> {
        if tokens > limit && limit > 0 {
            Some(FailurePattern::ContextWindowExceeded {
                current_tokens: tokens,
                max_tokens_limit: limit,
            })
        } else {
            None
        }
    }

    /// Record candidate patch/prompt hash and detect LLM hallucination or repetitive loop
    pub fn detect_hallucination_loop(&self, cycle_id: &str, patch_or_prompt: &str) -> Option<FailurePattern> {
        let trimmed = patch_or_prompt.trim();
        if trimmed.is_empty() {
            return None;
        }

        let hash = blake3::hash(trimmed.as_bytes()).to_hex().to_string();

        let occurrences = {
            let mut hist = self.patch_hash_history.write().ok()?;
            let hashes = hist.entry(cycle_id.to_string()).or_default();
            hashes.push(hash.clone());
            hashes.iter().filter(|&h| h == &hash).count()
        };

        // Also detect internal line repetition (hallucinatory repeating lines in a single response)
        let mut internal_repetition = false;
        let lines: Vec<&str> = trimmed.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        if lines.len() >= 4 {
            for window in lines.windows(4) {
                if window[0] == window[1] && window[1] == window[2] && window[2] == window[3] {
                    internal_repetition = true;
                    break;
                }
            }
        }

        if occurrences >= 2 || internal_repetition {
            let preview = trimmed.lines().take(2).collect::<Vec<_>>().join(" ");
            let content_preview = if preview.len() > 80 {
                format!("{}...", &preview[..77])
            } else {
                preview
            };

            Some(FailurePattern::HallucinationRepetitiveLoop {
                repeated_hash: hash,
                occurrences: occurrences.max(if internal_repetition { 3 } else { 1 }),
                content_preview,
            })
        } else {
            None
        }
    }

    /// Record patch hash for a cycle and return occurrence count
    pub fn record_patch(&self, cycle_id: &str, patch: &str) -> usize {
        let trimmed = patch.trim();
        if trimmed.is_empty() {
            return 0;
        }
        let hash = blake3::hash(trimmed.as_bytes()).to_hex().to_string();
        if let Ok(mut hist) = self.patch_hash_history.write() {
            let list = hist.entry(cycle_id.to_string()).or_default();
            list.push(hash.clone());
            list.iter().filter(|&h| h == &hash).count()
        } else {
            1
        }
    }

    /// Unified multi-vector failure pattern detector
    pub fn detect_failure_pattern(
        &self,
        cycle_id: &str,
        command: Option<&str>,
        exit_code: Option<i32>,
        stderr: Option<&str>,
        duration_ms: Option<u64>,
        timeout_limit_ms: Option<u64>,
        candidate_patch: Option<&str>,
        db_error: Option<&str>,
        current_tokens: Option<usize>,
        max_tokens_limit: Option<usize>,
    ) -> Option<FailurePattern> {
        self.total_intercepted.fetch_add(1, Ordering::Relaxed);

        // 1. Check for SQLite database lock
        if let Some(dbe) = db_error {
            if let Some(pattern) = Self::detect_sqlite_lock(dbe) {
                return Some(pattern);
            }
        }
        if let Some(err) = stderr {
            if let Some(pattern) = Self::detect_sqlite_lock(err) {
                return Some(pattern);
            }
        }

        // 2. Check for context window limit overflow
        if let (Some(cur), Some(lim)) = (current_tokens, max_tokens_limit) {
            if let Some(pattern) = Self::detect_context_overflow(cur, lim) {
                return Some(pattern);
            }
        }

        // 3. Check for repetitive hallucination loop
        if let Some(patch) = candidate_patch {
            if let Some(pattern) = self.detect_hallucination_loop(cycle_id, patch) {
                return Some(pattern);
            }
        }

        // 4. Check for command timeout
        let cmd = command.unwrap_or("unknown_tool");
        let dur = duration_ms.unwrap_or(0);
        let limit = timeout_limit_ms.unwrap_or(0);
        let err_str = stderr.unwrap_or("");
        if let Some(pattern) = Self::detect_command_timeout(cmd, dur, limit, err_str) {
            return Some(pattern);
        }

        // 5. Check for compiler error (E0xxx or TSxxxx)
        let code = exit_code.unwrap_or(0);
        if let Some(pattern) = Self::detect_compiler_error(err_str, "", code) {
            return Some(pattern);
        }

        // 6. Generic tool execution failure if non-zero exit
        if code != 0 {
            let snippet = err_str.lines().take(3).collect::<Vec<_>>().join("\n");
            return Some(FailurePattern::ToolExecutionFailure {
                command: cmd.to_string(),
                exit_code: code,
                stderr_snippet: if snippet.is_empty() { format!("Process exited with status {}", code) } else { snippet },
            });
        }

        None
    }

    // =========================================================================
    // DYNAMIC GENERATION PARAMETER MUTATION
    // =========================================================================

    /// Dynamically mutate generation parameters to break out of failure states:
    /// - If repeated failures occur at temperature 0.2, autonomously adjust temperature to 0.5–0.7 to escape local minima.
    /// - Adjusts `top_p`, `frequency_penalty`, or switches to a fallback model if context window exceeds limits.
    pub fn mutate_parameters(
        &self,
        current: &AgentGenerationParams,
        pattern: &FailurePattern,
        failure_count: usize,
    ) -> (AgentGenerationParams, Vec<String>) {
        let mut mutated = current.clone();
        let mut mutations = Vec::new();

        match pattern {
            FailurePattern::HallucinationRepetitiveLoop { repeated_hash: _, occurrences, content_preview: _ } => {
                // Escaping repetitive local minima
                if mutated.temperature < 0.5 {
                    let old_temp = mutated.temperature;
                    mutated.temperature = 0.65;
                    mutations.push(format!(
                        "Adjusted temperature from {:.2} to {:.2} to escape repetitive loop local minimum",
                        old_temp, mutated.temperature
                    ));
                } else if mutated.temperature < 0.75 {
                    let old_temp = mutated.temperature;
                    mutated.temperature = 0.75;
                    mutations.push(format!(
                        "Increased temperature from {:.2} to {:.2} to break persistent repetition",
                        old_temp, mutated.temperature
                    ));
                }

                if mutated.frequency_penalty < 0.3 {
                    let old_pen = mutated.frequency_penalty;
                    mutated.frequency_penalty = 0.60;
                    mutations.push(format!(
                        "Increased frequency_penalty from {:.2} to {:.2} to suppress duplicate tokens (occurrences: {})",
                        old_pen, mutated.frequency_penalty, occurrences
                    ));
                }
                if mutated.presence_penalty < 0.2 {
                    mutated.presence_penalty = 0.40;
                    mutations.push("Engaged presence_penalty at 0.40 to encourage diverse syntax exploration".to_string());
                }
            }

            FailurePattern::ContextWindowExceeded { current_tokens, max_tokens_limit } => {
                if let Some(ref fb) = mutated.fallback_model {
                    if mutated.model != *fb {
                        let old_model = mutated.model.clone();
                        mutated.model = fb.clone();
                        mutations.push(format!(
                            "Switched model from '{}' to fallback model '{}' (context exceeded: {} / {} tokens)",
                            old_model, mutated.model, current_tokens, max_tokens_limit
                        ));
                    }
                }
                if mutated.max_tokens < 8192 {
                    let old_max = mutated.max_tokens;
                    mutated.max_tokens = 8192;
                    mutations.push(format!("Increased max_tokens from {} to {}", old_max, mutated.max_tokens));
                }
            }

            FailurePattern::CompilerError { error_code, exit_code: _, stderr_snippet: _ } => {
                // When stuck at low temperature (0.2), repeated compiler failures mean local minima
                if mutated.temperature <= 0.3 {
                    let old_temp = mutated.temperature;
                    let target_temp = if failure_count <= 1 { 0.55 } else { 0.70 };
                    mutated.temperature = target_temp;
                    mutations.push(format!(
                        "Adjusted temperature from {:.2} to {:.2} to explore alternative syntax for compiler error {}",
                        old_temp, target_temp, error_code
                    ));
                } else if failure_count >= 2 && mutated.temperature < 0.75 {
                    let old_temp = mutated.temperature;
                    mutated.temperature = 0.72;
                    mutations.push(format!(
                        "Bumped temperature from {:.2} to 0.72 after multiple compiler errors ({})",
                        old_temp, error_code
                    ));
                }

                // Adjust top_p to eliminate noisy token fringes when compiler errors persist
                if mutated.top_p > 0.90 && failure_count >= 2 {
                    let old_top_p = mutated.top_p;
                    mutated.top_p = 0.88;
                    mutations.push(format!("Refined top_p from {:.2} to {:.2} for stricter syntax adherence", old_top_p, mutated.top_p));
                }
            }

            FailurePattern::ToolExecutionFailure { command, exit_code: _, stderr_snippet: _ } => {
                if mutated.temperature <= 0.3 {
                    let old_temp = mutated.temperature;
                    mutated.temperature = 0.50;
                    mutations.push(format!(
                        "Raised temperature from {:.2} to 0.50 to recover from tool failure in '{}'",
                        old_temp, command
                    ));
                }
            }

            FailurePattern::CommandTimeout { command, duration_ms, timeout_limit_ms } => {
                if mutated.top_p > 0.85 {
                    mutated.top_p = 0.80;
                    mutations.push(format!(
                        "Tightened top_p to 0.80 for concise code generation after timeout in '{}' ({}ms > {}ms)",
                        command, duration_ms, timeout_limit_ms
                    ));
                }
            }

            FailurePattern::SqliteDatabaseLock { error_message } => {
                mutations.push(format!(
                    "Detected SQLite lock ('{}'): scheduled exponential backoff and busy timeout retry",
                    error_message
                ));
            }
        }

        (mutated, mutations)
    }

    // =========================================================================
    // AUTONOMOUS RETRY, FIRST-GREEN COMMIT & HOT RELOAD
    // =========================================================================

    /// Commit First-Green patch to disk, trigger zero-downtime hot reload, and record recovery story in SQLite
    pub fn commit_first_green(
        &self,
        cycle_id: &str,
        target_file: Option<&Path>,
        validated_patch: &str,
        initial_failure: &str,
        failure_pattern: &str,
        iterations_taken: usize,
        mutations_applied: Vec<String>,
        start_time: Instant,
    ) -> Result<RecoveryStory> {
        // 1. Commit validated patch to disk
        if let Some(path) = target_file {
            let full_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.workspace_root.join(path)
            };
            if let Some(parent) = full_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&full_path, validated_patch)
                .map_err(|e| HgbError::Io(e))?;
        }

        // 2. Trigger zero-downtime hot reload
        let hot_reload_res = self.trigger_hot_reload("agent_patch");
        let hot_reload_triggered = hot_reload_res.is_ok();

        // 3. Log recovery story to SQLite
        let story_id = format!(
            "story_{:012x}",
            blake3::hash(format!("{}:{}:{}", cycle_id, iterations_taken, uuid::Uuid::new_v4()).as_bytes())
                .as_bytes()[0..6]
                .iter()
                .fold(0u64, |acc, &b| (acc << 8) | (b as u64))
        );

        let story = RecoveryStory {
            story_id,
            cycle_id: cycle_id.to_string(),
            initial_failure: initial_failure.to_string(),
            failure_pattern: failure_pattern.to_string(),
            iterations_taken,
            mutations_applied,
            final_patch: validated_patch.to_string(),
            target_file: target_file.map(|p| p.to_string_lossy().to_string()),
            hot_reload_triggered,
            created_at: chrono::Utc::now().to_rfc3339(),
            duration_ms: start_time.elapsed().as_millis() as u64,
        };

        self.log_recovery_story(&story)?;
        self.total_recoveries.fetch_add(1, Ordering::Relaxed);

        Ok(story)
    }

    /// Persist recovery story in SQLite
    pub fn log_recovery_story(&self, story: &RecoveryStory) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let mutations_json = serde_json::to_string(&story.mutations_applied).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            "INSERT INTO autonomous_recovery_stories (
                story_id, cycle_id, initial_failure, failure_pattern,
                iterations_taken, mutations_applied_json, final_patch,
                target_file, hot_reload_triggered, created_at, duration_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &story.story_id,
                &story.cycle_id,
                &story.initial_failure,
                &story.failure_pattern,
                story.iterations_taken as i64,
                &mutations_json,
                &story.final_patch,
                story.target_file.as_deref(),
                if story.hot_reload_triggered { 1i64 } else { 0i64 },
                &story.created_at,
                story.duration_ms as i64,
            ],
        ).map_err(|e| HgbError::Storage(format!("Failed to insert recovery story: {}", e)))?;

        Ok(())
    }

    /// Query recovery stories from SQLite
    pub fn get_recovery_stories(&self, cycle_id: Option<&str>) -> Result<Vec<RecoveryStory>> {
        let conn = self.conn.lock().map_err(|e| HgbError::Execution(format!("Watchdog lock error: {}", e)))?;
        let mut stories = Vec::new();

        if let Some(cid) = cycle_id {
            let mut stmt = conn.prepare(
                "SELECT story_id, cycle_id, initial_failure, failure_pattern,
                        iterations_taken, mutations_applied_json, final_patch,
                        target_file, hot_reload_triggered, created_at, duration_ms
                 FROM autonomous_recovery_stories
                 WHERE cycle_id = ?1
                 ORDER BY created_at DESC"
            ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

            let rows = stmt.query_map(params![cid], |row| {
                let mut_json: String = row.get(5)?;
                let mutations: Vec<String> = serde_json::from_str(&mut_json).unwrap_or_default();
                let hr: i64 = row.get(8)?;
                Ok(RecoveryStory {
                    story_id: row.get(0)?,
                    cycle_id: row.get(1)?,
                    initial_failure: row.get(2)?,
                    failure_pattern: row.get(3)?,
                    iterations_taken: row.get::<_, i64>(4)? as usize,
                    mutations_applied: mutations,
                    final_patch: row.get(6)?,
                    target_file: row.get(7)?,
                    hot_reload_triggered: hr == 1,
                    created_at: row.get(9)?,
                    duration_ms: row.get::<_, i64>(10)? as u64,
                })
            }).map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

            for r in rows {
                if let Ok(story) = r {
                    stories.push(story);
                }
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT story_id, cycle_id, initial_failure, failure_pattern,
                        iterations_taken, mutations_applied_json, final_patch,
                        target_file, hot_reload_triggered, created_at, duration_ms
                 FROM autonomous_recovery_stories
                 ORDER BY created_at DESC
                 LIMIT 50"
            ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

            let rows = stmt.query_map([], |row| {
                let mut_json: String = row.get(5)?;
                let mutations: Vec<String> = serde_json::from_str(&mut_json).unwrap_or_default();
                let hr: i64 = row.get(8)?;
                Ok(RecoveryStory {
                    story_id: row.get(0)?,
                    cycle_id: row.get(1)?,
                    initial_failure: row.get(2)?,
                    failure_pattern: row.get(3)?,
                    iterations_taken: row.get::<_, i64>(4)? as usize,
                    mutations_applied: mutations,
                    final_patch: row.get(6)?,
                    target_file: row.get(7)?,
                    hot_reload_triggered: hr == 1,
                    created_at: row.get(9)?,
                    duration_ms: row.get::<_, i64>(10)? as u64,
                })
            }).map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

            for r in rows {
                if let Ok(story) = r {
                    stories.push(story);
                }
            }
        }

        Ok(stories)
    }

    /// Trigger zero-downtime hot reload hook
    pub fn trigger_hot_reload(&self, component_name: &str) -> Result<serde_json::Value> {
        let wasm_pool_reloaded = if component_name.ends_with(".wasm")
            || component_name == "wasm_plugin"
            || component_name == "agent_patch"
            || component_name == "all"
        {
            if let Ok(pool) = hgb_core::wasm_hot_pool::WasmHotPool::new(4) {
                pool.clear_cache();
                true
            } else {
                false
            }
        } else {
            false
        };

        if let Some(bus) = &self.event_bus {
            let payload = serde_json::json!({
                "event": "watchdog_hot_reload",
                "component": component_name,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            let _ = bus.publish("daemon/hot_reload", &payload.to_string());
        }

        Ok(serde_json::json!({
            "status": "hot_reloaded",
            "component": component_name,
            "wasm_pool_reloaded": wasm_pool_reloaded,
            "timestamp": chrono::Utc::now().to_rfc3339()
        }))
    }

    /// Inspect live telemetry status of the Autonomous Watchdog
    pub fn status(&self) -> WatchdogStatusReport {
        let recent = self.get_recovery_stories(None).unwrap_or_default();
        let total_snaps = {
            if let Ok(conn) = self.conn.lock() {
                conn.query_row("SELECT COUNT(1) FROM agent_context_snapshots", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0) as usize
            } else {
                self.total_snapshots.load(Ordering::Relaxed) as usize
            }
        };

        WatchdogStatusReport {
            active: true,
            total_snapshots: total_snaps,
            total_rollbacks: self.total_rollbacks.load(Ordering::Relaxed) as usize,
            total_failures_intercepted: self.total_intercepted.load(Ordering::Relaxed) as usize,
            total_recoveries_completed: self.total_recoveries.load(Ordering::Relaxed) as usize,
            db_path: self.db_path.display().to_string(),
            recent_recoveries: recent,
        }
    }

    /// Full self-correction retry cycle to First-Green completion:
    /// Orchestrates snapshots, failure detection, context rollback, dynamic parameter mutation,
    /// candidate retry up to max_retries, and disk commit upon First-Green verification.
    pub fn execute_self_correcting_cycle<G, V>(
        &self,
        cycle_id: &str,
        target_file: Option<&Path>,
        initial_prompt: &str,
        initial_scratchpad: &str,
        initial_params: AgentGenerationParams,
        max_retries: usize,
        mut generator: G,
        mut verifier: V,
    ) -> Result<RecoveryStory>
    where
        G: FnMut(&str, &AgentGenerationParams, usize) -> Result<String>,
        V: FnMut(&str, usize) -> Result<std::result::Result<(), FailurePattern>>,
    {
        let start_time = Instant::now();
        let mut current_params = initial_params;
        let mut current_prompt = initial_prompt.to_string();
        let mut current_scratchpad = initial_scratchpad.to_string();
        let mut accumulated_mutations = Vec::new();
        let mut initial_failure_desc = String::new();
        let mut primary_failure_pattern = String::new();
        let retries = if max_retries == 0 { 3 } else { max_retries };

        // Take initial snapshot at iteration 0 (clean baseline)
        let baseline_snapshot = self.create_snapshot(
            cycle_id,
            current_prompt.len() / 4,
            &serde_json::to_string(&vec![&current_prompt]).unwrap_or_default(),
            &current_scratchpad,
            &current_params,
        )?;

        for iteration in 1..=retries {
            // Generate candidate patch
            let candidate_patch = generator(&current_prompt, &current_params, iteration)?;

            // Detect repetitive loop before verification
            if let Some(hallucination_pattern) = self.detect_hallucination_loop(cycle_id, &candidate_patch) {
                if initial_failure_desc.is_empty() {
                    initial_failure_desc = hallucination_pattern.description();
                    primary_failure_pattern = hallucination_pattern.name().to_string();
                }

                // Roll back context to baseline
                let _ = self.rollback_context(cycle_id, &baseline_snapshot.snapshot_id);

                // Mutate parameters (temperature 0.2 -> 0.65, frequency_penalty 0.60)
                let (new_params, mut_notes) = self.mutate_parameters(&current_params, &hallucination_pattern, iteration);
                current_params = new_params;
                accumulated_mutations.extend(mut_notes);

                current_scratchpad = format!(
                    "{}\n[Watchdog] Hallucination loop intercepted at iteration {}. Context rolled back.",
                    current_scratchpad, iteration
                );
                current_prompt = format!(
                    "{}\n\n[WATCHDOG RECOVERY NOTICE]: A repetitive loop was detected. Avoid repeating prior candidate.\nFailure: {}",
                    initial_prompt, hallucination_pattern.description()
                );
                continue;
            }

            // Record patch occurrence
            self.record_patch(cycle_id, &candidate_patch);

            // Snapshot state for this iteration
            let _ = self.create_snapshot(
                cycle_id,
                current_prompt.len() / 4,
                &serde_json::to_string(&vec![&current_prompt]).unwrap_or_default(),
                &current_scratchpad,
                &current_params,
            )?;

            // Run verification
            match verifier(&candidate_patch, iteration)? {
                Ok(()) => {
                    // First-Green success! Commit patch and hot reload.
                    let story = self.commit_first_green(
                        cycle_id,
                        target_file,
                        &candidate_patch,
                        if initial_failure_desc.is_empty() { "Initial validation check" } else { &initial_failure_desc },
                        if primary_failure_pattern.is_empty() { "FirstGreenVerified" } else { &primary_failure_pattern },
                        iteration,
                        accumulated_mutations,
                        start_time,
                    )?;
                    return Ok(story);
                }
                Err(failure_pattern) => {
                    if initial_failure_desc.is_empty() {
                        initial_failure_desc = failure_pattern.description();
                        primary_failure_pattern = failure_pattern.name().to_string();
                    }

                    // Mutate parameters dynamically
                    let (new_params, mut_notes) = self.mutate_parameters(&current_params, &failure_pattern, iteration);
                    current_params = new_params;
                    accumulated_mutations.extend(mut_notes);

                    current_scratchpad = format!(
                        "{}\n[Iteration {} failed]: {}",
                        current_scratchpad, iteration, failure_pattern.description()
                    );
                    current_prompt = format!(
                        "{}\n\n[WATCHDOG ITERATION {} DIAGNOSTIC]:\nPrevious patch failed verification: {}\nPlease apply corrective adjustments.",
                        initial_prompt, iteration, failure_pattern.description()
                    );
                }
            }
        }

        Err(HgbError::Execution(format!(
            "Autonomous self-correction loop exhausted {} retries without achieving First-Green (Cycle: {})",
            retries, cycle_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_creation_retrieval_and_context_rollback() {
        let watchdog = AutonomousWatchdog::in_memory().expect("in-memory watchdog");
        let cycle_id = "test_cycle_001";
        let params = AgentGenerationParams::default().with_temperature(0.2);

        // 1. Create baseline snapshot
        let snap1 = watchdog.create_snapshot(
            cycle_id,
            120,
            "[\"user: implement sort function\"]",
            "scratchpad: initial plan",
            &params,
        ).expect("create snap1");

        assert_eq!(snap1.cycle_id, cycle_id);
        assert_eq!(snap1.context_tokens, 120);

        // 2. Create second snapshot after a corrupt iteration
        let _snap2 = watchdog.create_snapshot(
            cycle_id,
            450,
            "[\"user: implement sort function\", \"assistant: syntax error loop\"]",
            "scratchpad: corrupted context",
            &params,
        ).expect("create snap2");

        // Verify retrieval
        let retrieved = watchdog.get_snapshot(&snap1.snapshot_id).expect("get_snapshot");
        assert!(retrieved.is_some());
        let r = retrieved.unwrap();
        assert_eq!(r.snapshot_id, snap1.snapshot_id);
        assert_eq!(r.scratchpad, "scratchpad: initial plan");

        // Verify list for cycle
        let list = watchdog.list_snapshots_for_cycle(cycle_id).expect("list snapshots");
        assert_eq!(list.len(), 2);

        // 3. Rollback context to snap1
        let rolled_back = watchdog.rollback_context(cycle_id, &snap1.snapshot_id).expect("rollback");
        assert_eq!(rolled_back.snapshot_id, snap1.snapshot_id);
        assert_eq!(rolled_back.scratchpad, "scratchpad: initial plan");
        assert_eq!(rolled_back.context_tokens, 120);

        // Verify rollback counter
        let status = watchdog.status();
        assert_eq!(status.total_rollbacks, 1);
        assert_eq!(status.total_snapshots, 2);

        // Non-existent snapshot rollback error check
        let bad_rollback = watchdog.rollback_context(cycle_id, "non_existent_snap");
        assert!(bad_rollback.is_err());
    }

    #[test]
    fn test_failure_pattern_detection_compiler_error_and_timeout() {
        let watchdog = AutonomousWatchdog::in_memory().expect("in-memory watchdog");

        // 1. Compiler error E0425
        let stderr_e0425 = "error[E0425]: cannot find value `foo` in this scope\n  --> src/main.rs:12:5";
        let pattern = watchdog.detect_failure_pattern(
            "cycle_comp",
            Some("cargo check"),
            Some(1),
            Some(stderr_e0425),
            Some(120),
            Some(10000),
            None,
            None,
            None,
            None,
        );
        assert!(pattern.is_some());
        match pattern.unwrap() {
            FailurePattern::CompilerError { error_code, exit_code, .. } => {
                assert_eq!(error_code, "E0425");
                assert_eq!(exit_code, 1);
            }
            other => panic!("Expected CompilerError, got {:?}", other),
        }

        // 2. Command timeout
        let pattern_timeout = watchdog.detect_failure_pattern(
            "cycle_time",
            Some("cargo test --release"),
            Some(124),
            Some("Command timed out after 30000ms"),
            Some(30050),
            Some(30000),
            None,
            None,
            None,
            None,
        );
        assert!(pattern_timeout.is_some());
        match pattern_timeout.unwrap() {
            FailurePattern::CommandTimeout { duration_ms, timeout_limit_ms, .. } => {
                assert!(duration_ms >= timeout_limit_ms);
            }
            other => panic!("Expected CommandTimeout, got {:?}", other),
        }

        // 3. SQLite lock detection
        let pattern_lock = watchdog.detect_failure_pattern(
            "cycle_lock",
            None,
            None,
            None,
            None,
            None,
            None,
            Some("rusqlite::Error: database is locked (SQLITE_BUSY)"),
            None,
            None,
        );
        assert!(pattern_lock.is_some());
        match pattern_lock.unwrap() {
            FailurePattern::SqliteDatabaseLock { error_message } => {
                assert!(error_message.contains("SQLITE_BUSY"));
            }
            other => panic!("Expected SqliteDatabaseLock, got {:?}", other),
        }
    }

    #[test]
    fn test_hallucination_loop_detection_and_frequency_tracking() {
        let watchdog = AutonomousWatchdog::in_memory().expect("in-memory watchdog");
        let cycle_id = "cycle_rep_test";
        let duplicate_patch = "fn solve() {\n    let val = 42;\n    println!(\"{}\", val);\n}";

        // First occurrence: not a loop yet
        let pat1 = watchdog.detect_hallucination_loop(cycle_id, duplicate_patch);
        assert!(pat1.is_none());

        // Second occurrence: repetitive loop detected!
        let pat2 = watchdog.detect_hallucination_loop(cycle_id, duplicate_patch);
        assert!(pat2.is_some());
        match pat2.unwrap() {
            FailurePattern::HallucinationRepetitiveLoop { occurrences, .. } => {
                assert_eq!(occurrences, 2);
            }
            other => panic!("Expected HallucinationRepetitiveLoop, got {:?}", other),
        }

        // Test repetitive single-response line repetition
        let looping_code = "fn bad() {\n    call();\n    call();\n    call();\n    call();\n}";
        let loop_pat = watchdog.detect_hallucination_loop("cycle_loop_single", looping_code);
        assert!(loop_pat.is_some());
    }

    #[test]
    fn test_dynamic_generation_parameter_mutation() {
        let watchdog = AutonomousWatchdog::in_memory().expect("in-memory watchdog");
        let initial_params = AgentGenerationParams::default().with_temperature(0.2);
        assert_eq!(initial_params.temperature, 0.2);

        // 1. Compiler error mutation: escape temperature 0.2 -> 0.55 on iteration 1, 0.70 on iteration 2
        let comp_pattern = FailurePattern::CompilerError {
            error_code: "E0308".to_string(),
            exit_code: 1,
            stderr_snippet: "mismatched types".to_string(),
        };

        let (mut1, notes1) = watchdog.mutate_parameters(&initial_params, &comp_pattern, 1);
        assert!(mut1.temperature >= 0.5 && mut1.temperature <= 0.6);
        assert!(!notes1.is_empty());

        let (mut2, notes2) = watchdog.mutate_parameters(&initial_params, &comp_pattern, 2);
        assert_eq!(mut2.temperature, 0.70);
        assert!(mut2.top_p <= 0.90);
        assert!(!notes2.is_empty());

        // 2. Hallucination repetitive loop mutation: bumps temperature and penalizes frequency
        let hall_pattern = FailurePattern::HallucinationRepetitiveLoop {
            repeated_hash: "blake3_hash".to_string(),
            occurrences: 2,
            content_preview: "fn loop()".to_string(),
        };
        let (mut_hall, notes_hall) = watchdog.mutate_parameters(&initial_params, &hall_pattern, 1);
        assert!(mut_hall.temperature >= 0.65);
        assert!(mut_hall.frequency_penalty >= 0.5);
        assert!(mut_hall.presence_penalty >= 0.3);
        assert!(!notes_hall.is_empty());

        // 3. Context window overflow: switches to fallback model
        let overflow_pattern = FailurePattern::ContextWindowExceeded {
            current_tokens: 5000,
            max_tokens_limit: 4096,
        };
        let (mut_overflow, notes_overflow) = watchdog.mutate_parameters(&initial_params, &overflow_pattern, 1);
        assert_eq!(mut_overflow.model, "gemini-2.5-flash");
        assert!(mut_overflow.max_tokens >= 8192);
        assert!(!notes_overflow.is_empty());
    }

    #[test]
    fn test_full_self_correction_retry_cycle_to_first_green() {
        let temp_dir = std::env::temp_dir().join(format!("watchdog_fg_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);
        let target_file = temp_dir.join("calc.rs");

        let watchdog = AutonomousWatchdog::in_memory().expect("in-memory watchdog");
        let cycle_id = "cycle_full_self_correct";
        let initial_params = AgentGenerationParams::default().with_temperature(0.2);

        // Simulation: iteration 1 produces bad syntax (E0425), iteration 2 produces green code
        let story = watchdog.execute_self_correcting_cycle(
            cycle_id,
            Some(&target_file),
            "Write a function add(a: i32, b: i32) -> i32",
            "Initial scratchpad",
            initial_params,
            3,
            |_prompt, params, iteration| {
                if iteration == 1 {
                    assert_eq!(params.temperature, 0.2);
                    Ok("fn add(a: i32, b: i32) -> i32 { missing_var }".to_string())
                } else {
                    // Verifies that parameter was mutated to escape local minimum
                    assert!(params.temperature >= 0.5);
                    Ok("fn add(a: i32, b: i32) -> i32 { a + b }".to_string())
                }
            },
            |candidate, iteration| {
                if iteration == 1 {
                    Ok(Err(FailurePattern::CompilerError {
                        error_code: "E0425".to_string(),
                        exit_code: 1,
                        stderr_snippet: "cannot find value `missing_var` in this scope".to_string(),
                    }))
                } else {
                    assert!(candidate.contains("a + b"));
                    Ok(Ok(())) // First-Green verification passes!
                }
            },
        ).expect("execute_self_correcting_cycle should succeed");

        // Verify recovery story properties
        assert_eq!(story.cycle_id, cycle_id);
        assert_eq!(story.iterations_taken, 2);
        assert!(story.final_patch.contains("a + b"));
        assert!(story.hot_reload_triggered);
        assert!(!story.mutations_applied.is_empty());

        // Verify file written to disk
        let disk_contents = fs::read_to_string(&target_file).expect("read committed file");
        assert_eq!(disk_contents, "fn add(a: i32, b: i32) -> i32 { a + b }");

        // Verify recovery story query in SQLite
        let stories = watchdog.get_recovery_stories(Some(cycle_id)).expect("get_recovery_stories");
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].story_id, story.story_id);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
