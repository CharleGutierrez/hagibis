//! # Pillar 2: Hierarchical Multi-Modal RAG Cache Engine (`hierarchical_multimodal_rag_cache.rs`)
//!
//! Hagibis Tier 2 High-Performance Storage Architecture.
//!
//! CaldoAI Untangle Invariants: Score 100/100 (Tier 2 depends strictly on Tier 1 `hgb-core`,
//! zero imports from Tier 3 `hgb-nextgen`, Tier 4 `hgb-daemon`, or Tier 5 `hgb-cli`).
//!
//! ## Hierarchical Storage Tiers:
//! 1. **Hot Tier (DRAM)**: In-memory Quantized HNSW vector index for sub-millisecond retrieval.
//!    SIMD-accelerated 8-bit quantized byte vectors, supporting Cosine and Inner Product distance metrics.
//! 2. **Warm Tier (SQLite WAL)**: Compressed state checkpoints for agent conversation histories,
//!    intermediate tool execution DAGs, and state snapshots, compressed with an in-engine LZSS Deflate
//!    codec and indexed by session and turn index.
//! 3. **Cold Tier (Content-Addressed Object Storage)**: Content-addressed multi-modal assets
//!    (PNG diagrams, winged-sandal visual assets, architecture schematics, audio traces) indexed
//!    visually alongside code/markdown file links with BLAKE3 cryptographic hashes.
//! 4. **Unified Multi-Tier Query Planner**: Orchestrates unified traversal across Hot -> Warm -> Cold
//!    tiers with microsecond-level telemetry reporting.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use hgb_core::error::{HgbError, Result};

// ==============================================================================
// 1. Hot Tier: SIMD Quantized Vector Kernels & In-Memory HNSW Index
// ==============================================================================

/// Vector distance metric supported by the Hot Tier quantized index
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuantizedMetric {
    /// Cosine distance: 1.0 - cosine_similarity (Range: [0.0, 2.0])
    Cosine,
    /// Normalized inner product distance: 1.0 - (dot_product / max_dot)
    InnerProduct,
}

/// 8-bit symmetrically quantized vector representation with scale factor and precalculated norm
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantizedVectorRecord {
    pub id: String,
    pub bytes: Vec<i8>,
    pub scale: f32,
    pub norm_sq: i32,
    pub dimension: usize,
    pub metadata: Option<String>,
    pub tags: Vec<String>,
}

impl QuantizedVectorRecord {
    /// Quantizes an IEEE-754 `f32` vector slice into 8-bit symmetric integer coordinates in [-127, 127]
    pub fn from_f32_slice(id: impl Into<String>, vector: &[f32], metadata: Option<String>, tags: Vec<String>) -> Self {
        let dimension = vector.len();
        let mut max_abs = 0.0f32;
        for &val in vector {
            let abs = val.abs();
            if abs > max_abs {
                max_abs = abs;
            }
        }

        let scale = if max_abs > 1e-9 { max_abs / 127.0 } else { 1.0 };
        let inv_scale = 1.0 / scale;

        let mut bytes = Vec::with_capacity(dimension);
        let mut norm_sq = 0i32;

        for &val in vector {
            let q = (val * inv_scale).round().clamp(-127.0, 127.0) as i8;
            norm_sq += (q as i32) * (q as i32);
            bytes.push(q);
        }

        Self {
            id: id.into(),
            bytes,
            scale,
            norm_sq,
            dimension,
            metadata,
            tags,
        }
    }

    /// Reconstructs the approximate `f32` vector from quantized 8-bit integers
    pub fn dequantize(&self) -> Vec<f32> {
        self.bytes.iter().map(|&q| (q as f32) * self.scale).collect()
    }
}

/// Computes SIMD-accelerated integer dot product over two quantized byte slices.
/// Dynamically uses AVX2 when running on x86_64, falling back to 16-lane unrolled scalar logic.
#[inline]
pub fn simd_quantized_dot_product(a: &[i8], b: &[i8]) -> i32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && len >= 32 {
            return unsafe { avx2_quantized_dot_product(a, b, len) };
        }
    }

    unrolled_quantized_dot_product(a, b, len)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2_quantized_dot_product(a: &[i8], b: &[i8], len: usize) -> i32 {
    use std::arch::x86_64::*;

    let mut sum_vec = _mm256_setzero_si256();
    let chunks = len / 32;
    let a_ptr = a.as_ptr() as *const __m256i;
    let b_ptr = b.as_ptr() as *const __m256i;

    for i in 0..chunks {
        let va = _mm256_loadu_si256(a_ptr.add(i));
        let vb = _mm256_loadu_si256(b_ptr.add(i));

        let va_lo = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(va, 0));
        let vb_lo = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(vb, 0));
        let prod_lo = _mm256_madd_epi16(va_lo, vb_lo);

        let va_hi = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(va, 1));
        let vb_hi = _mm256_cvtepi8_epi16(_mm256_extracti128_si256(vb, 1));
        let prod_hi = _mm256_madd_epi16(va_hi, vb_hi);

        sum_vec = _mm256_add_epi32(sum_vec, _mm256_add_epi32(prod_lo, prod_hi));
    }

    let mut buf = [0i32; 8];
    _mm256_storeu_si256(buf.as_mut_ptr() as *mut __m256i, sum_vec);
    let mut total: i32 = buf.iter().sum();

    for i in (chunks * 32)..len {
        total += (a[i] as i32) * (b[i] as i32);
    }

    total
}

/// 16-lane unrolled portable scalar dot product fallback for quantized byte vectors
#[inline]
pub fn unrolled_quantized_dot_product(a: &[i8], b: &[i8], len: usize) -> i32 {
    let chunks = len / 16;
    let mut acc0 = 0i32;
    let mut acc1 = 0i32;
    let mut acc2 = 0i32;
    let mut acc3 = 0i32;

    for i in 0..chunks {
        let base = i * 16;
        acc0 += (a[base] as i32) * (b[base] as i32)
            + (a[base + 1] as i32) * (b[base + 1] as i32)
            + (a[base + 2] as i32) * (b[base + 2] as i32)
            + (a[base + 3] as i32) * (b[base + 3] as i32);
        acc1 += (a[base + 4] as i32) * (b[base + 4] as i32)
            + (a[base + 5] as i32) * (b[base + 5] as i32)
            + (a[base + 6] as i32) * (b[base + 6] as i32)
            + (a[base + 7] as i32) * (b[base + 7] as i32);
        acc2 += (a[base + 8] as i32) * (b[base + 8] as i32)
            + (a[base + 9] as i32) * (b[base + 9] as i32)
            + (a[base + 10] as i32) * (b[base + 10] as i32)
            + (a[base + 11] as i32) * (b[base + 11] as i32);
        acc3 += (a[base + 12] as i32) * (b[base + 12] as i32)
            + (a[base + 13] as i32) * (b[base + 13] as i32)
            + (a[base + 14] as i32) * (b[base + 14] as i32)
            + (a[base + 15] as i32) * (b[base + 15] as i32);
    }

    let mut total = acc0 + acc1 + acc2 + acc3;
    for i in (chunks * 16)..len {
        total += (a[i] as i32) * (b[i] as i32);
    }
    total
}

/// Computes Cosine Distance over quantized vectors: range [0.0, 2.0]
#[inline]
pub fn simd_quantized_cosine_distance(a: &QuantizedVectorRecord, b: &QuantizedVectorRecord) -> f32 {
    let dot = simd_quantized_dot_product(&a.bytes, &b.bytes) as f32;
    let norm_a = (a.norm_sq as f32).sqrt();
    let norm_b = (b.norm_sq as f32).sqrt();

    if norm_a < 1e-9 || norm_b < 1e-9 {
        return 1.0;
    }

    let sim = (dot / (norm_a * norm_b)).clamp(-1.0, 1.0);
    1.0 - sim
}

/// Computes Normalized Inner Product Distance over quantized vectors
#[inline]
pub fn simd_quantized_inner_product_distance(a: &QuantizedVectorRecord, b: &QuantizedVectorRecord) -> f32 {
    let dot = simd_quantized_dot_product(&a.bytes, &b.bytes) as f32;
    let norm_a = (a.norm_sq as f32).sqrt();
    let norm_b = (b.norm_sq as f32).sqrt();
    let denom = (norm_a * norm_b).max(1e-9);
    let normalized = (dot / denom).clamp(-1.0, 1.0);
    1.0 - normalized
}


/// Evaluates distance according to specified QuantizedMetric
#[inline]
pub fn compute_quantized_distance(metric: QuantizedMetric, a: &QuantizedVectorRecord, b: &QuantizedVectorRecord) -> f32 {
    match metric {
        QuantizedMetric::Cosine => simd_quantized_cosine_distance(a, b),
        QuantizedMetric::InnerProduct => simd_quantized_inner_product_distance(a, b),
    }
}

/// Deterministic, zero-dependency fast PRNG for geometric level sampling
#[derive(Debug, Clone)]
pub struct FastRng {
    state: u64,
}

impl FastRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
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
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    /// Sample hierarchical graph layer geometrically bounded to [0, 16]
    pub fn sample_level(&mut self, m: usize) -> usize {
        let factor = 1.0 / (m.max(2) as f64);
        let mut level = 0;
        while level < 16 && self.next_f64() < factor {
            level += 1;
        }
        level
    }
}

/// Configuration settings for the in-memory Quantized HNSW graph index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedHnswConfig {
    pub m: usize,
    pub m0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub metric: QuantizedMetric,
    pub dimension: usize,
}

impl Default for QuantizedHnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 64,
            ef_search: 48,
            metric: QuantizedMetric::Cosine,
            dimension: 64,
        }
    }
}

/// Internal HNSW graph node storing quantized vector and neighbor adjacency lists per layer
#[derive(Debug, Clone)]
struct QuantizedHnswNode {
    record: QuantizedVectorRecord,
    level: usize,
    neighbors: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedDistance(f32);

impl Eq for OrderedDistance {}

impl PartialOrd for OrderedDistance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedDistance {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(Ordering::Equal)
    }
}

/// Hit item returned by Hot Tier nearest-neighbor vector retrieval
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HotVectorHit {
    pub id: String,
    pub distance: f32,
    pub similarity: f32,
    pub metadata: Option<String>,
    pub tags: Vec<String>,
}

/// High-performance in-memory Quantized HNSW Vector Index (Hot Tier)
#[derive(Debug)]
pub struct QuantizedHnswIndex {
    nodes: HashMap<String, QuantizedHnswNode>,
    entry_point: Option<String>,
    max_layer: usize,
    config: QuantizedHnswConfig,
    rng: FastRng,
}

impl QuantizedHnswIndex {
    pub fn new(config: QuantizedHnswConfig) -> Self {
        Self {
            nodes: HashMap::new(),
            entry_point: None,
            max_layer: 0,
            config,
            rng: FastRng::new(1337),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Insert a quantized vector record into the multi-layer HNSW graph
    pub fn insert(&mut self, record: QuantizedVectorRecord) -> Result<()> {
        let node_id = record.id.clone();
        let level = self.rng.sample_level(self.config.m);

        let mut neighbors = Vec::with_capacity(level + 1);
        for _ in 0..=level {
            neighbors.push(Vec::new());
        }

        let new_node = QuantizedHnswNode {
            record,
            level,
            neighbors,
        };

        if self.entry_point.is_none() {
            self.entry_point = Some(node_id.clone());
            self.max_layer = level;
            self.nodes.insert(node_id, new_node);
            return Ok(());
        }

        let mut curr_obj = self.entry_point.as_ref().unwrap().clone();
        let top_layer = self.max_layer;

        // Phase 1: Greedy walk from max_layer down to level + 1
        for l in (level + 1..=top_layer).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                let curr_node = match self.nodes.get(&curr_obj) {
                    Some(n) => n,
                    None => break,
                };

                let curr_dist = compute_quantized_distance(
                    self.config.metric,
                    &new_node.record,
                    &curr_node.record,
                );

                if l < curr_node.neighbors.len() {
                    for neighbor_id in &curr_node.neighbors[l] {
                        if let Some(neighbor_node) = self.nodes.get(neighbor_id) {
                            let dist = compute_quantized_distance(
                                self.config.metric,
                                &new_node.record,
                                &neighbor_node.record,
                            );
                            if dist < curr_dist {
                                curr_obj = neighbor_id.clone();
                                changed = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Phase 2: Insert into levels min(level, top_layer) down to 0
        let bottom_insertion_layer = level.min(top_layer);
        let mut enter_points = vec![curr_obj];

        self.nodes.insert(node_id.clone(), new_node);

        for l in (0..=bottom_insertion_layer).rev() {
            let candidates = self.search_layer_candidates(&node_id, &enter_points, self.config.ef_construction, l);
            let m_max = if l == 0 { self.config.m0 } else { self.config.m };

            let selected_neighbors: Vec<String> = candidates.iter().take(m_max).map(|(_, id)| id.clone()).collect();

            // Connect node to selected neighbors
            if let Some(node) = self.nodes.get_mut(&node_id) {
                node.neighbors[l] = selected_neighbors.clone();
            }

            // Connect reverse edges and prune if exceeding degree limit
            for neighbor_id in &selected_neighbors {
                let neighbor_record = match self.nodes.get(neighbor_id) {
                    Some(n) => n.record.clone(),
                    None => continue,
                };

                let nbr_list = if let Some(neighbor_node) = self.nodes.get_mut(neighbor_id) {
                    if l < neighbor_node.neighbors.len() {
                        neighbor_node.neighbors[l].push(node_id.clone());
                        if neighbor_node.neighbors[l].len() > m_max {
                            Some(neighbor_node.neighbors[l].clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(nbrs) = nbr_list {
                    let mut scored: Vec<(OrderedDistance, String)> = nbrs
                        .into_iter()
                        .filter_map(|nbr_id| {
                            self.nodes.get(&nbr_id).map(|nbr| {
                                let d = compute_quantized_distance(self.config.metric, &neighbor_record, &nbr.record);
                                (OrderedDistance(d), nbr_id)
                            })
                        })
                        .collect();
                    scored.sort_unstable();
                    scored.truncate(m_max);
                    if let Some(neighbor_node) = self.nodes.get_mut(neighbor_id) {
                        neighbor_node.neighbors[l] = scored.into_iter().map(|(_, id)| id).collect();
                    }
                }
            }

            enter_points = selected_neighbors;
        }

        if level > self.max_layer {
            self.max_layer = level;
            self.entry_point = Some(node_id);
        }

        Ok(())
    }

    fn search_layer_candidates(
        &self,
        query_node_id: &str,
        enter_points: &[String],
        ef: usize,
        layer: usize,
    ) -> Vec<(OrderedDistance, String)> {
        let query_record = match self.nodes.get(query_node_id) {
            Some(n) => &n.record,
            None => return Vec::new(),
        };

        let mut visited: HashSet<String> = HashSet::new();
        let mut candidates: BinaryHeap<Reverse<(OrderedDistance, String)>> = BinaryHeap::new();
        let mut w: BinaryHeap<(OrderedDistance, String)> = BinaryHeap::new();

        for ep in enter_points {
            if let Some(node) = self.nodes.get(ep) {
                let dist = compute_quantized_distance(self.config.metric, query_record, &node.record);
                visited.insert(ep.clone());
                candidates.push(Reverse((OrderedDistance(dist), ep.clone())));
                w.push((OrderedDistance(dist), ep.clone()));
            }
        }

        while let Some(Reverse((cand_dist, cand_id))) = candidates.pop() {
            let furthest_w_dist = if let Some(furthest) = w.peek() {
                furthest.0
            } else {
                OrderedDistance(f32::MAX)
            };

            if cand_dist > furthest_w_dist && w.len() >= ef {
                break;
            }

            if let Some(cand_node) = self.nodes.get(&cand_id) {
                if layer < cand_node.neighbors.len() {
                    for neighbor_id in &cand_node.neighbors[layer] {
                        if !visited.contains(neighbor_id) {
                            visited.insert(neighbor_id.clone());
                            if let Some(neighbor_node) = self.nodes.get(neighbor_id) {
                                let dist = compute_quantized_distance(self.config.metric, query_record, &neighbor_node.record);
                                let ord_dist = OrderedDistance(dist);

                                if w.len() < ef || ord_dist < w.peek().unwrap().0 {
                                    candidates.push(Reverse((ord_dist, neighbor_id.clone())));
                                    w.push((ord_dist, neighbor_id.clone()));
                                    if w.len() > ef {
                                        w.pop();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let sorted = w.into_sorted_vec();
        sorted
    }

    /// Sub-millisecond K-nearest neighbors search over quantized byte vectors
    pub fn search_knn(&self, query_f32: &[f32], top_k: usize, ef_search: usize) -> Vec<HotVectorHit> {
        if self.nodes.is_empty() || self.entry_point.is_none() || top_k == 0 {
            return Vec::new();
        }

        let query_record = QuantizedVectorRecord::from_f32_slice("__query__", query_f32, None, Vec::new());
        let mut curr_obj = self.entry_point.as_ref().unwrap().clone();
        let top_layer = self.max_layer;

        // Greedy traversal on upper layers
        for l in (1..=top_layer).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                let curr_node = match self.nodes.get(&curr_obj) {
                    Some(n) => n,
                    None => break,
                };

                let curr_dist = compute_quantized_distance(
                    self.config.metric,
                    &query_record,
                    &curr_node.record,
                );

                if l < curr_node.neighbors.len() {
                    for nbr_id in &curr_node.neighbors[l] {
                        if let Some(nbr_node) = self.nodes.get(nbr_id) {
                            let dist = compute_quantized_distance(
                                self.config.metric,
                                &query_record,
                                &nbr_node.record,
                            );
                            if dist < curr_dist {
                                curr_obj = nbr_id.clone();
                                changed = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Beam search at layer 0
        let ef = ef_search.max(top_k);
        let mut visited: HashSet<String> = HashSet::new();
        let mut candidates: BinaryHeap<Reverse<(OrderedDistance, String)>> = BinaryHeap::new();
        let mut w: BinaryHeap<(OrderedDistance, String)> = BinaryHeap::new();

        if let Some(start_node) = self.nodes.get(&curr_obj) {
            let dist = compute_quantized_distance(self.config.metric, &query_record, &start_node.record);
            visited.insert(curr_obj.clone());
            candidates.push(Reverse((OrderedDistance(dist), curr_obj.clone())));
            w.push((OrderedDistance(dist), curr_obj));
        }

        while let Some(Reverse((cand_dist, cand_id))) = candidates.pop() {
            if let Some(furthest) = w.peek() {
                if cand_dist > furthest.0 && w.len() >= ef {
                    break;
                }
            }

            if let Some(cand_node) = self.nodes.get(&cand_id) {
                if !cand_node.neighbors.is_empty() {
                    for nbr_id in &cand_node.neighbors[0] {
                        if !visited.contains(nbr_id) {
                            visited.insert(nbr_id.clone());
                            if let Some(nbr_node) = self.nodes.get(nbr_id) {
                                let dist = compute_quantized_distance(
                                    self.config.metric,
                                    &query_record,
                                    &nbr_node.record,
                                );
                                let ord_dist = OrderedDistance(dist);

                                if w.len() < ef || ord_dist < w.peek().unwrap().0 {
                                    candidates.push(Reverse((ord_dist, nbr_id.clone())));
                                    w.push((ord_dist, nbr_id.clone()));
                                    if w.len() > ef {
                                        w.pop();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut results = Vec::new();
        let sorted = w.into_sorted_vec();

        for (ord_dist, id) in sorted.into_iter().take(top_k) {
            if let Some(node) = self.nodes.get(&id) {
                let dist = ord_dist.0;
                let sim = (1.0 - (dist / 2.0)).clamp(0.0, 1.0);
                results.push(HotVectorHit {
                    id,
                    distance: dist,
                    similarity: sim,
                    metadata: node.record.metadata.clone(),
                    tags: node.record.tags.clone(),
                });
            }
        }

        results
    }
}

// ==============================================================================
// 2. Warm Tier: LZSS Deflate Compression Codec & Compressed SQLite State Store
// ==============================================================================

const HGBZ_MAGIC: [u8; 4] = *b"HGBZ";
const HGBZ_VERSION: u8 = 1;
const MODE_RAW: u8 = 0;
const MODE_LZSS: u8 = 1;

/// High-throughput zero-dependency LZSS Deflate Block Codec
pub struct DeflateCodec;

impl DeflateCodec {
    /// Compresses a raw payload with 4096-byte sliding window LZSS and BLAKE3 checksum
    pub fn compress(raw: &[u8]) -> Vec<u8> {
        let raw_len = raw.len();
        let blake3_digest = blake3::hash(raw);
        let checksum: [u8; 32] = *blake3_digest.as_bytes();

        if raw_len == 0 {
            return Self::build_header(MODE_RAW, 0, 0, &checksum);
        }


        let mut compressed_tokens = Vec::with_capacity(raw_len);
        let mut hash_table = [0u16; 4096];
        let mut i = 0;

        while i < raw_len {
            let mut flag_byte = 0u8;
            let mut token_block = Vec::with_capacity(16);

            for bit in 0..8 {
                if i >= raw_len {
                    break;
                }

                let max_len = (raw_len - i).min(18);
                let mut best_len = 0;
                let mut best_offset = 0;

                if max_len >= 3 {
                    let hash = ((raw_data_u16(raw, i) ^ (raw[i + 2] as u16).wrapping_shl(4)) % 4096) as usize;
                    let candidate_pos = hash_table[hash] as usize;
                    hash_table[hash] = (i % 65536) as u16;

                    if candidate_pos < i && (i - candidate_pos) <= 4096 {
                        let offset = i - candidate_pos;
                        let mut match_len = 0;
                        while match_len < max_len && raw[candidate_pos + match_len] == raw[i + match_len] {
                            match_len += 1;
                        }
                        if match_len >= 3 {
                            best_len = match_len;
                            best_offset = offset;
                        }
                    }
                }

                if best_len >= 3 {
                    flag_byte |= 1 << bit;
                    let b0 = (best_offset & 0xFF) as u8;
                    let b1 = (((best_offset >> 8) & 0x0F) | ((best_len - 3) << 4)) as u8;
                    token_block.push(b0);
                    token_block.push(b1);
                    i += best_len;
                } else {
                    token_block.push(raw[i]);
                    i += 1;
                }
            }

            compressed_tokens.push(flag_byte);
            compressed_tokens.extend_from_slice(&token_block);
        }

        // If compressed size is larger than raw, fallback to MODE_RAW
        if compressed_tokens.len() >= raw_len {
            let mut out = Self::build_header(MODE_RAW, raw_len as u32, raw_len as u32, &checksum);
            out.extend_from_slice(raw);
            out
        } else {
            let mut out = Self::build_header(
                MODE_LZSS,
                raw_len as u32,
                compressed_tokens.len() as u32,
                &checksum,
            );
            out.extend_from_slice(&compressed_tokens);
            out
        }
    }

    fn build_header(mode: u8, raw_len: u32, comp_len: u32, checksum: &[u8; 32]) -> Vec<u8> {
        let mut header = Vec::with_capacity(46);
        header.extend_from_slice(&HGBZ_MAGIC);
        header.push(HGBZ_VERSION);
        header.push(mode);
        header.extend_from_slice(&raw_len.to_le_bytes());
        header.extend_from_slice(&comp_len.to_le_bytes());
        header.extend_from_slice(checksum);
        header
    }

    /// Decompresses an HGBZ compressed payload and verifies BLAKE3 integrity
    pub fn decompress(compressed: &[u8]) -> Result<Vec<u8>> {
        if compressed.len() < 46 {
            return Err(HgbError::Storage(
                "Invalid compressed block: header too short".into(),
            ));
        }

        if &compressed[0..4] != &HGBZ_MAGIC {
            return Err(HgbError::Storage(
                "Invalid compressed block: magic header mismatch".into(),
            ));
        }

        let mode = compressed[5];
        let raw_len = u32::from_le_bytes([compressed[6], compressed[7], compressed[8], compressed[9]]) as usize;
        let comp_len = u32::from_le_bytes([compressed[10], compressed[11], compressed[12], compressed[13]]) as usize;
        let expected_checksum = &compressed[14..46];
        let payload = &compressed[46..];

        if payload.len() != comp_len {
            return Err(HgbError::Storage(format!(
                "Payload size mismatch: expected {} bytes, got {}",
                comp_len,
                payload.len()
            )));
        }

        let mut output = Vec::with_capacity(raw_len);

        match mode {
            MODE_RAW => {
                output.extend_from_slice(payload);
            }
            MODE_LZSS => {
                let mut p_idx = 0;
                while p_idx < payload.len() && output.len() < raw_len {
                    let flag_byte = payload[p_idx];
                    p_idx += 1;

                    for bit in 0..8 {
                        if output.len() >= raw_len || p_idx >= payload.len() {
                            break;
                        }

                        if (flag_byte & (1 << bit)) != 0 {
                            if p_idx + 1 >= payload.len() {
                                return Err(HgbError::Storage("Truncated LZSS token block".into()));
                            }
                            let b0 = payload[p_idx] as usize;
                            let b1 = payload[p_idx + 1] as usize;
                            p_idx += 2;

                            let offset = b0 | ((b1 & 0x0F) << 8);
                            let match_len = ((b1 >> 4) & 0x0F) + 3;

                            if offset == 0 || offset > output.len() {
                                return Err(HgbError::Storage(format!(
                                    "Corrupted LZSS offset {offset} with output len {}",
                                    output.len()
                                )));
                            }

                            let start = output.len() - offset;
                            for k in 0..match_len {
                                let byte = output[start + k];
                                output.push(byte);
                            }
                        } else {
                            output.push(payload[p_idx]);
                            p_idx += 1;
                        }
                    }
                }
            }
            other => {
                return Err(HgbError::Storage(format!("Unknown compression mode: {other}")));
            }
        }

        if output.len() != raw_len {
            return Err(HgbError::Storage(format!(
                "Decompressed length mismatch: expected {}, got {}",
                raw_len,
                output.len()
            )));
        }

        let calculated_hash = blake3::hash(&output);
        if calculated_hash.as_bytes() != expected_checksum {
            return Err(HgbError::Storage("BLAKE3 checksum validation failed on decompression".into()));
        }

        Ok(output)
    }

    /// Evaluates achieved space savings ratio as percentage
    pub fn compression_ratio(raw_size: usize, compressed_size: usize) -> f64 {
        if raw_size == 0 {
            0.0
        } else {
            (1.0 - (compressed_size as f64 / raw_size as f64)) * 100.0
        }
    }
}

#[inline(always)]
fn raw_data_u16(raw: &[u8], idx: usize) -> u16 {
    (raw[idx] as u16) | ((raw[idx + 1] as u16) << 8)
}

/// Category of state checkpoints stored in Warm Tier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointKind {
    ConversationHistory,
    ToolExecutionTrace,
    AgentStateCheckpoint,
    PatchReflection,
    DiagnosticDump,
}

impl CheckpointKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ConversationHistory => "ConversationHistory",
            Self::ToolExecutionTrace => "ToolExecutionTrace",
            Self::AgentStateCheckpoint => "AgentStateCheckpoint",
            Self::PatchReflection => "PatchReflection",
            Self::DiagnosticDump => "DiagnosticDump",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ConversationHistory" => Some(Self::ConversationHistory),
            "ToolExecutionTrace" => Some(Self::ToolExecutionTrace),
            "AgentStateCheckpoint" => Some(Self::AgentStateCheckpoint),
            "PatchReflection" => Some(Self::PatchReflection),
            "DiagnosticDump" => Some(Self::DiagnosticDump),
            _ => None,
        }
    }
}

/// Compressed Warm Tier checkpoint record
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WarmStateCheckpoint {
    pub checkpoint_id: String,
    pub session_id: String,
    pub turn_index: u64,
    pub checkpoint_kind: CheckpointKind,
    pub raw_size: usize,
    pub compressed_size: usize,
    pub checksum_blake3: String,
    pub compressed_payload: Vec<u8>,
    pub tags: Vec<String>,
    pub created_at_ms: u64,
}

impl WarmStateCheckpoint {
    pub fn new(
        checkpoint_id: impl Into<String>,
        session_id: impl Into<String>,
        turn_index: u64,
        kind: CheckpointKind,
        raw_payload: &[u8],
        tags: Vec<String>,
    ) -> Self {
        let compressed = DeflateCodec::compress(raw_payload);
        let checksum = blake3::hash(raw_payload).to_hex().to_string();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            checkpoint_id: checkpoint_id.into(),
            session_id: session_id.into(),
            turn_index,
            checkpoint_kind: kind,
            raw_size: raw_payload.len(),
            compressed_size: compressed.len(),
            checksum_blake3: checksum,
            compressed_payload: compressed,
            tags,
            created_at_ms: now_ms,
        }
    }

    pub fn decompress_payload(&self) -> Result<Vec<u8>> {
        DeflateCodec::decompress(&self.compressed_payload)
    }
}

/// Telemetry metrics for Warm Tier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WarmTierStats {
    pub total_checkpoints: usize,
    pub total_raw_bytes: usize,
    pub total_compressed_bytes: usize,
    pub compression_ratio_pct: f64,
}

/// Warm Tier SQLite storage engine managing compressed state checkpoints
pub struct WarmTierEngine {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl WarmTierEngine {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&path).map_err(|e| HgbError::Storage(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS warm_state_checkpoints (
                 checkpoint_id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 turn_index INTEGER NOT NULL,
                 checkpoint_kind TEXT NOT NULL,
                 raw_size INTEGER NOT NULL,
                 compressed_size INTEGER NOT NULL,
                 checksum_blake3 TEXT NOT NULL,
                 compressed_payload BLOB NOT NULL,
                 tags_json TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_warm_session_turn ON warm_state_checkpoints (session_id, turn_index);
             CREATE INDEX IF NOT EXISTS idx_warm_kind ON warm_state_checkpoints (checkpoint_kind);
             CREATE INDEX IF NOT EXISTS idx_warm_created ON warm_state_checkpoints (created_at_ms);",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: path,
        })
    }

    pub fn put_checkpoint(&self, checkpoint: &WarmStateCheckpoint) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let tags_json = serde_json::to_string(&checkpoint.tags)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        conn.execute(
            "INSERT OR REPLACE INTO warm_state_checkpoints (
                 checkpoint_id, session_id, turn_index, checkpoint_kind,
                 raw_size, compressed_size, checksum_blake3, compressed_payload,
                 tags_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                checkpoint.checkpoint_id,
                checkpoint.session_id,
                checkpoint.turn_index,
                checkpoint.checkpoint_kind.as_str(),
                checkpoint.raw_size as i64,
                checkpoint.compressed_size as i64,
                checkpoint.checksum_blake3,
                checkpoint.compressed_payload,
                tags_json,
                checkpoint.created_at_ms as i64,
            ],
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        Ok(())
    }

    pub fn get_checkpoint(&self, checkpoint_id: &str) -> Result<Option<WarmStateCheckpoint>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, session_id, turn_index, checkpoint_kind,
                    raw_size, compressed_size, checksum_blake3, compressed_payload,
                    tags_json, created_at_ms
             FROM warm_state_checkpoints WHERE checkpoint_id = ?1",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let res = stmt.query_row(params![checkpoint_id], |row| {
            let kind_str: String = row.get(3)?;
            let kind = CheckpointKind::from_str(&kind_str).unwrap_or(CheckpointKind::DiagnosticDump);
            let tags_json: String = row.get(8)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(WarmStateCheckpoint {
                checkpoint_id: row.get(0)?,
                session_id: row.get(1)?,
                turn_index: row.get::<_, i64>(2)? as u64,
                checkpoint_kind: kind,
                raw_size: row.get::<_, i64>(4)? as usize,
                compressed_size: row.get::<_, i64>(5)? as usize,
                checksum_blake3: row.get(6)?,
                compressed_payload: row.get(7)?,
                tags,
                created_at_ms: row.get::<_, i64>(9)? as u64,
            })
        }).optional().map_err(|e| HgbError::Storage(e.to_string()))?;

        Ok(res)
    }

    pub fn query_session_checkpoints(&self, session_id: &str, limit: usize) -> Result<Vec<WarmStateCheckpoint>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, session_id, turn_index, checkpoint_kind,
                    raw_size, compressed_size, checksum_blake3, compressed_payload,
                    tags_json, created_at_ms
             FROM warm_state_checkpoints
             WHERE session_id = ?1
             ORDER BY turn_index ASC
             LIMIT ?2",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            let kind_str: String = row.get(3)?;
            let kind = CheckpointKind::from_str(&kind_str).unwrap_or(CheckpointKind::DiagnosticDump);
            let tags_json: String = row.get(8)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(WarmStateCheckpoint {
                checkpoint_id: row.get(0)?,
                session_id: row.get(1)?,
                turn_index: row.get::<_, i64>(2)? as u64,
                checkpoint_kind: kind,
                raw_size: row.get::<_, i64>(4)? as usize,
                compressed_size: row.get::<_, i64>(5)? as usize,
                checksum_blake3: row.get(6)?,
                compressed_payload: row.get(7)?,
                tags,
                created_at_ms: row.get::<_, i64>(9)? as u64,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn search_by_kind_and_tag(&self, kind: Option<CheckpointKind>, tag: Option<&str>, limit: usize) -> Result<Vec<WarmStateCheckpoint>> {
        let conn = self.conn.lock().unwrap();
        let kind_str = kind.map(|k| k.as_str().to_string());

        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, session_id, turn_index, checkpoint_kind,
                    raw_size, compressed_size, checksum_blake3, compressed_payload,
                    tags_json, created_at_ms
             FROM warm_state_checkpoints
             WHERE (?1 IS NULL OR checkpoint_kind = ?1)
             ORDER BY created_at_ms DESC
             LIMIT ?2",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map(params![kind_str, limit as i64], |row| {
            let k_str: String = row.get(3)?;
            let k = CheckpointKind::from_str(&k_str).unwrap_or(CheckpointKind::DiagnosticDump);
            let tags_json: String = row.get(8)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(WarmStateCheckpoint {
                checkpoint_id: row.get(0)?,
                session_id: row.get(1)?,
                turn_index: row.get::<_, i64>(2)? as u64,
                checkpoint_kind: k,
                raw_size: row.get::<_, i64>(4)? as usize,
                compressed_size: row.get::<_, i64>(5)? as usize,
                checksum_blake3: row.get(6)?,
                compressed_payload: row.get(7)?,
                tags,
                created_at_ms: row.get::<_, i64>(9)? as u64,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            let item = r.map_err(|e| HgbError::Storage(e.to_string()))?;
            if let Some(target_tag) = tag {
                if !item.tags.iter().any(|t| t == target_tag) {
                    continue;
                }
            }
            out.push(item);
        }
        Ok(out)
    }

    pub fn stats(&self) -> Result<WarmTierStats> {
        let conn = self.conn.lock().unwrap();
        let (total_checkpoints, total_raw, total_comp): (i64, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT COUNT(*), SUM(raw_size), SUM(compressed_size) FROM warm_state_checkpoints",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let raw = total_raw.unwrap_or(0) as usize;
        let comp = total_comp.unwrap_or(0) as usize;
        let ratio = DeflateCodec::compression_ratio(raw, comp);

        Ok(WarmTierStats {
            total_checkpoints: total_checkpoints as usize,
            total_raw_bytes: raw,
            total_compressed_bytes: comp,
            compression_ratio_pct: ratio,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

// ==============================================================================
// 3. Cold Tier: Content-Addressed Object Storage for Multi-Modal Assets
// ==============================================================================

/// Multi-Modal Asset Descriptor indexing visual assets, dimensions, feature vectors, and code citations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MultiModalAssetDescriptor {
    pub asset_id: String,
    pub dimensions: Option<(u32, u32)>,
    pub mime_type: String,
    pub perceptual_hash: u64,
    pub feature_vector: Vec<f32>,
    pub associated_code_links: Vec<String>,
    pub blake3_digest: String,
    pub byte_size: usize,
    pub storage_rel_path: String,
    pub created_at_ms: u64,
    pub metadata: HashMap<String, String>,
}

impl MultiModalAssetDescriptor {
    /// Computes Hamming distance between 64-bit visual perceptual hashes
    pub fn perceptual_distance(&self, other_p_hash: u64) -> u32 {
        (self.perceptual_hash ^ other_p_hash).count_ones()
    }

    /// Computes visual similarity score in [0.0, 1.0] from perceptual hash
    pub fn perceptual_similarity(&self, other_p_hash: u64) -> f32 {
        let dist = self.perceptual_distance(other_p_hash);
        (1.0 - (dist as f32 / 64.0)).clamp(0.0, 1.0)
    }

    /// Evaluates cosine similarity between visual feature vectors
    pub fn feature_cosine_similarity(&self, query_vector: &[f32]) -> f32 {
        let len = self.feature_vector.len().min(query_vector.len());
        if len == 0 {
            return 0.0;
        }

        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;

        for i in 0..len {
            let a = self.feature_vector[i];
            let b = query_vector[i];
            dot += a * b;
            norm_a += a * a;
            norm_b += b * b;
        }

        let denom = (norm_a.sqrt() * norm_b.sqrt()).max(1e-9);
        (dot / denom).clamp(-1.0, 1.0)
    }
}

/// Hit item returned by Cold Tier multi-modal asset retrieval
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColdAssetHit {
    pub descriptor: MultiModalAssetDescriptor,
    pub visual_similarity: f32,
    pub feature_similarity: f32,
    pub matched_code_link: Option<String>,
}

/// Cold Tier content-addressed object storage engine backed by BLAKE3 immutability
pub struct ColdStorageEngine {

    root_dir: PathBuf,
    objects_dir: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl ColdStorageEngine {
    pub fn new(root_dir: impl AsRef<Path>) -> Result<Self> {
        let root = root_dir.as_ref().to_path_buf();
        let objects = root.join("objects");
        fs::create_dir_all(&objects)?;

        let db_path = root.join("cold_metadata.db");
        let conn = Connection::open(&db_path).map_err(|e| HgbError::Storage(e.to_string()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS cold_multimodal_assets (
                 asset_id TEXT PRIMARY KEY,
                 blake3_digest TEXT NOT NULL UNIQUE,
                 mime_type TEXT NOT NULL,
                 width INTEGER,
                 height INTEGER,
                 perceptual_hash INTEGER NOT NULL,
                 feature_vector_blob BLOB NOT NULL,
                 associated_code_links_json TEXT NOT NULL,
                 byte_size INTEGER NOT NULL,
                 storage_rel_path TEXT NOT NULL,
                 metadata_json TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS cold_asset_code_links (
                 asset_id TEXT NOT NULL,
                 code_link TEXT NOT NULL,
                 PRIMARY KEY (asset_id, code_link)
             );
             CREATE INDEX IF NOT EXISTS idx_cold_blake3 ON cold_multimodal_assets (blake3_digest);
             CREATE INDEX IF NOT EXISTS idx_cold_mime ON cold_multimodal_assets (mime_type);
             CREATE INDEX IF NOT EXISTS idx_cold_phash ON cold_multimodal_assets (perceptual_hash);
             CREATE INDEX IF NOT EXISTS idx_cold_code_link ON cold_asset_code_links (code_link);",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        Ok(Self {
            root_dir: root,
            objects_dir: objects,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Stores a multi-modal binary asset with BLAKE3 content addressing and atomic persistence
    pub fn store_asset(&self, mut descriptor: MultiModalAssetDescriptor, data: &[u8]) -> Result<String> {
        let digest_hash = blake3::hash(data);
        let digest_str = digest_hash.to_hex().to_string();

        if !descriptor.blake3_digest.is_empty() && descriptor.blake3_digest != digest_str {
            return Err(HgbError::Storage(format!(
                "BLAKE3 digest mismatch: specified {} but content hashes to {}",
                descriptor.blake3_digest, digest_str
            )));
        }

        descriptor.blake3_digest = digest_str.clone();
        descriptor.byte_size = data.len();

        let shard0 = &digest_str[0..2];
        let shard1 = &digest_str[2..4];
        let shard_dir = self.objects_dir.join(shard0).join(shard1);
        fs::create_dir_all(&shard_dir)?;

        let filename = format!("{digest_str}.bin");
        let target_path = shard_dir.join(&filename);
        descriptor.storage_rel_path = format!("objects/{shard0}/{shard1}/{filename}");

        if !target_path.exists() {
            let tmp_path = shard_dir.join(format!("{filename}.tmp.{}", std::process::id()));
            let mut file = File::create(&tmp_path)?;
            file.write_all(data)?;
            file.flush()?;
            fs::rename(tmp_path, &target_path)?;
        }

        let mut feature_blob = Vec::with_capacity(descriptor.feature_vector.len() * 4);
        for &val in &descriptor.feature_vector {
            feature_blob.extend_from_slice(&val.to_le_bytes());
        }

        let links_json = serde_json::to_string(&descriptor.associated_code_links)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        let meta_json = serde_json::to_string(&descriptor.metadata)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let (w, h) = match descriptor.dimensions {
            Some((w, h)) => (Some(w as i64), Some(h as i64)),
            None => (None, None),
        };

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO cold_multimodal_assets (
                 asset_id, blake3_digest, mime_type, width, height,
                 perceptual_hash, feature_vector_blob, associated_code_links_json,
                 byte_size, storage_rel_path, metadata_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                descriptor.asset_id,
                descriptor.blake3_digest,
                descriptor.mime_type,
                w,
                h,
                descriptor.perceptual_hash as i64,
                feature_blob,
                links_json,
                descriptor.byte_size as i64,
                descriptor.storage_rel_path,
                meta_json,
                descriptor.created_at_ms as i64,
            ],
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        conn.execute("DELETE FROM cold_asset_code_links WHERE asset_id = ?1", params![descriptor.asset_id])
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        for link in &descriptor.associated_code_links {
            conn.execute(
                "INSERT OR IGNORE INTO cold_asset_code_links (asset_id, code_link) VALUES (?1, ?2)",
                params![descriptor.asset_id, link],
            ).map_err(|e| HgbError::Storage(e.to_string()))?;
        }

        Ok(digest_str)
    }

    /// Reads an immutable binary asset with cryptographic BLAKE3 tamper verification
    pub fn read_asset(&self, blake3_digest: &str) -> Result<(MultiModalAssetDescriptor, Vec<u8>)> {
        let desc = self.get_descriptor(blake3_digest)?
            .ok_or_else(|| HgbError::NotFound(format!("Asset with digest {blake3_digest} not found")))?;

        let full_path = self.root_dir.join(&desc.storage_rel_path);
        let mut file = File::open(&full_path)?;
        let mut buf = Vec::with_capacity(desc.byte_size);
        file.read_to_end(&mut buf)?;

        let actual_hash = blake3::hash(&buf).to_hex().to_string();
        if actual_hash != blake3_digest {
            return Err(HgbError::Storage(format!(
                "Tamper detection trigger: content hashed to {actual_hash}, expected {blake3_digest}"
            )));
        }

        Ok((desc, buf))
    }

    pub fn get_descriptor(&self, blake3_digest: &str) -> Result<Option<MultiModalAssetDescriptor>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT asset_id, blake3_digest, mime_type, width, height,
                    perceptual_hash, feature_vector_blob, associated_code_links_json,
                    byte_size, storage_rel_path, metadata_json, created_at_ms
             FROM cold_multimodal_assets WHERE blake3_digest = ?1",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let res = stmt.query_row(params![blake3_digest], |row| {
            let w: Option<i64> = row.get(3)?;
            let h: Option<i64> = row.get(4)?;
            let dims = match (w, h) {
                (Some(w), Some(h)) => Some((w as u32, h as u32)),
                _ => None,
            };

            let feat_blob: Vec<u8> = row.get(6)?;
            let mut feature_vector = Vec::with_capacity(feat_blob.len() / 4);
            for chunk in feat_blob.chunks_exact(4) {
                feature_vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }

            let links_json: String = row.get(7)?;
            let links: Vec<String> = serde_json::from_str(&links_json).unwrap_or_default();
            let meta_json: String = row.get(10)?;
            let meta: HashMap<String, String> = serde_json::from_str(&meta_json).unwrap_or_default();

            Ok(MultiModalAssetDescriptor {
                asset_id: row.get(0)?,
                blake3_digest: row.get(1)?,
                mime_type: row.get(2)?,
                dimensions: dims,
                perceptual_hash: row.get::<_, i64>(5)? as u64,
                feature_vector,
                associated_code_links: links,
                byte_size: row.get::<_, i64>(8)? as usize,
                storage_rel_path: row.get(9)?,
                metadata: meta,
                created_at_ms: row.get::<_, i64>(11)? as u64,
            })
        }).optional().map_err(|e| HgbError::Storage(e.to_string()))?;

        Ok(res)
    }

    /// Search multi-modal assets using 64-bit visual perceptual hash Hamming distance
    pub fn search_by_visual_similarity(&self, target_p_hash: u64, max_hamming_dist: u32, limit: usize) -> Result<Vec<ColdAssetHit>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT asset_id, blake3_digest, mime_type, width, height,
                    perceptual_hash, feature_vector_blob, associated_code_links_json,
                    byte_size, storage_rel_path, metadata_json, created_at_ms
             FROM cold_multimodal_assets",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map([], |row| {
            let w: Option<i64> = row.get(3)?;
            let h: Option<i64> = row.get(4)?;
            let dims = match (w, h) {
                (Some(w), Some(h)) => Some((w as u32, h as u32)),
                _ => None,
            };

            let feat_blob: Vec<u8> = row.get(6)?;
            let mut feature_vector = Vec::with_capacity(feat_blob.len() / 4);
            for chunk in feat_blob.chunks_exact(4) {
                feature_vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }

            let links_json: String = row.get(7)?;
            let links: Vec<String> = serde_json::from_str(&links_json).unwrap_or_default();
            let meta_json: String = row.get(10)?;
            let meta: HashMap<String, String> = serde_json::from_str(&meta_json).unwrap_or_default();

            Ok(MultiModalAssetDescriptor {
                asset_id: row.get(0)?,
                blake3_digest: row.get(1)?,
                mime_type: row.get(2)?,
                dimensions: dims,
                perceptual_hash: row.get::<_, i64>(5)? as u64,
                feature_vector,
                associated_code_links: links,
                byte_size: row.get::<_, i64>(8)? as usize,
                storage_rel_path: row.get(9)?,
                metadata: meta,
                created_at_ms: row.get::<_, i64>(11)? as u64,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut candidates = Vec::new();
        for r in rows {
            let desc = r.map_err(|e| HgbError::Storage(e.to_string()))?;
            let dist = desc.perceptual_distance(target_p_hash);
            if dist <= max_hamming_dist {
                let sim = desc.perceptual_similarity(target_p_hash);
                candidates.push(ColdAssetHit {
                    descriptor: desc,
                    visual_similarity: sim,
                    feature_similarity: 0.0,
                    matched_code_link: None,
                });
            }
        }

        candidates.sort_by(|a, b| b.visual_similarity.partial_cmp(&a.visual_similarity).unwrap_or(Ordering::Equal));
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// Search multi-modal assets using high-dimensional visual feature vectors
    pub fn search_by_feature_vector(&self, query_vec: &[f32], min_similarity: f32, limit: usize) -> Result<Vec<ColdAssetHit>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT asset_id, blake3_digest, mime_type, width, height,
                    perceptual_hash, feature_vector_blob, associated_code_links_json,
                    byte_size, storage_rel_path, metadata_json, created_at_ms
             FROM cold_multimodal_assets",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map([], |row| {
            let w: Option<i64> = row.get(3)?;
            let h: Option<i64> = row.get(4)?;
            let dims = match (w, h) {
                (Some(w), Some(h)) => Some((w as u32, h as u32)),
                _ => None,
            };

            let feat_blob: Vec<u8> = row.get(6)?;
            let mut feature_vector = Vec::with_capacity(feat_blob.len() / 4);
            for chunk in feat_blob.chunks_exact(4) {
                feature_vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }

            let links_json: String = row.get(7)?;
            let links: Vec<String> = serde_json::from_str(&links_json).unwrap_or_default();
            let meta_json: String = row.get(10)?;
            let meta: HashMap<String, String> = serde_json::from_str(&meta_json).unwrap_or_default();

            Ok(MultiModalAssetDescriptor {
                asset_id: row.get(0)?,
                blake3_digest: row.get(1)?,
                mime_type: row.get(2)?,
                dimensions: dims,
                perceptual_hash: row.get::<_, i64>(5)? as u64,
                feature_vector,
                associated_code_links: links,
                byte_size: row.get::<_, i64>(8)? as usize,
                storage_rel_path: row.get(9)?,
                metadata: meta,
                created_at_ms: row.get::<_, i64>(11)? as u64,
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut candidates = Vec::new();
        for r in rows {
            let desc = r.map_err(|e| HgbError::Storage(e.to_string()))?;
            let sim = desc.feature_cosine_similarity(query_vec);
            if sim >= min_similarity {
                candidates.push(ColdAssetHit {
                    descriptor: desc,
                    visual_similarity: 0.0,
                    feature_similarity: sim,
                    matched_code_link: None,
                });
            }
        }

        candidates.sort_by(|a, b| b.feature_similarity.partial_cmp(&a.feature_similarity).unwrap_or(Ordering::Equal));
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// Search multi-modal assets associated with a specific code or markdown file path
    pub fn search_by_code_link(&self, code_path: &str) -> Result<Vec<ColdAssetHit>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.blake3_digest, a.mime_type, a.width, a.height,
                    a.perceptual_hash, a.feature_vector_blob, a.associated_code_links_json,
                    a.byte_size, a.storage_rel_path, a.metadata_json, a.created_at_ms,
                    l.code_link
             FROM cold_multimodal_assets a
             JOIN cold_asset_code_links l ON a.asset_id = l.asset_id
             WHERE l.code_link = ?1",
        ).map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt.query_map(params![code_path], |row| {
            let w: Option<i64> = row.get(3)?;
            let h: Option<i64> = row.get(4)?;
            let dims = match (w, h) {
                (Some(w), Some(h)) => Some((w as u32, h as u32)),
                _ => None,
            };

            let feat_blob: Vec<u8> = row.get(6)?;
            let mut feature_vector = Vec::with_capacity(feat_blob.len() / 4);
            for chunk in feat_blob.chunks_exact(4) {
                feature_vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }

            let links_json: String = row.get(7)?;
            let links: Vec<String> = serde_json::from_str(&links_json).unwrap_or_default();
            let meta_json: String = row.get(10)?;
            let meta: HashMap<String, String> = serde_json::from_str(&meta_json).unwrap_or_default();
            let matched: String = row.get(12)?;

            Ok(ColdAssetHit {
                descriptor: MultiModalAssetDescriptor {
                    asset_id: row.get(0)?,
                    blake3_digest: row.get(1)?,
                    mime_type: row.get(2)?,
                    dimensions: dims,
                    perceptual_hash: row.get::<_, i64>(5)? as u64,
                    feature_vector,
                    associated_code_links: links,
                    byte_size: row.get::<_, i64>(8)? as usize,
                    storage_rel_path: row.get(9)?,
                    metadata: meta,
                    created_at_ms: row.get::<_, i64>(11)? as u64,
                },
                visual_similarity: 1.0,
                feature_similarity: 1.0,
                matched_code_link: Some(matched),
            })
        }).map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
        }
        Ok(out)
    }
}

// ==============================================================================
// 4. Unified Multi-Tier Query Planner
// ==============================================================================

/// Specification for a unified multi-tier hierarchical query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiTierQuerySpec {
    pub semantic_vector: Option<Vec<f32>>,
    pub hot_top_k: usize,
    pub hot_ef_search: usize,
    pub session_id: Option<String>,
    pub checkpoint_kind: Option<CheckpointKind>,
    pub tag: Option<String>,
    pub warm_limit: usize,
    pub visual_perceptual_hash: Option<u64>,
    pub visual_feature_vector: Option<Vec<f32>>,
    pub max_hamming_distance: u32,
    pub associated_code_link: Option<String>,
    pub cold_limit: usize,
}

impl Default for MultiTierQuerySpec {
    fn default() -> Self {
        Self {
            semantic_vector: None,
            hot_top_k: 10,
            hot_ef_search: 48,
            session_id: None,
            checkpoint_kind: None,
            tag: None,
            warm_limit: 10,
            visual_perceptual_hash: None,
            visual_feature_vector: None,
            max_hamming_distance: 12,
            associated_code_link: None,
            cold_limit: 5,
        }
    }
}

/// Decompressed Warm Tier checkpoint trace hit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WarmTraceHit {
    pub checkpoint: WarmStateCheckpoint,
    pub decompressed_payload: Vec<u8>,
}

/// Microsecond-precision execution telemetry across hierarchical memory tiers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryPlanTelemetry {
    pub hot_tier_latency_us: u64,
    pub warm_tier_latency_us: u64,
    pub cold_tier_latency_us: u64,
    pub total_execution_time_us: u64,
    pub hot_hits_count: usize,
    pub warm_hits_count: usize,
    pub cold_hits_count: usize,
}

/// Consolidated hierarchical multi-modal query resolution bundle
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnifiedHierarchicalQueryResult {
    pub hot_vector_hits: Vec<HotVectorHit>,
    pub warm_state_traces: Vec<WarmTraceHit>,
    pub cold_multimodal_assets: Vec<ColdAssetHit>,
    pub telemetry: QueryPlanTelemetry,
}

/// Unified query planner that orchestrates Hot -> Warm -> Cold multi-tier resolution
pub struct UnifiedQueryPlanner {
    hot_tier: Arc<RwLock<QuantizedHnswIndex>>,
    warm_tier: Arc<WarmTierEngine>,
    cold_tier: Arc<ColdStorageEngine>,
}

/// Alias for ColdStorageEngine
pub type ColdObjectStorageEngine = ColdStorageEngine;
/// Alias for UnifiedQueryPlanner
pub type UnifiedHierarchicalRAGCache = UnifiedQueryPlanner;


impl UnifiedQueryPlanner {
    pub fn new(
        hot_tier: Arc<RwLock<QuantizedHnswIndex>>,
        warm_tier: Arc<WarmTierEngine>,
        cold_tier: Arc<ColdStorageEngine>,
    ) -> Self {
        Self {
            hot_tier,
            warm_tier,
            cold_tier,
        }
    }

    /// Execute unified hierarchical retrieval across all memory tiers
    pub fn execute(&self, spec: &MultiTierQuerySpec) -> Result<UnifiedHierarchicalQueryResult> {
        let overall_start = Instant::now();

        // 1. Hot Tier: Sub-millisecond SIMD Quantized HNSW vector retrieval
        let hot_start = Instant::now();
        let hot_hits = if let Some(ref vec) = spec.semantic_vector {
            let index = self.hot_tier.read().unwrap();
            index.search_knn(vec, spec.hot_top_k, spec.hot_ef_search)
        } else {
            Vec::new()
        };
        let hot_latency_us = hot_start.elapsed().as_micros() as u64;

        // 2. Warm Tier: Compressed SQLite state checkpoints & tool traces
        let warm_start = Instant::now();
        let mut warm_checkpoints = Vec::new();

        if let Some(ref session) = spec.session_id {
            let session_cps = self.warm_tier.query_session_checkpoints(session, spec.warm_limit)?;
            warm_checkpoints.extend(session_cps);
        } else if spec.checkpoint_kind.is_some() || spec.tag.is_some() {
            let filtered = self.warm_tier.search_by_kind_and_tag(
                spec.checkpoint_kind,
                spec.tag.as_deref(),
                spec.warm_limit,
            )?;
            warm_checkpoints.extend(filtered);
        }

        let mut warm_traces = Vec::with_capacity(warm_checkpoints.len());
        for cp in warm_checkpoints {
            let payload = cp.decompress_payload()?;
            warm_traces.push(WarmTraceHit {
                checkpoint: cp,
                decompressed_payload: payload,
            });
        }
        let warm_latency_us = warm_start.elapsed().as_micros() as u64;

        // 3. Cold Tier: Content-addressed multi-modal assets
        let cold_start = Instant::now();
        let mut cold_hits = Vec::new();

        if let Some(p_hash) = spec.visual_perceptual_hash {
            let hits = self.cold_tier.search_by_visual_similarity(p_hash, spec.max_hamming_distance, spec.cold_limit)?;
            cold_hits.extend(hits);
        }

        if let Some(ref feat_vec) = spec.visual_feature_vector {
            let hits = self.cold_tier.search_by_feature_vector(feat_vec, 0.70, spec.cold_limit)?;
            for h in hits {
                if !cold_hits.iter().any(|existing| existing.descriptor.blake3_digest == h.descriptor.blake3_digest) {
                    cold_hits.push(h);
                }
            }
        }

        if let Some(ref code_link) = spec.associated_code_link {
            let hits = self.cold_tier.search_by_code_link(code_link)?;
            for h in hits {
                if !cold_hits.iter().any(|existing| existing.descriptor.blake3_digest == h.descriptor.blake3_digest) {
                    cold_hits.push(h);
                }
            }
        }
        let cold_latency_us = cold_start.elapsed().as_micros() as u64;

        let total_us = overall_start.elapsed().as_micros() as u64;

        Ok(UnifiedHierarchicalQueryResult {
            hot_vector_hits: hot_hits.clone(),
            warm_state_traces: warm_traces.clone(),
            cold_multimodal_assets: cold_hits.clone(),
            telemetry: QueryPlanTelemetry {
                hot_tier_latency_us: hot_latency_us,
                warm_tier_latency_us: warm_latency_us,
                cold_tier_latency_us: cold_latency_us,
                total_execution_time_us: total_us,
                hot_hits_count: hot_hits.len(),
                warm_hits_count: warm_traces.len(),
                cold_hits_count: cold_hits.len(),
            },
        })
    }
}

// ==============================================================================
// 5. Complete Production Facade: HierarchicalMultiModalRagCache
// ==============================================================================

/// Hierarchical Multi-Modal RAG Cache Engine (Pillar 2)
pub struct HierarchicalMultiModalRagCache {
    hot_tier: Arc<RwLock<QuantizedHnswIndex>>,
    warm_tier: Arc<WarmTierEngine>,
    cold_tier: Arc<ColdStorageEngine>,
    planner: Arc<UnifiedQueryPlanner>,
}

impl HierarchicalMultiModalRagCache {
    /// Initializes all hierarchical tiers in the specified root directory
    pub fn new(root_dir: impl AsRef<Path>, hnsw_config: Option<QuantizedHnswConfig>) -> Result<Self> {
        let root = root_dir.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;

        let cfg = hnsw_config.unwrap_or_default();
        let hot_tier = Arc::new(RwLock::new(QuantizedHnswIndex::new(cfg)));
        let warm_tier = Arc::new(WarmTierEngine::new(root.join("warm_checkpoints.db"))?);
        let cold_tier = Arc::new(ColdStorageEngine::new(root.join("cold_store"))?);

        let planner = Arc::new(UnifiedQueryPlanner::new(
            hot_tier.clone(),
            warm_tier.clone(),
            cold_tier.clone(),
        ));

        Ok(Self {
            hot_tier,
            warm_tier,
            cold_tier,
            planner,
        })
    }

    pub fn hot_tier(&self) -> &Arc<RwLock<QuantizedHnswIndex>> {
        &self.hot_tier
    }

    pub fn warm_tier(&self) -> &Arc<WarmTierEngine> {
        &self.warm_tier
    }

    pub fn cold_tier(&self) -> &Arc<ColdStorageEngine> {
        &self.cold_tier
    }

    pub fn planner(&self) -> &Arc<UnifiedQueryPlanner> {
        &self.planner
    }

    /// Insert vector into Hot Tier in-memory quantized HNSW index
    pub fn insert_hot_vector(&self, id: &str, vector: &[f32], metadata: Option<String>, tags: Vec<String>) -> Result<()> {
        let record = QuantizedVectorRecord::from_f32_slice(id, vector, metadata, tags);
        let mut index = self.hot_tier.write().unwrap();
        index.insert(record)
    }

    /// Store compressed checkpoint into Warm Tier
    pub fn store_warm_checkpoint(
        &self,
        checkpoint_id: &str,
        session_id: &str,
        turn_index: u64,
        kind: CheckpointKind,
        payload: &[u8],
        tags: Vec<String>,
    ) -> Result<()> {
        let checkpoint = WarmStateCheckpoint::new(checkpoint_id, session_id, turn_index, kind, payload, tags);
        self.warm_tier.put_checkpoint(&checkpoint)
    }

    /// Store content-addressed multi-modal asset into Cold Tier
    pub fn store_cold_asset(&self, descriptor: MultiModalAssetDescriptor, data: &[u8]) -> Result<String> {
        self.cold_tier.store_asset(descriptor, data)
    }

    /// Read content-addressed multi-modal asset from Cold Tier with BLAKE3 tamper verification
    pub fn read_cold_asset(&self, blake3_digest: &str) -> Result<(MultiModalAssetDescriptor, Vec<u8>)> {
        self.cold_tier.read_asset(blake3_digest)
    }

    /// Execute unified query across Hot -> Warm -> Cold tiers
    pub fn query(&self, spec: &MultiTierQuerySpec) -> Result<UnifiedHierarchicalQueryResult> {
        self.planner.execute(spec)
    }
}

// ==============================================================================
// 6. Comprehensive Production Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_quantized_kernels_and_metrics() {
        let v1 = vec![0.5f32, -0.8f32, 0.1f32, 0.9f32, -0.3f32, 0.0f32, 0.7f32, -0.4f32];
        let v2 = vec![0.4f32, -0.7f32, 0.2f32, 0.8f32, -0.2f32, 0.1f32, 0.6f32, -0.5f32];

        let q1 = QuantizedVectorRecord::from_f32_slice("v1", &v1, None, vec!["tag1".into()]);
        let q2 = QuantizedVectorRecord::from_f32_slice("v2", &v2, None, vec!["tag2".into()]);

        assert_eq!(q1.dimension, 8);
        assert_eq!(q2.dimension, 8);

        let dot = simd_quantized_dot_product(&q1.bytes, &q2.bytes);
        assert!(dot > 0, "Collinear vectors must yield positive dot product");

        let cos_dist = simd_quantized_cosine_distance(&q1, &q2);
        assert!(cos_dist < 0.15, "Similar vectors must have small cosine distance: got {cos_dist}");

        let ip_dist = simd_quantized_inner_product_distance(&q1, &q2);
        assert!(ip_dist < 0.30, "Similar vectors must have small inner product distance");

        let deq1 = q1.dequantize();
        for i in 0..v1.len() {
            assert!((v1[i] - deq1[i]).abs() < 0.05, "Quantization error must be small");
        }
    }

    #[test]
    fn test_quantized_hnsw_construction_and_knn_search() {
        let config = QuantizedHnswConfig {
            m: 8,
            m0: 16,
            ef_construction: 32,
            ef_search: 24,
            metric: QuantizedMetric::Cosine,
            dimension: 8,
        };

        let mut index = QuantizedHnswIndex::new(config);

        for i in 0..40 {
            let mut vec = vec![0.0f32; 8];
            vec[i % 8] = 1.0;
            vec[(i + 1) % 8] = 0.5;
            let record = QuantizedVectorRecord::from_f32_slice(
                format!("node_{i}"),
                &vec,
                Some(format!("meta_{i}")),
                vec!["cluster_a".into()],
            );
            index.insert(record).expect("HNSW insertion must succeed");
        }

        assert_eq!(index.len(), 40);

        let mut query = vec![0.0f32; 8];
        query[0] = 1.0;
        query[1] = 0.5;

        let hits = index.search_knn(&query, 5, 24);
        assert!(!hits.is_empty(), "Must return nearest neighbors");
        assert_eq!(hits[0].id, "node_0", "Exact match must rank first");
        assert!(hits[0].distance < 0.05, "Exact match distance should be close to 0");
        assert!(hits[0].similarity > 0.95, "Exact match similarity should be close to 1.0");
    }

    #[test]
    fn test_lzss_deflate_codec_roundtrip_integrity() {
        let sample_trace = r#"{"tool":"run_cargo_test","exit_code":0,"stdout":"running 42 tests... test result: ok. 42 passed; 0 failed","tags":["build","fast"]}"#;
        let payload = sample_trace.repeat(10);
        let raw_bytes = payload.as_bytes();

        let compressed = DeflateCodec::compress(raw_bytes);
        assert!(compressed.len() < raw_bytes.len(), "Repeating trace must be compressed");

        let ratio = DeflateCodec::compression_ratio(raw_bytes.len(), compressed.len());
        assert!(ratio > 30.0, "Achieved compression ratio should exceed 30%, got {ratio:.2}%");

        let decompressed = DeflateCodec::decompress(&compressed).expect("Decompression must succeed");
        assert_eq!(decompressed, raw_bytes, "Roundtrip bytes must be identical");

        // Edge case: empty slice
        let empty_comp = DeflateCodec::compress(b"");
        let empty_decomp = DeflateCodec::decompress(&empty_comp).expect("Empty decompression must succeed");
        assert!(empty_decomp.is_empty());
    }

    #[test]
    fn test_warm_tier_sqlite_compressed_checkpoints() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_warm_test_{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("warm_tier.db");

        let warm_tier = WarmTierEngine::new(&db_path).expect("Engine initialization must succeed");

        let log_data = b"turn_1: agent analyzed AST syntax graph and found 0 compilation errors.";
        let cp = WarmStateCheckpoint::new(
            "cp_001",
            "session_xyz",
            1,
            CheckpointKind::ToolExecutionTrace,
            log_data,
            vec!["ast".into(), "diagnostic".into()],
        );

        warm_tier.put_checkpoint(&cp).expect("Checkpoint put must succeed");

        let retrieved = warm_tier.get_checkpoint("cp_001").expect("Get must succeed").expect("Checkpoint must exist");
        assert_eq!(retrieved.checkpoint_id, "cp_001");
        assert_eq!(retrieved.session_id, "session_xyz");
        assert_eq!(retrieved.turn_index, 1);
        assert_eq!(retrieved.checkpoint_kind, CheckpointKind::ToolExecutionTrace);

        let decompressed = retrieved.decompress_payload().expect("Decompress must succeed");
        assert_eq!(decompressed, log_data);

        let session_cps = warm_tier.query_session_checkpoints("session_xyz", 10).expect("Query must succeed");
        assert_eq!(session_cps.len(), 1);

        let stats = warm_tier.stats().expect("Stats must succeed");
        assert_eq!(stats.total_checkpoints, 1);
        assert_eq!(stats.total_raw_bytes, log_data.len());

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_cold_tier_multimodal_content_addressed_storage() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_cold_test_{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();

        let cold_tier = ColdStorageEngine::new(&temp_dir).expect("Cold engine init must succeed");

        let fake_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRwinged_sandal_schematic_data";
        let p_hash = 0xAAAAAAAAAAAAAAAAu64;
        let feat_vec = vec![0.1f32, 0.9f32, 0.0f32, -0.4f32];

        let desc = MultiModalAssetDescriptor {
            asset_id: "asset_winged_sandal_01".into(),
            dimensions: Some((512, 512)),
            mime_type: "image/png".into(),
            perceptual_hash: p_hash,
            feature_vector: feat_vec.clone(),
            associated_code_links: vec!["crates/hgb-storage/src/lib.rs".into()],
            blake3_digest: String::new(),
            byte_size: 0,
            storage_rel_path: String::new(),
            created_at_ms: 1000,
            metadata: HashMap::new(),
        };

        let digest = cold_tier.store_asset(desc, fake_png).expect("Store asset must succeed");
        assert!(!digest.is_empty());

        let (retrieved_desc, retrieved_bytes) = cold_tier.read_asset(&digest).expect("Read asset must succeed");
        assert_eq!(retrieved_bytes, fake_png);
        assert_eq!(retrieved_desc.dimensions, Some((512, 512)));
        assert_eq!(retrieved_desc.mime_type, "image/png");

        let similar_phash = 0xAAAAAAAAAAAAAAA0u64; // Hamming distance = 2
        let visual_hits = cold_tier.search_by_visual_similarity(similar_phash, 4, 5).expect("Visual search must succeed");
        assert_eq!(visual_hits.len(), 1);
        assert!(visual_hits[0].visual_similarity > 0.90);

        let code_hits = cold_tier.search_by_code_link("crates/hgb-storage/src/lib.rs").expect("Code search must succeed");
        assert_eq!(code_hits.len(), 1);
        assert_eq!(code_hits[0].descriptor.asset_id, "asset_winged_sandal_01");

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_unified_multi_tier_query_planner() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_unified_test_{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();

        let cache = HierarchicalMultiModalRagCache::new(&temp_dir, None).expect("Cache init must succeed");

        // 1. Seed Hot Tier
        let hot_vec = vec![0.8f32, 0.2f32, -0.1f32, 0.5f32];
        cache.insert_hot_vector("hot_doc_42", &hot_vec, Some("hot metadata".into()), vec!["rag".into()])
            .expect("Hot insert must succeed");

        // 2. Seed Warm Tier
        let warm_payload = br#"{"action":"reindex_ast","duration_ms":12}"#;
        cache.store_warm_checkpoint("cp_turn_1", "sess_alpha", 1, CheckpointKind::ToolExecutionTrace, warm_payload, vec!["ast".into()])
            .expect("Warm checkpoint store must succeed");

        // 3. Seed Cold Tier
        let diagram_data = b"cold_diagram_binary_representation_for_winged_sandal";
        let desc = MultiModalAssetDescriptor {
            asset_id: "diag_01".into(),
            dimensions: Some((1024, 768)),
            mime_type: "image/svg+xml".into(),
            perceptual_hash: 0x1122334455667788u64,
            feature_vector: vec![0.7f32, 0.3f32, 0.0f32, 0.1f32],
            associated_code_links: vec!["crates/hgb-storage/src/hierarchical_multimodal_rag_cache.rs".into()],
            blake3_digest: String::new(),
            byte_size: 0,
            storage_rel_path: String::new(),
            created_at_ms: 2000,
            metadata: HashMap::new(),
        };
        cache.store_cold_asset(desc, diagram_data).expect("Cold asset store must succeed");

        // 4. Unified Query across Hot, Warm, and Cold
        let query_spec = MultiTierQuerySpec {
            semantic_vector: Some(hot_vec),
            hot_top_k: 3,
            hot_ef_search: 24,
            session_id: Some("sess_alpha".into()),
            checkpoint_kind: None,
            tag: None,
            warm_limit: 5,
            visual_perceptual_hash: Some(0x1122334455667788u64),
            visual_feature_vector: None,
            max_hamming_distance: 2,
            associated_code_link: Some("crates/hgb-storage/src/hierarchical_multimodal_rag_cache.rs".into()),
            cold_limit: 3,
        };

        let result = cache.query(&query_spec).expect("Unified query execution must succeed");

        assert_eq!(result.hot_vector_hits.len(), 1);
        assert_eq!(result.hot_vector_hits[0].id, "hot_doc_42");

        assert_eq!(result.warm_state_traces.len(), 1);
        assert_eq!(result.warm_state_traces[0].checkpoint.checkpoint_id, "cp_turn_1");
        assert_eq!(result.warm_state_traces[0].decompressed_payload, warm_payload);

        assert!(!result.cold_multimodal_assets.is_empty());
        assert_eq!(result.cold_multimodal_assets[0].descriptor.asset_id, "diag_01");

        assert!(result.telemetry.total_execution_time_us > 0);
        assert_eq!(result.telemetry.hot_hits_count, 1);
        assert_eq!(result.telemetry.warm_hits_count, 1);
        assert!(result.telemetry.cold_hits_count >= 1);

        let _ = fs::remove_dir_all(temp_dir);
    }
}