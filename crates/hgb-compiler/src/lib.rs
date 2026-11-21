//! # Hagibis JIT WebAssembly-to-Native SIMD Pipeline (`hgb-compiler`)
//!
//! Sub-microsecond WebAssembly execution pipeline powered by Cranelift and Wasmtime:
//! - **Hot Execution Trace Profiler**: Tracks call frequency, fuel consumption, and loop iterations to trigger tiered compilation.
//! - **Cranelift SIMD Optimization Engine**: Detects host CPU vector extensions (AVX-512, AVX2, ARM Neon, SIMD128, Relaxed SIMD) and applies aggressive native code generation.
//! - **Near-Zero Latency Execution Cache**: Multi-tiered caching featuring in-memory pre-warmed modules (<1µs) and persistent memory-mapped (`Mmap`) artifacts with SHA-256 cryptographic verification.
//! - **SIMD Benchmark Suite**: High-throughput routines for vector dot-product, cosine distance, matrix multiply, and AST token scanning.

pub mod hot_trace_profiler;
pub mod simd_vectorizer;
pub mod native_cache;
pub mod jit_pipeline;
pub mod binary_decompilation_transpiler;
pub mod pgo_bolt_feedback;

// Re-exports
pub use hot_trace_profiler::{
    ExecutionTier, FunctionTrace, HotTraceProfiler, HotTraceSnapshot, PromotionPolicy,
    TierTransition,
};
pub use simd_vectorizer::{
    scalar_ast_token_scan, scalar_cosine_similarity_f32, scalar_dot_product_f32,
    scalar_matrix_multiply_f32, vector_ast_token_scan, vector_cosine_similarity_f32,
    vector_dot_product_f32, vector_matrix_multiply_f32, CpuFeatureReport, SimdVectorizer,
};
pub use native_cache::{
    ArtifactHeader, CacheError, CacheStats, NativeExecutionCache, PrewarmedModule,
    ARTIFACT_MAGIC, ARTIFACT_VERSION, DEFAULT_MAX_MEMORY_MODULES,
};
pub use jit_pipeline::{
    BenchmarkItemResult, ExecutionOutcome, JitError, JitPipeline, JitPipelineConfig,
    SimdBenchmarkSuiteReport,
};
pub use binary_decompilation_transpiler::{
    compute_shannon_entropy, BasicBlockNode, BinaryArch, BinaryFormat, BranchCondition, CfgBuilder,
    CfgEdge, ControlFlowGraph, DecompilationRequest, DecompilationResult, DefaultTranspilerEngine,
    ElfParser, FunctionParameter, MachOParser, PeParser, ReconstructedFunction,
    ReconstructedInterface, RustWrapperGenerator, SectionDescriptor, StringLiteralExtractor,
    SymbolDescriptor, SymbolKind, TranspilerEngine, TranspilerError, WasmParser, WitSynthesisEngine,
};
pub use pgo_bolt_feedback::{
    BenchmarkScore, ClassifiedHotPath, HotPathDetector, HotPathThresholds, NightlyBenchmarkConfig,
    NightlyBenchmarkRunner, OptimizationAction, OptimizationPassSynthesizer, PerformanceDelta,
    PgoFeedbackError, PgoOptimizationManifest, ProfiledFunctionMetric, SynthesizedCompilerConfig,
    WorkloadMetricParser, WorkloadProfileMetrics,
};

/// Returns the crate version string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crate_exports() {
        let cpu = CpuFeatureReport::detect();
        assert!(!cpu.target_arch.is_empty());

        let profiler = HotTraceProfiler::default();
        let res = profiler.record_call(1, Some("test"), 10, 1);
        assert_eq!(res, TierTransition::Unchanged(ExecutionTier::Tier0Interpreted));
    }
}
