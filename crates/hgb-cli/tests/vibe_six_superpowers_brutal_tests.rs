use hgb_core::ambient_vibe::{AmbientVibeConfig, AmbientVibeEngine, VibeWatchEvent};
use hgb_core::glance::{synthesize_component, ImagePayload};
use hgb_core::mcp::{McpClient, McpConfigFile, McpServerConfig};
use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_core::shell_hook::{CrashCategory, CrashInterceptor, CrashRecord, ShellHookGenerator, SupportedShell};
use hgb_core::timeline::TimelineManager;
use hgb_core::verification_gate::{GoldenInvariant, VerificationGate, VerificationStatus, VerificationStepResult};
use hgb_daemon::server::{DaemonState, HagibisDaemon};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

// =========================================================================
// SUPERPOWER 1: Universal Model Context Protocol (MCP) Client
// =========================================================================
#[tokio::test]
async fn test_mcp_config_discovery_and_tool_registration() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // 1. Write hagibis.mcp.json
    let config_content = r#"{
        "mcpServers": {
            "test-server": {
                "command": "cat",
                "args": [],
                "env": { "TEST_VAR": "1" }
            },
            "sqlite-mcp": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-sqlite"],
                "env": {}
            }
        }
    }"#;
    fs::write(temp_dir.join("hagibis.mcp.json"), config_content).unwrap();

    // 2. Discover config file
    let loaded = McpConfigFile::load_from_dir(&temp_dir)
        .expect("Discovery should not fail")
        .expect("Should discover hagibis.mcp.json");
    assert_eq!(loaded.mcp_servers.len(), 2);
    assert!(loaded.mcp_servers.contains_key("test-server"));
    assert!(loaded.mcp_servers.contains_key("sqlite-mcp"));

    let server_cfg = loaded.mcp_servers.get("test-server").unwrap();
    assert_eq!(server_cfg.command, "cat");
    assert_eq!(server_cfg.env.get("TEST_VAR"), Some(&"1".to_string()));

    // 3. Test MCP tool descriptor and mock stdio roundtrip
    let script = r#"
while read -r line; do
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d':' -f2)
  if [ "$method" = "initialize" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}"
  elif [ "$method" = "tools/list" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"weather_lookup\",\"description\":\"Look up forecast\",\"inputSchema\":{\"type\":\"object\"}}]}}"
  elif [ "$method" = "tools/call" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"sunny 24C\"}],\"isError\":false}}"
  fi
done
"#;
    let mock_cfg = McpServerConfig::new("bash").with_args(["-c", script]);
    let client = McpClient::spawn_and_handshake("weather_srv", &mock_cfg, None)
        .await
        .expect("MCP spawn and handshake must succeed");

    let tools = client.list_tools().await.expect("list_tools must succeed");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "weather_lookup");

    let call_res = client
        .call_tool("weather_lookup", serde_json::json!({"city": "Tokyo"}))
        .await
        .expect("call_tool must succeed");
    assert_eq!(call_res["content"][0]["text"], "sunny 24C");

    let hgb_tools = McpClient::create_hgb_tools(&client, Some("weather"))
        .await
        .expect("create_hgb_tools must succeed");
    assert_eq!(hgb_tools.len(), 1);
    assert_eq!(hgb_tools[0].name(), "weather_weather_lookup");

    client.close().await;
    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_daemon_mcp_list_and_call_dispatch() {
    let socket_path = PathBuf::from("/tmp/hgb_test_mcp_daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_daemon_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let cfg_path = temp_dir.join("hagibis.mcp.json");
    fs::write(&cfg_path, r#"{"mcpServers":{}}"#).unwrap();

    // Dispatch McpListTools
    let req = HgbRequest::McpListTools {
        config_path: Some(cfg_path.to_string_lossy().to_string()),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::McpToolsList(tools) => {
            assert!(tools.is_empty());
        }
        other => panic!("Expected McpToolsList, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// SUPERPOWER 2: Ephemeral Worktree "What-If" Timelines
// =========================================================================
#[tokio::test]
async fn test_ephemeral_timeline_lifecycle_creation_diff_and_discard() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_timeline_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Create an initial workspace structure
    let src_dir = temp_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("main.rs"), "fn main() { println!(\"original\"); }\n").unwrap();
    fs::write(temp_dir.join("config.toml"), "app_name = \"demo\"\n").unwrap();

    let manager = TimelineManager::new(&temp_dir);

    // 1. Create a speculative timeline
    let timeline_name = "speculative-feature-x";
    let info = manager.create_timeline(timeline_name, None).expect("Should create timeline");
    assert_eq!(info.name, timeline_name);
    assert!(info.path.exists());

    // Verify it lists the timeline
    let timelines = manager.list_timelines().expect("Should list timelines");
    assert_eq!(timelines.len(), 1);
    assert_eq!(timelines[0].name, timeline_name);

    // 2. Modify files in the timeline
    let timeline_main = info.path.join("src").join("main.rs");
    if timeline_main.exists() {
        fs::write(&timeline_main, "fn main() { println!(\"v2 with superpowers!\"); }\n").unwrap();
    }
    // Add a new file in timeline
    fs::write(info.path.join("src").join("new_feature.rs"), "pub fn extra() {}\n").unwrap();

    // 3. Diff timeline against workspace
    let diff = manager.diff_timeline(timeline_name).expect("Should diff timeline");
    assert_eq!(diff.timeline_name, timeline_name);
    assert_eq!(diff.files_changed, 2);
    assert!(diff.modified_files.contains(&"src/new_feature.rs".to_string()));
    assert!(diff.modified_files.contains(&"src/main.rs".to_string()));

    // 4. Merge timeline back to workspace
    let merge_report = manager.merge_timeline(timeline_name).expect("Should merge timeline");
    assert!(merge_report.success);
    assert!(temp_dir.join("src").join("new_feature.rs").exists());
    let merged_main = fs::read_to_string(temp_dir.join("src").join("main.rs")).unwrap();
    assert!(merged_main.contains("v2 with superpowers!"));

    // 5. Create another timeline and discard it
    let discard_name = "dead-end-experiment";
    let info2 = manager.create_timeline(discard_name, None).expect("Should create second timeline");
    assert!(info2.path.exists());

    manager.discard_timeline(discard_name).expect("Should discard timeline");
    assert!(!info2.path.exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_daemon_timeline_handlers_dispatch() {
    let socket_path = PathBuf::from("/tmp/hgb_test_timeline_daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    let temp_dir = std::env::temp_dir().join(format!("hgb_timeline_daemon_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    fs::write(temp_dir.join("app.js"), "console.log('init');").unwrap();

    let root_str = Some(temp_dir.to_string_lossy().to_string());

    // Create timeline
    let req = HgbRequest::TimelineCreate {
        name: "test-timeline".to_string(),
        base_branch: None,
        workspace_root: root_str.clone(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::TimelineCreated(info) => {
            assert_eq!(info.name, "test-timeline");
        }
        other => panic!("Expected TimelineCreated, got {:?}", other),
    }

    // List timelines
    let req = HgbRequest::TimelineList {
        workspace_root: root_str.clone(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::TimelineListReport(timelines) => {
            assert_eq!(timelines.len(), 1);
            assert_eq!(timelines[0].name, "test-timeline");
        }
        other => panic!("Expected TimelineListReport, got {:?}", other),
    }

    // Discard timeline
    let req = HgbRequest::TimelineDiscard {
        name: "test-timeline".to_string(),
        workspace_root: root_str.clone(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::TimelineDiscarded { name } => {
            assert_eq!(name, "test-timeline");
        }
        other => panic!("Expected TimelineDiscarded, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// SUPERPOWER 3: Verification Gate & Golden Invariant Guard
// =========================================================================
#[tokio::test]
async fn test_verification_gate_invariants_and_integrity_hash() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_gate_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Create Cargo.toml and src/main.rs
    fs::write(
        temp_dir.join("Cargo.toml"),
        "[package]\nname = \"dummy-gate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ).unwrap();
    fs::create_dir_all(temp_dir.join("src")).unwrap();
    fs::write(
        temp_dir.join("src/main.rs"),
        "fn main() { println!(\"verified\"); }\n",
    ).unwrap();

    let mut gate = VerificationGate::new(&temp_dir);
    gate.auto_detect_invariants();

    // Add a fast invariant
    gate = gate.with_invariant(GoldenInvariant::new("true_check", "true"));

    // Run verification gate
    let cert = gate.verify_and_heal(None::<fn(&[VerificationStepResult], usize) -> hgb_core::Result<Option<String>>>).expect("Should run verification");
    assert!(!cert.integrity_hash.is_empty());
    assert_eq!(cert.workspace_root, temp_dir);
    // Blake3 hash integrity verification
    assert!(cert.verify_integrity());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_verification_gate_self_healing_loop() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_gate_heal_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let mut gate = VerificationGate::new(&temp_dir);
    gate = gate.with_invariant(GoldenInvariant::new("Always True Invariant", "true"));

    let cert = gate.verify_and_heal(None::<fn(&[VerificationStepResult], usize) -> hgb_core::Result<Option<String>>>).expect("Verification loop should succeed");
    assert_eq!(cert.status, VerificationStatus::Passed);
    assert!(cert.verify_integrity());

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// SUPERPOWER 4: Shell Companion & Crash Interceptor
// =========================================================================
#[test]
fn test_shell_hook_generation_for_all_shells() {
    // 1. Bash hook
    let bash_sh = SupportedShell::from_str_name("bash").unwrap();
    let bash_hook = ShellHookGenerator::generate(bash_sh, "hgb");
    assert!(bash_hook.contains("__hgb_preexec"));
    assert!(bash_hook.contains("__hgb_prompt_hook"));
    assert!(bash_hook.contains("PROMPT_COMMAND"));

    // 2. Zsh hook
    let zsh_sh = SupportedShell::from_str_name("zsh").unwrap();
    let zsh_hook = ShellHookGenerator::generate(zsh_sh, "hgb");
    assert!(zsh_hook.contains("add-zsh-hook preexec"));
    assert!(zsh_hook.contains("add-zsh-hook precmd"));

    // 3. Fish hook
    let fish_sh = SupportedShell::from_str_name("fish").unwrap();
    let fish_hook = ShellHookGenerator::generate(fish_sh, "hgb");
    assert!(fish_hook.contains("--on-event fish_postexec"));

    // Unsupported shell
    assert!(SupportedShell::from_str_name("powershell").is_none());
}

#[tokio::test]
async fn test_crash_interceptor_and_diagnosis_rules() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_crash_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let interceptor = CrashInterceptor::new(&temp_dir);

    // 1. Record port in use crash
    let rec1 = CrashRecord {
        crash_id: String::new(),
        command: "npm run dev".to_string(),
        exit_code: 1,
        stdout_snippet: "".to_string(),
        stderr_snippet: "Error: listen EADDRINUSE: address already in use :::3000".to_string(),
        cwd: temp_dir.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        environment: HashMap::new(),
    };
    let crash_id = interceptor.record_crash(rec1.clone()).expect("Should record crash");
    assert!(!crash_id.is_empty());

    let latest = interceptor.load_latest_crash().expect("Should load latest crash").unwrap();
    assert_eq!(latest.command, "npm run dev");

    // 2. Diagnose port conflict
    let diag1 = interceptor.diagnose_and_fix(&rec1);
    assert_eq!(diag1.category, CrashCategory::PortConflict);
    assert!(diag1.root_cause.contains("already bound"));
    assert!(diag1.suggested_command_fix.contains("3000") || diag1.alternative_fixes.iter().any(|c| c.contains("3000")));

    // 3. Diagnose Rust compiler unresolved import / build error
    let rec2 = CrashRecord {
        crash_id: "test-rec-2".to_string(),
        command: "cargo check".to_string(),
        exit_code: 101,
        stdout_snippet: "".to_string(),
        stderr_snippet: "error[E0432]: unresolved import `serde_json::Value`\n  --> src/main.rs:2:5".to_string(),
        cwd: temp_dir.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        environment: HashMap::new(),
    };
    let diag2 = interceptor.diagnose_and_fix(&rec2);
    assert_eq!(diag2.category, CrashCategory::MissingDependency);
    assert!(diag2.root_cause.contains("Unresolved Rust crate"));
    assert!(diag2.suggested_command_fix.contains("cargo add"));

    // 4. Diagnose missing module
    let rec3 = CrashRecord {
        crash_id: "test-rec-3".to_string(),
        command: "node server.js".to_string(),
        exit_code: 1,
        stdout_snippet: "".to_string(),
        stderr_snippet: "Error: Cannot find module 'express'".to_string(),
        cwd: temp_dir.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        environment: HashMap::new(),
    };
    let diag3 = interceptor.diagnose_and_fix(&rec3);
    assert_eq!(diag3.category, CrashCategory::MissingDependency);
    assert!(diag3.suggested_command_fix.contains("npm install express"));

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// SUPERPOWER 5: Ambient "Watch-and-Vibe" Autonomous Loop
// =========================================================================
#[tokio::test]
async fn test_ambient_vibe_project_detection_and_change_tracking() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_vibe_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Setup Rust workspace
    fs::write(temp_dir.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::create_dir_all(temp_dir.join("src")).unwrap();
    let main_rs = temp_dir.join("src/main.rs");
    fs::write(&main_rs, "fn main() {}\n").unwrap();

    let suites = AmbientVibeEngine::detect_test_suites(&temp_dir);
    assert!(!suites.is_empty());
    assert_eq!(suites[0].name, "cargo test");

    let config = AmbientVibeConfig {
        debounce_duration_ms: 50,
        poll_interval_ms: 50,
        auto_run_suites: suites,
        ignored_patterns: vec![".git".into()],
        enable_speculative_fixes: true,
    };
    let mut engine = AmbientVibeEngine::new(temp_dir.clone(), config);

    // Initial check has no modified files
    let initial_changes = engine.scan_changed_files().unwrap();
    assert!(initial_changes.is_empty());

    // Modify a file
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(&main_rs, "fn main() { println!(\"vibe\"); }\n").unwrap();

    // Changed file detected
    let changes = engine.scan_changed_files().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0], main_rs);

    // Synthesize speculative patch
    let failure_msg = "error: unused import: `std::collections::HashMap`\n --> src/main.rs:1:5";
    let patch_ev = AmbientVibeEngine::synthesize_speculative_patch(failure_msg, &temp_dir);
    assert!(patch_ev.is_some());
    match patch_ev.unwrap() {
        VibeWatchEvent::SpeculativePatchReady { file_path, diff, .. } => {
            assert_eq!(file_path, main_rs);
            assert!(diff.contains("Speculative fix for unused import"));
        }
        other => panic!("Expected SpeculativePatchReady, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// SUPERPOWER 6: Visual Ingestion Component Synthesis
// =========================================================================
#[tokio::test]
async fn test_glance_visual_component_synthesis_all_frameworks() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_glance_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Create a 1x1 synthetic PNG image
    let dummy_png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG magic
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
        0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
        0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
        0x42, 0x60, 0x82,
    ];
    let img_path = temp_dir.join("test_ui.png");
    fs::write(&img_path, &dummy_png).unwrap();

    let payload = ImagePayload::load_from_path(&img_path).expect("Payload should load from path");
    assert_eq!(payload.mime_type, "image/png");

    // 1. Synthesize React + Tailwind
    let react_res = synthesize_component(&payload, "react", None).await.expect("React synthesis should succeed");
    println!("GLANCE SYNTHESIZED REACT:\n{}", react_res.component_code);
    assert_eq!(react_res.framework, "react");
    assert!(!react_res.component_code.is_empty());

    // 2. Synthesize HTML + Tailwind
    let html_res = synthesize_component(&payload, "html", None).await.expect("HTML synthesis should succeed");
    assert_eq!(html_res.framework, "html");
    assert!(!html_res.component_code.is_empty());

    // 3. Synthesize Ratatui TUI (Rust)
    let ratatui_res = synthesize_component(&payload, "ratatui", None).await.expect("Ratatui synthesis should succeed");
    assert_eq!(ratatui_res.framework, "ratatui");
    assert!(!ratatui_res.component_code.is_empty());

    let _ = fs::remove_dir_all(&temp_dir);
}
