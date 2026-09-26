pub mod db_sentinel;
pub mod db_time_machine;
pub mod dna_store;
pub mod memory;
pub mod semantic_telepathy;
pub mod skills;
pub mod spec_store;
pub mod style;
pub mod vector;

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
