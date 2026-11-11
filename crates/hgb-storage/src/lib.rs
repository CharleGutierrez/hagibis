pub mod db_sentinel;
pub mod db_time_machine;
pub mod dna_store;
pub mod memory;
pub mod semantic_telepathy;
pub mod skills;
pub mod spec_store;
pub mod style;
pub mod vector;
pub mod simd_vector_index;
pub mod semantic_cache;
pub mod distributed_vector_engine;

pub use db_sentinel::DbSentinel;
pub use db_time_machine::{DbSandbox, DbSnapshot, DbTimeMachine};
pub use dna_store::ArchitecturalDnaStore;
pub use memory::{
    AdrStatus, ArchitecturalDecision, DebtSeverity, MemoryDocument,
    ProjectMemoryLedger, ProjectRoadmapMilestone, TechDebtEntry,
};
pub use semantic_telepathy::{
    compute_zero_cost_embedding, tokenize_code, vector_cosine_similarity,
    TelepathyDocument, TelepathyIndex, TelepathySearchResult,
};
pub use skills::{SkillMetadata, SkillRecord, SkillStore};
pub use spec_store::SpecStore;
pub use style::{StyleFeedbackRecord, StyleMemoryVault};
pub use vector::{cosine_similarity, VectorEntry, VectorIndex};
pub use simd_vector_index::{SimdSearchMatch, SimdVectorIndex, SimdVectorRecord};
pub use semantic_cache::{
    blob_to_embedding, canonicalize_json_str, compute_entry_id, dot_product_scalar,
    dot_product_simd, embedding_to_blob, simd_cosine_similarity as simd_cache_cosine_similarity,
    CachedSemanticResponse, SemanticCacheEntry, SemanticCacheHit, SemanticCacheStats,
    SemanticQueryCache, SemanticResponseCache, SemanticResponseCacheStats,
    DEFAULT_CACHE_CAPACITY, DEFAULT_CACHE_TTL_SECS, DEFAULT_EMBEDDING_DIM,
    DEFAULT_SIMILARITY_THRESHOLD,
};

/// Pillar 3: Semantic Response Cache module
pub mod semantic_response_cache {
    pub use crate::semantic_cache::{
        blob_to_embedding, canonicalize_json_str, compute_entry_id, dot_product_scalar,
        dot_product_simd, embedding_to_blob, simd_cosine_similarity, CachedSemanticResponse,
        SemanticResponseCache, SemanticResponseCacheStats, DEFAULT_CACHE_CAPACITY,
        DEFAULT_CACHE_TTL_SECS, DEFAULT_EMBEDDING_DIM, DEFAULT_SIMILARITY_THRESHOLD,
    };
}
pub use distributed_vector_engine::{
    DistributedVectorEngine, VectorEngineStatus, VectorSearchResult, VectorStreamEvent,
};
pub mod shm_ring_buffer;
pub use shm_ring_buffer::{
    DEFAULT_RING_CAPACITY, ShmBenchReport, ShmRingBuffer, ShmRingBufferStatus,
};

// Distributed Edge-to-Cloud State Synchronization (CRDT Engine)
pub use hgb_core::crdt_edge_sync::{
    AdrRecord, ClockRelation, CrdtSyncDocument, EdgeSyncPayload, ElementTag, LwwRegister,
    MergeReport, OrSet, SkillRecord as CrdtSkillRecord, StylePreferences, VectorClock, VectorEmbeddingRef,
};

// Strategic Upgrade 3: Advanced Vector Memory & Long-Horizon Self-Correction
pub mod advanced_vector_memory;
pub use advanced_vector_memory::{
    EpisodicMatch, EpisodicMemoryStatusReport, EpisodicRecord, EpisodicVectorMemoryEngine,
    SelfCorrectionReflection, DEFAULT_VECTOR_DIM,
};

// Strategic Upgrade 2: Distributed Raft Vector & State Mesh
pub mod raft_state_mesh;
pub use raft_state_mesh::{
    AppendEntriesArgs, AppendEntriesReply, RaftCommand, RaftLogEntry, RaftMeshError,
    RaftMeshStatusReport, RaftRole, RaftSnapshot, RaftStateMesh, RequestVoteArgs, RequestVoteReply,
    VectorSearchResult as RaftVectorSearchResult,
};

// Strategic Upgrade 4: Distributed Edge-to-Cloud Vector Mesh (CRDT Engine)
pub mod distributed_crdt_mesh;
pub use distributed_crdt_mesh::{
    AgentMemoryNode, CloudflareWorkerEdgePayload, CrdtMeshDelta, DistributedCrdtMesh,
    EpisodicLogEntry, LwwVectorRegister, MemoryTag, MeshMergeReport, OrSetMemory,
    VectorEmbeddingRecord,
};

// Pillar III: Decentralized MicroVM State Sync (CRDT Engine)
pub mod microvm_crdt_sync;
pub use microvm_crdt_sync::{
    EpisodicFact, MicroVmCrdtStateSync, MicroVmReplica, MicroVmSyncDelta, MicroVmSyncReport,
    ScratchpadThought, SharedStateVariable, ToolArtifactRecord,
};

// Advanced Sandboxing: Firecracker MicroVM Execution Logs & Artifacts Storage
pub mod microvm_execution_log;
pub use microvm_execution_log::{
    MicrovmExecutionLogStore, MicrovmExecutionRecord,
};

// Strategic Upgrade 5: Unified Embedded HNSW Vector Index (Pillar 2)
pub mod hnsw_vector_index;
pub use hnsw_vector_index::{
    DistanceMetric as HnswDistanceMetric, FastRng, HnswConfig, HnswSearchResult, HnswStats, HnswVectorIndex,
};

// Strategic Upgrade 4: Vector-Native SQLite/DuckDB Edge Indexing
pub mod vector_native_edge;
pub use vector_native_edge::{
    decode_vector_blob, decode_vector_blob_into, encode_vector_blob,
    EdgeCollection, EdgeDistanceMetric, EdgeSearchBenchmarkReport,
    EdgeSearchResult, EdgeVectorItem, HybridSearchQuery, VectorNativeEdgeEngine,
};

// Pillar 2: Self-Healing State Machine & Persistent Checkpointing
pub mod actor_checkpoint_store;
pub use actor_checkpoint_store::{
    ActorCheckpoint, ActorCheckpointStore, ActorGoalStatus, ActorStateSnapshot,
};

// Pillar 4: Edge-to-Core Sync with Cloudflare Workers
pub mod edge_sync_tunnel;
pub use edge_sync_tunnel::{
    EdgeEventType, EdgeSyncConfig, EdgeSyncTunnel, HybridLogicalClock, SyncDecision, SyncEvent,
};

// Pillar 3.3: Persistent Vector Embedding Cache
pub mod embedding_cache_store;
pub use embedding_cache_store::{
    EmbeddingBatchQuery, EmbeddingCacheRecord, EmbeddingCacheStats, EmbeddingCacheStore,
    EmbeddingInsertEntry,
};

// ============================================================================
// Additions to crates/hgb-storage/src/lib.rs
// ============================================================================

// Upgrade 3: Local Vector Memory Indexing with SQLite FTS5 + HNSW
pub mod fts5_hnsw_hybrid;
pub use fts5_hnsw_hybrid::{
    glob_match, glob_to_regex, sanitize_fts5_query, Fts5HnswHybridEngine,
    HybridDocumentChunk, HybridEngineConfig, HybridRankingMode,
    HybridSearchFilter, HybridSearchQuery as Fts5HybridSearchQuery,
    HybridSearchResult,
};

// Skill 2: Dynamic SIMD Width Runtime Dispatch (AVX-512, AVX2, ARM Neon)
pub mod simd_runtime_dispatch;
pub use simd_runtime_dispatch::{
    active_simd_target_name, cosine_similarity_scalar, detect_simd_architecture,
    detect_simd_capabilities, dot_product_scalar as simd_dot_product_scalar, euclidean_distance_scalar,
    euclidean_distance_sq_scalar, normalize_inplace_scalar, simd_cosine_similarity,
    simd_dot_product, simd_euclidean_distance, simd_euclidean_distance_sq,
    simd_normalize_inplace, SimdArchitecture, SimdCapabilities,
};

// Pillar 3: Zero-Copy Shared Memory IPC via hgb-storage
pub mod shm_zero_copy_storage;
pub use shm_zero_copy_storage::{
    resolve_shm_storage_path, ShmStorageHeader, ShmTaskDispatch, ShmTaskExecutionReport,
    ShmTaskType, ShmTelemetryFrame, ShmZeroCopyBenchReport, ShmZeroCopyConsumer,
    ShmZeroCopyProducer, ShmZeroCopyStorage, ShmZeroCopyStorageStatus, TokenStreamChunk,
    DEFAULT_STORAGE_RING_CAPACITY, SHM_ZERO_COPY_MAGIC, SHM_ZERO_COPY_VERSION,
    TAG_EMBEDDING, TAG_PROMPT, TAG_RAW, TAG_TASK_DISPATCH, TAG_TASK_EXECUTION_REPORT,
    TAG_TELEMETRY_FRAME, TAG_TOKEN_STREAM, TAG_WASM_MEMORY,
};

// Pillar 3: True Zero-Copy Shared Memory for AI Inference Pipelines
pub mod shm_inference_pipeline;
pub use shm_inference_pipeline::{
    resolve_infer_stream_path, stream_ollama_embedding_to_wasm, InferControlHeader, InferDataType, TensorFrameHeader,
    TensorStreamBenchReport, TensorStreamStatus, WasmDirectMemoryBridge,
    ZeroCopyTensorConsumer, ZeroCopyTensorProducer, ZeroCopyTensorStream,
    DEFAULT_INFER_STREAM_CAPACITY, FLAG_CHECKSUMMED, FLAG_INTERMEDIATE_ACTIVATION,
    FLAG_IS_FINAL, FLAG_KV_CACHE, INFER_CONTROL_HEADER_SIZE, INFER_FRAME_HEADER_SIZE,
    INFER_FRAME_MAGIC, INFER_STREAM_VERSION,
};

// Pillar 3: Zero-Copy Mmap Vector Index & SIMD Memory Engine
pub mod mmap_vector_index;
pub use mmap_vector_index::{
    align_up_64, compute_cosine_similarity, compute_dot_product, compute_euclidean_sq,
    cosine_similarity_unrolled_8x, dot_product_unrolled_8x, euclidean_sq_unrolled_8x,
    resolve_mmap_vector_path, MmapMetadataRecord, MmapRecordSummary, MmapVectorBenchReport,
    MmapVectorHeader, MmapVectorIndex, MmapVectorMatch, MmapVectorReader, VectorMetric,
    HEADER_SIZE as MMAP_HEADER_SIZE, METADATA_ENTRY_SIZE, MMAP_VECTOR_MAGIC, MMAP_VECTOR_VERSION,
};

// Enhancement 2: Embedded Hybrid RAG Vector Index Engine
pub mod hybrid_rag_vector_index;
pub use hybrid_rag_vector_index::{
    HybridDocument, HybridRagConfig, HybridRagSearchResult, HybridRagVectorIndex,
    VectorMetric as RagVectorMetric, BM25_B, BM25_K1, DEFAULT_RRF_K, HYBRID_RAG_MAGIC,
};

// Pillar 2: Native Vector/Embedding Caching in hgb-storage with SIMD Acceleration
pub mod native_simd_mmap_rag;
pub use native_simd_mmap_rag::{
    align_up_64 as simd_rag_align_up_64,
    calculate_layout as simd_rag_calculate_layout,
    compute_deterministic_embedding,
    handle_simd_vector_rag_query,
    handle_simd_vector_rag_upsert,
    mcp_tools_definitions as simd_rag_mcp_tools_definitions,
    resolve_rag_storage_path,
    simd_cosine_distance as rag_simd_cosine_distance,
    simd_cosine_similarity as rag_simd_cosine_similarity,
    simd_dot_product as rag_simd_dot_product,
    simd_euclidean_distance_sq as rag_simd_euclidean_distance_sq,
    NativeSimdMmapRag, NativeSimdMmapReader, NativeSimdRagPipeline,
    RagCategory, SimdRagBenchReport, SimdRagConfig, SimdRagContext,
    SimdRagHeader, SimdRagMatch, SimdRagMetadataEntry, SimdRagStats,
    SimdRagUpsertItem, VectorMetric as SimdRagVectorMetric,
    DEFAULT_RAG_CAPACITY, DEFAULT_RAG_DIMENSION, MAX_SOURCE_ID_LEN as SIMD_RAG_MAX_SOURCE_ID_LEN,
    MAX_TAG_LEN as SIMD_RAG_MAX_TAG_LEN, SIMD_RAG_HEADER_SIZE, SIMD_RAG_MAGIC,
    SIMD_RAG_METADATA_SIZE, SIMD_RAG_VERSION,
};

// Pillar 1: Vector-Native SQLite & In-Process HNSW Storage
pub mod vector_native_sqlite_vss;
pub use vector_native_sqlite_vss::{
    as_f32_slice as as_vss_f32_slice, cosine_distance as vss_cosine_distance,
    cosine_similarity as vss_cosine_similarity, decode_blob_into as vss_decode_blob_into,
    decode_blob_to_vector as vss_decode_blob_to_vector, dot_product as vss_dot_product,
    encode_vector_to_blob as vss_encode_vector_to_blob, FastRng as VssFastRng,
    VectorIndexConfig as VssIndexConfig, VectorNativeSearchResult, VectorNativeSqliteStore,
    VectorRecord as VssVectorRecord, VectorSearchResult as VssSearchResult,
    VectorStoreStats as VssStoreStats,
};

// Architectural Power-Up III: Vector-Native Storage & MCP Memory Mesh
pub mod vector_mcp_memory_mesh;
pub use vector_mcp_memory_mesh::{
    decode_vector_blob as decode_mcp_vector_blob, encode_vector_blob as encode_mcp_vector_blob,
    IndexOptions, IndexReport, MemoryFilter, MemoryItem, MemorySearchResult, VectorMcpMemoryMesh,
    CATEGORY_CODE_CHUNK, CATEGORY_CODE_CONTEXT, CATEGORY_COMMAND_OUTCOME, CATEGORY_INTERACTION,
    CATEGORY_PAST_FIX_PATTERN, CATEGORY_USER_PREFERENCE, DEFAULT_VECTOR_DIM as MCP_MEMORY_VECTOR_DIM,
};

// Pillar 1: LanceDB-Compatible Columnar Vector Engine
pub mod lancedb_vector_engine;
pub use lancedb_vector_engine::{
    DistanceMetric, LanceDbVectorEngine, SearchQuery, SearchResult, VectorRecord,
};

// Strategic Upgrade: Unified Vector-Relational Memory Bus (Zero-Copy SQLite CDC Bridge)
pub mod unified_vector_relational_bus;
pub use unified_vector_relational_bus::{
    AgentStateTransition, TrajectoryOutcome, TrajectorySearchQuery, TrajectorySemanticMatch,
    UnifiedBusStats, UnifiedVectorRelationalBus, DEFAULT_HOT_CACHE_CAPACITY,
    TRAJECTORY_VECTOR_DIM,
};

// Strategic Upgrade: Native GPU-Accelerated Vector RAG & Zero-Copy SHM Buffer
pub mod gpu_tensor_lancedb_rag;
pub use gpu_tensor_lancedb_rag::{
    GpuTensorConfig, GpuTensorLanceDbRAGBridge, GpuTensorSearchResult,
    GpuTensorShmBuffer, LocalEmbeddingCache, DEFAULT_EMBEDDING_CACHE_CAPACITY,
    DEFAULT_GPU_TENSOR_DIM, GPU_TENSOR_MAGIC, GPU_TENSOR_VERSION,
};

// ============================================================================
// Pillar 3: Reactive Event Bus & SQLite WAL Append-Only Event Log
// ============================================================================
pub mod append_only_event_bus;
pub use append_only_event_bus::{
    EventBusStore, GitCommitPushedPayload, MicroVmStateChangedPayload,
    ModelWeightsDownloadedPayload, SystemEventRecord, SystemEventType,
    ThermalThresholdBreachedPayload, ToolExecutionCompletedPayload,
    DEFAULT_EVENT_BUS_CAPACITY as SQLITE_EVENT_BUS_CAPACITY,
};

// ============================================================================
// Pillar 1: Supercharge the WASM Kernel & Edge Isolation (Precompiled Module Cache)
// ============================================================================
pub mod precompiled_wasm_cache;
pub use precompiled_wasm_cache::{
    PrecompiledCacheStats, PrecompiledWasmCache, CACHE_FILE_EXTENSION,
    COMPILER_VERSION_TAG, DEFAULT_MAX_MEMORY_ENTRIES,
};

// ============================================================================
// Pillar 1: Unified Zero-Copy Memory Bus & SQLite Hot-Path Cache
// ============================================================================

pub use hgb_core::zero_copy_memory_bus::{
    MemoryBusEnvelope, MemoryBusFrameHeader, MemoryBusRecord, MemoryBusStatus, ZeroCopyMemoryBus,
    DEFAULT_BUS_CAPACITY, MEMORY_BUS_MAGIC, MEMORY_BUS_VERSION,
};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use hgb_core::Result;

/// Telemetry metrics for hot-path cache preceding SQLite reads
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryBusCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub hit_ratio: f64,
    pub total_operations: u64,
    pub hot_path_keys_count: usize,
    pub capacity: usize,
}

/// Unified in-memory hot-path cache wired ahead of persistent SQLite storage.
///
/// Bypasses disk I/O on hot-path memory lookups (<10us latency), automatically populates
/// cache on miss from SQLite disk, and provides asynchronous write-behind persistence.
pub struct ZeroCopySqliteStore {
    bus: Arc<ZeroCopyMemoryBus>,
    db_path: PathBuf,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

impl ZeroCopySqliteStore {
    /// Initialize with SQLite persistence file path and ring buffer capacity
    pub fn new<P: AsRef<Path>>(db_path: P, capacity: usize) -> Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        let cap = if capacity == 0 { DEFAULT_BUS_CAPACITY } else { capacity };
        let bus = Arc::new(ZeroCopyMemoryBus::with_sqlite_persistence(cap, &path)?);
        Ok(Self {
            bus,
            db_path: path,
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
        })
    }

    /// Wrap an existing shared ZeroCopyMemoryBus instance
    pub fn with_bus<P: AsRef<Path>>(bus: Arc<ZeroCopyMemoryBus>, db_path: P) -> Result<Self> {
        let path = db_path.as_ref().to_path_buf();
        Ok(Self {
            bus,
            db_path: path,
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
        })
    }

    /// Access reference to the underlying ZeroCopyMemoryBus
    pub fn bus(&self) -> &Arc<ZeroCopyMemoryBus> {
        &self.bus
    }

    /// Path to persistent SQLite database
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Read hot-path memory record. Checks in-memory lock-free bus first (<10us, 0 disk I/O).
    /// If cache miss, queries SQLite disk storage and populates in-memory hot-path cache.
    pub fn get(&self, memory_key: &str) -> Result<Option<MemoryBusRecord>> {
        // 1. Hot-path DRAM check
        if let Some(record) = self.bus.get_hot_path(memory_key) {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(record));
        }

        // 2. Cache miss -> query SQLite disk
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
        if let Some(disk_record) = self.bus.query_sqlite_record(memory_key)? {
            // Warm the in-memory cache so subsequent queries hit the hot path
            let _ = self.bus.publish_record(disk_record.clone());
            return Ok(Some(disk_record));
        }

        Ok(None)
    }

    /// Store a memory record. Publishes to the zero-copy memory bus immediately (hot-path sub-10us),
    /// which asynchronously drains to SQLite via write-behind persistence.
    pub fn put(&self, record: MemoryBusRecord) -> Result<u64> {
        self.bus.publish_record(record)
    }

    /// Retrieve session timeline. Checks in-memory hot path first, falling back to disk if needed.
    pub fn get_session_timeline(&self, session_id: &str) -> Result<Vec<MemoryBusRecord>> {
        let in_memory_timeline = self.bus.get_session_timeline(session_id);
        if !in_memory_timeline.is_empty() {
            return Ok(in_memory_timeline);
        }
        self.bus.query_sqlite_session_timeline(session_id)
    }

    /// Trigger synchronous checkpoint flush to SQLite disk
    pub fn flush_checkpoint(&self) -> Result<usize> {
        self.bus.flush_checkpoint()
    }

    /// Telemetry metrics for hot-path cache
    pub fn stats(&self) -> MemoryBusCacheStats {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_ratio = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };
        let bus_status: MemoryBusStatus = self.bus.status();

        MemoryBusCacheStats {
            hits,
            misses,
            hit_ratio,
            total_operations: total,
            hot_path_keys_count: bus_status.hot_path_keys_count,
            capacity: bus_status.capacity,
        }
    }
}

/// Type alias for backward compatibility and ergonomic naming
pub type CachedSqliteMemoryStore = ZeroCopySqliteStore;

#[cfg(test)]
mod zero_copy_store_tests {
    use super::*;

    #[test]
    fn test_zero_copy_sqlite_store_cache_layer() {
        let db_path = std::env::temp_dir().join(format!(
            "hgb_zero_copy_storage_{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        let store = ZeroCopySqliteStore::new(&db_path, 256).expect("store init");

        let rec = MemoryBusRecord::new("sess-hgb", "agent.pref", b"vibe_coding".to_vec())
            .with_step_index(10);
        let seq = store.put(rec).expect("put");
        assert_eq!(seq, 1);

        // Immediate read: Cache hit in DRAM
        let hit = store.get("agent.pref").expect("get").expect("record exists");
        assert_eq!(hit.payload, b"vibe_coding");
        assert_eq!(store.stats().hits, 1);
        assert_eq!(store.stats().misses, 0);

        // Flush checkpoint to SQLite disk
        let flushed = store.flush_checkpoint().expect("flush");
        assert!(flushed >= 1);

        // Miss check
        assert!(store.get("unknown.key").expect("get").is_none());
        assert_eq!(store.stats().misses, 1);

        let _ = std::fs::remove_file(db_path);
    }
}// Strategic Upgrade: Unified Vector-Semantic Intent Router (LanceDB + SQLite + MCP)
pub mod unified_intent_router;
pub use unified_intent_router::{
    ContextualMatchRecord, IntentCdcEvent, IntentEventKind, IntentRouterMetrics,
    PrePromptContextBundle, UnifiedIntentRouter, DEFAULT_INTENT_SEARCH_TOP_K,
    DEFAULT_INTENT_VECTOR_DIM,
};

// Strategic Power-Up: Local Quantized Fine-Tuning Pipeline for LLM Reflection
pub mod reflection_finetune_pipeline;
pub use reflection_finetune_pipeline::{
    FinetuneRunReport, InstructionTuningPair, LoRAWeightDelta, QuantizationMode,
    QuantizedLoRAConfig, ReflectionFinetunePipeline, SidecarLoRAAdapterBundle,
};

// Strategic Power-Up: Real-Time Kernel & Hardware Telemetry Vector Bridge
pub mod telemetry_vector_bridge;
pub use telemetry_vector_bridge::{
    HardwareKernelTelemetrySnapshot, TelemetryMatch, TelemetryVectorBridge,
    DEFAULT_TELEMETRY_VECTOR_DIM,
};

// Strategic Power-Up: High-Throughput Zero-Copy Shared Memory IPC Broker
pub mod shm_ipc_broker;
pub use shm_ipc_broker::{
    ShmBrokerHeader, ShmChannelDescriptor, ShmIpcBroker, ZeroCopyPayloadDescriptor,
    DEFAULT_SHM_CAPACITY, SHM_BROKER_MAGIC,
};

// Strategic Power-Up Directive 3: Hardware-Accelerated Vector & Graph Relational Fusion Engine
pub mod vector_graph_relational_fused_engine;
pub use vector_graph_relational_fused_engine::{
    fused_simd_cosine_similarity, FusedQueryRequest, FusedQueryResultItem,
    FusedVectorGraphRelationalEngine, GraphEdge, GraphEntity,
    DEFAULT_FUSED_VECTOR_DIM,
};

// Strategic Power-Up Directive 2: True Hardware-Accelerated Local RAG (GPU/NPU Offloading)
pub mod vulkan_gpu_tensor_rag;
pub use vulkan_gpu_tensor_rag::{
    AcceleratedRagHit, GpuVectorBuffer, HardwareRagBackend, HardwareRagMetrics,
    VulkanGpuTensorRag, DEFAULT_GPU_VECTOR_DIM, DRM_AMD_RENDER_NODE,
    DRM_INTEL_RENDER_NODE, NVIDIA_DEV_NODE,
};

// Power Upgrade 3: Native Hardware-Accelerated Vector RAG with LanceDB + Dynatune
pub mod hardware_adaptive_vector_rag;
pub use hardware_adaptive_vector_rag::{
    AdaptiveHit, AdaptiveRagSearchResult, AdaptiveVectorRecord,
    HardwareAdaptiveVectorRag, VectorQuantizationLevel,
};

// Strategic Superpower 1: Unified Vector Memory with Distributed Graph RAG
pub mod distributed_graph_vector_rag;
pub use distributed_graph_vector_rag::{
    CodeAstSummary, CodeEdgeRelation, CodeGraphEdge, CodeGraphNode, CodeNodeType,
    DistributedGraphVectorRag, GraphPathStep, GraphVectorHit, GraphVectorQueryResult,
    GraphVectorQuerySpec, DEFAULT_GRAPH_VECTOR_DIM,
};

// Strategic Enhancement Pillar 3: Real-Time Episodic Memory Graph Bus
pub mod realtime_episodic_memory_bus;
pub use realtime_episodic_memory_bus::{
    EpisodicEvent, EpisodicEventType, EpisodicRecallQuery, EpisodicRecallResult,
    RealtimeEpisodicMemoryBus, DEFAULT_EPISODIC_VECTOR_DIM,
};






