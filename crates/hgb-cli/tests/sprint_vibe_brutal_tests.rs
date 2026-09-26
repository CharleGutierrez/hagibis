//! # Brutal Integration Tests for Hagibis (hgb) Next Sprint Vibe Coding Architecture
//!
//! Exhaustive, 1000% real verification for all 6 Next Sprint pillars:
//! 1. Multimodal Visual Canvas & Layout Autopsy (`GlanceEngine`, binary magic bytes, dimensions, CSS suggestions)
//! 2. Dependency Hallucination Firewall (`PackageGuard`, registry lookups, timeouts, heuristics, `AgentShieldLight`)
//! 3. No-Leak Secret Sentinel (`EnvSentinel`, polyglot AST/regex scanner, reconciliation, `.env.example`, Shannon entropy, shredder)
//! 4. Ephemeral Mock Fabric (`MockFabric`, localhost REST server, dynamic CRUD, CORS, synthetic data generation)
//! 5. Ambient Execution Recorder (`TraceRingBuffer`, bounded circular queue, post-mortem XML dump)
//! 6. Atmospheric Git Worktrees & Semantic Stashing (`AtmosphericWorktreeHandle`, `SemanticStashManager`)
//! 7. Full Client-Daemon IPC Roundtrip across all new protocol requests

use hgb_core::env_sentinel::{EnvSentinel, EnvVarStatus};
use hgb_core::glance::{detect_mime_type, extract_dimensions, DefectCategory, ImagePayload};
use hgb_core::package_guard::{PackageEcosystem, PackageGuard, PackageStatus};
use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_core::security::AgentShieldLight;
use hgb_core::trace::TraceRingBuffer;
use hgb_nextgen::glance_engine::GlanceEngine;
use hgb_nextgen::mock_fabric::{MockFabric, MockFabricConfig};
use hgb_nextgen::worktree::{AtmosphericWorktreeHandle, SemanticStashManager};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn create_temp_dir(prefix: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "hgb_sprint_test_{}_{}_{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

// =========================================================================
// PILLAR 1: MULTIMODAL VISUAL CANVAS & LAYOUT AUTOPSY (GLANCE ENGINE)
// =========================================================================
#[tokio::test]
async fn test_pillar1_glance_engine_magic_bytes_dimensions_and_autopsy() {
    // 1. Synthesize PNG binary header (1920x1080)
    let mut png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png_bytes.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
    png_bytes.extend_from_slice(b"IHDR");
    png_bytes.extend_from_slice(&1920u32.to_be_bytes());
    png_bytes.extend_from_slice(&1080u32.to_be_bytes());
    png_bytes.extend_from_slice(&[8, 6, 0, 0, 0]);

    assert_eq!(detect_mime_type(&png_bytes), Some("image/png".to_string()));
    let (w, h) = extract_dimensions(&png_bytes, "image/png");
    assert_eq!(w, Some(1920));
    assert_eq!(h, Some(1080));

    let payload = ImagePayload::from_bytes(png_bytes, None).expect("payload creation");
    assert_eq!(payload.mime_type, "image/png");
    assert_eq!(payload.width, Some(1920));
    assert_eq!(payload.height, Some(1080));
    assert!(payload.to_data_uri().starts_with("data:image/png;base64,"));
    assert!(!payload.digest.is_empty());

    // 2. Synthesize JPEG binary header (800x600)
    let mut jpeg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x08, b'J', b'F', b'I', b'F', 0x00, 0x01];
    // Add SOF0 marker 0xFFC0 (length 11, height 600, width 800)
    jpeg_bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
    jpeg_bytes.extend_from_slice(&600u16.to_be_bytes());
    jpeg_bytes.extend_from_slice(&800u16.to_be_bytes());
    jpeg_bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
    jpeg_bytes.extend_from_slice(&[0xFF, 0xD9]); // EOI

    assert_eq!(detect_mime_type(&jpeg_bytes), Some("image/jpeg".to_string()));
    let (jw, jh) = extract_dimensions(&jpeg_bytes, "image/jpeg");
    assert_eq!(jw, Some(800));
    assert_eq!(jh, Some(600));

    // 3. Synthesize WebP binary header (1024x768)
    let mut webp_bytes = b"RIFF".to_vec();
    webp_bytes.extend_from_slice(&100u32.to_le_bytes()); // file size
    webp_bytes.extend_from_slice(b"WEBP");
    webp_bytes.extend_from_slice(b"VP8 ");
    webp_bytes.extend_from_slice(&40u32.to_le_bytes()); // chunk size
    webp_bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x9D, 0x01, 0x2A]);
    // 14-bit width = 1024 (0x0400), height = 768 (0x0300)
    webp_bytes.extend_from_slice(&[0x00, 0x04]);
    webp_bytes.extend_from_slice(&[0x00, 0x03]);

    assert_eq!(detect_mime_type(&webp_bytes), Some("image/webp".to_string()));
    let (ww, wh) = extract_dimensions(&webp_bytes, "image/webp");
    assert_eq!(ww, Some(1024));
    assert_eq!(wh, Some(768));

    // 4. Test Heuristic Layout Autopsy with CSS Context
    let engine = GlanceEngine::default();
    let broken_css = r#"
        .card-container {
            height: 100px;
            overflow: hidden;
        }
        .floating-modal {
            position: absolute;
            top: 20px;
        }
    "#;
    let report = engine.inspect_image(&payload, Some(broken_css)).await.expect("inspection");
    assert_eq!(report.dimensions, (1920, 1080));
    assert!(!report.defects.is_empty(), "Should detect layout issues from broken CSS");

    let clipping_defect = report.defects.iter().find(|d| d.category == DefectCategory::Clipping);
    assert!(clipping_defect.is_some(), "Should flag overflow:hidden clipping");

    let overlap_defect = report.defects.iter().find(|d| d.category == DefectCategory::Overlap);
    assert!(overlap_defect.is_some(), "Should flag unindexed absolute positioning");

    // 5. Test ANSI terminal card formatting
    let card_output = GlanceEngine::render_terminal_card(&report);
    assert!(card_output.contains("Visual Autopsy"));
    assert!(card_output.contains("Detected Defects"));
    assert!(card_output.contains("Recommended CSS Patches"));
}

// =========================================================================
// PILLAR 2: DEPENDENCY HALLUCINATION FIREWALL (PACKAGE GUARD)
// =========================================================================
#[tokio::test]
async fn test_pillar2_package_guard_heuristics_caching_and_security_shield() {
    let guard = PackageGuard::default();

    // 1. Hallucination heuristic detection
    let fake_packages = [
        ("tokio-curl-official-rust", PackageEcosystem::CratesIo),
        ("tokio-compat-0.2", PackageEcosystem::CratesIo),
        ("requests-async-v2", PackageEcosystem::PyPi),
        ("express-auth-ultimate", PackageEcosystem::Npm),
    ];

    for (pkg, eco) in &fake_packages {
        let rep = guard.verify_package(*eco, pkg, None).await;
        assert!(rep.is_hallucinated, "Package '{}' should be flagged as hallucinated", pkg);
        assert_eq!(rep.status, PackageStatus::SuspiciousHallucination);
        assert!(!rep.known_alternatives.is_empty(), "Should suggest real alternatives for '{}'", pkg);
    }

    // 2. In-memory TTL cache verification
    let rep1 = guard.verify_package(PackageEcosystem::CratesIo, "tokio-curl-cache-probe", None).await;
    assert!(!rep1.cached);
    let rep2 = guard.verify_package(PackageEcosystem::CratesIo, "tokio-curl-cache-probe", None).await;
    assert!(rep2.cached);

    // 3. AgentShieldLight integration blocking
    let shield_res = AgentShieldLight::audit_package_install("crates.io", "tokio-curl-official-rust", None).await;
    assert!(shield_res.is_err(), "AgentShieldLight must block hallucinated package");
    let err_msg = shield_res.unwrap_err().to_string();
    assert!(err_msg.contains("Dependency Hallucination Firewall blocked installation"));

    // 4. Offline / timeout graceful degradation test
    let slow_guard = PackageGuard::new(1, 1, 60); // 1ms timeout guarantees offline fallback
    let fallback_rep = slow_guard.verify_package(PackageEcosystem::CratesIo, "serde", Some("1.0.0")).await;
    assert!(!fallback_rep.is_hallucinated, "Offline fallback must not falsely flag valid packages as hallucinated");
    assert_eq!(fallback_rep.status, PackageStatus::RegistryUnavailable);
}

// =========================================================================
// PILLAR 3: NO-LEAK SECRET SENTINEL (ENV SENTINEL)
// =========================================================================
#[test]
fn test_pillar3_env_sentinel_polyglot_scanner_entropy_and_shredder() {
    let ws = create_temp_dir("env_sentinel");

    // 1. Create polyglot files
    let ts_file = ws.join("service.ts");
    let ts_code = r#"
        import { config } from 'dotenv';
        const apiKey = process.env.GEMINI_API_KEY;
        const dbUrl = process.env["DATABASE_URL"];
        const vitePort = import.meta.env.VITE_PORT;
    "#;
    fs::write(&ts_file, ts_code).unwrap();

    let rust_file = ws.join("main.rs");
    let rust_code = r#"
        let s3_bucket = std::env::var("AWS_S3_BUCKET").unwrap();
        let app_env = env!("APP_ENV");
    "#;
    fs::write(&rust_file, rust_code).unwrap();

    let py_file = ws.join("model.py");
    let py_code = r#"
        import os
        secret = os.environ["OPENAI_SECRET_KEY"]
        log_level = os.getenv("LOG_LEVEL", "info")
    "#;
    fs::write(&py_file, py_code).unwrap();

    let go_file = ws.join("server.go");
    let go_code = r#"
        port := os.Getenv("PORT")
        host, _ := os.LookupEnv("HOST")
    "#;
    fs::write(&go_file, go_code).unwrap();

    // 2. Create .env with mixed real, placeholder, and unused variables
    let env_file = ws.join(".env");
    let env_content = r#"
        GEMINI_API_KEY=AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q
        DATABASE_URL=your_api_key_here
        AWS_S3_BUCKET=production-bucket-us-east-1
        UNUSED_SECRET=some_value
    "#;
    fs::write(&env_file, env_content).unwrap();

    // 3. Audit workspace
    let report = EnvSentinel::audit_workspace(&ws, None).expect("audit report");
    assert!(report.env_file_found);
    assert!(!report.is_clean);

    let get_item = |name: &str| report.variables.iter().find(|v| v.name == name).expect("variable");
    assert!(matches!(get_item("GEMINI_API_KEY").status, EnvVarStatus::Configured { .. }));
    assert!(matches!(get_item("DATABASE_URL").status, EnvVarStatus::PlaceholderValue { .. }));
    assert!(matches!(get_item("OPENAI_SECRET_KEY").status, EnvVarStatus::MissingInEnv));
    assert!(matches!(get_item("PORT").status, EnvVarStatus::MissingInEnv));
    assert!(matches!(get_item("UNUSED_SECRET").status, EnvVarStatus::UnusedInCode));

    // 4. Generate clean .env.example
    let example = EnvSentinel::generate_env_example(&report);
    assert!(example.contains("PORT=3000"));
    assert!(example.contains("DATABASE_URL=postgres://user:password@localhost:5432/mydb"));
    assert!(example.contains("OPENAI_SECRET_KEY=<YOUR_OPENAI_SECRET_KEY_HERE>"));
    assert!(!example.contains("AIzaSyA1B2C3D4E5F6"), "Must never leak real credentials in .env.example");

    // 5. Secret shredder test
    let leaky_snippet = r#"
        const geminiKey = "AIzaSyB9C8D7E6F5G4H3I2J1K0L9M8N7O6P5Q4";
        const openaiKey = "sk-proj-1234567890abcdef1234567890abcdef1234567890abcdef12";
        const githubPat = "ghp_123456789012345678901234567890123456";
        const awsKey = "AKIA1234567890ABCDEF";
    "#;
    let (sanitized, leaks) = EnvSentinel::shred_secrets(leaky_snippet);
    assert_eq!(leaks.len(), 4);
    assert!(sanitized.contains("[REDACTED_SECRET:"));
    assert!(!sanitized.contains("AIzaSyB9C8D7E6"));
    assert!(!sanitized.contains("sk-proj-123456"));
    assert!(!sanitized.contains("ghp_123456"));
    assert!(!sanitized.contains("AKIA1234567890"));

    // 6. Shannon Entropy verification
    let high_entropy = "h48d7s6f5g4h3j2k1l9z8x7c6v5b4n3m";
    let low_entropy = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert!(EnvSentinel::calculate_shannon_entropy(high_entropy) > 4.0);
    assert!(EnvSentinel::calculate_shannon_entropy(low_entropy) < 0.1);

    let _ = fs::remove_dir_all(&ws);
}

// =========================================================================
// PILLAR 4: EPHEMERAL MOCK FABRIC (MOCK FABRIC)
// =========================================================================
#[tokio::test]
async fn test_pillar4_mock_fabric_http_rest_crud_and_synthetic_data() {
    let config = MockFabricConfig {
        resource_name: "products".to_string(),
        schema_template: serde_json::json!({
            "id": "prod_1",
            "name": "MacBook Pro M3",
            "price": 1999.99,
            "status": "in_stock"
        }),
        preferred_port: None,
        seed_count: 3,
    };

    let server = MockFabric::start(config).await.expect("mock server start");
    let port = server.port();
    assert!(port > 0);
    assert_eq!(server.resource_name(), "products");
    assert_eq!(server.count().await, 3);

    let base_url = format!("http://127.0.0.1:{}/api/products", port);
    let client = reqwest::Client::new();

    // 1. CORS Preflight OPTIONS
    let options_res = client
        .request(reqwest::Method::OPTIONS, &base_url)
        .send()
        .await
        .expect("options request");
    assert_eq!(options_res.status(), reqwest::StatusCode::OK);
    assert_eq!(
        options_res.headers().get("Access-Control-Allow-Origin").and_then(|v| v.to_str().ok()),
        Some("*")
    );

    // 2. GET /api/products (List all)
    let get_all_res = client.get(&base_url).send().await.expect("get all");
    assert_eq!(get_all_res.status(), reqwest::StatusCode::OK);
    let items: Vec<serde_json::Value> = get_all_res.json().await.expect("items json");
    assert_eq!(items.len(), 3);

    // 3. GET /api/products/:id
    let first_id = items[0]["id"].as_str().expect("id str");
    let get_one_res = client.get(format!("{}/{}", base_url, first_id)).send().await.expect("get one");
    assert_eq!(get_one_res.status(), reqwest::StatusCode::OK);
    let item: serde_json::Value = get_one_res.json().await.expect("item json");
    assert_eq!(item["id"], first_id);

    // 4. POST /api/products (Create)
    let new_product = serde_json::json!({
        "id": "prod_custom_99",
        "name": "Mechanical Keyboard",
        "price": 149.50,
        "status": "in_stock"
    });
    let post_res = client.post(&base_url).json(&new_product).send().await.expect("post request");
    assert_eq!(post_res.status(), reqwest::StatusCode::CREATED);
    assert_eq!(server.count().await, 4);

    // 5. PUT /api/products/:id (Update)
    let update_payload = serde_json::json!({
        "price": 129.99,
        "status": "on_sale"
    });
    let put_res = client
        .put(format!("{}/prod_custom_99", base_url))
        .json(&update_payload)
        .send()
        .await
        .expect("put request");
    assert_eq!(put_res.status(), reqwest::StatusCode::OK);
    let updated_item: serde_json::Value = put_res.json().await.expect("updated json");
    assert_eq!(updated_item["price"], 129.99);

    // 6. DELETE /api/products/:id
    let del_res = client
        .delete(format!("{}/prod_custom_99", base_url))
        .send()
        .await
        .expect("delete request");
    assert_eq!(del_res.status(), reqwest::StatusCode::OK);
    assert_eq!(server.count().await, 3);

    // 7. Stop server
    server.stop();
}

// =========================================================================
// PILLAR 5: AMBIENT EXECUTION RECORDER (TRACE RING BUFFER)
// =========================================================================
#[test]
fn test_pillar5_trace_ring_buffer_bounded_capacity_and_post_mortem() {
    let ring = TraceRingBuffer::new(4);
    assert!(ring.is_empty());

    // Record various execution events
    let seq1 = ring.record_thought("Initializing ReAct agent loop");
    let _seq2 = ring.record_tool("view_file", "path: src/main.rs", true, 5);
    let _seq3 = ring.record_command("cargo check", 101, 350, "", "error[E0425]: cannot find value `x`");
    let _seq4 = ring.record_http("GET", "http://127.0.0.1:3000/api/health", 500, 12);
    let seq5 = ring.record_alert("WARN", "Dev server port 3000 unresponsive");

    assert_eq!(seq1, 1);
    assert_eq!(seq5, 5);
    assert_eq!(ring.len(), 4, "Buffer capacity must be strictly bounded");

    let recent = ring.get_recent(10);
    assert_eq!(recent.len(), 4);
    assert_eq!(recent[0].sequence, 2, "Oldest event (seq 1) should have been FIFO evicted");
    assert_eq!(recent[3].sequence, 5);

    // Test XML prompt post-mortem rendering
    let post_mortem = ring.render_post_mortem(3);
    assert!(post_mortem.contains("<ambient_execution_recorder total_recorded=\"5\" showing_recent=\"3\">"));
    assert!(post_mortem.contains("<cmd>cargo check</cmd>"));
    assert!(post_mortem.contains("<stderr>error[E0425]: cannot find value `x`</stderr>"));
    assert!(post_mortem.contains("<event seq=\"4\" type=\"HttpProbe\" method=\"GET\" status=\"500\""));
    assert!(post_mortem.contains("</ambient_execution_recorder>"));
}

// =========================================================================
// PILLAR 6: ATMOSPHERIC GIT WORKTREES & SEMANTIC STASHING
// =========================================================================
#[test]
fn test_pillar6_atmospheric_worktree_lifecycle_and_semantic_stash() {
    let ws = create_temp_dir("worktree_test");

    // Initialize temporary git repo
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["init"])
        .output();
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["config", "user.name", "Hagibis Test"])
        .output();
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["config", "user.email", "test@hagibis.dev"])
        .output();

    let readme = ws.join("README.md");
    fs::write(&readme, "# Hagibis Test Repo\n").unwrap();
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["add", "README.md"])
        .output();
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["commit", "-m", "Initial commit"])
        .output();

    // 1. Create Atmospheric Worktree
    let mut wt = AtmosphericWorktreeHandle::create(&ws, "experiment-vibe", None, true)
        .expect("worktree create");
    assert!(wt.worktree_path.exists());
    assert_eq!(wt.branch_name, "experiment-vibe");

    // Modify file inside worktree
    let wt_file = wt.worktree_path.join("vibe.txt");
    fs::write(&wt_file, "Vibe coding in worktree").unwrap();

    // Clean up worktree
    wt.cleanup().expect("worktree cleanup");
    assert!(!wt.worktree_path.exists(), "Worktree path should be removed");

    // 2. Semantic Stash test
    let main_code = ws.join("main.rs");
    fs::write(&main_code, "fn main() { println!(\"Hello World\"); }\n").unwrap();
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["add", "main.rs"])
        .output();
    let _ = std::process::Command::new("git")
        .current_dir(&ws)
        .args(["commit", "-m", "Add main.rs"])
        .output();

    // Modify main.rs uncommitted
    fs::write(&main_code, "fn main() { println!(\"Hello Hagibis Semantic Stash\"); }\n").unwrap();

    let stash = SemanticStashManager::create_stash(&ws, "test-feature", Some("Experimental greeting"))
        .expect("create stash");
    assert_eq!(stash.tag, "test-feature");
    assert_eq!(stash.affected_files, vec!["main.rs".to_string()]);
    assert!(stash.diff.contains("+fn main() { println!(\"Hello Hagibis Semantic Stash\"); }"));

    let stashes = SemanticStashManager::list_stashes(&ws).expect("list stashes");
    assert_eq!(stashes.len(), 1);
    assert_eq!(stashes[0].tag, "test-feature");

    SemanticStashManager::drop_stash(&ws, "test-feature").expect("drop stash");
    assert!(SemanticStashManager::list_stashes(&ws).unwrap().is_empty());

    let _ = fs::remove_dir_all(&ws);
}

// =========================================================================
// PILLAR 7: FULL CLIENT-DAEMON IPC ROUNDTRIP ACROSS ALL NEW PROTOCOL REQUESTS
// =========================================================================
#[tokio::test]
async fn test_full_client_daemon_ipc_roundtrip_all_sprint_requests() {
    let ws = create_temp_dir("daemon_ipc_sprint");
    let socket_path = ws.join("hgbd_sprint.sock");

    let daemon = Arc::new(hgb_daemon::server::HagibisDaemon::new(&socket_path));
    let daemon_clone = Arc::clone(&daemon);

    let daemon_task = tokio::spawn(async move {
        let _ = daemon_clone.run().await;
    });

    // Wait up to 1 second for socket creation
    let mut ready = false;
    for _ in 0..50 {
        if socket_path.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert!(ready, "Daemon socket must be established");

    let client = hgb_cli::client::HgbClient::with_socket(&socket_path);

    // 1. PackageVerify IPC
    let pkg_req = HgbRequest::PackageVerify {
        ecosystem: "crates.io".to_string(),
        name: "tokio-curl-official-rust".to_string(),
        version: None,
    };
    let pkg_resp = client.send(pkg_req).await.expect("package verify ipc");
    if let HgbResponse::PackageVerified(rep) = pkg_resp {
        assert!(rep.is_hallucinated);
        assert_eq!(rep.status, PackageStatus::SuspiciousHallucination);
    } else {
        panic!("Unexpected response variant for PackageVerify");
    }

    // 2. EnvScan IPC
    let env_req = HgbRequest::EnvScan {
        workspace_root: Some(ws.display().to_string()),
        env_file: None,
    };
    let env_resp = client.send(env_req).await.expect("env scan ipc");
    assert!(matches!(env_resp, HgbResponse::EnvAudit(_)));

    // 3. EnvShred IPC
    let shred_req = HgbRequest::EnvShred {
        content: "const key = 'AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q';".to_string(),
    };
    let shred_resp = client.send(shred_req).await.expect("env shred ipc");
    if let HgbResponse::EnvShredded { sanitized_content, leaks_detected } = shred_resp {
        assert_eq!(leaks_detected, 1);
        assert!(sanitized_content.contains("[REDACTED_SECRET:"));
    } else {
        panic!("Unexpected response variant for EnvShred");
    }

    // 4. MockServerStart IPC
    let mock_req = HgbRequest::MockServerStart {
        resource_name: "orders".to_string(),
        schema_json: None,
        port: None,
        seed_count: 2,
    };
    let mock_resp = client.send(mock_req).await.expect("mock server start ipc");
    if let HgbResponse::MockServerStarted { url, port, resource, seed_count } = mock_resp {
        assert!(port > 0);
        assert_eq!(resource, "orders");
        assert_eq!(seed_count, 2);
        assert!(url.contains("/api/orders"));
    } else {
        panic!("Unexpected response variant for MockServerStart");
    }

    // 5. TraceGetContext IPC
    let trace_req = HgbRequest::TraceGetContext { last_n: 10 };
    let trace_resp = client.send(trace_req).await.expect("trace get context ipc");
    if let HgbResponse::TraceContext(dump) = trace_resp {
        assert!(dump.contains("<ambient_execution_recorder"));
    } else {
        panic!("Unexpected response variant for TraceGetContext");
    }

    daemon_task.abort();
    let _ = fs::remove_dir_all(&ws);
}
