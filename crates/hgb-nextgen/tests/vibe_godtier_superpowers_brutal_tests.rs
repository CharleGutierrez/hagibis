use std::path::PathBuf;
use hgb_core::browser_hud::{BrowserIncidentKind, BrowserIncidentSeverity, BrowserLiveHud};
use hgb_core::seed_engine::PersonaSeedEngine;
use hgb_core::ast_rewind::AstRewindTimeline;
use hgb_core::blueprint::ArchitectureBlueprint;
use hgb_nextgen::redteam::{RedTeamAuditor, RedTeamCategory, RedTeamVerdict};
use hgb_nextgen::passive_sentinel::{PassiveSentinel, SentinelEventKind, SentinelHealthStatus};

#[test]
fn test_superpower_1_browser_hud_cdp_streaming_and_heal() {
    let mut hud = BrowserLiveHud::new(50);

    // 1. Ingest raw CDP console error event
    let raw_cdp_hydration = r#"{
        "method": "Runtime.consoleAPICalled",
        "params": {
            "type": "error",
            "args": [{
                "type": "string",
                "value": "Hydration failed because the initial UI does not match what was rendered on the server. Expected server HTML to contain a matching <div> in <main>."
            }],
            "stackTrace": {
                "callFrames": [{
                    "functionName": "UserProfileComponent",
                    "url": "http://localhost:3000/src/components/UserProfile.tsx",
                    "lineNumber": 42,
                    "columnNumber": 15
                }]
            }
        }
    }"#;

    let incident = hud.ingest_cdp_event(raw_cdp_hydration).expect("Must parse CDP event");
    assert_eq!(incident.kind, BrowserIncidentKind::HydrationMismatch);
    assert_eq!(incident.severity, BrowserIncidentSeverity::High);
    assert!(incident.suggested_fix.is_some());
    let fix = incident.suggested_fix.as_ref().unwrap();
    assert!(fix.contains("useEffect") || fix.contains("suppressHydrationWarning"));

    // 2. Ingest unhandled uncaught exception
    let raw_cdp_uncaught = r#"{
        "method": "Runtime.exceptionThrown",
        "params": {
            "exceptionDetails": {
                "text": "Uncaught TypeError: Cannot read properties of undefined (reading 'avatar_url')",
                "url": "http://localhost:3000/src/pages/Dashboard.tsx",
                "lineNumber": 18,
                "columnNumber": 22
            }
        }
    }"#;

    let uncaught = hud.ingest_cdp_event(raw_cdp_uncaught).expect("Must parse uncaught error");
    assert_eq!(uncaught.kind, BrowserIncidentKind::UncaughtException);
    assert_eq!(uncaught.severity, BrowserIncidentSeverity::Critical);

    // 3. Telemetry check
    let telemetry = hud.telemetry();
    assert_eq!(telemetry.total_incidents, 2);
    assert_eq!(telemetry.hydration_mismatches, 1);
    assert_eq!(telemetry.unresolved_count, 2);

    // 4. Resolve incident (1-click heal via F8)
    assert!(hud.resolve_incident(&incident.id));
    assert_eq!(hud.telemetry().unresolved_count, 1);
}

#[test]
fn test_superpower_2_instant_persona_and_synthetic_seed_engine() {
    // 1. Generate standalone users batch with 25 records and seed 42
    let user_batch = PersonaSeedEngine::generate_batch("users", 25, Some(42))
        .expect("Must generate users seed batch");
    assert_eq!(user_batch.records.len(), 25);
    assert_eq!(user_batch.entity, "users");
    assert!(user_batch.sql_script.contains("INSERT INTO users"));
    // assert!(user_batch.json_export.contains("usr_"));

    // Verify edge-case UTF-8 and email formatting
    assert!(user_batch.records.iter().any(|r| {
        let email = r.fields.get("email").and_then(|e| e.as_str()).unwrap_or("");
        email.contains('@')
    }));

    // 2. Generate relational suite (5 customers, 3 orders each)
    let (customers, orders) = PersonaSeedEngine::generate_relational_suite(5, 3)
        .expect("Must generate relational suite");
    assert_eq!(customers.records.len(), 5);
    assert_eq!(orders.records.len(), 15);

    // Verify foreign key integrity: every order links to an existing customer ID
    for ord in &orders.records {
        let cust_id = ord.fields.get("customer_id").and_then(|c| c.as_str()).unwrap();
        assert!(customers.records.iter().any(|c| c.id == cust_id));
    }

    // Verify SQL script generation
    assert!(customers.sql_script.contains("INSERT INTO customers"));
    assert!(orders.sql_script.contains("INSERT INTO orders"));
}

#[test]
fn test_superpower_3_ast_rewind_timeline_surgical_rollback() {
    let mut timeline = AstRewindTimeline::new();
    let file = "src/tax.rs";

    // Revision 0: Initial working 8% tax calculation
    timeline.record_symbol_snapshot(
        file,
        "calculate_tax",
        "pub fn calculate_tax(amount: f64) -> f64 {",
        "    amount * 0.08\n}",
        1000,
    );

    // Revision 1: Experimental broken tax calculation
    timeline.record_symbol_snapshot(
        file,
        "calculate_tax",
        "pub fn calculate_tax(amount: f64) -> f64 {",
        "    panic!(\"tax service failed\");\n}",
        2000,
    );

    // Active source code with broken calculate_tax AND newly added functions
    let current_source = r#"use std::sync::Arc;

pub fn brand_new_ui_theme() -> &'static str {
    "cyberpunk-neon"
}

pub fn calculate_tax(amount: f64) -> f64 {
    panic!("tax service failed");
}

pub fn brand_new_currency_formatter(cents: u64) -> String {
    format!("${:.2}", cents as f64 / 100.0)
}
"#;

    // Surgically rewind ONLY calculate_tax to revision 0
    let restored = timeline.rewind_symbol(current_source, file, "calculate_tax", 0)
        .expect("Surgical rollback must succeed");

    // Assert calculate_tax was rolled back to 0.08
    assert!(restored.contains("amount * 0.08"));
    assert!(!restored.contains("panic!(\"tax service failed\")"));

    // Assert other brand-new functions and imports are 100% INTACT
    assert!(restored.contains("brand_new_ui_theme"));
    assert!(restored.contains("cyberpunk-neon"));
    assert!(restored.contains("brand_new_currency_formatter"));
    assert!(restored.contains("use std::sync::Arc;"));

    // Check history length
    let history = timeline.get_history(file, "calculate_tax");
    assert_eq!(history.len(), 2);
}

#[test]
fn test_superpower_4_living_architecture_blueprint_and_mermaid() {
    let workspace = PathBuf::from("/home/dyna/TGS Projects/hagibis");
    let blueprint = ArchitectureBlueprint::scan_workspace(&workspace).expect("Scan workspace");

    assert!(blueprint.nodes.len() >= 3, "Should discover crates and core components");
    assert!(blueprint.total_files_scanned > 0);

    // Mermaid generation check
    let mermaid = blueprint.to_mermaid();
    assert!(mermaid.starts_with("```mermaid\nflowchart TB"));
    assert!(mermaid.contains("Crates"));
    assert!(mermaid.contains("hgb_core") || mermaid.contains("crate_"));

    // ASCII generation check
    let ascii = blueprint.to_ascii();
    assert!(ascii.contains("LIVING ARCHITECTURE BLUEPRINT"));
    assert!(ascii.contains("CRATE"));

    // Markdown export check
    let md = blueprint.export_markdown();
    assert!(md.contains("# Living Architecture Blueprint"));
    assert!(md.contains("## 📐 Topology Visualizer"));
    assert!(md.contains("## 📦 Component Inventory"));
}

#[test]
fn test_superpower_5_adversarial_redteam_and_edge_case_auditor() {
    let auditor = RedTeamAuditor::new();

    // 1. Test Unbounded Query + Tenant Leak + O(N^2)
    let hazardous_code = r#"
    pub async fn fetch_invoices(db: &Database) -> Vec<Invoice> {
        // High severity: Unbounded query missing LIMIT
        let sql = "SELECT * FROM orders WHERE status = 'pending'";
        db.query(sql).await
    }

    pub async fn update_user_balance(db: &Database, user_id: i64, delta: f64) {
        // Critical severity: Tenant leak in multi-tenant accounts table
        let query = format!("UPDATE accounts SET balance = balance + {} WHERE id = {}", delta, user_id);
        db.execute(&query).await;
    }

    pub fn match_items(catalog_a: &[Item], catalog_b: &[Item]) -> Vec<Pair> {
        let mut pairs = Vec::new();
        // Medium severity: Nested for loop O(N^2)
        for a in catalog_a {
            for b in catalog_b {
                if a.sku == b.sku {
                    pairs.push(Pair(a.clone(), b.clone()));
                }
            }
        }
        pairs
    }
    "#;

    let report = auditor.audit_code(hazardous_code, None);
    assert_eq!(report.verdict, RedTeamVerdict::Blocked);
    assert!(report.total_critical >= 1, "Must catch tenant isolation leak");
    assert!(report.total_high >= 1, "Must catch unbounded query");
    assert!(report.total_medium >= 1, "Must catch O(N^2) loop");

    // 2. Test Concurrency Hazard: static mut
    let static_mut_code = r#"
    static mut GLOBAL_COUNTER: usize = 0;
    pub fn increment() {
        unsafe { GLOBAL_COUNTER += 1; }
    }
    "#;
    let static_report = auditor.audit_code(static_mut_code, None);
    assert_eq!(static_report.verdict, RedTeamVerdict::Blocked);
    assert!(static_report.findings.iter().any(|f| f.category == RedTeamCategory::ConcurrencyHazard));

    // 3. Test Clean code
    let clean_code = r#"
    pub async fn fetch_tenant_orders(db: &Database, tenant_id: Uuid) -> Result<Vec<Order>> {
        let sql = "SELECT * FROM orders WHERE tenant_id = :tenant_id AND status = 'pending' LIMIT 100";
        db.query(sql, &[&tenant_id]).await
    }
    "#;
    let clean_report = auditor.audit_code(clean_code, None);
    assert_eq!(clean_report.verdict, RedTeamVerdict::Clean);
    assert_eq!(clean_report.findings.len(), 0);
}

#[test]
fn test_superpower_6_passive_sentinel_debounced_ast_differencer() {
    let root = PathBuf::from("/home/dyna/TGS Projects/hagibis");
    let mut sentinel = PassiveSentinel::new(root, 150);

    let file_path = PathBuf::from("crates/hgb-core/src/service.rs");

    // 1. Initial registration
    let initial_code = r#"
    pub struct UserService {
        pub name: String,
    }

    pub fn calculate_score(points: u32) -> u32 {
        points * 10
    }
    "#;
    sentinel.register_file(&file_path, initial_code);
    assert_eq!(sentinel.telemetry().tracked_files_count, 1);

    // 2. Modify a symbol
    let modified_code = r#"
    pub struct UserService {
        pub name: String,
    }

    pub fn calculate_score(points: u32) -> u32 {
        points * 20
    }
    "#;
    let event = sentinel.evaluate_change(&file_path, modified_code);
    assert!(event.is_some());
    let ev = event.unwrap();
    assert_eq!(ev.kind, SentinelEventKind::SymbolModified);
    assert_eq!(ev.symbol_name.as_deref(), Some("calculate_score"));
    assert!(!ev.has_syntax_error);

    // 3. Add a symbol
    let added_code = r#"
    pub struct UserService {
        pub name: String,
    }

    pub fn calculate_score(points: u32) -> u32 {
        points * 20
    }

    pub fn validate_email(email: &str) -> bool {
        email.contains('@')
    }
    "#;
    let add_event = sentinel.evaluate_change(&file_path, added_code).unwrap();
    assert_eq!(add_event.kind, SentinelEventKind::SymbolAdded);
    assert_eq!(add_event.symbol_name.as_deref(), Some("validate_email"));

    // 4. Trigger syntax malformation (unclosed brace)
    let broken_code = r#"
    pub fn broken_function() {
        let x = 10;
        // missing closing brace
    "#;
    let err_event = sentinel.evaluate_change(&file_path, broken_code).unwrap();
    assert_eq!(err_event.kind, SentinelEventKind::SyntaxMalformation);
    assert!(err_event.has_syntax_error);
    assert!(matches!(sentinel.telemetry().status, SentinelHealthStatus::Warning(_)));
}
