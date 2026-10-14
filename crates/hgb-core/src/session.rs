//! Cockpit Session Persistence & Resumption Engine for Hagibis
//!
//! Stores and restores full Cockpit session data (`CockpitSessionData`, `SessionSummary`).
//! Persists atomic JSON into `.hgb/sessions/<id>.json` and maintains `.hgb/sessions/latest.json`.
//! Supports multi-turn chat history, DAG nodes, tool executions, artifact diffs, and prompt history.

use crate::error::{HgbError, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A recorded tool invocation within a session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionToolCall {
    pub tool_name: String,
    pub parameters_summary: String,
    pub status: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub output_snippet: Option<String>,
}

impl SessionToolCall {
    pub fn new(
        tool_name: impl Into<String>,
        parameters_summary: impl Into<String>,
        status: impl Into<String>,
        duration_ms: u64,
        output_snippet: Option<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            parameters_summary: parameters_summary.into(),
            status: status.into(),
            duration_ms,
            output_snippet,
        }
    }
}

/// A conversation turn item stored in the session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens: usize,
    pub timestamp: String,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<SessionToolCall>,
}

impl SessionChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            model: None,
            tokens: 0,
            timestamp: Utc::now().to_rfc3339(),
            thinking: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>, model: Option<String>, tokens: usize) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            model,
            tokens,
            timestamp: Utc::now().to_rfc3339(),
            thinking: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
            model: None,
            tokens: 0,
            timestamp: Utc::now().to_rfc3339(),
            thinking: None,
            tool_calls: Vec::new(),
        }
    }
}

/// A node in the execution DAG tracked in the session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionDagNode {
    pub id: String,
    pub name: String,
    pub model: String,
    pub status: String,
    pub tokens_used: u64,
    #[serde(default)]
    pub scratchpad: String,
    #[serde(default)]
    pub tool_calls: Vec<SessionToolCall>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// File artifact change diff tracked within the session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionArtifactDiff {
    pub file_path: String,
    pub added_lines: usize,
    pub deleted_lines: usize,
    pub diff_content: String,
}

/// Complete Cockpit session snapshot data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitSessionData {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub active_model: String,
    pub workspace_root: String,
    pub messages: Vec<SessionChatMessage>,
    #[serde(default)]
    pub dag_nodes: Vec<SessionDagNode>,
    #[serde(default)]
    pub prompt_history: Vec<String>,
    #[serde(default)]
    pub artifact_diffs: Vec<SessionArtifactDiff>,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub pinned_goal: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl CockpitSessionData {
    /// Create a new session initialized with generated ID and timestamps
    pub fn new(workspace_root: impl Into<String>, active_model: impl Into<String>) -> Self {
        let now = Utc::now().to_rfc3339();
        let id = format!("ses_{}", uuid::Uuid::new_v4().simple());
        Self {
            id,
            title: "New Cockpit Session".to_string(),
            created_at: now.clone(),
            updated_at: now,
            active_model: active_model.into(),
            workspace_root: workspace_root.into(),
            messages: Vec::new(),
            dag_nodes: Vec::new(),
            prompt_history: Vec::new(),
            artifact_diffs: Vec::new(),
            total_tokens: 0,
            pinned_goal: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.messages.push(SessionChatMessage::user(content));
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn add_assistant_message(
        &mut self,
        content: impl Into<String>,
        model: Option<String>,
        tokens: usize,
    ) {
        self.messages
            .push(SessionChatMessage::assistant(content, model, tokens));
        self.total_tokens += tokens as u64;
        self.updated_at = Utc::now().to_rfc3339();
    }

    pub fn to_summary(&self, file_size_bytes: u64) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            active_model: self.active_model.clone(),
            message_count: self.messages.len(),
            total_tokens: self.total_tokens,
            file_size_bytes,
        }
    }
}

/// Lightweight summary metadata of a saved session for fast list/selection queries
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub active_model: String,
    pub message_count: usize,
    pub total_tokens: u64,
    pub file_size_bytes: u64,
}

/// Resolve the session storage directory (`<root>/.hgb/sessions`)
pub fn resolve_sessions_dir(root_dir: Option<&Path>) -> PathBuf {
    let base = match root_dir {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    base.join(".hgb").join("sessions")
}

/// Save session data atomically into `.hgb/sessions/<id>.json` and maintain `.hgb/sessions/latest.json`
pub fn save(session: &CockpitSessionData, root_dir: Option<&Path>) -> Result<PathBuf> {
    let sessions_dir = resolve_sessions_dir(root_dir);
    fs::create_dir_all(&sessions_dir)
        .map_err(|e| HgbError::Storage(format!("Failed to create sessions directory: {e}")))?;

    let json_bytes = serde_json::to_vec_pretty(session)
        .map_err(|e| HgbError::Serialization(format!("Failed to serialize session JSON: {e}")))?;

    // 1. Atomic write to .hgb/sessions/<id>.json
    let target_file = sessions_dir.join(format!("{}.json", session.id));
    let tmp_file = sessions_dir.join(format!("{}.json.tmp.{}", session.id, uuid::Uuid::new_v4().simple()));

    {
        let mut f = File::create(&tmp_file)
            .map_err(|e| HgbError::Storage(format!("Failed to create temp session file: {e}")))?;
        f.write_all(&json_bytes)
            .map_err(|e| HgbError::Storage(format!("Failed to write temp session file: {e}")))?;
        f.sync_all()
            .map_err(|e| HgbError::Storage(format!("Failed to sync temp session file: {e}")))?;
    }

    fs::rename(&tmp_file, &target_file)
        .map_err(|e| HgbError::Storage(format!("Failed to rename temp file to target session: {e}")))?;

    // 2. Atomic write/update to .hgb/sessions/latest.json
    let latest_file = sessions_dir.join("latest.json");
    let latest_tmp = sessions_dir.join(format!("latest.json.tmp.{}", uuid::Uuid::new_v4().simple()));

    {
        let mut f = File::create(&latest_tmp)
            .map_err(|e| HgbError::Storage(format!("Failed to create temp latest file: {e}")))?;
        f.write_all(&json_bytes)
            .map_err(|e| HgbError::Storage(format!("Failed to write temp latest file: {e}")))?;
        f.sync_all()
            .map_err(|e| HgbError::Storage(format!("Failed to sync temp latest file: {e}")))?;
    }

    fs::rename(&latest_tmp, &latest_file)
        .map_err(|e| HgbError::Storage(format!("Failed to update latest.json: {e}")))?;

    Ok(target_file)
}

/// Load a specific session by its session ID from `.hgb/sessions/<id>.json`
pub fn load(session_id: &str, root_dir: Option<&Path>) -> Result<CockpitSessionData> {
    let sessions_dir = resolve_sessions_dir(root_dir);
    let target_file = sessions_dir.join(format!("{}.json", session_id));

    if !target_file.exists() {
        return Err(HgbError::NotFound(format!(
            "Session '{}' not found at {:?}",
            session_id, target_file
        )));
    }

    let raw = fs::read_to_string(&target_file)
        .map_err(|e| HgbError::Storage(format!("Failed to read session file: {e}")))?;

    serde_json::from_str(&raw)
        .map_err(|e| HgbError::Serialization(format!("Corrupt session data for '{}': {e}", session_id)))
}

/// Load the most recently saved session from `.hgb/sessions/latest.json`, or falls back to latest timestamp
pub fn load_latest(root_dir: Option<&Path>) -> Result<CockpitSessionData> {
    let sessions_dir = resolve_sessions_dir(root_dir);
    let latest_file = sessions_dir.join("latest.json");

    if latest_file.exists() {
        let raw = fs::read_to_string(&latest_file)
            .map_err(|e| HgbError::Storage(format!("Failed to read latest.json: {e}")))?;
        if let Ok(data) = serde_json::from_str::<CockpitSessionData>(&raw) {
            return Ok(data);
        }
    }

    // Fallback: search all session files and pick newest
    let sessions = list_sessions(root_dir)?;
    if let Some(first) = sessions.first() {
        load(&first.id, root_dir)
    } else {
        Err(HgbError::NotFound("No saved sessions exist in workspace".to_string()))
    }
}

/// List all saved sessions in `.hgb/sessions/`, sorted by `updated_at` descending
pub fn list_sessions(root_dir: Option<&Path>) -> Result<Vec<SessionSummary>> {
    let sessions_dir = resolve_sessions_dir(root_dir);
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(&sessions_dir)
        .map_err(|e| HgbError::Storage(format!("Failed to read sessions directory: {e}")))?;

    let mut summaries = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            // Skip latest.json and temporary files
            if filename == "latest.json" || filename.contains(".tmp.") || !filename.ends_with(".json") {
                continue;
            }

            if let Ok(metadata) = entry.metadata() {
                let file_size = metadata.len();
                if let Ok(raw) = fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<CockpitSessionData>(&raw) {
                        summaries.push(data.to_summary(file_size));
                    }
                }
            }
        }
    }

    // Sort by updated_at descending (newest first)
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    Ok(summaries)
}

/// Delete a session by its session ID
pub fn delete(session_id: &str, root_dir: Option<&Path>) -> Result<()> {
    let sessions_dir = resolve_sessions_dir(root_dir);
    let target_file = sessions_dir.join(format!("{}.json", session_id));

    if target_file.exists() {
        fs::remove_file(&target_file)
            .map_err(|e| HgbError::Storage(format!("Failed to remove session file: {e}")))?;
    }

    // If latest.json referenced this session, remove or update it
    let latest_file = sessions_dir.join("latest.json");
    if latest_file.exists() {
        if let Ok(raw) = fs::read_to_string(&latest_file) {
            if let Ok(data) = serde_json::from_str::<CockpitSessionData>(&raw) {
                if data.id == session_id {
                    let _ = fs::remove_file(&latest_file);
                    // Point to the next newest session if any exist
                    if let Ok(remaining) = list_sessions(root_dir) {
                        if let Some(next_newest) = remaining.first() {
                            if let Ok(next_data) = load(&next_newest.id, root_dir) {
                                let _ = save(&next_data, root_dir);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AutoCleanDir(PathBuf);
    impl AutoCleanDir {
        fn new(prefix: &str) -> Self {
            let p = std::env::temp_dir().join(format!("{}_{}", prefix, uuid::Uuid::new_v4().simple()));
            let _ = fs::create_dir_all(&p);
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for AutoCleanDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_session_lifecycle_save_load_list() {
        let temp_dir = AutoCleanDir::new("hgb_ses_test");
        let ws_root = temp_dir.path();

        let mut session = CockpitSessionData::new(ws_root.to_string_lossy(), "gemini-2.5-flash")
            .with_title("Test Persistence Sprint");

        session.add_user_message("Refactor the kernel");
        session.add_assistant_message("Here is the plan", Some("gemini-2.5-flash".to_string()), 450);

        session.dag_nodes.push(SessionDagNode {
            id: "node_1".to_string(),
            name: "Audit".to_string(),
            model: "gemini-2.5-flash".to_string(),
            status: "Completed".to_string(),
            tokens_used: 450,
            scratchpad: "Done".to_string(),
            tool_calls: vec![SessionToolCall::new("file_read", "lib.rs", "success", 12, None)],
            parent_id: None,
        });

        session.prompt_history.push("Refactor the kernel".to_string());

        // 1. Save
        let saved_path = save(&session, Some(ws_root)).expect("Save must succeed");
        assert!(saved_path.exists());

        // 2. Load by ID
        let loaded = load(&session.id, Some(ws_root)).expect("Load by ID must succeed");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.title, "Test Persistence Sprint");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.total_tokens, 450);
        assert_eq!(loaded.dag_nodes.len(), 1);

        // 3. Load latest
        let latest = load_latest(Some(ws_root)).expect("Load latest must succeed");
        assert_eq!(latest.id, session.id);

        // 4. List sessions
        let list = list_sessions(Some(ws_root)).expect("List sessions must succeed");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, session.id);
        assert_eq!(list[0].message_count, 2);

        // 5. Delete session
        delete(&session.id, Some(ws_root)).expect("Delete must succeed");
        let list_after = list_sessions(Some(ws_root)).expect("List after delete");
        assert!(list_after.is_empty());
        assert!(!saved_path.exists());
    }

    #[test]
    fn test_load_non_existent_session() {
        let temp_dir = AutoCleanDir::new("hgb_ses_err_test");
        let ws_root = temp_dir.path();
        let res = load("non_existent_id", Some(ws_root));
        assert!(res.is_err());
    }
}
