//! # Pillar 4: Autonomous Agent Loop Expansion (`autonomous_daemon_loop.rs`)
//!
//! Resident daemon service embedded in `hgbd` codifying Masterclass Chapter 8
//! (`docs/masterclass/ch8_autonomous_loops.py`).
//!
//! ## Core Responsibilities:
//! 1. **Resident Autonomous Daemon Service**:
//!    Maintains persistent agent loop execution state across daemon lifecycles.
//! 2. **Multi-Step Execution Graph Orchestration**:
//!    Receives high-level user intents, constructs and persists execution graphs in `.hgb/autonomous_loops.db`.
//! 3. **Safe Boundary Multi-Tier Sandbox Routing**:
//!    Dispatches across Tier 1 (fast WASM kernels <2ms) and Tier 2 (Firecracker / Landlock LSM + `/dev/shm` RAM CoW OverlaySandbox).
//! 4. **Closed-Loop AST Self-Healing**:
//!    Intercepts failure diagnostics, queries local LLMs / deterministic AST repair rules,
//!    speculatively validates in `/dev/shm` RAM CoW overlay, and retries up to `max_retries` until First-Green execution.
//! 5. **Cryptographic BLAKE3 Consensus Certification**:
//!    Seals execution receipt and recovery story into `AutonomousConsensusCertificate`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use hgb_core::error::Result;
use hgb_core::event_bus::DistributedEventBus;
use hgb_core::autonomous_wasm_plugin_healer::{
    AutonomousWasmPluginHealer, ToolRequirement,
};
use hgb_nextgen::ch8_autonomous_agent_loop::{
    AutonomousConsensusCertificate, Ch8AutonomousAgentLoop,
    WorkflowRunRecord, WorkflowStatus,
};

/// Telemetry status snapshot of resident autonomous daemon loop service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomousDaemonLoopStatusReport {
    pub active: bool,
    pub workspace_root: String,
    pub db_path: String,
    pub total_workflows_executed: u64,
    pub total_self_heals: u64,
    pub active_workflows: usize,
    pub first_green_success_rate: f64,
    pub recent_workflows: Vec<WorkflowRunRecord>,
}

/// Request envelope accepted by autonomous daemon loop service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AutonomousDaemonRequest {
    ExecuteWorkflow {
        intent: String,
        max_retries: Option<usize>,
    },
    WorkflowStatus {
        workflow_id: Option<String>,
    },
    ListWorkflows {
        limit: Option<usize>,
    },
    WorkflowSteps {
        workflow_id: String,
    },
    WorkflowCertificate {
        workflow_id: String,
    },
}

/// Resident daemon service managing autonomous agent loops in `hgbd`
pub struct AutonomousDaemonLoopService {
    workspace_root: PathBuf,
    agent_loop: Arc<Ch8AutonomousAgentLoop>,
    active_workflows: Arc<RwLock<HashMap<String, String>>>,
    total_executed: Arc<AtomicU64>,
    total_healed: Arc<AtomicU64>,
    event_bus: Option<Arc<DistributedEventBus>>,
    wasm_healer: Arc<RwLock<Option<Arc<AutonomousWasmPluginHealer>>>>,
    ipc_governor: Arc<crate::dynatune_ipc_governor::DynatuneIpcGovernor>,
}

impl AutonomousDaemonLoopService {
    /// Initialize resident service for a given workspace root (`.hgb/autonomous_loops.db`)
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let ws = workspace_root.as_ref().to_path_buf();
        let agent_loop = Arc::new(Ch8AutonomousAgentLoop::new(&ws)?);

        Ok(Self {
            workspace_root: ws,
            agent_loop,
            active_workflows: Arc::new(RwLock::new(HashMap::new())),
            total_executed: Arc::new(AtomicU64::new(0)),
            total_healed: Arc::new(AtomicU64::new(0)),
            event_bus: None,
            wasm_healer: Arc::new(RwLock::new(None)),
            ipc_governor: Arc::new(crate::dynatune_ipc_governor::DynatuneIpcGovernor::new()),
        })
    }

    /// Open with custom database path
    pub fn with_db_path(workspace_root: impl AsRef<Path>, db_path: impl AsRef<Path>) -> Result<Self> {
        let ws = workspace_root.as_ref().to_path_buf();
        let agent_loop = Arc::new(Ch8AutonomousAgentLoop::with_db_path(&ws, db_path)?);

        Ok(Self {
            workspace_root: ws,
            agent_loop,
            active_workflows: Arc::new(RwLock::new(HashMap::new())),
            total_executed: Arc::new(AtomicU64::new(0)),
            total_healed: Arc::new(AtomicU64::new(0)),
            event_bus: None,
            wasm_healer: Arc::new(RwLock::new(None)),
            ipc_governor: Arc::new(crate::dynatune_ipc_governor::DynatuneIpcGovernor::new()),
        })
    }

    /// Fast in-memory service instance for isolated testing
    pub fn in_memory(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let ws = workspace_root.as_ref().to_path_buf();
        let agent_loop = Arc::new(Ch8AutonomousAgentLoop::in_memory(&ws)?);

        Ok(Self {
            workspace_root: ws,
            agent_loop,
            active_workflows: Arc::new(RwLock::new(HashMap::new())),
            total_executed: Arc::new(AtomicU64::new(0)),
            total_healed: Arc::new(AtomicU64::new(0)),
            event_bus: None,
            wasm_healer: Arc::new(RwLock::new(None)),
            ipc_governor: Arc::new(crate::dynatune_ipc_governor::DynatuneIpcGovernor::new()),
        })
    }

    /// Attach event bus for broadcast notifications
    pub fn with_event_bus(mut self, event_bus: Arc<DistributedEventBus>) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    /// Attach autonomous WASM plugin healer
    pub fn with_wasm_healer(self, healer: Arc<AutonomousWasmPluginHealer>) -> Self {
        *self.wasm_healer.write().unwrap() = Some(healer);
        self
    }

    /// Set runtime reference to autonomous WASM plugin healer
    pub fn set_wasm_healer(&self, healer: Arc<AutonomousWasmPluginHealer>) {
        *self.wasm_healer.write().unwrap() = Some(healer);
    }

    /// Access reference to autonomous WASM plugin healer
    pub fn wasm_healer(&self) -> Option<Arc<AutonomousWasmPluginHealer>> {
        self.wasm_healer.read().unwrap().clone()
    }

    /// Access reference to Dynatune IPC Governor
    pub fn ipc_governor(&self) -> &Arc<crate::dynatune_ipc_governor::DynatuneIpcGovernor> {
        &self.ipc_governor
    }

    /// Access underlying autonomous agent loop orchestrator
    pub fn agent_loop(&self) -> &Arc<Ch8AutonomousAgentLoop> {
        &self.agent_loop
    }

    /// Execute workflow with dynamic tool synthesis and hot-swap injection
    pub fn execute_workflow_with_tools(
        &self,
        intent: &str,
        required_tools: &[ToolRequirement],
        max_retries: usize,
    ) -> Result<AutonomousConsensusCertificate> {
        if let Some(ref healer) = *self.wasm_healer.read().unwrap() {
            for req in required_tools {
                if !healer.hot_pool().has_plugin(&req.name) {
                    let _ = healer.intercept_and_heal(req.clone());
                }
            }
        }
        self.execute_workflow(intent, max_retries)
    }

    /// Execute high-level user intent through autonomous multi-step planning and self-healing
    pub fn execute_workflow(&self, intent: &str, max_retries: usize) -> Result<AutonomousConsensusCertificate> {
        let _ipc_lease = self.ipc_governor.acquire_lease("autonomous_daemon_loop");
        let _governor_guard = hgb_core::dynatune_subsystem_fuse::DynatuneWorkloadGuard::acquire_for_workspace(
            &self.workspace_root,
            "autonomous_daemon_loop",
        );

        let loop_orchestrator = (*self.agent_loop).clone().with_max_retries(max_retries);

        // Publish event if bus configured
        if let Some(ref bus) = self.event_bus {
            let _ = bus.publish(
                "autonomous:workflow:start",
                serde_json::json!({
                    "intent": intent,
                    "max_retries": max_retries,
                    "timestamp": Utc::now().to_rfc3339(),
                }).to_string(),
            );
        }

        let cert = loop_orchestrator.execute_intent(intent)?;

        self.total_executed.fetch_add(1, Ordering::SeqCst);
        if cert.self_healing_iterations > 0 {
            self.total_healed.fetch_add(cert.self_healing_iterations as u64, Ordering::SeqCst);
        }

        if let Some(ref bus) = self.event_bus {
            let _ = bus.publish(
                "autonomous:workflow:completed",
                serde_json::json!({
                    "workflow_id": cert.workflow_id,
                    "intent": cert.intent,
                    "blake3_hash": cert.blake3_hash,
                    "first_green_verified": cert.first_green_verified,
                    "self_healing_iterations": cert.self_healing_iterations,
                    "timestamp": cert.signed_timestamp,
                }).to_string(),
            );
        }

        Ok(cert)
    }

    /// Query status of autonomous daemon service or specific workflow
    pub fn query_status(&self, workflow_id: Option<&str>) -> Result<serde_json::Value> {
        if let Some(wid) = workflow_id {
            let workflow = self.agent_loop.query_workflow(wid)?;
            let steps = self.agent_loop.query_workflow_steps(wid)?;
            let cert = self.agent_loop.query_certificate(wid)?;

            Ok(serde_json::json!({
                "workflow": workflow,
                "steps": steps,
                "certificate": cert,
            }))
        } else {
            let recent = self.agent_loop.list_workflows(10)?;
            let total_exec = self.total_executed.load(Ordering::SeqCst);
            let total_healed = self.total_healed.load(Ordering::SeqCst);
            let active_count = self.active_workflows.read().unwrap().len();

            let passed_count = recent.iter().filter(|w| w.status == WorkflowStatus::Completed || w.status == WorkflowStatus::FirstGreenAchieved).count();
            let rate = if !recent.is_empty() {
                (passed_count as f64 / recent.len() as f64) * 100.0
            } else {
                100.0
            };

            let report = AutonomousDaemonLoopStatusReport {
                active: true,
                workspace_root: self.workspace_root.display().to_string(),
                db_path: self.workspace_root.join(".hgb").join("autonomous_loops.db").display().to_string(),
                total_workflows_executed: total_exec,
                total_self_heals: total_healed,
                active_workflows: active_count,
                first_green_success_rate: rate,
                recent_workflows: recent,
            };

            Ok(serde_json::to_value(&report).unwrap_or_default())
        }
    }

    /// Handle structured `AutonomousDaemonRequest`
    pub fn handle_request(&self, req: AutonomousDaemonRequest) -> Result<serde_json::Value> {
        match req {
            AutonomousDaemonRequest::ExecuteWorkflow { intent, max_retries } => {
                let cert = self.execute_workflow(&intent, max_retries.unwrap_or(3))?;
                Ok(serde_json::to_value(&cert).unwrap_or_default())
            }
            AutonomousDaemonRequest::WorkflowStatus { workflow_id } => {
                self.query_status(workflow_id.as_deref())
            }
            AutonomousDaemonRequest::ListWorkflows { limit } => {
                let runs = self.agent_loop.list_workflows(limit.unwrap_or(20))?;
                Ok(serde_json::to_value(&runs).unwrap_or_default())
            }
            AutonomousDaemonRequest::WorkflowSteps { workflow_id } => {
                let steps = self.agent_loop.query_workflow_steps(&workflow_id)?;
                Ok(serde_json::to_value(&steps).unwrap_or_default())
            }
            AutonomousDaemonRequest::WorkflowCertificate { workflow_id } => {
                let cert = self.agent_loop.query_certificate(&workflow_id)?;
                Ok(serde_json::to_value(&cert).unwrap_or_default())
            }
        }
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_daemon_service_execute_and_query_status() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_dservice_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let service = AutonomousDaemonLoopService::in_memory(&temp_dir).expect("in memory daemon service");

        // Initial status query (no workflows yet)
        let initial_status = service.query_status(None).expect("query status");
        assert!(initial_status["active"].as_bool().unwrap());
        assert_eq!(initial_status["total_workflows_executed"].as_u64().unwrap(), 0);

        // Execute workflow via intent
        let cert = service.execute_workflow("add caching layer to auth service", 3)
            .expect("execute workflow");

        assert!(cert.first_green_verified);
        assert_eq!(cert.total_steps_executed, 4);
        assert!(!cert.blake3_hash.is_empty());

        // Query status for specific workflow ID
        let status_val = service.query_status(Some(&cert.workflow_id))
            .expect("query specific status");

        assert!(status_val["workflow"].is_object());
        assert_eq!(status_val["workflow"]["id"].as_str().unwrap(), cert.workflow_id);
        assert!(status_val["steps"].is_array());
        assert_eq!(status_val["steps"].as_array().unwrap().len(), 4);
        assert_eq!(status_val["certificate"]["blake3_hash"].as_str().unwrap(), cert.blake3_hash);

        // Global status query must reflect completed workflow
        let global_status = service.query_status(None).expect("query global status");
        assert_eq!(global_status["total_workflows_executed"].as_u64().unwrap(), 1);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_daemon_service_structured_request_handling() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_test_dreq_{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&temp_dir);

        let service = AutonomousDaemonLoopService::in_memory(&temp_dir).expect("in memory daemon service");

        let req = AutonomousDaemonRequest::ExecuteWorkflow {
            intent: "optimize vector search distance kernel".to_string(),
            max_retries: Some(4),
        };

        let res = service.handle_request(req).expect("handle request");
        assert!(res.is_object());
        let wid = res["workflow_id"].as_str().expect("workflow_id present");

        // Query steps
        let steps_req = AutonomousDaemonRequest::WorkflowSteps {
            workflow_id: wid.to_string(),
        };
        let steps_res = service.handle_request(steps_req).expect("steps request");
        assert!(steps_res.is_array());
        assert_eq!(steps_res.as_array().unwrap().len(), 4);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
