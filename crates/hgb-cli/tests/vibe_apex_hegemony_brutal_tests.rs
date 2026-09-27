//! # Tier 7 Competitor Apex Hegemony Brutal Integration Tests
//!
//! Brutal end-to-end integration and stress tests for:
//! 1. RepoMapRanker (Tree-sitter PageRank Symbol Graph & Token Density)
//! 2. ShadowWorkspace (Cursor-Style Pre-Flight Validator & Auto-Repair)
//! 3. StreamSqueezer (Claude Code-Style Terminal Stream Squeezer)
//! 4. MutationFuzzer (Qodo-Style Anti-Placebo Test Integrity Audit)
//! 5. DomPreviewBridge (Bolt.new-Style Visual Click-to-Code DOM Telemetry)
//! 6. McpHostOrchestrator (Goose-Style Universal MCP Fleet Hub)

use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_core::{
    BoundingBox, DomPreviewBridge, McpHostOrchestrator, McpServerConfig, McpServerStatus,
    MutantStatus, MutationFuzzer, MutationOperator, RepoMapRanker,
    ShadowWorkspace, StreamSqueezer,
};
use hgb_daemon::server::{DaemonState, HagibisDaemon};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn create_temp_test_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hgb_apex_test_{}_{}", name, nanos));
    fs::create_dir_all(&dir).expect("create test dir");
    dir
}

#[test]
fn test_repo_map_ranker_pagerank_and_token_budget() {
    let tmp = create_temp_test_dir("repomap");
    let src_dir = tmp.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let main_rs = r#"
        pub struct AppState {
            pub db: DatabaseClient,
        }

        pub async fn run_server() {
            let client = DatabaseClient::connect();
            client.query();
        }
    "#;
    fs::write(src_dir.join("main.rs"), main_rs).expect("write main.rs");

    let db_rs = r#"
        pub struct DatabaseClient {
            pub pool_size: usize,
        }

        impl DatabaseClient {
            pub fn connect() -> Self {
                Self { pool_size: 10 }
            }

            pub fn query(&self) -> bool {
                true
            }
        }
    "#;
    fs::write(src_dir.join("db.rs"), db_rs).expect("write db.rs");

    let ts_file = r#"
        export interface UserProfile {
            id: string;
            role: string;
        }

        export async function fetchUser(id: string): Promise<UserProfile> {
            return { id, role: "admin" };
        }
    "#;
    fs::write(src_dir.join("api.ts"), ts_file).expect("write api.ts");

    let ranker = RepoMapRanker::new(&tmp);
    let graph = ranker
        .analyze_repo(&["rs", "ts"])
        .expect("analyze repo failed");

    assert!(graph.symbols.len() >= 4, "Expected at least 4 symbols discovered");

    // Verify symbols contain AppState and DatabaseClient
    let has_app_state = graph.symbols.values().any(|s| s.name == "AppState");
    let has_db_client = graph.symbols.values().any(|s| s.name == "DatabaseClient");
    let has_fetch_user = graph.symbols.values().any(|s| s.name == "fetchUser");

    assert!(has_app_state, "Expected AppState in symbols");
    assert!(has_db_client, "Expected DatabaseClient in symbols");
    assert!(has_fetch_user, "Expected fetchUser in symbols");

    // Render with budget of 150 tokens (~600 chars)
    let rendered = RepoMapRanker::render_ranked_map(&graph, 150);
    assert!(rendered.contains("HGB ARCHITECTURAL REPO-MAP"));
    assert!(rendered.len() <= 150 * 5, "Rendered output should respect token budget limit");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_shadow_workspace_preflight_and_speculative_repair() {
    let tmp = create_temp_test_dir("shadow");
    let shadow = ShadowWorkspace::new(&tmp, Some(".test_shadow"));

    // 1. Stage and validate a clean Python file
    let clean_py = "def calculate_total(items):\n    return sum(items)\n";
    let res = shadow
        .validate_file(std::path::Path::new("calc.py"), clean_py)
        .expect("validate clean file");
    assert!(res.is_valid, "Clean python code should pass");

    // 2. Stage unclosed curly braces in Rust code and trigger auto-repair
    let broken_rs = "pub fn execute() {\n    let val = 42;\n";
    let res_broken = shadow
        .validate_file(std::path::Path::new("broken.rs"), broken_rs)
        .expect("validate broken file");
    assert!(!res_broken.is_valid, "Broken rust code should fail");
    assert!(res_broken.repaired_content.is_some(), "Speculative auto-repair should be generated");

    let repaired = res_broken.repaired_content.unwrap();
    assert!(repaired.contains('}'), "Repaired code should append missing closing brace");

    // 3. Clean up shadow workspace
    shadow.cleanup().expect("cleanup shadow");
    assert!(!shadow.shadow_root.exists(), "Shadow root should be removed");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_stream_squeezer_compression_and_noise_reduction() {
    let mut noisy_log = String::new();

    // 500 lines of download progress noise
    for i in 0..500 {
        noisy_log.push_str(&format!("  [=====>    ] {}% Downloading crates.io index\r\n", i % 100));
    }

    // ANSI escape color codes
    noisy_log.push_str("\x1b[31merror[E0425]: cannot find value `token` in this scope\x1b[0m\n");
    noisy_log.push_str(" --> src/auth/jwt.rs:45:12\n");
    noisy_log.push_str("    |\n");
    noisy_log.push_str(" 45 |     verify(token)\n");
    noisy_log.push_str("    |            ^^^^^ not found in this scope\n");

    // Repeated warnings
    for _ in 0..50 {
        noisy_log.push_str("warning: unused variable `req` in function `handle_ping`\n");
    }

    // Squeeze output with max 1000 tokens
    let digest = StreamSqueezer::squeeze(&noisy_log, 1000);

    assert_eq!(digest.errors.len(), 1, "Expected exactly 1 critical error captured");
    assert!(digest.compression_ratio_pct > 80.0, "Expected >80% compression ratio, got {:.1}%", digest.compression_ratio_pct);
    assert!(!digest.deduplicated_warnings.is_empty(), "Expected warnings deduplicated");
    assert!(digest.deduplicated_warnings[0].contains("repeated 50x"), "Warning should record repetition count");
    assert!(digest.file_locations.iter().any(|loc| loc.contains("src/auth/jwt.rs:45")), "Should extract code location");
    assert!(!digest.compressed_view.contains("\x1b["), "ANSI escape codes should be completely stripped");
}

#[test]
fn test_mutation_fuzzer_anti_placebo_verification() {
    let code = r#"
        pub fn can_access_resource(role: &str, is_active: bool, score: i32) -> bool {
            if role == "admin" && is_active && score > 50 {
                true
            } else {
                false
            }
        }
    "#;

    let mutants = MutationFuzzer::scan_mutants(code);
    assert!(!mutants.is_empty(), "Should discover mutation candidates");

    let has_eq = mutants.iter().any(|m| m.operator == MutationOperator::InvertEquality);
    let has_cmp = mutants.iter().any(|m| m.operator == MutationOperator::InvertComparison);
    let has_bool = mutants.iter().any(|m| m.operator == MutationOperator::InvertBoolean);
    let has_log = mutants.iter().any(|m| m.operator == MutationOperator::InvertLogical);

    assert!(has_eq, "Should identify equality mutation");
    assert!(has_cmp, "Should identify comparison mutation");
    assert!(has_bool, "Should identify boolean mutation");
    assert!(has_log, "Should identify logical mutation");

    // Test mutation application
    let eq_mutant = mutants.iter().find(|m| m.operator == MutationOperator::InvertEquality).unwrap();
    let mutated_code = MutationFuzzer::apply_mutant(code, eq_mutant);
    assert!(mutated_code.contains(" != "), "Mutated code should contain inverted equality");

    // Evaluate suite with 3 killed and 1 survived
    let outcomes = vec![
        (mutants[0].clone(), MutantStatus::Killed),
        (mutants[1].clone(), MutantStatus::Killed),
        (mutants[2].clone(), MutantStatus::Killed),
        (mutants[3].clone(), MutantStatus::Survived),
    ];

    let report = MutationFuzzer::generate_report(&outcomes);
    assert_eq!(report.total_mutants, 4);
    assert_eq!(report.killed_mutants, 3);
    assert_eq!(report.survived_mutants, 1);
    assert_eq!(report.mutation_score_pct, 75.0);
    assert!(report.integrity_grade.contains("B"));
    assert!(!report.recommendations.is_empty());
}

#[test]
fn test_dom_preview_bridge_click_to_code_and_hierarchy() {
    let template = r#"
        <div id="container" class="layout-flex">
            <header class="app-header">
                <h1 class="title">Hagibis Cockpit</h1>
            </header>
            <main class="content-body">
                <button id="submit-btn" class="btn btn-primary">Deploy</button>
            </main>
        </div>
    "#;

    // 1. Inject source telemetry
    let injected = DomPreviewBridge::inject_source_telemetry(template, "src/Dashboard.tsx");
    assert!(injected.contains("data-hgb-source=\"src/Dashboard.tsx:2\""));
    assert!(injected.contains("data-hgb-source=\"src/Dashboard.tsx:3\""));

    // 2. Parse elements
    let mut elements = DomPreviewBridge::parse_elements(template, "src/Dashboard.tsx");
    assert!(elements.len() >= 4, "Expected at least 4 parsed elements");

    let button = elements.iter().find(|e| e.tag == "button").expect("button element");
    assert_eq!(button.id.as_deref(), Some("submit-btn"));
    assert!(button.classes.contains(&"btn-primary".to_string()));

    // 3. Attach bounding boxes and test coordinate click resolution
    elements[0].bounding_box = Some(BoundingBox { x: 0.0, y: 0.0, width: 800.0, height: 600.0 });
    let btn_idx = elements.iter().position(|e| e.tag == "button").unwrap();
    elements[btn_idx].bounding_box = Some(BoundingBox { x: 100.0, y: 200.0, width: 120.0, height: 40.0 });

    // Click inside button (110, 210) should resolve to button (smallest area), not container
    let clicked = DomPreviewBridge::resolve_coordinate(&elements, 110.0, 210.0);
    assert!(clicked.is_some(), "Coordinate should hit element");
    assert_eq!(clicked.unwrap().tag, "button", "Click should resolve to deepest element (button)");

    // 4. Query selector
    let by_class = DomPreviewBridge::query_selector(&elements, ".title");
    assert_eq!(by_class.len(), 1);
    assert_eq!(by_class[0].tag, "h1");

    let by_id = DomPreviewBridge::query_selector(&elements, "#submit-btn");
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].tag, "button");
}

#[tokio::test]
async fn test_mcp_host_orchestrator_multi_server_lifecycle() {
    let tmp = create_temp_test_dir("mcp_host");
    let orchestrator = McpHostOrchestrator::new(&tmp);

    // Register two virtual servers
    let pg_cfg = McpServerConfig::new("echo").with_args(["postgres-mcp"]);
    let gh_cfg = McpServerConfig::new("echo").with_args(["github-mcp"]);

    orchestrator.register_server("postgres", pg_cfg).await;
    orchestrator.register_server("github", gh_cfg).await;

    let health = orchestrator.health_summary().await;
    assert_eq!(health.len(), 2);
    assert_eq!(health.get("postgres"), Some(&McpServerStatus::Registered));
    assert_eq!(health.get("github"), Some(&McpServerStatus::Registered));

    // Test stop server
    orchestrator.stop_server("postgres").await.expect("stop server");
    let health_post = orchestrator.health_summary().await;
    assert_eq!(health_post.get("postgres"), Some(&McpServerStatus::Stopped));

    let _ = fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_daemon_ipc_end_to_end_apex_hegemony() {
    let tmp = create_temp_test_dir("ipc");
    let sock = tmp.join("hgb_apex_test.sock");
    let state = Arc::new(DaemonState::new(sock));

    // 1. RepoMapRank IPC
    let req = HgbRequest::RepoMapRank {
        extensions: vec!["rs".to_string()],
        token_budget: Some(512),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::RepoMapRankResult(map) => {
            assert!(map.contains("HGB ARCHITECTURAL REPO-MAP"));
        }
        _ => panic!("Expected RepoMapRankResult"),
    }

    // 2. ShadowPreflight IPC
    let req = HgbRequest::ShadowPreflight {
        relative_path: "src/sample.rs".to_string(),
        candidate_content: "pub fn valid_routine() -> i32 { 100 }\n".to_string(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::ShadowPreflightResult(res) => {
            assert!(res.is_valid);
            assert!(res.diff_stats.contains("lines"));
        }
        _ => panic!("Expected ShadowPreflightResult"),
    }

    // 3. StreamSqueeze IPC
    let req = HgbRequest::StreamSqueeze {
        raw_output: "Compiling hgb v0.1.0\nwarning: unused variable `a`\nerror: aborting due to 1 previous error\n".to_string(),
        max_tokens: Some(1024),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::StreamSqueezeResult(digest) => {
            assert_eq!(digest.total_raw_lines, 3);
            assert_eq!(digest.errors.len(), 1);
        }
        _ => panic!("Expected StreamSqueezeResult"),
    }

    // 4. MutationAudit IPC
    let req = HgbRequest::MutationAudit {
        source_code: "pub fn check(a: i32, b: i32) -> bool { a == b && a > 0 }".to_string(),
        file_name: "src/check.rs".to_string(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::MutationAuditResult(rep) => {
            assert!(rep.total_mutants >= 2);
            assert!(rep.mutation_score_pct > 0.0);
        }
        _ => panic!("Expected MutationAuditResult"),
    }

    // 5. DomInspect IPC
    let req = HgbRequest::DomInspect {
        template_content: "<div id=\"hero\" class=\"container\"><h1 class=\"headline\">Apex Hegemony</h1></div>".to_string(),
        file_name: "src/Hero.tsx".to_string(),
        click_coords: None,
        css_selector: Some(".headline".to_string()),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::DomInspectResult { elements, hierarchy_map, target_element } => {
            assert_eq!(elements.len(), 2);
            assert!(hierarchy_map.contains("<h1"));
            assert!(target_element.is_some());
            assert_eq!(target_element.unwrap().tag, "h1");
        }
        _ => panic!("Expected DomInspectResult"),
    }

    // 6. McpOrchestrate IPC
    let req = HgbRequest::McpOrchestrate {
        action: "list".to_string(),
        server_name: None,
        tool_name: None,
        arguments: None,
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::McpOrchestrateResult { active_servers: _, tools: _, tool_output: _ } => {
            // Succeeded without error
        }
        _ => panic!("Expected McpOrchestrateResult"),
    }

    let _ = fs::remove_dir_all(&tmp);
}
