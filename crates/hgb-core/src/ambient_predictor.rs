//! # AmbientPredictor - Next-Action & Multi-File Cascade Anticipator
//!
//! Elevates Windsurf Cascade and Supermaven proactive intelligence.
//! Tracks recent edit history and leverages the AST dependency graph to
//! anticipate the next logical edits across related files before the user opens them.

use crate::live_graph_watcher::LiveGraphWatcher;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

/// Type of change observed in a symbol
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EditKind {
    SignatureModified,
    FieldAddedOrRemoved,
    TypeRenamed,
    ImplementationChanged,
    ContractUpdated,
}

/// Event representing a recent user or AI edit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditEvent {
    pub file_path: String,
    pub symbol_name: String,
    pub change_kind: EditKind,
    pub old_snippet: Option<String>,
    pub new_snippet: Option<String>,
    pub timestamp_epoch: u64,
}

/// Predicted next edit recommended for proactive application
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PredictedNextEdit {
    pub target_file: String,
    pub target_line: usize,
    pub affected_symbol: String,
    pub suggested_action: String,
    pub suggested_diff: String,
    pub confidence_score: u8,
    pub rationale: String,
}

/// Comprehensive report containing anticipated cascade edits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionBatchReport {
    pub trigger_symbol: String,
    pub trigger_file: String,
    pub predictions: Vec<PredictedNextEdit>,
    pub call_sites_analyzed: usize,
    pub elapsed_us: u64,
}

/// Ambient Next-Action & Multi-File Cascade Predictor
pub struct AmbientPredictor {
    pub history: Vec<EditEvent>,
    pub max_history: usize,
}

impl Default for AmbientPredictor {
    fn default() -> Self {
        Self::new(50)
    }
}

impl AmbientPredictor {
    pub fn new(max_history: usize) -> Self {
        Self {
            history: Vec::new(),
            max_history,
        }
    }

    /// Record a recent edit event to build temporal locality context
    pub fn record_edit(&mut self, event: EditEvent) {
        self.history.push(event);
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }
    }

    /// Predict next edits based on an edit event and current live AST graph
    pub fn predict_next_edits(
        &self,
        event: &EditEvent,
        watcher: &LiveGraphWatcher,
    ) -> PredictionBatchReport {
        let start = SystemTime::now();
        let mut predictions = Vec::new();
        let callers = watcher.find_callers(&event.symbol_name);
        let call_sites_count = callers.len();

        for caller_rel_path in callers {
            // Skip the file where the edit occurred
            if caller_rel_path == event.file_path {
                continue;
            }

            let full_path = watcher.workspace_root.join(&caller_rel_path);
            if let Ok(content) = fs::read_to_string(&full_path) {
                let lines: Vec<&str> = content.lines().collect();
                for (idx, line) in lines.iter().enumerate() {
                    if line.contains(&event.symbol_name) {
                        let line_no = idx + 1;
                        let (diff, action, conf) = match &event.change_kind {
                            EditKind::SignatureModified => {
                                let old_call = event.old_snippet.as_deref().unwrap_or(&event.symbol_name);
                                let new_call = event.new_snippet.as_deref().unwrap_or(&event.symbol_name);
                                let modified_line = line.replace(old_call, new_call);
                                let d = format!("@@ -{},1 +{},1 @@\n- {}\n+ {}", line_no, line_no, line.trim(), modified_line.trim());
                                (d, format!("Update call-site of '{}' to match new signature", event.symbol_name), 92)
                            }
                            EditKind::TypeRenamed => {
                                let old_type = event.old_snippet.as_deref().unwrap_or(&event.symbol_name);
                                let new_type = event.new_snippet.as_deref().unwrap_or("NewType");
                                let modified_line = line.replace(old_type, new_type);
                                let d = format!("@@ -{},1 +{},1 @@\n- {}\n+ {}", line_no, line_no, line.trim(), modified_line.trim());
                                (d, format!("Rename type reference '{}' -> '{}'", old_type, new_type), 96)
                            }
                            EditKind::FieldAddedOrRemoved => {
                                let d = format!("@@ -{},1 +{},1 @@\n// Ensure struct construction initializes updated fields for {}", line_no, line_no, event.symbol_name);
                                (d, format!("Reconcile struct fields for '{}'", event.symbol_name), 88)
                            }
                            _ => {
                                let d = format!("@@ -{},1 +{},1 @@\n// Verify dependent invocation of {}", line_no, line_no, event.symbol_name);
                                (d, format!("Verify dependent contract invocation for '{}'", event.symbol_name), 80)
                            }
                        };

                        predictions.push(PredictedNextEdit {
                            target_file: caller_rel_path.clone(),
                            target_line: line_no,
                            affected_symbol: event.symbol_name.clone(),
                            suggested_action: action,
                            suggested_diff: diff,
                            confidence_score: conf,
                            rationale: format!(
                                "Call-site in '{}' at line {} directly depends on modified symbol '{}'",
                                caller_rel_path, line_no, event.symbol_name
                            ),
                        });
                        break; // One primary prediction per caller file
                    }
                }
            }
        }

        // Sort descending by confidence score
        predictions.sort_by(|a, b| b.confidence_score.cmp(&a.confidence_score));

        let elapsed_us = start.elapsed().unwrap_or_default().as_micros() as u64;

        PredictionBatchReport {
            trigger_symbol: event.symbol_name.clone(),
            trigger_file: event.file_path.clone(),
            predictions,
            call_sites_analyzed: call_sites_count,
            elapsed_us,
        }
    }

    /// Helper to construct an edit event with current timestamp
    pub fn create_event(
        file_path: impl Into<String>,
        symbol_name: impl Into<String>,
        change_kind: EditKind,
        old_snippet: Option<String>,
        new_snippet: Option<String>,
    ) -> EditEvent {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        EditEvent {
            file_path: file_path.into(),
            symbol_name: symbol_name.into(),
            change_kind,
            old_snippet,
            new_snippet,
            timestamp_epoch: now,
        }
    }
}
