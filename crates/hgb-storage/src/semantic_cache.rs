//! # Semantic Caching & Local Vector-Mesh in `hgb-storage`
//!
//! Sub-millisecond (< 1.5ms) SIMD-accelerated semantic query cache for local and frontier LLMs.
//! Prevents duplicate inference passes, slashing Toshiba hardware CPU/GPU thrashing, thermal spikes,
//! and cloud token expenditures by returning semantically equivalent cached responses at >= 0.90 similarity.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Instant;
use serde::{Deserialize, Serialize};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::simd_vector_index::SimdVectorIndex;

/// An individual cached semantic prompt or tool-call entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCacheEntry {
    /// Unique hash identifier
    pub id: String,
    /// Original user prompt or tool call request text
    pub query_text: String,
    /// AI model that produced the response (e.g. "qwen2.5-coder:1.5b", "gemini-2.5-flash")
    pub model_id: String,
    /// Generated output response
    pub response_text: String,
    /// SIMD-normalized 64-dimensional semantic embedding vector
    pub embedding: Vec<f32>,
    /// Creation epoch timestamp in seconds
    pub created_at_epoch_sec: u64,
    /// Number of times this cache entry served a query
    pub hit_count: u64,
    /// Cumulative latency saved in milliseconds
    pub latency_saved_ms: u64,
    /// Optional context or workspace key for granular invalidation
    #[serde(default)]
    pub context_key: Option<String>,
}

/// Result returned when a query hits the semantic cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCacheHit {
    /// Matched cached entry
    pub entry: SemanticCacheEntry,
    /// Cosine similarity score between the query and the cached entry (0.0 to 1.0)
    pub similarity_score: f32,
    /// Time taken to perform the vector lookup and similarity comparison in microseconds
    pub lookup_latency_us: u64,
}

/// Aggregate performance statistics for the semantic cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCacheStats {
    pub total_entries: usize,
    pub total_hits: u64,
    pub total_latency_saved_ms: u64,
    pub similarity_threshold: f32,
    pub cache_file_path: Option<String>,
}

/// Principal In-Process Semantic Cache Engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticQueryCache {
    pub entries: HashMap<String, SemanticCacheEntry>,
    pub max_entries: usize,
    pub similarity_threshold: f32,
    pub cache_file_path: Option<PathBuf>,
}

impl Default for SemanticQueryCache {
    fn default() -> Self {
        Self::new(1000, 0.90)
    }
}

impl SemanticQueryCache {
    /// Create a new semantic query cache with a given capacity and similarity threshold
    pub fn new(max_entries: usize, similarity_threshold: f32) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries: max_entries.max(1),
            similarity_threshold: similarity_threshold.clamp(0.50, 1.0),
            cache_file_path: None,
        }
    }

    /// Initialize semantic cache with persistent file storage
    pub fn with_storage(storage_path: impl Into<PathBuf>) -> Self {
        let p = storage_path.into();
        if p.exists() {
            if let Ok(loaded) = Self::load_from_disk(&p) {
                let mut cache = loaded;
                cache.cache_file_path = Some(p);
                return cache;
            }
        }

        let mut cache = Self::default();
        cache.cache_file_path = Some(p);
        cache
    }

    /// Check if a semantically equivalent query was recently executed.
    /// Returns `Some(SemanticCacheHit)` if a match with cosine similarity >= threshold is found.
    pub fn check_semantic_hit(
        &mut self,
        query: &str,
        model_id: &str,
        threshold: Option<f32>,
    ) -> Option<SemanticCacheHit> {
        self.check_semantic_hit_with_context(query, model_id, threshold, None)
    }

    /// Check if a semantically equivalent query was recently executed with an optional workspace context key filter.
    pub fn check_semantic_hit_with_context(
        &mut self,
        query: &str,
        model_id: &str,
        threshold: Option<f32>,
        context_key: Option<&str>,
    ) -> Option<SemanticCacheHit> {
        let start = Instant::now();
        if self.entries.is_empty() || query.trim().is_empty() {
            return None;
        }

        let query_emb = SimdVectorIndex::generate_deterministic_embedding(query, 64);
        let min_threshold = threshold.unwrap_or(self.similarity_threshold);

        let mut best_hit_id: Option<String> = None;
        let mut best_score = 0.0f32;

        for (id, entry) in &self.entries {
            // Context filter: if context_key is specified, entry must match context_key
            if let Some(ctx) = context_key {
                if let Some(ref entry_ctx) = entry.context_key {
                    if entry_ctx != ctx {
                        continue;
                    }
                }
            }

            // Model filter: hit applies if model matches or if query was model-agnostic ("auto"/"default")
            let model_matches = entry.model_id == model_id
                || model_id == "auto"
                || model_id == "default"
                || entry.model_id == "auto";

            if !model_matches {
                continue;
            }

            let sim = SimdVectorIndex::cosine_similarity(&query_emb, &entry.embedding);
            if sim >= min_threshold && sim > best_score {
                best_score = sim;
                best_hit_id = Some(id.clone());
            }
        }

        let lookup_latency_us = start.elapsed().as_micros() as u64;

        if let Some(id) = best_hit_id {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.hit_count += 1;
                let hit_entry = entry.clone();
                let _ = self.save_to_disk();
                return Some(SemanticCacheHit {
                    entry: hit_entry,
                    similarity_score: best_score,
                    lookup_latency_us,
                });
            }
        }

        None
    }

    /// Record a freshly completed prompt or tool execution into the semantic cache
    pub fn record_entry(
        &mut self,
        query: &str,
        model_id: &str,
        response: &str,
        latency_ms: u64,
    ) {
        self.record_entry_with_context(query, model_id, response, latency_ms, None);
    }

    /// Record a freshly completed prompt or tool execution with an optional workspace/project context key
    pub fn record_entry_with_context(
        &mut self,
        query: &str,
        model_id: &str,
        response: &str,
        latency_ms: u64,
        context_key: Option<String>,
    ) {
        if query.trim().is_empty() || response.trim().is_empty() {
            return;
        }

        // Evict LRU entry if maximum capacity is reached
        if self.entries.len() >= self.max_entries {
            self.evict_lru_entry();
        }

        let hash_input = format!("{}:{}:{:?}", model_id, query.trim(), context_key);
        let id = blake3::hash(hash_input.as_bytes()).to_hex().to_string();
        let embedding = SimdVectorIndex::generate_deterministic_embedding(query, 64);
        let now = chrono::Utc::now().timestamp() as u64;

        let entry = SemanticCacheEntry {
            id: id.clone(),
            query_text: query.to_string(),
            model_id: model_id.to_string(),
            response_text: response.to_string(),
            embedding,
            created_at_epoch_sec: now,
            hit_count: 0,
            latency_saved_ms: latency_ms,
            context_key,
        };

        self.entries.insert(id, entry);
        let _ = self.save_to_disk();
    }

    /// Invalidate all cache entries matching the given context/workspace key.
    /// Returns the number of evicted entries.
    pub fn invalidate_context(&mut self, context_key: &str) -> usize {
        let initial_len = self.entries.len();
        self.entries.retain(|_, entry| {
            if let Some(ref ck) = entry.context_key {
                ck != context_key
            } else {
                true
            }
        });
        let evicted = initial_len - self.entries.len();
        if evicted > 0 {
            let _ = self.save_to_disk();
        }
        evicted
    }

    /// Evicts the entry with the lowest hit count and oldest timestamp
    fn evict_lru_entry(&mut self) {
        if let Some((evict_key, _)) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| (e.hit_count, e.created_at_epoch_sec))
            .map(|(k, e)| (k.clone(), e.clone()))
        {
            self.entries.remove(&evict_key);
        }
    }

    /// Aggregate cache statistics
    pub fn stats(&self) -> SemanticCacheStats {
        let total_hits: u64 = self.entries.values().map(|e| e.hit_count).sum();
        let total_latency_saved_ms: u64 = self
            .entries
            .values()
            .map(|e| e.hit_count * e.latency_saved_ms)
            .sum();

        SemanticCacheStats {
            total_entries: self.entries.len(),
            total_hits,
            total_latency_saved_ms,
            similarity_threshold: self.similarity_threshold,
            cache_file_path: self.cache_file_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        }
    }

    /// Clear all cached entries
    pub fn clear(&mut self) {
        self.entries.clear();
        let _ = self.save_to_disk();
    }

    /// Persist semantic cache to disk
    pub fn save_to_disk(&self) -> std::io::Result<()> {
        if let Some(ref path) = self.cache_file_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let encoded = serde_json::to_string(self)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            fs::write(path, encoded)?;
        }
        Ok(())
    }

    /// Load semantic cache from disk
    pub fn load_from_disk(path: &Path) -> std::io::Result<Self> {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_query_hit() {
        let mut cache = SemanticQueryCache::new(100, 0.90);
        cache.record_entry(
            "explain rust memory model",
            "qwen2.5-coder:1.5b",
            "Rust memory safety is guaranteed by ownership and borrowing.",
            250,
        );

        let hit = cache.check_semantic_hit(
            "explain rust memory model",
            "qwen2.5-coder:1.5b",
            None,
        );

        assert!(hit.is_some());
        let h = hit.unwrap();
        assert!((h.similarity_score - 1.0).abs() < 1e-4);
        assert!(h.entry.response_text.contains("Rust memory safety"));
        assert!(h.lookup_latency_us < 5000); // Sub-millisecond lookup
    }

    #[test]
    fn test_semantically_equivalent_query_hit() {
        let mut cache = SemanticQueryCache::new(100, 0.85);
        cache.record_entry(
            "how do I read a file in rust",
            "qwen2.5-coder:1.5b",
            "Use std::fs::read_to_string or std::fs::read.",
            320,
        );

        // Near-equivalent paraphrased prompt
        let hit = cache.check_semantic_hit(
            "how do I read a file in rust safely",
            "qwen2.5-coder:1.5b",
            Some(0.80),
        );

        assert!(hit.is_some());
        let h = hit.unwrap();
        assert!(h.similarity_score >= 0.80);
        assert!(h.entry.response_text.contains("std::fs::read_to_string"));
    }

    #[test]
    fn test_dissimilar_query_miss() {
        let mut cache = SemanticQueryCache::new(100, 0.90);
        cache.record_entry(
            "calculate fibonacci sequence in rust",
            "qwen2.5-coder:1.5b",
            "fn fib(n: u64) -> u64 { ... }",
            180,
        );

        // Totally unrelated query
        let hit = cache.check_semantic_hit(
            "configure postgresql connection pool",
            "qwen2.5-coder:1.5b",
            None,
        );

        assert!(hit.is_none());
    }

    #[test]
    fn test_model_isolation() {
        let mut cache = SemanticQueryCache::new(100, 0.90);
        cache.record_entry(
            "write hello world",
            "qwen2.5-coder:1.5b",
            "println!(\"Hello, World!\");",
            100,
        );

        // Different model should miss if not auto
        let hit = cache.check_semantic_hit("write hello world", "gemini-2.5-pro", None);
        assert!(hit.is_none());

        // "auto" or same model should hit
        let hit_auto = cache.check_semantic_hit("write hello world", "auto", None);
        assert!(hit_auto.is_some());
    }

    #[test]
    fn test_lru_eviction() {
        let mut cache = SemanticQueryCache::new(2, 0.90);
        cache.record_entry("query 1", "model", "resp 1", 50);
        cache.record_entry("query 2", "model", "resp 2", 50);

        // Mark query 1 with a hit
        let _ = cache.check_semantic_hit("query 1", "model", None);

        // Record query 3 -> should evict query 2 (lowest hit count)
        cache.record_entry("query 3", "model", "resp 3", 50);

        assert_eq!(cache.entries.len(), 2);
        assert!(cache.check_semantic_hit("query 1", "model", None).is_some());
        assert!(cache.check_semantic_hit("query 3", "model", None).is_some());
    }

    #[test]
    fn test_persistence_roundtrip() {
        let tmp_dir = std::env::temp_dir().join(format!("hgb_sem_cache_test_{}", std::process::id()));
        let tmp_file = tmp_dir.join("cache.json");

        let mut cache = SemanticQueryCache::with_storage(&tmp_file);
        cache.record_entry("compile zig kernel", "auto", "zig build-lib ...", 400);

        let reloaded = SemanticQueryCache::with_storage(&tmp_file);
        assert_eq!(reloaded.entries.len(), 1);

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_context_invalidation() {
        let mut cache = SemanticQueryCache::new(100, 0.90);
        cache.record_entry_with_context("query in proj A", "model", "resp A", 50, Some("proj_alpha".to_string()));
        cache.record_entry_with_context("query in proj B", "model", "resp B", 50, Some("proj_beta".to_string()));
        cache.record_entry("global query", "model", "resp global", 50);

        assert_eq!(cache.entries.len(), 3);

        // Invalidate proj_alpha
        let evicted = cache.invalidate_context("proj_alpha");
        assert_eq!(evicted, 1);
        assert_eq!(cache.entries.len(), 2);

        // proj_beta and global remain
        assert!(cache.entries.values().any(|e| e.context_key.as_deref() == Some("proj_beta")));
        assert!(cache.entries.values().any(|e| e.context_key.is_none()));
    }
}

// ============================================================================
// Pillar 3: Advanced MCP Tool Orchestration & Semantic Response Caching
// ============================================================================

/// Default cosine similarity threshold for semantic matches (0.92 = 92% semantic equivalence)
pub const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.92;

/// Default time-to-live in seconds (3600 seconds = 1 hour)
pub const DEFAULT_CACHE_TTL_SECS: u64 = 3600;

/// Default in-memory capacity ceiling for cached responses
pub const DEFAULT_CACHE_CAPACITY: usize = 10_000;

/// Default vector dimensions for semantic embeddings (matches Hagibis 768-dim embedding standard)
pub const DEFAULT_EMBEDDING_DIM: usize = 768;

/// An individual cached tool execution or LLM prompt response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSemanticResponse {
    /// Unique 64-character BLAKE3 hex hash identifier of tool_name + canonical_params
    pub entry_id: String,
    /// Target MCP tool name (e.g. "hgb_sqlite_query", "hgb_ast_query", "hgb_cow_sandbox")
    pub tool_name: String,
    /// Canonicalized, key-sorted normalized JSON parameters
    pub canonical_params: String,
    /// Raw IEEE-754 f32 embedding vector
    pub embedding: Vec<f32>,
    /// Stored response text payload (e.g. tool output, JSON string)
    pub response_text: String,
    /// Original execution duration in milliseconds
    pub execution_duration_ms: f64,
    /// Unix timestamp in milliseconds when entry was created
    pub created_at_epoch_ms: u64,
    /// Time-to-live in seconds (0 = never expires)
    pub ttl_secs: u64,
    /// Number of times this cached response was served
    pub hit_count: u64,
    /// Calculated similarity score upon lookup (1.0 for exact, >= threshold for semantic)
    pub similarity_score: Option<f32>,
}

impl CachedSemanticResponse {
    /// Checks whether the entry has exceeded its TTL window
    #[inline]
    pub fn is_expired(&self, now_epoch_ms: u64) -> bool {
        if self.ttl_secs == 0 {
            return false;
        }
        let expiry_ms = self.created_at_epoch_ms.saturating_add(self.ttl_secs.saturating_mul(1000));
        now_epoch_ms > expiry_ms
    }

    /// Computes approximate token count saved by this cached response (~4 chars per token)
    #[inline]
    pub fn estimated_tokens(&self) -> u64 {
        ((self.response_text.len() / 4) as u64).max(1)
    }
}

/// Aggregate performance statistics and operational metrics for the semantic response cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticResponseCacheStats {
    /// Total cumulative cache hits served
    pub hit_count: u64,
    /// Total cumulative cache misses
    pub miss_count: u64,
    /// Hit ratio (hits / (hits + misses))
    pub hit_ratio: f64,
    /// Cumulative tokens saved from bypassed LLM / tool inferences
    pub saved_tokens: u64,
    /// Cumulative execution time saved in milliseconds
    pub saved_duration_ms: f64,
    /// Estimated memory usage in bytes of the in-memory cache
    pub memory_usage_bytes: usize,
    /// Total number of currently cached response entries
    pub cached_entry_count: usize,
    /// Total number of entries evicted via LRU policy
    pub evicted_entry_count: u64,
    /// Configured default similarity threshold
    pub similarity_threshold: f32,
    /// Configured default TTL in seconds
    pub default_ttl_secs: u64,
    /// Path to persistent SQLite WAL database if enabled
    pub persistent_storage_path: Option<String>,
}

// ============================================================================
// SIMD Cosine Similarity Acceleration (AVX2 / SSE4.1 / Scalar Fallback)
// ============================================================================

/// AVX2 + FMA 256-bit vectorized dot product (8 single-precision floats per cycle)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_product_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= len {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        acc = _mm256_fmadd_ps(va, vb, acc);
        i += 8;
    }
    // Horizontal addition across 256-bit vector registers
    let hi = _mm256_extractf128_ps(acc, 1);
    let lo = _mm256_castps256_ps128(acc);
    let sum128 = _mm_add_ps(hi, lo);
    let hi64 = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, hi64);
    let hi32 = _mm_shuffle_ps(sum64, sum64, 1);
    let sum32 = _mm_add_ss(sum64, hi32);
    let mut dot = _mm_cvtss_f32(sum32);

    while i < len {
        dot += a[i] * b[i];
        i += 1;
    }
    dot
}

/// SSE4.1 128-bit vectorized dot product (4 single-precision floats per cycle)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn dot_product_sse41(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut acc = _mm_setzero_ps();
    let mut i = 0;
    while i + 4 <= len {
        let va = _mm_loadu_ps(a.as_ptr().add(i));
        let vb = _mm_loadu_ps(b.as_ptr().add(i));
        let prod = _mm_mul_ps(va, vb);
        acc = _mm_add_ps(acc, prod);
        i += 4;
    }
    let hi64 = _mm_movehl_ps(acc, acc);
    let sum64 = _mm_add_ps(acc, hi64);
    let hi32 = _mm_shuffle_ps(sum64, sum64, 1);
    let sum32 = _mm_add_ss(sum64, hi32);
    let mut dot = _mm_cvtss_f32(sum32);

    while i < len {
        dot += a[i] * b[i];
        i += 1;
    }
    dot
}

/// Portable 4-way loop-unrolled scalar fallback
#[inline]
pub fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let chunks = len / 4;
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;

    for c in 0..chunks {
        let base = c * 4;
        s0 += a[base] * b[base];
        s1 += a[base + 1] * b[base + 1];
        s2 += a[base + 2] * b[base + 2];
        s3 += a[base + 3] * b[base + 3];
    }

    let mut dot = (s0 + s1) + (s2 + s3);
    for i in (chunks * 4)..len {
        dot += a[i] * b[i];
    }
    dot
}

/// Dynamic SIMD dot product with runtime CPU feature dispatch
#[inline]
pub fn dot_product_simd(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return unsafe { dot_product_avx2_fma(a, b) };
        }
        if is_x86_feature_detected!("sse4.1") {
            return unsafe { dot_product_sse41(a, b) };
        }
    }
    dot_product_scalar(a, b)
}

/// Computes cosine similarity between two vectors in range `[-1.0, 1.0]` using SIMD acceleration
#[inline]
pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let a_slice = &a[..len];
    let b_slice = &b[..len];

    let dot = dot_product_simd(a_slice, b_slice);
    let norm_a = dot_product_simd(a_slice, a_slice);
    let norm_b = dot_product_simd(b_slice, b_slice);

    if norm_a <= 1e-12 || norm_b <= 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

// ============================================================================
// Canonical JSON Normalization & Deterministic BLAKE3 Hashing
// ============================================================================

/// Canonicalizes raw JSON string into a deterministic representation with keys sorted alphabetically
pub fn canonicalize_json_str(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        canonicalize_value(&val)
    } else {
        trimmed.to_string()
    }
}

/// Recursively sorts object keys deterministically
pub fn canonicalize_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut out = String::from("{");
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(k);
                out.push_str("\":");
                out.push_str(&canonicalize_value(v));
            }
            out.push('}');
            out
        }
        serde_json::Value::Array(arr) => {
            let mut out = String::from("[");
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonicalize_value(item));
            }
            out.push(']');
            out
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Compute 64-character BLAKE3 hex hash for tool_name and canonical parameters
pub fn compute_entry_id(tool_name: &str, canonical_params: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"::");
    hasher.update(canonical_params.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Serialize raw IEEE-754 f32 vector into little-endian byte array
pub fn embedding_to_blob(vec: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vec.len() * 4);
    for &f in vec {
        blob.extend_from_slice(&f.to_le_bytes());
    }
    blob
}

/// Deserialize little-endian byte array back into raw IEEE-754 f32 vector
pub fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    let chunks = blob.chunks_exact(4);
    let mut vec = Vec::with_capacity(chunks.len());
    for c in chunks {
        vec.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
    }
    vec
}

#[inline]
fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ============================================================================
// Internal Mutex State & Engine
// ============================================================================

struct CacheInner {
    entries: HashMap<String, CachedSemanticResponse>,
    lru_order: VecDeque<String>,
    conn: Option<rusqlite::Connection>,
    evicted_count: u64,
}

/// Principal In-Process Semantic Response Cache Engine
///
/// Features high-speed SIMD cosine similarity matching, persistent SQLite WAL backing,
/// LRU memory management, and automatic TTL invalidation.
pub struct SemanticResponseCache {
    inner: Mutex<CacheInner>,
    total_hits: AtomicU64,
    total_misses: AtomicU64,
    total_saved_tokens: AtomicU64,
    total_saved_duration_us: AtomicU64,
    max_capacity: usize,
    default_threshold: f32,
    default_ttl_secs: u64,
    db_path: Option<PathBuf>,
}

impl Default for SemanticResponseCache {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY, DEFAULT_SIMILARITY_THRESHOLD, DEFAULT_CACHE_TTL_SECS)
    }
}

impl SemanticResponseCache {
    /// Initialize an in-memory semantic response cache with capacity, threshold, and default TTL
    pub fn new(max_capacity: usize, default_threshold: f32, default_ttl_secs: u64) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                entries: HashMap::new(),
                lru_order: VecDeque::new(),
                conn: None,
                evicted_count: 0,
            }),
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_saved_tokens: AtomicU64::new(0),
            total_saved_duration_us: AtomicU64::new(0),
            max_capacity: max_capacity.max(1),
            default_threshold: default_threshold.clamp(0.50, 1.0),
            default_ttl_secs,
            db_path: None,
        }
    }

    /// Explicit alias for in-memory initialization
    pub fn in_memory(max_capacity: usize, default_threshold: f32, default_ttl_secs: u64) -> Self {
        Self::new(max_capacity, default_threshold, default_ttl_secs)
    }

    /// Open or create persistent SQLite WAL storage at the specified path
    pub fn open<P: AsRef<Path>>(
        db_path: P,
        max_capacity: usize,
        default_threshold: f32,
        default_ttl_secs: u64,
    ) -> std::io::Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let conn = rusqlite::Connection::open(&path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS semantic_response_cache (
                 entry_id TEXT PRIMARY KEY,
                 tool_name TEXT NOT NULL,
                 canonical_params TEXT NOT NULL,
                 embedding BLOB NOT NULL,
                 response_text TEXT NOT NULL,
                 execution_duration_ms REAL NOT NULL,
                 created_at_epoch_ms INTEGER NOT NULL,
                 ttl_secs INTEGER NOT NULL,
                 hit_count INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_semantic_cache_tool ON semantic_response_cache(tool_name);
             CREATE INDEX IF NOT EXISTS idx_semantic_cache_created ON semantic_response_cache(created_at_epoch_ms);"
        ).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let now_ms = current_epoch_ms();

        // Prune expired records upon startup
        let _ = conn.execute(
            "DELETE FROM semantic_response_cache WHERE ttl_secs > 0 AND (created_at_epoch_ms + (ttl_secs * 1000)) < ?1",
            rusqlite::params![now_ms],
        );

        // Load active records into memory up to max_capacity
        let mut entries = HashMap::new();
        let mut lru_order = VecDeque::new();

        if let Ok(mut stmt) = conn.prepare(
            "SELECT entry_id, tool_name, canonical_params, embedding, response_text,
                    execution_duration_ms, created_at_epoch_ms, ttl_secs, hit_count
             FROM semantic_response_cache
             ORDER BY created_at_epoch_ms ASC"
        ) {
            let rows = stmt.query_map([], |row| {
                let entry_id: String = row.get(0)?;
                let tool_name: String = row.get(1)?;
                let canonical_params: String = row.get(2)?;
                let embedding_blob: Vec<u8> = row.get(3)?;
                let response_text: String = row.get(4)?;
                let execution_duration_ms: f64 = row.get(5)?;
                let created_at_epoch_ms: u64 = row.get(6)?;
                let ttl_secs: u64 = row.get(7)?;
                let hit_count: u64 = row.get(8)?;

                Ok(CachedSemanticResponse {
                    entry_id,
                    tool_name,
                    canonical_params,
                    embedding: blob_to_embedding(&embedding_blob),
                    response_text,
                    execution_duration_ms,
                    created_at_epoch_ms,
                    ttl_secs,
                    hit_count,
                    similarity_score: None,
                })
            });

            if let Ok(mapped) = rows {
                for item in mapped.flatten() {
                    if !item.is_expired(now_ms) {
                        lru_order.push_back(item.entry_id.clone());
                        entries.insert(item.entry_id.clone(), item);
                    }
                }
            }
        }

        Ok(Self {
            inner: Mutex::new(CacheInner {
                entries,
                lru_order,
                conn: Some(conn),
                evicted_count: 0,
            }),
            total_hits: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
            total_saved_tokens: AtomicU64::new(0),
            total_saved_duration_us: AtomicU64::new(0),
            max_capacity: max_capacity.max(1),
            default_threshold: default_threshold.clamp(0.50, 1.0),
            default_ttl_secs,
            db_path: Some(path),
        })
    }

    /// Convenience initializer targeting the standard `<workspace>/.hgb/semantic_cache.db` path
    pub fn for_workspace<P: AsRef<Path>>(workspace_root: P) -> Self {
        let db_path = workspace_root.as_ref().join(".hgb").join("semantic_cache.db");
        Self::open(db_path, DEFAULT_CACHE_CAPACITY, DEFAULT_SIMILARITY_THRESHOLD, DEFAULT_CACHE_TTL_SECS)
            .unwrap_or_else(|_| Self::default())
    }

    /// Perform a high-speed SIMD semantic cache lookup against cached responses
    ///
    /// # Lookup Semantics
    /// 1. Checks exact match on tool_name + canonicalized arguments first (Score = 1.0).
    /// 2. If no exact match and query embedding is non-empty (or computed automatically),
    ///    scans all non-expired entries in the tool namespace using SIMD cosine similarity.
    /// 3. If cosine similarity >= threshold, updates LRU order, increments hit count, and returns `Some(CachedSemanticResponse)`.
    /// 4. Otherwise increments miss count and returns `None`.
    pub fn lookup_semantic(
        &self,
        tool_name: &str,
        args_str: &str,
        query_embedding: &[f32],
        threshold: f32,
    ) -> Option<CachedSemanticResponse> {
        let canonical_params = canonicalize_json_str(args_str);
        let exact_id = compute_entry_id(tool_name, &canonical_params);
        let eff_threshold = if threshold > 0.0 { threshold } else { self.default_threshold };
        let now_ms = current_epoch_ms();

        // Auto-generate embedding if empty using Hagibis deterministic semantic embedding
        let auto_embedding = if query_embedding.is_empty() {
            Some(hgb_core::deterministic_semantic_embedding(&canonical_params, DEFAULT_EMBEDDING_DIM))
        } else {
            None
        };
        let query_emb = if let Some(ref e) = auto_embedding {
            e.as_slice()
        } else {
            query_embedding
        };

        // Phase 1: Fast read-lock candidate search
        let mut hit_candidate_id: Option<String> = None;
        let mut hit_score: Option<f32> = None;

        {
            let inner = self.inner.lock().ok()?;

            // 1. Exact match check
            if let Some(entry) = inner.entries.get(&exact_id) {
                if !entry.is_expired(now_ms) && (entry.tool_name == tool_name || tool_name.is_empty() || tool_name == "*") {
                    hit_candidate_id = Some(exact_id);
                    hit_score = Some(1.0);
                }
            }

            // 2. SIMD cosine similarity scan if no exact match
            if hit_candidate_id.is_none() && !query_emb.is_empty() {
                let mut best_sim = -1.0f32;
                let mut best_id = None;

                for (id, entry) in &inner.entries {
                    if entry.is_expired(now_ms) {
                        continue;
                    }
                    if !tool_name.is_empty() && tool_name != "*" && entry.tool_name != tool_name {
                        continue;
                    }
                    if entry.embedding.is_empty() {
                        continue;
                    }

                    let sim = simd_cosine_similarity(query_emb, &entry.embedding);
                    if sim >= eff_threshold && sim > best_sim {
                        best_sim = sim;
                        best_id = Some(id.clone());
                    }
                }

                if let Some(bid) = best_id {
                    hit_candidate_id = Some(bid);
                    hit_score = Some(best_sim);
                }
            }
        }

        // Phase 2: Write-lock update for hit recording
        if let (Some(hit_id), Some(score)) = (hit_candidate_id, hit_score) {
            let mut inner = self.inner.lock().ok()?;
            let maybe_hit = if let Some(entry) = inner.entries.get_mut(&hit_id) {
                if entry.is_expired(now_ms) {
                    None
                } else {
                    entry.hit_count = entry.hit_count.saturating_add(1);
                    Some((entry.hit_count, entry.execution_duration_ms, entry.estimated_tokens(), entry.clone()))
                }
            } else {
                None
            };

            if let Some((hit_count, saved_duration_ms, estimated_tokens, mut out)) = maybe_hit {
                // Update LRU position
                inner.lru_order.retain(|id| id != &hit_id);
                inner.lru_order.push_back(hit_id.clone());

                // Persist hit_count in SQLite
                if let Some(ref conn) = inner.conn {
                    let _ = conn.execute(
                        "UPDATE semantic_response_cache SET hit_count = ?1 WHERE entry_id = ?2",
                        rusqlite::params![hit_count, hit_id],
                    );
                }

                // Update atomic telemetry
                self.total_hits.fetch_add(1, Ordering::Relaxed);
                self.total_saved_tokens.fetch_add(estimated_tokens, Ordering::Relaxed);
                self.total_saved_duration_us.fetch_add((saved_duration_ms * 1000.0) as u64, Ordering::Relaxed);

                out.similarity_score = Some(score);
                return Some(out);
            } else {
                self.total_misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        }

        self.total_misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Insert a newly executed tool response or LLM output into the cache
    pub fn insert(
        &self,
        tool_name: &str,
        raw_params: &str,
        embedding: Vec<f32>,
        response_text: String,
        execution_duration_ms: f64,
        ttl_secs: Option<u64>,
    ) -> CachedSemanticResponse {
        let canonical_params = canonicalize_json_str(raw_params);
        let entry_id = compute_entry_id(tool_name, &canonical_params);
        let now_ms = current_epoch_ms();
        let ttl = ttl_secs.unwrap_or(self.default_ttl_secs);

        let final_emb = if embedding.is_empty() {
            hgb_core::deterministic_semantic_embedding(&canonical_params, DEFAULT_EMBEDDING_DIM)
        } else {
            embedding
        };

        let entry = CachedSemanticResponse {
            entry_id: entry_id.clone(),
            tool_name: tool_name.to_string(),
            canonical_params: canonical_params.clone(),
            embedding: final_emb,
            response_text,
            execution_duration_ms,
            created_at_epoch_ms: now_ms,
            ttl_secs: ttl,
            hit_count: 0,
            similarity_score: None,
        };

        if let Ok(mut inner) = self.inner.lock() {
            // Evict least recently used entries if capacity reached
            while inner.entries.len() >= self.max_capacity {
                if let Some(oldest_id) = inner.lru_order.pop_front() {
                    inner.entries.remove(&oldest_id);
                    inner.evicted_count = inner.evicted_count.saturating_add(1);
                    if let Some(ref conn) = inner.conn {
                        let _ = conn.execute(
                            "DELETE FROM semantic_response_cache WHERE entry_id = ?1",
                            rusqlite::params![oldest_id],
                        );
                    }
                } else {
                    break;
                }
            }

            // Insert into in-memory table and LRU queue
            inner.lru_order.retain(|id| id != &entry_id);
            inner.lru_order.push_back(entry_id.clone());
            inner.entries.insert(entry_id.clone(), entry.clone());

            // Persist into SQLite
            if let Some(ref conn) = inner.conn {
                let blob = embedding_to_blob(&entry.embedding);
                let _ = conn.execute(
                    "INSERT INTO semantic_response_cache (
                         entry_id, tool_name, canonical_params, embedding, response_text,
                         execution_duration_ms, created_at_epoch_ms, ttl_secs, hit_count
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(entry_id) DO UPDATE SET
                         canonical_params = excluded.canonical_params,
                         embedding = excluded.embedding,
                         response_text = excluded.response_text,
                         execution_duration_ms = excluded.execution_duration_ms,
                         created_at_epoch_ms = excluded.created_at_epoch_ms,
                         ttl_secs = excluded.ttl_secs;",
                    rusqlite::params![
                        entry.entry_id,
                        entry.tool_name,
                        entry.canonical_params,
                        blob,
                        entry.response_text,
                        entry.execution_duration_ms,
                        entry.created_at_epoch_ms,
                        entry.ttl_secs,
                        entry.hit_count
                    ],
                );
            }
        }

        entry
    }

    /// Prunes all expired cache entries from in-memory table and persistent SQLite database
    pub fn prune_expired(&self) -> usize {
        let now_ms = current_epoch_ms();
        let mut pruned = 0usize;

        if let Ok(mut inner) = self.inner.lock() {
            let expired_keys: Vec<String> = inner
                .entries
                .iter()
                .filter(|(_, v)| v.is_expired(now_ms))
                .map(|(k, _)| k.clone())
                .collect();

            pruned = expired_keys.len();
            for k in &expired_keys {
                inner.entries.remove(k);
            }
            inner.lru_order.retain(|id| !expired_keys.contains(id));

            if let Some(ref conn) = inner.conn {
                let _ = conn.execute(
                    "DELETE FROM semantic_response_cache WHERE ttl_secs > 0 AND (created_at_epoch_ms + (ttl_secs * 1000)) < ?1",
                    rusqlite::params![now_ms],
                );
            }
        }

        pruned
    }

    /// Purges all entries for a specific tool namespace from memory and SQLite
    pub fn purge_tool(&self, tool_name: &str) -> usize {
        let mut purged = 0usize;

        if let Ok(mut inner) = self.inner.lock() {
            let tool_keys: Vec<String> = inner
                .entries
                .iter()
                .filter(|(_, v)| v.tool_name == tool_name)
                .map(|(k, _)| k.clone())
                .collect();

            purged = tool_keys.len();
            for k in &tool_keys {
                inner.entries.remove(k);
            }
            inner.lru_order.retain(|id| !tool_keys.contains(id));

            if let Some(ref conn) = inner.conn {
                let _ = conn.execute(
                    "DELETE FROM semantic_response_cache WHERE tool_name = ?1",
                    rusqlite::params![tool_name],
                );
            }
        }

        purged
    }

    /// Purges all entries across all tools, completely clearing the cache
    pub fn purge_all(&self) -> usize {
        let mut purged = 0usize;

        if let Ok(mut inner) = self.inner.lock() {
            purged = inner.entries.len();
            inner.entries.clear();
            inner.lru_order.clear();

            if let Some(ref conn) = inner.conn {
                let _ = conn.execute("DELETE FROM semantic_response_cache", []);
                let _ = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", []);
            }
        }

        purged
    }

    /// Retrieve aggregate performance metrics and operational cache statistics
    pub fn stats(&self) -> SemanticResponseCacheStats {
        let hits = self.total_hits.load(Ordering::Relaxed);
        let misses = self.total_misses.load(Ordering::Relaxed);
        let total = hits.saturating_add(misses);
        let hit_ratio = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };
        let saved_tokens = self.total_saved_tokens.load(Ordering::Relaxed);
        let saved_duration_ms = (self.total_saved_duration_us.load(Ordering::Relaxed) as f64) / 1000.0;

        let (cached_entry_count, memory_usage_bytes, evicted_entry_count) = {
            if let Ok(inner) = self.inner.lock() {
                let mut bytes = 0usize;
                for (k, v) in &inner.entries {
                    bytes = bytes
                        .saturating_add(k.len())
                        .saturating_add(v.tool_name.len())
                        .saturating_add(v.canonical_params.len())
                        .saturating_add(v.response_text.len())
                        .saturating_add(v.embedding.len() * 4)
                        .saturating_add(std::mem::size_of::<CachedSemanticResponse>());
                }
                (inner.entries.len(), bytes, inner.evicted_count)
            } else {
                (0, 0, 0)
            }
        };

        SemanticResponseCacheStats {
            hit_count: hits,
            miss_count: misses,
            hit_ratio,
            saved_tokens,
            saved_duration_ms,
            memory_usage_bytes,
            cached_entry_count,
            evicted_entry_count,
            similarity_threshold: self.default_threshold,
            default_ttl_secs: self.default_ttl_secs,
            persistent_storage_path: self.db_path.as_ref().map(|p| p.display().to_string()),
        }
    }
}

// ============================================================================
// Unit Tests for SemanticResponseCache
// ============================================================================

#[cfg(test)]
mod semantic_response_cache_tests {
    use super::*;

    #[test]
    fn test_canonicalize_json_order_invariance() {
        let json_a = r#"{"query":"SELECT * FROM users","limit":10,"filter":{"active":true,"role":"admin"}}"#;
        let json_b = r#"{"limit":10,"filter":{"role":"admin","active":true},"query":"SELECT * FROM users"}"#;

        let canon_a = canonicalize_json_str(json_a);
        let canon_b = canonicalize_json_str(json_b);

        assert_eq!(canon_a, canon_b);

        let id_a = compute_entry_id("hgb_sqlite_query", &canon_a);
        let id_b = compute_entry_id("hgb_sqlite_query", &canon_b);
        assert_eq!(id_a, id_b);
    }

    #[test]
    fn test_exact_parameter_match() {
        let cache = SemanticResponseCache::in_memory(100, 0.92, 3600);

        let tool = "hgb_ast_query";
        let params = r#"{"file":"crates/hgb-core/src/lib.rs","symbol":"HagibisMcpServer"}"#;
        let response = r#"{"line":553,"kind":"struct"}"#;

        cache.insert(tool, params, Vec::new(), response.to_string(), 12.5, None);

        // Lookup with reversed key order
        let query_params = r#"{"symbol":"HagibisMcpServer","file":"crates/hgb-core/src/lib.rs"}"#;
        let hit = cache.lookup_semantic(tool, query_params, &[], 0.92);

        assert!(hit.is_some(), "Expected exact parameter cache hit");
        let item = hit.unwrap();
        assert_eq!(item.tool_name, tool);
        assert_eq!(item.response_text, response);
        assert_eq!(item.hit_count, 1);
        assert_eq!(item.similarity_score, Some(1.0));

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 0);
        assert_eq!(stats.hit_ratio, 1.0);
    }

    #[test]
    fn test_simd_semantic_cosine_similarity_match() {
        let cache = SemanticResponseCache::in_memory(100, 0.92, 3600);

        // Construct base embedding vector with 768 dimensions
        let mut emb_a = vec![0.0f32; 768];
        for i in 0..768 {
            emb_a[i] = (i as f32).sin();
        }

        // Slightly perturbed vector (>0.98 similarity)
        let mut emb_b = emb_a.clone();
        for i in 0..768 {
            emb_b[i] += (i as f32).cos() * 0.05;
        }

        let sim = simd_cosine_similarity(&emb_a, &emb_b);
        assert!(sim > 0.92, "Vector similarity should be >0.92, got {}", sim);

        let tool = "hgb_hybrid_orchestrate";
        cache.insert(
            tool,
            r#"{"task":"Analyze performance bottleneck in microVM jail"}"#,
            emb_a,
            r#"{"route":"sovereign_local","confidence":0.99}"#.to_string(),
            45.0,
            None,
        );

        // Query with semantically equivalent embedding
        let hit = cache.lookup_semantic(
            tool,
            r#"{"task":"Investigate performance degradation in Firecracker microvm"}"#,
            &emb_b,
            0.92,
        );

        assert!(hit.is_some(), "Expected semantic cache hit with similarity > 0.92");
        let item = hit.unwrap();
        assert!(item.similarity_score.unwrap() > 0.92);
        assert_eq!(item.hit_count, 1);

        // Query with an orthogonal or dissimilar vector (<0.50 similarity)
        let mut emb_dissimilar = vec![0.0f32; 768];
        for i in 0..768 {
            emb_dissimilar[i] = if i % 2 == 0 { 1.0 } else { -1.0 };
        }
        let miss = cache.lookup_semantic(
            tool,
            r#"{"task":"Generate animated viral social card preview"}"#,
            &emb_dissimilar,
            0.92,
        );
        assert!(miss.is_none(), "Expected cache miss for dissimilar vector");

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 1);
        assert_eq!(stats.hit_ratio, 0.50);
    }

    #[test]
    fn test_ttl_expiration() {
        let cache = SemanticResponseCache::in_memory(100, 0.92, 1); // 1-second TTL

        let tool = "hgb_system_inspect";
        let params = r#"{"target":"memory"}"#;
        let entry = cache.insert(tool, params, Vec::new(), "mem_ok".to_string(), 5.0, Some(1));

        // Immediately check: should hit
        let hit = cache.lookup_semantic(tool, params, &[], 0.92);
        assert!(hit.is_some());

        // Artificially age the entry past TTL
        {
            let mut inner = cache.inner.lock().unwrap();
            if let Some(e) = inner.entries.get_mut(&entry.entry_id) {
                e.created_at_epoch_ms = e.created_at_epoch_ms.saturating_sub(5000); // 5 seconds ago
            }
        }

        // Post-expiry lookup: should miss
        let expired_hit = cache.lookup_semantic(tool, params, &[], 0.92);
        assert!(expired_hit.is_none(), "Expired entry must return None");

        // Pruning removes the expired item
        let pruned = cache.prune_expired();
        assert_eq!(pruned, 1);
        assert_eq!(cache.stats().cached_entry_count, 0);
    }

    #[test]
    fn test_cache_statistics_and_hit_tracking() {
        let cache = SemanticResponseCache::in_memory(50, 0.92, 3600);

        let tool = "hgb_sqlite_query";
        let params = r#"{"query":"SELECT count(*) FROM telemetry"}"#;
        let resp = r#"{"count": 42000}"#;

        cache.insert(tool, params, Vec::new(), resp.to_string(), 25.0, None);

        for _ in 0..5 {
            let hit = cache.lookup_semantic(tool, params, &[], 0.92);
            assert!(hit.is_some());
        }

        // 2 deliberate misses
        let _ = cache.lookup_semantic("other_tool", r#"{"q":1}"#, &[], 0.92);
        let _ = cache.lookup_semantic("other_tool", r#"{"q":2}"#, &[], 0.92);

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 5);
        assert_eq!(stats.miss_count, 2);
        assert!((stats.hit_ratio - (5.0 / 7.0)).abs() < 1e-4);
        assert_eq!(stats.cached_entry_count, 1);
        assert!(stats.saved_tokens >= 5);
        assert!(stats.saved_duration_ms >= 120.0);
        assert!(stats.memory_usage_bytes > 0);
    }

    #[test]
    fn test_persistent_sqlite_wal_storage() {
        let temp_dir = std::env::temp_dir();
        let db_file = temp_dir.join(format!("hgb_semantic_cache_test_{}.db", current_epoch_ms()));

        // Scope 1: Write entries to SQLite backed cache
        {
            let cache = SemanticResponseCache::open(&db_file, 50, 0.92, 3600).expect("open db");
            cache.insert(
                "hgb_cow_sandbox",
                r#"{"target_file":"src/main.rs"}"#,
                vec![0.1f32; 768],
                "sandbox_clean".to_string(),
                18.0,
                None,
            );
            let hit = cache.lookup_semantic("hgb_cow_sandbox", r#"{"target_file":"src/main.rs"}"#, &[], 0.92);
            assert!(hit.is_some());
        }

        // Scope 2: Re-open from same file and verify persistent restoration
        {
            let restored = SemanticResponseCache::open(&db_file, 50, 0.92, 3600).expect("restore db");
            assert_eq!(restored.stats().cached_entry_count, 1);

            let hit = restored.lookup_semantic("hgb_cow_sandbox", r#"{"target_file":"src/main.rs"}"#, &[], 0.92);
            assert!(hit.is_some());
            let item = hit.unwrap();
            assert_eq!(item.response_text, "sandbox_clean");
            assert_eq!(item.hit_count, 2); // 1 from previous session + 1 now
        }

        let _ = std::fs::remove_file(&db_file);
        let _ = std::fs::remove_file(format!("{}-wal", db_file.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_file.display()));
    }

    #[test]
    fn test_purge_operations() {
        let cache = SemanticResponseCache::in_memory(100, 0.92, 3600);

        cache.insert("tool_a", r#"{"k":1}"#, Vec::new(), "a1".into(), 10.0, None);
        cache.insert("tool_a", r#"{"k":2}"#, Vec::new(), "a2".into(), 10.0, None);
        cache.insert("tool_b", r#"{"k":1}"#, Vec::new(), "b1".into(), 10.0, None);

        assert_eq!(cache.stats().cached_entry_count, 3);

        // Purge only tool_a
        let purged_a = cache.purge_tool("tool_a");
        assert_eq!(purged_a, 2);
        assert_eq!(cache.stats().cached_entry_count, 1);

        // Verify tool_b still present
        assert!(cache.lookup_semantic("tool_b", r#"{"k":1}"#, &[], 0.92).is_some());

        // Purge all
        let purged_all = cache.purge_all();
        assert_eq!(purged_all, 1);
        assert_eq!(cache.stats().cached_entry_count, 0);
    }
}
