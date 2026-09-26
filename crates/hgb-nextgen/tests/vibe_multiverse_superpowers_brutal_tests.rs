//! # Brutal Integration Tests: 6 Multiverse Vibe Developer Superpowers
//!
//! Verifies:
//! 1. Chrono-Warp Omni-Undo (4D Snapshots & <50ms Atomic Rollback)
//! 2. Phantom Swarm Traffic Simulator (Virtual Client Bots & Latency Percentiles)
//! 3. Zero-Friction Wiretap & Contract Healer (Schema Drift & Dual-Sided Patching)
//! 4. Hallucination Sentry Package Fact-Checker (LLM Dependency Interception)
//! 5. Direct Clipboard Xerox Component Engine (Image -> React/Tailwind & Ratatui)
//! 6. Token & Wattage Governor (Local-First Routing & Hardware Wattage Throttling)
//! 7. Cockpit Multiverse Slash Commands & In-Canvas Cards (/warp, /traffic, /wiretap, /sentry, /xerox, /governor)

use hgb_nextgen::chrono_warp::ChronoWarpEngine;
use hgb_nextgen::clipboard_xerox::{ClipboardXeroxEngine, ColorRole};
use hgb_nextgen::hallucination_sentry::HallucinationSentry;
use hgb_nextgen::phantom_swarm::{PhantomSwarmConfig, PhantomSwarmEngine};
use hgb_nextgen::wattage_governor::{GovernorConfig, WattageGovernor};
use hgb_nextgen::wiretap::{DriftKind, ExpectedField, WiretapEngine};
use serde_json::json;
use std::path::{Path, PathBuf};

// ============================================================================
// SUPERPOWER 1: Chrono-Warp Omni-Undo Tests
// ============================================================================

#[test]
fn test_chrono_warp_4d_capture_and_sub_50ms_atomic_rollback() {
    let mut engine = ChronoWarpEngine::new("/tmp/hgb_chrono_warp_test");

    // 1. Setup dimensions
    std::env::set_var("HGB_WARP_KEY", "initial_v1");
    engine.track_process(4100, "cargo check --workspace");

    let files_v1 = vec![
        (PathBuf::from("crates/core/src/state.rs"), b"pub struct State { v: 1 }".to_vec()),
        (PathBuf::from("crates/core/src/config.rs"), b"pub const VER: u32 = 1;".to_vec()),
    ];
    let db_v1 = b"SQLITE_WAL_BLOCK_GENESIS_ROOT";

    // 2. Capture 4D Snapshot
    let snap1 = engine.capture_4d_snapshot("v1_stable", &files_v1, Some(db_v1));
    assert_eq!(snap1.id, "v1_stable");
    assert_eq!(snap1.file_tree_hashes.len(), 2);
    assert_eq!(snap1.processes.len(), 1);
    assert!(!snap1.blake3_root.is_empty());

    // 3. Mutate workspace into V2
    std::env::set_var("HGB_WARP_KEY", "mutated_v2");
    let files_v2 = vec![
        (PathBuf::from("crates/core/src/state.rs"), b"pub struct State { v: 2 }".to_vec()),
    ];
    engine.capture_4d_snapshot("v2_buggy", &files_v2, Some(b"SQLITE_CORRUPT_BLOCK"));

    // 4. Atomic Rollback to V1
    let report = engine.rollback_4d("v1_stable").expect("Must rollback cleanly");
    assert!(report.success);
    assert!(report.rollback_duration_ms < 50, "Rollback took {}ms, must be <50ms", report.rollback_duration_ms);
    assert_eq!(report.files_restored, 2);
    assert!(report.db_restored);
    assert_eq!(std::env::var("HGB_WARP_KEY").unwrap(), "initial_v1");
}

// ============================================================================
// SUPERPOWER 2: Phantom Swarm Traffic Simulator Tests
// ============================================================================

#[tokio::test]
async fn test_phantom_swarm_traffic_and_percentiles() {
    let swarm = PhantomSwarmEngine::new();

    // 1. Verify percentile math accuracy
    let latencies = vec![
        5.0, 8.0, 10.0, 12.0, 15.0, 18.0, 20.0, 22.0, 25.0, 30.0,
        35.0, 40.0, 45.0, 50.0, 60.0, 75.0, 90.0, 110.0, 150.0, 300.0,
    ];
    let p = PhantomSwarmEngine::calculate_percentiles(latencies);
    assert_eq!(p.min_ms, 5.0);
    assert_eq!(p.max_ms, 300.0);
    assert!(p.p50_ms >= 25.0 && p.p50_ms <= 35.0);
    assert!(p.p95_ms >= 150.0);

    // 2. Run virtual bot load test
    let config = PhantomSwarmConfig {
        target_url: "http://127.0.0.1:8888/metrics".to_string(),
        concurrency: 10,
        request_count: 20,
        http_method: "GET".to_string(),
        payload: None,
        timeout_ms: 100,
    };

    let report = swarm.simulate_traffic(config).await;
    assert_eq!(report.concurrency, 10);
    assert_eq!(report.total_requests, 20);
    assert!(report.requests_per_second > 0.0);
    assert!(report.latency.max_ms >= 0.0);
}

// ============================================================================
// SUPERPOWER 3: Zero-Friction Wiretap & Contract Healer Tests
// ============================================================================

#[test]
fn test_wiretap_drift_detection_and_dual_patching() {
    let wiretap = WiretapEngine::new();

    let expected = vec![
        ExpectedField {
            name: "teamId".to_string(), // expects camelCase
            expected_type: "number".to_string(),
            required: true,
        },
        ExpectedField {
            name: "roleName".to_string(),
            expected_type: "string".to_string(),
            required: true,
        },
        ExpectedField {
            name: "isActive".to_string(),
            expected_type: "boolean".to_string(),
            required: true,
        },
    ];

    // Payload has snake_case `team_id`, string `isActive: "true"`, and missing `roleName`
    let live_payload = json!({
        "team_id": 99,
        "isActive": "true", // type mismatch: string instead of boolean
    });

    let drifts = wiretap.inspect_payload("/api/roles", &expected, &live_payload);
    assert_eq!(drifts.len(), 3);

    assert!(drifts.iter().any(|d| d.kind == DriftKind::NamingConventionMismatch));
    assert!(drifts.iter().any(|d| d.kind == DriftKind::TypeMismatch));
    assert!(drifts.iter().any(|d| d.kind == DriftKind::MissingField));

    // Synthesize dual-sided patch
    let patch = wiretap.heal_contract("/api/roles", &drifts);
    assert_eq!(patch.drifts_found, 3);
    assert!(patch.frontend_ts_adapter.contains("Healed_api_rolesResponse"));
    assert!(patch.backend_rust_dto.contains("pub struct Healed_api_rolesDto"));
    assert!(patch.diff_summary.contains("Healed 3 contract drift(s)"));
}

// ============================================================================
// SUPERPOWER 4: Hallucination Sentry Package Fact-Checker Tests
// ============================================================================

#[test]
fn test_hallucination_sentry_multi_ecosystem_verification() {
    let sentry = HallucinationSentry::new();

    // 1. Cargo.toml audit with hallucinated crate `serde-super`
    let cargo_manifest = r#"
[dependencies]
tokio = "1.0"
serde = "1.0"
serde-super = "1.4"
ratatui = "0.29"
"#;
    let report_cargo = sentry.audit_manifest(Path::new("Cargo.toml"), cargo_manifest);
    assert_eq!(report_cargo.total_packages, 4);
    assert_eq!(report_cargo.verified_valid, 3);
    assert_eq!(report_cargo.hallucinated_count, 1);
    assert!(report_cargo.healed_content.unwrap().contains("serde = \"1.4\""));

    // 2. package.json audit with hallucinated package `next-vibe`
    let npm_manifest = r#"{
      "dependencies": {
        "react": "18.0.0",
        "next": "14.0.0",
        "next-vibe": "1.0.0"
      }
    }"#;
    let report_npm = sentry.audit_manifest(Path::new("package.json"), npm_manifest);
    assert_eq!(report_npm.total_packages, 3);
    assert_eq!(report_npm.verified_valid, 2);
    assert_eq!(report_npm.hallucinated_count, 1);
    assert_eq!(
        report_npm.entries.iter().find(|e| e.package_name == "next-vibe").unwrap().canonical_replacement.as_deref(),
        Some("next")
    );
}

// ============================================================================
// SUPERPOWER 5: Direct Clipboard Xerox Component Engine Tests
// ============================================================================

#[test]
fn test_clipboard_xerox_image_to_component_synthesis() {
    let xerox = ClipboardXeroxEngine::new();
    let dummy_image_bytes = vec![255u8; 1280 * 720 * 3];

    let result = xerox.xerox_image("MetricsPanel", 1280, 720, &dummy_image_bytes);

    assert_eq!(result.component_name, "MetricsPanel");
    assert_eq!(result.aspect_ratio, "16:9 Landscape");
    assert_eq!(result.palette.len(), 4);
    assert!(result.palette.iter().any(|c| c.role == ColorRole::Primary));
    assert!(result.palette.iter().any(|c| c.role == ColorRole::Background));

    // Assert React component code
    assert!(result.react_tailwind_code.contains("export const MetricsPanel"));
    assert!(result.react_tailwind_code.contains("bg-[#0f172a]"));
    assert!(result.react_tailwind_code.contains("<aside className=\"w-64"));

    // Assert Ratatui TUI component code
    assert!(result.ratatui_tui_code.contains("pub fn render_metricspanel"));
    assert!(result.ratatui_tui_code.contains("Layout::default()"));
}

// ============================================================================
// SUPERPOWER 6: Token & Wattage Governor Tests
// ============================================================================

#[test]
fn test_wattage_governor_smart_routing_and_budget_caps() {
    let config = GovernorConfig {
        daily_budget_usd: 0.10, // strict 10 cents cap
        session_budget_usd: 0.05,
        max_cloud_tokens: 10_000,
        prefer_local_ollama: true,
        idle_power_mw: 1000.0,
        core_power_mw: 500.0,
    };
    let mut governor = WattageGovernor::new(config);

    // 1. Initial power estimation
    let idle_pwr = governor.estimate_power_draw_mw(0);
    assert_eq!(idle_pwr, 1000.0);
    let hw = governor.hardware_threads();
    let expected_active = 1000.0 + (4usize.min(hw) as f64 * 500.0);
    let active_pwr = governor.estimate_power_draw_mw(4);
    assert_eq!(active_pwr, expected_active);

    // 2. Routine task routes to zero-cost local Ollama
    let decision1 = governor.route_task("refactor function arguments", 30);
    assert!(decision1.is_local);
    assert_eq!(decision1.estimated_cost_usd, 0.0);
    assert!(decision1.target_model.contains("ollama"));

    // 3. Complex task routes to Cloud
    let decision2 = governor.route_task("Distributed Raft Consensus Verification", 90);
    assert!(!decision2.is_local);
    assert_eq!(decision2.target_model, "gemini-2.5-pro");

    // 4. Record token usage and verify hard budget cap throttling
    governor.record_tokens(false, 1_000_000); // $0.15 > $0.10 limit
    let tel = governor.get_telemetry();
    assert!(tel.is_throttled);
    assert_eq!(tel.remaining_budget_usd, 0.0);

    // 5. Subsequent complex tasks are automatically throttled to local model
    let decision3 = governor.route_task("Distributed Raft Consensus Verification", 90);
    assert!(decision3.is_local);
    assert!(decision3.reason.contains("Budget cap reached"));
}

// ============================================================================
// INTEGRATION: Cockpit Slash Commands for Multiverse Superpowers
// ============================================================================

#[test]
fn test_cockpit_multiverse_slash_commands() {
    use hgb_nextgen::cockpit::CockpitVibeManager;

    // 1. /warp
    let item1 = CockpitVibeManager::handle_vibe_slash_command("/warp", "").expect("Must handle /warp");
    if let hgb_nextgen::CockpitItem::ChronoWarpCard(card) = item1 {
        assert_eq!(card.action, "4D SNAPSHOT CAPTURED");
    } else {
        panic!("Expected ChronoWarpCard");
    }

    // 2. /traffic
    let item2 = CockpitVibeManager::handle_vibe_slash_command("/traffic", "http://localhost:3000").expect("Must handle /traffic");
    if let hgb_nextgen::CockpitItem::PhantomSwarmCard(card) = item2 {
        assert_eq!(card.concurrency, 10);
        assert!(card.rps > 0.0);
    } else {
        panic!("Expected PhantomSwarmCard");
    }

    // 3. /wiretap
    let item3 = CockpitVibeManager::handle_vibe_slash_command("/wiretap", "/api/v1/auth").expect("Must handle /wiretap");
    if let hgb_nextgen::CockpitItem::WiretapCard(card) = item3 {
        assert_eq!(card.endpoint, "/api/v1/auth");
    } else {
        panic!("Expected WiretapCard");
    }

    // 4. /sentry
    let item4 = CockpitVibeManager::handle_vibe_slash_command("/sentry", "Cargo.toml").expect("Must handle /sentry");
    if let hgb_nextgen::CockpitItem::HallucinationSentryCard(card) = item4 {
        assert_eq!(card.manifest, "Cargo.toml");
        assert_eq!(card.hallucinated, 1);
    } else {
        panic!("Expected HallucinationSentryCard");
    }

    // 5. /xerox
    let item5 = CockpitVibeManager::handle_vibe_slash_command("/xerox", "mockup.png").expect("Must handle /xerox");
    if let hgb_nextgen::CockpitItem::ClipboardXeroxCard(card) = item5 {
        assert_eq!(card.component_name, "Component");
    } else {
        panic!("Expected ClipboardXeroxCard");
    }

    // 6. /governor
    let item6 = CockpitVibeManager::handle_vibe_slash_command("/governor", "").expect("Must handle /governor");
    if let hgb_nextgen::CockpitItem::WattageGovernorCard(card) = item6 {
        assert!(card.power_mw > 0.0);
    } else {
        panic!("Expected WattageGovernorCard");
    }
}
