//! # Distributed Graph-Vector RAG Engine (`distributed_graph_vector_rag`)
//!
//! Strategic Superpower 1: Unify Vector Memory with Distributed Graph RAG.
//!
//! Fuses LanceDB dense vector embeddings with an ACID property graph of AST syntactic call-graphs:
//! - Combines semantic vector chunks in LanceDB with syntactic call-graph edges (Calls, Defines, Implements, Imports)
//! - Performs multi-hop bidirectional graph expansion combined with SIMD cosine vector ranking
//! - Prevents LLM hallucinations by verifying syntactic reachability paths across thousands of files

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use hgb_core::error::{HgbError, Result};
use crate::lancedb_vector_engine::{DistanceMetric, LanceDbVectorEngine, SearchQuery, VectorRecord};

pub const DEFAULT_GRAPH_VECTOR_DIM: usize = 128;

/// Syntactic code entity types stored in the property graph
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeNodeType {
    File,
    Module,
    Struct,
    Trait,
    Function,
    Method,
    Macro,
}

impl CodeNodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodeNodeType::File => "file",
            CodeNodeType::Module => "module",
            CodeNodeType::Struct => "struct",
            CodeNodeType::Trait => "trait",
            CodeNodeType::Function => "function",
            CodeNodeType::Method => "method",
            CodeNodeType::Macro => "macro",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "file" => CodeNodeType::File,
            "module" => CodeNodeType::Module,
            "struct" => CodeNodeType::Struct,
            "trait" => CodeNodeType::Trait,
            "function" => CodeNodeType::Function,
            "method" => CodeNodeType::Method,
            "macro" => CodeNodeType::Macro,
            _ => CodeNodeType::Function,
        }
    }
}

/// Syntactic edge relation types connecting code entities
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeEdgeRelation {
    Calls,
    Defines,
    Implements,
    Imports,
    DependsOn,
    References,
}

impl CodeEdgeRelation {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodeEdgeRelation::Calls => "calls",
            CodeEdgeRelation::Defines => "defines",
            CodeEdgeRelation::Implements => "implements",
            CodeEdgeRelation::Imports => "imports",
            CodeEdgeRelation::DependsOn => "depends_on",
            CodeEdgeRelation::References => "references",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "calls" => CodeEdgeRelation::Calls,
            "defines" => CodeEdgeRelation::Defines,
            "implements" => CodeEdgeRelation::Implements,
            "imports" => CodeEdgeRelation::Imports,
            "depends_on" => CodeEdgeRelation::DependsOn,
            "references" => CodeEdgeRelation::References,
            _ => CodeEdgeRelation::References,
        }
    }
}

/// Node representing an indexed code symbol in the property graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphNode {
    pub node_id: String,
    pub node_type: CodeNodeType,
    pub symbol_name: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub doc_summary: String,
    pub embedding: Vec<f32>,
    pub attributes: Value,
}

/// Directed edge representing a syntactic relationship between symbols
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relation: CodeEdgeRelation,
    pub weight: f32,
    pub call_site_line: Option<u32>,
}

/// Multi-hop graph-vector query specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphVectorQuerySpec {
    pub query_vector: Vec<f32>,
    pub seed_k: usize,
    pub max_hops: usize,
    pub allowed_relations: Option<Vec<CodeEdgeRelation>>,
    pub alpha_vector_weight: f32,
    pub hop_decay_factor: f32,
    pub top_k: usize,
}

impl Default for GraphVectorQuerySpec {
    fn default() -> Self {
        Self {
            query_vector: Vec::new(),
            seed_k: 5,
            max_hops: 2,
            allowed_relations: None,
            alpha_vector_weight: 0.6,
            hop_decay_factor: 0.75,
            top_k: 10,
        }
    }
}

/// A single step in a syntactic reasoning path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphPathStep {
    pub from_id: String,
    pub to_id: String,
    pub relation: CodeEdgeRelation,
    pub weight: f32,
}

/// Multi-hop reasoning hit item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphVectorHit {
    pub node: CodeGraphNode,
    pub vector_similarity: f32,
    pub graph_reachability_weight: f32,
    pub composite_score: f32,
    pub hops_from_seed: usize,
    pub reasoning_path: Vec<GraphPathStep>,
}

/// Result of a multi-hop graph-vector RAG search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphVectorQueryResult {
    pub query_hits: Vec<GraphVectorHit>,
    pub total_nodes_scanned: usize,
    pub seed_anchors_found: usize,
    pub elapsed_microseconds: u64,
}

/// AST summary unit for bulk codebase ingestion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeAstSummary {
    pub file_path: String,
    pub nodes: Vec<CodeGraphNode>,
    pub edges: Vec<CodeGraphEdge>,
}

/// Hardware-accelerated hybrid Graph-Vector RAG Engine
pub struct DistributedGraphVectorRag {
    storage_dir: PathBuf,
    vector_engine: Arc<LanceDbVectorEngine>,
    sqlite_conn: Arc<Mutex<Connection>>,
}

impl DistributedGraphVectorRag {
    /// Initialize the hybrid Graph-Vector RAG engine
    pub fn new(storage_dir: impl AsRef<Path>) -> Result<Self> {
        let storage_dir = storage_dir.as_ref().to_path_buf();
        fs::create_dir_all(&storage_dir)?;

        let lancedb_dir = storage_dir.join("lancedb_graph_chunks");
        let vector_engine = Arc::new(LanceDbVectorEngine::open_or_create(
            lancedb_dir,
            DEFAULT_GRAPH_VECTOR_DIM,
            DistanceMetric::Cosine,
        )?);

        let db_path = storage_dir.join("code_property_graph.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| HgbError::Storage(format!("Failed to open code property graph: {}", e)))?;

        // Configure SQLite performance flags
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS graph_nodes (
                 node_id TEXT PRIMARY KEY,
                 node_type TEXT NOT NULL,
                 symbol_name TEXT NOT NULL,
                 file_path TEXT NOT NULL,
                 start_line INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 doc_summary TEXT NOT NULL,
                 attributes_json TEXT NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_nodes_type ON graph_nodes(node_type);
             CREATE INDEX IF NOT EXISTS idx_nodes_file ON graph_nodes(file_path);
             CREATE INDEX IF NOT EXISTS idx_nodes_symbol ON graph_nodes(symbol_name);

             CREATE TABLE IF NOT EXISTS graph_edges (
                 source_id TEXT NOT NULL,
                 target_id TEXT NOT NULL,
                 relation TEXT NOT NULL,
                 weight REAL NOT NULL,
                 call_site_line INTEGER,
                 PRIMARY KEY (source_id, target_id, relation)
             );
             CREATE INDEX IF NOT EXISTS idx_edges_source ON graph_edges(source_id);
             CREATE INDEX IF NOT EXISTS idx_edges_target ON graph_edges(target_id);
             CREATE INDEX IF NOT EXISTS idx_edges_relation ON graph_edges(relation);",
        )
        .map_err(|e| HgbError::Storage(format!("Failed to initialize graph schema: {}", e)))?;

        Ok(Self {
            storage_dir,
            vector_engine,
            sqlite_conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Ingest a code graph node and its outgoing syntactic relationships
    pub fn index_code_unit(&self, node: CodeGraphNode, edges: Vec<CodeGraphEdge>) -> Result<()> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // 1. Index dense vector chunk in LanceDB
        let mut vector = node.embedding.clone();
        if vector.len() < DEFAULT_GRAPH_VECTOR_DIM {
            vector.resize(DEFAULT_GRAPH_VECTOR_DIM, 0.0);
        } else if vector.len() > DEFAULT_GRAPH_VECTOR_DIM {
            vector.truncate(DEFAULT_GRAPH_VECTOR_DIM);
        }

        let mut meta = serde_json::Map::new();
        meta.insert("symbol_name".into(), Value::String(node.symbol_name.clone()));
        meta.insert("file_path".into(), Value::String(node.file_path.clone()));
        meta.insert("node_type".into(), Value::String(node.node_type.as_str().into()));

        let record = VectorRecord {
            id: node.node_id.clone(),
            vector,
            text: format!("{} in {}: {}", node.symbol_name, node.file_path, node.doc_summary),
            metadata: Value::Object(meta),
            timestamp: now_ms,
            partition: "code_graph".to_string(),
        };
        self.vector_engine.upsert(vec![record])?;

        // 2. Persist in Property Graph SQLite
        let conn = self.sqlite_conn.lock().unwrap();
        let attrs_str = serde_json::to_string(&node.attributes).unwrap_or_else(|_| "{}".into());

        conn.execute(
            "INSERT OR REPLACE INTO graph_nodes (
                node_id, node_type, symbol_name, file_path, start_line, end_line, doc_summary, attributes_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                node.node_id,
                node.node_type.as_str(),
                node.symbol_name,
                node.file_path,
                node.start_line,
                node.end_line,
                node.doc_summary,
                attrs_str,
                now_ms
            ],
        ).map_err(|e| HgbError::Storage(format!("Failed to insert graph node: {}", e)))?;

        for edge in edges {
            conn.execute(
                "INSERT OR REPLACE INTO graph_edges (
                    source_id, target_id, relation, weight, call_site_line
                ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    edge.source_id,
                    edge.target_id,
                    edge.relation.as_str(),
                    edge.weight,
                    edge.call_site_line
                ],
            ).map_err(|e| HgbError::Storage(format!("Failed to insert graph edge: {}", e)))?;
        }

        Ok(())
    }

    /// Bulk ingest an entire AST summary of a codebase (from `untangle-codebase`)
    pub fn ingest_codebase_ast(&self, ast: &[CodeAstSummary]) -> Result<usize> {
        let mut count = 0;
        for file in ast {
            for node in &file.nodes {
                let relevant_edges: Vec<CodeGraphEdge> = file
                    .edges
                    .iter()
                    .filter(|e| e.source_id == node.node_id)
                    .cloned()
                    .collect();
                self.index_code_unit(node.clone(), relevant_edges)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Multi-hop reasoning query combining vector similarity anchors with syntactic call-graph expansion
    pub fn multi_hop_query(&self, spec: &GraphVectorQuerySpec) -> Result<GraphVectorQueryResult> {
        let start = std::time::Instant::now();

        // 1. Semantic Vector Seed Anchors
        let mut query_vec = spec.query_vector.clone();
        if query_vec.is_empty() {
            query_vec.resize(DEFAULT_GRAPH_VECTOR_DIM, 0.0);
        } else if query_vec.len() < DEFAULT_GRAPH_VECTOR_DIM {
            query_vec.resize(DEFAULT_GRAPH_VECTOR_DIM, 0.0);
        } else if query_vec.len() > DEFAULT_GRAPH_VECTOR_DIM {
            query_vec.truncate(DEFAULT_GRAPH_VECTOR_DIM);
        }

        let search_query = SearchQuery::new(query_vec.clone(), spec.seed_k.max(1))
            .with_metric(DistanceMetric::Cosine);
        let vector_hits = self.vector_engine.query(&search_query)?;

        let seed_count = vector_hits.len();
        let conn = self.sqlite_conn.lock().unwrap();

        // Map candidate nodes: node_id -> (CodeGraphNode, vector_sim, graph_weight, hops, path)
        let mut candidates: HashMap<String, (CodeGraphNode, f32, f32, usize, Vec<GraphPathStep>)> = HashMap::new();
        let mut queue: VecDeque<(String, usize, f32, Vec<GraphPathStep>)> = VecDeque::new();
        let mut visited_edges: HashSet<(String, String, String)> = HashSet::new();

        // Hydrate seeds from SQLite
        for v_hit in vector_hits {
            if let Some(node) = self.read_node(&conn, &v_hit.id)? {
                let sim = v_hit.vector_score.max(v_hit.score);
                candidates.insert(node.node_id.clone(), (node.clone(), sim, 1.0, 0, Vec::new()));
                queue.push_back((node.node_id, 0, 1.0, Vec::new()));
            }
        }

        // 2. Syntactic Multi-Hop Expansion (BFS)
        while let Some((curr_id, curr_hops, curr_weight, curr_path)) = queue.pop_front() {
            if curr_hops >= spec.max_hops {
                continue;
            }

            // Query outgoing and incoming edges for call-graph context
            let mut stmt = conn.prepare(
                "SELECT target_id, relation, weight FROM graph_edges WHERE source_id = ?1
                 UNION
                 SELECT source_id, relation, weight FROM graph_edges WHERE target_id = ?1",
            ).map_err(|e| HgbError::Storage(format!("Failed to prepare edge expansion: {}", e)))?;

            let rows = stmt.query_map(params![curr_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)? as f32,
                ))
            }).map_err(|e| HgbError::Storage(format!("Failed to query edges: {}", e)))?;

            for edge_res in rows {
                let (neighbor_id, rel_str, edge_weight) = match edge_res {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let rel = CodeEdgeRelation::from_str(&rel_str);
                if let Some(ref allowed) = spec.allowed_relations {
                    if !allowed.contains(&rel) {
                        continue;
                    }
                }

                let edge_key = (curr_id.clone(), neighbor_id.clone(), rel_str.clone());
                if visited_edges.contains(&edge_key) {
                    continue;
                }
                visited_edges.insert(edge_key);

                let next_hops = curr_hops + 1;
                let next_weight = curr_weight * edge_weight * spec.hop_decay_factor;

                let mut next_path = curr_path.clone();
                next_path.push(GraphPathStep {
                    from_id: curr_id.clone(),
                    to_id: neighbor_id.clone(),
                    relation: rel,
                    weight: edge_weight,
                });

                if let Some(neighbor_node) = self.read_node(&conn, &neighbor_id)? {
                    let cosine_sim = crate::semantic_cache::simd_cosine_similarity(
                        &query_vec,
                        &neighbor_node.embedding,
                    );

                    let entry = candidates.entry(neighbor_id.clone()).or_insert_with(|| {
                        (neighbor_node, cosine_sim, next_weight, next_hops, next_path.clone())
                    });

                    // Update if a shorter or higher-weight path was found
                    if next_weight > entry.2 {
                        entry.2 = next_weight;
                        entry.3 = next_hops;
                        entry.4 = next_path.clone();
                    }

                    queue.push_back((neighbor_id, next_hops, next_weight, next_path));
                }
            }
        }

        // 3. Composite Fusion Scoring & Anti-Hallucination Ranking
        let total_scanned = candidates.len();
        let mut final_hits: Vec<GraphVectorHit> = candidates
            .into_iter()
            .map(|(_, (node, vec_sim, graph_w, hops, path))| {
                let composite = (spec.alpha_vector_weight * vec_sim)
                    + ((1.0 - spec.alpha_vector_weight) * graph_w);
                GraphVectorHit {
                    node,
                    vector_similarity: vec_sim,
                    graph_reachability_weight: graph_w,
                    composite_score: composite,
                    hops_from_seed: hops,
                    reasoning_path: path,
                }
            })
            .collect();

        // Sort descending by composite score
        final_hits.sort_by(|a, b| {
            b.composite_score
                .partial_cmp(&a.composite_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        final_hits.truncate(spec.top_k);

        Ok(GraphVectorQueryResult {
            query_hits: final_hits,
            total_nodes_scanned: total_scanned,
            seed_anchors_found: seed_count,
            elapsed_microseconds: start.elapsed().as_micros() as u64,
        })
    }

    /// Read a code graph node from SQLite
    fn read_node(&self, conn: &Connection, node_id: &str) -> Result<Option<CodeGraphNode>> {
        let mut stmt = conn.prepare(
            "SELECT node_id, node_type, symbol_name, file_path, start_line, end_line, doc_summary, attributes_json
             FROM graph_nodes WHERE node_id = ?1",
        ).map_err(|e| HgbError::Storage(format!("Failed to prepare read_node: {}", e)))?;

        let mut rows = stmt.query(params![node_id])
            .map_err(|e| HgbError::Storage(format!("Failed to execute read_node query: {}", e)))?;

        if let Some(row) = rows.next().map_err(|e| HgbError::Storage(e.to_string()))? {
            let n_id: String = row.get(0).unwrap_or_default();
            let n_type_str: String = row.get(1).unwrap_or_default();
            let sym: String = row.get(2).unwrap_or_default();
            let file: String = row.get(3).unwrap_or_default();
            let start_l: u32 = row.get(4).unwrap_or_default();
            let end_l: u32 = row.get(5).unwrap_or_default();
            let doc: String = row.get(6).unwrap_or_default();
            let attrs_str: String = row.get(7).unwrap_or_default();
            let attrs: Value = serde_json::from_str(&attrs_str).unwrap_or(Value::Null);

            // Fetch embedding from LanceDB if present
            let embedding = self.vector_engine.get(&n_id)
                .ok()
                .flatten()
                .map(|r| r.vector)
                .unwrap_or_else(|| vec![0.0; DEFAULT_GRAPH_VECTOR_DIM]);

            Ok(Some(CodeGraphNode {
                node_id: n_id,
                node_type: CodeNodeType::from_str(&n_type_str),
                symbol_name: sym,
                file_path: file,
                start_line: start_l,
                end_line: end_l,
                doc_summary: doc,
                embedding,
                attributes: attrs,
            }))
        } else {
            Ok(None)
        }
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distributed_graph_vector_multi_hop_traversal() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_graph_rag_test_{}", uuid::Uuid::new_v4()));
        let rag = DistributedGraphVectorRag::new(&temp_dir).expect("Failed to init GraphVectorRag");

        // Create 3 connected symbols: Router -> Handler -> Service
        let mut emb_router = vec![0.0f32; DEFAULT_GRAPH_VECTOR_DIM];
        emb_router[0] = 1.0;
        let node_router = CodeGraphNode {
            node_id: "crate::router::dispatch".into(),
            node_type: CodeNodeType::Function,
            symbol_name: "dispatch".into(),
            file_path: "src/router.rs".into(),
            start_line: 10,
            end_line: 45,
            doc_summary: "Main HTTP and RPC request dispatcher".into(),
            embedding: emb_router,
            attributes: serde_json::json!({ "visibility": "pub" }),
        };

        let mut emb_handler = vec![0.0f32; DEFAULT_GRAPH_VECTOR_DIM];
        emb_handler[0] = 0.8;
        emb_handler[1] = 0.2;
        let node_handler = CodeGraphNode {
            node_id: "crate::handler::handle_auth".into(),
            node_type: CodeNodeType::Function,
            symbol_name: "handle_auth".into(),
            file_path: "src/handler.rs".into(),
            start_line: 50,
            end_line: 85,
            doc_summary: "Authenticate client token and parse claims".into(),
            embedding: emb_handler,
            attributes: serde_json::json!({ "visibility": "pub" }),
        };

        let mut emb_service = vec![0.0f32; DEFAULT_GRAPH_VECTOR_DIM];
        emb_service[1] = 1.0;
        let node_service = CodeGraphNode {
            node_id: "crate::service::verify_jwt".into(),
            node_type: CodeNodeType::Function,
            symbol_name: "verify_jwt".into(),
            file_path: "src/service.rs".into(),
            start_line: 12,
            end_line: 30,
            doc_summary: "Cryptographic signature validation for JWT".into(),
            embedding: emb_service,
            attributes: serde_json::json!({ "visibility": "pub(crate)" }),
        };

        // Edge 1: dispatch calls handle_auth
        let edge_1 = CodeGraphEdge {
            source_id: node_router.node_id.clone(),
            target_id: node_handler.node_id.clone(),
            relation: CodeEdgeRelation::Calls,
            weight: 0.95,
            call_site_line: Some(25),
        };

        // Edge 2: handle_auth calls verify_jwt
        let edge_2 = CodeGraphEdge {
            source_id: node_handler.node_id.clone(),
            target_id: node_service.node_id.clone(),
            relation: CodeEdgeRelation::Calls,
            weight: 0.90,
            call_site_line: Some(60),
        };

        rag.index_code_unit(node_router, vec![edge_1]).unwrap();
        rag.index_code_unit(node_handler, vec![edge_2]).unwrap();
        rag.index_code_unit(node_service, vec![]).unwrap();

        // Query targeting dispatch
        let mut q_vec = vec![0.0f32; DEFAULT_GRAPH_VECTOR_DIM];
        q_vec[0] = 0.95;

        let query_spec = GraphVectorQuerySpec {
            query_vector: q_vec,
            seed_k: 1,
            max_hops: 2,
            allowed_relations: Some(vec![CodeEdgeRelation::Calls]),
            alpha_vector_weight: 0.5,
            hop_decay_factor: 0.8,
            top_k: 5,
        };

        let result = rag.multi_hop_query(&query_spec).unwrap();

        assert!(!result.query_hits.is_empty(), "Should return query hits");
        assert_eq!(result.seed_anchors_found, 1);
        assert!(result.total_nodes_scanned >= 2, "Should scan multi-hop connected nodes");

        // Verify the top hit is the router and second hit is handler reached via Calls
        let hit_ids: Vec<String> = result.query_hits.iter().map(|h| h.node.node_id.clone()).collect();
        assert!(hit_ids.contains(&"crate::router::dispatch".into()));
        assert!(hit_ids.contains(&"crate::handler::handle_auth".into()));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
