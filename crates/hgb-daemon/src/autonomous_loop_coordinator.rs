//! # Autonomous Loop Coordinator & Shared Lease Arbiter
//!
//! Synchronizes all background autonomous feedback, compilation, PGO, evolutionary,
//! and self-healing loops across `hgb-daemon`. Enforces mutually exclusive workspace
//! mutations to prevent concurrent compiler invocations, race conditions on target files,
//! and hardware governor contention.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

#[derive(Error, Debug)]
pub enum LoopCoordinatorError {
    #[error("Workspace mutation lease busy, held by: {0}")]
    LeaseBusy(String),
    #[error("Lease acquisition timed out after {0:?}")]
    Timeout(Duration),
}

/// Category of autonomous loop requesting exclusive workspace or hardware mutation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopSubsystem {
    CompilerHealer,
    EvolutionaryOptimizer,
    PgoBoltRewriter,
    ChaosInoculator,
    HardwareTuner,
    StacktraceSelfHealer,
}

impl LoopSubsystem {
    pub fn name(&self) -> &'static str {
        match self {
            Self::CompilerHealer => "CompilerHealer",
            Self::EvolutionaryOptimizer => "EvolutionaryOptimizer",
            Self::PgoBoltRewriter => "PgoBoltRewriter",
            Self::ChaosInoculator => "ChaosInoculator",
            Self::HardwareTuner => "HardwareTuner",
            Self::StacktraceSelfHealer => "StacktraceSelfHealer",
        }
    }
}

/// Metadata describing an active or historical lease
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseReport {
    pub subsystem: String,
    pub acquired_at_ms: u64,
    pub duration_ms: u64,
    pub description: String,
    pub released_cleanly: bool,
}

#[derive(Debug, Clone)]
struct ActiveLeaseInfo {
    subsystem: LoopSubsystem,
    acquired_at: Instant,
    description: String,
}

/// RAII Guard representing exclusive ownership of workspace mutation rights
pub struct AutonomousLease {
    _guard: OwnedMutexGuard<()>,
    subsystem: LoopSubsystem,
    start_time: Instant,
    description: String,
    coordinator: Arc<AutonomousLoopCoordinatorInner>,
}

impl Drop for AutonomousLease {
    fn drop(&mut self) {
        let duration_ms = self.start_time.elapsed().as_millis() as u64;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let report = LeaseReport {
            subsystem: self.subsystem.name().to_string(),
            acquired_at_ms: now_ms.saturating_sub(duration_ms),
            duration_ms,
            description: self.description.clone(),
            released_cleanly: true,
        };

        let inner = Arc::clone(&self.coordinator);
        tokio::spawn(async move {
            let mut active = inner.active_lease.write().await;
            *active = None;
            let mut history = inner.history.write().await;
            if history.len() >= 100 {
                history.pop_front();
            }
            history.push_back(report);
        });

        tracing::debug!(
            subsystem = self.subsystem.name(),
            duration_ms,
            "Released autonomous workspace mutation lease"
        );
    }
}

struct AutonomousLoopCoordinatorInner {
    workspace_lock: Arc<Mutex<()>>,
    active_lease: RwLock<Option<ActiveLeaseInfo>>,
    history: RwLock<VecDeque<LeaseReport>>,
}

/// Shared coordinator ensuring mutually exclusive workspace mutations
#[derive(Clone)]
pub struct AutonomousLoopCoordinator {
    inner: Arc<AutonomousLoopCoordinatorInner>,
}

impl AutonomousLoopCoordinator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AutonomousLoopCoordinatorInner {
                workspace_lock: Arc::new(Mutex::new(())),
                active_lease: RwLock::new(None),
                history: RwLock::new(VecDeque::with_capacity(100)),
            }),
        }
    }

    /// Try to acquire exclusive workspace mutation lease without blocking
    pub async fn try_acquire_lease(
        &self,
        subsystem: LoopSubsystem,
        description: impl Into<String>,
    ) -> Result<AutonomousLease, LoopCoordinatorError> {
        let desc = description.into();
        let guard = match self.inner.workspace_lock.clone().try_lock_owned() {
            Ok(g) => g,
            Err(_) => {
                let current = self.inner.active_lease.read().await;
                let holder = current
                    .as_ref()
                    .map(|c| format!("{}: {}", c.subsystem.name(), c.description))
                    .unwrap_or_else(|| "Unknown".to_string());
                return Err(LoopCoordinatorError::LeaseBusy(holder));
            }
        };

        let now = Instant::now();
        {
            let mut active = self.inner.active_lease.write().await;
            *active = Some(ActiveLeaseInfo {
                subsystem,
                acquired_at: now,
                description: desc.clone(),
            });
        }

        tracing::info!(
            subsystem = subsystem.name(),
            desc = %desc,
            "Acquired autonomous workspace mutation lease"
        );

        Ok(AutonomousLease {
            _guard: guard,
            subsystem,
            start_time: now,
            description: desc,
            coordinator: Arc::clone(&self.inner),
        })
    }

    /// Acquire exclusive workspace mutation lease with a timeout
    pub async fn acquire_lease_timeout(
        &self,
        subsystem: LoopSubsystem,
        description: impl Into<String>,
        timeout: Duration,
    ) -> Result<AutonomousLease, LoopCoordinatorError> {
        let desc = description.into();
        let guard_fut = self.inner.workspace_lock.clone().lock_owned();
        let guard = match tokio::time::timeout(timeout, guard_fut).await {
            Ok(g) => g,
            Err(_) => return Err(LoopCoordinatorError::Timeout(timeout)),
        };

        let now = Instant::now();
        {
            let mut active = self.inner.active_lease.write().await;
            *active = Some(ActiveLeaseInfo {
                subsystem,
                acquired_at: now,
                description: desc.clone(),
            });
        }

        tracing::info!(
            subsystem = subsystem.name(),
            desc = %desc,
            "Acquired autonomous workspace mutation lease after waiting"
        );

        Ok(AutonomousLease {
            _guard: guard,
            subsystem,
            start_time: now,
            description: desc,
            coordinator: Arc::clone(&self.inner),
        })
    }

    /// Retrieve summary status of coordinator leases
    pub async fn status_report(&self) -> serde_json::Value {
        let active = self.inner.active_lease.read().await;
        let history = self.inner.history.read().await;

        let active_val = active.as_ref().map(|a| {
            serde_json::json!({
                "subsystem": a.subsystem.name(),
                "duration_ms": a.acquired_at.elapsed().as_millis() as u64,
                "description": a.description,
            })
        });

        serde_json::json!({
            "is_locked": active.is_some(),
            "active_lease": active_val,
            "recent_leases_count": history.len(),
            "recent_leases": history.iter().rev().take(10).collect::<Vec<_>>(),
        })
    }
}

impl Default for AutonomousLoopCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
