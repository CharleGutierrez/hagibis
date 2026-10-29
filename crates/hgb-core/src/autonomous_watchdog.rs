//! # Pillar 4: Autonomous Watchdog Data Models & Telemetry Protocol
//!
//! Shared types for autonomous watchdog snapshotting, context rollback, dynamic parameter mutation,
//! failure taxonomies, and recovery stories across daemon, CLI, and microkernel layers.

use serde::{Deserialize, Serialize};

/// Dynamic LLM generation parameters for agent loops
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentGenerationParams {
    pub temperature: f64,
    pub top_p: f64,
    pub frequency_penalty: f64,
    pub presence_penalty: f64,
    pub model: String,
    pub fallback_model: Option<String>,
    pub max_tokens: usize,
}

impl Default for AgentGenerationParams {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            top_p: 0.95,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
            model: "qwen2.5-coder:1.5b".to_string(),
            fallback_model: Some("gemini-2.5-flash".to_string()),
            max_tokens: 4096,
        }
    }
}

impl AgentGenerationParams {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn with_temperature(mut self, temp: f64) -> Self {
        self.temperature = temp.clamp(0.0, 2.0);
        self
    }

    pub fn with_top_p(mut self, top_p: f64) -> Self {
        self.top_p = top_p.clamp(0.0, 1.0);
        self
    }

    pub fn with_frequency_penalty(mut self, penalty: f64) -> Self {
        self.frequency_penalty = penalty.clamp(-2.0, 2.0);
        self
    }

    pub fn with_fallback_model(mut self, fallback: impl Into<String>) -> Self {
        self.fallback_model = Some(fallback.into());
        self
    }

    pub fn with_max_tokens(mut self, max: usize) -> Self {
        self.max_tokens = max;
        self
    }
}

/// Agent conversation and memory state snapshot stored in SQLite WAL
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentContextSnapshot {
    pub snapshot_id: String,
    pub cycle_id: String,
    pub context_tokens: usize,
    pub prompt_history_json: String,
    pub scratchpad: String,
    pub generation_params: AgentGenerationParams,
    pub created_at: String,
}

/// Taxonomy of execution loop failure patterns detected by the watchdog
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FailurePattern {
    /// Compiler error with explicit diagnostic code (`E0xxx` or `TSxxxx`), exit code, and stderr snippet
    CompilerError {
        error_code: String,
        exit_code: i32,
        stderr_snippet: String,
    },
    /// Command execution timeout exceeding threshold
    CommandTimeout {
        command: String,
        duration_ms: u64,
        timeout_limit_ms: u64,
    },
    /// Generic tool execution failure with non-zero exit code
    ToolExecutionFailure {
        command: String,
        exit_code: i32,
        stderr_snippet: String,
    },
    /// LLM hallucination or repetitive loop: duplicate patch/prompt generated repeatedly
    HallucinationRepetitiveLoop {
        repeated_hash: String,
        occurrences: usize,
        content_preview: String,
    },
    /// SQLite database lock contention (`SQLITE_BUSY`, database is locked)
    SqliteDatabaseLock {
        error_message: String,
    },
    /// Context window limit exceeded
    ContextWindowExceeded {
        current_tokens: usize,
        max_tokens_limit: usize,
    },
}

impl FailurePattern {
    pub fn name(&self) -> &'static str {
        match self {
            Self::CompilerError { .. } => "CompilerError",
            Self::CommandTimeout { .. } => "CommandTimeout",
            Self::ToolExecutionFailure { .. } => "ToolExecutionFailure",
            Self::HallucinationRepetitiveLoop { .. } => "HallucinationRepetitiveLoop",
            Self::SqliteDatabaseLock { .. } => "SqliteDatabaseLock",
            Self::ContextWindowExceeded { .. } => "ContextWindowExceeded",
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::CompilerError { error_code, exit_code, stderr_snippet } => {
                format!("Compiler error code {} (exit {}): {}", error_code, exit_code, stderr_snippet)
            }
            Self::CommandTimeout { command, duration_ms, timeout_limit_ms } => {
                format!("Command '{}' timed out after {}ms (limit: {}ms)", command, duration_ms, timeout_limit_ms)
            }
            Self::ToolExecutionFailure { command, exit_code, stderr_snippet } => {
                format!("Command '{}' failed with exit code {}: {}", command, exit_code, stderr_snippet)
            }
            Self::HallucinationRepetitiveLoop { occurrences, content_preview, .. } => {
                format!("Repetitive loop detected: identical output generated {} times: '{}'", occurrences, content_preview)
            }
            Self::SqliteDatabaseLock { error_message } => {
                format!("SQLite database lock contention: {}", error_message)
            }
            Self::ContextWindowExceeded { current_tokens, max_tokens_limit } => {
                format!("Context window tokens ({} > {}) exceeded", current_tokens, max_tokens_limit)
            }
        }
    }
}

/// Persistent recovery story recorded upon First-Green success
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecoveryStory {
    pub story_id: String,
    pub cycle_id: String,
    pub initial_failure: String,
    pub failure_pattern: String,
    pub iterations_taken: usize,
    pub mutations_applied: Vec<String>,
    pub final_patch: String,
    pub target_file: Option<String>,
    pub hot_reload_triggered: bool,
    pub created_at: String,
    pub duration_ms: u64,
}

/// Telemetry status report for the Autonomous Watchdog
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchdogStatusReport {
    pub active: bool,
    pub total_snapshots: usize,
    pub total_rollbacks: usize,
    pub total_failures_intercepted: usize,
    pub total_recoveries_completed: usize,
    pub db_path: String,
    pub recent_recoveries: Vec<RecoveryStory>,
}
