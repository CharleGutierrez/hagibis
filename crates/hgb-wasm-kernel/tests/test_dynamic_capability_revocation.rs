//! # `test_dynamic_capability_revocation.rs` - Dynamic WASM Capability Revocation Verification
//! =========================================================================================
//! Tests:
//! 1. Dynamic injection of memory & network tokens into running instances without restart.
//! 2. Instant runtime revocation causing subsequent operations to be denied (`PermissionDenied`).
//! 3. Zero-restart lifecycle maintenance (dynamic privilege escalation and demotion).
//! 4. Network token domain, port, request counter, and egress byte quota enforcement.
//! 5. Multithreaded concurrent revocation stress.

use std::sync::Arc;
use std::thread;

use hgb_wasm_kernel::canonical_abi_shm_views::{
    AbiError, MemoryCapabilityToken, NetworkPermissionToken,
    ReadOnlyCapabilityToken, ReadWriteCapabilityToken,
};
use hgb_wasm_kernel::dynamic_capability_manager::DynamicCapabilityManager;

#[test]
fn test_dynamic_injection_memory_and_network_tokens() {
    let secret = b"unit_test_secret_key_hagibis_2026";
    let mgr = DynamicCapabilityManager::with_secret(secret);
    let instance_id = "plugin-data-processor-01";

    let mem_token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
        secret, 100, 0, 1, 1001, 0, 4096,
    ));
    let net_token = NetworkPermissionToken::issue(
        secret,
        200,
        vec!["*.hagibis.io".to_string(), "api.github.com".to_string()],
        443,
        443,
        10,
        65536,
        2001,
    );

    assert_eq!(mgr.inject_memory_token(instance_id, mem_token).unwrap(), 100);
    assert_eq!(mgr.inject_network_token(instance_id, net_token).unwrap(), 200);

    // Verify operations succeed
    assert!(mgr.verify_memory_access(instance_id, 100, 0, 1024, true).is_ok());
    assert!(mgr.verify_network_access(instance_id, 200, "node1.hagibis.io", 443, 512).is_ok());
}

#[test]
fn test_instant_runtime_revocation_denies_subsequent_operations() {
    let secret = b"instant_revocation_secret_2026";
    let mgr = DynamicCapabilityManager::with_secret(secret);
    let instance_id = "plugin-suspect-agent-02";

    let mem_token = MemoryCapabilityToken::ReadOnly(ReadOnlyCapabilityToken::issue(
        secret, 300, 0, 1, 3001, 0, 8192,
    ));
    mgr.inject_memory_token(instance_id, mem_token).expect("inject");

    assert!(mgr.verify_memory_access(instance_id, 300, 0, 512, false).is_ok());

    // Instant revocation
    let revoked = mgr.revoke_capability(300, "Malicious memory inspection detected").expect("revoke");
    assert!(revoked);
    assert!(mgr.is_capability_revoked(300));

    // Subsequent call must immediately fail with PermissionDenied
    let err = mgr.verify_memory_access(instance_id, 300, 0, 512, false);
    assert!(matches!(err, Err(AbiError::PermissionDenied)));
}

#[test]
fn test_zero_restart_lifecycle_maintenance() {
    let secret = b"zero_restart_lifecycle_secret";
    let mgr = DynamicCapabilityManager::with_secret(secret);
    let instance_id = "long-running-microvm-plugin";

    // Cycle 1: RO Token
    let ro_token = MemoryCapabilityToken::ReadOnly(ReadOnlyCapabilityToken::issue(
        secret, 401, 0, 1, 4001, 0, 2048,
    ));
    mgr.inject_memory_token(instance_id, ro_token).expect("inject ro");
    assert!(mgr.verify_memory_access(instance_id, 401, 0, 1024, false).is_ok());
    assert!(matches!(mgr.verify_memory_access(instance_id, 401, 0, 1024, true), Err(AbiError::PermissionDenied)));

    // Cycle 2: Dynamically escalate to RW Token without restart
    let rw_token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
        secret, 402, 0, 1, 4002, 0, 2048,
    ));
    mgr.inject_memory_token(instance_id, rw_token).expect("inject rw");
    assert!(mgr.verify_memory_access(instance_id, 402, 0, 1024, true).is_ok());

    // Cycle 3: Revoke RW token only
    mgr.revoke_capability(402, "Demoting back to RO").expect("revoke rw");
    assert!(matches!(mgr.verify_memory_access(instance_id, 402, 0, 1024, true), Err(AbiError::PermissionDenied)));
    // RO token remains active and valid!
    assert!(mgr.verify_memory_access(instance_id, 401, 0, 1024, false).is_ok());
}

#[test]
fn test_network_token_host_port_and_quota_enforcement() {
    let secret = b"quota_enforcement_secret_2026";
    let mgr = DynamicCapabilityManager::with_secret(secret);
    let instance_id = "plugin-restricted-egress";

    let net_token = NetworkPermissionToken::issue(
        secret,
        500,
        vec!["internal.mesh".to_string()],
        8080,
        8088,
        2,
        100,
        5001,
    );
    mgr.inject_network_token(instance_id, net_token).expect("inject");

    // Invalid host
    assert!(matches!(mgr.verify_network_access(instance_id, 500, "evil.com", 8080, 10), Err(AbiError::PermissionDenied)));
    // Invalid port
    assert!(matches!(mgr.verify_network_access(instance_id, 500, "internal.mesh", 443, 10), Err(AbiError::PermissionDenied)));

    // Request 1: OK (50 bytes)
    assert!(mgr.verify_network_access(instance_id, 500, "internal.mesh", 8080, 50).is_ok());
    // Request 2: OK (50 bytes) -> total 100 bytes reached
    assert!(mgr.verify_network_access(instance_id, 500, "internal.mesh", 8080, 50).is_ok());
    // Request 3: Denied because max_outbound_requests = 2
    assert!(matches!(mgr.verify_network_access(instance_id, 500, "internal.mesh", 8080, 10), Err(AbiError::PermissionDenied)));
}

#[test]
fn test_concurrent_multithreaded_revocation_stress() {
    let secret: &'static [u8] = b"multithreaded_revocation_stress_key";
    let mgr = Arc::new(DynamicCapabilityManager::with_secret(secret));
    let instance_id = "concurrent-worker-instance";

    let token = MemoryCapabilityToken::ReadWrite(ReadWriteCapabilityToken::issue(
        secret, 777, 0, 1, 9999, 0, 65536,
    ));
    mgr.inject_memory_token(instance_id, token).expect("inject");

    let num_threads = 4;
    let iterations_per_thread = 2000;
    let mut handles = Vec::new();

    for _ in 0..num_threads {
        let m = Arc::clone(&mgr);
        handles.push(thread::spawn(move || {
            let mut ok_count = 0;
            let mut denied_count = 0;
            for _ in 0..iterations_per_thread {
                match m.verify_memory_access(instance_id, 777, 0, 64, true) {
                    Ok(_) => ok_count += 1,
                    Err(AbiError::PermissionDenied) => denied_count += 1,
                    Err(e) => panic!("Unexpected error: {:?}", e),
                }
            }
            (ok_count, denied_count)
        }));
    }

    // Revoke concurrently while workers are running
    thread::sleep(std::time::Duration::from_millis(1));
    mgr.revoke_capability(777, "Stress test concurrent trigger").expect("revoke");

    let mut total_denied = 0;
    for h in handles {
        let (_ok, denied) = h.join().expect("join");
        total_denied += denied;
    }
    assert!(total_denied > 0);
    // After revocation, subsequent access must always be denied
    assert!(matches!(mgr.verify_memory_access(instance_id, 777, 0, 64, true), Err(AbiError::PermissionDenied)));
}
