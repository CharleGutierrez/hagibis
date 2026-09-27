//! # PromptModeDocsHarvester - Composable Vibe Modes & Live Docs Slicer
//!
//! Elevates Continue.dev & Roo Code specialized prompt modes and dynamic documentation harvesting.
//! Tailors system prompt instructions, token budgets, and output constraints to specific development
//! phases (Architect, CodeSprint, DebugTriage, SecurityAudit), and extracts high-signal API signatures
//! and examples from official documentation without chrome or boilerplate.

use serde::{Deserialize, Serialize};

/// Specialized operating modes for vibe coding
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VibePromptMode {
    Architect,
    CodeSprint,
    DebugTriage,
    SecurityAudit,
    DocReview,
}

impl VibePromptMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Architect => "ARCHITECT",
            Self::CodeSprint => "CODE_SPRINT",
            Self::DebugTriage => "DEBUG_TRIAGE",
            Self::SecurityAudit => "SECURITY_AUDIT",
            Self::DocReview => "DOC_REVIEW",
        }
    }

    pub fn system_directive(&self) -> &'static str {
        match self {
            Self::Architect => {
                "Role: Principal System Architect. Focus exclusively on structural boundaries, type invariants, non-breaking migrations, and API contracts. Produce formal specs with zero hand-waving."
            }
            Self::CodeSprint => {
                "Role: Elite Vibe Coding Engineer. Focus on high-velocity, high-confidence implementation. Minimize conversational filler. Produce clean, self-contained, surgical code patches and unit tests."
            }
            Self::DebugTriage => {
                "Role: Lead Debugging Forensic Specialist. Analyze root causes from error logs, pinpoint exact failing invariants, and synthesize minimal regression reproducers before patching."
            }
            Self::SecurityAudit => {
                "Role: Offensive Security & AppSec Auditor. Audit for secret leaks, injection flaws, memory safety bounds, O(N^2) DoS hazards, and unsafe deserialization."
            }
            Self::DocReview => {
                "Role: Technical Documentation Architect. Produce crystal-clear docstrings, API reference manuals, architectural summaries, and usage examples adhering to Effective guidelines."
            }
        }
    }
}

/// Sliced and condensed documentation content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocsHarvestResult {
    pub source_target: String,
    pub extracted_title: String,
    pub signatures: Vec<String>,
    pub code_examples: Vec<String>,
    pub condensed_markdown: String,
    pub raw_tokens_estimate: usize,
    pub condensed_tokens_estimate: usize,
    pub compression_ratio: f32,
}

/// Full prompt compilation report combining active mode directive and sliced documentation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeHarvesterReport {
    pub active_mode: VibePromptMode,
    pub system_prompt_directive: String,
    pub harvested_docs: Vec<DocsHarvestResult>,
    pub augmented_prompt: String,
}

pub struct PromptModeDocsHarvester;

impl PromptModeDocsHarvester {
    /// Slice raw documentation text into high-density signatures and examples
    pub fn harvest_content(source_target: &str, raw_content: &str) -> DocsHarvestResult {
        let mut signatures = Vec::new();
        let mut code_examples = Vec::new();
        let mut in_code_block = false;
        let mut current_block = Vec::new();

        let lines = raw_content.lines();
        let first_header = lines
            .clone()
            .find(|l| l.starts_with("# ") || l.starts_with("## "))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .unwrap_or_else(|| source_target.to_string());

        for line in lines {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                if in_code_block {
                    in_code_block = false;
                    let block_str = current_block.join("\n");
                    if !block_str.trim().is_empty() {
                        code_examples.push(block_str);
                    }
                    current_block.clear();
                } else {
                    in_code_block = true;
                }
            } else if in_code_block {
                current_block.push(line);
            } else if trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("pub trait ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("export interface ")
            {
                signatures.push(trimmed.to_string());
            }
        }

        let mut condensed = format!("### Docs: {}\n", first_header);
        if !signatures.is_empty() {
            condensed.push_str("#### Key Signatures:\n");
            for sig in &signatures {
                condensed.push_str(&format!("- `{}`\n", sig));
            }
        }

        if !code_examples.is_empty() {
            condensed.push_str("#### Verified Examples:\n```\n");
            for ex in code_examples.iter().take(2) {
                condensed.push_str(ex);
                condensed.push('\n');
            }
            condensed.push_str("```\n");
        }

        let raw_tokens = (raw_content.len() / 4).max(1);
        let condensed_tokens = (condensed.len() / 4).max(1);
        let ratio = (raw_tokens as f32) / (condensed_tokens as f32).max(1.0);

        DocsHarvestResult {
            source_target: source_target.to_string(),
            extracted_title: first_header,
            signatures,
            code_examples,
            condensed_markdown: condensed,
            raw_tokens_estimate: raw_tokens,
            condensed_tokens_estimate: condensed_tokens,
            compression_ratio: ratio,
        }
    }

    /// Assemble full augmented prompt with active mode instructions and harvested context
    pub fn assemble_prompt(
        mode: VibePromptMode,
        user_prompt: &str,
        docs: Vec<DocsHarvestResult>,
    ) -> ModeHarvesterReport {
        let directive = mode.system_directive().to_string();
        let mut augmented = format!("[SYSTEM MODE: {}]\n{}\n\n", mode.label(), directive);

        if !docs.is_empty() {
            augmented.push_str("<!-- HARVESTED HIGH-SIGNAL API DOCUMENTATION -->\n");
            for doc in &docs {
                augmented.push_str(&doc.condensed_markdown);
                augmented.push('\n');
            }
            augmented.push_str("<!-- END HARVESTED DOCUMENTATION -->\n\n");
        }

        augmented.push_str("USER REQUEST:\n");
        augmented.push_str(user_prompt);

        ModeHarvesterReport {
            active_mode: mode,
            system_prompt_directive: directive,
            harvested_docs: docs,
            augmented_prompt: augmented,
        }
    }
}
