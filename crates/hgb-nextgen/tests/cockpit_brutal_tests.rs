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

#[test]
fn test_brutal_chat_canvas_scrolling_and_indicator() {
    let mut state = CockpitState::new();
    assert_eq!(state.chat_scroll, 0);

    // Add many messages to fill the viewport
    for i in 1..=20 {
        state.add_user_message(format!("Turn #{}: Show me invariant check", i));
        state.add_assistant_message(
            format!("Response #{}: Verification succeeded with Blake3 leaf hash 0x{:04x}", i, i * 42),
            "gemini-2.5-flash",
            120,
            15,
            None,
            Vec::new(),
        );
    }

    // When at bottom (chat_scroll == 0), rendered output contains latest message and NO scrolled-up pill
    let rendered_bottom = state.render_headless_to_string(100, 25);
    assert!(rendered_bottom.contains("Turn #20"));
    assert!(!rendered_bottom.contains("SCROLLED UP"));

    // Scroll up by 6 lines
    state.scroll_chat_up(6);
    assert_eq!(state.chat_scroll, 6);

    let rendered_scrolled = state.render_headless_to_string(100, 25);
    // Floating gold pill must now be visible
    assert!(rendered_scrolled.contains("SCROLLED UP"));
    assert!(rendered_scrolled.contains("+6 lines"));

    // Scroll down by 2 lines
    state.scroll_chat_down(2);
    assert_eq!(state.chat_scroll, 4);

    // Scroll to top
    state.scroll_chat_to_top();
    assert!(state.chat_scroll >= 100);

    // Scroll back to bottom
    state.scroll_chat_to_bottom();
    assert_eq!(state.chat_scroll, 0);

    let rendered_reset = state.render_headless_to_string(100, 25);
    assert!(!rendered_reset.contains("SCROLLED UP"));
}

#[test]
fn test_brutal_agy_prompt_keybindings_and_kill_ring() {
    let mut state = CockpitState::new();

    // 1. Text insertion & cursor movement
    state.insert_str("hello world rust");
    assert_eq!(state.prompt_input, "hello world rust");
    assert_eq!(state.cursor_position, 16);

    state.move_cursor_left();
    assert_eq!(state.cursor_position, 15);

    state.move_to_start();
    assert_eq!(state.cursor_position, 0);

    state.move_to_end();
    assert_eq!(state.cursor_position, 16);

    // Word navigation backward/forward
    state.move_word_backward();
    assert_eq!(state.cursor_position, 12); // start of "rust"

    state.move_word_backward();
    assert_eq!(state.cursor_position, 6); // start of "world"

    state.move_word_forward();
    assert_eq!(state.cursor_position, 12); // after "world "

    // 2. Kill ring & deletion
    // Kill word forward ("rust")
    state.kill_word_forward();
    assert_eq!(state.prompt_input, "hello world ");
    assert_eq!(state.kill_ring, "rust");

    // Yank back at cursor
    state.yank();
    assert_eq!(state.prompt_input, "hello world rust");

    // Kill word backward ("rust")
    state.kill_word_backward();
    assert_eq!(state.prompt_input, "hello world ");
    assert_eq!(state.kill_ring, "rust");

    // Kill to start (Ctrl+U)
    state.kill_to_start();
    assert_eq!(state.prompt_input, "");
    assert_eq!(state.kill_ring, "hello world ");
    assert_eq!(state.cursor_position, 0);

    // Yank back
    state.yank();
    assert_eq!(state.prompt_input, "hello world ");

    // Kill to end (Ctrl+K)
    state.cursor_position = 5;
    state.kill_to_end();
    assert_eq!(state.prompt_input, "hello");
    assert_eq!(state.kill_ring, " world ");

    // 3. Prompt History navigation (Up / Down)
    state.prompt_history = vec![
        "cargo check".to_string(),
        "git status".to_string(),
        "hgb doctor".to_string(),
    ];
    state.history_index = 3;
    state.prompt_input = "my current draft".to_string();
    state.cursor_position = 16;

    // Up: save draft and go to "hgb doctor"
    state.history_prev();
    assert_eq!(state.history_index, 2);
    assert_eq!(state.prompt_input, "hgb doctor");

    // Up: go to "git status"
    state.history_prev();
    assert_eq!(state.history_index, 1);
    assert_eq!(state.prompt_input, "git status");

    // Up: go to "cargo check"
    state.history_prev();
    assert_eq!(state.history_index, 0);
    assert_eq!(state.prompt_input, "cargo check");

    // Down: go to "git status"
    state.history_next();
    assert_eq!(state.history_index, 1);
    assert_eq!(state.prompt_input, "git status");

    // Down: go to "hgb doctor"
    state.history_next();
    assert_eq!(state.history_index, 2);
    assert_eq!(state.prompt_input, "hgb doctor");

    // Down: restore draft!
    state.history_next();
    assert_eq!(state.history_index, 3);
    assert_eq!(state.prompt_input, "my current draft");
}

#[test]
fn test_brutal_banner_auto_width_and_unbroken_lines() {
    let mut state = CockpitState::new();
    let long_snippet = "I'm sorry, I didn't understand what you meant by \"adada\". Could you please provide more context or clarify what you're asking?";
    let tool_call = CockpitToolCall::new(
        "ollama/local_inference",
        "model=qwen2.5-coder:1.5b",
        "SUCCESS",
        12120,
        Some(long_snippet.to_string()),
    );

    state.add_assistant_message(
        "Direct model output processed.".to_string(),
        "qwen2.5-coder:1.5b",
        85,
        12120,
        None,
        vec![tool_call],
    );

    // 1. Render in a wide terminal (140 cols): banner should auto-expand to fit long snippet cleanly
    let rendered_wide = state.render_headless_to_string(140, 25);
    assert!(rendered_wide.contains("ollama/local_inference"));
    assert!(rendered_wide.contains("model=qwen2.5-coder:1.5b"));
    assert!(rendered_wide.contains("[SUCCESS]"));
    assert!(rendered_wide.contains("Duration: 12120ms"));
    assert!(rendered_wide.contains("I'm sorry, I didn't understand"));

    // Verify unbroken lines:
    let lines: Vec<&str> = rendered_wide.lines().collect();
    let banner_top = lines.iter().find(|l| l.contains("╭─") && l.contains("ollama/local_inference")).expect("Banner top found");
    let banner_status = lines.iter().find(|l| l.contains("│ Status: [SUCCESS]")).expect("Status line found");
    let banner_snippet = lines.iter().find(|l| l.contains("│   I'm sorry")).expect("Snippet line found");
    let banner_bottom = lines.iter().find(|l| l.contains('╰') && l.contains('╯')).expect("Bottom border found");

    println!("\n--- RENDERED BANNER WIDE ---");
    println!("{}", banner_top);
    println!("{}", banner_status);
    println!("{}", banner_snippet);
    println!("{}", banner_bottom);
    // Check coordinates in Ratatui Buffer
    let buf = state.render_headless(140, 25);
    let mut top_corner_x = None;
    let mut status_bar_x = None;
    let mut bottom_corner_x = None;

    for y in 0..25 {
        for x in 0..140 {
            if let Some(c) = buf.cell((x, y)) {
                if c.symbol() == "╮" {
                    top_corner_x = Some(x);
                }
                if c.symbol() == "│" && status_bar_x.is_none() {
                    // find rightmost │ on status line
                    let mut rightmost_pipe = x;
                    for x2 in (x + 1)..140 {
                        if let Some(c2) = buf.cell((x2, y)) {
                            if c2.symbol() == "│" {
                                rightmost_pipe = x2;
                            }
                        }
                    }
                    if rightmost_pipe > x {
                        status_bar_x = Some(rightmost_pipe);
                    }
                }
                if c.symbol() == "╯" {
                    bottom_corner_x = Some(x);
                }
            }
        }
    }

    println!("top_corner_x: {:?}", top_corner_x);
    println!("status_bar_x: {:?}", status_bar_x);
    println!("bottom_corner_x: {:?}", bottom_corner_x);

    assert_eq!(top_corner_x, bottom_corner_x, "Top and bottom corners must match!");
    assert_eq!(status_bar_x, bottom_corner_x, "Status right border and bottom corner must match!");

    // 2. Render in a narrower terminal (80 cols): long line must wrap cleanly inside without breaking the box
    let rendered_narrow = state.render_headless_to_string(80, 25);
    println!("\n--- RENDERED BANNER NARROW (80 cols) ---");
    for line in rendered_narrow.lines() {
        if line.contains('╭') || line.contains('│') || line.contains('╰') {
            println!("{}", line);
        }
    }
    println!("----------------------------------------\n");
    assert!(rendered_narrow.contains("ollama/local_inference"));
    assert!(rendered_narrow.contains("[SUCCESS]"));
    assert!(rendered_narrow.contains("Duration: 12120ms"));
    assert!(rendered_narrow.contains("I'm sorry"));
    // Verify words are cleanly wrapped at word boundaries without breaking mid-word
    assert!(rendered_narrow.contains("Could you"));
    assert!(rendered_narrow.contains("please provide more context"));
    assert!(!rendered_narrow.contains("Could you pl"));
}

#[test]
fn test_brutal_code_banner_auto_width_and_border_alignment() {
    let mut state = CockpitState::new();
    let code_content = "Here is the implementation:\n```rust\nfn compute(x: i32) -> i32 {\n    let y = x * 2;\n    y + 42\n}\n```\nDone.";

    state.add_assistant_message(
        code_content.to_string(),
        "qwen2.5-coder:1.5b",
        45,
        1500,
        None,
        vec![],
    );

    // 1. Render in 120-column terminal: code banner should auto-fit content (not stretch to 100 or 120)
    let buf = state.render_headless(120, 25);
    let mut code_top_x = None;
    let mut code_pipe_x = None;
    let mut code_bot_x = None;

    for y in 0..25 {
        for x in 0..120 {
            if let Some(c) = buf.cell((x, y)) {
                if c.symbol() == "╮" && code_top_x.is_none() {
                    // Check if line contains "rust" tag
                    let mut row_str = String::new();
                    for xi in 0..120 {
                        if let Some(ci) = buf.cell((xi, y)) {
                            row_str.push_str(ci.symbol());
                        }
                    }
                    if row_str.contains("[rust]") {
                        code_top_x = Some(x);
                    }
                }
                if c.symbol() == "╯" && code_bot_x.is_none() {
                    code_bot_x = Some(x);
                }
            }
        }
    }

    // Also find the rightmost '│' on the code content lines
    for y in 0..25 {
        let mut row_str = String::new();
        for xi in 0..120 {
            if let Some(ci) = buf.cell((xi, y)) {
                row_str.push_str(ci.symbol());
            }
        }
        if row_str.contains("compute") || row_str.contains("let y =") {
            let mut rightmost = 0;
            for xi in 0..120 {
                if let Some(ci) = buf.cell((xi, y)) {
                    if ci.symbol() == "│" {
                        rightmost = xi;
                    }
                }
            }
            code_pipe_x = Some(rightmost);
            break;
        }
    }

    let rendered = state.render_headless_to_string(120, 25);
    println!("\n--- RENDERED CODE BANNER AUTO-WIDTH ---");
    for line in rendered.lines() {
        if line.contains('╭') || line.contains('│') || line.contains('╰') {
            println!("{}", line);
        }
    }
    println!("code_top_x: {:?}", code_top_x);
    println!("code_pipe_x: {:?}", code_pipe_x);
    println!("code_bot_x: {:?}", code_bot_x);

    assert!(code_top_x.is_some(), "Code top corner ╮ must be found");
    assert!(code_bot_x.is_some(), "Code bottom corner ╯ must be found");
    assert!(code_pipe_x.is_some(), "Code content right border │ must be found");

    // All must match at the EXACT same column!
    assert_eq!(code_top_x, code_bot_x, "Top right corner and bottom right corner must match!");
    assert_eq!(code_pipe_x, code_bot_x, "Content right border and bottom corner must match!");

    // Auto-width verification: the code snippet is ~27 chars wide, so banner width should be around 36, NOT 100 or 120!
    let width = code_top_x.unwrap();
    assert!(width < 50, "Code banner must auto-fit to content (was {} cols, expected < 50)", width);
    assert!(width >= 35, "Code banner must satisfy minimum visual width");

    // Indentation verification: verify 4 spaces of indentation preserved in "    let y = x * 2;"
    assert!(rendered.contains("    let y = x * 2;"), "Indentation in code banner must be preserved!");
}

#[tokio::test]
async fn test_brutal_prompt_processing_animation_and_cancel() {
    let mut state = CockpitState::new();

    // 1. Initial state checks
    assert!(!state.is_processing);
    assert_eq!(state.processing_tick, 0);
    assert!(state.processing_start.is_none());

    // 2. Set processing state and simulate tick 3
    state.is_processing = true;
    state.processing_tick = 3;
    state.processing_start = Some(chrono::Utc::now());
    state.processing_prompt_preview = "optimize neural search algorithm".to_string();

    let backend = ratatui::backend::TestBackend::new(120, 25);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| state.render_ui(f)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Inspect rendered buffer
    let mut rendered_lines = Vec::new();
    for y in 0..25 {
        let mut row_str = String::new();
        for x in 0..120 {
            let symbol = buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ");
            row_str.push_str(symbol);
        }
        rendered_lines.push(row_str);
    }

    let full_rendered = rendered_lines.join("\n");
    println!("\n--- RENDERED PROCESSING CARD ANIMATION ---");
    for line in full_rendered.lines() {
        if line.contains('╭') || line.contains('│') || line.contains('╰') || line.contains("Thinking") || line.contains("PROCESSING") {
            println!("{}", line);
        }
    }

    // Verify Braille spinner frame 3: '⠸'
    assert!(full_rendered.contains('⠸'), "Braille spinner frame '⠸' must be present for tick 3");
    assert!(full_rendered.contains("Thinking..."), "Card must display 'Thinking...'");
    assert!(full_rendered.contains("[Esc to cancel]"), "Esc to cancel pill must be rendered");
    assert!(full_rendered.contains("optimize neural search algorithm"), "Prompt preview must be rendered");
    assert!(full_rendered.contains("PROCESSING ⠸"), "Input box title must show animated spinner");

    // Verify Border alignment: find corners of the processing card
    let mut card_top_x = None;
    let mut card_pipe_x = None;
    let mut card_bot_x = None;

    for y in 0..25 {
        let row_str = &rendered_lines[y as usize];
        if row_str.contains("Processing Prompt") {
            for xi in 0..120 {
                if let Some(ci) = buf.cell((xi, y)) {
                    if ci.symbol() == "╮" {
                        card_top_x = Some(xi);
                    }
                }
            }
        }
        if row_str.contains("Thinking...") {
            let mut rightmost = 0;
            for xi in 0..120 {
                if let Some(ci) = buf.cell((xi, y)) {
                    if ci.symbol() == "│" {
                        rightmost = xi;
                    }
                }
            }
            card_pipe_x = Some(rightmost);
        }
        if card_top_x.is_some() && card_bot_x.is_none() && row_str.contains('╰') && row_str.contains('╯') {
            for xi in 0..120 {
                if let Some(ci) = buf.cell((xi, y)) {
                    if ci.symbol() == "╯" {
                        card_bot_x = Some(xi);
                    }
                }
            }
        }
    }

    assert!(card_top_x.is_some(), "Top right corner ╮ of processing card must be found");
    assert!(card_bot_x.is_some(), "Bottom right corner ╯ of processing card must be found");
    assert!(card_pipe_x.is_some(), "Content right border │ of processing card must be found");

    assert_eq!(card_top_x, card_bot_x, "Top right and bottom right corner must align!");
    assert_eq!(card_pipe_x, card_bot_x, "Content right border and bottom corner must align!");

    // 3. Advance tick to 5 and verify frame update
    state.processing_tick = 5;
    terminal.draw(|f| state.render_ui(f)).unwrap();
    let buf5 = terminal.backend().buffer().clone();
    let mut rendered_lines5 = Vec::new();
    for y in 0..25 {
        let mut row_str = String::new();
        for x in 0..120 {
            let symbol = buf5.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ");
            row_str.push_str(symbol);
        }
        rendered_lines5.push(row_str);
    }
    let full_rendered5 = rendered_lines5.join("\n");
    // Tick 5 spinner frame: '⠴'
    assert!(full_rendered5.contains('⠴'), "Braille spinner frame '⠴' must be present for tick 5");

    // 4. Test dynamic_badge on Running node
    let running_node = CockpitDagNode::new("task-1", "Test Node", "gemini-2.5-flash")
        .with_status(CockpitNodeStatus::Running { progress_pct: 25 });
    let (badge_str, _) = running_node.status.dynamic_badge(5);
    assert_eq!(badge_str, "[⠴ RUNNING]");

    // 5. Test cancellation
    state.cancel_processing();
    assert!(!state.is_processing);
    assert!(state.processing_start.is_none());
    assert!(state.processing_prompt_preview.is_empty());
    assert!(state.conversation.iter().any(|c| c.content.contains("cancelled by user")));

    // 6. Test submit_current_prompt
    state.model_pill = "proxy-standalone-model".to_string();
    state.prompt_input = "calculate fibonacci(40)".to_string();
    let rx = state.submit_current_prompt();
    assert!(rx.is_some(), "submit_current_prompt should return background receiver");
    assert!(state.is_processing, "state.is_processing must be true after submission");
    assert_eq!(state.nodes.len(), 1);
    assert_eq!(state.conversation.len(), 2); // 1 cancelled assistant + 1 user prompt

    // Wait for the background result and apply it
    let result = rx.unwrap().await.unwrap();
    state.apply_prompt_result(result);
    assert!(!state.is_processing, "state.is_processing must be false after applying result");
    assert_eq!(state.conversation.len(), 3); // cancelled + user + new assistant
}

#[test]
fn test_brutal_response_box_copy_icon_and_mouse_click() {
    let mut state = CockpitState::new();

    let response_text = "• The ultimate chicken-and-egg question! Scientifically and historically speaking, the answer is **the egg**.\nHere is why:\n1. Evolutionary Biology: Animals that lay eggs existed hundreds of millions of years before pigs.";

    let tool_call = CockpitToolCall::new(
        "gemini_api_direct",
        "model=wizardlm-uncensored:latest",
        "SUCCESS",
        56348,
        Some(response_text.to_string()),
    );

    state.add_assistant_message(
        response_text.to_string(),
        "wizardlm-uncensored:latest",
        120,
        56348,
        None,
        vec![tool_call],
    );

    // 1. Render in 120-column terminal
    let backend = ratatui::backend::TestBackend::new(120, 25);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| state.render_ui(f)).unwrap();

    let rendered_text = state.render_headless_to_string(120, 25);
    println!("\n--- RENDERED BOX WITH COPY ICON ---");
    for line in rendered_text.lines() {
        if line.contains('╭') || line.contains('│') || line.contains('╰') {
            println!("{}", line);
        }
    }

    // 2. Verify top border contains tool icon 🛠️, title, and only copy icon near the top right corner
    assert!(rendered_text.contains("gemini_api_direct"), "Must contain tool name");
    assert!(rendered_text.contains("model=wizardlm-uncensored:latest"), "Must contain model tag");
    assert!(rendered_text.contains("🛠️"), "Must contain tool icon 🛠️ for gemini_api_direct");
    assert!(rendered_text.contains("📋"), "Must contain copy icon 📋");
    assert!(!rendered_text.contains("Copy"), "Must omit the word 'Copy'");
    assert!(!rendered_text.contains("[📋]"), "Must omit brackets around copy icon");
    assert!(rendered_text.contains("📋") && rendered_text.contains("─╮"), "Must contain only copy icon 📋 near corner");

    // 3. Verify hitboxes populated in state
    let hitboxes = state.copy_hitboxes.lock().unwrap().clone();
    assert_eq!(hitboxes.len(), 1, "Exactly one copy hitbox must be registered for the response box");
    let hb = &hitboxes[0];
    assert_eq!(hb.content, response_text, "Hitbox must contain the full response content");
    assert_eq!(hb.label, "gemini_api_direct response");

    // 4. Simulate mouse click on the copy icon coordinates
    let click_x = (hb.x_start + hb.x_end) / 2;
    let click_y = hb.screen_y;
    let handled = state.handle_mouse_click(click_x, click_y);
    assert!(handled, "Mouse click on copy icon must be handled");
    assert_eq!(state.last_copied_id, Some(hb.card_id.clone()));

    // 5. Re-render and verify feedback changes to ✓
    let rendered_after_copy = state.render_headless_to_string(120, 25);
    println!("\n--- RENDERED BOX AFTER COPY (FEEDBACK) ---");
    for line in rendered_after_copy.lines() {
        if line.contains('╭') || line.contains('│') || line.contains('╰') {
            println!("{}", line);
        }
    }
    assert!(rendered_after_copy.contains("✓"), "Button must show ✓ after clicking");
    assert!(!rendered_after_copy.contains("[✓]"), "Must omit brackets around checkmark");

    // 6. Test copy_latest_response helper
    state.last_copied_id = None;
    state.copy_latest_response();
    assert!(state.last_copied_id.is_some(), "copy_latest_response must successfully copy the response");
}


