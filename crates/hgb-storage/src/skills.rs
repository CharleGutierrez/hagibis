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

    pub fn match_skills(&self, prompt: &str) -> Result<Vec<SkillRecord>> {
        let conn = Connection::open(&self.db_path).map_err(|e| HgbError::Execution(e.to_string()))?;
        let mut stmt = conn.prepare("SELECT name, description, content, tier, triggers FROM skills")
            .map_err(|e| HgbError::Execution(e.to_string()))?;
        
        let mut rows = stmt.query([]).map_err(|e| HgbError::Execution(e.to_string()))?;
        let mut matched = Vec::new();
        let prompt_lower = prompt.to_lowercase();

        while let Some(row) = rows.next().map_err(|e| HgbError::Execution(e.to_string()))? {
            let triggers_raw: String = row.get(4).unwrap_or_default();
            let triggers: Vec<String> = serde_json::from_str(&triggers_raw).unwrap_or_default();
            
            let should_include = triggers.iter().any(|t| prompt_lower.contains(&t.to_lowercase()));
            
            if should_include {
                matched.push(SkillRecord {
                    name: row.get(0).unwrap_or_default(),
                    description: row.get(1).unwrap_or_default(),
                    content: row.get(2).unwrap_or_default(),
                    tier: row.get(3).unwrap_or_default(),
                    triggers,
                });
            }
        }
        Ok(matched)
    }

    /// Seeds the store with 1000% real default skills if empty
    pub fn seed(&self) -> Result<()> {
        if self.count_skills().unwrap_or(0) > 0 {
            return Ok(()); // Already seeded
        }

        let default_skills = vec![
            SkillRecord {
                name: "TDD_Loop".to_string(),
                description: "Test-Driven Development self-healing loop".to_string(),
                content: "When writing code, always write failing tests first, then implement the minimal code to make them pass, and finally refactor. Use the `cargo test` command to verify.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["tdd".to_string(), "test-driven".to_string(), "failing test".to_string()],
            },
            SkillRecord {
                name: "Rust_Systems_Expert".to_string(),
                description: "Expertise in zero-cost abstractions and memory safety".to_string(),
                content: "Prioritize stack allocation, avoid unnecessary `.clone()`, and use `std::borrow::Cow` or references with lifetimes when dealing with large strings or byte arrays.".to_string(),
                tier: "expert".to_string(),
                triggers: vec!["rust".to_string(), "performance".to_string(), "memory".to_string(), "systems".to_string()],
            },
            SkillRecord {
                name: "Architecture_Review".to_string(),
                description: "Holistic architectural review skill".to_string(),
                content: "Always analyze dependencies, data flow, and separation of concerns. Break monoliths into crates if compilation times exceed 5s, and use ports & adapters (hexagonal) architecture.".to_string(),
                tier: "architect".to_string(),
                triggers: vec!["architecture".to_string(), "design".to_string(), "refactor".to_string(), "review".to_string()],
            },
            SkillRecord {
                name: "Universal_MCP_Integration".to_string(),
                description: "Universal Model Context Protocol client integration".to_string(),
                content: "Dynamically connects to external MCP servers (Postgres, GitHub, Slack) to aggregate tools under a unified namespace. Auto-discovers .hgb/mcp.json files.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["mcp".to_string(), "tools".to_string(), "protocol".to_string(), "server".to_string()],
            },
            SkillRecord {
                name: "Local_First_SLM_Orchestration".to_string(),
                description: "Routes logic to local models vs cloud based on complexity".to_string(),
                content: "Use LLM Cost Gateway to route simple tasks to local Ollama (qwen2.5-coder) or cheap models, and complex reasoning to frontier models, avoiding runaway API costs.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["slm".to_string(), "ollama".to_string(), "local".to_string(), "cost".to_string(), "arbitrage".to_string()],
            },
            SkillRecord {
                name: "Continuous_Duplex_Voice".to_string(),
                description: "WebRTC and WebSocket architecture for real-time voice".to_string(),
                content: "Maintains a full-duplex voice loop using VAD thresholds, handling barge-ins gracefully, and triggering acoustic earcons for ambient feedback.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["voice".to_string(), "webrtc".to_string(), "duplex".to_string(), "audio".to_string()],
            },
            SkillRecord {
                name: "Phantom_Swarm_Worktrees".to_string(),
                description: "Parallel autonomous dev branches using git worktrees".to_string(),
                content: "Orchestrates multi-agent parallel execution across N isolated git worktrees, preventing index locks and state collision for invisible agentic fixes.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["worktree".to_string(), "phantom".to_string(), "swarm".to_string(), "parallel".to_string()],
            },
            SkillRecord {
                name: "Remote_Cloud_Micro_Sandboxing".to_string(),
                description: "Secure ephemeral code execution inside micro-VMs".to_string(),
                content: "Executes untrusted tasks in a capability-restricted jail with strict environment sanitization, ephemeral overlay FS, and timeouts.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["sandbox".to_string(), "jail".to_string(), "ephemeral".to_string(), "security".to_string()],
            },
            SkillRecord {
                name: "Semantic_Code_Graph".to_string(),
                description: "AST-based continuous spatial memory for relationships".to_string(),
                content: "Analyzes ambient AST contexts continuously, determining enclosing symbols, gathering specific imports, and resolving multi-crate architectural relationships beyond standard vector space.".to_string(),
                tier: "core".to_string(),
                triggers: vec!["ast".to_string(), "semantic".to_string(), "graph".to_string(), "ambient".to_string()],
            }
        ];

        for skill in default_skills {
            self.insert_skill(&skill)?;
        }

        Ok(())
    }
}
