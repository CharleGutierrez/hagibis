//! # Hot Execution Trace Profiler (`hot_trace_profiler`)
//!
//! Multi-tiered execution tracking for WebAssembly functions and kernels.
//! Records call frequency, fuel usage, and loop iterations per function index,
//! providing deterministic promotion escalation (e.g. Tier-0 interpreted/baseline -> Tier-2 SIMD).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use serde::{Deserialize, Serialize};

/// Execution tiers representing the compilation and optimization level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ExecutionTier {
    /// Tier 0: Interpreted or zero-optimization baseline JIT.
    /// Fast cold-start startup with profiling instrumentation.
    Tier0Interpreted = 0,
    /// Tier 1: Baseline Jit compilation (Cranelift speed-and-size).
    /// Low compilation latency, standard register allocation.
    Tier1BaselineJit = 1,
    /// Tier 2: SIMD-Optimized Native JIT (Cranelift Speed + AVX-512/AVX2/Neon vectorization).
    /// Aggressive loop unrolling, SIMD lane vectorization, zero-copy linear memory bounds.
    Tier2SimdOptimized = 2,
}

impl ExecutionTier {
    pub fn from_u8(val: u8) -> Self {
        match val {
            0 => ExecutionTier::Tier0Interpreted,
            1 => ExecutionTier::Tier1BaselineJit,
            _ => ExecutionTier::Tier2SimdOptimized,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionTier::Tier0Interpreted => "Tier0_Interpreted",
            ExecutionTier::Tier1BaselineJit => "Tier1_BaselineJit",
            ExecutionTier::Tier2SimdOptimized => "Tier2_SimdOptimized",
        }
    }

    pub fn is_simd(&self) -> bool {
        matches!(self, ExecutionTier::Tier2SimdOptimized)
    }

    pub fn level(&self) -> u8 {
        *self as u8
    }

    pub fn next(&self) -> Option<Self> {
        match self {
            ExecutionTier::Tier0Interpreted => Some(ExecutionTier::Tier1BaselineJit),
            ExecutionTier::Tier1BaselineJit => Some(ExecutionTier::Tier2SimdOptimized),
            ExecutionTier::Tier2SimdOptimized => None,
        }
    }
}

/// Transition outcome when recording function invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierTransition {
    /// The function remains at its current tier.
    Unchanged(ExecutionTier),
    /// The function has crossed a threshold and was promoted to a higher tier.
    Promoted {
        from: ExecutionTier,
        to: ExecutionTier,
    },
}

/// Dynamic promotion threshold configuration for JIT escalation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionPolicy {
    /// Invocations required to promote from Tier-0 to Tier-1.
    pub tier1_invocation_threshold: u64,
    /// Loop iterations required to promote from Tier-0 to Tier-1.
    pub tier1_loop_threshold: u64,
    /// Invocations required to promote from Tier-1 to Tier-2 (SIMD).
    pub tier2_invocation_threshold: u64,
    /// Loop iterations required to promote from Tier-1 to Tier-2.
    pub tier2_loop_threshold: u64,
    /// Cumulative fuel consumed threshold triggering Tier-2 SIMD optimization.
    pub tier2_fuel_threshold: u64,
}

impl Default for PromotionPolicy {
    fn default() -> Self {
        Self {
            tier1_invocation_threshold: 10,
            tier1_loop_threshold: 500,
            tier2_invocation_threshold: 100,
            tier2_loop_threshold: 5_000,
            tier2_fuel_threshold: 50_000,
        }
    }
}

/// Thread-safe execution trace counters for an individual WebAssembly function.
pub struct FunctionTrace {
    pub fn_index: u32,
    pub fn_name: Option<String>,
    pub invocation_count: AtomicU64,
    pub fuel_consumed: AtomicU64,
    pub loop_iterations: AtomicU64,
    current_tier: AtomicU8,
    pub created_at: Instant,
    pub last_escalated: RwLock<Instant>,
}

impl FunctionTrace {
    pub fn new(fn_index: u32, fn_name: Option<String>, initial_tier: ExecutionTier) -> Self {
        let now = Instant::now();
        Self {
            fn_index,
            fn_name,
            invocation_count: AtomicU64::new(0),
            fuel_consumed: AtomicU64::new(0),
            loop_iterations: AtomicU64::new(0),
            current_tier: AtomicU8::new(initial_tier as u8),
            created_at: now,
            last_escalated: RwLock::new(now),
        }
    }

    /// Read the current execution tier atomically.
    pub fn tier(&self) -> ExecutionTier {
        ExecutionTier::from_u8(self.current_tier.load(Ordering::Acquire))
    }

    /// Set a new execution tier, returning true if escalated.
    pub fn set_tier(&self, new_tier: ExecutionTier) -> bool {
        let prev = self.current_tier.swap(new_tier as u8, Ordering::AcqRel);
        if prev != new_tier as u8 {
            if let Ok(mut lock) = self.last_escalated.write() {
                *lock = Instant::now();
            }
            true
        } else {
            false
        }
    }

    /// Record a single call with fuel spent and loop iterations.
    pub fn record_call(&self, fuel_spent: u64, loop_iters: u64) {
        self.invocation_count.fetch_add(1, Ordering::Relaxed);
        self.fuel_consumed.fetch_add(fuel_spent, Ordering::Relaxed);
        self.loop_iterations.fetch_add(loop_iters, Ordering::Relaxed);
    }

    /// Evaluate if this function qualifies for tier escalation according to policy.
    pub fn evaluate_escalation(&self, policy: &PromotionPolicy) -> Option<ExecutionTier> {
        let current = self.tier();
        let invocations = self.invocation_count.load(Ordering::Relaxed);
        let loops = self.loop_iterations.load(Ordering::Relaxed);
        let fuel = self.fuel_consumed.load(Ordering::Relaxed);

        match current {
            ExecutionTier::Tier0Interpreted => {
                if invocations >= policy.tier2_invocation_threshold
                    || loops >= policy.tier2_loop_threshold
                    || fuel >= policy.tier2_fuel_threshold
                {
                    Some(ExecutionTier::Tier2SimdOptimized)
                } else if invocations >= policy.tier1_invocation_threshold
                    || loops >= policy.tier1_loop_threshold
                {
                    Some(ExecutionTier::Tier1BaselineJit)
                } else {
                    None
                }
            }
            ExecutionTier::Tier1BaselineJit => {
                if invocations >= policy.tier2_invocation_threshold
                    || loops >= policy.tier2_loop_threshold
                    || fuel >= policy.tier2_fuel_threshold
                {
                    Some(ExecutionTier::Tier2SimdOptimized)
                } else {
                    None
                }
            }
            ExecutionTier::Tier2SimdOptimized => None,
        }
    }

    /// Export an immutable snapshot of this function's profile.
    pub fn snapshot(&self) -> HotTraceSnapshot {
        HotTraceSnapshot {
            fn_index: self.fn_index,
            fn_name: self.fn_name.clone(),
            invocation_count: self.invocation_count.load(Ordering::Relaxed),
            fuel_consumed: self.fuel_consumed.load(Ordering::Relaxed),
            loop_iterations: self.loop_iterations.load(Ordering::Relaxed),
            tier: self.tier(),
            age_ms: self.created_at.elapsed().as_millis() as u64,
        }
    }
}

/// Serializable snapshot of a function execution trace for reporting and inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotTraceSnapshot {
    pub fn_index: u32,
    pub fn_name: Option<String>,
    pub invocation_count: u64,
    pub fuel_consumed: u64,
    pub loop_iterations: u64,
    pub tier: ExecutionTier,
    pub age_ms: u64,
}

/// Hot Execution Trace Profiler managing runtime statistics for all functions in an executable.
pub struct HotTraceProfiler {
    traces: Arc<RwLock<HashMap<u32, Arc<FunctionTrace>>>>,
    pub policy: PromotionPolicy,
    total_invocations: AtomicU64,
    total_promotions: AtomicU64,
}

impl Default for HotTraceProfiler {
    fn default() -> Self {
        Self::new(PromotionPolicy::default())
    }
}

impl HotTraceProfiler {
    /// Create a new profiler with the specified promotion policy.
    pub fn new(policy: PromotionPolicy) -> Self {
        Self {
            traces: Arc::new(RwLock::new(HashMap::new())),
            policy,
            total_invocations: AtomicU64::new(0),
            total_promotions: AtomicU64::new(0),
        }
    }

    /// Retrieve or register a function trace by its function index.
    pub fn get_or_register(
        &self,
        fn_index: u32,
        fn_name: Option<String>,
        initial_tier: ExecutionTier,
    ) -> Arc<FunctionTrace> {
        {
            let map = self.traces.read().unwrap();
            if let Some(trace) = map.get(&fn_index) {
                return Arc::clone(trace);
            }
        }

        let mut map = self.traces.write().unwrap();
        map.entry(fn_index)
            .or_insert_with(|| Arc::new(FunctionTrace::new(fn_index, fn_name, initial_tier)))
            .clone()
    }

    /// Record a call to a function and check if it should be escalated.
    pub fn record_call(
        &self,
        fn_index: u32,
        fn_name: Option<&str>,
        fuel_spent: u64,
        loop_iters: u64,
    ) -> TierTransition {
        self.total_invocations.fetch_add(1, Ordering::Relaxed);
        let trace = self.get_or_register(
            fn_index,
            fn_name.map(|s| s.to_string()),
            ExecutionTier::Tier0Interpreted,
        );

        trace.record_call(fuel_spent, loop_iters);

        let current_tier = trace.tier();
        if let Some(target_tier) = trace.evaluate_escalation(&self.policy) {
            if target_tier > current_tier && trace.set_tier(target_tier) {
                self.total_promotions.fetch_add(1, Ordering::Relaxed);
                return TierTransition::Promoted {
                    from: current_tier,
                    to: target_tier,
                };
            }
        }

        TierTransition::Unchanged(current_tier)
    }

    /// Read the tier of a function.
    pub fn check_tier(&self, fn_index: u32) -> ExecutionTier {
        let map = self.traces.read().unwrap();
        map.get(&fn_index)
            .map(|t| t.tier())
            .unwrap_or(ExecutionTier::Tier0Interpreted)
    }

    /// Manually promote a function to a specific tier.
    pub fn promote_tier(&self, fn_index: u32, target_tier: ExecutionTier) -> bool {
        let trace = self.get_or_register(fn_index, None, ExecutionTier::Tier0Interpreted);
        let promoted = trace.set_tier(target_tier);
        if promoted {
            self.total_promotions.fetch_add(1, Ordering::Relaxed);
        }
        promoted
    }

    /// Get total tracked function count.
    pub fn tracked_function_count(&self) -> usize {
        self.traces.read().unwrap().len()
    }

    /// Get total system-wide function invocations.
    pub fn total_invocations(&self) -> u64 {
        self.total_invocations.load(Ordering::Relaxed)
    }

    /// Get total tier promotions executed.
    pub fn total_promotions(&self) -> u64 {
        self.total_promotions.load(Ordering::Relaxed)
    }

    /// List all function trace snapshots.
    pub fn snapshots(&self) -> Vec<HotTraceSnapshot> {
        let map = self.traces.read().unwrap();
        map.values().map(|t| t.snapshot()).collect()
    }

    /// Retrieve the top N hottest functions sorted by invocation count.
    pub fn top_hot_functions(&self, limit: usize) -> Vec<HotTraceSnapshot> {
        let mut list = self.snapshots();
        list.sort_by(|a, b| b.invocation_count.cmp(&a.invocation_count));
        list.truncate(limit);
        list
    }

    /// Reset all traces.
    pub fn reset(&self) {
        let mut map = self.traces.write().unwrap();
        map.clear();
        self.total_invocations.store(0, Ordering::Relaxed);
        self.total_promotions.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_transitions() {
        assert_eq!(ExecutionTier::Tier0Interpreted.level(), 0);
        assert_eq!(ExecutionTier::Tier1BaselineJit.level(), 1);
        assert_eq!(ExecutionTier::Tier2SimdOptimized.level(), 2);

        assert!(!ExecutionTier::Tier0Interpreted.is_simd());
        assert!(!ExecutionTier::Tier1BaselineJit.is_simd());
        assert!(ExecutionTier::Tier2SimdOptimized.is_simd());

        assert_eq!(
            ExecutionTier::Tier0Interpreted.next(),
            Some(ExecutionTier::Tier1BaselineJit)
        );
        assert_eq!(
            ExecutionTier::Tier1BaselineJit.next(),
            Some(ExecutionTier::Tier2SimdOptimized)
        );
        assert_eq!(ExecutionTier::Tier2SimdOptimized.next(), None);
    }

    #[test]
    fn test_trace_counter_and_escalation() {
        let policy = PromotionPolicy {
            tier1_invocation_threshold: 5,
            tier1_loop_threshold: 100,
            tier2_invocation_threshold: 20,
            tier2_loop_threshold: 1_000,
            tier2_fuel_threshold: 10_000,
        };

        let profiler = HotTraceProfiler::new(policy);
        let fn_id = 42;

        for _ in 0..4 {
            let res = profiler.record_call(fn_id, Some("hot_kernel"), 100, 1);
            assert_eq!(res, TierTransition::Unchanged(ExecutionTier::Tier0Interpreted));
        }

        let res = profiler.record_call(fn_id, Some("hot_kernel"), 100, 1);
        assert_eq!(
            res,
            TierTransition::Promoted {
                from: ExecutionTier::Tier0Interpreted,
                to: ExecutionTier::Tier1BaselineJit
            }
        );
        assert_eq!(profiler.check_tier(fn_id), ExecutionTier::Tier1BaselineJit);

        for _ in 5..19 {
            let res = profiler.record_call(fn_id, Some("hot_kernel"), 100, 1);
            assert_eq!(res, TierTransition::Unchanged(ExecutionTier::Tier1BaselineJit));
        }

        let res = profiler.record_call(fn_id, Some("hot_kernel"), 100, 1);
        assert_eq!(
            res,
            TierTransition::Promoted {
                from: ExecutionTier::Tier1BaselineJit,
                to: ExecutionTier::Tier2SimdOptimized
            }
        );
        assert_eq!(profiler.check_tier(fn_id), ExecutionTier::Tier2SimdOptimized);

        let res = profiler.record_call(fn_id, Some("hot_kernel"), 100, 1);
        assert_eq!(res, TierTransition::Unchanged(ExecutionTier::Tier2SimdOptimized));

        assert_eq!(profiler.total_invocations(), 21);
        assert_eq!(profiler.total_promotions(), 2);
    }

    #[test]
    fn test_loop_iteration_escalation() {
        let policy = PromotionPolicy {
            tier1_invocation_threshold: 100,
            tier1_loop_threshold: 50,
            tier2_invocation_threshold: 1000,
            tier2_loop_threshold: 500,
            tier2_fuel_threshold: 100_000,
        };

        let profiler = HotTraceProfiler::new(policy);
        let fn_id = 7;

        let res = profiler.record_call(fn_id, Some("loop_kernel"), 10, 60);
        assert_eq!(
            res,
            TierTransition::Promoted {
                from: ExecutionTier::Tier0Interpreted,
                to: ExecutionTier::Tier1BaselineJit
            }
        );

        let res = profiler.record_call(fn_id, Some("loop_kernel"), 10, 500);
        assert_eq!(
            res,
            TierTransition::Promoted {
                from: ExecutionTier::Tier1BaselineJit,
                to: ExecutionTier::Tier2SimdOptimized
            }
        );
        assert_eq!(profiler.check_tier(fn_id), ExecutionTier::Tier2SimdOptimized);
    }

    #[test]
    fn test_top_hot_functions_ranking() {
        let profiler = HotTraceProfiler::default();
        profiler.record_call(1, Some("cold"), 10, 1);
        for _ in 0..10 {
            profiler.record_call(2, Some("warm"), 10, 1);
        }
        for _ in 0..50 {
            profiler.record_call(3, Some("hot"), 10, 1);
        }

        let top = profiler.top_hot_functions(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].fn_index, 3);
        assert_eq!(top[0].invocation_count, 50);
        assert_eq!(top[1].fn_index, 2);
        assert_eq!(top[1].invocation_count, 10);
    }
}
