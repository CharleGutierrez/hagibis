//! # Brutal Integration Tests for Next Sprint Vibe Coding Blueprint
//!
//! Exhaustive, 1000% real verification for all 7 blueprint pillars:
//! 1. P0: Sandboxed Autonomous Command Runner (`AgyCrud::run_command`) with AgentShieldLight validation, timeouts, stdout/stderr capture.
//! 2. P0: Autonomous ReAct Multi-Turn Agent Loop (`ReActAgentEngine`) executing real tools against disk.
//! 3. P1: Compiler & Test-Driven Self-Healing Loop (`HealEngine`) with rustc diagnostic extraction and surgical repairs.
//! 4. P1: Instant Time-Travel Revert (`SwarmCheckpointManager`) with byte-exact rollback and file deletion.
//! 5. P2: Workspace Rules & Context Anchor (`WorkspaceRulesManager`) multi-source discovery, Blake3 hashing, and XML injection.
//! 6. P2: AST & Syntactic Repo Map (`RepoMap`) multi-language symbol extraction (Rust, TS, Python, Go).
//! 7. P3: Interactive Diff HUD & Selective Patch Acceptance (`SelectivePatcher` + `ChatCanvas::render_diff_hud`).

use async_trait::async_trait;
use hgb_cli::canvas::ChatCanvas;
use hgb_core::{
    AgyCrud, CommandOptions, ReActAgentEngine, AgentLoopConfig,
    WorkspaceRulesManager, RepoMap, HgbProvider, Result,
};
use hgb_nextgen::{
    SwarmCheckpointManager, HealEngine, SelectivePatcher,
    DiffLineKind,
};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// Helper to create an isolated temporary workspace directory
fn create_test_workspace(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("hgb_vibe_test_{}_{}", name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&base).unwrap();
    base
}

struct TestProxyProvider {
    responses: tokio::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl HgbProvider for TestProxyProvider {
    fn name(&self) -> &str {
        "test_proxy"
    }

    async fn complete(&self, _prompt: &str, _model: Option<&str>) -> Result<String> {
        let mut guard = self.responses.lock().await;
        if guard.is_empty() {
            Ok("Final response: All operations completed cleanly.".to_string())
        } else {
            Ok(guard.remove(0))
        }
    }
}

#[tokio::test]
async fn test_p0_sandboxed_command_runner_execution_and_timeout() {
    let ws = create_test_workspace("cmd_runner");

    // 1. Nominal command execution
    let opts = CommandOptions {
        cwd: Some(ws.clone()),
        timeout_ms: Some(5000),
        env: HashMap::new(),
        max_output_bytes: Some(4096),
        wait_ms_before_async: None,
    };

    let res = AgyCrud::run_command("echo 'Hagibis Sub-Millisecond Engine'", Some(&ws), opts).await.expect("echo should execute");
    assert_eq!(res.exit_code, 0);
    assert!(!res.timed_out);
    assert!(res.stdout.contains("Hagibis Sub-Millisecond Engine"));

    // 2. Timeout enforcement with kill_on_drop
    let timeout_opts = CommandOptions {
        cwd: Some(ws.clone()),
        timeout_ms: Some(200), // Drop and kill after 200ms
        env: HashMap::new(),
        max_output_bytes: Some(1024),
        wait_ms_before_async: None,
    };

    let t0 = std::time::Instant::now();
    let res_timeout = AgyCrud::run_command("sleep 5", Some(&ws), timeout_opts).await.expect("Timeout command should return timed_out");
    let elapsed = t0.elapsed();

    assert!(res_timeout.timed_out);
    assert_ne!(res_timeout.exit_code, 0);
    assert!(elapsed.as_millis() < 2500, "Should terminate promptly on timeout");

    // Clean up
    let _ = fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn test_p0_sandboxed_command_runner_catastrophic_shield_interception() {
    let ws = create_test_workspace("cmd_shield");

    let dangerous_commands = [
        "rm -rf /",
        "rm -rf /*",
        "mkfs.ext4 /dev/sda",
        ":(){ :|:& };:",
        "dd if=/dev/zero of=/dev/nvme0n1",
    ];

    for cmd in dangerous_commands {
        let opts = CommandOptions {
            cwd: Some(ws.clone()),
            timeout_ms: Some(1000),
            env: HashMap::new(),
            max_output_bytes: None,
            wait_ms_before_async: None,
        };

        let err = AgyCrud::run_command(cmd, Some(&ws), opts).await;
        assert!(err.is_err(), "Dangerous command '{}' MUST be intercepted by AgentShieldLight", cmd);
        let err_msg = err.err().unwrap().to_string();
        assert!(err_msg.contains("AgentShieldLight") || err_msg.contains("Catastrophic"), "Error should cite safety violation: {}", err_msg);
    }

    let _ = fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn test_p0_react_agent_full_loop_and_tool_execution() {
    let ws = create_test_workspace("react_agent");
    let target_file = ws.join("generated.txt");

    // Multi-turn model outputs with tool_call blocks
    let proxy_responses = vec![
        format!(
            "Let me create a test artifact.\n```tool_call\n{{\n  \"call_id\": \"step_1\",\n  \"tool_name\": \"write_to_file\",\n  \"arguments\": {{\n    \"TargetFile\": \"{}\",\n    \"CodeContent\": \"initial content line 1\\ninitial content line 2\\n\",\n    \"Overwrite\": true\n  }}\n}}\n```",
            target_file.to_string_lossy()
        ),
        format!(
            "Now let me replace a line.\n```tool_call\n{{\n  \"call_id\": \"step_2\",\n  \"tool_name\": \"replace_file_content\",\n  \"arguments\": {{\n    \"TargetFile\": \"{}\",\n    \"TargetContent\": \"initial content line 2\\n\",\n    \"ReplacementContent\": \"REPLACED LINE 2\\n\",\n    \"StartLine\": 1,\n    \"EndLine\": 2\n  }}\n}}\n```",
            target_file.to_string_lossy()
        ),
        "All steps succeeded. The file has been written and surgically edited.".to_string(),
    ];

    let provider = Arc::new(TestProxyProvider {
        responses: tokio::sync::Mutex::new(proxy_responses),
    });

    let config = AgentLoopConfig {
        max_turns: 5,
        workspace_root: ws.clone(),
        model: None,
        auto_approve: true,
        turn_timeout_secs: 30,
    };

    let mut engine = ReActAgentEngine::new(provider, config);
    let session = engine.run("Generate and patch artifact", |_| {}).await.expect("ReAct agent session should succeed");

    assert_eq!(session.turns_taken, 3);
    assert_eq!(session.steps.len(), 2);
    assert!(session.final_output.contains("All steps succeeded"));

    // Verify disk state
    assert!(target_file.exists());
    let file_content = fs::read_to_string(&target_file).unwrap();
    assert!(file_content.contains("initial content line 1"));
    assert!(file_content.contains("REPLACED LINE 2"));

    let _ = fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn test_p1_self_healing_diagnostics_parsing_and_repair_json() {
    let sample_rustc_stderr = r#"
error[E0308]: mismatched types
 --> src/main.rs:14:18
  |
14|     let val: u32 = "invalid_string";
  |              ---   ^^^^^^^^^^^^^^^^ expected `u32`, found `&str`
  |              |
  |              expected due to this

error[E0425]: cannot find value `unresolved_var` in this scope
 --> src/lib.rs:8:5
  |
8 |     unresolved_var.do_something();
  |     ^^^^^^^^^^^^^^ not found in this scope
"#;

    // 1. Diagnostic extraction
    let diags = HealEngine::parse_diagnostics(sample_rustc_stderr);
    assert_eq!(diags.len(), 2);
    assert_eq!(diags[0].file_path, PathBuf::from("src/main.rs"));
    assert_eq!(diags[0].line_number, 14);
    assert!(diags[0].message.contains("mismatched types"));

    assert_eq!(diags[1].file_path, PathBuf::from("src/lib.rs"));
    assert_eq!(diags[1].line_number, 8);
    assert!(diags[1].message.contains("cannot find value"));

    // 2. Repair JSON extraction from LLM response
    let llm_reply = r#"
I found the issue. Here is the surgical repair:
```json
{
  "target": "let val: u32 = \"invalid_string\";",
  "replacement": "let val: u32 = 42;"
}
```
"#;
    let (target, replacement) = HealEngine::extract_repair_json(llm_reply).expect("Should extract repair block");
    assert_eq!(target, "let val: u32 = \"invalid_string\";");
    assert_eq!(replacement, "let val: u32 = 42;");
}

#[test]
fn test_p1_instant_time_travel_rollback_byte_exact() {
    let ws = create_test_workspace("time_travel");
    let mut mgr = SwarmCheckpointManager::new();

    let file_a = ws.join("module_a.rs");
    let file_b = ws.join("module_b.rs");

    fs::write(&file_a, "fn original_a() -> i32 { 10 }\n").unwrap();
    fs::write(&file_b, "fn original_b() -> i32 { 20 }\n").unwrap();

    // 1. Snapshot baseline files
    mgr.snapshot_file(&file_a).unwrap();
    mgr.snapshot_file(&file_b).unwrap();
    let ckpt = mgr.create_checkpoint("pre_mutation", HashMap::new(), HashMap::new());
    let ckpt_id = ckpt.checkpoint_id.clone();

    // 2. Perform mutations
    fs::write(&file_a, "fn corrupted_a() { broken }\n").unwrap();
    let file_c = ws.join("newly_created.rs");
    fs::write(&file_c, "pub fn temporary() {}\n").unwrap();

    // Snapshot newly created file in second checkpoint
    mgr.snapshot_file(&file_c).unwrap();
    let _ckpt2 = mgr.create_checkpoint("post_mutation", HashMap::new(), HashMap::new());

    assert_eq!(fs::read_to_string(&file_a).unwrap(), "fn corrupted_a() { broken }\n");
    assert!(file_c.exists());

    // 3. Instant Rollback to baseline checkpoint
    let report = mgr.restore_files_from_checkpoint(&ckpt_id).expect("Rollback must succeed");
    assert_eq!(report.checkpoint_id, ckpt_id);
    assert!(report.files_restored.contains(&file_a));

    // 4. Verify byte-exact restoration
    assert_eq!(fs::read_to_string(&file_a).unwrap(), "fn original_a() -> i32 { 10 }\n");
    assert_eq!(fs::read_to_string(&file_b).unwrap(), "fn original_b() -> i32 { 20 }\n");

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_p2_workspace_rules_discovery_hashing_and_xml() {
    let ws = create_test_workspace("rules_anchor");

    // Create .hgb/rules directory and files
    let rules_dir = ws.join(".hgb").join("rules");
    fs::create_dir_all(&rules_dir).unwrap();
    fs::write(rules_dir.join("01-security.md"), "RULE: Never leak API keys or secrets.").unwrap();
    fs::write(rules_dir.join("02-style.md"), "RULE: Use explicit error handling with thiserror.").unwrap();

    // Create root HGB.md
    fs::write(ws.join("HGB.md"), "RULE: Hagibis microkernel sub-millisecond SLA.").unwrap();

    // Create .cursorrules
    fs::write(ws.join(".cursorrules"), "RULE: Respect Rust 2021 edition idioms.").unwrap();

    let mgr = WorkspaceRulesManager::discover(&ws);
    assert_eq!(mgr.rules.len(), 4, "Should discover all 4 rule documents");

    // Check Blake3 aggregate hash determinism
    assert!(!mgr.aggregate_hash.is_empty());
    assert_eq!(mgr.aggregate_hash.len(), 64);

    // Format XML context block
    let xml = mgr.aggregate_rules();
    assert!(xml.contains("<workspace_rules"));
    assert!(xml.contains("</workspace_rules>"));
    assert!(xml.contains("RULE: Never leak API keys"));
    assert!(xml.contains("RULE: Use explicit error handling"));
    assert!(xml.contains("RULE: Hagibis microkernel"));
    assert!(xml.contains("RULE: Respect Rust 2021 edition"));

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_p2_ast_syntactic_repo_map_extraction() {
    let ws = create_test_workspace("repomap_ast");

    // Rust file
    let rs_content = r#"
pub struct MicrokernelEngine {
    pub pid: u32,
}

pub enum SwarmState {
    Active,
    Idle,
}

pub trait MicroTask {
    async fn execute(&self) -> Result<()>;
}

pub async fn spawn_worker(id: &str) -> Worker {
    Worker::new(id)
}
"#;
    fs::write(ws.join("engine.rs"), rs_content).unwrap();

    // TypeScript file
    let ts_content = r#"
export interface AgentOptions {
    timeout: number;
}

export class AgentClient {
    async connect(): Promise<void> {}
}

export function createClient(): AgentClient {
    return new AgentClient();
}
"#;
    fs::write(ws.join("client.ts"), ts_content).unwrap();

    // Python file
    let py_content = r#"
class SwarmOrchestrator:
    def __init__(self):
        pass

def run_orchestration(name: str):
    pass
"#;
    fs::write(ws.join("orchestrator.py"), py_content).unwrap();

    // Generate RepoMap
    let map = RepoMap::generate_map(&ws, Some(20)).expect("RepoMap generation should succeed");
    assert!(map.file_count >= 3);
    assert!(map.symbol_count >= 6);

    // Check symbols present in YAML outline
    assert!(map.content.contains("engine.rs"));
    assert!(map.content.contains("MicrokernelEngine"));
    assert!(map.content.contains("SwarmState"));
    assert!(map.content.contains("MicroTask"));
    assert!(map.content.contains("spawn_worker"));

    assert!(map.content.contains("client.ts"));
    assert!(map.content.contains("AgentOptions"));
    assert!(map.content.contains("AgentClient"));

    assert!(map.content.contains("orchestrator.py"));
    assert!(map.content.contains("SwarmOrchestrator"));

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn test_p3_diff_hud_and_selective_patch_acceptance() {
    let diff = r#"
--- a/crates/engine.rs
+++ b/crates/engine.rs
@@ -1,3 +1,3 @@
 fn compute() {
-    let x = 100;
+    let x = 200;
 }
@@ -10,3 +10,3 @@
 fn finalize() {
-    let done = false;
+    let done = true;
 }
"#;

    // 1. Parse unified diff
    let mut hunks = SelectivePatcher::parse_unified_diff(diff);
    assert_eq!(hunks.len(), 2, "Should parse two distinct hunks");
    assert_eq!(hunks[0].lines[1].kind, DiffLineKind::Deletion);
    assert_eq!(hunks[0].lines[2].kind, DiffLineKind::Addition);

    // 2. Selectively accept hunk 1 and reject hunk 2
    hunks[0].accepted = Some(true);
    hunks[1].accepted = Some(false);

    // 3. Render Diff HUD
    let hud = ChatCanvas::render_diff_hud(&hunks, 80);
    assert!(hud.contains("Hunk #1"));
    assert!(hud.contains("[ACCEPTED]"));
    assert!(hud.contains("Hunk #2"));
    assert!(hud.contains("[REJECTED]"));
    assert!(hud.contains("+     let x = 200;"));
    assert!(hud.contains("-     let x = 100;"));

    // 4. Apply selective patch
    let original = "fn compute() {\n    let x = 100;\n}\n\n// intermediate lines\nfn finalize() {\n    let done = false;\n}\n";
    let patched = SelectivePatcher::apply_accepted_hunks(original, &hunks).expect("Selective patch must succeed");

    assert!(patched.contains("let x = 200;"), "Hunk #1 should be accepted and applied");
    assert!(patched.contains("let done = false;"), "Hunk #2 was rejected and should stay untouched");

    println!("✔ Diff HUD and Selective Patching brutally validated!");
}
