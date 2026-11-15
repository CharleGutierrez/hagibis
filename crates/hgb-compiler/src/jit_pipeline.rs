//! # JIT WebAssembly-to-Native SIMD Pipeline (`jit_pipeline`)
//!
//! Orchestrates the end-to-end execution lifecycle:
//! 1. Hot trace function frequency and fuel profiling (`HotTraceProfiler`)
//! 2. Tiered Cranelift engine selection (Tier 0 -> Tier 1 -> Tier 2 SIMD)
//! 3. Sub-microsecond cached module retrieval (`NativeExecutionCache`)
//! 4. Sandboxed execution with fuel consumption and memory bounds
//! 5. Comprehensive SIMD performance benchmarking suite

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use wasmtime::{Engine, Instance, Linker, Module, Store, Val};

use crate::hot_trace_profiler::{ExecutionTier, HotTraceProfiler, PromotionPolicy, TierTransition};
use crate::native_cache::{CacheError, NativeExecutionCache};
use crate::simd_vectorizer::{
    scalar_ast_token_scan, scalar_cosine_similarity_f32, scalar_dot_product_f32,
    scalar_matrix_multiply_f32, vector_ast_token_scan, vector_cosine_similarity_f32,
    vector_dot_product_f32, vector_matrix_multiply_f32, CpuFeatureReport, SimdVectorizer,
};

/// Pipeline errors.
#[derive(Debug, thiserror::Error)]
pub enum JitError {
    #[error("Cache error: {0}")]
    Cache(#[from] CacheError),
    #[error("Wasmtime runtime error: {0}")]
    Wasmtime(#[from] wasmtime::Error),
    #[error("Export function '{0}' not found in module")]
    FunctionNotFound(String),
    #[error("Type mismatch or signature error: {0}")]
    SignatureMismatch(String),
    #[error("Compilation error: {0}")]
    Compilation(String),
}

/// Pipeline configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JitPipelineConfig {
    pub cache_dir: PathBuf,
    pub max_memory_entries: usize,
    pub promotion_policy: PromotionPolicy,
    pub initial_tier: ExecutionTier,
    pub fuel_budget: Option<u64>,
}

impl Default for JitPipelineConfig {
    fn default() -> Self {
        Self {
            cache_dir: PathBuf::from(".hgb/jit_cache"),
            max_memory_entries: 128,
            promotion_policy: PromotionPolicy::default(),
            initial_tier: ExecutionTier::Tier0Interpreted,
            fuel_budget: Some(1_000_000),
        }
    }
}

/// Outcome of a JIT function execution.
#[derive(Debug, Clone)]
pub struct ExecutionOutcome {
    pub results: Vec<Val>,
    pub tier: ExecutionTier,
    pub duration_ns: u64,
    pub fuel_consumed: u64,
    pub escalated: bool,
}

/// Benchmark result for an individual computational benchmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkItemResult {
    pub name: String,
    pub scalar_latency_ns: f64,
    pub simd_latency_ns: f64,
    pub speedup_factor: f64,
    pub throughput_mops: f64,
}

/// Complete report of the SIMD performance benchmark suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimdBenchmarkSuiteReport {
    pub host_cpu: CpuFeatureReport,
    pub items: Vec<BenchmarkItemResult>,
    pub overall_average_speedup: f64,
}

/// JIT WebAssembly-to-Native SIMD Pipeline.
pub struct JitPipeline {
    pub profiler: Arc<HotTraceProfiler>,
    pub vectorizer: Arc<SimdVectorizer>,
    pub cache: Arc<NativeExecutionCache>,
    pub config: JitPipelineConfig,
    engine_tier0: Engine,
    engine_tier1: Engine,
    engine_tier2: Engine,
    total_executions: AtomicU64,
    total_recompilations: AtomicU64,
}

impl JitPipeline {
    /// Initialize JIT pipeline with configuration.
    pub fn new(config: JitPipelineConfig) -> Result<Self, JitError> {
        let vectorizer = Arc::new(SimdVectorizer::new());
        let profiler = Arc::new(HotTraceProfiler::new(config.promotion_policy.clone()));
        let cache = Arc::new(
            NativeExecutionCache::new(&config.cache_dir, config.max_memory_entries)
                .map_err(JitError::Cache)?,
        );

        // Build Cranelift engines for all 3 tiers
        let cfg0 = vectorizer.build_wasmtime_config(ExecutionTier::Tier0Interpreted);
        let cfg1 = vectorizer.build_wasmtime_config(ExecutionTier::Tier1BaselineJit);
        let cfg2 = vectorizer.build_wasmtime_config(ExecutionTier::Tier2SimdOptimized);

        let engine_tier0 = Engine::new(&cfg0).map_err(JitError::Wasmtime)?;
        let engine_tier1 = Engine::new(&cfg1).map_err(JitError::Wasmtime)?;
        let engine_tier2 = Engine::new(&cfg2).map_err(JitError::Wasmtime)?;

        Ok(Self {
            profiler,
            vectorizer,
            cache,
            config,
            engine_tier0,
            engine_tier1,
            engine_tier2,
            total_executions: AtomicU64::new(0),
            total_recompilations: AtomicU64::new(0),
        })
    }

    /// Select engine matching execution tier.
    pub fn engine_for_tier(&self, tier: ExecutionTier) -> &Engine {
        match tier {
            ExecutionTier::Tier0Interpreted => &self.engine_tier0,
            ExecutionTier::Tier1BaselineJit => &self.engine_tier1,
            ExecutionTier::Tier2SimdOptimized => &self.engine_tier2,
        }
    }

    /// Compile or retrieve module from multi-tiered cache.
    pub fn get_or_compile(&self, wasm_bytes: &[u8], tier: ExecutionTier) -> Result<Module, JitError> {
        let engine = self.engine_for_tier(tier);
        self.cache
            .get_or_compile(engine, wasm_bytes, tier)
            .map_err(JitError::Cache)
    }

    /// Execute a function with automatic hot-trace profiling and tier escalation.
    pub fn execute(
        &self,
        wasm_bytes: &[u8],
        fn_name: &str,
        params: &[Val],
    ) -> Result<ExecutionOutcome, JitError> {
        self.total_executions.fetch_add(1, Ordering::Relaxed);

        // Derive function index hash for profiling
        let fn_id = {
            let mut h = 0u32;
            for b in fn_name.bytes() {
                h = h.wrapping_mul(31).wrapping_add(b as u32);
            }
            h
        };

        // Determine current tier from profiler
        let current_tier = self.profiler.check_tier(fn_id);
        let engine = self.engine_for_tier(current_tier);

        // Compile or fetch module
        let module = self
            .cache
            .get_or_compile(engine, wasm_bytes, current_tier)
            .map_err(JitError::Cache)?;

        let mut store = Store::new(engine, ());
        if let Some(fuel) = self.config.fuel_budget {
            let _ = store.set_fuel(fuel);
        }

        let linker = Linker::new(engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(JitError::Wasmtime)?;

        let func = instance
            .get_func(&mut store, fn_name)
            .ok_or_else(|| JitError::FunctionNotFound(fn_name.to_string()))?;

        let param_types = func.ty(&store).params().collect::<Vec<_>>();
        let result_types = func.ty(&store).results().collect::<Vec<_>>();

        if param_types.len() != params.len() {
            return Err(JitError::SignatureMismatch(format!(
                "Expected {} parameters, got {}",
                param_types.len(),
                params.len()
            )));
        }

        let mut results = vec![Val::I32(0); result_types.len()];

        let start = Instant::now();
        let initial_fuel = store.get_fuel().unwrap_or(0);

        // Sub-microsecond execution
        func.call(&mut store, params, &mut results)
            .map_err(JitError::Wasmtime)?;

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        let final_fuel = store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(final_fuel);

        // Record execution in hot trace profiler
        let transition = self.profiler.record_call(fn_id, Some(fn_name), fuel_consumed, 1);
        let mut escalated = false;

        if let TierTransition::Promoted { to, .. } = transition {
            escalated = true;
            self.total_recompilations.fetch_add(1, Ordering::Relaxed);
            // Pre-compile into newly promoted tier in background/cache
            let next_engine = self.engine_for_tier(to);
            let _ = self.cache.get_or_compile(next_engine, wasm_bytes, to);
        }

        Ok(ExecutionOutcome {
            results,
            tier: current_tier,
            duration_ns: elapsed_ns,
            fuel_consumed,
            escalated,
        })
    }

    /// Run the comprehensive SIMD Vectorization Benchmark Suite.
    pub fn run_simd_benchmarks(&self) -> SimdBenchmarkSuiteReport {
        let mut items = Vec::new();

        // 1. Vector Dot Product (16,384 elements)
        {
            let n = 16_384;
            let a: Vec<f32> = (0..n).map(|i| (i as f32) * 0.001).collect();
            let b: Vec<f32> = (0..n).map(|i| (i as f32) * 0.002).collect();

            // Warm up
            let _ = vector_dot_product_f32(&a, &b);
            let _ = scalar_dot_product_f32(&a, &b);

            let iters = 1_000;
            let start_v = Instant::now();
            for _ in 0..iters {
                let _ = vector_dot_product_f32(&a, &b);
            }
            let v_ns = (start_v.elapsed().as_nanos() as f64) / (iters as f64);

            let start_s = Instant::now();
            for _ in 0..iters {
                let _ = scalar_dot_product_f32(&a, &b);
            }
            let s_ns = (start_s.elapsed().as_nanos() as f64) / (iters as f64);

            let speedup = s_ns / v_ns.max(1.0);
            let mops = (n as f64) / (v_ns / 1_000.0);

            items.push(BenchmarkItemResult {
                name: "Vector Dot Product (f32, N=16k)".to_string(),
                scalar_latency_ns: s_ns,
                simd_latency_ns: v_ns,
                speedup_factor: speedup,
                throughput_mops: mops,
            });
        }

        // 2. Cosine Similarity (1,536-dim embedding vector)
        {
            let dim = 1_536;
            let a: Vec<f32> = (0..dim).map(|i| ((i % 100) as f32) * 0.01).collect();
            let b: Vec<f32> = (0..dim).map(|i| (((i + 50) % 100) as f32) * 0.01).collect();

            let iters = 5_000;
            let start_v = Instant::now();
            for _ in 0..iters {
                let _ = vector_cosine_similarity_f32(&a, &b);
            }
            let v_ns = (start_v.elapsed().as_nanos() as f64) / (iters as f64);

            let start_s = Instant::now();
            for _ in 0..iters {
                let _ = scalar_cosine_similarity_f32(&a, &b);
            }
            let s_ns = (start_s.elapsed().as_nanos() as f64) / (iters as f64);

            let speedup = s_ns / v_ns.max(1.0);
            let mops = (dim as f64) / (v_ns / 1_000.0);

            items.push(BenchmarkItemResult {
                name: "Cosine Distance (f32, 1536-dim Embedding)".to_string(),
                scalar_latency_ns: s_ns,
                simd_latency_ns: v_ns,
                speedup_factor: speedup,
                throughput_mops: mops,
            });
        }

        // 3. Matrix Multiplication (64x64 GEMM)
        {
            let m = 64;
            let k = 64;
            let n = 64;
            let a: Vec<f32> = (0..(m * k)).map(|i| (i as f32) * 0.01).collect();
            let b: Vec<f32> = (0..(k * n)).map(|i| (i as f32) * 0.02).collect();
            let mut c_v = vec![0.0f32; m * n];
            let mut c_s = vec![0.0f32; m * n];

            let iters = 100;
            let start_v = Instant::now();
            for _ in 0..iters {
                vector_matrix_multiply_f32(m, k, n, &a, &b, &mut c_v);
            }
            let v_ns = (start_v.elapsed().as_nanos() as f64) / (iters as f64);

            let start_s = Instant::now();
            for _ in 0..iters {
                scalar_matrix_multiply_f32(m, k, n, &a, &b, &mut c_s);
            }
            let s_ns = (start_s.elapsed().as_nanos() as f64) / (iters as f64);

            let speedup = s_ns / v_ns.max(1.0);
            let ops = (2 * m * k * n) as f64;
            let mops = ops / (v_ns / 1_000.0);

            items.push(BenchmarkItemResult {
                name: "Matrix Multiplication (64x64 GEMM)".to_string(),
                scalar_latency_ns: s_ns,
                simd_latency_ns: v_ns,
                speedup_factor: speedup,
                throughput_mops: mops,
            });
        }

        // 4. AST Token Scan (64 KiB buffer)
        {
            let mut buffer = Vec::with_capacity(65_536);
            for i in 0..65_536 {
                if i % 16 == 0 {
                    buffer.push(b' ');
                } else {
                    buffer.push(b'a' + ((i % 26) as u8));
                }
            }

            let iters = 500;
            let start_v = Instant::now();
            for _ in 0..iters {
                let _ = vector_ast_token_scan(&buffer, b' ');
            }
            let v_ns = (start_v.elapsed().as_nanos() as f64) / (iters as f64);

            let start_s = Instant::now();
            for _ in 0..iters {
                let _ = scalar_ast_token_scan(&buffer, b' ');
            }
            let s_ns = (start_s.elapsed().as_nanos() as f64) / (iters as f64);

            let speedup = s_ns / v_ns.max(1.0);
            let mb_sec = (buffer.len() as f64 / (1024.0 * 1024.0)) / (v_ns / 1_000_000_000.0);

            items.push(BenchmarkItemResult {
                name: "AST Token Delimiter Scan (64 KiB)".to_string(),
                scalar_latency_ns: s_ns,
                simd_latency_ns: v_ns,
                speedup_factor: speedup,
                throughput_mops: mb_sec,
            });
        }

        let avg_speedup = if items.is_empty() {
            1.0
        } else {
            items.iter().map(|it| it.speedup_factor).sum::<f64>() / (items.len() as f64)
        };

        SimdBenchmarkSuiteReport {
            host_cpu: self.vectorizer.cpu_features.clone(),
            items,
            overall_average_speedup: avg_speedup,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_execution_and_escalation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut config = JitPipelineConfig::default();
        config.cache_dir = temp_dir.path().to_path_buf();
        config.promotion_policy = PromotionPolicy {
            tier1_invocation_threshold: 3,
            tier1_loop_threshold: 50,
            tier2_invocation_threshold: 6,
            tier2_loop_threshold: 100,
            tier2_fuel_threshold: 5000,
        };

        let pipeline = JitPipeline::new(config).unwrap();

        let wat = r#"
            (module
                (func (export "add") (param i32 i32) (result i32)
                    local.get 0
                    local.get 1
                    i32.add
                )
            )
        "#;
        let wasm = wat::parse_str(wat).unwrap();

        let params = vec![Val::I32(10), Val::I32(32)];

        // Run multiple executions to trigger escalation
        for i in 1..=8 {
            let outcome = pipeline.execute(&wasm, "add", &params).unwrap();
            assert_eq!(outcome.results[0].unwrap_i32(), 42);

            if i >= 7 {
                assert_eq!(outcome.tier, ExecutionTier::Tier2SimdOptimized);
            } else if i >= 4 {
                assert_eq!(outcome.tier, ExecutionTier::Tier1BaselineJit);
            } else {
                assert_eq!(outcome.tier, ExecutionTier::Tier0Interpreted);
            }
            if i == 3 || i == 6 {
                assert!(outcome.escalated);
            }
        }
    }

    #[test]
    fn test_simd_benchmark_suite_execution() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut config = JitPipelineConfig::default();
        config.cache_dir = temp_dir.path().to_path_buf();

        let pipeline = JitPipeline::new(config).unwrap();
        let report = pipeline.run_simd_benchmarks();

        assert_eq!(report.items.len(), 4);
        assert!(report.overall_average_speedup >= 0.5);

        for item in &report.items {
            assert!(item.simd_latency_ns > 0.0);
            assert!(item.throughput_mops > 0.0);
        }
    }
}
