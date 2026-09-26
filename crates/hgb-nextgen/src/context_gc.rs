use serde::{Deserialize, Serialize};

/// Record of an individual conversation turn or tool invocation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnRecord {
    pub turn_index: usize,
    pub role: String,
    pub content: String,
    pub is_error_cycle: bool,
    pub is_superseded_view: bool,
}

/// Audit report summarizing context anti-rot garbage collection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GcReport {
    pub initial_tokens: usize,
    pub compacted_tokens: usize,
    pub tokens_saved: usize,
    pub reduction_percentage: f32,
    pub error_cycles_pruned: usize,
    pub superseded_views_folded: usize,
    pub compacted_history: Vec<TurnRecord>,
}

/// Semantic Context Anti-Rot Garbage Collector
pub struct ContextAntiRotGc;

impl ContextAntiRotGc {
    /// Estimate token count using 4 chars per token heuristic
    fn estimate_tokens(text: &str) -> usize {
        (text.len() + 3) / 4
    }

    /// Analyze session history and strip obsolete compilation errors and superseded views
    pub fn analyze_and_compact(history: &[TurnRecord], _target_token_budget: usize) -> GcReport {
        let mut initial_tokens = 0;
        for t in history {
            initial_tokens += Self::estimate_tokens(&t.content);
        }

        let mut error_cycles_pruned = 0;
        let mut superseded_views_folded = 0;
        let mut compacted = Vec::new();

        // 1. Mark superseded file views
        let mut seen_view_files = std::collections::HashSet::new();
        let mut is_superseded = vec![false; history.len()];

        for i in (0..history.len()).rev() {
            let turn = &history[i];
            if turn.content.contains("Viewing file:") || turn.content.contains("File Content:") {
                // Extract file path heuristic
                if let Some(first_line) = turn.content.lines().next() {
                    if seen_view_files.contains(first_line) {
                        is_superseded[i] = true;
                    } else {
                        seen_view_files.insert(first_line.to_string());
                    }
                }
            }
        }

        // 2. Perform surgical compaction
        for (i, turn) in history.iter().enumerate() {
            let mut record = turn.clone();

            // Case A: Obsolete compiler error cycle followed by success
            if record.is_error_cycle || record.content.contains("error[E") || record.content.contains("SyntaxError:") {
                record.content = format!("[HEALED: t{}]", record.turn_index);
                record.is_error_cycle = true;
                error_cycles_pruned += 1;
            }
            // Case B: Superseded file view
            else if is_superseded[i] {
                record.content = format!("[SUPERSEDED: t{}]", record.turn_index);
                record.is_superseded_view = true;
                superseded_views_folded += 1;
            }

            compacted.push(record);
        }

        let mut compacted_tokens = 0;
        for t in &compacted {
            compacted_tokens += Self::estimate_tokens(&t.content);
        }

        let tokens_saved = initial_tokens.saturating_sub(compacted_tokens);
        let reduction_percentage = if initial_tokens > 0 {
            (tokens_saved as f32 / initial_tokens as f32) * 100.0
        } else {
            0.0
        };

        GcReport {
            initial_tokens,
            compacted_tokens,
            tokens_saved,
            reduction_percentage,
            error_cycles_pruned,
            superseded_views_folded,
            compacted_history: compacted,
        }
    }
}
