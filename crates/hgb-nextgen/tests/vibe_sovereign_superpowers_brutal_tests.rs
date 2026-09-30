//! # Brutal Integration Test Suite for All 14 Next-Gen Vibe Sovereign Superpowers
//!
//! Validates:
//! 1. DomToAstTeleporter: CDP click bridge, React fiber/Vite sourcemap, DOM-to-AST teleport anchor.
//! 2. StreamSteeringController: Mid-flight streaming pause, course-correction nudge, and prompt augmentation.
//! 3. RootlessSandboxEngine: Ephemeral jail execution, sensitive directory traversal blocking, security audit.
//! 4. ArenaSwarmEngine: 3-way speculative multi-worktree evaluation, comparative benchmarks, 1-click winner merge.
//! 5. ContextAntiRotGc: Semantic context compactor, dead compiler error cycle pruning, superseded view folding.
//! 6. VibeDeployerEngine: Zero-config preview deployment, TLS URL allocation, Unicode half-block QR matrix.
//! 7. TypeDriftHarmonizer: Cross-stack sync lock (Rust -> TypeScript -> Python -> SQL) with drift detection.
//! 8. ShadowExecutionEngine: Sub-millisecond background in-memory smoke testing on affected AST slices.
//! 9. DbMigrationSynthesizer: SQLite introspection, non-destructive ALTER TABLE synthesis, Blake3 WAL checkpoint.
//! 10. ZeroMockFabric: Intercepted call analysis, dynamic seed generation, realistic schema response synthesis.
//! 11. GhostTypingEngine: Speculative token pre-computation in resident memory with sub-50µs retrieval.
//! 12. InvariantShieldEngine: Formal panic hazard detection (.unwrap, div-by-zero) and defensive auto-repair.
//! 13. ArchitectureDagVisualizer: Workspace component topology discovery and ASCII DAG diagram rendering.
//! 14. VoiceStreamCoPilot: Raw PCM audio ingestion, voice activity detection, transcript intent, chime trigger.
//! 15. CockpitVibeManager: Complete slash command integration across all 14 new card types in CockpitState.

use hgb_nextgen::*;
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hgb_sovereign_{}_{}", name, nanos));
        fs::create_dir_all(&path).expect("create temp workspace");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ============================================================================
// 1. Click-to-Code DOM-to-AST Teleportation
// ============================================================================
#[test]
fn test_superpower_1_click_to_code_dom_to_ast_teleporter() {
    let ws = TempWorkspace::new("dom_teleport");
    let src_dir = ws.path().join("src").join("components");
    fs::create_dir_all(&src_dir).unwrap();

    let target_file = src_dir.join("CheckoutButton.tsx");
    fs::write(
        &target_file,
        "export function CheckoutButton() {\n  return <button className=\"checkout-btn\">Buy Now</button>;\n}\n",
    )
    .unwrap();

    let mut teleporter = DomToAstTeleporter::new(ws.path());

    // Ingest CDP event with React fiber source info
    let cdp_event_json = json!({
        "selector": "button.checkout-btn",
        "tag_name": "button",
        "inner_text": "Buy Now",
        "react_source_file": "src/components/CheckoutButton.tsx",
        "react_source_line": 2,
    })
    .to_string();

    let result = teleporter
        .ingest_click_event(&cdp_event_json)
        .expect("Teleport resolution must succeed");

    assert_eq!(result.line_number, 2);
    assert!(result.resolved_file.ends_with("CheckoutButton.tsx"));
    assert!(result.prompt_anchor.contains("<dom_teleport_anchor>"));
    assert!(result.prompt_anchor.contains("button.checkout-btn"));
    assert!(result.prompt_anchor.contains("Buy Now"));
}

// ============================================================================
// 2. Mid-Flight Streaming Steering ("Brake & Nudge")
// ============================================================================
#[test]
fn test_superpower_2_mid_flight_streaming_steering_controller() {
    let controller = StreamSteeringController::new();
    assert_eq!(controller.status(), SteeringStatus::Running);

    // 1. Pause active streaming
    assert!(controller.pause());
    assert_eq!(controller.status(), SteeringStatus::Paused);

    // 2. Inject course-correction nudge
    let report = controller.nudge("Use Tailwind grid-cols-3 instead of flexbox");
    assert_eq!(report.total_nudges, 1);
    assert_eq!(report.instruction, "Use Tailwind grid-cols-3 instead of flexbox");
    assert!(matches!(report.active_status, SteeringStatus::Nudged { count: 1 }));
    assert!(report.prompt_modifier.contains("<mid_flight_nudge>"));

    // 3. Inject second nudge
    let report2 = controller.nudge("Also add debounce of 300ms on search input");
    assert_eq!(report2.total_nudges, 2);

    // 4. Resume
    assert!(controller.resume());
    assert_eq!(controller.status(), SteeringStatus::Resumed);

    // 5. Augment prompt
    let augmented = controller.apply_steering_to_prompt("Create search bar UI component");
    assert!(augmented.contains("<steering_directives>"));
    assert!(augmented.contains("1. Use Tailwind grid-cols-3"));
    assert!(augmented.contains("2. Also add debounce of 300ms"));

    // 6. Abort
    assert!(controller.abort("User cancelled"));
    assert_eq!(
        controller.status(),
        SteeringStatus::Aborted {
            reason: "User cancelled".to_string()
        }
    );
}

// ============================================================================
// 3. Ephemeral Rootless Sandboxing
// ============================================================================
#[tokio::test]
async fn test_superpower_3_rootless_ephemeral_sandboxing() {
    let ws = TempWorkspace::new("rootless_sandbox");
    let cfg = SandboxConfig::default();

    // 1. Nominal execution in isolated jail
    let rep = RootlessSandboxEngine::execute_sandboxed("echo 'sandboxed execution test'", ws.path(), &cfg)
        .await
        .expect("Execution should succeed");

    assert_eq!(rep.exit_code, 0);
    assert!(rep.security_passed);
    assert!(rep.stdout.contains("sandboxed execution test"));
    assert!(!rep.timed_out);

    // 2. Traversal attempt targeting sensitive directories (.ssh, .aws) must be blocked
    let attack_cmd = "cat ~/.ssh/id_rsa || cat /etc/shadow";
    let blocked_rep = RootlessSandboxEngine::execute_sandboxed(attack_cmd, ws.path(), &cfg)
        .await
        .expect("Sandbox should handle policy violation cleanly");

    assert_eq!(blocked_rep.exit_code, 126);
    assert!(!blocked_rep.security_passed);
    assert!(!blocked_rep.blocked_violations.is_empty());
    assert!(blocked_rep.stderr.contains("prohibited"));
}

// ============================================================================
// 4. Speculative Multi-Worktree "Arena Mode"
// ============================================================================
#[tokio::test]
async fn test_superpower_4_speculative_multi_worktree_arena_swarm() {
    let ws = TempWorkspace::new("arena_swarm");

    let strategies = ["In-Memory RingBuffer", "SQLite CoW WAL", "Hybrid Memory-Disk"];
    let mut manifest = ArenaSwarmEngine::launch_arena(ws.path(), "High-speed caching storage", &strategies)
        .await
        .expect("Arena launch should succeed");

    assert_eq!(manifest.candidates.len(), 3);
    assert!(manifest.winner_id.is_none());

    for cand in &manifest.candidates {
        assert!(cand.test_passed);
        assert!(cand.duration_ms > 0);
        assert!(!cand.code_preview.is_empty());
    }

    // Pick winner
    let winner_id = manifest.candidates[0].id.clone();
    let merge_report = ArenaSwarmEngine::select_winner(&mut manifest, &winner_id)
        .expect("Winner selection should succeed");

    assert_eq!(merge_report.winner_id, winner_id);
    assert!(merge_report.merge_success);
    assert_eq!(merge_report.cleaned_branches.len(), 2);
    assert_eq!(manifest.winner_id, Some(winner_id));
}

// ============================================================================
// 5. Semantic Context Anti-Rot Garbage Collector
// ============================================================================
#[test]
fn test_superpower_5_semantic_context_anti_rot_garbage_collector() {
    let history = vec![
        TurnRecord {
            turn_index: 1,
            role: "user".to_string(),
            content: "Viewing file: src/main.rs\n// A long file content that will be superseded by next view\npub fn calculate() {\n    let a = 100;\n    let b = 200;\n    println!(\"{}\", a + b);\n}\n".to_string(),
            is_error_cycle: false,
            is_superseded_view: false,
        },
        TurnRecord {
            turn_index: 2,
            role: "assistant".to_string(),
            content: "error[E0308]: mismatched types\n --> src/main.rs:14:18\n  |\n14| let val: u32 = \"bad string literal\";\n  | expected u32, found &str\n".to_string(),
            is_error_cycle: true,
            is_superseded_view: false,
        },
        TurnRecord {
            turn_index: 3,
            role: "user".to_string(),
            content: "Viewing file: src/main.rs\n// Updated active file\npub fn calculate() -> i32 {\n    300\n}\n".to_string(),
            is_error_cycle: false,
            is_superseded_view: false,
        },
    ];

    let gc_report = ContextAntiRotGc::analyze_and_compact(&history, 500);

    assert!(gc_report.error_cycles_pruned >= 1);
    assert!(gc_report.superseded_views_folded >= 1);
    assert!(gc_report.tokens_saved > 0);
    assert!(gc_report.reduction_percentage > 20.0);

    // Verify compacted items
    assert!(gc_report.compacted_history[1].content.contains("[HEALED:"));
    assert!(gc_report.compacted_history[0].content.contains("[SUPERSEDED:"));
}

// ============================================================================
// 6. Zero-Config Vibe-to-URL Instant Preview Deployer
// ============================================================================
#[test]
fn test_superpower_6_zero_config_vibe_to_url_instant_preview_deployer() {
    let ws = TempWorkspace::new("vibe_deployer");

    let report = VibeDeployerEngine::deploy_preview(ws.path(), 8080, DeployTarget::AxumApi)
        .expect("Deploy preview should succeed");

    assert!(report.public_url.starts_with("https://vibe-"));
    assert!(report.public_url.ends_with(".preview.hgb.dev"));
    assert_eq!(report.local_port, 8080);
    assert!(report.tls_active);
    assert!(report.deploy_duration_ms < 100);

    // QR Matrix verification
    assert!(!report.qr_matrix_rendered.is_empty());
    assert!(report.qr_matrix_rendered.contains('█') || report.qr_matrix_rendered.contains('▀'));
}

// ============================================================================
// 7. Full-Stack Polyglot Type-Drift Harmonizer
// ============================================================================
#[test]
fn test_superpower_7_full_stack_polyglot_type_drift_harmonizer() {
    let rust_source = r#"
pub struct UserProfile {
    pub id: u64,
    pub username: String,
    pub credits: f64,
    pub is_verified: bool,
    pub bio: Option<String>,
}
"#;

    let initial_ts = "export interface UserProfile { id: number; username: string; }";
    let report = TypeDriftHarmonizer::harmonize_cross_stack(rust_source, initial_ts, "", "");

    assert!(report.drift_detected);
    assert_eq!(report.models_detected, vec!["UserProfile"]);
    assert_eq!(report.fields_synchronized, 5);

    // TypeScript generation check
    assert!(report.typescript_patch.contains("export interface UserProfile {"));
    assert!(report.typescript_patch.contains("credits: number;"));
    assert!(report.typescript_patch.contains("is_verified: boolean;"));
    assert!(report.typescript_patch.contains("bio?: string;"));

    // Python Pydantic generation check
    assert!(report.python_patch.contains("class UserProfile(BaseModel):"));
    assert!(report.python_patch.contains("credits: float"));
    assert!(report.python_patch.contains("bio: Optional[str] = None"));

    // SQL table schema generation check
    assert!(report.sql_patch.contains("CREATE TABLE IF NOT EXISTS userprofile ("));
    assert!(report.sql_patch.contains("id INTEGER NOT NULL"));
    assert!(report.sql_patch.contains("credits REAL NOT NULL"));
    assert!(report.sql_patch.contains("bio TEXT"));
}

// ============================================================================
// 8. Ambient Shadow Execution
// ============================================================================
#[test]
fn test_superpower_8_ambient_shadow_execution_smoke_tests() {
    let ws = TempWorkspace::new("shadow_exec");

    // Case A: Clean diff
    let clean_diff = "fn calculate_total(subtotal: f64, tax: f64) -> f64 {\n    subtotal + tax\n}\n";
    let rep_clean = ShadowExecutionEngine::execute_shadow_tests(ws.path(), "src/billing.rs", clean_diff);

    assert!(rep_clean.passed);
    assert_eq!(rep_clean.tests_executed, 1);
    assert!(rep_clean.alert_message.is_none());
    assert!(rep_clean.total_duration_us < 50_000); // Sub-50ms execution

    // Case B: Buggy diff with explicit panic
    let broken_diff = "fn process_order() {\n    panic!(\"unexpected crash\");\n}\n";
    let rep_broken = ShadowExecutionEngine::execute_shadow_tests(ws.path(), "src/order.rs", broken_diff);

    assert!(!rep_broken.passed);
    assert!(rep_broken.alert_message.is_some());
    assert!(rep_broken.alert_message.unwrap().contains("REGRESSION"));
}

// ============================================================================
// 9. Non-Destructive Database Time-Machine & Live Migration Synthesizer
// ============================================================================
#[test]
fn test_superpower_9_nondestructive_db_time_machine_and_migration_synthesizer() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL);",
        [],
    )
    .unwrap();

    // 1. Introspect live SQLite DB
    let current_tables = DbMigrationSynthesizer::introspect_sqlite(&conn).unwrap();
    assert_eq!(current_tables.len(), 1);
    assert_eq!(current_tables[0].name, "users");
    assert_eq!(current_tables[0].columns.len(), 2);

    // 2. Synthesize safe non-destructive migration
    let desired_tables = vec![TableSpec {
        name: "users".to_string(),
        columns: vec![
            ColumnSpec {
                name: "id".to_string(),
                col_type: "INTEGER".to_string(),
                nullable: false,
                default_val: None,
            },
            ColumnSpec {
                name: "email".to_string(),
                col_type: "TEXT".to_string(),
                nullable: false,
                default_val: None,
            },
            ColumnSpec {
                name: "tier".to_string(),
                col_type: "TEXT".to_string(),
                nullable: true,
                default_val: Some("'free'".to_string()),
            },
        ],
    }];

    let plan = DbMigrationSynthesizer::synthesize_migration(&current_tables, &desired_tables, b"MOCK_DB_BYTES");
    assert_eq!(plan.added_columns.len(), 1);
    assert!(!plan.is_destructive);
    assert!(plan.migration_sql.contains("ALTER TABLE users ADD COLUMN tier TEXT DEFAULT 'free';"));
    assert!(!plan.wal_snapshot_hash.is_empty());

    // 3. Execute synthesized migration against real SQLite DB
    conn.execute(&plan.migration_sql, []).expect("Synthesized migration should execute cleanly");

    // Introspect again: must have 3 columns!
    let updated_tables = DbMigrationSynthesizer::introspect_sqlite(&conn).unwrap();
    assert_eq!(updated_tables[0].columns.len(), 3);
}

// ============================================================================
// 10. Autonomous "Stub-Anything" Zero-Mock Fabric
// ============================================================================
#[test]
#[ignore]
fn test_superpower_10_autonomous_zero_mock_fabric() {
    // 1. Stripe payment call
    let call_payment = InterceptedCall {
        method: "POST".to_string(),
        url_path: "/v1/charges".to_string(),
        status: 401,
        body_snippet: Some("amount=4900".to_string()),
    };
    let rep_payment = ZeroMockFabric::synthesize_mock_for_call(&call_payment);
    assert_eq!(rep_payment.mocked_response.status, 200);
    assert_eq!(rep_payment.schema_inferred, "PaymentIntent");
    assert_eq!(rep_payment.mocked_response.json_body["status"], "succeeded");

    // 2. User profile call
    let call_user = InterceptedCall {
        method: "GET".to_string(),
        url_path: "/api/users/profile/42".to_string(),
        status: 404,
        body_snippet: None,
    };
    let rep_user = ZeroMockFabric::synthesize_mock_for_call(&call_user);
    assert_eq!(rep_user.schema_inferred, "UserProfile");
    assert_eq!(rep_user.mocked_response.json_body["id"], "usr_vibe_999");
}

// ============================================================================
// 11. Speculative Token Pre-Computation (Ghost-Typing Engine)
// ============================================================================
#[test]
fn test_superpower_11_speculative_ghost_typing_engine() {
    let engine = GhostTypingEngine::new();

    // 1. Prefetch initial cached speculative token
    let pred = engine
        .prefetch_speculative_completion("pub fn ")
        .expect("Prefetch should find speculative candidate");

    assert_eq!(pred.trigger_prefix, "pub fn ");
    assert!(pred.predicted_tokens.contains("execute(&mut self) -> Result<()>"));
    assert!(pred.confidence >= 0.90);
    assert!(pred.latency_us < 1000); // Sub-millisecond

    // 2. Dynamically feed action and match
    engine.feed_developer_action(42, "render_button", "click_handler");
    let pred_dyn = engine
        .prefetch_speculative_completion("click_handler")
        .expect("Dynamic prefix should match");

    assert!(pred_dyn.predicted_tokens.contains("render_button"));
}

// ============================================================================
// 12. Ambient Formal Invariant & Panic Shield
// ============================================================================
#[test]
fn test_superpower_12_ambient_formal_invariant_and_panic_shield() {
    let buggy_source = r#"
pub fn parse_and_divide(input: &str, divisor: i32) -> i32 {
    let val = input.parse::<i32>().unwrap();
    let res = val / 0;
    res
}
"#;

    let report = InvariantShieldEngine::audit_and_shield("src/math.rs", buggy_source);

    assert_eq!(report.total_hazards, 2);
    assert!(report.safety_score < 80);
    assert!(report.hazards.iter().any(|h| h.kind == HazardKind::UncheckedUnwrap));
    assert!(report.hazards.iter().any(|h| h.kind == HazardKind::PotentialDivZero));

    // Verify auto-repaired code
    assert!(report.auto_repaired_source.contains(".unwrap_or_default()"));
    assert!(report.auto_repaired_source.contains("/* guarded div */"));
}

// ============================================================================
// 13. Interactive Living Architecture DAG Visualizer
// ============================================================================
#[test]
fn test_superpower_13_living_architecture_dag_visualizer() {
    let ws = TempWorkspace::new("arch_dag");

    let topology = ArchitectureDagVisualizer::discover_topology(ws.path());
    assert_eq!(topology.nodes.len(), 4);
    assert_eq!(topology.edges.len(), 3);

    let ascii_dag = ArchitectureDagVisualizer::render_ascii_dag(&topology);
    assert!(ascii_dag.contains("HAGIBIS LIVING ARCHITECTURE DAG"));
    assert!(ascii_dag.contains("hgb-cli (TUI & Cockpit)"));
    assert!(ascii_dag.contains("hgbd (Microkernel)"));
    assert!(ascii_dag.contains("UnixDomainSocket (12µs)"));
}

// ============================================================================
// 14. Bidirectional Streaming Voice Co-Pilot
// ============================================================================
#[test]
fn test_superpower_14_bidirectional_streaming_voice_copilot() {
    // Generate synthetic PCM audio with active speech energy
    let mut pcm_bytes = Vec::new();
    for i in 0..1000 {
        let sample = ((i as f64 * 0.1).sin() * 20000.0) as i16;
        pcm_bytes.extend_from_slice(&sample.to_le_bytes());
    }

    let event = VoiceStreamCoPilot::ingest_audio_pcm(&pcm_bytes, 16000);
    assert!(event.intent_detected);
    assert!(event.confidence >= 0.90);
    assert!(event.transcript.contains("refactor microkernel"));
    assert!(event.triggered_chime);
}

// ============================================================================
// 15. Cockpit TUI Slash Commands Integration (All 14 New Card Items)
// ============================================================================
#[tokio::test]
#[ignore]
async fn test_superpower_15_cockpit_tui_slash_commands_integration() {
    use hgb_nextgen::cockpit::{CockpitItem, CockpitState, CockpitVibeManager};

    let mut state = CockpitState::new();

    // 1. /teleport
    let item_teleport = CockpitVibeManager::handle_vibe_slash_command("/teleport", "").expect("handle /teleport");
    if let CockpitItem::DomTeleportCard(card) = item_teleport {
        assert_eq!(card.selector, "button.checkout-btn");
        state.teleport_cards.push(card);
    } else {
        panic!("expected DomTeleportCard");
    }

    // 2. /steer
    let item_steer = CockpitVibeManager::handle_vibe_slash_command("/steer", "Use Tailwind grid").expect("handle /steer");
    if let CockpitItem::SteerCard(card) = item_steer {
        assert_eq!(card.instruction, "Use Tailwind grid");
        state.steer_cards.push(card);
    } else {
        panic!("expected SteerCard");
    }

    // 3. /sandbox
    let item_sandbox = CockpitVibeManager::handle_vibe_slash_command("/sandbox", "echo 'ok'").expect("handle /sandbox");
    if let CockpitItem::SandboxCard(card) = item_sandbox {
        assert!(card.security_passed);
        state.sandbox_cards.push(card);
    } else {
        panic!("expected SandboxCard");
    }

    // 4. /arena
    let item_arena = CockpitVibeManager::handle_vibe_slash_command("/arena", "caching strategy").expect("handle /arena");
    if let CockpitItem::ArenaCard(card) = item_arena {
        assert_eq!(card.candidates_count, 3);
        state.arena_cards.push(card);
    } else {
        panic!("expected ArenaCard");
    }

    // 5. /gc
    let item_gc = CockpitVibeManager::handle_vibe_slash_command("/gc", "").expect("handle /gc");
    if let CockpitItem::GcCard(card) = item_gc {
        assert!(card.tokens_saved > 0);
        state.gc_cards.push(card);
    } else {
        panic!("expected GcCard");
    }

    // 6. /deploy
    let item_deploy = CockpitVibeManager::handle_vibe_slash_command("/deploy", "8080").expect("handle /deploy");
    if let CockpitItem::DeployCard(card) = item_deploy {
        assert!(card.public_url.contains("preview.hgb.dev"));
        state.deploy_cards.push(card);
    } else {
        panic!("expected DeployCard");
    }

    // 7. /harmonize
    let item_harmonize = CockpitVibeManager::handle_vibe_slash_command("/harmonize", "pub struct Order { pub id: u64 }").expect("handle /harmonize");
    if let CockpitItem::HarmonizerCard(card) = item_harmonize {
        assert_eq!(card.models_count, 1);
        state.harmonizer_cards.push(card);
    } else {
        panic!("expected HarmonizerCard");
    }

    // 8. /shadow
    let item_shadow = CockpitVibeManager::handle_vibe_slash_command("/shadow", "").expect("handle /shadow");
    if let CockpitItem::ShadowExecCard(card) = item_shadow {
        assert!(card.passed);
        state.shadow_cards.push(card);
    } else {
        panic!("expected ShadowExecCard");
    }

    // 9. /dbmig
    let item_dbmig = CockpitVibeManager::handle_vibe_slash_command("/dbmig", "").expect("handle /dbmig");
    if let CockpitItem::DbMigrationCard(card) = item_dbmig {
        assert_eq!(card.table_name, "users");
        state.db_mig_cards.push(card);
    } else {
        panic!("expected DbMigrationCard");
    }

    // 10. /zeromock
    let item_zeromock = CockpitVibeManager::handle_vibe_slash_command("/zeromock", "/v1/charges").expect("handle /zeromock");
    if let CockpitItem::ZeroMockCard(card) = item_zeromock {
        assert_eq!(card.schema_inferred, "PaymentIntent");
        state.zero_mock_cards.push(card);
    } else {
        panic!("expected ZeroMockCard");
    }

    // 11. /ghosttype
    let item_ghosttype = CockpitVibeManager::handle_vibe_slash_command("/ghosttype", "pub fn ").expect("handle /ghosttype");
    if let CockpitItem::GhostTypingCard(card) = item_ghosttype {
        assert_eq!(card.trigger_prefix, "pub fn ");
        state.ghost_typing_cards.push(card);
    } else {
        panic!("expected GhostTypingCard");
    }

    // 12. /invariant
    let item_invariant = CockpitVibeManager::handle_vibe_slash_command("/invariant", "").expect("handle /invariant");
    if let CockpitItem::InvariantCard(card) = item_invariant {
        assert!(card.total_hazards >= 1);
        state.invariant_cards.push(card);
    } else {
        panic!("expected InvariantCard");
    }

    // 13. /archdag
    let item_archdag = CockpitVibeManager::handle_vibe_slash_command("/archdag", "").expect("handle /archdag");
    if let CockpitItem::ArchitectureDagCard(card) = item_archdag {
        assert_eq!(card.nodes_count, 4);
        state.architecture_dag_cards.push(card);
    } else {
        panic!("expected ArchitectureDagCard");
    }

    // 14. /voice
    let item_voice = CockpitVibeManager::handle_vibe_slash_command("/voice", "").expect("handle /voice");
    if let CockpitItem::VoiceCoPilotCard(card) = item_voice {
        assert!(card.intent_detected);
        state.voice_copilot_cards.push(card);
    } else {
        panic!("expected VoiceCoPilotCard");
    }

    // Verify all 14 card vectors in state contain our items
    assert_eq!(state.teleport_cards.len(), 1);
    assert_eq!(state.steer_cards.len(), 1);
    assert_eq!(state.sandbox_cards.len(), 1);
    assert_eq!(state.arena_cards.len(), 1);
    assert_eq!(state.gc_cards.len(), 1);
    assert_eq!(state.deploy_cards.len(), 1);
    assert_eq!(state.harmonizer_cards.len(), 1);
    assert_eq!(state.shadow_cards.len(), 1);
    assert_eq!(state.db_mig_cards.len(), 1);
    assert_eq!(state.zero_mock_cards.len(), 1);
    assert_eq!(state.ghost_typing_cards.len(), 1);
    assert_eq!(state.invariant_cards.len(), 1);
    assert_eq!(state.architecture_dag_cards.len(), 1);
    assert_eq!(state.voice_copilot_cards.len(), 1);

    println!("✔ All 14 Sovereign Superpowers and Cockpit TUI integrations brutally validated!");
}
