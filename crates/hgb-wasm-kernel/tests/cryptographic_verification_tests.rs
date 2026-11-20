//! # Strict Cryptographic WIT Validation Tests (BLAKE3 + Ed25519)
//!
//! Verifies BLAKE3 checksum computation, Ed25519 digital signature creation and verification,
//! and strict rejection of tampered WASM binaries before instantiation.

use ed25519_dalek::{Signer, SigningKey};
use hgb_wasm_kernel::canonical_abi_shm_views::{
    AbiError, ComponentIntegrityManifest, CryptographicVerificationOutcome, CryptographicWitValidator,
};

fn generate_test_wasm_payload() -> Vec<u8> {
    // Valid standard WebAssembly module (8-byte header: \0asm + version 1)
    vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
}

#[test]
fn test_cryptographic_validator_blake3_digest() {
    let payload = b"HAGIBIS_SOVEREIGN_SYSTEMS_PAYLOAD_2026";
    let computed_digest = CryptographicWitValidator::compute_blake3(payload);
    let reference_digest = *blake3::hash(payload).as_bytes();

    assert_eq!(computed_digest, reference_digest);
    assert_ne!(computed_digest, [0u8; 32]);
}

#[test]
fn test_cryptographic_validator_ed25519_sign_and_verify() {
    let secret = [0x5au8; 32];
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let pubkey_bytes = verifying_key.to_bytes();

    let message = b"CRITICAL_MICROKERNEL_ABI_DISPATCH_TRANSACTION";
    let signature = signing_key.sign(message);
    let sig_bytes = signature.to_bytes();

    let verified = CryptographicWitValidator::verify_ed25519_signature(message, &sig_bytes, &pubkey_bytes)
        .expect("verify signature");
    assert!(verified);

    // Corrupted message must fail
    let corrupted_msg = b"CRITICAL_MICROKERNEL_ABI_DISPATCH_TAMPERED";
    let err = CryptographicWitValidator::verify_ed25519_signature(corrupted_msg, &sig_bytes, &pubkey_bytes);
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
}

#[test]
fn test_cryptographic_validator_full_payload_validation_success() {
    let wasm = generate_test_wasm_payload();
    let secret = [0x42u8; 32];
    let signing_key = SigningKey::from_bytes(&secret);

    let manifest = ComponentIntegrityManifest::sign(
        "vector_math_accelerator",
        "1.0.0",
        &wasm,
        &signing_key,
        10_000_000,
        16,
    );

    assert_eq!(manifest.component_name, "vector_math_accelerator");
    assert_eq!(manifest.payload_byte_length, 8);
    assert_eq!(manifest.max_memory_pages, 16);

    let outcome = CryptographicWitValidator::validate_component_payload(&wasm, &manifest)
        .expect("validation should succeed");

    assert!(outcome.is_valid);
    assert!(outcome.signature_verified);
    assert_eq!(outcome.computed_blake3, manifest.blake3_digest);
    assert!(outcome.error_diagnostic.is_none());
    assert!(outcome.verified_timestamp_ns > 0);
}

#[test]
fn test_cryptographic_validator_tampered_payload_rejection() {
    let mut wasm = generate_test_wasm_payload();
    let secret = [0x33u8; 32];
    let signing_key = SigningKey::from_bytes(&secret);

    let manifest = ComponentIntegrityManifest::sign(
        "sandboxed_tool",
        "0.1.0",
        &wasm,
        &signing_key,
        5_000_000,
        32,
    );

    // Tamper with one byte in the payload
    wasm[7] ^= 0x01;

    let err = CryptographicWitValidator::validate_component_payload(&wasm, &manifest);
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
    if let Err(AbiError::CryptographicValidationFailed { reason }) = err {
        assert!(reason.contains("BLAKE3 checksum mismatch"), "Error should report checksum mismatch: {}", reason);
    }
}

#[test]
fn test_cryptographic_validator_tampered_signature_rejection() {
    let wasm = generate_test_wasm_payload();
    let secret = [0x77u8; 32];
    let signing_key = SigningKey::from_bytes(&secret);

    let mut manifest = ComponentIntegrityManifest::sign(
        "secure_gateway",
        "2.0.0",
        &wasm,
        &signing_key,
        15_000_000,
        64,
    );

    // Flip byte in signature
    manifest.ed25519_signature[10] ^= 0xFF;

    let err = CryptographicWitValidator::validate_component_payload(&wasm, &manifest);
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
    if let Err(AbiError::CryptographicValidationFailed { reason }) = err {
        assert!(reason.contains("Ed25519 signature verification failed"), "Error should report signature failure: {}", reason);
    }
}

#[test]
fn test_cryptographic_validator_wrong_public_key_rejection() {
    let wasm = generate_test_wasm_payload();
    let key1 = SigningKey::from_bytes(&[0x11u8; 32]);
    let key2 = SigningKey::from_bytes(&[0x22u8; 32]);

    let mut manifest = ComponentIntegrityManifest::sign(
        "auth_filter",
        "1.1.0",
        &wasm,
        &key1,
        2_000_000,
        8,
    );

    // Replace public key with key2's public key
    manifest.ed25519_public_key = key2.verifying_key().to_bytes();

    let err = CryptographicWitValidator::validate_component_payload(&wasm, &manifest);
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
}

#[test]
fn test_cryptographic_validator_size_mismatch_rejection() {
    let wasm = generate_test_wasm_payload();
    let key = SigningKey::from_bytes(&[0x99u8; 32]);

    let mut manifest = ComponentIntegrityManifest::sign(
        "payload_checker",
        "1.0.0",
        &wasm,
        &key,
        1_000_000,
        4,
    );

    manifest.payload_byte_length = 999;

    let err = CryptographicWitValidator::validate_component_payload(&wasm, &manifest);
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
    if let Err(AbiError::CryptographicValidationFailed { reason }) = err {
        assert!(reason.contains("Payload size mismatch"), "Error should report size mismatch: {}", reason);
    }
}

#[test]
fn test_cryptographic_validator_invalid_magic_rejection() {
    let bad_wasm = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x00, 0x00, 0x00];
    let key = SigningKey::from_bytes(&[0x55u8; 32]);

    let manifest = ComponentIntegrityManifest::sign(
        "bad_magic_component",
        "1.0.0",
        &bad_wasm,
        &key,
        1_000_000,
        4,
    );

    let err = CryptographicWitValidator::validate_component_payload(&bad_wasm, &manifest);
    assert!(matches!(err, Err(AbiError::CryptographicValidationFailed { .. })));
    if let Err(AbiError::CryptographicValidationFailed { reason }) = err {
        assert!(reason.contains("Invalid WASM magic header"), "Error should report bad magic: {}", reason);
    }
}
