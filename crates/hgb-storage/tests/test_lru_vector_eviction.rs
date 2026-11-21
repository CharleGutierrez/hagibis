//! # `test_lru_vector_eviction.rs` - Zero-Copy Storage Compaction & LRU Caching Verification
//! ========================================================================================
//! Tests:
//! 1. LRU eviction order based on access recency.
//! 2. TTL expiration pruning based on entry age ($N$ days).
//! 3. Relevance-scored filtering (low-relevance AST snapshots pruned).
//! 4. Zero-data-loss streaming Zstandard & GzipDeflate compression with CRC32 verification.
//! 5. Partition pressure tier response from statvfs guardrails.

use hgb_storage::lru_vector_eviction::{
    LruVectorCache, LruVectorEvictionConfig, VectorCompressionCodec, VectorEmbeddingSnapshot,
};
use hgb_storage::storage_guardrails_pruning_daemon::{PartitionStats, StoragePressureLevel};

#[test]
fn test_lru_recency_eviction_order() {
    let config = LruVectorEvictionConfig {
        max_capacity: 3,
        max_memory_bytes: 10 * 1024 * 1024,
        ttl_days: 30.0,
        min_relevance_threshold: 0.1,
        cold_idle_days_threshold: 10.0,
        codec: VectorCompressionCodec::Zstandard,
        zstd_compression_level: 1,
    };

    let mut cache = LruVectorCache::new(config);

    let e1 = VectorEmbeddingSnapshot::new("entry-1", vec![0.1, 0.2, 0.3], 0.9, None);
    let e2 = VectorEmbeddingSnapshot::new("entry-2", vec![0.4, 0.5, 0.6], 0.8, None);
    let e3 = VectorEmbeddingSnapshot::new("entry-3", vec![0.7, 0.8, 0.9], 0.7, None);

    cache.insert(e1);
    cache.insert(e2);
    cache.insert(e3);

    assert_eq!(cache.len(), 3);
    assert!(cache.contains_key("entry-1"));
    assert!(cache.contains_key("entry-2"));
    assert!(cache.contains_key("entry-3"));

    // Access entry-1, refreshing its LRU recency to the back of the queue
    let touched = cache.get("entry-1");
    assert!(touched.is_some());
    assert_eq!(touched.unwrap().access_count, 2);

    // Insert entry-4: capacity is 3, so the least recently used entry (entry-2) must be evicted!
    let e4 = VectorEmbeddingSnapshot::new("entry-4", vec![1.0, 1.1, 1.2], 0.6, None);
    cache.insert(e4);

    assert_eq!(cache.len(), 3);
    assert!(cache.contains_key("entry-1"), "entry-1 was accessed, must remain");
    assert!(!cache.contains_key("entry-2"), "entry-2 was LRU, must be evicted");
    assert!(cache.contains_key("entry-3"), "entry-3 remains");
    assert!(cache.contains_key("entry-4"), "entry-4 was just inserted");
}

#[test]
fn test_ttl_expiration_eviction() {
    let config = LruVectorEvictionConfig::default();
    let mut cache = LruVectorCache::new(config);

    let mut e1 = VectorEmbeddingSnapshot::new("old-entry", vec![0.1, 0.2], 0.85, None);
    let now = 1_700_000_000u64;

    // Simulate e1 created 15 days ago (15 * 86400 = 1,296,000 secs)
    e1.created_at_unix_secs = now - 15 * 86400;

    let mut e2 = VectorEmbeddingSnapshot::new("fresh-entry", vec![0.3, 0.4], 0.90, None);
    // e2 created 2 days ago
    e2.created_at_unix_secs = now - 2 * 86400;

    cache.insert(e1);
    cache.insert(e2);
    assert_eq!(cache.len(), 2);

    // Evict entries older than 14 days
    let evicted = cache.evict_expired(now, 14.0);
    assert_eq!(evicted, 1);
    assert!(!cache.contains_key("old-entry"));
    assert!(cache.contains_key("fresh-entry"));
}

#[test]
fn test_relevance_scored_pruning() {
    let config = LruVectorEvictionConfig::default();
    let mut cache = LruVectorCache::new(config);

    let low1 = VectorEmbeddingSnapshot::new("low-1", vec![0.1], 0.15, Some(b"ast-low-1".to_vec()));
    let low2 = VectorEmbeddingSnapshot::new("low-2", vec![0.2], 0.25, Some(b"ast-low-2".to_vec()));
    let high = VectorEmbeddingSnapshot::new("high-1", vec![0.9], 0.88, Some(b"ast-high-1".to_vec()));

    cache.insert(low1);
    cache.insert(low2);
    cache.insert(high);
    assert_eq!(cache.len(), 3);

    // Evict snapshots with relevance < 0.35
    let count = cache.evict_low_relevance(0.35);
    assert_eq!(count, 2);
    assert!(!cache.contains_key("low-1"));
    assert!(!cache.contains_key("low-2"));
    assert!(cache.contains_key("high-1"));
}

#[test]
fn test_zero_data_loss_compression_and_crc32() {
    let payload = b"fn sovereign_execution_loop() { let x = 42; let y = x * 2; return y; }".to_vec();
    let mut entry = VectorEmbeddingSnapshot::new("ast-code-1", vec![0.5, 0.5], 0.95, Some(payload.clone()));

    assert!(!entry.is_compressed);
    assert!(entry.uncompressed_ast_payload.is_some());
    let orig_crc = entry.crc32_checksum;
    assert_ne!(orig_crc, 0);

    // Compress with Zstandard
    let (orig_len, comp_len) = entry.compress_ast(VectorCompressionCodec::Zstandard, 3).expect("compress zstd");
    assert_eq!(orig_len, payload.len());
    assert!(comp_len > 0);
    assert!(entry.is_compressed);
    assert!(entry.uncompressed_ast_payload.is_none());
    assert!(entry.compressed_ast_payload.is_some());

    // Decompress and verify byte-for-byte exact match and CRC32 preservation
    let decomp_len = entry.decompress_ast(VectorCompressionCodec::Zstandard).expect("decompress zstd");
    assert_eq!(decomp_len, payload.len());
    assert!(!entry.is_compressed);
    assert_eq!(entry.crc32_checksum, orig_crc);
    assert_eq!(entry.uncompressed_ast_payload.as_ref().unwrap(), &payload);

    // Test GzipDeflate roundtrip as well
    let (g_orig, g_comp) = entry.compress_ast(VectorCompressionCodec::GzipDeflate, 6).expect("compress gzip");
    assert_eq!(g_orig, payload.len());
    assert!(g_comp > 0);
    assert!(entry.is_compressed);

    let g_decomp = entry.decompress_ast(VectorCompressionCodec::GzipDeflate).expect("decompress gzip");
    assert_eq!(g_decomp, payload.len());
    assert_eq!(entry.uncompressed_ast_payload.as_ref().unwrap(), &payload);
}

#[test]
fn test_storage_guardrails_pressure_tier_actuation() {
    let config = LruVectorEvictionConfig {
        max_capacity: 10,
        max_memory_bytes: 100_000,
        ttl_days: 10.0,
        min_relevance_threshold: 0.40,
        cold_idle_days_threshold: 2.0,
        codec: VectorCompressionCodec::Zstandard,
        zstd_compression_level: 1,
    };

    let mut cache = LruVectorCache::new(config);
    let now = 1_700_000_000u64;

    // Insert 5 entries:
    // 1 expired (>10d), 1 low relevance (<0.40), 1 cold (idle >2d), 2 normal active
    let mut e_expired = VectorEmbeddingSnapshot::new("expired", vec![0.1], 0.90, None);
    e_expired.created_at_unix_secs = now - 12 * 86400;

    let e_low = VectorEmbeddingSnapshot::new("low-rel", vec![0.2], 0.20, Some(b"small-ast".to_vec()));

    let mut e_cold = VectorEmbeddingSnapshot::new("cold-ast", vec![0.3], 0.80, Some(b"cold-heavy-ast-snapshot-payload".to_vec()));
    e_cold.last_accessed_unix_secs = now - 3 * 86400;

    let e_active1 = VectorEmbeddingSnapshot::new("active-1", vec![0.4], 0.85, None);
    let e_active2 = VectorEmbeddingSnapshot::new("active-2", vec![0.5], 0.95, None);

    cache.insert(e_expired);
    cache.insert(e_low);
    cache.insert(e_cold);
    cache.insert(e_active1);
    cache.insert(e_active2);
    assert_eq!(cache.len(), 5);

    // Tier 1: Normal (<75%)
    let stats_normal = PartitionStats {
        mount_point: "/".into(),
        total_bytes: 100_000_000_000,
        free_bytes: 50_000_000_000,
        system_free_bytes: 50_000_000_000,
        used_bytes: 50_000_000_000,
        used_pct: 50.0,
        total_inodes: 1_000_000,
        free_inodes: 900_000,
        inode_used_pct: 10.0,
        tier: StoragePressureLevel::Normal,
        timestamp_unix_secs: now,
    };

    let report = cache.enforce_storage_guardrails(&stats_normal, now);
    assert_eq!(report.expired_evicted, 1);
    assert!(!cache.contains_key("expired"));
    assert_eq!(cache.len(), 4);

    // Tier 3: AggressivePruning (80-90%)
    let stats_aggressive = PartitionStats {
        mount_point: "/".into(),
        total_bytes: 100_000_000_000,
        free_bytes: 15_000_000_000,
        system_free_bytes: 15_000_000_000,
        used_bytes: 85_000_000_000,
        used_pct: 85.0,
        total_inodes: 1_000_000,
        free_inodes: 500_000,
        inode_used_pct: 50.0,
        tier: StoragePressureLevel::AggressivePruning,
        timestamp_unix_secs: now,
    };

    let report_aggr = cache.enforce_storage_guardrails(&stats_aggressive, now);
    assert_eq!(report_aggr.low_relevance_evicted, 1);
    assert!(!cache.contains_key("low-rel"));
    assert!(report_aggr.compressed_cold_entries >= 1);
    let cold_entry = cache.get("cold-ast").unwrap();
    assert!(cold_entry.is_compressed);
}
