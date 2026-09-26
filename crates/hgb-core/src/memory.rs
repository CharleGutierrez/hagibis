//! # Living Project Memory & Decision Ledger
//!
//! Maintains persistent, versioned architectural context, Architectural Decision Records (ADRs),
//! tech debt entries, and project milestones in `.hgb/memory.json` for prompt injection.

use blake3::Hasher;
use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Lifecycle status of an Architectural Decision Record
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdrStatus {
    Proposed,
    Accepted,
    Superseded,
    Rejected,
}

impl std::fmt::Display for AdrStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Proposed => write!(f, "Proposed"),
            Self::Accepted => write!(f, "Accepted"),
            Self::Superseded => write!(f, "Superseded"),
            Self::Rejected => write!(f, "Rejected"),
        }
    }
}

/// Severity classification of a tracked technical debt item
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebtSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for DebtSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "Low"),
            Self::Medium => write!(f, "Medium"),
            Self::High => write!(f, "High"),
            Self::Critical => write!(f, "Critical"),
        }
    }
}

/// An immutable Architectural Decision Record (ADR)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitecturalDecision {
    pub id: String,
    pub title: String,
    pub status: AdrStatus,
    pub context: String,
    pub decision: String,
    pub consequences: String,
    pub timestamp_utc: String,
    pub blake3_hash: String,
}

/// A tracked technical debt item
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TechDebtEntry {
    pub id: String,
    pub title: String,
    pub description: String,
    pub affected_files: Vec<String>,
    pub severity: DebtSeverity,
    pub workaround: Option<String>,
    pub resolved: bool,
}

/// A roadmap milestone tracking overall project vision
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRoadmapMilestone {
    pub id: String,
    pub title: String,
    pub completed: bool,
    pub target_version: Option<String>,
}

/// JSON document schema persisted in `.hgb/memory.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDocument {
    pub version: u32,
    pub project_name: String,
    pub decisions: Vec<ArchitecturalDecision>,
    pub tech_debt: Vec<TechDebtEntry>,
    pub milestones: Vec<ProjectRoadmapMilestone>,
}

impl Default for MemoryDocument {
    fn default() -> Self {
        Self {
            version: 1,
            project_name: "Hagibis Project".to_string(),
            decisions: Vec::new(),
            tech_debt: Vec::new(),
            milestones: Vec::new(),
        }
    }
}

/// The Project Memory Ledger manager
pub struct ProjectMemoryLedger {
    pub file_path: PathBuf,
    pub doc: MemoryDocument,
}

impl ProjectMemoryLedger {
    /// Load existing `.hgb/memory.json` or initialize default
    pub fn load_or_init<P: AsRef<Path>>(workspace_root: P) -> Result<Self> {
        let hgb_dir = workspace_root.as_ref().join(".hgb");
        fs::create_dir_all(&hgb_dir).map_err(HgbError::Io)?;

        let file_path = hgb_dir.join("memory.json");
        let doc = if file_path.exists() {
            let content = fs::read_to_string(&file_path).map_err(HgbError::Io)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            let mut default_doc = MemoryDocument::default();
            // Seed project name from workspace folder name
            if let Some(folder_name) = workspace_root.as_ref().file_name().and_then(|s| s.to_str()) {
                default_doc.project_name = folder_name.to_string();
            }
            let initial_json = serde_json::to_string_pretty(&default_doc)
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            fs::write(&file_path, initial_json).map_err(HgbError::Io)?;
            default_doc
        };

        Ok(Self { file_path, doc })
    }

    /// Record a new Architectural Decision Record (ADR)
    pub fn record_decision(
        &mut self,
        title: &str,
        decision: &str,
        context: &str,
    ) -> Result<String> {
        let count = self.doc.decisions.len() + 1;
        let id = format!("ADR-{:03}", count);
        let timestamp_utc = chrono::Utc::now().to_rfc3339();

        let mut hasher = Hasher::new();
        hasher.update(title.as_bytes());
        hasher.update(decision.as_bytes());
        hasher.update(context.as_bytes());
        let blake3_hash = hasher.finalize().to_hex().to_string();

        let adr = ArchitecturalDecision {
            id: id.clone(),
            title: title.to_string(),
            status: AdrStatus::Accepted,
            context: context.to_string(),
            decision: decision.to_string(),
            consequences: "Documented in Project Memory Ledger.".to_string(),
            timestamp_utc,
            blake3_hash,
        };

        self.doc.decisions.push(adr);
        self.save()?;
        Ok(id)
    }

    /// Record a new technical debt item
    pub fn record_tech_debt(
        &mut self,
        title: &str,
        description: &str,
        severity: DebtSeverity,
        files: Vec<String>,
    ) -> Result<String> {
        let count = self.doc.tech_debt.len() + 1;
        let id = format!("DEBT-{:03}", count);

        let entry = TechDebtEntry {
            id: id.clone(),
            title: title.to_string(),
            description: description.to_string(),
            affected_files: files,
            severity,
            workaround: None,
            resolved: false,
        };

        self.doc.tech_debt.push(entry);
        self.save()?;
        Ok(id)
    }

    /// Search decisions matching a query string (case-insensitive)
    pub fn search_decisions(&self, query: &str) -> Vec<&ArchitecturalDecision> {
        let q = query.to_lowercase();
        self.doc
            .decisions
            .iter()
            .filter(|d| {
                d.id.to_lowercase().contains(&q)
                    || d.title.to_lowercase().contains(&q)
                    || d.decision.to_lowercase().contains(&q)
                    || d.context.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Search technical debt entries matching a query string (case-insensitive)
    pub fn search_debts(&self, query: &str) -> Vec<&TechDebtEntry> {
        let q = query.to_lowercase();
        self.doc
            .tech_debt
            .iter()
            .filter(|d| {
                d.id.to_lowercase().contains(&q)
                    || d.title.to_lowercase().contains(&q)
                    || d.description.to_lowercase().contains(&q)
                    || d.affected_files.iter().any(|f| f.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Render a concise XML context block for LLM prompt injection
    pub fn render_llm_anchor(&self, max_tokens: usize) -> String {
        if self.doc.decisions.is_empty() && self.doc.tech_debt.is_empty() && self.doc.milestones.is_empty() {
            return String::new();
        }

        let mut hasher = Hasher::new();
        for d in &self.doc.decisions {
            hasher.update(d.blake3_hash.as_bytes());
        }
        let digest = hasher.finalize().to_hex().to_string();
        let short_digest = if digest.len() > 8 { &digest[..8] } else { &digest };

        let mut out = format!("<project_memory aggregate_hash=\"{}\" project=\"{}\">\n", short_digest, self.doc.project_name);

        if !self.doc.decisions.is_empty() {
            out.push_str("  <architectural_decisions>\n");
            for d in &self.doc.decisions {
                out.push_str(&format!(
                    "    - [{}] ({}): {} => {}\n",
                    d.id, d.status, d.title, d.decision
                ));
            }
            out.push_str("  </architectural_decisions>\n");
        }

        let active_debts: Vec<&TechDebtEntry> = self.doc.tech_debt.iter().filter(|d| !d.resolved).collect();
        if !active_debts.is_empty() {
            out.push_str("  <active_technical_debt>\n");
            for debt in active_debts {
                let files_str = if debt.affected_files.is_empty() {
                    String::new()
                } else {
                    format!(" in [{}]", debt.affected_files.join(", "))
                };
                out.push_str(&format!(
                    "    - [{}] (Severity: {}): {}{}\n",
                    debt.id, debt.severity, debt.title, files_str
                ));
            }
            out.push_str("  </active_technical_debt>\n");
        }

        if !self.doc.milestones.is_empty() {
            out.push_str("  <project_milestones>\n");
            for m in &self.doc.milestones {
                let status = if m.completed { "Completed" } else { "Pending" };
                let ver = if let Some(ref v) = m.target_version {
                    format!(" (Target: {})", v)
                } else {
                    String::new()
                };
                out.push_str(&format!("    - [{}] ({}): {}{}\n", m.id, status, m.title, ver));
            }
            out.push_str("  </project_milestones>\n");
        }

        out.push_str("</project_memory>\n");

        // Clamping to token budget approx (4 chars/token)
        let max_chars = max_tokens * 4;
        if out.len() > max_chars && max_chars > 50 {
            format!("{}...\n</project_memory>\n", &out[..max_chars.saturating_sub(25)])
        } else {
            out
        }
    }

    /// Atomically persist current document to disk
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.doc)
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        let tmp_file = self.file_path.with_extension("tmp");
        let mut file = File::create(&tmp_file).map_err(HgbError::Io)?;
        file.write_all(json.as_bytes()).map_err(HgbError::Io)?;
        file.flush().map_err(HgbError::Io)?;

        fs::rename(tmp_file, &self.file_path).map_err(HgbError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_ledger_recording_and_anchor() {
        let tmp = std::env::temp_dir().join(format!("hgb_memory_core_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&tmp).unwrap();

        let mut ledger = ProjectMemoryLedger::load_or_init(&tmp).unwrap();
        assert!(ledger.file_path.exists());

        // 1. Record decision
        let adr_id = ledger.record_decision(
            "Adopt Systems-Grade Rust Microkernel",
            "Replace legacy script wrappers with native Rust async daemon",
            "Latency was unacceptable on UDS roundtrips"
        ).unwrap();
        assert_eq!(adr_id, "ADR-001");

        // 2. Record tech debt
        let debt_id = ledger.record_tech_debt(
            "Legacy socket timeout fallback",
            "Timeout defaults to 5s if peer does not reply",
            DebtSeverity::Medium,
            vec!["daemon/src/server.rs".to_string()]
        ).unwrap();
        assert_eq!(debt_id, "DEBT-001");

        // 3. Verify XML prompt anchor
        let anchor = ledger.render_llm_anchor(500);
        assert!(anchor.contains("<project_memory"));
        assert!(anchor.contains("</project_memory>"));
        assert!(anchor.contains("ADR-001"));
        assert!(anchor.contains("Adopt Systems-Grade Rust Microkernel"));
        assert!(anchor.contains("DEBT-001"));
        assert!(anchor.contains("Legacy socket timeout fallback"));

        // 4. Verify search
        let decs = ledger.search_decisions("microkernel");
        assert_eq!(decs.len(), 1);
        let debts = ledger.search_debts("socket");
        assert_eq!(debts.len(), 1);

        // 5. Verify persistence on reload
        let reloaded = ProjectMemoryLedger::load_or_init(&tmp).unwrap();
        assert_eq!(reloaded.doc.decisions.len(), 1);
        assert_eq!(reloaded.doc.tech_debt.len(), 1);

        let _ = fs::remove_dir_all(&tmp);
    }
}
