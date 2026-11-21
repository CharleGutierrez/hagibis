//! # Comprehensive Formal Verification & Proof Engine Integration Tests
//!
//! Validates:
//! 1. MemorySafety and NullPointerFreedom: raw pointer dereference without null check is refuted with counterexample.
//! 2. BoundsPreserved: unverified buffer indexing is refuted; guarded indexing is proven.
//! 3. NoArithmeticPanic: division-by-zero risk is refuted; checked denominators are proven.
//! 4. Unguarded .unwrap() panics: detected and refuted under strict formal config.
//! 5. Patch diff verification: confirms fixed slicing bug discharges all VCs.
//! 6. Automated Proof Harness Synthesis: generates executable property contracts.
//! 7. Mathematical soundness of Symbolic Interval Arithmetic domain.

use std::path::PathBuf;
use tempfile::NamedTempFile;

use hgb_compiler::formal_verification::{
    CodeInvariant, CounterExample, FormalVerificationEngine, Interval, ProofMethod,
    VerificationConfig, VerificationOutcome,
};

#[test]
fn test_interval_arithmetic_soundness() {
    let i1 = Interval::new(10, 20);
    let i2 = Interval::new(5, 10);

    // Addition: [15, 30]
    let add = i1.add(&i2);
    assert_eq!(add.min, 15);
    assert_eq!(add.max, 30);

    // Subtraction: [0, 15]
    let sub = i1.sub(&i2);
    assert_eq!(sub.min, 0);
    assert_eq!(sub.max, 15);

    // Multiplication: [50, 200]
    let mul = i1.mul(&i2);
    assert_eq!(mul.min, 50);
    assert_eq!(mul.max, 200);

    // Division: [1, 4]
    let div = i1.div(&i2).expect("Division must succeed for non-zero interval");
    assert_eq!(div.min, 1);
    assert_eq!(div.max, 4);

    // Division by interval containing zero must fail
    let zero_interval = Interval::new(-2, 5);
    assert!(zero_interval.contains_zero());
    assert!(i1.div(&zero_interval).is_err());
}

#[test]
fn test_null_pointer_dereference_refuted() {
    let engine = FormalVerificationEngine::default();

    let unsafe_source = r#"
        pub unsafe fn read_kernel_header(raw_ptr: *const u32) -> u32 {
            // Bug: raw pointer dereferenced directly without nullness check
            *raw_ptr
        }
    "#;

    let report = engine.verify_source(unsafe_source, "read_kernel_header");
    assert_eq!(report.outcome, VerificationOutcome::Refuted);
    assert!(!report.is_safe);

    let violation = report
        .violations
        .iter()
        .find(|v| v.invariant == CodeInvariant::NullPointerFreedom)
        .expect("Must identify NullPointerFreedom violation");

    assert!(violation.violation_reason.contains("raw_ptr"));
    let counterexample = violation.counterexample.as_ref().expect("Must yield counterexample");
    assert_eq!(counterexample.variable_assignments.get("raw_ptr").unwrap(), "0x0 (NULL)");
}

#[test]
fn test_guarded_null_pointer_proven() {
    let engine = FormalVerificationEngine::default();

    let safe_source = r#"
        pub fn read_kernel_header(raw_ptr: *const u32) -> Option<u32> {
            if !raw_ptr.is_null() {
                Some(unsafe { *raw_ptr })
            } else {
                None
            }
        }
    "#;

    let report = engine.verify_source(safe_source, "read_kernel_header_safe");
    assert_eq!(report.outcome, VerificationOutcome::Proven);
    assert!(report.is_safe);
    assert!(report.violations.is_empty());
}

#[test]
fn test_bounds_preservation_unbounded_index_refuted() {
    let engine = FormalVerificationEngine::default();

    let buggy_source = r#"
        pub fn get_sample(buffer: &[f32], idx: usize) -> f32 {
            // Bug: direct indexing without checking idx < buffer.len()
            buffer[idx]
        }
    "#;

    let report = engine.verify_source(buggy_source, "get_sample");
    assert_eq!(report.outcome, VerificationOutcome::Refuted);
    assert!(!report.is_safe);

    let bounds_violation = report
        .violations
        .iter()
        .find(|v| v.invariant == CodeInvariant::BoundsPreserved)
        .expect("Must detect BoundsPreserved violation");

    assert!(bounds_violation.violation_reason.contains("buffer[idx]"));
    let cx = bounds_violation.counterexample.as_ref().unwrap();
    assert_eq!(cx.variable_assignments.get("idx").unwrap(), "buffer.len()");
}

#[test]
fn test_bounds_preservation_guarded_proven() {
    let engine = FormalVerificationEngine::default();

    let safe_source = r#"
        pub fn get_sample(buffer: &[f32], idx: usize) -> Option<f32> {
            if idx < buffer.len() {
                Some(buffer[idx])
            } else {
                None
            }
        }
    "#;

    let report = engine.verify_source(safe_source, "get_sample_guarded");
    assert_eq!(report.outcome, VerificationOutcome::Proven);
    assert!(report.is_safe);
    assert!(report.violations.is_empty());
}

#[test]
fn test_division_by_zero_refuted() {
    let engine = FormalVerificationEngine::default();

    let div_source = r#"
        pub fn compute_ratio(numerator: i64, denominator: i64) -> i64 {
            // Bug: denominator can be 0 at runtime
            numerator / denominator
        }
    "#;

    let report = engine.verify_source(div_source, "compute_ratio");
    assert_eq!(report.outcome, VerificationOutcome::Refuted);
    assert!(!report.is_safe);

    let arith_violation = report
        .violations
        .iter()
        .find(|v| v.invariant == CodeInvariant::NoArithmeticPanic)
        .expect("Must detect division by zero risk");

    assert!(arith_violation.violation_reason.contains("denominator"));
    let cx = arith_violation.counterexample.as_ref().unwrap();
    assert_eq!(cx.variable_assignments.get("denominator").unwrap(), "0");
}

#[test]
fn test_guarded_division_proven() {
    let engine = FormalVerificationEngine::default();

    let safe_div_source = r#"
        pub fn compute_ratio(numerator: i64, denominator: i64) -> Result<i64, &'static str> {
            if denominator != 0 {
                Ok(numerator / denominator)
            } else {
                Err("division by zero")
            }
        }
    "#;

    let report = engine.verify_source(safe_div_source, "compute_ratio_safe");
    assert_eq!(report.outcome, VerificationOutcome::Proven);
    assert!(report.is_safe);
}

#[test]
fn test_unguarded_unwrap_panic_refuted() {
    let engine = FormalVerificationEngine::default();

    let unwrap_source = r#"
        pub fn extract_payload(opt: Option<String>) -> String {
            // Bug: unguarded unwrap causes panic on None
            opt.unwrap()
        }
    "#;

    let report = engine.verify_source(unwrap_source, "extract_payload");
    assert_eq!(report.outcome, VerificationOutcome::Refuted);
    assert!(!report.is_safe);

    let unwrap_violation = report
        .violations
        .iter()
        .find(|v| v.violation_reason.contains("unwrap()"))
        .expect("Must detect unwrap panic risk");

    assert_eq!(unwrap_violation.invariant, CodeInvariant::NoArithmeticPanic);
}

#[test]
fn test_patch_diff_verification_healing() {
    let engine = FormalVerificationEngine::default();

    let original = r#"
        pub fn compute_centroids(k: usize) {
            let mut centroids = Vec::new();
            for _ in 1..k {
                if centroids.len() < _ + 1 {
                    centroids.push(42);
                }
            }
        }
    "#;

    let repaired = r#"
        pub fn compute_centroids(k: usize) {
            let mut centroids = Vec::new();
            for step in 1..k {
                if centroids.len() < step + 1 {
                    centroids.push(42);
                }
            }
        }
    "#;

    let report = engine.verify_patch(original, repaired, "compute_centroids");
    assert_eq!(report.outcome, VerificationOutcome::Proven);
    assert!(report.is_safe);
    assert!(report.violations.is_empty());
}

#[test]
fn test_proof_harness_synthesis() {
    let engine = FormalVerificationEngine::default();

    let source = r#"
        pub fn compute_cluster_centroid(items: &[f32]) -> f32 {
            let total = 100.0;
            total
        }
    "#;

    let report = engine.verify_source(source, "compute_cluster_centroid");
    let harness = report.synthesized_harness.expect("Harness must be synthesized");

    assert!(harness.contains("formal_verification_harness"));
    assert!(harness.contains("verify_compute_cluster_centroid_bounds_preservation"));
    assert!(harness.contains("verify_compute_cluster_centroid_arithmetic_safety"));
    assert!(harness.contains("verify_compute_cluster_centroid_null_pointer_freedom"));
}
