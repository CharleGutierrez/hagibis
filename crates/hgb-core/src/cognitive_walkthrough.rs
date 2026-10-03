//! # CognitiveWalkthrough - Interactive Invariant & Architectural Diff Walkthrough
//!
//! Eliminates the "Understanding Bottleneck" in vibe coding.
//! Instead of dumping unparsed, overwhelming diffs, synthesizes a structured cognitive mental model:
//! 1. Core Invariant: The business rule or contract established.
//! 2. Blast Radius & Callers: Downstream dependents requiring attention.
//! 3. Verification Anchor: How correctness and regression resistance are proven.

use serde::{Deserialize, Serialize};

/// High-signal cognitive card for a single modified file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitiveCard {
    pub file_path: String,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub core_invariant: String,
    pub blast_radius_summary: String,
    pub verification_anchor: String,
    pub cognitive_load_score: u8, // 1 to 10
    pub key_takeaways: Vec<String>,
}

/// Comprehensive walkthrough report across all modified files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkthroughReport {
    pub cards: Vec<CognitiveCard>,
    pub total_files_changed: usize,
    pub total_lines_added: usize,
    pub total_lines_removed: usize,
    pub overall_architectural_intent: String,
    pub average_cognitive_score: f32,
    pub executive_summary: String,
}

pub struct CognitiveWalkthrough;

impl CognitiveWalkthrough {
    /// Generate a structured cognitive walkthrough from file diffs and intent
    pub fn generate_walkthrough(
        intent: &str,
        file_diffs: &[(&str, &str)], // (file_path, diff_content)
    ) -> WalkthroughReport {
        let mut cards = Vec::new();
        let mut total_add = 0;
        let mut total_del = 0;
        let mut total_score: u32 = 0;

        for (path, diff) in file_diffs {
            let mut added = 0;
            let mut removed = 0;
            let mut has_control_flow = false;
            let mut has_type_def = false;
            let mut has_error_handling = false;
            let mut has_auth = false;

            for line in diff.lines() {
                if line.starts_with('+') && !line.starts_with("+++") {
                    added += 1;
                    let trimmed = line.trim_start_matches('+').trim();
                    if trimmed.contains("if ") || trimmed.contains("match ") || trimmed.contains("for ") || trimmed.contains("while ") {
                        has_control_flow = true;
                    }
                    if trimmed.contains("struct ") || trimmed.contains("enum ") || trimmed.contains("interface ") || trimmed.contains("type ") {
                        has_type_def = true;
                    }
                    if trimmed.contains("Result<") || trimmed.contains("Error") || trimmed.contains("try ") || trimmed.contains("catch ") {
                        has_error_handling = true;
                    }
                    if trimmed.contains("auth") || trimmed.contains("token") || trimmed.contains("jwt") || trimmed.contains("session") {
                        has_auth = true;
                    }
                } else if line.starts_with('-') && !line.starts_with("---") {
                    removed += 1;
                }
            }

            total_add += added;
            total_del += removed;

            // Calculate cognitive complexity (1-10)
            let mut score = 2u8;
            if added > 50 { score += 2; }
            if has_control_flow { score += 2; }
            if has_type_def { score += 1; }
            if has_error_handling { score += 1; }
            if has_auth { score += 2; }
            score = score.min(10);
            total_score += score as u32;

            // Synthesize Invariant
            let invariant = if has_auth {
                "Enforces authenticated identity validation and prevents unauthorized state mutation.".to_string()
            } else if has_type_def {
                "Defines immutable domain types and structural schema boundaries.".to_string()
            } else if has_error_handling {
                "Guarantees fail-safe error propagation with explicit Result wrapping.".to_string()
            } else {
                format!("Implements business logic changes for '{}'", intent)
            };

            // Synthesize Blast Radius
            let blast = if path.contains("model") || path.contains("schema") || has_type_def {
                "High: Upstream database repositories and downstream API serializers depend on this schema.".to_string()
            } else if path.contains("route") || path.contains("handler") || path.contains("controller") {
                "Medium: Client-facing HTTP contract modified; external consumers and API tests impacted.".to_string()
            } else {
                "Localized: Internal helper/service logic; blast radius is confined to immediate callers.".to_string()
            };

            // Synthesize Verification Anchor
            let verification = if path.contains("test") {
                "Direct unit test coverage ensuring assertions hold.".to_string()
            } else {
                "Compiler typecheck + AntiPlacebo mutation verification gate required before merging.".to_string()
            };

            let mut takeaways = Vec::new();
            takeaways.push(format!("Modified {} lines (+{}, -{})", added + removed, added, removed));
            if has_control_flow {
                takeaways.push("Introduced conditional branching logic".to_string());
            }
            if has_type_def {
                takeaways.push("Updated structural type contracts".to_string());
            }
            
            // Actually read the physical file to get byte and line counts!
            if let Ok(content) = std::fs::read_to_string(path) {
                let bytes = content.len();
                let lines = content.lines().count();
                takeaways.push(format!("Physical file size: {} bytes, {} lines", bytes, lines));
            }

            cards.push(CognitiveCard {
                file_path: path.to_string(),
                lines_added: added,
                lines_removed: removed,
                core_invariant: invariant,
                blast_radius_summary: blast,
                verification_anchor: verification,
                cognitive_load_score: score,
                key_takeaways: takeaways,
            });
        }

        let total_files = cards.len();
        let avg_score = if total_files > 0 {
            total_score as f32 / total_files as f32
        } else {
            1.0
        };

        let exec_summary = format!(
            "Walkthrough for '{}': {} files modified (+{}, -{}) with average cognitive load {:.1}/10.",
            intent, total_files, total_add, total_del, avg_score
        );

        WalkthroughReport {
            cards,
            total_files_changed: total_files,
            total_lines_added: total_add,
            total_lines_removed: total_del,
            overall_architectural_intent: intent.to_string(),
            average_cognitive_score: avg_score,
            executive_summary: exec_summary,
        }
    }
}
