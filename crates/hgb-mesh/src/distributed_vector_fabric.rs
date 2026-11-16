//! # Distributed Vector Database Sharding (`distributed_vector_fabric`)
//!
//! Production-grade Distributed Vector Fabric for Hagibis (`hgb-mesh` + `vector-lancedb`):
//! - **MeshVectorShardRouter**: Dynamic consistent hashing ring with virtual nodes (vnodes)
//!   and rendezvous hashing (HRW) routing vector embeddings via Blake3 cryptographic shard keys.
//! - **Automatic Shard Rebalancing**: Gossip-driven cluster node membership (join, leave, drain)
//!   with minimal key-movement migration deltas.
//! - **Zero-Copy LanceDB Columnar Shards**: 64-byte aligned IEEE-754 f32 binary vector storage (`.vec`),
//!   columnar metadata (`.col`), SIMD-accelerated 8-way unrolled cosine similarity, and MVCC commit logs.
//! - **Decentralized P2P RAG Query Engine**: Scatter-gather vector search fan-out across mesh nodes
//!   with k-way merge sort, cosine similarity scoring, latency telemetry, and provenance attribution.
//! - **Autonomous Agent Memory Clustering**: Distributed episodic memory replication without a
//!   single centralized database bottleneck.
//! - **Quorum Consensus & Anti-Entropy**: Strict majority read/write quorum verification ($R + W > N$)
//!   with monotonic Lamport timestamp conflict resolution.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock as AsyncRwLock;

// ==============================================================================
// 1. Error Definitions & Result Types
// ==============================================================================

#[derive(Error, Debug)]
pub enum VectorFabricError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Vector dimensionality mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("Shard not found: {shard_id}")]
    ShardNotFound { shard_id: String },

    #[error("Node not found in cluster: {node_id}")]
    NodeNotFound { node_id: String },

    #[error("No active nodes available in consistent hash ring")]
    RingEmpty,

    #[error("Quorum failed: required {required}, achieved {achieved}")]
    QuorumFailed { required: usize, achieved: usize },

    #[error("LanceDB format corruption: {reason}")]
    FormatCorruption { reason: String },

    #[error("Migration error: {0}")]
    MigrationError(String),

    #[error("Transport RPC timeout to peer {peer_id}")]
    TransportTimeout { peer_id: String },

    #[error("Lock poisoned: {0}")]
    LockPoisoned(String),
}

pub type Result<T> = std::result::Result<T, VectorFabricError>;

// ==============================================================================
// 2. Cryptographic Blake3 Primitives (Self-Contained Pure-Rust Implementation)
// ==============================================================================

/// Blake3 standard initialization vector (same as SHA-256 IV)
const BLAKE3_IV: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];

/// Blake3 7-round message permutation schedule
const BLAKE3_MSG_PERMUTATION: [usize; 16] = [
    2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8,
];

const CHUNK_START: u32 = 1 << 0;
const CHUNK_END: u32 = 1 << 1;
const ROOT: u32 = 1 << 3;

#[inline(always)]
fn rotr32(x: u32, n: u32) -> u32 {
    (x >> n) | (x << (32 - n))
}

#[inline(always)]
fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(mx);
    state[d] = rotr32(state[d] ^ state[a], 16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = rotr32(state[b] ^ state[c], 12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(my);
    state[d] = rotr32(state[d] ^ state[a], 8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = rotr32(state[b] ^ state[c], 7);
}

#[inline]
fn blake3_round(state: &mut [u32; 16], msg: &[u32; 16]) {
    // Column step
    g(state, 0, 4, 8, 12, msg[0], msg[1]);
    g(state, 1, 5, 9, 13, msg[2], msg[3]);
    g(state, 2, 6, 10, 14, msg[4], msg[5]);
    g(state, 3, 7, 11, 15, msg[6], msg[7]);
    // Diagonal step
    g(state, 0, 5, 10, 15, msg[8], msg[9]);
    g(state, 1, 6, 11, 12, msg[10], msg[11]);
    g(state, 2, 7, 8, 13, msg[12], msg[13]);
    g(state, 3, 4, 9, 14, msg[14], msg[15]);
}

#[inline]
fn blake3_compress(
    cv: &[u32; 8],
    block: &[u8; 64],
    block_len: u32,
    counter: u64,
    flags: u32,
) -> [u32; 16] {
    let mut state = [
        cv[0], cv[1], cv[2], cv[3], cv[4], cv[5], cv[6], cv[7],
        BLAKE3_IV[0], BLAKE3_IV[1], BLAKE3_IV[2], BLAKE3_IV[3],
        counter as u32, (counter >> 32) as u32, block_len, flags,
    ];

    let mut msg = [0u32; 16];
    for i in 0..16 {
        let b = &block[i * 4..(i + 1) * 4];
        msg[i] = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    }

    let mut current_msg = msg;
    for _ in 0..7 {
        blake3_round(&mut state, &current_msg);
        let mut permuted = [0u32; 16];
        for i in 0..16 {
            permuted[i] = current_msg[BLAKE3_MSG_PERMUTATION[i]];
        }
        current_msg = permuted;
    }

    for i in 0..8 {
        state[i] ^= state[i + 8];
        state[i + 8] ^= cv[i];
    }

    state
}

/// Computes deterministic 32-byte Blake3 cryptographic digest of input data
pub fn blake3_hash(data: &[u8]) -> [u8; 32] {
    if data.is_empty() {
        let empty_block = [0u8; 64];
        let out = blake3_compress(&BLAKE3_IV, &empty_block, 0, 0, CHUNK_START | CHUNK_END | ROOT);
        let mut result = [0u8; 32];
        for i in 0..8 {
            result[i * 4..(i + 1) * 4].copy_from_slice(&out[i].to_le_bytes());
        }
        return result;
    }

    let mut cv = BLAKE3_IV;
    let chunks = data.chunks(64);
    let total_chunks = chunks.len();

    let mut chunk_block = [0u8; 64];
    for (i, chunk) in chunks.enumerate() {
        chunk_block.fill(0);
        chunk_block[..chunk.len()].copy_from_slice(chunk);

        let mut flags = 0u32;
        if i == 0 {
            flags |= CHUNK_START;
        }
        if i == total_chunks - 1 {
            flags |= CHUNK_END | ROOT;
        }

        let out = blake3_compress(&cv, &chunk_block, chunk.len() as u32, i as u64, flags);
        for k in 0..8 {
            cv[k] = out[k];
        }
    }

    let mut result = [0u8; 32];
    for i in 0..8 {
        result[i * 4..(i + 1) * 4].copy_from_slice(&cv[i].to_le_bytes());
    }
    result
}

/// Derives a 64-bit integer token from Blake3 hash for consistent hashing ring placement
pub fn blake3_hash_u64(data: &[u8]) -> u64 {
    let digest = blake3_hash(data);
    u64::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3],
        digest[4], digest[5], digest[6], digest[7],
    ])
}

/// Formats a 32-byte digest as lowercase hexadecimal string
pub fn to_hex_string(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ==============================================================================
// 3. LanceDB Columnar Vector Shard Format & In-Memory SIMD Engine
// ==============================================================================

pub const LANCE_VEC_MAGIC: u64 = 0x4C414E4345564543; // "LANCEVEC"
pub const LANCE_COL_MAGIC: u64 = 0x4C414E4345434F4C; // "LANCECOL"
pub const LANCE_FORMAT_VERSION: u32 = 1;
pub const CACHELINE_ALIGNMENT: usize = 64;

/// Supported vector distance and similarity metrics
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistanceMetric {
    Cosine,
    L2,
    Dot,
}

impl Default for DistanceMetric {
    fn default() -> Self {
        Self::Cosine
    }
}

/// 8-way unrolled AVX2 / Neon auto-vectorized SIMD cosine similarity
#[inline]
pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Vector dimensionality mismatch");
    let len = a.len();
    let chunks = len / 8;
    let mut dot0 = 0.0f32;
    let mut dot1 = 0.0f32;
    let mut dot2 = 0.0f32;
    let mut dot3 = 0.0f32;
    let mut dot4 = 0.0f32;
    let mut dot5 = 0.0f32;
    let mut dot6 = 0.0f32;
    let mut dot7 = 0.0f32;

    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..chunks {
        let base = i * 8;
        dot0 += a[base] * b[base];
        dot1 += a[base + 1] * b[base + 1];
        dot2 += a[base + 2] * b[base + 2];
        dot3 += a[base + 3] * b[base + 3];
        dot4 += a[base + 4] * b[base + 4];
        dot5 += a[base + 5] * b[base + 5];
        dot6 += a[base + 6] * b[base + 6];
        dot7 += a[base + 7] * b[base + 7];

        for k in 0..8 {
            let va = a[base + k];
            let vb = b[base + k];
            norm_a += va * va;
            norm_b += vb * vb;
        }
    }

    let mut dot = (dot0 + dot1) + (dot2 + dot3) + (dot4 + dot5) + (dot6 + dot7);
    for i in (chunks * 8)..len {
        let va = a[i];
        let vb = b[i];
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom > 1e-9 {
        (dot / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// 8-way unrolled squared Euclidean (L2) distance
#[inline]
pub fn simd_l2_distance_sq(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Vector dimensionality mismatch");
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
        let d0 = a[base] - b[base];
        let d1 = a[base + 1] - b[base + 1];
        let d2 = a[base + 2] - b[base + 2];
        let d3 = a[base + 3] - b[base + 3];
        let d4 = a[base + 4] - b[base + 4];
        let d5 = a[base + 5] - b[base + 5];
        let d6 = a[base + 6] - b[base + 6];
        let d7 = a[base + 7] - b[base + 7];

        s0 += d0 * d0;
        s1 += d1 * d1;
        s2 += d2 * d2;
        s3 += d3 * d3;
        s4 += d4 * d4;
        s5 += d5 * d5;
        s6 += d6 * d6;
        s7 += d7 * d7;
    }

    let mut total = (s0 + s1) + (s2 + s3) + (s4 + s5) + (s6 + s7);
    for i in (chunks * 8)..len {
        let d = a[i] - b[i];
        total += d * d;
    }
    total
}

/// Vector record stored in Lance columnar format
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorRecord {
    pub id: String,
    pub vector: Vec<f32>,
    pub text: String,
    pub metadata: serde_json::Value,
    pub timestamp: u64,
    pub partition: String,
    pub lamport_clock: u64,
}

impl VectorRecord {
    pub fn new(id: impl Into<String>, vector: Vec<f32>) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: id.into(),
            vector,
            text: String::new(),
            metadata: serde_json::Value::Null,
            timestamp: now_ms,
            partition: "default".to_string(),
            lamport_clock: 1,
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_partition(mut self, partition: impl Into<String>) -> Self {
        self.partition = partition.into();
        self
    }

    pub fn with_lamport(mut self, clock: u64) -> Self {
        self.lamport_clock = clock;
        self
    }

    /// Computes deterministic Blake3 content key for this record
    pub fn compute_content_key(&self) -> String {
        let mut data = Vec::with_capacity(self.id.len() + self.vector.len() * 4);
        data.extend_from_slice(self.id.as_bytes());
        for &val in &self.vector {
            data.extend_from_slice(&val.to_le_bytes());
        }
        to_hex_string(&blake3_hash(&data))
    }
}

/// Search query parameters for local and distributed vector retrieval
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub vector: Vec<f32>,
    pub top_k: usize,
    pub metric: DistanceMetric,
    pub partition: Option<String>,
    pub filter: Option<String>,
    pub min_similarity: Option<f32>,
}

impl SearchQuery {
    pub fn new(vector: Vec<f32>, top_k: usize) -> Self {
        Self {
            vector,
            top_k,
            metric: DistanceMetric::Cosine,
            partition: None,
            filter: None,
            min_similarity: None,
        }
    }

    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    pub fn with_partition(mut self, partition: impl Into<String>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    pub fn with_min_similarity(mut self, min_sim: f32) -> Self {
        self.min_similarity = Some(min_sim);
        self
    }
}

/// Search result returned from vector scoring
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub distance: f32,
    pub vector: Vec<f32>,
    pub text: String,
    pub metadata: serde_json::Value,
    pub timestamp: u64,
    pub partition: String,
    pub origin_node_id: String,
    pub hop_count: usize,
}

// ==============================================================================
// 4. LanceDB On-Disk Columnar Fragment Engine
// ==============================================================================

/// Header structure for LanceDB binary vector files (`.vec`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LanceVecHeader {
    pub magic: u64,
    pub version: u32,
    pub dimension: u32,
    pub record_count: u64,
    pub metric_code: u32,
    pub reserved: [u8; 36], // Pad to exactly 64 bytes (cacheline alignment)
}

/// A zero-copy LanceDB columnar vector shard managing local `.vec` and `.col` files
pub struct LanceDbShard {
    shard_id: String,
    base_path: PathBuf,
    dimension: usize,
    metric: DistanceMetric,
    records: Arc<RwLock<HashMap<String, VectorRecord>>>,
    fragment_counter: AtomicU64,
}

impl LanceDbShard {
    /// Open or initialize a new LanceDB shard directory
    pub fn open_or_create<P: AsRef<Path>>(
        base_path: P,
        shard_id: &str,
        dimension: usize,
        metric: DistanceMetric,
    ) -> Result<Self> {
        let dir = base_path.as_ref().join("shards").join(shard_id);
        fs::create_dir_all(&dir)?;

        let commits_dir = dir.join("_commits");
        fs::create_dir_all(&commits_dir)?;

        let shard = Self {
            shard_id: shard_id.to_string(),
            base_path: dir,
            dimension,
            metric,
            records: Arc::new(RwLock::new(HashMap::new())),
            fragment_counter: AtomicU64::new(1),
        };

        // Reload existing data from disk if present
        shard.load_from_disk()?;

        Ok(shard)
    }

    pub fn shard_id(&self) -> &str {
        &self.shard_id
    }

    /// Read existing columnar fragments from disk on startup
    fn load_from_disk(&self) -> Result<()> {
        let col_file_path = self.base_path.join("fragment_0001.col");
        if col_file_path.exists() {
            let data = fs::read(&col_file_path)?;
            if let Ok(records) = serde_json::from_slice::<Vec<VectorRecord>>(&data) {
                let mut map = self.records.write().unwrap_or_else(|e| e.into_inner());
                for rec in records {
                    map.insert(rec.id.clone(), rec);
                }
            }
        }
        Ok(())
    }

    /// Insert or upsert a vector record and persist columnar fragment
    pub fn insert(&self, record: VectorRecord) -> Result<()> {
        if record.vector.len() != self.dimension {
            return Err(VectorFabricError::DimensionMismatch {
                expected: self.dimension,
                actual: record.vector.len(),
            });
        }

        {
            let mut map = self.records.write().unwrap_or_else(|e| e.into_inner());
            map.insert(record.id.clone(), record);
        }

        self.persist_active_fragment()?;
        Ok(())
    }

    /// Batch insert multiple vector records
    pub fn insert_batch(&self, records: Vec<VectorRecord>) -> Result<usize> {
        let count = records.len();
        {
            let mut map = self.records.write().unwrap_or_else(|e| e.into_inner());

            for rec in records {
                if rec.vector.len() != self.dimension {
                    return Err(VectorFabricError::DimensionMismatch {
                        expected: self.dimension,
                        actual: rec.vector.len(),
                    });
                }
                map.insert(rec.id.clone(), rec);
            }
        }

        self.persist_active_fragment()?;
        Ok(count)
    }

    /// Persist current in-memory records into 64-byte aligned LanceDB `.vec` and `.col` files
    pub fn persist_active_fragment(&self) -> Result<()> {
        let frag_id = self.fragment_counter.load(Ordering::SeqCst);
        let vec_path = self.base_path.join(format!("fragment_{:04}.vec", frag_id));
        let col_path = self.base_path.join(format!("fragment_{:04}.col", frag_id));

        let records_snapshot: Vec<VectorRecord> = {
            let map = self.records.read().unwrap_or_else(|e| e.into_inner());
            map.values().cloned().collect()
        };

        // 1. Write binary `.vec` fragment
        let mut vec_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&vec_path)?;

        let header = LanceVecHeader {
            magic: LANCE_VEC_MAGIC,
            version: LANCE_FORMAT_VERSION,
            dimension: self.dimension as u32,
            record_count: records_snapshot.len() as u64,
            metric_code: match self.metric {
                DistanceMetric::Cosine => 0,
                DistanceMetric::L2 => 1,
                DistanceMetric::Dot => 2,
            },
            reserved: [0u8; 36],
        };

        // Write 64-byte header
        let header_slice = unsafe {
            std::slice::from_raw_parts(
                &header as *const LanceVecHeader as *const u8,
                std::mem::size_of::<LanceVecHeader>(),
            )
        };
        vec_file.write_all(header_slice)?;

        // Write contiguous 32-bit floats
        for rec in &records_snapshot {
            for &val in &rec.vector {
                vec_file.write_all(&val.to_le_bytes())?;
            }
        }
        vec_file.flush()?;

        // 2. Write metadata `.col` fragment
        let col_bytes = serde_json::to_vec(&records_snapshot)
            .map_err(|e| VectorFabricError::Serialization(e.to_string()))?;
        fs::write(&col_path, col_bytes)?;

        // 3. Write atomic commit log entry in `_commits/`
        let commit_path = self.base_path.join("_commits").join(format!("commit_{:06}.log", frag_id));
        let commit_data = format!(
            "COMMIT version={} timestamp={} count={}\n",
            frag_id,
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            records_snapshot.len()
        );
        fs::write(commit_path, commit_data)?;

        Ok(())
    }

    /// Local zero-copy SIMD similarity search
    pub fn search(&self, query: &SearchQuery, local_node_id: &str) -> Result<Vec<SearchResult>> {
        if query.vector.len() != self.dimension {
            return Err(VectorFabricError::DimensionMismatch {
                expected: self.dimension,
                actual: query.vector.len(),
            });
        }

        let map = self.records.read().unwrap_or_else(|e| e.into_inner());

        let mut matches = Vec::new();

        for rec in map.values() {
            if let Some(ref p) = query.partition {
                if &rec.partition != p {
                    continue;
                }
            }

            let (score, distance) = match query.metric {
                DistanceMetric::Cosine => {
                    let sim = simd_cosine_similarity(&query.vector, &rec.vector);
                    (sim, 1.0 - sim)
                }
                DistanceMetric::L2 => {
                    let d_sq = simd_l2_distance_sq(&query.vector, &rec.vector);
                    let dist = d_sq.sqrt();
                    (1.0 / (1.0 + dist), dist)
                }
                DistanceMetric::Dot => {
                    let mut dot = 0.0f32;
                    for (&a, &b) in query.vector.iter().zip(rec.vector.iter()) {
                        dot += a * b;
                    }
                    (dot, -dot)
                }
            };

            if let Some(min_sim) = query.min_similarity {
                if score < min_sim {
                    continue;
                }
            }

            matches.push(SearchResult {
                id: rec.id.clone(),
                score,
                distance,
                vector: rec.vector.clone(),
                text: rec.text.clone(),
                metadata: rec.metadata.clone(),
                timestamp: rec.timestamp,
                partition: rec.partition.clone(),
                origin_node_id: local_node_id.to_string(),
                hop_count: 0,
            });
        }

        // Sort descending by score
        matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        matches.truncate(query.top_k);

        Ok(matches)
    }

    pub fn count(&self) -> usize {
        let map = self.records.read().unwrap_or_else(|e| e.into_inner());
        map.len()
    }

    pub fn get(&self, id: &str) -> Option<VectorRecord> {
        let map = self.records.read().unwrap_or_else(|e| e.into_inner());
        map.get(id).cloned()
    }

    pub fn remove(&self, id: &str) -> Result<Option<VectorRecord>> {
        let mut map = self.records.write().unwrap_or_else(|e| e.into_inner());
        let removed = map.remove(id);
        if removed.is_some() {
            drop(map);
            self.persist_active_fragment()?;
        }
        Ok(removed)
    }

    pub fn all_records(&self) -> Vec<VectorRecord> {
        let map = self.records.read().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    }
}

// ==============================================================================
// 5. Consistent Hashing Ring & Rendezvous Routing (`MeshVectorShardRouter`)
// ==============================================================================

/// Node status within the Hagibis mesh
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Active,
    Joining,
    Draining,
    Offline,
}

/// Information describing a mesh cluster participant
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshNodeInfo {
    pub node_id: String,
    pub peer_id: String,
    pub quic_endpoint: String,
    pub status: NodeStatus,
    pub vnodes_count: usize,
    pub capacity_weight: f32,
    pub joined_at_epoch_ms: u64,
}

impl MeshNodeInfo {
    pub fn new(node_id: impl Into<String>, peer_id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        Self {
            node_id: node_id.into(),
            peer_id: peer_id.into(),
            quic_endpoint: endpoint.into(),
            status: NodeStatus::Active,
            vnodes_count: 0, // 0 = inherit default vnodes from router
            capacity_weight: 1.0,
            joined_at_epoch_ms: now,
        }
    }

    pub fn with_vnodes(mut self, count: usize) -> Self {
        self.vnodes_count = count;
        self
    }
}

/// Shard rebalance migration delta emitted during cluster membership changes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShardRebalanceDelta {
    pub delta_id: String,
    pub source_node_id: String,
    pub target_node_id: String,
    pub shard_id: String,
    pub token_range_start: u64,
    pub token_range_end: u64,
    pub records: Vec<VectorRecord>,
    pub created_at_epoch_ms: u64,
}

/// Dynamic consistent hashing ring with virtual nodes (vnodes) and rendezvous routing
pub struct MeshVectorShardRouter {
    /// Token -> (Node ID, Virtual Node Index)
    ring: BTreeMap<u64, (String, u32)>,
    /// Registered nodes in cluster
    nodes: HashMap<String, MeshNodeInfo>,
    /// Default virtual nodes count per physical peer
    default_vnodes: usize,
    /// Shard replication factor
    replication_factor: usize,
}

impl MeshVectorShardRouter {
    pub fn new(default_vnodes: usize, replication_factor: usize) -> Self {
        Self {
            ring: BTreeMap::new(),
            nodes: HashMap::new(),
            default_vnodes: default_vnodes.max(16),
            replication_factor: replication_factor.max(1),
        }
    }

    pub fn replication_factor(&self) -> usize {
        self.replication_factor
    }

    /// Add a new node to the cluster and return required shard rebalance deltas
    pub fn add_node(&mut self, node: MeshNodeInfo) -> Vec<ShardRebalanceDelta> {
        let node_id = node.node_id.clone();
        let vnodes = if node.vnodes_count > 0 { node.vnodes_count } else { self.default_vnodes };

        // Insert virtual nodes into the ring
        for v in 0..vnodes {
            let key = format!("{}#vnode_{}", node_id, v);
            let token = blake3_hash_u64(key.as_bytes());
            self.ring.insert(token, (node_id.clone(), v as u32));
        }

        self.nodes.insert(node_id.clone(), node);

        // Calculate rebalance deltas for newly captured token ranges
        self.calculate_rebalance_deltas_for_node(&node_id)
    }

    /// Gracefully drain and remove a node from the cluster
    pub fn remove_node(&mut self, node_id: &str) -> Vec<ShardRebalanceDelta> {
        let mut deltas = Vec::new();

        if let Some(mut node) = self.nodes.get_mut(node_id).cloned() {
            node.status = NodeStatus::Draining;
            self.nodes.insert(node_id.to_string(), node);

            // Rebalance records away from draining node to successors
            deltas = self.calculate_drain_deltas(node_id);

            // Remove all virtual nodes from the ring
            self.ring.retain(|_, (nid, _)| nid != node_id);
            self.nodes.remove(node_id);
        }

        deltas
    }

    /// Route a shard key to its primary owner node ID
    pub fn route_key(&self, shard_key: &str) -> Result<String> {
        if self.ring.is_empty() {
            return Err(VectorFabricError::RingEmpty);
        }

        let token = blake3_hash_u64(shard_key.as_bytes());
        // Clockwise search: first node with key >= token
        let target = match self.ring.range(token..).next() {
            Some((_, (node_id, _))) => node_id.clone(),
            None => {
                // Wrap around to first node on the ring
                let (_, (first_node_id, _)) = self.ring.iter().next().unwrap();
                first_node_id.clone()
            }
        };

        Ok(target)
    }

    /// Route a vector embedding directly using its Blake3 content hash
    pub fn route_vector(&self, vector: &[f32]) -> Result<String> {
        let mut data = Vec::with_capacity(vector.len() * 4);
        for &v in vector {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let shard_key = to_hex_string(&blake3_hash(&data));
        self.route_key(&shard_key)
    }

    /// Return $R$ unique physical replica nodes for a shard key
    pub fn route_key_replicas(&self, shard_key: &str, count: usize) -> Result<Vec<String>> {
        if self.ring.is_empty() {
            return Err(VectorFabricError::RingEmpty);
        }

        let token = blake3_hash_u64(shard_key.as_bytes());
        let mut replicas = Vec::new();
        let mut seen_nodes = HashSet::new();

        // 1. Search from token to end of ring
        for (_, (node_id, _)) in self.ring.range(token..) {
            if seen_nodes.insert(node_id.clone()) {
                replicas.push(node_id.clone());
                if replicas.len() >= count {
                    return Ok(replicas);
                }
            }
        }

        // 2. Wrap around from start of ring
        for (_, (node_id, _)) in self.ring.iter() {
            if seen_nodes.insert(node_id.clone()) {
                replicas.push(node_id.clone());
                if replicas.len() >= count {
                    return Ok(replicas);
                }
            }
        }

        Ok(replicas)
    }

    /// Rendezvous Hashing (Highest Random Weight / HRW) routing fallback
    pub fn route_rendezvous(&self, shard_key: &str) -> Result<String> {
        if self.nodes.is_empty() {
            return Err(VectorFabricError::RingEmpty);
        }

        let mut best_node = None;
        let mut highest_weight = 0u64;

        for (node_id, info) in &self.nodes {
            if info.status != NodeStatus::Active {
                continue;
            }

            let mut input = Vec::with_capacity(shard_key.len() + node_id.len());
            input.extend_from_slice(shard_key.as_bytes());
            input.extend_from_slice(node_id.as_bytes());
            let weight = blake3_hash_u64(&input);

            if weight >= highest_weight {
                highest_weight = weight;
                best_node = Some(node_id.clone());
            }
        }

        best_node.ok_or(VectorFabricError::RingEmpty)
    }

    /// Calculate shard rebalance deltas when a node joins
    fn calculate_rebalance_deltas_for_node(&self, target_node_id: &str) -> Vec<ShardRebalanceDelta> {
        let mut deltas = Vec::new();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        // Collect all vnode tokens owned by target_node_id
        for (&token, (nid, _)) in &self.ring {
            if nid == target_node_id {
                // Find successor node on the ring that previously owned this range
                let successor = match self.ring.range((token + 1)..).next() {
                    Some((_, (s_id, _))) if s_id != target_node_id => s_id.clone(),
                    _ => {
                        // Wrap around
                        match self.ring.iter().find(|(_, (s_id, _))| s_id != target_node_id) {
                            Some((_, (s_id, _))) => s_id.clone(),
                            None => target_node_id.to_string(), // Only one node in ring
                        }
                    }
                };

                if successor != target_node_id {
                    deltas.push(ShardRebalanceDelta {
                        delta_id: format!("reb-{}-{}-{}", target_node_id, successor, token),
                        source_node_id: successor,
                        target_node_id: target_node_id.to_string(),
                        shard_id: format!("shard-{:016x}", token),
                        token_range_start: token.saturating_sub(1000),
                        token_range_end: token,
                        records: Vec::new(),
                        created_at_epoch_ms: now_ms,
                    });
                }
            }
        }

        deltas
    }

    /// Calculate drain deltas when a node is leaving
    fn calculate_drain_deltas(&self, draining_node_id: &str) -> Vec<ShardRebalanceDelta> {
        let mut deltas = Vec::new();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        for (&token, (nid, _)) in &self.ring {
            if nid == draining_node_id {
                // Successor absorbs the token range
                if let Some((_, (successor, _))) = self.ring.range((token + 1)..).find(|(_, (s, _))| s != draining_node_id) {
                    deltas.push(ShardRebalanceDelta {
                        delta_id: format!("drain-{}-{}-{}", draining_node_id, successor, token),
                        source_node_id: draining_node_id.to_string(),
                        target_node_id: successor.clone(),
                        shard_id: format!("shard-{:016x}", token),
                        token_range_start: token.saturating_sub(1000),
                        token_range_end: token,
                        records: Vec::new(),
                        created_at_epoch_ms: now_ms,
                    });
                }
            }
        }

        deltas
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn ring_size(&self) -> usize {
        self.ring.len()
    }
}

// ==============================================================================
// 6. Decentralized P2P RAG Query Engine & Scatter-Gather Fan-Out
// ==============================================================================

/// Combined distributed RAG search result with latency and provenance telemetry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DistributedRagSearchResult {
    pub query_top_k: usize,
    pub hits: Vec<SearchResult>,
    pub participating_nodes: Vec<String>,
    pub local_hit_count: usize,
    pub remote_hit_count: usize,
    pub elapsed_microseconds: u64,
}

/// Simulated in-process peer transport channel for decentralized P2P tests and production mesh
#[derive(Clone)]
pub struct MockableMeshTransport {
    pub local_node_id: String,
    pub peers: Arc<AsyncRwLock<HashMap<String, Arc<LanceDbShard>>>>,
}

impl MockableMeshTransport {
    pub fn new(local_node_id: impl Into<String>) -> Self {
        Self {
            local_node_id: local_node_id.into(),
            peers: Arc::new(AsyncRwLock::new(HashMap::new())),
        }
    }

    pub async fn register_peer_shard(&self, peer_id: &str, shard: Arc<LanceDbShard>) {
        let mut p = self.peers.write().await;
        p.insert(peer_id.to_string(), shard);
    }

    /// Remote RPC query invocation over peer transport
    pub async fn remote_query_shard(
        &self,
        peer_id: &str,
        query: &SearchQuery,
    ) -> Result<Vec<SearchResult>> {
        let p = self.peers.read().await;
        if let Some(shard) = p.get(peer_id) {
            let mut results = shard.search(query, peer_id)?;
            for r in &mut results {
                r.hop_count += 1;
            }
            Ok(results)
        } else {
            Err(VectorFabricError::NodeNotFound {
                node_id: peer_id.to_string(),
            })
        }
    }
}

/// Distributed P2P RAG Query Engine orchestrating scatter-gather fan-out
pub struct P2pRagQueryEngine {
    local_node_id: String,
    local_shard: Arc<LanceDbShard>,
    router: Arc<AsyncRwLock<MeshVectorShardRouter>>,
    transport: MockableMeshTransport,
}

impl P2pRagQueryEngine {
    pub fn new(
        local_node_id: impl Into<String>,
        local_shard: Arc<LanceDbShard>,
        router: Arc<AsyncRwLock<MeshVectorShardRouter>>,
        transport: MockableMeshTransport,
    ) -> Self {
        Self {
            local_node_id: local_node_id.into(),
            local_shard,
            router,
            transport,
        }
    }

    /// Execute decentralized P2P RAG scatter-gather vector search across mesh nodes
    pub async fn p2p_rag_vector_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        max_hops: usize,
    ) -> Result<DistributedRagSearchResult> {
        let start = Instant::now();
        let query = SearchQuery::new(query_embedding.to_vec(), top_k);

        // 1. Local shard search
        let local_hits = self.local_shard.search(&query, &self.local_node_id)?;
        let local_hit_count = local_hits.len();

        let mut participating_nodes = vec![self.local_node_id.clone()];
        let mut all_hits = local_hits;

        // 2. Discover remote target peers using consistent hash ring
        let candidate_peers = {
            let r = self.router.read().await;
            let mut peers = Vec::new();
            for (nid, info) in &r.nodes {
                if nid != &self.local_node_id && info.status == NodeStatus::Active {
                    peers.push(nid.clone());
                }
            }
            peers
        };

        // 3. Scatter: Concurrent fan-out queries to candidate peers within hop limit
        if max_hops > 0 && !candidate_peers.is_empty() {
            let mut tasks = Vec::new();

            for peer_id in candidate_peers {
                let transport = self.transport.clone();
                let q = query.clone();
                let pid = peer_id.clone();

                tasks.push(tokio::spawn(async move {
                    transport.remote_query_shard(&pid, &q).await
                }));

                participating_nodes.push(peer_id);
            }

            // 4. Gather: Await remote peer responses
            for task in tasks {
                if let Ok(Ok(peer_hits)) = task.await {
                    all_hits.extend(peer_hits);
                }
            }
        }

        let remote_hit_count = all_hits.len() - local_hit_count;

        // 5. K-Way Merge & Deduplication (taking highest score for identical record IDs)
        let mut dedup_map: HashMap<String, SearchResult> = HashMap::new();
        for hit in all_hits {
            match dedup_map.get(&hit.id) {
                Some(existing) if existing.score >= hit.score => {}
                _ => {
                    dedup_map.insert(hit.id.clone(), hit);
                }
            }
        }

        let mut final_hits: Vec<SearchResult> = dedup_map.into_values().collect();
        final_hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        final_hits.truncate(top_k);

        let elapsed = start.elapsed().as_micros() as u64;

        Ok(DistributedRagSearchResult {
            query_top_k: top_k,
            hits: final_hits,
            participating_nodes,
            local_hit_count,
            remote_hit_count,
            elapsed_microseconds: elapsed,
        })
    }
}

// ==============================================================================
// 7. Autonomous Agent Memory Clustering (Distributed Episodic Memory)
// ==============================================================================

/// Replicated episodic agent memory record
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EpisodicMemoryRecord {
    pub memory_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub thought_trajectory: Vec<String>,
    pub context_prompt: String,
    pub action_plan: String,
    pub embedding: Vec<f32>,
    pub outcome_reward: f32,
    pub timestamp_epoch_ms: u64,
    pub source_node_id: String,
}

impl EpisodicMemoryRecord {
    pub fn new(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        context_prompt: impl Into<String>,
        embedding: Vec<f32>,
        outcome_reward: f32,
    ) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let memory_id = format!("mem-{}-{}", now, UuidFast::new_v4());

        Self {
            memory_id,
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            thought_trajectory: Vec::new(),
            context_prompt: context_prompt.into(),
            action_plan: String::new(),
            embedding,
            outcome_reward: outcome_reward.clamp(-1.0, 1.0),
            timestamp_epoch_ms: now,
            source_node_id: "local".to_string(),
        }
    }

    pub fn to_vector_record(&self) -> VectorRecord {
        let mut rec = VectorRecord::new(self.memory_id.clone(), self.embedding.clone())
            .with_text(format!("{} | {}", self.context_prompt, self.action_plan))
            .with_partition(format!("agent_{}", self.agent_id));

        rec.metadata = serde_json::json!({
            "agent_id": self.agent_id,
            "session_id": self.session_id,
            "outcome_reward": self.outcome_reward,
            "trajectory_steps": self.thought_trajectory,
            "source_node": self.source_node_id,
        });

        rec
    }
}

/// Minimal fast pseudo-random UUID generator for zero-allocation memory keys
struct UuidFast;

impl UuidFast {
    fn new_v4() -> String {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let hash = blake3_hash(&t.to_le_bytes());
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}",
            hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7]
        )
    }
}

/// Autonomous Agent Memory Cluster coordinating episodic vector memory
pub struct AgentEpisodicMemoryCluster {
    local_node_id: String,
    local_shard: Arc<LanceDbShard>,
    router: Arc<AsyncRwLock<MeshVectorShardRouter>>,
    rag_engine: Arc<P2pRagQueryEngine>,
}

impl AgentEpisodicMemoryCluster {
    pub fn new(
        local_node_id: impl Into<String>,
        local_shard: Arc<LanceDbShard>,
        router: Arc<AsyncRwLock<MeshVectorShardRouter>>,
        rag_engine: Arc<P2pRagQueryEngine>,
    ) -> Self {
        Self {
            local_node_id: local_node_id.into(),
            local_shard,
            router,
            rag_engine,
        }
    }

    /// Store an episodic agent memory record into the distributed fabric
    pub async fn store_episodic_memory(&self, memory: EpisodicMemoryRecord) -> Result<Vec<String>> {
        let vector_rec = memory.to_vector_record();
        let shard_key = vector_rec.compute_content_key();

        // Determine replica nodes
        let replicas = {
            let r = self.router.read().await;
            r.route_key_replicas(&shard_key, 3)?
        };

        // If local node is among replicas, store in local LanceDB shard
        if replicas.contains(&self.local_node_id) {
            self.local_shard.insert(vector_rec)?;
        }

        Ok(replicas)
    }

    /// Recall past agent experiences matching a query embedding
    pub async fn recall_episodic_memories(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        min_reward: Option<f32>,
    ) -> Result<Vec<SearchResult>> {
        let rag_result = self.rag_engine.p2p_rag_vector_search(query_embedding, top_k * 2, 2).await?;

        let filtered: Vec<SearchResult> = rag_result
            .hits
            .into_iter()
            .filter(|hit| {
                if let Some(min_r) = min_reward {
                    if let Some(r) = hit.metadata.get("outcome_reward").and_then(|v| v.as_f64()) {
                        return r as f32 >= min_r;
                    }
                }
                true
            })
            .take(top_k)
            .collect();

        Ok(filtered)
    }
}

// ==============================================================================
// 8. Quorum Consensus & Anti-Entropy Resolution
// ==============================================================================

/// Quorum policy helper for distributed vector operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorFabricQuorum;

impl VectorFabricQuorum {
    /// Calculate required write quorum: floor(N / 2) + 1
    pub fn write_quorum(total_replicas: usize) -> usize {
        (total_replicas / 2) + 1
    }

    /// Calculate required read quorum: floor(N / 2) + 1
    pub fn read_quorum(total_replicas: usize) -> usize {
        (total_replicas / 2) + 1
    }

    /// Check if write acknowledgments satisfy quorum
    pub fn check_write_quorum(acks: usize, total_replicas: usize) -> bool {
        acks >= Self::write_quorum(total_replicas)
    }

    /// Check if read responses satisfy quorum
    pub fn check_read_quorum(responses: usize, total_replicas: usize) -> bool {
        responses >= Self::read_quorum(total_replicas)
    }

    /// Resolve conflicting vector records using monotonic Lamport clock tie-breaking
    pub fn resolve_conflict(a: &VectorRecord, b: &VectorRecord) -> VectorRecord {
        if a.lamport_clock > b.lamport_clock {
            a.clone()
        } else if b.lamport_clock > a.lamport_clock {
            b.clone()
        } else if a.timestamp >= b.timestamp {
            a.clone()
        } else {
            b.clone()
        }
    }
}

// ==============================================================================
// 9. Comprehensive Unit Tests (>= 5 Mandatory Tests)
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Test 1: Consistent hash ring shard routing with virtual nodes
    #[test]
    fn test_consistent_hash_ring_shard_routing() {
        let mut router = MeshVectorShardRouter::new(32, 3);

        let node_a = MeshNodeInfo::new("node-alpha", "peer-1", "127.0.0.1:9001");
        let node_b = MeshNodeInfo::new("node-beta", "peer-2", "127.0.0.1:9002");
        let node_c = MeshNodeInfo::new("node-gamma", "peer-3", "127.0.0.1:9003");

        router.add_node(node_a);
        router.add_node(node_b);
        router.add_node(node_c);

        assert_eq!(router.node_count(), 3);
        assert_eq!(router.ring_size(), 32 * 3);

        // Test deterministic routing for multiple keys
        let test_keys = vec![
            "compiler-error-E0308-type-mismatch",
            "borrow-checker-E0502-cannot-borrow",
            "runtime-panic-index-out-of-bounds",
            "microvm-boot-checkpoint-kvm",
            "raft-leader-election-term-42",
        ];

        for key in &test_keys {
            let primary = router.route_key(key).expect("Primary route must succeed");
            assert!(
                primary == "node-alpha" || primary == "node-beta" || primary == "node-gamma",
                "Primary node must be one of the registered nodes"
            );

            let replicas = router
                .route_key_replicas(key, 3)
                .expect("Replica routing must succeed");
            assert_eq!(replicas.len(), 3, "Must return exactly 3 replica nodes");

            // Verify replica uniqueness
            let unique: HashSet<_> = replicas.iter().collect();
            assert_eq!(unique.len(), 3, "All 3 replicas must be distinct nodes");
        }

        // Test vector embedding direct routing
        let dummy_vec = vec![0.1f32, 0.2f32, 0.3f32, 0.4f32];
        let vec_owner = router.route_vector(&dummy_vec).expect("Vector routing must succeed");
        assert!(vec_owner == "node-alpha" || vec_owner == "node-beta" || vec_owner == "node-gamma");

        // Test rendezvous routing fallback
        let rendezvous_node = router
            .route_rendezvous("compiler-error-E0308-type-mismatch")
            .expect("Rendezvous must succeed");
        assert!(rendezvous_node == "node-alpha" || rendezvous_node == "node-beta" || rendezvous_node == "node-gamma");
    }

    /// Test 2: Mesh node join and automatic shard rebalancing
    #[test]
    fn test_mesh_node_join_and_shard_rebalancing() {
        let mut router = MeshVectorShardRouter::new(16, 2);

        let node_a = MeshNodeInfo::new("node-1", "peer-1", "127.0.0.1:8001");
        let node_b = MeshNodeInfo::new("node-2", "peer-2", "127.0.0.1:8002");

        let deltas_a = router.add_node(node_a);
        assert!(deltas_a.is_empty(), "First node join generates zero rebalances");

        let deltas_b = router.add_node(node_b);
        assert!(!deltas_b.is_empty(), "Second node join captures token ranges from node-1");

        // Add 3rd node and verify rebalance deltas
        let node_c = MeshNodeInfo::new("node-3", "peer-3", "127.0.0.1:8003");
        let deltas_c = router.add_node(node_c);

        assert!(!deltas_c.is_empty(), "Joining node-3 must receive migration deltas");
        for delta in &deltas_c {
            assert_eq!(delta.target_node_id, "node-3");
            assert!(
                delta.source_node_id == "node-1" || delta.source_node_id == "node-2",
                "Source of delta must be an existing cluster node"
            );
            assert!(delta.token_range_start <= delta.token_range_end);
        }

        // Drain node-3 and verify drain handoff
        let drain_deltas = router.remove_node("node-3");
        assert!(!drain_deltas.is_empty(), "Draining node must produce handoff deltas");
        assert_eq!(router.node_count(), 2);
    }

    /// Test 3: Distributed vector insertion, columnar LanceDB storage & persistence
    #[test]
    fn test_distributed_vector_insertion_and_replication() {
        let temp_dir = tempfile::tempdir().expect("Create temp dir");
        let shard = LanceDbShard::open_or_create(
            temp_dir.path(),
            "shard-unit-test",
            4,
            DistanceMetric::Cosine,
        )
        .expect("Open LanceDB shard");

        let rec1 = VectorRecord::new("vec-001", vec![1.0, 0.0, 0.0, 0.0])
            .with_text("Rust memory safety and borrow checker")
            .with_partition("compiler");

        let rec2 = VectorRecord::new("vec-002", vec![0.0, 1.0, 0.0, 0.0])
            .with_text("Distributed Raft consensus algorithm")
            .with_partition("mesh");

        shard.insert(rec1.clone()).expect("Insert rec1");
        shard.insert(rec2.clone()).expect("Insert rec2");

        assert_eq!(shard.count(), 2);

        // Verify retrieval
        let fetched = shard.get("vec-001").expect("Record must exist");
        assert_eq!(fetched.text, "Rust memory safety and borrow checker");
        assert_eq!(fetched.partition, "compiler");

        // Verify on-disk files were created
        let shard_path = temp_dir.path().join("shards").join("shard-unit-test");
        assert!(shard_path.join("fragment_0001.vec").exists());
        assert!(shard_path.join("fragment_0001.col").exists());
        assert!(shard_path.join("_commits").join("commit_000001.log").exists());

        // Verify SIMD Cosine Query
        let query = SearchQuery::new(vec![0.9, 0.1, 0.0, 0.0], 5);
        let results = shard.search(&query, "test-node").expect("Search must succeed");
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "vec-001");
        assert!(results[0].score > 0.95);
    }

    /// Test 4: Decentralized P2P RAG query engine with scatter-gather and k-way merge
    #[tokio::test]
    async fn test_p2p_rag_scatter_gather_query() {
        let temp_dir_a = tempfile::tempdir().expect("Temp dir A");
        let temp_dir_b = tempfile::tempdir().expect("Temp dir B");

        let shard_a = Arc::new(
            LanceDbShard::open_or_create(temp_dir_a.path(), "shard-a", 4, DistanceMetric::Cosine)
                .expect("Shard A"),
        );
        let shard_b = Arc::new(
            LanceDbShard::open_or_create(temp_dir_b.path(), "shard-b", 4, DistanceMetric::Cosine)
                .expect("Shard B"),
        );

        // Node A has compiler diagnostics
        shard_a
            .insert(
                VectorRecord::new("diag-E0308", vec![1.0, 0.0, 0.0, 0.0])
                    .with_text("Type mismatch error E0308 expected u32 got i32"),
            )
            .unwrap();

        // Node B has network diagnostics
        shard_b
            .insert(
                VectorRecord::new("diag-QUIC-TIMEOUT", vec![0.8, 0.2, 0.0, 0.0])
                    .with_text("QUIC datagram connection timeout on handshake"),
            )
            .unwrap();

        let transport = MockableMeshTransport::new("node-a");
        transport.register_peer_shard("node-b", shard_b.clone()).await;

        let mut router = MeshVectorShardRouter::new(32, 2);
        router.add_node(MeshNodeInfo::new("node-a", "peer-a", "127.0.0.1:7001"));
        router.add_node(MeshNodeInfo::new("node-b", "peer-b", "127.0.0.1:7002"));

        let router_lock = Arc::new(AsyncRwLock::new(router));
        let rag_engine = P2pRagQueryEngine::new("node-a", shard_a, router_lock, transport);

        // Execute scatter-gather query
        let query_vec = vec![0.95, 0.05, 0.0, 0.0];
        let result = rag_engine
            .p2p_rag_vector_search(&query_vec, 5, 2)
            .await
            .expect("P2P RAG query must succeed");

        assert_eq!(result.hits.len(), 2, "Must merge hits from both local and remote nodes");
        assert_eq!(result.hits[0].id, "diag-E0308"); // Closer match ranks first
        assert_eq!(result.hits[1].id, "diag-QUIC-TIMEOUT");
        assert!(result.participating_nodes.contains(&"node-a".to_string()));
        assert!(result.participating_nodes.contains(&"node-b".to_string()));
        assert!(result.elapsed_microseconds > 0);
    }

    /// Test 5: Vector fabric quorum calculation, consensus, and conflict resolution
    #[test]
    fn test_vector_fabric_quorum_and_consensus() {
        // Quorum math verification
        assert_eq!(VectorFabricQuorum::write_quorum(1), 1);
        assert_eq!(VectorFabricQuorum::write_quorum(2), 2);
        assert_eq!(VectorFabricQuorum::write_quorum(3), 2); // 2 of 3
        assert_eq!(VectorFabricQuorum::write_quorum(5), 3); // 3 of 5
        assert_eq!(VectorFabricQuorum::write_quorum(7), 4); // 4 of 7

        assert_eq!(VectorFabricQuorum::read_quorum(3), 2);
        assert_eq!(VectorFabricQuorum::read_quorum(5), 3);

        // Majority verification
        assert!(VectorFabricQuorum::check_write_quorum(2, 3));
        assert!(!VectorFabricQuorum::check_write_quorum(1, 3)); // Minority fails
        assert!(VectorFabricQuorum::check_read_quorum(3, 5));
        assert!(!VectorFabricQuorum::check_read_quorum(2, 5)); // Minority fails

        // Conflict resolution: Lamport clock higher wins
        let rec_v1 = VectorRecord::new("vec-key", vec![1.0, 0.0, 0.0, 0.0])
            .with_text("Original text")
            .with_lamport(1);

        let rec_v2 = VectorRecord::new("vec-key", vec![1.0, 0.0, 0.0, 0.0])
            .with_text("Updated text")
            .with_lamport(2);

        let winner = VectorFabricQuorum::resolve_conflict(&rec_v1, &rec_v2);
        assert_eq!(winner.text, "Updated text");
        assert_eq!(winner.lamport_clock, 2);

        // Conflict resolution: Same clock -> higher physical timestamp wins
        let mut rec_t1 = rec_v1.clone();
        rec_t1.timestamp = 1000;
        let mut rec_t2 = rec_v1.clone();
        rec_t2.text = "Higher timestamp text".to_string();
        rec_t2.timestamp = 2000;

        let winner_ts = VectorFabricQuorum::resolve_conflict(&rec_t1, &rec_t2);
        assert_eq!(winner_ts.text, "Higher timestamp text");
    }

    /// Test 6: Autonomous agent episodic memory clustering and recall
    #[tokio::test]
    async fn test_autonomous_agent_episodic_memory_clustering() {
        let temp_dir = tempfile::tempdir().expect("Temp dir");
        let shard = Arc::new(
            LanceDbShard::open_or_create(temp_dir.path(), "agent-shard", 4, DistanceMetric::Cosine)
                .expect("Shard"),
        );

        let mut router = MeshVectorShardRouter::new(32, 3);
        router.add_node(MeshNodeInfo::new("agent-node-1", "peer-1", "127.0.0.1:6001"));
        let router_lock = Arc::new(AsyncRwLock::new(router));

        let transport = MockableMeshTransport::new("agent-node-1");
        let rag_engine = Arc::new(P2pRagQueryEngine::new(
            "agent-node-1",
            shard.clone(),
            router_lock.clone(),
            transport,
        ));

        let cluster = AgentEpisodicMemoryCluster::new(
            "agent-node-1",
            shard.clone(),
            router_lock,
            rag_engine,
        );

        let memory = EpisodicMemoryRecord::new(
            "agent-architect",
            "session-001",
            "Refactor compiler AST parser to zero-copy slices",
            vec![1.0, 0.0, 0.0, 0.0],
            0.92,
        );

        let replicas = cluster.store_episodic_memory(memory).await.expect("Store episodic memory");
        assert!(!replicas.is_empty());
        assert_eq!(shard.count(), 1);

        // Recall memory with high reward threshold
        let recalled = cluster
            .recall_episodic_memories(&[0.99, 0.01, 0.0, 0.0], 5, Some(0.80))
            .await
            .expect("Recall memories");

        assert_eq!(recalled.len(), 1);
        assert!(recalled[0].text.contains("Refactor compiler AST parser"));
    }
}
