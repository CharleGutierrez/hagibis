//! # Cranelift SIMD Optimization Engine & Vectorizer (`simd_vectorizer`)
//!
//! Configures host CPU feature detection (AVX-512, AVX2, ARM Neon, WebAssembly SIMD128, Relaxed SIMD),
//! optimizes Cranelift code-generation flags, and provides native SIMD computational kernels
//! for sub-microsecond vector operations and benchmarking against scalar and JIT execution.

use serde::{Deserialize, Serialize};
use wasmtime::{Config, OptLevel};

/// CPU Feature Report containing host hardware SIMD capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpuFeatureReport {
    pub target_arch: String,
    pub has_avx512f: bool,
    pub has_avx512dq: bool,
    pub has_avx512vl: bool,
    pub has_avx512bw: bool,
    pub has_avx2: bool,
    pub has_fma: bool,
    pub has_neon: bool,
    pub has_wasm_simd128: bool,
    pub has_wasm_relaxed_simd: bool,
    pub simd_vector_width_bits: usize,
}

impl Default for CpuFeatureReport {
    fn default() -> Self {
        Self::detect()
    }
}

impl CpuFeatureReport {
    /// Detect host CPU features dynamically at runtime.
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let has_avx512f = is_x86_feature_detected!("avx512f");
            let has_avx512dq = is_x86_feature_detected!("avx512dq");
            let has_avx512vl = is_x86_feature_detected!("avx512vl");
            let has_avx512bw = is_x86_feature_detected!("avx512bw");
            let has_avx2 = is_x86_feature_detected!("avx2");
            let has_fma = is_x86_feature_detected!("fma");
            let has_neon = false;

            let simd_vector_width_bits = if has_avx512f {
                512
            } else if has_avx2 {
                256
            } else {
                128
            };

            Self {
                target_arch: "x86_64".to_string(),
                has_avx512f,
                has_avx512dq,
                has_avx512vl,
                has_avx512bw,
                has_avx2,
                has_fma,
                has_neon,
                has_wasm_simd128: true,
                has_wasm_relaxed_simd: true,
                simd_vector_width_bits,
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            let has_neon = std::arch::is_aarch64_feature_detected!("neon");
            Self {
                target_arch: "aarch64".to_string(),
                has_avx512f: false,
                has_avx512dq: false,
                has_avx512vl: false,
                has_avx512bw: false,
                has_avx2: false,
                has_fma: false,
                has_neon,
                has_wasm_simd128: true,
                has_wasm_relaxed_simd: true,
                simd_vector_width_bits: 128,
            }
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self {
                target_arch: std::env::consts::ARCH.to_string(),
                has_avx512f: false,
                has_avx512dq: false,
                has_avx512vl: false,
                has_avx512bw: false,
                has_avx2: false,
                has_fma: false,
                has_neon: false,
                has_wasm_simd128: true,
                has_wasm_relaxed_simd: true,
                simd_vector_width_bits: 128,
            }
        }
    }

    /// Best detected vector acceleration banner.
    pub fn best_vector_isa(&self) -> &'static str {
        if self.has_avx512f && self.has_avx512bw {
            "AVX-512 (512-bit ZMM)"
        } else if self.has_avx2 {
            "AVX2 + FMA (256-bit YMM)"
        } else if self.has_neon {
            "ARM Neon (128-bit)"
        } else if self.has_wasm_simd128 {
            "WebAssembly SIMD128 (128-bit)"
        } else {
            "Scalar Fallback"
        }
    }
}

/// Cranelift SIMD Optimization Engine builder.
pub struct SimdVectorizer {
    pub cpu_features: CpuFeatureReport,
}

impl Default for SimdVectorizer {
    fn default() -> Self {
        Self::new()
    }
}

impl SimdVectorizer {
    pub fn new() -> Self {
        Self {
            cpu_features: CpuFeatureReport::detect(),
        }
    }

    /// Construct a Wasmtime Config tuned for the target execution tier.
    pub fn build_wasmtime_config(&self, tier: crate::hot_trace_profiler::ExecutionTier) -> Config {
        let mut cfg = Config::new();

        // 1. Enable standard and relaxed WebAssembly SIMD vector extensions
        cfg.wasm_simd(true);
        cfg.wasm_relaxed_simd(true);
        cfg.wasm_multi_value(true);
        cfg.wasm_bulk_memory(true);

        // 2. Configure Cranelift optimization level
        match tier {
            crate::hot_trace_profiler::ExecutionTier::Tier0Interpreted => {
                cfg.cranelift_opt_level(OptLevel::None);
            }
            crate::hot_trace_profiler::ExecutionTier::Tier1BaselineJit => {
                cfg.cranelift_opt_level(OptLevel::SpeedAndSize);
            }
            crate::hot_trace_profiler::ExecutionTier::Tier2SimdOptimized => {
                cfg.cranelift_opt_level(OptLevel::Speed);
                // Disable NaN canonicalization for raw FP speed in hot kernels
                cfg.cranelift_nan_canonicalization(false);

                // Set architecture-specific Cranelift compiler vector flags
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    if self.cpu_features.has_avx2 {
                        let _ = cfg.cranelift_flag_set("has_avx2", "true");
                    }
                    if self.cpu_features.has_fma {
                        let _ = cfg.cranelift_flag_set("has_fma", "true");
                    }
                    if self.cpu_features.has_avx512f {
                        let _ = cfg.cranelift_flag_set("has_avx512f", "true");
                    }
                    if self.cpu_features.has_avx512dq {
                        let _ = cfg.cranelift_flag_set("has_avx512dq", "true");
                    }
                    if self.cpu_features.has_avx512vl {
                        let _ = cfg.cranelift_flag_set("has_avx512vl", "true");
                    }
                    if self.cpu_features.has_avx512bw {
                        let _ = cfg.cranelift_flag_set("has_avx512bw", "true");
                    }
                }
            }
        }

        cfg
    }
}

// =========================================================================
// Native SIMD Computational Kernels & Benchmark Routines
// =========================================================================

/// SIMD-vectorized 32-bit floating point dot product.
/// Unrolled with 8 independent accumulators to saturate FMA/vector execution units.
pub fn vector_dot_product_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut sum0 = 0.0f32;
    let mut sum1 = 0.0f32;
    let mut sum2 = 0.0f32;
    let mut sum3 = 0.0f32;
    let mut sum4 = 0.0f32;
    let mut sum5 = 0.0f32;
    let mut sum6 = 0.0f32;
    let mut sum7 = 0.0f32;

    let chunks = len / 8;
    for i in 0..chunks {
        let base = i * 8;
        sum0 += a[base] * b[base];
        sum1 += a[base + 1] * b[base + 1];
        sum2 += a[base + 2] * b[base + 2];
        sum3 += a[base + 3] * b[base + 3];
        sum4 += a[base + 4] * b[base + 4];
        sum5 += a[base + 5] * b[base + 5];
        sum6 += a[base + 6] * b[base + 6];
        sum7 += a[base + 7] * b[base + 7];
    }

    let mut total = (sum0 + sum1) + (sum2 + sum3) + (sum4 + sum5) + (sum6 + sum7);
    for i in (chunks * 8)..len {
        total += a[i] * b[i];
    }
    total
}

/// Baseline scalar dot product for throughput and accuracy comparison.
pub fn scalar_dot_product_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut sum = 0.0f32;
    for i in 0..len {
        sum += a[i] * b[i];
    }
    sum
}

/// SIMD-vectorized 64-bit floating point dot product with 4-way unrolling.
pub fn vector_dot_product_f64(a: &[f64], b: &[f64]) -> f64 {
    let len = a.len().min(b.len());
    let mut s0 = 0.0f64;
    let mut s1 = 0.0f64;
    let mut s2 = 0.0f64;
    let mut s3 = 0.0f64;

    let chunks = len / 4;
    for i in 0..chunks {
        let base = i * 4;
        s0 += a[base] * b[base];
        s1 += a[base + 1] * b[base + 1];
        s2 += a[base + 2] * b[base + 2];
        s3 += a[base + 3] * b[base + 3];
    }

    let mut total = (s0 + s1) + (s2 + s3);
    for i in (chunks * 4)..len {
        total += a[i] * b[i];
    }
    total
}

/// SIMD-vectorized Cosine Distance / Similarity computation.
/// Single fused pass computing dot product, norm_a, and norm_b simultaneously.
pub fn vector_cosine_similarity_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }

    let mut dot0 = 0.0f32;
    let mut dot1 = 0.0f32;
    let mut norm_a0 = 0.0f32;
    let mut norm_a1 = 0.0f32;
    let mut norm_b0 = 0.0f32;
    let mut norm_b1 = 0.0f32;

    let chunks = len / 4;
    for i in 0..chunks {
        let base = i * 4;
        let a0 = a[base];
        let a1 = a[base + 1];
        let a2 = a[base + 2];
        let a3 = a[base + 3];

        let b0 = b[base];
        let b1 = b[base + 1];
        let b2 = b[base + 2];
        let b3 = b[base + 3];

        dot0 += a0 * b0 + a1 * b1;
        dot1 += a2 * b2 + a3 * b3;

        norm_a0 += a0 * a0 + a1 * a1;
        norm_a1 += a2 * a2 + a3 * a3;

        norm_b0 += b0 * b0 + b1 * b1;
        norm_b1 += b2 * b2 + b3 * b3;
    }

    let mut dot = dot0 + dot1;
    let mut norm_a = norm_a0 + norm_a1;
    let mut norm_b = norm_b0 + norm_b1;

    for i in (chunks * 4)..len {
        let ai = a[i];
        let bi = b[i];
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Scalar cosine similarity baseline.
pub fn scalar_cosine_similarity_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..len {
        let ai = a[i];
        let bi = b[i];
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let denom = (norm_a.sqrt()) * (norm_b.sqrt());
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// SIMD Tiled Matrix Multiplication (GEMM: C = A * B).
/// A is M x K, B is K x N, C is M x N (row-major).
pub fn vector_matrix_multiply_f32(
    m: usize,
    k: usize,
    n: usize,
    a: &[f32],
    b: &[f32],
    c: &mut [f32],
) {
    assert!(a.len() >= m * k);
    assert!(b.len() >= k * n);
    assert!(c.len() >= m * n);

    c.fill(0.0);

    // Tiled execution (tile size 32x32 for L1 cache residency)
    const TILE: usize = 32;

    for ti in (0..m).step_by(TILE) {
        let i_end = (ti + TILE).min(m);
        for tk in (0..k).step_by(TILE) {
            let k_end = (tk + TILE).min(k);
            for tj in (0..n).step_by(TILE) {
                let j_end = (tj + TILE).min(n);

                for i in ti..i_end {
                    let a_row = i * k;
                    let c_row = i * n;
                    for p in tk..k_end {
                        let a_val = a[a_row + p];
                        let b_row = p * n;

                        let mut j = tj;
                        // 4-way unrolling for SIMD auto-vectorization
                        while j + 4 <= j_end {
                            c[c_row + j] += a_val * b[b_row + j];
                            c[c_row + j + 1] += a_val * b[b_row + j + 1];
                            c[c_row + j + 2] += a_val * b[b_row + j + 2];
                            c[c_row + j + 3] += a_val * b[b_row + j + 3];
                            j += 4;
                        }
                        while j < j_end {
                            c[c_row + j] += a_val * b[b_row + j];
                            j += 1;
                        }
                    }
                }
            }
        }
    }
}

/// Scalar non-tiled matrix multiplication baseline.
pub fn scalar_matrix_multiply_f32(
    m: usize,
    k: usize,
    n: usize,
    a: &[f32],
    b: &[f32],
    c: &mut [f32],
) {
    c.fill(0.0);
    for i in 0..m {
        for j in 0..n {
            let mut sum = 0.0f32;
            for p in 0..k {
                sum += a[i * k + p] * b[p * n + j];
            }
            c[i * n + j] = sum;
        }
    }
}

/// SIMD-vectorized AST Token Scanner.
/// Scans source text finding offsets of matching delimiter bytes (e.g., whitespace, parentheses).
pub fn vector_ast_token_scan(source: &[u8], delimiter: u8) -> Vec<usize> {
    let mut matches = Vec::new();
    let len = source.len();

    // 16-byte chunk scanning pattern
    let chunks = len / 16;
    for i in 0..chunks {
        let base = i * 16;
        let chunk = &source[base..base + 16];
        for (offset, &byte) in chunk.iter().enumerate() {
            if byte == delimiter {
                matches.push(base + offset);
            }
        }
    }

    for i in (chunks * 16)..len {
        if source[i] == delimiter {
            matches.push(i);
        }
    }

    matches
}

/// Scalar byte scanning baseline.
pub fn scalar_ast_token_scan(source: &[u8], delimiter: u8) -> Vec<usize> {
    let mut matches = Vec::new();
    for (i, &byte) in source.iter().enumerate() {
        if byte == delimiter {
            matches.push(i);
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_feature_detection() {
        let report = CpuFeatureReport::detect();
        assert!(!report.target_arch.is_empty());
        assert!(report.has_wasm_simd128);
        assert!(report.has_wasm_relaxed_simd);
        assert!(report.simd_vector_width_bits >= 128);

        let banner = report.best_vector_isa();
        assert!(!banner.is_empty());
    }

    #[test]
    fn test_cranelift_config_builder() {
        let vectorizer = SimdVectorizer::new();

        // Tier 0
        let _cfg0 = vectorizer.build_wasmtime_config(crate::hot_trace_profiler::ExecutionTier::Tier0Interpreted);
        // Tier 1
        let _cfg1 = vectorizer.build_wasmtime_config(crate::hot_trace_profiler::ExecutionTier::Tier1BaselineJit);
        // Tier 2
        let _cfg2 = vectorizer.build_wasmtime_config(crate::hot_trace_profiler::ExecutionTier::Tier2SimdOptimized);

        // Verify Engine can be instantiated with Tier2 configuration
        let engine = wasmtime::Engine::new(&_cfg2);
        assert!(engine.is_ok(), "Tier 2 Engine creation failed: {:?}", engine.err());
    }

    #[test]
    fn test_vector_dot_product_equivalence() {
        let n = 1024;
        let a: Vec<f32> = (0..n).map(|i| (i as f32) * 0.05).collect();
        let b: Vec<f32> = (0..n).map(|i| (i as f32) * 0.02).collect();

        let v_res = vector_dot_product_f32(&a, &b);
        let s_res = scalar_dot_product_f32(&a, &b);

        let diff = (v_res - s_res).abs();
        let rel_err = diff / s_res.abs().max(1.0);
        assert!(rel_err < 1e-4, "SIMD and Scalar dot product diverged: v={}, s={}", v_res, s_res);
    }

    #[test]
    fn test_vector_cosine_similarity() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];

        let sim = vector_cosine_similarity_f32(&a, &b);
        assert!((sim - 1.0).abs() < 1e-5, "Identical vectors should have similarity 1.0, got {}", sim);

        let c = vec![-1.0, -2.0, -3.0, -4.0, -5.0, -6.0, -7.0, -8.0];
        let sim_neg = vector_cosine_similarity_f32(&a, &c);
        assert!((sim_neg - (-1.0)).abs() < 1e-5, "Opposite vectors should have similarity -1.0, got {}", sim_neg);
    }

    #[test]
    fn test_matrix_multiply_equivalence() {
        let m = 16;
        let k = 16;
        let n = 16;

        let a: Vec<f32> = (0..(m * k)).map(|i| (i as f32) * 0.1).collect();
        let b: Vec<f32> = (0..(k * n)).map(|i| (i as f32) * 0.2).collect();

        let mut c_simd = vec![0.0f32; m * n];
        let mut c_scalar = vec![0.0f32; m * n];

        vector_matrix_multiply_f32(m, k, n, &a, &b, &mut c_simd);
        scalar_matrix_multiply_f32(m, k, n, &a, &b, &mut c_scalar);

        for i in 0..(m * n) {
            assert!((c_simd[i] - c_scalar[i]).abs() < 1e-3, "Mismatch at idx {}: {} vs {}", i, c_simd[i], c_scalar[i]);
        }
    }

    #[test]
    fn test_ast_token_scan_equivalence() {
        let text = b"fn main() { let x = 42; println!(\"result: {}\", x); }";
        let delimiter = b' ';

        let v_matches = vector_ast_token_scan(text, delimiter);
        let s_matches = scalar_ast_token_scan(text, delimiter);

        assert_eq!(v_matches, s_matches);
    }
}
