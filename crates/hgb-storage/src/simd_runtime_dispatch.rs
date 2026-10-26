//! # Dynamic SIMD Runtime Dispatch Engine
//!
//! High-performance SIMD-accelerated linear algebra kernels for vector distance,
//! dot product, cosine similarity, and in-place normalization.
//! Dynamically detects CPU vector extensions at runtime without requiring static
//! compiler target-cpu flags:
//! - AVX-512 Foundation (`avx512f`) on modern x86_64 CPUs (512-bit registers, 16 floats/cycle)
//! - AVX2 + Fused Multiply-Add (`avx2`, `fma`) on standard modern x86_64 CPUs (256-bit registers, 8 floats/cycle)
//! - ARM Neon (`target_arch = "aarch64"`) on 64-bit ARM architectures (128-bit registers, 4 floats/cycle)
//! - High-throughput 4-way unrolled portable scalar fallback for universal cross-platform compatibility.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

/// Recognized hardware vector instruction sets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimdArchitecture {
    /// Intel/AMD AVX-512 Foundation (512-bit SIMD registers)
    Avx512,
    /// Intel/AMD AVX2 with FMA (256-bit SIMD registers with fused multiply-add)
    Avx2Fma,
    /// ARMv8-A Neon (128-bit SIMD registers)
    Neon,
    /// High-throughput 4-way unrolled portable scalar fallback
    ScalarFallback,
}

impl std::fmt::Display for SimdArchitecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Avx512 => write!(f, "AVX-512 (avx512f)"),
            Self::Avx2Fma => write!(f, "AVX2 + FMA (256-bit)"),
            Self::Neon => write!(f, "ARM Neon (128-bit)"),
            Self::ScalarFallback => write!(f, "Scalar Fallback (4-way unrolled)"),
        }
    }
}

/// Telemetry structure representing host SIMD capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimdCapabilities {
    pub avx512f: bool,
    pub avx2: bool,
    pub fma: bool,
    pub neon: bool,
    pub selected_architecture: SimdArchitecture,
    pub selected_label: String,
}

static DETECTED_ARCH_CODE: AtomicU8 = AtomicU8::new(0);

const ARCH_UNINITIALIZED: u8 = 0;
const ARCH_SCALAR: u8 = 1;
const ARCH_NEON: u8 = 2;
const ARCH_AVX2_FMA: u8 = 3;
const ARCH_AVX512: u8 = 4;

/// Detects the highest performance SIMD architecture available on the current CPU.
/// The result is cached in an atomic cell for zero-overhead subsequent calls.
pub fn detect_simd_architecture() -> SimdArchitecture {
    let current = DETECTED_ARCH_CODE.load(Ordering::Relaxed);
    if current != ARCH_UNINITIALIZED {
        return match current {
            ARCH_AVX512 => SimdArchitecture::Avx512,
            ARCH_AVX2_FMA => SimdArchitecture::Avx2Fma,
            ARCH_NEON => SimdArchitecture::Neon,
            _ => SimdArchitecture::ScalarFallback,
        };
    }

    let detected = detect_simd_architecture_uncached();
    let code = match detected {
        SimdArchitecture::Avx512 => ARCH_AVX512,
        SimdArchitecture::Avx2Fma => ARCH_AVX2_FMA,
        SimdArchitecture::Neon => ARCH_NEON,
        SimdArchitecture::ScalarFallback => ARCH_SCALAR,
    };
    DETECTED_ARCH_CODE.store(code, Ordering::Relaxed);
    detected
}

fn detect_simd_architecture_uncached() -> SimdArchitecture {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx512f") {
            return SimdArchitecture::Avx512;
        }
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return SimdArchitecture::Avx2Fma;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return SimdArchitecture::Neon;
    }
    #[allow(unreachable_code)]
    SimdArchitecture::ScalarFallback
}

/// Returns detailed host SIMD feature telemetry
pub fn detect_simd_capabilities() -> SimdCapabilities {
    #[cfg(target_arch = "x86_64")]
    let (avx512f, avx2, fma, neon) = (
        is_x86_feature_detected!("avx512f"),
        is_x86_feature_detected!("avx2"),
        is_x86_feature_detected!("fma"),
        false,
    );

    #[cfg(target_arch = "aarch64")]
    let (avx512f, avx2, fma, neon) = (false, false, false, true);

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let (avx512f, avx2, fma, neon) = (false, false, false, false);

    let selected = detect_simd_architecture();
    SimdCapabilities {
        avx512f,
        avx2,
        fma,
        neon,
        selected_architecture: selected,
        selected_label: selected.to_string(),
    }
}

/// Returns a human-readable identifier of the active SIMD execution path
pub fn active_simd_target_name() -> &'static str {
    match detect_simd_architecture() {
        SimdArchitecture::Avx512 => "AVX-512",
        SimdArchitecture::Avx2Fma => "AVX2+FMA",
        SimdArchitecture::Neon => "ARM-Neon",
        SimdArchitecture::ScalarFallback => "Scalar-Fallback-4x",
    }
}

// ============================================================================
// Portable 4-Way Unrolled Scalar Baseline Implementations
// ============================================================================

/// Computes dot product using portable 4-way loop unrolling with independent accumulators
#[inline]
pub fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    let chunks_a = a_slice.chunks_exact(4);
    let chunks_b = b_slice.chunks_exact(4);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;

    for (ca, cb) in chunks_a.zip(chunks_b) {
        s0 += ca[0] * cb[0];
        s1 += ca[1] * cb[1];
        s2 += ca[2] * cb[2];
        s3 += ca[3] * cb[3];
    }

    let mut dot = (s0 + s1) + (s2 + s3);
    for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
        dot += x * y;
    }
    dot
}

/// Computes squared Euclidean distance using portable 4-way loop unrolling
#[inline]
pub fn euclidean_distance_sq_scalar(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    let chunks_a = a_slice.chunks_exact(4);
    let chunks_b = b_slice.chunks_exact(4);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;

    for (ca, cb) in chunks_a.zip(chunks_b) {
        let d0 = ca[0] - cb[0];
        let d1 = ca[1] - cb[1];
        let d2 = ca[2] - cb[2];
        let d3 = ca[3] - cb[3];
        s0 += d0 * d0;
        s1 += d1 * d1;
        s2 += d2 * d2;
        s3 += d3 * d3;
    }

    let mut sum_sq = (s0 + s1) + (s2 + s3);
    for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
        let d = x - y;
        sum_sq += d * d;
    }
    sum_sq
}

/// Computes Euclidean distance using portable 4-way loop unrolling
#[inline]
pub fn euclidean_distance_scalar(a: &[f32], b: &[f32]) -> f32 {
    euclidean_distance_sq_scalar(a, b).max(0.0).sqrt()
}

/// Computes cosine similarity using portable 4-way loop unrolling
#[inline]
pub fn cosine_similarity_scalar(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    let dot = dot_product_scalar(a_slice, b_slice);
    let norm_a = dot_product_scalar(a_slice, a_slice);
    let norm_b = dot_product_scalar(b_slice, b_slice);

    if norm_a <= 1e-12 || norm_b <= 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// In-place vector normalization using portable 4-way loop unrolling
#[inline]
pub fn normalize_inplace_scalar(v: &mut [f32]) {
    if v.is_empty() {
        return;
    }
    let dot = dot_product_scalar(v, v);
    if dot <= 1e-12 {
        return;
    }
    let inv_norm = 1.0 / dot.sqrt();
    let len = v.len();
    let mut i = 0;
    while i + 4 <= len {
        v[i] *= inv_norm;
        v[i + 1] *= inv_norm;
        v[i + 2] *= inv_norm;
        v[i + 3] *= inv_norm;
        i += 4;
    }
    while i < len {
        v[i] *= inv_norm;
        i += 1;
    }
}

// ============================================================================
// AVX2 + FMA SIMD Kernels (x86_64)
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
pub unsafe fn dot_product_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len().min(b.len());
    let mut i = 0;
    let mut sum0 = _mm256_setzero_ps();
    let mut sum1 = _mm256_setzero_ps();

    while i + 16 <= len {
        let va0 = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb0 = _mm256_loadu_ps(b.as_ptr().add(i));
        sum0 = _mm256_fmadd_ps(va0, vb0, sum0);

        let va1 = _mm256_loadu_ps(a.as_ptr().add(i + 8));
        let vb1 = _mm256_loadu_ps(b.as_ptr().add(i + 8));
        sum1 = _mm256_fmadd_ps(va1, vb1, sum1);
        i += 16;
    }
    if i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        sum0 = _mm256_fmadd_ps(va, vb, sum0);
        i += 8;
    }
    sum0 = _mm256_add_ps(sum0, sum1);

    let valow = _mm256_castps256_ps128(sum0);
    let vahigh = _mm256_extractf128_ps(sum0, 1);
    let sum128 = _mm_add_ps(valow, vahigh);
    let shuf = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
    let sum32 = _mm_add_ss(sum64, shuf2);
    let mut dot = _mm_cvtss_f32(sum32);

    while i < len {
        dot += a[i] * b[i];
        i += 1;
    }
    dot
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
pub unsafe fn euclidean_distance_sq_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len().min(b.len());
    let mut i = 0;
    let mut sum0 = _mm256_setzero_ps();
    let mut sum1 = _mm256_setzero_ps();

    while i + 16 <= len {
        let va0 = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb0 = _mm256_loadu_ps(b.as_ptr().add(i));
        let diff0 = _mm256_sub_ps(va0, vb0);
        sum0 = _mm256_fmadd_ps(diff0, diff0, sum0);

        let va1 = _mm256_loadu_ps(a.as_ptr().add(i + 8));
        let vb1 = _mm256_loadu_ps(b.as_ptr().add(i + 8));
        let diff1 = _mm256_sub_ps(va1, vb1);
        sum1 = _mm256_fmadd_ps(diff1, diff1, sum1);
        i += 16;
    }
    if i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        let diff = _mm256_sub_ps(va, vb);
        sum0 = _mm256_fmadd_ps(diff, diff, sum0);
        i += 8;
    }
    sum0 = _mm256_add_ps(sum0, sum1);

    let valow = _mm256_castps256_ps128(sum0);
    let vahigh = _mm256_extractf128_ps(sum0, 1);
    let sum128 = _mm_add_ps(valow, vahigh);
    let shuf = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
    let sum32 = _mm_add_ss(sum64, shuf2);
    let mut sum_sq = _mm_cvtss_f32(sum32);

    while i < len {
        let diff = a[i] - b[i];
        sum_sq += diff * diff;
        i += 1;
    }
    sum_sq
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
pub unsafe fn normalize_inplace_avx2_fma(v: &mut [f32]) {
    use std::arch::x86_64::*;
    let dot = dot_product_avx2_fma(v, v);
    if dot <= 1e-12 {
        return;
    }
    let inv_norm = 1.0 / dot.sqrt();
    let vinv = _mm256_set1_ps(inv_norm);
    let len = v.len();
    let mut i = 0;
    while i + 8 <= len {
        let val = _mm256_loadu_ps(v.as_ptr().add(i));
        let scaled = _mm256_mul_ps(val, vinv);
        _mm256_storeu_ps(v.as_mut_ptr().add(i), scaled);
        i += 8;
    }
    while i < len {
        v[i] *= inv_norm;
        i += 1;
    }
}

// ============================================================================
// AVX-512 SIMD Kernels (x86_64)
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
pub unsafe fn dot_product_avx512(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len().min(b.len());
    let mut i = 0;
    let mut sum512 = _mm512_setzero_ps();

    while i + 16 <= len {
        let va = _mm512_loadu_ps(a.as_ptr().add(i));
        let vb = _mm512_loadu_ps(b.as_ptr().add(i));
        sum512 = _mm512_fmadd_ps(va, vb, sum512);
        i += 16;
    }

    let low256 = _mm512_castps512_ps256(sum512);
    let high256 = _mm512_extractf32x8_ps(sum512, 1);
    let sum256 = _mm256_add_ps(low256, high256);

    let valow = _mm256_castps256_ps128(sum256);
    let vahigh = _mm256_extractf128_ps(sum256, 1);
    let sum128 = _mm_add_ps(valow, vahigh);
    let shuf = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
    let sum32 = _mm_add_ss(sum64, shuf2);
    let mut dot = _mm_cvtss_f32(sum32);

    while i < len {
        dot += a[i] * b[i];
        i += 1;
    }
    dot
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
pub unsafe fn euclidean_distance_sq_avx512(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len().min(b.len());
    let mut i = 0;
    let mut sum512 = _mm512_setzero_ps();

    while i + 16 <= len {
        let va = _mm512_loadu_ps(a.as_ptr().add(i));
        let vb = _mm512_loadu_ps(b.as_ptr().add(i));
        let diff = _mm512_sub_ps(va, vb);
        sum512 = _mm512_fmadd_ps(diff, diff, sum512);
        i += 16;
    }

    let low256 = _mm512_castps512_ps256(sum512);
    let high256 = _mm512_extractf32x8_ps(sum512, 1);
    let sum256 = _mm256_add_ps(low256, high256);

    let valow = _mm256_castps256_ps128(sum256);
    let vahigh = _mm256_extractf128_ps(sum256, 1);
    let sum128 = _mm_add_ps(valow, vahigh);
    let shuf = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
    let sum32 = _mm_add_ss(sum64, shuf2);
    let mut sum_sq = _mm_cvtss_f32(sum32);

    while i < len {
        let diff = a[i] - b[i];
        sum_sq += diff * diff;
        i += 1;
    }
    sum_sq
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
pub unsafe fn normalize_inplace_avx512(v: &mut [f32]) {
    use std::arch::x86_64::*;
    let dot = dot_product_avx512(v, v);
    if dot <= 1e-12 {
        return;
    }
    let inv_norm = 1.0 / dot.sqrt();
    let vinv = _mm512_set1_ps(inv_norm);
    let len = v.len();
    let mut i = 0;
    while i + 16 <= len {
        let val = _mm512_loadu_ps(v.as_ptr().add(i));
        let scaled = _mm512_mul_ps(val, vinv);
        _mm512_storeu_ps(v.as_mut_ptr().add(i), scaled);
        i += 16;
    }
    while i < len {
        v[i] *= inv_norm;
        i += 1;
    }
}

// ============================================================================
// ARM Neon SIMD Kernels (aarch64)
// ============================================================================

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub unsafe fn dot_product_neon(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let len = a.len().min(b.len());
    let mut i = 0;
    let mut sum0 = vdupq_n_f32(0.0);
    let mut sum1 = vdupq_n_f32(0.0);

    while i + 8 <= len {
        let va0 = vld1q_f32(a.as_ptr().add(i));
        let vb0 = vld1q_f32(b.as_ptr().add(i));
        sum0 = vfmaq_f32(sum0, va0, vb0);

        let va1 = vld1q_f32(a.as_ptr().add(i + 4));
        let vb1 = vld1q_f32(b.as_ptr().add(i + 4));
        sum1 = vfmaq_f32(sum1, va1, vb1);
        i += 8;
    }
    if i + 4 <= len {
        let va0 = vld1q_f32(a.as_ptr().add(i));
        let vb0 = vld1q_f32(b.as_ptr().add(i));
        sum0 = vfmaq_f32(sum0, va0, vb0);
        i += 4;
    }
    let sum = vaddq_f32(sum0, sum1);
    let mut dot = vaddvq_f32(sum);

    while i < len {
        dot += a[i] * b[i];
        i += 1;
    }
    dot
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub unsafe fn euclidean_distance_sq_neon(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let len = a.len().min(b.len());
    let mut i = 0;
    let mut sum0 = vdupq_n_f32(0.0);
    let mut sum1 = vdupq_n_f32(0.0);

    while i + 8 <= len {
        let va0 = vld1q_f32(a.as_ptr().add(i));
        let vb0 = vld1q_f32(b.as_ptr().add(i));
        let diff0 = vsubq_f32(va0, vb0);
        sum0 = vfmaq_f32(sum0, diff0, diff0);

        let va1 = vld1q_f32(a.as_ptr().add(i + 4));
        let vb1 = vld1q_f32(b.as_ptr().add(i + 4));
        let diff1 = vsubq_f32(va1, vb1);
        sum1 = vfmaq_f32(sum1, diff1, diff1);
        i += 8;
    }
    if i + 4 <= len {
        let va0 = vld1q_f32(a.as_ptr().add(i));
        let vb0 = vld1q_f32(b.as_ptr().add(i));
        let diff0 = vsubq_f32(va0, vb0);
        sum0 = vfmaq_f32(sum0, diff0, diff0);
        i += 4;
    }
    let sum = vaddq_f32(sum0, sum1);
    let mut sum_sq = vaddvq_f32(sum);

    while i < len {
        let diff = a[i] - b[i];
        sum_sq += diff * diff;
        i += 1;
    }
    sum_sq
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub unsafe fn normalize_inplace_neon(v: &mut [f32]) {
    use std::arch::aarch64::*;
    let dot = dot_product_neon(v, v);
    if dot <= 1e-12 {
        return;
    }
    let inv_norm = 1.0 / dot.sqrt();
    let vinv = vdupq_n_f32(inv_norm);
    let len = v.len();
    let mut i = 0;
    while i + 4 <= len {
        let val = vld1q_f32(v.as_ptr().add(i));
        let scaled = vmulq_f32(val, vinv);
        vst1q_f32(v.as_mut_ptr().add(i), scaled);
        i += 4;
    }
    while i < len {
        v[i] *= inv_norm;
        i += 1;
    }
}

// ============================================================================
// Public Dynamically Dispatched High-Level API
// ============================================================================

/// Dynamically dispatched vector dot product using highest available SIMD features
#[inline]
pub fn simd_dot_product(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    match detect_simd_architecture() {
        #[cfg(target_arch = "x86_64")]
        SimdArchitecture::Avx512 => unsafe { dot_product_avx512(a_slice, b_slice) },
        #[cfg(target_arch = "x86_64")]
        SimdArchitecture::Avx2Fma => unsafe { dot_product_avx2_fma(a_slice, b_slice) },
        #[cfg(target_arch = "aarch64")]
        SimdArchitecture::Neon => unsafe { dot_product_neon(a_slice, b_slice) },
        _ => dot_product_scalar(a_slice, b_slice),
    }
}

/// Dynamically dispatched cosine similarity: range `[-1.0, 1.0]`
#[inline]
pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    let dot = simd_dot_product(a_slice, b_slice);
    let norm_a = simd_dot_product(a_slice, a_slice);
    let norm_b = simd_dot_product(b_slice, b_slice);

    if norm_a <= 1e-12 || norm_b <= 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// Dynamically dispatched squared Euclidean distance
#[inline]
pub fn simd_euclidean_distance_sq(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    match detect_simd_architecture() {
        #[cfg(target_arch = "x86_64")]
        SimdArchitecture::Avx512 => unsafe { euclidean_distance_sq_avx512(a_slice, b_slice) },
        #[cfg(target_arch = "x86_64")]
        SimdArchitecture::Avx2Fma => unsafe { euclidean_distance_sq_avx2_fma(a_slice, b_slice) },
        #[cfg(target_arch = "aarch64")]
        SimdArchitecture::Neon => unsafe { euclidean_distance_sq_neon(a_slice, b_slice) },
        _ => euclidean_distance_sq_scalar(a_slice, b_slice),
    }
}

/// Dynamically dispatched Euclidean distance: range `[0.0, +inf)`
#[inline]
pub fn simd_euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    simd_euclidean_distance_sq(a, b).max(0.0).sqrt()
}

/// Dynamically dispatched in-place L2 vector normalization
#[inline]
pub fn simd_normalize_inplace(v: &mut [f32]) {
    if v.is_empty() {
        return;
    }
    match detect_simd_architecture() {
        #[cfg(target_arch = "x86_64")]
        SimdArchitecture::Avx512 => unsafe { normalize_inplace_avx512(v) },
        #[cfg(target_arch = "x86_64")]
        SimdArchitecture::Avx2Fma => unsafe { normalize_inplace_avx2_fma(v) },
        #[cfg(target_arch = "aarch64")]
        SimdArchitecture::Neon => unsafe { normalize_inplace_neon(v) },
        _ => normalize_inplace_scalar(v),
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
pub mod tests {
    use super::*;

    fn generate_pseudorandom_vector(dim: usize, seed: u64) -> Vec<f32> {
        let mut v = Vec::with_capacity(dim);
        let mut state = seed;
        for _ in 0..dim {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let val = ((state >> 32) as i32 as f32) / (i32::MAX as f32);
            v.push(val);
        }
        v
    }

    #[test]
    fn test_simd_feature_detection() {
        let caps = detect_simd_capabilities();
        println!("Detected SIMD architecture: {}", caps.selected_label);
        println!(
            "Caps: avx512f={}, avx2={}, fma={}, neon={}",
            caps.avx512f, caps.avx2, caps.fma, caps.neon
        );
        assert!(!caps.selected_label.is_empty());
        let arch = detect_simd_architecture();
        assert_eq!(arch, caps.selected_architecture);
    }

    #[test]
    fn test_simd_scalar_consistency_all_dimensions() {
        let dimensions = [128, 384, 768, 1536];

        for &dim in &dimensions {
            let a = generate_pseudorandom_vector(dim, 1000 + dim as u64);
            let b = generate_pseudorandom_vector(dim, 2000 + dim as u64);

            let dot_scalar = dot_product_scalar(&a, &b);
            let dot_simd = simd_dot_product(&a, &b);
            let diff_dot = (dot_scalar - dot_simd).abs();
            assert!(
                diff_dot < 1e-4,
                "Dot product mismatch for dim {}: scalar={}, simd={}, diff={}",
                dim, dot_scalar, dot_simd, diff_dot
            );

            let cos_scalar = cosine_similarity_scalar(&a, &b);
            let cos_simd = simd_cosine_similarity(&a, &b);
            let diff_cos = (cos_scalar - cos_simd).abs();
            assert!(
                diff_cos < 1e-5,
                "Cosine similarity mismatch for dim {}: scalar={}, simd={}, diff={}",
                dim, cos_scalar, cos_simd, diff_cos
            );

            let dist_scalar = euclidean_distance_scalar(&a, &b);
            let dist_simd = simd_euclidean_distance(&a, &b);
            let diff_dist = (dist_scalar - dist_simd).abs();
            assert!(
                diff_dist < 1e-4,
                "Euclidean distance mismatch for dim {}: scalar={}, simd={}, diff={}",
                dim, dist_scalar, dist_simd, diff_dist
            );

            // Inplace normalization consistency
            let mut v_scalar = a.clone();
            normalize_inplace_scalar(&mut v_scalar);
            let mut v_simd = a.clone();
            simd_normalize_inplace(&mut v_simd);

            for i in 0..dim {
                assert!(
                    (v_scalar[i] - v_simd[i]).abs() < 1e-5,
                    "Normalized mismatch at index {} for dim {}: scalar={}, simd={}",
                    i, dim, v_scalar[i], v_simd[i]
                );
            }

            let final_norm_scalar = dot_product_scalar(&v_scalar, &v_scalar).sqrt();
            let final_norm_simd = simd_dot_product(&v_simd, &v_simd).sqrt();
            assert!((final_norm_scalar - 1.0).abs() < 1e-5);
            assert!((final_norm_simd - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn test_zero_and_empty_vectors() {
        let empty_a: Vec<f32> = vec![];
        let empty_b: Vec<f32> = vec![];

        assert_eq!(simd_dot_product(&empty_a, &empty_b), 0.0);
        assert_eq!(simd_cosine_similarity(&empty_a, &empty_b), 0.0);
        assert_eq!(simd_euclidean_distance(&empty_a, &empty_b), 0.0);

        let mut v_empty: Vec<f32> = vec![];
        simd_normalize_inplace(&mut v_empty);
        assert!(v_empty.is_empty());

        let zeros_a = vec![0.0f32; 128];
        let zeros_b = vec![0.0f32; 128];
        assert_eq!(simd_dot_product(&zeros_a, &zeros_b), 0.0);
        assert_eq!(simd_cosine_similarity(&zeros_a, &zeros_b), 0.0);
        assert_eq!(simd_euclidean_distance(&zeros_a, &zeros_b), 0.0);

        let mut v_zeros = vec![0.0f32; 128];
        simd_normalize_inplace(&mut v_zeros);
        for &x in &v_zeros {
            assert_eq!(x, 0.0);
        }
    }

    #[test]
    fn test_identical_and_orthogonal_vectors() {
        let v1 = vec![1.0, 0.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0, 0.0];
        let v3 = vec![1.0, 0.0, 0.0, 0.0];

        // Identical
        assert!((simd_cosine_similarity(&v1, &v3) - 1.0).abs() < 1e-6);
        assert_eq!(simd_euclidean_distance(&v1, &v3), 0.0);

        // Orthogonal
        assert_eq!(simd_dot_product(&v1, &v2), 0.0);
        assert_eq!(simd_cosine_similarity(&v1, &v2), 0.0);
        assert!((simd_euclidean_distance(&v1, &v2) - 2.0f32.sqrt()).abs() < 1e-5);
    }
}
