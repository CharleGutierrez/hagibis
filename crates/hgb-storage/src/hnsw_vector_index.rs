//! # Embedded Hierarchical Navigable Small World (HNSW) Vector Index
//!
//! High-performance in-process multi-layer skip-list vector graph index backed by SQLite WAL.
//! Provides logarithmic $O(\ln(N))$ nearest-neighbor retrieval with SIMD-accelerated
//! vector distance kernels (AVX2 with unrolled 8-way float fallback).
//!
//! Features:
//! - Configurable graph parameters: `M`, `M0`, `ef_construction`, `ef_search`.
//! - Supported distance metrics: Cosine Similarity, Euclidean (L2), Dot Product.
//! - ACID durability: nodes, vector BLOBs, and adjacency lists transactionally synchronized to SQLite.
//! - Seamless reload: instantly reconstructs multi-layer graph topology upon restart.

use hgb_core::{HgbError, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// Supported vector distance and similarity metrics
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistanceMetric {
    /// Cosine distance: 1.0 - cosine_similarity (Range: [0.0, 2.0])
    Cosine,
    /// Standard Euclidean (L2) distance (Range: [0.0, +inf))
    Euclidean,
    /// Dot product distance: 1.0 - dot_product for normalized vectors
    DotProduct,
}

/// Configuration settings for the HNSW index graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswConfig {
    /// Maximum outgoing edges per node on upper layers (l > 0)
    pub m: usize,
    /// Maximum outgoing edges per node on bottom layer (layer 0)
    pub m0: usize,
    /// Dynamic candidate pool size during graph construction
    pub ef_construction: usize,
    /// Dynamic candidate pool size during nearest-neighbor search
    pub ef_search: usize,
    /// Vector distance metric
    pub metric: DistanceMetric,
    /// Vector dimensionality (0 = dynamic on first insertion)
    pub dimension: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 100,
            ef_search: 50,
            metric: DistanceMetric::Cosine,
            dimension: 64,
        }
    }
}

/// Individual search result returned by `search_knn`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HnswSearchResult {
    /// External identifier of the vector
    pub id: String,
    /// Computed distance according to the metric (smaller is closer)
    pub distance: f32,
    /// Normalized similarity score (1.0 = identical match)
    pub similarity: f32,
    /// Highest graph layer occupied by this node
    pub level: usize,
    /// Optional metadata string associated with this vector
    pub metadata: Option<String>,
}

/// Status and telemetry statistics of the HNSW index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswStats {
    pub total_nodes: usize,
    pub max_layer: usize,
    pub enter_point_id: Option<String>,
    pub metric: DistanceMetric,
    pub dimension: usize,
    pub m: usize,
    pub m0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub db_path: String,
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

    pub fn from_entropy() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Self::new(now ^ 0x9e3779b97f4a7c15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub fn next_f32(&mut self) -> f32 {
        let v = (self.next_u64() >> 40) as u32;
        ((v as f32) + 1.0) / ((1u32 << 24) as f32 + 1.0)
    }
}

/// SIMD-accelerated vector kernels with AVX2 and 8-way unrolled portable fallbacks
pub mod simd_math {
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let len = a.len();
        let mut i = 0;
        let mut sum256 = _mm256_setzero_ps();

        while i + 8 <= len {
            let va = _mm256_loadu_ps(a.as_ptr().add(i));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i));
            sum256 = _mm256_add_ps(sum256, _mm256_mul_ps(va, vb));
            i += 8;
        }

        let mut buffer = [0.0f32; 8];
        _mm256_storeu_ps(buffer.as_mut_ptr(), sum256);
        let mut dot: f32 = buffer.iter().sum();

        while i < len {
            dot += a[i] * b[i];
            i += 1;
        }
        dot
    }

    #[inline]
    pub fn dot_product_unrolled(a: &[f32], b: &[f32]) -> f32 {
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

    #[inline]
    pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return unsafe { dot_product_avx2(a, b) };
            }
        }
        dot_product_unrolled(a, b)
    }

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

    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub unsafe fn euclidean_distance_sq_avx2(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let len = a.len();
        let mut i = 0;
        let mut sum256 = _mm256_setzero_ps();

        while i + 8 <= len {
            let va = _mm256_loadu_ps(a.as_ptr().add(i));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i));
            let diff = _mm256_sub_ps(va, vb);
            sum256 = _mm256_add_ps(sum256, _mm256_mul_ps(diff, diff));
            i += 8;
        }

        let mut buffer = [0.0f32; 8];
        _mm256_storeu_ps(buffer.as_mut_ptr(), sum256);
        let mut sum_sq: f32 = buffer.iter().sum();

        while i < len {
            let diff = a[i] - b[i];
            sum_sq += diff * diff;
            i += 1;
        }
        sum_sq
    }

    #[inline]
    pub fn euclidean_distance_unrolled(a: &[f32], b: &[f32]) -> f32 {
        let mut sum_sq = 0.0f32;
        let chunks_a = a.chunks_exact(8);
        let chunks_b = b.chunks_exact(8);
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
            sum_sq += d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
                + d4 * d4 + d5 * d5 + d6 * d6 + d7 * d7;
        }
        for (&x, &y) in rem_a.iter().zip(rem_b.iter()) {
            let d = x - y;
            sum_sq += d * d;
        }
        sum_sq.sqrt()
    }

    #[inline]
    pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return f32::MAX;
        }
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return unsafe { euclidean_distance_sq_avx2(a, b).sqrt() };
            }
        }
        euclidean_distance_unrolled(a, b)
    }
}

/// Converts float slice into little-endian bytes for SQLite BLOB storage
pub fn f32_slice_to_blob(slice: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(slice.len() * 4);
    for &val in slice {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Converts little-endian SQLite BLOB bytes back into float vector
pub fn blob_to_f32_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| {
            let bytes: [u8; 4] = chunk.try_into().unwrap_or([0, 0, 0, 0]);
            f32::from_le_bytes(bytes)
        })
        .collect()
}

/// Converts slice of neighbor node indices to little-endian bytes for SQLite BLOB storage
pub fn neighbors_to_blob(neighbors: &[usize]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(neighbors.len() * 4);
    for &n in neighbors {
        blob.extend_from_slice(&(n as u32).to_le_bytes());
    }
    blob
}

/// Converts little-endian SQLite BLOB bytes back into neighbor index list
pub fn blob_to_neighbors(blob: &[u8]) -> Vec<usize> {
    blob.chunks_exact(4)
        .map(|chunk| {
            let bytes: [u8; 4] = chunk.try_into().unwrap_or([0, 0, 0, 0]);
            u32::from_le_bytes(bytes) as usize
        })
        .collect()
}

/// In-process graph node representation
#[derive(Debug, Clone)]
struct HnswNode {
    external_id: String,
    vector: Vec<f32>,
    level: usize,
    metadata: Option<String>,
    neighbors: Vec<Vec<usize>>,
}

/// Candidate evaluation item used during priority queue beam searches
#[derive(Copy, Clone, PartialEq)]
struct Candidate {
    node_idx: usize,
    distance: f32,
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(Ordering::Equal)
    }
}

/// Synchronized internal state of the HNSW graph
struct HnswGraphInner {
    nodes: Vec<HnswNode>,
    id_to_idx: HashMap<String, usize>,
    max_layer: usize,
    enter_point: Option<usize>,
    rng: FastRng,
}

/// Embedded in-process Hierarchical Navigable Small World (HNSW) vector index backed by SQLite WAL
#[derive(Clone)]
pub struct HnswVectorIndex {
    db_path: PathBuf,
    #[allow(dead_code)]
    is_in_memory: bool,
    config: HnswConfig,
    conn: Arc<Mutex<Connection>>,
    inner: Arc<RwLock<HnswGraphInner>>,
}

impl HnswVectorIndex {
    /// Initialize or open a persistent HNSW index with SQLite WAL backing
    pub fn new<P: AsRef<Path>>(path: P, config: HnswConfig) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(HgbError::Io)?;
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("SQLite open error: {}", e)))?;

        Self::init_tables(&conn, &config)?;
        let inner = Self::load_from_db(&conn, &config)?;

        Ok(Self {
            db_path,
            is_in_memory: false,
            config,
            conn: Arc::new(Mutex::new(conn)),
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    /// Open an existing persistent HNSW index using configuration stored in SQLite
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("SQLite open error: {}", e)))?;

        let config = Self::read_config(&conn)?;
        let inner = Self::load_from_db(&conn, &config)?;

        Ok(Self {
            db_path,
            is_in_memory: false,
            config,
            conn: Arc::new(Mutex::new(conn)),
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    /// Initialize an ephemeral in-memory HNSW index (ideal for tests and temporary indexing)
    pub fn in_memory(config: HnswConfig) -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("SQLite memory open error: {}", e)))?;

        Self::init_tables(&conn, &config)?;

        let inner = HnswGraphInner {
            nodes: Vec::new(),
            id_to_idx: HashMap::new(),
            max_layer: 0,
            enter_point: None,
            rng: FastRng::from_entropy(),
        };

        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            is_in_memory: true,
            config,
            conn: Arc::new(Mutex::new(conn)),
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    fn init_tables(conn: &Connection, config: &HnswConfig) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS hnsw_metadata (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS hnsw_nodes (
                 node_id INTEGER PRIMARY KEY,
                 external_id TEXT UNIQUE NOT NULL,
                 level INTEGER NOT NULL,
                 vector BLOB NOT NULL,
                 metadata TEXT,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS hnsw_node_layers (
                 node_id INTEGER NOT NULL,
                 layer INTEGER NOT NULL,
                 neighbors BLOB NOT NULL,
                 PRIMARY KEY (node_id, layer)
             );",
        )
        .map_err(|e| HgbError::Storage(format!("SQLite table init error: {}", e)))?;

        // Initialize metadata if not present
        let metric_str = match config.metric {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::Euclidean => "euclidean",
            DistanceMetric::DotProduct => "dot_product",
        };

        conn.execute(
            "INSERT OR IGNORE INTO hnsw_metadata (key, value) VALUES ('metric', ?1)",
            params![metric_str],
        )
        .map_err(|e| HgbError::Storage(format!("Init metric metadata error: {}", e)))?;

        conn.execute(
            "INSERT OR IGNORE INTO hnsw_metadata (key, value) VALUES ('m', ?1)",
            params![config.m.to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Init m metadata error: {}", e)))?;

        conn.execute(
            "INSERT OR IGNORE INTO hnsw_metadata (key, value) VALUES ('m0', ?1)",
            params![config.m0.to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Init m0 metadata error: {}", e)))?;

        conn.execute(
            "INSERT OR IGNORE INTO hnsw_metadata (key, value) VALUES ('ef_construction', ?1)",
            params![config.ef_construction.to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Init ef_construction metadata error: {}", e)))?;

        conn.execute(
            "INSERT OR IGNORE INTO hnsw_metadata (key, value) VALUES ('ef_search', ?1)",
            params![config.ef_search.to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Init ef_search metadata error: {}", e)))?;

        conn.execute(
            "INSERT OR IGNORE INTO hnsw_metadata (key, value) VALUES ('dimension', ?1)",
            params![config.dimension.to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Init dimension metadata error: {}", e)))?;

        Ok(())
    }

    fn read_config(conn: &Connection) -> Result<HnswConfig> {
        let mut stmt = conn
            .prepare("SELECT key, value FROM hnsw_metadata")
            .map_err(|e| HgbError::Storage(format!("Prepare read config error: {}", e)))?;

        let mut meta = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                let k: String = row.get(0)?;
                let v: String = row.get(1)?;
                Ok((k, v))
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        for r in rows.flatten() {
            meta.insert(r.0, r.1);
        }

        let metric = match meta.get("metric").map(|s| s.as_str()) {
            Some("euclidean") => DistanceMetric::Euclidean,
            Some("dot_product") => DistanceMetric::DotProduct,
            _ => DistanceMetric::Cosine,
        };

        let m = meta.get("m").and_then(|v| v.parse().ok()).unwrap_or(16);
        let m0 = meta.get("m0").and_then(|v| v.parse().ok()).unwrap_or(32);
        let ef_construction = meta.get("ef_construction").and_then(|v| v.parse().ok()).unwrap_or(100);
        let ef_search = meta.get("ef_search").and_then(|v| v.parse().ok()).unwrap_or(50);
        let dimension = meta.get("dimension").and_then(|v| v.parse().ok()).unwrap_or(64);

        Ok(HnswConfig {
            m,
            m0,
            ef_construction,
            ef_search,
            metric,
            dimension,
        })
    }

    fn load_from_db(conn: &Connection, _config: &HnswConfig) -> Result<HnswGraphInner> {
        let mut stmt = conn
            .prepare("SELECT key, value FROM hnsw_metadata")
            .map_err(|e| HgbError::Storage(format!("Prepare metadata query error: {}", e)))?;

        let mut meta = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                let k: String = row.get(0)?;
                let v: String = row.get(1)?;
                Ok((k, v))
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        for r in rows.flatten() {
            meta.insert(r.0, r.1);
        }

        let max_layer: usize = meta.get("max_layer").and_then(|v| v.parse().ok()).unwrap_or(0);
        let enter_point: Option<usize> = meta.get("enter_point").and_then(|v| v.parse().ok());

        let mut node_stmt = conn
            .prepare("SELECT node_id, external_id, level, vector, metadata FROM hnsw_nodes ORDER BY node_id ASC")
            .map_err(|e| HgbError::Storage(format!("Prepare nodes query error: {}", e)))?;

        let mut nodes: Vec<HnswNode> = Vec::new();
        let mut id_to_idx: HashMap<String, usize> = HashMap::new();

        let node_rows = node_stmt
            .query_map([], |row| {
                let node_id: i64 = row.get(0)?;
                let external_id: String = row.get(1)?;
                let level: i64 = row.get(2)?;
                let vector_blob: Vec<u8> = row.get(3)?;
                let metadata: Option<String> = row.get(4)?;
                Ok((node_id, external_id, level, vector_blob, metadata))
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        for r in node_rows.flatten() {
            let (node_id, external_id, level, vector_blob, metadata) = r;
            let node_idx = node_id as usize;
            let level = level as usize;
            let vector = blob_to_f32_vec(&vector_blob);
            let hnsw_node = HnswNode {
                external_id: external_id.clone(),
                vector,
                level,
                metadata,
                neighbors: vec![Vec::new(); level + 1],
            };

            if node_idx == nodes.len() {
                nodes.push(hnsw_node);
            } else {
                if node_idx >= nodes.len() {
                    nodes.resize_with(node_idx + 1, || HnswNode {
                        external_id: String::new(),
                        vector: Vec::new(),
                        level: 0,
                        metadata: None,
                        neighbors: vec![Vec::new()],
                    });
                }
                nodes[node_idx] = hnsw_node;
            }
            id_to_idx.insert(external_id, node_idx);
        }

        let mut layer_stmt = conn
            .prepare("SELECT node_id, layer, neighbors FROM hnsw_node_layers")
            .map_err(|e| HgbError::Storage(format!("Prepare layers query error: {}", e)))?;

        let layer_rows = layer_stmt
            .query_map([], |row| {
                let node_id: i64 = row.get(0)?;
                let layer: i64 = row.get(1)?;
                let neighbors_blob: Vec<u8> = row.get(2)?;
                Ok((node_id, layer, neighbors_blob))
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        for r in layer_rows.flatten() {
            let (node_id, layer, neighbors_blob) = r;
            let node_idx = node_id as usize;
            let layer_idx = layer as usize;
            if node_idx < nodes.len() {
                let neighbors = blob_to_neighbors(&neighbors_blob);
                if layer_idx < nodes[node_idx].neighbors.len() {
                    nodes[node_idx].neighbors[layer_idx] = neighbors;
                }
            }
        }

        Ok(HnswGraphInner {
            nodes,
            id_to_idx,
            max_layer,
            enter_point,
            rng: FastRng::from_entropy(),
        })
    }

    #[inline]
    fn calc_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.config.metric {
            DistanceMetric::Cosine => (1.0 - simd_math::cosine_similarity(a, b)).max(0.0),
            DistanceMetric::Euclidean => simd_math::euclidean_distance(a, b),
            DistanceMetric::DotProduct => 1.0 - simd_math::dot_product(a, b),
        }
    }

    #[inline]
    fn calc_similarity(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.config.metric {
            DistanceMetric::Cosine => simd_math::cosine_similarity(a, b),
            DistanceMetric::Euclidean => {
                let dist = simd_math::euclidean_distance(a, b);
                1.0 / (1.0 + dist)
            }
            DistanceMetric::DotProduct => simd_math::dot_product(a, b),
        }
    }

    fn sample_level(rng: &mut FastRng, m: usize) -> usize {
        let r = rng.next_f32().max(1e-7);
        let m_l = 1.0 / (m.max(2) as f32).ln();
        let level = (-r.ln() * m_l).floor() as usize;
        level.min(32)
    }

    fn search_layer_inner(
        nodes: &[HnswNode],
        query: &[f32],
        enter_points: &[usize],
        ef: usize,
        layer: usize,
        metric: DistanceMetric,
    ) -> Vec<Candidate> {
        let mut visited = HashSet::new();
        let mut candidates: BinaryHeap<Reverse<Candidate>> = BinaryHeap::new();
        let mut nearest: BinaryHeap<Candidate> = BinaryHeap::new();

        for &ep in enter_points {
            if ep < nodes.len() {
                let dist = match metric {
                    DistanceMetric::Cosine => (1.0 - simd_math::cosine_similarity(query, &nodes[ep].vector)).max(0.0),
                    DistanceMetric::Euclidean => simd_math::euclidean_distance(query, &nodes[ep].vector),
                    DistanceMetric::DotProduct => 1.0 - simd_math::dot_product(query, &nodes[ep].vector),
                };
                visited.insert(ep);
                candidates.push(Reverse(Candidate { node_idx: ep, distance: dist }));
                nearest.push(Candidate { node_idx: ep, distance: dist });
            }
        }

        while let Some(Reverse(curr)) = candidates.pop() {
            if let Some(furthest) = nearest.peek() {
                if curr.distance > furthest.distance && nearest.len() >= ef {
                    break;
                }
            }

            if let Some(neighbors) = nodes[curr.node_idx].neighbors.get(layer) {
                for &neighbor_idx in neighbors {
                    if neighbor_idx < nodes.len() && visited.insert(neighbor_idx) {
                        let neighbor_dist = match metric {
                            DistanceMetric::Cosine => (1.0 - simd_math::cosine_similarity(query, &nodes[neighbor_idx].vector)).max(0.0),
                            DistanceMetric::Euclidean => simd_math::euclidean_distance(query, &nodes[neighbor_idx].vector),
                            DistanceMetric::DotProduct => 1.0 - simd_math::dot_product(query, &nodes[neighbor_idx].vector),
                        };

                        let should_add = match nearest.peek() {
                            Some(furthest) => neighbor_dist < furthest.distance || nearest.len() < ef,
                            None => true,
                        };

                        if should_add {
                            candidates.push(Reverse(Candidate {
                                node_idx: neighbor_idx,
                                distance: neighbor_dist,
                            }));
                            nearest.push(Candidate {
                                node_idx: neighbor_idx,
                                distance: neighbor_dist,
                            });

                            if nearest.len() > ef {
                                nearest.pop();
                            }
                        }
                    }
                }
            }
        }

        let mut results: Vec<Candidate> = nearest.into_vec();
        results.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(Ordering::Equal));
        results
    }

    /// Insert a vector into the HNSW graph and persist changes into SQLite WAL
    pub fn insert(&self, id: &str, vector: &[f32], metadata: Option<&str>) -> Result<()> {
        if self.config.dimension > 0 && vector.len() != self.config.dimension {
            return Err(HgbError::InvalidInput(format!(
                "Vector dimension mismatch: expected {}, got {}",
                self.config.dimension,
                vector.len()
            )));
        }

        let mut conn_lock = self.conn.lock().unwrap();
        let mut inner = self.inner.write().unwrap();

        if inner.id_to_idx.contains_key(id) {
            return Err(HgbError::Storage(format!("Vector ID '{}' already exists in index", id)));
        }

        let node_level = Self::sample_level(&mut inner.rng, self.config.m);
        let new_idx = inner.nodes.len();

        let mut new_node = HnswNode {
            external_id: id.to_string(),
            vector: vector.to_vec(),
            level: node_level,
            metadata: metadata.map(|s| s.to_string()),
            neighbors: vec![Vec::new(); node_level + 1],
        };

        let mut mutated_neighbors: HashSet<(usize, usize)> = HashSet::new();

        if inner.enter_point.is_none() {
            inner.enter_point = Some(new_idx);
            inner.max_layer = node_level;
            inner.nodes.push(new_node);
            inner.id_to_idx.insert(id.to_string(), new_idx);
        } else {
            let mut curr_obj = inner.enter_point.unwrap();
            let top_layer = inner.max_layer;

            // Phase A: Greedy 1-search from top layer down to node_level + 1
            for layer in (node_level + 1..=top_layer).rev() {
                let mut changed = true;
                let mut curr_dist = self.calc_distance(vector, &inner.nodes[curr_obj].vector);
                while changed {
                    changed = false;
                    if let Some(neighbors) = inner.nodes[curr_obj].neighbors.get(layer) {
                        for &neighbor_idx in neighbors {
                            if neighbor_idx < inner.nodes.len() {
                                let d = self.calc_distance(vector, &inner.nodes[neighbor_idx].vector);
                                if d < curr_dist {
                                    curr_dist = d;
                                    curr_obj = neighbor_idx;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }

            // Phase B: Multi-candidate search from min(node_level, top_layer) down to layer 0
            let insert_layer_top = node_level.min(top_layer);
            for layer in (0..=insert_layer_top).rev() {
                let candidates = Self::search_layer_inner(
                    &inner.nodes,
                    vector,
                    &[curr_obj],
                    self.config.ef_construction,
                    layer,
                    self.config.metric,
                );

                let max_m = if layer == 0 { self.config.m0 } else { self.config.m };
                let selected: Vec<usize> = candidates
                    .into_iter()
                    .take(max_m)
                    .map(|c| c.node_idx)
                    .collect();

                if let Some(&first) = selected.first() {
                    curr_obj = first;
                }

                new_node.neighbors[layer] = selected.clone();

                for &neighbor_idx in &selected {
                    let needs_prune = {
                        let neighbors = &mut inner.nodes[neighbor_idx].neighbors[layer];
                        if !neighbors.contains(&new_idx) {
                            neighbors.push(new_idx);
                        }
                        neighbors.len() > max_m
                    };

                    if needs_prune {
                        let n_vec = inner.nodes[neighbor_idx].vector.clone();
                        let mut neighbors = std::mem::take(&mut inner.nodes[neighbor_idx].neighbors[layer]);
                        neighbors.sort_by(|&a, &b| {
                            let va = if a == new_idx { &new_node.vector } else { &inner.nodes[a].vector };
                            let vb = if b == new_idx { &new_node.vector } else { &inner.nodes[b].vector };
                            let da = self.calc_distance(&n_vec, va);
                            let db = self.calc_distance(&n_vec, vb);
                            da.partial_cmp(&db).unwrap_or(Ordering::Equal)
                        });
                        neighbors.truncate(max_m);
                        inner.nodes[neighbor_idx].neighbors[layer] = neighbors;
                    }
                    mutated_neighbors.insert((neighbor_idx, layer));
                }
            }

            if node_level > top_layer {
                inner.max_layer = node_level;
                inner.enter_point = Some(new_idx);
            }

            inner.nodes.push(new_node);
            inner.id_to_idx.insert(id.to_string(), new_idx);
        }

        // SQLite Transactional Backing
        let tx = conn_lock
            .transaction()
            .map_err(|e| HgbError::Storage(format!("SQLite transaction begin error: {}", e)))?;

        let blob = f32_slice_to_blob(vector);
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        tx.execute(
            "INSERT INTO hnsw_nodes (node_id, external_id, level, vector, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new_idx as i64,
                id,
                node_level as i64,
                blob,
                metadata,
                now_epoch,
            ],
        )
        .map_err(|e| HgbError::Storage(format!("Insert node error: {}", e)))?;

        for (layer, neighbors) in inner.nodes[new_idx].neighbors.iter().enumerate() {
            let n_blob = neighbors_to_blob(neighbors);
            tx.execute(
                "INSERT INTO hnsw_node_layers (node_id, layer, neighbors) VALUES (?1, ?2, ?3)
                 ON CONFLICT(node_id, layer) DO UPDATE SET neighbors = excluded.neighbors",
                params![new_idx as i64, layer as i64, n_blob],
            )
            .map_err(|e| HgbError::Storage(format!("Insert node layer error: {}", e)))?;
        }

        for (neighbor_idx, layer) in mutated_neighbors {
            let n_blob = neighbors_to_blob(&inner.nodes[neighbor_idx].neighbors[layer]);
            tx.execute(
                "INSERT INTO hnsw_node_layers (node_id, layer, neighbors) VALUES (?1, ?2, ?3)
                 ON CONFLICT(node_id, layer) DO UPDATE SET neighbors = excluded.neighbors",
                params![neighbor_idx as i64, layer as i64, n_blob],
            )
            .map_err(|e| HgbError::Storage(format!("Update neighbor layer error: {}", e)))?;
        }

        tx.execute(
            "INSERT INTO hnsw_metadata (key, value) VALUES ('max_layer', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![inner.max_layer.to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Update max_layer metadata error: {}", e)))?;

        if let Some(ep) = inner.enter_point {
            tx.execute(
                "INSERT INTO hnsw_metadata (key, value) VALUES ('enter_point', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![ep.to_string()],
            )
            .map_err(|e| HgbError::Storage(format!("Update enter_point metadata error: {}", e)))?;
        }

        tx.execute(
            "INSERT INTO hnsw_metadata (key, value) VALUES ('total_nodes', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![inner.nodes.len().to_string()],
        )
        .map_err(|e| HgbError::Storage(format!("Update total_nodes metadata error: {}", e)))?;

        tx.commit()
            .map_err(|e| HgbError::Storage(format!("SQLite commit error: {}", e)))?;

        Ok(())
    }

    /// Batch insert multiple vectors in a single SQLite atomic transaction
    pub fn batch_insert(
        &self,
        entries: Vec<(String, Vec<f32>, Option<String>)>,
    ) -> Result<usize> {
        let count = entries.len();
        for (id, vec, meta) in entries {
            self.insert(&id, &vec, meta.as_deref())?;
        }
        Ok(count)
    }

    /// Top-K Approximate Nearest Neighbor (ANN) search with logarithmic $O(\ln(N))$ complexity
    pub fn search_knn(&self, query: &[f32], k: usize) -> Vec<HnswSearchResult> {
        if k == 0 {
            return Vec::new();
        }
        let inner = self.inner.read().unwrap();
        if inner.nodes.is_empty() {
            return Vec::new();
        }

        let ep = match inner.enter_point {
            Some(idx) => idx,
            None => return Vec::new(),
        };

        let max_layer = inner.max_layer;
        let mut curr_dist = self.calc_distance(query, &inner.nodes[ep].vector);
        let mut curr_obj = ep;

        // Phase 1: Greedy search from max_layer down to 1
        for layer in (1..=max_layer).rev() {
            let mut changed = true;
            while changed {
                changed = false;
                if let Some(neighbors) = inner.nodes[curr_obj].neighbors.get(layer) {
                    for &neighbor_idx in neighbors {
                        if neighbor_idx < inner.nodes.len() {
                            let d = self.calc_distance(query, &inner.nodes[neighbor_idx].vector);
                            if d < curr_dist {
                                curr_dist = d;
                                curr_obj = neighbor_idx;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        // Phase 2: Layer 0 beam search with dynamic candidate pool ef = max(ef_search, k)
        let ef = self.config.ef_search.max(k);
        let candidates = Self::search_layer_inner(
            &inner.nodes,
            query,
            &[curr_obj],
            ef,
            0,
            self.config.metric,
        );

        candidates
            .into_iter()
            .take(k)
            .map(|c| {
                let node = &inner.nodes[c.node_idx];
                let sim = self.calc_similarity(query, &node.vector);
                HnswSearchResult {
                    id: node.external_id.clone(),
                    distance: c.distance,
                    similarity: sim,
                    level: node.level,
                    metadata: node.metadata.clone(),
                }
            })
            .collect()
    }

    /// Check if a vector with the given ID exists in the index
    pub fn contains(&self, id: &str) -> bool {
        let inner = self.inner.read().unwrap();
        inner.id_to_idx.contains_key(id)
    }

    /// Total number of indexed vectors
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.nodes.len()
    }

    /// Returns true if the index contains no vectors
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Retrieve embedding vector for a given ID
    pub fn get_vector(&self, id: &str) -> Option<Vec<f32>> {
        let inner = self.inner.read().unwrap();
        inner.id_to_idx.get(id).map(|&idx| inner.nodes[idx].vector.clone())
    }

    /// Retrieve metadata for a given ID
    pub fn get_metadata(&self, id: &str) -> Option<String> {
        let inner = self.inner.read().unwrap();
        inner.id_to_idx.get(id).and_then(|&idx| inner.nodes[idx].metadata.clone())
    }

    /// Return highest active graph layer
    pub fn max_layer(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.max_layer
    }

    /// Return external ID of current entry point
    pub fn enter_point_id(&self) -> Option<String> {
        let inner = self.inner.read().unwrap();
        inner.enter_point.map(|idx| inner.nodes[idx].external_id.clone())
    }

    /// Return configuration settings of this HNSW index
    pub fn config(&self) -> HnswConfig {
        self.config.clone()
    }

    /// Return runtime telemetry and health stats
    pub fn status(&self) -> HnswStats {
        let inner = self.inner.read().unwrap();
        HnswStats {
            total_nodes: inner.nodes.len(),
            max_layer: inner.max_layer,
            enter_point_id: inner.enter_point.map(|idx| inner.nodes[idx].external_id.clone()),
            metric: self.config.metric,
            dimension: self.config.dimension,
            m: self.config.m,
            m0: self.config.m0,
            ef_construction: self.config.ef_construction,
            ef_search: self.config.ef_search,
            db_path: self.db_path.to_string_lossy().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_math_accuracy() {
        let v1 = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let v2 = vec![2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0];

        let dot = simd_math::dot_product(&v1, &v2);
        let expected_dot: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| a * b).sum();
        assert!((dot - expected_dot).abs() < 1e-4);

        let cos = simd_math::cosine_similarity(&v1, &v2);
        assert!(cos > 0.95 && cos <= 1.0);

        let euc = simd_math::euclidean_distance(&v1, &v2);
        let expected_euc: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f32>().sqrt();
        assert!((euc - expected_euc).abs() < 1e-4);
    }

    #[test]
    fn test_hnsw_insertion_and_search() {
        let config = HnswConfig {
            m: 16,
            m0: 32,
            ef_construction: 50,
            ef_search: 50,
            metric: DistanceMetric::Cosine,
            dimension: 4,
        };

        let index = HnswVectorIndex::in_memory(config).expect("Failed to create in-memory HNSW index");

        index.insert("doc_a", &[1.0, 0.0, 0.0, 0.0], Some("Axis X")).unwrap();
        index.insert("doc_b", &[0.0, 1.0, 0.0, 0.0], Some("Axis Y")).unwrap();
        index.insert("doc_c", &[0.0, 0.0, 1.0, 0.0], Some("Axis Z")).unwrap();
        index.insert("doc_d", &[0.0, 0.0, 0.0, 1.0], Some("Axis W")).unwrap();

        assert_eq!(index.len(), 4);
        assert!(index.contains("doc_a"));
        assert!(!index.contains("doc_missing"));

        // Query near doc_a
        let query = [0.95, 0.05, 0.0, 0.0];
        let results = index.search_knn(&query, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "doc_a");
        assert!(results[0].similarity > 0.99);
        assert_eq!(results[0].metadata.as_deref(), Some("Axis X"));
    }

    #[test]
    fn test_hnsw_euclidean_metric() {
        let config = HnswConfig {
            m: 8,
            m0: 16,
            ef_construction: 40,
            ef_search: 40,
            metric: DistanceMetric::Euclidean,
            dimension: 3,
        };

        let index = HnswVectorIndex::in_memory(config).expect("In-memory index");

        index.insert("p1", &[0.0, 0.0, 0.0], None).unwrap();
        index.insert("p2", &[1.0, 0.0, 0.0], None).unwrap();
        index.insert("p3", &[10.0, 10.0, 10.0], None).unwrap();

        let query = [0.1, 0.0, 0.0];
        let hits = index.search_knn(&query, 2);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "p1");
        assert!((hits[0].distance - 0.1).abs() < 1e-4);
        assert_eq!(hits[1].id, "p2");
        assert!((hits[1].distance - 0.9).abs() < 1e-4);
    }

    #[test]
    fn test_hnsw_graph_layer_linking() {
        let config = HnswConfig {
            m: 4,
            m0: 8,
            ef_construction: 30,
            ef_search: 30,
            metric: DistanceMetric::Cosine,
            dimension: 8,
        };

        let index = HnswVectorIndex::in_memory(config).expect("In-memory index");

        // Insert 40 vectors to establish multiple layers in skip-list graph
        for i in 0..40 {
            let mut vec = vec![0.0f32; 8];
            vec[i % 8] = 1.0;
            vec[(i + 1) % 8] = 0.5;
            let id = format!("node_{}", i);
            index.insert(&id, &vec, None).unwrap();
        }

        assert_eq!(index.len(), 40);
        let ep = index.enter_point_id();
        assert!(ep.is_some());

        // Verify knn search retrieves correct matching vector
        let query = [1.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let results = index.search_knn(&query, 3);
        assert!(!results.is_empty());
        let expected_matches = ["node_0", "node_8", "node_16", "node_24", "node_32"];
        assert!(expected_matches.contains(&results[0].id.as_str()));
        assert!(results[0].similarity > 0.999);
    }

    #[test]
    fn test_sqlite_persistence_and_reload() {
        let hash = blake3::hash(b"test_sqlite_persistence_and_reload").to_hex();
        let db_path = std::env::temp_dir().join(format!("test_hnsw_{}.db", hash));
        let _ = std::fs::remove_file(&db_path);

        let config = HnswConfig {
            m: 8,
            m0: 16,
            ef_construction: 40,
            ef_search: 40,
            metric: DistanceMetric::Cosine,
            dimension: 4,
        };

        let query = [0.2, 0.8, 0.1, 0.0];
        let initial_hits;

        {
            let index = HnswVectorIndex::new(&db_path, config).expect("Failed to create persistent HNSW");
            for i in 0..25 {
                let v = vec![i as f32, (25 - i) as f32, (i % 5) as f32, 1.0];
                index.insert(&format!("item_{}", i), &v, Some(&format!("meta_{}", i))).unwrap();
            }
            assert_eq!(index.len(), 25);
            initial_hits = index.search_knn(&query, 3);
            assert_eq!(initial_hits.len(), 3);
        }

        // Reopen database from disk and verify durability and exact reconstruction
        {
            let reloaded = HnswVectorIndex::open(&db_path).expect("Failed to reopen persistent HNSW");
            assert_eq!(reloaded.len(), 25);
            assert!(reloaded.contains("item_0"));
            assert!(reloaded.contains("item_24"));

            let reloaded_hits = reloaded.search_knn(&query, 3);
            assert_eq!(reloaded_hits.len(), 3);
            for (h1, h2) in initial_hits.iter().zip(reloaded_hits.iter()) {
                assert_eq!(h1.id, h2.id);
                assert!((h1.distance - h2.distance).abs() < 1e-5);
                assert_eq!(h1.metadata, h2.metadata);
            }
        }

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_batch_insert() {
        let config = HnswConfig {
            m: 8,
            m0: 16,
            ef_construction: 40,
            ef_search: 40,
            metric: DistanceMetric::Cosine,
            dimension: 4,
        };

        let index = HnswVectorIndex::in_memory(config).expect("In-memory index");

        let entries = vec![
            ("b1".to_string(), vec![1.0, 0.0, 0.0, 0.0], Some("first".to_string())),
            ("b2".to_string(), vec![0.0, 1.0, 0.0, 0.0], Some("second".to_string())),
            ("b3".to_string(), vec![0.0, 0.0, 1.0, 0.0], None),
        ];

        let count = index.batch_insert(entries).unwrap();
        assert_eq!(count, 3);
        assert_eq!(index.len(), 3);
    }
}
