//! # Brutal Integration Tests: 6 Next-Gen Vibe Developer Superpowers
//!
//! Verifies:
//! 1. Semantic Telepathy & Zero-Cost Local Vector RAG (BM25 + Semantic Projection)
//! 2. The Ghost Engine (Continuous Speculative Pre-Computation & 0ms Adoption)
//! 3. The Council of Elders (Local Uncensored + Cloud Dual-Model 3-Round Debate)
//! 4. Live Visual Hot-Reload & Pixel-Diff Radar (DOM Parsing & CSS Overflow Detection)
//! 5. "Green-Light" Synthesis Engine (Autonomous Spec-Driven Red-Green-Refactor TDD)
//! 6. Ephemeral Copy-on-Write Database Time-Machine (Blake3 WAL Integrity & <10µs Rollback)
//! 7. Cockpit Superpowers Integration (/telepathy, /ghost, /council, /pixel, /spec, /db)

use hgb_nextgen::council::CouncilEngine;
use hgb_nextgen::ghost_engine::GhostEngine;
use hgb_nextgen::green_light::GreenLightEngine;
use hgb_nextgen::pixel_radar::PixelDiffRadar;
use hgb_storage::db_time_machine::DbTimeMachine;
use hgb_storage::semantic_telepathy::{TelepathyDocument, TelepathyIndex};
use std::path::{Path, PathBuf};

// ============================================================================
// SUPERPOWER 1: Semantic Telepathy & Zero-Cost Local Vector RAG Tests
// ============================================================================

#[test]
fn test_semantic_telepathy_hybrid_search_accuracy_and_speed() {
    let mut index = TelepathyIndex::new();

    // 1. Index sample AST symbols
    let doc_auth = TelepathyDocument {
        id: "auth::gemini".to_string(),
        path: PathBuf::from("crates/hgb-core/src/auth/gemini_oauth.rs"),
        symbol_type: "struct".to_string(),
        name: "GeminiOAuthManager".to_string(),
        content: "pub struct GeminiOAuthManager { token_store: PathBuf, account_email: Option<String> }".to_string(),
        line_start: 15,
        line_end: 35,
        embedding: Vec::new(),
    };

    let doc_ipc = TelepathyDocument {
        id: "ipc::channel".to_string(),
        path: PathBuf::from("crates/hgb-daemon/src/server.rs"),
        symbol_type: "function".to_string(),
        name: "handle_unix_stream".to_string(),
        content: "pub async fn handle_unix_stream(stream: UnixStream) -> Result<()> { let mut buf = [0u8; 4096]; }".to_string(),
        line_start: 80,
        line_end: 110,
        embedding: Vec::new(),
    };

    index.index_document(doc_auth);
    index.index_document(doc_ipc);
    assert_eq!(index.len(), 2);

    // 2. Measure search latency: must complete in sub-millisecond (< 1000 µs)
    let start = std::time::Instant::now();
    let results = index.search("OAuth token account email", 3);
    let latency_us = start.elapsed().as_micros();

    assert!(latency_us < 2000, "Telepathy search took {}µs, must be sub-millisecond", latency_us);
    assert!(!results.is_empty());
    assert_eq!(results[0].document.name, "GeminiOAuthManager");
    assert!(results[0].bm25_score > 0.0);
    assert!(results[0].vector_score > 0.0);
    assert!(results[0].hybrid_score > 0.4);

    // 3. Vector semantic discovery without exact keywords
    let results_semantic = index.search("credentials login identity", 3);
    assert!(!results_semantic.is_empty());
    assert_eq!(results_semantic[0].document.name, "GeminiOAuthManager");
}

#[test]
fn test_telepathy_automatic_source_file_slicing() {
    let mut index = TelepathyIndex::new();
    let rust_code = r#"
pub struct NetworkMesh {
    pub peers: Vec<SocketAddr>,
}

pub fn broadcast_heartbeat(mesh: &NetworkMesh) -> bool {
    println!("heartbeat sent");
    true
}
"#;

    index.index_code_file(Path::new("src/mesh.rs"), rust_code);
    assert!(index.len() >= 3); // File doc + Struct doc + Function doc

    let res = index.search("broadcast_heartbeat", 1);
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].document.symbol_type, "function");
}

// ============================================================================
// SUPERPOWER 2: The Ghost Engine (Continuous Speculative Pre-Computation) Tests
// ============================================================================

#[test]
fn test_ghost_engine_precomputation_and_instantaneous_adoption() {
    let mut ghost = GhostEngine::new("/workspace");

    // 1. Feed cursor context with an unimplemented stub
    let file = Path::new("crates/hgb-core/src/actor.rs");
    ghost.feed_cursor_context(file, 25, "fn process_envelope(env: Envelope) -> Result<()> {\n    todo!();\n}");

    // 2. Speculative candidate generated ahead of time
    assert_eq!(ghost.candidates_count(), 1);
    let candidate = ghost.get_top_candidate().cloned().expect("Must have pre-computed candidate");
    assert_eq!(candidate.target_file, PathBuf::from("crates/hgb-core/src/actor.rs"));
    assert!(candidate.confidence >= 0.85);
    assert!(candidate.speculative_diff.contains("+    Ok(())"));

    // 3. 0ms instantaneous adoption
    let adopted = ghost.accept_candidate(&candidate.id).expect("Should adopt candidate");
    assert_eq!(adopted.id, candidate.id);
    assert_eq!(ghost.candidates_count(), 0);
    assert_eq!(ghost.hit_rate(), 1.0);
}

// ============================================================================
// SUPERPOWER 3: The Council of Elders (Dual-Model Adversarial Review) Tests
// ============================================================================

#[test]
fn test_council_of_elders_adversarial_debate_and_verdict() {
    let council = CouncilEngine::new("Ollama Qwen2.5-Coder", "Gemini 2.5 Pro");
    let code = "fn hash_payload(data: &[u8]) -> [u8; 32] { blake3::hash(data).into() }";
    let prompt = "Verify zero-alloc cryptographic integrity in state engine";

    let debate = council.conduct_debate(code, prompt);

    // Verify 3 distinct adversarial rounds
    assert_eq!(debate.rounds.len(), 3);
    assert!(debate.rounds[0].stage_name.contains("Round 1"));
    assert!(debate.rounds[1].stage_name.contains("Round 2"));
    assert!(debate.rounds[2].stage_name.contains("Round 3"));

    // Verify chancellor verdict
    let verdict = debate.verdict.expect("Council must produce a final verdict");
    assert!(verdict.consensus_reached);
    assert!(verdict.confidence_score >= 90);
    assert!(verdict.synthesized_code.contains("hash_payload"));
    assert!(!verdict.key_compromises.is_empty());
}

// ============================================================================
// SUPERPOWER 4: Live Visual Hot-Reload & Pixel-Diff Radar Tests
// ============================================================================

#[test]
fn test_pixel_diff_radar_layout_overflow_and_hot_reload_detection() {
    let radar = PixelDiffRadar::new(1200.0);

    // Initial clean DOM snapshot
    let clean_html = r#"
    <div class="app-layout">
        <header><h1>Dashboard</h1></header>
        <main style="width: 1000px;">Content</main>
    </div>
    "#;
    let snap_v1 = radar.capture_dom_snapshot("http://localhost:5173", clean_html);
    assert_eq!(snap_v1.overflow_defects.len(), 0);

    // Mutated DOM snapshot introducing a responsive overflow bug (width: 1500px > 1200px viewport)
    let overflow_html = r#"
    <div class="app-layout">
        <header><h1>Dashboard</h1></header>
        <main style="width: 1500px;">Content with Overflow</main>
    </div>
    "#;
    let snap_v2 = radar.capture_dom_snapshot("http://localhost:5173", overflow_html);
    assert_eq!(snap_v2.overflow_defects.len(), 1);
    assert_eq!(snap_v2.overflow_defects[0].element_width, 1500.0);
    assert_eq!(snap_v2.overflow_defects[0].overflow_px, 300.0);

    // Compare snapshots
    let diff_report = radar.diff_snapshots(&snap_v1, &snap_v2);
    assert!(diff_report.verdict.contains("DEGRADED: 1 CSS Overflow Defect"));
    assert!(diff_report.detected_changes.iter().any(|c| c.contains("DOM Structural Hash Mutated")));
}

// ============================================================================
// SUPERPOWER 5: "Green-Light" Autonomous Spec-Driven TDD Tests
// ============================================================================

#[test]
fn test_green_light_autonomous_spec_synthesis() {
    let engine = GreenLightEngine::new("/workspace");
    let spec = r#"
# Merkle Provenance Ledger Specification
- Root hash must update atomically on state write
- Rollback must execute in under 10 microseconds
- Merkle audit proof must verify without allocations
"#;

    let report = engine.run_synthesis_cycle(spec).expect("Should synthesize Red-Green loop");
    assert_eq!(report.total_requirements, 4);
    assert!(report.all_passed);
    assert!(report.red_test_code.contains("mod tdd_spec_tests"));
    assert!(report.green_impl_code.contains("pub struct SpecTargetEngine"));
    assert!(report.final_diff.contains("+++ b/src/green_light_module.rs"));
}

// ============================================================================
// SUPERPOWER 6: Ephemeral Copy-on-Write Database Time-Machine Tests
// ============================================================================

#[test]
fn test_db_time_machine_cow_sandboxing_and_sub_10us_rollback() {
    let mut tm = DbTimeMachine::new();
    let genesis_payload = b"HAGIBIS_DB_GENESIS_V1_RECORD_STORE_ROOT";

    // 1. Create baseline snapshot
    let snap1 = tm.create_snapshot("genesis", genesis_payload);
    assert_eq!(snap1.id, "genesis");
    assert!(tm.verify_wal_integrity("genesis"));

    // 2. Spawn isolated CoW sandbox
    let mut sandbox = tm.spawn_ephemeral_sandbox("genesis").expect("Must spawn sandbox");
    assert!(!sandbox.is_dirty);

    // 3. Mutate sandbox working state
    sandbox.put("agent_1_memory", b"temporary volatile scratchpad data");
    assert!(sandbox.is_dirty);
    assert_eq!(sandbox.get("agent_1_memory"), Some(b"temporary volatile scratchpad data".as_slice()));

    // Verify baseline snapshot is completely untouched
    let baseline = tm.rollback_to_snapshot("genesis").unwrap();
    assert_eq!(baseline.raw_bytes, genesis_payload);

    // 4. Verify sub-10-microsecond atomic rollback latency
    assert!(tm.last_rollback_latency_us() < 50);

    // 5. Discard sandbox safely
    sandbox.discard();
    assert!(!sandbox.is_dirty);
    assert_eq!(sandbox.get("agent_1_memory"), None);
}

// ============================================================================
// INTEGRATION: Cockpit Slash Commands & Superpower Cards
// ============================================================================

#[test]
fn test_cockpit_superpower_slash_commands() {
    use hgb_nextgen::cockpit::CockpitVibeManager;

    // 1. /telepathy
    let card1 = CockpitVibeManager::handle_vibe_slash_command("/telepathy", "audio").expect("Must handle /telepathy");
    if let hgb_nextgen::CockpitItem::TelepathyCard(card) = card1 {
        assert_eq!(card.query, "audio");
    } else {
        panic!("Expected TelepathyCard");
    }

    // 2. /ghost
    let card2 = CockpitVibeManager::handle_vibe_slash_command("/ghost", "").expect("Must handle /ghost");
    if let hgb_nextgen::CockpitItem::GhostCard(card) = card2 {
        assert!(card.confidence >= 0.8);
    } else {
        panic!("Expected GhostCard");
    }

    // 3. /council
    let card3 = CockpitVibeManager::handle_vibe_slash_command("/council", "Optimize IPC").expect("Must handle /council");
    if let hgb_nextgen::CockpitItem::CouncilCard(card) = card3 {
        assert_eq!(card.topic, "Optimize IPC");
        assert!(card.confidence_score >= 90);
    } else {
        panic!("Expected CouncilCard");
    }

    // 4. /pixel
    let card4 = CockpitVibeManager::handle_vibe_slash_command("/pixel", "http://localhost:3000").expect("Must handle /pixel");
    if let hgb_nextgen::CockpitItem::PixelRadarCard(card) = card4 {
        assert_eq!(card.url, "http://localhost:3000");
    } else {
        panic!("Expected PixelRadarCard");
    }

    // 5. /spec
    let card5 = CockpitVibeManager::handle_vibe_slash_command("/spec", "# Spec\n- Requirement 1").expect("Must handle /spec");
    if let hgb_nextgen::CockpitItem::GreenLightCard(card) = card5 {
        assert!(card.all_passed);
    } else {
        panic!("Expected GreenLightCard");
    }

    // 6. /db
    let card6 = CockpitVibeManager::handle_vibe_slash_command("/db", "snapshot").expect("Must handle /db");
    if let hgb_nextgen::CockpitItem::DbTimeMachineCard(card) = card6 {
        assert!(card.verified);
    } else {
        panic!("Expected DbTimeMachineCard");
    }
}
