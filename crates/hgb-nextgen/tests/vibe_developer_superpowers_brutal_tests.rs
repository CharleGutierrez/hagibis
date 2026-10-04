//! # Brutal Integration Tests: 6 Essential Vibe Developer Features
//!
//! Verifies:
//! 1. Pinned North Star Goal HUD & Smart Auto-Compaction
//! 2. In-Canvas Inline Diff Cards with 1-click/keyboard hunk acceptance & rejection
//! 3. Crash Interceptor & "1-Click Heal" Prompt (rustc, cargo, panic, python, node)
//! 4. Ephemeral Mobile QR-Code & Dev Tunnels (/share, /tunnel, ANSI QR matrix)
//! 5. In-Terminal Visual UI Previews (Kitty/Sixel/24-bit Half-Block TrueColor)
//! 6. Push-to-Talk Voice Prompting Hook (F7 / /listen, audio capture, transcription)

use hgb_nextgen::compactor::SmartAutoCompactor;
use hgb_nextgen::crash_interceptor::{CrashDetector, CrashSourceType, InterceptedCrash};
use hgb_nextgen::diff_hud::SelectivePatcher;
use hgb_nextgen::qr::{render_mobile_test_card, QrMatrix};
use hgb_nextgen::terminal_graphics::{
    detect_graphics_protocol, ImageBuffer, RgbPixel, TerminalGraphicsProtocol,
};
use hgb_nextgen::{
    CockpitChatItem, CockpitChatSender, CockpitState, CockpitToolCall,
    DiffAction, DiffActionHitbox, DiffCardStatus,
};
use std::path::PathBuf;

// ============================================================================
// FEATURE 1: Pinned North Star Goal HUD & Smart Auto-Compaction Tests
// ============================================================================

#[test]
fn test_pinned_north_star_goal_lifecycle() {
    let mut state = CockpitState::new();
    assert_eq!(state.pinned_goal, None);

    // 1. Set goal via method / command
    state.set_pinned_goal("Refactor Microkernel to sub-100 microsecond latency");
    assert_eq!(
        state.pinned_goal.as_deref(),
        Some("Refactor Microkernel to sub-100 microsecond latency")
    );

    // 2. Render headless canvas: Glowing North Star Banner must be present!
    let rendered = state.render_headless_to_string(100, 30);
    assert!(
        rendered.contains("NORTH STAR") || rendered.contains("🎯"),
        "Banner should be rendered when pinned_goal is present"
    );
    assert!(
        rendered.contains("Refactor Microkernel"),
        "Goal text must be present in rendered banner"
    );

    // 3. Clear goal
    state.clear_pinned_goal();
    assert_eq!(state.pinned_goal, None);

    let rendered_cleared = state.render_headless_to_string(100, 30);
    assert!(
        !rendered_cleared.contains("Refactor Microkernel"),
        "Cleared goal text must no longer appear"
    );
}

#[test]
fn test_smart_auto_compactor_preserves_intent_diffs_and_goal() {
    let mut conv = vec![
        CockpitChatItem {
            sender: CockpitChatSender::User,
            content: "Fix memory leak in zero-copy buffer".to_string(),
            tokens: 8,
            duration_ms: 0,
            timestamp: "10:00:00".to_string(),
            thinking: None,
            tool_calls: Vec::new(),
        },
        CockpitChatItem {
            sender: CockpitChatSender::Assistant { model: "gemini-2.5-pro".to_string() },
            content: "I have identified the leak. Here is the unified diff:".to_string(),
            tokens: 12,
            duration_ms: 200,
            timestamp: "10:00:02".to_string(),
            thinking: Some("Deep thinking line 1\nLine 2\nLine 3\nLine 4\nLine 5".to_string()),
            tool_calls: vec![
                CockpitToolCall::new(
                    "run_command",
                    "cargo test --test leak_tests",
                    "SUCCESS",
                    350,
                    Some("running 80 tests\n... test 1 ok\n... test 80 ok\ntest result: ok. 80 passed; 0 failed".to_string()),
                ),
                CockpitToolCall::new(
                    "replace_file_content",
                    "path=buffer.rs",
                    "SUCCESS",
                    45,
                    Some("diff --git a/buffer.rs b/buffer.rs\n--- a/buffer.rs\n+++ b/buffer.rs\n@@ -10,3 +10,3 @@\n-let leak = Box::leak(b);\n+let safe = Arc::new(b);\n".to_string()),
                ),
            ],
        },
    ];

    let goal = "Zero-Leak Microkernel Milestone";
    let report = SmartAutoCompactor::compact_conversation(&mut conv, Some(goal));

    // 1. User intent must be 100% intact
    assert_eq!(conv[0].content, "Fix memory leak in zero-copy buffer");
    assert_eq!(conv[0].sender, CockpitChatSender::User);

    // 2. Goal must be preserved in report
    assert_eq!(report.pinned_goal_preserved.as_deref(), Some(goal));

    // 3. Diff tool output must be preserved intact (never summarized)
    let diff_call = &conv[1].tool_calls[1];
    let diff_out = diff_call.output_snippet.as_ref().unwrap();
    assert!(diff_out.contains("diff --git a/buffer.rs b/buffer.rs"));
    assert!(diff_out.contains("+let safe = Arc::new(b);"));

    // 4. Test run tool output must be collapsed into semantic summary badge
    let test_call = &conv[1].tool_calls[0];
    let test_out = test_call.output_snippet.as_ref().unwrap();
    assert!(test_out.contains("Compacted: test run -> test result: ok. 80 passed; 0 failed"));

    // 5. Verbose thinking stream collapsed
    assert!(conv[1].thinking.as_ref().unwrap().contains("Compacted thinking"));
    assert!(report.characters_saved > 0);
}

// ============================================================================
// FEATURE 2: In-Canvas Interactive Diff Cards & Hunk Stamping Tests
// ============================================================================

#[test]
fn test_in_canvas_diff_card_accept_and_reject() {
    let mut state = CockpitState::new();

    let raw_diff = r#"--- a/src/core.rs
+++ b/src/core.rs
@@ -1,3 +1,3 @@
 fn compute() {
-    let x = 1;
+    let x = 42;
 }
"#;

    // Add diff card
    state.add_diff_card("src/core.rs", raw_diff);
    assert_eq!(state.diff_cards.len(), 1);
    assert_eq!(state.diff_cards[0].status, DiffCardStatus::Pending);

    // Verify hunks parsed via SelectivePatcher
    assert_eq!(state.diff_cards[0].hunks.len(), 1);
    assert_eq!(state.diff_cards[0].hunks[0].file_path, PathBuf::from("src/core.rs"));

    // Render headless: diff card with [✓ Accept] and [✗ Reject] buttons must appear
    let rendered = state.render_headless_to_string(100, 30);
    assert!(rendered.contains("Diff: src/core.rs") || rendered.contains("src/core.rs"));
    assert!(rendered.contains("Accept") || rendered.contains("✓"));
    assert!(rendered.contains("Reject") || rendered.contains("✗"));

    // Test Alt+A / Accept action
    let card_id = state.diff_cards[0].id.clone();
    let accepted = state.accept_diff_card(Some(&card_id));
    assert!(accepted);
    assert_eq!(state.diff_cards[0].status, DiffCardStatus::Accepted);

    // Test applying accepted hunks with SelectivePatcher
    let original = "fn compute() {\n    let x = 1;\n}\n";
    let patched = SelectivePatcher::apply_accepted_hunks(original, &state.diff_cards[0].hunks).unwrap();
    assert_eq!(patched, "fn compute() {\n    let x = 42;\n}\n");

    // Test Rejection
    state.add_diff_card("src/rejected.rs", raw_diff);
    let rej_id = state.diff_cards[1].id.clone();
    let rejected = state.reject_diff_card(Some(&rej_id));
    assert!(rejected);
    assert_eq!(state.diff_cards[1].status, DiffCardStatus::Rejected);

    // Rejected hunk leaves original content unchanged
    let orig_rejected = "fn compute() {\n    let x = 1;\n}\n";
    let mut rej_hunks = state.diff_cards[1].hunks.clone();
    rej_hunks[0].accepted = Some(false);
    let patched_rej = SelectivePatcher::apply_accepted_hunks(orig_rejected, &rej_hunks).unwrap();
    assert_eq!(patched_rej, orig_rejected);
}

#[test]
fn test_diff_card_mouse_hitbox_interaction() {
    let mut state = CockpitState::new();
    state.add_diff_card("src/service.rs", "--- a/s\n+++ b/s\n@@ -1,1 +1,1 @@\n-old\n+new\n");

    let card_id = state.diff_cards[0].id.clone();

    // Register simulated hitboxes on screen row 10
    if let Ok(mut hbs) = state.diff_hitboxes.lock() {
        hbs.push(DiffActionHitbox {
            screen_y: 10,
            x_start: 40,
            x_end: 50,
            card_id: card_id.clone(),
            action: DiffAction::Accept,
        });
        hbs.push(DiffActionHitbox {
            screen_y: 10,
            x_start: 55,
            x_end: 65,
            card_id: card_id.clone(),
            action: DiffAction::Reject,
        });
    }

    // Click on Accept hitbox (row 10, col 45)
    let handled = state.handle_mouse_click(45, 10);
    assert!(handled);
    assert_eq!(state.diff_cards[0].status, DiffCardStatus::Accepted);
}

// ============================================================================
// FEATURE 3: Crash Interceptor & "Zero-Click Heal" Prompt Tests
// ============================================================================

#[test]
fn test_crash_detector_all_runtimes() {
    // 1. Rustc Compiler Diagnostic
    let rustc_out = r#"
error[E0425]: cannot find value `unresolved_var` in this scope
  --> crates/hgb-core/src/actor.rs:99:12
   |
99 |     println!("{}", unresolved_var);
   |                    ^^^^^^^^^^^^^^ not found in this scope
"#;
    let crash1 = CrashDetector::detect(rustc_out).expect("Must detect rustc error");
    assert_eq!(crash1.source_type, CrashSourceType::Rustc);
    assert_eq!(crash1.file_path, PathBuf::from("crates/hgb-core/src/actor.rs"));
    assert_eq!(crash1.line_number, 99);
    assert_eq!(crash1.column_number, Some(12));
    assert_eq!(crash1.error_type, "E0425");

    // 2. Unhandled Rust Panic
    let panic_out = "thread 'main' panicked at crates/hgb-storage/src/wal.rs:52:9:\nWAL corruption: checksum mismatch";
    let crash2 = CrashDetector::detect(panic_out).expect("Must detect panic");
    assert_eq!(crash2.source_type, CrashSourceType::UnhandledPanic);
    assert_eq!(crash2.file_path, PathBuf::from("crates/hgb-storage/src/wal.rs"));
    assert_eq!(crash2.line_number, 52);
    assert!(crash2.error_message.contains("WAL corruption"));

    // 3. Python Traceback
    let py_out = r#"
Traceback (most recent call last):
  File "scripts/benchmark.py", line 120, in run_benchmark
    res = 100 / divisor
ZeroDivisionError: division by zero
"#;
    let crash3 = CrashDetector::detect(py_out).expect("Must detect python error");
    assert_eq!(crash3.source_type, CrashSourceType::PythonTraceback);
    assert_eq!(crash3.file_path, PathBuf::from("scripts/benchmark.py"));
    assert_eq!(crash3.line_number, 120);
    assert_eq!(crash3.error_type, "ZeroDivisionError");

    // 4. Node.js Exception
    let node_out = r#"
ReferenceError: activeSession is not defined
    at Server.handleRequest (/server/src/api.js:84:10)
"#;
    let crash4 = CrashDetector::detect(node_out).expect("Must detect node error");
    assert_eq!(crash4.source_type, CrashSourceType::NodeCrash);
    assert_eq!(crash4.file_path, PathBuf::from("/server/src/api.js"));
    assert_eq!(crash4.line_number, 84);
}

#[test]
fn test_crash_interceptor_banner_and_heal_trigger() {
    let mut state = CockpitState::new();

    let crash = InterceptedCrash {
        source_type: CrashSourceType::Rustc,
        file_path: PathBuf::from("src/main.rs"),
        line_number: 77,
        column_number: Some(5),
        error_type: "E0308".to_string(),
        error_message: "mismatched types".to_string(),
        stack_snippet: None,
    };

    state.intercepted_crash = Some(crash);

    // Verify Crash Banner appears in rendered canvas
    let rendered = state.render_headless_to_string(100, 30);
    assert!(rendered.contains("CRASH INTERCEPTOR"));
    assert!(rendered.contains("src/main.rs:77"));
    assert!(rendered.contains("1-Click Heal") || rendered.contains("🚑"));

    // Trigger Heal (via F5 / /heal / click)
    state.trigger_heal();
    assert!(state.prompt_input.contains("/heal"));
    assert!(state.prompt_input.contains("src/main.rs:77"));
}

// ============================================================================
// FEATURE 4: Ephemeral Dev Tunnel & ASCII Mobile QR-Code Tests
// ============================================================================

#[test]
fn test_qr_encoder_and_unicode_half_blocks() {
    let url = "http://192.168.1.100:8080";
    let qr = QrMatrix::encode_url(url);
    assert!(qr.size >= 21);

    // Verify finder patterns
    assert!(qr.modules[0][0]);
    assert!(qr.modules[0][6]);
    assert!(qr.modules[6][0]);

    // Verify 2-row half-blocks
    let lines = qr.render_half_blocks();
    assert!(!lines.is_empty());
    for line in &lines {
        for ch in line.chars() {
            assert!(ch == '█' || ch == '▀' || ch == '▄' || ch == ' ');
        }
    }
}

#[test]
fn test_mobile_test_card_generation() {
    let card = render_mobile_test_card("http://192.168.1.55:3000", 3000, 80);
    let output = card.join("\n");

    assert!(output.contains("Ephemeral Dev Tunnel & Mobile Test"));
    assert!(output.contains("http://192.168.1.55:3000"));
    assert!(output.contains("3000"));
    assert!(output.contains('█') || output.contains('▀'));
}

#[test]
fn test_cockpit_tunnel_card_lifecycle() {
    let mut state = CockpitState::new();
    state.add_tunnel_card(5173);

    let rendered = state.render_headless_to_string(100, 35);
    assert!(rendered.contains("Ephemeral Dev Tunnel") || rendered.contains("5173"));
}

// ============================================================================
// FEATURE 5: In-Terminal Visual UI Previews Tests
// ============================================================================

#[test]
fn test_terminal_graphics_protocol_and_truecolor_rendering() {
    let proto = detect_graphics_protocol();
    assert!(matches!(
        proto,
        TerminalGraphicsProtocol::HalfBlockTrueColor | TerminalGraphicsProtocol::Kitty | TerminalGraphicsProtocol::Sixel
    ));

    // Create 4x2 test image with distinct RGB colors
    let mut img = ImageBuffer::new(4, 2, RgbPixel::BLACK);
    img.set_pixel(0, 0, RgbPixel::RED);
    img.set_pixel(1, 0, RgbPixel::GREEN);
    img.set_pixel(0, 1, RgbPixel::BLUE);
    img.set_pixel(1, 1, RgbPixel::YELLOW);

    let truecolor_lines = img.render_truecolor_half_blocks();
    assert_eq!(truecolor_lines.len(), 1); // 2 rows packed into 1 half-block line
    assert!(truecolor_lines[0].contains("▀"));
    assert!(truecolor_lines[0].contains("\x1b[38;2;"));
    assert!(truecolor_lines[0].contains("\x1b[48;2;"));
}

#[test]
fn test_cockpit_image_preview_card_in_canvas() {
    let mut state = CockpitState::new();
    state.add_image_preview("Authentication Modal Proxyup", "assets/login_proxyup.png");

    let rendered = state.render_headless_to_string(100, 35);
    assert!(rendered.contains("UI Preview: Authentication Modal Proxyup") || rendered.contains("Authentication Modal"));
    assert!(rendered.contains("Dimensions:") || rendered.contains("Protocol:"));
}

// ============================================================================
// FEATURE 6: Push-to-Talk / Audio Prompting Hook Tests
// ============================================================================

#[test]
fn test_push_to_talk_state_and_recording_indicator() {
    let mut state = CockpitState::new();
    assert!(!state.is_listening);

    // Toggle listening ON (F7 or /listen)
    state.toggle_listening();
    assert!(state.is_listening);
    assert!(state.voice_engine.session.is_active);

    // Canvas must display the animated recording indicator
    let rendered = state.render_headless_to_string(100, 30);
    assert!(rendered.contains("LISTENING") || rendered.contains("F7 to Stop"));

    // Provide simulated speech and toggle OFF
    state.voice_engine.with_simulated_speech("Add unit tests for the crash detector");
    state.toggle_listening();
    assert!(!state.is_listening);

    // Transcribed prompt must be injected into the prompt_input buffer!
    assert_eq!(state.prompt_input, "Add unit tests for the crash detector");
    assert_eq!(state.cursor_position, state.prompt_input.chars().count());
}

// ============================================================================
// COMPREHENSIVE COMBINED STRESS TEST
// ============================================================================

#[test]
fn test_all_six_vibe_features_simultaneously() {
    let mut state = CockpitState::new();

    // 1. Goal HUD
    state.set_pinned_goal("Achieve 100% Vibe Coding Perfection");

    // 2. Diff Card
    state.add_diff_card("src/superpower.rs", "--- a/src\n+++ b/src\n@@ -1,1 +1,1 @@\n-vibe_draft\n+vibe_complete\n");

    // 3. Crash Interceptor
    let crash = InterceptedCrash {
        source_type: CrashSourceType::Rustc,
        file_path: PathBuf::from("src/engine.rs"),
        line_number: 10,
        column_number: Some(2),
        error_type: "E0599".to_string(),
        error_message: "no method named `vibe`".to_string(),
        stack_snippet: None,
    };
    state.intercepted_crash = Some(crash);

    // 4. Mobile Tunnel QR Card
    state.add_tunnel_card(8000);

    // 5. Visual UI Preview Card
    state.add_image_preview("Landing Page Vibe", "assets/hero.png");

    // 6. Push-to-Talk
    state.toggle_listening();

    // Render headless canvas with ALL 6 features active simultaneously!
    let full_canvas = state.render_headless_to_string(120, 80);

    assert!(full_canvas.contains("NORTH STAR") || full_canvas.contains("100% Vibe Coding"));
    assert!(full_canvas.contains("Diff: src/superpower.rs") || full_canvas.contains("superpower.rs"));
    assert!(full_canvas.contains("CRASH INTERCEPTOR"));
    assert!(full_canvas.contains("Ephemeral Dev Tunnel") || full_canvas.contains("8000"));
    assert!(full_canvas.contains("UI Preview: Landing Page Vibe") || full_canvas.contains("Landing Page Vibe"));
    assert!(full_canvas.contains("LISTENING") || full_canvas.contains("F7 to Stop"));
}
