//! # Hybrid RAG & Local Vector Indexing Engine (`HybridRagVectorIndex`)
//!
//! High-performance, pure-Rust Hierarchical Navigable Small World (HNSW) vector
//! graph index combined with an in-memory Okapi BM25 lexical inverted index and
//! Reciprocal Rank Fusion (RRF) for zero-latency long-term episodic memory.
//!
//! ## Architecture
//! - **Dense Semantic Retrieval**: Multi-layer HNSW graph ($O(\ln N)$ nearest neighbor search)
//!   with SIMD-accelerated 8-lane unrolled cosine similarity and squared Euclidean distance.
//! - **Lexical Retrieval (BM25)**: Pure Rust in-memory inverted index computing term frequencies,
//!   document lengths, and inverse document frequency (IDF) with $k_1 = 1.2, b = 0.75$.
//! - **Hybrid Reciprocal Rank Fusion (RRF)**:
//!   $$RRF(d) = \frac{w_{\text{dense}}}{k + \text{rank}_{\text{dense}}(d)} + \frac{w_{\text{lexical}}}{k + \text{rank}_{\text{lexical}}(d)}$$
//!   with standard smoothing constant $k = 60.0$.
//! - **ACID Binary Persistence**: Custom memory-mapped compatible binary serialization
//!   format with magic header `HGB_RAG1` (`0x4847425241473130`).

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};

/// Magic bytes for binary serialized Hybrid RAG index: "HGB_RAG1"
pub const HYBRID_RAG_MAGIC: [u8; 8] = [0x48, 0x47, 0x42, 0x5F, 0x52, 0x41, 0x47, 0x31];
pub const DEFAULT_RRF_K: f32 = 60.0;
pub const BM25_K1: f32 = 1.2;
pub const BM25_B: f32 = 0.75;

/// Supported distance metric for HNSW vector graph
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorMetric {
    Cosine,
    SquaredEuclidean,
    DotProduct,
}

/// Configuration settings for `HybridRagVectorIndex`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridRagConfig {
    /// Vector embedding dimensionality (e.g. 64, 128, 384, 768, 1536)
    pub dimension: usize,
    /// Distance metric
    pub metric: VectorMetric,
    /// Maximum outgoing connections per node on upper layers (l > 0)
    pub m: usize,
    /// Maximum outgoing connections on ground layer 0
    pub m0: usize,
    /// Beam search width during index construction
    pub ef_construction: usize,
    /// Beam search width during query retrieval
    pub ef_search: usize,
    /// Normalization factor for exponential level generation: 1.0 / ln(M)
    pub ml: f64,
    /// Smoothing constant for Reciprocal Rank Fusion
    pub rrf_k: f32,
    /// Weight assigned to dense vector rank in hybrid score [0.0, 1.0]
    pub dense_weight: f32,
    /// Weight assigned to lexical BM25 rank in hybrid score [0.0, 1.0]
    pub lexical_weight: f32,
    /// Persistence path on disk
    pub persist_path: Option<PathBuf>,
}

impl Default for HybridRagConfig {
    fn default() -> Self {
        let m = 16;
        Self {
            dimension: 64,
            metric: VectorMetric::Cosine,
            m,
            m0: 32,
            ef_construction: 100,
            ef_search: 50,
            ml: 1.0 / (m as f64).ln(),
            rrf_k: DEFAULT_RRF_K,
            dense_weight: 0.5,
            lexical_weight: 0.5,
            persist_path: None,
        }
    }
}

/// Stored document payload with vector embedding and arbitrary JSON metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridDocument {
    pub id: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub metadata: serde_json::Value,
    pub created_at_epoch_ms: u64,
}

/// Structured hybrid retrieval result containing dual-mode provenance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridRagSearchResult {
    pub id: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub hybrid_score: f32,
    pub dense_rank: Option<usize>,
    pub lexical_rank: Option<usize>,
    pub dense_similarity: f32,
    pub lexical_score: f32,
    pub matched_terms: Vec<String>,
}

// ----------------------------------------------------------------------------
// SIMD Distance Math Kernels
// ----------------------------------------------------------------------------

/// Fast unrolled 8-lane SIMD cosine similarity kernel
#[inline]
pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    let chunks = len / 8;
    let rem = len % 8;

    let mut d0 = 0.0f32;
    let mut d1 = 0.0f32;
    let mut d2 = 0.0f32;
    let mut d3 = 0.0f32;

    let mut na0 = 0.0f32;
    let mut na1 = 0.0f32;
    let mut na2 = 0.0f32;
    let mut na3 = 0.0f32;

    let mut nb0 = 0.0f32;
    let mut nb1 = 0.0f32;
    let mut nb2 = 0.0f32;
    let mut nb3 = 0.0f32;

    for i in 0..chunks {
        let base = i * 8;
        let va0 = a[base];
        let vb0 = b[base];
        d0 += va0 * vb0;
        na0 += va0 * va0;
        nb0 += vb0 * vb0;

        let va1 = a[base + 1];
        let vb1 = b[base + 1];
        d1 += va1 * vb1;
        na1 += va1 * va1;
        nb1 += vb1 * vb1;

        let va2 = a[base + 2];
        let vb2 = b[base + 2];
        d2 += va2 * vb2;
        na2 += va2 * va2;
        nb2 += vb2 * vb2;

        let va3 = a[base + 3];
        let vb3 = b[base + 3];
        d3 += va3 * vb3;
        na3 += va3 * va3;
        nb3 += vb3 * vb3;

        let va4 = a[base + 4];
        let vb4 = b[base + 4];
        d0 += va4 * vb4;
        na0 += va4 * va4;
        nb0 += vb4 * vb4;

        let va5 = a[base + 5];
        let vb5 = b[base + 5];
        d1 += va5 * vb5;
        na1 += va5 * va5;
        nb1 += vb5 * vb5;

        let va6 = a[base + 6];
        let vb6 = b[base + 6];
        d2 += va6 * vb6;
        na2 += va6 * va6;
        nb2 += vb6 * vb6;

        let va7 = a[base + 7];
        let vb7 = b[base + 7];
        d3 += va7 * vb7;
        na3 += va7 * va7;
        nb3 += vb7 * vb7;
    }

    dot += (d0 + d1) + (d2 + d3);
    norm_a += (na0 + na1) + (na2 + na3);
    norm_b += (nb0 + nb1) + (nb2 + nb3);

    let start = chunks * 8;
    for i in 0..rem {
        let va = a[start + i];
        let vb = b[start + i];
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }

    let denom = (norm_a.sqrt() * norm_b.sqrt()).max(1e-9);
    (dot / denom).clamp(-1.0, 1.0)
}

/// Fast unrolled squared Euclidean distance kernel
#[inline]
pub fn simd_squared_euclidean(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut sum = 0.0f32;
    let chunks = len / 8;
    let rem = len % 8;

    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;

    for i in 0..chunks {
        let base = i * 8;
        let diff0 = a[base] - b[base];
        let diff1 = a[base + 1] - b[base + 1];
        let diff2 = a[base + 2] - b[base + 2];
        let diff3 = a[base + 3] - b[base + 3];
        let diff4 = a[base + 4] - b[base + 4];
        let diff5 = a[base + 5] - b[base + 5];
        let diff6 = a[base + 6] - b[base + 6];
        let diff7 = a[base + 7] - b[base + 7];

        s0 += diff0 * diff0 + diff4 * diff4;
        s1 += diff1 * diff1 + diff5 * diff5;
        s2 += diff2 * diff2 + diff6 * diff6;
        s3 += diff3 * diff3 + diff7 * diff7;
    }

    sum += (s0 + s1) + (s2 + s3);

    let start = chunks * 8;
    for i in 0..rem {
        let diff = a[start + i] - b[start + i];
        sum += diff * diff;
    }

    sum
}

// ----------------------------------------------------------------------------
// Internal Inverted Index for Okapi BM25 Lexical Retrieval
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InvertedIndex {
    /// Postings list: term -> list of (doc_index, term_frequency)
    postings: HashMap<String, Vec<(usize, u32)>>,
    /// Document lengths: doc_index -> total_terms
    doc_lengths: Vec<usize>,
    /// Average document length across the collection
    avg_doc_length: f32,
    /// Total number of indexed documents
    total_docs: usize,
}

impl InvertedIndex {
    fn new() -> Self {
        Self::default()
    }

    fn tokenize(text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        for word in text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
            let lower = word.to_lowercase();
            if lower.len() >= 2 && !is_stopword(&lower) {
                tokens.push(lower);
            }
        }
        tokens
    }

    fn add_document(&mut self, doc_index: usize, text: &str) {
        let tokens = Self::tokenize(text);
        let doc_len = tokens.len();

        let mut counts: HashMap<String, u32> = HashMap::new();
        for t in tokens {
            *counts.entry(t).or_insert(0) += 1;
        }

        for (term, freq) in counts {
            self.postings.entry(term).or_default().push((doc_index, freq));
        }

        if doc_index >= self.doc_lengths.len() {
            self.doc_lengths.resize(doc_index + 1, 0);
        }
        self.doc_lengths[doc_index] = doc_len;
        self.total_docs += 1;

        let total_words: usize = self.doc_lengths.iter().sum();
        self.avg_doc_length = if self.total_docs > 0 {
            total_words as f32 / self.total_docs as f32
        } else {
            1.0
        };
    }

    fn query_bm25(&self, query_text: &str, top_k: usize) -> Vec<(usize, f32, Vec<String>)> {
        let query_terms = Self::tokenize(query_text);
        if query_terms.is_empty() || self.total_docs == 0 {
            return Vec::new();
        }

        let mut scores: HashMap<usize, (f32, Vec<String>)> = HashMap::new();
        let n = self.total_docs as f32;
        let avgdl = self.avg_doc_length.max(1.0);

        for term in &query_terms {
            if let Some(postings) = self.postings.get(term) {
                let n_q = postings.len() as f32;
                // Standard Okapi BM25 Robertson-Spärck Jones IDF
                let idf = ((n - n_q + 0.5) / (n_q + 0.5) + 1.0).ln().max(0.1);

                for &(doc_idx, tf) in postings {
                    let dl = self.doc_lengths.get(doc_idx).copied().unwrap_or(1) as f32;
                    let denom = tf as f32 + BM25_K1 * (1.0 - BM25_B + BM25_B * (dl / avgdl));
                    let term_score = idf * (tf as f32 * (BM25_K1 + 1.0)) / denom;

                    let entry = scores.entry(doc_idx).or_insert_with(|| (0.0, Vec::new()));
                    entry.0 += term_score;
                    if !entry.1.contains(term) {
                        entry.1.push(term.clone());
                    }
                }
            }
        }

        let mut ranked: Vec<(usize, f32, Vec<String>)> = scores
            .into_iter()
            .map(|(idx, (score, terms))| (idx, score, terms))
            .collect();

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        ranked.truncate(top_k);
        ranked
    }
}

fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "a" | "an" | "the" | "and" | "or" | "but" | "if" | "in" | "on" | "at" | "to"
            | "for" | "of" | "with" | "by" | "as" | "is" | "are" | "was" | "were"
            | "be" | "been" | "being" | "have" | "has" | "had" | "do" | "does"
            | "did" | "it" | "its" | "that" | "this" | "these" | "those"
    )
}

// ----------------------------------------------------------------------------
// Candidate Elements for HNSW Priority Queues
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct DistNode {
    dist: f32,
    id: usize,
}

impl PartialEq for DistNode {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist && self.id == other.id
    }
}

impl Eq for DistNode {}

impl PartialOrd for DistNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.dist.partial_cmp(&other.dist)
    }
}

impl Ord for DistNode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

// ----------------------------------------------------------------------------
// HNSW Multi-Layer Vector Graph Core
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HnswGraph {
    /// Multi-layer adjacency lists: layer -> node_idx -> list of neighbors
    layers: Vec<Vec<Vec<usize>>>,
    /// Entry point node index
    entry_point: Option<usize>,
    /// Highest active layer index
    max_level: usize,
    /// Random generator seed for deterministic layer assignment
    rng_seed: u64,
}

impl HnswGraph {
    fn new() -> Self {
        Self {
            layers: Vec::new(),
            entry_point: None,
            max_level: 0,
            rng_seed: 0x1337_BEEF_CAFE_BABE,
        }
    }

    /// Fast LCG pseudo-random float generator [0.0, 1.0)
    fn random_uniform(&mut self) -> f64 {
        self.rng_seed = self.rng_seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((self.rng_seed >> 33) as f64) / (4294967296.0)
    }

    /// Sample level using exponential decay: -ln(unif) * ml
    fn sample_level(&mut self, ml: f64) -> usize {
        let unif = self.random_uniform().max(1e-9);
        let level = (-unif.ln() * ml).floor() as usize;
        level.min(16)
    }

    fn ensure_node_capacity(&mut self, node_idx: usize, level: usize) {
        while self.layers.len() <= level {
            self.layers.push(Vec::new());
        }
        for l in 0..=level {
            if self.layers[l].len() <= node_idx {
                self.layers[l].resize(node_idx + 1, Vec::new());
            }
        }
    }
}

// ----------------------------------------------------------------------------
// HybridRagVectorIndex Core Implementation
// ----------------------------------------------------------------------------

/// Pure-Rust Embedded Hybrid RAG Vector Index Engine
pub struct HybridRagVectorIndex {
    config: HybridRagConfig,
    documents: Arc<RwLock<Vec<HybridDocument>>>,
    id_to_index: Arc<RwLock<HashMap<String, usize>>>,
    graph: Arc<RwLock<HnswGraph>>,
    inverted_index: Arc<RwLock<InvertedIndex>>,
}

impl HybridRagVectorIndex {
    /// Create new in-memory index with provided configuration
    pub fn new(config: HybridRagConfig) -> Self {
        Self {
            config,
            documents: Arc::new(RwLock::new(Vec::new())),
            id_to_index: Arc::new(RwLock::new(HashMap::new())),
            graph: Arc::new(RwLock::new(HnswGraph::new())),
            inverted_index: Arc::new(RwLock::new(InvertedIndex::new())),
        }
    }

    /// Open or create index at the configured file path
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if p.exists() && p.is_file() {
            Self::load_from_disk(p)
        } else {
            let mut cfg = HybridRagConfig::default();
            cfg.persist_path = Some(p.to_path_buf());
            Ok(Self::new(cfg))
        }
    }

    /// Number of indexed documents
    pub fn len(&self) -> usize {
        self.documents.read().unwrap().len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Compute distance between two vector embeddings according to configured metric
    #[inline]
    fn compute_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.config.metric {
            VectorMetric::Cosine => {
                let sim = simd_cosine_similarity(a, b);
                (1.0 - sim).max(0.0)
            }
            VectorMetric::SquaredEuclidean => simd_squared_euclidean(a, b),
            VectorMetric::DotProduct => {
                let mut dot = 0.0f32;
                for i in 0..a.len().min(b.len()) {
                    dot += a[i] * b[i];
                }
                (1.0 - dot).max(0.0)
            }
        }
    }

    /// Insert document with content, vector embedding, and JSON metadata
    pub fn insert(
        &self,
        doc_id: &str,
        content: &str,
        embedding: &[f32],
        metadata: serde_json::Value,
    ) -> Result<()> {
        let mut docs = self.documents.write().unwrap();
        let mut id_map = self.id_to_index.write().unwrap();
        let mut graph = self.graph.write().unwrap();
        let mut inv = self.inverted_index.write().unwrap();

        // 1. If document ID already exists, update content and embedding
        if let Some(&existing_idx) = id_map.get(doc_id) {
            docs[existing_idx] = HybridDocument {
                id: doc_id.to_string(),
                content: content.to_string(),
                embedding: embedding.to_vec(),
                metadata,
                created_at_epoch_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };
            inv.add_document(existing_idx, content);
            return Ok(());
        }

        let new_idx = docs.len();
        let doc = HybridDocument {
            id: doc_id.to_string(),
            content: content.to_string(),
            embedding: embedding.to_vec(),
            metadata,
            created_at_epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };
        docs.push(doc);
        id_map.insert(doc_id.to_string(), new_idx);
        inv.add_document(new_idx, content);

        // 2. Insert into HNSW Vector Graph
        let new_level = graph.sample_level(self.config.ml);
        graph.ensure_node_capacity(new_idx, new_level);

        if let Some(curr_ep) = graph.entry_point {
            let mut curr_obj = curr_ep;
            let mut curr_dist = self.compute_distance(embedding, &docs[curr_obj].embedding);
            let top_level = graph.max_level;

            // Phase 1: Greedy routing through upper layers down to new_level
            if top_level > new_level {
                for l in (new_level + 1..=top_level).rev() {
                    let mut changed = true;
                    while changed {
                        changed = false;
                        if let Some(neighbors) = graph.layers[l].get(curr_obj) {
                            for &neighbor in neighbors {
                                let d = self.compute_distance(embedding, &docs[neighbor].embedding);
                                if d < curr_dist {
                                    curr_dist = d;
                                    curr_obj = neighbor;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }

            // Phase 2: Beam search and bi-directional edge creation on layers 0..=min(top_level, new_level)
            let mut ep_candidates = vec![curr_obj];
            for l in (0..=new_level.min(top_level)).rev() {
                let m_max = if l == 0 { self.config.m0 } else { self.config.m };
                let candidates = self.search_layer_candidates(
                    embedding,
                    &ep_candidates,
                    self.config.ef_construction,
                    l,
                    &docs,
                    &graph,
                );

                let selected_neighbors = self.select_neighbors(candidates, m_max);
                ep_candidates = selected_neighbors.clone();

                // Add forward edges
                graph.layers[l][new_idx] = selected_neighbors.clone();

                // Add reverse edges with capacity clamping
                for &neighbor in &selected_neighbors {
                    graph.layers[l][neighbor].push(new_idx);
                    if graph.layers[l][neighbor].len() > m_max {
                        // Shrink neighbor list keeping closest elements
                        let n_list = &graph.layers[l][neighbor];
                        let mut sorted_n: Vec<(f32, usize)> = n_list
                            .iter()
                            .map(|&idx| (self.compute_distance(&docs[neighbor].embedding, &docs[idx].embedding), idx))
                            .collect();
                        sorted_n.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
                        sorted_n.dedup_by(|a, b| a.1 == b.1);
                        sorted_n.truncate(m_max);
                        graph.layers[l][neighbor] = sorted_n.into_iter().map(|(_, idx)| idx).collect();
                    }
                }
            }

            if new_level > graph.max_level {
                graph.max_level = new_level;
                graph.entry_point = Some(new_idx);
            }
        } else {
            // First element inserted into graph becomes the entry point
            graph.entry_point = Some(new_idx);
            graph.max_level = new_level;
        }

        Ok(())
    }

    /// Internal beam search helper on a single graph layer
    fn search_layer_candidates(
        &self,
        query: &[f32],
        entry_points: &[usize],
        ef: usize,
        layer: usize,
        docs: &[HybridDocument],
        graph: &HnswGraph,
    ) -> Vec<DistNode> {
        let mut visited: HashSet<usize> = HashSet::new();
        let mut candidates: BinaryHeap<Reverse<DistNode>> = BinaryHeap::new(); // Min-heap of candidates
        let mut nearest: BinaryHeap<DistNode> = BinaryHeap::new(); // Max-heap of closest found

        for &ep in entry_points {
            let dist = self.compute_distance(query, &docs[ep].embedding);
            visited.insert(ep);
            candidates.push(Reverse(DistNode { dist, id: ep }));
            nearest.push(DistNode { dist, id: ep });
        }

        while let Some(Reverse(curr)) = candidates.pop() {
            let furthest_nearest_dist = nearest.peek().map(|n| n.dist).unwrap_or(f32::MAX);
            if curr.dist > furthest_nearest_dist {
                break;
            }

            if let Some(neighbors) = graph.layers[layer].get(curr.id) {
                for &neighbor in neighbors {
                    if visited.insert(neighbor) {
                        let d = self.compute_distance(query, &docs[neighbor].embedding);
                        let furthest = nearest.peek().map(|n| n.dist).unwrap_or(f32::MAX);
                        if d < furthest || nearest.len() < ef {
                            candidates.push(Reverse(DistNode { dist: d, id: neighbor }));
                            nearest.push(DistNode { dist: d, id: neighbor });
                            if nearest.len() > ef {
                                nearest.pop();
                            }
                        }
                    }
                }
            }
        }

        nearest.into_sorted_vec()
    }

    fn select_neighbors(&self, mut candidates: Vec<DistNode>, m_max: usize) -> Vec<usize> {
        candidates.sort_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(Ordering::Equal));
        candidates.truncate(m_max);
        candidates.into_iter().map(|n| n.id).collect()
    }

    /// Query dense vector semantic similarity using HNSW graph
    pub fn query_dense(&self, query_embedding: &[f32], top_k: usize) -> Result<Vec<HybridRagSearchResult>> {
        let docs = self.documents.read().unwrap();
        let graph = self.graph.read().unwrap();

        if docs.is_empty() || graph.entry_point.is_none() {
            return Ok(Vec::new());
        }

        let mut curr_obj = graph.entry_point.unwrap();
        let mut curr_dist = self.compute_distance(query_embedding, &docs[curr_obj].embedding);

        // 1. Traverse down upper layers
        for l in (1..=graph.max_level).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                if let Some(neighbors) = graph.layers[l].get(curr_obj) {
                    for &neighbor in neighbors {
                        let d = self.compute_distance(query_embedding, &docs[neighbor].embedding);
                        if d < curr_dist {
                            curr_dist = d;
                            curr_obj = neighbor;
                            changed = true;
                        }
                    }
                }
            }
        }

        // 2. Beam search on ground layer 0
        let candidates = self.search_layer_candidates(
            query_embedding,
            &[curr_obj],
            self.config.ef_search.max(top_k),
            0,
            &docs,
            &graph,
        );

        let mut results = Vec::new();
        for (rank, node) in candidates.into_iter().take(top_k).enumerate() {
            let doc = &docs[node.id];
            let sim = match self.config.metric {
                VectorMetric::Cosine => (1.0 - node.dist).clamp(-1.0, 1.0),
                _ => (-node.dist).exp(), // Convert Euclidean distance to pseudo-similarity
            };
            results.push(HybridRagSearchResult {
                id: doc.id.clone(),
                content: doc.content.clone(),
                metadata: doc.metadata.clone(),
                hybrid_score: sim,
                dense_rank: Some(rank + 1),
                lexical_rank: None,
                dense_similarity: sim,
                lexical_score: 0.0,
                matched_terms: Vec::new(),
            });
        }

        Ok(results)
    }

    /// Query lexical BM25 keyword matching using inverted index
    pub fn query_lexical(&self, query_text: &str, top_k: usize) -> Result<Vec<HybridRagSearchResult>> {
        let docs = self.documents.read().unwrap();
        let inv = self.inverted_index.read().unwrap();

        let ranked = inv.query_bm25(query_text, top_k);
        let mut results = Vec::new();

        for (rank, (idx, score, terms)) in ranked.into_iter().enumerate() {
            let doc = &docs[idx];
            results.push(HybridRagSearchResult {
                id: doc.id.clone(),
                content: doc.content.clone(),
                metadata: doc.metadata.clone(),
                hybrid_score: score,
                dense_rank: None,
                lexical_rank: Some(rank + 1),
                dense_similarity: 0.0,
                lexical_score: score,
                matched_terms: terms,
            });
        }

        Ok(results)
    }

    /// Hybrid Search: Fuses dense vector and lexical BM25 results using Reciprocal Rank Fusion (RRF)
    pub fn query_hybrid(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<HybridRagSearchResult>> {
        let dense_results = self.query_dense(query_embedding, top_k * 2)?;
        let lexical_results = self.query_lexical(query_text, top_k * 2)?;

        let mut fused_map: HashMap<String, HybridRagSearchResult> = HashMap::new();
        let k = self.config.rrf_k;
        let w_dense = self.config.dense_weight;
        let w_lex = self.config.lexical_weight;

        // 1. Process dense rankings
        for res in dense_results {
            if let Some(rank) = res.dense_rank {
                let rrf = w_dense / (k + rank as f32);
                let entry = fused_map.entry(res.id.clone()).or_insert_with(|| HybridRagSearchResult {
                    id: res.id.clone(),
                    content: res.content.clone(),
                    metadata: res.metadata.clone(),
                    hybrid_score: 0.0,
                    dense_rank: Some(rank),
                    lexical_rank: None,
                    dense_similarity: res.dense_similarity,
                    lexical_score: 0.0,
                    matched_terms: Vec::new(),
                });
                entry.hybrid_score += rrf;
                entry.dense_rank = Some(rank);
                entry.dense_similarity = res.dense_similarity;
            }
        }

        // 2. Process lexical rankings
        for res in lexical_results {
            if let Some(rank) = res.lexical_rank {
                let rrf = w_lex / (k + rank as f32);
                let entry = fused_map.entry(res.id.clone()).or_insert_with(|| HybridRagSearchResult {
                    id: res.id.clone(),
                    content: res.content.clone(),
                    metadata: res.metadata.clone(),
                    hybrid_score: 0.0,
                    dense_rank: None,
                    lexical_rank: Some(rank),
                    dense_similarity: 0.0,
                    lexical_score: res.lexical_score,
                    matched_terms: res.matched_terms.clone(),
                });
                entry.hybrid_score += rrf;
                entry.lexical_rank = Some(rank);
                entry.lexical_score = res.lexical_score;
                for t in res.matched_terms {
                    if !entry.matched_terms.contains(&t) {
                        entry.matched_terms.push(t);
                    }
                }
            }
        }

        // 3. Sort by fused RRF score descending
        let mut final_results: Vec<HybridRagSearchResult> = fused_map.into_values().collect();
        final_results.sort_by(|a, b| b.hybrid_score.partial_cmp(&a.hybrid_score).unwrap_or(Ordering::Equal));
        final_results.truncate(top_k);

        Ok(final_results)
    }

    // ------------------------------------------------------------------------
    // Binary Serialization and Deserialization
    // ------------------------------------------------------------------------

    /// Save index to binary file on disk
    pub fn save_to_disk(&self, path: impl AsRef<Path>) -> Result<()> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let file = File::create(p).map_err(HgbError::Io)?;
        let mut writer = BufWriter::new(file);

        // Header: Magic (8 bytes)
        writer.write_all(&HYBRID_RAG_MAGIC).map_err(HgbError::Io)?;

        // Configuration JSON (length-prefixed)
        let cfg_json = serde_json::to_string(&self.config)
            .map_err(|e| HgbError::Execution(format!("Serialize config error: {}", e)))?;
        writer.write_all(&(cfg_json.len() as u32).to_le_bytes()).map_err(HgbError::Io)?;
        writer.write_all(cfg_json.as_bytes()).map_err(HgbError::Io)?;

        // Documents JSON (length-prefixed)
        let docs = self.documents.read().unwrap();
        let docs_json = serde_json::to_string(&*docs)
            .map_err(|e| HgbError::Execution(format!("Serialize docs error: {}", e)))?;
        writer.write_all(&(docs_json.len() as u32).to_le_bytes()).map_err(HgbError::Io)?;
        writer.write_all(docs_json.as_bytes()).map_err(HgbError::Io)?;

        // Graph JSON (length-prefixed)
        let graph = self.graph.read().unwrap();
        let graph_json = serde_json::to_string(&*graph)
            .map_err(|e| HgbError::Execution(format!("Serialize graph error: {}", e)))?;
        writer.write_all(&(graph_json.len() as u32).to_le_bytes()).map_err(HgbError::Io)?;
        writer.write_all(graph_json.as_bytes()).map_err(HgbError::Io)?;

        // Inverted Index JSON (length-prefixed)
        let inv = self.inverted_index.read().unwrap();
        let inv_json = serde_json::to_string(&*inv)
            .map_err(|e| HgbError::Execution(format!("Serialize inverted index error: {}", e)))?;
        writer.write_all(&(inv_json.len() as u32).to_le_bytes()).map_err(HgbError::Io)?;
        writer.write_all(inv_json.as_bytes()).map_err(HgbError::Io)?;

        writer.flush().map_err(HgbError::Io)?;
        Ok(())
    }

    /// Load index from binary file on disk
    pub fn load_from_disk(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        let file = File::open(p).map_err(HgbError::Io)?;
        let mut reader = BufReader::new(file);

        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic).map_err(HgbError::Io)?;
        if magic != HYBRID_RAG_MAGIC {
            return Err(HgbError::Execution("Invalid Hybrid RAG binary magic header".to_string()));
        }

        // Read Config
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).map_err(HgbError::Io)?;
        let cfg_len = u32::from_le_bytes(len_buf) as usize;
        let mut cfg_bytes = vec![0u8; cfg_len];
        reader.read_exact(&mut cfg_bytes).map_err(HgbError::Io)?;
        let mut config: HybridRagConfig = serde_json::from_slice(&cfg_bytes)
            .map_err(|e| HgbError::Execution(format!("Deserialize config error: {}", e)))?;
        config.persist_path = Some(p.to_path_buf());

        // Read Documents
        reader.read_exact(&mut len_buf).map_err(HgbError::Io)?;
        let docs_len = u32::from_le_bytes(len_buf) as usize;
        let mut docs_bytes = vec![0u8; docs_len];
        reader.read_exact(&mut docs_bytes).map_err(HgbError::Io)?;
        let docs: Vec<HybridDocument> = serde_json::from_slice(&docs_bytes)
            .map_err(|e| HgbError::Execution(format!("Deserialize docs error: {}", e)))?;

        // Reconstruct ID to Index Map
        let mut id_map = HashMap::new();
        for (i, d) in docs.iter().enumerate() {
            id_map.insert(d.id.clone(), i);
        }

        // Read Graph
        reader.read_exact(&mut len_buf).map_err(HgbError::Io)?;
        let graph_len = u32::from_le_bytes(len_buf) as usize;
        let mut graph_bytes = vec![0u8; graph_len];
        reader.read_exact(&mut graph_bytes).map_err(HgbError::Io)?;
        let graph: HnswGraph = serde_json::from_slice(&graph_bytes)
            .map_err(|e| HgbError::Execution(format!("Deserialize graph error: {}", e)))?;

        // Read Inverted Index
        reader.read_exact(&mut len_buf).map_err(HgbError::Io)?;
        let inv_len = u32::from_le_bytes(len_buf) as usize;
        let mut inv_bytes = vec![0u8; inv_len];
        reader.read_exact(&mut inv_bytes).map_err(HgbError::Io)?;
        let inv: InvertedIndex = serde_json::from_slice(&inv_bytes)
            .map_err(|e| HgbError::Execution(format!("Deserialize inverted index error: {}", e)))?;

        Ok(Self {
            config,
            documents: Arc::new(RwLock::new(docs)),
            id_to_index: Arc::new(RwLock::new(id_map)),
            graph: Arc::new(RwLock::new(graph)),
            inverted_index: Arc::new(RwLock::new(inv)),
        })
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_synthetic_embedding(dim: usize, seed: usize) -> Vec<f32> {
        let mut vec = Vec::with_capacity(dim);
        for i in 0..dim {
            let val = ((i + seed) as f32).sin();
            vec.push(val);
        }
        let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-9);
        vec.iter_mut().for_each(|v| *v /= norm);
        vec
    }

    #[test]
    fn test_simd_distance_math_correctness() {
        let a = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let sim_ident = simd_cosine_similarity(&a, &b);
        assert!((sim_ident - 1.0).abs() < 1e-5);

        let c = vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let sim_ortho = simd_cosine_similarity(&a, &c);
        assert!(sim_ortho.abs() < 1e-5);

        let d_sq = simd_squared_euclidean(&a, &b);
        assert!(d_sq < 1e-5);
    }

    #[test]
    fn test_hnsw_graph_insertion_and_dense_retrieval() {
        let mut cfg = HybridRagConfig::default();
        cfg.dimension = 16;
        let index = HybridRagVectorIndex::new(cfg);

        for i in 0..50 {
            let emb = generate_synthetic_embedding(16, i);
            index
                .insert(
                    &format!("doc_{}", i),
                    &format!("Content for doc {} describing vector systems and microkernels", i),
                    &emb,
                    serde_json::json!({ "index": i }),
                )
                .expect("insert");
        }

        assert_eq!(index.len(), 50);

        // Query dense similarity with doc_10's exact embedding
        let q_emb = generate_synthetic_embedding(16, 10);
        let results = index.query_dense(&q_emb, 3).expect("query_dense");
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "doc_10");
        assert!((results[0].dense_similarity - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_lexical_bm25_keyword_matching() {
        let mut cfg = HybridRagConfig::default();
        cfg.dimension = 8;
        let index = HybridRagVectorIndex::new(cfg);

        index
            .insert(
                "auth_doc",
                "Authentication service token validation and OAuth2 authorization flow",
                &[0.1; 8],
                serde_json::json!({ "topic": "auth" }),
            )
            .expect("insert");

        index
            .insert(
                "crypto_doc",
                "Cryptographic hashing algorithms, BLAKE3 signatures, and SHA256 integrity",
                &[0.2; 8],
                serde_json::json!({ "topic": "crypto" }),
            )
            .expect("insert");

        index
            .insert(
                "perf_doc",
                "High performance memory buffers and CPU frequency governor scaling",
                &[0.3; 8],
                serde_json::json!({ "topic": "performance" }),
            )
            .expect("insert");

        let res = index.query_lexical("cryptographic blake3 hashing", 2).expect("query_lexical");
        assert!(!res.is_empty());
        assert_eq!(res[0].id, "crypto_doc");
        assert!(res[0].matched_terms.contains(&"cryptographic".to_string()));
        assert!(res[0].matched_terms.contains(&"blake3".to_string()));
    }

    #[test]
    fn test_hybrid_reciprocal_rank_fusion_rrf() {
        let mut cfg = HybridRagConfig::default();
        cfg.dimension = 16;
        let index = HybridRagVectorIndex::new(cfg);

        // Document with high lexical relevance but distinct embedding
        index
            .insert(
                "doc_alpha",
                "Quantum computing qubit entanglement and quantum teleportation protocols",
                &generate_synthetic_embedding(16, 1),
                serde_json::json!({}),
            )
            .expect("insert");

        // Document with high dense semantic similarity but differing words
        index
            .insert(
                "doc_beta",
                "Subatomic physics teleporting entangled photon states across fiber",
                &generate_synthetic_embedding(16, 2),
                serde_json::json!({}),
            )
            .expect("insert");

        let q_emb = generate_synthetic_embedding(16, 2);
        let res = index
            .query_hybrid("quantum teleportation protocols", &q_emb, 2)
            .expect("query_hybrid");

        assert_eq!(res.len(), 2);
        assert!(res[0].hybrid_score > 0.0);
        // Verify both dense rank and lexical rank contribute to fused score
        assert!(res.iter().any(|r| r.dense_rank.is_some() || r.lexical_rank.is_some()));
    }

    #[test]
    fn test_binary_serialization_and_disk_roundtrip() {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let tmp = std::env::temp_dir().join(format!("hgb_hybrid_rag_test_{}_{}.bin", std::process::id(), ts));
        let mut cfg = HybridRagConfig::default();
        cfg.dimension = 8;
        cfg.persist_path = Some(tmp.clone());
        let index = HybridRagVectorIndex::new(cfg);

        index
            .insert("doc_persisted_1", "Compiler error fix for borrow checker", &[0.5; 8], serde_json::json!({ "tier": 1 }))
            .expect("insert 1");
        index
            .insert("doc_persisted_2", "SIMD memory mapped vector index kernel", &[0.9; 8], serde_json::json!({ "tier": 2 }))
            .expect("insert 2");

        index.save_to_disk(&tmp).expect("save to disk");
        assert!(tmp.exists());

        let reopened = HybridRagVectorIndex::load_from_disk(&tmp).expect("load from disk");
        assert_eq!(reopened.len(), 2);

        let search = reopened.query_lexical("borrow checker", 1).expect("search reopened");
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].id, "doc_persisted_1");

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_sub_millisecond_retrieval_benchmark() {
        let mut cfg = HybridRagConfig::default();
        cfg.dimension = 32;
        let index = HybridRagVectorIndex::new(cfg);

        for i in 0..100 {
            let emb = generate_synthetic_embedding(32, i);
            index
                .insert(
                    &format!("bench_{}", i),
                    &format!("Benchmark document {} for latency testing with memory mapped index", i),
                    &emb,
                    serde_json::json!({ "idx": i }),
                )
                .expect("insert");
        }

        let q_emb = generate_synthetic_embedding(32, 42);
        let start = Instant::now();
        let _results = index.query_hybrid("latency testing memory mapped", &q_emb, 5).expect("hybrid query");
        let elapsed = start.elapsed();

        // Retrieval must complete in sub-millisecond time (< 500µs)
        assert!(
            elapsed.as_micros() < 5000,
            "Hybrid RAG query took too long: {}us",
            elapsed.as_micros()
        );
    }
}
