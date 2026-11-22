//! crates/hgb-autopoietic/tests/test_static_analysis_fuzzer_healer.rs
use std::path::Path;
use tempfile::tempdir;
use hgb_autopoietic::static_analysis_fuzzer_healer::*;

#[test]
fn test_pattern_detection_dangerous_idioms() {
    let source = r#"
        pub unsafe fn raw_access(ptr: *const u8, offset: usize, buffer: &[u32], idx: usize, user_input: &str) {
            let val = *ptr.add(offset);
            let item = buffer[idx];
            println!(user_input);
            let key = std::env::var("UNTRUSTED_KEY").unwrap();
            let num: u32 = key.parse().unwrap();
        }
    "#;

    let diags = StaticAnalysisAuditEngine::scan_source_code_patterns(Path::new("src/dangerous.rs"), source).unwrap();
    assert!(diags.len() >= 4, "Expected >=4 detected vulnerabilities, found {}", diags.len());

    assert!(diags.iter().any(|d| d.cwe == CweCategory::Cwe476NullPointerDereference));
    assert!(diags.iter().any(|d| d.cwe == CweCategory::Cwe120BufferCopyWithoutCheckingSize));
    assert!(diags.iter().any(|d| d.cwe == CweCategory::Cwe134UseOfExternallyControlledFormatString));
    assert!(diags.iter().any(|d| d.cwe == CweCategory::Cwe703ImproperCheckOrHandlingOfExceptionalConditions));
}

#[test]
fn test_cargo_audit_and_clippy_ingestion() {
    let audit_json = r#"{
        "vulnerabilities": {
            "list": [
                {
                    "advisory": {
                        "id": "RUSTSEC-2023-0001",
                        "package": "hyper",
                        "title": "Buffer overflow in header parsing",
                        "description": "Vulnerability details",
                        "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
                        "cwe_ids": ["CWE-120"]
                    }
                }
            ]
        }
    }"#;

    let audit_diags = StaticAnalysisAuditEngine::parse_cargo_audit_json(audit_json).unwrap();
    assert_eq!(audit_diags.len(), 1);
    assert_eq!(audit_diags[0].severity, VulnerabilitySeverity::Critical);
    assert_eq!(audit_diags[0].cwe, CweCategory::Cwe120BufferCopyWithoutCheckingSize);

    let clippy_stream = r#"{"reason":"compiler-message","message":{"code":{"code":"clippy::indexing_slicing"},"level":"warning","message":"indexing may panic","rendered":"warning: indexing may panic","spans":[{"file_name":"src/main.rs","line_start":10,"line_end":10,"column_start":5,"column_end":15,"is_primary":true}]}}"#;
    let clippy_diags = StaticAnalysisAuditEngine::parse_clippy_json_stream(clippy_stream).unwrap();
    assert_eq!(clippy_diags.len(), 1);
    assert_eq!(clippy_diags[0].cwe, CweCategory::Cwe120BufferCopyWithoutCheckingSize);
}

#[test]
fn test_defensive_patch_synthesis_and_invariants() {
    let source = "pub fn compute(arr: &[u32], idx: usize) -> Result<u32, hgb_core::error::HgbError> {\n    let val = arr[idx];\n    Ok(val)\n}";
    let diag = VulnerabilityDiagnostic::new(
        DiagnosticSource::EmbeddedScanner,
        VulnerabilitySeverity::High,
        CweCategory::Cwe120BufferCopyWithoutCheckingSize,
        "Unchecked slice indexing",
        "arr[idx]",
        Path::new("src/compute.rs"),
        2,
    );

    let candidate = AutonomousSecurityPatchGenerator::synthesize_patch(source, &diag).unwrap();
    assert!(!candidate.patch_diff.is_empty());
    assert_ne!(candidate.pre_patch_ast_hash, candidate.post_patch_ast_hash);
    assert!(candidate.patched_code.contains(".get(idx)"));

    let verified = AutonomousSecurityPatchGenerator::verify_safety_invariants(&candidate.patched_code, &candidate.safety_invariants, &candidate.target_file).unwrap();
    assert!(verified);
}

#[test]
fn test_otel_cryptographic_ledger_integrity() {
    let temp = tempdir().unwrap();
    let sink = temp.path().join("otel.jsonl");
    let ledger = temp.path().join("audit_ledger.jsonl");
    let key = [42u8; 32];

    let reporter = OtelSecurityAuditReporter::new(&sink, &ledger, key);
    let diag = VulnerabilityDiagnostic::new(
        DiagnosticSource::EmbeddedScanner,
        VulnerabilitySeverity::High,
        CweCategory::Cwe190IntegerOverflow,
        "Integer overflow",
        "a + b",
        Path::new("src/math.rs"),
        5,
    );

    reporter.report_audit_scan(&[diag], 1000).unwrap();
    assert!(reporter.verify_ledger_chain(&key).unwrap());

    // Verify tampering detection
    let tampered = std::fs::read_to_string(&ledger).unwrap().replace("CWE-190", "CWE-999");
    std::fs::write(&ledger, tampered).unwrap();
    assert!(reporter.verify_ledger_chain(&key).is_err());
}