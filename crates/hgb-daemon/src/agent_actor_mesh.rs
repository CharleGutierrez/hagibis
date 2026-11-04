//! # Pillar 2: Event-Driven Actor Model (`agent_actor_mesh.rs`)
//!
//! High-concurrency event-driven Actor Model for autonomous agent loops embedded in `hgb-daemon`.
//! Codifies Masterclass Chapter 8 (`docs/masterclass/ch8_autonomous_loops.py`).
//!
//! ## Core Architecture:
//! - **Isolated Actor Mailboxes**: Each autonomous agent runs inside an isolated Tokio task
//!   with its own `mpsc::Receiver<ActorMessage>` mailbox.
//! - **ActorSupervisor**: Central actor supervisor managing lifecycle, heartbeat health monitoring,
//!   cross-agent message passing routing, and self-healing recovery.
//! - **Panic Isolation**: If an agent panics or suffers an unhandled error, the supervisor
//!   isolates the failure cleanly via `AssertUnwindSafe` / Tokio `JoinHandle`, preventing daemon crashes,
//!   and can automatically resume execution from the persistent checkpoint store.
//! - **Supported Actor Messages**:
//!   1. `StartGoal { goal_id, instruction }`
//!   2. `StepHeartbeat { step_index, status }`
//!   3. `MessagePass { to_actor_id, payload }`
//!   4. `StateCheckpointRequest`
//!   5. `Terminate { reason }`

use chrono::Utc;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, RwLock};

use hgb_core::error::{HgbError, Result};
use hgb_storage::actor_checkpoint_store::{
    ActorCheckpoint, ActorCheckpointStore, ActorGoalStatus, ActorStateSnapshot,
};

/// Default capacity for actor mailboxes
pub const DEFAULT_ACTOR_MAILBOX_CAPACITY: usize = 256;
/// Default heartbeat timeout in milliseconds before actor is flagged as degraded (30 seconds)
pub const DEFAULT_HEARTBEAT_TIMEOUT_MS: u64 = 30_000;

// ============================================================================
// Actor Messages & Health Status
// ============================================================================

/// Messages accepted and dispatched across autonomous agent actors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActorMessage {
    /// Initiate a new high-level autonomous goal
    StartGoal {
        goal_id: String,
        instruction: String,
    },
    /// Heartbeat signal indicating progress and current step execution
    StepHeartbeat {
        step_index: usize,
        status: String,
    },
    /// Cross-agent communication message passed across actor boundaries
    MessagePass {
        to_actor_id: String,
        payload: serde_json::Value,
    },
    /// Trigger an atomic snapshot and persistent checkpoint flush to SQLite WAL
    StateCheckpointRequest,
    /// Gracefully terminate actor loop with a specified reason
    Terminate {
        reason: String,
    },
    /// Direct execution instruction for a discrete autonomous step
    ExecuteStep {
        step_index: usize,
        action: String,
        payload: Option<serde_json::Value>,
    },
    /// Inbound message delivered from another actor via the supervisor router
    DeliverMessage {
        from_actor_id: String,
        payload: serde_json::Value,
    },
}

/// Dynamic health and lifecycle state of an autonomous agent actor
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActorHealthStatus {
    Uninitialized,
    Idle,
    Running,
    Suspended,
    Recovering,
    Faulted(String),
    Panicked(String),
    Terminated(String),
}

impl std::fmt::Display for ActorHealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uninitialized => write!(f, "Uninitialized"),
            Self::Idle => write!(f, "Idle"),
            Self::Running => write!(f, "Running"),
            Self::Suspended => write!(f, "Suspended"),
            Self::Recovering => write!(f, "Recovering"),
            Self::Faulted(err) => write!(f, "Faulted: {}", err),
            Self::Panicked(err) => write!(f, "Panicked: {}", err),
            Self::Terminated(reason) => write!(f, "Terminated: {}", reason),
        }
    }
}

/// Status summary report for telemetry and cockpit HUD
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorStatusSummary {
    pub actor_id: String,
    pub status: ActorHealthStatus,
    pub active_goal: Option<String>,
    pub step_index: usize,
    pub last_heartbeat_ms: u64,
    pub messages_processed: u64,
    pub checkpoints_saved: u64,
}

/// Dead letter message record for undeliverable cross-actor messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    pub from_actor_id: String,
    pub to_actor_id: String,
    pub payload: serde_json::Value,
    pub reason: String,
    pub timestamp_ms: i64,
}

/// Internal supervisor event channel payload
#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    Heartbeat {
        actor_id: String,
        step_index: usize,
        status: String,
    },
    RouteMessage {
        from_actor_id: String,
        to_actor_id: String,
        payload: serde_json::Value,
    },
    ActorCompleted {
        actor_id: String,
        goal_id: String,
    },
    ActorPanicked {
        actor_id: String,
        error_message: String,
    },
}

// ============================================================================
// AgentActor Implementation
// ============================================================================

/// Represents an autonomous agent loop instance with an isolated actor mailbox
pub struct AgentActor {
    pub actor_id: String,
    mailbox_rx: mpsc::Receiver<ActorMessage>,
    supervisor_tx: mpsc::Sender<SupervisorEvent>,
    checkpoint_store: Arc<ActorCheckpointStore>,
    state_snapshot: ActorStateSnapshot,
    message_inbox: Vec<(String, serde_json::Value)>,
    messages_processed: Arc<AtomicU64>,
    checkpoints_saved: Arc<AtomicU64>,
    current_step: Arc<AtomicUsize>,
    last_heartbeat: Arc<AtomicU64>,
}

impl AgentActor {
    /// Create a new AgentActor instance
    pub fn new(
        actor_id: impl Into<String>,
        mailbox_rx: mpsc::Receiver<ActorMessage>,
        supervisor_tx: mpsc::Sender<SupervisorEvent>,
        checkpoint_store: Arc<ActorCheckpointStore>,
    ) -> Self {
        let id = actor_id.into();
        let initial_snapshot = ActorStateSnapshot::new(
            &id,
            "none",
            "Awaiting goal",
            0,
            ActorGoalStatus::Pending,
        );

        Self {
            actor_id: id,
            mailbox_rx,
            supervisor_tx,
            checkpoint_store,
            state_snapshot: initial_snapshot,
            message_inbox: Vec::new(),
            messages_processed: Arc::new(AtomicU64::new(0)),
            checkpoints_saved: Arc::new(AtomicU64::new(0)),
            current_step: Arc::new(AtomicUsize::new(0)),
            last_heartbeat: Arc::new(AtomicU64::new(Utc::now().timestamp_millis() as u64)),
        }
    }

    /// Restore an actor from a persistent state snapshot
    pub fn restore_from_snapshot(
        actor_id: impl Into<String>,
        mailbox_rx: mpsc::Receiver<ActorMessage>,
        supervisor_tx: mpsc::Sender<SupervisorEvent>,
        checkpoint_store: Arc<ActorCheckpointStore>,
        snapshot: ActorStateSnapshot,
    ) -> Self {
        let id = actor_id.into();
        let step = snapshot.step_index;

        Self {
            actor_id: id,
            mailbox_rx,
            supervisor_tx,
            checkpoint_store,
            state_snapshot: snapshot,
            message_inbox: Vec::new(),
            messages_processed: Arc::new(AtomicU64::new(0)),
            checkpoints_saved: Arc::new(AtomicU64::new(0)),
            current_step: Arc::new(AtomicUsize::new(step)),
            last_heartbeat: Arc::new(AtomicU64::new(Utc::now().timestamp_millis() as u64)),
        }
    }

    /// Run the primary actor loop, processing messages from the isolated mailbox
    pub async fn run(mut self) {
        self.emit_heartbeat(self.current_step.load(Ordering::Relaxed), "Initialized").await;

        while let Some(msg) = self.mailbox_rx.recv().await {
            self.messages_processed.fetch_add(1, Ordering::Relaxed);
            self.last_heartbeat.store(Utc::now().timestamp_millis() as u64, Ordering::Relaxed);

            match msg {
                ActorMessage::StartGoal { goal_id, instruction } => {
                    self.state_snapshot.goal_id = goal_id.clone();
                    self.state_snapshot.instruction = instruction.clone();
                    self.state_snapshot.status = ActorGoalStatus::InProgress;
                    self.state_snapshot.step_index = 0;
                    self.state_snapshot.timestamp_ms = Utc::now().timestamp_millis();
                    self.state_snapshot.execution_log.push(format!("Goal started: {}", instruction));
                    self.current_step.store(0, Ordering::Relaxed);

                    // Save initial checkpoint
                    let _ = self.save_state_checkpoint().await;
                    self.emit_heartbeat(0, "GoalStarted").await;
                }

                ActorMessage::StepHeartbeat { step_index, status } => {
                    self.current_step.store(step_index, Ordering::Relaxed);
                    self.state_snapshot.step_index = step_index;
                    self.state_snapshot.timestamp_ms = Utc::now().timestamp_millis();
                    self.state_snapshot.execution_log.push(format!("Step {}: {}", step_index, status));

                    self.emit_heartbeat(step_index, &status).await;
                }

                ActorMessage::MessagePass { to_actor_id, payload } => {
                    if to_actor_id == self.actor_id {
                        // Loopback / self-message
                        self.message_inbox.push((self.actor_id.clone(), payload));
                    } else {
                        // Route cross-agent message through supervisor
                        let _ = self.supervisor_tx.send(SupervisorEvent::RouteMessage {
                            from_actor_id: self.actor_id.clone(),
                            to_actor_id,
                            payload,
                        }).await;
                    }
                }

                ActorMessage::DeliverMessage { from_actor_id, payload } => {
                    self.state_snapshot.execution_log.push(format!("Received message from {}", from_actor_id));
                    self.message_inbox.push((from_actor_id, payload));
                    self.emit_heartbeat(self.current_step.load(Ordering::Relaxed), "MessageReceived").await;
                }

                ActorMessage::StateCheckpointRequest => {
                    let _ = self.save_state_checkpoint().await;
                }

                ActorMessage::ExecuteStep { step_index, action, payload } => {
                    self.current_step.store(step_index, Ordering::Relaxed);
                    self.state_snapshot.step_index = step_index;
                    self.state_snapshot.execution_log.push(format!("Executing: {}", action));
                    if let Some(p) = payload {
                        self.state_snapshot.variables.insert(format!("step_{}_output", step_index), p);
                    }

                    // Auto-checkpoint on step completion
                    let _ = self.save_state_checkpoint().await;
                    self.emit_heartbeat(step_index, &format!("StepExecuted: {}", action)).await;
                }

                ActorMessage::Terminate { reason } => {
                    self.state_snapshot.execution_log.push(format!("Terminating: {}", reason));
                    self.state_snapshot.status = ActorGoalStatus::Interrupted;
                    let _ = self.save_state_checkpoint().await;
                    self.emit_heartbeat(self.current_step.load(Ordering::Relaxed), &format!("Terminated: {}", reason)).await;
                    break;
                }
            }
        }
    }

    /// Save state snapshot to the persistent SQLite WAL store
    async fn save_state_checkpoint(&mut self) -> Result<()> {
        let json = self.state_snapshot.to_json()?;
        let cp = ActorCheckpoint::new(
            &self.actor_id,
            &self.state_snapshot.goal_id,
            self.state_snapshot.step_index,
            json,
        );
        self.checkpoint_store.save_checkpoint(&cp)?;
        self.checkpoints_saved.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Emit heartbeat to supervisor
    async fn emit_heartbeat(&self, step_index: usize, status: &str) {
        let _ = self.supervisor_tx.send(SupervisorEvent::Heartbeat {
            actor_id: self.actor_id.clone(),
            step_index,
            status: status.to_string(),
        }).await;
    }
}

// ============================================================================
// Actor Registration & Handle
// ============================================================================

/// Handle held by the supervisor representing a managed actor
pub struct ActorHandle {
    pub actor_id: String,
    pub mailbox_tx: mpsc::Sender<ActorMessage>,
    pub status: Arc<RwLock<ActorHealthStatus>>,
    pub active_goal: Arc<RwLock<Option<String>>>,
    pub current_step: Arc<AtomicUsize>,
    pub last_heartbeat_ms: Arc<AtomicU64>,
    pub messages_sent: Arc<AtomicU64>,
    pub join_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

impl ActorHandle {
    pub fn is_alive(&self) -> bool {
        let guard = self.join_handle.try_read();
        match guard {
            Ok(handle_lock) => {
                if let Some(ref jh) = *handle_lock {
                    !jh.is_finished()
                } else {
                    false
                }
            }
            Err(_) => true,
        }
    }
}

// ============================================================================
// ActorSupervisor Implementation
// ============================================================================

/// Supervisor managing active agent actors, health monitoring, and cross-agent routing
pub struct ActorSupervisor {
    workspace_root: PathBuf,
    checkpoint_store: Arc<ActorCheckpointStore>,
    active_actors: Arc<RwLock<HashMap<String, Arc<ActorHandle>>>>,
    supervisor_tx: mpsc::Sender<SupervisorEvent>,
    dead_letters: Arc<RwLock<Vec<DeadLetterRecord>>>,
    total_restarts: Arc<AtomicU64>,
}

impl ActorSupervisor {
    /// Initialize a new actor supervisor with persistent checkpoint store
    pub fn new(
        workspace_root: impl AsRef<Path>,
        checkpoint_store: Arc<ActorCheckpointStore>,
    ) -> Self {
        let (supervisor_tx, supervisor_rx) = mpsc::channel(1024);
        let supervisor = Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            checkpoint_store,
            active_actors: Arc::new(RwLock::new(HashMap::new())),
            supervisor_tx,
            dead_letters: Arc::new(RwLock::new(Vec::new())),
            total_restarts: Arc::new(AtomicU64::new(0)),
        };

        supervisor.start_supervisor_event_loop(supervisor_rx);
        supervisor
    }

    /// Initialize an in-memory supervisor for testing and lightweight sessions
    pub fn in_memory(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let store = Arc::new(ActorCheckpointStore::in_memory()?);
        Ok(Self::new(workspace_root, store))
    }

    /// Spawn a new autonomous agent actor with an isolated mailbox
    pub async fn spawn_actor(&self, actor_id: &str) -> Result<mpsc::Sender<ActorMessage>> {
        let mut actors = self.active_actors.write().await;

        if let Some(existing) = actors.get(actor_id) {
            if existing.is_alive() {
                return Ok(existing.mailbox_tx.clone());
            }
        }

        let (mailbox_tx, mailbox_rx) = mpsc::channel(DEFAULT_ACTOR_MAILBOX_CAPACITY);
        let status = Arc::new(RwLock::new(ActorHealthStatus::Idle));
        let active_goal = Arc::new(RwLock::new(None));
        let current_step = Arc::new(AtomicUsize::new(0));
        let last_heartbeat_ms = Arc::new(AtomicU64::new(Utc::now().timestamp_millis() as u64));
        let messages_sent = Arc::new(AtomicU64::new(0));

        let actor = AgentActor::new(
            actor_id,
            mailbox_rx,
            self.supervisor_tx.clone(),
            self.checkpoint_store.clone(),
        );

        let actor_id_str = actor_id.to_string();
        let status_clone = status.clone();
        let supervisor_tx_clone = self.supervisor_tx.clone();

        // Spawn isolated Tokio task with panic safety
        let join_handle = tokio::spawn(async move {
            *status_clone.write().await = ActorHealthStatus::Running;
            let result = std::panic::AssertUnwindSafe(actor.run()).catch_unwind().await;

            if let Err(panic_err) = result {
                let panic_msg = if let Some(s) = panic_err.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_err.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown actor panic".to_string()
                };

                *status_clone.write().await = ActorHealthStatus::Panicked(panic_msg.clone());
                let _ = supervisor_tx_clone.send(SupervisorEvent::ActorPanicked {
                    actor_id: actor_id_str,
                    error_message: panic_msg,
                }).await;
            } else {
                *status_clone.write().await = ActorHealthStatus::Idle;
            }
        });

        let handle = Arc::new(ActorHandle {
            actor_id: actor_id.to_string(),
            mailbox_tx: mailbox_tx.clone(),
            status,
            active_goal,
            current_step,
            last_heartbeat_ms,
            messages_sent,
            join_handle: Arc::new(RwLock::new(Some(join_handle))),
        });

        actors.insert(actor_id.to_string(), handle);
        Ok(mailbox_tx)
    }

    /// Dispatch message to an actor's mailbox
    pub async fn send_to_actor(&self, actor_id: &str, msg: ActorMessage) -> Result<()> {
        let actors = self.active_actors.read().await;
        if let Some(handle) = actors.get(actor_id) {
            handle.messages_sent.fetch_add(1, Ordering::Relaxed);
            handle.mailbox_tx.send(msg).await
                .map_err(|e| HgbError::Ipc(format!("Failed to send to actor {}: {}", actor_id, e)))?;
            Ok(())
        } else {
            Err(HgbError::NotFound(format!("Actor '{}' not found in supervisor registry", actor_id)))
        }
    }

    /// Route cross-agent message from one actor to another
    pub async fn route_cross_agent_message(
        &self,
        from_actor_id: &str,
        to_actor_id: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let actors = self.active_actors.read().await;
        if let Some(target) = actors.get(to_actor_id) {
            if target.is_alive() {
                let msg = ActorMessage::DeliverMessage {
                    from_actor_id: from_actor_id.to_string(),
                    payload,
                };
                target.mailbox_tx.send(msg).await
                    .map_err(|e| HgbError::Ipc(format!("Failed to deliver message to {}: {}", to_actor_id, e)))?;
                return Ok(());
            }
        }

        // Target actor unreachable or dead: record in dead letter queue
        let mut dl = self.dead_letters.write().await;
        dl.push(DeadLetterRecord {
            from_actor_id: from_actor_id.to_string(),
            to_actor_id: to_actor_id.to_string(),
            payload,
            reason: format!("Target actor '{}' unavailable or dead", to_actor_id),
            timestamp_ms: Utc::now().timestamp_millis(),
        });

        Ok(())
    }

    /// Request atomic checkpointing of a specific actor's state
    pub async fn checkpoint_actor(&self, actor_id: &str) -> Result<()> {
        self.send_to_actor(actor_id, ActorMessage::StateCheckpointRequest).await
    }

    /// Request checkpoints across all registered actors
    pub async fn checkpoint_all(&self) -> Result<usize> {
        let actors = self.active_actors.read().await;
        let mut count = 0;
        for (id, handle) in actors.iter() {
            if handle.is_alive() {
                if handle.mailbox_tx.send(ActorMessage::StateCheckpointRequest).await.is_ok() {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Gracefully terminate an actor
    pub async fn terminate_actor(&self, actor_id: &str, reason: &str) -> Result<()> {
        self.send_to_actor(actor_id, ActorMessage::Terminate { reason: reason.to_string() }).await
    }

    /// Terminate all running actors
    pub async fn terminate_all(&self, reason: &str) -> Result<usize> {
        let actors = self.active_actors.read().await;
        let mut count = 0;
        for (_, handle) in actors.iter() {
            if handle.is_alive() {
                if handle.mailbox_tx.send(ActorMessage::Terminate { reason: reason.to_string() }).await.is_ok() {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Get real-time health status of a specific actor
    pub async fn get_actor_health(&self, actor_id: &str) -> Option<ActorHealthStatus> {
        let actors = self.active_actors.read().await;
        if let Some(handle) = actors.get(actor_id) {
            Some(handle.status.read().await.clone())
        } else {
            None
        }
    }

    /// List status summaries for all registered actors
    pub async fn list_active_actors(&self) -> Vec<ActorStatusSummary> {
        let actors = self.active_actors.read().await;
        let mut list = Vec::new();

        for (id, handle) in actors.iter() {
            list.push(ActorStatusSummary {
                actor_id: id.clone(),
                status: handle.status.read().await.clone(),
                active_goal: handle.active_goal.read().await.clone(),
                step_index: handle.current_step.load(Ordering::Relaxed),
                last_heartbeat_ms: handle.last_heartbeat_ms.load(Ordering::Relaxed),
                messages_processed: handle.messages_sent.load(Ordering::Relaxed),
                checkpoints_saved: 0,
            });
        }

        list
    }

    /// Self-Healing Recovery: On daemon boot or thermal restart, detect incomplete goals
    /// and resume execution precisely where it was interrupted.
    pub async fn resume_incomplete_goals(&self) -> Result<Vec<String>> {
        let incomplete_checkpoints = self.checkpoint_store.list_incomplete_goals()?;
        let mut resumed_goals = Vec::new();

        for cp in incomplete_checkpoints {
            let actor_id = cp.actor_id.clone();
            let goal_id = cp.goal_id.clone();
            let step = cp.step_index;

            if let Ok(snapshot) = cp.parse_snapshot() {
                // Spawn actor restored with its state snapshot
                let (mailbox_tx, mailbox_rx) = mpsc::channel(DEFAULT_ACTOR_MAILBOX_CAPACITY);
                let status = Arc::new(RwLock::new(ActorHealthStatus::Running));
                let active_goal = Arc::new(RwLock::new(Some(goal_id.clone())));
                let current_step = Arc::new(AtomicUsize::new(step));
                let last_heartbeat_ms = Arc::new(AtomicU64::new(Utc::now().timestamp_millis() as u64));
                let messages_sent = Arc::new(AtomicU64::new(0));

                let actor = AgentActor::restore_from_snapshot(
                    &actor_id,
                    mailbox_rx,
                    self.supervisor_tx.clone(),
                    self.checkpoint_store.clone(),
                    snapshot,
                );

                let actor_id_str = actor_id.clone();
                let status_clone = status.clone();
                let supervisor_tx_clone = self.supervisor_tx.clone();

                let join_handle = tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(actor.run()).catch_unwind().await;
                    if let Err(panic_err) = result {
                        let panic_msg = if let Some(s) = panic_err.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "Panic during resumed execution".to_string()
                        };
                        *status_clone.write().await = ActorHealthStatus::Panicked(panic_msg.clone());
                        let _ = supervisor_tx_clone.send(SupervisorEvent::ActorPanicked {
                            actor_id: actor_id_str,
                            error_message: panic_msg,
                        }).await;
                    }
                });

                let handle = Arc::new(ActorHandle {
                    actor_id: actor_id.clone(),
                    mailbox_tx,
                    status,
                    active_goal,
                    current_step,
                    last_heartbeat_ms,
                    messages_sent,
                    join_handle: Arc::new(RwLock::new(Some(join_handle))),
                });

                let mut actors = self.active_actors.write().await;
                actors.insert(actor_id.clone(), handle);
                resumed_goals.push(format!("{}:{}@step_{}", actor_id, goal_id, step));
            }
        }

        Ok(resumed_goals)
    }

    /// Background event loop processing supervisor events (heartbeats, routing, panics)
    fn start_supervisor_event_loop(&self, mut supervisor_rx: mpsc::Receiver<SupervisorEvent>) {
        let active_actors = self.active_actors.clone();
        let dead_letters = self.dead_letters.clone();
        let total_restarts = self.total_restarts.clone();

        tokio::spawn(async move {
            while let Some(event) = supervisor_rx.recv().await {
                match event {
                    SupervisorEvent::Heartbeat { actor_id, step_index, status } => {
                        let actors = active_actors.read().await;
                        if let Some(handle) = actors.get(&actor_id) {
                            handle.current_step.store(step_index, Ordering::Relaxed);
                            handle.last_heartbeat_ms.store(Utc::now().timestamp_millis() as u64, Ordering::Relaxed);
                            let mut st = handle.status.write().await;
                            if *st != ActorHealthStatus::Running {
                                *st = ActorHealthStatus::Running;
                            }
                        }
                    }

                    SupervisorEvent::RouteMessage { from_actor_id, to_actor_id, payload } => {
                        let actors = active_actors.read().await;
                        let delivered = if let Some(target) = actors.get(&to_actor_id) {
                            if target.is_alive() {
                                let msg = ActorMessage::DeliverMessage {
                                    from_actor_id: from_actor_id.clone(),
                                    payload: payload.clone(),
                                };
                                target.mailbox_tx.send(msg).await.is_ok()
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if !delivered {
                            let mut dl = dead_letters.write().await;
                            dl.push(DeadLetterRecord {
                                from_actor_id,
                                to_actor_id: to_actor_id.clone(),
                                payload,
                                reason: format!("Could not route to actor '{}'", to_actor_id),
                                timestamp_ms: Utc::now().timestamp_millis(),
                            });
                        }
                    }

                    SupervisorEvent::ActorCompleted { actor_id, goal_id } => {
                        let actors = active_actors.read().await;
                        if let Some(handle) = actors.get(&actor_id) {
                            *handle.status.write().await = ActorHealthStatus::Idle;
                            *handle.active_goal.write().await = None;
                        }
                    }

                    SupervisorEvent::ActorPanicked { actor_id, error_message } => {
                        total_restarts.fetch_add(1, Ordering::Relaxed);
                        let actors = active_actors.read().await;
                        if let Some(handle) = actors.get(&actor_id) {
                            *handle.status.write().await = ActorHealthStatus::Panicked(error_message);
                        }
                    }
                }
            }
        });
    }

    /// Access checkpoint store reference
    pub fn checkpoint_store(&self) -> &Arc<ActorCheckpointStore> {
        &self.checkpoint_store
    }

    /// Access dead letters
    pub async fn get_dead_letters(&self) -> Vec<DeadLetterRecord> {
        self.dead_letters.read().await.clone()
    }
}

// ============================================================================
// Unit Tests & Verification
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_actor_message_passing_and_cross_agent_routing() {
        let supervisor = ActorSupervisor::in_memory("/tmp/hgb_test").expect("supervisor init");

        // Spawn Actor A and Actor B
        let tx_a = supervisor.spawn_actor("actor-alpha").await.expect("spawn alpha");
        let _tx_b = supervisor.spawn_actor("actor-beta").await.expect("spawn beta");

        // Start goals on both
        supervisor.send_to_actor("actor-alpha", ActorMessage::StartGoal {
            goal_id: "goal-a".into(),
            instruction: "Extract AST nodes".into(),
        }).await.expect("start goal a");

        supervisor.send_to_actor("actor-beta", ActorMessage::StartGoal {
            goal_id: "goal-b".into(),
            instruction: "Verify AST bounds".into(),
        }).await.expect("start goal b");

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Alpha sends cross-agent message to Beta
        let payload = serde_json::json!({
            "ast_node_count": 42,
            "signature": "fn calculate() -> bool"
        });

        tx_a.send(ActorMessage::MessagePass {
            to_actor_id: "actor-beta".into(),
            payload: payload.clone(),
        }).await.expect("send message pass");

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify status of both actors
        let status_a = supervisor.get_actor_health("actor-alpha").await.expect("status a");
        let status_b = supervisor.get_actor_health("actor-beta").await.expect("status b");
        assert_eq!(status_a, ActorHealthStatus::Running);
        assert_eq!(status_b, ActorHealthStatus::Running);

        // Terminate cleanly
        supervisor.terminate_all("test completed").await.expect("terminate all");
    }

    #[tokio::test]
    async fn test_panic_isolation_without_crashing_daemon() {
        let supervisor = ActorSupervisor::in_memory("/tmp/hgb_test").expect("supervisor init");

        // Spawn a normal actor and a chaotic actor
        let _tx_normal = supervisor.spawn_actor("actor-stable").await.expect("spawn stable");
        let tx_chaotic = supervisor.spawn_actor("actor-chaotic").await.expect("spawn chaotic");

        supervisor.send_to_actor("actor-stable", ActorMessage::StartGoal {
            goal_id: "stable-01".into(),
            instruction: "Run stable task".into(),
        }).await.unwrap();

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(supervisor.get_actor_health("actor-stable").await.unwrap(), ActorHealthStatus::Running);

        // Inject simulated panic into chaotic actor by dropping or aborting its task
        // We simulate a task failure
        {
            let actors = supervisor.active_actors.read().await;
            if let Some(handle) = actors.get("actor-chaotic") {
                let jh_lock = handle.join_handle.read().await;
                if let Some(ref jh) = *jh_lock {
                    jh.abort();
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Stable actor MUST continue running without interruption!
        let stable_status = supervisor.get_actor_health("actor-stable").await.unwrap();
        assert_eq!(stable_status, ActorHealthStatus::Running, "Stable actor must NOT crash when sibling panics!");

        supervisor.terminate_all("panic test done").await.unwrap();
    }

    #[tokio::test]
    async fn test_persistent_checkpoint_and_resume_incomplete_goals() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(ActorCheckpointStore::new(temp_dir.path().join("checkpoints.db")).unwrap());

        // Phase 1: Supervisor runs, actor checkpoints at step 2, interrupted
        {
            let supervisor = ActorSupervisor::new(temp_dir.path(), store.clone());
            let _tx = supervisor.spawn_actor("actor-interrupted").await.unwrap();

            supervisor.send_to_actor("actor-interrupted", ActorMessage::StartGoal {
                goal_id: "goal-recovery".into(),
                instruction: "Repair buffer overflow".into(),
            }).await.unwrap();

            supervisor.send_to_actor("actor-interrupted", ActorMessage::ExecuteStep {
                step_index: 1,
                action: "Analyze memory leak".into(),
                payload: None,
            }).await.unwrap();

            supervisor.send_to_actor("actor-interrupted", ActorMessage::ExecuteStep {
                step_index: 2,
                action: "Patch buffer allocation".into(),
                payload: Some(serde_json::json!({"alloc_bytes": 1024})),
            }).await.unwrap();

            tokio::time::sleep(Duration::from_millis(60)).await;
            // Interruption simulation: supervisor drops
        }

        // Phase 2: Fresh supervisor boots up, scans checkpoints, resumes execution at step 2
        {
            let rebooted_supervisor = ActorSupervisor::new(temp_dir.path(), store.clone());
            let resumed = rebooted_supervisor.resume_incomplete_goals().await.expect("resume goals");

            assert_eq!(resumed.len(), 1, "Must resume exactly 1 incomplete goal");
            assert!(resumed[0].contains("actor-interrupted:goal-recovery@step_2"));

            tokio::time::sleep(Duration::from_millis(30)).await;
            let health = rebooted_supervisor.get_actor_health("actor-interrupted").await;
            assert_eq!(health, Some(ActorHealthStatus::Running));

            // Continue to step 3 and complete
            rebooted_supervisor.send_to_actor("actor-interrupted", ActorMessage::ExecuteStep {
                step_index: 3,
                action: "Verify unit test pass".into(),
                payload: None,
            }).await.unwrap();

            store.mark_goal_completed("actor-interrupted", "goal-recovery").unwrap();

            rebooted_supervisor.terminate_all("test done").await.unwrap();
        }
    }
}
