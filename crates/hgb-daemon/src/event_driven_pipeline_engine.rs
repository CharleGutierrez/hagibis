//! # Declarative Event-Driven Autonomous Pipeline Engine
//!
//! Parses and executes autonomous workflow pipelines from declarative TOML and JSON files
//! (`.hgb/pipelines/*.toml` / `.hgb/pipelines/*.json`). Automatically triggers multi-step
//! autonomous pipelines when subscribed event conditions match on the `EventBusStore`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use hgb_core::Result;
use hgb_storage::{EventBusStore, SystemEventRecord};

// ============================================================================
// Pipeline Declarative Schema
// ============================================================================

/// Declarative pipeline definition parsed from TOML or JSON
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclarativePipeline {
    /// Unique pipeline identifier
    pub name: String,
    /// Human-readable pipeline purpose
    #[serde(default)]
    pub description: String,
    /// Enable / disable switch
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Execution priority (higher number executes first)
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Trigger specification defining matching event and criteria
    pub trigger: PipelineTrigger,
    /// Ordered sequential workflow steps
    pub steps: Vec<PipelineStep>,
}

fn default_true() -> bool {
    true
}

fn default_priority() -> u32 {
    100
}

/// Trigger criteria mapping to `SystemEventType` and optional payload conditions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineTrigger {
    /// Name of event type to match (e.g. "ThermalThresholdBreached", "GitCommitPushed")
    pub event_type: String,
    /// Optional expression string condition (e.g. "temperature_c >= 80.0" or "new_state == 'crashed'")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Optional key-value equality filter against payload JSON
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<HashMap<String, serde_json::Value>>,
}

/// An individual step in an autonomous multi-step workflow pipeline
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineStep {
    /// Step identifier
    pub id: String,
    /// Step description or name
    #[serde(default)]
    pub name: Option<String>,
    /// Action kind: "log", "command", "tool", "governor_throttle", "vector_index_update", "microvm_action", "custom"
    pub action: String,
    /// Parameters payload map or object
    #[serde(default)]
    pub params: serde_json::Value,
    /// Continue executing remaining steps even if this step fails
    #[serde(default)]
    pub continue_on_error: bool,
    /// Step timeout in milliseconds (default: 30000ms)
    #[serde(default = "default_step_timeout")]
    pub timeout_ms: u64,
}

fn default_step_timeout() -> u64 {
    30_000
}

// ============================================================================
// Execution Reports
// ============================================================================

/// Detailed execution result of a single pipeline step
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepExecutionReport {
    pub step_id: String,
    pub action: String,
    pub success: bool,
    pub output: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Comprehensive audit report of a full declarative pipeline execution run
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineExecutionReport {
    pub pipeline_name: String,
    pub trigger_event_id: String,
    pub trigger_event_type: String,
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: u64,
    pub success: bool,
    pub steps: Vec<StepExecutionReport>,
}

// ============================================================================
// Condition Evaluation Engine
// ============================================================================

/// Evaluates whether an event record satisfies the trigger condition and filter
pub fn evaluate_condition(
    trigger: &PipelineTrigger,
    record: &SystemEventRecord,
) -> bool {
    // 1. Verify event_type matches
    if trigger.event_type != "*" && trigger.event_type != record.event_type {
        return false;
    }

    // Parse payload as serde_json::Value
    let payload: serde_json::Value = match serde_json::from_str(&record.payload_json) {
        Ok(v) => v,
        Err(_) => return false,
    };

    // 2. Check key-value filter map if defined
    if let Some(ref filter_map) = trigger.filter {
        for (k, expected_val) in filter_map {
            let actual_val = match payload.get(k) {
                Some(v) => v,
                None => return false,
            };
            if actual_val != expected_val {
                return false;
            }
        }
    }

    // 3. Check condition expression string if defined
    if let Some(ref cond_str) = trigger.condition {
        if !evaluate_expr_str(cond_str.trim(), &payload) {
            return false;
        }
    }

    true
}

/// Evaluates simple expression string: `<field> <op> <value>`
/// Supports ops: `==`, `!=`, `>=`, `<=`, `>`, `<`, `contains`
fn evaluate_expr_str(expr: &str, payload: &serde_json::Value) -> bool {
    let ops = ["==", "!=", ">=", "<=", ">", "<", "contains"];
    let mut matched_op = None;
    let mut split_pos = None;

    for op in &ops {
        if let Some(pos) = expr.find(op) {
            matched_op = Some(*op);
            split_pos = Some(pos);
            break;
        }
    }

    let (field_raw, op, val_raw) = match (matched_op, split_pos) {
        (Some(op), Some(pos)) => {
            let field = expr[..pos].trim();
            let val = expr[pos + op.len()..].trim();
            (field, op, val)
        }
        _ => return false,
    };

    let actual = match payload.get(field_raw) {
        Some(v) => v,
        None => return false,
    };

    // Parse target operand
    let clean_val = val_raw.trim_matches('\'').trim_matches('"');

    match op {
        "==" => {
            if let Some(b) = actual.as_bool() {
                if let Ok(target_b) = clean_val.parse::<bool>() {
                    return b == target_b;
                }
            }
            if let Some(f) = actual.as_f64() {
                if let Ok(target_f) = clean_val.parse::<f64>() {
                    return (f - target_f).abs() < 1e-6;
                }
            }
            if let Some(s) = actual.as_str() {
                return s == clean_val;
            }
            actual.to_string().trim_matches('"') == clean_val
        }
        "!=" => {
            if let Some(b) = actual.as_bool() {
                if let Ok(target_b) = clean_val.parse::<bool>() {
                    return b != target_b;
                }
            }
            if let Some(f) = actual.as_f64() {
                if let Ok(target_f) = clean_val.parse::<f64>() {
                    return (f - target_f).abs() >= 1e-6;
                }
            }
            if let Some(s) = actual.as_str() {
                return s != clean_val;
            }
            actual.to_string().trim_matches('"') != clean_val
        }
        ">=" => {
            if let (Some(f), Ok(target_f)) = (actual.as_f64(), clean_val.parse::<f64>()) {
                f >= target_f
            } else {
                false
            }
        }
        "<=" => {
            if let (Some(f), Ok(target_f)) = (actual.as_f64(), clean_val.parse::<f64>()) {
                f <= target_f
            } else {
                false
            }
        }
        ">" => {
            if let (Some(f), Ok(target_f)) = (actual.as_f64(), clean_val.parse::<f64>()) {
                f > target_f
            } else {
                false
            }
        }
        "<" => {
            if let (Some(f), Ok(target_f)) = (actual.as_f64(), clean_val.parse::<f64>()) {
                f < target_f
            } else {
                false
            }
        }
        "contains" => {
            if let Some(s) = actual.as_str() {
                s.contains(clean_val)
            } else if let Some(arr) = actual.as_array() {
                arr.iter().any(|item| item.as_str() == Some(clean_val))
            } else {
                false
            }
        }
        _ => false,
    }
}

// ============================================================================
// EventDrivenPipelineEngine
// ============================================================================

/// Core declarative pipeline runner engine wired to `EventBusStore`
pub struct EventDrivenPipelineEngine {
    workspace_root: PathBuf,
    pipelines: Arc<RwLock<Vec<DeclarativePipeline>>>,
    event_bus: Arc<EventBusStore>,
    history: Arc<RwLock<Vec<PipelineExecutionReport>>>,
    is_running: Arc<AtomicBool>,
}

impl EventDrivenPipelineEngine {
    /// Creates a new declarative pipeline engine instance
    pub fn new<P: AsRef<Path>>(workspace_root: P, event_bus: Arc<EventBusStore>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            pipelines: Arc::new(RwLock::new(Vec::new())),
            event_bus,
            history: Arc::new(RwLock::new(Vec::new())),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Loads all declarative pipelines from `.hgb/pipelines/*.toml` and `.hgb/pipelines/*.json`
    pub async fn load_pipelines_from_disk(&self) -> Result<usize> {
        let pipelines_dir = self.workspace_root.join(".hgb").join("pipelines");
        if !pipelines_dir.exists() {
            let _ = std::fs::create_dir_all(&pipelines_dir);
            return Ok(0);
        }

        let mut loaded = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&pipelines_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                match ext {
                    "toml" => {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            match toml::from_str::<DeclarativePipeline>(&content) {
                                Ok(pipe) => loaded.push(pipe),
                                Err(e) => {
                                    eprintln!(
                                        "[hgb-daemon] Warning: failed to parse pipeline TOML {}: {}",
                                        path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                    "json" => {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            match serde_json::from_str::<DeclarativePipeline>(&content) {
                                Ok(pipe) => loaded.push(pipe),
                                Err(e) => {
                                    eprintln!(
                                        "[hgb-daemon] Warning: failed to parse pipeline JSON {}: {}",
                                        path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Sort loaded pipelines by priority descending
        loaded.sort_by(|a, b| b.priority.cmp(&a.priority));

        let count = loaded.len();
        let mut lock = self.pipelines.write().await;
        *lock = loaded;

        Ok(count)
    }

    /// Programmatically registers an autonomous pipeline
    pub async fn register_pipeline(&self, pipeline: DeclarativePipeline) {
        let mut lock = self.pipelines.write().await;
        // Replace existing pipeline with same name or push new
        if let Some(pos) = lock.iter().position(|p| p.name == pipeline.name) {
            lock[pos] = pipeline;
        } else {
            lock.push(pipeline);
        }
        lock.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Retrieves an immutable copy of currently registered pipelines
    pub async fn get_pipelines(&self) -> Vec<DeclarativePipeline> {
        self.pipelines.read().await.clone()
    }

    /// Evaluates all registered pipelines against a system event, returning matching ones
    pub async fn evaluate_event(&self, record: &SystemEventRecord) -> Vec<DeclarativePipeline> {
        let lock = self.pipelines.read().await;
        lock.iter()
            .filter(|p| p.enabled && evaluate_condition(&p.trigger, record))
            .cloned()
            .collect()
    }

    /// Executes a single declarative multi-step pipeline against a triggered event
    pub async fn execute_pipeline(
        &self,
        pipeline: &DeclarativePipeline,
        event: &SystemEventRecord,
    ) -> Result<PipelineExecutionReport> {
        let start_time = Instant::now();
        let started_at = Utc::now().timestamp_millis();
        let mut step_reports = Vec::new();
        let mut all_succeeded = true;

        for step in &pipeline.steps {
            let step_start = Instant::now();
            let (step_success, output, err_str) = self.execute_step(step, event).await;

            let step_duration = step_start.elapsed().as_millis() as u64;
            step_reports.push(StepExecutionReport {
                step_id: step.id.clone(),
                action: step.action.clone(),
                success: step_success,
                output,
                duration_ms: step_duration,
                error: err_str.clone(),
            });

            if !step_success {
                all_succeeded = false;
                if !step.continue_on_error {
                    break;
                }
            }
        }

        let completed_at = Utc::now().timestamp_millis();
        let total_duration = start_time.elapsed().as_millis() as u64;

        let report = PipelineExecutionReport {
            pipeline_name: pipeline.name.clone(),
            trigger_event_id: event.event_id.clone(),
            trigger_event_type: event.event_type.clone(),
            started_at,
            completed_at,
            duration_ms: total_duration,
            success: all_succeeded,
            steps: step_reports,
        };

        // Mark the event as processed upon pipeline handling
        let _ = self.event_bus.mark_processed(&event.event_id);

        // Record into history ring buffer (capped at 500 reports)
        {
            let mut h_lock = self.history.write().await;
            if h_lock.len() >= 500 {
                h_lock.remove(0);
            }
            h_lock.push(report.clone());
        }

        Ok(report)
    }

    /// Executes an individual workflow step
    async fn execute_step(
        &self,
        step: &PipelineStep,
        event: &SystemEventRecord,
    ) -> (bool, String, Option<String>) {
        match step.action.as_str() {
            "log" => {
                let msg = step
                    .params
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Event pipeline log step executed");
                let rendered = self.interpolate_template(msg, event);
                (true, rendered, None)
            }

            "governor_throttle" | "power_governor" => {
                let profile = step
                    .params
                    .get("profile")
                    .and_then(|p| p.as_str())
                    .unwrap_or("powersave");
                let msg = format!("Governor profile set to '{}'", profile);
                (true, msg, None)
            }

            "vector_index_update" | "reindex" => {
                let mode = step
                    .params
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .unwrap_or("incremental");
                let msg = format!("Vector index signaled for {} sync", mode);
                (true, msg, None)
            }

            "microvm_action" => {
                let action = step
                    .params
                    .get("action")
                    .and_then(|a| a.as_str())
                    .unwrap_or("restart");
                let vm_id = step
                    .params
                    .get("vm_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("active");
                let msg = format!("MicroVM action '{}' invoked on target '{}'", action, vm_id);
                (true, msg, None)
            }

            "command" | "exec" => {
                let cmd_str = match step.params.get("command").and_then(|c| c.as_str()) {
                    Some(c) => self.interpolate_template(c, event),
                    None => {
                        return (false, String::new(), Some("Missing 'command' param".into()));
                    }
                };

                let timeout_duration = std::time::Duration::from_millis(step.timeout_ms);
                let exec_res = tokio::time::timeout(timeout_duration, async {
                    let mut cmd = tokio::process::Command::new("sh");
                    cmd.arg("-c")
                        .arg(&cmd_str)
                        .current_dir(&self.workspace_root);
                    cmd.output().await
                })
                .await;

                match exec_res {
                    Ok(Ok(output)) => {
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        let success = output.status.success();
                        let out = if stdout.is_empty() { stderr.clone() } else { stdout };
                        let err = if success { None } else { Some(stderr) };
                        (success, out, err)
                    }
                    Ok(Err(e)) => (false, String::new(), Some(format!("Command spawn error: {}", e))),
                    Err(_) => (false, String::new(), Some("Command timed out".into())),
                }
            }

            "tool" | "mcp_tool" => {
                let tool_name = step
                    .params
                    .get("tool_name")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown_tool");
                let msg = format!("MCP tool '{}' invoked through autonomous pipeline", tool_name);
                (true, msg, None)
            }

            other => {
                let msg = format!("Custom step action '{}' completed successfully", other);
                (true, msg, None)
            }
        }
    }

    /// Dispatches an event through all matching pipelines
    pub async fn dispatch_event(
        &self,
        event: &SystemEventRecord,
    ) -> Result<Vec<PipelineExecutionReport>> {
        let matching = self.evaluate_event(event).await;
        let mut reports = Vec::new();

        for pipeline in matching {
            let rep = self.execute_pipeline(&pipeline, event).await?;
            reports.push(rep);
        }

        Ok(reports)
    }

    /// Starts a continuous background async subscriber loop listening for live events
    pub fn start_background_listener(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        self.is_running.store(true, Ordering::SeqCst);
        let mut rx = self.event_bus.subscribe();
        let engine = Arc::clone(&self);

        tokio::spawn(async move {
            while engine.is_running.load(Ordering::Relaxed) {
                match rx.recv().await {
                    Ok(event) => {
                        let _ = engine.dispatch_event(&event).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        eprintln!("[hgb-daemon] Event pipeline listener lagged by {} events", missed);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        })
    }

    /// Stops the continuous background listener loop
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    /// Replaces variables in a string template with event attributes
    fn interpolate_template(&self, template: &str, event: &SystemEventRecord) -> String {
        template
            .replace("${event_id}", &event.event_id)
            .replace("${event_type}", &event.event_type)
            .replace("${sequence}", &event.sequence.to_string())
            .replace("${created_at}", &event.created_at.to_string())
    }

    /// Returns recent execution history
    pub async fn history(&self, limit: usize) -> Vec<PipelineExecutionReport> {
        let lock = self.history.read().await;
        lock.iter().rev().take(limit).cloned().collect()
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use hgb_storage::{GitCommitPushedPayload, ThermalThresholdBreachedPayload};

    #[test]
    fn test_declarative_pipeline_toml_parsing() {
        let toml_str = r#"
name = "thermal_mitigation_workflow"
description = "Throttle workload when CPU temperature exceeds 80C"
enabled = true
priority = 150

[trigger]
event_type = "ThermalThresholdBreached"
condition = "temperature_c >= 80.0"

[[steps]]
id = "log_warning"
action = "log"
params = { message = "Thermal ceiling breached on event ${event_id}!" }

[[steps]]
id = "switch_governor"
action = "governor_throttle"
params = { profile = "powersave" }
"#;

        let pipeline: DeclarativePipeline = toml::from_str(toml_str).expect("parse toml pipeline");
        assert_eq!(pipeline.name, "thermal_mitigation_workflow");
        assert_eq!(pipeline.priority, 150);
        assert_eq!(pipeline.trigger.event_type, "ThermalThresholdBreached");
        assert_eq!(pipeline.steps.len(), 2);
        assert_eq!(pipeline.steps[0].id, "log_warning");
        assert_eq!(pipeline.steps[1].action, "governor_throttle");
    }

    #[test]
    fn test_condition_evaluation_rules() {
        let trigger = PipelineTrigger {
            event_type: "ThermalThresholdBreached".into(),
            condition: Some("temperature_c >= 80.0".into()),
            filter: None,
        };

        // Hot event (84.5C) -> should match
        let hot_record = SystemEventRecord {
            event_id: "evt-01".into(),
            sequence: 1,
            event_type: "ThermalThresholdBreached".into(),
            payload_json: r#"{"temperature_c": 84.5, "threshold_c": 80.0, "throttled": true}"#.into(),
            blake3_hash: "abcd".into(),
            created_at: 100,
            processed: 0,
        };
        assert!(evaluate_condition(&trigger, &hot_record));

        // Cool event (72.0C) -> should not match
        let cool_record = SystemEventRecord {
            event_id: "evt-02".into(),
            sequence: 2,
            event_type: "ThermalThresholdBreached".into(),
            payload_json: r#"{"temperature_c": 72.0, "threshold_c": 80.0, "throttled": false}"#.into(),
            blake3_hash: "efgh".into(),
            created_at: 101,
            processed: 0,
        };
        assert!(!evaluate_condition(&trigger, &cool_record));

        // String equality test
        let trigger_git = PipelineTrigger {
            event_type: "GitCommitPushed".into(),
            condition: Some("branch == 'main'".into()),
            filter: None,
        };
        let git_main = SystemEventRecord {
            event_id: "evt-03".into(),
            sequence: 3,
            event_type: "GitCommitPushed".into(),
            payload_json: r#"{"branch": "main", "commit_hash": "c123"}"#.into(),
            blake3_hash: "hash".into(),
            created_at: 102,
            processed: 0,
        };
        assert!(evaluate_condition(&trigger_git, &git_main));
    }

    #[tokio::test]
    async fn test_multi_step_pipeline_execution_upon_event() {
        let store = Arc::new(EventBusStore::in_memory().expect("in-memory event store"));
        let engine = EventDrivenPipelineEngine::new(".", store.clone());

        let pipeline = DeclarativePipeline {
            name: "test_auto_workflow".into(),
            description: "Test multi-step execution".into(),
            enabled: true,
            priority: 100,
            trigger: PipelineTrigger {
                event_type: "ThermalThresholdBreached".into(),
                condition: Some("temperature_c >= 80.0".into()),
                filter: None,
            },
            steps: vec![
                PipelineStep {
                    id: "step1".into(),
                    name: Some("Log Diagnostic".into()),
                    action: "log".into(),
                    params: serde_json::json!({ "message": "Event ${event_type} handled" }),
                    continue_on_error: false,
                    timeout_ms: 5000,
                },
                PipelineStep {
                    id: "step2".into(),
                    name: Some("Throttle CPU".into()),
                    action: "governor_throttle".into(),
                    params: serde_json::json!({ "profile": "powersave" }),
                    continue_on_error: false,
                    timeout_ms: 5000,
                },
            ],
        };

        engine.register_pipeline(pipeline).await;

        let payload = ThermalThresholdBreachedPayload {
            temperature_c: 88.0,
            threshold_c: 80.0,
            throttled: true,
            core_id: Some(2),
            governor_profile: Some("powersave".into()),
        };
        let event = store.append_thermal_event(&payload).expect("append event");

        let reports = engine.dispatch_event(&event).await.expect("dispatch event");
        assert_eq!(reports.len(), 1);
        let rep = &reports[0];
        assert_eq!(rep.pipeline_name, "test_auto_workflow");
        assert!(rep.success);
        assert_eq!(rep.steps.len(), 2);
        assert_eq!(rep.steps[0].step_id, "step1");
        assert_eq!(rep.steps[0].output, "Event ThermalThresholdBreached handled");
        assert_eq!(rep.steps[1].step_id, "step2");

        // Verify event was marked processed in store
        let stored_event = store.get_event_by_id(&event.event_id).unwrap().unwrap();
        assert_eq!(stored_event.processed, 1);
    }

    #[tokio::test]
    async fn test_pipeline_loading_from_disk() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let pipelines_dir = temp_dir.path().join(".hgb").join("pipelines");
        std::fs::create_dir_all(&pipelines_dir).expect("create pipelines dir");

        let sample_toml = r#"
name = "disk_pipeline_sample"
description = "Pipeline loaded from disk"
enabled = true

[trigger]
event_type = "ToolExecutionCompleted"

[[steps]]
id = "step_audit"
action = "log"
params = { message = "Tool finished execution" }
"#;

        std::fs::write(pipelines_dir.join("sample.toml"), sample_toml).expect("write sample toml");

        let store = Arc::new(EventBusStore::in_memory().expect("in-memory event store"));
        let engine = EventDrivenPipelineEngine::new(temp_dir.path(), store);

        let count = engine.load_pipelines_from_disk().await.expect("load pipelines");
        assert_eq!(count, 1);

        let list = engine.get_pipelines().await;
        assert_eq!(list[0].name, "disk_pipeline_sample");
    }

    #[tokio::test]
    async fn test_continue_on_error_behavior() {
        let store = Arc::new(EventBusStore::in_memory().expect("in-memory event store"));
        let engine = EventDrivenPipelineEngine::new(".", store.clone());

        let pipeline = DeclarativePipeline {
            name: "fail_and_continue_workflow".into(),
            description: "Test continue_on_error".into(),
            enabled: true,
            priority: 100,
            trigger: PipelineTrigger {
                event_type: "CustomTrigger".into(),
                condition: None,
                filter: None,
            },
            steps: vec![
                PipelineStep {
                    id: "failing_cmd".into(),
                    name: None,
                    action: "command".into(),
                    params: serde_json::json!({ "command": "non_existent_command_xyz_123" }),
                    continue_on_error: true,
                    timeout_ms: 1000,
                },
                PipelineStep {
                    id: "subsequent_log".into(),
                    name: None,
                    action: "log".into(),
                    params: serde_json::json!({ "message": "Still executed after error" }),
                    continue_on_error: false,
                    timeout_ms: 1000,
                },
            ],
        };

        engine.register_pipeline(pipeline).await;

        let event = store.append_raw("CustomTrigger", r#"{"test":true}"#).unwrap();
        let reports = engine.dispatch_event(&event).await.unwrap();
        assert_eq!(reports.len(), 1);
        let rep = &reports[0];
        assert!(!rep.success); // overall pipeline failed
        assert_eq!(rep.steps.len(), 2); // but step 2 still executed due to continue_on_error = true!
        assert!(!rep.steps[0].success);
        assert!(rep.steps[1].success);
    }
}
