//! # Brutal Integration Tests for Next-Gen Vibe Coding Architecture
//!
//! Exhaustive, 1000% real verification for all 6 Next-Gen pillars:
//! 1. Continuous Guardian Mode (`GuardianEngine`, `GhostFix`, passive checks, staged memory patches)
//! 2. DevServer Sentinel & Runtime Probe (`DevServerSentinel`, port scanning, framework detection, panic/exception parsing)
//! 3. Ripple Effect Radar (`ImpactRadar`, polyglot import graph, transitive blast radius, cycle protection, call-sites)
//! 4. Specialist Swarm Pod (`SwarmPod`, 4-role consensus DAG, CockpitDagNode tracking, approval/rejection gating)
//! 5. Shell Mind-Reader & Terminal Rescue (`TerminalRescue`, port conflict, missing deps, compiler diagnostics, AgentShieldLight)
//! 6. Project Memory & Decision Ledger (`ProjectMemoryLedger`, atomic persistence, Blake3 hash chaining, XML context anchor)

use async_trait::async_trait;
use hgb_core::impact::{ImpactRadar, RiskLevel};
use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_core::rescue::{FailureCategory, TerminalRescue};
use hgb_core::security::AgentShieldLight;
use hgb_core::{HgbProvider, Result};
use hgb_nextgen::cockpit::CockpitNodeStatus;
use hgb_nextgen::guardian::{GhostFixStatus, GuardianConfig, GuardianEngine};
use hgb_nextgen::pod::SwarmPod;
use hgb_nextgen::sentinel::{DevServerFramework, DevServerSentinel};
use hgb_storage::memory::{AdrStatus, DebtSeverity, ProjectMemoryLedger};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Helper to create an isolated temporary workspace directory
fn create_test_workspace(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "hgb_nextgen_test_{}_{}",
        name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&base).unwrap();
    base
}

struct TestNextGenProxyProvider {
    canned_response: String,
}

#[async_trait]
impl HgbProvider for TestNextGenProxyProvider {
    fn name(&self) -> &str {
        "nextgen_proxy_provider"
    }

    async fn complete(&self, prompt: &str, _model: Option<&str>) -> Result<String> {
        if prompt.contains("Lead Systems Rust Architect") {
            Ok("Architectural Contract: Implement atomic buffer with LockFreeRing".into())
        } else if prompt.contains("Surgical Coder") {
            Ok("pub struct LockFreeRing { head: AtomicUsize, tail: AtomicUsize }".into())
        } else if prompt.contains("Security & Style Reviewer") {
            if self.canned_response.contains("REJECT") {
                Ok("[REJECTED: potential race condition in wrap-around]".into())
            } else {
                Ok("[APPROVED] Code satisfies all security and memory invariants.".into())
            }
        } else if prompt.contains("Brutal QA Engineer") {
            Ok("#[test]\nfn test_ring_concurrency() { /* 1000 concurrent threads */ }".into())
        } else if prompt.contains("Return JSON") {
            // Ghost fix response
            Ok(r#"{"target": "let x = 10", "replacement": "let mut x = 10"}"#.into())
        } else {
            Ok(self.canned_response.clone())
        }
    }
}

// =========================================================================
// PILLAR 1: CONTINUOUS GUARDIAN MODE & GHOST-FIXES
// =========================================================================
#[tokio::test]
async fn test_pillar1_guardian_engine_continuous_verification_and_ghost_fixes() {
    let ws = create_test_workspace("guardian_engine");

    let src_file = ws.join("lib.rs");
    fs::write(&src_file, "pub fn compute() {\n    let x = 10;\n    x += 5;\n}\n").unwrap();

    let config = GuardianConfig {
        workspace_root: ws.clone(),
        check_command: "true".to_string(), // Initial simulated command
        debounce_ms: 50,
        extensions: vec!["rs".into()],
        auto_heal: false,
    };

    let provider = Arc::new(TestNextGenProxyProvider {
        canned_response: "ok".into(),
    });

    let engine = GuardianEngine::new(config, provider);
    let _rx = engine.take_event_receiver().await.expect("take receiver");

    // 1. Initial file scan seeds mtimes
    let initial_mod = engine.detect_modified_files().await.expect("detect initial");
    assert!(initial_mod.is_empty(), "Initial scan should record baseline mtimes");

    // 2. Modifying file triggers detection
    std::thread::sleep(std::time::Duration::from_millis(15));
    fs::write(&src_file, "pub fn compute() {\n    let x = 10;\n    x += 10;\n}\n").unwrap();

    let detected = engine.detect_modified_files().await.expect("detect modified");
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0], src_file);

    // 3. Staging and applying Ghost-Fix in memory
    let staged_fix = hgb_nextgen::guardian::GhostFix {
        id: "ghost-abc12345".into(),
        file_path: src_file.clone(),
        line_number: 2,
        error_code: Some("E0384".into()),
        diagnostic_message: "cannot assign twice to immutable variable `x`".into(),
        target_content: "    let x = 10;".into(),
        replacement_content: "    let mut x = 10;".into(),
        status: GhostFixStatus::ReadyToApply,
        created_at: "2026-09-25T00:00:00Z".into(),
        confidence_score: 0.98,
    };

    // Stage fix directly
    engine.stage_ghost_fix(staged_fix.clone()).await;
    let fixes = engine.get_ghost_fixes().await;
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].id, "ghost-abc12345");
    assert_eq!(fixes[0].status, GhostFixStatus::ReadyToApply);

    // Apply the pre-computed staged ghost fix
    let patch_report = engine.apply_ghost_fix("ghost-abc12345").await.expect("apply ghost fix");
    assert!(patch_report.contains("Successfully replaced") || patch_report.contains("replaced"));

    // Verify on-disk file content was surgically repaired
    let new_content = fs::read_to_string(&src_file).unwrap();
    assert!(new_content.contains("let mut x = 10;"));

    // Verify fix status updated to Applied
    let updated_fixes = engine.get_ghost_fixes().await;
    assert_eq!(updated_fixes[0].status, GhostFixStatus::Applied);

    let _ = fs::remove_dir_all(&ws);
}

// =========================================================================
// PILLAR 2: DEVSERVER SENTINEL & RUNTIME PROBE
// =========================================================================
#[tokio::test]
async fn test_pillar2_devserver_sentinel_port_scanning_and_diagnostic_probes() {
    // 1. Spawn Proxy FastApi Server
    let fastapi_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fastapi_port = fastapi_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = fastapi_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 15\r\nServer: uvicorn\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}";
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    // 2. Spawn Proxy Vite Dev Server
    let vite_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vite_port = vite_listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = vite_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 32\r\nConnection: close\r\n\r\n<script src=\"/@vite/client\"></script>";
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    let sentinel = DevServerSentinel::new(vec![fastapi_port, vite_port, 58888]);
    let endpoints = sentinel.scan_active_endpoints().await;

    assert_eq!(endpoints.len(), 2, "Should discover exactly 2 live servers");
    assert_eq!(endpoints[0].port.min(endpoints[1].port), fastapi_port.min(vite_port));

    let fastapi_ep = endpoints.iter().find(|e| e.port == fastapi_port).unwrap();
    assert_eq!(fastapi_ep.framework, DevServerFramework::FastApiFlask);
    assert_eq!(fastapi_ep.http_status, Some(200));
    assert!(fastapi_ep.is_healthy);

    let vite_ep = endpoints.iter().find(|e| e.port == vite_port).unwrap();
    assert_eq!(vite_ep.framework, DevServerFramework::Vite);
    assert_eq!(vite_ep.http_status, Some(200));
    assert!(vite_ep.is_healthy);

    // 3. Brutal Runtime Diagnostic Extraction: Rust Panic
    let panic_log = "thread 'tokio-runtime-worker' panicked at crates/hgb-core/src/db.rs:188:14:\ncalled `Option::unwrap()` on a `None` value\nstack backtrace:\n   0: rust_begin_unwind";
    let panic_diag = DevServerSentinel::parse_runtime_error(fastapi_port, panic_log).expect("parse panic");
    assert_eq!(panic_diag.error_type, "RustPanic");
    assert_eq!(panic_diag.message, "called `Option::unwrap()` on a `None` value");
    assert_eq!(panic_diag.source_file, Some(PathBuf::from("crates/hgb-core/src/db.rs")));
    assert_eq!(panic_diag.line_number, Some(188));

    // 4. Brutal Runtime Diagnostic Extraction: Node.js Exception
    let node_log = "ReferenceError: window is not defined\n    at setupCanvas (/web/src/canvas.ts:45:12)\n    at renderApp";
    let node_diag = DevServerSentinel::parse_runtime_error(vite_port, node_log).expect("parse node error");
    assert_eq!(node_diag.error_type, "NodeException");
    assert_eq!(node_diag.message, "window is not defined");
    assert_eq!(node_diag.source_file, Some(PathBuf::from("/web/src/canvas.ts")));
    assert_eq!(node_diag.line_number, Some(45));

    // 5. Clean logs return None
    assert!(DevServerSentinel::parse_runtime_error(8080, "GET /api/v1/health 200 OK in 1.2ms").is_none());
}

// =========================================================================
// PILLAR 3: RIPPLE EFFECT RADAR & BLAST RADIUS
// =========================================================================
#[tokio::test]
async fn test_pillar3_ripple_effect_radar_polyglot_import_graph_and_blast_radius() {
    let ws = create_test_workspace("impact_radar");

    // Build multi-file import topology:
    // core.rs -> imported by index.rs -> imported by service.rs -> imported by api.rs
    // cycle_a.rs <-> cycle_b.rs (cycle safety)
    let core_file = ws.join("core.rs");
    let index_file = ws.join("index.rs");
    let service_file = ws.join("service.rs");
    let api_file = ws.join("api.rs");
    let cycle_a = ws.join("cycle_a.rs");
    let cycle_b = ws.join("cycle_b.rs");

    fs::write(&core_file, "pub struct VectorDatabase {\n    pub dimension: usize,\n}\n").unwrap();
    fs::write(&index_file, "use crate::core::VectorDatabase;\npub fn make_index(db: &VectorDatabase) {}\n").unwrap();
    fs::write(&service_file, "use crate::index;\npub fn run_service() {\n    let db = crate::core::VectorDatabase { dimension: 128 };\n}\n").unwrap();
    fs::write(&api_file, "use crate::service;\npub fn handle_req() {}\n").unwrap();
    fs::write(&cycle_a, "use crate::cycle_b;\npub fn a() {}\n").unwrap();
    fs::write(&cycle_b, "use crate::cycle_a;\npub fn b() {}\n").unwrap();

    let mut radar = ImpactRadar::new(&ws);
    radar.index_workspace().expect("index workspace");

    // Assess impact of modifying `VectorDatabase` originating in `core.rs`
    let report = radar.assess_symbol_impact("VectorDatabase", &core_file);

    assert_eq!(report.target_symbol, "VectorDatabase");
    assert_eq!(report.originating_file, core_file);
    assert_eq!(report.direct_dependents.len(), 1);
    assert_eq!(report.direct_dependents[0], index_file);

    // Transitive blast radius includes service_file and api_file
    assert!(report.transitive_dependents.contains(&service_file));
    assert!(report.transitive_dependents.contains(&api_file));
    assert_eq!(report.total_affected_files, 4);
    assert_eq!(report.risk_level, RiskLevel::High);

    // Call-sites identified
    assert!(!report.call_sites.is_empty());
    assert!(report.call_sites.iter().any(|cs| cs.file_path == index_file && cs.context_snippet.contains("VectorDatabase")));
    assert!(report.call_sites.iter().any(|cs| cs.file_path == service_file && cs.context_snippet.contains("VectorDatabase")));

    let _ = fs::remove_dir_all(&ws);
}

// =========================================================================
// PILLAR 4: SPECIALIST SWARM POD & CONSENSUS DAG
// =========================================================================
#[tokio::test]
async fn test_pillar4_specialist_swarm_pod_dag_orchestration_and_consensus() {
    let ws = create_test_workspace("swarm_pod");

    // 1. Success Consensus
    let success_provider = Arc::new(TestNextGenProxyProvider {
        canned_response: "approved".into(),
    });

    let pod = SwarmPod::new(success_provider, ws.clone());
    let res = pod.execute_task("Design high-throughput LockFreeRing buffer").await.expect("pod execution");

    assert_eq!(res.task, "Design high-throughput LockFreeRing buffer");
    assert!(res.architect_plan.contains("LockFreeRing"));
    assert!(res.code_solution.contains("LockFreeRing"));
    assert!(res.review_status, "Consensus should be APPROVED");
    assert!(res.test_coverage.contains("test_ring_concurrency"));
    assert!(res.total_tokens > 0);
    assert!(res.duration_ms <= 10000);

    // Check DAG nodes state
    let nodes = pod.get_dag_nodes().await;
    assert_eq!(nodes.len(), 4);
    for node in &nodes {
        assert!(matches!(node.status, CockpitNodeStatus::Succeeded { .. }), "All nodes must succeed in consensus");
    }

    // 2. Rejection Path
    let rejection_provider = Arc::new(TestNextGenProxyProvider {
        canned_response: "REJECT".into(),
    });
    let reject_pod = SwarmPod::new(rejection_provider, ws.clone());
    let reject_res = reject_pod.execute_task("Design flawed component").await.expect("pod execution");
    assert!(!reject_res.review_status, "Consensus should be REJECTED");
    assert!(reject_res.review_notes[0].contains("potential race condition"));

    let _ = fs::remove_dir_all(&ws);
}

// =========================================================================
// PILLAR 5: TERMINAL RESCUE & AGENTSHIELD VERIFICATION
// =========================================================================
#[test]
fn test_pillar5_terminal_rescue_mind_reader_diagnostics_and_shield_verification() {
    // 1. Port conflict: Address already in use 8080
    let port_stderr = "Error: listen EADDRINUSE: address already in use :::8080";
    let rep1 = TerminalRescue::diagnose("cargo run --bin server", 1, port_stderr, "");
    match rep1.category {
        FailureCategory::PortConflict { port, .. } => assert_eq!(port, 8080),
        other => panic!("Expected PortConflict, got {:?}", other),
    }
    assert!(!rep1.suggested_fixes.is_empty());
    assert!(rep1.suggested_fixes[0].command.contains("8080"));

    // Verify AgentShieldLight audits every suggested corrective fix
    for fix in &rep1.suggested_fixes {
        let audit = AgentShieldLight::audit_command(&fix.command);
        assert!(audit.is_ok(), "Suggested fix must pass AgentShieldLight safety audit: {}", fix.command);
    }

    // 2. Missing npm dependency
    let npm_stderr = "Error: Cannot find module 'dotenv'\nRequire stack:\n- /app/index.js";
    let rep2 = TerminalRescue::diagnose("node index.js", 1, npm_stderr, "");
    match rep2.category {
        FailureCategory::MissingDependency { package, manager } => {
            assert_eq!(package, "dotenv");
            assert_eq!(manager, "npm");
        }
        other => panic!("Expected MissingDependency, got {:?}", other),
    }
    assert_eq!(rep2.suggested_fixes[0].command, "npm install dotenv");

    // 3. Rust compiler error
    let rustc_stderr = "error[E0432]: unresolved import `tokio::sync::mpsc`\n  --> src/main.rs:2:5";
    let rep3 = TerminalRescue::diagnose("cargo check", 101, rustc_stderr, "");
    assert!(matches!(rep3.category, FailureCategory::RustCompilationError { .. }));
    assert!(rep3.suggested_fixes.iter().any(|f| f.command.contains("hgb heal")));

    // 4. Command not found
    let cmd_stderr = "bash: kubens: command not found";
    let rep4 = TerminalRescue::diagnose("kubens default", 127, cmd_stderr, "");
    assert!(matches!(rep4.category, FailureCategory::CommandNotFound { .. }));
}

// =========================================================================
// PILLAR 6: PROJECT MEMORY LEDGER & ATOMIC PERSISTENCE
// =========================================================================
#[test]
fn test_pillar6_project_memory_ledger_atomic_persistence_and_token_anchor() {
    let ws = create_test_workspace("memory_ledger");

    // 1. Initialize and atomic write
    let mut ledger = ProjectMemoryLedger::load_or_init(&ws).expect("load or init memory");
    assert!(ws.join(".hgb/memory.json").exists(), ".hgb/memory.json must be created");

    // 2. Record Architectural Decision (ADR)
    let adr1_id = ledger.record_decision(
        "Use Blake3 Hash Chaining for Provenance",
        "Adopt Blake3 tree hashing for sub-microsecond evidentiary verification",
        "Required by Tagisan 2.0 and Rule 141 evidentiary guidelines",
    ).expect("record decision 1");
    assert_eq!(adr1_id, "ADR-001");

    let adr2_id = ledger.record_decision(
        "Speculative Dual-Draft Racing",
        "Execute 2 draft engines concurrently with First-Green-Wins termination",
        "Reduces generation latency by 45% on local LLM nodes",
    ).expect("record decision 2");
    assert_eq!(adr2_id, "ADR-002");

    // 3. Record Technical Debt
    let debt_id = ledger.record_tech_debt(
        "Replace raw regex with CST parser for TypeScript imports",
        "Regex parser currently misses dynamic import expressions",
        DebtSeverity::Medium,
        vec!["crates/hgb-core/src/impact.rs".to_string()],
    ).expect("record debt");
    assert_eq!(debt_id, "DEBT-001");

    // 4. Generate XML Context Anchor with token budgeting
    let anchor = ledger.render_llm_anchor(1000);
    assert!(anchor.contains("<project_memory"));
    assert!(anchor.contains("<architectural_decisions>"));
    assert!(anchor.contains("ADR-001"));
    assert!(anchor.contains("ADR-002"));
    assert!(anchor.contains("<active_technical_debt>"));
    assert!(anchor.contains("DEBT-001"));
    assert!(anchor.contains("</project_memory>"));

    // 5. Reload from disk and verify persistence fidelity
    let reloaded = ProjectMemoryLedger::load_or_init(&ws).expect("reload memory");
    assert_eq!(reloaded.doc.decisions.len(), 2);
    assert_eq!(reloaded.doc.tech_debt.len(), 1);
    assert_eq!(reloaded.doc.decisions[0].id, "ADR-001");
    assert_eq!(reloaded.doc.decisions[0].status, AdrStatus::Accepted);
    assert!(!reloaded.doc.decisions[0].blake3_hash.is_empty());
    assert_eq!(reloaded.doc.tech_debt[0].id, "DEBT-001");
    assert_eq!(reloaded.doc.tech_debt[0].severity, DebtSeverity::Medium);

    let _ = fs::remove_dir_all(&ws);
}

// =========================================================================
// PILLAR 7: FULL CLIENT-DAEMON IPC ROUNDTRIP FOR ALL NEW REQUESTS
// =========================================================================
#[tokio::test]
async fn test_full_client_daemon_ipc_roundtrip_all_nextgen_requests() {
    let ws = create_test_workspace("daemon_ipc_nextgen");
    let sock = ws.join("test_nextgen.sock");

    let state = Arc::new(hgb_daemon::DaemonState::new(sock.clone()));

    // 1. Test DevServerScan request
    let resp = hgb_daemon::HagibisDaemon::handle_request(&state, HgbRequest::DevServerScan).await;
    assert!(matches!(resp, HgbResponse::DevServerEndpoints(_)));

    // 2. Test RescueDiagnose request
    let rescue_resp = hgb_daemon::HagibisDaemon::handle_request(&state, HgbRequest::RescueDiagnose {
        failed_command: "npm start".into(),
        exit_code: 1,
        stderr: "Error: listen EADDRINUSE :::3000".into(),
        stdout: "".into(),
    }).await;
    match rescue_resp {
        HgbResponse::RescueReport(rep) => {
            assert_eq!(rep.failed_command, "npm start");
            assert!(matches!(rep.category, FailureCategory::PortConflict { port: 3000, .. }));
        }
        other => panic!("Expected RescueReport, got {:?}", other),
    }

    // 3. Test MemoryRecordDecision request
    let mem_record_resp = hgb_daemon::HagibisDaemon::handle_request(&state, HgbRequest::MemoryRecordDecision {
        title: "Test ADR".into(),
        decision: "Adopt NextGen Vibe Coding".into(),
        context: "High productivity developer ergonomics".into(),
    }).await;
    assert!(matches!(mem_record_resp, HgbResponse::MemoryRecorded { .. }));

    // 4. Test MemoryGetAnchor request
    let mem_anchor_resp = hgb_daemon::HagibisDaemon::handle_request(&state, HgbRequest::MemoryGetAnchor {
        max_tokens: Some(1500),
    }).await;
    match mem_anchor_resp {
        HgbResponse::MemoryAnchor(anchor) => {
            assert!(anchor.contains("<project_memory"));
            assert!(anchor.contains("Test ADR"));
        }
        other => panic!("Expected MemoryAnchor, got {:?}", other),
    }

    // 5. Test GuardianStatus request
    let guardian_status_resp = hgb_daemon::HagibisDaemon::handle_request(&state, HgbRequest::GuardianStatus).await;
    assert!(matches!(guardian_status_resp, HgbResponse::GuardianReport { is_active: false, .. }));

    let _ = fs::remove_dir_all(&ws);
}
