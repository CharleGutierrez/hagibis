//! Comprehensive Test Suite: Pillar 3 Multi-Agent MicroVM Swarm Orchestration & Security Containment
//! ===============================================================================================

use std::fs;
use std::path::Path;
use tempfile::tempdir;

use hgb_core::microvm_swarm_orchestrator::{
    IsolationTier, MicroVmSwarmOrchestrator, SwarmTask, SwarmWorkerType, TaskComplexity,
};

#[test]
fn test_microvm_swarm_orchestrator_initialization() {
    let temp = tempdir().expect("tempdir");
    let orchestrator = MicroVmSwarmOrchestrator::new(temp.path()).expect("orchestrator init");

    let status = orchestrator.status();
    assert_eq!(status.total_dispatched, 0);
    assert!(status.wasm_hot_slots > 0);
    assert_eq!(status.workspace_root, temp.path().to_string_lossy().to_string());
}

#[test]
fn test_task_complexity_classification() {
    let temp = tempdir().expect("tempdir");
    let orchestrator = MicroVmSwarmOrchestrator::new(temp.path()).expect("orchestrator init");

    // 1. Lightweight tasks -> WasmHotPool
    let task_json = SwarmTask::new_lightweight("json_transform", "json", r#"{"key": "value"}"#);
    assert_eq!(orchestrator.classify_task(&task_json), TaskComplexity::Lightweight);

    let task_regex = SwarmTask::new_lightweight("pattern_match", "regex", r#"^[a-z]+$"#);
    assert_eq!(orchestrator.classify_task(&task_regex), TaskComplexity::Lightweight);

    let task_ast = SwarmTask::new_lightweight("syntax_check", "ast", "fn foo() -> u32 { 42 }");
    assert_eq!(orchestrator.classify_task(&task_ast), TaskComplexity::Lightweight);

    // 2. Heavy untrusted tasks -> Firecracker microVM / Landlock RAM CoW
    let task_bash = SwarmTask::new_heavy("bash", "echo 'Running untrusted script'");
    assert_eq!(orchestrator.classify_task(&task_bash), TaskComplexity::HeavyUntrusted);

    let task_cargo = SwarmTask {
        task_name: Some("cargo_compile_task".to_string()),
        language: "bash".to_string(),
        code: "cargo build --release".to_string(),
        ..Default::default()
    };
    assert_eq!(orchestrator.classify_task(&task_cargo), TaskComplexity::HeavyUntrusted);

    let task_sim = SwarmTask {
        task_name: Some("agent_sandbox_simulation".to_string()),
        language: "bash".to_string(),
        code: "python3 simulate.py".to_string(),
        ..Default::default()
    };
    assert_eq!(orchestrator.classify_task(&task_sim), TaskComplexity::HeavyUntrusted);
}

#[test]
fn test_ephemeral_microvm_dispatch_safe_containment() {
    let temp = tempdir().expect("tempdir");
    let orchestrator = MicroVmSwarmOrchestrator::new(temp.path()).expect("orchestrator init");

    let task = SwarmTask::new_heavy(
        "bash",
        "echo 'HGB_MICROVM_CONTAINMENT_VERIFIED'",
    );

    let outcome = orchestrator.dispatch(&task).expect("microvm dispatch");
    assert!(outcome.success, "Expected successful execution: stderr={}", outcome.stderr);
    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome.stdout.contains("HGB_MICROVM_CONTAINMENT_VERIFIED"),
        "Stdout missing verification token: {}",
        outcome.stdout
    );
    assert_eq!(outcome.worker_type, SwarmWorkerType::FirecrackerMicroVm);
    assert_eq!(outcome.complexity, TaskComplexity::HeavyUntrusted);
    assert!(
        outcome.isolation_tier.contains("Firecracker") || outcome.isolation_tier.contains("Landlock"),
        "Expected hardware or Landlock isolation tier, got: {}",
        outcome.isolation_tier
    );
    assert!(outcome.duration_ms > 0 || outcome.duration_us > 0);
}

#[test]
fn test_untrusted_script_forbidden_host_mutation() {
    let temp = tempdir().expect("tempdir");
    let orchestrator = MicroVmSwarmOrchestrator::new(temp.path()).expect("orchestrator init");

    // Script attempts to write to /etc/hgb_untrusted_test_probe or access host disk directly
    let probe_file = Path::new("/etc/hgb_untrusted_test_probe");
    let task = SwarmTask::new_heavy(
        "bash",
        r#"
if touch /etc/hgb_untrusted_test_probe 2>/dev/null; then
    echo "HOST_VULNERABLE"
    rm -f /etc/hgb_untrusted_test_probe
else
    echo "HOST_SECURELY_PROTECTED"
fi
"#,
    );

    let outcome = orchestrator.dispatch(&task).expect("dispatch");
    assert!(outcome.success);
    assert!(
        outcome.stdout.contains("HOST_SECURELY_PROTECTED"),
        "Untrusted code must be denied write access to host /etc! Output: {}",
        outcome.stdout
    );
    assert!(!probe_file.exists(), "Probe file must NOT exist on host filesystem!");
}

#[test]
fn test_multi_agent_swarm_batch_dispatch() {
    let temp = tempdir().expect("tempdir");
    let orchestrator = MicroVmSwarmOrchestrator::new(temp.path()).expect("orchestrator init");

    // Multi-agent workflow:
    // Agent 1: Lightweight AST syntax validation
    // Agent 2: Lightweight JSON transformation
    // Agent 3: Heavy untrusted shell computation
    // Agent 4: Heavy untrusted simulation script
    let tasks = vec![
        SwarmTask::new_lightweight("agent_ast_validator", "ast", "pub fn agent_logic() -> bool { true }"),
        SwarmTask::new_lightweight("agent_json_transformer", "json", r#"{"agent_id": 42, "role": "scout"}"#),
        SwarmTask::new_heavy("bash", "echo 'Agent 3: MicroVM task executed'"),
        SwarmTask::new_heavy("bash", "echo 'Agent 4: Multi-agent fuzz test passed'"),
    ];

    let outcomes = orchestrator.dispatch_batch(&tasks).expect("batch dispatch");
    assert_eq!(outcomes.len(), 4);

    // Verify Agent 1 (AST) routed to WasmWorker
    assert_eq!(outcomes[0].worker_type, SwarmWorkerType::WasmWorker);
    assert!(outcomes[0].success);

    // Verify Agent 2 (JSON) routed to WasmWorker
    assert_eq!(outcomes[1].worker_type, SwarmWorkerType::WasmWorker);
    assert!(outcomes[1].success);

    // Verify Agent 3 routed to FirecrackerMicroVm
    assert_eq!(outcomes[2].worker_type, SwarmWorkerType::FirecrackerMicroVm);
    assert!(outcomes[2].success);
    assert!(outcomes[2].stdout.contains("Agent 3: MicroVM task executed"));

    // Verify Agent 4 routed to FirecrackerMicroVm
    assert_eq!(outcomes[3].worker_type, SwarmWorkerType::FirecrackerMicroVm);
    assert!(outcomes[3].success);
    assert!(outcomes[3].stdout.contains("Agent 4: Multi-agent fuzz test passed"));

    // Verify status tracking
    let st = orchestrator.status();
    assert_eq!(st.total_dispatched, 4);
    assert_eq!(st.wasm_dispatched, 2);
    assert_eq!(st.microvm_dispatched, 2);
}

#[test]
fn test_microvm_snapshot_prewarm() {
    let temp = tempdir().expect("tempdir");
    let orchestrator = MicroVmSwarmOrchestrator::new(temp.path()).expect("orchestrator init");

    let report = orchestrator.prewarm_snapshots(None).expect("prewarm snapshots");
    assert!(report.success);
    assert!(!report.snapshot_path.is_empty());
    assert!(!report.mem_file_path.is_empty());
    assert!(Path::new(&report.snapshot_path).exists() || Path::new(&report.snapshot_dir).exists());
}
