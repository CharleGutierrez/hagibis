//! # Brutal Integration Tests for Hagibis Cockpit TUI Engine
//!
//! Verifies:
//! - Multi-agent DAG state transitions and parent/child hierarchy
//! - Active tool call tracking and output snippets
//! - Artifact diff storage and unified diff rendering
//! - Background tasks monitor and status transitions
//! - All 6 mid-flight steering actions (Pause, Resume, EditScratchpad, RedirectTool, Abort, InjectContext)
//! - Multi-tab headless rendering without panics or memory corruption
//! - Selection navigation and wrap-around boundaries
//! - Telemetry updates and prompt buffer editing

use hgb_nextgen::{
    CockpitActiveTab, CockpitArtifactDiff, CockpitBackgroundTask, CockpitDagNode,
    CockpitInputMode, CockpitNodeStatus, CockpitOverlay, CockpitState, CockpitTelemetry,
    CockpitToolCall, CockpitViewMode, SteeringAction,
};

#[test]
fn test_brutal_dag_hierarchy_and_state_transitions() {
    let mut state = CockpitState::new();

    // 1. Create multi-agent hierarchy: root -> subagent1 -> subagent2
    let root = CockpitDagNode::new("root", "Microkernel Ingestion", "in-process-gguf")
        .with_status(CockpitNodeStatus::Pending)
        .with_tokens(100);

    let sub1 = CockpitDagNode::new("sub1", "Gemini 2.5 Reasoner", "gemini-2.5-flash")
        .with_parent("root")
        .with_status(CockpitNodeStatus::Pending)
        .with_tokens(500);

    let sub2 = CockpitDagNode::new("sub2", "AST Surgical Crud", "hgb-fs")
        .with_parent("sub1")
        .with_status(CockpitNodeStatus::Pending)
        .with_tokens(0);

    let sub3 = CockpitDagNode::new("sub3", "Merkle Auditor", "hgb-prov")
        .with_parent("root")
        .with_status(CockpitNodeStatus::Pending);

    state.add_node(root);
    state.add_node(sub1);
    state.add_node(sub2);
    state.add_node(sub3);

    assert_eq!(state.nodes.len(), 4);
    assert_eq!(state.telemetry.active_agents, 4);

    // Verify parent relationships
    assert_eq!(state.nodes[0].parent_id, None);
    assert_eq!(state.nodes[1].parent_id, Some("root".to_string()));
    assert_eq!(state.nodes[2].parent_id, Some("sub1".to_string()));
    assert_eq!(state.nodes[3].parent_id, Some("root".to_string()));

    // State transitions: Pending -> Running -> Succeeded
    state.update_node_status("root", CockpitNodeStatus::Running { progress_pct: 40 });
    assert_eq!(state.nodes[0].status, CockpitNodeStatus::Running { progress_pct: 40 });

    state.update_node_status("root", CockpitNodeStatus::Succeeded { duration_ms: 15 });
    assert_eq!(state.nodes[0].status, CockpitNodeStatus::Succeeded { duration_ms: 15 });

    // Node failure transition
    state.update_node_status("sub2", CockpitNodeStatus::Failed { error: "AST parse timeout".into() });
    assert_eq!(
        state.nodes[2].status,
        CockpitNodeStatus::Failed { error: "AST parse timeout".into() }
    );
}

#[test]
fn test_brutal_active_tools_and_output_snippets() {
    let mut state = CockpitState::new();
    let mut node = CockpitDagNode::new("agent-1", "Code Synthesizer", "gemini-2.5-flash");

    let call1 = CockpitToolCall::new(
        "read_file",
        "path=crates/hgb-core/src/lib.rs",
        "SUCCESS",
        4,
        Some("pub mod auth;\npub mod crud;".into()),
    );
    let call2 = CockpitToolCall::new(
        "run_command",
        "cargo check --workspace",
        "RUNNING",
        120,
        None,
    );
    let call3 = CockpitToolCall::new(
        "surgical_edit",
        "file=cockpit.rs, lines=10-25",
        "SUCCESS",
        18,
        Some("Replaced 15 lines atomically".into()),
    );

    node.add_tool_call(call1);
    node.add_tool_call(call2);
    node.add_tool_call(call3);

    state.add_node(node);

    assert_eq!(state.nodes[0].tool_calls.len(), 3);
    assert_eq!(state.nodes[0].tool_calls[0].tool_name, "read_file");
    assert_eq!(state.nodes[0].tool_calls[0].status, "SUCCESS");
    assert!(state.nodes[0].tool_calls[0].output_snippet.is_some());

    assert_eq!(state.nodes[0].tool_calls[1].tool_name, "run_command");
    assert_eq!(state.nodes[0].tool_calls[1].status, "RUNNING");
    assert!(state.nodes[0].tool_calls[1].output_snippet.is_none());

    // Add another tool call directly via state helper
    state.add_tool_call(
        "agent-1",
        CockpitToolCall::new("blake3_hash", "input_len=4096", "SUCCESS", 1, Some("hash=0xabcdef".into())),
    );
    assert_eq!(state.nodes[0].tool_calls.len(), 4);
}

#[test]
fn test_brutal_artifact_diff_tracking_and_rendering() {
    let mut state = CockpitState::new();

    let diff1 = CockpitArtifactDiff::new(
        "crates/hgb-nextgen/src/cockpit.rs",
        120,
        15,
        "@@ -10,6 +10,12 @@\n+use ratatui::widgets::Tabs;\n-use old_renderer;\n+use new_diff_viewer;\n pub struct CockpitState;",
    );
    let diff2 = CockpitArtifactDiff::new(
        "crates/hgb-cli/src/main.rs",
        45,
        2,
        "@@ -100,2 +100,5 @@\n+    Commands::Cockpit { headless } => {\n+        state.run_interactive().await?;",
    );

    state.add_artifact_diff(diff1);
    state.add_artifact_diff(diff2);

    assert_eq!(state.artifact_diffs.len(), 2);
    assert_eq!(state.artifact_diffs[0].added_lines, 120);
    assert_eq!(state.artifact_diffs[0].deleted_lines, 15);
    assert_eq!(state.artifact_diffs[1].file_path, "crates/hgb-cli/src/main.rs");

    // Render headless in ArtifactDiffs tab
    state.set_active_tab(CockpitActiveTab::ArtifactDiffs);
    let buffer = state.render_headless(120, 35);
    assert!(buffer.content.len() >= 120 * 35);
}

#[test]
fn test_brutal_background_tasks_monitor() {
    let mut state = CockpitState::new();

    let task1 = CockpitBackgroundTask::new("bg-001", "P2P Mesh Gossip Heartbeat", "RUNNING", 12);
    let task2 = CockpitBackgroundTask::new("bg-002", "Merkle WAL Compaction", "FINISHED", 60);

    state.add_background_task(task1);
    state.add_background_task(task2);

    assert_eq!(state.background_tasks.len(), 2);
    assert_eq!(state.background_tasks[0].task_id, "bg-001");
    assert_eq!(state.background_tasks[0].status, "RUNNING");

    // Update background task status
    state.update_background_task_status("bg-001", "FINISHED", 45);
    assert_eq!(state.background_tasks[0].status, "FINISHED");
    assert_eq!(state.background_tasks[0].elapsed_secs, 45);

    // Render headless in BackgroundTasks tab
    state.set_active_tab(CockpitActiveTab::BackgroundTasks);
    let buffer = state.render_headless(120, 35);
    assert!(buffer.content.len() >= 120 * 35);
}

#[test]
fn test_brutal_all_six_steering_actions() {
    let mut state = CockpitState::new();
    let node = CockpitDagNode::new("steer-node", "Steered Worker", "gemini-2.5-flash");
    state.add_node(node);

    // 1. Pause
    state.apply_steering(SteeringAction::Pause { node_id: "steer-node".into() }).unwrap();
    assert_eq!(state.nodes[0].status, CockpitNodeStatus::Paused);
    assert_eq!(state.steering_history.len(), 1);

    // 2. Resume
    state.apply_steering(SteeringAction::Resume { node_id: "steer-node".into() }).unwrap();
    assert_eq!(state.nodes[0].status, CockpitNodeStatus::Running { progress_pct: 0 });
    assert_eq!(state.steering_history.len(), 2);

    // 3. EditScratchpad
    state.apply_steering(SteeringAction::EditScratchpad {
        node_id: "steer-node".into(),
        new_scratchpad: "Mid-flight altered reasoning instructions".into(),
    }).unwrap();
    assert_eq!(state.nodes[0].scratchpad, "Mid-flight altered reasoning instructions");
    assert!(matches!(state.nodes[0].status, CockpitNodeStatus::Steered { .. }));
    assert_eq!(state.steering_history.len(), 3);

    // 4. RedirectTool
    state.apply_steering(SteeringAction::RedirectTool {
        node_id: "steer-node".into(),
        new_tool_name: "safe_sandboxed_exec".into(),
        parameters: serde_json::json!({"sanitized": true}),
    }).unwrap();
    assert_eq!(state.nodes[0].tool_calls.len(), 1);
    assert_eq!(state.nodes[0].tool_calls[0].tool_name, "safe_sandboxed_exec");
    assert_eq!(state.nodes[0].tool_calls[0].status, "REDIRECTED");
    assert_eq!(state.steering_history.len(), 4);

    // 5. InjectContext
    state.apply_steering(SteeringAction::InjectContext {
        node_id: "steer-node".into(),
        additional_context: "CRITICAL: Enforce memory invariant".into(),
    }).unwrap();
    assert!(state.nodes[0].scratchpad.contains("CRITICAL: Enforce memory invariant"));
    assert_eq!(state.steering_history.len(), 5);

    // 6. Abort
    state.apply_steering(SteeringAction::Abort {
        node_id: "steer-node".into(),
        reason: "Security boundary check failed".into(),
    }).unwrap();
    assert!(matches!(state.nodes[0].status, CockpitNodeStatus::Failed { .. }));
    assert_eq!(state.steering_history.len(), 6);

    // Steering action on non-existent node returns Err
    let err = state.apply_steering(SteeringAction::Pause { node_id: "ghost".into() });
    assert!(err.is_err());
}

#[test]
fn test_brutal_multi_tab_headless_rendering_integrity() {
    let mut state = CockpitState::new();

    // Populate with comprehensive data
    let mut n1 = CockpitDagNode::new("n1", "Root Agent", "in-process-gguf")
        .with_status(CockpitNodeStatus::Succeeded { duration_ms: 12 })
        .with_tokens(240)
        .with_scratchpad("Chain of thought: Ingesting code graph.");
    n1.add_tool_call(CockpitToolCall::new("indexer", "path=src", "SUCCESS", 8, Some("Indexed 45 files".into())));

    let mut n2 = CockpitDagNode::new("n2", "Reasoning Worker", "gemini-2.5-flash")
        .with_parent("n1")
        .with_status(CockpitNodeStatus::Running { progress_pct: 65 })
        .with_tokens(1200)
        .with_scratchpad("Reasoning about concurrency safety in UDS IPC socket.");
    n2.add_tool_call(CockpitToolCall::new("gemini_api", "model=gemini-2.5-flash", "RUNNING", 300, None));

    state.add_node(n1);
    state.add_node(n2);

    state.add_artifact_diff(CockpitArtifactDiff::new(
        "crates/hgb-nextgen/src/cockpit.rs",
        85,
        12,
        "@@ -1,5 +1,10 @@\n+pub struct CockpitState\n-old code\n+new verified code",
    ));

    state.add_background_task(CockpitBackgroundTask::new(
        "bg-mesh",
        "P2P ZeroConf Swarm Gossip",
        "RUNNING",
        35,
    ));

    state.update_telemetry(CockpitTelemetry {
        total_tokens: 1440,
        tokens_per_sec: 142.5,
        peak_temperature_celsius: 48.5,
        battery_pct: Some(92),
        lakandiwa_entropy_bits: 0.38,
        speculative_acceptance_rate: 0.94,
        uds_latency_us: 110,
        active_agents: 2,
    });

    state.set_auth_account("buzer.agy@gmail.com");
    state.set_model("gemini-2.5-flash");
    state.set_reasoning_effort("High");
    state.set_workspace("/home/dyna/TGS Projects/hagibis");

    // Render Tab 1: LiveStream
    state.set_active_tab(CockpitActiveTab::LiveStream);
    let buf1 = state.render_headless(140, 40);
    assert_eq!(buf1.content.len(), 140 * 40);
    let str1 = state.render_headless_to_string(140, 40);
    assert!(str1.contains("HAGIBIS AGY COCKPIT"));
    assert!(str1.contains("gemini-2.5-flash"));
    assert!(str1.contains("buzer.agy@gmail.com"));

    // Render Tab 2: ArtifactDiffs
    state.set_active_tab(CockpitActiveTab::ArtifactDiffs);
    let buf2 = state.render_headless(140, 40);
    assert_eq!(buf2.content.len(), 140 * 40);
    let str2 = state.render_headless_to_string(140, 40);
    assert!(str2.contains("cockpit.rs"));
    assert!(str2.contains("+85"));

    // Render Tab 3: BackgroundTasks
    state.set_active_tab(CockpitActiveTab::BackgroundTasks);
    let buf3 = state.render_headless(140, 40);
    assert_eq!(buf3.content.len(), 140 * 40);
    let str3 = state.render_headless_to_string(140, 40);
    assert!(str3.contains("bg-mesh"));
    assert!(str3.contains("P2P ZeroConf Swarm Gossip"));
}

#[test]
fn test_brutal_selection_navigation_and_boundaries() {
    let mut state = CockpitState::new();

    // Empty state should not panic
    state.select_next();
    state.select_prev();
    assert_eq!(state.selected_node(), None);

    // Add 3 nodes
    state.add_node(CockpitDagNode::new("n1", "Node 1", "m1"));
    state.add_node(CockpitDagNode::new("n2", "Node 2", "m2"));
    state.add_node(CockpitDagNode::new("n3", "Node 3", "m3"));

    assert_eq!(state.selected_index, 0);
    assert_eq!(state.selected_node().unwrap().id, "n1");

    state.select_next();
    assert_eq!(state.selected_index, 1);
    assert_eq!(state.selected_node().unwrap().id, "n2");

    state.select_next();
    assert_eq!(state.selected_index, 2);
    assert_eq!(state.selected_node().unwrap().id, "n3");

    // Wrap-around forward
    state.select_next();
    assert_eq!(state.selected_index, 0);
    assert_eq!(state.selected_node().unwrap().id, "n1");

    // Wrap-around backward
    state.select_prev();
    assert_eq!(state.selected_index, 2);
    assert_eq!(state.selected_node().unwrap().id, "n3");
}

#[test]
fn test_brutal_tab_cycling() {
    let mut state = CockpitState::new();
    assert_eq!(state.active_tab, CockpitActiveTab::LiveStream);
    assert_eq!(state.active_tab.index(), 0);

    state.next_tab();
    assert_eq!(state.active_tab, CockpitActiveTab::ArtifactDiffs);
    assert_eq!(state.active_tab.index(), 1);

    state.next_tab();
    assert_eq!(state.active_tab, CockpitActiveTab::BackgroundTasks);
    assert_eq!(state.active_tab.index(), 2);

    state.next_tab();
    assert_eq!(state.active_tab, CockpitActiveTab::LiveStream);

    state.prev_tab();
    assert_eq!(state.active_tab, CockpitActiveTab::BackgroundTasks);

    state.prev_tab();
    assert_eq!(state.active_tab, CockpitActiveTab::ArtifactDiffs);
}

#[test]
fn test_brutal_prompt_input_and_history() {
    let mut state = CockpitState::new();
    assert_eq!(state.input_mode, CockpitInputMode::Normal);

    state.input_mode = CockpitInputMode::Input;
    state.prompt_input = "Verify Blake3 invariant".to_string();
    state.cursor_position = state.prompt_input.len();

    // Verify headless rendering in ChatCanvas mode shows prompt
    let rendered_canvas = state.render_headless_to_string(100, 30);
    assert!(rendered_canvas.contains("Verify Blake3 invariant"));

    // Verify headless rendering in CockpitSplit mode shows cursor and mode badge
    state.view_mode = CockpitViewMode::CockpitSplit;
    let rendered_split = state.render_headless_to_string(100, 30);
    assert!(rendered_split.contains("Verify Blake3 invariant"));
    assert!(rendered_split.contains("PROMPT INPUT"));
}

#[test]
fn test_brutal_chat_canvas_and_conversation_rendering() {
    let mut state = CockpitState::new();
    assert_eq!(state.view_mode, CockpitViewMode::ChatCanvas);

    // Add user turn
    state.add_user_message("Can you verify the Blake3 Merkle tree and audit root?");

    // Add assistant turn with thinking, tool calls, and markdown
    let tool_call = CockpitToolCall::new(
        "verify_merkle",
        "leaves=8, root=0x9a8f",
        "SUCCESS",
        4,
        Some("Audit root validated against statutory Rule 141".into()),
    );
    state.add_assistant_message(
        "Here is the formal verification result:\n\n```rust\nlet root = tree.compute_root();\nassert!(root.is_valid());\n```\n\nAll Merkle invariants hold.",
        "gemini-2.5-flash",
        340,
        18,
        Some("1. Ingesting leaf nodes into Blake3 hasher\n2. Computing internal node hashes\n3. Checking statutory compliance".into()),
        vec![tool_call],
    );

    // Add system notice
    state.add_system_notice("Checkpoint ckpt-001 created automatically.");

    let rendered = state.render_headless_to_string(120, 35);

    // Verify header components
    assert!(rendered.contains("HAGIBIS"));
    // Verify user turn
    assert!(rendered.contains("verify the Blake3 Merkle tree"));
    // Verify assistant model badge & content
    assert!(rendered.contains("gemini-2.5-flash"));
    assert!(rendered.contains("All Merkle invariants hold."));
    // Verify thinking block
    assert!(rendered.contains("Thinking Process"));
    assert!(rendered.contains("Ingesting leaf nodes"));
    // Verify tool call card
    assert!(rendered.contains("verify_merkle"));
    assert!(rendered.contains("Audit root validated"));
    // Verify code block
    assert!(rendered.contains("let root = tree.compute_root()"));
    // Verify system notice
    assert!(rendered.contains("Checkpoint ckpt-001 created automatically"));
    // Verify prompt input box
    assert!(rendered.contains("type a prompt"));
    // Verify statusline shortcuts
    assert!(rendered.contains("shortcuts"));
    assert!(rendered.contains("Shift+Tab"));
    assert!(rendered.contains("Ctrl+T"));
}

#[test]
fn test_brutal_overlays_rendering() {
    let mut state = CockpitState::new();

    // 1. Shortcuts overlay
    state.overlay = CockpitOverlay::Shortcuts;
    let rendered_shortcuts = state.render_headless_to_string(100, 30);
    assert!(rendered_shortcuts.contains("Keyboard Shortcuts & Slash Commands"));
    assert!(rendered_shortcuts.contains("Ctrl+T"));
    assert!(rendered_shortcuts.contains("Shift+Tab"));
    assert!(rendered_shortcuts.contains("/model"));

    // 2. ModelPicker overlay
    state.overlay = CockpitOverlay::ModelPicker { selected: 0 };
    let rendered_model_picker = state.render_headless_to_string(100, 30);
    assert!(rendered_model_picker.contains("Select AI Model"));
    assert!(rendered_model_picker.contains("qwen2.5-coder:1.5b"));
    assert!(rendered_model_picker.contains("gemini-2.5-flash"));

    // 3. Tasks overlay
    state.add_background_task(CockpitBackgroundTask::new(
        "task-99",
        "Surgical AST Validation",
        "RUNNING",
        5,
    ));
    state.overlay = CockpitOverlay::Tasks;
    let rendered_tasks = state.render_headless_to_string(100, 30);
    assert!(rendered_tasks.contains("Background Tasks Monitor"));
    assert!(rendered_tasks.contains("task-99"));
    assert!(rendered_tasks.contains("Surgical AST Validation"));
}

#[test]
fn test_brutal_view_mode_and_execution_mode_toggles() {
    let mut state = CockpitState::new();
    assert_eq!(state.view_mode, CockpitViewMode::ChatCanvas);

    // Toggle to CockpitSplit
    state.toggle_view_mode();
    assert_eq!(state.view_mode, CockpitViewMode::CockpitSplit);

    // Toggle back to ChatCanvas
    state.toggle_view_mode();
    assert_eq!(state.view_mode, CockpitViewMode::ChatCanvas);

    // Test execution mode cycling
    assert_eq!(state.execution_mode, "default");
    state.cycle_execution_mode();
    assert_eq!(state.execution_mode, "plan");
    state.cycle_execution_mode();
    assert_eq!(state.execution_mode, "accept-edits");
    state.cycle_execution_mode();
    assert_eq!(state.execution_mode, "default");
}
