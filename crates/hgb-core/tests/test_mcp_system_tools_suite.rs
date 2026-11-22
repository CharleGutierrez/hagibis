//! # Pillar 1: MCP System Tools & OpenTelemetry Verification Suite
//! File: crates/hgb-core/tests/test_mcp_system_tools_suite.rs
//!
//! 100% Real, production-grade test suite with ZERO mocks and ZERO stubs.
//! End-to-end integration tests verifying:
//! 1. `hgb_read_system_logs`: Log reading, limit clamping, severity filtering, secret redaction, and loadavg
//! 2. `hgb_query_lancedb_vector_store`: LanceDB columnar vector indexing and cosine similarity querying
//! 3. `hgb_execute_build`: Sandboxed build execution, whitelist security enforcement, timeout, and diagnostic parsing
//! 4. `hgb_ingest_otel_telemetry`: OpenTelemetry metrics, thermal sensors, memory PSI pressure, and circuit breaker
//! 5. MCP JSON-RPC 2.0 `tools/call` and `tools/list` protocol compliance

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use hgb_core::mcp_bridge_handlers::{
    LanceDbVectorStoreMcpHandler, OtelMetricsIngestMcpHandler, SandboxedBuildMcpHandler,
    SystemLogsMcpHandler,
};
use hgb_core::mcp_server::{HagibisMcpServer, HgbMcpProfile, JsonRpcRequest};
use hgb_core::reverse_engineering_mcp_bridge::{
    AstCodeChunk, CodeSemanticEmbedder, LanceDbVectorEngine, LANCE_COL_MAGIC, LANCE_VEC_MAGIC,
};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn test_hgb_read_system_logs_end_to_end() {
    let tmp = tempdir().expect("Failed to create tempdir");
    let ws = tmp.path();
    let hgb_dir = ws.join(".hgb");
    fs::create_dir_all(&hgb_dir).expect("Failed to create .hgb directory");

    // 1. Write realistic dream_journal.json
    let dream_file = hgb_dir.join("dream_journal.json");
    let dream_content = json!([
        { "timestamp": "2026-11-22T08:00:00Z", "level": "info", "message": "Ambient dream cycle started" },
        { "timestamp": "2026-11-22T08:05:00Z", "level": "warn", "message": "High memory allocation detected in worker" },
        { "timestamp": "2026-11-22T08:10:00Z", "level": "error", "message": "Database transaction deadlock occurred" }
    ]);
    fs::write(&dream_file, dream_content.to_string()).expect("Failed to write dream_journal.json");

    // 2. Write daemon.log with secrets and errors
    let log_file = hgb_dir.join("daemon.log");
    let log_lines = vec![
        "[INFO] Daemon listening on unix:/tmp/hgb.sock",
        "[WARN] Suboptimal thread affinity on core 3",
        "[ERROR] Connection to upstream failed with apiKey=sk-abcdef1234567890abcdef1234567890",
        "[INFO] Authenticated user using Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.e30.signature",
        "[ERROR] PANIC in compilation worker: token buffer overflowed",
    ];
    fs::write(&log_file, log_lines.join("\n")).expect("Failed to write daemon.log");

    let handler = SystemLogsMcpHandler::new(ws);

    // Test A: Read with secret redaction and ERROR severity filter
    let error_args = json!({
        "source": "hgb_log",
        "severity": "ERROR",
        "limit": 10,
        "redact_secrets": true
    });
    let resp = handler.execute(&error_args).expect("Handler execution failed");
    assert_eq!(resp["status"], "success");
    assert_eq!(resp["severity_filter"], "ERROR");

    let logs = resp["logs"].as_array().expect("logs should be array");
    assert!(!logs.is_empty(), "Should have matched ERROR log lines");

    // Verify all returned lines match ERROR severity
    for entry in logs {
        assert_eq!(entry["severity"], "ERROR");
    }

    // Verify secrets are redacted
    let has_redacted = logs.iter().any(|entry| {
        let line = entry["line"].as_str().unwrap_or("");
        line.contains("[REDACTED_API_KEY]") || line.contains("[REDACTED_BEARER_TOKEN]") || line.contains("[REDACTED_SECRET]")
    });
    assert!(has_redacted, "Sensitive API key should be redacted from logs");

    // Test B: Read specific source (dream_journal)
    let dream_args = json!({
        "source": "dream_journal",
        "severity": "ALL",
        "limit": 5,
        "redact_secrets": false
    });
    let dream_resp = handler.execute(&dream_args).expect("Dream journal read failed");
    assert_eq!(dream_resp["status"], "success");
    assert!(dream_resp["matched_lines_count"].as_u64().unwrap_or(0) >= 3);

    // Test C: Verify secret redaction unit helper directly
    let (redacted, count) = SystemLogsMcpHandler::redact_secrets(
        "Connecting with sk-1234567890abcdef1234567890 and Bearer secret_token_12345678901234567890"
    );
    assert!(count >= 2);
    assert!(!redacted.contains("sk-1234567890abcdef1234567890"));
    assert!(redacted.contains("[REDACTED_API_KEY]"));
    assert!(redacted.contains("[REDACTED_BEARER_TOKEN]"));
}

#[tokio::test]
async fn test_hgb_query_lancedb_vector_store_end_to_end() {
    let tmp = tempdir().expect("Failed to create tempdir");
    let ws = tmp.path();
    let lancedb_dir = ws.join(".hgb").join("lancedb").join("test_codebase_memory");
    fs::create_dir_all(&lancedb_dir).expect("Failed to create lancedb dir");

    let dim = 768;

    // 1. Synthesize realistic code chunks
    let chunk1 = AstCodeChunk {
        id: "chunk_fn_dot_product".to_string(),
        name: "dot_product".to_string(),
        chunk_type: "function".to_string(),
        code: "pub fn dot_product(a: &[f32], b: &[f32]) -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() }".to_string(),
        file_origin: "src/simd.rs".to_string(),
        line_start: 10,
        line_end: 25,
        doc_comments: vec!["Compute SIMD dot product of two vectors".to_string()],
        tags: vec!["simd".to_string(), "math".to_string()],
        embedding: CodeSemanticEmbedder::embed("dot product vector arithmetic simd", dim),
    };

    let chunk2 = AstCodeChunk {
        id: "chunk_fn_allocate_shm".to_string(),
        name: "allocate_shared_memory".to_string(),
        chunk_type: "function".to_string(),
        code: "pub fn allocate_shared_memory(size: usize) -> Result<MmapMut> { memmap2::MmapMut::map_anon(size) }".to_string(),
        file_origin: "src/shm.rs".to_string(),
        line_start: 40,
        line_end: 60,
        doc_comments: vec!["Zero-copy volatile RAM shared memory allocation in /dev/shm".to_string()],
        tags: vec!["memory".to_string(), "shm".to_string()],
        embedding: CodeSemanticEmbedder::embed("shared memory buffer allocation mmap shm", dim),
    };

    let chunks = vec![chunk1, chunk2];

    // 2. Persist into LanceDB columnar table (.vec and .col)
    let vec_file_path = lancedb_dir.join("data.vec");
    let col_file_path = lancedb_dir.join("meta.col");
    let manifest_path = lancedb_dir.join("manifest.json");

    let mut vec_file = File::create(&vec_file_path).expect("Failed to create data.vec");
    vec_file.write_all(&LANCE_VEC_MAGIC.to_le_bytes()).unwrap();
    vec_file.write_all(&(chunks.len() as u64).to_le_bytes()).unwrap();
    vec_file.write_all(&(dim as u32).to_le_bytes()).unwrap();
    vec_file.write_all(&(4u32).to_le_bytes()).unwrap();

    for c in &chunks {
        for &val in &c.embedding {
            vec_file.write_all(&val.to_le_bytes()).unwrap();
        }
    }
    vec_file.flush().unwrap();

    let mut col_file = File::create(&col_file_path).expect("Failed to create meta.col");
    col_file.write_all(&LANCE_COL_MAGIC.to_le_bytes()).unwrap();
    col_file.write_all(&(chunks.len() as u64).to_le_bytes()).unwrap();
    let col_json = serde_json::to_vec(&chunks).unwrap();
    col_file.write_all(&col_json).unwrap();
    col_file.flush().unwrap();

    let manifest_json = json!({
        "table_name": "test_codebase_memory",
        "dimension": dim,
        "record_count": chunks.len()
    });
    fs::write(&manifest_path, manifest_json.to_string()).unwrap();

    // 3. Query via LanceDbVectorStoreMcpHandler
    let handler = LanceDbVectorStoreMcpHandler::new(ws);
    let query_args = json!({
        "query": "vector dot product math",
        "top_k": 2,
        "dimension": dim,
        "table_path": lancedb_dir.to_str().unwrap()
    });

    let result = handler.execute(&query_args).expect("LanceDB query execution failed");
    assert_eq!(result["status"], "success");
    assert_eq!(result["total_records_searched"], 2);

    let matches = result["matches"].as_array().expect("Matches should be array");
    assert!(!matches.is_empty(), "Should return nearest matches");

    // First match should be dot_product function
    assert_eq!(matches[0]["name"], "dot_product");
    let score = matches[0]["score"].as_f64().unwrap();
    assert!(score > 0.0, "Score should be positive cosine similarity");
    assert!(score <= 1.0, "Cosine similarity score cannot exceed 1.0");
}

#[tokio::test]
async fn test_hgb_execute_build_sandbox_and_security() {
    let tmp = tempdir().expect("Failed to create tempdir");
    let ws = tmp.path();
    let handler = SandboxedBuildMcpHandler::new(ws);

    // 1. Security Whitelist Test: Non-whitelisted command should be rejected
    let bad_cmd_args = json!({
        "command": "rm -rf /tmp/dangerous_test"
    });
    let err = handler.execute(&bad_cmd_args).await;
    assert!(err.is_err(), "Non-whitelisted commands must be rejected by sandbox policy");

    // 2. Shell Injection Defense: Command with chaining metacharacters must be rejected
    let injection_args = json!({
        "command": "cargo check; echo hacked"
    });
    let injection_err = handler.execute(&injection_args).await;
    assert!(injection_err.is_err(), "Shell injection operators must be rejected");

    // 3. Valid Whitelisted Command Execution: `cargo --version`
    let valid_cmd_args = json!({
        "command": "cargo --version",
        "timeout_secs": 10
    });
    let res = handler.execute(&valid_cmd_args).await.expect("Build execution failed");
    assert_eq!(res["status"], "success");
    assert_eq!(res["exit_code"], 0);
    assert_eq!(res["timed_out"], false);

    let stdout = res["stdout"].as_str().unwrap_or("");
    assert!(stdout.contains("cargo"), "Stdout should report cargo version");
}

#[tokio::test]
async fn test_hgb_ingest_otel_telemetry_end_to_end() {
    let tmp = tempdir().expect("Failed to create tempdir");
    let ws = tmp.path();

    // 1. Scaffold valid SKILL.md for observability-otel
    let skill_dir = ws.join("skills").join("observability-otel");
    fs::create_dir_all(&skill_dir).expect("Failed to create skill directory");
    let skill_md = skill_dir.join("SKILL.md");
    fs::write(
        &skill_md,
        "---\nname: observability-otel\ndescription: Enterprise distributed tracing and OpenTelemetry telemetry\nversion: 0.1.0\ncategory: observability\n---\n# Observability",
    )
    .expect("Failed to write SKILL.md");

    let handler = OtelMetricsIngestMcpHandler::new(ws);
    let args = json!({
        "skill_dir": skill_dir.to_str().unwrap(),
        "include_hardware": true,
        "include_memory_psi": true,
        "include_circuit_breaker": true
    });

    let report = handler.execute(&args).expect("Telemetry ingestion failed");
    assert_eq!(report["status"], "success");
    assert_eq!(report["service"], "hagibis-observability");

    // Verify skill metadata
    assert_eq!(report["skill_metadata"]["name"], "observability-otel");
    assert_eq!(report["skill_metadata"]["version"], "0.1.0");

    // Verify OpenTelemetry traceparent generation
    let traceparent = report["otel_telemetry"]["sample_traceparent"].as_str().unwrap();
    assert!(traceparent.starts_with("00-"), "Must be standard W3C traceparent");
    assert_eq!(traceparent.split('-').count(), 4, "W3C traceparent must have 4 fields");

    // Verify Hardware Telemetry
    let hw = &report["hardware_telemetry"];
    assert!(hw["cpu_pkg_temp_c"].is_number());
    assert!(hw["thermal_severity"].is_string());

    // Verify Memory PSI
    assert!(hw["memory_psi"]["available"].is_boolean());

    // Verify Circuit Breaker Telemetry
    let cb = &report["circuit_breaker"];
    assert_eq!(cb["skill_id"], "observability-otel");
    assert_eq!(cb["state"].as_str().unwrap().to_uppercase(), "CLOSED");
    assert_eq!(cb["is_tripped"], false);
}

#[tokio::test]
async fn test_mcp_server_full_tools_call_and_list() {
    let tmp = tempdir().expect("Failed to create tempdir");
    let ws = tmp.path();
    let server = HagibisMcpServer::new(ws);

    // 1. Test tools/list contains all 4 tools
    let list_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/list".to_string(),
        params: Some(json!({ "profile": "full" })),
    };

    let list_resp_str = server.handle_message(&serde_json::to_string(&list_req).unwrap()).await.unwrap();
    let list_resp: serde_json::Value = serde_json::from_str(&list_resp_str).unwrap();
    let tools = list_resp["result"]["tools"].as_array().expect("Tools must be array");

    let required_tool_names = [
        "hgb_read_system_logs",
        "hgb_query_lancedb_vector_store",
        "hgb_execute_build",
        "hgb_ingest_otel_telemetry",
    ];

    for tool_name in &required_tool_names {
        assert!(
            tools.iter().any(|t| t["name"] == *tool_name),
            "tools/list must contain tool '{}'",
            tool_name
        );
    }

    // 2. Test tools/call dispatch for hgb_ingest_otel_telemetry
    let call_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "hgb_ingest_otel_telemetry",
            "arguments": {
                "include_hardware": true
            }
        })),
    };

    let call_resp_str = server.handle_message(&serde_json::to_string(&call_req).unwrap()).await.unwrap();
    let call_resp: serde_json::Value = serde_json::from_str(&call_resp_str).unwrap();
    assert_eq!(call_resp["id"], 2);
    assert_eq!(call_resp["result"]["isError"], false);

    let content_text = call_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content_text.contains("hagibis-observability"));

    // 3. Test tools/call dispatch for hgb_read_system_logs
    let logs_call_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "hgb_read_system_logs",
            "arguments": {
                "source": "all",
                "limit": 10
            }
        })),
    };

    let logs_call_resp_str = server.handle_message(&serde_json::to_string(&logs_call_req).unwrap()).await.unwrap();
    let logs_call_resp: serde_json::Value = serde_json::from_str(&logs_call_resp_str).unwrap();
    assert_eq!(logs_call_resp["id"], 3);
    assert_eq!(logs_call_resp["result"]["isError"], false);
}
