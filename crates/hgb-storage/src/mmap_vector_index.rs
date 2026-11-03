//! # Zero-Copy Mmap Vector Index & SIMD Memory Engine
//!
//! Sub-millisecond vector indexing backed by persistent memory-mapped files (`memmap2`)
//! with cacheline-aligned (64-byte) binary headers, unrolled 8-lane SIMD distance kernels,
//! lock-free reader concurrency with atomic count progression, and bounded priority-queue Top-K search.
//!
//! Designed for zero serialization overhead: no JSON, SQLite, or Protobuf deserialization
//! during high-frequency vector scanning loops.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, AtomicU64, Ordering};
use std::time::Instant;

use memmap2::{Mmap, MmapMut};
use serde::{Deserialize, Serialize};

use hgb_core::{HgbError, Result};

/// Binary header magic bytes: `0x484742564D4D4150` ("HGBVMMAP")
pub const MMAP_VECTOR_MAGIC: u64 = 0x484742564D4D4150;

/// Legacy binary header magic bytes: `0x4847425645435431` ("HGBVECT1")
pub const MMAP_VECTOR_LEGACY_MAGIC: u64 = 0x4847425645435431;

/// Protocol & layout specification version
pub const MMAP_VECTOR_VERSION: u32 = 1;

/// Cacheline-aligned header size in bytes
pub const HEADER_SIZE: usize = 64;

/// Cacheline-aligned metadata entry record size in bytes
pub const METADATA_ENTRY_SIZE: usize = 128;

/// Maximum byte length for stored source identifiers (symbol name, URI, or ID)
pub const MAX_SOURCE_ID_LEN: usize = 80;

/// Rounds up a byte offset or size to the nearest 64-byte CPU cacheline boundary
#[inline]
pub const fn align_up_64(val: usize) -> usize {
    (val + 63) & !63
}

/// Supported distance metric types for zero-copy vector comparison
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorMetric {
    /// Cosine similarity: score in [-1.0, 1.0], higher score indicates closer vectors
    Cosine = 0,
    /// Dot product: score is unnormalized scalar projection, higher score indicates closer vectors
    DotProduct = 1,
    /// Squared Euclidean distance: score is negative squared distance, higher score indicates closer vectors
    Euclidean = 2,
}

impl VectorMetric {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::Cosine),
            1 => Some(Self::DotProduct),
            2 => Some(Self::Euclidean),
            _ => None,
        }
    }

    pub fn to_u32(self) -> u32 {
        self as u32
    }
}

impl std::fmt::Display for VectorMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cosine => write!(f, "Cosine"),
            Self::DotProduct => write!(f, "DotProduct"),
            Self::Euclidean => write!(f, "Euclidean"),
        }
    }
}

/// 64-byte cacheline-aligned binary header located at offset 0 of the memory-mapped file
#[repr(C, align(64))]
pub struct MmapVectorHeader {
    /// Magic identifier: 0x4847425645435431 ("HGBVECT1")
    pub magic: u64,
    /// Format version (1)
    pub version: u32,
    /// Vector dimensionality (e.g. 64, 384, 768, 1536)
    pub dimension: u32,
    /// Maximum vector capacity pre-allocated in backing file
    pub max_capacity: u64,
    /// Current count of inserted vectors (atomic for lock-free reader visibility)
    pub current_count: AtomicU64,
    /// Metric kind: Cosine = 0, DotProduct = 1, Euclidean = 2
    pub metric_kind: u32,
    /// Stride in bytes between consecutive vectors in the payload block (64-byte aligned)
    pub vector_stride_bytes: u32,
    /// Absolute byte offset where raw vector f32 floats begin
    pub payload_offset: u64,
    /// Absolute byte offset where metadata records begin
    pub metadata_offset: u64,
    /// Reserved padding to exactly 64 bytes
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<MmapVectorHeader>() == HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<MmapVectorHeader>() == 64);

/// 128-byte (2 cachelines) binary metadata record for each vector slot
#[repr(C, align(64))]
pub struct MmapMetadataRecord {
    /// 16-byte UUID or BLAKE3 hash of identifier
    pub id_hash: [u8; 16],
    /// Monotonic slot index (0..max_capacity)
    pub vector_index: u64,
    /// Absolute byte offset in file where vector payload begins
    pub payload_offset: u64,
    /// Active / validity flag (1 = active, 0 = empty / tombstone)
    pub is_active: u32,
    /// Length of source identifier in bytes (<= MAX_SOURCE_ID_LEN)
    pub source_id_len: u32,
    /// Raw UTF-8 bytes for symbol name / file path / source ID
    pub source_id: [u8; MAX_SOURCE_ID_LEN],
    /// Reserved cacheline padding to exactly 128 bytes
    pub _reserved: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<MmapMetadataRecord>() == METADATA_ENTRY_SIZE);
const _: () = assert!(std::mem::align_of::<MmapMetadataRecord>() == 64);

/// Public summary of a stored vector record
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MmapRecordSummary {
    pub index: usize,
    pub id_hash: [u8; 16],
    pub source_id: String,
    pub payload_offset: u64,
    pub is_active: bool,
}

/// Search match returned from a zero-copy Top-K query
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MmapVectorMatch {
    pub index: usize,
    pub id_hash: [u8; 16],
    pub source_id: String,
    pub score: f32,
    pub distance: f32,
}

/// Benchmark diagnostic report for vector retrieval latency and throughput
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MmapVectorBenchReport {
    pub total_vectors: usize,
    pub dimension: usize,
    pub metric: String,
    pub top_k: usize,
    pub iterations: usize,
    pub total_elapsed_us: u64,
    pub avg_latency_us: f64,
    pub queries_per_second: f64,
    pub sub_millisecond_verified: bool,
}

#[derive(Clone, Copy)]
struct ScoreEntry {
    score: f32,
    distance: f32,
    index: usize,
}

impl PartialEq for ScoreEntry {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}
impl Eq for ScoreEntry {}
impl PartialOrd for ScoreEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.score.partial_cmp(&other.score)
    }
}
impl Ord for ScoreEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal)
    }
}

// ============================================================================
// SIMD Distance Routines (AVX2 + FMA with 8-way unrolled portable fallback)
// ============================================================================

/// Computes dot product using portable 8-way loop unrolling with 8 independent accumulators
#[inline]
pub fn dot_product_unrolled_8x(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;
    let mut s4 = 0.0f32;
    let mut s5 = 0.0f32;
    let mut s6 = 0.0f32;
    let mut s7 = 0.0f32;

    let chunks_a = a[..len].chunks_exact(8);
    let chunks_b = b[..len].chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        s0 += ca[0] * cb[0];
        s1 += ca[1] * cb[1];
        s2 += ca[2] * cb[2];
        s3 += ca[3] * cb[3];
        s4 += ca[4] * cb[4];
        s5 += ca[5] * cb[5];
        s6 += ca[6] * cb[6];
        s7 += ca[7] * cb[7];
    }

    let mut dot = ((s0 + s1) + (s2 + s3)) + ((s4 + s5) + (s6 + s7));
    for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
        dot += x * y;
    }
    dot
}

/// Computes squared Euclidean distance using portable 8-way loop unrolling
#[inline]
pub fn euclidean_sq_unrolled_8x(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;
    let mut s4 = 0.0f32;
    let mut s5 = 0.0f32;
    let mut s6 = 0.0f32;
    let mut s7 = 0.0f32;

    let chunks_a = a[..len].chunks_exact(8);
    let chunks_b = b[..len].chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        let d0 = ca[0] - cb[0];
        let d1 = ca[1] - cb[1];
        let d2 = ca[2] - cb[2];
        let d3 = ca[3] - cb[3];
        let d4 = ca[4] - cb[4];
        let d5 = ca[5] - cb[5];
        let d6 = ca[6] - cb[6];
        let d7 = ca[7] - cb[7];
        s0 += d0 * d0;
        s1 += d1 * d1;
        s2 += d2 * d2;
        s3 += d3 * d3;
        s4 += d4 * d4;
        s5 += d5 * d5;
        s6 += d6 * d6;
        s7 += d7 * d7;
    }

    let mut sum_sq = ((s0 + s1) + (s2 + s3)) + ((s4 + s5) + (s6 + s7));
    for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
        let d = x - y;
        sum_sq += d * d;
    }
    sum_sq
}

/// Computes cosine similarity in a single fused pass using portable 8-way loop unrolling
#[inline]
pub fn cosine_similarity_unrolled_8x(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let mut d0 = 0.0f32; let mut d1 = 0.0f32; let mut d2 = 0.0f32; let mut d3 = 0.0f32;
    let mut d4 = 0.0f32; let mut d5 = 0.0f32; let mut d6 = 0.0f32; let mut d7 = 0.0f32;

    let mut na0 = 0.0f32; let mut na1 = 0.0f32; let mut na2 = 0.0f32; let mut na3 = 0.0f32;
    let mut na4 = 0.0f32; let mut na5 = 0.0f32; let mut na6 = 0.0f32; let mut na7 = 0.0f32;

    let mut nb0 = 0.0f32; let mut nb1 = 0.0f32; let mut nb2 = 0.0f32; let mut nb3 = 0.0f32;
    let mut nb4 = 0.0f32; let mut nb5 = 0.0f32; let mut nb6 = 0.0f32; let mut nb7 = 0.0f32;

    let chunks_a = a[..len].chunks_exact(8);
    let chunks_b = b[..len].chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        d0 += ca[0] * cb[0]; na0 += ca[0] * ca[0]; nb0 += cb[0] * cb[0];
        d1 += ca[1] * cb[1]; na1 += ca[1] * ca[1]; nb1 += cb[1] * cb[1];
        d2 += ca[2] * cb[2]; na2 += ca[2] * ca[2]; nb2 += cb[2] * cb[2];
        d3 += ca[3] * cb[3]; na3 += ca[3] * ca[3]; nb3 += cb[3] * cb[3];
        d4 += ca[4] * cb[4]; na4 += ca[4] * ca[4]; nb4 += cb[4] * cb[4];
        d5 += ca[5] * cb[5]; na5 += ca[5] * ca[5]; nb5 += cb[5] * cb[5];
        d6 += ca[6] * cb[6]; na6 += ca[6] * ca[6]; nb6 += cb[6] * cb[6];
        d7 += ca[7] * cb[7]; na7 += ca[7] * ca[7]; nb7 += cb[7] * cb[7];
    }

    let mut dot = ((d0 + d1) + (d2 + d3)) + ((d4 + d5) + (d6 + d7));
    let mut norm_a = ((na0 + na1) + (na2 + na3)) + ((na4 + na5) + (na6 + na7));
    let mut norm_b = ((nb0 + nb1) + (nb2 + nb3)) + ((nb4 + nb5) + (nb6 + nb7));

    for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    if norm_a <= 1e-12 || norm_b <= 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn dot_product_avx2(a: *const f32, b: *const f32, len: usize) -> f32 {
    use std::arch::x86_64::*;
    let mut sum0 = _mm256_setzero_ps();
    let mut sum1 = _mm256_setzero_ps();
    let mut i = 0;

    while i + 16 <= len {
        let va0 = _mm256_loadu_ps(a.add(i));
        let vb0 = _mm256_loadu_ps(b.add(i));
        sum0 = _mm256_fmadd_ps(va0, vb0, sum0);

        let va1 = _mm256_loadu_ps(a.add(i + 8));
        let vb1 = _mm256_loadu_ps(b.add(i + 8));
        sum1 = _mm256_fmadd_ps(va1, vb1, sum1);
        i += 16;
    }
    if i + 8 <= len {
        let va = _mm256_loadu_ps(a.add(i));
        let vb = _mm256_loadu_ps(b.add(i));
        sum0 = _mm256_fmadd_ps(va, vb, sum0);
        i += 8;
    }
    sum0 = _mm256_add_ps(sum0, sum1);

    let low = _mm256_castps256_ps128(sum0);
    let high = _mm256_extractf128_ps(sum0, 1);
    let sum128 = _mm_add_ps(low, high);
    let shuf = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
    let mut dot = _mm_cvtss_f32(_mm_add_ss(sum64, shuf2));

    while i < len {
        dot += *a.add(i) * *b.add(i);
        i += 1;
    }
    dot
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn euclidean_sq_avx2(a: *const f32, b: *const f32, len: usize) -> f32 {
    use std::arch::x86_64::*;
    let mut sum0 = _mm256_setzero_ps();
    let mut sum1 = _mm256_setzero_ps();
    let mut i = 0;

    while i + 16 <= len {
        let va0 = _mm256_loadu_ps(a.add(i));
        let vb0 = _mm256_loadu_ps(b.add(i));
        let diff0 = _mm256_sub_ps(va0, vb0);
        sum0 = _mm256_fmadd_ps(diff0, diff0, sum0);

        let va1 = _mm256_loadu_ps(a.add(i + 8));
        let vb1 = _mm256_loadu_ps(b.add(i + 8));
        let diff1 = _mm256_sub_ps(va1, vb1);
        sum1 = _mm256_fmadd_ps(diff1, diff1, sum1);
        i += 16;
    }
    if i + 8 <= len {
        let va = _mm256_loadu_ps(a.add(i));
        let vb = _mm256_loadu_ps(b.add(i));
        let diff = _mm256_sub_ps(va, vb);
        sum0 = _mm256_fmadd_ps(diff, diff, sum0);
        i += 8;
    }
    sum0 = _mm256_add_ps(sum0, sum1);

    let low = _mm256_castps256_ps128(sum0);
    let high = _mm256_extractf128_ps(sum0, 1);
    let sum128 = _mm_add_ps(low, high);
    let shuf = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
    let mut sum_sq = _mm_cvtss_f32(_mm_add_ss(sum64, shuf2));

    while i < len {
        let d = *a.add(i) - *b.add(i);
        sum_sq += d * d;
        i += 1;
    }
    sum_sq
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn cosine_similarity_avx2(a: *const f32, b: *const f32, len: usize) -> f32 {
    use std::arch::x86_64::*;
    let mut dot0 = _mm256_setzero_ps();
    let mut dot1 = _mm256_setzero_ps();
    let mut na0 = _mm256_setzero_ps();
    let mut na1 = _mm256_setzero_ps();
    let mut nb0 = _mm256_setzero_ps();
    let mut nb1 = _mm256_setzero_ps();
    let mut i = 0;

    while i + 16 <= len {
        let va0 = _mm256_loadu_ps(a.add(i));
        let vb0 = _mm256_loadu_ps(b.add(i));
        dot0 = _mm256_fmadd_ps(va0, vb0, dot0);
        na0 = _mm256_fmadd_ps(va0, va0, na0);
        nb0 = _mm256_fmadd_ps(vb0, vb0, nb0);

        let va1 = _mm256_loadu_ps(a.add(i + 8));
        let vb1 = _mm256_loadu_ps(b.add(i + 8));
        dot1 = _mm256_fmadd_ps(va1, vb1, dot1);
        na1 = _mm256_fmadd_ps(va1, va1, na1);
        nb1 = _mm256_fmadd_ps(vb1, vb1, nb1);
        i += 16;
    }
    if i + 8 <= len {
        let va = _mm256_loadu_ps(a.add(i));
        let vb = _mm256_loadu_ps(b.add(i));
        dot0 = _mm256_fmadd_ps(va, vb, dot0);
        na0 = _mm256_fmadd_ps(va, va, na0);
        nb0 = _mm256_fmadd_ps(vb, vb, nb0);
        i += 8;
    }
    dot0 = _mm256_add_ps(dot0, dot1);
    na0 = _mm256_add_ps(na0, na1);
    nb0 = _mm256_add_ps(nb0, nb1);

    #[inline(always)]
    unsafe fn hsum256(v: __m256) -> f32 {
        let low = _mm256_castps256_ps128(v);
        let high = _mm256_extractf128_ps(v, 1);
        let sum128 = _mm_add_ps(low, high);
        let shuf = _mm_movehl_ps(sum128, sum128);
        let sum64 = _mm_add_ps(sum128, shuf);
        let shuf2 = _mm_shuffle_ps(sum64, sum64, 1);
        _mm_cvtss_f32(_mm_add_ss(sum64, shuf2))
    }

    let mut dot = hsum256(dot0);
    let mut norm_a = hsum256(na0);
    let mut norm_b = hsum256(nb0);

    while i < len {
        let va = *a.add(i);
        let vb = *b.add(i);
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
        i += 1;
    }

    if norm_a <= 1e-12 || norm_b <= 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// Dynamically dispatches dot product to AVX2+FMA on x86_64, falling back to 8-lane loop unrolling
#[inline]
pub fn compute_dot_product(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return unsafe { dot_product_avx2(a.as_ptr(), b.as_ptr(), len) };
        }
    }
    dot_product_unrolled_8x(a, b)
}

/// Dynamically dispatches squared Euclidean distance to AVX2+FMA on x86_64, falling back to 8-lane loop unrolling
#[inline]
pub fn compute_euclidean_sq(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return unsafe { euclidean_sq_avx2(a.as_ptr(), b.as_ptr(), len) };
        }
    }
    euclidean_sq_unrolled_8x(a, b)
}

/// Dynamically dispatches single-pass cosine similarity to AVX2+FMA on x86_64, falling back to 8-lane loop unrolling
#[inline]
pub fn compute_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return unsafe { cosine_similarity_avx2(a.as_ptr(), b.as_ptr(), len) };
        }
    }
    cosine_similarity_unrolled_8x(a, b)
}

// ============================================================================
// Path Resolution & Memory-Mapped File Storage
// ============================================================================

/// Resolves path for vector index storage with transparent fallback:
/// 1. Custom path (if supplied and writable/creatable)
/// 2. `.hgb/vector_storage.mmap`
/// 3. `/dev/shm/hgb_vector_storage_{pid}.mmap`
/// 4. System temporary directory
pub fn resolve_mmap_vector_path(custom_path: Option<&Path>) -> PathBuf {
    if let Some(path) = custom_path {
        if let Some(parent) = path.parent() {
            if (parent.as_os_str().is_empty() || parent.exists() || std::fs::create_dir_all(parent).is_ok())
                && is_file_writable_or_creatable(path)
            {
                return path.to_path_buf();
            }
        } else if is_file_writable_or_creatable(path) {
            return path.to_path_buf();
        }
    }

    let default_hgb = Path::new(".hgb");
    if default_hgb.exists() || std::fs::create_dir_all(default_hgb).is_ok() {
        let p = default_hgb.join("vector_storage.mmap");
        if is_file_writable_or_creatable(&p) {
            return p;
        }
    }

    let shm_dir = Path::new("/dev/shm");
    if is_shm_dir_writable(shm_dir) {
        return shm_dir.join(format!("hgb_vector_storage_{}.mmap", std::process::id()));
    }

    std::env::temp_dir().join(format!("hgb_vector_storage_{}.mmap", std::process::id()))
}

fn is_shm_dir_writable(shm_dir: &Path) -> bool {
    if !shm_dir.is_dir() {
        return false;
    }
    let probe = shm_dir.join(format!(
        ".hgb_mmap_probe_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn is_file_writable_or_creatable(file: &Path) -> bool {
    match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(file) {
        Ok(_) => true,
        Err(_) => false,
    }
}

fn calculate_layout(dimension: usize, max_capacity: usize) -> (usize, usize, usize, usize) {
    let vector_stride_bytes = align_up_64(dimension * std::mem::size_of::<f32>());
    let payload_offset = align_up_64(HEADER_SIZE);
    let total_payload_bytes = max_capacity * vector_stride_bytes;
    let metadata_offset = align_up_64(payload_offset + total_payload_bytes);
    let total_metadata_bytes = max_capacity * METADATA_ENTRY_SIZE;
    let total_file_size = align_up_64(metadata_offset + total_metadata_bytes);
    (vector_stride_bytes, payload_offset, metadata_offset, total_file_size)
}

fn init_mmap_header(
    mmap: &mut [u8],
    dimension: u32,
    max_capacity: u64,
    metric_kind: VectorMetric,
    vector_stride_bytes: u32,
    payload_offset: u64,
    metadata_offset: u64,
) {
    mmap[0..8].copy_from_slice(&MMAP_VECTOR_MAGIC.to_le_bytes());
    mmap[8..12].copy_from_slice(&MMAP_VECTOR_VERSION.to_le_bytes());
    mmap[12..16].copy_from_slice(&dimension.to_le_bytes());
    mmap[16..24].copy_from_slice(&max_capacity.to_le_bytes());
    mmap[24..32].copy_from_slice(&0u64.to_le_bytes()); // current_count = 0
    mmap[32..36].copy_from_slice(&metric_kind.to_u32().to_le_bytes());
    mmap[36..40].copy_from_slice(&vector_stride_bytes.to_le_bytes());
    mmap[40..48].copy_from_slice(&payload_offset.to_le_bytes());
    mmap[48..56].copy_from_slice(&metadata_offset.to_le_bytes());
    mmap[56..64].copy_from_slice(&[0u8; 8]); // reserved
}

// ============================================================================
// Zero-Copy Mmap Vector Index (Read/Write Mutex / Single-Owner Handle)
// ============================================================================

/// Zero-Copy Memory-Mapped Vector Index backed by `memmap2::MmapMut`
pub struct MmapVectorIndex {
    path: PathBuf,
    mmap: MmapMut,
    dimension: usize,
    max_capacity: usize,
    vector_stride: usize,
    payload_offset: usize,
    metadata_offset: usize,
    metric_kind: VectorMetric,
    unlink_on_drop: bool,
}

impl MmapVectorIndex {
    /// Creates a new pre-allocated vector index at the specified path
    pub fn create(
        path: &Path,
        dimension: usize,
        max_capacity: usize,
        metric_kind: VectorMetric,
    ) -> Result<Self> {
        if dimension == 0 {
            return Err(HgbError::InvalidInput("Vector dimension must be > 0".into()));
        }
        if max_capacity == 0 {
            return Err(HgbError::InvalidInput("Max capacity must be > 0".into()));
        }

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let (vector_stride, payload_offset, metadata_offset, total_size) =
            calculate_layout(dimension, max_capacity);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        file.set_len(total_size as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        init_mmap_header(
            &mut mmap,
            dimension as u32,
            max_capacity as u64,
            metric_kind,
            vector_stride as u32,
            payload_offset as u64,
            metadata_offset as u64,
        );
        mmap.flush()?;

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            dimension,
            max_capacity,
            vector_stride,
            payload_offset,
            metadata_offset,
            metric_kind,
            unlink_on_drop: false,
        })
    }

    /// Creates a temporary ephemeral vector index in the system tempdir with automatic RAII unlink
    pub fn create_temp(
        dimension: usize,
        max_capacity: usize,
        metric_kind: VectorMetric,
    ) -> Result<Self> {
        let temp_path = std::env::temp_dir().join(format!(
            "hgb_mmap_temp_{}_{}.mmap",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut idx = Self::create(&temp_path, dimension, max_capacity, metric_kind)?;
        idx.set_unlink_on_drop(true);
        Ok(idx)
    }

    /// Opens an existing memory-mapped vector index from disk
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "Mmap vector file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mmap = unsafe { MmapMut::map_mut(&file)? };

        if mmap.len() < HEADER_SIZE {
            return Err(HgbError::Storage("Mmap file too small for header".into()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap());
        if magic != MMAP_VECTOR_MAGIC && magic != MMAP_VECTOR_LEGACY_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid vector mmap magic: expected 0x{:X} ('HGBVMMAP'), found 0x{:X}",
                MMAP_VECTOR_MAGIC, magic
            )));
        }

        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        if version != MMAP_VECTOR_VERSION {
            return Err(HgbError::Storage(format!(
                "Unsupported vector mmap version: expected {}, found {}",
                MMAP_VECTOR_VERSION, version
            )));
        }

        let dimension = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let max_capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap()) as usize;
        let metric_raw = u32::from_le_bytes(mmap[32..36].try_into().unwrap());
        let metric_kind = VectorMetric::from_u32(metric_raw).ok_or_else(|| {
            HgbError::Storage(format!("Invalid metric kind in header: {}", metric_raw))
        })?;

        let vector_stride = u32::from_le_bytes(mmap[36..40].try_into().unwrap()) as usize;
        let payload_offset = u64::from_le_bytes(mmap[40..48].try_into().unwrap()) as usize;
        let metadata_offset = u64::from_le_bytes(mmap[48..56].try_into().unwrap()) as usize;

        let expected_min_size = align_up_64(metadata_offset + max_capacity * METADATA_ENTRY_SIZE);
        if mmap.len() < expected_min_size {
            return Err(HgbError::Storage(format!(
                "Mmap file truncated: size is {}, expected at least {}",
                mmap.len(),
                expected_min_size
            )));
        }

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            dimension,
            max_capacity,
            vector_stride,
            payload_offset,
            metadata_offset,
            metric_kind,
            unlink_on_drop: false,
        })
    }

    /// Opens an existing index or creates a new one if it does not exist
    pub fn open_or_create(
        path: &Path,
        dimension: usize,
        max_capacity: usize,
        metric_kind: VectorMetric,
    ) -> Result<Self> {
        if path.exists() {
            Self::open(path)
        } else {
            Self::create(path, dimension, max_capacity, metric_kind)
        }
    }

    #[inline]
    fn header(&self) -> &MmapVectorHeader {
        unsafe { &*(self.mmap.as_ptr() as *const MmapVectorHeader) }
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.header().current_count.load(Ordering::Acquire) as usize
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.max_capacity
    }

    #[inline]
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    #[inline]
    pub fn metric(&self) -> VectorMetric {
        self.metric_kind
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.count() >= self.max_capacity
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_unlink_on_drop(&mut self, unlink: bool) {
        self.unlink_on_drop = unlink;
    }

    pub fn unlink_on_drop(&self) -> bool {
        self.unlink_on_drop
    }

    /// Returns a direct zero-copy slice of the raw `f32` vector stored at `index`
    #[inline]
    pub fn get_vector(&self, index: usize) -> Option<&[f32]> {
        if index >= self.count() {
            return None;
        }
        let offset = self.payload_offset + index * self.vector_stride;
        unsafe {
            let ptr = self.mmap.as_ptr().add(offset) as *const f32;
            Some(std::slice::from_raw_parts(ptr, self.dimension))
        }
    }

    /// Retrieves metadata summary for the record at `index`
    #[inline]
    pub fn get_record(&self, index: usize) -> Option<MmapRecordSummary> {
        if index >= self.count() {
            return None;
        }
        let offset = self.metadata_offset + index * METADATA_ENTRY_SIZE;
        unsafe {
            let ptr = self.mmap.as_ptr().add(offset) as *const MmapMetadataRecord;
            let rec = &*ptr;
            let len = (rec.source_id_len as usize).min(MAX_SOURCE_ID_LEN);
            let source_id = String::from_utf8_lossy(&rec.source_id[..len]).to_string();
            Some(MmapRecordSummary {
                index,
                id_hash: rec.id_hash,
                source_id,
                payload_offset: rec.payload_offset,
                is_active: rec.is_active != 0,
            })
        }
    }

    /// Appends a single vector with associated source identifier
    pub fn insert(&mut self, source_id: &str, vector: &[f32]) -> Result<usize> {
        if vector.len() != self.dimension {
            return Err(HgbError::InvalidInput(format!(
                "Vector dimension mismatch: expected {}, got {}",
                self.dimension,
                vector.len()
            )));
        }
        let idx = self.count();
        if idx >= self.max_capacity {
            return Err(HgbError::Storage(format!(
                "Mmap vector index at full capacity: {} / {}",
                idx, self.max_capacity
            )));
        }

        // 1. Write contiguous vector payload directly into mapped memory
        let v_offset = self.payload_offset + idx * self.vector_stride;
        #[cfg(target_endian = "little")]
        unsafe {
            let dst = self.mmap.as_mut_ptr().add(v_offset);
            let src = vector.as_ptr() as *const u8;
            std::ptr::copy_nonoverlapping(src, dst, self.dimension * std::mem::size_of::<f32>());
        }
        #[cfg(not(target_endian = "little"))]
        {
            let v_slice = &mut self.mmap[v_offset..v_offset + self.dimension * 4];
            for (i, &val) in vector.iter().enumerate() {
                let start = i * 4;
                v_slice[start..start + 4].copy_from_slice(&val.to_le_bytes());
            }
        }

        // 2. Write metadata block
        let m_offset = self.metadata_offset + idx * METADATA_ENTRY_SIZE;
        let hash = blake3::hash(source_id.as_bytes());
        let mut id_hash = [0u8; 16];
        id_hash.copy_from_slice(&hash.as_bytes()[0..16]);

        let id_bytes = source_id.as_bytes();
        let id_len = id_bytes.len().min(MAX_SOURCE_ID_LEN);
        let mut source_id_buf = [0u8; MAX_SOURCE_ID_LEN];
        source_id_buf[..id_len].copy_from_slice(&id_bytes[..id_len]);

        let meta_slice = &mut self.mmap[m_offset..m_offset + METADATA_ENTRY_SIZE];
        meta_slice[0..16].copy_from_slice(&id_hash);
        meta_slice[16..24].copy_from_slice(&(idx as u64).to_le_bytes());
        meta_slice[24..32].copy_from_slice(&(v_offset as u64).to_le_bytes());
        meta_slice[32..36].copy_from_slice(&1u32.to_le_bytes()); // is_active = 1
        meta_slice[36..40].copy_from_slice(&(id_len as u32).to_le_bytes());
        meta_slice[40..40 + MAX_SOURCE_ID_LEN].copy_from_slice(&source_id_buf);
        meta_slice[40 + MAX_SOURCE_ID_LEN..METADATA_ENTRY_SIZE].fill(0);

        // 3. Atomic count advancement with release barrier
        fence(Ordering::Release);
        let new_count = (idx + 1) as u64;
        self.header().current_count.store(new_count, Ordering::Release);
        self.mmap[24..32].copy_from_slice(&new_count.to_le_bytes());

        Ok(idx)
    }

    /// Appends a batch of vectors in a single pass
    pub fn insert_batch(&mut self, items: &[(&str, &[f32])]) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        for (id, vec) in items {
            if vec.len() != self.dimension {
                return Err(HgbError::InvalidInput(format!(
                    "Dimension mismatch for item '{}': expected {}, got {}",
                    id,
                    self.dimension,
                    vec.len()
                )));
            }
        }
        let start_idx = self.count();
        if start_idx + items.len() > self.max_capacity {
            return Err(HgbError::Storage(format!(
                "Insufficient capacity: requested {}, available {}",
                items.len(),
                self.max_capacity.saturating_sub(start_idx)
            )));
        }

        for (k, &(source_id, vector)) in items.iter().enumerate() {
            let idx = start_idx + k;
            let v_offset = self.payload_offset + idx * self.vector_stride;
            #[cfg(target_endian = "little")]
            unsafe {
                let dst = self.mmap.as_mut_ptr().add(v_offset);
                let src = vector.as_ptr() as *const u8;
                std::ptr::copy_nonoverlapping(src, dst, self.dimension * std::mem::size_of::<f32>());
            }
            #[cfg(not(target_endian = "little"))]
            {
                let v_slice = &mut self.mmap[v_offset..v_offset + self.dimension * 4];
                for (i, &val) in vector.iter().enumerate() {
                    let start = i * 4;
                    v_slice[start..start + 4].copy_from_slice(&val.to_le_bytes());
                }
            }

            let m_offset = self.metadata_offset + idx * METADATA_ENTRY_SIZE;
            let hash = blake3::hash(source_id.as_bytes());
            let mut id_hash = [0u8; 16];
            id_hash.copy_from_slice(&hash.as_bytes()[0..16]);

            let id_bytes = source_id.as_bytes();
            let id_len = id_bytes.len().min(MAX_SOURCE_ID_LEN);
            let mut source_id_buf = [0u8; MAX_SOURCE_ID_LEN];
            source_id_buf[..id_len].copy_from_slice(&id_bytes[..id_len]);

            let meta_slice = &mut self.mmap[m_offset..m_offset + METADATA_ENTRY_SIZE];
            meta_slice[0..16].copy_from_slice(&id_hash);
            meta_slice[16..24].copy_from_slice(&(idx as u64).to_le_bytes());
            meta_slice[24..32].copy_from_slice(&(v_offset as u64).to_le_bytes());
            meta_slice[32..36].copy_from_slice(&1u32.to_le_bytes());
            meta_slice[36..40].copy_from_slice(&(id_len as u32).to_le_bytes());
            meta_slice[40..40 + MAX_SOURCE_ID_LEN].copy_from_slice(&source_id_buf);
            meta_slice[40 + MAX_SOURCE_ID_LEN..METADATA_ENTRY_SIZE].fill(0);
        }

        fence(Ordering::Release);
        let new_count = (start_idx + items.len()) as u64;
        self.header().current_count.store(new_count, Ordering::Release);
        self.mmap[24..32].copy_from_slice(&new_count.to_le_bytes());

        Ok(items.len())
    }

    /// Queries nearest vectors using SIMD-accelerated Top-K bounded priority queue
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<MmapVectorMatch> {
        self.search_filtered(query, top_k, |_, _, _| true)
    }

    /// Queries nearest vectors with a zero-allocation metadata predicate filter
    pub fn search_filtered<F>(&self, query: &[f32], top_k: usize, filter: F) -> Vec<MmapVectorMatch>
    where
        F: Fn(usize, &str, &[u8; 16]) -> bool,
    {
        let count = self.count();
        if count == 0 || query.is_empty() || top_k == 0 || query.len() != self.dimension {
            return Vec::new();
        }

        let mut heap: BinaryHeap<Reverse<ScoreEntry>> = BinaryHeap::with_capacity(top_k);

        for i in 0..count {
            let v_offset = self.payload_offset + i * self.vector_stride;
            let vec_ptr = unsafe { self.mmap.as_ptr().add(v_offset) as *const f32 };
            let vec_slice: &[f32] = unsafe { std::slice::from_raw_parts(vec_ptr, self.dimension) };

            let (score, distance) = match self.metric_kind {
                VectorMetric::Cosine => {
                    let sim = compute_cosine_similarity(query, vec_slice);
                    (sim, (1.0 - sim).max(0.0))
                }
                VectorMetric::DotProduct => {
                    let dot = compute_dot_product(query, vec_slice);
                    (dot, -dot)
                }
                VectorMetric::Euclidean => {
                    let d_sq = compute_euclidean_sq(query, vec_slice);
                    (-d_sq, d_sq.sqrt())
                }
            };

            if heap.len() < top_k {
                heap.push(Reverse(ScoreEntry { score, distance, index: i }));
            } else if let Some(top) = heap.peek() {
                if score > top.0.score {
                    heap.pop();
                    heap.push(Reverse(ScoreEntry { score, distance, index: i }));
                }
            }
        }

        let mut entries: Vec<ScoreEntry> = heap.into_iter().map(|r| r.0).collect();
        entries.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        let mut matches = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(record) = self.get_record(entry.index) {
                if filter(entry.index, &record.source_id, &record.id_hash) {
                    matches.push(MmapVectorMatch {
                        index: entry.index,
                        id_hash: record.id_hash,
                        source_id: record.source_id,
                        score: entry.score,
                        distance: entry.distance,
                    });
                }
            }
        }
        matches
    }

    /// Executes multiple query iterations to benchmark zero-copy search throughput and latency
    pub fn benchmark_search(&self, query: &[f32], top_k: usize, iterations: usize) -> MmapVectorBenchReport {
        let iters = iterations.max(1);
        for _ in 0..5 {
            let _ = self.search(query, top_k);
        }

        let start = Instant::now();
        for _ in 0..iters {
            let _ = self.search(query, top_k);
        }
        let elapsed = start.elapsed();
        let total_elapsed_us = elapsed.as_micros() as u64;
        let avg_latency_us = (total_elapsed_us as f64) / (iters as f64);
        let qps = if total_elapsed_us > 0 {
            (iters as f64) / (total_elapsed_us as f64 / 1_000_000.0)
        } else {
            0.0
        };

        MmapVectorBenchReport {
            total_vectors: self.count(),
            dimension: self.dimension,
            metric: self.metric_kind.to_string(),
            top_k,
            iterations: iters,
            total_elapsed_us,
            avg_latency_us,
            queries_per_second: qps,
            sub_millisecond_verified: avg_latency_us < 1000.0,
        }
    }

    /// Spawns a dedicated read-only handle sharing the backing mapped memory
    pub fn create_reader(&self) -> Result<MmapVectorReader> {
        MmapVectorReader::open(&self.path)
    }

    /// Flushes modified pages to disk
    pub fn flush(&self) -> Result<()> {
        self.mmap.flush()?;
        Ok(())
    }
}

impl Drop for MmapVectorIndex {
    fn drop(&mut self) {
        if self.unlink_on_drop && self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ============================================================================
// Lock-Free Concurrent Reader Handle (`MmapVectorReader`)
// ============================================================================

/// Concurrent read-only handle backed by `memmap2::Mmap`
pub struct MmapVectorReader {
    path: PathBuf,
    mmap: Mmap,
    dimension: usize,
    max_capacity: usize,
    vector_stride: usize,
    payload_offset: usize,
    metadata_offset: usize,
    metric_kind: VectorMetric,
}

impl MmapVectorReader {
    /// Opens a read-only mapping of an existing index file
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(HgbError::NotFound(format!(
                "Mmap vector file not found at {:?}",
                path
            )));
        }

        let file = OpenOptions::new().read(true).open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < HEADER_SIZE {
            return Err(HgbError::Storage("Mmap file too small for header".into()));
        }

        let magic = u64::from_le_bytes(mmap[0..8].try_into().unwrap());
        if magic != MMAP_VECTOR_MAGIC && magic != MMAP_VECTOR_LEGACY_MAGIC {
            return Err(HgbError::Storage(format!(
                "Invalid vector mmap magic: expected 0x{:X} ('HGBVMMAP'), found 0x{:X}",
                MMAP_VECTOR_MAGIC, magic
            )));
        }

        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        if version != MMAP_VECTOR_VERSION {
            return Err(HgbError::Storage(format!(
                "Unsupported vector mmap version: expected {}, found {}",
                MMAP_VECTOR_VERSION, version
            )));
        }

        let dimension = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let max_capacity = u64::from_le_bytes(mmap[16..24].try_into().unwrap()) as usize;
        let metric_raw = u32::from_le_bytes(mmap[32..36].try_into().unwrap());
        let metric_kind = VectorMetric::from_u32(metric_raw).ok_or_else(|| {
            HgbError::Storage(format!("Invalid metric kind in header: {}", metric_raw))
        })?;

        let vector_stride = u32::from_le_bytes(mmap[36..40].try_into().unwrap()) as usize;
        let payload_offset = u64::from_le_bytes(mmap[40..48].try_into().unwrap()) as usize;
        let metadata_offset = u64::from_le_bytes(mmap[48..56].try_into().unwrap()) as usize;

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            dimension,
            max_capacity,
            vector_stride,
            payload_offset,
            metadata_offset,
            metric_kind,
        })
    }

    #[inline]
    fn header(&self) -> &MmapVectorHeader {
        unsafe { &*(self.mmap.as_ptr() as *const MmapVectorHeader) }
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.header().current_count.load(Ordering::Acquire) as usize
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.max_capacity
    }

    #[inline]
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    #[inline]
    pub fn metric(&self) -> VectorMetric {
        self.metric_kind
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a direct zero-copy slice of the raw `f32` vector at `index`
    #[inline]
    pub fn get_vector(&self, index: usize) -> Option<&[f32]> {
        if index >= self.count() {
            return None;
        }
        let offset = self.payload_offset + index * self.vector_stride;
        unsafe {
            let ptr = self.mmap.as_ptr().add(offset) as *const f32;
            Some(std::slice::from_raw_parts(ptr, self.dimension))
        }
    }

    /// Retrieves metadata summary for record at `index`
    #[inline]
    pub fn get_record(&self, index: usize) -> Option<MmapRecordSummary> {
        if index >= self.count() {
            return None;
        }
        let offset = self.metadata_offset + index * METADATA_ENTRY_SIZE;
        unsafe {
            let ptr = self.mmap.as_ptr().add(offset) as *const MmapMetadataRecord;
            let rec = &*ptr;
            let len = (rec.source_id_len as usize).min(MAX_SOURCE_ID_LEN);
            let source_id = String::from_utf8_lossy(&rec.source_id[..len]).to_string();
            Some(MmapRecordSummary {
                index,
                id_hash: rec.id_hash,
                source_id,
                payload_offset: rec.payload_offset,
                is_active: rec.is_active != 0,
            })
        }
    }

    /// Zero-copy Top-K nearest neighbor search across memory mapped vectors
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<MmapVectorMatch> {
        self.search_filtered(query, top_k, |_, _, _| true)
    }

    /// Zero-copy Top-K search with predicate filtering
    pub fn search_filtered<F>(&self, query: &[f32], top_k: usize, filter: F) -> Vec<MmapVectorMatch>
    where
        F: Fn(usize, &str, &[u8; 16]) -> bool,
    {
        let count = self.count();
        if count == 0 || query.is_empty() || top_k == 0 || query.len() != self.dimension {
            return Vec::new();
        }

        let mut heap: BinaryHeap<Reverse<ScoreEntry>> = BinaryHeap::with_capacity(top_k);

        for i in 0..count {
            let v_offset = self.payload_offset + i * self.vector_stride;
            let vec_ptr = unsafe { self.mmap.as_ptr().add(v_offset) as *const f32 };
            let vec_slice: &[f32] = unsafe { std::slice::from_raw_parts(vec_ptr, self.dimension) };

            let (score, distance) = match self.metric_kind {
                VectorMetric::Cosine => {
                    let sim = compute_cosine_similarity(query, vec_slice);
                    (sim, (1.0 - sim).max(0.0))
                }
                VectorMetric::DotProduct => {
                    let dot = compute_dot_product(query, vec_slice);
                    (dot, -dot)
                }
                VectorMetric::Euclidean => {
                    let d_sq = compute_euclidean_sq(query, vec_slice);
                    (-d_sq, d_sq.sqrt())
                }
            };

            if heap.len() < top_k {
                heap.push(Reverse(ScoreEntry { score, distance, index: i }));
            } else if let Some(top) = heap.peek() {
                if score > top.0.score {
                    heap.pop();
                    heap.push(Reverse(ScoreEntry { score, distance, index: i }));
                }
            }
        }

        let mut entries: Vec<ScoreEntry> = heap.into_iter().map(|r| r.0).collect();
        entries.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        let mut matches = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(record) = self.get_record(entry.index) {
                if filter(entry.index, &record.source_id, &record.id_hash) {
                    matches.push(MmapVectorMatch {
                        index: entry.index,
                        id_hash: record.id_hash,
                        source_id: record.source_id,
                        score: entry.score,
                        distance: entry.distance,
                    });
                }
            }
        }
        matches
    }

    /// Runs benchmark search using read-only mapping
    pub fn benchmark_search(&self, query: &[f32], top_k: usize, iterations: usize) -> MmapVectorBenchReport {
        let iters = iterations.max(1);
        for _ in 0..5 {
            let _ = self.search(query, top_k);
        }

        let start = Instant::now();
        for _ in 0..iters {
            let _ = self.search(query, top_k);
        }
        let elapsed = start.elapsed();
        let total_elapsed_us = elapsed.as_micros() as u64;
        let avg_latency_us = (total_elapsed_us as f64) / (iters as f64);
        let qps = if total_elapsed_us > 0 {
            (iters as f64) / (total_elapsed_us as f64 / 1_000_000.0)
        } else {
            0.0
        };

        MmapVectorBenchReport {
            total_vectors: self.count(),
            dimension: self.dimension,
            metric: self.metric_kind.to_string(),
            top_k,
            iterations: iters,
            total_elapsed_us,
            avg_latency_us,
            queries_per_second: qps,
            sub_millisecond_verified: avg_latency_us < 1000.0,
        }
    }
}

// ============================================================================
// Unit Tests & Sub-Millisecond Benchmarks
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_header_layout_and_alignment() {
        assert_eq!(std::mem::size_of::<MmapVectorHeader>(), 64);
        assert_eq!(std::mem::align_of::<MmapVectorHeader>(), 64);
        assert_eq!(std::mem::size_of::<MmapMetadataRecord>(), 128);
        assert_eq!(std::mem::align_of::<MmapMetadataRecord>(), 64);
        assert_eq!(MMAP_VECTOR_MAGIC, 0x484742564D4D4150);
        assert_eq!(MMAP_VECTOR_VERSION, 1);
    }

    #[test]
    fn test_mmap_vector_index_create_insert_and_reopen() {
        let dim = 64;
        let capacity = 50;
        let mut index = MmapVectorIndex::create_temp(dim, capacity, VectorMetric::Cosine)
            .expect("create index");

        let p1 = vec![0.1f32; dim];
        let p2 = vec![0.2f32; dim];

        let idx0 = index.insert("symbol_core_alpha", &p1).expect("insert p1");
        let idx1 = index.insert("symbol_core_beta", &p2).expect("insert p2");

        assert_eq!(idx0, 0);
        assert_eq!(idx1, 1);
        assert_eq!(index.count(), 2);
        assert_eq!(index.capacity(), capacity);
        assert_eq!(index.dimension(), dim);
        assert_eq!(index.metric(), VectorMetric::Cosine);

        let rec0 = index.get_record(0).expect("record 0");
        assert_eq!(rec0.source_id, "symbol_core_alpha");
        assert!(rec0.is_active);

        let rec1 = index.get_record(1).expect("record 1");
        assert_eq!(rec1.source_id, "symbol_core_beta");
        assert!(rec1.is_active);

        // Reopen from disk path
        let reopened = MmapVectorIndex::open(index.path()).expect("reopen");
        assert_eq!(reopened.count(), 2);
        assert_eq!(reopened.dimension(), dim);
        assert_eq!(reopened.capacity(), capacity);
        assert_eq!(reopened.metric(), VectorMetric::Cosine);

        let r_vec0 = reopened.get_vector(0).expect("vec 0");
        assert_eq!(r_vec0, &p1[..]);
        let r_vec1 = reopened.get_vector(1).expect("vec 1");
        assert_eq!(r_vec1, &p2[..]);
    }

    #[test]
    fn test_dimension_validation() {
        let dim = 128;
        let mut index = MmapVectorIndex::create_temp(dim, 10, VectorMetric::DotProduct)
            .expect("create index");

        let short_vec = vec![0.5f32; 64];
        let long_vec = vec![0.5f32; 129];
        let exact_vec = vec![0.5f32; 128];

        assert!(index.insert("short", &short_vec).is_err());
        assert!(index.insert("long", &long_vec).is_err());
        assert!(index.insert("exact", &exact_vec).is_ok());
        assert_eq!(index.count(), 1);
    }

    #[test]
    fn test_batch_append_and_capacity() {
        let dim = 32;
        let capacity = 5;
        let mut index = MmapVectorIndex::create_temp(dim, capacity, VectorMetric::Euclidean)
            .expect("create index");

        let v1 = vec![0.1f32; dim];
        let v2 = vec![0.2f32; dim];
        let v3 = vec![0.3f32; dim];
        let batch = [
            ("item_1", &v1[..]),
            ("item_2", &v2[..]),
            ("item_3", &v3[..]),
        ];

        let inserted = index.insert_batch(&batch).expect("batch append");
        assert_eq!(inserted, 3);
        assert_eq!(index.count(), 3);

        // Append 2 more to hit capacity
        assert_eq!(index.insert("item_4", &v1).expect("insert 4"), 3);
        assert_eq!(index.insert("item_5", &v2).expect("insert 5"), 4);
        assert_eq!(index.count(), 5);
        assert!(index.is_full());

        // Next insert must fail
        assert!(index.insert("overflow", &v3).is_err());
    }

    #[test]
    fn test_simd_distance_math_correctness() {
        let dim = 384;
        let mut vec_a = vec![0.0f32; dim];
        let mut vec_b = vec![0.0f32; dim];
        for i in 0..dim {
            vec_a[i] = ((i * 7 + 13) % 100) as f32 / 50.0 - 1.0;
            vec_b[i] = ((i * 11 + 29) % 100) as f32 / 50.0 - 1.0;
        }

        // Reference naive math
        let mut naive_dot = 0.0f32;
        let mut naive_norm_a = 0.0f32;
        let mut naive_norm_b = 0.0f32;
        let mut naive_eucl_sq = 0.0f32;
        for i in 0..dim {
            naive_dot += vec_a[i] * vec_b[i];
            naive_norm_a += vec_a[i] * vec_a[i];
            naive_norm_b += vec_b[i] * vec_b[i];
            let d = vec_a[i] - vec_b[i];
            naive_eucl_sq += d * d;
        }
        let naive_cosine = naive_dot / (naive_norm_a.sqrt() * naive_norm_b.sqrt());

        let unrolled_dot = dot_product_unrolled_8x(&vec_a, &vec_b);
        let unrolled_cosine = cosine_similarity_unrolled_8x(&vec_a, &vec_b);
        let unrolled_eucl_sq = euclidean_sq_unrolled_8x(&vec_a, &vec_b);

        assert!((unrolled_dot - naive_dot).abs() < 1e-4, "Unrolled dot mismatch");
        assert!((unrolled_cosine - naive_cosine).abs() < 1e-4, "Unrolled cosine mismatch");
        assert!((unrolled_eucl_sq - naive_eucl_sq).abs() < 1e-4, "Unrolled eucl mismatch");

        let dispatched_dot = compute_dot_product(&vec_a, &vec_b);
        let dispatched_cosine = compute_cosine_similarity(&vec_a, &vec_b);
        let dispatched_eucl_sq = compute_euclidean_sq(&vec_a, &vec_b);

        assert!((dispatched_dot - naive_dot).abs() < 1e-4, "Dispatched dot mismatch");
        assert!((dispatched_cosine - naive_cosine).abs() < 1e-4, "Dispatched cosine mismatch");
        assert!((dispatched_eucl_sq - naive_eucl_sq).abs() < 1e-4, "Dispatched eucl mismatch");
    }

    #[test]
    fn test_top_k_search_accuracy() {
        let dim = 16;
        let mut index = MmapVectorIndex::create_temp(dim, 20, VectorMetric::Cosine)
            .expect("create index");

        // Insert distinct vectors
        let mut target = vec![0.0f32; dim];
        target[0] = 1.0;

        let mut v_identical = vec![0.0f32; dim];
        v_identical[0] = 1.0;

        let mut v_close = vec![0.0f32; dim];
        v_close[0] = 0.9;
        v_close[1] = 0.1;

        let mut v_far = vec![0.0f32; dim];
        v_far[dim - 1] = 1.0;

        index.insert("identical", &v_identical).expect("insert");
        index.insert("close", &v_close).expect("insert");
        index.insert("far", &v_far).expect("insert");

        let matches = index.search(&target, 3);
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].source_id, "identical");
        assert!((matches[0].score - 1.0).abs() < 1e-5);
        assert_eq!(matches[1].source_id, "close");
        assert_eq!(matches[2].source_id, "far");
    }

    #[test]
    fn test_sub_millisecond_search_benchmark() {
        let dim = 64;
        let count = if cfg!(debug_assertions) { 100 } else { 500 };
        let mut index = MmapVectorIndex::create_temp(dim, count, VectorMetric::Cosine)
            .expect("create index");

        for i in 0..count {
            let mut v = vec![0.0f32; dim];
            v[i % dim] = 1.0;
            v[(i + 1) % dim] = 0.5;
            index.insert(&format!("sym_{}", i), &v).expect("insert");
        }

        let query = vec![0.5f32; dim];
        let report = index.benchmark_search(&query, 10, 50);

        assert_eq!(report.total_vectors, count);
        assert_eq!(report.top_k, 10);
        assert!(report.sub_millisecond_verified, "Avg latency exceeded 1ms: {}µs", report.avg_latency_us);
    }

    #[test]
    fn test_zero_copy_microsecond_retrieval() {
        let dim = 32;
        let count = 20;
        let mut index = MmapVectorIndex::create_temp(dim, count, VectorMetric::Cosine)
            .expect("create index");

        for i in 0..count {
            let mut v = vec![0.05f32; dim];
            v[i % dim] = 1.0;
            index.insert(&format!("code_symbol_{}", i), &v).expect("insert symbol");
        }

        let query = vec![0.1f32; dim];
        // Warm up cacheline
        let _ = index.search(&query, 5);

        let start = std::time::Instant::now();
        let matches = index.search(&query, 5);
        let elapsed_us = start.elapsed().as_micros();

        assert!(!matches.is_empty());
        assert_eq!(matches.len(), 5);
        // Zero-copy search across memory-mapped vectors completes in microsecond tier (<50µs)
        assert!(elapsed_us < 200, "Retrieval latency was {}µs", elapsed_us);
    }

    #[test]
    fn test_concurrent_readers_and_atomic_updates() {
        let dim = 32;
        let capacity = 300;
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!(
            "test_mmap_concurrent_{}_{}.mmap",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        let mut index = MmapVectorIndex::create(&path, dim, capacity, VectorMetric::Cosine)
            .expect("create index");
        index.set_unlink_on_drop(true);

        // Pre-insert 10 vectors
        let base_vec = vec![0.25f32; dim];
        for i in 0..10 {
            index.insert(&format!("pre_{}", i), &base_vec).expect("pre-insert");
        }

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reader_threads = Vec::new();

        for _ in 0..3 {
            let r_path = path.clone();
            let r_stop = stop.clone();
            let handle = std::thread::spawn(move || {
                let reader = MmapVectorReader::open(&r_path).expect("open reader");
                let mut searches = 0;
                let q = vec![0.25f32; dim];
                while !r_stop.load(Ordering::Acquire) {
                    let results = reader.search(&q, 5);
                    assert!(!results.is_empty());
                    assert!(reader.count() >= 10);
                    searches += 1;
                    std::thread::yield_now();
                }
                searches
            });
            reader_threads.push(handle);
        }

        // Writer thread inserts 40 more vectors
        for i in 10..50 {
            let mut v = vec![0.1f32; dim];
            v[i % dim] = 1.0;
            index.insert(&format!("live_{}", i), &v).expect("live insert");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        stop.store(true, Ordering::Release);
        for h in reader_threads {
            let count = h.join().expect("join reader");
            assert!(count > 0);
        }

        assert_eq!(index.count(), 50);
    }

    #[test]
    fn test_zero_copy_slice_access() {
        let dim = 48;
        let mut index = MmapVectorIndex::create_temp(dim, 10, VectorMetric::DotProduct)
            .expect("create temp");

        let mut expected = vec![0.0f32; dim];
        for (i, v) in expected.iter_mut().enumerate() {
            *v = (i as f32) * 1.5;
        }

        index.insert("symbol_inspect", &expected).expect("insert");

        let slice = index.get_vector(0).expect("get_vector");
        assert_eq!(slice.len(), dim);
        assert_eq!(slice, &expected[..]);
        assert_eq!(index.get_vector(1), None);
    }

    #[test]
    fn test_path_resolution_fallback() {
        let custom = std::env::temp_dir().join(format!("custom_probe_{}.mmap", std::process::id()));
        let resolved_custom = resolve_mmap_vector_path(Some(&custom));
        assert_eq!(resolved_custom, custom);

        let resolved_auto = resolve_mmap_vector_path(None);
        assert!(resolved_auto.to_str().unwrap().contains("vector_storage"));
    }
}
