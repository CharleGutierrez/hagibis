use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Blocked,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub dependencies: Vec<String>,
    pub created_at_utc: String,
    pub updated_at_utc: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProjectAdrStatus {
    Proposed,
    Accepted,
    Rejected,
    Superceded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdrRecord {
    pub id: usize,
    pub title: String,
    pub status: ProjectAdrStatus,
    pub context: String,
    pub decision: String,
    pub consequences: String,
    pub why_rationale: String,
    pub created_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCoordinatorSnapshot {
    pub project_name: String,
    pub total_tasks: usize,
    pub pending_tasks: usize,
    pub in_progress_tasks: usize,
    pub completed_tasks: usize,
    pub blocked_tasks: usize,
    pub adrs_count: usize,
    pub completion_percentage: f32,
    pub handoff_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCoordinatorState {
    pub project_name: String,
    pub tasks: HashMap<String, ProjectTask>,
    pub adrs: Vec<AdrRecord>,
    pub active_milestone: String,
    pub last_updated_utc: String,
}

impl Default for ProjectCoordinatorState {
    fn default() -> Self {
        Self {
            project_name: "hagibis_project".to_string(),
            tasks: HashMap::new(),
            adrs: Vec::new(),
            active_milestone: "v1.0.0".to_string(),
            last_updated_utc: chrono::Utc::now().to_rfc3339(),
        }
    }
}

pub struct ProjectCoordinator {
    state_file: PathBuf,
    state: Mutex<ProjectCoordinatorState>,
}

static GLOBAL_COORDINATOR: OnceLock<Arc<ProjectCoordinator>> = OnceLock::new();

impl ProjectCoordinator {
    pub fn global() -> Arc<Self> {
        GLOBAL_COORDINATOR
            .get_or_init(|| {
                Arc::new(ProjectCoordinator::new(
                    PathBuf::from(".hgb").join("project").join("coordinator_state.json"),
                ))
            })
            .clone()
    }

    pub fn new(state_file: PathBuf) -> Self {
        let state = if state_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&state_file) {
                serde_json::from_str(&content).unwrap_or_default()
            } else {
                ProjectCoordinatorState::default()
            }
        } else {
            ProjectCoordinatorState::default()
        };

        Self {
            state_file,
            state: Mutex::new(state),
        }
    }

    pub fn add_task(&self, title: &str, description: &str, priority: TaskPriority, tags: Vec<String>) -> ProjectTask {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = format!("task_{:04}", guard.tasks.len() + 1);
        let now = chrono::Utc::now().to_rfc3339();

        let task = ProjectTask {
            id: id.clone(),
            title: title.to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            priority,
            dependencies: Vec::new(),
            created_at_utc: now.clone(),
            updated_at_utc: now,
            tags,
        };

        guard.tasks.insert(id, task.clone());
        guard.last_updated_utc = chrono::Utc::now().to_rfc3339();
        let _ = self.save_locked(&guard);
        task
    }

    pub fn update_task_status(&self, task_id: &str, status: TaskStatus) -> Option<ProjectTask> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = guard.tasks.get_mut(task_id) {
            task.status = status;
            task.updated_at_utc = chrono::Utc::now().to_rfc3339();
            let res = task.clone();
            guard.last_updated_utc = chrono::Utc::now().to_rfc3339();
            let _ = self.save_locked(&guard);
            Some(res)
        } else {
            None
        }
    }

    pub fn list_tasks(&self) -> Vec<ProjectTask> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<ProjectTask> = guard.tasks.values().cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    pub fn add_adr(&self, title: &str, context: &str, decision: &str, consequences: &str, why: &str) -> AdrRecord {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = guard.adrs.len() + 1;
        let record = AdrRecord {
            id,
            title: title.to_string(),
            status: ProjectAdrStatus::Accepted,
            context: context.to_string(),
            decision: decision.to_string(),
            consequences: consequences.to_string(),
            why_rationale: why.to_string(),
            created_at_utc: chrono::Utc::now().to_rfc3339(),
        };

        guard.adrs.push(record.clone());
        guard.last_updated_utc = chrono::Utc::now().to_rfc3339();
        let _ = self.save_locked(&guard);
        record
    }

    pub fn list_adrs(&self) -> Vec<AdrRecord> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.adrs.clone()
    }

    pub fn snapshot(&self) -> ProjectCoordinatorSnapshot {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let total = guard.tasks.len();
        let pending = guard.tasks.values().filter(|t| t.status == TaskStatus::Pending).count();
        let in_prog = guard.tasks.values().filter(|t| t.status == TaskStatus::InProgress).count();
        let completed = guard.tasks.values().filter(|t| t.status == TaskStatus::Completed).count();
        let blocked = guard.tasks.values().filter(|t| t.status == TaskStatus::Blocked).count();

        let pct = if total > 0 {
            (completed as f32 / total as f32) * 100.0
        } else {
            0.0
        };

        let handoff_summary = format!(
            "Project '{}' Milestone '{}': {}/{} tasks completed ({:.1}%). Active: {}, Blocked: {}. Recorded ADRs: {}.",
            guard.project_name, guard.active_milestone, completed, total, pct, in_prog, blocked, guard.adrs.len()
        );

        ProjectCoordinatorSnapshot {
            project_name: guard.project_name.clone(),
            total_tasks: total,
            pending_tasks: pending,
            in_progress_tasks: in_prog,
            completed_tasks: completed,
            blocked_tasks: blocked,
            adrs_count: guard.adrs.len(),
            completion_percentage: pct,
            handoff_summary,
        }
    }

    fn save_locked(&self, state: &ProjectCoordinatorState) -> std::io::Result<()> {
        if let Some(parent) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(state).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&self.state_file, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinator_tasks_and_adrs() {
        let tmp = std::env::temp_dir().join("hgb_coord_test.json");
        let _ = std::fs::remove_file(&tmp);
        let coord = ProjectCoordinator::new(tmp.clone());

        let t1 = coord.add_task("Build Rails Engine", "Implement ActiveRecord zero downtime linter", TaskPriority::High, vec!["rails".to_string()]);
        assert_eq!(t1.id, "task_0001");
        assert_eq!(t1.status, TaskStatus::Pending);

        let updated = coord.update_task_status(&t1.id, TaskStatus::Completed).unwrap();
        assert_eq!(updated.status, TaskStatus::Completed);

        let adr = coord.add_adr(
            "ADR-001: Sub-millisecond Bincode Framing",
            "Needed length-delimited framing over UDS",
            "Adopt 4-byte big-endian length prefix with bincode",
            "Eliminates 64KB message cap",
            "Max throughput and zero deserialization ambiguity",
        );
        assert_eq!(adr.id, 1);
        assert_eq!(adr.status, ProjectAdrStatus::Accepted);

        let snap = coord.snapshot();
        assert_eq!(snap.total_tasks, 1);
        assert_eq!(snap.completed_tasks, 1);
        assert_eq!(snap.completion_percentage, 100.0);
        assert_eq!(snap.adrs_count, 1);

        let _ = std::fs::remove_file(&tmp);
    }
}
