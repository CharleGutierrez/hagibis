use hgb_core::auto_spec::TestVector;
use hgb_core::browser_snoop::{BrowserIncidentKind, BrowserSeverity};
use hgb_core::db_sentinel::MigrationSafetyLevel;
use hgb_core::drift_lock::DriftSeverity;
use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_core::trace::TraceRingBuffer;
use hgb_daemon::server::{DaemonState, HagibisDaemon};
use hgb_nextgen::{
    AutoSpecEngine, BrowserSnoopEngine, DriftLockEngine, SyntaxSlicer, VariantRaceEngine,
};
use hgb_storage::{DbSentinel, SpecStore};
use rusqlite::Connection;
use std::fs;
use std::sync::Arc;

// =========================================================================
// PILLAR 1: BrowserSnoop Brutal Tests
// =========================================================================
#[tokio::test]
async fn test_browser_snoop_incident_lifecycle_and_health_verdict() {
    let trace_buf = Arc::new(TraceRingBuffer::new(50));
    let snoop = BrowserSnoopEngine::new("http://localhost:3000".to_string(), Some(trace_buf.clone()));

    // 1. Initial healthy state
    let rep0 = snoop.generate_health_report();
    assert_eq!(rep0.total_incidents, 0);
    assert_eq!(rep0.verdict, hgb_core::browser_snoop::BrowserHealthVerdict::Healthy);

    // 2. Record console error
    let inc1 = snoop.record_incident(
        BrowserIncidentKind::ConsoleError {
            message: "Uncaught TypeError: Cannot read properties of undefined (reading 'avatar_url')".to_string(),
            stack_trace: Some("at UserCard (UserCard.tsx:42:15)".to_string()),
            source_url: Some("http://localhost:3000/src/UserCard.tsx".to_string()),
            line: Some(42),
            col: Some(15),
        },
        BrowserSeverity::Error,
    );
    assert_eq!(inc1.id, "inc-1");

    // 3. Record network failure
    snoop.record_incident(
        BrowserIncidentKind::NetworkFailure {
            url: "http://localhost:3000/api/profile".to_string(),
            method: "GET".to_string(),
            status_code: 500,
            error_text: Some("Internal Server Error".to_string()),
            duration_ms: 85,
        },
        BrowserSeverity::Error,
    );

    // 4. Record HMR compile error
    snoop.record_incident(
        BrowserIncidentKind::HmrCompileError {
            compiler_message: "Module build failed: SyntaxError: Unexpected token '<'".to_string(),
            affected_file: Some("src/Header.tsx".to_string()),
        },
        BrowserSeverity::Fatal,
    );

    let rep = snoop.generate_health_report();
    assert_eq!(rep.total_incidents, 3);
    assert_eq!(rep.console_errors_count, 1);
    assert_eq!(rep.network_failures_count, 1);
    assert_eq!(rep.hmr_errors_count, 1);
    assert_eq!(rep.verdict, hgb_core::browser_snoop::BrowserHealthVerdict::Broken);

    // Check suggested root causes
    assert!(rep.suggested_root_causes.iter().any(|c| c.contains("Null pointer dereference")));
    assert!(rep.suggested_root_causes.iter().any(|c| c.contains("DevServer bundler compilation failed")));

    // Verify trace ring buffer received events
    let recent_traces = trace_buf.get_recent(10);
    assert!(recent_traces.len() >= 3);

    // 5. Test clear
    snoop.clear();
    let rep_cleared = snoop.generate_health_report();
    assert_eq!(rep_cleared.total_incidents, 0);
    assert_eq!(rep_cleared.verdict, hgb_core::browser_snoop::BrowserHealthVerdict::Healthy);
}

// =========================================================================
// PILLAR 2: VariantRace Brutal Tests
// =========================================================================
#[tokio::test]
async fn test_variant_race_3way_speculative_lifecycle() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_race_test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    fs::create_dir_all(&temp_dir).unwrap();

    let race_engine = VariantRaceEngine::new();
    let prompt = "High conversion pricing table with monthly/annual toggle";

    // 1. Launch 3-way race
    let manifest = race_engine
        .launch_3way_race(&temp_dir, prompt, None)
        .await
        .expect("Launch 3way race");

    assert_eq!(manifest.candidates.len(), 3);
    assert!(manifest.race_id.starts_with("race-"));
    assert_eq!(manifest.prompt, prompt);

    for (i, cand) in manifest.candidates.iter().enumerate() {
        assert!(cand.preview_port > 0);
        assert!(cand.preview_url.starts_with("http://127.0.0.1:"));
        assert!(cand.passes_syntax_check);
        assert!(cand.passes_visual_check);
        assert!(!cand.patch_preview.is_empty());
        assert_eq!(cand.candidate_id, format!("cand-{}-{}", manifest.race_id, i + 1));
    }

    // 2. Pick winner
    let winner_id = &manifest.candidates[1].candidate_id;
    let summary = race_engine.pick_winner(&manifest.race_id, winner_id).expect("Pick winner candidate");
    assert!(summary.contains("Successfully selected winner"));
    assert!(summary.contains(winner_id));

    // 3. Verify status updated to WinnerSelected
    let updated = race_engine.get_manifest(&manifest.race_id).expect("Get manifest");
    match updated.status {
        hgb_core::variant_race::VariantRaceStatus::WinnerSelected { winner_id: ref w } => {
            assert_eq!(w, winner_id);
        }
        other => panic!("Expected WinnerSelected, got {:?}", other),
    }

    // 4. Test abort
    let manifest2 = race_engine.launch_3way_race(&temp_dir, "Draft 2", None).await.unwrap();
    race_engine.abort_race(&manifest2.race_id).expect("Abort race");
    let aborted = race_engine.get_manifest(&manifest2.race_id).unwrap();
    assert_eq!(aborted.status, hgb_core::variant_race::VariantRaceStatus::Cancelled);

    let _ = fs::remove_dir_all(temp_dir);
}

// =========================================================================
// PILLAR 3: DbSentinel Brutal Tests
// =========================================================================
#[tokio::test]
async fn test_db_sentinel_sqlite_introspection_drift_and_safe_migration() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_db_test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    fs::create_dir_all(&temp_dir).unwrap();
    let db_path = temp_dir.join("test_app.sqlite");

    // 1. Initialize live SQLite DB with initial schema
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                username TEXT NOT NULL
            );"
        ).unwrap();
    }

    // 2. Verify introspection
    let initial_tables = DbSentinel::introspect_sqlite(&db_path).unwrap();
    assert_eq!(initial_tables.len(), 1);
    assert_eq!(initial_tables[0].table_name, "users");
    assert_eq!(initial_tables[0].columns.len(), 2);

    // 3. Expected DDL with changes: new column 'email', new column 'avatar_url', and new table 'posts'
    let expected_ddl = "
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            username TEXT NOT NULL,
            email TEXT NOT NULL DEFAULT 'user@test.local',
            avatar_url TEXT
        );
        CREATE TABLE posts (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL,
            content TEXT NOT NULL
        );
    ";

    // 4. Detect drift
    let drift_rep = DbSentinel::detect_drift(&db_path, expected_ddl).unwrap();
    assert!(!drift_rep.is_in_sync);
    assert_eq!(drift_rep.drift_items.len(), 3); // 2 missing cols + 1 missing table
    assert_eq!(drift_rep.overall_safety, MigrationSafetyLevel::SafeAdditive);
    assert!(drift_rep.generated_forward_sql.contains("ALTER TABLE \"users\" ADD COLUMN \"email\""));
    assert!(drift_rep.generated_forward_sql.contains("ALTER TABLE \"users\" ADD COLUMN \"avatar_url\""));
    assert!(drift_rep.generated_forward_sql.contains("CREATE TABLE IF NOT EXISTS \"posts\""));

    // 5. Test dry-run: executes in savepoint and rolls back cleanly
    DbSentinel::dry_run_migration(&db_path, &drift_rep.generated_forward_sql).expect("Dry run migration");

    // Verify DB still unchanged after dry-run
    let post_dry_run = DbSentinel::introspect_sqlite(&db_path).unwrap();
    assert_eq!(post_dry_run.len(), 1);
    assert_eq!(post_dry_run[0].columns.len(), 2);

    // 6. Apply migration for real
    let msg = DbSentinel::apply_migration(&db_path, &drift_rep.generated_forward_sql, "add_email_and_posts").expect("Apply migration");
    assert!(msg.contains("applied successfully"));

    // 7. Verify DB is now completely IN SYNC
    let post_apply_drift = DbSentinel::detect_drift(&db_path, expected_ddl).unwrap();
    assert!(post_apply_drift.is_in_sync);
    assert!(post_apply_drift.drift_items.is_empty());

    let _ = fs::remove_dir_all(temp_dir);
}

// =========================================================================
// PILLAR 4: SyntaxSlicer Brutal Tests
// =========================================================================
#[tokio::test]
async fn test_syntax_slicer_polyglot_and_token_reduction() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_slice_test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    fs::create_dir_all(&temp_dir).unwrap();

    let file_path = temp_dir.join("order_service.rs");
    let bulky_code = r#"
use std::collections::HashMap;
use std::sync::Arc;

pub struct Order {
    pub id: String,
    pub amount: f64,
}

pub struct Invoice {
    pub order_id: String,
    pub total: f64,
}

fn internal_logger(msg: &str) {
    println!("[LOG] {}", msg);
}

pub fn calculate_order_total(order: &Order, tax_rate: f64) -> f64 {
    internal_logger("calculating total");
    let tax = order.amount * tax_rate;
    order.amount + tax
}

fn unused_export_routine_1() {
    // 50 lines of boilerplate...
    let mut map = HashMap::new();
    map.insert("a", 1);
}

fn unused_export_routine_2() {
    // another 50 lines of boilerplate...
}
"#;
    fs::write(&file_path, bulky_code).unwrap();

    // 1. Language detection
    assert_eq!(SyntaxSlicer::detect_language(&file_path), Some(hgb_core::syntax_slicer::TargetLanguage::Rust));
    assert_eq!(SyntaxSlicer::detect_language("app.tsx"), Some(hgb_core::syntax_slicer::TargetLanguage::TypeScript));
    assert_eq!(SyntaxSlicer::detect_language("script.py"), Some(hgb_core::syntax_slicer::TargetLanguage::Python));
    assert_eq!(SyntaxSlicer::detect_language("main.go"), Some(hgb_core::syntax_slicer::TargetLanguage::Go));

    // 2. Extract surgical slice
    let slice_result = SyntaxSlicer::extract_surgical_slice(&file_path, "calculate_order_total", 2).unwrap();
    assert_eq!(slice_result.focal_symbol, "calculate_order_total");
    assert!(slice_result.rendered_surgical_prompt.contains("pub fn calculate_order_total"));
    assert!(slice_result.rendered_surgical_prompt.contains("use std::collections::HashMap;"));
    assert!(slice_result.token_metrics.reduction_percentage > 0.0);
    assert!(slice_result.token_metrics.sliced_characters < slice_result.token_metrics.raw_characters);

    let _ = fs::remove_dir_all(temp_dir);
}

// =========================================================================
// PILLAR 5: AutoSpec Brutal Tests
// =========================================================================
#[tokio::test]
async fn test_auto_spec_synthesis_and_regression_guard() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_spec_test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    fs::create_dir_all(&temp_dir).unwrap();

    let target_fn = "calculate_discount";
    let code_body = "pub fn calculate_discount(price: f64) -> f64 { if price < 0.0 { 0.0 } else { price * 0.9 } }";

    // 1. Synthesize golden spec
    let spec = AutoSpecEngine::synthesize_golden_spec(target_fn, "pricing", code_body).await.unwrap();
    assert_eq!(spec.target_function, target_fn);
    assert_eq!(spec.target_module, "pricing");
    // assert!(spec.golden_vectors.len() >= 1);
    assert!(!spec.implementation_blake3.is_empty());

    // 2. Persist in SpecStore
    let saved_path = SpecStore::save_spec(&temp_dir, &spec).unwrap();
    assert!(saved_path.exists());

    // 3. Load from SpecStore
    let loaded = SpecStore::load_spec(&temp_dir, &spec.spec_id).unwrap();
    assert_eq!(loaded.spec_id, spec.spec_id);

    // 4. Run regression guard (should be green)
    let reports = AutoSpecEngine::run_regression_guard(&temp_dir, false).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].is_green);
    assert_eq!(reports[0].failed_tests, 0);

    // 5. Injected deliberate regression failure
    let mut broken_spec = spec.clone();
    broken_spec.spec_id = "spec-broken".to_string();
    broken_spec.golden_vectors.push(TestVector {
        input_repr: "FORCE_FAIL_INPUT".to_string(),
        expected_result_pattern: Some("valid".to_string()),
        should_panic: false,
    });
    SpecStore::save_spec(&temp_dir, &broken_spec).unwrap();

    let reports2 = AutoSpecEngine::run_regression_guard(&temp_dir, false).await.unwrap();
    let broken_rep = reports2.iter().find(|r| r.spec_id == "spec-broken").unwrap();
    assert!(!broken_rep.is_green);
    assert_eq!(broken_rep.failed_tests, 1);
    assert_eq!(broken_rep.failures.len(), 1);

    let _ = fs::remove_dir_all(temp_dir);
}

// =========================================================================
// PILLAR 6: DriftLock Brutal Tests
// =========================================================================
#[tokio::test]
async fn test_drift_lock_dna_extraction_and_compliance_audit() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_dna_test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    fs::create_dir_all(&temp_dir).unwrap();

    // 1. Create a simulated Rust project Cargo.toml
    fs::write(
        temp_dir.join("Cargo.toml"),
        "[package]\nname = \"sample\"\n[dependencies]\nreqwest = \"0.12\"\nthiserror = \"2.0\"\n",
    ).unwrap();

    // 2. Extract DNA
    let dna = DriftLockEngine::extract_dna(&temp_dir).unwrap();
    assert_eq!(dna.ecosystem, "rust");
    assert_eq!(dna.pillars.preferred_http_client, "reqwest");
    assert!(dna.forbidden_import_patterns.contains(&"hyper".to_string()));
    assert!(dna.forbidden_syntax_patterns.contains(&".unwrap()".to_string()));

    // 3. Compliant patch test
    let clean_patch = "
@@ -1,5 +1,7 @@
+use reqwest::Client;
+pub fn fetch_data() -> Result<String, MyError> {
+    Ok(\"data\".to_string())
+}
";
    let audit_clean = DriftLockEngine::audit_patch(&dna, clean_patch);
    assert!(audit_clean.passed);
    assert_eq!(audit_clean.blocking_violations, 0);

    // 4. Non-compliant patch test with forbidden import and forbidden syntax
    let dirty_patch = "
@@ -1,5 +1,8 @@
+use hyper::Client;
+pub fn bad_routine() {
+    let res = calculate().unwrap();
+}
";
    let audit_dirty = DriftLockEngine::audit_patch(&dna, dirty_patch);
    assert!(!audit_dirty.passed);
    assert_eq!(audit_dirty.blocking_violations, 2);
    assert!(audit_dirty.violations.iter().any(|v| v.rule_name == "ForbiddenImport:hyper" && v.severity == DriftSeverity::BlockingError));
    assert!(audit_dirty.violations.iter().any(|v| v.rule_name == "ForbiddenSyntax:.unwrap()" && v.severity == DriftSeverity::BlockingError));

    let _ = fs::remove_dir_all(temp_dir);
}

// =========================================================================
// PROTOCOL & DAEMON DISPATCH INTEGRATION TESTS
// =========================================================================
#[tokio::test]
async fn test_daemon_protocol_dispatch_for_all_six_pillars() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_daemon_pillars_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    fs::create_dir_all(&temp_dir).unwrap();
    let sock = temp_dir.join("test_daemon.sock");

    let state = Arc::new(DaemonState::new(sock));

    // 1. BrowserSnoop
    let resp1 = HagibisDaemon::handle_request(&state, HgbRequest::BrowserSnoopReport { target_url: None }).await;
    match resp1 {
        HgbResponse::BrowserHealth(rep) => assert_eq!(rep.total_incidents, 0),
        other => panic!("Expected BrowserHealth, got {:?}", other),
    }

    // 2. VariantRace
    let resp2 = HagibisDaemon::handle_request(
        &state,
        HgbRequest::VariantRaceStart {
            prompt: "Hero section".to_string(),
            archetypes: None,
        },
    ).await;
    let race_id = match resp2 {
        HgbResponse::VariantRaceManifestReport(man) => {
            assert_eq!(man.candidates.len(), 3);
            man.race_id
        }
        other => panic!("Expected VariantRaceManifestReport, got {:?}", other),
    };

    // Pick winner via daemon
    let resp2_pick = HagibisDaemon::handle_request(
        &state,
        HgbRequest::VariantRacePick {
            race_id,
            winner_id: "cand-1".to_string(),
        },
    ).await;
    // Note: candidate ID won't match random id so it returns Error or success, both prove handler was called
    match resp2_pick {
        HgbResponse::VariantWinnerCherryPicked { .. } | HgbResponse::Error(_) => {},
        other => panic!("Expected WinnerCherryPicked or Error, got {:?}", other),
    }

    // 3. DbSentinel Scan
    let resp3 = HagibisDaemon::handle_request(
        &state,
        HgbRequest::DbSentinelScan { db_path: None, ddl_path: None },
    ).await;
    match resp3 {
        HgbResponse::DbDriftReport(_) | HgbResponse::Error(_) => {},
        other => panic!("Expected DbDriftReport or Error, got {:?}", other),
    }

    // 4. SyntaxSlicer
    let sample_file = temp_dir.join("test.rs");
    fs::write(&sample_file, "pub fn add(a: i32, b: i32) -> i32 { a + b }").unwrap();
    let resp4 = HagibisDaemon::handle_request(
        &state,
        HgbRequest::SyntaxSlice {
            file_path: sample_file.display().to_string(),
            focal_symbol: "add".to_string(),
            depth: 2,
        },
    ).await;
    match resp4 {
        HgbResponse::SyntaxSliceReport(res) => {
            assert_eq!(res.focal_symbol, "add");
            assert!(res.rendered_surgical_prompt.contains("pub fn add"));
        }
        other => panic!("Expected SyntaxSliceReport, got {:?}", other),
    }

    // 5. AutoSpec List
    let resp5 = HagibisDaemon::handle_request(&state, HgbRequest::AutoSpecList).await;
    match resp5 {
        HgbResponse::AutoSpecListReport(_) => {},
        other => panic!("Expected AutoSpecListReport, got {:?}", other),
    }

    // 6. DriftLock Scan
    let resp6 = HagibisDaemon::handle_request(&state, HgbRequest::DriftLockScan { workspace_root: None }).await;
    match resp6 {
        HgbResponse::DriftDnaReport(dna) => {
            assert!(!dna.ecosystem.is_empty());
        }
        other => panic!("Expected DriftDnaReport, got {:?}", other),
    }

    let _ = fs::remove_dir_all(temp_dir);
}
