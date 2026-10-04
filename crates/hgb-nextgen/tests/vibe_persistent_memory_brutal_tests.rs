//! # Brutal Integration Tests for Hagibis Active Persistent Cross-Session Memory
//!
//! Validates:
//! 1. Automatic system prompt memory anchor injection (<project_memory> and <style_guidance>)
//! 2. Agent tool execution: record_memory persisting ADRs and technical debts to .hgb/memory.json
//! 3. Agent tool execution: search_memory finding relevant past ADRs, debts, and style feedback
//! 4. Agent tool execution: record_style_feedback storing accepted conventions and rejected anti-patterns in SQLite StyleMemoryVault
//! 5. Cross-session simulation: Session 1 records a fix -> Session 2 initializes with fix automatically injected in prompt anchor
//! 6. Cockpit TUI integration: /memory and /mem slash commands (status, record, search) and Ratatui Canvas card rendering

use async_trait::async_trait;
use hgb_core::agent::{AgentLoopConfig, ReActAgentEngine};
use hgb_core::memory::{DebtSeverity, ProjectMemoryLedger};
use hgb_core::style::StyleMemoryVault;
use hgb_core::traits::HgbProvider;
use hgb_core::Result;
use hgb_nextgen::cockpit::{CockpitItem, CockpitState, CockpitVibeManager, MemoryCardItem};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

struct ProxyProvider;

#[async_trait]
impl HgbProvider for ProxyProvider {
    fn name(&self) -> &str {
        "proxy_memory_provider"
    }

    async fn complete(&self, _prompt: &str, _model: Option<&str>) -> Result<String> {
        Ok("Proxy completed.".to_string())
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hgb_mem_{}_{}", prefix, nanos));
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn test_1_auto_system_prompt_memory_anchor_injection() {
    let tmp = TestDir::new("prompt_inject");
    let ws = tmp.path().to_path_buf();

    // Initialize ledger and vault with records
    let mut ledger = ProjectMemoryLedger::load_or_init(&ws).expect("init ledger");
    ledger.record_decision(
        "Adopt Zero-Cost Telepathy Embeddings",
        "Use Blake3 + token frequency vectors for instant zero-LLM search",
        "Reduce token billing latency to 0ms"
    ).expect("record decision");

    ledger.record_tech_debt(
        "Ollama live streaming chunking",
        "Stream chunks may split UTF-8 boundaries",
        DebtSeverity::High,
        vec!["crates/hgb-core/src/providers/ollama.rs".to_string()]
    ).expect("record debt");

    let vault_path = ws.join(".hgb").join("style_vault.db");
    let vault = StyleMemoryVault::new(&vault_path).expect("init vault");
    vault.record_feedback("use async-trait for provider implementations", true).expect("record accepted");
    vault.record_feedback("std::sync::Mutex inside async handlers", false).expect("record rejected");

    let provider = Arc::new(ProxyProvider);
    let config = AgentLoopConfig {
        workspace_root: ws.clone(),
        ..Default::default()
    };
    let engine = ReActAgentEngine::new(provider, config);

    // Call build_system_prompt with None (triggering auto-discovery)
    let prompt = engine.build_system_prompt(None, None, None, None);

    // Assertions
    assert!(prompt.contains("<project_memory"), "System prompt must contain <project_memory>");
    assert!(prompt.contains("</project_memory>"), "System prompt must close </project_memory>");
    assert!(prompt.contains("ADR-001"), "Prompt must include ADR-001");
    assert!(prompt.contains("Adopt Zero-Cost Telepathy Embeddings"), "Prompt must include ADR title");
    assert!(prompt.contains("DEBT-001"), "Prompt must include DEBT-001");
    assert!(prompt.contains("Ollama live streaming chunking"), "Prompt must include debt title");

    assert!(prompt.contains("<style_guidance>"), "System prompt must contain <style_guidance>");
    assert!(prompt.contains("</style_guidance>"), "System prompt must close </style_guidance>");
    assert!(prompt.contains("async-trait for provider implementations"), "Prompt must include accepted convention");
    assert!(prompt.contains("std::sync::Mutex inside async handlers"), "Prompt must include rejected anti-pattern");

    // Also verify tool descriptions 10, 11, 12 appear in prompt
    assert!(prompt.contains("record_memory"));
    assert!(prompt.contains("search_memory"));
    assert!(prompt.contains("record_style_feedback"));
}

#[tokio::test]
async fn test_2_agent_executes_record_memory_tool() {
    let tmp = TestDir::new("record_memory");
    let ws = tmp.path().to_path_buf();

    let provider = Arc::new(ProxyProvider);
    let config = AgentLoopConfig {
        workspace_root: ws.clone(),
        ..Default::default()
    };
    let engine = ReActAgentEngine::new(provider, config);

    // 1. Record an ADR via tool call
    let adr_args = json!({
        "title": "Adopt SQLite WAL Mode for Vaults",
        "decision": "PRAGMA journal_mode = WAL for concurrent reads",
        "context": "Avoid SQLite database busy lock contention"
    });
    let adr_res = engine.execute_tool("record_memory", &adr_args).await.expect("execute record_memory ADR");
    assert!(adr_res.contains("ADR-001"));
    assert!(adr_res.contains("Adopt SQLite WAL Mode for Vaults"));

    // 2. Record a Tech Debt via tool call
    let debt_args = json!({
        "title": "Hardcoded UDS socket backlog",
        "decision": "Backlog fixed to 128 connections",
        "severity": "Medium",
        "is_technical_debt": true,
        "affected_files": ["crates/hgb-daemon/src/server.rs"]
    });
    let debt_res = engine.execute_tool("record_memory", &debt_args).await.expect("execute record_memory debt");
    assert!(debt_res.contains("DEBT-001"));
    assert!(debt_res.contains("Hardcoded UDS socket backlog"));

    // 3. Verify .hgb/memory.json file exists on disk and reloads cleanly
    let memory_file = ws.join(".hgb").join("memory.json");
    assert!(memory_file.exists(), ".hgb/memory.json must be written to disk");

    let ledger = ProjectMemoryLedger::load_or_init(&ws).expect("reload ledger from disk");
    assert_eq!(ledger.doc.decisions.len(), 1);
    assert_eq!(ledger.doc.tech_debt.len(), 1);
    assert_eq!(ledger.doc.decisions[0].id, "ADR-001");
    assert_eq!(ledger.doc.tech_debt[0].id, "DEBT-001");
}

#[tokio::test]
async fn test_3_agent_executes_search_memory_tool() {
    let tmp = TestDir::new("search_memory");
    let ws = tmp.path().to_path_buf();

    let mut ledger = ProjectMemoryLedger::load_or_init(&ws).expect("init ledger");
    ledger.record_decision(
        "Bincode Compact Daemon IPC",
        "Use bincode serializer over Unix domain socket",
        "JSON serialization was 4x slower in benchmarks"
    ).expect("record decision");

    ledger.record_tech_debt(
        "Missing TLS on remote agent bridges",
        "Localhost bridges are plaintext; remote bridges need TLS v1.3",
        DebtSeverity::High,
        vec!["crates/hgb-daemon/src/bridge.rs".to_string()]
    ).expect("record debt");

    let vault_path = ws.join(".hgb").join("style_vault.db");
    let vault = StyleMemoryVault::new(&vault_path).expect("init vault");
    vault.record_feedback("prefer Option::map_or_else over nested match", true).expect("feedback");

    let provider = Arc::new(ProxyProvider);
    let config = AgentLoopConfig {
        workspace_root: ws.clone(),
        ..Default::default()
    };
    let engine = ReActAgentEngine::new(provider, config);

    // Search for decision
    let search_adr_args = json!({ "query": "Bincode" });
    let adr_output = engine.execute_tool("search_memory", &search_adr_args).await.expect("search ADR");
    assert!(adr_output.contains("ADR-001"));
    assert!(adr_output.contains("Bincode Compact Daemon IPC"));

    // Search for debt
    let search_debt_args = json!({ "query": "TLS" });
    let debt_output = engine.execute_tool("search_memory", &search_debt_args).await.expect("search debt");
    assert!(debt_output.contains("DEBT-001"));
    assert!(debt_output.contains("Missing TLS on remote agent bridges"));

    // Search for style feedback
    let search_style_args = json!({ "query": "map_or_else" });
    let style_output = engine.execute_tool("search_memory", &search_style_args).await.expect("search style");
    assert!(style_output.contains("ACCEPTED PATTERN"));
    assert!(style_output.contains("map_or_else"));

    // Search for nonexistent term
    let search_none_args = json!({ "query": "quantum_flux_capacitor" });
    let none_output = engine.execute_tool("search_memory", &search_none_args).await.expect("search none");
    assert!(none_output.contains("No memory entries found"));
}

#[tokio::test]
async fn test_4_agent_executes_record_style_feedback_tool() {
    let tmp = TestDir::new("test");
    let ws = tmp.path().to_path_buf();

    let provider = Arc::new(ProxyProvider);
    let config = AgentLoopConfig {
        workspace_root: ws.clone(),
        ..Default::default()
    };
    let engine = ReActAgentEngine::new(provider, config);

    // 1. Record accepted style
    let acc_args = json!({
        "snippet": "derive(Debug, Clone, Serialize, Deserialize) on all DTOs",
        "accepted": true
    });
    let acc_res = engine.execute_tool("record_style_feedback", &acc_args).await.expect("record accepted");
    assert!(acc_res.contains("Accepted Convention"));

    // 2. Record rejected anti-pattern
    let rej_args = json!({
        "snippet": "eprintln! in core library crates (use tracing/log instead)",
        "accepted": false
    });
    let rej_res = engine.execute_tool("record_style_feedback", &rej_args).await.expect("record rejected");
    assert!(rej_res.contains("Rejected Anti-Pattern"));

    // 3. Verify SQLite StyleMemoryVault directly
    let vault_path = ws.join(".hgb").join("style_vault.db");
    assert!(vault_path.exists(), "style_vault.db must exist on disk");

    let vault = StyleMemoryVault::new(&vault_path).expect("open vault");
    let (acc_count, rej_count) = vault.get_feedback_count().expect("counts");
    assert_eq!(acc_count, 1);
    assert_eq!(rej_count, 1);

    let guidance = vault.render_prompt_guidance(5);
    assert!(guidance.contains("<style_guidance>"));
    assert!(guidance.contains("derive(Debug, Clone, Serialize, Deserialize)"));
    assert!(guidance.contains("eprintln! in core library crates"));
}

#[tokio::test]
async fn test_5_cross_session_memory_persistence_simulation() {
    let tmp = TestDir::new("test");
    let ws = tmp.path().to_path_buf();

    // SESSION 1: Agent initializes and records a critical architectural fix
    {
        let provider = Arc::new(ProxyProvider);
        let config = AgentLoopConfig {
            workspace_root: ws.clone(),
            ..Default::default()
        };
        let session1_engine = ReActAgentEngine::new(provider, config);

        let fix_args = json!({
            "title": "Atomic File Renaming for State Dumps",
            "decision": "Write to .tmp then rename atomically to prevent corrupt partial writes",
            "context": "Power loss or SIGKILL corrupted state in earlier versions"
        });
        session1_engine.execute_tool("record_memory", &fix_args).await.expect("session 1 record fix");

        let style_args = json!({
            "snippet": "prefer std::fs::rename over copy+remove for atomic writes",
            "accepted": true
        });
        session1_engine.execute_tool("record_style_feedback", &style_args).await.expect("session 1 record style");
    } // Session 1 terminates completely, memory dropped

    // SESSION 2: New agent initializes in the same workspace (no prompt passed, clean slate)
    {
        let provider = Arc::new(ProxyProvider);
        let config = AgentLoopConfig {
            workspace_root: ws.clone(),
            ..Default::default()
        };
        let session2_engine = ReActAgentEngine::new(provider, config);

        // System prompt automatically auto-discovers memory from disk!
        let session2_prompt = session2_engine.build_system_prompt(None, None, None, None);

        // Verify Session 2 has Session 1's architectural fix and style preference in its anchor
        assert!(session2_prompt.contains("<project_memory"), "Session 2 must auto-inject <project_memory>");
        assert!(session2_prompt.contains("ADR-001"), "Session 2 must know ADR-001");
        assert!(session2_prompt.contains("Atomic File Renaming for State Dumps"), "Session 2 must know title");
        assert!(session2_prompt.contains("Write to .tmp then rename atomically"), "Session 2 must know decision");

        assert!(session2_prompt.contains("<style_guidance>"), "Session 2 must auto-inject <style_guidance>");
        assert!(session2_prompt.contains("prefer std::fs::rename over copy+remove"), "Session 2 must know style preference");
    }
}

#[tokio::test]
async fn test_6_cockpit_slash_command_and_canvas_card_rendering() {
    let tmp = TestDir::new("test");
    let ws = tmp.path().to_path_buf();

    // Pre-populate memory
    let mut ledger = ProjectMemoryLedger::load_or_init(&ws).expect("init ledger");
    ledger.record_decision(
        "Adopt Systems-Grade Microkernel",
        "Replace wrapper scripts with native Rust async daemon",
        "Sub-millisecond UDS roundtrip requirement"
    ).expect("record decision");

    // Change current directory to temp workspace for Cockpit command execution
    let orig_dir = std::env::current_dir().unwrap();
    let _ = std::env::set_current_dir(&ws);

    // 1. Test /mem status command
    let status_card = CockpitVibeManager::handle_vibe_slash_command("/mem", "status").await;
    assert!(status_card.is_some(), "/mem status must produce a card");
    if let Some(CockpitItem::MemoryCard(card)) = status_card {
        assert_eq!(card.action, "STATUS");
        assert_eq!(card.total_decisions, 1);
        assert!(card.decision.contains("ADR-001"));
    } else {
        panic!("Expected CockpitItem::MemoryCard");
    }

    // 2. Test /mem record command
    let rec_card = CockpitVibeManager::handle_vibe_slash_command(
        "/mem",
        "record Distributed Telemetry Ring Buffer => Zero-allocation bounded memory buffer"
    ).await;
    assert!(rec_card.is_some(), "/mem record must produce a card");
    if let Some(CockpitItem::MemoryCard(card)) = rec_card {
        assert_eq!(card.action, "RECORD");
        assert!(card.title.contains("Distributed Telemetry Ring Buffer"));
        assert_eq!(card.decision, "Zero-allocation bounded memory buffer");
        assert_eq!(card.total_decisions, 2);
    } else {
        panic!("Expected CockpitItem::MemoryCard");
    }

    // 3. Test /mem search command
    let search_card = CockpitVibeManager::handle_vibe_slash_command("/mem", "search Microkernel").await;
    assert!(search_card.is_some(), "/mem search must produce a card");
    if let Some(CockpitItem::MemoryCard(card)) = search_card {
        assert_eq!(card.action, "SEARCH");
        assert!(card.decision.contains("Adopt Systems-Grade Microkernel"));
    } else {
        panic!("Expected CockpitItem::MemoryCard");
    }

    // 4. Test Ratatui Canvas rendering of MemoryCardItem
    let mut state = CockpitState::new();
    state.memory_cards.push(MemoryCardItem {
        title: "Ratatui Visual Memory Card".to_string(),
        action: "STATUS".to_string(),
        decision: "Full in-canvas persistent memory display".to_string(),
        context: "2 ADRs tracked in active ledger".to_string(),
        total_decisions: 2,
        total_debts: 0,
    });

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("create terminal");
    terminal.draw(|f| state.render_chat_canvas(f)).expect("draw chat canvas");

    let buf = terminal.backend().buffer().clone();
    let mut rendered_lines = Vec::new();
    for y in 0..30 {
        let mut row_str = String::new();
        for x in 0..100 {
            let symbol = buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ");
            row_str.push_str(symbol);
        }
        rendered_lines.push(row_str);
    }
    let text_content = rendered_lines.join("\n");

    assert!(text_content.contains("Living Project Memory"), "Canvas must render memory card header");
    assert!(text_content.contains("Ratatui Visual Memory Card"), "Canvas must render card title");
    assert!(text_content.contains("Full in-canvas persistent memory display"), "Canvas must render decision");
    assert!(text_content.contains("ADRs: 2"), "Canvas must render total ADRs");

    // Also verify render_chat_view alias works identically
    terminal.draw(|f| state.render_chat_view(f)).expect("draw chat view");

    // Restore working directory
    let _ = std::env::set_current_dir(&orig_dir);
}
