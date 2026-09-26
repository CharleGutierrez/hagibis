use crate::error::{HgbError, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleFeedbackRecord {
    pub id: i64,
    pub snippet: String,
    pub accepted: bool,
    pub timestamp: i64,
}

/// Persistent Style Memory & Reject-Learner Vault
///
/// Backed by SQLite table `style_feedback (id INTEGER PRIMARY KEY, snippet TEXT, accepted BOOLEAN, timestamp INTEGER)`.
/// Retains accepted conventions and user-rejected anti-patterns to steer future generation.
pub struct StyleMemoryVault {
    db_path: Option<PathBuf>,
    in_memory: Option<Mutex<Connection>>,
}

impl StyleMemoryVault {
    /// Create or open a persistent SQLite StyleMemoryVault at `path`
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(HgbError::Io)?;
        }
        let conn = Connection::open(&db_path).map_err(|e| HgbError::Execution(e.to_string()))?;
        Self::init_tables(&conn)?;
        Ok(Self {
            db_path: Some(db_path),
            in_memory: None,
        })
    }

    /// Create an ephemeral in-memory StyleMemoryVault (ideal for tests and isolated sessions)
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| HgbError::Execution(e.to_string()))?;
        Self::init_tables(&conn)?;
        Ok(Self {
            db_path: None,
            in_memory: Some(Mutex::new(conn)),
        })
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS style_feedback (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                snippet TEXT NOT NULL,
                accepted BOOLEAN NOT NULL,
                timestamp INTEGER NOT NULL
            )",
            [],
        )
        .map_err(|e| HgbError::Execution(e.to_string()))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_style_feedback_accepted ON style_feedback(accepted)",
            [],
        )
        .map_err(|e| HgbError::Execution(e.to_string()))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_style_feedback_timestamp ON style_feedback(timestamp)",
            [],
        )
        .map_err(|e| HgbError::Execution(e.to_string()))?;

        Ok(())
    }

    fn with_conn<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R>,
    {
        if let Some(ref mem_lock) = self.in_memory {
            let conn = mem_lock
                .lock()
                .map_err(|e| HgbError::Execution(format!("Lock poisoned: {}", e)))?;
            f(&conn)
        } else if let Some(ref path) = self.db_path {
            let conn = Connection::open(path).map_err(|e| HgbError::Execution(e.to_string()))?;
            f(&conn)
        } else {
            Err(HgbError::Execution("Invalid StyleMemoryVault state".to_string()))
        }
    }

    /// Record a code or architectural snippet as accepted (true) or rejected anti-pattern (false)
    pub fn record_feedback(&self, snippet: &str, accepted: bool) -> Result<()> {
        let trimmed = snippet.trim();
        if trimmed.is_empty() {
            return Err(HgbError::Execution("Cannot record empty style snippet".to_string()));
        }

        let now = chrono::Utc::now().timestamp();
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO style_feedback (snippet, accepted, timestamp) VALUES (?1, ?2, ?3)",
                params![trimmed, accepted, now],
            )
            .map_err(|e| HgbError::Execution(e.to_string()))?;
            Ok(())
        })
    }

    /// Synthesize learned style guidelines from accepted patterns and rejected anti-patterns
    pub fn get_style_guidelines(&self) -> Result<String> {
        self.with_conn(|conn| {
            let mut accepted_stmt = conn
                .prepare("SELECT snippet FROM style_feedback WHERE accepted = 1 ORDER BY id DESC LIMIT 50")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let accepted_rows = accepted_stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| HgbError::Execution(e.to_string()))?;

            let mut accepted_snippets = Vec::new();
            for r in accepted_rows.flatten() {
                accepted_snippets.push(r);
            }

            let mut rejected_stmt = conn
                .prepare("SELECT snippet FROM style_feedback WHERE accepted = 0 ORDER BY id DESC LIMIT 50")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let rejected_rows = rejected_stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| HgbError::Execution(e.to_string()))?;

            let mut rejected_snippets = Vec::new();
            for r in rejected_rows.flatten() {
                rejected_snippets.push(r);
            }

            if accepted_snippets.is_empty() && rejected_snippets.is_empty() {
                return Ok(
                    "# Hagibis Learned Style Guidelines\n\nNo specific style preferences recorded yet. Follow standard idiomatic Rust / AGY conventions.".to_string(),
                );
            }

            let mut doc = String::from("# Hagibis Learned Style Guidelines (Reject-Learner Vault)\n\n");

            if !accepted_snippets.is_empty() {
                doc.push_str("## Preferred Style Patterns (Accepted by User):\n");
                for s in &accepted_snippets {
                    doc.push_str(&format!("- {}\n", s.replace('\n', " ")));
                }
                doc.push('\n');
            }

            if !rejected_snippets.is_empty() {
                doc.push_str("## Anti-Patterns to Avoid (Rejected by User):\n");
                for s in &rejected_snippets {
                    doc.push_str(&format!("- ⛔ {}\n", s.replace('\n', " ")));
                }
                doc.push('\n');
            }

            Ok(doc)
        })
    }

    /// Render XML prompt guidance for LLM injection
    pub fn render_prompt_guidance(&self, limit: usize) -> String {
        self.with_conn(|conn| {
            let mut accepted_stmt = conn
                .prepare("SELECT snippet FROM style_feedback WHERE accepted = 1 ORDER BY id DESC LIMIT ?1")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let accepted_rows = accepted_stmt
                .query_map(params![limit as i64], |row| row.get::<_, String>(0))
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let mut accepted_snippets = Vec::new();
            for r in accepted_rows.flatten() {
                accepted_snippets.push(r);
            }

            let mut rejected_stmt = conn
                .prepare("SELECT snippet FROM style_feedback WHERE accepted = 0 ORDER BY id DESC LIMIT ?1")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let rejected_rows = rejected_stmt
                .query_map(params![limit as i64], |row| row.get::<_, String>(0))
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let mut rejected_snippets = Vec::new();
            for r in rejected_rows.flatten() {
                rejected_snippets.push(r);
            }

            if accepted_snippets.is_empty() && rejected_snippets.is_empty() {
                return Ok(String::new());
            }

            let mut out = String::from("<style_guidance>\n");
            if !accepted_snippets.is_empty() {
                out.push_str("  <preferred_patterns>\n");
                for s in &accepted_snippets {
                    out.push_str(&format!("    - {}\n", s.replace('\n', " ")));
                }
                out.push_str("  </preferred_patterns>\n");
            }
            if !rejected_snippets.is_empty() {
                out.push_str("  <rejected_anti_patterns>\n");
                for s in &rejected_snippets {
                    out.push_str(&format!("    - ⛔ {}\n", s.replace('\n', " ")));
                }
                out.push_str("  </rejected_anti_patterns>\n");
            }
            out.push_str("</style_guidance>\n");
            Ok(out)
        })
        .unwrap_or_default()
    }

    /// Search recorded feedback matching a query string
    pub fn search_feedback(&self, query: &str, limit: usize) -> Result<Vec<StyleFeedbackRecord>> {
        let pattern = format!("%{}%", query);
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT id, snippet, accepted, timestamp FROM style_feedback WHERE snippet LIKE ?1 ORDER BY id DESC LIMIT ?2")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let rows = stmt
                .query_map(params![pattern, limit as i64], |row| {
                    Ok(StyleFeedbackRecord {
                        id: row.get(0)?,
                        snippet: row.get(1)?,
                        accepted: row.get(2)?,
                        timestamp: row.get(3)?,
                    })
                })
                .map_err(|e| HgbError::Execution(e.to_string()))?;

            let mut records = Vec::new();
            for r in rows.flatten() {
                records.push(r);
            }
            Ok(records)
        })
    }

    /// Retrieve counts of (accepted, rejected) records
    pub fn get_feedback_count(&self) -> Result<(usize, usize)> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT accepted, COUNT(*) FROM style_feedback GROUP BY accepted")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    let accepted: bool = row.get(0)?;
                    let count: i64 = row.get(1)?;
                    Ok((accepted, count as usize))
                })
                .map_err(|e| HgbError::Execution(e.to_string()))?;

            let mut accepted_count = 0;
            let mut rejected_count = 0;
            for r in rows.flatten() {
                if r.0 {
                    accepted_count = r.1;
                } else {
                    rejected_count = r.1;
                }
            }
            Ok((accepted_count, rejected_count))
        })
    }

    /// Retrieve all recorded feedback entries
    pub fn get_all_feedback(&self) -> Result<Vec<StyleFeedbackRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT id, snippet, accepted, timestamp FROM style_feedback ORDER BY id ASC")
                .map_err(|e| HgbError::Execution(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(StyleFeedbackRecord {
                        id: row.get(0)?,
                        snippet: row.get(1)?,
                        accepted: row.get(2)?,
                        timestamp: row.get(3)?,
                    })
                })
                .map_err(|e| HgbError::Execution(e.to_string()))?;

            let mut entries = Vec::new();
            for r in rows.flatten() {
                entries.push(r);
            }
            Ok(entries)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_style_vault_record_and_guidelines() {
        let vault = StyleMemoryVault::in_memory().expect("in memory vault");

        // Initial guidelines
        let initial = vault.get_style_guidelines().expect("guidelines");
        assert!(initial.contains("No specific style preferences"));

        // Record accepted pattern
        vault
            .record_feedback("use tokio::sync::RwLock instead of std::sync::RwLock", true)
            .expect("record accepted");

        // Record rejected anti-pattern
        vault
            .record_feedback("unwrap() inside daemon request handlers", false)
            .expect("record rejected");

        let (acc, rej) = vault.get_feedback_count().expect("counts");
        assert_eq!(acc, 1);
        assert_eq!(rej, 1);

        let guidance = vault.get_style_guidelines().expect("guidelines");
        assert!(guidance.contains("Preferred Style Patterns"));
        assert!(guidance.contains("tokio::sync::RwLock"));
        assert!(guidance.contains("Anti-Patterns to Avoid"));
        assert!(guidance.contains("unwrap()"));

        // XML prompt guidance
        let xml = vault.render_prompt_guidance(5);
        assert!(xml.contains("<style_guidance>"));
        assert!(xml.contains("<preferred_patterns>"));
        assert!(xml.contains("tokio::sync::RwLock"));
        assert!(xml.contains("<rejected_anti_patterns>"));
        assert!(xml.contains("unwrap()"));

        // Search feedback
        let results = vault.search_feedback("RwLock", 5).expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].accepted, true);
    }
}
