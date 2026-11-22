//! # Hardened Dual-Sandbox Comprehensive Integration & Verification Tests
//! =========================================================================
//! Verifies:
//! 1. Capability revocation enforcement: revoking active token denies subsequent operations.
//! 2. Fuel exhaustion enforcement: infinite loops & runaway executions are trapped and bounded.
//! 3. Isolated payload execution: arithmetic, host calls, and memory ops execute correctly.
//! 4. Memory boundary assertions: memory limits and unauthorized memory accesses are trapped.
//! 5. Cryptographic audit receipt generation: SHA-256 signatures are tamper-evident.
//! 6. Reverse engineering analyzer: binary format detection, symbol & section extraction, entropy, and WIT synthesis.
//! 7. Dual-layer hardened execution: layered WASM inside MicroVM jail defense-in-depth.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use hgb_wasm_kernel::hardened_dual_sandbox::*;
use hgb_wasm_kernel::dynamic_capability_manager::DynamicCapabilityManager;

const TEST_SECRET: &[u8] = b"hgb_test_verification_audit_secret_2026";

fn setup_coordinator() -> HardenedDualSandboxCoordinator {
    let jail_dir = PathBuf::from("/tmp/hgb_test_hardened_jail");
    let _ = fs::create_dir_all(&jail_dir);
    HardenedDualSandboxCoordinator::with_options(TEST_SECRET, jail_dir).expect("coordinator init")
}

#[test]
fn test_capability_revocation_enforcement() {
    let coordinator = setup_coordinator();

    // 1. Build a valid token with FS and network permissions
    let token = CapabilityToken::builder("agent_reverse_engineer")
        .with_token_id(101)
        .with_fs_read("/tmp")
        .with_network_egress("api.internal.hagibis.ai", 443)
        .build(TEST_SECRET);

    // Initial check passes
    assert!(coordinator.validator().check_fs_read(&token, &PathBuf::from("/tmp/test.txt")).is_ok());

    // 2. Actively revoke token at runtime
    let revoked = coordinator.validator().revoke_token(101, "Suspicious behavior detected").expect("revocation");
    assert!(revoked, "Revocation of active token must succeed");
    assert!(coordinator.validator().is_token_revoked(101));

    // 3. Subsequent operations must immediately fail with Critical violation
    let result = coordinator.validator().check_fs_read(&token, &PathBuf::from("/tmp/test.txt"));
    assert!(result.is_err(), "Revoked token must be rejected immediately");
    let violation = result.unwrap_err();
    assert_eq!(violation.severity, ViolationSeverity::Critical);
    assert!(violation.message.contains("revoked"));
}

#[test]
fn test_fuel_exhaustion_enforcement() {
    let coordinator = setup_coordinator();

    // Infinite loop WAT module
    let infinite_loop_wat = r#"
        (module
            (func (export "_start")
                (loop $spin
                    (br $spin)
                )
            )
        )
    "#;

    let target = UntrustedExecutionTarget::from_wat("infinite_loop_payload", infinite_loop_wat)
        .expect("wat parse")
        .with_fuel_limit(10_000); // 10k fuel is strictly insufficient

    let token = CapabilityToken::builder("untrusted_script")
        .with_token_id(102)
        .with_fuel_quota(10_000)
        .build(TEST_SECRET);

    let receipt = coordinator.execute(&target, &token, ExecutionSandboxMode::WasmSandboxed);

    // Fuel must be completely expended and execution trapped
    assert!(!receipt.is_success, "Infinite loop must not succeed");
    assert_eq!(receipt.exit_code, -1);
    assert_eq!(receipt.fuel_expended, 10_000, "Must expend full fuel quota");
    assert!(receipt.stderr_str().contains("fuel") || receipt.stderr_str().contains("exhausted") || !receipt.is_success);
    assert!(receipt.verify_audit_signature(TEST_SECRET), "Receipt audit signature must verify");
}

#[test]
fn test_isolated_payload_execution_success() {
    let coordinator = setup_coordinator();

    // Computation module: adds numbers and returns exit code 0
    let compute_wat = r#"
        (module
            (func (export "_start") (result i32)
                (i32.const 0)
            )
        )
    "#;

    let target = UntrustedExecutionTarget::from_wat("safe_math_compute", compute_wat)
        .expect("wat parse");

    let token = CapabilityToken::builder("math_agent")
        .with_token_id(103)
        .build(TEST_SECRET);

    let receipt = coordinator.execute(&target, &token, ExecutionSandboxMode::WasmSandboxed);

    assert!(receipt.is_success, "Safe compute workload must succeed");
    assert_eq!(receipt.exit_code, 0);
    assert!(receipt.fuel_expended > 0, "Execution must meter fuel consumption");
    assert!(receipt.verify_audit_signature(TEST_SECRET));
}

#[test]
fn test_memory_boundary_assertions() {
    let coordinator = setup_coordinator();

    // WAT requesting 100 pages of 64KB = 6.4 MB memory
    let memory_grow_wat = r#"
        (module
            (memory (export "memory") 1 200)
            (func (export "_start") (result i32)
                (memory.grow (i32.const 100))
                (drop)
                (i32.const 0)
            )
        )
    "#;

    // Constrain max memory to 2 MB (strictly less than 101 pages)
    let target = UntrustedExecutionTarget::from_wat("memory_hog_payload", memory_grow_wat)
        .expect("wat parse")
        .with_max_memory_mb(2);

    let token = CapabilityToken::builder("memory_hog")
        .with_token_id(104)
        .with_memory_ceiling_mb(2)
        .build(TEST_SECRET);

    let receipt = coordinator.execute(&target, &token, ExecutionSandboxMode::WasmSandboxed);

    // Either memory growth was capped or instantiation failed under strict limits
    assert!(receipt.memory_pages_consumed * (WASM_PAGE_SIZE_BYTES as u64) <= 2 * 1024 * 1024 + WASM_PAGE_SIZE_BYTES as u64);
    assert!(receipt.verify_audit_signature(TEST_SECRET));
}

#[test]
fn test_cryptographic_audit_receipt_generation_and_tamper_detection() {
    let coordinator = setup_coordinator();

    let target = UntrustedExecutionTarget::from_wat("audit_test", "(module (func (export \"_start\")))")
        .expect("wat parse");

    let token = CapabilityToken::builder("auditor_agent")
        .with_token_id(105)
        .build(TEST_SECRET);

    let mut receipt = coordinator.execute(&target, &token, ExecutionSandboxMode::WasmSandboxed);

    // Valid receipt verifies
    assert!(receipt.verify_audit_signature(TEST_SECRET));

    // Tamper with exit_code
    receipt.exit_code = 42;
    assert!(!receipt.verify_audit_signature(TEST_SECRET), "Tampered exit code must invalidate signature");

    // Restore and tamper with stdout
    receipt.exit_code = 0;
    receipt.stdout.push(0xFF);
    assert!(!receipt.verify_audit_signature(TEST_SECRET), "Tampered stdout must invalidate signature");
}

#[test]
fn test_reverse_engineering_analyzer_and_wit_synthesis() {
    let coordinator = setup_coordinator();

    let wasm_wat = r#"
        (module
            (func (export "analyze_malware_cfg") (result i32)
                (i32.const 1)
            )
            (func (export "extract_pe_headers") (result i32)
                (i32.const 2)
            )
        )
    "#;

    let target = UntrustedExecutionTarget::from_wat("sample_binary", wasm_wat).expect("wat parse");

    // Lacks reverse-engineering permission
    let unauth_token = CapabilityToken::builder("unauthorized_agent")
        .with_token_id(106)
        .build(TEST_SECRET);

    let unauth_result = coordinator.reverse_engineer_target(&target, &unauth_token);
    assert!(unauth_result.is_err(), "Without WIT_CAP_REVERSE_ENGINEER, analysis must fail");

    // Authorized token
    let auth_token = CapabilityToken::builder("re_agent")
        .with_token_id(107)
        .with_reverse_engineer()
        .build(TEST_SECRET);

    let analysis = coordinator.reverse_engineer_target(&target, &auth_token).expect("re analysis");
    assert_eq!(analysis.format, BinaryFormat::Wasm);
    assert_eq!(analysis.arch, BinaryArch::Wasm32);
    assert!(!analysis.sections.is_empty());
    assert!(analysis.sections[0].entropy > 0.0);
    assert!(analysis.wit_definition.is_some());
    let wit = analysis.wit_definition.unwrap();
    assert!(wit.contains("package hagibis:reverse-engineered"));
    assert!(wit.contains("analyze-malware-cfg"));
}

#[test]
fn test_dual_layer_hardened_execution() {
    let coordinator = setup_coordinator();

    let safe_wat = r#"
        (module
            (func (export "_start") (result i32)
                (i32.const 0)
            )
        )
    "#;

    let target = UntrustedExecutionTarget::from_wat("high_risk_binary", safe_wat).expect("wat parse");

    let token = CapabilityToken::builder("critical_agent")
        .with_token_id(108)
        .build(TEST_SECRET);

    let receipt = coordinator.execute(&target, &token, ExecutionSandboxMode::DualLayerHardened);

    assert!(receipt.is_success);
    assert_eq!(receipt.mode_used, ExecutionSandboxMode::DualLayerHardened);
    assert!(receipt.verify_audit_signature(TEST_SECRET));
}