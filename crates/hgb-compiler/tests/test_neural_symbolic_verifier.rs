//! Comprehensive Integration Tests for Neural-Symbolic SMT Verification Engine
//! (`test_neural_symbolic_verifier.rs`)
//!
//! Validates:
//! 1. Array bounds invariant proven ($0 \le \text{idx} < \text{len}$) and unguarded bounds refuted.
//! 2. Division-by-zero refuted with concrete counterexample model ($denom = 0$).
//! 3. Standards-compliant SMT-LIB v2.6 syntax generation and roundtrip parser validation.
//! 4. Cryptographic Proof Certificate generation, canonical SHA-256 signature sealing, and tamper rejection.
//! 5. Native embedded Simplex linear arithmetic solver solving systems of inequalities without external binaries.
//! 6. Raw pointer non-null dereference and loop convergence invariant verification.

use std::collections::BTreeMap;
use hgb_compiler::neural_symbolic_verifier::{
    CounterexampleModel, InvariantProofStatus, LinearExpr, NeuralSymbolicVerifier,
    NeuralSymbolicVerifierConfig, RelOp, SimplexSolver, SmtBenchmark, SmtError, SmtExpr,
    SmtInvariantKind, SmtLogic, SmtProofCertificate, SmtSolverBackend, SmtSolverMetrics,
    SmtSort, SmtValue, SmtVerificationCondition,
};

#[test]
fn test_array_bounds_invariant_proven() {
    let verifier = NeuralSymbolicVerifier::new(SmtSolverBackend::EmbeddedNative);

    // Case A: Guarded array index (0 <= idx < len) -> PROVEN
    let cert_guarded = verifier
        .prove_bounds_preservation("idx", "len", true, true)
        .expect("Verification must succeed");

    assert_eq!(
        cert_guarded.status,
        InvariantProofStatus::ProvenUnsat,
        "Guarded array indexing must be mathematically proven UNSAT for negation"
    );
    assert!(
        cert_guarded.counterexample.is_none(),
        "Proven theorem must have no counterexample"
    );
    assert!(
        cert_guarded.verify_signature(),
        "Proof certificate SHA-256 signature must be mathematically valid"
    );

    // Case B: Unguarded upper bound (idx >= len possible) -> REFUTED with counterexample
    let cert_unguarded = verifier
        .prove_bounds_preservation("idx", "len", false, true)
        .expect("Verification must succeed");

    assert_eq!(
        cert_unguarded.status,
        InvariantProofStatus::RefutedSatWithCounterexample,
        "Unguarded array index must be refuted by solver"
    );
    assert!(
        cert_unguarded.counterexample.is_some(),
        "Refuted verification must yield concrete counterexample model"
    );

    let ce = cert_unguarded.counterexample.as_ref().unwrap();
    assert!(
        ce.bindings.contains_key("idx"),
        "Counterexample must bind 'idx'"
    );
    assert!(
        ce.bindings.contains_key("len"),
        "Counterexample must bind 'len'"
    );
    assert!(
        cert_unguarded.verify_signature(),
        "Refutation certificate signature must be cryptographically valid"
    );
}

#[test]
fn test_division_by_zero_refuted_with_counterexample_model() {
    let verifier = NeuralSymbolicVerifier::new(SmtSolverBackend::EmbeddedNative);

    // Case A: Unguarded division -> REFUTED with counterexample model
    let cert_unguarded = verifier
        .prove_division_by_zero_freedom("denominator", false)
        .expect("Verification query must succeed");

    assert_eq!(
        cert_unguarded.status,
        InvariantProofStatus::RefutedSatWithCounterexample,
        "Unguarded denominator must be refuted as potential divide-by-zero"
    );
    assert!(
        cert_unguarded.counterexample.is_some(),
        "Counterexample model must be produced"
    );

    let ce = cert_unguarded.counterexample.as_ref().unwrap();
    if let Some(SmtValue::Int(denom_val)) = ce.bindings.get("denominator") {
        assert!(
            *denom_val <= 0,
            "Counterexample denominator must demonstrate unsafe divisor <= 0, got {}",
            denom_val
        );
    }
    assert!(
        cert_unguarded.verify_signature(),
        "Proof certificate signature must verify"
    );

    // Case B: Guarded division (denominator >= 1) -> PROVEN
    let cert_guarded = verifier
        .prove_division_by_zero_freedom("denominator", true)
        .expect("Guarded division query must succeed");

    assert_eq!(
        cert_guarded.status,
        InvariantProofStatus::ProvenUnsat,
        "Guarded divisor must be proven safe"
    );
    assert!(cert_guarded.counterexample.is_none());
    assert!(cert_guarded.verify_signature());
}

#[test]
fn test_smtlib2_syntax_generation_and_roundtrip_validation() {
    // 1. Construct SMT-LIB2 benchmark for QF_LIA
    let mut bm = SmtBenchmark::new(SmtLogic::QfLia);
    bm.declare_var("x", SmtSort::Int);
    bm.declare_var("y", SmtSort::Int);
    bm.declare_var("limit", SmtSort::Int);

    bm.assert_formula(SmtExpr::ge(SmtExpr::var("x"), SmtExpr::int(0)));
    bm.assert_formula(SmtExpr::lt(SmtExpr::var("x"), SmtExpr::var("limit")));
    bm.assert_formula(SmtExpr::eq(
        SmtExpr::var("y"),
        SmtExpr::add(vec![SmtExpr::var("x"), SmtExpr::int(1)]),
    ));

    let smt2_script = bm.to_smt2_script();

    // Verify standards-compliant SMT-LIB v2.6 syntax elements:
    assert!(
        smt2_script.contains("(set-logic QF_LIA)"),
        "Must specify standard logic header"
    );
    assert!(
        smt2_script.contains("(declare-const x Int)"),
        "Must declare integer const x"
    );
    assert!(
        smt2_script.contains("(declare-const y Int)"),
        "Must declare integer const y"
    );
    assert!(
        smt2_script.contains("(assert (>= x 0))"),
        "Must format >= expression correctly"
    );
    assert!(
        smt2_script.contains("(assert (< x limit))"),
        "Must format < expression correctly"
    );
    assert!(
        smt2_script.contains("(check-sat)"),
        "Must include check-sat command"
    );
    assert!(
        smt2_script.contains("(get-model)"),
        "Must request model production"
    );

    // 2. Test roundtrip parsing from SMT-LIB2 script
    let parsed_bm = SmtBenchmark::from_smt2_script(&smt2_script)
        .expect("SMT-LIB2 script must parse cleanly");

    assert_eq!(parsed_bm.logic, SmtLogic::QfLia);
    assert_eq!(parsed_bm.declarations.get("x"), Some(&SmtSort::Int));
    assert_eq!(parsed_bm.declarations.get("y"), Some(&SmtSort::Int));
    assert_eq!(parsed_bm.declarations.get("limit"), Some(&SmtSort::Int));
    assert_eq!(parsed_bm.assertions.len(), 3);
    assert!(parsed_bm.check_sat);

    // 3. Test QF_BV logic syntax
    let mut bv_bm = SmtBenchmark::new(SmtLogic::QfBv);
    bv_bm.declare_var("reg_a", SmtSort::BitVec(32));
    bv_bm.declare_var("reg_b", SmtSort::BitVec(32));
    bv_bm.assert_formula(SmtExpr::bv_ult(
        SmtExpr::var("reg_a"),
        SmtExpr::var("reg_b"),
    ));

    let bv_script = bv_bm.to_smt2_script();
    assert!(bv_script.contains("(set-logic QF_BV)"));
    assert!(bv_script.contains("(declare-const reg_a (_ BitVec 32))"));
    assert!(bv_script.contains("(assert (bvult reg_a reg_b))"));
}

#[test]
fn test_cryptographic_certificate_generation_and_verification() {
    let verifier = NeuralSymbolicVerifier::new(SmtSolverBackend::EmbeddedNative);
    let cert = verifier
        .prove_bounds_preservation("offset", "capacity", true, true)
        .expect("Certificate emission must succeed");

    // 1. Verify self-consistency of cryptographic signature
    assert!(
        cert.verify_signature(),
        "Fresh certificate must have valid signature"
    );
    assert!(!cert.certificate_id.is_empty());
    assert_eq!(cert.smtlib2_query_sha256.len(), 64);
    assert_eq!(cert.certificate_signature_sha256.len(), 64);

    // 2. Test tamper resistance: modifying certificate fields must invalidate signature
    let mut tampered_cert = cert.clone();
    tampered_cert.status = InvariantProofStatus::RefutedSatWithCounterexample;
    assert!(
        !tampered_cert.verify_signature(),
        "Tampered proof status must fail cryptographic signature verification"
    );

    let mut tampered_time = cert.clone();
    tampered_time.timestamp_rfc3339 = "2020-01-01T00:00:00Z".to_string();
    assert!(
        !tampered_time.verify_signature(),
        "Tampered timestamp must fail cryptographic signature verification"
    );
}

#[test]
fn test_native_embedded_solver_linear_inequalities() {
    // Test native rational and integer Simplex solver on a system of linear inequalities:
    // x >= 2
    // y >= 3
    // x + y <= 10
    // 2x - y <= 4
    let mut solver = SimplexSolver::new();
    let x_idx = solver.register_var("x", true);
    let y_idx = solver.register_var("y", true);

    // x >= 2 => x - 2 >= 0
    let mut c1 = LinearExpr::from_var("x".into());
    c1.constant = hgb_compiler::neural_symbolic_verifier::Rational::from_int(-2);
    solver.add_linear_constraint(&c1, RelOp::Ge).unwrap();

    // y >= 3 => y - 3 >= 0
    let mut c2 = LinearExpr::from_var("y".into());
    c2.constant = hgb_compiler::neural_symbolic_verifier::Rational::from_int(-3);
    solver.add_linear_constraint(&c2, RelOp::Ge).unwrap();

    // x + y <= 10 => x + y - 10 <= 0
    let mut c3 = LinearExpr::from_var("x".into()).add(&LinearExpr::from_var("y".into()));
    c3.constant = hgb_compiler::neural_symbolic_verifier::Rational::from_int(-10);
    solver.add_linear_constraint(&c3, RelOp::Le).unwrap();

    // 2x - y <= 4 => 2x - y - 4 <= 0
    let mut c4 = LinearExpr::from_var("x".into())
        .mul_scalar(hgb_compiler::neural_symbolic_verifier::Rational::from_int(2))
        .sub(&LinearExpr::from_var("y".into()));
    c4.constant = hgb_compiler::neural_symbolic_verifier::Rational::from_int(-4);
    solver.add_linear_constraint(&c4, RelOp::Le).unwrap();

    let sat = solver.solve_integer(5000).expect("Simplex solve must execute");
    assert!(sat, "System of linear inequalities must be feasible (SAT)");

    let model = solver.extract_model();
    if let (Some(SmtValue::Int(x_val)), Some(SmtValue::Int(y_val))) =
        (model.get("x"), model.get("y"))
    {
        // Verify mathematical constraints hold on concrete integer values:
        assert!(*x_val >= 2, "x >= 2 violated: {}", x_val);
        assert!(*y_val >= 3, "y >= 3 violated: {}", y_val);
        assert!(*x_val + *y_val <= 10, "x + y <= 10 violated: {} + {}", x_val, y_val);
        assert!(
            2 * *x_val - *y_val <= 4,
            "2x - y <= 4 violated: 2*{} - {}",
            x_val,
            y_val
        );
    } else {
        panic!("Model must contain integer bindings for x and y");
    }

    // Test Infeasible system (UNSAT):
    // z >= 5 AND z <= 4
    let mut inf_solver = SimplexSolver::new();
    let _z_idx = inf_solver.register_var("z", true);

    let mut cz1 = LinearExpr::from_var("z".into());
    cz1.constant = hgb_compiler::neural_symbolic_verifier::Rational::from_int(-5);
    inf_solver.add_linear_constraint(&cz1, RelOp::Ge).unwrap();

    let mut cz2 = LinearExpr::from_var("z".into());
    cz2.constant = hgb_compiler::neural_symbolic_verifier::Rational::from_int(-4);
    inf_solver.add_linear_constraint(&cz2, RelOp::Le).unwrap();

    let inf_sat = inf_solver
        .solve_integer(1000)
        .expect("Simplex solve must execute");
    assert!(
        !inf_sat,
        "System z >= 5 and z <= 4 must be mathematically UNSAT"
    );
}

#[test]
fn test_pointer_non_nullness_and_loop_convergence() {
    let verifier = NeuralSymbolicVerifier::new(SmtSolverBackend::EmbeddedNative);

    // 1. Pointer Non-nullness
    let cert_ptr_guarded = verifier
        .prove_pointer_non_nullness("raw_ptr", true)
        .expect("Pointer verification must execute");
    assert_eq!(
        cert_ptr_guarded.status,
        InvariantProofStatus::ProvenUnsat,
        "Guarded pointer dereference must be proven safe"
    );

    let cert_ptr_unguarded = verifier
        .prove_pointer_non_nullness("raw_ptr", false)
        .expect("Pointer verification must execute");
    assert_eq!(
        cert_ptr_unguarded.status,
        InvariantProofStatus::RefutedSatWithCounterexample,
        "Unguarded raw pointer dereference must be refuted"
    );

    // 2. Loop Convergence
    let cert_loop = verifier
        .prove_loop_convergence("k", "bound", 1)
        .expect("Loop convergence proof must execute");
    assert_eq!(
        cert_loop.status,
        InvariantProofStatus::ProvenUnsat,
        "Monotonic variant step=1 must prove loop convergence"
    );
    assert!(cert_loop.verify_signature());
}

#[test]
fn test_code_patch_verification_pipeline() {
    let verifier = NeuralSymbolicVerifier::new(SmtSolverBackend::EmbeddedNative);

    let safe_code = r#"
pub fn safe_buffer_access(data: &[u8], idx: usize, denom: usize) -> u8 {
    if idx < data.len() {
        if denom > 0 {
            let res = data[idx] / denom;
            res
        } else {
            0
        }
    } else {
        0
    }
}
"#;

    let report_safe = verifier
        .verify_code_patch(safe_code, "safe_module.rs")
        .expect("Safe code patch verification must succeed");

    assert!(
        report_safe.is_safe,
        "Properly guarded code patch must be verified safe"
    );
    assert!(!report_safe.overall_sha256.is_empty());

    let unsafe_code = r#"
pub fn unsafe_buffer_access(data: &[u8], idx: usize, denom: usize) -> u8 {
    let val = data[idx];
    let res = val / denom;
    res
}
"#;

    let report_unsafe = verifier
        .verify_code_patch(unsafe_code, "unsafe_module.rs")
        .expect("Unsafe code patch verification must execute");

    assert!(
        !report_unsafe.is_safe,
        "Unguarded indexing and division must be flagged as unsafe"
    );
    assert!(
        report_unsafe
            .certificates
            .iter()
            .any(|c| c.status == InvariantProofStatus::RefutedSatWithCounterexample),
        "At least one certificate must refute unsafe operations"
    );
}
