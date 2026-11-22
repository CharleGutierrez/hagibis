//! Comprehensive Integration Tests for Roadmap Item 4:
//! Autonomous AST Compaction & Vector Refactoring (`test_autonomous_ast_compaction`)
//!
//! Validates:
//! 1. Fragmentation detection: fragmented .vec/.col count, dead vector ratio from overwritten
//!    AST symbols, tombstones, and centroid distortion.
//! 2. Zero-loss merging of multiple vector fragments into a single consolidated, cacheline-aligned fragment.
//! 3. Correct retrieval of AST symbols post-compaction with unchanged or improved cosine similarity scores.
//! 4. SQLite WAL and table incremental vacuuming during compaction.

use hgb_storage::autonomous_ast_compaction_cron::{
    AutonomousAstCompactionCron, CompactionPolicy,
};
use hgb_storage::lancedb_vector_engine::{
    DistanceMetric, LanceDbVectorEngine, SearchQuery, VectorRecord,
};

#[test]
fn test_autonomous_ast_compaction_full_lifecycle() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let lance_path = temp_dir.path().join("ast_symbols.lance");
    let sqlite_path = temp_dir.path().join("ast_metadata.db");

    // Initialize LanceDbVectorEngine with 4-dimensional embeddings for AST symbols
    let dim = 4;
    let engine = LanceDbVectorEngine::open_or_create(&lance_path, dim, DistanceMetric::Cosine)
        .expect("create lance engine");

    // Initialize SQLite metadata database with WAL mode
    let conn = rusqlite::Connection::open(&sqlite_path).expect("open sqlite");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE ast_symbol_metadata (
             id TEXT PRIMARY KEY,
             symbol_name TEXT,
             file_path TEXT,
             line_number INTEGER
         );",
    )
    .expect("init sqlite");

    // 1. Ingest multiple fragments representing AST code symbols across separate commits
    // Batch 1: Functions
    let b1 = vec![
        VectorRecord::new("fn_ast_parse", vec![0.8, 0.6, 0.0, 0.0])
            .with_partition("ast_parser")
            .with_text("pub fn parse_ast(tokens: TokenStream) -> AstTree"),
        VectorRecord::new("fn_token_lex", vec![0.7, 0.7, 0.1, 0.0])
            .with_partition("lexer")
            .with_text("pub fn lex_source(src: &str) -> Vec<Token>"),
    ];
    engine.upsert(b1).expect("upsert b1");

    // Batch 2: Structs
    let b2 = vec![
        VectorRecord::new("struct_ast_node", vec![0.6, 0.8, 0.0, 0.1])
            .with_partition("ast_nodes")
            .with_text("pub struct AstNode { pub id: NodeId, pub kind: NodeKind }"),
    ];
    engine.upsert(b2).expect("upsert b2");

    // Batch 3: Type checker
    let b3 = vec![
        VectorRecord::new("fn_type_check", vec![0.1, 0.2, 0.9, 0.3])
            .with_partition("type_checker")
            .with_text("pub fn type_check(ast: &AstTree) -> Result<TypeTable>"),
    ];
    engine.upsert(b3).expect("upsert b3");

    // Batch 4: Visitor
    let b4 = vec![
        VectorRecord::new("trait_ast_visitor", vec![0.5, 0.5, 0.5, 0.5])
            .with_partition("visitor")
            .with_text("pub trait AstVisitor { fn visit_node(&mut self, node: &AstNode); }"),
    ];
    engine.upsert(b4).expect("upsert b4");

    // Batch 5: Code generator & Overwrite of fn_ast_parse (producing dead vector)
    let b5 = vec![
        VectorRecord::new("fn_codegen", vec![0.2, 0.1, 0.3, 0.9])
            .with_partition("codegen")
            .with_text("pub fn generate_wasm(ast: &AstTree) -> Vec<u8>"),
        VectorRecord::new("fn_ast_parse", vec![0.85, 0.52, 0.05, 0.0])
            .with_partition("ast_parser")
            .with_text("pub fn parse_ast_v2(tokens: TokenStream, arena: &BumpArena) -> AstTree"),
    ];
    engine.upsert(b5).expect("upsert b5");

    // Batch 6: Delete fn_token_lex (producing a tombstone)
    engine.delete("fn_token_lex").expect("delete fn_token_lex");

    // Batch 7: Another symbol
    let b7 = vec![
        VectorRecord::new("struct_type_table", vec![0.15, 0.25, 0.85, 0.35])
            .with_partition("type_checker")
            .with_text("pub struct TypeTable { symbols: HashMap<String, TypeId> }"),
    ];
    engine.upsert(b7).expect("upsert b7");

    // Populate SQLite metadata
    conn.execute(
        "INSERT INTO ast_symbol_metadata VALUES ('fn_ast_parse', 'parse_ast_v2', 'src/ast.rs', 42);",
        [],
    )
    .expect("insert sqlite");
    conn.execute(
        "INSERT INTO ast_symbol_metadata VALUES ('struct_ast_node', 'AstNode', 'src/ast.rs', 105);",
        [],
    )
    .expect("insert sqlite");

    // 2. Query baseline similarity scores BEFORE compaction
    let query_parser = SearchQuery::new(vec![0.85, 0.52, 0.05, 0.0], 5)
        .with_metric(DistanceMetric::Cosine);
    let results_before_parser = engine.query(&query_parser).expect("query parser");
    let match_parse_before = results_before_parser
        .iter()
        .find(|r| r.id == "fn_ast_parse")
        .expect("found fn_ast_parse");
    let score_parse_before = match_parse_before.vector_score;

    let query_type = SearchQuery::new(vec![0.1, 0.2, 0.9, 0.3], 5)
        .with_metric(DistanceMetric::Cosine);
    let results_before_type = engine.query(&query_type).expect("query type");
    let match_type_before = results_before_type
        .iter()
        .find(|r| r.id == "fn_type_check")
        .expect("found fn_type_check");
    let score_type_before = match_type_before.vector_score;

    // Drop engine to flush all file handles
    drop(engine);

    // 3. Initialize AutonomousAstCompactionCron and verify Fragmentation Detection
    let policy = CompactionPolicy {
        max_fragments_threshold: 4,
        max_dead_vector_ratio: 0.10,
        max_centroid_distortion: 0.02,
        min_interval_secs: 0, // allow immediate compaction in test
        max_idle_load_average: 100.0,
        compact_sqlite: true,
        kmeans_iterations: 15,
    };

    let cron = AutonomousAstCompactionCron::new(&lance_path)
        .with_sqlite(&sqlite_path)
        .with_policy(policy);

    let frag_report = cron.analyze_fragmentation().expect("analyze fragmentation");

    // Assert fragmentation detection
    assert!(
        frag_report.fragment_count >= 5,
        "Expected >= 5 fragments, found {}",
        frag_report.fragment_count
    );
    assert_eq!(
        frag_report.tombstone_count, 1,
        "Expected 1 tombstone (fn_token_lex)"
    );
    assert!(
        frag_report.dead_vector_count >= 1,
        "Expected >= 1 dead vector (superseded fn_ast_parse)"
    );
    assert!(frag_report.dead_vector_ratio > 0.0);
    assert!(frag_report.requires_compaction);

    // 4. Trigger Autonomous Zero-Copy Background Compaction
    let compaction_report = cron.run_compaction().expect("execute compaction");

    // Assert zero-loss merging into a single consolidated fragment
    assert!(compaction_report.fragments_before >= 5);
    assert_eq!(
        compaction_report.fragments_after, 1,
        "Must consolidate into exactly 1 fragment"
    );
    assert_eq!(
        compaction_report.dead_vectors_purged,
        frag_report.dead_vector_count
    );
    assert_eq!(compaction_report.tombstones_purged, 1);
    assert!(compaction_report.bytes_reclaimed > 0);
    assert!(compaction_report.sqlite_vacuumed);

    // 5. Post-Compaction Retrieval Verification: Zero-Loss and Preserved/Improved Similarity
    let engine_after = LanceDbVectorEngine::open(&lance_path).expect("open compacted lance engine");

    // Verify deleted symbol is gone (tombstone honoured)
    assert!(engine_after.get("fn_token_lex").expect("get").is_none());

    // Verify active symbols exist and are intact
    let fn_ast_parse = engine_after
        .get("fn_ast_parse")
        .expect("get")
        .expect("fn_ast_parse exists");
    assert_eq!(
        fn_ast_parse.text,
        "pub fn parse_ast_v2(tokens: TokenStream, arena: &BumpArena) -> AstTree"
    );
    assert_eq!(fn_ast_parse.vector, vec![0.85, 0.52, 0.05, 0.0]);

    let fn_type_check = engine_after
        .get("fn_type_check")
        .expect("get")
        .expect("fn_type_check exists");
    assert_eq!(
        fn_type_check.text,
        "pub fn type_check(ast: &AstTree) -> Result<TypeTable>"
    );

    let struct_ast_node = engine_after
        .get("struct_ast_node")
        .expect("get")
        .expect("struct_ast_node exists");
    assert_eq!(struct_ast_node.partition, "ast_nodes");

    let trait_ast_visitor = engine_after
        .get("trait_ast_visitor")
        .expect("get")
        .expect("trait_ast_visitor exists");
    assert_eq!(trait_ast_visitor.partition, "visitor");

    // Query and verify cosine similarity is unchanged or improved
    let results_after_parser = engine_after
        .query(&query_parser)
        .expect("query parser after");
    let match_parse_after = results_after_parser
        .iter()
        .find(|r| r.id == "fn_ast_parse")
        .expect("found fn_ast_parse after");
    let score_parse_after = match_parse_after.vector_score;
    assert!(
        (score_parse_after - score_parse_before).abs() < 1e-5
            || score_parse_after >= score_parse_before,
        "Cosine similarity degraded: before={}, after={}",
        score_parse_before,
        score_parse_after
    );

    let results_after_type = engine_after.query(&query_type).expect("query type after");
    let match_type_after = results_after_type
        .iter()
        .find(|r| r.id == "fn_type_check")
        .expect("found fn_type_check after");
    let score_type_after = match_type_after.vector_score;
    assert!(
        (score_type_after - score_type_before).abs() < 1e-5 || score_type_after >= score_type_before,
        "Cosine similarity degraded: before={}, after={}",
        score_type_before,
        score_type_after
    );

    // Verify index is now completely healthy
    let frag_after = cron.analyze_fragmentation().expect("analyze post compaction");
    assert_eq!(frag_after.fragment_count, 1);
    assert_eq!(frag_after.dead_vector_count, 0);
    assert_eq!(frag_after.tombstone_count, 0);
    assert!(!frag_after.requires_compaction);
}
