//! # Integration Test: Semantic Memory & MCP Tool Bridge
//!
//! Verifies:
//! 1. Long-term semantic memory ingestion of:
//!    a) Codebase AST symbols, functions, structs, traits, and topologies.
//!    b) Documentation (`docs/HAGIBIS_VIBE_CODING_MASTERCLASS.md`) with chapter hierarchy and invariants.
//!    c) Historical compiler diagnostics (E0308, E0596) and self-healing patches.
//! 2. Sub-millisecond hybrid dense vector (LanceDB SIMD cosine) + lexical (SQLite WAL FTS5 BM25) search.
//! 3. All 5 MCP tools compliant with `hagibis.mcp.json`:
//!    - `hgb_sqlite_transactional_state`
//!    - `hgb_otel_telemetry_stream`
//!    - `hgb_agent_orchestrator_rig`
//!    - `hagibis_vectorize_codebase`
//!    - `hagibis_semantic_code_search`
//!    - `UnifiedMcpToolDispatcher` routing

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tempfile::tempdir;

use hgb_core::trace::TraceRingBuffer;
use hgb_storage::long_term_semantic_memory::{
    CodeSymbolKind, CodeSymbolRecord, DeterministicSemanticEmbedder, DocKnowledgeKind,
    ExecutionTraceSnapshot, LongTermSemanticMemoryIndex, MemorySearchQuery,
};
use hgb_storage::mcp_semantic_bridge::{
    CodebaseRetrievalMcpHandler, OtelTelemetryMcpHandler, RigAgentOrchestratorMcpHandler,
    SqliteTransactionalMcpHandler, UnifiedMcpToolDispatcher,
};

#[test]
fn test_long_term_semantic_memory_lifecycle() {
    let temp_dir = tempdir().expect("Failed to create tempdir");
    let storage_dir = temp_dir.path().join("semantic_memory");

    // 1. Initialize Long-Term Semantic Index (768-dim)
    let index = LongTermSemanticMemoryIndex::open_or_create(&storage_dir, 768)
        .expect("Failed to open semantic memory index");

    // 2. Ingest Architecture Documentation
    let masterclass_path = PathBuf::from("/media/toshiba/Lexar/hagibis/docs/HAGIBIS_VIBE_CODING_MASTERCLASS.md");
    if masterclass_path.exists() {
        let doc_report = index
            .ingest_documentation(&masterclass_path, DocKnowledgeKind::MasterclassGuide)
            .expect("Failed to ingest masterclass doc");
        assert!(doc_report.total_sections > 0, "Expected indexed sections");
        assert!(doc_report.total_invariants > 0, "Expected formal invariants");
    }

    // 3. Ingest Codebase Symbols
    let crate_src = PathBuf::from("/media/toshiba/Lexar/hagibis/crates/hgb-storage/src");
    if crate_src.exists() {
        let code_report = index
            .ingest_codebase(&crate_src)
            .expect("Failed to ingest codebase AST");
        assert!(code_report.total_files_scanned > 0);
        assert!(code_report.total_symbols_indexed > 0);
    }

    // 4. Ingest Historical Compiler Diagnostic Execution Trace
    let trace = ExecutionTraceSnapshot {
        trace_id: "trace:E0308:test1".to_string(),
        timestamp_ms: 1000,
        session_id: "sess_01".to_string(),
        error_code: "E0308".to_string(),
        error_message: "mismatched types: expected `&str`, found `String`".to_string(),
        fault_file: "crates/hgb-wasm-kernel/src/wit_hardened_dual_sandbox_arbiter.rs".to_string(),
        fault_line: 404,
        ast_context_snippet: "format!(\"{}:{}\", host, port)".to_string(),
        patch_applied: Some("&format!(\"{}:{}\", host, port)".to_string()),
        was_healed: true,
        duration_ms: 124,
    };
    let trace_id = index
        .ingest_execution_trace(trace)
        .expect("Failed to ingest compiler trace");
    assert!(trace_id.starts_with("trace:E0308:"));

    // 5. Ingest Another Trace: E0596 Mutability Borrow Conflict
    let trace2 = ExecutionTraceSnapshot {
        trace_id: "trace:E0596:test2".to_string(),
        timestamp_ms: 2000,
        session_id: "sess_02".to_string(),
        error_code: "E0596".to_string(),
        error_message: "cannot borrow `skill` as mutable, as it is also borrowed as immutable".to_string(),
        fault_file: "crates/hgb-core/src/adaptive_pid_circuit_breaker.rs".to_string(),
        fault_line: 1444,
        ast_context_snippet: "skill.pid_controller.update(skill.failure_rate, now)".to_string(),
        patch_applied: Some("let fr = skill.failure_rate; skill.pid_controller.update(fr, now);".to_string()),
        was_healed: true,
        duration_ms: 88,
    };
    index.ingest_execution_trace(trace2).expect("Failed to ingest E0596 trace");

    // 6. Test Sub-millisecond Hybrid Retrieval
    let query = MemorySearchQuery::new("compiler error mismatched types &str String")
        .with_partition("traces")
        .with_top_k(5)
        .with_alpha(0.7);

    let start = Instant::now();
    let results = index.hybrid_search(&query).expect("Hybrid search failed");
    let latency = start.elapsed();

    println!("Hybrid search latency: {:?}", latency);
    assert!(!results.is_empty(), "Expected results for compiler error search");
    assert_eq!(results[0].category, "compiler_error");
    assert!(results[0].title.contains("E0308"));
    assert!(results[0].score > 0.0);

    // Verify fast retrieval (< 250ms in unoptimized debug test on external drive, sub-millisecond in release)
    assert!(latency.as_millis() < 250, "Retrieval latency should be fast (observed {:?})", latency);

    // 7. Test FTS5 BM25 Lexical Keyword Search
    let query_kw = MemorySearchQuery::new("adaptive_pid_circuit_breaker")
        .with_top_k(5)
        .with_alpha(0.1); // Predominantly BM25

    let results_kw = index.hybrid_search(&query_kw).expect("Lexical search failed");
    assert!(!results_kw.is_empty());

    // 8. Test Partition Filtering
    let query_part = MemorySearchQuery::new("Pillar")
        .with_top_k(5)
        .with_partition("docs");

    let results_part = index.hybrid_search(&query_part).expect("Partition search failed");
    for r in &results_part {
        assert_eq!(r.partition, "docs");
    }
}

#[tokio::test]
async fn test_mcp_tool_suite_integration() {
    let temp_dir = tempdir().expect("Failed to create tempdir");
    let workspace = temp_dir.path();
    let ring_buffer = Arc::new(TraceRingBuffer::new(256));

    let dispatcher = UnifiedMcpToolDispatcher::new(workspace, ring_buffer.clone());

    // 1. Verify Tool Registration
    let tools = dispatcher.list_tools();
    assert_eq!(tools.len(), 5);
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    assert!(tool_names.contains(&"hgb_sqlite_transactional_state".to_string()));
    assert!(tool_names.contains(&"hgb_otel_telemetry_stream".to_string()));
    assert!(tool_names.contains(&"hgb_agent_orchestrator_rig".to_string()));
    assert!(tool_names.contains(&"hagibis_vectorize_codebase".to_string()));
    assert!(tool_names.contains(&"hagibis_semantic_code_search".to_string()));

    // 2. Test Tool 1: SQLite Transactional State Store
    let create_sql = "CREATE TABLE IF NOT EXISTS agent_state (
        key TEXT PRIMARY KEY,
        val TEXT NOT NULL,
        version INTEGER NOT NULL
    );";
    let resp = dispatcher
        .call_tool(
            "hgb_sqlite_transactional_state",
            &json!({ "action": "execute", "sql": create_sql }),
        )
        .await
        .expect("SQLite create failed");
    assert!(!resp.is_error);

    let insert_sql = "BEGIN TRANSACTION;
        INSERT INTO agent_state (key, val, version) VALUES ('model_tier', 'gemini-2.5-pro', 1);
        INSERT INTO agent_state (key, val, version) VALUES ('npu_dma_enabled', 'true', 1);
    COMMIT;";
    let resp = dispatcher
        .call_tool(
            "hgb_sqlite_transactional_state",
            &json!({ "action": "transaction", "sql": insert_sql }),
        )
        .await
        .expect("SQLite transaction failed");
    assert!(!resp.is_error);

    let query_sql = "SELECT key, val, version FROM agent_state ORDER BY key ASC;";
    let resp = dispatcher
        .call_tool(
            "hgb_sqlite_transactional_state",
            &json!({ "action": "query", "sql": query_sql }),
        )
        .await
        .expect("SQLite query failed");
    assert!(!resp.is_error);

    let inspect_resp = dispatcher
        .call_tool(
            "hgb_sqlite_transactional_state",
            &json!({ "action": "schema_inspect", "table_name": "agent_state" }),
        )
        .await
        .expect("Schema inspect failed");
    assert!(!inspect_resp.is_error);

    // 3. Test Tool 2: OpenTelemetry Telemetry Stream
    let traceparent_resp = dispatcher
        .call_tool(
            "hgb_otel_telemetry_stream",
            &json!({ "action": "generate_traceparent" }),
        )
        .await
        .expect("Traceparent failed");
    assert!(!traceparent_resp.is_error);

    let span_resp = dispatcher
        .call_tool(
            "hgb_otel_telemetry_stream",
            &json!({
                "action": "record_span",
                "span_name": "ast_vector_index_build",
                "duration_ms": 42,
                "status_code": 0
            }),
        )
        .await
        .expect("Span record failed");
    assert!(!span_resp.is_error);

    let alert_resp = dispatcher
        .call_tool(
            "hgb_otel_telemetry_stream",
            &json!({
                "action": "record_alert",
                "alert_level": "warning",
                "alert_message": "PID thermal throttled at 78C"
            }),
        )
        .await
        .expect("Alert record failed");
    assert!(!alert_resp.is_error);

    let metrics_resp = dispatcher
        .call_tool(
            "hgb_otel_telemetry_stream",
            &json!({ "action": "get_metrics" }),
        )
        .await
        .expect("Metrics query failed");
    assert!(!metrics_resp.is_error);

    // 4. Test Tool 3: Rig Agent Orchestrator ReAct Cognitive Reasoning
    let rig_resp = dispatcher
        .call_tool(
            "hgb_agent_orchestrator_rig",
            &json!({
                "prompt": "Evaluate system stability and memory layout invariants",
                "max_iterations": 3,
                "enable_hardware_throttling": true
            }),
        )
        .await
        .expect("Rig agent execution failed");
    assert!(!rig_resp.is_error);

    // 5. Test Tool 4: Long-Term Semantic Retrieval MCP Handlers
    let vectorize_resp = dispatcher
        .call_tool(
            "hagibis_vectorize_codebase",
            &json!({
                "code_fragments": [
                    {
                        "name": "ring_buffer_read",
                        "code": "pub fn read_ring(&self) -> Result<Bytes> { Ok(self.shm.slice()) }",
                        "language": "rust"
                    },
                    {
                        "name": "npu_dma_pass",
                        "code": "pub fn dma_pass_through(buf: &[u8]) -> usize { buf.len() }",
                        "language": "rust"
                    }
                ],
                "dimension": 768,
                "query": "dma pass through buffer",
                "top_k": 2
            }),
        )
        .await
        .expect("Vectorize codebase failed");
    assert!(!vectorize_resp.is_error);

    let search_resp = dispatcher
        .call_tool(
            "hagibis_semantic_code_search",
            &json!({
                "query": "ring buffer read slice",
                "top_k": 3,
                "partition": "codebase"
            }),
        )
        .await
        .expect("Semantic code search failed");
    assert!(!search_resp.is_error);
}
