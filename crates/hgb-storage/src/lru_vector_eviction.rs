//! # `lru_vector_eviction.rs` - Zero-Copy Storage Compaction & Tiered LRU Caching
//! ===============================================================================
//! Project Hagibis: Pillar 1 Enhancement (Storage Compaction & Tiered Caching)
//!
//! Production-grade LRU cache eviction and cold snapshot compaction engine:
//! 1. **Automated LRU Vector & AST Eviction**:
//!    - Evicts/prunes vector embeddings older than $N$ days (configurable TTL) or low-relevance
//!      AST snapshots before disk saturation occurs.
//!    - Maintains true LRU access recency order with dual index lookup (`HashMap` + `VecDeque`).
//! 2. **Relevance-Scored Pruning & Guardrails**:
//!    - Dynamically evaluates entry relevance score ($0.0 \le \text{relevance} \le 1.0$),
//!      access frequency, age in days, and disk pressure tier from `statvfs`.
//! 3. **Zero-Data-Loss Compression**:
//!    - Employs streaming Zstandard / GzipDeflate compression for cold AST snapshots.
//!    - Verifies CRC32 checksum integrity before unlinking or stripping uncompressed bytes.
//! 4. **Adaptive Tiered Storage Backpressure**:
//!    - Seamlessly integrates with `storage_guardrails_pruning_daemon::StoragePressureLevel`.
//!    - Under `Backpressure` (75-80%): compresses cold entries older than $N/2$ days.
//!    - Under `AggressivePruning` (80-90%): compacts all AST snapshots, purges sub-threshold relevance items.
//!    - Under `EmergencyDrop` (>90%): sheds non-essential embeddings, keeping strictly pinned/high-relevance.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage_guardrails_pruning_daemon::{PartitionStats, StoragePressureLevel};

/// Compression codec for cold AST snapshots
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorCompressionCodec {
    Zstandard,
    GzipDeflate,
}

impl Default for VectorCompressionCodec {
    fn default() -> Self {
        Self::Zstandard
    }
}

/// Metadata and payload of a vector embedding and its associated AST snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorEmbeddingSnapshot {
    pub entry_id: String,
    pub vector: Vec<f32>,
    pub relevance_score: f64,
    pub created_at_unix_secs: u64,
    pub last_accessed_unix_secs: u64,
    pub access_count: u64,
    pub is_compressed: bool,
    pub uncompressed_ast_payload: Option<Vec<u8>>,
    pub compressed_ast_payload: Option<Vec<u8>>,
    pub crc32_checksum: u32,
    pub size_bytes: usize,
}

impl VectorEmbeddingSnapshot {
    pub fn new(
        entry_id: impl Into<String>,
        vector: Vec<f32>,
        relevance_score: f64,
        ast_payload: Option<Vec<u8>>,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let mut crc = 0u32;
        let ast_len = if let Some(ref p) = ast_payload {
            crc = crc32fast::hash(p);
            p.len()
        } else {
            0
        };

        let vec_size = vector.len() * std::mem::size_of::<f32>();
        let size_bytes = vec_size + ast_len + std::mem::size_of::<Self>();

        Self {
            entry_id: entry_id.into(),
            vector,
            relevance_score: relevance_score.clamp(0.0, 1.0),
            created_at_unix_secs: now,
            last_accessed_unix_secs: now,
            access_count: 1,
            is_compressed: false,
            uncompressed_ast_payload: ast_payload,
            compressed_ast_payload: None,
            crc32_checksum: crc,
            size_bytes,
        }
    }

    /// Calculate entry age in days
    pub fn age_days(&self, now_unix_secs: u64) -> f64 {
        let elapsed = now_unix_secs.saturating_sub(self.created_at_unix_secs);
        elapsed as f64 / 86400.0
    }

    /// Calculate idle recency in days
    pub fn idle_days(&self, now_unix_secs: u64) -> f64 {
        let elapsed = now_unix_secs.saturating_sub(self.last_accessed_unix_secs);
        elapsed as f64 / 86400.0
    }

    /// Safely compress AST snapshot payload using Zstandard or Deflate with CRC32 verification
    pub fn compress_ast(
        &mut self,
        codec: VectorCompressionCodec,
        zstd_level: i32,
    ) -> io::Result<(usize, usize)> {
        if self.is_compressed || self.uncompressed_ast_payload.is_none() {
            return Ok((0, 0));
        }

        let raw = self.uncompressed_ast_payload.as_ref().unwrap();
        let orig_len = raw.len();
        if orig_len == 0 {
            return Ok((0, 0));
        }

        // 1. Verify CRC32 before compression
        let pre_crc = crc32fast::hash(raw);
        if self.crc32_checksum != 0 && self.crc32_checksum != pre_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CRC32 mismatch on entry [{}]: recorded 0x{:08x}, computed 0x{:08x}",
                    self.entry_id, self.crc32_checksum, pre_crc
                ),
            ));
        }
        self.crc32_checksum = pre_crc;

        // 2. Compress
        let compressed = match codec {
            VectorCompressionCodec::Zstandard => {
                zstd::encode_all(&raw[..], zstd_level)?
            }
            VectorCompressionCodec::GzipDeflate => {
                let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(raw)?;
                encoder.finish()?
            }
        };

        let comp_len = compressed.len();

        // 3. Verify round-trip decompression integrity (Zero-Data-Loss Invariant)
        let decompressed = match codec {
            VectorCompressionCodec::Zstandard => {
                zstd::decode_all(&compressed[..])?
            }
            VectorCompressionCodec::GzipDeflate => {
                let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
                let mut decomp = Vec::new();
                decoder.read_to_end(&mut decomp)?;
                decomp
            }
        };

        let post_crc = crc32fast::hash(&decompressed);
        if post_crc != pre_crc || decompressed != *raw {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Integrity verification failed during round-trip compression test",
            ));
        }

        // 4. Update entry state and discard raw payload
        self.compressed_ast_payload = Some(compressed);
        self.uncompressed_ast_payload = None;
        self.is_compressed = true;

        let vec_size = self.vector.len() * std::mem::size_of::<f32>();
        self.size_bytes = vec_size + comp_len + std::mem::size_of::<Self>();

        Ok((orig_len, comp_len))
    }

    /// Decompress and restore AST payload, verifying checksum
    pub fn decompress_ast(&mut self, codec: VectorCompressionCodec) -> io::Result<usize> {
        if !self.is_compressed || self.compressed_ast_payload.is_none() {
            return Ok(self.uncompressed_ast_payload.as_ref().map(|p| p.len()).unwrap_or(0));
        }

        let comp = self.compressed_ast_payload.as_ref().unwrap();
        let decompressed = match codec {
            VectorCompressionCodec::Zstandard => {
                zstd::decode_all(&comp[..])?
            }
            VectorCompressionCodec::GzipDeflate => {
                let mut decoder = flate2::read::GzDecoder::new(&comp[..]);
                let mut decomp = Vec::new();
                decoder.read_to_end(&mut decomp)?;
                decomp
            }
        };

        let post_crc = crc32fast::hash(&decompressed);
        if post_crc != self.crc32_checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "CRC32 mismatch on decompressing entry [{}]: expected 0x{:08x}, got 0x{:08x}",
                    self.entry_id, self.crc32_checksum, post_crc
                ),
            ));
        }

        let raw_len = decompressed.len();
        self.uncompressed_ast_payload = Some(decompressed);
        self.compressed_ast_payload = None;
        self.is_compressed = false;

        let vec_size = self.vector.len() * std::mem::size_of::<f32>();
        self.size_bytes = vec_size + raw_len + std::mem::size_of::<Self>();

        Ok(raw_len)
    }
}

/// Eviction and compaction report
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EvictionCompactionReport {
    pub total_scanned: usize,
    pub expired_evicted: usize,
    pub low_relevance_evicted: usize,
    pub lru_capacity_evicted: usize,
    pub compressed_cold_entries: usize,
    pub uncompressed_bytes_reduced: usize,
    pub compressed_bytes_current: usize,
    pub total_bytes_freed: usize,
    pub remaining_entries: usize,
}

/// Configuration options for the LRU Vector Cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LruVectorEvictionConfig {
    pub max_capacity: usize,
    pub max_memory_bytes: usize,
    pub ttl_days: f64,
    pub min_relevance_threshold: f64,
    pub cold_idle_days_threshold: f64,
    pub codec: VectorCompressionCodec,
    pub zstd_compression_level: i32,
}

impl Default for LruVectorEvictionConfig {
    fn default() -> Self {
        Self {
            max_capacity: 50_000,
            max_memory_bytes: 512 * 1024 * 1024, // 512 MB
            ttl_days: 14.0,                      // 14 days TTL default
            min_relevance_threshold: 0.35,        // < 0.35 considered low relevance
            cold_idle_days_threshold: 3.0,       // Idle for 3 days -> compress cold
            codec: VectorCompressionCodec::Zstandard,
            zstd_compression_level: 3,
        }
    }
}

/// Tiered LRU Vector & AST Snapshot Cache
#[derive(Debug, Clone)]
pub struct LruVectorCache {
    config: LruVectorEvictionConfig,
    entries: HashMap<String, VectorEmbeddingSnapshot>,
    lru_order: VecDeque<String>, // Front: least recently used; Back: most recently used
    total_bytes: usize,
}

impl LruVectorCache {
    pub fn new(config: LruVectorEvictionConfig) -> Self {
        Self {
            config,
            entries: HashMap::new(),
            lru_order: VecDeque::new(),
            total_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn config(&self) -> &LruVectorEvictionConfig {
        &self.config
    }

    /// Insert or update an entry in the LRU cache
    pub fn insert(&mut self, entry: VectorEmbeddingSnapshot) {
        let id = entry.entry_id.clone();
        let size = entry.size_bytes;

        if let Some(old) = self.entries.insert(id.clone(), entry) {
            self.total_bytes = self.total_bytes.saturating_sub(old.size_bytes);
            self.lru_order.retain(|k| k != &id);
        }

        self.total_bytes += size;
        self.lru_order.push_back(id);

        self.enforce_capacity_limits();
    }

    /// Retrieve an entry, marking it as most recently used (LRU touch)
    pub fn get(&mut self, entry_id: &str) -> Option<&VectorEmbeddingSnapshot> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        if let Some(entry) = self.entries.get_mut(entry_id) {
            entry.last_accessed_unix_secs = now;
            entry.access_count += 1;

            let id_str = entry_id.to_string();
            self.lru_order.retain(|k| k != &id_str);
            self.lru_order.push_back(id_str);

            self.entries.get(entry_id)
        } else {
            None
        }
    }

    /// Check if key exists without altering LRU order
    pub fn contains_key(&self, entry_id: &str) -> bool {
        self.entries.contains_key(entry_id)
    }

    /// Evict entries whose age exceeds $N$ days (TTL expiration)
    pub fn evict_expired(&mut self, now_unix_secs: u64, ttl_days: f64) -> usize {
        let mut expired_ids = Vec::new();
        for (id, entry) in &self.entries {
            if entry.age_days(now_unix_secs) >= ttl_days {
                expired_ids.push(id.clone());
            }
        }

        let count = expired_ids.len();
        for id in expired_ids {
            self.remove(&id);
        }
        count
    }

    /// Evict low-relevance AST snapshots whose relevance score falls below threshold
    pub fn evict_low_relevance(&mut self, min_relevance: f64) -> usize {
        let mut low_relevance_ids = Vec::new();
        for (id, entry) in &self.entries {
            if entry.relevance_score < min_relevance {
                low_relevance_ids.push(id.clone());
            }
        }

        let count = low_relevance_ids.len();
        for id in low_relevance_ids {
            self.remove(&id);
        }
        count
    }

    /// Compress cold AST snapshots (idle > threshold days)
    pub fn compress_cold_snapshots(
        &mut self,
        now_unix_secs: u64,
        idle_days_threshold: f64,
    ) -> (usize, usize, usize) {
        let mut compressed_count = 0;
        let mut uncompressed_total = 0;
        let mut compressed_total = 0;

        let codec = self.config.codec;
        let zstd_level = self.config.zstd_compression_level;

        for entry in self.entries.values_mut() {
            if !entry.is_compressed && entry.idle_days(now_unix_secs) >= idle_days_threshold {
                let old_size = entry.size_bytes;
                if let Ok((orig, comp)) = entry.compress_ast(codec, zstd_level) {
                    if orig > 0 {
                        compressed_count += 1;
                        uncompressed_total += orig;
                        compressed_total += comp;
                        self.total_bytes = self.total_bytes.saturating_sub(old_size) + entry.size_bytes;
                    }
                }
            }
        }

        (compressed_count, uncompressed_total, compressed_total)
    }

    /// Remove a specific entry by ID
    pub fn remove(&mut self, entry_id: &str) -> Option<VectorEmbeddingSnapshot> {
        if let Some(entry) = self.entries.remove(entry_id) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes);
            self.lru_order.retain(|k| k != entry_id);
            Some(entry)
        } else {
            None
        }
    }

    /// Enforce LRU capacity and byte bounds by popping oldest entries from front of queue
    fn enforce_capacity_limits(&mut self) {
        while self.entries.len() > self.config.max_capacity {
            if let Some(oldest_id) = self.lru_order.pop_front() {
                if let Some(entry) = self.entries.remove(&oldest_id) {
                    self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes);
                }
            } else {
                break;
            }
        }

        while self.total_bytes > self.config.max_memory_bytes && !self.entries.is_empty() {
            if let Some(oldest_id) = self.lru_order.pop_front() {
                if let Some(entry) = self.entries.remove(&oldest_id) {
                    self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes);
                }
            } else {
                break;
            }
        }
    }

    /// Autonomous Storage Guardrails Actuator:
    /// Responds to filesystem pressure tier detected by POSIX `statvfs`
    pub fn enforce_storage_guardrails(
        &mut self,
        stats: &PartitionStats,
        now_unix_secs: u64,
    ) -> EvictionCompactionReport {
        let mut report = EvictionCompactionReport {
            total_scanned: self.entries.len(),
            ..Default::default()
        };

        let initial_bytes = self.total_bytes;

        match stats.tier {
            StoragePressureLevel::Normal => {
                // Gentle maintenance: evict expired entries (> ttl_days)
                report.expired_evicted = self.evict_expired(now_unix_secs, self.config.ttl_days);
            }

            StoragePressureLevel::Backpressure => {
                // Tier 2 (75-80%): evict expired (> ttl_days/2) and compress cold (> idle_days/2)
                let half_ttl = (self.config.ttl_days / 2.0).max(1.0);
                let half_idle = (self.config.cold_idle_days_threshold / 2.0).max(0.5);

                report.expired_evicted = self.evict_expired(now_unix_secs, half_ttl);
                let (c_count, u_bytes, c_bytes) =
                    self.compress_cold_snapshots(now_unix_secs, half_idle);
                report.compressed_cold_entries = c_count;
                report.uncompressed_bytes_reduced = u_bytes;
                report.compressed_bytes_current = c_bytes;
            }

            StoragePressureLevel::AggressivePruning => {
                // Tier 3 (80-90%):
                // 1. Evict expired entries older than 3 days
                report.expired_evicted = self.evict_expired(now_unix_secs, 3.0);
                // 2. Evict low relevance snapshots (< min_relevance_threshold)
                report.low_relevance_evicted =
                    self.evict_low_relevance(self.config.min_relevance_threshold);
                // 3. Compress all uncompressed AST snapshots regardless of idle time
                let (c_count, u_bytes, c_bytes) = self.compress_cold_snapshots(now_unix_secs, 0.0);
                report.compressed_cold_entries = c_count;
                report.uncompressed_bytes_reduced = u_bytes;
                report.compressed_bytes_current = c_bytes;

                // 4. Enforce aggressive capacity reduction to 60% of max
                let target_capacity = (self.config.max_capacity * 6) / 10;
                while self.entries.len() > target_capacity {
                    if let Some(oldest_id) = self.lru_order.pop_front() {
                        if let Some(entry) = self.entries.remove(&oldest_id) {
                            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes);
                            report.lru_capacity_evicted += 1;
                        }
                    } else {
                        break;
                    }
                }
            }

            StoragePressureLevel::EmergencyDrop => {
                // Tier 4 (>90%):
                // Emergency shed: purge everything below high relevance (0.80),
                // compress everything remaining, limit to 20% capacity
                report.expired_evicted = self.evict_expired(now_unix_secs, 1.0);
                report.low_relevance_evicted = self.evict_low_relevance(0.80);
                let (c_count, u_bytes, c_bytes) = self.compress_cold_snapshots(now_unix_secs, 0.0);
                report.compressed_cold_entries = c_count;
                report.uncompressed_bytes_reduced = u_bytes;
                report.compressed_bytes_current = c_bytes;

                let emergency_capacity = (self.config.max_capacity * 2) / 10;
                while self.entries.len() > emergency_capacity {
                    if let Some(oldest_id) = self.lru_order.pop_front() {
                        if let Some(entry) = self.entries.remove(&oldest_id) {
                            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes);
                            report.lru_capacity_evicted += 1;
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        report.total_bytes_freed = initial_bytes.saturating_sub(self.total_bytes);
        report.remaining_entries = self.entries.len();
        report
    }
}
