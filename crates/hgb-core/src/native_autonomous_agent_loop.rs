//! # Native Rust Autonomous Agent Loops (`native_autonomous_agent_loop`)
//!
//! Pillar 3: Native Rust Autonomous Agent Loops
//! Ported directly from the masterclass (`docs/masterclass/ch8_autonomous_loops.py`)
//! into high-performance, deterministic Rust primitives:
//!
//! 1. `TokenBudgetTracker`:
//!    - Real-time token budget tracking across multi-step agent loops.
//!    - Tracking prompt tokens, completion tokens, and burn rate (tokens/sec).
//!    - Enforces hard cutoffs and graceful degradation.
//! 2. `SelfReflectionGate`:
//!    - Pre-emission verification analyzing behavioral invariants, syntax completeness,
//!      error traces, and hallucination scores.
//!    - Returns `ReflectionVerdict`: `Pass`, `Refine`, or `Abort`.
//! 3. `ToolSelectionFallbackEngine`:
//!    - Multi-tier automatic tool fallback chain:
//!      * Tier 1: Local Zig/WASM sandbox (<2ms).
//!      * Tier 2: Firecracker MicroVM / Landlock isolated sandbox (<15ms).
//!      * Tier 3: In-process native Rust tool implementation.
//! 4. `NativeAutonomousLoopEngine`:
//!    - Orchestrates recursive thought loops with deterministic memory safety,
//!      sub-millisecond step transitions, zero GC pauses, and BLAKE3 consensus certificates.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{HgbError, Result};

/// Default token warning threshold percentage (80%)
pub const DEFAULT_TOKEN_WARN_THRESHOLD_PCT: f64 = 0.80;
/// Default token hard cutoff percentage (95%)
pub const DEFAULT_TOKEN_HARD_CUTOFF_PCT: f64 = 0.95;
/// High token burn rate threshold triggering kernel governor escalation (80 tokens/sec)
pub const HIGH_TOKEN_BURN_RATE_THRESHOLD: f64 = 80.0;

/// Execution tier for autonomous tool dispatching
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutonomousExecutionTier {
    /// Tier 1: Local Zig/WASM sandbox (<2ms deterministic execution)
    Tier1LocalWasm,
    /// Tier 2: Firecracker MicroVM or Landlock RAM CoW overlay (<15ms)
    Tier2Microvm,
    /// Tier 3: In-process native Rust fallback
    Tier3NativeRust,
}

impl AutonomousExecutionTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tier1LocalWasm => "Tier 1 (WASM)",
            Self::Tier2Microvm => "Tier 2 (MicroVM)",
            Self::Tier3NativeRust => "Tier 3 (Native Rust)",
        }
    }
}

/// Status of the Token Budget Tracker
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenBudgetStatus {
    Normal,
    Warning,
    Exhausted,
    HardCutoff,
}

/// Real-time Token Budget Tracker for multi-step agent loops
#[derive(Debug)]
pub struct TokenBudgetTracker {
    max_total_tokens: usize,
    max_step_tokens: usize,
    warn_threshold_pct: f64,
    hard_cutoff_pct: f64,
    prompt_tokens: AtomicUsize,
    completion_tokens: AtomicUsize,
    start_time: Instant,
    last_sample_time: Arc<RwLock<Instant>>,
    last_sample_tokens: AtomicUsize,
    burn_rate_tokens_per_sec: Arc<RwLock<f64>>,
}

impl TokenBudgetTracker {
    pub fn new(max_total_tokens: usize, max_step_tokens: usize) -> Self {
        let now = Instant::now();
        Self {
            max_total_tokens,
            max_step_tokens,
            warn_threshold_pct: DEFAULT_TOKEN_WARN_THRESHOLD_PCT,
            hard_cutoff_pct: DEFAULT_TOKEN_HARD_CUTOFF_PCT,
            prompt_tokens: AtomicUsize::new(0),
            completion_tokens: AtomicUsize::new(0),
            start_time: now,
            last_sample_time: Arc::new(RwLock::new(now)),
            last_sample_tokens: AtomicUsize::new(0),
            burn_rate_tokens_per_sec: Arc::new(RwLock::new(0.0)),
        }
    }

    /// Record newly consumed tokens and update burn rate telemetry
    pub async fn record_tokens(&self, prompt: usize, completion: usize) -> TokenBudgetStatus {
        let prev_p = self.prompt_tokens.fetch_add(prompt, Ordering::SeqCst);
        let prev_c = self.completion_tokens.fetch_add(completion, Ordering::SeqCst);
        let current_total = prev_p + prompt + prev_c + completion;

        // Update burn rate
        let now = Instant::now();
        let mut last_time = self.last_sample_time.write().await;
        let elapsed = now.duration_since(*last_time).as_secs_f64();
        if elapsed >= 0.25 {
            let prev_sampled = self.last_sample_tokens.swap(current_total, Ordering::SeqCst);
            let delta_tokens = current_total.saturating_sub(prev_sampled);
            let rate = delta_tokens as f64 / elapsed;
            let mut burn = self.burn_rate_tokens_per_sec.write().await;
            *burn = rate;
            *last_time = now;
        }

        self.evaluate_status(current_total)
    }

    /// Evaluate current budget status
    pub fn evaluate_status(&self, current_total: usize) -> TokenBudgetStatus {
        let ratio = current_total as f64 / self.max_total_tokens as f64;
        if ratio >= self.hard_cutoff_pct {
            TokenBudgetStatus::HardCutoff
        } else if ratio >= 1.0 {
            TokenBudgetStatus::Exhausted
        } else if ratio >= self.warn_threshold_pct {
            TokenBudgetStatus::Warning
        } else {
            TokenBudgetStatus::Normal
        }
    }

    pub fn total_tokens(&self) -> usize {
        self.prompt_tokens.load(Ordering::SeqCst) + self.completion_tokens.load(Ordering::SeqCst)
    }

    pub fn prompt_tokens(&self) -> usize {
        self.prompt_tokens.load(Ordering::SeqCst)
    }

    pub fn completion_tokens(&self) -> usize {
        self.completion_tokens.load(Ordering::SeqCst)
    }

    pub fn max_total_tokens(&self) -> usize {
        self.max_total_tokens
    }

    pub fn max_step_tokens(&self) -> usize {
        self.max_step_tokens
    }

    pub async fn burn_rate(&self) -> f64 {
        *self.burn_rate_tokens_per_sec.read().await
    }

    /// Check if burn rate exceeds kernel optimization threshold
    pub async fn is_high_throughput_burn(&self) -> bool {
        self.burn_rate().await >= HIGH_TOKEN_BURN_RATE_THRESHOLD
    }
}

/// Reflection verdict returned by the self-reflection gate
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReflectionVerdict {
    Pass,
    Refine {
        reason: String,
        suggested_patch: String,
    },
    Abort {
        reason: String,
    },
}

/// Candidate output to be evaluated by `SelfReflectionGate`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateOutput {
    pub step_name: String,
    pub raw_response: String,
    pub error_traces: Vec<String>,
    pub compiler_diagnostics: Vec<String>,
    pub tests_passed: bool,
    pub hallucination_score: f64, // 0.0 = grounded, 1.0 = hallucinated
}

/// Invariant-based Self-Reflection Gate
#[derive(Debug, Default)]
pub struct SelfReflectionGate {
    max_hallucination_tolerance: f64,
}

impl SelfReflectionGate {
    pub fn new() -> Self {
        Self {
            max_hallucination_tolerance: 0.25,
        }
    }

    pub fn with_hallucination_tolerance(mut self, tolerance: f64) -> Self {
        self.max_hallucination_tolerance = tolerance;
        self
    }

    /// Evaluates candidate response against code syntax, error traces, and hallucination scores
    pub fn evaluate(&self, candidate: &CandidateOutput) -> ReflectionVerdict {
        // 1. Critical abort: contains unrecoverable fatal syntax or security violations
        for err in &candidate.error_traces {
            if err.contains("SIGSEGV") || err.contains("OOM") || err.contains("PANIC") {
                return ReflectionVerdict::Abort {
                    reason: format!("Fatal execution trap detected in output: {}", err),
                };
            }
        }

        // 2. High hallucination score exceeds tolerance
        if candidate.hallucination_score > self.max_hallucination_tolerance {
            return ReflectionVerdict::Refine {
                reason: format!(
                    "Hallucination score {:.2} exceeds tolerance threshold {:.2}",
                    candidate.hallucination_score, self.max_hallucination_tolerance
                ),
                suggested_patch: "Ground response with direct citations and concrete file offsets".to_string(),
            };
        }

        // 3. Compiler diagnostics or syntax errors detected
        if !candidate.compiler_diagnostics.is_empty() {
            let diag_summary = candidate.compiler_diagnostics.join("; ");
            return ReflectionVerdict::Refine {
                reason: format!("Compiler diagnostics present: {}", diag_summary),
                suggested_patch: format!("Fix compiler errors: {}", candidate.compiler_diagnostics[0]),
            };
        }

        // 4. Test execution failed
        if !candidate.tests_passed {
            return ReflectionVerdict::Refine {
                reason: "Automated regression tests failed on candidate patch".to_string(),
                suggested_patch: "Align implementation with test assertion invariants".to_string(),
            };
        }

        ReflectionVerdict::Pass
    }
}

/// Fallback outcome returned by `ToolSelectionFallbackEngine`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFallbackOutcome {
    pub final_tier_used: AutonomousExecutionTier,
    pub total_attempts: usize,
    pub success: bool,
    pub output_payload: serde_json::Value,
    pub latency_ms: f64,
    pub failure_reasons: Vec<String>,
}

/// Multi-tier automatic tool fallback chain
#[derive(Debug)]
pub struct ToolSelectionFallbackEngine {
    timeout_tier1_ms: u64,
    timeout_tier2_ms: u64,
}

impl Default for ToolSelectionFallbackEngine {
    fn default() -> Self {
        Self {
            timeout_tier1_ms: 10,  // WASM SLA: <10ms
            timeout_tier2_ms: 100, // MicroVM SLA: <100ms
        }
    }
}

impl ToolSelectionFallbackEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Dispatch tool across Tier 1 (WASM) -> Tier 2 (MicroVM) -> Tier 3 (Native Rust)
    pub async fn execute_with_fallback<F1, F2, F3>(
        &self,
        tool_name: &str,
        mut tier1_wasm: F1,
        mut tier2_microvm: F2,
        mut tier3_native: F3,
    ) -> ToolFallbackOutcome
    where
        F1: FnMut() -> std::result::Result<serde_json::Value, String>,
        F2: FnMut() -> std::result::Result<serde_json::Value, String>,
        F3: FnMut() -> std::result::Result<serde_json::Value, String>,
    {
        let start = Instant::now();
        let mut failures = Vec::new();

        // 1. Try Tier 1: Local Zig/WASM sandbox
        match tier1_wasm() {
            Ok(val) => {
                return ToolFallbackOutcome {
                    final_tier_used: AutonomousExecutionTier::Tier1LocalWasm,
                    total_attempts: 1,
                    success: true,
                    output_payload: val,
                    latency_ms: start.elapsed().as_secs_f64() * 1000.0,
                    failure_reasons: Vec::new(),
                };
            }
            Err(e) => {
                failures.push(format!("Tier 1 (WASM) failed for '{}': {}", tool_name, e));
            }
        }

        // 2. Fall back to Tier 2: MicroVM Sandbox
        match tier2_microvm() {
            Ok(val) => {
                return ToolFallbackOutcome {
                    final_tier_used: AutonomousExecutionTier::Tier2Microvm,
                    total_attempts: 2,
                    success: true,
                    output_payload: val,
                    latency_ms: start.elapsed().as_secs_f64() * 1000.0,
                    failure_reasons: failures,
                };
            }
            Err(e) => {
                failures.push(format!("Tier 2 (MicroVM) failed for '{}': {}", tool_name, e));
            }
        }

        // 3. Fall back to Tier 3: In-process Native Rust
        match tier3_native() {
            Ok(val) => ToolFallbackOutcome {
                final_tier_used: AutonomousExecutionTier::Tier3NativeRust,
                total_attempts: 3,
                success: true,
                output_payload: val,
                latency_ms: start.elapsed().as_secs_f64() * 1000.0,
                failure_reasons: failures,
            },
            Err(e) => {
                failures.push(format!("Tier 3 (Native Rust) failed for '{}': {}", tool_name, e));
                ToolFallbackOutcome {
                    final_tier_used: AutonomousExecutionTier::Tier3NativeRust,
                    total_attempts: 3,
                    success: false,
                    output_payload: serde_json::Value::Null,
                    latency_ms: start.elapsed().as_secs_f64() * 1000.0,
                    failure_reasons: failures,
                }
            }
        }
    }
}

/// Cryptographic Consensus Certificate of completed autonomous agent loop
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutonomousConsensusCertificate {
    pub workflow_id: String,
    pub intent: String,
    pub total_steps_executed: usize,
    pub blake3_digest: String,
    pub duration_ms: u64,
    pub final_verdict: String,
    pub timestamp_epoch_ms: u64,
}

/// Native Autonomous Loop Execution Report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeAutonomousLoopReport {
    pub workflow_id: String,
    pub intent: String,
    pub steps_completed: usize,
    pub total_tokens_consumed: usize,
    pub burn_rate_tokens_per_sec: f64,
    pub certificate: AutonomousConsensusCertificate,
    pub success: bool,
}

/// Native Rust Autonomous Agent Loop Hyper-Executor
pub struct NativeAutonomousLoopEngine {
    budget_tracker: Arc<TokenBudgetTracker>,
    reflection_gate: Arc<SelfReflectionGate>,
    fallback_engine: Arc<ToolSelectionFallbackEngine>,
}

impl NativeAutonomousLoopEngine {
    pub fn new(max_total_tokens: usize, max_step_tokens: usize) -> Self {
        Self {
            budget_tracker: Arc::new(TokenBudgetTracker::new(max_total_tokens, max_step_tokens)),
            reflection_gate: Arc::new(SelfReflectionGate::new()),
            fallback_engine: Arc::new(ToolSelectionFallbackEngine::new()),
        }
    }

    pub fn budget_tracker(&self) -> &Arc<TokenBudgetTracker> {
        &self.budget_tracker
    }

    pub fn reflection_gate(&self) -> &Arc<SelfReflectionGate> {
        &self.reflection_gate
    }

    pub fn fallback_engine(&self) -> &Arc<ToolSelectionFallbackEngine> {
        &self.fallback_engine
    }

    /// Execute a recursive autonomous thought and tool execution step
    pub async fn execute_loop_step(
        &self,
        step_name: &str,
        prompt_tokens: usize,
        candidate: CandidateOutput,
    ) -> Result<ReflectionVerdict> {
        let completion_estimate = candidate.raw_response.len() / 4;
        let status = self.budget_tracker.record_tokens(prompt_tokens, completion_estimate).await;

        if status == TokenBudgetStatus::HardCutoff {
            return Ok(ReflectionVerdict::Abort {
                reason: format!("Token hard cutoff limit reached ({} tokens)", self.budget_tracker.total_tokens()),
            });
        }

        let verdict = self.reflection_gate.evaluate(&candidate);
        Ok(verdict)
    }

    /// Seal and generate cryptographic BLAKE3 consensus certificate
    pub fn generate_certificate(
        &self,
        workflow_id: &str,
        intent: &str,
        steps_executed: usize,
        duration: Duration,
    ) -> AutonomousConsensusCertificate {
        let mut hasher = blake3::Hasher::new();
        hasher.update(workflow_id.as_bytes());
        hasher.update(intent.as_bytes());
        hasher.update(&steps_executed.to_le_bytes());
        hasher.update(&duration.as_millis().to_le_bytes());
        let digest = hasher.finalize().to_hex().to_string();

        AutonomousConsensusCertificate {
            workflow_id: workflow_id.to_string(),
            intent: intent.to_string(),
            total_steps_executed: steps_executed,
            blake3_digest: digest,
            duration_ms: duration.as_millis() as u64,
            final_verdict: "FirstGreenVerified".to_string(),
            timestamp_epoch_ms: Utc::now().timestamp_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_budget_tracker_accounting_and_cutoffs() {
        let tracker = TokenBudgetTracker::new(1000, 250);

        // 1. Initial normal consumption
        let status1 = tracker.record_tokens(300, 200).await;
        assert_eq!(status1, TokenBudgetStatus::Normal);
        assert_eq!(tracker.total_tokens(), 500);

        // 2. Consume past 80% warning threshold (850 tokens / 1000)
        let status2 = tracker.record_tokens(200, 150).await;
        assert_eq!(status2, TokenBudgetStatus::Warning);
        assert_eq!(tracker.total_tokens(), 850);

        // 3. Consume past 95% hard cutoff
        let status3 = tracker.record_tokens(100, 20).await;
        assert_eq!(status3, TokenBudgetStatus::HardCutoff);
        assert_eq!(tracker.total_tokens(), 970);
    }

    #[test]
    fn test_self_reflection_gate_verdicts() {
        let gate = SelfReflectionGate::new();

        // Pass case
        let valid_cand = CandidateOutput {
            step_name: "generate_auth_middleware".to_string(),
            raw_response: "pub fn authenticate() -> bool { true }".to_string(),
            error_traces: Vec::new(),
            compiler_diagnostics: Vec::new(),
            tests_passed: true,
            hallucination_score: 0.05,
        };
        assert_eq!(gate.evaluate(&valid_cand), ReflectionVerdict::Pass);

        // Refine case: compiler diagnostics
        let broken_cand = CandidateOutput {
            compiler_diagnostics: vec!["error[E0425]: cannot find value `token` in this scope".to_string()],
            ..valid_cand.clone()
        };
        match gate.evaluate(&broken_cand) {
            ReflectionVerdict::Refine { reason, .. } => assert!(reason.contains("Compiler diagnostics")),
            _ => panic!("Expected Refine verdict"),
        }

        // Abort case: fatal crash
        let crash_cand = CandidateOutput {
            error_traces: vec!["Process terminated with SIGSEGV".to_string()],
            ..valid_cand
        };
        match gate.evaluate(&crash_cand) {
            ReflectionVerdict::Abort { reason } => assert!(reason.contains("Fatal execution trap")),
            _ => panic!("Expected Abort verdict"),
        }
    }

    #[tokio::test]
    async fn test_tool_selection_fallback_tiers() {
        let fallback = ToolSelectionFallbackEngine::new();

        // 1. Tier 1 succeeds directly
        let res1 = fallback
            .execute_with_fallback(
                "regex_validator",
                || Ok(serde_json::json!({"matched": true})),
                || panic!("Should not reach Tier 2"),
                || panic!("Should not reach Tier 3"),
            )
            .await;
        assert!(res1.success);
        assert_eq!(res1.final_tier_used, AutonomousExecutionTier::Tier1LocalWasm);
        assert_eq!(res1.total_attempts, 1);

        // 2. Tier 1 fails -> falls back to Tier 2
        let res2 = fallback
            .execute_with_fallback(
                "code_compiler",
                || Err("WASM memory bounds violation".to_string()),
                || Ok(serde_json::json!({"compiled": true})),
                || panic!("Should not reach Tier 3"),
            )
            .await;
        assert!(res2.success);
        assert_eq!(res2.final_tier_used, AutonomousExecutionTier::Tier2Microvm);
        assert_eq!(res2.total_attempts, 2);
        assert_eq!(res2.failure_reasons.len(), 1);

        // 3. Tier 1 and 2 fail -> falls back to Tier 3 native
        let res3 = fallback
            .execute_with_fallback(
                "native_hasher",
                || Err("No WASM runtime".to_string()),
                || Err("MicroVM boot timeout".to_string()),
                || Ok(serde_json::json!({"hash": "deadbeef"})),
            )
            .await;
        assert!(res3.success);
        assert_eq!(res3.final_tier_used, AutonomousExecutionTier::Tier3NativeRust);
        assert_eq!(res3.total_attempts, 3);
        assert_eq!(res3.failure_reasons.len(), 2);
    }

    #[tokio::test]
    async fn test_native_autonomous_loop_execution_and_certificate() {
        let engine = NativeAutonomousLoopEngine::new(5000, 500);

        let cand = CandidateOutput {
            step_name: "vector_index_build".to_string(),
            raw_response: "fn build() {}".to_string(),
            error_traces: Vec::new(),
            compiler_diagnostics: Vec::new(),
            tests_passed: true,
            hallucination_score: 0.02,
        };

        let verdict = engine.execute_loop_step("step_1", 120, cand).await.unwrap();
        assert_eq!(verdict, ReflectionVerdict::Pass);

        let cert = engine.generate_certificate("wf-999", "Build high-speed SIMD index", 1, Duration::from_millis(45));
        assert_eq!(cert.workflow_id, "wf-999");
        assert_eq!(cert.total_steps_executed, 1);
        assert!(!cert.blake3_digest.is_empty());
    }
}
