//! # Vector-Native SQLite Edge Indexing (`sqlite-vec` Compatible)
//!
//! High-performance embedded vector storage and hybrid search engine backed by SQLite WAL.
//! Implements binary float vector serialization compatible with the `sqlite-vec` (`vec0`) standard,
//! transactional virtual/edge collections and items tables, and pure SIMD AVX2 distance scanning
//! for sub-millisecond similarity search across codebases and documentation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use hgb_core::{HgbError, Result};

/// Encodes an f32 slice into raw little-endian binary bytes.
/// Fully compatible with the `sqlite-vec` (`vec0`) standard format.
#[inline]
pub fn encode_vector_blob(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vector.len() * 4);
    for &val in vector {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Decodes raw little-endian binary bytes into an f32 vector.
/// Validates that the BLOB length is a multiple of 4 bytes.
#[inline]
pub fn decode_vector_blob(blob: &[u8]) -> Result<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(HgbError::Storage(format!(
            "Invalid sqlite-vec BLOB length: {} bytes (must be multiple of 4)",
            blob.len()
        )));
    }
    let count = blob.len() / 4;
    let mut vec = Vec::with_capacity(count);
    for chunk in blob.chunks_exact(4) {
        let bytes: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
        vec.push(f32::from_le_bytes(bytes));
    }
    Ok(vec)
}

/// Reusable zero-alloc decode into an existing destination buffer
#[inline]
pub fn decode_vector_blob_into(blob: &[u8], dest: &mut Vec<f32>) -> Result<()> {
    if blob.len() % 4 != 0 {
        return Err(HgbError::Storage(format!(
            "Invalid sqlite-vec BLOB length: {} bytes (must be multiple of 4)",
            blob.len()
        )));
    }
    dest.clear();
    dest.reserve(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        let bytes: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
        dest.push(f32::from_le_bytes(bytes));
    }
    Ok(())
}

/// Supported distance metrics for vector similarity calculations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeDistanceMetric {
    Cosine,
    Euclidean,
    DotProduct,
}

impl EdgeDistanceMetric {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::Euclidean => "euclidean",
            Self::DotProduct => "dot",
        }
    }

    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "cosine" => Some(Self::Cosine),
            "euclidean" | "l2" => Some(Self::Euclidean),
            "dot" | "dot_product" => Some(Self::DotProduct),
            _ => None,
        }
    }
}

/// SIMD-accelerated math kernels with AVX2 and unrolled 8-way portable fallbacks
pub mod simd_kernels {
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

/// Metadata record for an edge vector collection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeCollection {
    pub id: String,
    pub name: String,
    pub dimension: usize,
    pub metric: EdgeDistanceMetric,
    pub created_at: i64,
}

/// Document chunk record with embedding BLOB stored in SQLite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeVectorItem {
    pub id: String,
    pub collection: String,
    pub embedding: Vec<f32>,
    pub document_chunk: String,
    pub metadata_json: String,
    pub created_at: i64,
}

/// Result returned from hybrid search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSearchResult {
    pub id: String,
    pub collection: String,
    pub document_chunk: String,
    pub metadata_json: String,
    pub vector_score: f32,
    pub lexical_score: f32,
    pub combined_score: f32,
    pub distance: f32,
    pub created_at: i64,
}

/// Query parameters for hybrid lexical and SIMD vector retrieval
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchQuery {
    pub collection: String,
    pub query_vector: Option<Vec<f32>>,
    pub lexical_query: Option<String>,
    pub metadata_filter: Option<HashMap<String, serde_json::Value>>,
    /// Weight assigned to vector score: combined = alpha * vector + (1 - alpha) * lexical
    /// Range [0.0, 1.0]. Default: 0.7
    pub alpha: f32,
    pub limit: usize,
    /// Optional pre-filter to use SQLite LIKE on document_chunk before scanning
    pub use_sql_prefilter: bool,
}

impl Default for HybridSearchQuery {
    fn default() -> Self {
        Self {
            collection: String::new(),
            query_vector: None,
            lexical_query: None,
            metadata_filter: None,
            alpha: 0.7,
            limit: 10,
            use_sql_prefilter: false,
        }
    }
}

/// Telemetry and benchmark report for sub-millisecond retrieval
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSearchBenchmarkReport {
    pub collection: String,
    pub total_items: usize,
    pub dimension: usize,
    pub iterations: usize,
    pub total_elapsed_us: u64,
    pub avg_latency_us: f64,
    pub sub_millisecond: bool,
    pub queries_per_second: f64,
}

/// Edge indexing engine integrating SQLite persistence with SIMD vector scanning
#[derive(Clone)]
pub struct VectorNativeEdgeEngine {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl VectorNativeEdgeEngine {
    /// Open or create an edge indexing database at the specified path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("Failed to open SQLite database: {}", e)))?;

        Self::configure_sqlite(&conn)?;
        Self::initialize_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    /// Open an ephemeral in-memory database for testing and fast transient caching
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("Failed to open in-memory SQLite: {}", e)))?;

        Self::configure_sqlite(&conn)?;
        Self::initialize_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Returns the underlying database path
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn configure_sqlite(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA temp_store = MEMORY;
            PRAGMA cache_size = -64000;
            "#,
        )
        .map_err(|e| HgbError::Storage(format!("Failed to configure SQLite PRAGMAs: {}", e)))?;
        Ok(())
    }

    fn initialize_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS edge_vector_collections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                dimension INTEGER NOT NULL,
                metric TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS edge_vector_items (
                id TEXT PRIMARY KEY,
                collection TEXT NOT NULL,
                embedding BLOB NOT NULL,
                document_chunk TEXT NOT NULL,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                created_at INTEGER NOT NULL,
                FOREIGN KEY(collection) REFERENCES edge_vector_collections(name) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_edge_vector_items_collection 
                ON edge_vector_items(collection);

            CREATE INDEX IF NOT EXISTS idx_edge_vector_items_created 
                ON edge_vector_items(created_at);
            "#,
        )
        .map_err(|e| HgbError::Storage(format!("Failed to initialize edge schema: {}", e)))?;
        Ok(())
    }

    /// Create a new edge vector collection
    pub fn create_collection(
        &self,
        name: &str,
        dimension: usize,
        metric: EdgeDistanceMetric,
    ) -> Result<EdgeCollection> {
        let id = format!("coll_{}_{}", name, chrono::Utc::now().timestamp_millis());
        let created_at = chrono::Utc::now().timestamp();

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO edge_vector_collections (id, name, dimension, metric, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, dimension as i64, metric.as_str(), created_at],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to create collection '{}': {}", name, e)))?;

        Ok(EdgeCollection {
            id,
            name: name.to_string(),
            dimension,
            metric,
            created_at,
        })
    }

    /// Retrieve metadata for a collection
    pub fn get_collection(&self, name: &str) -> Result<Option<EdgeCollection>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, dimension, metric, created_at FROM edge_vector_collections WHERE name = ?1")
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut rows = stmt
            .query(params![name])
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
            let metric_str: String = row.get(3).unwrap_or_default();
            let metric = EdgeDistanceMetric::from_str_name(&metric_str)
                .unwrap_or(EdgeDistanceMetric::Cosine);

            Ok(Some(EdgeCollection {
                id: row.get(0).map_err(|e| HgbError::Storage(e.to_string()))?,
                name: row.get(1).map_err(|e| HgbError::Storage(e.to_string()))?,
                dimension: row.get::<_, i64>(2).unwrap_or(0) as usize,
                metric,
                created_at: row.get(4).unwrap_or(0),
            }))
        } else {
            Ok(None)
        }
    }

    /// List all collections registered in this edge store
    pub fn list_collections(&self) -> Result<Vec<EdgeCollection>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, dimension, metric, created_at FROM edge_vector_collections ORDER BY name ASC")
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let metric_str: String = row.get(3)?;
                let metric = EdgeDistanceMetric::from_str_name(&metric_str)
                    .unwrap_or(EdgeDistanceMetric::Cosine);

                Ok(EdgeCollection {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    dimension: row.get::<_, i64>(2)? as usize,
                    metric,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
        }
        Ok(list)
    }

    /// Delete a collection and all associated vector items
    pub fn delete_collection(&self, name: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM edge_vector_collections WHERE name = ?1", params![name])
            .map_err(|e| HgbError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Upsert a single vector item
    pub fn upsert_item(&self, item: &EdgeVectorItem) -> Result<()> {
        let blob = encode_vector_blob(&item.embedding);
        let conn = self.conn.lock().unwrap();

        conn.execute(
            r#"
            INSERT INTO edge_vector_items (id, collection, embedding, document_chunk, metadata_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(id) DO UPDATE SET
                collection = excluded.collection,
                embedding = excluded.embedding,
                document_chunk = excluded.document_chunk,
                metadata_json = excluded.metadata_json,
                created_at = excluded.created_at
            "#,
            params![
                item.id,
                item.collection,
                blob,
                item.document_chunk,
                item.metadata_json,
                item.created_at
            ],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to upsert vector item '{}': {}", item.id, e)))?;

        Ok(())
    }

    /// Batch upsert multiple vector items within a single transaction
    pub fn upsert_batch(&self, items: &[EdgeVectorItem]) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| HgbError::Storage(format!("Failed to start transaction: {}", e)))?;

        {
            let mut stmt = tx
                .prepare(
                    r#"
                    INSERT INTO edge_vector_items (id, collection, embedding, document_chunk, metadata_json, created_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ON CONFLICT(id) DO UPDATE SET
                        collection = excluded.collection,
                        embedding = excluded.embedding,
                        document_chunk = excluded.document_chunk,
                        metadata_json = excluded.metadata_json,
                        created_at = excluded.created_at
                    "#,
                )
                .map_err(|e| HgbError::Storage(e.to_string()))?;

            for item in items {
                let blob = encode_vector_blob(&item.embedding);
                stmt.execute(params![
                    item.id,
                    item.collection,
                    blob,
                    item.document_chunk,
                    item.metadata_json,
                    item.created_at
                ])
                .map_err(|e| HgbError::Storage(format!("Failed inserting item '{}': {}", item.id, e)))?;
            }
        }

        tx.commit()
            .map_err(|e| HgbError::Storage(format!("Failed to commit batch: {}", e)))?;

        Ok(items.len())
    }

    /// Get an item by its primary key
    pub fn get_item(&self, id: &str) -> Result<Option<EdgeVectorItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, collection, embedding, document_chunk, metadata_json, created_at FROM edge_vector_items WHERE id = ?1")
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        let mut rows = stmt
            .query(params![id])
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
            let blob: Vec<u8> = row.get(2).map_err(|e| HgbError::Storage(e.to_string()))?;
            let embedding = decode_vector_blob(&blob)?;

            Ok(Some(EdgeVectorItem {
                id: row.get(0).map_err(|e| HgbError::Storage(e.to_string()))?,
                collection: row.get(1).map_err(|e| HgbError::Storage(e.to_string()))?,
                embedding,
                document_chunk: row.get(3).map_err(|e| HgbError::Storage(e.to_string()))?,
                metadata_json: row.get(4).unwrap_or_default(),
                created_at: row.get(5).unwrap_or(0),
            }))
        } else {
            Ok(None)
        }
    }

    /// Delete an item by ID
    pub fn delete_item(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute("DELETE FROM edge_vector_items WHERE id = ?1", params![id])
            .map_err(|e| HgbError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Count total items in a collection
    pub fn count_items(&self, collection: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edge_vector_items WHERE collection = ?1",
                params![collection],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count as usize)
    }

    /// Hybrid search combining coarse filtering, SIMD AVX2 distance scanning, and lexical reranking
    pub fn hybrid_search(&self, query: &HybridSearchQuery) -> Result<Vec<EdgeSearchResult>> {
        let collection = self
            .get_collection(&query.collection)?
            .ok_or_else(|| HgbError::NotFound(format!("Collection '{}' not found", query.collection)))?;

        if let Some(ref q_vec) = query.query_vector {
            if q_vec.len() != collection.dimension {
                return Err(HgbError::InvalidInput(format!(
                    "Query vector dimension {} does not match collection dimension {}",
                    q_vec.len(),
                    collection.dimension
                )));
            }
        }

        let conn = self.conn.lock().unwrap();

        // Fetch candidate rows
        let sql = if query.use_sql_prefilter && query.lexical_query.is_some() {
            "SELECT id, collection, embedding, document_chunk, metadata_json, created_at \
             FROM edge_vector_items WHERE collection = ?1 AND document_chunk LIKE ?2"
        } else {
            "SELECT id, collection, embedding, document_chunk, metadata_json, created_at \
             FROM edge_vector_items WHERE collection = ?1"
        };

        let mut stmt = conn.prepare(sql).map_err(|e| HgbError::Storage(e.to_string()))?;

        struct Candidate {
            id: String,
            collection: String,
            embedding_blob: Vec<u8>,
            document_chunk: String,
            metadata_json: String,
            created_at: i64,
        }

        let mut candidates = Vec::new();

        if query.use_sql_prefilter && query.lexical_query.is_some() {
            let pattern = format!("%{}%", query.lexical_query.as_ref().unwrap().trim());
            let mut rows = stmt
                .query(params![query.collection, pattern])
                .map_err(|e| HgbError::Storage(e.to_string()))?;
            while let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
                candidates.push(Candidate {
                    id: row.get(0).map_err(|e| HgbError::Storage(e.to_string()))?,
                    collection: row.get(1).map_err(|e| HgbError::Storage(e.to_string()))?,
                    embedding_blob: row.get(2).map_err(|e| HgbError::Storage(e.to_string()))?,
                    document_chunk: row.get(3).map_err(|e| HgbError::Storage(e.to_string()))?,
                    metadata_json: row.get(4).unwrap_or_default(),
                    created_at: row.get(5).unwrap_or(0),
                });
            }
        } else {
            let mut rows = stmt
                .query(params![query.collection])
                .map_err(|e| HgbError::Storage(e.to_string()))?;
            while let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
                candidates.push(Candidate {
                    id: row.get(0).map_err(|e| HgbError::Storage(e.to_string()))?,
                    collection: row.get(1).map_err(|e| HgbError::Storage(e.to_string()))?,
                    embedding_blob: row.get(2).map_err(|e| HgbError::Storage(e.to_string()))?,
                    document_chunk: row.get(3).map_err(|e| HgbError::Storage(e.to_string()))?,
                    metadata_json: row.get(4).unwrap_or_default(),
                    created_at: row.get(5).unwrap_or(0),
                });
            }
        }

        let mut scratch_vec = Vec::with_capacity(collection.dimension);
        let mut results = Vec::with_capacity(candidates.len());

        let lexical_tokens: Vec<String> = query
            .lexical_query
            .as_ref()
            .map(|lq| {
                lq.split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();

        for candidate in candidates {
            // Apply metadata filtering
            if let Some(ref filter) = query.metadata_filter {
                if !matches_metadata_filter(&candidate.metadata_json, filter) {
                    continue;
                }
            }

            // Vector scoring
            let (vector_score, distance) = if let Some(ref q_vec) = query.query_vector {
                if let Ok(()) = decode_vector_blob_into(&candidate.embedding_blob, &mut scratch_vec) {
                    match collection.metric {
                        EdgeDistanceMetric::Cosine => {
                            let sim = simd_kernels::cosine_similarity(q_vec, &scratch_vec);
                            let dist = (1.0 - sim).max(0.0);
                            let score = ((sim + 1.0) / 2.0).clamp(0.0, 1.0);
                            (score, dist)
                        }
                        EdgeDistanceMetric::Euclidean => {
                            let dist = simd_kernels::euclidean_distance(q_vec, &scratch_vec);
                            let score = 1.0 / (1.0 + dist);
                            (score, dist)
                        }
                        EdgeDistanceMetric::DotProduct => {
                            let dot = simd_kernels::dot_product(q_vec, &scratch_vec);
                            let dist = -dot;
                            let score = ((dot + 1.0) / 2.0).clamp(0.0, 1.0);
                            (score, dist)
                        }
                    }
                } else {
                    (0.0, f32::MAX)
                }
            } else {
                (0.0, 0.0)
            };

            // Lexical scoring
            let lexical_score = if !lexical_tokens.is_empty() {
                score_lexical_tokens(&candidate.document_chunk, &lexical_tokens, query.lexical_query.as_deref())
            } else {
                0.0
            };

            // Combine scores
            let combined_score = match (query.query_vector.is_some(), !lexical_tokens.is_empty()) {
                (true, true) => query.alpha * vector_score + (1.0 - query.alpha) * lexical_score,
                (true, false) => vector_score,
                (false, true) => lexical_score,
                (false, false) => 0.0,
            };

            results.push(EdgeSearchResult {
                id: candidate.id,
                collection: candidate.collection,
                document_chunk: candidate.document_chunk,
                metadata_json: candidate.metadata_json,
                vector_score,
                lexical_score,
                combined_score,
                distance,
                created_at: candidate.created_at,
            });
        }

        // Sort descending by combined_score
        results.sort_by(|a, b| b.combined_score.partial_cmp(&a.combined_score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(query.limit);

        Ok(results)
    }

    /// Pure SIMD vector nearest-neighbor search
    pub fn vector_search(
        &self,
        collection: &str,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<EdgeSearchResult>> {
        self.hybrid_search(&HybridSearchQuery {
            collection: collection.to_string(),
            query_vector: Some(query_vector.to_vec()),
            lexical_query: None,
            metadata_filter: None,
            alpha: 1.0,
            limit,
            use_sql_prefilter: false,
        })
    }

    /// Pure lexical search across documents
    pub fn lexical_search(
        &self,
        collection: &str,
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<EdgeSearchResult>> {
        self.hybrid_search(&HybridSearchQuery {
            collection: collection.to_string(),
            query_vector: None,
            lexical_query: Some(query_text.to_string()),
            metadata_filter: None,
            alpha: 0.0,
            limit,
            use_sql_prefilter: false,
        })
    }

    /// Micro-benchmark to verify sub-millisecond retrieval latency
    pub fn benchmark_search(
        &self,
        collection: &str,
        query_vector: &[f32],
        iterations: usize,
    ) -> Result<EdgeSearchBenchmarkReport> {
        let total_items = self.count_items(collection)?;
        let start = Instant::now();

        let query = HybridSearchQuery {
            collection: collection.to_string(),
            query_vector: Some(query_vector.to_vec()),
            lexical_query: None,
            metadata_filter: None,
            alpha: 1.0,
            limit: 10,
            use_sql_prefilter: false,
        };

        for _ in 0..iterations {
            let res = self.hybrid_search(&query)?;
            let _ = res.len();
        }

        let total_elapsed_us = start.elapsed().as_micros() as u64;
        let avg_latency_us = if iterations > 0 {
            total_elapsed_us as f64 / iterations as f64
        } else {
            0.0
        };

        let queries_per_second = if avg_latency_us > 0.0 {
            1_000_000.0 / avg_latency_us
        } else {
            0.0
        };

        Ok(EdgeSearchBenchmarkReport {
            collection: collection.to_string(),
            total_items,
            dimension: query_vector.len(),
            iterations,
            total_elapsed_us,
            avg_latency_us,
            sub_millisecond: avg_latency_us < 1000.0,
            queries_per_second,
        })
    }
}

/// Helper to score lexical token overlap and phrase matches
fn score_lexical_tokens(doc_chunk: &str, query_tokens: &[String], raw_query: Option<&str>) -> f32 {
    if query_tokens.is_empty() {
        return 0.0;
    }
    let doc_lower = doc_chunk.to_lowercase();
    let mut score = 0.0f32;

    // Full phrase exact match bonus
    if let Some(raw) = raw_query {
        let raw_clean = raw.trim().to_lowercase();
        if !raw_clean.is_empty() && doc_lower.contains(&raw_clean) {
            score += 0.4;
        }
    }

    // Individual token coverage
    let mut matched = 0;
    for token in query_tokens {
        if doc_lower.contains(token) {
            matched += 1;
        }
    }

    let token_ratio = matched as f32 / query_tokens.len() as f32;
    score += 0.6 * token_ratio;

    score.clamp(0.0, 1.0)
}

/// Helper to match JSON metadata filters
fn matches_metadata_filter(
    metadata_json: &str,
    filter: &HashMap<String, serde_json::Value>,
) -> bool {
    let parsed: serde_json::Value = match serde_json::from_str(metadata_json) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return false,
    };

    for (k, v) in filter {
        if let Some(actual) = obj.get(k) {
            if actual != v {
                return false;
            }
        } else {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_blob_serialization_roundtrip() {
        let vec = vec![1.0f32, 2.0f32, 3.0f32, -0.5f32, 0.0f32];
        let blob = encode_vector_blob(&vec);

        // Verify sqlite-vec byte length: exactly 4 bytes per dimension
        assert_eq!(blob.len(), vec.len() * 4);

        // Verify IEEE-754 little-endian representation
        assert_eq!(&blob[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&blob[4..8], &2.0f32.to_le_bytes());
        assert_eq!(&blob[8..12], &3.0f32.to_le_bytes());

        let decoded = decode_vector_blob(&blob).expect("decode failed");
        assert_eq!(decoded, vec);

        // Verify invalid length rejection
        let invalid_blob = vec![0u8; 15]; // Not multiple of 4
        assert!(decode_vector_blob(&invalid_blob).is_err());
    }

    #[test]
    fn test_sqlite_persistence_collections_and_items() {
        let engine = VectorNativeEdgeEngine::open_in_memory().expect("open memory db");

        let coll = engine
            .create_collection("rust_symbols", 4, EdgeDistanceMetric::Cosine)
            .expect("create collection");
        assert_eq!(coll.name, "rust_symbols");
        assert_eq!(coll.dimension, 4);

        let item1 = EdgeVectorItem {
            id: "sym_001".to_string(),
            collection: "rust_symbols".to_string(),
            embedding: vec![1.0, 0.0, 0.0, 0.0],
            document_chunk: "pub fn zero_copy_ring_buffer()".to_string(),
            metadata_json: r#"{"crate": "hgb-core", "type": "function"}"#.to_string(),
            created_at: 1000,
        };

        let item2 = EdgeVectorItem {
            id: "sym_002".to_string(),
            collection: "rust_symbols".to_string(),
            embedding: vec![0.0, 1.0, 0.0, 0.0],
            document_chunk: "pub struct ShmHeader".to_string(),
            metadata_json: r#"{"crate": "hgb-core", "type": "struct"}"#.to_string(),
            created_at: 1001,
        };

        engine.upsert_batch(&[item1, item2]).expect("batch upsert");
        assert_eq!(engine.count_items("rust_symbols").unwrap(), 2);

        let fetched = engine.get_item("sym_001").unwrap().expect("item found");
        assert_eq!(fetched.document_chunk, "pub fn zero_copy_ring_buffer()");
        assert_eq!(fetched.embedding, vec![1.0, 0.0, 0.0, 0.0]);

        // Cascading delete
        assert!(engine.delete_collection("rust_symbols").unwrap());
        assert_eq!(engine.count_items("rust_symbols").unwrap(), 0);
    }

    #[test]
    fn test_simd_avx2_cosine_and_euclidean() {
        let v1 = vec![1.0f32, 0.0f32, 0.0f32, 0.0f32, 0.5f32, 0.5f32, 0.0f32, 0.0f32];
        let v2 = vec![1.0f32, 0.0f32, 0.0f32, 0.0f32, 0.5f32, 0.5f32, 0.0f32, 0.0f32];
        let v3 = vec![0.0f32, 1.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.5f32, 0.5f32];

        // Identical vectors: cosine = 1.0, euclidean = 0.0
        let cos_ident = simd_kernels::cosine_similarity(&v1, &v2);
        assert!((cos_ident - 1.0).abs() < 1e-6);

        let dist_ident = simd_kernels::euclidean_distance(&v1, &v2);
        assert!(dist_ident.abs() < 1e-6);

        // Orthogonal components: cosine < 1.0, euclidean > 0.0
        let cos_diff = simd_kernels::cosine_similarity(&v1, &v3);
        assert!(cos_diff < 0.5);

        let dist_diff = simd_kernels::euclidean_distance(&v1, &v3);
        assert!(dist_diff > 0.0);
    }

    #[test]
    fn test_hybrid_search_accuracy() {
        let engine = VectorNativeEdgeEngine::open_in_memory().expect("open memory db");
        engine
            .create_collection("docs_knowledge", 4, EdgeDistanceMetric::Cosine)
            .expect("create coll");

        let items = vec![
            EdgeVectorItem {
                id: "doc_auth".to_string(),
                collection: "docs_knowledge".to_string(),
                embedding: vec![0.9, 0.1, 0.0, 0.0],
                document_chunk: "OAuth2 bearer token security authentication flow".to_string(),
                metadata_json: r#"{"category": "security"}"#.to_string(),
                created_at: 100,
            },
            EdgeVectorItem {
                id: "doc_db".to_string(),
                collection: "docs_knowledge".to_string(),
                embedding: vec![0.0, 0.9, 0.1, 0.0],
                document_chunk: "SQLite database WAL journal connection pooling".to_string(),
                metadata_json: r#"{"category": "storage"}"#.to_string(),
                created_at: 101,
            },
            EdgeVectorItem {
                id: "doc_simd".to_string(),
                collection: "docs_knowledge".to_string(),
                embedding: vec![0.0, 0.0, 0.9, 0.1],
                document_chunk: "SIMD AVX2 vector dot product kernel optimization".to_string(),
                metadata_json: r#"{"category": "compute"}"#.to_string(),
                created_at: 102,
            },
        ];

        engine.upsert_batch(&items).expect("insert");

        // Hybrid query: vector close to doc_auth, lexical matching "OAuth2 token"
        let q = HybridSearchQuery {
            collection: "docs_knowledge".to_string(),
            query_vector: Some(vec![0.85, 0.15, 0.0, 0.0]),
            lexical_query: Some("OAuth2 token".to_string()),
            metadata_filter: None,
            alpha: 0.6,
            limit: 2,
            use_sql_prefilter: false,
        };

        let results = engine.hybrid_search(&q).expect("search");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "doc_auth");
        assert!(results[0].combined_score > results[1].combined_score);

        // Metadata filter query
        let mut filter = HashMap::new();
        filter.insert("category".to_string(), serde_json::Value::String("storage".to_string()));

        let q_filtered = HybridSearchQuery {
            collection: "docs_knowledge".to_string(),
            query_vector: Some(vec![0.0, 0.9, 0.1, 0.0]),
            lexical_query: None,
            metadata_filter: Some(filter),
            alpha: 1.0,
            limit: 10,
            use_sql_prefilter: false,
        };

        let filtered_res = engine.hybrid_search(&q_filtered).expect("filtered search");
        assert_eq!(filtered_res.len(), 1);
        assert_eq!(filtered_res[0].id, "doc_db");
    }

    #[test]
    fn test_sub_millisecond_retrieval() {
        let engine = VectorNativeEdgeEngine::open_in_memory().expect("open memory db");
        let dim = 32;
        engine
            .create_collection("bench_coll", dim, EdgeDistanceMetric::Cosine)
            .expect("create coll");

        let mut items = Vec::new();
        for i in 0..200 {
            let mut emb = vec![0.0f32; dim];
            emb[i % dim] = 1.0;
            emb[(i + 1) % dim] = 0.5;

            items.push(EdgeVectorItem {
                id: format!("bench_item_{:04}", i),
                collection: "bench_coll".to_string(),
                embedding: emb,
                document_chunk: format!("Source code chunk indexing item #{}", i),
                metadata_json: r#"{"status": "indexed"}"#.to_string(),
                created_at: i as i64,
            });
        }
        engine.upsert_batch(&items).expect("batch insert");

        let mut q_vec = vec![0.0f32; dim];
        q_vec[0] = 1.0;

        let rep = engine.benchmark_search("bench_coll", &q_vec, 50).expect("benchmark");
        if cfg!(debug_assertions) {
            assert!(rep.avg_latency_us < 6000.0, "Latency was {} us (expected < 6000 us in unoptimized debug)", rep.avg_latency_us);
            assert!(rep.queries_per_second > 100.0);
        } else {
            assert!(rep.sub_millisecond, "Latency was {} us (expected < 1000 us in release)", rep.avg_latency_us);
            assert!(rep.queries_per_second > 1000.0);
        }
    }
}
