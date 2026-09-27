//! # Brutal Integration Tests for Hagibis Superpowers 74..78
//!
//! Brutally tests:
//! 1. Superpower 74: VisualCanvasHud (Webview HUD, DOM element selection, AST mapping, live CSS tweaks, model switching)
//! 2. Superpower 75: EdgeDeployer (Cloudflare, Vercel, Fly.io, Vella network, framework detection, Blake3 manifest)
//! 3. Superpower 76: VisualAnnotationParser (Spatial bounding boxes, SVG parsing, AST component binding, Xerox clipboard)
//! 4. Superpower 77: MultiplayerSwarmHub (Peer presence, shared speculative racing, flight graph sync, heartbeat pruning)
//! 5. Superpower 78: CompanionEditorBridge (VS Code, Neovim, Helix, Zed configs, socket probing, installation)
//! 6. Daemon IPC protocol serialization and roundtrips (Requests 74..78 <-> Responses 74..78)

use hgb_core::*;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}_{}", prefix, nanos));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// =========================================================================
// 1. Superpower 74: VisualCanvasHud
// =========================================================================
#[tokio::test]
async fn test_brutal_superpower_74_visual_canvas_hud() {
    let temp = make_temp_dir("brutal_hud");
    let src = temp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let comp_file = src.join("Button.tsx");
    std::fs::write(&comp_file, "export function Button() { return <button className=\"bg-blue-600 text-white\">Click</button>; }").unwrap();

    let cfg = CanvasHudConfig {
        preferred_port: None,
        workspace_root: temp.clone(),
        active_model: Some("gemini-2.5-flash".to_string()),
        enable_disk_sync: true,
    };

    let handle = VisualCanvasHud::start(cfg).await.expect("Failed to start VisualCanvasHud");
    assert!(handle.port() > 0);
    assert!(handle.hud_url().contains(&handle.port().to_string()));
    assert_eq!(handle.active_model().await, "gemini-2.5-flash");

    // 1.1 Element Selection & AST Mapping
    let sel = HudElementSelection {
        selector: "button.bg-blue-600".to_string(),
        tag: "button".to_string(),
        id: None,
        classes: vec!["bg-blue-600".to_string(), "text-white".to_string()],
        inner_text: Some("Click".to_string()),
        bounding_box: HudBoundingBox::new(20.0, 40.0, 120.0, 36.0),
        ast_mapping: None,
        timestamp_ms: 1000,
    };
    handle.record_selection(sel, &temp).await;
    let recorded = handle.get_selections().await;
    assert_eq!(recorded.len(), 1);
    let ast = recorded[0].ast_mapping.as_ref().expect("Should have mapped AST component");
    assert_eq!(ast.component_name, "Button");
    assert_eq!(ast.element_tag, "button");

    // 1.2 Live CSS Tweak & Disk Mutation
    let tweak = CssLiveTweak {
        selector: "button.bg-blue-600".to_string(),
        property: "className".to_string(),
        old_value: "bg-blue-600".to_string(),
        new_value: "bg-emerald-500 shadow-xl".to_string(),
        applied_to_disk: false,
        timestamp_ms: 1001,
    };
    let written = VisualCanvasHud::apply_css_tweak_to_file(&temp, &tweak).unwrap();
    assert!(written, "Tweak must be written to disk");
    let mutated_content = std::fs::read_to_string(&comp_file).unwrap();
    assert!(mutated_content.contains("bg-emerald-500 shadow-xl"));

    handle.record_tweak(tweak).await;
    assert_eq!(handle.get_tweaks().await.len(), 1);

    // 1.3 Browser-driven Model Switch
    handle.set_active_model("gemini-2.5-pro").await;
    assert_eq!(handle.active_model().await, "gemini-2.5-pro");

    // 1.4 HTML generation
    let html = VisualCanvasHud::render_hud_html(handle.port(), "gemini-2.5-pro");
    assert!(html.contains("Hagibis Visual Canvas HUD"));
    assert!(html.contains("PORT"));

    // 1.5 Report
    let rep = handle.generate_report().await;
    assert_eq!(rep.status, "running");
    assert_eq!(rep.selections_count, 1);
    assert_eq!(rep.tweaks_count, 1);

    handle.stop();
    let _ = std::fs::remove_dir_all(&temp);
}

// =========================================================================
// 2. Superpower 75: EdgeDeployer
// =========================================================================
#[test]
fn test_brutal_superpower_75_edge_deployer() {
    let temp = make_temp_dir("brutal_edge");

    // Framework detection: Next.js
    std::fs::write(temp.join("package.json"), r#"{"dependencies": {"next": "14.2.0"}}"#).unwrap();
    assert_eq!(EdgeDeployer::detect_framework(&temp), DetectedFramework::NextJs);

    // Framework detection: Vite
    std::fs::write(temp.join("package.json"), r#"{"devDependencies": {"vite": "5.1.0"}}"#).unwrap();
    assert_eq!(EdgeDeployer::detect_framework(&temp), DetectedFramework::ViteReact);

    // Framework detection: Rust Axum
    let _ = std::fs::remove_file(temp.join("package.json"));
    std::fs::write(temp.join("Cargo.toml"), r#"[package]
name = "api"
[dependencies]
axum = "0.7""#).unwrap();
    assert_eq!(EdgeDeployer::detect_framework(&temp), DetectedFramework::RustAxum);

    // Deploy to Cloudflare Pages
    let cf_cfg = EdgeDeployConfig {
        provider: EdgeProvider::CloudflarePages,
        project_slug: "hagibis-edge-test".to_string(),
        workspace_root: temp.clone(),
        custom_domain: None,
        environment: Some("production".to_string()),
        write_config_files: true,
    };
    let cf_rep = EdgeDeployer::deploy(cf_cfg).expect("Cloudflare deploy should succeed");
    assert_eq!(cf_rep.public_url, "https://hagibis-edge-test.pages.dev");
    assert!(cf_rep.preview_urls.len() >= 2);
    assert!(temp.join("wrangler.toml").exists());
    assert!(temp.join("_routes.json").exists());
    assert!(cf_rep.manifest_checksum.starts_with("blake3:"));

    // Deploy to Vercel
    let vercel_cfg = EdgeDeployConfig {
        provider: EdgeProvider::Vercel,
        project_slug: "hagibis-vercel".to_string(),
        workspace_root: temp.clone(),
        custom_domain: Some("https://custom.hagibis.io".to_string()),
        environment: Some("production".to_string()),
        write_config_files: true,
    };
    let vercel_rep = EdgeDeployer::deploy(vercel_cfg).expect("Vercel deploy should succeed");
    assert_eq!(vercel_rep.public_url, "https://custom.hagibis.io");
    assert!(temp.join("vercel.json").exists());

    // Deploy to Fly.io
    let fly_cfg = EdgeDeployConfig {
        provider: EdgeProvider::FlyIo,
        project_slug: "hagibis-fly".to_string(),
        workspace_root: temp.clone(),
        custom_domain: None,
        environment: Some("production".to_string()),
        write_config_files: true,
    };
    let fly_rep = EdgeDeployer::deploy(fly_cfg).expect("Fly deploy should succeed");
    assert_eq!(fly_rep.public_url, "https://hagibis-fly.fly.dev");
    assert!(temp.join("fly.toml").exists());

    // Deploy to Vella Network
    let vella_cfg = EdgeDeployConfig {
        provider: EdgeProvider::VellaNetwork,
        project_slug: "hagibis-vella".to_string(),
        workspace_root: temp.clone(),
        custom_domain: None,
        environment: Some("production".to_string()),
        write_config_files: true,
    };
    let vella_rep = EdgeDeployer::deploy(vella_cfg).expect("Vella deploy should succeed");
    assert_eq!(vella_rep.public_url, "https://hagibis-vella.vella.network");
    assert!(temp.join("vella.edge.json").exists());

    let _ = std::fs::remove_dir_all(&temp);
}

// =========================================================================
// 3. Superpower 76: VisualAnnotationParser
// =========================================================================
#[test]
fn test_brutal_superpower_76_visual_annotation() {
    let temp = make_temp_dir("brutal_ann");
    let src = temp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("Navbar.tsx"), "export function Navbar() { return <header>Header</header>; }").unwrap();
    std::fs::write(src.join("Footer.tsx"), "export function Footer() { return <footer>Footer</footer>; }").unwrap();

    // 3.1 JSON Parsing
    let json = r#"[
        {
            "id": "box_nav",
            "kind": "bounding_box",
            "x": 0.0,
            "y": 0.05,
            "width": 1.0,
            "height": 0.15,
            "label": "Top Navigation",
            "intent": "Add responsive hamburger menu",
            "hint": "Navbar"
        },
        {
            "id": "box_footer",
            "kind": "crop_region",
            "x": 0.0,
            "y": 0.85,
            "width": 1.0,
            "height": 0.15,
            "label": "Footer Copyright",
            "intent": "Update copyright year to 2026",
            "hint": "Footer"
        }
    ]"#;
    let items = VisualAnnotationParser::parse_from_json(json).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].kind, AnnotationKind::BoundingBox);
    assert_eq!(items[1].kind, AnnotationKind::CropRegion);

    // 3.2 AST Binding
    let rep = VisualAnnotationParser::bind_to_ast(&items, &temp).unwrap();
    assert_eq!(rep.annotations_count, 2);
    assert_eq!(rep.ast_bindings.len(), 2);
    assert!(rep.ast_bindings[0].component_name.contains("Navbar"));
    assert!(rep.ast_bindings[1].component_name.contains("Footer"));
    assert!(rep.multimodal_prompt.contains("MULTIMODAL VISUAL ANNOTATION DIRECTIVES"));

    // 3.3 SVG Parsing
    let svg = r#"<svg width="1000" height="800">
        <rect x="100" y="50" width="400" height="200" />
        <text x="120" y="100">Fix contrast ratio</text>
    </svg>"#;
    let svg_items = VisualAnnotationParser::parse_from_svg(svg);
    assert_eq!(svg_items.len(), 2);
    assert_eq!(svg_items[0].kind, AnnotationKind::BoundingBox);
    assert_eq!(svg_items[1].kind, AnnotationKind::TextCallout);

    // 3.4 Clipboard Xerox Ingestion
    let xerox = VisualAnnotationParser::ingest_clipboard_xerox("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==", &temp).unwrap();
    assert_eq!(xerox.annotations_count, 1);
    assert!(xerox.items[0].label.contains("Clipboard Screenshot Crop"));

    let _ = std::fs::remove_dir_all(&temp);
}

// =========================================================================
// 4. Superpower 77: MultiplayerSwarmHub
// =========================================================================
#[test]
fn test_brutal_superpower_77_multiplayer_swarm() {
    let hub = MultiplayerSwarmHub::new();

    // 4.1 Host creates session
    let host = SwarmPeer::new("p_driver", "charlie", SwarmPeerRole::Driver, "gemini-2.5-pro");
    let rep1 = hub.create_or_join_session("room_alpha", "Brutal Racing Room", host);
    assert_eq!(rep1.session_id, "room_alpha");
    assert_eq!(rep1.peer_count, 1);
    assert_eq!(rep1.host_peer_id, "p_driver");

    // 4.2 Peers join
    let navigator = SwarmPeer::new("p_nav", "dave", SwarmPeerRole::Navigator, "qwen2.5-coder:7b");
    let reviewer = SwarmPeer::new("p_rev", "eve", SwarmPeerRole::Reviewer, "deepseek-chat");
    let spectator = SwarmPeer::new("p_spec", "frank", SwarmPeerRole::Spectator, "gemini-2.5-flash");

    hub.create_or_join_session("room_alpha", "Brutal Racing Room", navigator);
    hub.create_or_join_session("room_alpha", "Brutal Racing Room", reviewer);
    let rep_full = hub.create_or_join_session("room_alpha", "Brutal Racing Room", spectator);
    assert_eq!(rep_full.peer_count, 4);

    // 4.3 Update Presence
    let pres = hub.update_presence("room_alpha", "p_nav", Some("crates/hgb-core/src/lib.rs".to_string()), Some(110)).unwrap();
    assert_eq!(pres.cursor_line, Some(110));
    assert_eq!(pres.active_peers, 4);

    // 4.4 Speculative Dual-Draft Racing Synchronization
    let sync = hub.sync_race("room_alpha", "race_101", "Design ultra-fast zero-alloc parser", Some("qwen2.5-coder:7b".to_string())).unwrap();
    assert_eq!(sync.race_id, "race_101");
    assert_eq!(sync.consensus_winner.as_deref(), Some("qwen2.5-coder:7b"));
    assert_eq!(sync.synced_peers, 4);

    // 4.5 Flight Graph DAG broadcast
    let dag = serde_json::json!({
        "nodes": [
            { "id": "n1", "label": "Tokenize" },
            { "id": "n2", "label": "Speculative Race" },
            { "id": "n3", "label": "AST Verification" }
        ]
    });
    let fg = hub.broadcast_flight_graph("room_alpha", dag).unwrap();
    assert_eq!(fg.node_count, 3);
    assert_eq!(fg.version, 2);

    // 4.6 Leave session
    let left = hub.leave_session("room_alpha", "p_spec").unwrap();
    assert!(left);
    let rep_after = hub.get_session_report("room_alpha").unwrap();
    assert_eq!(rep_after.peer_count, 3);

    // 4.7 Heartbeat Prune (timeout 1000s -> 0 pruned)
    let pruned = hub.prune_stale_peers("room_alpha", 1000).unwrap();
    assert_eq!(pruned, 0);
}

// =========================================================================
// 5. Superpower 78: CompanionEditorBridge
// =========================================================================
#[test]
fn test_brutal_superpower_78_companion_bridge() {
    let temp = make_temp_dir("brutal_companion");

    // 5.1 VS Code
    let vscode_cfg = CompanionBridgeConfig {
        editor: CompanionEditorKind::VsCode,
        socket_path: "/tmp/hgbd_brutal.sock".to_string(),
        workspace_root: temp.clone(),
        enable_ghost_completions: true,
        custom_keymaps: true,
    };
    let vsc_rep = CompanionEditorBridge::generate_bridge(&vscode_cfg).unwrap();
    assert_eq!(vsc_rep.files.len(), 3);
    assert!(vsc_rep.files.iter().any(|f| f.relative_path == ".vscode/settings.json"));
    assert!(vsc_rep.files.iter().any(|f| f.relative_path == ".vscode/tasks.json"));
    assert!(vsc_rep.files.iter().any(|f| f.relative_path == ".vscode/keybindings.json"));

    // 5.2 Neovim
    let nvim_cfg = CompanionBridgeConfig {
        editor: CompanionEditorKind::Neovim,
        socket_path: "/tmp/hgbd_brutal.sock".to_string(),
        workspace_root: temp.clone(),
        enable_ghost_completions: true,
        custom_keymaps: true,
    };
    let nvim_rep = CompanionEditorBridge::generate_bridge(&nvim_cfg).unwrap();
    assert_eq!(nvim_rep.files.len(), 1);
    assert_eq!(nvim_rep.files[0].relative_path, "lua/hagibis.lua");
    assert!(nvim_rep.files[0].content.contains("vim.loop"));

    // 5.3 Helix
    let helix_cfg = CompanionBridgeConfig {
        editor: CompanionEditorKind::Helix,
        socket_path: "/tmp/hgbd_brutal.sock".to_string(),
        workspace_root: temp.clone(),
        enable_ghost_completions: true,
        custom_keymaps: true,
    };
    let hx_rep = CompanionEditorBridge::generate_bridge(&helix_cfg).unwrap();
    assert_eq!(hx_rep.files.len(), 2);
    assert!(hx_rep.files.iter().any(|f| f.relative_path == ".helix/languages.toml"));

    // 5.4 Zed
    let zed_cfg = CompanionBridgeConfig {
        editor: CompanionEditorKind::Zed,
        socket_path: "/tmp/hgbd_brutal.sock".to_string(),
        workspace_root: temp.clone(),
        enable_ghost_completions: true,
        custom_keymaps: true,
    };
    let zed_rep = CompanionEditorBridge::generate_bridge(&zed_cfg).unwrap();
    assert_eq!(zed_rep.files.len(), 1);
    assert_eq!(zed_rep.files[0].relative_path, ".zed/settings.json");

    // 5.5 Install to Workspace
    let inst_rep = CompanionEditorBridge::install_bridge(&vscode_cfg).unwrap();
    assert!(inst_rep.success);
    assert!(temp.join(".vscode/settings.json").exists());
    assert!(temp.join(".vscode/tasks.json").exists());
    assert!(temp.join(".vscode/keybindings.json").exists());

    let _ = std::fs::remove_dir_all(&temp);
}

// =========================================================================
// 6. Protocol End-to-End Serialization Roundtrip Tests (74..78)
// =========================================================================
#[test]
fn test_brutal_protocol_roundtrip_superpowers_74_to_78() {
    // 74. VisualCanvasHudStart
    let req_74 = HgbRequest::VisualCanvasHudStart {
        port: Some(7474),
        preferred_model: Some("gemini-2.5-pro".to_string()),
    };
    let json_74 = serde_json::to_string(&req_74).unwrap();
    let des_74: HgbRequest = serde_json::from_str(&json_74).unwrap();
    match des_74 {
        HgbRequest::VisualCanvasHudStart { port, preferred_model } => {
            assert_eq!(port, Some(7474));
            assert_eq!(preferred_model.as_deref(), Some("gemini-2.5-pro"));
        }
        _ => panic!("Expected VisualCanvasHudStart"),
    }

    let resp_74 = HgbResponse::VisualCanvasHudResult(CanvasHudReport {
        port: 7474,
        hud_url: "http://127.0.0.1:7474".to_string(),
        active_model: "gemini-2.5-pro".to_string(),
        status: "running".to_string(),
        selections_count: 5,
        tweaks_count: 2,
    });
    let bin_74 = bincode::serialize(&resp_74).unwrap();
    let des_resp_74: HgbResponse = bincode::deserialize(&bin_74).unwrap();
    match des_resp_74 {
        HgbResponse::VisualCanvasHudResult(rep) => {
            assert_eq!(rep.port, 7474);
            assert_eq!(rep.selections_count, 5);
        }
        _ => panic!("Expected VisualCanvasHudResult"),
    }

    // 75. EdgeDeploy
    let req_75 = HgbRequest::EdgeDeploy {
        provider: EdgeProvider::CloudflarePages,
        project_slug: "my-app".to_string(),
        write_configs: Some(true),
        custom_domain: Some("https://my-app.com".to_string()),
    };
    let json_75 = serde_json::to_string(&req_75).unwrap();
    let des_75: HgbRequest = serde_json::from_str(&json_75).unwrap();
    match des_75 {
        HgbRequest::EdgeDeploy { provider, project_slug, .. } => {
            assert_eq!(provider, EdgeProvider::CloudflarePages);
            assert_eq!(project_slug, "my-app");
        }
        _ => panic!("Expected EdgeDeploy"),
    }

    // 76. VisualAnnotate
    let req_76 = HgbRequest::VisualAnnotate {
        raw_annotation: r#"[{"x": 10.0, "y": 20.0, "width": 100.0, "height": 50.0, "intent": "Fix button"}]"#.to_string(),
    };
    let bin_76 = bincode::serialize(&req_76).unwrap();
    let des_76: HgbRequest = bincode::deserialize(&bin_76).unwrap();
    match des_76 {
        HgbRequest::VisualAnnotate { raw_annotation } => {
            assert!(raw_annotation.contains("Fix button"));
        }
        _ => panic!("Expected VisualAnnotate"),
    }

    // 77. MultiplayerSwarmAction
    let req_77 = HgbRequest::MultiplayerSwarmAction {
        session_id: "room_9".to_string(),
        action: "join".to_string(),
        payload: serde_json::json!({ "username": "alice" }),
    };
    let json_77 = serde_json::to_string(&req_77).unwrap();
    let des_77: HgbRequest = serde_json::from_str(&json_77).unwrap();
    match des_77 {
        HgbRequest::MultiplayerSwarmAction { session_id, action, .. } => {
            assert_eq!(session_id, "room_9");
            assert_eq!(action, "join");
        }
        _ => panic!("Expected MultiplayerSwarmAction"),
    }

    // 78. CompanionBridgeSetup
    let req_78 = HgbRequest::CompanionBridgeSetup {
        editor: CompanionEditorKind::Neovim,
        install: Some(false),
    };
    let bin_78 = bincode::serialize(&req_78).unwrap();
    let des_78: HgbRequest = bincode::deserialize(&bin_78).unwrap();
    match des_78 {
        HgbRequest::CompanionBridgeSetup { editor, install } => {
            assert_eq!(editor, CompanionEditorKind::Neovim);
            assert_eq!(install, Some(false));
        }
        _ => panic!("Expected CompanionBridgeSetup"),
    }
}
