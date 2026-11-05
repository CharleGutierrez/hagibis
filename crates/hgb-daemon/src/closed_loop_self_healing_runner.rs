//! # Pillar 3: Advanced Closed-Loop Autonomous Reflection Runner (`closed_loop_self_healing_runner`)
//!
//! Systems-grade daemon service executing automated closed-loop reflection, speculative sandboxing,
//! compiler diagnostic interception, and iterative self-healing for untrusted scripts and tool plugins.
//!
//! ## Core Architecture:
//! 1. **Untrusted Multi-Tenant Sandbox Integration**:
//!    - Dispatches execution into `multitenant_wasm_sandbox::TenantSessionManager` for WebAssembly (WAT/WASM),
//!      Python micro-kernels, and JavaScript engines with instruction gas meters and page memory quotas.
//!    - Incorporates isolated RAM CoW overlay sandbox (`/dev/shm`) as fallback for native polyglot scripts.
//! 2. **Continuous Reflection Loop**:
//!    - Speculatively runs code inside the sandbox.
//!    - Intercepts compiler errors, syntax traps, assertion failures (`assert_eq!`), and fuel exhaustions.
//!    - Pipes diagnostics and failing source lines into `ReflectionDiagnosticAnalyzer` and `ReflectionAstFixer`.
//!    - Iteratively synthesizes surgical patches, re-verifying in the sandbox up to `max_retries` (default 3) until green.
//! 3. **Cryptographic Consensus Issuance**:
//!    - Upon green execution (exit code 0, 0 traps, assertions verified), generates a tamper-evident BLAKE3 and
//!      Ed25519 `ReflectionConsensusCertificate`.
//! 4. **Live Model Context Protocol (MCP) Streaming**:
//!    - Exposes canonical MCP tool `hgb_autonomous_self_heal_script`.
//!    - Streams real-time reflection events over Server-Sent Events (`/mcp/sse`) and WebSocket (`/hud`)
//!      to connected IDEs (Cursor, Windsurf, Claude Desktop).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::response::sse::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use hgb_core::closed_loop_wasm_reflection::{
    ClosedLoopWasmReflectionEngine, ReflectionAstFixer, ReflectionAttempt,
    ReflectionConsensusCertificate, ReflectionDiagnostic, ReflectionDiagnosticAnalyzer,
    ReflectionEngineConfig, ReflectionErrorKind, ReflectionSessionReport,
    DEFAULT_MAX_REFLECTION_RETRIES, DEFAULT_REFLECTION_GAS_BUDGET,
    MAX_REFLECTION_RETRIES_CEILING,
};
use hgb_core::error::{HgbError, Result};
use hgb_core::multitenant_wasm_sandbox::{LanguageKind, TenantExecutionResult, TenantSessionManager};
use crate::mcp_sse_server::McpSseState;

/// Canonical MCP tool name for autonomous closed-loop self-healing
pub const AUTONOMOUS_SELF_HEAL_TOOL_NAME: &str = "hgb_autonomous_self_heal_script";

// ==============================================================================
// 1. MCP Request & Response Payloads
// ==============================================================================

/// Invocation payload for `hgb_autonomous_self_heal_script`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutonomousSelfHealRequest {
    /// Unique identifier for the script, agent session, or dynamic plugin
    pub script_id: String,
    /// Programming language: "wat", "wasm", "python", "javascript"
    pub language: String,
    /// Initial candidate source code text
    pub source_code: String,
    /// Optional unit test or assertion script to verify correctness
    #[serde(default)]
    pub test_assertion_script: Option<String>,
    /// Optional maximum self-healing retry iterations (default 3, max 10)
    #[serde(default)]
    pub max_retries: Option<usize>,
    /// Optional CPU instruction fuel budget cap
    #[serde(default)]
    pub gas_budget: Option<u64>,
}

impl AutonomousSelfHealRequest {
    pub fn new(
        script_id: impl Into<String>,
        language: impl Into<String>,
        source_code: impl Into<String>,
    ) -> Self {
        Self {
            script_id: script_id.into(),
            language: language.into(),
            source_code: source_code.into(),
            test_assertion_script: None,
            max_retries: Some(DEFAULT_MAX_REFLECTION_RETRIES),
            gas_budget: Some(DEFAULT_REFLECTION_GAS_BUDGET),
        }
    }

    pub fn with_test_assertions(mut self, assertions: impl Into<String>) -> Self {
        self.test_assertion_script = Some(assertions.into());
        self
    }

    pub fn with_retries(mut self, retries: usize) -> Self {
        self.max_retries = Some(retries);
        self
    }

    pub fn with_gas_budget(mut self, budget: u64) -> Self {
        self.gas_budget = Some(budget);
        self
    }
}

/// Structured outcome returned by `hgb_autonomous_self_heal_script`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutonomousSelfHealResponse {
    pub success: bool,
    pub script_id: String,
    pub language: String,
    pub rounds_attempted: usize,
    pub final_code: String,
    pub initial_error_diagnostic: Option<String>,
    pub patch_summaries: Vec<String>,
    pub total_duration_ms: f64,
    pub total_gas_consumed: u64,
    pub consensus_certificate: Option<ReflectionConsensusCertificate>,
    pub final_stdout: String,
    pub final_stderr: String,
    pub error: Option<String>,
}

impl AutonomousSelfHealResponse {
    pub fn from_session_report(report: ReflectionSessionReport) -> Self {
        let initial_err = report
            .attempts
            .first()
            .and_then(|a| a.diagnostics.first())
            .map(|d| d.error_message.clone());

        let patches: Vec<String> = report
            .attempts
            .iter()
            .filter_map(|a| a.applied_patch_summary.clone())
            .collect();

        let final_stdout = report
            .final_execution_result
            .as_ref()
            .map(|r| r.stdout.clone())
            .unwrap_or_default();

        let final_stderr = report
            .final_execution_result
            .as_ref()
            .map(|r| r.stderr.clone())
            .unwrap_or_default();

        Self {
            success: report.healed,
            script_id: report.script_id,
            language: report.language.to_string(),
            rounds_attempted: report.total_rounds,
            final_code: report.final_code,
            initial_error_diagnostic: initial_err,
            patch_summaries: patches,
            total_duration_ms: report.total_duration_ms,
            total_gas_consumed: report.total_gas_consumed,
            consensus_certificate: report.consensus_certificate,
            final_stdout,
            final_stderr,
            error: if report.healed {
                None
            } else {
                Some("Failed to achieve green execution state within retry budget".to_string())
            },
        }
    }

    pub fn err(script_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        let sid = script_id.into();
        let err_msg = error_message.into();
        Self {
            success: false,
            script_id: sid,
            language: "unknown".to_string(),
            rounds_attempted: 0,
            final_code: String::new(),
            initial_error_diagnostic: Some(err_msg.clone()),
            patch_summaries: Vec::new(),
            total_duration_ms: 0.0,
            total_gas_consumed: 0,
            consensus_certificate: None,
            final_stdout: String::new(),
            final_stderr: err_msg.clone(),
            error: Some(err_msg),
        }
    }
}

// ==============================================================================
// 2. Closed-Loop Self-Healing Runner Service
// ==============================================================================

/// Autonomous reflection and self-healing runner service
pub struct ClosedLoopSelfHealingRunner {
    engine: Arc<ClosedLoopWasmReflectionEngine>,
    workspace_root: PathBuf,
}

impl ClosedLoopSelfHealingRunner {
    /// Initialize with an active TenantSessionManager and workspace root
    pub fn new<P: AsRef<Path>>(
        tenant_manager: Arc<TenantSessionManager>,
        workspace_root: P,
    ) -> Self {
        let engine = Arc::new(ClosedLoopWasmReflectionEngine::new(tenant_manager));
        Self {
            engine,
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Access inner reflection engine
    pub fn engine(&self) -> &Arc<ClosedLoopWasmReflectionEngine> {
        &self.engine
    }

    /// Return Model Context Protocol declarative tool definition
    pub fn tool_definition() -> Value {
        serde_json::json!({
            "name": AUTONOMOUS_SELF_HEAL_TOOL_NAME,
            "description": "Autonomously executes, compiles, and self-heals scripts or code snippets in an untrusted sandbox (WASM/MicroVM), intercepting errors, feeding stderr into a closed-loop reflection engine, and iteratively patching code until green.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "script_id": {
                        "type": "string",
                        "description": "Unique identifier for the code snippet, task, or dynamic plugin"
                    },
                    "language": {
                        "type": "string",
                        "enum": ["wat", "wasm", "python", "javascript"],
                        "description": "Language of the code snippet ('wat', 'wasm', 'python', 'javascript')"
                    },
                    "source_code": {
                        "type": "string",
                        "description": "Initial candidate source code to compile, execute, and verify"
                    },
                    "test_assertion_script": {
                        "type": "string",
                        "description": "Optional unit test or assertion script to verify semantic correctness"
                    },
                    "max_retries": {
                        "type": "integer",
                        "description": "Maximum allowed closed-loop self-healing retry iterations (default 3, max 10)"
                    },
                    "gas_budget": {
                        "type": "integer",
                        "description": "Optional CPU instruction fuel budget limit per speculative attempt"
                    }
                },
                "required": ["script_id", "language", "source_code"]
            }
        })
    }

    /// Execute the closed-loop self-healing reflection cycle with live SSE progress streaming
    pub async fn execute_self_healing(
        &self,
        request: AutonomousSelfHealRequest,
        sse_state: Option<Arc<McpSseState>>,
        sse_session_id: Option<&str>,
    ) -> Result<AutonomousSelfHealResponse> {
        let lang = match request.language.parse::<LanguageKind>() {
            Ok(l) => l,
            Err(e) => return Ok(AutonomousSelfHealResponse::err(&request.script_id, e.to_string())),
        };

        let session_id_owned = sse_session_id.map(|s| s.to_string());
        let sse_state_owned = sse_state.clone();

        // Progress callback emitting real-time Server-Sent Events to connected IDE
        let progress_hook = move |attempt: &ReflectionAttempt| {
            if let (Some(ref state), Some(ref sid)) = (&sse_state_owned, &session_id_owned) {
                if !sid.is_empty() {
                    let event_kind = if attempt.success {
                        "reflection_success"
                    } else if attempt.diagnostics.is_empty() {
                        "reflection_round_exec"
                    } else {
                        "reflection_patch_applied"
                    };

                    let sse_event = Event::default().event(event_kind).data(
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/message",
                            "params": {
                                "level": if attempt.success { "info" } else { "warning" },
                                "logger": "closed_loop_self_healing",
                                "data": {
                                    "round": attempt.round,
                                    "success": attempt.success,
                                    "exit_code": attempt.exit_code,
                                    "gas_consumed": attempt.gas_consumed,
                                    "patch_summary": attempt.applied_patch_summary,
                                    "diagnostics_count": attempt.diagnostics.len(),
                                }
                            }
                        })
                        .to_string(),
                    );
                    let state_clone = Arc::clone(state);
                    let sid_clone = sid.clone();
                    tokio::spawn(async move {
                        let _ = state_clone.emit_to_session(&sid_clone, sse_event).await;
                    });
                }
            }
        };

        let report = self
            .engine
            .run_reflection_loop(
                &request.script_id,
                lang,
                &request.source_code,
                request.test_assertion_script.as_deref(),
                request.max_retries,
                request.gas_budget,
                Some(progress_hook),
            )
            .await?;

        Ok(AutonomousSelfHealResponse::from_session_report(report))
    }

    /// Dispatches MCP tool call from generic JSON arguments and returns MCP tool result Value
    pub async fn handle_tool_call(
        state: &Arc<McpSseState>,
        arguments: &Value,
        session_id: &str,
    ) -> (Value, bool) {
        let req: AutonomousSelfHealRequest = match serde_json::from_value(arguments.clone()) {
            Ok(r) => r,
            Err(e) => {
                let err_resp =
                    AutonomousSelfHealResponse::err("unknown", format!("Invalid tool parameters: {}", e));
                let json_str = serde_json::to_string_pretty(&err_resp).unwrap_or_default();
                return (
                    serde_json::json!({
                        "content": [{ "type": "text", "text": json_str }],
                        "isError": true
                    }),
                    true,
                );
            }
        };

        let runner = Self::new(Arc::clone(&state.tenant_manager), &state.workspace_root);
        let s_id_opt = if session_id.is_empty() { None } else { Some(session_id) };

        let resp = match runner.execute_self_healing(req, Some(Arc::clone(state)), s_id_opt).await {
            Ok(r) => r,
            Err(e) => AutonomousSelfHealResponse::err("error", format!("Reflection runner error: {}", e)),
        };

        let is_error = !resp.success;
        let resp_json = serde_json::to_string_pretty(&resp).unwrap_or_default();

        (
            serde_json::json!({
                "content": [{ "type": "text", "text": resp_json }],
                "isError": is_error
            }),
            is_error,
        )
    }
}

// ==============================================================================
// 3. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autonomous_self_heal_tool_definition_schema() {
        let def = ClosedLoopSelfHealingRunner::tool_definition();
        assert_eq!(def["name"], AUTONOMOUS_SELF_HEAL_TOOL_NAME);
        let schema = &def["inputSchema"];
        assert_eq!(schema["type"], "object");
        let props = &schema["properties"];
        assert!(props.get("script_id").is_some());
        assert!(props.get("language").is_some());
        assert!(props.get("source_code").is_some());
        assert!(props.get("test_assertion_script").is_some());
        assert!(props.get("max_retries").is_some());
        assert!(props.get("gas_budget").is_some());

        let req_arr = schema["required"].as_array().expect("required array");
        assert!(req_arr.contains(&Value::String("script_id".to_string())));
        assert!(req_arr.contains(&Value::String("language".to_string())));
        assert!(req_arr.contains(&Value::String("source_code".to_string())));
    }

    #[tokio::test]
    async fn test_autonomous_self_healing_wat_opcode_repair() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let tenant_mgr = Arc::new(
            TenantSessionManager::with_base_root(temp_dir.path()).expect("tenant manager"),
        );
        let runner = ClosedLoopSelfHealingRunner::new(tenant_mgr, temp_dir.path());

        // Code with invalid opcode i32.plus that must be auto-healed to i32.add
        let buggy_wat = r#"(module
  (func (export "add") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.plus
  )
)"#;

        let req = AutonomousSelfHealRequest::new("wat_add_test", "wat", buggy_wat)
            .with_retries(3);

        let res = runner
            .execute_self_healing(req, None, None)
            .await
            .expect("self healing result");

        assert!(res.success, "Should successfully self-heal to green");
        assert!(res.final_code.contains("i32.add"), "Code should now contain i32.add");
        assert!(!res.final_code.contains("i32.plus"), "Code should no longer contain i32.plus");
        assert!(res.consensus_certificate.is_some(), "Should issue consensus certificate");
        let cert = res.consensus_certificate.unwrap();
        assert!(cert.verify_signature(), "Consensus certificate signature must verify");
    }

    #[tokio::test]
    async fn test_autonomous_self_healing_wat_parentheses_repair() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let tenant_mgr = Arc::new(
            TenantSessionManager::with_base_root(temp_dir.path()).expect("tenant manager"),
        );
        let runner = ClosedLoopSelfHealingRunner::new(tenant_mgr, temp_dir.path());

        // Code missing closing parenthesis
        let unclosed_wat = r#"(module
  (func (export "_start")
  )
"#; // Missing 1 closing parenthesis for module

        let req = AutonomousSelfHealRequest::new("wat_paren_test", "wat", unclosed_wat)
            .with_retries(3);

        let res = runner
            .execute_self_healing(req, None, None)
            .await
            .expect("self healing result");

        assert!(res.success, "Should successfully heal unbalanced parens");
        assert!(res.final_code.ends_with(')'), "Final code should end with closing paren");
    }
}
