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
    SemanticCacheEntry, SemanticCacheHit, SemanticCacheStats, SemanticQueryCache,
};
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

// Strategic Upgrade 5: Unified Embedded HNSW Vector Index (Pillar 2)
pub mod hnsw_vector_index;
pub use hnsw_vector_index::{
    DistanceMetric, FastRng, HnswConfig, HnswSearchResult, HnswStats, HnswVectorIndex,
};




