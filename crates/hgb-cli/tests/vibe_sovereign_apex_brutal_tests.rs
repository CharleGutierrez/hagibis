//! # Tier 9 Vibe Sovereign Apex Frontier Dimensions Brutal Integration Tests
//!
//! Brutal end-to-end integration and stress tests for:
//! 1. AmbientPredictor (Windsurf Cascade & Supermaven Next-Edit Anticipator)
//! 2. CdpTweakMirror (Bolt.new & Devin Bidirectional DevTools Click-to-Source Mirror)
//! 3. PromptModeDocsHarvester (Continue.dev & Roo Code Composable Modes & Live Docs Harvester)
//! 4. EphemeralStackSandbox (Replit Agent & WebContainers Zero-Config In-Memory Stack Sandbox)
//! 5. AntiPlaceboGatekeeper (Qodo & Meta SapFix Behavioral Mutation Gatekeeper)
//! 6. Full Daemon IPC Roundtrip across all Tier 9 Subsystems

use hgb_core::ambient_predictor::{AmbientPredictor, EditKind};
use hgb_core::anti_placebo_gatekeeper::{AntiPlaceboGatekeeper, MutationKind};
use hgb_core::cdp_tweak_mirror::{CdpTweakMirror, DomTweakEvent};
use hgb_core::ephemeral_stack_sandbox::EphemeralStackSandbox;
use hgb_core::live_graph_watcher::LiveGraphWatcher;
use hgb_core::prompt_mode_docs_harvester::{PromptModeDocsHarvester, VibePromptMode};
use hgb_core::protocol::{HgbRequest, HgbResponse};
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

// =========================================================================
// 1. AMBIENT PREDICTOR (Cascade Next-Action Anticipation)
// =========================================================================
#[test]
fn test_brutal_ambient_predictor() {
    let tmp = create_temp_test_dir("ambient_pred");
    let src_dir = tmp.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    // 1. Provider file defining a function
    let provider_rs = r#"
        pub struct UserAccount {
            pub id: u64,
            pub email: String,
        }

        pub fn authenticate_user(account: &UserAccount) -> bool {
            !account.email.is_empty()
        }
    "#;
    fs::write(src_dir.join("auth.rs"), provider_rs).expect("write auth.rs");

    // 2. Consumer file calling the function
    let consumer_rs = r#"
        use crate::auth::{authenticate_user, UserAccount};

        pub async fn handle_request() {
            let acc = UserAccount { id: 1, email: "user@example.com".into() };
            let valid = authenticate_user(&acc);
            println!("Valid: {}", valid);
        }
    "#;
    fs::write(src_dir.join("routes.rs"), consumer_rs).expect("write routes.rs");

    let mut watcher = LiveGraphWatcher::new(&tmp);
    let summary = watcher.sync_workspace(&["rs"]).expect("sync workspace");
    assert_eq!(summary.total_files_tracked, 2);

    let mut predictor = AmbientPredictor::new(10);
    let event = AmbientPredictor::create_event(
        "src/auth.rs",
        "authenticate_user",
        EditKind::SignatureModified,
        Some("authenticate_user(&acc)".to_string()),
        Some("authenticate_user(&acc, true)".to_string()),
    );
    predictor.record_edit(event.clone());
    assert_eq!(predictor.history.len(), 1);

    // Anticipate cascade edits triggered by the modification
    let report = predictor.predict_next_edits(&event, &watcher);
    assert_eq!(report.trigger_symbol, "authenticate_user");
    assert_eq!(report.trigger_file, "src/auth.rs");
    assert!(report.call_sites_analyzed >= 1, "Expected call site analysis");
    assert!(!report.predictions.is_empty(), "Expected next-edit prediction");

    let p = &report.predictions[0];
    assert!(p.target_file.contains("routes.rs"));
    assert!(p.confidence_score >= 80);
    assert!(p.suggested_diff.contains("authenticate_user"));
    assert!(p.rationale.contains("directly depends on modified symbol"));

    let _ = fs::remove_dir_all(&tmp);
}

// =========================================================================
// 2. CDP TWEAK MIRROR (Bidirectional DevTools to Source Sync)
// =========================================================================
#[test]
fn test_brutal_cdp_tweak_mirror() {
    let tmp = create_temp_test_dir("cdp_mirror");
    let comp_dir = tmp.join("src").join("components");
    fs::create_dir_all(&comp_dir).expect("create components dir");

    let button_tsx = r#"import React from 'react';

export function ActionButton() {
    return (
        <button className="px-4 py-2 bg-indigo-600 text-white rounded-lg shadow">
            Confirm Payment
        </button>
    );
}
"#;
    let file_path = comp_dir.join("ActionButton.tsx");
    fs::write(&file_path, button_tsx).expect("write ActionButton.tsx");

    let mirror = CdpTweakMirror::new(&tmp);
    let event = DomTweakEvent {
        selector: "button.rounded-lg".to_string(),
        property_or_attr: "className".to_string(),
        old_value: "bg-indigo-600".to_string(),
        new_value: "bg-emerald-600".to_string(),
        component_hint: Some("ActionButton".to_string()),
        file_hint: Some("src/components/ActionButton.tsx".to_string()),
    };

    // 1. Dry run verification
    let dry_run = mirror.sync_tweak(&event, false).expect("dry run tweak");
    assert!(dry_run.success);
    assert!(!dry_run.file_written);
    assert_eq!(dry_run.matched_line, 5);
    assert!(dry_run.new_line.contains("bg-emerald-600"));
    assert!(dry_run.diff_applied.contains("- <button className=\"px-4 py-2 bg-indigo-600 text-white rounded-lg shadow\">"));
    assert!(dry_run.diff_applied.contains("+ <button className=\"px-4 py-2 bg-emerald-600 text-white rounded-lg shadow\">"));

    // Verify disk was NOT altered during dry run
    let content_dry = fs::read_to_string(&file_path).expect("read file");
    assert!(content_dry.contains("bg-indigo-600"));

    // 2. Live apply verification
    let live_run = mirror.sync_tweak(&event, true).expect("live apply tweak");
    assert!(live_run.success);
    assert!(live_run.file_written);
    assert!(!live_run.blake3_hash.is_empty());

    // Verify disk WAS altered with Blake3 validation
    let content_live = fs::read_to_string(&file_path).expect("read file");
    assert!(content_live.contains("bg-emerald-600"));
    assert!(!content_live.contains("bg-indigo-600"));

    let _ = fs::remove_dir_all(&tmp);
}

// =========================================================================
// 3. PROMPT MODE DOCS HARVESTER (Composable Modes & Live Docs Slicer)
// =========================================================================
#[test]
fn test_brutal_prompt_mode_docs_harvester() {
    // 1. Verify Mode System Directives
    let modes = [
        VibePromptMode::Architect,
        VibePromptMode::CodeSprint,
        VibePromptMode::DebugTriage,
        VibePromptMode::SecurityAudit,
        VibePromptMode::DocReview,
    ];
    for m in modes {
        assert!(!m.system_directive().is_empty());
        assert!(!m.label().is_empty());
    }

    // 2. Harvest raw documentation
    let raw_docs = r#"# Axum Framework Routing & State Extraction
The Axum routing engine provides type-safe state injection and extractors.

### Basic Handler Definition
pub async fn handle_health() -> &'static str {
    "OK"
}

pub fn create_router() -> Router {
    Router::new().route("/health", get(handle_health))
}

```rust
use axum::{routing::get, Router};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let app = Router::new().route("/", get(|| async { "Hello World" }));
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    axum::Server::bind(&addr).serve(app.into_make_service()).await.unwrap();
}
```

Detailed architectural commentary follows for another 5,000 words...
"#;

    let harvested = PromptModeDocsHarvester::harvest_content("axum_docs", raw_docs);
    assert_eq!(harvested.source_target, "axum_docs");
    assert!(harvested.extracted_title.contains("Axum Framework"));
    assert!(harvested.signatures.iter().any(|s| s.contains("pub async fn handle_health")));
    assert!(harvested.signatures.iter().any(|s| s.contains("pub fn create_router")));
    assert_eq!(harvested.code_examples.len(), 1);
    assert!(harvested.condensed_tokens_estimate < harvested.raw_tokens_estimate);
    assert!(harvested.compression_ratio >= 1.0);

    // 3. Assemble complete augmented prompt
    let report = PromptModeDocsHarvester::assemble_prompt(
        VibePromptMode::CodeSprint,
        "Implement graceful shutdown for axum server",
        vec![harvested],
    );
    assert_eq!(report.active_mode, VibePromptMode::CodeSprint);
    assert!(report.augmented_prompt.contains("[SYSTEM MODE: CODE_SPRINT]"));
    assert!(report.augmented_prompt.contains("Axum Framework Routing"));
    assert!(report.augmented_prompt.contains("Implement graceful shutdown for axum server"));
}

// =========================================================================
// 4. EPHEMERAL STACK SANDBOX (Zero-Config In-Memory Stack & SQLite Seeding)
// =========================================================================
#[test]
fn test_brutal_ephemeral_stack_sandbox() {
    // 1. Port allocation
    let port = EphemeralStackSandbox::allocate_ephemeral_port().expect("allocate port");
    assert!(port > 1024);

    // 2. Spin up stack sandbox
    let report = EphemeralStackSandbox::spin_up(
        "actix_sqlite_stack",
        &["orders", "payments", "shipments"],
    ).expect("spin up sandbox");

    assert_eq!(report.status, "ONLINE_READY");
    assert_eq!(report.session.stack_name, "actix_sqlite_stack");
    assert!(report.session.primary_port > 0);
    assert_eq!(report.session.seeded_records_count, 6); // 3 tables * 2 records
    assert!(report.session.db_uri.starts_with("sqlite://"));
    assert!(report.health_url.contains(&report.session.primary_port.to_string()));

    // Verify SQLite file was created and can be queried directly
    let db_path = report.session.db_uri.trim_start_matches("sqlite://");
    assert!(std::path::Path::new(db_path).exists());

    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM orders", [], |row| row.get(0))
        .expect("count orders");
    assert_eq!(count, 2);

    // 3. Teardown
    let cleaned = EphemeralStackSandbox::teardown(&report.session.session_id);
    assert!(cleaned);
    assert!(!std::path::Path::new(db_path).exists());
}

// =========================================================================
// 5. ANTI-PLACEBO GATEKEEPER (Behavioral Mutation Testing Oracle)
// =========================================================================
#[test]
fn test_brutal_anti_placebo_gatekeeper() {
    let source_code = r#"
        pub fn authorize_transaction(amount: u64, is_verified: bool) -> bool {
            if amount < 1000 && is_verified == true {
                return true;
            }
            false
        }
    "#;

    // 1. Mutant synthesis verification
    let mutants = AntiPlaceboGatekeeper::generate_mutants(source_code);
    assert!(mutants.len() >= 3, "Expected at least 3 mutants generated, got {}", mutants.len());
    assert!(mutants.iter().any(|m| m.kind == MutationKind::BoundaryShift));
    assert!(mutants.iter().any(|m| m.kind == MutationKind::ConditionInversion));
    assert!(mutants.iter().any(|m| m.kind == MutationKind::LogicalFlip));

    // 2. Placebo test detection (trivial assertion)
    let placebo_test = r#"
        #[test]
        fn test_placebo_always_passes() {
            assert!(true);
        }
    "#;
    let audit_placebo = AntiPlaceboGatekeeper::audit_tests(source_code, placebo_test);
    assert_eq!(audit_placebo.mutation_score_pct, 0.0);
    assert_eq!(audit_placebo.mutants_survived, mutants.len());
    assert!(!audit_placebo.is_production_ready);
    assert!(!audit_placebo.placebo_tests_detected.is_empty());
    assert!(audit_placebo.placebo_tests_detected[0].contains("trivial or missing assertions"));

    // 3. Production-ready test suite (actively catches mutants)
    let robust_test = r#"
        #[test]
        fn test_authorization_coverage() {
            assert!(authorize_transaction(500, true));
            assert!(!authorize_transaction(1500, true));
            assert!(!authorize_transaction(500, false));
            let amount = 500;
            let is_verified = true;
            assert_eq!(authorize_transaction(amount, is_verified), true);
        }
    "#;
    let audit_robust = AntiPlaceboGatekeeper::audit_tests(source_code, robust_test);
    assert!(audit_robust.mutants_killed > 0);
    assert!(audit_robust.mutation_score_pct > 0.0);
}

// =========================================================================
// 6. FULL DAEMON IPC ROUNDTRIP ACROSS ALL TIER 9 SUBSYSTEMS
// =========================================================================
#[tokio::test]
async fn test_brutal_tier9_daemon_ipc_roundtrip() {
    let tmp = create_temp_test_dir("daemon_tier9");
    let state = Arc::new(DaemonState::new(tmp.join("daemon.sock")));

    // 1. AmbientPredict IPC
    let req = HgbRequest::AmbientPredict {
        file_path: "src/billing.rs".to_string(),
        symbol_name: "charge_card".to_string(),
        change_kind: EditKind::SignatureModified,
        old_snippet: None,
        new_snippet: None,
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::AmbientPredictResult(report) => {
            assert_eq!(report.trigger_symbol, "charge_card");
            assert_eq!(report.trigger_file, "src/billing.rs");
        }
        _ => panic!("Expected AmbientPredictResult"),
    }

    // 2. CdpTweakSync IPC (live file sync roundtrip)
    let target_tweak_file = PathBuf::from("test_tweak_card.tsx");
    fs::write(&target_tweak_file, "<button className=\"btn bg-blue-500\">Click</button>\n").expect("write test file");

    let req = HgbRequest::CdpTweakSync {
        event: DomTweakEvent {
            selector: ".btn".to_string(),
            property_or_attr: "className".to_string(),
            old_value: "bg-blue-500".to_string(),
            new_value: "bg-emerald-500".to_string(),
            component_hint: None,
            file_hint: Some("test_tweak_card.tsx".to_string()),
        },
        apply_to_disk: true,
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::CdpTweakSyncResult(report) => {
            assert!(report.success);
            assert!(report.file_written);
            assert_eq!(report.matched_line, 1);
            let updated = fs::read_to_string(&target_tweak_file).expect("read updated file");
            assert!(updated.contains("bg-emerald-500"));
            assert!(!report.blake3_hash.is_empty());
        }
        _ => panic!("Expected CdpTweakSyncResult"),
    }
    let _ = fs::remove_file(target_tweak_file);

    // 3. PromptModeHarvest IPC
    let req = HgbRequest::PromptModeHarvest {
        mode: VibePromptMode::SecurityAudit,
        user_prompt: "Check for SQL injection and auth bypass".to_string(),
        doc_targets: vec!["db_api".to_string()],
        raw_doc_content: Some("# Database Security API\npub fn sanitize_input(sql: &str) -> String;\n```rust\nassert_eq!(sanitize_input(\"' OR 1=1\"), \"\");\n```\n".to_string()),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::PromptModeHarvestResult(report) => {
            assert_eq!(report.active_mode, VibePromptMode::SecurityAudit);
            assert_eq!(report.harvested_docs.len(), 1);
            assert!(report.augmented_prompt.contains("[SYSTEM MODE: SECURITY_AUDIT]"));
        }
        _ => panic!("Expected PromptModeHarvestResult"),
    }

    // 4. EphemeralSandboxSpinUp IPC
    let req = HgbRequest::EphemeralSandboxSpinUp {
        stack_name: "tokio_inmemory_stack".to_string(),
        tables_to_seed: vec!["accounts".to_string(), "logs".to_string()],
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::EphemeralSandboxResult(report) => {
            assert_eq!(report.status, "ONLINE_READY");
            assert_eq!(report.session.seeded_records_count, 4);
            // Clean up sandbox
            let _ = EphemeralStackSandbox::teardown(&report.session.session_id);
        }
        _ => panic!("Expected EphemeralSandboxResult"),
    }

    // 5. AntiPlaceboAudit IPC
    let req = HgbRequest::AntiPlaceboAudit {
        source_code: "pub fn check_gate(val: i32) -> bool { val > 10 }".to_string(),
        test_code: "#[test] fn test_gate() { assert!(check_gate(15)); assert!(!check_gate(5)); }".to_string(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::AntiPlaceboAuditResult(report) => {
            assert!(report.total_mutants_generated > 0);
            assert!(report.mutants_killed > 0);
            assert!(report.mutation_score_pct > 0.0);
        }
        _ => panic!("Expected AntiPlaceboAuditResult"),
    }

    let _ = fs::remove_dir_all(&tmp);
}
