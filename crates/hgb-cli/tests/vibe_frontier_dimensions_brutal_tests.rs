//! # Tier 8 Competitor Frontier Dimensions Brutal Integration Tests
//!
//! Brutal end-to-end integration and stress tests for:
//! 1. LiveGraphWatcher (Augment Code-Style In-Memory Codebase Index)
//! 2. ShellPanicHook (Warp Terminal-Style Interactive Shell Panic & 1-Key Auto-Repair)
//! 3. SpecDecomposer (Copilot Workspace-Style Spec -> Plan -> Diff Task Decomposer)
//! 4. DynamicAtContext (Continue.dev & Roo Code-Style Dynamic @Context Expander)
//! 5. VisualRegressionSentry (Devin & Replit-Style Visual Layout Regression Oracle)
//! 6. ContinuousFlakyWatchdog (Meta SapFix & Qodo-Style Autonomous Healing Loop)
//! 7. Full Daemon IPC Roundtrip across all Tier 8 Subsystems

use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_core::{
    ContinuousFlakyWatchdog, DynamicAtContext, LiveGraphWatcher, ShellFailureCategory,
    ShellIncident, ShellPanicHook, SpecDecomposer, StepStatus, VisualDeltaType,
    VisualNodeSnapshot, VisualRegressionSentry,
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
    let dir = std::env::temp_dir().join(format!("hgb_frontier_test_{}_{}", name, nanos));
    fs::create_dir_all(&dir).expect("create test dir");
    dir
}

// =========================================================================
// 1. LIVE GRAPH WATCHER
// =========================================================================
#[test]
fn test_brutal_live_graph_watcher() {
    let tmp = create_temp_test_dir("live_graph");
    let src_dir = tmp.join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");

    let auth_rs = r#"
        pub struct AuthToken {
            pub raw: String,
        }

        pub fn verify_signature() -> bool {
            true
        }
    "#;
    fs::write(src_dir.join("auth.rs"), auth_rs).expect("write auth.rs");

    let server_rs = r#"
        use crate::auth::AuthToken;

        pub async fn handle_request() {
            let _token = AuthToken { raw: "test".into() };
        }
    "#;
    fs::write(src_dir.join("server.rs"), server_rs).expect("write server.rs");

    let mut watcher = LiveGraphWatcher::new(&tmp);
    let summary = watcher.sync_workspace(&["rs"]).expect("sync workspace");

    assert_eq!(summary.total_files_tracked, 2);
    assert!(summary.total_symbols_indexed >= 3, "Expected at least 3 symbols indexed");

    // Test callers & dependents
    let callers = watcher.find_callers("AuthToken");
    assert!(!callers.is_empty(), "server.rs should be identified as caller of AuthToken");

    let deps = watcher.find_dependents("src/auth.rs");
    assert!(!deps.is_empty(), "server.rs should be identified as dependent of auth.rs");

    let _ = fs::remove_dir_all(&tmp);
}

// =========================================================================
// 2. SHELL PANIC HOOK
// =========================================================================
#[test]
fn test_brutal_shell_panic_hook() {
    // 1. Port conflict EADDRINUSE
    let port_incident = ShellIncident {
        command: "npm run dev".to_string(),
        exit_code: 1,
        stderr: "Error: listen EADDRINUSE: address already in use :::3000".to_string(),
        working_dir: "/home/user/app".to_string(),
    };
    let diag = ShellPanicHook::diagnose(&port_incident);
    assert_eq!(diag.category, ShellFailureCategory::PortConflict);
    assert!(diag.suggested_fix_command.contains("3000"));
    assert_eq!(diag.confidence_score, 95);

    // 2. Git non-fast-forward divergence
    let git_incident = ShellIncident {
        command: "git push origin main".to_string(),
        exit_code: 1,
        stderr: "! [rejected] main -> main (non-fast-forward) fetch first".to_string(),
        working_dir: "/home/user/app".to_string(),
    };
    let diag_git = ShellPanicHook::diagnose(&git_incident);
    assert_eq!(diag_git.category, ShellFailureCategory::GitRemoteDivergence);
    assert!(diag_git.suggested_fix_command.contains("git pull --rebase"));

    // 3. Compiler error
    let cargo_incident = ShellIncident {
        command: "cargo check".to_string(),
        exit_code: 101,
        stderr: "error[E0425]: cannot find value `foo` in this scope\n --> src/main.rs:10:5".to_string(),
        working_dir: "/home/user/app".to_string(),
    };
    let diag_cargo = ShellPanicHook::diagnose(&cargo_incident);
    assert_eq!(diag_cargo.category, ShellFailureCategory::CompilationError);
    assert!(diag_cargo.suggested_fix_command.contains("hgb preflight"));

    // 4. Test shell companion hook generation
    let bash_hook = ShellPanicHook::generate_shell_hook("bash");
    assert!(bash_hook.contains("hgb shell-panic"));

    let zsh_hook = ShellPanicHook::generate_shell_hook("zsh");
    assert!(zsh_hook.contains("add-zsh-hook"));

    let fish_hook = ShellPanicHook::generate_shell_hook("fish");
    assert!(fish_hook.contains("fish_postexec"));
}

// =========================================================================
// 3. SPEC DECOMPOSER
// =========================================================================
#[test]
fn test_brutal_spec_decomposer() {
    let workspace_files = vec![
        "crates/core/src/auth.rs".to_string(),
        "crates/api/src/routes.rs".to_string(),
        "src/main.rs".to_string(),
    ];

    let mut report = SpecDecomposer::decompose(
        "Implement biometric WebAuthn passkey authentication gate",
        &workspace_files,
    );

    assert_eq!(report.total_steps, 4);
    assert!(report.architecture_spec.contains("WebAuthn"));
    assert_eq!(report.steps[0].status, StepStatus::Completed);
    assert_eq!(report.steps[1].status, StepStatus::InProgress);

    // Advance step 2
    SpecDecomposer::advance_step(&mut report, 2);
    assert_eq!(report.steps[1].status, StepStatus::Completed);
    assert_eq!(report.steps[2].status, StepStatus::InProgress);
    assert_eq!(report.completed_steps, 2);
    assert_eq!(report.progress_percent, 50);

    let ascii = SpecDecomposer::format_ascii(&report);
    assert!(ascii.contains("COPILOT WORKSPACE-STYLE SPEC DECOMPOSITION"));
}

// =========================================================================
// 4. DYNAMIC @CONTEXT EXPANDER
// =========================================================================
#[test]
fn test_brutal_dynamic_at_context() {
    let tmp = create_temp_test_dir("at_context");
    let sample_file = tmp.join("config.toml");
    fs::write(&sample_file, "title = 'Hagibis Vibe Engine'\nversion = '1.0'\n").expect("write file");

    let prompt = "Explain @file:config.toml and check @env";
    let res = DynamicAtContext::expand(&tmp, prompt);

    assert_eq!(res.attachments.len(), 2);
    assert!(res.expanded_prompt.contains("ATTACHED DYNAMIC CONTEXT"));
    assert!(res.expanded_prompt.contains("Hagibis Vibe Engine"));
    assert!(res.expanded_prompt.contains("Target OS: linux"));
    assert!(res.total_injected_tokens > 0);

    let _ = fs::remove_dir_all(&tmp);
}

// =========================================================================
// 5. VISUAL REGRESSION SENTRY
// =========================================================================
#[test]
fn test_brutal_visual_regression_sentry() {
    let baseline = vec![
        VisualNodeSnapshot {
            tag: "header".to_string(),
            id: Some("topbar".to_string()),
            classes: vec!["bar".to_string()],
            x: 0.0, y: 0.0, width: 1024.0, height: 60.0,
            text_preview: Some("Hagibis Cockpit".to_string()),
        },
        VisualNodeSnapshot {
            tag: "button".to_string(),
            id: Some("deploy-btn".to_string()),
            classes: vec!["btn".to_string()],
            x: 800.0, y: 15.0, width: 120.0, height: 35.0,
            text_preview: Some("Deploy".to_string()),
        },
    ];

    // Current has button shifted drastically by 70px (breaking layout shift)
    let current_shifted = vec![
        VisualNodeSnapshot {
            tag: "header".to_string(),
            id: Some("topbar".to_string()),
            classes: vec!["bar".to_string()],
            x: 0.0, y: 0.0, width: 1024.0, height: 60.0,
            text_preview: Some("Hagibis Cockpit".to_string()),
        },
        VisualNodeSnapshot {
            tag: "button".to_string(),
            id: Some("deploy-btn".to_string()),
            classes: vec!["btn".to_string()],
            x: 800.0, y: 85.0, width: 120.0, height: 35.0, // dy = +70px
            text_preview: Some("Deploy".to_string()),
        },
    ];

    let rep = VisualRegressionSentry::compare(&baseline, &current_shifted);
    assert_eq!(rep.total_deltas, 1);
    assert_eq!(rep.breaking_shifts, 1);
    assert!(!rep.is_visually_stable);
    assert!(rep.visual_stability_score < 80.0);
    assert!(matches!(rep.deltas[0].delta_type, VisualDeltaType::LayoutShift { .. }));

    // Test clean stable state (tiny 2px shift)
    let current_stable = vec![
        VisualNodeSnapshot {
            tag: "header".to_string(),
            id: Some("topbar".to_string()),
            classes: vec!["bar".to_string()],
            x: 0.0, y: 0.0, width: 1024.0, height: 60.0,
            text_preview: Some("Hagibis Cockpit".to_string()),
        },
        VisualNodeSnapshot {
            tag: "button".to_string(),
            id: Some("deploy-btn".to_string()),
            classes: vec!["btn".to_string()],
            x: 800.0, y: 16.0, width: 120.0, height: 35.0, // dy = +1px (within tolerance)
            text_preview: Some("Deploy".to_string()),
        },
    ];

    let rep_stable = VisualRegressionSentry::compare(&baseline, &current_stable);
    assert_eq!(rep_stable.breaking_shifts, 0);
    assert!(rep_stable.is_visually_stable);
    assert_eq!(rep_stable.visual_stability_score, 100.0);
}

// =========================================================================
// 6. CONTINUOUS FLAKY WATCHDOG
// =========================================================================
#[test]
fn test_brutal_continuous_flaky_watchdog() {
    let compiler_errors = vec!["src/billing.rs:45: error: mismatched types".to_string()];
    let flaky_tests = vec!["test_stripe_webhook_race".to_string()];

    let rep = ContinuousFlakyWatchdog::inspect_and_heal(&compiler_errors, &flaky_tests);

    assert_eq!(rep.regressions_detected, 2);
    assert_eq!(rep.autonomous_fixes_ready, 2);
    assert!(rep.workspace_health_status.contains("HEALING_ACTIVE"));
    assert!(rep.actions.iter().any(|a| a.target_file == "src/billing.rs"));
    assert!(rep.actions.iter().any(|a| a.target_file.contains("test_stripe_webhook_race")));
}

// =========================================================================
// 7. DAEMON IPC END-TO-END ROUNDTRIP ACROSS ALL TIER 8 SUBSYSTEMS
// =========================================================================
#[tokio::test]
async fn test_brutal_tier8_daemon_ipc_roundtrip() {
    let tmp = create_temp_test_dir("ipc_tier8");
    let sock = tmp.join("hgb_tier8_test.sock");
    let state = Arc::new(DaemonState::new(sock));

    // 1. LiveGraphSync IPC
    let req = HgbRequest::LiveGraphSync {
        extensions: vec!["rs".to_string()],
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::LiveGraphSyncResult(summary) => {
            assert!(summary.total_files_tracked > 0);
            let _ = summary.sync_duration_us;
        }
        _ => panic!("Expected LiveGraphSyncResult"),
    }

    // 2. ShellPanicDiagnose IPC
    let req = HgbRequest::ShellPanicDiagnose {
        command: "npm start".to_string(),
        exit_code: 1,
        stderr: "listen EADDRINUSE: address already in use :::8080".to_string(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::ShellPanicDiagnosisResult(diag) => {
            assert_eq!(diag.category, ShellFailureCategory::PortConflict);
            assert!(diag.suggested_fix_command.contains("8080"));
        }
        _ => panic!("Expected ShellPanicDiagnosisResult"),
    }

    // 3. SpecDecompose IPC
    let req = HgbRequest::SpecDecompose {
        intent: "Setup Redis session token revoker".to_string(),
        workspace_files: vec!["src/main.rs".to_string()],
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::SpecDecomposeResult(report) => {
            assert_eq!(report.total_steps, 4);
            assert!(report.architecture_spec.contains("Redis"));
        }
        _ => panic!("Expected SpecDecomposeResult"),
    }

    // 4. DynamicContextExpand IPC
    let req = HgbRequest::DynamicContextExpand {
        prompt: "Review @env settings".to_string(),
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::DynamicContextExpandResult(res) => {
            assert!(!res.attachments.is_empty());
            assert!(res.expanded_prompt.contains("Target OS: linux"));
        }
        _ => panic!("Expected DynamicContextExpandResult"),
    }

    // 5. VisualRegressionAudit IPC
    let req = HgbRequest::VisualRegressionAudit {
        baseline_nodes: vec![VisualNodeSnapshot {
            tag: "div".to_string(),
            id: Some("hero".to_string()),
            classes: vec!["hero".to_string()],
            x: 0.0, y: 0.0, width: 500.0, height: 300.0,
            text_preview: None,
        }],
        current_nodes: vec![VisualNodeSnapshot {
            tag: "div".to_string(),
            id: Some("hero".to_string()),
            classes: vec!["hero".to_string()],
            x: 0.0, y: 0.0, width: 500.0, height: 300.0,
            text_preview: None,
        }],
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::VisualRegressionResult(rep) => {
            assert!(rep.is_visually_stable);
            assert_eq!(rep.breaking_shifts, 0);
        }
        _ => panic!("Expected VisualRegressionResult"),
    }

    // 6. ContinuousHealWatch IPC
    let req = HgbRequest::ContinuousHealWatch {
        workspace_errors: vec!["error[E0425]: cannot find `token`".to_string()],
        flaky_tests: vec!["test_webhook_ack".to_string()],
    };
    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::ContinuousHealResult(rep) => {
            assert_eq!(rep.regressions_detected, 2);
            assert_eq!(rep.autonomous_fixes_ready, 2);
        }
        _ => panic!("Expected ContinuousHealResult"),
    }

    let _ = fs::remove_dir_all(&tmp);
}
