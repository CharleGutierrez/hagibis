//! test_semantic_ast_synthesizer.rs
//! Comprehensive Unit & Proptest Property Test Suite for Semantic AST Diffing & Synthesis
//!
//! Test Coverage:
//! 1. Structured AST Diffing (`AstDiffer`): detects additions, deletions, modifications,
//!    visibility narrowing, and public API contract breaks.
//! 2. Semantic AST Transformations (`SemanticAstSynthesizer`):
//!    - E0308: `&str` to `String` (.to_string())
//!    - E0596: Mutability injection and redundant `mut` stripping
//!    - Loop placeholder `_` identifier correction
//!    - Namespace collision aliasing (`as BridgeTensorDataType`)
//!    - Statement injection at function boundaries
//! 3. Property-Based Testing (`proptest` / `PropertyTestEngine`):
//!    - `syntactically_valid`: healed code must parse into valid syn::File with 0 syntax errors
//!    - `non_empty_tokens`: token count must remain positive and non-empty
//!    - `public_api_preserved`: public function signatures and types must not regress or disappear

#[cfg(test)]
mod tests {
    use hgb_core::semantic_ast_synthesizer::{
        AstDiffer, CodeChange, DiagnosticSite, InjectionLocation, MutationPatch,
        PropertyInvariant, PropertyTestEngine, SemanticAstSynthesizer, StructuredAstDiff,
    };
    use proptest::prelude::*;

    #[test]
    fn test_ast_diff_detects_additions_and_removals() {
        let original = r#"
            pub fn alpha() -> usize { 1 }
            fn beta() -> usize { 2 }
        "#;

        let modified = r#"
            pub fn alpha() -> usize { 1 }
            pub fn gamma() -> usize { 3 }
        "#;

        let diff = AstDiffer::diff(original, modified).expect("diff should succeed");
        assert_eq!(diff.added_items.len(), 1);
        assert_eq!(diff.removed_items.len(), 1);
        assert_eq!(diff.added_items[0].ident, "gamma");
        assert_eq!(diff.removed_items[0].ident, "beta");
        assert!(!diff.public_api_broken);
    }

    #[test]
    fn test_ast_diff_detects_public_api_breakage() {
        let original = r#"
            pub fn public_api_endpoint(x: u32) -> u32 { x + 1 }
        "#;

        // Narrowing visibility from pub to private breaks public API contract!
        let modified = r#"
            fn public_api_endpoint(x: u32) -> u32 { x + 1 }
        "#;

        let diff = AstDiffer::diff(original, modified).expect("diff should succeed");
        assert!(diff.public_api_broken);
        assert!(diff.has_changes());
    }

    #[test]
    fn test_e0308_type_mismatch_str_to_string_repair() {
        let input = r#"
            pub fn build_id() -> String {
                let id = "model_alpha";
                id
            }
        "#;

        let site = DiagnosticSite {
            file_path: "src/lib.rs".to_string(),
            error_code: "E0308".to_string(),
            line: 3,
            column: 17,
            message: "expected `String`, found `&str`".to_string(),
        };

        let patch = SemanticAstSynthesizer::synthesize_patch(input, &site)
            .expect("synthesis should succeed");

        assert!(patch.healed_source.contains(".to_string()"));
        assert_eq!(patch.error_code, "E0308");

        // Verify with property invariant engine
        let engine = PropertyTestEngine::standard_suite();
        let report = engine.verify(input, &patch.healed_source);
        assert!(report.passed);
    }

    #[test]
    fn test_e0596_mutability_injection_and_stripping() {
        // Subcase A: Mutability Injection
        let input_immut = r#"
            pub fn mutate_buffer() {
                let buf = Vec::new();
                buf.push(42);
            }
        "#;
        let site_inject = DiagnosticSite {
            file_path: "src/lib.rs".to_string(),
            error_code: "E0596".to_string(),
            line: 3,
            column: 17,
            message: "cannot borrow `buf` as mutable".to_string(),
        };
        let patch_inject = SemanticAstSynthesizer::synthesize_patch(input_immut, &site_inject)
            .expect("should inject mut");
        assert!(patch_inject.healed_source.contains("let mut buf = Vec::new();"));

        // Subcase B: Redundant Mut Stripping
        let input_mut = r#"
            pub fn read_buffer() -> usize {
                let mut buf = 42;
                buf
            }
        "#;
        let site_strip = DiagnosticSite {
            file_path: "src/lib.rs".to_string(),
            error_code: "E0596".to_string(),
            line: 3,
            column: 17,
            message: "variable does not need to be mutable".to_string(),
        };
        let patch_strip = SemanticAstSynthesizer::synthesize_patch(input_mut, &site_strip)
            .expect("should strip mut");
        assert!(patch_strip.healed_source.contains("let buf = 42;"));
    }

    #[test]
    fn test_loop_identifier_underscore_repair() {
        let broken = r#"
            pub fn cluster() {
                let k = 5;
                for _ in 1..k {
                    if centroids.len() < _ + 1 {
                        continue;
                    }
                }
            }
        "#;

        let site = DiagnosticSite {
            file_path: "src/lib.rs".to_string(),
            error_code: "E0425".to_string(),
            line: 4,
            column: 42,
            message: "in expressions, `_` can only be used on the left-hand side of an assignment".to_string(),
        };

        let patch = SemanticAstSynthesizer::synthesize_patch(broken, &site)
            .expect("should repair loop identifier");

        assert!(patch.healed_source.contains("for step in 1..k {"));
        assert!(patch.healed_source.contains("centroids.len() < step + 1"));
    }

    #[test]
    fn test_namespace_import_collision_aliasing() {
        let collision_source = r#"
            use crate::engine::{PinnedTensorDescriptor, TensorDataType, KernelType};
        "#;

        let site = DiagnosticSite {
            file_path: "src/lib.rs".to_string(),
            error_code: "E0252".to_string(),
            line: 2,
            column: 10,
            message: "the name `TensorDataType` is defined multiple times".to_string(),
        };

        let patch = SemanticAstSynthesizer::synthesize_patch(collision_source, &site)
            .expect("should alias duplicate import");

        assert!(patch.healed_source.contains("TensorDataType as BridgeTensorDataType"));
    }

    #[test]
    fn test_statement_injection() {
        let code = r#"
            fn process() {
                let x = 1;
            }
        "#;

        let injected = SemanticAstSynthesizer::inject_statement(
            code,
            "process",
            "log::info!(\"enter\");",
            InjectionLocation::BeginningOfFunction,
        ).expect("injection should succeed");

        assert!(injected.contains("log::info!(\"enter\");"));
        assert!(syn::parse_file(&injected).is_ok());
    }

    // =========================================================================
    // Proptest Property Invariant Suite
    // =========================================================================
    proptest! {
        #[test]
        fn prop_syntactically_valid_code_parses_cleanly(s in "\\PC*") {
            let wrapped = format!("pub fn generated() {{ let _val = \"{}\"; }}", s.replace('"', "\\\"").replace('\\', "\\\\"));
            let is_valid = PropertyInvariant::syntactically_valid().check("", &wrapped);
            prop_assert!(is_valid);
        }

        #[test]
        fn prop_non_empty_tokens_invariant(s in "[a-zA-Z_][a-zA-Z0-9_]{1,15}") {
            let code = format!("pub fn func_{}() -> u32 {{ 42 }}", s);
            let check = PropertyInvariant::non_empty_tokens().check("", &code);
            prop_assert!(check);
        }

        #[test]
        fn prop_public_api_preservation_under_internal_transforms(val in 0u32..1000) {
            let orig = "pub fn calculate_hash() -> u32 { 10 }";
            let transformed = format!("pub fn calculate_hash() -> u32 {{ let _temp = {}; 10 }}", val);
            let check = PropertyInvariant::public_api_preserved().check(orig, &transformed);
            prop_assert!(check);
        }
    }
}
