//! # Smart Auto-Compactor for Hagibis
//!
//! Provides intelligent conversation history compaction:
//! - Preserves user intent (all user turns kept intact)
//! - Preserves surgical diffs and hunk modifications
//! - Preserves the Pinned North Star Goal
//! - Compacts verbose tool outputs into concise semantic summaries
//! - Tracks token and character savings

use crate::cockpit::{CockpitChatItem, CockpitChatSender};
use serde::{Deserialize, Serialize};

/// Report generated after conversation compaction
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReport {
    pub original_tokens: usize,
    pub compacted_tokens: usize,
    pub characters_saved: usize,
    pub items_compacted: usize,
    pub tool_calls_summarized: usize,
    pub pinned_goal_preserved: Option<String>,
}

/// Smart Auto-Compaction Engine
pub struct SmartAutoCompactor;

impl SmartAutoCompactor {
    /// Determines whether the conversation has grown large enough to trigger auto-compaction
    pub fn should_auto_compact(conversation: &[CockpitChatItem], max_items: usize) -> bool {
        if conversation.len() > max_items {
            return true;
        }
        // Also check character weight of tool calls
        let total_chars: usize = conversation
            .iter()
            .map(|item| {
                let tool_len: usize = item
                    .tool_calls
                    .iter()
                    .filter_map(|t| t.output_snippet.as_ref())
                    .map(|s| s.len())
                    .sum();
                item.content.len() + tool_len
            })
            .sum();

        total_chars > 12_000
    }

    /// Check if a text snippet represents a unified diff that should be preserved
    pub fn contains_unified_diff(text: &str) -> bool {
        text.contains("diff --git")
            || (text.contains("--- ") && text.contains("+++ "))
            || text.contains("@@ -")
            || text.contains("```diff")
    }

    /// Summarize a single tool output into a concise semantic badge
    pub fn summarize_tool_output(tool_name: &str, output: &str) -> String {
        let trimmed = output.trim();
        let lines: Vec<&str> = trimmed.lines().collect();

        // If the output contains a unified diff, preserve it 100% intact!
        if Self::contains_unified_diff(output) {
            return output.to_string();
        }

        // If output is already concise and not a test/build run, keep it
        if lines.len() <= 2 && trimmed.len() <= 120 && !trimmed.contains("test result:") {
            return trimmed.to_string();
        }

        match tool_name {
            "run_command" | "exec" | "sh" | "bash" => {
                // Check for test outputs
                if trimmed.contains("test result:") {
                    for line in &lines {
                        if line.contains("test result:") {
                            return format!("[Compacted: test run -> {} ({} lines collapsed)]", line.trim(), lines.len());
                        }
                    }
                }
                // Check for build / compile outputs
                if trimmed.contains("Finished") || trimmed.contains("Compiling") {
                    let last = lines.last().copied().unwrap_or("done");
                    return format!("[Compacted: build output -> {} ({} lines collapsed)]", last.trim(), lines.len());
                }
                // Generic command: head + tail
                let first = lines.first().copied().unwrap_or("");
                let last = lines.last().copied().unwrap_or("");
                format!(
                    "{}\n... [Compacted {} verbose command lines] ...\n{}",
                    first,
                    lines.len().saturating_sub(2),
                    last
                )
            }
            "view_file" | "cat" | "read" => {
                let first = lines.first().copied().unwrap_or("");
                let last = lines.last().copied().unwrap_or("");
                format!(
                    "{}\n... [Compacted {} lines of file content] ...\n{}",
                    first,
                    lines.len().saturating_sub(2),
                    last
                )
            }
            "list_dir" | "ls" => {
                format!("[Compacted: directory listing containing {} entries]", lines.len())
            }
            "grep_search" | "grep" => {
                format!("[Compacted: grep results containing {} matches]", lines.len())
            }
            _ => {
                if lines.len() > 6 {
                    let first = lines.first().copied().unwrap_or("");
                    let last = lines.last().copied().unwrap_or("");
                    format!(
                        "{}\n... [Compacted {} lines] ...\n{}",
                        first,
                        lines.len().saturating_sub(2),
                        last
                    )
                } else {
                    trimmed.to_string()
                }
            }
        }
    }

    /// Compact conversation history while preserving user intent, diffs, and North Star Goal
    pub fn compact_conversation(
        conversation: &mut Vec<CockpitChatItem>,
        pinned_goal: Option<&str>,
    ) -> CompactionReport {
        let initial_items = conversation.len();
        let mut original_tokens = 0;
        let mut original_chars = 0;
        let mut tool_calls_summarized = 0;

        for item in conversation.iter() {
            original_tokens += item.tokens.max(item.content.len() / 4);
            original_chars += item.content.len();
            for tool in &item.tool_calls {
                if let Some(ref snippet) = tool.output_snippet {
                    original_chars += snippet.len();
                    original_tokens += snippet.len() / 4;
                }
            }
        }

        // Iterate through items: User items are NEVER removed or trimmed.
        // Assistant items have their verbose tool outputs summarized, and old thinking streams collapsed.
        for item in conversation.iter_mut() {
            match item.sender {
                CockpitChatSender::User => {
                    // Strict preservation: user intent remains untouched
                    continue;
                }
                CockpitChatSender::System => {
                    continue;
                }
                CockpitChatSender::Assistant { .. } => {
                    // Collapse verbose thinking streams into concise 1-line summary
                    if let Some(ref thinking) = item.thinking {
                        if thinking.lines().count() > 3 {
                            let first_line = thinking.lines().next().unwrap_or("Reasoning process completed.");
                            item.thinking = Some(format!("💭 [Compacted thinking: {}]", first_line.trim()));
                        }
                    }

                    // Compact tool outputs
                    for tool in item.tool_calls.iter_mut() {
                        if let Some(ref snippet) = tool.output_snippet {
                            let summarized = Self::summarize_tool_output(&tool.tool_name, snippet);
                            if summarized != *snippet {
                                tool.output_snippet = Some(summarized);
                                tool_calls_summarized += 1;
                            }
                        }
                    }
                }
            }
        }

        let mut compacted_tokens = 0;
        let mut compacted_chars = 0;
        for item in conversation.iter() {
            compacted_tokens += item.tokens.max(item.content.len() / 4);
            compacted_chars += item.content.len();
            for tool in &item.tool_calls {
                if let Some(ref snippet) = tool.output_snippet {
                    compacted_chars += snippet.len();
                    compacted_tokens += snippet.len() / 4;
                }
            }
        }

        let characters_saved = original_chars.saturating_sub(compacted_chars);

        CompactionReport {
            original_tokens,
            compacted_tokens,
            characters_saved,
            items_compacted: initial_items,
            tool_calls_summarized,
            pinned_goal_preserved: pinned_goal.map(|s| s.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::CockpitToolCall;

    #[test]
    fn test_user_intent_strictly_preserved() {
        let mut conv = vec![
            CockpitChatItem {
                sender: CockpitChatSender::User,
                content: "Implement the North Star pinned goal HUD in Hagibis".to_string(),
                tokens: 15,
                duration_ms: 0,
                timestamp: "12:00:00".to_string(),
                thinking: None,
                tool_calls: Vec::new(),
            },
            CockpitChatItem {
                sender: CockpitChatSender::Assistant { model: "gemini-2.5-flash".to_string() },
                content: "Here is the implementation.".to_string(),
                tokens: 10,
                duration_ms: 120,
                timestamp: "12:00:01".to_string(),
                thinking: Some("Let's analyze the goal HUD\nStep 1\nStep 2\nStep 3\nStep 4\nStep 5".to_string()),
                tool_calls: vec![CockpitToolCall::new(
                    "run_command",
                    "cargo test",
                    "SUCCESS",
                    150,
                    Some("running 50 tests\n... test 1 ok\n... test 2 ok\n... test 50 ok\ntest result: ok. 50 passed; 0 failed".to_string()),
                )],
            },
        ];

        let report = SmartAutoCompactor::compact_conversation(&mut conv, Some("Ship 6 Vibe Features"));

        // User turn content must remain completely identical
        assert_eq!(conv[0].content, "Implement the North Star pinned goal HUD in Hagibis");
        assert_eq!(conv[0].sender, CockpitChatSender::User);

        // Assistant tool call must be compacted
        let tool_out = conv[1].tool_calls[0].output_snippet.as_ref().unwrap();
        assert!(tool_out.contains("Compacted: test run -> test result: ok. 50 passed; 0 failed"));

        // Goal preserved
        assert_eq!(report.pinned_goal_preserved.as_deref(), Some("Ship 6 Vibe Features"));
        assert!(report.characters_saved > 0);
        assert_eq!(report.tool_calls_summarized, 1);
    }

    #[test]
    fn test_unified_diffs_strictly_preserved() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
-fn old() {}
+fn new() {}
"#;
        let mut conv = vec![CockpitChatItem {
            sender: CockpitChatSender::Assistant { model: "gemini".to_string() },
            content: "Diff generated".to_string(),
            tokens: 5,
            duration_ms: 10,
            timestamp: "12:00:00".to_string(),
            thinking: None,
            tool_calls: vec![CockpitToolCall::new(
                "replace_file_content",
                "target=src/main.rs",
                "SUCCESS",
                12,
                Some(diff.to_string()),
            )],
        }];

        let report = SmartAutoCompactor::compact_conversation(&mut conv, None);
        assert_eq!(conv[0].tool_calls[0].output_snippet.as_deref(), Some(diff));
        assert_eq!(report.tool_calls_summarized, 0);
    }

    #[test]
    fn test_should_auto_compact_thresholds() {
        let conv_small = vec![CockpitChatItem {
            sender: CockpitChatSender::User,
            content: "hello".into(),
            tokens: 1,
            duration_ms: 0,
            timestamp: "00:00".into(),
            thinking: None,
            tool_calls: Vec::new(),
        }];
        assert!(!SmartAutoCompactor::should_auto_compact(&conv_small, 10));

        let conv_many: Vec<_> = (0..15)
            .map(|i| CockpitChatItem {
                sender: CockpitChatSender::User,
                content: format!("msg {}", i),
                tokens: 2,
                duration_ms: 0,
                timestamp: "00:00".into(),
                thinking: None,
                tool_calls: Vec::new(),
            })
            .collect();
        assert!(SmartAutoCompactor::should_auto_compact(&conv_many, 10));
    }
}
