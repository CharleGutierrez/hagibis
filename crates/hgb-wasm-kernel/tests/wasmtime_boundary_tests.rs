//! # Wasmtime Boundary, Fuel Metering & Epoch Interruption Tests
//!
//! Verifies:
//! - Instruction fuel exhaustion traps on infinite and heavy loops
//! - Asynchronous epoch interruption preemption of runaway WASM executions
//! - Linear memory capping and containment enforcement
//! - SPSC ring buffer integration mapped directly over Wasmtime linear memory
//! - Cryptographic rejection of tampered modules before compilation or instantiation

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use ed25519_dalek::SigningKey;
use wasmtime::Val;

use hgb_wasm_kernel::canonical_abi_shm_views::{
    AbiError, ComponentIntegrityManifest, SpscRingElementHeader, WasmtimeSecureRuntime,
};

fn generate_metered_wasm_binary() -> Vec<u8> {
    wat::parse_str(r#"
        (module
            (memory (export "memory") 1 2)
            (func (export "infinite_loop")
                (loop $loop
                    (br $loop)
                )
            )
            (func (export "heavy_work")
                (local $i i32)
                (loop $loop
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br_if $loop (i32.lt_s (local.get $i) (i32.const 1000)))
                )
            )
            (func (export "add") (param $a i32) (param $b i32) (result i32)
                (i32.add (local.get $a) (local.get $b))
            )
        )
    "#).expect("parse WAT into WASM binary")
}

#[test]
fn test_wasmtime_basic_execution_and_fuel_consumption() {
    let wasm = generate_metered_wasm_binary();
    let signing_key = SigningKey::from_bytes(&[0x11u8; 32]);
    let initial_fuel = 50_000u64;

    let manifest = ComponentIntegrityManifest::sign(
        "metered_test_module",
        "1.0.0",
        &wasm,
        &signing_key,
        initial_fuel,
        2,
    );

    let runtime = WasmtimeSecureRuntime::new().expect("runtime");
    let mut instance = runtime.instantiate_verified_module(&wasm, &manifest, 16)
        .expect("instantiate");

    assert_eq!(instance.get_fuel().expect("fuel"), initial_fuel);

    // Call "add" (10 + 25 = 35)
    let res = instance.call_func("add", &[Val::I32(10), Val::I32(25)]).expect("call add");
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].i32(), Some(35));

    // Verify fuel was consumed
    let remaining_fuel = instance.get_fuel().expect("get fuel");
    assert!(remaining_fuel < initial_fuel, "Fuel must decrease after execution");
    let consumed = initial_fuel - remaining_fuel;
    println!("'add' consumed {} units of fuel (remaining: {})", consumed, remaining_fuel);
}

#[test]
fn test_wasmtime_fuel_exhaustion_trap() {
    let wasm = generate_metered_wasm_binary();
    let signing_key = SigningKey::from_bytes(&[0x22u8; 32]);
    let tight_fuel = 50u64; // Insufficient for infinite loop or 1000-iter heavy work

    let manifest = ComponentIntegrityManifest::sign(
        "runaway_candidate",
        "1.0.0",
        &wasm,
        &signing_key,
        tight_fuel,
        2,
    );

    let runtime = WasmtimeSecureRuntime::new().expect("runtime");
    let mut instance = runtime.instantiate_verified_module(&wasm, &manifest, 16)
        .expect("instantiate");

    // Calling infinite_loop with only 50 fuel must trap with FuelExhausted!
    let trap_result = instance.call_func("infinite_loop", &[]);
    assert!(matches!(trap_result, Err(AbiError::FuelExhausted { .. })));

    if let Err(AbiError::FuelExhausted { limit, .. }) = trap_result {
        assert_eq!(limit, tight_fuel);
    }
}

#[test]
fn test_wasmtime_epoch_interruption_preemption() {
    let wasm = generate_metered_wasm_binary();
    let signing_key = SigningKey::from_bytes(&[0x33u8; 32]);
    let large_fuel = 100_000_000u64; // Large fuel budget so it won't run out

    let manifest = ComponentIntegrityManifest::sign(
        "infinite_loop_component",
        "1.0.0",
        &wasm,
        &signing_key,
        large_fuel,
        2,
    );

    let runtime = WasmtimeSecureRuntime::new().expect("runtime");
    let mut instance = runtime.instantiate_verified_module(&wasm, &manifest, 16)
        .expect("instantiate");

    let engine = instance.engine.clone();

    // Spawn watcher thread that will increment the epoch after 20ms
    let watcher_handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        engine.increment_epoch();
    });

    let start = Instant::now();
    let result = instance.call_func("infinite_loop", &[]);
    let elapsed = start.elapsed();

    watcher_handle.join().expect("watcher join");

    // The runaway execution MUST be preempted by the epoch deadline
    assert!(matches!(result, Err(AbiError::EpochInterruption { .. })), "Expected EpochInterruption, got: {:?}", result);
    println!("Asynchronous epoch interruption preempted infinite loop in {:?}", elapsed);
    assert!(elapsed < Duration::from_millis(500), "Preemption must occur promptly");
}

#[test]
fn test_wasmtime_linear_memory_capping_containment() {
    let wasm = generate_metered_wasm_binary();
    let signing_key = SigningKey::from_bytes(&[0x44u8; 32]);
    let capped_pages = 1u32; // Exactly 1 page = 64 KiB = 65536 bytes
    let capped_bytes = (capped_pages as usize) * 65536;

    let manifest = ComponentIntegrityManifest::sign(
        "bounded_memory_module",
        "1.0.0",
        &wasm,
        &signing_key,
        1_000_000,
        capped_pages,
    );

    let runtime = WasmtimeSecureRuntime::new().expect("runtime");
    let mut instance = runtime.instantiate_verified_module(&wasm, &manifest, capped_pages)
        .expect("instantiate with 1 page cap");

    assert_eq!(instance.memory_cap_bytes(), capped_bytes);

    // 1. In-bounds writes and reads succeed
    let test_data = b"BOUNDED_WASMTIME_LINEAR_MEMORY_TEST";
    instance.write_linear_memory(1024, test_data).expect("write in bounds");
    let read_back = instance.read_linear_memory(1024, test_data.len()).expect("read in bounds");
    assert_eq!(&read_back, test_data);

    // 2. Out-of-bounds write beyond 64 KiB capped limit must fail with LinearMemoryCapExceeded
    let oob_offset = capped_bytes - 10;
    let oob_slice = vec![0u8; 20]; // 10 bytes past boundary
    let oob_err = instance.write_linear_memory(oob_offset, &oob_slice);
    assert!(matches!(oob_err, Err(AbiError::LinearMemoryCapExceeded { .. })));

    // 3. Out-of-bounds read beyond capped limit must fail
    let oob_read_err = instance.read_linear_memory(capped_bytes, 16);
    assert!(matches!(oob_read_err, Err(AbiError::LinearMemoryCapExceeded { .. })));
}

#[test]
fn test_wasmtime_spsc_ring_buffer_mapped_in_linear_memory() {
    let wasm = generate_metered_wasm_binary();
    let signing_key = SigningKey::from_bytes(&[0x55u8; 32]);
    let pages = 2u32; // 2 pages = 128 KiB

    let manifest = ComponentIntegrityManifest::sign(
        "spsc_shm_module",
        "1.0.0",
        &wasm,
        &signing_key,
        5_000_000,
        pages,
    );

    let runtime = WasmtimeSecureRuntime::new().expect("runtime");
    let mut instance = runtime.instantiate_verified_module(&wasm, &manifest, pages)
        .expect("instantiate");

    // Create lock-free SPSC ring buffer mapped directly over Wasmtime linear memory
    let ring = instance.create_spsc_ring_buffer(64, 16, 64, 1001)
        .expect("create SPSC ring in linear memory");

    assert_eq!(ring.descriptor().view_id, 1001);
    assert_eq!(ring.descriptor().ring_capacity, 16);
    assert!(ring.is_empty());

    let payload = b"WASM_GUEST_HOST_FAST_PATH_CONDUIT_MESSAGE";
    let hdr = SpscRingElementHeader::new(0, payload.len() as u32, 0, 8888, 0);

    ring.try_push(hdr, payload).expect("push into Wasmtime SPSC ring");
    assert_eq!(ring.ring_available(), 1);

    // Pop zero-copy
    let pop_res = ring.pop_zero_copy().expect("zero copy pop from WASM linear memory");
    assert_eq!(pop_res.header.timestamp_ns, 8888);
    assert_eq!(pop_res.payload_slice.len, payload.len() as u64);

    let slice_bytes = unsafe {
        std::slice::from_raw_parts(pop_res.payload_slice.ptr as *const u8, pop_res.payload_slice.len as usize)
    };
    assert_eq!(slice_bytes, payload);
    assert!(ring.is_empty());
}

#[test]
fn test_wasmtime_tampered_module_instantiation_blocked() {
    let mut wasm = generate_metered_wasm_binary();
    let signing_key = SigningKey::from_bytes(&[0x66u8; 32]);

    let manifest = ComponentIntegrityManifest::sign(
        "tamper_proof_module",
        "1.0.0",
        &wasm,
        &signing_key,
        1_000_000,
        2,
    );

    // Tamper with the WASM binary by modifying a byte in the code section
    let code_len = wasm.len();
    wasm[code_len - 1] ^= 0x42;

    let runtime = WasmtimeSecureRuntime::new().expect("runtime");
    let err = runtime.instantiate_verified_module(&wasm, &manifest, 16);

    // Instantiation MUST fail cryptographically before any Wasmtime compilation occurs!
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
}
