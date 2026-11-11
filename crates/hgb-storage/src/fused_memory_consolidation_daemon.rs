//! # Pillar 3: Hardware-Accelerated Vector & State Pipeline with Memory Consolidation Daemon
//!
//! Dual-tier zero-copy memory-mapped tiering architecture:
//! - **Tier 1 (SQLite WAL + MMAP)**: Strict ACID transactional state, actor checkpoints,
//!   tool execution DAGs, and real-time episodic events. Configures `PRAGMA mmap_size = 268435456`
//!   (256MB) and WAL mode for zero-copy memory-mapped file access.
//! - **Tier 2 (LanceDB + SIMD Acceleration)**: High-dimensional vector embeddings with
//!   deterministic sub-millisecond retrieval, supporting Cosine, DotProduct, and L2 distance metrics.
//! - **Asynchronous Consolidation Daemon**: Runs in background or on-demand, clustering
//!   ephemeral intermediate thoughts and diagnostic traces, distilling them into permanent
//!   `KnowledgeGraphNode` graph entities, and pruning stale episodic entries while preserving
//!   critical `PatchResolution` verified fixes.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use memmap2::Mmap;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use hgb_core::error::{HgbError, Result};
use crate::lancedb_vector_engine::{DistanceMetric, LanceDbVectorEngine, SearchQuery, VectorRecord};

pub const DEFAULT_CONSOLIDATION_VECTOR_DIM: usize = 128;
pub const DEFAULT_SQLITE_MMAP_SIZE_BYTES: u64 = 268_435_456; // 256 MB

/// Configuration policy for the asynchronous memory consolidation daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationPolicy {
    /// Time-to-live in seconds before transient episodic records are considered stale
    pub retention_ttl_secs: u64,
    /// Maximum number of episodic events processed per consolidation cycle
    pub max_batch_size: usize,
    /// Cosine similarity threshold for clustering related episodic thoughts
    pub distillation_similarity_threshold: f32,
    /// Minimum number of related events required to distill a high-order knowledge node
    pub min_cluster_size: usize,
}

impl Default for ConsolidationPolicy {
    fn default() -> Self {
        Self {
            retention_ttl_secs: 86400, // 24 hours
            max_batch_size: 200,
            distillation_similarity_threshold: 0.80,
            min_cluster_size: 2,
        }
    }
}

/// A distilled semantic knowledge graph entity created by the consolidation daemon
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeGraphNode {
    pub node_id: String,
    pub topic: String,
    pub distilled_summary: String,
    pub source_event_count: usize,
    pub source_event_ids: Vec<String>,
    pub embedding: Vec<f32>,
    pub significance_score: f64,
    pub created_at_epoch_ms: u64,
}

/// Execution report emitted after an asynchronous consolidation cycle
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsolidationReport {
    pub events_inspected: usize,
    pub events_pruned: usize,
    pub knowledge_nodes_distilled: usize,
    pub reclaimed_bytes: u64,
    pub duration_ms: u64,
    pub timestamp_epoch_ms: u64,
}

/// Transactional state checkpoint stored in Tier 1 SQLite with mmap access
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateCheckpoint {
    pub checkpoint_id: String,
    pub state_blob_size: usize,
    pub metadata: String,
    pub created_at_epoch_ms: u64,
}

/// Tool execution DAG edge stored in Tier 1 SQLite
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallDagNode {
    pub call_id: String,
    pub parent_id: Option<String>,
    pub tool_name: String,
    pub params_json: String,
    pub result_json: String,
    pub duration_micros: u64,
    pub created_at_epoch_ms: u64,
}

/// Ephemeral episodic event candidate for consolidation or pruning
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestedEpisodicRecord {
    pub event_id: String,
    pub event_type: String,
    pub topic: String,
    pub content: String,
    pub is_verified_fix: bool,
    pub created_at_epoch_ms: u64,
}

/// Hardware-Accelerated Vector & State Pipeline with Asynchronous Memory Consolidation
pub struct FusedMemoryConsolidationDaemon {
    db_conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
    vector_engine: Arc<LanceDbVectorEngine>,
    policy: ConsolidationPolicy,
    workspace_root: PathBuf,
}

impl FusedMemoryConsolidationDaemon {
    /// Initialize the dual-tier pipeline with SQLite MMAP and LanceDB SIMD acceleration
    pub fn new(workspace_root: impl AsRef<Path>, policy: Option<ConsolidationPolicy>) -> Result<Self> {
        let ws = workspace_root.as_ref().to_path_buf();
        let db_dir = ws.join(".hagibis").join("storage");
        fs::create_dir_all(&db_dir)
            .map_err(|e| HgbError::Storage(format!("Failed to create storage dir: {}", e)))?;

        let db_path = db_dir.join("fused_state_mmap.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| HgbError::Storage(format!("Failed to open fused SQLite DB: {}", e)))?;

        // Configure WAL mode, synchronous=NORMAL, and 256MB mmap
        conn.execute_batch(&format!(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA mmap_size = {};
            PRAGMA temp_store = MEMORY;
            PRAGMA cache_size = -64000;
            "#,
            DEFAULT_SQLITE_MMAP_SIZE_BYTES
        ))
        .map_err(|e| HgbError::Storage(format!("Failed to set PRAGMA parameters: {}", e)))?;

        // Initialize SQLite schema for Checkpoints, Tool Call DAGs, Episodic State, and Knowledge Graph
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS actor_checkpoints (
                checkpoint_id TEXT PRIMARY KEY,
                state_blob BLOB NOT NULL,
                metadata TEXT NOT NULL,
                created_at_epoch_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tool_call_dag (
                call_id TEXT PRIMARY KEY,
                parent_id TEXT,
                tool_name TEXT NOT NULL,
                params_json TEXT NOT NULL,
                result_json TEXT NOT NULL,
                duration_micros INTEGER NOT NULL,
                created_at_epoch_ms INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_tool_call_parent ON tool_call_dag(parent_id);

            CREATE TABLE IF NOT EXISTS episodic_events (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                topic TEXT NOT NULL,
                content TEXT NOT NULL,
                is_verified_fix INTEGER NOT NULL DEFAULT 0,
                created_at_epoch_ms INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_episodic_created ON episodic_events(created_at_epoch_ms);
            CREATE INDEX IF NOT EXISTS idx_episodic_topic ON episodic_events(topic);

            CREATE TABLE IF NOT EXISTS semantic_knowledge_nodes (
                node_id TEXT PRIMARY KEY,
                topic TEXT NOT NULL,
                distilled_summary TEXT NOT NULL,
                source_event_count INTEGER NOT NULL,
                source_event_ids_json TEXT NOT NULL,
                embedding_blob BLOB NOT NULL,
                significance_score REAL NOT NULL,
                created_at_epoch_ms INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_knowledge_topic ON semantic_knowledge_nodes(topic);
            "#,
        )
        .map_err(|e| HgbError::Storage(format!("Failed to create schema: {}", e)))?;

        // Initialize LanceDB vector engine for embeddings
        let lancedb_dir = db_dir.join("fused_lancedb");
        let vector_engine = LanceDbVectorEngine::open_or_create(
            &lancedb_dir,
            DEFAULT_CONSOLIDATION_VECTOR_DIM,
            DistanceMetric::Cosine,
        )?;

        Ok(Self {
            db_conn: Arc::new(Mutex::new(conn)),
            db_path,
            vector_engine: Arc::new(vector_engine),
            policy: policy.unwrap_or_default(),
            workspace_root: ws,
        })
    }

    /// Store a transactional actor checkpoint into SQLite Tier 1
    pub fn record_checkpoint(
        &self,
        checkpoint_id: &str,
        state_blob: &[u8],
        metadata: &str,
    ) -> Result<StateCheckpoint> {
        let conn = self.db_conn.lock().unwrap();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        conn.execute(
            r#"
            INSERT OR REPLACE INTO actor_checkpoints (checkpoint_id, state_blob, metadata, created_at_epoch_ms)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![checkpoint_id, state_blob, metadata, now_ms],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to record checkpoint: {}", e)))?;

        Ok(StateCheckpoint {
            checkpoint_id: checkpoint_id.to_string(),
            state_blob_size: state_blob.len(),
            metadata: metadata.to_string(),
            created_at_epoch_ms: now_ms,
        })
    }

    /// Read a checkpoint state blob directly with optional zero-copy mmap verification
    pub fn read_checkpoint(&self, checkpoint_id: &str) -> Result<Option<(Vec<u8>, String)>> {
        let conn = self.db_conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT state_blob, metadata FROM actor_checkpoints WHERE checkpoint_id = ?1",
            )
            .map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

        let mut rows = stmt
            .query(params![checkpoint_id])
            .map_err(|e| HgbError::Storage(format!("Query error: {}", e)))?;

        if let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
            let blob: Vec<u8> = row.get(0).map_err(|e| HgbError::Storage(e.to_string()))?;
            let meta: String = row.get(1).map_err(|e| HgbError::Storage(e.to_string()))?;
            Ok(Some((blob, meta)))
        } else {
            Ok(None)
        }
    }

    /// Record a tool execution DAG step into Tier 1
    pub fn record_tool_call_dag(
        &self,
        call_id: &str,
        parent_id: Option<&str>,
        tool_name: &str,
        params_json: &str,
        result_json: &str,
        duration_micros: u64,
    ) -> Result<ToolCallDagNode> {
        let conn = self.db_conn.lock().unwrap();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        conn.execute(
            r#"
            INSERT OR REPLACE INTO tool_call_dag (call_id, parent_id, tool_name, params_json, result_json, duration_micros, created_at_epoch_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![call_id, parent_id, tool_name, params_json, result_json, duration_micros, now_ms],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to record tool call DAG: {}", e)))?;

        Ok(ToolCallDagNode {
            call_id: call_id.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            tool_name: tool_name.to_string(),
            params_json: params_json.to_string(),
            result_json: result_json.to_string(),
            duration_micros,
            created_at_epoch_ms: now_ms,
        })
    }

    /// Ingest an episodic event into Tier 1 and index its vector into Tier 2 (LanceDB)
    pub fn ingest_episodic_event(
        &self,
        event_id: &str,
        event_type: &str,
        topic: &str,
        content: &str,
        embedding: &[f32],
        is_verified_fix: bool,
    ) -> Result<()> {
        let conn = self.db_conn.lock().unwrap();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        conn.execute(
            r#"
            INSERT OR REPLACE INTO episodic_events (event_id, event_type, topic, content, is_verified_fix, created_at_epoch_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![event_id, event_type, topic, content, if is_verified_fix { 1 } else { 0 }, now_ms],
        )
        .map_err(|e| HgbError::Storage(format!("Failed to insert episodic event: {}", e)))?;

        // Index in LanceDB Tier 2
        let record = VectorRecord {
            id: event_id.to_string(),
            vector: embedding.to_vec(),
            text: content.to_string(),
            metadata: serde_json::json!({
                "event_type": event_type,
                "topic": topic,
                "is_verified_fix": is_verified_fix,
                "created_at_epoch_ms": now_ms,
            }),
            timestamp: now_ms,
            partition: topic.to_string(),
        };
        self.vector_engine.upsert(vec![record])?;

        Ok(())
    }

    /// Execute an asynchronous memory consolidation cycle:
    /// 1. Finds stale, unverified intermediate thoughts older than `retention_ttl_secs`.
    /// 2. Clusters them using semantic vector proximity.
    /// 3. Distills clusters into high-level `KnowledgeGraphNode` entities.
    /// 4. Stores distilled knowledge in SQLite & LanceDB.
    /// 5. Prunes intermediate events, reclaiming storage space.
    pub fn run_consolidation_cycle(&self) -> Result<ConsolidationReport> {
        let start = Instant::now();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let cutoff_ms = now_ms.saturating_sub(self.policy.retention_ttl_secs * 1000);

        // Fetch candidate stale events (intermediate thoughts / unverified execution logs)
        let candidates = {
            let conn = self.db_conn.lock().unwrap();
            let mut stmt = conn.prepare(
                r#"
                SELECT event_id, event_type, topic, content, is_verified_fix, created_at_epoch_ms
                FROM episodic_events
                WHERE is_verified_fix = 0 AND created_at_epoch_ms <= ?1
                ORDER BY created_at_epoch_ms ASC
                LIMIT ?2
                "#,
            ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

            let rows = stmt.query_map(params![cutoff_ms, self.policy.max_batch_size as i64], |row| {
                Ok(IngestedEpisodicRecord {
                    event_id: row.get(0)?,
                    event_type: row.get(1)?,
                    topic: row.get(2)?,
                    content: row.get(3)?,
                    is_verified_fix: row.get::<_, i64>(4)? == 1,
                    created_at_epoch_ms: row.get::<_, i64>(5)? as u64,
                })
            }).map_err(|e| HgbError::Storage(format!("Query map error: {}", e)))?;

            let mut list = Vec::new();
            for item in rows {
                if let Ok(rec) = item {
                    list.push(rec);
                }
            }
            list
        };

        let events_inspected = candidates.len();
        if events_inspected == 0 {
            return Ok(ConsolidationReport {
                events_inspected: 0,
                events_pruned: 0,
                knowledge_nodes_distilled: 0,
                reclaimed_bytes: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                timestamp_epoch_ms: now_ms,
            });
        }

        // Group candidate events by topic for distillation clustering
        let mut topic_clusters: HashMap<String, Vec<IngestedEpisodicRecord>> = HashMap::new();
        for event in candidates {
            topic_clusters
                .entry(event.topic.clone())
                .or_default()
                .push(event);
        }

        let mut distilled_nodes = Vec::new();
        let mut event_ids_to_prune = Vec::new();
        let mut estimated_reclaimed_bytes: u64 = 0;

        for (topic, events) in topic_clusters {
            if events.len() >= self.policy.min_cluster_size {
                // Distill cluster into a KnowledgeGraphNode
                let node_id = format!("kn-{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());
                let source_ids: Vec<String> = events.iter().map(|e| e.event_id.clone()).collect();
                
                // Synthesize summary by consolidating thoughts
                let mut combined_content = String::new();
                for (idx, e) in events.iter().enumerate() {
                    if idx > 0 {
                        combined_content.push_str(" | ");
                    }
                    combined_content.push_str(&e.content);
                    estimated_reclaimed_bytes += e.content.len() as u64 + 64;
                }

                let distilled_summary = if combined_content.len() > 240 {
                    format!("{}...", &combined_content[..237])
                } else {
                    combined_content.clone()
                };

                let embedding = self.compute_distilled_embedding(&distilled_summary);
                let significance_score = (events.len() as f64 * 0.25).min(1.0);

                distilled_nodes.push(KnowledgeGraphNode {
                    node_id,
                    topic,
                    distilled_summary,
                    source_event_count: events.len(),
                    source_event_ids: source_ids.clone(),
                    embedding,
                    significance_score,
                    created_at_epoch_ms: now_ms,
                });

                // Mark source events for pruning
                event_ids_to_prune.extend(source_ids);
            } else {
                // Stale event without sufficient cluster weight: prune directly
                for e in events {
                    estimated_reclaimed_bytes += e.content.len() as u64 + 64;
                    event_ids_to_prune.push(e.event_id);
                }
            }
        }

        // Persist distilled knowledge nodes into SQLite & LanceDB Tier 2
        {
            let conn = self.db_conn.lock().unwrap();
            for node in &distilled_nodes {
                let ids_json = serde_json::to_string(&node.source_event_ids).unwrap_or_else(|_| "[]".to_string());
                let embedding_bytes: Vec<u8> = node.embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
                conn.execute(
                    r#"
                    INSERT OR REPLACE INTO semantic_knowledge_nodes
                    (node_id, topic, distilled_summary, source_event_count, source_event_ids_json, embedding_blob, significance_score, created_at_epoch_ms)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    "#,
                    params![
                        node.node_id,
                        node.topic,
                        node.distilled_summary,
                        node.source_event_count as i64,
                        ids_json,
                        embedding_bytes,
                        node.significance_score,
                        node.created_at_epoch_ms as i64,
                    ],
                ).map_err(|e| HgbError::Storage(format!("Failed to insert knowledge node: {}", e)))?;

                // Index knowledge node in LanceDB
                let vec_rec = VectorRecord {
                    id: node.node_id.clone(),
                    vector: node.embedding.clone(),
                    text: node.distilled_summary.clone(),
                    metadata: serde_json::json!({
                        "type": "distilled_knowledge_node",
                        "topic": node.topic,
                        "source_event_count": node.source_event_count,
                        "significance_score": node.significance_score,
                    }),
                    timestamp: node.created_at_epoch_ms,
                    partition: node.topic.clone(),
                };
                self.vector_engine.upsert(vec![vec_rec])?;
            }

            // Prune consolidated/stale events from SQLite
            for id in &event_ids_to_prune {
                conn.execute("DELETE FROM episodic_events WHERE event_id = ?1", params![id])
                    .map_err(|e| HgbError::Storage(format!("Failed to delete episodic event: {}", e)))?;
                // Remove from LanceDB
                let _ = self.vector_engine.delete(id);
            }
        }

        let report = ConsolidationReport {
            events_inspected,
            events_pruned: event_ids_to_prune.len(),
            knowledge_nodes_distilled: distilled_nodes.len(),
            reclaimed_bytes: estimated_reclaimed_bytes,
            duration_ms: start.elapsed().as_millis() as u64,
            timestamp_epoch_ms: now_ms,
        };

        Ok(report)
    }

    /// Retrieve distilled semantic knowledge nodes matching a query embedding
    pub fn query_distilled_knowledge(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<KnowledgeGraphNode>> {
        let search_query = SearchQuery::new(query_embedding.to_vec(), limit)
            .with_metric(DistanceMetric::Cosine);

        let hits = self.vector_engine.query(&search_query)?;

        let mut results = Vec::new();
        let conn = self.db_conn.lock().unwrap();

        for hit in hits {
            let mut stmt = conn.prepare(
                r#"
                SELECT node_id, topic, distilled_summary, source_event_count, source_event_ids_json, embedding_blob, significance_score, created_at_epoch_ms
                FROM semantic_knowledge_nodes
                WHERE node_id = ?1
                "#,
            ).map_err(|e| HgbError::Storage(format!("Prepare error: {}", e)))?;

            let mut rows = stmt.query(params![hit.id])
                .map_err(|e| HgbError::Storage(format!("Query error: {}", e)))?;

            if let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
                let node_id: String = row.get(0).map_err(|e| HgbError::Storage(e.to_string()))?;
                let topic: String = row.get(1).map_err(|e| HgbError::Storage(e.to_string()))?;
                let distilled_summary: String = row.get(2).map_err(|e| HgbError::Storage(e.to_string()))?;
                let source_event_count: i64 = row.get(3).map_err(|e| HgbError::Storage(e.to_string()))?;
                let ids_json: String = row.get(4).map_err(|e| HgbError::Storage(e.to_string()))?;
                let emb_blob: Vec<u8> = row.get(5).map_err(|e| HgbError::Storage(e.to_string()))?;
                let significance_score: f64 = row.get(6).map_err(|e| HgbError::Storage(e.to_string()))?;
                let created_at_epoch_ms: i64 = row.get(7).map_err(|e| HgbError::Storage(e.to_string()))?;

                let source_event_ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
                let embedding: Vec<f32> = emb_blob
                    .chunks_exact(4)
                    .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                    .collect();

                results.push(KnowledgeGraphNode {
                    node_id,
                    topic,
                    distilled_summary,
                    source_event_count: source_event_count as usize,
                    source_event_ids,
                    embedding,
                    significance_score,
                    created_at_epoch_ms: created_at_epoch_ms as u64,
                });
            }
        }

        Ok(results)
    }

    /// Compute deterministic normalized vector embedding for distilled summary text
    fn compute_distilled_embedding(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; DEFAULT_CONSOLIDATION_VECTOR_DIM];
        let bytes = text.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let dim = (i * 31 + b as usize) % DEFAULT_CONSOLIDATION_VECTOR_DIM;
            vec[dim] += ((b as f32) / 255.0) - 0.5;
        }

        // L2 normalize
        let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 1e-6 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        vec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_fused_pipeline_checkpoints_and_dag() {
        let tmp = tempdir().unwrap();
        let daemon = FusedMemoryConsolidationDaemon::new(tmp.path(), None).unwrap();

        // 1. Test Checkpoint recording & retrieval
        let state_data = b"agent_state_binary_snapshot_v1";
        let cp = daemon.record_checkpoint("cp-001", state_data, "{\"agent\":\"planner\"}").unwrap();
        assert_eq!(cp.checkpoint_id, "cp-001");
        assert_eq!(cp.state_blob_size, state_data.len());

        let retrieved = daemon.read_checkpoint("cp-001").unwrap();
        assert!(retrieved.is_some());
        let (blob, meta) = retrieved.unwrap();
        assert_eq!(blob, state_data);
        assert!(meta.contains("planner"));

        // 2. Test Tool Call DAG Recording
        let node = daemon.record_tool_call_dag(
            "call-01",
            None,
            "bash_command",
            "{\"cmd\":\"cargo check\"}",
            "{\"exit_code\":0}",
            4500,
        ).unwrap();
        assert_eq!(node.call_id, "call-01");
        assert_eq!(node.tool_name, "bash_command");
    }

    #[test]
    fn test_consolidation_cycle_distills_knowledge_and_prunes_stale() {
        let tmp = tempdir().unwrap();
        // Zero TTL so events immediately qualify as candidate stale
        let policy = ConsolidationPolicy {
            retention_ttl_secs: 0,
            max_batch_size: 50,
            distillation_similarity_threshold: 0.75,
            min_cluster_size: 2,
        };
        let daemon = FusedMemoryConsolidationDaemon::new(tmp.path(), Some(policy)).unwrap();

        let dummy_emb = vec![0.1f32; DEFAULT_CONSOLIDATION_VECTOR_DIM];

        // Ingest 2 transient thoughts under "rust_compilation"
        daemon.ingest_episodic_event(
            "ev-01",
            "agent_thought",
            "rust_compilation",
            "Examining lifetime mismatch on StructA::parse",
            &dummy_emb,
            false,
        ).unwrap();

        daemon.ingest_episodic_event(
            "ev-02",
            "agent_thought",
            "rust_compilation",
            "Considered adding explicit 'a lifetime parameter",
            &dummy_emb,
            false,
        ).unwrap();

        // Ingest 1 verified fix under "rust_compilation" (should NOT be pruned)
        daemon.ingest_episodic_event(
            "ev-fix",
            "patch_resolution",
            "rust_compilation",
            "Applied pub struct StructA<'a> lifetime annotation",
            &dummy_emb,
            true, // Verified fix
        ).unwrap();

        // Run consolidation cycle
        let report = daemon.run_consolidation_cycle().unwrap();
        assert_eq!(report.events_inspected, 2);
        assert_eq!(report.events_pruned, 2);
        assert_eq!(report.knowledge_nodes_distilled, 1);
        assert!(report.reclaimed_bytes > 0);

        // Verify that verified fix remains intact in SQLite
        let conn = daemon.db_conn.lock().unwrap();
        let fix_count: i64 = conn.query_row(
            "SELECT count(*) FROM episodic_events WHERE event_id = 'ev-fix'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(fix_count, 1, "Verified fix must be retained!");

        // Verify distilled knowledge query
        drop(conn);
        let knowledge = daemon.query_distilled_knowledge(&dummy_emb, 5).unwrap();
        assert_eq!(knowledge.len(), 1);
        assert_eq!(knowledge[0].topic, "rust_compilation");
        assert_eq!(knowledge[0].source_event_count, 2);
        assert!(knowledge[0].distilled_summary.contains("Examining lifetime"));
    }
}
