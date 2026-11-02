//! # Vector-Native SQLite & In-Process HNSW Storage (`vector_native_sqlite_vss`)
//!
//! Ultra-high performance vector storage engine embedding SQLite with native Hierarchical Navigable
//! Small World (HNSW) indexing and SIMD-accelerated linear algebra kernels (AVX2 + FMA, SSE4.1, unrolled fallback).
//!
//! Features:
//! - Table `vector_native_index`: `id TEXT PRIMARY KEY`, `namespace TEXT`, `dimensions INTEGER`,
//!   `vector_blob BLOB`, `metadata_json TEXT`, `created_at INTEGER`.
//! - Sub-millisecond similarity search (`search_similar(query_vector: &[f32], top_k: usize) -> Result<Vec<VectorSearchResult>>`).
//! - Zero-copy IEEE-754 little-endian `f32` tensor serialization without JSON or string overhead.
//! - Atomic transaction commits with SQLite WAL mode (`PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL`).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use hgb_core::{HgbError, Result};

/// Encodes a slice of IEEE-754 `f32` floats into a raw little-endian binary BLOB.
/// Zero allocations beyond the destination byte vector.
#[inline]
pub fn encode_vector_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vector.len() * 4);
    for &val in vector {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Decodes raw little-endian binary bytes into an `f32` vector.
/// Validates that the input length is a strict multiple of 4 bytes.
#[inline]
pub fn decode_blob_to_vector(blob: &[u8]) -> Result<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(HgbError::Storage(format!(
            "Invalid vector blob byte length {}: must be a multiple of 4",
            blob.len()
        )));
    }
    let count = blob.len() / 4;
    let mut vec = Vec::with_capacity(count);
    for chunk in blob.chunks_exact(4) {
        vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(vec)
}

/// Decodes raw little-endian bytes into an existing destination buffer to eliminate allocations
#[inline]
pub fn decode_blob_into(blob: &[u8], dest: &mut Vec<f32>) -> Result<()> {
    if blob.len() % 4 != 0 {
        return Err(HgbError::Storage(format!(
            "Invalid vector blob byte length {}: must be a multiple of 4",
            blob.len()
        )));
    }
    dest.clear();
    dest.reserve(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        dest.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(())
}

/// Zero-copy borrow of raw blob as an aligned IEEE-754 `&[f32]` slice when memory alignment permits
#[inline]
pub fn as_f32_slice(blob: &[u8]) -> Result<&[f32]> {
    if blob.len() % 4 != 0 {
        return Err(HgbError::Storage(format!(
            "Invalid vector blob length {}: not a multiple of 4",
            blob.len()
        )));
    }
    let ptr = blob.as_ptr();
    if ptr as usize % std::mem::align_of::<f32>() != 0 {
        return Err(HgbError::Storage(
            "Vector blob memory pointer is unaligned for f32 access".to_string(),
        ));
    }
    let count = blob.len() / 4;
    Ok(unsafe { std::slice::from_raw_parts(ptr as *const f32, count) })
}

// ============================================================================
// SIMD Linear Algebra Kernels (AVX2 + FMA, SSE4.1, Unrolled Fallback)
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len();
    let mut sum256 = _mm256_setzero_ps();
    let mut i = 0;

    while i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        sum256 = _mm256_fmadd_ps(va, vb, sum256);
        i += 8;
    }

    let mut buf = [0.0f32; 8];
    _mm256_storeu_ps(buf.as_mut_ptr(), sum256);
    let mut dot: f32 = buf.iter().sum();

    while i < len {
        dot += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    dot
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn dot_product_sse41(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len();
    let mut sum128 = _mm_setzero_ps();
    let mut i = 0;

    while i + 4 <= len {
        let va = _mm_loadu_ps(a.as_ptr().add(i));
        let vb = _mm_loadu_ps(b.as_ptr().add(i));
        sum128 = _mm_add_ps(sum128, _mm_mul_ps(va, vb));
        i += 4;
    }

    let mut buf = [0.0f32; 4];
    _mm_storeu_ps(buf.as_mut_ptr(), sum128);
    let mut dot: f32 = buf.iter().sum();

    while i < len {
        dot += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    dot
}

#[inline]
fn dot_product_fallback(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let chunks_a = a.chunks_exact(8);
    let chunks_b = b.chunks_exact(8);
    let rem_a = chunks_a.remainder();
    let rem_b = chunks_b.remainder();

    for (ca, cb) in chunks_a.zip(chunks_b) {
        dot += ca[0] * cb[0]
            + ca[1] * cb[1]
            + ca[2] * cb[2]
            + ca[3] * cb[3]
            + ca[4] * cb[4]
            + ca[5] * cb[5]
            + ca[6] * cb[6]
            + ca[7] * cb[7];
    }
    for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
        dot += x * y;
    }
    dot
}

/// Dynamically dispatches dot product computation across AVX2+FMA, SSE4.1, or 8-way unrolled fallback
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
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

    dot_product_fallback(a, b)
}

/// Computes cosine similarity between two vector slices using SIMD dot product
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot = dot_product(a, b);
    let norm_a = dot_product(a, a);
    let norm_b = dot_product(b, b);
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// Computes cosine distance: `1.0 - cosine_similarity(a, b)` (Range: `[0.0, 2.0]`)
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    1.0 - cosine_similarity(a, b)
}

// ============================================================================
// Data Models & Configuration
// ============================================================================

/// Individual vector entry stored in SQLite table `vector_native_index`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorRecord {
    pub id: String,
    pub namespace: String,
    pub dimensions: usize,
    pub vector_blob: Vec<u8>,
    pub metadata_json: Option<String>,
    pub created_at: i64,
}

impl VectorRecord {
    /// Constructs a record by serializing an `&[f32]` float vector
    pub fn new(
        id: impl Into<String>,
        namespace: impl Into<String>,
        vector: &[f32],
        metadata_json: Option<String>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        Self {
            id: id.into(),
            namespace: namespace.into(),
            dimensions: vector.len(),
            vector_blob: encode_vector_to_blob(vector),
            metadata_json,
            created_at: now,
        }
    }

    /// Decodes the binary float BLOB into an owned `Vec<f32>`
    #[inline]
    pub fn vector(&self) -> Result<Vec<f32>> {
        decode_blob_to_vector(&self.vector_blob)
    }
}

/// Search result item returned from `search_similar`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub id: String,
    pub namespace: String,
    pub score: f32,
    pub distance: f32,
    pub metadata_json: Option<String>,
}

/// Type alias for VectorSearchResult
pub type VectorNativeSearchResult = VectorSearchResult;

impl VectorSearchResult {
    #[inline]
    pub fn collection(&self) -> &str {
        &self.namespace
    }

    #[inline]
    pub fn metadata(&self) -> &str {
        self.metadata_json.as_deref().unwrap_or("")
    }
}

/// HNSW indexing graph configuration parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorIndexConfig {
    /// Maximum outgoing edges per node on upper layers (l > 0)
    pub m: usize,
    /// Maximum outgoing edges per node on bottom layer (layer 0)
    pub m0: usize,
    /// Candidate pool size during graph construction
    pub ef_construction: usize,
    /// Candidate pool size during similarity search
    pub ef_search: usize,
    /// Expected vector dimensions (0 for dynamic detection)
    pub expected_dimensions: usize,
}

impl Default for VectorIndexConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 100,
            ef_search: 64,
            expected_dimensions: 0,
        }
    }
}

/// Status and telemetry statistics of the vector-native store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorStoreStats {
    pub total_vectors: usize,
    pub namespaces: HashMap<String, usize>,
    pub dimensions: usize,
    pub max_hnsw_level: usize,
    pub db_path: String,
}

// ============================================================================
// Deterministic PRNG & In-Process HNSW Graph Index Cache
// ============================================================================

/// Deterministic, zero-dependency fast PRNG for geometric level sampling
#[derive(Debug, Clone)]
pub struct FastRng {
    state: u64,
}

impl FastRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x517cc1b727220a95 } else { seed },
        }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

#[derive(Debug, Clone)]
struct HnswNode {
    id: String,
    namespace: String,
    level: usize,
    vector: Vec<f32>,
    metadata_json: Option<String>,
    neighbors: Vec<Vec<String>>, // neighbors[layer]
}

#[derive(Copy, Clone, PartialEq)]
struct Candidate {
    dist: f32,
    id_hash: u64,
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist
            .partial_cmp(&other.dist)
            .unwrap_or(Ordering::Equal)
    }
}

#[derive(Debug)]
struct HnswGraphCache {
    nodes: HashMap<String, HnswNode>,
    enter_point_id: Option<String>,
    max_level: usize,
    rng: FastRng,
    config: VectorIndexConfig,
    dimensions: usize,
}

impl HnswGraphCache {
    fn new(config: VectorIndexConfig) -> Self {
        Self {
            nodes: HashMap::new(),
            enter_point_id: None,
            max_level: 0,
            rng: FastRng::new(0x48474253),
            dimensions: config.expected_dimensions,
            config,
        }
    }

    fn sample_level(&mut self) -> usize {
        let m = self.config.m.max(2);
        let ml = 1.0 / (m as f64).ln();
        let uniform = self.rng.next_f64().max(1e-9);
        let lvl = (-uniform.ln() * ml).floor() as usize;
        lvl.min(16)
    }

    fn insert(&mut self, record: VectorRecord, vector: Vec<f32>) {
        if self.dimensions == 0 {
            self.dimensions = vector.len();
        }

        let level = self.sample_level();
        let node_id = record.id.clone();

        if self.enter_point_id.is_none() {
            let mut neighbors = Vec::with_capacity(level + 1);
            for _ in 0..=level {
                neighbors.push(Vec::new());
            }
            let node = HnswNode {
                id: node_id.clone(),
                namespace: record.namespace,
                level,
                vector,
                metadata_json: record.metadata_json,
                neighbors,
            };
            self.nodes.insert(node_id.clone(), node);
            self.enter_point_id = Some(node_id);
            self.max_level = level;
            return;
        }

        let enter_id = self.enter_point_id.clone().unwrap();
        let mut curr_obj = enter_id;

        // Greedy search through layers higher than node's assigned level
        if self.max_level > level {
            for l in (level + 1..=self.max_level).rev() {
                let mut changed = true;
                while changed {
                    changed = false;
                    let curr_vec = match self.nodes.get(&curr_obj) {
                        Some(n) => &n.vector,
                        None => break,
                    };
                    let mut best_dist = cosine_distance(&vector, curr_vec);

                    let neighbors = match self.nodes.get(&curr_obj) {
                        Some(n) if l < n.neighbors.len() => n.neighbors[l].clone(),
                        _ => Vec::new(),
                    };

                    for neighbor_id in neighbors {
                        if let Some(n_node) = self.nodes.get(&neighbor_id) {
                            let d = cosine_distance(&vector, &n_node.vector);
                            if d < best_dist {
                                best_dist = d;
                                curr_obj = neighbor_id;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        let mut node_neighbors = Vec::with_capacity(level + 1);
        for _ in 0..=level {
            node_neighbors.push(Vec::new());
        }

        // Bottom-up insertion from min(level, max_level) down to layer 0
        let search_top = level.min(self.max_level);
        for l in (0..=search_top).rev() {
            let candidates = self.search_layer_candidates(&vector, &curr_obj, self.config.ef_construction, l);
            let m_max = if l == 0 { self.config.m0 } else { self.config.m };

            let mut selected: Vec<String> = Vec::with_capacity(m_max);
            for (dist, cand_id) in candidates {
                let _ = dist;
                if cand_id != node_id && !selected.contains(&cand_id) {
                    selected.push(cand_id);
                    if selected.len() >= m_max {
                        break;
                    }
                }
            }

            node_neighbors[l] = selected.clone();

            for neighbor_id in &selected {
                let need_prune = if let Some(n_node) = self.nodes.get_mut(neighbor_id) {
                    if l < n_node.neighbors.len() {
                        if !n_node.neighbors[l].contains(&node_id) {
                            n_node.neighbors[l].push(node_id.clone());
                        }
                        n_node.neighbors[l].len() > m_max
                    } else {
                        false
                    }
                } else {
                    false
                };

                if need_prune {
                    let (n_vec, current_neighbors) = {
                        let n_node = self.nodes.get(neighbor_id).unwrap();
                        (n_node.vector.clone(), n_node.neighbors[l].clone())
                    };

                    let mut ranked: Vec<(f32, String)> = current_neighbors
                        .iter()
                        .filter_map(|other_id| {
                            self.nodes.get(other_id).map(|on| {
                                (cosine_distance(&n_vec, &on.vector), other_id.clone())
                            })
                        })
                        .collect();
                    ranked.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
                    let pruned: Vec<String> = ranked.into_iter().take(m_max).map(|(_, id)| id).collect();

                    if let Some(n_node) = self.nodes.get_mut(neighbor_id) {
                        n_node.neighbors[l] = pruned;
                    }
                }
            }

            if let Some(first) = selected.first() {
                curr_obj = first.clone();
            }
        }

        let new_node = HnswNode {
            id: node_id.clone(),
            namespace: record.namespace,
            level,
            vector,
            metadata_json: record.metadata_json,
            neighbors: node_neighbors,
        };
        self.nodes.insert(node_id.clone(), new_node);

        if level > self.max_level {
            self.max_level = level;
            self.enter_point_id = Some(node_id);
        }
    }

    fn search_layer_candidates(
        &self,
        query: &[f32],
        enter_id: &str,
        ef: usize,
        layer: usize,
    ) -> Vec<(f32, String)> {
        let mut visited = HashSet::new();
        visited.insert(enter_id.to_string());

        let enter_node = match self.nodes.get(enter_id) {
            Some(n) => n,
            None => return Vec::new(),
        };

        let enter_dist = cosine_distance(query, &enter_node.vector);

        // Min-heap for nearest candidates exploration
        let mut candidates = BinaryHeap::new();
        candidates.push(std::cmp::Reverse((Candidate { dist: enter_dist, id_hash: 0 }, enter_id.to_string())));

        // Max-heap for keeping the best ef elements found so far
        let mut best = BinaryHeap::new();
        best.push((Candidate { dist: enter_dist, id_hash: 0 }, enter_id.to_string()));

        while let Some(std::cmp::Reverse((cand, curr_id))) = candidates.pop() {
            let furthest_best = match best.peek() {
                Some((b, _)) => b.dist,
                None => f32::INFINITY,
            };

            if cand.dist > furthest_best && best.len() >= ef {
                break;
            }

            let neighbors = match self.nodes.get(&curr_id) {
                Some(n) if layer < n.neighbors.len() => &n.neighbors[layer],
                _ => continue,
            };

            for neighbor_id in neighbors {
                if visited.insert(neighbor_id.clone()) {
                    if let Some(n_node) = self.nodes.get(neighbor_id) {
                        let d = cosine_distance(query, &n_node.vector);
                        let furthest_best = best.peek().map(|(b, _)| b.dist).unwrap_or(f32::INFINITY);

                        if d < furthest_best || best.len() < ef {
                            candidates.push(std::cmp::Reverse((Candidate { dist: d, id_hash: 0 }, neighbor_id.clone())));
                            best.push((Candidate { dist: d, id_hash: 0 }, neighbor_id.clone()));
                            if best.len() > ef {
                                best.pop();
                            }
                        }
                    }
                }
            }
        }

        let mut results: Vec<(f32, String)> = best
            .into_iter()
            .map(|(c, id)| (c.dist, id))
            .collect();
        results.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        results
    }

    fn search_knn(
        &self,
        query: &[f32],
        top_k: usize,
        namespace_filter: Option<&str>,
    ) -> Vec<VectorSearchResult> {
        if self.nodes.is_empty() || self.enter_point_id.is_none() {
            return Vec::new();
        }

        let mut curr_obj = self.enter_point_id.clone().unwrap();

        // 1. Greedily descend from max_level down to layer 1
        for l in (1..=self.max_level).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                let curr_vec = match self.nodes.get(&curr_obj) {
                    Some(n) => &n.vector,
                    None => break,
                };
                let mut best_dist = cosine_distance(query, curr_vec);

                let neighbors = match self.nodes.get(&curr_obj) {
                    Some(n) if l < n.neighbors.len() => &n.neighbors[l],
                    _ => continue,
                };

                for neighbor_id in neighbors {
                    if let Some(n_node) = self.nodes.get(neighbor_id) {
                        let d = cosine_distance(query, &n_node.vector);
                        if d < best_dist {
                            best_dist = d;
                            curr_obj = neighbor_id.clone();
                            changed = true;
                        }
                    }
                }
            }
        }

        // 2. Beam search at layer 0 with ef candidate pool
        let ef = self.config.ef_search.max(top_k * 2);
        let candidates = self.search_layer_candidates(query, &curr_obj, ef, 0);

        let mut results = Vec::with_capacity(top_k);
        for (dist, id) in candidates {
            if let Some(node) = self.nodes.get(&id) {
                if let Some(ns) = namespace_filter {
                    if node.namespace != ns {
                        continue;
                    }
                }
                let score = (1.0 - dist).clamp(-1.0, 1.0);
                results.push(VectorSearchResult {
                    id: node.id.clone(),
                    namespace: node.namespace.clone(),
                    score,
                    distance: dist,
                    metadata_json: node.metadata_json.clone(),
                });
                if results.len() >= top_k {
                    break;
                }
            }
        }

        // Fallback: If HNSW candidate search yielded fewer items than top_k (e.g. strict namespace filter),
        // perform full scan across remaining namespace vectors
        if results.len() < top_k && self.nodes.len() < 50_000 {
            let existing_ids: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
            let mut fallback_candidates = Vec::new();

            for (id, node) in &self.nodes {
                if existing_ids.contains(id) {
                    continue;
                }
                if let Some(ns) = namespace_filter {
                    if node.namespace != ns {
                        continue;
                    }
                }
                let d = cosine_distance(query, &node.vector);
                fallback_candidates.push((d, node));
            }

            fallback_candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
            for (dist, node) in fallback_candidates {
                results.push(VectorSearchResult {
                    id: node.id.clone(),
                    namespace: node.namespace.clone(),
                    score: (1.0 - dist).clamp(-1.0, 1.0),
                    distance: dist,
                    metadata_json: node.metadata_json.clone(),
                });
                if results.len() >= top_k {
                    break;
                }
            }
        }

        results
    }

    fn remove(&mut self, id: &str) -> bool {
        if let Some(node) = self.nodes.remove(id) {
            for (l, layer_neighbors) in node.neighbors.into_iter().enumerate() {
                for neighbor_id in layer_neighbors {
                    if let Some(n_node) = self.nodes.get_mut(&neighbor_id) {
                        if l < n_node.neighbors.len() {
                            n_node.neighbors[l].retain(|other| other != id);
                        }
                    }
                }
            }
            if self.enter_point_id.as_deref() == Some(id) {
                self.enter_point_id = self.nodes.keys().next().cloned();
                self.max_level = self
                    .enter_point_id
                    .as_ref()
                    .and_then(|eid| self.nodes.get(eid))
                    .map(|n| n.level)
                    .unwrap_or(0);
            }
            true
        } else {
            false
        }
    }
}

// ============================================================================
// VectorNativeSqliteStore
// ============================================================================

#[inline]
fn sql_err(e: rusqlite::Error) -> HgbError {
    HgbError::Storage(format!("SQLite VSS error: {}", e))
}

/// Vector-Native SQLite & In-Process HNSW Storage Engine
pub struct VectorNativeSqliteStore {
    conn: Arc<Mutex<Connection>>,
    hnsw: Arc<RwLock<HnswGraphCache>>,
    config: VectorIndexConfig,
    db_path: PathBuf,
}

impl VectorNativeSqliteStore {
    /// Opens or creates a vector store backed by SQLite at `path`
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with_config(path, VectorIndexConfig::default())
    }

    /// Opens an in-memory SQLite vector store (`:memory:`)
    pub fn open_in_memory() -> Result<Self> {
        Self::open(":memory:")
    }

    /// Opens with custom HNSW configuration settings
    pub fn open_with_config<P: AsRef<Path>>(path: P, config: VectorIndexConfig) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let is_memory = db_path.to_string_lossy() == ":memory:";
        let conn = if is_memory {
            Connection::open_in_memory().map_err(sql_err)?
        } else {
            Connection::open(&db_path).map_err(sql_err)?
        };

        // Initialize WAL mode and high-performance PRAGMAs
        if !is_memory {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA temp_store = MEMORY;
                 PRAGMA cache_size = -64000;",
            )
            .map_err(sql_err)?;
        }

        // Create table vector_native_index
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vector_native_index (
                id TEXT PRIMARY KEY,
                namespace TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                vector_blob BLOB NOT NULL,
                metadata_json TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vss_namespace ON vector_native_index(namespace);
            CREATE INDEX IF NOT EXISTS idx_vss_created_at ON vector_native_index(created_at);",
        )
        .map_err(sql_err)?;

        let mut hnsw_cache = HnswGraphCache::new(config.clone());

        // Reconstruct in-memory HNSW graph cache from existing SQLite vectors
        {
            let mut stmt = conn
                .prepare(
                    "SELECT id, namespace, dimensions, vector_blob, metadata_json, created_at FROM vector_native_index",
                )
                .map_err(sql_err)?;

            let rows = stmt
                .query_map([], |row| {
                    Ok(VectorRecord {
                        id: row.get(0)?,
                        namespace: row.get(1)?,
                        dimensions: row.get(2)?,
                        vector_blob: row.get(3)?,
                        metadata_json: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                })
                .map_err(sql_err)?;

            for row in rows {
                let rec = row.map_err(sql_err)?;
                if let Ok(vec) = decode_blob_to_vector(&rec.vector_blob) {
                    hnsw_cache.insert(rec, vec);
                }
            }
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            hnsw: Arc::new(RwLock::new(hnsw_cache)),
            config,
            db_path,
        })
    }

    /// Inserts or updates an individual vector entry into SQLite and the HNSW graph cache
    pub fn insert_vector(
        &self,
        id: &str,
        namespace: &str,
        vector: &[f32],
        metadata_json: Option<&str>,
    ) -> Result<()> {
        let record = VectorRecord::new(id, namespace, vector, metadata_json.map(|s| s.to_string()));
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO vector_native_index (id, namespace, dimensions, vector_blob, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 namespace = excluded.namespace,
                 dimensions = excluded.dimensions,
                 vector_blob = excluded.vector_blob,
                 metadata_json = excluded.metadata_json,
                 created_at = excluded.created_at",
            params![
                record.id,
                record.namespace,
                record.dimensions,
                record.vector_blob,
                record.metadata_json,
                record.created_at,
            ],
        )
        .map_err(sql_err)?;

        let mut hnsw = self.hnsw.write().unwrap();
        hnsw.insert(record, vector.to_vec());
        Ok(())
    }

    /// Batch inserts vector entries atomically within a single SQLite transaction
    pub fn insert_vector_batch(&self, records: &[VectorRecord]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sql_err)?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO vector_native_index (id, namespace, dimensions, vector_blob, metadata_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(id) DO UPDATE SET
                         namespace = excluded.namespace,
                         dimensions = excluded.dimensions,
                         vector_blob = excluded.vector_blob,
                         metadata_json = excluded.metadata_json,
                         created_at = excluded.created_at",
                )
                .map_err(sql_err)?;

            for r in records {
                stmt.execute(params![
                    r.id,
                    r.namespace,
                    r.dimensions,
                    r.vector_blob,
                    r.metadata_json,
                    r.created_at,
                ])
                .map_err(sql_err)?;
            }
        }

        tx.commit().map_err(sql_err)?;

        // Update HNSW in-memory graph cache
        let mut hnsw = self.hnsw.write().unwrap();
        for r in records {
            if let Ok(vec) = decode_blob_to_vector(&r.vector_blob) {
                hnsw.insert(r.clone(), vec);
            }
        }

        Ok(())
    }

    /// Sub-millisecond similarity search returning the top-k most similar vectors
    pub fn search_similar(
        &self,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<VectorSearchResult>> {
        let hnsw = self.hnsw.read().unwrap();
        Ok(hnsw.search_knn(query_vector, top_k, None))
    }

    /// Similarity search scoped strictly to a specific namespace
    pub fn search_similar_in_namespace(
        &self,
        namespace: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<VectorSearchResult>> {
        let hnsw = self.hnsw.read().unwrap();
        Ok(hnsw.search_knn(query_vector, top_k, Some(namespace)))
    }

    /// Retrieves an individual vector record by its ID
    pub fn get_vector(&self, id: &str) -> Result<Option<VectorRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, namespace, dimensions, vector_blob, metadata_json, created_at FROM vector_native_index WHERE id = ?1",
            )
            .map_err(sql_err)?;

        let mut rows = stmt.query(params![id]).map_err(sql_err)?;
        if let Some(row) = rows.next().map_err(sql_err)? {
            let id_str: String = row.get(0).map_err(sql_err)?;
            let ns_str: String = row.get(1).map_err(sql_err)?;
            let dims: usize = row.get(2).map_err(sql_err)?;
            let blob: Vec<u8> = row.get(3).map_err(sql_err)?;
            let meta: Option<String> = row.get(4).map_err(sql_err)?;
            let created: i64 = row.get(5).map_err(sql_err)?;
            Ok(Some(VectorRecord {
                id: id_str,
                namespace: ns_str,
                dimensions: dims,
                vector_blob: blob,
                metadata_json: meta,
                created_at: created,
            }))
        } else {
            Ok(None)
        }
    }

    /// Deletes a vector record from SQLite and the HNSW graph cache
    pub fn delete_vector(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute("DELETE FROM vector_native_index WHERE id = ?1", params![id])
            .map_err(sql_err)?;
        let mut hnsw = self.hnsw.write().unwrap();
        hnsw.remove(id);
        Ok(rows > 0)
    }

    /// Total number of stored vectors
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM vector_native_index",
                [],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        Ok(count)
    }

    /// Total number of stored vectors within a specific namespace
    pub fn count_in_namespace(&self, namespace: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM vector_native_index WHERE namespace = ?1",
                params![namespace],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        Ok(count)
    }

    /// Returns telemetry and status statistics
    pub fn stats(&self) -> Result<VectorStoreStats> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT namespace, COUNT(*) FROM vector_native_index GROUP BY namespace",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                let ns: String = row.get(0)?;
                let c: usize = row.get(1)?;
                Ok((ns, c))
            })
            .map_err(sql_err)?;

        let mut namespaces = HashMap::new();
        let mut total = 0;
        for r in rows {
            let (ns, count) = r.map_err(sql_err)?;
            total += count;
            namespaces.insert(ns, count);
        }

        let hnsw = self.hnsw.read().unwrap();
        Ok(VectorStoreStats {
            total_vectors: total,
            namespaces,
            dimensions: hnsw.dimensions,
            max_hnsw_level: hnsw.max_level,
            db_path: self.db_path.to_string_lossy().to_string(),
        })
    }

    /// Returns the HNSW index configuration
    #[inline]
    pub fn config(&self) -> &VectorIndexConfig {
        &self.config
    }

    /// Returns the SQLite database file path
    #[inline]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_vector_blob_serialization_roundtrip() {
        let original = vec![1.234f32, -5.678f32, 0.0f32, 42.42f32];
        let blob = encode_vector_to_blob(&original);
        assert_eq!(blob.len(), original.len() * 4);

        let decoded = decode_blob_to_vector(&blob).unwrap();
        assert_eq!(decoded.len(), original.len());
        for (a, b) in original.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }

        // Test reusable decode into existing Vec
        let mut reusable = Vec::new();
        decode_blob_into(&blob, &mut reusable).unwrap();
        assert_eq!(reusable, decoded);

        // Test zero-copy slice borrow
        let slice = as_f32_slice(&blob).unwrap();
        assert_eq!(slice, original.as_slice());

        // Test error on truncated blob
        let truncated = &blob[..blob.len() - 1];
        assert!(decode_blob_to_vector(truncated).is_err());
    }

    #[test]
    fn test_simd_dot_product_and_cosine() {
        let a = vec![1.0f32, 2.0f32, 3.0f32, 4.0f32, 5.0f32, 6.0f32, 7.0f32, 8.0f32, 9.0f32];
        let b = vec![9.0f32, 8.0f32, 7.0f32, 6.0f32, 5.0f32, 4.0f32, 3.0f32, 2.0f32, 1.0f32];

        let expected_dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let computed_dot = dot_product(&a, &b);
        assert!((expected_dot - computed_dot).abs() < 1e-5);

        // Identical vector cosine similarity must be 1.0
        let sim_self = cosine_similarity(&a, &a);
        assert!((sim_self - 1.0).abs() < 1e-5);
        assert!(cosine_distance(&a, &a).abs() < 1e-5);

        // Orthogonal vectors
        let o1 = vec![1.0, 0.0, 0.0];
        let o2 = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&o1, &o2).abs() < 1e-5);
        assert!((cosine_distance(&o1, &o2) - 1.0).abs() < 1e-5);

        // Opposite vectors
        let op1 = vec![1.0, 2.0];
        let op2 = vec![-1.0, -2.0];
        assert!((cosine_similarity(&op1, &op2) - (-1.0)).abs() < 1e-5);
        assert!((cosine_distance(&op1, &op2) - 2.0).abs() < 1e-5);
    }

    #[test]
    fn test_vector_insertion_and_sqlite_wal_persistence() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join(format!("test_vss_{}.db", std::process::id()));
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }

        // 1. Create and populate store
        {
            let store = VectorNativeSqliteStore::open(&db_path).unwrap();

            // Verify SQLite WAL journal mode
            let conn = store.conn.lock().unwrap();
            let journal_mode: String = conn
                .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
                .unwrap();
            assert_eq!(journal_mode.to_lowercase(), "wal");
            drop(conn);

            store
                .insert_vector("doc1", "docs", &[1.0, 0.0, 0.0], Some("{\"title\":\"intro\"}"))
                .unwrap();
            store
                .insert_vector("doc2", "docs", &[0.0, 1.0, 0.0], Some("{\"title\":\"setup\"}"))
                .unwrap();
            store
                .insert_vector("code1", "code", &[0.0, 0.0, 1.0], Some("{\"fn\":\"main\"}"))
                .unwrap();

            assert_eq!(store.count().unwrap(), 3);
            assert_eq!(store.count_in_namespace("docs").unwrap(), 2);
            assert_eq!(store.count_in_namespace("code").unwrap(), 1);
        }

        // 2. Re-open store from existing SQLite file and verify persistence + HNSW reconstruct
        {
            let store = VectorNativeSqliteStore::open(&db_path).unwrap();
            assert_eq!(store.count().unwrap(), 3);

            let doc1 = store.get_vector("doc1").unwrap().expect("doc1 must exist");
            assert_eq!(doc1.namespace, "docs");
            assert_eq!(doc1.dimensions, 3);
            assert_eq!(doc1.vector().unwrap(), vec![1.0, 0.0, 0.0]);
            assert_eq!(doc1.metadata_json.as_deref(), Some("{\"title\":\"intro\"}"));

            // Perform similarity query on reconstructed in-memory HNSW cache
            let results = store.search_similar(&[0.9, 0.1, 0.0], 1).unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, "doc1");
            assert!(results[0].score > 0.95);
        }

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_hnsw_graph_construction_and_similarity_search() {
        let store = VectorNativeSqliteStore::open_in_memory().unwrap();

        // Populate with synthetic vector embeddings across 3 distinct semantic clusters
        let mut records = Vec::new();
        for i in 0..100 {
            let vec: Vec<f32> = if i < 33 {
                // Cluster A (pointing along positive X axis)
                vec![1.0, (i as f32) * 0.005, 0.0, 0.0]
            } else if i < 66 {
                // Cluster B (pointing along positive Y axis)
                vec![0.0, 1.0, ((i - 33) as f32) * 0.005, 0.0]
            } else {
                // Cluster C (pointing along positive Z axis)
                vec![0.0, 0.0, 1.0, ((i - 66) as f32) * 0.005]
            };
            records.push(VectorRecord::new(
                format!("vec_{}", i),
                "clusters",
                &vec,
                Some(format!("{{\"cluster\":{}}}", i / 33)),
            ));
        }

        store.insert_vector_batch(&records).unwrap();
        assert_eq!(store.count().unwrap(), 100);

        // Query near Cluster A
        let query_a = vec![1.0, 0.0, 0.0, 0.0];
        let start = Instant::now();
        let top_a = store.search_similar(&query_a, 5).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(top_a.len(), 5);
        assert_eq!(top_a[0].id, "vec_0");
        assert!(top_a[0].score > 0.99);

        // Sub-millisecond requirement check (< 1000 microseconds)
        assert!(
            elapsed < std::time::Duration::from_millis(1),
            "Search latency was {:?}",
            elapsed
        );
    }

    #[test]
    fn test_batch_atomic_insert_and_namespace_filtering() {
        let store = VectorNativeSqliteStore::open_in_memory().unwrap();

        let mut batch = Vec::new();
        for i in 0..20 {
            let ns = if i % 2 == 0 { "alpha" } else { "beta" };
            batch.push(VectorRecord::new(
                format!("item_{}", i),
                ns,
                &[i as f32, (i * 2) as f32, 1.0],
                None,
            ));
        }

        store.insert_vector_batch(&batch).unwrap();

        // Search specifically in "alpha"
        let alpha_results = store
            .search_similar_in_namespace("alpha", &[10.0, 20.0, 1.0], 5)
            .unwrap();
        assert_eq!(alpha_results.len(), 5);
        for res in &alpha_results {
            assert_eq!(res.namespace, "alpha");
        }

        // Search specifically in "beta"
        let beta_results = store
            .search_similar_in_namespace("beta", &[11.0, 22.0, 1.0], 5)
            .unwrap();
        assert_eq!(beta_results.len(), 5);
        for res in &beta_results {
            assert_eq!(res.namespace, "beta");
        }
    }
}
