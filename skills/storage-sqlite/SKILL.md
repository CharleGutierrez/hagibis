---
name: storage-sqlite
version: 0.1.0
tier: Persistence / Foundation
author: CaldoAI Untangling Architect
description: Pluggable SQLite & Vector persistence capability for Hagibis. Provides high-throughput concurrent storage with Write-Ahead Logging (WAL), FTS5 full-text indexing, zero-copy vector similarity search, and automated schema drift introspection.
triggers:
  - "storage-sqlite"
  - "vector-search"
  - "db-migrate"
  - "wal-checkpoint"
  - "fts-query"
---

# 🗄️ Skill: Pluggable SQLite & Vector Persistence

## 1. Overview
The `storage-sqlite` capability encapsulates all persistent relational and multi-modal vector state for the Hagibis system. By centralizing SQLite connection handling, WAL mode, FTS5 virtual tables, and SIMD-accelerated vector indexing, it eliminates data leakage across layers and guarantees ACID transaction guarantees with multi-reader concurrency.

## 2. Capabilities & Architecture
- **Concurrency & WAL Mode**: Multi-reader concurrent access with single-writer serialization:
  ```sql
  PRAGMA journal_mode = WAL;
  PRAGMA synchronous = NORMAL;
  PRAGMA foreign_keys = ON;
  PRAGMA busy_timeout = 5000;
  ```
- **FTS5 Semantic Telepathy**: Full-text indexing for instant semantic search across source code, specifications, and agent memories:
  ```sql
  CREATE VIRTUAL TABLE IF NOT EXISTS telepathy_fts USING fts5(
      doc_id UNINDEXED,
      category,
      title,
      content,
      tokenize='porter unicode61'
  );
  ```
- **In-Memory & Disk Vector Persistence**: Cosine similarity calculations over f32 embeddings with SIMD AVX-2 / NEON acceleration:
  * In-process index caching with zero-allocation slicing.
  * Dot-product and normalized cosine distance.
  * Top-K nearest neighbor search with dynamic score filtering.
- **Automated Schema Drift Introspection (`DbSentinel`)**: Dynamic PRAGMA table inspection validating migration compatibility before applying runtime alterations.

## 3. WebAssembly Interface Types (WIT)
Sandboxed Wasm guests interact with the persistence layer exclusively through the `hagibis:storage@0.1.0` WIT contract defined in `wit/storage-sqlite.wit`:
- `sqlite-store`: Safe parameterized query and execution interface.
- `vector-store`: Fast vector ingestion, cosine retrieval, and batch indexing.

## 4. Usage Example
```rust
use hgb_storage::{SkillRecord, SkillStore, VectorEntry, VectorIndex};

// Instantiate SQLite skill store with automatic WAL configuration
let store = SkillStore::new(".hgb/storage.db")?;

// Insert skill capability
store.insert_skill(&SkillRecord {
    name: "storage-sqlite".to_string(),
    description: "SQLite + Vector capability".to_string(),
    content: "...".to_string(),
    tier: "Persistence".to_string(),
    triggers: vec!["vector-search".to_string()],
})?;
```
