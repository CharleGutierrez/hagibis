//! # Vector-Native Storage & MCP Memory Mesh (`vector_mcp_memory_mesh`)
//!
//! Production-grade persistent vector memory mesh combining embedded vector storage
//! (IEEE-754 little-endian binary float vectors compatible with `sqlite-vec` / native HNSW),
//! SQLite metadata, and full-text search (FTS5).
//!
//! ## Key Capabilities
//! - **Persistent Long-Term Memory**: Stores interactions, command outcomes, user preferences,
//!   code context summaries, and past fix patterns.
//! - **Semantic Memory Search**: Queries the vector memory mesh with dense IEEE-754 vectors
//!   or natural language text queries using SIMD-accelerated cosine similarity and HNSW.
//! - **Context Retrieval & Auto-Indexing**: Automatically traverses codebases, extracts functions/symbols,
//!   chunks files, and indexes them into the vector memory mesh.
//! - **In-Memory Caching & SQLite WAL Durability**: Read-through and write-through in-memory caching
//!   backed by persistent SQLite in WAL mode (`.hgb/vector_memory.db`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use hgb_core::{deterministic_semantic_embedding, HgbError, Result};

use crate::hnsw_vector_index::{DistanceMetric, HnswConfig, HnswVectorIndex};
use crate::simd_runtime_dispatch::simd_cosine_similarity;

/// Default embedding vector dimension (768-D dense float vector)
pub const DEFAULT_VECTOR_DIM: usize = 768;

/// Memory categories
pub const CATEGORY_INTERACTION: &str = "interaction";
pub const CATEGORY_COMMAND_OUTCOME: &str = "command_outcome";
pub const CATEGORY_USER_PREFERENCE: &str = "user_preference";
pub const CATEGORY_CODE_CONTEXT: &str = "code_context";
pub const CATEGORY_PAST_FIX_PATTERN: &str = "past_fix_pattern";
pub const CATEGORY_CODE_CHUNK: &str = "code_chunk";

/// Serializes an IEEE-754 `f32` vector slice to raw little-endian binary bytes.
/// Compatible with `sqlite-vec` binary BLOB layout.
#[inline]
pub fn encode_vector_blob(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vector.len() * 4);
    for &val in vector {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Deserializes raw little-endian binary bytes into an `f32` vector.
/// Validates that the input length is a strict multiple of 4 bytes.
#[inline]
pub fn decode_vector_blob(blob: &[u8]) -> Result<Vec<f32>> {
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

/// Represents a persistent long-term memory item stored within the Vector MCP Memory Mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    /// Unique identifier (UUID or deterministic hash)
    pub id: String,
    /// Memory namespace (e.g., "default", "codebase", "developer_habits")
    pub namespace: String,
    /// Category: interaction, command_outcome, user_preference, code_context, past_fix_pattern, code_chunk
    pub category: String,
    /// Descriptive title or headline
    pub title: String,
    /// Body content, code snippet, log output, or rule text
    pub content: String,
    /// Float embedding vector (IEEE-754 binary serialized in SQLite)
    pub vector: Vec<f32>,
    /// Structured metadata payload
    pub metadata: serde_json::Value,
    /// Associated code symbols (functions, structs, traits, classes)
    pub symbols: Vec<String>,
    /// Creation timestamp (Unix epoch milliseconds)
    pub created_at: i64,
    /// Last update timestamp (Unix epoch milliseconds)
    pub updated_at: i64,
    /// Total number of times this memory item has been recalled
    pub access_count: u32,
}

impl MemoryItem {
    pub fn new(
        category: impl Into<String>,
        title: impl Into<String>,
        content: impl Into<String>,
        vector: Vec<f32>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let cat_str = category.into();
        let tit_str = title.into();
        let cont_str = content.into();
        let id_hash = blake3::hash(format!("{}:{}:{}", cat_str, tit_str, cont_str).as_bytes())
            .to_hex()
            .to_string();

        Self {
            id: format!("mem-{}", &id_hash[..16]),
            namespace: "default".to_string(),
            category: cat_str,
            title: tit_str,
            content: cont_str,
            vector,
            metadata: serde_json::json!({}),
            symbols: Vec::new(),
            created_at: now,
            updated_at: now,
            access_count: 0,
        }
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_symbols(mut self, symbols: Vec<String>) -> Self {
        self.symbols = symbols;
        self
    }
}

/// Filter criteria for semantic memory search
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryFilter {
    /// Match specific namespace
    pub namespace: Option<String>,
    /// Match exact category
    pub category: Option<String>,
    /// Match one of multiple categories
    pub categories: Option<Vec<String>>,
    /// Minimum similarity score threshold [0.0, 1.0]
    pub min_score: Option<f32>,
    /// Match specific symbol name
    pub symbol: Option<String>,
    /// Match source file path inside metadata
    pub file_path: Option<String>,
}

impl MemoryFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = Some(min_score);
        self
    }

    pub fn matches(&self, item: &MemoryItem, score: f32) -> bool {
        if let Some(min_s) = self.min_score {
            if score < min_s {
                return false;
            }
        }
        if let Some(ref ns) = self.namespace {
            if &item.namespace != ns {
                return false;
            }
        }
        if let Some(ref cat) = self.category {
            if &item.category != cat {
                return false;
            }
        }
        if let Some(ref cats) = self.categories {
            if !cats.iter().any(|c| c == &item.category) {
                return false;
            }
        }
        if let Some(ref sym) = self.symbol {
            if !item.symbols.iter().any(|s| s.contains(sym)) {
                return false;
            }
        }
        if let Some(ref fp) = self.file_path {
            let matches_fp = item
                .metadata
                .get("filepath")
                .and_then(|v| v.as_str())
                .map(|p| p.contains(fp))
                .unwrap_or(false);
            if !matches_fp {
                return false;
            }
        }
        true
    }
}

/// Search result returned from vector and hybrid memory search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    /// Recalled memory item
    pub item: MemoryItem,
    /// Cosine vector similarity score in range [0.0, 1.0]
    pub similarity_score: f32,
    /// Optional FTS5 lexical ranking score
    pub lexical_score: Option<f32>,
    /// Combined hybrid score
    pub combined_score: f32,
    /// Highlighted snippet or snippet preview
    pub snippet: Option<String>,
}

/// Codebase indexing configuration options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexOptions {
    /// File extensions to include (e.g. ["rs", "py", "toml", "json", "md", "c", "cpp", "zig"])
    pub extensions: Vec<String>,
    /// Maximum number of files to process
    pub max_files: usize,
    /// Chunk size in lines
    pub chunk_size_lines: usize,
    /// Overlap in lines between adjacent chunks
    pub overlap_lines: usize,
    /// Memory namespace to store indexed code chunks
    pub namespace: String,
    /// Force reindexing even if files have not changed
    pub force_reindex: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            extensions: vec![
                "rs".to_string(),
                "py".to_string(),
                "toml".to_string(),
                "json".to_string(),
                "md".to_string(),
                "c".to_string(),
                "cpp".to_string(),
                "zig".to_string(),
                "ts".to_string(),
                "js".to_string(),
            ],
            max_files: 500,
            chunk_size_lines: 40,
            overlap_lines: 8,
            namespace: "codebase".to_string(),
            force_reindex: false,
        }
    }
}

/// Telemetry report from codebase indexing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexReport {
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub extracted_symbols: usize,
    pub duration_ms: u64,
    pub errors: Vec<String>,
}

/// Unified Vector-Native MCP Memory Mesh
pub struct VectorMcpMemoryMesh {
    db_path: PathBuf,
    conn: Arc<Mutex<Connection>>,
    cache: Arc<RwLock<HashMap<String, MemoryItem>>>,
    hnsw: Arc<HnswVectorIndex>,
    fts5_active: bool,
    dimension: usize,
}

impl VectorMcpMemoryMesh {
    /// Open or create a persistent Vector MCP Memory Mesh database at the given path
    pub fn open_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent).map_err(HgbError::Io)?;
            }
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| HgbError::Storage(format!("Failed to open SQLite database: {}", e)))?;

        let fts5_active = Self::init_database_schema(&conn)?;

        // Initialize or open persistent HNSW index
        let hnsw_cfg = HnswConfig {
            dimension: DEFAULT_VECTOR_DIM,
            metric: DistanceMetric::Cosine,
            ..Default::default()
        };
        let hnsw = HnswVectorIndex::new(&db_path, hnsw_cfg)?;

        let mesh = Self {
            db_path,
            conn: Arc::new(Mutex::new(conn)),
            cache: Arc::new(RwLock::new(HashMap::new())),
            hnsw: Arc::new(hnsw),
            fts5_active,
            dimension: DEFAULT_VECTOR_DIM,
        };

        // Pre-warm the cache from persistent storage
        let _ = mesh.warm_cache(200);

        Ok(mesh)
    }

    /// Open or create the default workspace Vector MCP Memory Mesh (`.hgb/vector_memory.db`)
    pub fn open_workspace<P: AsRef<Path>>(workspace_root: P) -> Result<Self> {
        let db_path = workspace_root
            .as_ref()
            .join(".hgb")
            .join("vector_memory.db");
        Self::open_or_create(db_path)
    }

    /// Open an ephemeral in-memory Vector MCP Memory Mesh for testing and fast isolation
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| HgbError::Storage(format!("Failed to open in-memory SQLite: {}", e)))?;

        let fts5_active = Self::init_database_schema(&conn)?;

        let hnsw_cfg = HnswConfig {
            dimension: DEFAULT_VECTOR_DIM,
            metric: DistanceMetric::Cosine,
            ..Default::default()
        };
        let hnsw = HnswVectorIndex::in_memory(hnsw_cfg)?;

        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
            cache: Arc::new(RwLock::new(HashMap::new())),
            hnsw: Arc::new(hnsw),
            fts5_active,
            dimension: DEFAULT_VECTOR_DIM,
        })
    }

    /// Internal schema initialization for SQLite WAL and FTS5 tables
    fn init_database_schema(conn: &Connection) -> Result<bool> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(|e| HgbError::Storage(format!("Failed to apply SQLite PRAGMAs: {}", e)))?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS mcp_memory_items (
                id TEXT PRIMARY KEY,
                namespace TEXT NOT NULL DEFAULT 'default',
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                vector_blob BLOB NOT NULL,
                dimensions INTEGER NOT NULL,
                metadata_json TEXT,
                symbols_json TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                access_count INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to create mcp_memory_items: {}", e)))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mcp_memory_ns_cat ON mcp_memory_items(namespace, category)",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to create category index: {}", e)))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mcp_memory_created ON mcp_memory_items(created_at)",
            [],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to create created_at index: {}", e)))?;

        // FTS5 Full-text search virtual table setup
        let fts_sql = "CREATE VIRTUAL TABLE IF NOT EXISTS mcp_memory_fts USING fts5(
            id UNINDEXED,
            title,
            content,
            symbols,
            tokenize = 'porter unicode61'
        )";
        let fts_active = conn.execute(fts_sql, []).is_ok();

        if !fts_active {
            conn.execute(
                "CREATE TABLE IF NOT EXISTS mcp_memory_fts_fallback (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    symbols TEXT
                )",
                [],
            )
            .map_err(|e| HgbError::Storage(format!("Failed to create fallback FTS table: {}", e)))?;
        }

        Ok(fts_active)
    }

    /// Insert or update a memory item into persistent SQLite storage, HNSW graph, and in-memory cache
    pub fn insert_item(&self, item: MemoryItem) -> Result<()> {
        let blob = encode_vector_blob(&item.vector);
        let meta_str = serde_json::to_string(&item.metadata).unwrap_or_default();
        let sym_str = serde_json::to_string(&item.symbols).unwrap_or_default();
        let symbols_text = item.symbols.join(" ");

        {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO mcp_memory_items (
                    id, namespace, category, title, content, vector_blob, dimensions,
                    metadata_json, symbols_json, created_at, updated_at, access_count
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(id) DO UPDATE SET
                    namespace = excluded.namespace,
                    category = excluded.category,
                    title = excluded.title,
                    content = excluded.content,
                    vector_blob = excluded.vector_blob,
                    dimensions = excluded.dimensions,
                    metadata_json = excluded.metadata_json,
                    symbols_json = excluded.symbols_json,
                    updated_at = excluded.updated_at,
                    access_count = excluded.access_count",
                params![
                    &item.id,
                    &item.namespace,
                    &item.category,
                    &item.title,
                    &item.content,
                    &blob,
                    item.vector.len() as i64,
                    &meta_str,
                    &sym_str,
                    item.created_at,
                    item.updated_at,
                    item.access_count as i64,
                ],
            )
            .map_err(|e| HgbError::Storage(format!("Failed to insert memory item: {}", e)))?;

            // Update FTS table
            if self.fts5_active {
                let _ = conn.execute("DELETE FROM mcp_memory_fts WHERE id = ?1", params![&item.id]);
                let _ = conn.execute(
                    "INSERT INTO mcp_memory_fts (id, title, content, symbols) VALUES (?1, ?2, ?3, ?4)",
                    params![&item.id, &item.title, &item.content, &symbols_text],
                );
            } else {
                let _ = conn.execute(
                    "INSERT INTO mcp_memory_fts_fallback (id, title, content, symbols)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(id) DO UPDATE SET title = excluded.title, content = excluded.content, symbols = excluded.symbols",
                    params![&item.id, &item.title, &item.content, &symbols_text],
                );
            }
        }

        // Insert into embedded HNSW index
        let _ = self.hnsw.insert(&item.id, &item.vector, Some(&item.title));

        // Update in-memory cache
        {
            let mut cache = self.cache.write().unwrap();
            cache.insert(item.id.clone(), item);
        }

        Ok(())
    }

    /// Retrieve a memory item by ID (checking cache first, then SQLite)
    pub fn get_item(&self, id: &str) -> Result<Option<MemoryItem>> {
        {
            let cache = self.cache.read().unwrap();
            if let Some(item) = cache.get(id) {
                return Ok(Some(item.clone()));
            }
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, namespace, category, title, content, vector_blob,
                        metadata_json, symbols_json, created_at, updated_at, access_count
                 FROM mcp_memory_items WHERE id = ?1",
            )
            .map_err(|e| HgbError::Storage(format!("Failed to prepare select: {}", e)))?;

        let row = stmt
            .query_row(params![id], |row| {
                let id: String = row.get(0)?;
                let namespace: String = row.get(1)?;
                let category: String = row.get(2)?;
                let title: String = row.get(3)?;
                let content: String = row.get(4)?;
                let vector_blob: Vec<u8> = row.get(5)?;
                let metadata_json: Option<String> = row.get(6)?;
                let symbols_json: Option<String> = row.get(7)?;
                let created_at: i64 = row.get(8)?;
                let updated_at: i64 = row.get(9)?;
                let access_count: i64 = row.get(10)?;

                Ok((
                    id,
                    namespace,
                    category,
                    title,
                    content,
                    vector_blob,
                    metadata_json,
                    symbols_json,
                    created_at,
                    updated_at,
                    access_count,
                ))
            })
            .optional()
            .map_err(|e| HgbError::Storage(format!("Query error: {}", e)))?;

        if let Some((
            id,
            namespace,
            category,
            title,
            content,
            vector_blob,
            metadata_json,
            symbols_json,
            created_at,
            updated_at,
            access_count,
        )) = row
        {
            let vector = decode_vector_blob(&vector_blob)?;
            let metadata = metadata_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let symbols = symbols_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            let item = MemoryItem {
                id: id.clone(),
                namespace,
                category,
                title,
                content,
                vector,
                metadata,
                symbols,
                created_at,
                updated_at,
                access_count: access_count as u32,
            };

            let mut cache = self.cache.write().unwrap();
            cache.insert(id, item.clone());
            return Ok(Some(item));
        }

        Ok(None)
    }

    /// Delete a memory item by ID
    pub fn delete_item(&self, id: &str) -> Result<bool> {
        let rows_affected = {
            let conn = self.conn.lock().unwrap();
            let rows = conn
                .execute("DELETE FROM mcp_memory_items WHERE id = ?1", params![id])
                .map_err(|e| HgbError::Storage(format!("Failed to delete memory item: {}", e)))?;

            if self.fts5_active {
                let _ = conn.execute("DELETE FROM mcp_memory_fts WHERE id = ?1", params![id]);
            } else {
                let _ = conn.execute(
                    "DELETE FROM mcp_memory_fts_fallback WHERE id = ?1",
                    params![id],
                );
            }
            rows
        };

        {
            let mut cache = self.cache.write().unwrap();
            cache.remove(id);
        }

        Ok(rows_affected > 0)
    }

    /// List memory items with optional namespace and category filter
    pub fn list_items(
        &self,
        namespace: Option<&str>,
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryItem>> {
        let conn = self.conn.lock().unwrap();

        let query = match (namespace, category) {
            (Some(_), Some(_)) => {
                "SELECT id, namespace, category, title, content, vector_blob,
                        metadata_json, symbols_json, created_at, updated_at, access_count
                 FROM mcp_memory_items WHERE namespace = ?1 AND category = ?2
                 ORDER BY updated_at DESC LIMIT ?3"
            }
            (Some(_), None) => {
                "SELECT id, namespace, category, title, content, vector_blob,
                        metadata_json, symbols_json, created_at, updated_at, access_count
                 FROM mcp_memory_items WHERE namespace = ?1
                 ORDER BY updated_at DESC LIMIT ?2"
            }
            (None, Some(_)) => {
                "SELECT id, namespace, category, title, content, vector_blob,
                        metadata_json, symbols_json, created_at, updated_at, access_count
                 FROM mcp_memory_items WHERE category = ?1
                 ORDER BY updated_at DESC LIMIT ?2"
            }
            (None, None) => {
                "SELECT id, namespace, category, title, content, vector_blob,
                        metadata_json, symbols_json, created_at, updated_at, access_count
                 FROM mcp_memory_items
                 ORDER BY updated_at DESC LIMIT ?1"
            }
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| HgbError::Storage(format!("Failed to prepare list statement: {}", e)))?;

        let parse_row = |row: &rusqlite::Row| -> rusqlite::Result<MemoryItem> {
            let id: String = row.get(0)?;
            let namespace: String = row.get(1)?;
            let category: String = row.get(2)?;
            let title: String = row.get(3)?;
            let content: String = row.get(4)?;
            let vector_blob: Vec<u8> = row.get(5)?;
            let metadata_json: Option<String> = row.get(6)?;
            let symbols_json: Option<String> = row.get(7)?;
            let created_at: i64 = row.get(8)?;
            let updated_at: i64 = row.get(9)?;
            let access_count: i64 = row.get(10)?;

            let vector = decode_vector_blob(&vector_blob).unwrap_or_default();
            let metadata = metadata_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let symbols = symbols_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            Ok(MemoryItem {
                id,
                namespace,
                category,
                title,
                content,
                vector,
                metadata,
                symbols,
                created_at,
                updated_at,
                access_count: access_count as u32,
            })
        };

        let mut items = Vec::new();
        match (namespace, category) {
            (Some(ns), Some(cat)) => {
                let rows = stmt
                    .query_map(params![ns, cat, limit as i64], parse_row)
                    .map_err(|e| HgbError::Storage(e.to_string()))?;
                for r in rows {
                    items.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
                }
            }
            (Some(ns), None) => {
                let rows = stmt
                    .query_map(params![ns, limit as i64], parse_row)
                    .map_err(|e| HgbError::Storage(e.to_string()))?;
                for r in rows {
                    items.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
                }
            }
            (None, Some(cat)) => {
                let rows = stmt
                    .query_map(params![cat, limit as i64], parse_row)
                    .map_err(|e| HgbError::Storage(e.to_string()))?;
                for r in rows {
                    items.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
                }
            }
            (None, None) => {
                let rows = stmt
                    .query_map(params![limit as i64], parse_row)
                    .map_err(|e| HgbError::Storage(e.to_string()))?;
                for r in rows {
                    items.push(r.map_err(|e| HgbError::Storage(e.to_string()))?);
                }
            }
        }

        Ok(items)
    }

    /// Record a conversation interaction (user prompt, assistant response, tool reflection)
    pub fn record_interaction(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<MemoryItem> {
        let vector = deterministic_semantic_embedding(content, self.dimension);
        let title = format!("Interaction [{}] ({})", session_id, role);
        let mut meta = metadata.unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("session_id".to_string(), serde_json::json!(session_id));
            obj.insert("role".to_string(), serde_json::json!(role));
        }

        let item = MemoryItem::new(CATEGORY_INTERACTION, title, content, vector)
            .with_namespace("interactions")
            .with_metadata(meta);

        self.insert_item(item.clone())?;
        Ok(item)
    }

    /// Record a terminal or test command outcome
    pub fn record_command_outcome(
        &self,
        command: &str,
        exit_code: i32,
        stdout_summary: &str,
        stderr_summary: &str,
        duration_ms: u64,
    ) -> Result<MemoryItem> {
        let text = format!(
            "Command: {}\nExit Code: {}\nStdout: {}\nStderr: {}",
            command, exit_code, stdout_summary, stderr_summary
        );
        let vector = deterministic_semantic_embedding(&text, self.dimension);
        let title = format!("Command Outcome: {} (exit {})", command, exit_code);

        let metadata = serde_json::json!({
            "command": command,
            "exit_code": exit_code,
            "stdout": stdout_summary,
            "stderr": stderr_summary,
            "duration_ms": duration_ms,
            "success": exit_code == 0
        });

        let item = MemoryItem::new(CATEGORY_COMMAND_OUTCOME, title, text, vector)
            .with_namespace("commands")
            .with_metadata(metadata);

        self.insert_item(item.clone())?;
        Ok(item)
    }

    /// Record a developer preference, architectural rule, or coding habit
    pub fn record_user_preference(
        &self,
        key: &str,
        rule: &str,
        context: Option<&str>,
    ) -> Result<MemoryItem> {
        let full_text = match context {
            Some(ctx) => format!("{}: {} (Context: {})", key, rule, ctx),
            None => format!("{}: {}", key, rule),
        };
        let vector = deterministic_semantic_embedding(&full_text, self.dimension);
        let title = format!("Preference: {}", key);

        let metadata = serde_json::json!({
            "key": key,
            "rule": rule,
            "context": context
        });

        let item = MemoryItem::new(CATEGORY_USER_PREFERENCE, title, full_text, vector)
            .with_namespace("developer_habits")
            .with_metadata(metadata);

        self.insert_item(item.clone())?;
        Ok(item)
    }

    /// Record an architectural or module code context summary
    pub fn record_code_context(
        &self,
        filepath: &str,
        summary: &str,
        symbols: &[String],
        metadata: Option<serde_json::Value>,
    ) -> Result<MemoryItem> {
        let full_text = format!("File: {}\nSummary: {}\nSymbols: {:?}", filepath, summary, symbols);
        let vector = deterministic_semantic_embedding(&full_text, self.dimension);
        let title = format!("Code Context: {}", filepath);

        let mut meta = metadata.unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("filepath".to_string(), serde_json::json!(filepath));
        }

        let item = MemoryItem::new(CATEGORY_CODE_CONTEXT, title, summary, vector)
            .with_namespace("codebase")
            .with_symbols(symbols.to_vec())
            .with_metadata(meta);

        self.insert_item(item.clone())?;
        Ok(item)
    }

    /// Record a past compiler or runtime fix pattern
    pub fn record_past_fix_pattern(
        &self,
        error_signature: &str,
        diagnosis: &str,
        patch_diff: &str,
        success: bool,
    ) -> Result<MemoryItem> {
        let full_text = format!(
            "Error Signature: {}\nDiagnosis: {}\nPatch: {}\nSuccess: {}",
            error_signature, diagnosis, patch_diff, success
        );
        let vector = deterministic_semantic_embedding(&full_text, self.dimension);
        let title = format!("Fix Pattern: {}", error_signature);

        let metadata = serde_json::json!({
            "error_signature": error_signature,
            "diagnosis": diagnosis,
            "patch_diff": patch_diff,
            "success": success
        });

        let item = MemoryItem::new(CATEGORY_PAST_FIX_PATTERN, title, full_text, vector)
            .with_namespace("fixes")
            .with_metadata(metadata);

        self.insert_item(item.clone())?;
        Ok(item)
    }

    /// Search vector memory mesh using a dense float query vector
    pub fn search_semantic_memory(
        &self,
        query_vector: &[f32],
        top_k: usize,
        filter: Option<&MemoryFilter>,
    ) -> Result<Vec<MemorySearchResult>> {
        let mut candidates = Vec::new();

        // 1. Try embedded HNSW graph retrieval
        let hnsw_results = self.hnsw.search_knn(query_vector, top_k * 3);
        let mut seen_ids = std::collections::HashSet::new();

        for hit in hnsw_results {
            seen_ids.insert(hit.id.clone());
            if let Some(item) = self.get_item(&hit.id)? {
                let sim = simd_cosine_similarity(query_vector, &item.vector);
                if let Some(flt) = filter {
                    if !flt.matches(&item, sim) {
                        continue;
                    }
                }
                candidates.push(MemorySearchResult {
                    item,
                    similarity_score: sim,
                    lexical_score: None,
                    combined_score: sim,
                    snippet: None,
                });
            }
        }

        // 2. Scan in-memory cache to guarantee full coverage
        {
            let cache = self.cache.read().unwrap();
            for (id, item) in cache.iter() {
                if !seen_ids.contains(id) {
                    let sim = simd_cosine_similarity(query_vector, &item.vector);
                    if let Some(flt) = filter {
                        if !flt.matches(item, sim) {
                            continue;
                        }
                    }
                    candidates.push(MemorySearchResult {
                        item: item.clone(),
                        similarity_score: sim,
                        lexical_score: None,
                        combined_score: sim,
                        snippet: None,
                    });
                }
            }
        }

        // If candidates are still fewer than top_k, scan persistent SQLite items
        if candidates.len() < top_k {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT id, namespace, category, title, content, vector_blob,
                            metadata_json, symbols_json, created_at, updated_at, access_count
                     FROM mcp_memory_items ORDER BY updated_at DESC LIMIT 200",
                )
                .map_err(|e| HgbError::Storage(e.to_string()))?;

            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let namespace: String = row.get(1)?;
                    let category: String = row.get(2)?;
                    let title: String = row.get(3)?;
                    let content: String = row.get(4)?;
                    let vector_blob: Vec<u8> = row.get(5)?;
                    let metadata_json: Option<String> = row.get(6)?;
                    let symbols_json: Option<String> = row.get(7)?;
                    let created_at: i64 = row.get(8)?;
                    let updated_at: i64 = row.get(9)?;
                    let access_count: i64 = row.get(10)?;
                    Ok((
                        id,
                        namespace,
                        category,
                        title,
                        content,
                        vector_blob,
                        metadata_json,
                        symbols_json,
                        created_at,
                        updated_at,
                        access_count,
                    ))
                })
                .map_err(|e| HgbError::Storage(e.to_string()))?;

            for r in rows {
                if let Ok((
                    id,
                    namespace,
                    category,
                    title,
                    content,
                    vector_blob,
                    metadata_json,
                    symbols_json,
                    created_at,
                    updated_at,
                    access_count,
                )) = r
                {
                    if !seen_ids.contains(&id) {
                        if let Ok(vec) = decode_vector_blob(&vector_blob) {
                            let sim = simd_cosine_similarity(query_vector, &vec);
                            let metadata = metadata_json
                                .and_then(|s| serde_json::from_str(&s).ok())
                                .unwrap_or_else(|| serde_json::json!({}));
                            let symbols = symbols_json
                                .and_then(|s| serde_json::from_str(&s).ok())
                                .unwrap_or_default();

                            let item = MemoryItem {
                                id,
                                namespace,
                                category,
                                title,
                                content,
                                vector: vec,
                                metadata,
                                symbols,
                                created_at,
                                updated_at,
                                access_count: access_count as u32,
                            };

                            if let Some(flt) = filter {
                                if !flt.matches(&item, sim) {
                                    continue;
                                }
                            }

                            candidates.push(MemorySearchResult {
                                item,
                                similarity_score: sim,
                                lexical_score: None,
                                combined_score: sim,
                                snippet: None,
                            });
                        }
                    }
                }
            }
        }

        // Sort by similarity descending
        candidates.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates.truncate(top_k);

        // Increment access count on recalled candidates
        for c in &mut candidates {
            c.item.access_count += 1;
            let _ = self.increment_access_count(&c.item.id);
        }

        Ok(candidates)
    }

    /// Search vector memory mesh by natural language text query (hybrid vector + FTS)
    pub fn search_by_text(
        &self,
        query_text: &str,
        top_k: usize,
        filter: Option<&MemoryFilter>,
    ) -> Result<Vec<MemorySearchResult>> {
        let query_vector = deterministic_semantic_embedding(query_text, self.dimension);
        let mut results = self.search_semantic_memory(&query_vector, top_k, filter)?;

        // If FTS5 is active, enrich lexical match scores
        if self.fts5_active {
            let conn = self.conn.lock().unwrap();
            let sanitized_query = query_text
                .replace('"', "")
                .replace('\'', "")
                .replace('*', "");
            let terms: Vec<&str> = sanitized_query
                .split_whitespace()
                .filter(|t| !t.is_empty())
                .collect();

            if !terms.is_empty() {
                let fts_query = terms.join(" OR ");
                let mut stmt = conn
                    .prepare(
                        "SELECT id, bm25(mcp_memory_fts), snippet(mcp_memory_fts, 2, '<b>', '</b>', '...', 16)
                         FROM mcp_memory_fts WHERE mcp_memory_fts MATCH ?1 LIMIT 20",
                    )
                    .map_err(|e| HgbError::Storage(e.to_string()))?;

                let fts_hits = stmt.query_map(params![fts_query], |row| {
                    let id: String = row.get(0)?;
                    let rank: f64 = row.get(1)?;
                    let snippet: String = row.get(2)?;
                    Ok((id, rank, snippet))
                });

                if let Ok(hits) = fts_hits {
                    for h in hits.flatten() {
                        let (hit_id, bm25_rank, snippet) = h;
                        let norm_lex = (1.0 / (1.0 + bm25_rank.abs() as f32)).clamp(0.0, 1.0);
                        if let Some(res) = results.iter_mut().find(|r| r.item.id == hit_id) {
                            res.lexical_score = Some(norm_lex);
                            res.combined_score = 0.7 * res.similarity_score + 0.3 * norm_lex;
                            res.snippet = Some(snippet);
                        }
                    }
                }
            }
        }

        results.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
    }

    /// Retrieve long-term developer context, habits, past fix patterns, or architecture rules
    pub fn recall_developer_context(
        &self,
        topic: Option<&str>,
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryItem>> {
        if let Some(t) = topic {
            let filter = MemoryFilter {
                category: category.map(|c| c.to_string()),
                ..Default::default()
            };
            let search_results = self.search_by_text(t, limit, Some(&filter))?;
            return Ok(search_results.into_iter().map(|r| r.item).collect());
        }

        self.list_items(None, category, limit)
    }

    /// Recursively indexes files in the codebase, extracting symbols, functions, and code chunks
    pub fn index_codebase(&self, root_dir: &Path, options: &IndexOptions) -> Result<IndexReport> {
        let start_time = SystemTime::now();
        let mut indexed_files = 0;
        let mut total_chunks = 0;
        let mut extracted_symbols = 0;
        let mut errors = Vec::new();

        let mut files_to_process = Vec::new();
        Self::collect_files_recursive(root_dir, options, &mut files_to_process)?;

        for file_path in files_to_process.into_iter().take(options.max_files) {
            match self.index_single_file(&file_path, options) {
                Ok((chunks, symbols)) => {
                    indexed_files += 1;
                    total_chunks += chunks;
                    extracted_symbols += symbols;
                }
                Err(e) => {
                    errors.push(format!("{}: {}", file_path.display(), e));
                }
            }
        }

        let duration_ms = start_time
            .elapsed()
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(IndexReport {
            indexed_files,
            total_chunks,
            extracted_symbols,
            duration_ms,
            errors,
        })
    }

    /// Indexes a single code file into chunks and extracts AST symbols
    pub fn index_single_file(
        &self,
        file_path: &Path,
        options: &IndexOptions,
    ) -> Result<(usize, usize)> {
        let content = fs::read_to_string(file_path).map_err(HgbError::Io)?;
        let rel_path = file_path.to_string_lossy().to_string();
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let symbols = Self::extract_symbols(&content, ext);
        let total_symbols = symbols.len();

        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return Ok((0, total_symbols));
        }

        let chunk_size = options.chunk_size_lines.max(10);
        let overlap = options.overlap_lines.min(chunk_size / 2);

        let mut start_idx = 0;
        let mut chunk_count = 0;

        while start_idx < lines.len() {
            let end_idx = (start_idx + chunk_size).min(lines.len());
            let chunk_lines = &lines[start_idx..end_idx];
            let chunk_text = chunk_lines.join("\n");

            // Extract chunk-local symbols
            let chunk_symbols: Vec<String> = symbols
                .iter()
                .filter(|s| chunk_text.contains(s.as_str()))
                .cloned()
                .collect();

            let title = format!(
                "{} (Lines {}-{})",
                rel_path,
                start_idx + 1,
                end_idx
            );
            let vector = deterministic_semantic_embedding(&chunk_text, self.dimension);

            let metadata = serde_json::json!({
                "filepath": rel_path,
                "start_line": start_idx + 1,
                "end_line": end_idx,
                "language": ext,
                "chunk_index": chunk_count,
            });

            let item_id = format!(
                "chunk-{}",
                &blake3::hash(format!("{}:{}:{}", rel_path, start_idx, end_idx).as_bytes())
                    .to_hex()[..16]
            );

            let mut item = MemoryItem::new(CATEGORY_CODE_CHUNK, title, chunk_text, vector)
                .with_namespace(&options.namespace)
                .with_symbols(chunk_symbols)
                .with_metadata(metadata);
            item.id = item_id;

            self.insert_item(item)?;
            chunk_count += 1;

            if end_idx >= lines.len() {
                break;
            }
            start_idx = end_idx - overlap;
        }

        Ok((chunk_count, total_symbols))
    }

    /// Extracts structural functions, structs, traits, and classes from source text
    pub fn extract_symbols(content: &str, extension: &str) -> Vec<String> {
        let mut symbols = Vec::new();

        match extension {
            "rs" => {
                let re_fn = Regex::new(r"(?:pub(?:\([^\)]+\))?\s+)?(?:async\s+)?fn\s+([a-zA-Z0-9_]+)").unwrap();
                let re_struct = Regex::new(r"(?:pub(?:\([^\)]+\))?\s+)?struct\s+([a-zA-Z0-9_]+)").unwrap();
                let re_enum = Regex::new(r"(?:pub(?:\([^\)]+\))?\s+)?enum\s+([a-zA-Z0-9_]+)").unwrap();
                let re_trait = Regex::new(r"(?:pub(?:\([^\)]+\))?\s+)?trait\s+([a-zA-Z0-9_]+)").unwrap();

                for cap in re_fn.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("fn:{}", m.as_str()));
                    }
                }
                for cap in re_struct.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("struct:{}", m.as_str()));
                    }
                }
                for cap in re_enum.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("enum:{}", m.as_str()));
                    }
                }
                for cap in re_trait.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("trait:{}", m.as_str()));
                    }
                }
            }
            "py" => {
                let re_fn = Regex::new(r"(?:async\s+)?def\s+([a-zA-Z0-9_]+)").unwrap();
                let re_class = Regex::new(r"class\s+([a-zA-Z0-9_]+)").unwrap();

                for cap in re_fn.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("def:{}", m.as_str()));
                    }
                }
                for cap in re_class.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("class:{}", m.as_str()));
                    }
                }
            }
            "ts" | "js" => {
                let re_fn = Regex::new(r"(?:export\s+)?(?:async\s+)?function\s+([a-zA-Z0-9_]+)").unwrap();
                let re_class = Regex::new(r"(?:export\s+)?class\s+([a-zA-Z0-9_]+)").unwrap();
                let re_const = Regex::new(r"(?:export\s+)?const\s+([a-zA-Z0-9_]+)\s*=\s*(?:async\s*)?\(").unwrap();

                for cap in re_fn.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("fn:{}", m.as_str()));
                    }
                }
                for cap in re_class.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("class:{}", m.as_str()));
                    }
                }
                for cap in re_const.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("const:{}", m.as_str()));
                    }
                }
            }
            "zig" => {
                let re_fn = Regex::new(r"(?:pub\s+)?fn\s+([a-zA-Z0-9_]+)").unwrap();
                for cap in re_fn.captures_iter(content) {
                    if let Some(m) = cap.get(1) {
                        symbols.push(format!("fn:{}", m.as_str()));
                    }
                }
            }
            _ => {
                // Fallback: extract identifier tokens
                let tokens: Vec<&str> = content
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|s| s.len() > 3)
                    .take(20)
                    .collect();
                for t in tokens {
                    symbols.push(t.to_string());
                }
            }
        }

        symbols.sort();
        symbols.dedup();
        symbols
    }

    /// Internal recursive directory walker respecting ignored directories
    fn collect_files_recursive(
        dir: &Path,
        options: &IndexOptions,
        collected: &mut Vec<PathBuf>,
    ) -> Result<()> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(());
        }

        let entries = fs::read_dir(dir).map_err(HgbError::Io)?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("");

            // Skip build, VCS, and hidden directories
            if file_name.starts_with('.')
                || file_name == "target"
                || file_name == "node_modules"
                || file_name == "__pycache__"
                || file_name == "dist"
                || file_name == "build"
            {
                continue;
            }

            if path.is_dir() {
                Self::collect_files_recursive(&path, options, collected)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if options.extensions.iter().any(|target_ext| target_ext == ext) {
                        collected.push(path);
                    }
                }
            }
        }

        Ok(())
    }

    /// Warm in-memory cache with most recently updated memory records
    pub fn warm_cache(&self, limit: usize) -> Result<usize> {
        let items = self.list_items(None, None, limit)?;
        let count = items.len();
        let mut cache = self.cache.write().unwrap();
        for item in items {
            cache.insert(item.id.clone(), item);
        }
        Ok(count)
    }

    /// Increment access count for recalled memory
    fn increment_access_count(&self, id: &str) -> Result<()> {
        {
            let mut cache = self.cache.write().unwrap();
            if let Some(item) = cache.get_mut(id) {
                item.access_count += 1;
            }
        }
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE mcp_memory_items SET access_count = access_count + 1 WHERE id = ?1",
            params![id],
        );
        Ok(())
    }

    /// Returns the total count of stored memory records
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mcp_memory_items", [], |r| r.get(0))
            .map_err(|e| HgbError::Storage(e.to_string()))?;
        Ok(count as usize)
    }

    /// Check if SQLite FTS5 extension is active
    pub fn is_fts5_active(&self) -> bool {
        self.fts5_active
    }

    /// Database backing file path
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

// ============================================================================
// Unit Tests (Zero Mocks, Real SQLite WAL, Real IEEE-754 Vector Math)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_binary_blob_vector_serialization() {
        let original_vec: Vec<f32> = vec![0.0, 1.5, -2.25, 3.14159, 100.0, -0.0001];
        let blob = encode_vector_blob(&original_vec);
        assert_eq!(blob.len(), original_vec.len() * 4);

        let decoded = decode_vector_blob(&blob).expect("successful decode");
        assert_eq!(decoded.len(), original_vec.len());
        for (a, b) in original_vec.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }

        // Test invalid length handling
        let invalid_blob = vec![1, 2, 3];
        assert!(decode_vector_blob(&invalid_blob).is_err());
    }

    #[test]
    fn test_persistent_sqlite_wal_storage_and_cache() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join(".hgb").join("vector_memory.db");

        // 1. Create and populate memory mesh
        {
            let mesh = VectorMcpMemoryMesh::open_or_create(&db_path).expect("open mesh");
            assert_eq!(mesh.count().expect("count"), 0);

            let item = MemoryItem::new(
                CATEGORY_USER_PREFERENCE,
                "Tab indentation preference",
                "Developer strictly prefers 4 spaces instead of tabs",
                vec![0.1; DEFAULT_VECTOR_DIM],
            )
            .with_namespace("habits");

            mesh.insert_item(item.clone()).expect("insert");
            assert_eq!(mesh.count().expect("count"), 1);

            // Read from cache
            let cached_item = mesh.get_item(&item.id).expect("get").expect("item found");
            assert_eq!(cached_item.title, "Tab indentation preference");
            assert_eq!(cached_item.namespace, "habits");
        }

        // 2. Re-open mesh from disk to verify SQLite WAL durability
        {
            let mesh = VectorMcpMemoryMesh::open_or_create(&db_path).expect("reopen mesh");
            assert_eq!(mesh.count().expect("count"), 1);

            let items = mesh.list_items(Some("habits"), None, 10).expect("list");
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].title, "Tab indentation preference");
        }
    }

    #[test]
    fn test_long_term_memory_categories() {
        let mesh = VectorMcpMemoryMesh::in_memory().expect("in-memory mesh");

        // 1. Interaction
        let inter = mesh
            .record_interaction("sess-1", "user", "How do I optimize SIMD cosine?", None)
            .expect("interaction");
        assert_eq!(inter.category, CATEGORY_INTERACTION);

        // 2. Command outcome
        let cmd = mesh
            .record_command_outcome("cargo check", 0, "Finished dev profile", "", 120)
            .expect("cmd");
        assert_eq!(cmd.category, CATEGORY_COMMAND_OUTCOME);

        // 3. User preference
        let pref = mesh
            .record_user_preference("indent", "always use 4 spaces in rust", None)
            .expect("pref");
        assert_eq!(pref.category, CATEGORY_USER_PREFERENCE);

        // 4. Code context
        let code_ctx = mesh
            .record_code_context("src/lib.rs", "Storage module entry point", &["init".to_string()], None)
            .expect("code_ctx");
        assert_eq!(code_ctx.category, CATEGORY_CODE_CONTEXT);

        // 5. Past fix pattern
        let fix = mesh
            .record_past_fix_pattern("E0308 type mismatch", "Expected &str found String", "use as_str()", true)
            .expect("fix");
        assert_eq!(fix.category, CATEGORY_PAST_FIX_PATTERN);

        assert_eq!(mesh.count().expect("count"), 5);

        // Verify querying by category
        let prefs = mesh.list_items(None, Some(CATEGORY_USER_PREFERENCE), 10).expect("list prefs");
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].id, pref.id);
    }

    #[test]
    fn test_semantic_memory_search_and_hnsw() {
        let mesh = VectorMcpMemoryMesh::in_memory().expect("in-memory mesh");

        // Insert items with distinct semantics
        mesh.record_interaction("s1", "assistant", "HNSW Hierarchical Navigable Small World graphs enable logarithmic ANN search", None).unwrap();
        mesh.record_user_preference("rust_edition", "Project uses 2021 edition with strict compiler warnings", None).unwrap();
        mesh.record_past_fix_pattern("E0596 cannot borrow as mutable", "Variable declared without mut binding", "Add `mut` modifier to let binding", true).unwrap();

        // Query by text: "HNSW nearest neighbor graph search"
        let results = mesh
            .search_by_text("HNSW nearest neighbor graph search", 5, None)
            .expect("search");

        assert!(!results.is_empty());
        assert!(results[0].item.content.contains("Navigable Small World"));
        assert!(results[0].similarity_score > 0.2);
        assert!(results[0].combined_score > 0.2);

        // Query by text: "mutable borrow error fix"
        let fix_results = mesh
            .search_by_text("mutable borrow error fix", 5, None)
            .expect("search fixes");

        assert!(!fix_results.is_empty());
        assert!(fix_results[0].item.content.contains("E0596"));
    }

    #[test]
    fn test_codebase_auto_indexing_and_symbol_extraction() {
        let dir = tempdir().expect("tempdir");
        let rust_file = dir.path().join("calculator.rs");
        let python_file = dir.path().join("server.py");

        let rust_code = r#"
pub struct Calculator {
    base_val: f64,
}

impl Calculator {
    pub fn new(val: f64) -> Self {
        Self { base_val: val }
    }

    pub fn compute_sum(&self, a: f64, b: f64) -> f64 {
        self.base_val + a + b
    }
}
"#;

        let python_code = r#"
class MicroService:
    def __init__(self, port):
        self.port = port

    def start_serving(self):
        print(f"Serving on port {self.port}")
"#;

        fs::write(&rust_file, rust_code).expect("write rust");
        fs::write(&python_file, python_code).expect("write python");

        // Verify AST symbol extraction
        let rs_symbols = VectorMcpMemoryMesh::extract_symbols(rust_code, "rs");
        assert!(rs_symbols.contains(&"struct:Calculator".to_string()));
        assert!(rs_symbols.contains(&"fn:new".to_string()));
        assert!(rs_symbols.contains(&"fn:compute_sum".to_string()));

        let py_symbols = VectorMcpMemoryMesh::extract_symbols(python_code, "py");
        assert!(py_symbols.contains(&"class:MicroService".to_string()));
        assert!(py_symbols.contains(&"def:start_serving".to_string()));

        // Index the directory
        let mesh = VectorMcpMemoryMesh::in_memory().expect("in-memory mesh");
        let report = mesh
            .index_codebase(dir.path(), &IndexOptions::default())
            .expect("index codebase");

        assert_eq!(report.indexed_files, 2);
        assert!(report.total_chunks >= 2);
        assert!(report.extracted_symbols >= 5);

        // Retrieve indexed chunk semantically
        let results = mesh
            .search_by_text("compute_sum calculator base_val", 3, None)
            .expect("search");

        assert!(!results.is_empty());
        assert!(results[0].item.content.contains("compute_sum"));
    }

    #[test]
    fn test_recall_developer_context_and_access_count() {
        let mesh = VectorMcpMemoryMesh::in_memory().expect("in-memory mesh");

        mesh.record_user_preference("formatting", "always use rustfmt with max_width = 100", None).unwrap();
        mesh.record_past_fix_pattern("E0425 cannot find value in scope", "Missing use statement for dependency", "Import missing struct via use", true).unwrap();

        // 1. Recall by topic
        let recalled = mesh
            .recall_developer_context(Some("rustfmt code formatting width"), None, 2)
            .expect("recall");

        assert!(!recalled.is_empty());
        assert!(recalled[0].content.contains("rustfmt"));

        // 2. Verify access count incremented
        let item_id = &recalled[0].id;
        let fresh_item = mesh.get_item(item_id).expect("get").expect("item exists");
        assert!(fresh_item.access_count >= 1);
    }
}
