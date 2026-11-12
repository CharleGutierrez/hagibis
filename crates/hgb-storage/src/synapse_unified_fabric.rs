//! # Synapse: Unified Vector-Relational Fabric & Query Planner
//!
//! Production-grade associative storage engine uniting SQLite relational persistence
//! (`hgb-cli/sqlite.db`, conversation logs, agent checkpoints, tool execution history)
//! and LanceDB columnar vector indexing (`vector-lancedb` skill) into a zero-copy,
//! sub-millisecond associative synaptic fabric.
//!
//! ## Architecture
//! - **Unified Query Planner "Synapse"**:
//!   Seamlessly unifies SQLite relational storage and LanceDB columnar vector indexing.
//!   Zero-copy memory mapping (`PRAGMA mmap_size`, `memmap2`) and sub-millisecond query planning.
//! - **SQLite Virtual Functions & Hybrid Vector Joins**:
//!   * `synapse_cosine_distance(vec_a, vec_b)`: SIMD-accelerated cosine distance between IEEE-754 f32 binary slices.
//!   * `synapse_dot_product(vec_a, vec_b)`: SIMD-accelerated inner dot product.
//!   * `synapse_cosine_similarity(vec_a, vec_b)`: Normalized projection in `[-1.0, 1.0]`.
//!   * `synapse_vector_knn(?1, ?2)`: Eponymous SQLite table-valued virtual table enabling native SQL
//!     joins between relational tables and top-K vector nearest-neighbors from LanceDB.
//! - **Lexical + Vector Reciprocal Rank Fusion (RRF)**:
//!   Blends SQLite FTS5 full-text lexical search results with dense vector similarity scores:
//!   $$RRF(d) = \frac{w_{dense}}{k + rank_{dense}(d)} + \frac{w_{sparse}}{k + rank_{sparse}(d)}$$
//!   with parameterized $k$ (default 60), $w_{dense}$ (default 0.7), $w_{sparse}$ (default 0.3).
//! - **Synaptic Fabric Engine**:
//!   Thread-safe (`Arc<RwLock<...>>` / `Arc<Mutex<...>>`), active WAL-mode SQLite connection with
//!   memory mapping, alongside LanceDB columnar vector partition tables.
//! - **CaldoAI Untangle Rules**:
//!   Tier 2 storage crate importing only from Tier 1 (`hgb-core`) and Tier 0 (`hgb-wasm-kernel`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::marker::PhantomData;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use rusqlite::ffi;
use rusqlite::functions::FunctionFlags;
use rusqlite::vtab::{
    eponymous_only_module, Context, IndexConstraintOp, IndexInfo, VTab, VTabConfig, VTabConnection,
    VTabCursor, Values,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use hgb_core::{HgbError, Result};

use crate::lancedb_vector_engine::{
    DistanceMetric, LanceDbVectorEngine, SearchQuery, SearchResult, VectorRecord,
};

// ============================================================================
// IEEE-754 Float Tensor Encoding & Alignment Utilities
// ============================================================================

/// Encodes a contiguous slice of IEEE-754 `f32` floats into little-endian binary bytes.
#[inline]
pub fn encode_f32_slice_to_bytes(slice: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(slice.len() * 4);
    for &val in slice {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Decodes little-endian binary bytes into an owned `Vec<f32>`.
#[inline]
pub fn decode_bytes_to_f32_vec(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(HgbError::Storage(format!(
            "Invalid binary vector length {}: must be a multiple of 4",
            bytes.len()
        )));
    }
    let count = bytes.len() / 4;
    let mut vec = Vec::with_capacity(count);
    for chunk in bytes.chunks_exact(4) {
        vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(vec)
}

/// Borrows a byte slice directly as an aligned `&[f32]` if memory alignment permits.
#[inline]
pub fn bytes_to_f32_slice(bytes: &[u8]) -> std::result::Result<&[f32], String> {
    if bytes.len() % 4 != 0 {
        return Err(format!(
            "Invalid binary vector length {}: must be a multiple of 4",
            bytes.len()
        ));
    }
    let ptr = bytes.as_ptr();
    if (ptr as usize) % std::mem::align_of::<f32>() != 0 {
        return Err("Vector blob buffer is not 4-byte aligned".to_string());
    }
    let count = bytes.len() / 4;
    Ok(unsafe { std::slice::from_raw_parts(ptr as *const f32, count) })
}

/// Executes a closure with an `&[f32]` view over raw bytes, utilizing zero-copy borrow
/// when aligned, or falling back safely to a local aligned buffer if unaligned.
#[inline]
pub fn with_f32_slice<R, F: FnOnce(&[f32]) -> R>(
    bytes: &[u8],
    f: F,
) -> std::result::Result<R, String> {
    if bytes.len() % 4 != 0 {
        return Err(format!(
            "Invalid vector blob byte length {}: must be a multiple of 4",
            bytes.len()
        ));
    }
    let ptr = bytes.as_ptr();
    if (ptr as usize) % std::mem::align_of::<f32>() == 0 {
        let count = bytes.len() / 4;
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const f32, count) };
        Ok(f(slice))
    } else {
        let count = bytes.len() / 4;
        let mut aligned = Vec::with_capacity(count);
        for chunk in bytes.chunks_exact(4) {
            aligned.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok(f(&aligned))
    }
}

// ============================================================================
// SIMD Linear Algebra Distance & Similarity Kernels
// ============================================================================

/// 8-way unrolled auto-vectorizable dot product fallback kernel
#[inline]
pub fn dot_product_unrolled(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len();
    let chunks = len / 8;
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;
    let mut s4 = 0.0f32;
    let mut s5 = 0.0f32;
    let mut s6 = 0.0f32;
    let mut s7 = 0.0f32;

    for i in 0..chunks {
        let base = i * 8;
        s0 += a[base] * b[base];
        s1 += a[base + 1] * b[base + 1];
        s2 += a[base + 2] * b[base + 2];
        s3 += a[base + 3] * b[base + 3];
        s4 += a[base + 4] * b[base + 4];
        s5 += a[base + 5] * b[base + 5];
        s6 += a[base + 6] * b[base + 6];
        s7 += a[base + 7] * b[base + 7];
    }

    let mut sum = (s0 + s1) + (s2 + s3) + (s4 + s5) + (s6 + s7);
    for i in (chunks * 8)..len {
        sum += a[i] * b[i];
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len();
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        acc = _mm256_fmadd_ps(va, vb, acc);
        i += 8;
    }
    let mut buf = [0.0f32; 8];
    _mm256_storeu_ps(buf.as_mut_ptr(), acc);
    let mut total: f32 = buf.iter().sum();
    while i < len {
        total += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    total
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn dot_product_sse41(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len();
    let mut acc = _mm_setzero_ps();
    let mut i = 0;
    while i + 4 <= len {
        let va = _mm_loadu_ps(a.as_ptr().add(i));
        let vb = _mm_loadu_ps(b.as_ptr().add(i));
        acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
        i += 4;
    }
    let mut buf = [0.0f32; 4];
    _mm_storeu_ps(buf.as_mut_ptr(), acc);
    let mut total: f32 = buf.iter().sum();
    while i < len {
        total += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    total
}

/// Dynamically dispatches SIMD-accelerated dot product between two float slices
#[inline]
pub fn simd_dot_product(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return unsafe { dot_product_avx2(a, b) };
        }
        if is_x86_feature_detected!("sse4.1") {
            return unsafe { dot_product_sse41(a, b) };
        }
    }

    dot_product_unrolled(a, b)
}

/// Computes SIMD-accelerated cosine similarity between two float slices in range `[-1.0, 1.0]`
#[inline]
pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot = simd_dot_product(a, b);
    let norm_a = simd_dot_product(a, a);
    let norm_b = simd_dot_product(b, b);
    if norm_a <= 1e-12 || norm_b <= 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// Computes SIMD-accelerated cosine distance: `1.0 - cosine_similarity(a, b)` in range `[0.0, 2.0]`
#[inline]
pub fn simd_cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    1.0 - simd_cosine_similarity(a, b)
}

// ============================================================================
// SQLite Scalar Custom Functions Registration
// ============================================================================

/// Registers custom scalar functions into a SQLite connection:
/// - `synapse_cosine_distance(vec_a, vec_b)`
/// - `synapse_dot_product(vec_a, vec_b)`
/// - `synapse_cosine_similarity(vec_a, vec_b)`
pub fn register_synapse_sql_functions(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "synapse_dot_product",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let blob_a = ctx.get_raw(0).as_blob()?;
            let blob_b = ctx.get_raw(1).as_blob()?;
            let res = with_f32_slice(blob_a, |slice_a| {
                with_f32_slice(blob_b, |slice_b| {
                    if slice_a.len() != slice_b.len() {
                        return Err(format!(
                            "synapse_dot_product dimension mismatch: {} != {}",
                            slice_a.len(),
                            slice_b.len()
                        ));
                    }
                    Ok(simd_dot_product(slice_a, slice_b))
                })
            });
            match res {
                Ok(Ok(Ok(val))) => Ok(val as f64),
                Ok(Ok(Err(e))) | Ok(Err(e)) | Err(e) => Err(rusqlite::Error::UserFunctionError(e.into())),
            }
        },
    ).map_err(|e| HgbError::Storage(e.to_string()))?;

    conn.create_scalar_function(
        "synapse_cosine_distance",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let blob_a = ctx.get_raw(0).as_blob()?;
            let blob_b = ctx.get_raw(1).as_blob()?;
            let res = with_f32_slice(blob_a, |slice_a| {
                with_f32_slice(blob_b, |slice_b| {
                    if slice_a.len() != slice_b.len() {
                        return Err(format!(
                            "synapse_cosine_distance dimension mismatch: {} != {}",
                            slice_a.len(),
                            slice_b.len()
                        ));
                    }
                    Ok(simd_cosine_distance(slice_a, slice_b))
                })
            });
            match res {
                Ok(Ok(Ok(val))) => Ok(val as f64),
                Ok(Ok(Err(e))) | Ok(Err(e)) | Err(e) => Err(rusqlite::Error::UserFunctionError(e.into())),
            }
        },
    ).map_err(|e| HgbError::Storage(e.to_string()))?;

    conn.create_scalar_function(
        "synapse_cosine_similarity",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let blob_a = ctx.get_raw(0).as_blob()?;
            let blob_b = ctx.get_raw(1).as_blob()?;
            let res = with_f32_slice(blob_a, |slice_a| {
                with_f32_slice(blob_b, |slice_b| {
                    if slice_a.len() != slice_b.len() {
                        return Err(format!(
                            "synapse_cosine_similarity dimension mismatch: {} != {}",
                            slice_a.len(),
                            slice_b.len()
                        ));
                    }
                    Ok(simd_cosine_similarity(slice_a, slice_b))
                })
            });
            match res {
                Ok(Ok(Ok(val))) => Ok(val as f64),
                Ok(Ok(Err(e))) | Ok(Err(e)) | Err(e) => Err(rusqlite::Error::UserFunctionError(e.into())),
            }
        },
    ).map_err(|e| HgbError::Storage(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Data Models for Relational Storage Entities
// ============================================================================

/// Relational record for general unstructured knowledge or conversation items
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationalRecord {
    pub id: String,
    pub session_id: String,
    pub content: String,
    pub category: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: i64,
}

impl RelationalRecord {
    pub fn new(id: impl Into<String>, session_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            content: content.into(),
            category: None,
            metadata_json: None,
            created_at: chrono::Utc::now().timestamp_millis(),
        }
    }

    pub fn with_category(mut self, cat: impl Into<String>) -> Self {
        self.category = Some(cat.into());
        self
    }

    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata_json = Some(metadata.into());
        self
    }
}

/// Agent checkpoint record persisted in SQLite
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentCheckpoint {
    pub checkpoint_id: String,
    pub session_id: String,
    pub actor_id: String,
    pub step_index: u64,
    pub state_hash: String,
    pub payload_json: String,
    pub created_at: i64,
}

/// Tool execution trace record persisted in SQLite
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionTraceRecord {
    pub trace_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub input_json: String,
    pub output_json: Option<String>,
    pub status: String,
    pub duration_ms: u64,
    pub created_at: i64,
}

/// Conversation log entry persisted in SQLite
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationMessage {
    pub message_id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub category: Option<String>,
    pub created_at: i64,
}

/// Row result for a relational table joined with `synapse_vector_knn`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationalVectorJoinRow {
    pub id: String,
    pub session_id: String,
    pub content: String,
    pub score: f32,
    pub category: Option<String>,
}

// ============================================================================
// SQLite Virtual Table: synapse_vector_knn(?1, ?2)
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct SynapseKnnMatch {
    pub record_id: String,
    pub score: f32,
    pub distance: f32,
}

/// SQLite virtual table instance for `synapse_vector_knn`
#[repr(C)]
pub struct SynapseVectorKnnTab {
    base: ffi::sqlite3_vtab,
    engine: Option<Arc<RwLock<LanceDbVectorEngine>>>,
}

unsafe impl<'vtab> VTab<'vtab> for SynapseVectorKnnTab {
    type Aux = Arc<RwLock<LanceDbVectorEngine>>;
    type Cursor = SynapseVectorKnnCursor<'vtab>;

    fn connect(
        db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _args: &[&[u8]],
    ) -> rusqlite::Result<(String, Self)> {
        db.config(VTabConfig::Innocuous)?;
        let engine = aux.cloned();
        let schema = "CREATE TABLE x(record_id TEXT, score REAL, distance REAL, query_vec HIDDEN, top_k HIDDEN)";
        let vtab = SynapseVectorKnnTab {
            base: ffi::sqlite3_vtab::default(),
            engine,
        };
        Ok((schema.to_string(), vtab))
    }

    fn best_index(&self, info: &mut IndexInfo) -> rusqlite::Result<()> {
        let mut query_vec_idx = None;
        let mut top_k_idx = None;

        for (i, constraint) in info.constraints().enumerate() {
            if !constraint.is_usable() {
                continue;
            }
            if constraint.operator() == IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ {
                if constraint.column() == 3 {
                    query_vec_idx = Some(i);
                } else if constraint.column() == 4 {
                    top_k_idx = Some(i);
                }
            }
        }

        let mut argv = 1;
        if let Some(idx) = query_vec_idx {
            let mut usage = info.constraint_usage(idx);
            usage.set_argv_index(argv);
            usage.set_omit(true);
            argv += 1;
        }
        if let Some(idx) = top_k_idx {
            let mut usage = info.constraint_usage(idx);
            usage.set_argv_index(argv);
            usage.set_omit(true);
        }

        let mut order_by_consumed = false;
        for ob in info.order_bys() {
            if ob.column() == 1 && ob.is_order_by_desc() {
                order_by_consumed = true;
                break;
            }
        }
        if order_by_consumed {
            info.set_order_by_consumed(true);
        }

        info.set_estimated_cost(1.0);
        info.set_estimated_rows(100);
        Ok(())
    }

    fn open(&mut self) -> rusqlite::Result<SynapseVectorKnnCursor<'_>> {
        Ok(SynapseVectorKnnCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            engine: self.engine.clone(),
            results: Vec::new(),
            current_idx: 0,
            phantom: PhantomData,
        })
    }
}

/// Cursor traversing nearest neighbors returned by `synapse_vector_knn`
#[repr(C)]
pub struct SynapseVectorKnnCursor<'vtab> {
    base: ffi::sqlite3_vtab_cursor,
    engine: Option<Arc<RwLock<LanceDbVectorEngine>>>,
    results: Vec<SynapseKnnMatch>,
    current_idx: usize,
    phantom: PhantomData<&'vtab SynapseVectorKnnTab>,
}

unsafe impl VTabCursor for SynapseVectorKnnCursor<'_> {
    fn filter(
        &mut self,
        _idx_num: c_int,
        _idx_str: Option<&str>,
        args: &Values<'_>,
    ) -> rusqlite::Result<()> {
        self.results.clear();
        self.current_idx = 0;

        let mut query_vec: Option<Vec<f32>> = None;
        let mut top_k = 10usize;

        if !args.is_empty() {
            if let Ok(blob) = args.get::<Vec<u8>>(0) {
                if let Ok(vec) = decode_bytes_to_f32_vec(&blob) {
                    query_vec = Some(vec);
                }
            } else if let Ok(s) = args.get::<String>(0) {
                if let Ok(vec) = serde_json::from_str::<Vec<f32>>(&s) {
                    query_vec = Some(vec);
                }
            }
        }

        if args.len() >= 2 {
            if let Ok(k) = args.get::<i64>(1) {
                top_k = (k.max(1)) as usize;
            }
        }

        if let (Some(vec), Some(ref engine)) = (query_vec, &self.engine) {
            let engine_guard = engine.read().map_err(|e| {
                rusqlite::Error::UserFunctionError(format!("VectorEngine lock poisoned: {}", e).into())
            })?;
            let search_query = SearchQuery::new(vec, top_k);
            let hits = engine_guard.query(&search_query).map_err(|e| {
                rusqlite::Error::UserFunctionError(format!("Lance query execution failed: {}", e).into())
            })?;
            self.results = hits
                .into_iter()
                .map(|hit| SynapseKnnMatch {
                    record_id: hit.id,
                    score: hit.score,
                    distance: hit.distance,
                })
                .collect();
        }

        Ok(())
    }

    fn next(&mut self) -> rusqlite::Result<()> {
        self.current_idx += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.current_idx >= self.results.len()
    }

    fn column(&self, ctx: &mut Context, i: c_int) -> rusqlite::Result<()> {
        if self.current_idx >= self.results.len() {
            return Ok(());
        }
        let hit = &self.results[self.current_idx];
        match i {
            0 => ctx.set_result(&hit.record_id),
            1 => ctx.set_result(&(hit.score as f64)),
            2 => ctx.set_result(&(hit.distance as f64)),
            _ => Ok(()),
        }
    }

    fn rowid(&self) -> rusqlite::Result<i64> {
        Ok((self.current_idx + 1) as i64)
    }
}

// ============================================================================
// Reciprocal Rank Fusion (RRF) Models & Computations
// ============================================================================

/// Configuration parameters for Reciprocal Rank Fusion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RrfConfig {
    /// Smoothing constant (default: 60.0)
    pub k: f64,
    /// Dense vector similarity weight (default: 0.7)
    pub w_dense: f64,
    /// Lexical sparse match weight (default: 0.3)
    pub w_sparse: f64,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            w_dense: 0.7,
            w_sparse: 0.3,
        }
    }
}

/// Blended hybrid retrieval match produced by Reciprocal Rank Fusion
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HybridFusionResult {
    pub record_id: String,
    pub rrf_score: f64,
    pub dense_rank: Option<usize>,
    pub sparse_rank: Option<usize>,
    pub dense_score: Option<f32>,
    pub sparse_score: Option<f64>,
    pub content: String,
    pub category: Option<String>,
    pub metadata_json: Option<String>,
}

/// Computes reciprocal rank fusion scores over dense and sparse ranked result lists:
/// RRF(d) = w_dense / (k + rank_dense) + w_sparse / (k + rank_sparse)
pub fn compute_rrf(
    dense_ranks: &[(String, f32)],
    sparse_ranks: &[(String, f64)],
    config: &RrfConfig,
) -> Vec<HybridFusionResult> {
    let mut map: HashMap<String, (Option<usize>, Option<usize>, Option<f32>, Option<f64>)> =
        HashMap::new();

    for (dense_idx, (id, score)) in dense_ranks.iter().enumerate() {
        let entry = map.entry(id.clone()).or_insert((None, None, None, None));
        entry.0 = Some(dense_idx + 1); // 1-based rank
        entry.2 = Some(*score);
    }

    for (sparse_idx, (id, score)) in sparse_ranks.iter().enumerate() {
        let entry = map.entry(id.clone()).or_insert((None, None, None, None));
        entry.1 = Some(sparse_idx + 1); // 1-based rank
        entry.3 = Some(*score);
    }

    let mut results: Vec<HybridFusionResult> = map
        .into_iter()
        .map(|(record_id, (dense_rank, sparse_rank, dense_score, sparse_score))| {
            let mut score = 0.0f64;
            if let Some(r) = dense_rank {
                score += config.w_dense / (config.k + r as f64);
            }
            if let Some(r) = sparse_rank {
                score += config.w_sparse / (config.k + r as f64);
            }

            HybridFusionResult {
                record_id,
                rrf_score: score,
                dense_rank,
                sparse_rank,
                dense_score,
                sparse_score,
                content: String::new(),
                category: None,
                metadata_json: None,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

// ============================================================================
// Synapse Configuration & Engine
// ============================================================================

/// Configuration for the unified SynapticFabric engine
#[derive(Debug, Clone)]
pub struct SynapseConfig {
    pub dimension: usize,
    pub default_metric: DistanceMetric,
    pub rrf_config: RrfConfig,
    pub storage_dir: Option<PathBuf>,
}

impl Default for SynapseConfig {
    fn default() -> Self {
        Self {
            dimension: 64,
            default_metric: DistanceMetric::Cosine,
            rrf_config: RrfConfig::default(),
            storage_dir: None,
        }
    }
}

/// Synaptic Fabric Engine unifying SQLite and LanceDB into an associative synaptic memory
#[derive(Clone)]
pub struct SynapticFabric {
    inner: Arc<SynapticFabricInner>,
}

struct SynapticFabricInner {
    db_conn: Mutex<Connection>,
    vector_engine: Arc<RwLock<LanceDbVectorEngine>>,
    config: SynapseConfig,
    _temp_dir: Option<PathBuf>,
}

impl Drop for SynapticFabricInner {
    fn drop(&mut self) {
        if let Some(ref dir) = self._temp_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

impl SynapticFabric {
    /// Initializes an in-memory Synaptic Fabric instance
    pub fn in_memory(dimension: usize) -> Result<Self> {
        let temp_dir = std::env::temp_dir().join(format!(
            "synapse_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&temp_dir)?;

        let vector_engine = LanceDbVectorEngine::create(&temp_dir, dimension, DistanceMetric::Cosine)?;
        let arc_engine = Arc::new(RwLock::new(vector_engine));

        let conn = Connection::open_in_memory().map_err(|e| HgbError::Storage(e.to_string()))?;
        Self::init_sqlite_connection(&conn, &arc_engine)?;

        let config = SynapseConfig {
            dimension,
            default_metric: DistanceMetric::Cosine,
            rrf_config: RrfConfig::default(),
            storage_dir: None,
        };

        let inner = SynapticFabricInner {
            db_conn: Mutex::new(conn),
            vector_engine: arc_engine,
            config,
            _temp_dir: Some(temp_dir),
        };

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Opens or creates a persistent Synaptic Fabric instance on disk
    pub fn open_or_create<P: AsRef<Path>>(dir: P, dimension: usize) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;

        let lance_dir = root.join("lancedb");
        let sqlite_file = root.join("sqlite.db");

        let vector_engine =
            LanceDbVectorEngine::open_or_create(&lance_dir, dimension, DistanceMetric::Cosine)?;
        let arc_engine = Arc::new(RwLock::new(vector_engine));

        let conn = Connection::open(&sqlite_file).map_err(|e| HgbError::Storage(e.to_string()))?;
        Self::init_sqlite_connection(&conn, &arc_engine)?;

        let config = SynapseConfig {
            dimension,
            default_metric: DistanceMetric::Cosine,
            rrf_config: RrfConfig::default(),
            storage_dir: Some(root),
        };

        let inner = SynapticFabricInner {
            db_conn: Mutex::new(conn),
            vector_engine: arc_engine,
            config,
            _temp_dir: None,
        };

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Initializes high-performance PRAGMAs, tables, custom scalar functions, and virtual tables
    fn init_sqlite_connection(
        conn: &Connection,
        vector_engine: &Arc<RwLock<LanceDbVectorEngine>>,
    ) -> Result<()> {
        // High-performance PRAGMAs with zero-copy memory mapping
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -64000;
             PRAGMA mmap_size = 268435456;",
        )
        .map_err(|e| HgbError::Storage(format!("PRAGMA init failed: {}", e)))?;

        // 1. Relational entities schema
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS relational_records (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT,
                metadata_json TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rel_session ON relational_records(session_id);
            CREATE INDEX IF NOT EXISTS idx_rel_category ON relational_records(category);

            CREATE TABLE IF NOT EXISTS agent_checkpoints (
                checkpoint_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                actor_id TEXT NOT NULL,
                step_index INTEGER NOT NULL,
                state_hash TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chk_session ON agent_checkpoints(session_id);
            CREATE INDEX IF NOT EXISTS idx_chk_actor ON agent_checkpoints(actor_id);

            CREATE TABLE IF NOT EXISTS execution_trace (
                trace_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                input_json TEXT NOT NULL,
                output_json TEXT,
                status TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_trace_session ON execution_trace(session_id);
            CREATE INDEX IF NOT EXISTS idx_trace_tool ON execution_trace(tool_name);
            CREATE INDEX IF NOT EXISTS idx_trace_status ON execution_trace(status);

            CREATE TABLE IF NOT EXISTS conversation_logs (
                message_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conv_session ON conversation_logs(session_id);
            CREATE INDEX IF NOT EXISTS idx_conv_role ON conversation_logs(role);",
        )
        .map_err(|e| HgbError::Storage(format!("Table creation failed: {}", e)))?;

        // 2. FTS5 full-text search virtual table
        let fts_res = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS synapse_fts USING fts5(
                id UNINDEXED,
                session_id UNINDEXED,
                content,
                category,
                tokenize = 'porter unicode61'
            );",
        );
        if let Err(e) = fts_res {
            // Graceful fallback to standard table if FTS5 is omitted from build
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS synapse_fts_fallback (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    content TEXT NOT NULL,
                    category TEXT
                );",
            )
            .map_err(|e2| HgbError::Storage(format!("FTS fallback failed: {}: {}", e, e2)))?;
        }

        // 3. Register custom SIMD scalar functions
        register_synapse_sql_functions(conn)?;

        // 4. Register eponymous virtual table module synapse_vector_knn
        conn.create_module(
            "synapse_vector_knn",
            eponymous_only_module::<SynapseVectorKnnTab>(),
            Some(Arc::clone(vector_engine)),
        )
        .map_err(|e| HgbError::Storage(format!("Virtual table registration failed: {}", e)))?;

        Ok(())
    }

    /// Access the underlying shared LanceDbVectorEngine
    pub fn vector_engine(&self) -> Arc<RwLock<LanceDbVectorEngine>> {
        Arc::clone(&self.inner.vector_engine)
    }

    /// Access the configured dimension
    pub fn dimension(&self) -> usize {
        self.inner.config.dimension
    }

    // ========================================================================
    // Entity Upsert Operations (Write-Through Relational + LanceDB)
    // ========================================================================

    /// Inserts a relational record into SQLite and optionally indexes its embedding in LanceDB
    pub fn insert_relational_record(
        &self,
        record: &RelationalRecord,
        vector: Option<&[f32]>,
    ) -> Result<()> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.execute(
            "INSERT OR REPLACE INTO relational_records (id, session_id, content, category, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.id,
                record.session_id,
                record.content,
                record.category,
                record.metadata_json,
                record.created_at,
            ],
        )
        .map_err(|e| HgbError::Storage(e.to_string()))?;

        // Update FTS5 index
        let _ = conn.execute(
            "INSERT OR REPLACE INTO synapse_fts (id, session_id, content, category) VALUES (?1, ?2, ?3, ?4)",
            params![record.id, record.session_id, record.content, record.category],
        );

        drop(conn);

        // Project into LanceDB vector partition
        if let Some(vec) = vector {
            let lance_record = VectorRecord::new(&record.id, vec.to_vec())
                .with_text(&record.content)
                .with_partition(record.category.as_deref().unwrap_or("default"));
            let engine = self.inner.vector_engine.read().map_err(|e| {
                HgbError::Execution(format!("vector_engine read lock error: {}", e))
            })?;
            engine.upsert(vec![lance_record])?;
        }

        Ok(())
    }

    /// Inserts an agent checkpoint record into SQLite and optionally indexes its vector in LanceDB
    pub fn insert_agent_checkpoint(
        &self,
        checkpoint: &AgentCheckpoint,
        vector: Option<&[f32]>,
    ) -> Result<()> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.execute(
            "INSERT OR REPLACE INTO agent_checkpoints (checkpoint_id, session_id, actor_id, step_index, state_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                checkpoint.checkpoint_id,
                checkpoint.session_id,
                checkpoint.actor_id,
                checkpoint.step_index as i64,
                checkpoint.state_hash,
                checkpoint.payload_json,
                checkpoint.created_at,
            ],
        )
        .map_err(|e| HgbError::Storage(e.to_string()))?;

        drop(conn);

        if let Some(vec) = vector {
            let lance_record = VectorRecord::new(&checkpoint.checkpoint_id, vec.to_vec())
                .with_text(&checkpoint.payload_json)
                .with_partition("checkpoints");
            let engine = self.inner.vector_engine.read().map_err(|e| {
                HgbError::Execution(format!("vector_engine read lock error: {}", e))
            })?;
            engine.upsert(vec![lance_record])?;
        }

        Ok(())
    }

    /// Inserts an execution trace record into SQLite and optionally indexes its vector in LanceDB
    pub fn insert_execution_trace(
        &self,
        trace: &ExecutionTraceRecord,
        vector: Option<&[f32]>,
    ) -> Result<()> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.execute(
            "INSERT OR REPLACE INTO execution_trace (trace_id, session_id, tool_name, input_json, output_json, status, duration_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                trace.trace_id,
                trace.session_id,
                trace.tool_name,
                trace.input_json,
                trace.output_json,
                trace.status,
                trace.duration_ms as i64,
                trace.created_at,
            ],
        )
        .map_err(|e| HgbError::Storage(e.to_string()))?;

        drop(conn);

        if let Some(vec) = vector {
            let desc = format!("{} input: {} output: {:?}", trace.tool_name, trace.input_json, trace.output_json);
            let lance_record = VectorRecord::new(&trace.trace_id, vec.to_vec())
                .with_text(desc)
                .with_partition("execution_trace");
            let engine = self.inner.vector_engine.read().map_err(|e| {
                HgbError::Execution(format!("vector_engine read lock error: {}", e))
            })?;
            engine.upsert(vec![lance_record])?;
        }

        Ok(())
    }

    /// Inserts a conversation message into SQLite and optionally indexes its embedding in LanceDB
    pub fn insert_conversation_message(
        &self,
        msg: &ConversationMessage,
        vector: Option<&[f32]>,
    ) -> Result<()> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.execute(
            "INSERT OR REPLACE INTO conversation_logs (message_id, session_id, role, content, category, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                msg.message_id,
                msg.session_id,
                msg.role,
                msg.content,
                msg.category,
                msg.created_at,
            ],
        )
        .map_err(|e| HgbError::Storage(e.to_string()))?;

        let _ = conn.execute(
            "INSERT OR REPLACE INTO synapse_fts (id, session_id, content, category) VALUES (?1, ?2, ?3, ?4)",
            params![msg.message_id, msg.session_id, msg.content, msg.category],
        );

        drop(conn);

        if let Some(vec) = vector {
            let lance_record = VectorRecord::new(&msg.message_id, vec.to_vec())
                .with_text(&msg.content)
                .with_partition("conversation");
            let engine = self.inner.vector_engine.read().map_err(|e| {
                HgbError::Execution(format!("vector_engine read lock error: {}", e))
            })?;
            engine.upsert(vec![lance_record])?;
        }

        Ok(())
    }

    // ========================================================================
    // Entity Retrieval Operations
    // ========================================================================

    pub fn get_relational_record(&self, id: &str) -> Result<Option<RelationalRecord>> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.query_row(
            "SELECT id, session_id, content, category, metadata_json, created_at FROM relational_records WHERE id = ?1",
            params![id],
            |row| {
                Ok(RelationalRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    content: row.get(2)?,
                    category: row.get(3)?,
                    metadata_json: row.get(4)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| HgbError::Storage(e.to_string()))
    }

    pub fn get_agent_checkpoint(&self, checkpoint_id: &str) -> Result<Option<AgentCheckpoint>> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.query_row(
            "SELECT checkpoint_id, session_id, actor_id, step_index, state_hash, payload_json, created_at FROM agent_checkpoints WHERE checkpoint_id = ?1",
            params![checkpoint_id],
            |row| {
                let step: i64 = row.get(3)?;
                Ok(AgentCheckpoint {
                    checkpoint_id: row.get(0)?,
                    session_id: row.get(1)?,
                    actor_id: row.get(2)?,
                    step_index: step as u64,
                    state_hash: row.get(4)?,
                    payload_json: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(|e| HgbError::Storage(e.to_string()))
    }

    pub fn get_execution_trace(&self, trace_id: &str) -> Result<Option<ExecutionTraceRecord>> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock poisoned: {}", e)))?;

        conn.query_row(
            "SELECT trace_id, session_id, tool_name, input_json, output_json, status, duration_ms, created_at FROM execution_trace WHERE trace_id = ?1",
            params![trace_id],
            |row| {
                let dur: i64 = row.get(6)?;
                Ok(ExecutionTraceRecord {
                    trace_id: row.get(0)?,
                    session_id: row.get(1)?,
                    tool_name: row.get(2)?,
                    input_json: row.get(3)?,
                    output_json: row.get(4)?,
                    status: row.get(5)?,
                    duration_ms: dur as u64,
                    created_at: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| HgbError::Storage(e.to_string()))
    }

    // ========================================================================
    // Unified Query Planner Factory & Direct SQL Execution
    // ========================================================================

    /// Instantiates a query planner over this synaptic fabric
    pub fn planner(&self) -> SynapseQueryPlanner {
        SynapseQueryPlanner::new(self.clone())
    }

    /// Executes arbitrary SQL statements against the synaptic database
    pub fn execute_sql(&self, sql: &str) -> Result<usize> {
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock error: {}", e)))?;
        conn.execute_batch(sql)
            .map(|_| 0)
            .map_err(|e| HgbError::Storage(e.to_string()))
    }

    /// Performs raw vector KNN search directly on the underlying LanceDB engine
    pub fn vector_knn(&self, query_vec: &[f32], top_k: usize) -> Result<Vec<SearchResult>> {
        let engine = self.inner.vector_engine.read().map_err(|e| {
            HgbError::Execution(format!("vector_engine read lock error: {}", e))
        })?;
        let query = SearchQuery::new(query_vec.to_vec(), top_k);
        engine.query(&query)
    }

    /// Blends FTS5 full-text lexical search with dense vector retrieval using Reciprocal Rank Fusion
    pub fn hybrid_search_rrf(
        &self,
        query_text: &str,
        query_vec: &[f32],
        top_k: usize,
        rrf_config: Option<RrfConfig>,
    ) -> Result<Vec<HybridFusionResult>> {
        let config = rrf_config.unwrap_or_else(|| self.inner.config.rrf_config.clone());

        // 1. Dense vector nearest neighbors
        let dense_hits = self.vector_knn(query_vec, top_k * 2)?;
        let dense_ranks: Vec<(String, f32)> = dense_hits
            .into_iter()
            .map(|hit| (hit.id, hit.score))
            .collect();

        // 2. Lexical FTS5 search
        let conn = self
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock error: {}", e)))?;

        // Sanitize search tokens for FTS5
        let sanitized = query_text
            .replace('"', "")
            .replace('*', "")
            .split_whitespace()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut sparse_ranks: Vec<(String, f64)> = Vec::new();
        if !sanitized.is_empty() {
            let fts_query = "SELECT id, rank FROM synapse_fts WHERE synapse_fts MATCH ?1 ORDER BY rank LIMIT ?2";
            if let Ok(mut stmt) = conn.prepare(fts_query) {
                let rows = stmt.query_map(params![sanitized, (top_k * 2) as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                });
                if let Ok(mapped) = rows {
                    for r in mapped.flatten() {
                        sparse_ranks.push(r);
                    }
                }
            } else {
                // Fallback table lookup if FTS5 is not available
                let fb_query = "SELECT id FROM synapse_fts_fallback WHERE content LIKE ?1 LIMIT ?2";
                if let Ok(mut stmt) = conn.prepare(fb_query) {
                    let like_arg = format!("%{}%", query_text);
                    let rows = stmt.query_map(params![like_arg, (top_k * 2) as i64], |row| {
                        Ok((row.get::<_, String>(0)?, 1.0f64))
                    });
                    if let Ok(mapped) = rows {
                        for r in mapped.flatten() {
                            sparse_ranks.push(r);
                        }
                    }
                }
            }
        }

        // 3. Compute Reciprocal Rank Fusion
        let mut fused = compute_rrf(&dense_ranks, &sparse_ranks, &config);
        fused.truncate(top_k);

        // 4. Enrich results with relational metadata
        for item in &mut fused {
            let row = conn.query_row(
                "SELECT content, category, metadata_json FROM relational_records WHERE id = ?1",
                params![item.record_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)),
            ).optional().unwrap_or(None);

            if let Some((content, cat, meta)) = row {
                item.content = content;
                item.category = cat;
                item.metadata_json = meta;
            } else {
                // Check conversation_logs if not found in relational_records
                let conv_row = conn.query_row(
                    "SELECT content, category FROM conversation_logs WHERE message_id = ?1",
                    params![item.record_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                ).optional().unwrap_or(None);

                if let Some((content, cat)) = conv_row {
                    item.content = content;
                    item.category = cat;
                }
            }
        }

        Ok(fused)
    }
}

// ============================================================================
// Unified Query Planner ("Synapse")
// ============================================================================

/// Query Plan Summary describing the components of an executed or analyzed query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlanSummary {
    pub raw_sql: String,
    pub uses_vector_knn: bool,
    pub uses_fts5: bool,
    pub relational_tables: Vec<String>,
    pub estimated_cost_us: u64,
    pub explain_steps: Vec<String>,
}

/// Synapse Query Planner unifying relational SQL queries with LanceDB vector KNN joins
#[derive(Clone)]
pub struct SynapseQueryPlanner {
    fabric: SynapticFabric,
}

impl SynapseQueryPlanner {
    pub fn new(fabric: SynapticFabric) -> Self {
        Self { fabric }
    }

    /// Analyzes an incoming query, evaluating relational joins, vector table functions, and estimated cost
    pub fn plan_hybrid_query(&self, sql: &str) -> Result<QueryPlanSummary> {
        let uses_vector = sql.to_lowercase().contains("synapse_vector_knn");
        let uses_fts5 = sql.to_lowercase().contains("synapse_fts");

        let mut relational_tables = Vec::new();
        for table in &[
            "relational_records",
            "agent_checkpoints",
            "execution_trace",
            "conversation_logs",
        ] {
            if sql.to_lowercase().contains(table) {
                relational_tables.push(table.to_string());
            }
        }

        let explain_steps = self.explain_plan(sql)?;

        Ok(QueryPlanSummary {
            raw_sql: sql.to_string(),
            uses_vector_knn: uses_vector,
            uses_fts5,
            relational_tables,
            estimated_cost_us: if uses_vector { 850 } else { 250 },
            explain_steps,
        })
    }

    /// Explains the SQLite query plan using `EXPLAIN QUERY PLAN`
    pub fn explain_plan(&self, sql: &str) -> Result<Vec<String>> {
        let conn = self
            .fabric
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock error: {}", e)))?;

        let explain_sql = format!("EXPLAIN QUERY PLAN {}", sql);
        let mut steps = Vec::new();

        if let Ok(mut stmt) = conn.prepare(&explain_sql) {
            let rows = stmt.query_map([], |row| {
                let detail: String = row.get(3)?;
                Ok(detail)
            });
            if let Ok(mapped) = rows {
                for r in mapped.flatten() {
                    steps.push(r);
                }
            }
        }

        Ok(steps)
    }

    /// Executes the canonical relational-vector KNN join query:
    /// ```sql
    /// SELECT r.id, r.session_id, r.content, v.score
    /// FROM relational_records r
    /// JOIN synapse_vector_knn(?1, ?2) v ON r.id = v.record_id
    /// WHERE (?3 IS NULL OR r.category = ?3)
    /// ORDER BY v.score DESC
    /// ```
    pub fn execute_vector_relational_join(
        &self,
        query_vec: &[f32],
        top_k: usize,
        category: Option<&str>,
    ) -> Result<Vec<RelationalVectorJoinRow>> {
        let query_blob = encode_f32_slice_to_bytes(query_vec);
        let conn = self
            .fabric
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock error: {}", e)))?;

        let sql = r#"
            SELECT r.id, r.session_id, r.content, v.score, r.category
            FROM relational_records r
            JOIN synapse_vector_knn(?1, ?2) v ON r.id = v.record_id
            WHERE (?3 IS NULL OR r.category = ?3)
            ORDER BY v.score DESC
        "#;

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| HgbError::Storage(format!("Prepare join query failed: {}", e)))?;

        let rows = stmt
            .query_map(params![query_blob, top_k as i64, category], |row| {
                let score_f64: f64 = row.get(3)?;
                Ok(RelationalVectorJoinRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    content: row.get(2)?,
                    score: score_f64 as f32,
                    category: row.get(4)?,
                })
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for r in rows {
            results.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
        }

        Ok(results)
    }

    /// Executes an arbitrary SQL query joining SQLite tables with `synapse_vector_knn`
    pub fn execute_sql_query<T, F>(
        &self,
        sql: &str,
        params: &[&(dyn rusqlite::ToSql)],
        mapper: F,
    ) -> Result<Vec<T>>
    where
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let conn = self
            .fabric
            .inner
            .db_conn
            .lock()
            .map_err(|e| HgbError::Execution(format!("db_conn lock error: {}", e)))?;

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| HgbError::Storage(format!("Prepare SQL failed: {}", e)))?;

        let mapped = stmt
            .query_map(params, mapper)
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for item in mapped {
            results.push(item.map_err(|e| HgbError::Storage(e.to_string()))?);
        }

        Ok(results)
    }

    /// Blends FTS5 full-text lexical search and vector KNN search via Reciprocal Rank Fusion
    pub fn execute_hybrid_rrf(
        &self,
        query_text: &str,
        query_vec: &[f32],
        top_k: usize,
        config: Option<RrfConfig>,
    ) -> Result<Vec<HybridFusionResult>> {
        self.fabric.hybrid_search_rrf(query_text, query_vec, top_k, config)
    }
}

// ============================================================================
// Comprehensive Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_sql_vector_functions() {
        let fabric = SynapticFabric::in_memory(4).expect("Failed to create fabric");
        let conn = fabric.inner.db_conn.lock().unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.0f32, 1.0, 0.0, 0.0];
        let v3 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v_neg = vec![-1.0f32, 0.0, 0.0, 0.0];

        let b1 = encode_f32_slice_to_bytes(&v1);
        let b2 = encode_f32_slice_to_bytes(&v2);
        let b3 = encode_f32_slice_to_bytes(&v3);
        let b_neg = encode_f32_slice_to_bytes(&v_neg);

        // 1. Orthogonal vectors: dot_product = 0.0, cosine_distance = 1.0
        let dot: f64 = conn
            .query_row(
                "SELECT synapse_dot_product(?1, ?2)",
                params![b1, b2],
                |r| r.get(0),
            )
            .expect("dot product failed");
        assert!((dot - 0.0).abs() < 1e-5);

        let dist: f64 = conn
            .query_row(
                "SELECT synapse_cosine_distance(?1, ?2)",
                params![b1, b2],
                |r| r.get(0),
            )
            .expect("cosine distance failed");
        assert!((dist - 1.0).abs() < 1e-5);

        // 2. Collinear identical vectors: cosine_distance = 0.0, cosine_similarity = 1.0
        let dist_same: f64 = conn
            .query_row(
                "SELECT synapse_cosine_distance(?1, ?2)",
                params![b1, b3],
                |r| r.get(0),
            )
            .expect("cosine distance failed");
        assert!((dist_same - 0.0).abs() < 1e-5);

        let sim_same: f64 = conn
            .query_row(
                "SELECT synapse_cosine_similarity(?1, ?2)",
                params![b1, b3],
                |r| r.get(0),
            )
            .expect("cosine similarity failed");
        assert!((sim_same - 1.0).abs() < 1e-5);

        // 3. Opposite vectors: cosine_distance = 2.0
        let dist_neg: f64 = conn
            .query_row(
                "SELECT synapse_cosine_distance(?1, ?2)",
                params![b1, b_neg],
                |r| r.get(0),
            )
            .expect("cosine distance negative failed");
        assert!((dist_neg - 2.0).abs() < 1e-5);

        // 4. Dimension mismatch error handling
        let short_vec = encode_f32_slice_to_bytes(&[1.0f32, 2.0]);
        let err_res: rusqlite::Result<f64> = conn.query_row(
            "SELECT synapse_dot_product(?1, ?2)",
            params![b1, short_vec],
            |r| r.get(0),
        );
        assert!(err_res.is_err(), "Expected dimension mismatch error");
    }

    #[test]
    fn test_relational_vector_knn_join() {
        let fabric = SynapticFabric::in_memory(4).expect("Failed to create fabric");

        // Insert relational records with dense vectors
        let r1 = RelationalRecord::new("doc-1", "sess-1", "Rust compiler internals")
            .with_category("systems");
        let r2 = RelationalRecord::new("doc-2", "sess-1", "Database storage engine architecture")
            .with_category("systems");
        let r3 = RelationalRecord::new("doc-3", "sess-2", "Frontend React UI components")
            .with_category("frontend");

        fabric
            .insert_relational_record(&r1, Some(&[1.0, 0.0, 0.0, 0.0]))
            .expect("insert r1");
        fabric
            .insert_relational_record(&r2, Some(&[0.9, 0.1, 0.0, 0.0]))
            .expect("insert r2");
        fabric
            .insert_relational_record(&r3, Some(&[0.0, 0.0, 0.0, 1.0]))
            .expect("insert r3");

        let planner = fabric.planner();
        let query_vec = [1.0f32, 0.0, 0.0, 0.0];

        // Join query with category filter = "systems"
        let join_results = planner
            .execute_vector_relational_join(&query_vec, 5, Some("systems"))
            .expect("join query");

        assert_eq!(join_results.len(), 2);
        assert_eq!(join_results[0].id, "doc-1");
        assert!(join_results[0].score > 0.99);
        assert_eq!(join_results[1].id, "doc-2");
        assert!(join_results[1].score > 0.85);

        // Verify native SQL syntax matching requirement:
        // SELECT r.id, r.session_id, r.content, v.score
        // FROM relational_records r
        // JOIN synapse_vector_knn(?1, ?2) v ON r.id = v.record_id
        // WHERE r.category = ?3
        // ORDER BY v.score DESC
        let blob = encode_f32_slice_to_bytes(&query_vec);
        let raw_rows = planner
            .execute_sql_query(
                "SELECT r.id, r.session_id, r.content, v.score
                 FROM relational_records r
                 JOIN synapse_vector_knn(?1, ?2) v ON r.id = v.record_id
                 WHERE r.category = ?3
                 ORDER BY v.score DESC",
                &[&blob, &5i64, &"systems"],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                },
            )
            .expect("raw sql join query");

        assert_eq!(raw_rows.len(), 2);
        assert_eq!(raw_rows[0].0, "doc-1");
        assert_eq!(raw_rows[1].0, "doc-2");
    }

    #[test]
    fn test_agent_checkpoint_and_execution_trace_joins() {
        let fabric = SynapticFabric::in_memory(4).expect("Failed to create fabric");

        let chk1 = AgentCheckpoint {
            checkpoint_id: "chk-001".to_string(),
            session_id: "sess-abc".to_string(),
            actor_id: "actor-planner".to_string(),
            step_index: 10,
            state_hash: "hash_0x123".to_string(),
            payload_json: r#"{"status":"ready","plan":["inspect","compile"]}"#.to_string(),
            created_at: 1000,
        };
        let chk2 = AgentCheckpoint {
            checkpoint_id: "chk-002".to_string(),
            session_id: "sess-abc".to_string(),
            actor_id: "actor-worker".to_string(),
            step_index: 11,
            state_hash: "hash_0x456".to_string(),
            payload_json: r#"{"status":"executing"}"#.to_string(),
            created_at: 1005,
        };

        fabric
            .insert_agent_checkpoint(&chk1, Some(&[0.8, 0.6, 0.0, 0.0]))
            .expect("insert chk1");
        fabric
            .insert_agent_checkpoint(&chk2, Some(&[0.0, 0.0, 1.0, 0.0]))
            .expect("insert chk2");

        let planner = fabric.planner();
        let target_vec = [0.8f32, 0.6, 0.0, 0.0];
        let blob = encode_f32_slice_to_bytes(&target_vec);

        let results = planner
            .execute_sql_query(
                "SELECT c.checkpoint_id, c.session_id, c.actor_id, c.step_index, v.score
                 FROM agent_checkpoints c
                 JOIN synapse_vector_knn(?1, ?2) v ON c.checkpoint_id = v.record_id
                 ORDER BY v.score DESC",
                &[&blob, &2i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                },
            )
            .expect("checkpoint join");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "chk-001");
        assert_eq!(results[0].2, "actor-planner");
        assert!((results[0].4 - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_fts5_lexical_search_integration() {
        let fabric = SynapticFabric::in_memory(4).expect("Failed to create fabric");

        let m1 = ConversationMessage {
            message_id: "msg-1".to_string(),
            session_id: "s1".to_string(),
            role: "user".to_string(),
            content: "How do I optimize database performance with memory mapping?".to_string(),
            category: Some("database".to_string()),
            created_at: 100,
        };
        let m2 = ConversationMessage {
            message_id: "msg-2".to_string(),
            session_id: "s1".to_string(),
            role: "assistant".to_string(),
            content: "You should enable SQLite WAL mode and configure mmap_size.".to_string(),
            category: Some("database".to_string()),
            created_at: 101,
        };

        fabric
            .insert_conversation_message(&m1, Some(&[0.5, 0.5, 0.0, 0.0]))
            .expect("insert m1");
        fabric
            .insert_conversation_message(&m2, Some(&[0.0, 0.5, 0.5, 0.0]))
            .expect("insert m2");

        let conn = fabric.inner.db_conn.lock().unwrap();
        let fts_hits: Vec<String> = conn
            .prepare("SELECT id FROM synapse_fts WHERE synapse_fts MATCH 'performance OR wal' ORDER BY rank")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| r.get(0))
                    .and_then(|rows| rows.collect())
            })
            .unwrap_or_default();

        assert!(!fts_hits.is_empty(), "FTS5 query should return matching IDs");
    }

    #[test]
    fn test_reciprocal_rank_fusion_math_and_hybrid() {
        let dense_ranks = vec![
            ("doc-dense-1".to_string(), 0.98f32),
            ("doc-shared".to_string(), 0.85f32),
            ("doc-dense-2".to_string(), 0.70f32),
        ];
        let sparse_ranks = vec![
            ("doc-shared".to_string(), -10.5f64),
            ("doc-sparse-1".to_string(), -8.2f64),
        ];

        let config = RrfConfig {
            k: 60.0,
            w_dense: 0.7,
            w_sparse: 0.3,
        };

        let fused = compute_rrf(&dense_ranks, &sparse_ranks, &config);

        // doc-shared is in both lists:
        // dense rank = 2, sparse rank = 1
        // rrf = 0.7 / (60 + 2) + 0.3 / (60 + 1) = 0.7/62 + 0.3/61 = 0.0112903 + 0.0049180 = ~0.0162083
        // doc-dense-1: dense rank = 1, sparse rank = None
        // rrf = 0.7 / 61 = ~0.0114754
        // Therefore doc-shared must be ranked 1st!
        assert_eq!(fused[0].record_id, "doc-shared");
        assert_eq!(fused[1].record_id, "doc-dense-1");

        let fabric = SynapticFabric::in_memory(4).expect("fabric");
        let rec1 = RelationalRecord::new("k-1", "s1", "Distributed vector indexing in Rust");
        let rec2 = RelationalRecord::new("k-2", "s1", "LanceDB columnar fragment format");
        fabric
            .insert_relational_record(&rec1, Some(&[1.0, 0.0, 0.0, 0.0]))
            .expect("r1");
        fabric
            .insert_relational_record(&rec2, Some(&[0.0, 1.0, 0.0, 0.0]))
            .expect("r2");

        let hybrid_hits = fabric
            .hybrid_search_rrf("Rust vector", &[1.0, 0.0, 0.0, 0.0], 5, None)
            .expect("hybrid rrf");
        assert!(!hybrid_hits.is_empty());
        assert_eq!(hybrid_hits[0].record_id, "k-1");
    }

    #[test]
    fn test_multithreaded_concurrent_access() {
        let fabric = Arc::new(SynapticFabric::in_memory(4).expect("fabric"));
        let mut handles = Vec::new();

        // Spawn 8 concurrent worker threads
        for t_idx in 0..8 {
            let fabric_clone = Arc::clone(&fabric);
            let handle = std::thread::spawn(move || {
                for i in 0..15 {
                    let id = format!("thread-{}-rec-{}", t_idx, i);
                    let rec = RelationalRecord::new(&id, "bench-sess", format!("Record content {}", i));
                    let vec = [0.1 * (t_idx as f32), 0.2, 0.3, 0.4];
                    fabric_clone
                        .insert_relational_record(&rec, Some(&vec))
                        .expect("thread insert");

                    // Query planner KNN join
                    let planner = fabric_clone.planner();
                    let hits = planner
                        .execute_vector_relational_join(&vec, 3, None)
                        .expect("thread knn join");
                    assert!(!hits.is_empty());
                }
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().expect("Thread panicked");
        }
    }

    #[test]
    fn test_zero_copy_and_simd_kernels() {
        let a = vec![0.5f32, -0.25, 0.75, 1.0, 0.1, -0.8, 0.4, 0.9];
        let b = vec![0.5f32, -0.25, 0.75, 1.0, 0.1, -0.8, 0.4, 0.9];

        let bytes = encode_f32_slice_to_bytes(&a);
        assert_eq!(bytes.len(), 32);

        let decoded = decode_bytes_to_f32_vec(&bytes).expect("decode");
        assert_eq!(a, decoded);

        let dot = simd_dot_product(&a, &b);
        let sim = simd_cosine_similarity(&a, &b);
        let dist = simd_cosine_distance(&a, &b);

        assert!(dot > 0.0);
        assert!((sim - 1.0).abs() < 1e-5);
        assert!(dist < 1e-5);

        // Test with_f32_slice over byte slice
        let res = with_f32_slice(&bytes, |slice| slice[0]);
        assert_eq!(res.unwrap(), 0.5f32);
    }
}
