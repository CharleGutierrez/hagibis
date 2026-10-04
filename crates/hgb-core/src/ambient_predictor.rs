//! # AmbientPredictor - Next-Action & Multi-File Cascade Anticipator
//!
//! Elevates Windsurf Cascade and Supermaven proactive intelligence.
//! Tracks recent edit history and leverages the AST dependency graph to
//! anticipate the next logical edits across related files before the user opens them.

use crate::live_graph_watcher::LiveGraphWatcher;
use serde::{Deserialize, Serialize};
use crate::providers::OllamaProvider;
use crate::traits::HgbProvider;
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
    pub async fn predict_next_edits(
        &self,
        event: &EditEvent,
        watcher: &LiveGraphWatcher,
    ) -> PredictionBatchReport {
        let start = SystemTime::now();
        let mut predictions = Vec::new();
        let callers = watcher.find_callers(&event.symbol_name);
        let call_sites_count = callers.len();
        
        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));

        for caller_rel_path in callers {
            // Skip the file where the edit occurred
            if caller_rel_path == event.file_path {
                continue;
            }

            let full_path = watcher.workspace_root.join(&caller_rel_path);
            if let Ok(content) = fs::read_to_string(&full_path) {
                let lines: Vec<&str> = content.lines().collect();
                let mut target_line = 0;
                let mut context = String::new();
                for (idx, line) in lines.iter().enumerate() {
                    if line.contains(&event.symbol_name) {
                        target_line = idx + 1;
                        let start_idx = idx.saturating_sub(2);
                        let end_idx = (idx + 3).min(lines.len());
                        context = lines[start_idx..end_idx].join("\n");
                        break;
                    }
                }
                
                if target_line > 0 {
                    let prompt = format!(
                        "Symbol '{}' in '{}' was modified (Kind: {:?}). Old: {:?}, New: {:?}.
Here is a call site in '{}' at line {}:
{}
Predict the necessary next edit to fix this call site. Respond strictly with JSON:
{{
  \"suggested_action\": \"string\",
  \"suggested_diff\": \"string (unified diff format)\",
  \"confidence_score\": 85,
  \"rationale\": \"string\"
}}",
                        event.symbol_name, event.file_path, event.change_kind, event.old_snippet, event.new_snippet,
                        caller_rel_path, target_line, context
                    );
                    
                    let resp = provider.complete(&prompt, None).await.unwrap_or_else(|_| "{}".to_string());
                    let start_json = resp.find('{').unwrap_or(0);
                    let end_json = resp.rfind('}').unwrap_or(resp.len() - 1) + 1;
                    let json_str = &resp[start_json..end_json];
                    
                    #[derive(serde::Deserialize)]
                    struct Pred {
                        suggested_action: String,
                        suggested_diff: String,
                        confidence_score: u8,
                        rationale: String,
                    }
                    
                    if let Ok(p) = serde_json::from_str::<Pred>(json_str) {
                        predictions.push(PredictedNextEdit {
                            target_file: caller_rel_path.clone(),
                            target_line,
                            affected_symbol: event.symbol_name.clone(),
                            suggested_action: p.suggested_action,
                            suggested_diff: p.suggested_diff,
                            confidence_score: p.confidence_score,
                            rationale: p.rationale,
                        });
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
