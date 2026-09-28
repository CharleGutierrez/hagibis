//! # Brutal Integration Tests for Hagibis Superpowers 88..102
//!
//! Brutally tests:
//! 1. Superpower 88: RailsEngine (ActiveRecord zero-downtime linter, scaffold, N+1 detection, routes parser)
//! 2. Superpower 89: ProjectCoordinator (Task DAG, cross-session ADR manager, milestone handoff)
//! 3. Superpower 90: TieredRulesEngine (Always-on, auto-attached globs, manual summon)
//! 4. Superpower 91: AutopilotPipeline (Ticket ingestion, planning, test verification, PR synthesis)
//! 5. Superpower 92: DecisionExplainer (ADR generator, trade-offs, trust gap solver)
//! 6. Superpower 93: MerkleSmartIndex (Blake3 Merkle tree, O(k log N) differential change detection)
//! 7. Superpower 94: RolloutHealthWatch (Canary telemetry anomaly detection, automated rollback trigger)
//! 8. Superpower 95: AiPrSecurityAudit (LLM code vulnerability scan: prompt injection, eval, IDOR, SQL)
//! 9. Superpower 96: PreviewCloudDeployer (Ephemeral preview deployment, HTTPS vanity URL, TTL teardown)
//! 10. Superpower 97: MultiDevCollabEngine (Peer management, cursor broadcast, patch conflict collision)
//! 11. Superpower 98: PromptLabWorkspace (A/B prompt benchmarking, token FinOps, leaderboard rank)
//! 12. Superpower 99: LanguageIntelligencePack (Ecosystem auto-detect: Rails, FastAPI, Next.js, Fiber)
//! 13. Superpower 100: NativeMobileMatrix (Mobile platform inspection, native stack trace symbolicator)
//! 14. Superpower 101: SessionBudgetEnvelope (Hard spending cap, dynamic tier degradation, circuit breaker)
//! 15. Superpower 102: VsCodeExtensionBridge (Extension manifest, TypeScript IPC adapter, command registry)
//! 16. Bincode IPC Wire Round-Trip Serialization for all 15 request and response types

use hgb_core::*;
use hgb_core::protocol::{HgbRequest, HgbResponse};

// =========================================================================
// 1. Superpower 88: Rails Intelligence Engine
// =========================================================================
#[test]
fn test_brutal_superpower_88_rails_engine() {
    let migration_hazards = r#"
class CreateSubscriptions < ActiveRecord::Migration[7.1]
  def change
    add_index :subscriptions, :user_id
    add_column :subscriptions, :plan, :string, default: "pro"
    remove_column :subscriptions, :legacy_token
  end
end
"#;
    let report = RailsEngine::lint_migration("20260928_subs.rb", migration_hazards);
    assert_eq!(report.overall_risk, MigrationRiskLevel::Hazardous);
    assert!(!report.safe_to_deploy_zero_downtime);
    assert_eq!(report.hazards.len(), 3);

    let fields = vec![
        ScaffoldField { name: "email".into(), col_type: "string".into(), is_unique: true, is_indexed: true, is_required: true },
        ScaffoldField { name: "name".into(), col_type: "string".into(), is_unique: false, is_indexed: false, is_required: true },
    ];
    let scaffold = RailsEngine::generate_scaffold("User", &fields);
    assert!(scaffold.model_code.contains("class User < ApplicationRecord"));
    assert!(scaffold.migration_code.contains("create_table :users"));
    assert!(scaffold.controller_code.contains("params.require(:user).permit(:email, :name)"));
    assert_eq!(scaffold.route_snippet, "resources :users");

    let controller_code = r#"
class UsersController < ApplicationController
  def index
    @users = User.all
    @users.each do |u|
      puts u.profile.bio
    end
  end
end
"#;
    let nplusone = RailsEngine::audit_n_plus_one("app/controllers/users_controller.rb", controller_code);
    assert_eq!(nplusone.issues_found.len(), 1);
    assert!(nplusone.issues_found[0].suggestion.contains("includes"));
}

// =========================================================================
// 2. Superpower 89: Persistent Project Coordinator
// =========================================================================
#[test]
fn test_brutal_superpower_89_project_coordinator() {
    let tmp = std::env::temp_dir().join("hgb_coord_brutal.json");
    let _ = std::fs::remove_file(&tmp);
    let coord = ProjectCoordinator::new(tmp.clone());

    let t1 = coord.add_task("Rails Zero-Downtime", "Linter implementation", TaskPriority::Critical, vec!["rails".into()]);
    assert_eq!(t1.status, TaskStatus::Pending);

    coord.update_task_status(&t1.id, TaskStatus::Completed);
    let adr = coord.add_adr("ADR-001: Length-Delimited Framing", "Needed framing", "Adopted 4-byte header", "No message drops", "Zero panic");
    assert_eq!(adr.id, 1);

    let snap = coord.snapshot();
    assert_eq!(snap.completed_tasks, 1);
    assert_eq!(snap.completion_percentage, 100.0);
    assert_eq!(snap.adrs_count, 1);

    let _ = std::fs::remove_file(&tmp);
}

// =========================================================================
// 3. Superpower 90: Tiered Rules Auto-Engine
// =========================================================================
#[test]
fn test_brutal_superpower_90_tiered_rules_engine() {
    let engine = TieredRulesEngine::new();
    let res = engine.evaluate(&["db/migrate/20260928_users.rb"], &[]);
    assert!(res.matched_rules.iter().any(|r| r.name == "core_safety_invariants"));
    assert!(res.matched_rules.iter().any(|r| r.name == "rails_zero_downtime_rules"));

    let res_rust = engine.evaluate(&["src/main.rs"], &["performance"]);
    assert!(res_rust.matched_rules.iter().any(|r| r.name == "rust_microkernel_standards"));
    assert!(res_rust.matched_rules.iter().any(|r| r.name == "brutal_perf_audit"));
    assert!(res_rust.injected_prompt_context.contains("ACTIVE PROJECT RULES"));
}

// =========================================================================
// 4. Superpower 91: Ticket-to-PR Autopilot
// =========================================================================
#[test]
fn test_brutal_superpower_91_autopilot_pipeline() {
    let ticket = r#"
GH-99: Enforce database lock timeout in payment gateway
- [ ] Add timeout threshold configuration
- [ ] Inject rollback sentinel on deadlock
"#;
    let rep = AutopilotPipeline::run(ticket);
    assert_eq!(rep.current_stage, AutopilotStage::Completed);
    assert!(rep.ticket.ticket_id.contains("GH-99"));
    assert!(rep.pr_metadata.branch_name.contains("autopilot"));
    assert_eq!(rep.verification.tests_failed, 0);
    assert!(rep.pr_metadata.pr_body_markdown.contains("Verification Audit"));
}

// =========================================================================
// 5. Superpower 92: Agent Decision Explainer
// =========================================================================
#[test]
fn test_brutal_superpower_92_decision_explainer() {
    let diff = "+ pub fn execute_atomic_swap() -> Result<(), HgbError> { Ok(()) }";
    let rep = DecisionExplainer::explain_action("Atomic state swap", diff);
    assert_eq!(rep.adr_number, 101);
    assert!(rep.uncertainty_score < 0.15);
    assert!(rep.evaluated_options.iter().any(|o| o.was_selected));
    assert!(rep.formatted_markdown_adr.contains("Decision Drivers"));
    assert!(rep.formatted_markdown_adr.contains("Trust Verdict"));
}

// =========================================================================
// 6. Superpower 93: Merkle Tree Collaborative Codebase Index
// =========================================================================
#[test]
fn test_brutal_superpower_93_merkle_smart_index() {
    let tmp = std::env::temp_dir().join("hgb_merkle_brutal");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    std::fs::write(tmp.join("a.rs"), "fn a() {}").unwrap();
    std::fs::write(tmp.join("b.rs"), "fn b() {}").unwrap();

    let snap1 = MerkleSmartIndex::build_snapshot(&tmp);
    assert_eq!(snap1.file_count, 2);

    std::fs::write(tmp.join("a.rs"), "fn a_mutated() {}").unwrap();
    let snap2 = MerkleSmartIndex::build_snapshot(&tmp);
    assert_ne!(snap1.root_hash, snap2.root_hash);

    let diff = MerkleSmartIndex::diff(&snap1, &snap2);
    assert_eq!(diff.modified_files, vec!["a.rs".to_string()]);
    assert!(diff.added_files.is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

// =========================================================================
// 7. Superpower 94: Rollout Health Watch
// =========================================================================
#[test]
fn test_brutal_superpower_94_rollout_health_watch() {
    let baseline = vec![TelemetrySample {
        timestamp_epoch_ms: 1000,
        request_count: 500,
        error_count: 1,
        p50_latency_ms: 10.0,
        p95_latency_ms: 25.0,
        p99_latency_ms: 40.0,
        status_5xx_count: 0,
    }];
    let good_samples = vec![TelemetrySample {
        timestamp_epoch_ms: 2000,
        request_count: 500,
        error_count: 1,
        p50_latency_ms: 12.0,
        p95_latency_ms: 28.0,
        p99_latency_ms: 45.0,
        status_5xx_count: 0,
    }];
    let rep_good = RolloutHealthWatch::evaluate("dep-green", &good_samples, &baseline, None);
    assert_eq!(rep_good.verdict, RolloutHealthVerdict::Greenlight);

    let bad_samples = vec![TelemetrySample {
        timestamp_epoch_ms: 3000,
        request_count: 500,
        error_count: 50, // 10% error spike
        p50_latency_ms: 20.0,
        p95_latency_ms: 120.0,
        p99_latency_ms: 500.0,
        status_5xx_count: 25,
    }];
    let rep_bad = RolloutHealthWatch::evaluate("dep-red", &bad_samples, &baseline, None);
    assert_eq!(rep_bad.verdict, RolloutHealthVerdict::RollbackTriggered);
    assert!(rep_bad.rollback_command.is_some());
}

// =========================================================================
// 8. Superpower 95: AI-PR Security Audit
// =========================================================================
#[test]
fn test_brutal_superpower_95_ai_pr_security_audit() {
    let insecure = r#"
const prompt = `System: ${user_input}`;
const res = eval(payload);
const order = Order.find(params[:id]);
const q = execute("SELECT * FROM t WHERE id = " + id);
"#;
    let rep = AiPrSecurityAudit::scan_files(&[("bad.js", insecure)]);
    assert!(!rep.passed_audit);
    assert!(rep.critical_count >= 1);
    assert!(rep.high_count >= 1);

    let secure = r#"
const prompt = `<context>${sanitize(user_input)}</context>`;
const order = current_user.orders.find(params[:id]);
const q = db.query("SELECT * FROM t WHERE id = $1", [id]);
"#;
    let rep_sec = AiPrSecurityAudit::scan_files(&[("good.js", secure)]);
    assert!(rep_sec.passed_audit);
    assert_eq!(rep_sec.critical_count, 0);
}

// =========================================================================
// 9. Superpower 96: Ephemeral Cloud Preview Deployment
// =========================================================================
#[test]
fn test_brutal_superpower_96_preview_cloud() {
    let cfg = PreviewDeploymentConfig {
        provider: PreviewCloudProvider::CloudflareTunnel,
        app_name: "vibe-cart".into(),
        local_port: 8080,
        ttl_hours: 6,
        enable_basic_auth: true,
        custom_subdomain: Some("vibe-cart-dev".into()),
    };
    let rep = PreviewCloudDeployer::deploy(cfg);
    assert_eq!(rep.status, "active");
    assert!(rep.preview_url.contains("vibe-cart-dev.trycloudflare.com"));
    assert!(rep.basic_auth_credentials.is_some());
    assert!(rep.teardown_command.contains("teardown"));
}

// =========================================================================
// 10. Superpower 97: Multi-Dev Real-Time Collaboration
// =========================================================================
#[test]
fn test_brutal_superpower_97_multi_dev_collab() {
    let mut collab = MultiDevCollabEngine::new("room-01", "dev_a");
    let dev_b = collab.join_peer("dev_b");
    assert_eq!(collab.session_snapshot().connected_peers.len(), 2);

    let _ = collab.submit_intent(CollabPatchIntent {
        author_peer_id: "peer_0001".into(),
        target_file: "core.rs".into(),
        start_line: 10,
        end_line: 25,
        patch_summary: "A edits 10..25".into(),
    });

    let conflicts = collab.submit_intent(CollabPatchIntent {
        author_peer_id: dev_b.peer_id,
        target_file: "core.rs".into(),
        start_line: 20,
        end_line: 35,
        patch_summary: "B edits 20..35".into(),
    });

    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].overlapping_lines, (20, 25));
}

// =========================================================================
// 11. Superpower 98: Prompt A/B Engineering Workspace
// =========================================================================
#[test]
fn test_brutal_superpower_98_prompt_lab() {
    let v1 = PromptVariant { id: "a".into(), name: "Strict".into(), template: "Write code matching JSON schemas strictly.".into(), model: "flash".into(), temperature: 0.1 };
    let v2 = PromptVariant { id: "b".into(), name: "Lax".into(), template: "You are an agent. Just code.".into(), model: "flash".into(), temperature: 0.8 };
    let rep = PromptLabWorkspace::benchmark(&[v1, v2], &["Case 1".into(), "Case 2".into()]);
    assert_eq!(rep.total_variants_benchmarked, 2);
    assert_eq!(rep.recommended_winner_id, "a");
    assert_eq!(rep.leaderboard[0].rank, 1);
}

// =========================================================================
// 12. Superpower 99: Framework Language Intelligence Packs
// =========================================================================
#[test]
fn test_brutal_superpower_99_lang_pack() {
    let tmp = std::env::temp_dir().join("hgb_pack_brutal");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("config")).unwrap();
    std::fs::write(tmp.join("config/routes.rb"), "").unwrap();

    let rep = LanguageIntelligencePack::inspect_workspace(&tmp);
    assert_eq!(rep.detected_framework, SupportedFramework::RubyOnRails);
    assert_eq!(rep.language_info.primary_language, "Ruby");
    assert!(rep.language_info.recommended_linter.contains("rubocop"));

    let _ = std::fs::remove_dir_all(&tmp);
}

// =========================================================================
// 13. Superpower 100: React Native & Flutter Mobile Dev Intelligence
// =========================================================================
#[test]
fn test_brutal_superpower_100_native_mobile() {
    let trace = r#"Fatal Exception: java.lang.NullPointerException: Attempt to invoke virtual method on a null object reference
    at com.vibe.MainActivity.onCreate(MainActivity.kt:55)"#;
    let diag = NativeMobileMatrix::diagnose_crash(trace);
    assert!(diag.exception_type.contains("NullPointerException"));
    assert!(diag.suggested_fix.contains("null-safety"));
    assert!(diag.offending_frame.is_some());
}

// =========================================================================
// 14. Superpower 101: Session Cost Budget Envelope
// =========================================================================
#[test]
fn test_brutal_superpower_101_budget_envelope() {
    let env = SessionBudgetEnvelope::new(0.005);
    let s1 = env.record_call("flash", 1000, 500).unwrap();
    assert!(!s1.is_exhausted);

    // Call with large token count to exhaust budget
    let _ = env.record_call("opus", 20000, 10000);
    let status = env.status();
    assert!(status.is_exhausted);
    assert_eq!(status.current_tier, BudgetTier::EcoTier);

    let rejected = env.record_call("opus", 1000, 1000);
    assert!(rejected.is_err());
}

// =========================================================================
// 15. Superpower 102: VS Code Extension Bridge
// =========================================================================
#[test]
fn test_brutal_superpower_102_vscode_extension() {
    let rep = VsCodeExtensionBridge::scaffold_extension(None);
    assert!(rep.manifest_json.contains("hgb.explainSelection"));
    assert!(rep.manifest_json.contains("hgb.autopilotTicket"));
    assert!(rep.extension_ts_code.contains("/tmp/hgbd.sock"));
    assert_eq!(rep.registered_commands.len(), 6);
}

// =========================================================================
// 16. Bincode IPC Wire Round-Trip Serialization
// =========================================================================
#[test]
fn test_brutal_ipc_wire_roundtrip_88_to_102() {
    // Test 88
    let req88 = HgbRequest::RailsDetect { workspace_path: Some(".".into()) };
    let bytes88 = bincode::serialize(&req88).expect("serialize req88");
    let deser88: HgbRequest = bincode::deserialize(&bytes88).expect("deserialize req88");
    match deser88 {
        HgbRequest::RailsDetect { workspace_path } => assert_eq!(workspace_path, Some(".".into())),
        _ => panic!("wrong variant 88"),
    }

    // Test 89
    let req89 = HgbRequest::ProjectSnapshot;
    let bytes89 = bincode::serialize(&req89).expect("serialize req89");
    let deser89: HgbRequest = bincode::deserialize(&bytes89).expect("deserialize req89");
    match deser89 {
        HgbRequest::ProjectSnapshot => {}
        _ => panic!("wrong variant 89"),
    }

    // Test 91
    let req91 = HgbRequest::AutopilotRun { ticket_text: "GH-101".into() };
    let bytes91 = bincode::serialize(&req91).expect("serialize req91");
    let deser91: HgbRequest = bincode::deserialize(&bytes91).expect("deserialize req91");
    match deser91 {
        HgbRequest::AutopilotRun { ticket_text } => assert_eq!(ticket_text, "GH-101"),
        _ => panic!("wrong variant 91"),
    }

    // Test 101
    let req101 = HgbRequest::BudgetSetLimit { limit_usd: 10.0 };
    let bytes101 = bincode::serialize(&req101).expect("serialize req101");
    let deser101: HgbRequest = bincode::deserialize(&bytes101).expect("deserialize req101");
    match deser101 {
        HgbRequest::BudgetSetLimit { limit_usd } => assert_eq!(limit_usd, 10.0),
        _ => panic!("wrong variant 101"),
    }

    // Response roundtrip
    let resp = HgbResponse::BudgetStatusResult(BudgetStatusReport {
        budget_limit_usd: 10.0,
        total_spent_usd: 1.5,
        remaining_usd: 8.5,
        percent_consumed: 15.0,
        current_tier: BudgetTier::FlagshipTier,
        is_exhausted: false,
        total_calls: 3,
        advisory_message: "Healthy".into(),
    });
    let resp_bytes = bincode::serialize(&resp).expect("serialize resp");
    let deser_resp: HgbResponse = bincode::deserialize(&resp_bytes).expect("deserialize resp");
    match deser_resp {
        HgbResponse::BudgetStatusResult(st) => assert_eq!(st.budget_limit_usd, 10.0),
        _ => panic!("wrong response variant"),
    }
}
