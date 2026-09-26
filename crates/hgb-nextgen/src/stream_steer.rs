use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Lifecycle status of the mid-flight streaming steering controller
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SteeringStatus {
    Running,
    Paused,
    Nudged { count: usize },
    Resumed,
    Aborted { reason: String },
}

/// Detailed report generated when a developer injects an in-flight guidance nudge
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NudgeReport {
    pub instruction: String,
    pub total_nudges: usize,
    pub previous_status: SteeringStatus,
    pub active_status: SteeringStatus,
    pub prompt_modifier: String,
}

/// Mid-Flight Streaming Steering Controller ("Brake & Nudge")
#[derive(Debug, Clone)]
pub struct StreamSteeringController {
    status: Arc<Mutex<SteeringStatus>>,
    nudge_history: Arc<Mutex<Vec<String>>>,
}

impl Default for StreamSteeringController {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamSteeringController {
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(SteeringStatus::Running)),
            nudge_history: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Pause the active LLM token generation stream or ReAct agent step
    pub fn pause(&self) -> bool {
        let mut guard = self.status.lock().unwrap();
        if *guard == SteeringStatus::Running || matches!(*guard, SteeringStatus::Nudged { .. }) {
            *guard = SteeringStatus::Paused;
            true
        } else {
            false
        }
    }

    /// Inject an in-flight course correction nudge into the active session
    pub fn nudge(&self, instruction: &str) -> NudgeReport {
        let mut status_guard = self.status.lock().unwrap();
        let mut history_guard = self.nudge_history.lock().unwrap();

        let prev = status_guard.clone();
        history_guard.push(instruction.trim().to_string());
        let count = history_guard.len();

        let new_status = SteeringStatus::Nudged { count };
        *status_guard = new_status.clone();

        let prompt_modifier = format!(
            "<mid_flight_nudge>\n\
             COURSE CORRECTION ORDER #{}:\n\
             \"{}\"\n\
             INSTRUCTION: Immediately pivot. Discard any in-flight conflicting edits and align strictly with this course correction.\n\
             </mid_flight_nudge>",
            count,
            instruction.trim()
        );

        NudgeReport {
            instruction: instruction.trim().to_string(),
            total_nudges: count,
            previous_status: prev,
            active_status: new_status,
            prompt_modifier,
        }
    }

    /// Resume execution with course corrections integrated
    pub fn resume(&self) -> bool {
        let mut guard = self.status.lock().unwrap();
        if *guard == SteeringStatus::Paused || matches!(*guard, SteeringStatus::Nudged { .. }) {
            *guard = SteeringStatus::Resumed;
            true
        } else {
            false
        }
    }

    /// Abort the generation immediately
    pub fn abort(&self, reason: &str) -> bool {
        let mut guard = self.status.lock().unwrap();
        *guard = SteeringStatus::Aborted {
            reason: reason.to_string(),
        };
        true
    }

    /// Current steering status
    pub fn status(&self) -> SteeringStatus {
        self.status.lock().unwrap().clone()
    }

    /// List of all injected nudges for this session
    pub fn nudge_history(&self) -> Vec<String> {
        self.nudge_history.lock().unwrap().clone()
    }

    /// Augment an outgoing or in-flight prompt with all accumulated nudges
    pub fn apply_steering_to_prompt(&self, base_prompt: &str) -> String {
        let history = self.nudge_history();
        if history.is_empty() {
            return base_prompt.to_string();
        }

        let mut out = base_prompt.to_string();
        out.push_str("\n\n<steering_directives>\n");
        for (i, n) in history.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, n));
        }
        out.push_str("</steering_directives>");
        out
    }
}
