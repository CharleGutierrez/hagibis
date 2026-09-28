//! Superpower 106: Persistent Project Memory & Context Profiles (Windsurf Cascade Parity)
//!
//! Per-project memory profile storing persistent architectural decisions, coding
//! conventions (indentation, test commands, linting rules), model preferences,
//! and developer intent rings across sessions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::SystemTime;
use crate::error::HgbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectCodingConventions {
    pub indent_style: String,
    pub test_framework: String,
    pub linter_command: Option<String>,
    pub strict_null_safety: bool,
}

impl Default for ProjectCodingConventions {
    fn default() -> Self {
        Self {
            indent_style: "4-spaces".to_string(),
            test_framework: "cargo-test".to_string(),
            linter_command: Some("cargo clippy".to_string()),
            strict_null_safety: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProfile {
    pub project_name: String,
    pub root_path: String,
    pub preferred_model: String,
    pub conventions: ProjectCodingConventions,
    pub architectural_invariants: Vec<String>,
    pub persistent_notes: Vec<String>,
    pub last_synced_at_secs: u64,
}

static PROFILE_STORE: RwLock<Option<HashMap<String, ProjectProfile>>> = RwLock::new(None);

pub struct ProfileEngine;

impl ProfileEngine {
    fn with_store<F, R>(f: F) -> R
    where
        F: FnOnce(&mut HashMap<String, ProjectProfile>) -> R,
    {
        let mut guard = PROFILE_STORE.write().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(HashMap::new());
        }
        f(guard.as_mut().unwrap())
    }

    /// Fetches the profile for a given root directory, or initializes defaults if not present
    pub fn get_profile(root_path: &str) -> Result<ProjectProfile, HgbError> {
        let clean_path = if root_path.trim().is_empty() { "." } else { root_path.trim() };

        Self::with_store(|store| {
            if let Some(profile) = store.get(clean_path) {
                return Ok(profile.clone());
            }

            let timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let project_name = clean_path
                .trim_end_matches('/')
                .split('/')
                .last()
                .unwrap_or("hagibis-project")
                .to_string();

            let conventions = if std::path::Path::new(&format!("{}/package.json", clean_path)).exists() {
                ProjectCodingConventions {
                    indent_style: "2-spaces".to_string(),
                    test_framework: "jest".to_string(),
                    linter_command: Some("npm run lint".to_string()),
                    strict_null_safety: true,
                }
            } else if std::path::Path::new(&format!("{}/pyproject.toml", clean_path)).exists() {
                ProjectCodingConventions {
                    indent_style: "4-spaces".to_string(),
                    test_framework: "pytest".to_string(),
                    linter_command: Some("ruff check .".to_string()),
                    strict_null_safety: true,
                }
            } else {
                ProjectCodingConventions::default()
            };

            let profile = ProjectProfile {
                project_name,
                root_path: clean_path.to_string(),
                preferred_model: "auto".to_string(),
                conventions,
                architectural_invariants: vec![
                    "Enforce zero panics on hot execution paths".to_string(),
                    "Keep IPC wire framing length-delimited with Bincode".to_string(),
                ],
                persistent_notes: vec![
                    "Auto-generated persistent memory profile by Hagibis".to_string(),
                ],
                last_synced_at_secs: timestamp,
            };

            store.insert(clean_path.to_string(), profile.clone());
            Ok(profile)
        })
    }

    /// Patches conventions for an existing project profile
    pub fn patch_conventions(
        root_path: &str,
        conventions: ProjectCodingConventions,
    ) -> Result<ProjectProfile, HgbError> {
        let clean_path = if root_path.trim().is_empty() { "." } else { root_path.trim() };

        Self::with_store(|store| {
            let mut profile = store.remove(clean_path).unwrap_or_else(|| {
                let timestamp = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                ProjectProfile {
                    project_name: "hagibis-project".to_string(),
                    root_path: clean_path.to_string(),
                    preferred_model: "auto".to_string(),
                    conventions: ProjectCodingConventions::default(),
                    architectural_invariants: Vec::new(),
                    persistent_notes: Vec::new(),
                    last_synced_at_secs: timestamp,
                }
            });

            profile.conventions = conventions;
            profile.last_synced_at_secs = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            store.insert(clean_path.to_string(), profile.clone());
            Ok(profile)
        })
    }
}
