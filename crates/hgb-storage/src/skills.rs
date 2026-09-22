use hgb_core::{HgbError, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub tier: String,
    pub triggers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRecord {
    pub name: String,
    pub description: String,
    pub content: String,
    pub tier: String,
    pub triggers: Vec<String>,
}

/// Lightweight SQLite-backed Skill Store (replaces monolithic in-memory string compilation)
pub struct SkillStore {
    db_path: PathBuf,
}

impl SkillStore {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| HgbError::Io(e))?;
        }
        let conn = Connection::open(&db_path).map_err(|e| HgbError::Execution(e.to_string()))?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS skills (
                name TEXT PRIMARY KEY,
                description TEXT NOT NULL,
                content TEXT NOT NULL,
                tier TEXT NOT NULL,
                triggers TEXT NOT NULL
            )",
            [],
        ).map_err(|e| HgbError::Execution(e.to_string()))?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_skills_name ON skills(name)",
            [],
        ).map_err(|e| HgbError::Execution(e.to_string()))?;

        Ok(Self { db_path })
    }

    pub fn insert_skill(&self, skill: &SkillRecord) -> Result<()> {
        let conn = Connection::open(&self.db_path).map_err(|e| HgbError::Execution(e.to_string()))?;
        let triggers_json = serde_json::to_string(&skill.triggers).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "INSERT OR REPLACE INTO skills (name, description, content, tier, triggers) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![skill.name, skill.description, skill.content, skill.tier, triggers_json],
        ).map_err(|e| HgbError::Execution(e.to_string()))?;
        Ok(())
    }

    pub fn get_skill(&self, name: &str) -> Result<Option<SkillRecord>> {
        let conn = Connection::open(&self.db_path).map_err(|e| HgbError::Execution(e.to_string()))?;
        let mut stmt = conn.prepare("SELECT name, description, content, tier, triggers FROM skills WHERE name = ?1")
            .map_err(|e| HgbError::Execution(e.to_string()))?;
        
        let mut rows = stmt.query(params![name]).map_err(|e| HgbError::Execution(e.to_string()))?;
        if let Some(row) = rows.next().map_err(|e| HgbError::Execution(e.to_string()))? {
            let triggers_raw: String = row.get(4).unwrap_or_default();
            let triggers: Vec<String> = serde_json::from_str(&triggers_raw).unwrap_or_default();
            Ok(Some(SkillRecord {
                name: row.get(0).map_err(|e| HgbError::Execution(e.to_string()))?,
                description: row.get(1).map_err(|e| HgbError::Execution(e.to_string()))?,
                content: row.get(2).map_err(|e| HgbError::Execution(e.to_string()))?,
                tier: row.get(3).map_err(|e| HgbError::Execution(e.to_string()))?,
                triggers,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn count_skills(&self) -> Result<usize> {
        let conn = Connection::open(&self.db_path).map_err(|e| HgbError::Execution(e.to_string()))?;
        let count: i64 = conn.query_row("SELECT count(*) FROM skills", [], |row| row.get(0))
            .unwrap_or(0);
        Ok(count as usize)
    }
}
