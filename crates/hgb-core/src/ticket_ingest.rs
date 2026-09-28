//! Superpower 105: Universal Issue Ingestor (Devin & Copilot Parity)
//!
//! Universal parser for GitHub issues, Linear tickets (ENG-123), Jira keys (PROJ-456),
//! or raw Markdown. Automatically extracts acceptance criteria, stack traces,
//! referenced files, and suggests git branch names.

use serde::{Deserialize, Serialize};
use regex::Regex;
use std::sync::LazyLock;
use crate::error::HgbError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TicketProvider {
    GitHub,
    Linear,
    Jira,
    MarkdownFile,
    RawText,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptanceCriterion {
    pub description: String,
    pub is_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTicketContext {
    pub provider: TicketProvider,
    pub issue_key: String,
    pub title: String,
    pub raw_body: String,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub referenced_files: Vec<String>,
    pub stack_traces: Vec<String>,
    pub suggested_branch_name: String,
}

static GITHUB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"github\.com/[^/]+/[^/]+/issues/(\d+)").expect("Valid regex")
});

static LINEAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([A-Z]{2,6}-\d+)\b").expect("Valid regex")
});

static FILE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:`|'|\s)([a-zA-Z0-9_\-\./]+\.(?:rs|ts|tsx|js|jsx|py|rb|go|json|toml|yaml|yml|md))(?:`|'|\s)").expect("Valid regex")
});

pub struct TicketIngestEngine;

impl TicketIngestEngine {
    /// Ingests a ticket URL, issue key, or raw ticket body
    pub fn ingest(input: &str) -> Result<ParsedTicketContext, HgbError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(HgbError::InvalidInput("Ticket input cannot be empty".to_string()));
        }

        let mut provider = TicketProvider::RawText;
        let mut issue_key = "ISSUE-LOCAL".to_string();

        // 1. Provider & Key Detection
        if let Some(caps) = GITHUB_RE.captures(trimmed) {
            provider = TicketProvider::GitHub;
            issue_key = format!("GH-{}", caps.get(1).map(|m| m.as_str()).unwrap_or("0"));
        } else if let Some(caps) = LINEAR_RE.captures(trimmed) {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("ENG-1");
            if key.starts_with("PROJ") || key.starts_with("JIRA") {
                provider = TicketProvider::Jira;
            } else {
                provider = TicketProvider::Linear;
            }
            issue_key = key.to_uppercase();
        } else if trimmed.ends_with(".md") || trimmed.ends_with(".markdown") {
            provider = TicketProvider::MarkdownFile;
            issue_key = trimmed.split('/').last().unwrap_or("ticket.md").to_string();
        }

        // 2. Title & Body extraction
        let mut lines = trimmed.lines();
        let first_line = lines.next().unwrap_or("Untitled Ticket").trim();
        let title = if first_line.starts_with('#') {
            first_line.trim_start_matches('#').trim().to_string()
        } else {
            first_line.to_string()
        };

        let raw_body = trimmed.to_string();

        // 3. Acceptance Criteria Extraction (- [ ] or - [x])
        let mut acceptance_criteria = Vec::new();
        for line in trimmed.lines() {
            let l = line.trim();
            if l.starts_with("- [ ]") || l.starts_with("* [ ]") {
                acceptance_criteria.push(AcceptanceCriterion {
                    description: l[5..].trim().to_string(),
                    is_verified: false,
                });
            } else if l.starts_with("- [x]") || l.starts_with("- [X]") || l.starts_with("* [x]") {
                acceptance_criteria.push(AcceptanceCriterion {
                    description: l[5..].trim().to_string(),
                    is_verified: true,
                });
            }
        }

        if acceptance_criteria.is_empty() {
            acceptance_criteria.push(AcceptanceCriterion {
                description: format!("Fulfill requirements for {}", issue_key),
                is_verified: false,
            });
            acceptance_criteria.push(AcceptanceCriterion {
                description: "Pass zero-regression test suite".to_string(),
                is_verified: false,
            });
        }

        // 4. Referenced Files Extraction
        let mut referenced_files = Vec::new();
        for cap in FILE_PATH_RE.captures_iter(trimmed) {
            if let Some(m) = cap.get(1) {
                let file = m.as_str().to_string();
                if !referenced_files.contains(&file) {
                    referenced_files.push(file);
                }
            }
        }

        // 5. Stack Trace Extraction
        let mut stack_traces = Vec::new();
        let mut current_trace = Vec::new();
        let mut in_trace = false;

        for line in trimmed.lines() {
            let l = line.trim();
            if l.contains("panicked at") || l.contains("Traceback (most recent call last)") || l.contains("Error:") || l.starts_with("at ") {
                in_trace = true;
                current_trace.push(line.to_string());
            } else if in_trace {
                if l.starts_with("at ") || l.starts_with("File ") || l.contains("-->") || l.starts_with("...") {
                    current_trace.push(line.to_string());
                } else if !l.is_empty() {
                    current_trace.push(line.to_string());
                    stack_traces.push(current_trace.join("\n"));
                    current_trace.clear();
                    in_trace = false;
                }
            }
        }
        if !current_trace.is_empty() {
            stack_traces.push(current_trace.join("\n"));
        }

        // 6. Branch Name Synthesis
        let slug: String = title
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
            .collect::<String>()
            .to_lowercase()
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join("-");

        let branch_prefix = if stack_traces.is_empty() && !title.to_lowercase().contains("fix") && !title.to_lowercase().contains("bug") {
            "feat"
        } else {
            "fix"
        };

        let suggested_branch_name = format!("{}/{}-{}", branch_prefix, issue_key.to_lowercase(), if slug.is_empty() { "patch" } else { &slug });

        Ok(ParsedTicketContext {
            provider,
            issue_key,
            title,
            raw_body,
            acceptance_criteria,
            referenced_files,
            stack_traces,
            suggested_branch_name,
        })
    }
}
