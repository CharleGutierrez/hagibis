//! # SpecDecomposer - Copilot Workspace-Style Spec -> Plan -> Diff Pipeline
//!
//! Elevates Copilot Workspace's task decomposition model. Deconstructs vague
//! high-level user intents or GitHub issues into architectural specifications,
//! ordered task plans, target file diff manifests, and verification gates.

use serde::{Deserialize, Serialize};

/// Execution status of a planned step
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

impl StepStatus {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Pending => "⏳ PENDING",
            Self::InProgress => "⚡ IN_PROGRESS",
            Self::Completed => "✅ COMPLETED",
            Self::Blocked => "⛔ BLOCKED",
        }
    }
}

/// An individual decomposed plan step
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecomposedStep {
    pub step_number: usize,
    pub title: String,
    pub target_files: Vec<String>,
    pub action_description: String,
    pub verification_command: String,
    pub status: StepStatus,
}

/// Full decomposition report with architectural spec and execution steps
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecDecompositionReport {
    pub intent: String,
    pub architecture_spec: String,
    pub affected_components: Vec<String>,
    pub steps: Vec<DecomposedStep>,
    pub total_steps: usize,
    pub completed_steps: usize,
    pub progress_percent: u8,
}

pub struct SpecDecomposer;

impl SpecDecomposer {
    /// Decompose high-level feature intent or bug description into structured plan
    pub fn decompose(intent: &str, workspace_files: &[String]) -> SpecDecompositionReport {
        let intent_trimmed = intent.trim();
        let intent_lower = intent_trimmed.to_lowercase();

        let mut affected_components = Vec::new();
        let mut steps = Vec::new();

        // 1. Identify affected components from workspace files
        for f in workspace_files {
            if intent_lower.contains("auth") && f.contains("auth") {
                affected_components.push(f.clone());
            } else if intent_lower.contains("db") || intent_lower.contains("database") {
                if f.contains("db") || f.contains("schema") || f.contains("model") {
                    affected_components.push(f.clone());
                }
            } else if intent_lower.contains("api") || intent_lower.contains("route") {
                if f.contains("api") || f.contains("server") || f.contains("route") {
                    affected_components.push(f.clone());
                }
            } else if intent_lower.contains("ui") || intent_lower.contains("frontend") {
                if f.ends_with(".tsx") || f.ends_with(".jsx") || f.contains("cockpit") {
                    affected_components.push(f.clone());
                }
            }
        }

        if affected_components.is_empty() {
            affected_components.push("src/main.rs".to_string());
        }

        // 2. Synthesize architectural spec
        let architecture_spec = format!(
            "Architecture Spec for '{}':\n\
            - Primary Objective: Safely implement desired behavior without breaking API invariants.\n\
            - Structural Scope: {} affected source units identified.\n\
            - Verification Strategy: Pre-flight shadow compilation followed by behavioral contract test assertions.",
            intent_trimmed, affected_components.len()
        );

        // 3. Decompose into ordered chronological steps
        steps.push(DecomposedStep {
            step_number: 1,
            title: "Pre-Flight Workspace & Invariant Audit".to_string(),
            target_files: affected_components.clone(),
            action_description: "Audit existing symbol definitions, public interfaces, and compile state.".to_string(),
            verification_command: "hgb preflight src/lib.rs".to_string(),
            status: StepStatus::Completed,
        });

        steps.push(DecomposedStep {
            step_number: 2,
            title: "Synthesize Core Logic & Type Interfaces".to_string(),
            target_files: affected_components.clone(),
            action_description: format!("Implement core functionality for: {}", intent_trimmed),
            verification_command: "cargo check --message-format=short".to_string(),
            status: StepStatus::InProgress,
        });

        steps.push(DecomposedStep {
            step_number: 3,
            title: "Inject Behavioral Contract Tests & Mutation Audit".to_string(),
            target_files: vec!["tests/integration_tests.rs".to_string()],
            action_description: "Write contract assertions covering nominal, edge, and error branches.".to_string(),
            verification_command: "hgb fuzz-test src/lib.rs".to_string(),
            status: StepStatus::Pending,
        });

        steps.push(DecomposedStep {
            step_number: 4,
            title: "Atomic Micro-Commit & Clean Deployment".to_string(),
            target_files: affected_components.clone(),
            action_description: "Record conventional git commit with verified syntax hash.".to_string(),
            verification_command: "hgb micro-commit --intent 'verified feature delivery'".to_string(),
            status: StepStatus::Pending,
        });

        let total_steps = steps.len();
        let completed_steps = steps.iter().filter(|s| s.status == StepStatus::Completed).count();
        let progress_percent = if total_steps > 0 {
            ((completed_steps as f64 / total_steps as f64) * 100.0) as u8
        } else {
            0
        };

        SpecDecompositionReport {
            intent: intent_trimmed.to_string(),
            architecture_spec,
            affected_components,
            steps,
            total_steps,
            completed_steps,
            progress_percent,
        }
    }

    /// Advance a step status
    pub fn advance_step(report: &mut SpecDecompositionReport, step_num: usize) {
        for s in &mut report.steps {
            if s.step_number == step_num {
                s.status = StepStatus::Completed;
            } else if s.step_number == step_num + 1 && s.status == StepStatus::Pending {
                s.status = StepStatus::InProgress;
            }
        }
        report.completed_steps = report.steps.iter().filter(|s| s.status == StepStatus::Completed).count();
        if report.total_steps > 0 {
            report.progress_percent = ((report.completed_steps as f64 / report.total_steps as f64) * 100.0) as u8;
        }
    }

    /// Format as an interactive ASCII checklist for terminal and Cockpit
    pub fn format_ascii(report: &SpecDecompositionReport) -> String {
        let mut out = String::new();
        out.push_str("=== COPILOT WORKSPACE-STYLE SPEC DECOMPOSITION ===\n");
        out.push_str(&format!("Intent: {}\n", report.intent));
        out.push_str(&format!("Progress: {}% ({}/{} steps)\n\n", report.progress_percent, report.completed_steps, report.total_steps));
        out.push_str(&format!("📋 {}\n\n", report.architecture_spec));

        out.push_str("Execution Steps:\n");
        for s in &report.steps {
            out.push_str(&format!(
                "  [{}] Step #{}: {}\n        Target: {:?}\n        Verify: {}\n",
                s.status.badge(),
                s.step_number,
                s.title,
                s.target_files,
                s.verification_command
            ));
        }

        out
    }
}
