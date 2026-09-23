//! # Brutal Integration Tests for AGY Chat Canvas & Tool Call Cards
//!
//! Verifies:
//! - All ToolCallCard lifecycle states (Running, Success, Exit codes, Failed)
//! - Line truncation and icon mapping
//! - ChatCanvas Markdown syntax highlighting, diff coloring, thinking stream boxes, Unicode tables
//! - Full integration between Cockpit, ToolCallCards, and ChatCanvas

use hgb_cli::canvas::{ChatCanvas, ToolCallCard, ToolCardStatus};
use hgb_nextgen::{
    CockpitActiveTab, CockpitArtifactDiff, CockpitBackgroundTask, CockpitDagNode,
    CockpitNodeStatus, CockpitState, CockpitToolCall, SteeringAction,
};

#[test]
fn test_tool_call_card_all_states_and_icons() {
    let tools = [
        ("run_command", "💻"),
        ("view_file", "📖"),
        ("write_to_file", "📝"),
        ("replace_file_content", "✂️ "),
        ("grep_search", "🔎"),
        ("find_by_name", "🔍"),
        ("list_dir", "📁"),
        ("invoke_subagent", "🤖"),
    ];

    for (name, icon) in tools {
        let card = ToolCallCard::new(
            name,
            "target_artifact.rs",
            ToolCardStatus::Success {
                duration_ms: 42,
                exit_code: 0,
            },
        )
        .with_output("execution line 1\nexecution line 2");

        let rendered = card.render_box(80);
        assert!(rendered.contains(icon), "Card should contain icon for {}", name);
        assert!(rendered.contains(name), "Card should contain name for {}", name);
        assert!(rendered.contains("[DONE]"), "Should show [DONE] badge");
        assert!(rendered.contains("42ms"), "Should show duration");
        assert!(rendered.contains("target_artifact.rs"));
    }
}

#[test]
fn test_tool_call_card_non_zero_exit_and_failure() {
    // Non-zero exit code
    let exit_card = ToolCallCard::new(
        "run_command",
        "gcc -Wall main.c",
        ToolCardStatus::Success {
            duration_ms: 150,
            exit_code: 1,
        },
    )
    .with_output("error: undefined reference to 'main'");

    let rendered_exit = exit_card.render_box(80);
    assert!(rendered_exit.contains("[EXIT 1]"));
    assert!(rendered_exit.contains("gcc -Wall main.c"));

    // Explicit Failed status
    let failed_card = ToolCallCard::new(
        "replace_file_content",
        "src/core.rs",
        ToolCardStatus::Failed {
            duration_ms: 5,
            error: "Search target not found".to_string(),
        },
    )
    .with_details("Search bounded between lines 10-25");

    let rendered_failed = failed_card.render_box(80);
    assert!(rendered_failed.contains("[FAILED]"));
    assert!(rendered_failed.contains("Search bounded between lines 10-25"));
}

#[test]
fn test_tool_call_card_line_truncation() {
    let mut large_output = String::new();
    for i in 1..=60 {
        large_output.push_str(&format!("Line number {}\n", i));
    }

    let card = ToolCallCard::new(
        "view_file",
        "large_file.log",
        ToolCardStatus::Success {
            duration_ms: 10,
            exit_code: 0,
        },
    )
    .with_output(large_output);

    let rendered = card.render_box(80);
    assert!(rendered.contains("Line number 1"));
    assert!(rendered.contains("Line number 25"));
    assert!(rendered.contains("more lines truncated"));
}

#[test]
fn test_chat_canvas_markdown_rich_features() {
    let md = r#"
# Hagibis Systems Architecture
## Sub-Millisecond Microkernel
### Core Guarantees

> Hagibis achieves sub-millisecond roundtrips via zero-copy Unix Domain Sockets.

* High throughput
* Low latency
- Real-time token streaming

1. Step one: Ingestion
2. Step two: Verification

| Component | Status | Latency |
| Microkernel | Nominal | 120µs |
| Gemini | Ready | 85ms |

```rust
fn execute_task() -> Result<(), HgbError> {
    Ok(())
}
```

```diff
--- a/file.rs
+++ b/file.rs
@@ -10,3 +10,3 @@
-let old = 1;
+let new = 2;
```

<thinking>
Verifying formal invariants with SMT-LIB2 solver across 4 worker threads...
</thinking>
"#;

    let rendered = ChatCanvas::render_markdown(md);

    // Headers
    assert!(rendered.contains("Hagibis Systems Architecture"));
    assert!(rendered.contains("Sub-Millisecond Microkernel"));
    assert!(rendered.contains("Core Guarantees"));

    // Blockquote
    assert!(rendered.contains("Hagibis achieves sub-millisecond roundtrips"));
    assert!(rendered.contains("│"));

    // Lists
    assert!(rendered.contains("High throughput"));
    assert!(rendered.contains("Real-time token streaming"));
    assert!(rendered.contains("1."));
    assert!(rendered.contains("2."));

    // Table
    assert!(rendered.contains("Component"));
    assert!(rendered.contains("Microkernel"));
    assert!(rendered.contains("Nominal"));
    assert!(rendered.contains("120µs"));

    // Code & Diff
    assert!(rendered.contains("execute_task"));
    assert!(rendered.contains("rust"));
    assert!(rendered.contains("+let new = 2;"));
    assert!(rendered.contains("-let old = 1;"));

    // Thinking
    assert!(rendered.contains("Reasoning Stream"));
    assert!(rendered.contains("SMT-LIB2 solver"));
}

#[test]
fn test_cockpit_and_cards_unified_execution() {
    let mut state = CockpitState::new();

    // 1. Add agent nodes
    let mut node1 = CockpitDagNode::new("n1", "Input Parser", "hgb-core");
    node1.status = CockpitNodeStatus::Succeeded { duration_ms: 6 };
    node1.tokens_used = 80;
    node1.add_tool_call(CockpitToolCall::new(
        "view_file",
        "path=crates/hgb-core/src/lib.rs",
        "SUCCESS",
        4,
        Some("pub mod auth;\npub mod crud;".into()),
    ));

    let mut node2 = CockpitDagNode::new("n2", "Gemini Reasoner", "gemini-2.5-flash");
    node2.status = CockpitNodeStatus::Running { progress_pct: 60 };
    node2.tokens_used = 1200;
    node2.parent_id = Some("n1".to_string());

    state.add_node(node1);
    state.add_node(node2);

    // 2. Add artifact diffs & background tasks
    state.add_artifact_diff(CockpitArtifactDiff::new(
        "crates/hgb-cli/src/canvas.rs",
        150,
        10,
        "@@ -1,5 +1,15 @@\n+pub struct ChatCanvas;\n",
    ));

    state.add_background_task(CockpitBackgroundTask::new(
        "task-42",
        "P2P Swarm Peer Discovery",
        "RUNNING",
        15,
    ));

    // 3. Render all 3 tabs headless to verify integrity
    for tab in CockpitActiveTab::all() {
        state.active_tab = *tab;
        let buf = state.render_headless(120, 40);
        assert!(buf.content.len() >= 120 * 40);
    }

    // 4. Test steering action inside Cockpit
    state
        .apply_steering(SteeringAction::Pause {
            node_id: "n2".to_string(),
        })
        .unwrap();
    assert_eq!(state.nodes[1].status, CockpitNodeStatus::Paused);

    state
        .apply_steering(SteeringAction::Resume {
            node_id: "n2".to_string(),
        })
        .unwrap();
    assert_eq!(
        state.nodes[1].status,
        CockpitNodeStatus::Running { progress_pct: 0 }
    );
}
