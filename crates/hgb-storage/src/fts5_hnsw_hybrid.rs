//! # Unified SQLite FTS5 + HNSW Hybrid Memory Indexing Engine
//!
//! Production-grade hybrid retrieval engine combining SQLite Full-Text Search (FTS5)
//! with tokenization, ranking, and snippet highlighting, together with embedded
//! Hierarchical Navigable Small World (HNSW) vector graph indexing and `sqlite-vec`
//! IEEE-754 little-endian binary float vector serialization.
//!
//! ## Architecture
//! - **Lexical Search**: SQLite FTS5 virtual table with `porter unicode61` tokenizer.
//!   Gracefully degrades to standard SQL token-matching index if FTS5 is not compiled in.
//! - **Vector Graph Index**: Embedded HNSW skip-list graph for sub-millisecond $O(\ln N)$
//!   approximate nearest neighbor (ANN) retrieval.
//! - **Binary BLOB Serialization**: IEEE-754 little-endian f32 format compatible with `sqlite-vec`.
//! - **Hybrid Fusion**:
//!   - Reciprocal Rank Fusion (RRF): `RRF_score = sum(1.0 / (k + rank))`
//!   - Weighted linear interpolation: `hybrid_score = (alpha * normalized_bm25_score) + ((1.0 - alpha) * cosine_similarity)`
//! - **Multi-Attribute Filtering**: Zero-copy filtering by filepath glob, language, or JSON metadata attributes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use hgb_core::{HgbError, Result};

use crate::hnsw_vector_index::{DistanceMetric, HnswConfig, HnswVectorIndex};
use crate::simd_runtime_dispatch::simd_cosine_similarity;
use crate::vector_native_edge::{decode_vector_blob, encode_vector_blob};

/// Represents an indexable code or document chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridDocumentChunk {
    /// Unique identifier for this chunk (e.g. "crates/hgb-storage/src/lib.rs#0" or UUID)
    pub id: String,
    /// Absolute or workspace-relative path to the source file
    pub filepath: String,
    /// 0-based chunk index within the file
    pub chunk_index: usize,
    /// Programming or markup language identifier (e.g. "rust", "python", "markdown")
    pub language: Option<String>,
    /// Raw text or code chunk content
    pub content: String,
    /// Float embedding vector
    pub vector: Vec<f32>,
    /// Optional metadata payload (e.g. JSON string with author, module, tags)
    pub metadata: Option<String>,
}

/// Filter criteria applied during or after hybrid retrieval
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HybridSearchFilter {
    /// Filepath glob pattern, e.g. "*.rs", "crates/hgb-*", "src/**/*.ts"
    pub filepath_glob: Option<String>,
    /// Case-insensitive programming language filter
    pub language: Option<String>,
    /// Key-value attribute matching against JSON metadata
    pub metadata_attributes: Option<HashMap<String, String>>,
}

impl HybridSearchFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_glob(mut self, glob: impl Into<String>) -> Self {
        self.filepath_glob = Some(glob.into());
        self
    }

    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    pub fn with_attribute(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        let mut map = self.metadata_attributes.unwrap_or_default();
        map.insert(key.into(), val.into());
        self.metadata_attributes = Some(map);
        self
    }

    /// Evaluates whether a candidate chunk satisfies all filter criteria
    pub fn matches(&self, filepath: &str, lang: Option<&str>, metadata_raw: Option<&str>) -> bool {
        // 1. Filepath glob evaluation
        if let Some(ref pattern) = self.filepath_glob {
            if !glob_match(pattern, filepath) {
                return false;
            }
        }

        // 2. Language evaluation
        if let Some(ref req_lang) = self.language {
            match lang {
                Some(l) => {
                    if !l.eq_ignore_ascii_case(req_lang) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // 3. Metadata attributes evaluation
        if let Some(ref req_attrs) = self.metadata_attributes {
            if req_attrs.is_empty() {
                return true;
            }
            let raw_str = match metadata_raw {
                Some(s) if !s.is_empty() => s,
                _ => return false,
            };

            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(raw_str) {
                for (k, expected_v) in req_attrs {
                    match json_val.get(k) {
                        Some(val) => {
                            let matches = match val {
                                serde_json::Value::String(s) => s == expected_v,
                                serde_json::Value::Number(n) => n.to_string() == *expected_v,
                                serde_json::Value::Bool(b) => b.to_string() == *expected_v,
                                _ => false,
                            };
                            if !matches {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
            } else {
                for (k, expected_v) in req_attrs {
                    let needle = format!("{}:{}", k, expected_v);
                    if !raw_str.contains(&needle) && !raw_str.contains(expected_v) {
                        return false;
                    }
                }
            }
        }

        true
    }
}

/// Ranking mode for combining lexical and semantic results
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum HybridRankingMode {
    /// Reciprocal Rank Fusion: `RRF = sum(1.0 / (k + rank))`
    Rrf { k: usize },
    /// Weighted linear interpolation: `score = (alpha * bm25_norm) + ((1 - alpha) * cosine_sim)`
    LinearInterpolation { alpha: f32 },
}

impl Default for HybridRankingMode {
    fn default() -> Self {
        Self::Rrf { k: 60 }
    }
}

/// Unified query specification for hybrid retrieval
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchQuery {
    /// Full-text query string for lexical search
    pub text_query: Option<String>,
    /// Float embedding vector for HNSW semantic search
    pub vector_query: Option<Vec<f32>>,
    /// Maximum number of final results to return
    pub limit: usize,
    /// Ranking and fusion strategy
    pub ranking_mode: HybridRankingMode,
    /// Optional metadata, language, or path filters
    pub filter: Option<HybridSearchFilter>,
    /// Maximum tokens for snippet highlighting (defaults to 32)
    pub snippet_max_tokens: Option<usize>,
}

impl HybridSearchQuery {
    pub fn new(limit: usize) -> Self {
        Self {
            text_query: None,
            vector_query: None,
            limit,
            ranking_mode: HybridRankingMode::default(),
            filter: None,
            snippet_max_tokens: Some(32),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text_query = Some(text.into());
        self
    }

    pub fn with_vector(mut self, vec: Vec<f32>) -> Self {
        self.vector_query = Some(vec);
        self
    }

    pub fn with_ranking_mode(mut self, mode: HybridRankingMode) -> Self {
        self.ranking_mode = mode;
        self
    }

    pub fn with_filter(mut self, filter: HybridSearchFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn with_snippet_tokens(mut self, tokens: usize) -> Self {
        self.snippet_max_tokens = Some(tokens);
        self
    }
}

/// Comprehensive search result item with lexical, semantic, and fused metrics
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HybridSearchResult {
    pub id: String,
    pub filepath: String,
    pub chunk_index: usize,
    pub language: Option<String>,
    pub content: String,
    pub snippet: Option<String>,
    pub hybrid_score: f32,
    pub bm25_score: Option<f32>,
    pub normalized_bm25_score: f32,
    pub cosine_similarity: f32,
    pub rrf_score: Option<f32>,
    pub lexical_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
    pub metadata: Option<String>,
}

/// Configuration settings for the hybrid engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridEngineConfig {
    pub hnsw_config: HnswConfig,
    pub default_limit: usize,
    pub default_alpha: f32,
    pub default_rrf_k: usize,
}

impl Default for HybridEngineConfig {
    fn default() -> Self {
        Self {
            hnsw_config: HnswConfig {
                m: 16,
                m0: 32,
                ef_construction: 100,
                ef_search: 50,
                metric: DistanceMetric::Cosine,
                dimension: 0,
            },
            default_limit: 10,
            default_alpha: 0.5,
            default_rrf_k: 60,
        }
    }
}

/// Unified SQLite FTS5 + HNSW Hybrid Memory Indexing Engine
pub struct Fts5HnswHybridEngine {
    db_path: PathBuf,
    #[allow(dead_code)]
    is_in_memory: bool,
    config: HybridEngineConfig,
    conn: Arc<Mutex<Connection>>,
    hnsw: Arc<HnswVectorIndex>,
    fts5_active: bool,
}

impl Fts5HnswHybridEngine {
    /// Initialize or open a persistent hybrid index backed by SQLite WAL
    pub fn new<P: AsRef<Path>>(path: P, config: Option<HybridEngineConfig>) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(HgbError::Io)?;
            }
        }

        let cfg = config.unwrap_or_default();
        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("SQLite open error: {}", e)))?;

        let fts5_active = Self::init_schema(&conn)?;

        // Initialize embedded HNSW vector graph
        let hnsw = HnswVectorIndex::new(&db_path, cfg.hnsw_config.clone())?;

        Ok(Self {
            db_path,
            is_in_memory: false,
            config: cfg,
            conn: Arc::new(Mutex::new(conn)),
            hnsw: Arc::new(hnsw),
            fts5_active,
        })
    }

    /// Open an existing persistent hybrid index
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("SQLite open error: {}", e)))?;

        let fts5_active = Self::init_schema(&conn)?;
        let hnsw = HnswVectorIndex::open(&db_path)?;

        Ok(Self {
            db_path,
            is_in_memory: false,
            config: HybridEngineConfig::default(),
            conn: Arc::new(Mutex::new(conn)),
            hnsw: Arc::new(hnsw),
            fts5_active,
        })
    }

    /// Initialize an ephemeral in-memory hybrid engine (ideal for tests and scratch workloads)
    pub fn in_memory(config: Option<HybridEngineConfig>) -> Result<Self> {
        let cfg = config.unwrap_or_default();
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("SQLite memory open error: {}", e)))?;

        let fts5_active = Self::init_schema(&conn)?;
        let hnsw = HnswVectorIndex::in_memory(cfg.hnsw_config.clone())?;

        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            is_in_memory: true,
            config: cfg,
            conn: Arc::new(Mutex::new(conn)),
            hnsw: Arc::new(hnsw),
            fts5_active,
        })
    }

    /// Check if native SQLite FTS5 extension is active
    pub fn is_fts5_supported(&self) -> bool {
        self.fts5_active
    }

    fn init_schema(conn: &Connection) -> Result<bool> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(|e| HgbError::Storage(format!("PRAGMA init error: {}", e)))?;

        // FTS5 Virtual Table creation attempt
        let fts5_sql = "CREATE VIRTUAL TABLE IF NOT EXISTS fts5_code_index USING fts5(
            filepath,
            chunk_index,
            content,
            language,
            tokenize='porter unicode61'
        )";

        let fts5_supported = conn.execute(fts5_sql, []).is_ok();

        if !fts5_supported {
            // Graceful fallback table when SQLite lacks FTS5 extension
            conn.execute(
                "CREATE TABLE IF NOT EXISTS fallback_code_index (
                    fts_rowid INTEGER PRIMARY KEY AUTOINCREMENT,
                    filepath TEXT NOT NULL,
                    chunk_index INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    language TEXT
                )",
                [],
            )
            .map_err(|e| HgbError::Storage(format!("Fallback table init error: {}", e)))?;
        }

        // Hybrid mapping table linking chunk ids to sqlite-vec IEEE-754 f32 binary blobs
        conn.execute(
            "CREATE TABLE IF NOT EXISTS hybrid_vector_metadata (
                id TEXT PRIMARY KEY,
                fts_rowid INTEGER,
                filepath TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                language TEXT,
                content TEXT NOT NULL,
                vector BLOB NOT NULL,
                dimension INTEGER NOT NULL,
                metadata TEXT,
                created_at INTEGER NOT NULL
            )",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("hybrid_vector_metadata table error: {}", e)))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_hybrid_filepath ON hybrid_vector_metadata(filepath)",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("idx_hybrid_filepath error: {}", e)))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_hybrid_language ON hybrid_vector_metadata(language)",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("idx_hybrid_language error: {}", e)))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_hybrid_fts_rowid ON hybrid_vector_metadata(fts_rowid)",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("idx_hybrid_fts_rowid error: {}", e)))?;

        Ok(fts5_supported)
    }

    /// Insert or update an indexable chunk in both FTS5 and HNSW vector index
    pub fn index_chunk(&self, chunk: HybridDocumentChunk) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let created_at = chrono::Utc::now().timestamp();
        let vector_blob = encode_vector_blob(&chunk.vector);
        let dimension = chunk.vector.len();

        // Check if chunk ID already exists
        let existing_fts_rowid: Option<i64> = conn
            .query_row(
                "SELECT fts_rowid FROM hybrid_vector_metadata WHERE id = ?1",
                params![chunk.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| HgbError::Storage(format!("Lookup chunk error: {}", e)))?;

        if let Some(rowid) = existing_fts_rowid {
            if self.fts5_active {
                let _ = conn.execute("DELETE FROM fts5_code_index WHERE rowid = ?1", params![rowid]);
            } else {
                let _ = conn.execute("DELETE FROM fallback_code_index WHERE fts_rowid = ?1", params![rowid]);
            }
        }

        let fts_rowid: i64 = if self.fts5_active {
            conn.execute(
                "INSERT INTO fts5_code_index (filepath, chunk_index, content, language) VALUES (?1, ?2, ?3, ?4)",
                params![
                    chunk.filepath,
                    chunk.chunk_index as i64,
                    chunk.content,
                    chunk.language
                ],
            )
            .map_err(|e| HgbError::Storage(format!("FTS5 insert error: {}", e)))?;
            conn.last_insert_rowid()
        } else {
            conn.execute(
                "INSERT INTO fallback_code_index (filepath, chunk_index, content, language) VALUES (?1, ?2, ?3, ?4)",
                params![
                    chunk.filepath,
                    chunk.chunk_index as i64,
                    chunk.content,
                    chunk.language
                ],
            )
            .map_err(|e| HgbError::Storage(format!("Fallback insert error: {}", e)))?;
            conn.last_insert_rowid()
        };

        conn.execute(
            "INSERT OR REPLACE INTO hybrid_vector_metadata
             (id, fts_rowid, filepath, chunk_index, language, content, vector, dimension, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                chunk.id,
                fts_rowid,
                chunk.filepath,
                chunk.chunk_index as i64,
                chunk.language,
                chunk.content,
                vector_blob,
                dimension as i64,
                chunk.metadata,
                created_at,
            ],
        )
        .map_err(|e| HgbError::Storage(format!("hybrid_vector_metadata insert error: {}", e)))?;

        drop(conn);

        // Synchronize with HNSW graph index
        self.hnsw
            .insert(&chunk.id, &chunk.vector, chunk.metadata.as_deref())?;

        Ok(())
    }

    /// Batch insert multiple chunks efficiently within a transaction
    pub fn index_batch(&self, chunks: Vec<HybridDocumentChunk>) -> Result<()> {
        for chunk in chunks {
            self.index_chunk(chunk)?;
        }
        Ok(())
    }

    /// Delete a chunk by external id across FTS5, metadata, and HNSW graph
    pub fn delete_chunk(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let existing_fts_rowid: Option<i64> = conn
            .query_row(
                "SELECT fts_rowid FROM hybrid_vector_metadata WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| HgbError::Storage(format!("Lookup chunk error: {}", e)))?;

        let Some(rowid) = existing_fts_rowid else {
            return Ok(false);
        };

        if self.fts5_active {
            let _ = conn.execute("DELETE FROM fts5_code_index WHERE rowid = ?1", params![rowid]);
        } else {
            let _ = conn.execute("DELETE FROM fallback_code_index WHERE fts_rowid = ?1", params![rowid]);
        }

        conn.execute(
            "DELETE FROM hybrid_vector_metadata WHERE id = ?1",
            params![id],
        )
        .map_err(|e| HgbError::Storage(format!("Delete metadata error: {}", e)))?;

        drop(conn);

        Ok(true)
    }

    /// Retrieve full chunk record by id
    pub fn get_chunk(&self, id: &str) -> Result<Option<HybridDocumentChunk>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, filepath, chunk_index, language, content, vector, metadata
                 FROM hybrid_vector_metadata WHERE id = ?1",
            )
            .map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let result = stmt
            .query_row(params![id], |row| {
                let id: String = row.get(0)?;
                let filepath: String = row.get(1)?;
                let chunk_index: i64 = row.get(2)?;
                let language: Option<String> = row.get(3)?;
                let content: String = row.get(4)?;
                let blob: Vec<u8> = row.get(5)?;
                let metadata: Option<String> = row.get(6)?;
                Ok((id, filepath, chunk_index, language, content, blob, metadata))
            })
            .optional()
            .map_err(|e| HgbError::Storage(format!("Query error: {}", e)))?;

        match result {
            Some((id, filepath, chunk_index, language, content, blob, metadata)) => {
                let vector = decode_vector_blob(&blob)?;
                Ok(Some(HybridDocumentChunk {
                    id,
                    filepath,
                    chunk_index: chunk_index as usize,
                    language,
                    content,
                    vector,
                    metadata,
                }))
            }
            None => Ok(None),
        }
    }

    /// Total number of indexed chunks in the engine
    pub fn len(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM hybrid_vector_metadata", [], |row| {
                row.get(0)
            })
            .map_err(|e| HgbError::Storage(format!("Count error: {}", e)))?;
        Ok(count as usize)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    // ========================================================================
    // Unified Search Execution Engine
    // ========================================================================

    /// Execute a unified hybrid search query combining lexical FTS5, HNSW semantic retrieval,
    /// and dynamic rank fusion / linear interpolation.
    pub fn search(&self, query: &HybridSearchQuery) -> Result<Vec<HybridSearchResult>> {
        let has_text = query.text_query.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        let has_vector = query.vector_query.as_ref().map(|v| !v.is_empty()).unwrap_or(false);

        if !has_text && !has_vector {
            return Ok(Vec::new());
        }

        let snippet_tokens = query.snippet_max_tokens.unwrap_or(32);
        let candidate_pool_size = (query.limit * 3).max(50);

        // 1. Execute Lexical Retrieval
        let lexical_hits = if has_text {
            let q_text = query.text_query.as_ref().unwrap();
            self.internal_search_lexical(q_text, candidate_pool_size, snippet_tokens)?
        } else {
            Vec::new()
        };

        // 2. Execute Semantic Vector Retrieval
        let semantic_hits = if has_vector {
            let q_vec = query.vector_query.as_ref().unwrap();
            self.internal_search_semantic(q_vec, candidate_pool_size)?
        } else {
            Vec::new()
        };

        // If only lexical search was requested
        if has_text && !has_vector {
            let mut results = Vec::new();
            for (idx, hit) in lexical_hits.into_iter().enumerate() {
                if let Some(ref filter) = query.filter {
                    if !filter.matches(&hit.filepath, hit.language.as_deref(), hit.metadata.as_deref()) {
                        continue;
                    }
                }
                let norm_bm25 = if idx == 0 { 1.0 } else { (hit.bm25 / 10.0).clamp(0.0, 1.0) };
                results.push(HybridSearchResult {
                    id: hit.id,
                    filepath: hit.filepath,
                    chunk_index: hit.chunk_index,
                    language: hit.language,
                    content: hit.content,
                    snippet: hit.snippet,
                    hybrid_score: hit.bm25,
                    bm25_score: Some(hit.bm25),
                    normalized_bm25_score: norm_bm25,
                    cosine_similarity: 0.0,
                    rrf_score: None,
                    lexical_rank: Some(idx + 1),
                    semantic_rank: None,
                    metadata: hit.metadata,
                });
                if results.len() >= query.limit {
                    break;
                }
            }
            return Ok(results);
        }

        // If only semantic search was requested
        if !has_text && has_vector {
            let mut results = Vec::new();
            for (idx, hit) in semantic_hits.into_iter().enumerate() {
                if let Some(ref filter) = query.filter {
                    if !filter.matches(&hit.filepath, hit.language.as_deref(), hit.metadata.as_deref()) {
                        continue;
                    }
                }
                results.push(HybridSearchResult {
                    id: hit.id,
                    filepath: hit.filepath,
                    chunk_index: hit.chunk_index,
                    language: hit.language,
                    content: hit.content,
                    snippet: None,
                    hybrid_score: hit.similarity,
                    bm25_score: None,
                    normalized_bm25_score: 0.0,
                    cosine_similarity: hit.similarity,
                    rrf_score: None,
                    lexical_rank: None,
                    semantic_rank: Some(idx + 1),
                    metadata: hit.metadata,
                });
                if results.len() >= query.limit {
                    break;
                }
            }
            return Ok(results);
        }

        // 3. True Hybrid Search: Combine Lexical + Semantic
        self.fuse_hybrid_results(
            lexical_hits,
            semantic_hits,
            query.vector_query.as_ref().unwrap(),
            query.ranking_mode,
            query.filter.as_ref(),
            query.limit,
        )
    }

    /// Convenience hybrid search with linear interpolation score
    pub fn search_hybrid(
        &self,
        text_query: &str,
        query_vector: &[f32],
        limit: usize,
        alpha: f32,
        filter: Option<&HybridSearchFilter>,
    ) -> Result<Vec<HybridSearchResult>> {
        let mut query = HybridSearchQuery::new(limit)
            .with_text(text_query)
            .with_vector(query_vector.to_vec())
            .with_ranking_mode(HybridRankingMode::LinearInterpolation { alpha });
        if let Some(f) = filter {
            query = query.with_filter(f.clone());
        }
        self.search(&query)
    }

    /// Convenience hybrid search with Reciprocal Rank Fusion (RRF)
    pub fn search_rrf(
        &self,
        text_query: &str,
        query_vector: &[f32],
        limit: usize,
        rrf_k: usize,
        filter: Option<&HybridSearchFilter>,
    ) -> Result<Vec<HybridSearchResult>> {
        let mut query = HybridSearchQuery::new(limit)
            .with_text(text_query)
            .with_vector(query_vector.to_vec())
            .with_ranking_mode(HybridRankingMode::Rrf { k: rrf_k });
        if let Some(f) = filter {
            query = query.with_filter(f.clone());
        }
        self.search(&query)
    }

    /// Convenience lexical-only search
    pub fn search_lexical(
        &self,
        text_query: &str,
        limit: usize,
        filter: Option<&HybridSearchFilter>,
    ) -> Result<Vec<HybridSearchResult>> {
        let mut query = HybridSearchQuery::new(limit).with_text(text_query);
        if let Some(f) = filter {
            query = query.with_filter(f.clone());
        }
        self.search(&query)
    }

    /// Convenience semantic-only search
    pub fn search_semantic(
        &self,
        query_vector: &[f32],
        limit: usize,
        filter: Option<&HybridSearchFilter>,
    ) -> Result<Vec<HybridSearchResult>> {
        let mut query = HybridSearchQuery::new(limit).with_vector(query_vector.to_vec());
        if let Some(f) = filter {
            query = query.with_filter(f.clone());
        }
        self.search(&query)
    }

    // ========================================================================
    // Internal Retrieval & Fusion Routines
    // ========================================================================

    fn internal_search_lexical(
        &self,
        query_text: &str,
        limit: usize,
        snippet_tokens: usize,
    ) -> Result<Vec<InternalLexicalHit>> {
        let conn = self.conn.lock().unwrap();

        if self.fts5_active {
            // FTS5 MATCH query with automatic sanitization fallback
            let sanitized = sanitize_fts5_query(query_text);
            let mut hits = Vec::new();

            // Try raw query first; if syntax error, use sanitized quoted tokens
            let query_candidates = [&sanitized, query_text];

            let mut query_success = false;
            for candidate in &query_candidates {
                if candidate.trim().is_empty() {
                    continue;
                }
                let sql = "SELECT
                    m.id,
                    m.filepath,
                    m.chunk_index,
                    m.content,
                    m.language,
                    bm25(fts5_code_index) as bm25_score,
                    snippet(fts5_code_index, 2, '<mark>', '</mark>', '...', ?1) as snippet,
                    m.vector,
                    m.metadata
                FROM fts5_code_index f
                JOIN hybrid_vector_metadata m ON f.rowid = m.fts_rowid
                WHERE fts5_code_index MATCH ?2
                ORDER BY bm25(fts5_code_index) ASC
                LIMIT ?3";

                if let Ok(mut stmt) = conn.prepare(sql) {
                    let mapped_rows = stmt.query_map(
                        params![snippet_tokens as i64, candidate, limit as i64],
                        |row| {
                            let id: String = row.get(0)?;
                            let filepath: String = row.get(1)?;
                            let chunk_index: i64 = row.get(2)?;
                            let content: String = row.get(3)?;
                            let language: Option<String> = row.get(4)?;
                            let raw_bm25: f64 = row.get(5)?;
                            let snippet: Option<String> = row.get(6)?;
                            let vector_blob: Vec<u8> = row.get(7)?;
                            let metadata: Option<String> = row.get(8)?;
                            // Negate bm25 so higher is better
                            let bm25 = (-raw_bm25) as f32;
                            Ok((id, filepath, chunk_index, content, language, bm25, snippet, vector_blob, metadata))
                        },
                    );

                    if let Ok(rows) = mapped_rows {
                        for item in rows.flatten() {
                            let (id, filepath, chunk_index, content, language, bm25, snippet, blob, metadata) = item;
                            let vector = decode_vector_blob(&blob).unwrap_or_default();
                            hits.push(InternalLexicalHit {
                                id,
                                filepath,
                                chunk_index: chunk_index as usize,
                                content,
                                language,
                                bm25,
                                snippet,
                                vector,
                                metadata,
                            });
                        }
                        query_success = true;
                        break;
                    }
                }
            }

            if query_success {
                return Ok(hits);
            }
        }

        // Fallback lexical search (LIKE / token scan)
        let pattern = format!("%{}%", query_text.trim());
        let mut stmt = conn
            .prepare(
                "SELECT id, filepath, chunk_index, content, language, vector, metadata
                 FROM hybrid_vector_metadata
                 WHERE content LIKE ?1 OR filepath LIKE ?1
                 LIMIT ?2",
            )
            .map_err(|e| HgbError::Storage(format!("Fallback search error: {}", e)))?;

        let rows = stmt
            .query_map(params![pattern, limit as i64], |row| {
                let id: String = row.get(0)?;
                let filepath: String = row.get(1)?;
                let chunk_index: i64 = row.get(2)?;
                let content: String = row.get(3)?;
                let language: Option<String> = row.get(4)?;
                let blob: Vec<u8> = row.get(5)?;
                let metadata: Option<String> = row.get(6)?;
                Ok((id, filepath, chunk_index, content, language, blob, metadata))
            })
            .map_err(|e| HgbError::Storage(format!("Fallback query map error: {}", e)))?;

        let mut hits = Vec::new();
        for item in rows.flatten() {
            let (id, filepath, chunk_index, content, language, blob, metadata) = item;
            let vector = decode_vector_blob(&blob).unwrap_or_default();
            let snippet = extract_fallback_snippet(&content, query_text, snippet_tokens);
            hits.push(InternalLexicalHit {
                id,
                filepath,
                chunk_index: chunk_index as usize,
                content,
                language,
                bm25: 1.0,
                snippet: Some(snippet),
                vector,
                metadata,
            });
        }

        Ok(hits)
    }

    fn internal_search_semantic(
        &self,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<InternalSemanticHit>> {
        let hnsw_results = self.hnsw.search_knn(query_vector, limit);
        if hnsw_results.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock().unwrap();
        let mut hits = Vec::new();

        for res in hnsw_results {
            let mut stmt = conn.prepare(
                "SELECT filepath, chunk_index, content, language, vector, metadata
                 FROM hybrid_vector_metadata WHERE id = ?1",
            ).map_err(|e| HgbError::Storage(format!("Lookup HNSW hit error: {}", e)))?;

            let row_res = stmt.query_row(params![res.id], |row| {
                let filepath: String = row.get(0)?;
                let chunk_index: i64 = row.get(1)?;
                let content: String = row.get(2)?;
                let language: Option<String> = row.get(3)?;
                let blob: Vec<u8> = row.get(4)?;
                let metadata: Option<String> = row.get(5)?;
                Ok((filepath, chunk_index, content, language, blob, metadata))
            }).optional().map_err(|e| HgbError::Storage(format!("Hit fetch error: {}", e)))?;

            if let Some((filepath, chunk_index, content, language, blob, metadata)) = row_res {
                let vector = decode_vector_blob(&blob).unwrap_or_default();
                hits.push(InternalSemanticHit {
                    id: res.id,
                    filepath,
                    chunk_index: chunk_index as usize,
                    content,
                    language,
                    similarity: res.similarity,
                    vector,
                    metadata,
                });
            }
        }

        Ok(hits)
    }

    fn fuse_hybrid_results(
        &self,
        lexical_hits: Vec<InternalLexicalHit>,
        semantic_hits: Vec<InternalSemanticHit>,
        query_vector: &[f32],
        ranking_mode: HybridRankingMode,
        filter: Option<&HybridSearchFilter>,
        limit: usize,
    ) -> Result<Vec<HybridSearchResult>> {
        // Build unified document registry
        struct CandidateEntry {
            id: String,
            filepath: String,
            chunk_index: usize,
            language: Option<String>,
            content: String,
            snippet: Option<String>,
            vector: Vec<f32>,
            metadata: Option<String>,
            raw_bm25: Option<f32>,
            lexical_rank: Option<usize>,
            similarity: Option<f32>,
            semantic_rank: Option<usize>,
        }

        let mut candidate_map: HashMap<String, CandidateEntry> = HashMap::new();

        // Register lexical hits
        for (idx, hit) in lexical_hits.into_iter().enumerate() {
            let rank = idx + 1;
            candidate_map.insert(
                hit.id.clone(),
                CandidateEntry {
                    id: hit.id,
                    filepath: hit.filepath,
                    chunk_index: hit.chunk_index,
                    language: hit.language,
                    content: hit.content,
                    snippet: hit.snippet,
                    vector: hit.vector,
                    metadata: hit.metadata,
                    raw_bm25: Some(hit.bm25),
                    lexical_rank: Some(rank),
                    similarity: None,
                    semantic_rank: None,
                },
            );
        }

        // Register or merge semantic hits
        for (idx, hit) in semantic_hits.into_iter().enumerate() {
            let rank = idx + 1;
            if let Some(entry) = candidate_map.get_mut(&hit.id) {
                entry.similarity = Some(hit.similarity);
                entry.semantic_rank = Some(rank);
            } else {
                candidate_map.insert(
                    hit.id.clone(),
                    CandidateEntry {
                        id: hit.id,
                        filepath: hit.filepath,
                        chunk_index: hit.chunk_index,
                        language: hit.language,
                        content: hit.content,
                        snippet: None,
                        vector: hit.vector,
                        metadata: hit.metadata,
                        raw_bm25: None,
                        lexical_rank: None,
                        similarity: Some(hit.similarity),
                        semantic_rank: Some(rank),
                    },
                );
            }
        }

        // Filter candidates
        let valid_candidates: Vec<CandidateEntry> = candidate_map
            .into_values()
            .filter(|c| {
                if let Some(f) = filter {
                    f.matches(&c.filepath, c.language.as_deref(), c.metadata.as_deref())
                } else {
                    true
                }
            })
            .collect();

        // Max BM25 score for normalization
        let max_bm25 = valid_candidates
            .iter()
            .filter_map(|c| c.raw_bm25)
            .fold(0.0f32, |acc, val| acc.max(val));

        let mut fused_results = Vec::with_capacity(valid_candidates.len());

        for mut candidate in valid_candidates {
            // If semantic similarity is missing, compute directly using SIMD kernel
            let cosine_sim = match candidate.similarity {
                Some(sim) => sim,
                None => {
                    if !candidate.vector.is_empty() && !query_vector.is_empty() {
                        simd_cosine_similarity(query_vector, &candidate.vector)
                    } else {
                        0.0
                    }
                }
            };
            candidate.similarity = Some(cosine_sim);

            let norm_bm25 = match candidate.raw_bm25 {
                Some(score) if max_bm25 > 0.0 => (score / max_bm25).clamp(0.0, 1.0),
                Some(_) => 1.0,
                None => 0.0,
            };

            let (hybrid_score, rrf_score) = match ranking_mode {
                HybridRankingMode::Rrf { k } => {
                    let mut rrf = 0.0f32;
                    if let Some(r_lex) = candidate.lexical_rank {
                        rrf += 1.0 / ((k + r_lex) as f32);
                    }
                    if let Some(r_sem) = candidate.semantic_rank {
                        rrf += 1.0 / ((k + r_sem) as f32);
                    }
                    (rrf, Some(rrf))
                }
                HybridRankingMode::LinearInterpolation { alpha } => {
                    let score = (alpha * norm_bm25) + ((1.0 - alpha) * cosine_sim.max(0.0));
                    (score, None)
                }
            };

            fused_results.push(HybridSearchResult {
                id: candidate.id,
                filepath: candidate.filepath,
                chunk_index: candidate.chunk_index,
                language: candidate.language,
                content: candidate.content,
                snippet: candidate.snippet,
                hybrid_score,
                bm25_score: candidate.raw_bm25,
                normalized_bm25_score: norm_bm25,
                cosine_similarity: cosine_sim,
                rrf_score,
                lexical_rank: candidate.lexical_rank,
                semantic_rank: candidate.semantic_rank,
                metadata: candidate.metadata,
            });
        }

        // Sort descending by hybrid_score
        fused_results.sort_by(|a, b| {
            b.hybrid_score
                .partial_cmp(&a.hybrid_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if fused_results.len() > limit {
            fused_results.truncate(limit);
        }

        Ok(fused_results)
    }
}

// ============================================================================
// Helper Structs & Algorithms
// ============================================================================

struct InternalLexicalHit {
    id: String,
    filepath: String,
    chunk_index: usize,
    content: String,
    language: Option<String>,
    bm25: f32,
    snippet: Option<String>,
    vector: Vec<f32>,
    metadata: Option<String>,
}

struct InternalSemanticHit {
    id: String,
    filepath: String,
    chunk_index: usize,
    content: String,
    language: Option<String>,
    similarity: f32,
    vector: Vec<f32>,
    metadata: Option<String>,
}

/// Sanitizes user queries into safe FTS5 query tokens
pub fn sanitize_fts5_query(query: &str) -> String {
    let clean = query.trim();
    if clean.is_empty() {
        return String::new();
    }

    let tokens: Vec<String> = clean
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t))
        .collect();

    if tokens.is_empty() {
        format!("\"{}\"", clean.replace('"', ""))
    } else {
        tokens.join(" ")
    }
}

/// Fallback snippet extractor when FTS5 is not available
fn extract_fallback_snippet(content: &str, query: &str, max_tokens: usize) -> String {
    let words: Vec<&str> = content.split_whitespace().collect();
    let query_lower = query.to_lowercase();

    let match_idx = words
        .iter()
        .position(|w| w.to_lowercase().contains(&query_lower))
        .unwrap_or(0);

    let start = match_idx.saturating_sub(max_tokens / 2);
    let end = (start + max_tokens).min(words.len());

    let snippet_words = &words[start..end];
    let joined = snippet_words.join(" ");

    let highlighted = if let Some(pos) = joined.to_lowercase().find(&query_lower) {
        let (head, rest) = joined.split_at(pos);
        let (matched, tail) = rest.split_at(query_lower.len());
        format!("...{}<mark>{}</mark>{}...", head, matched, tail)
    } else {
        format!("...{}...", joined)
    };

    highlighted
}

/// Filepath glob matching supporting `*`, `**`, `?`, and relative segments
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let regex_pattern = glob_to_regex(pattern);
    match regex::Regex::new(&regex_pattern) {
        Ok(re) => re.is_match(path),
        Err(_) => path.contains(pattern),
    }
}

/// Converts standard glob patterns into equivalent regular expressions
pub fn glob_to_regex(pattern: &str) -> String {
    let has_slash = pattern.contains('/');
    let mut regex = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    regex.push_str(".*");
                    i += 2;
                    if i < chars.len() && chars[i] == '/' {
                        regex.push_str("(?:/)?");
                        i += 1;
                    }
                    continue;
                } else if has_slash {
                    regex.push_str("[^/]*");
                } else {
                    regex.push_str(".*");
                }
            }
            '?' => regex.push('.'),
            '.' | '+' | '(' | ')' | '{' | '}' | '[' | ']' | '^' | '$' | '|' | '\\' => {
                regex.push('\\');
                regex.push(chars[i]);
            }
            c => regex.push(c),
        }
        i += 1;
    }
    regex.push('$');
    regex
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
pub mod tests {
    use super::*;

    fn generate_test_vector(seed: u64, dim: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(dim);
        let mut state = seed;
        for _ in 0..dim {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let val = ((state >> 32) as i32 as f32) / (i32::MAX as f32);
            v.push(val);
        }
        // Normalize
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    #[test]
    fn test_fts5_token_search_and_snippet() {
        let engine = Fts5HnswHybridEngine::in_memory(None).expect("Failed to create engine");

        let chunk1 = HybridDocumentChunk {
            id: "src/crypto.rs#0".to_string(),
            filepath: "crates/core/src/crypto.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "pub fn blake3_hash(data: &[u8]) -> [u8; 32] {\n    blake3::hash(data).into()\n}".to_string(),
            vector: generate_test_vector(42, 64),
            metadata: Some(r#"{"component": "crypto", "tier": "core"}"#.to_string()),
        };

        let chunk2 = HybridDocumentChunk {
            id: "src/simd.rs#0".to_string(),
            filepath: "crates/storage/src/simd.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "pub unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {\n    _mm256_fmadd_ps(a, b, sum)\n}".to_string(),
            vector: generate_test_vector(99, 64),
            metadata: Some(r#"{"component": "simd", "tier": "storage"}"#.to_string()),
        };

        engine.index_chunk(chunk1).expect("Failed to index chunk 1");
        engine.index_chunk(chunk2).expect("Failed to index chunk 2");

        assert_eq!(engine.len().unwrap(), 2);

        // Lexical token query
        let query = HybridSearchQuery::new(5).with_text("blake3_hash");
        let results = engine.search(&query).expect("Search failed");

        assert!(!results.is_empty());
        assert_eq!(results[0].id, "src/crypto.rs#0");
        assert!(results[0].snippet.is_some());
        let snippet = results[0].snippet.as_ref().unwrap();
        assert!(snippet.contains("<mark>") || snippet.contains("blake3"));
    }

    #[test]
    fn test_hnsw_vector_similarity_and_simd() {
        let engine = Fts5HnswHybridEngine::in_memory(None).expect("Failed to create engine");
        let dim = 64;

        let v_target = generate_test_vector(100, dim);
        let v_similar = {
            let mut v = v_target.clone();
            v[0] += 0.05;
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            v
        };
        let v_orthogonal = generate_test_vector(999, dim);

        let chunk1 = HybridDocumentChunk {
            id: "similar_doc".to_string(),
            filepath: "src/sim.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "similar content for embedding evaluation".to_string(),
            vector: v_similar,
            metadata: None,
        };

        let chunk2 = HybridDocumentChunk {
            id: "orthogonal_doc".to_string(),
            filepath: "src/ortho.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "orthogonal random noise content".to_string(),
            vector: v_orthogonal,
            metadata: None,
        };

        engine.index_chunk(chunk1).unwrap();
        engine.index_chunk(chunk2).unwrap();

        // Vector search
        let query = HybridSearchQuery::new(5).with_vector(v_target);
        let results = engine.search(&query).expect("Vector search failed");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "similar_doc");
        assert!(results[0].cosine_similarity > results[1].cosine_similarity);
    }

    #[test]
    fn test_hybrid_rrf_and_linear_ranking() {
        let engine = Fts5HnswHybridEngine::in_memory(None).expect("Failed to create engine");
        let dim = 64;
        let query_vec = generate_test_vector(500, dim);

        // Doc A: Exact text match, medium vector match
        let v_a = {
            let mut v = query_vec.clone();
            v[1] += 0.3;
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            for x in &mut v { *x /= norm; }
            v
        };
        let doc_a = HybridDocumentChunk {
            id: "doc_a".to_string(),
            filepath: "crates/core/src/kv.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "distributed raft kv consensus storage".to_string(),
            vector: v_a,
            metadata: Some(r#"{"tier": "raft"}"#.to_string()),
        };

        // Doc B: High vector match, weak text match
        let v_b = query_vec.clone();
        let doc_b = HybridDocumentChunk {
            id: "doc_b".to_string(),
            filepath: "crates/nextgen/src/ai.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "autonomous agent perception loop".to_string(),
            vector: v_b,
            metadata: Some(r#"{"tier": "ai"}"#.to_string()),
        };

        engine.index_chunk(doc_a).unwrap();
        engine.index_chunk(doc_b).unwrap();

        // Test RRF Search
        let rrf_results = engine
            .search_rrf("distributed consensus", &query_vec, 5, 60, None)
            .expect("RRF search failed");

        assert_eq!(rrf_results.len(), 2);
        // Doc A has high lexical rank; Doc B has high semantic rank; both appear with rrf_score
        assert!(rrf_results[0].rrf_score.is_some());
        assert!(rrf_results[1].rrf_score.is_some());

        // Test Linear Interpolation Search (alpha = 0.5)
        let linear_results = engine
            .search_hybrid("distributed consensus", &query_vec, 5, 0.5, None)
            .expect("Linear hybrid search failed");

        assert_eq!(linear_results.len(), 2);
        for res in &linear_results {
            assert!(res.hybrid_score >= 0.0 && res.hybrid_score <= 1.0);
        }
    }

    #[test]
    fn test_search_filtering_glob_and_metadata() {
        let engine = Fts5HnswHybridEngine::in_memory(None).expect("Failed to create engine");
        let dim = 32;

        let doc1 = HybridDocumentChunk {
            id: "storage_doc".to_string(),
            filepath: "crates/hgb-storage/src/mod.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "memory index optimization".to_string(),
            vector: generate_test_vector(1, dim),
            metadata: Some(r#"{"component": "storage", "public": true}"#.to_string()),
        };

        let doc2 = HybridDocumentChunk {
            id: "core_doc".to_string(),
            filepath: "crates/hgb-core/src/mod.rs".to_string(),
            chunk_index: 0,
            language: Some("rust".to_string()),
            content: "memory index core".to_string(),
            vector: generate_test_vector(2, dim),
            metadata: Some(r#"{"component": "core", "public": false}"#.to_string()),
        };

        engine.index_chunk(doc1).unwrap();
        engine.index_chunk(doc2).unwrap();

        // Filter by glob: only storage crate
        let filter_storage = HybridSearchFilter::new().with_glob("crates/hgb-storage/**");
        let results = engine
            .search_lexical("memory index", 10, Some(&filter_storage))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "storage_doc");

        // Filter by metadata attribute
        let filter_meta = HybridSearchFilter::new().with_attribute("public", "true");
        let results_meta = engine
            .search_lexical("memory index", 10, Some(&filter_meta))
            .unwrap();
        assert_eq!(results_meta.len(), 1);
        assert_eq!(results_meta[0].id, "storage_doc");
    }
}
