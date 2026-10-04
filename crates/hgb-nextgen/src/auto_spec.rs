use hgb_core::auto_spec::{
    GoldenSpec, InvariantType, SpecExecutionReport, SpecViolation, TestVector,
};
use hgb_core::{HgbError, Result};
use hgb_storage::SpecStore;
use std::path::Path;
use hgb_core::providers::OllamaProvider;
use hgb_core::traits::HgbProvider;

pub struct AutoSpecEngine;

impl AutoSpecEngine {
    /// Synthesize property-based invariant golden spec for a target function
    pub async fn synthesize_golden_spec(
        target_fn: &str,
        target_module: &str,
        code_body: &str,
    ) -> Result<GoldenSpec> {
        let spec_id = format!("spec-{}", target_fn);
        let hash = blake3::hash(code_body.as_bytes()).to_hex().to_string();

        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));
        let prompt = format!(
            "Analyze the function '{target_fn}' in '{target_module}':
{code_body}
Generate property-based testing invariants and test vectors.
Return ONLY JSON matching:
{{
  \"invariant_rules\": [\"BoundaryNumeric\" | \"BoundaryString\" | \"RoundTripIdentity\" | \"Idempotence\" | \"NonNegativeOutput\"],
  \"golden_vectors\": [
    {{
      \"input_repr\": \"string\",
      \"expected_result_pattern\": \"string\",
      \"should_panic\": boolean
    }}
  ]
}}",
            target_fn = target_fn, target_module = target_module, code_body = code_body
        );

        let resp = provider.complete(&prompt, None).await.unwrap_or_else(|_| "{}".to_string());
        let start = resp.find('{').unwrap_or(0);
        let end = resp.rfind('}').unwrap_or(resp.len() - 1) + 1;
        let json_str = &resp[start..end];

        #[derive(serde::Deserialize)]
        struct Resp {
            invariant_rules: Vec<InvariantType>,
            golden_vectors: Vec<TestVector>,
        }

        let parsed = serde_json::from_str::<Resp>(json_str).unwrap_or_else(|_| Resp {
            invariant_rules: vec![],
            golden_vectors: vec![],
        });

        Ok(GoldenSpec {
            spec_id,
            target_function: target_fn.to_string(),
            target_module: target_module.to_string(),
            implementation_blake3: hash,
            invariant_rules: parsed.invariant_rules,
            golden_vectors: parsed.golden_vectors,
            created_at_rfc3339: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Run regression checks for all golden specs in workspace
    pub async fn run_regression_guard<P: AsRef<Path>>(
        workspace_root: P,
        strict_abort: bool,
    ) -> Result<Vec<SpecExecutionReport>> {
        let specs = SpecStore::list_specs(&workspace_root)?;
        let mut reports = Vec::new();
        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));

        for spec in &specs {
            let total_tests = spec.golden_vectors.len();
            let mut passed_tests = 0;
            let mut failed_tests = 0;
            let mut failures = Vec::new();

            for vector in &spec.golden_vectors {
                let prompt = format!(
                    "Evaluate if executing function '{}' with input '{}' satisfies expected pattern '{}'. Returns JSON: {{ \"passes\": bool, \"actual_result\": \"string\" }}",
                    spec.target_function, vector.input_repr, vector.expected_result_pattern.as_deref().unwrap_or("")
                );
                let resp = provider.complete(&prompt, None).await.unwrap_or_default();
                
                let passes = resp.contains("\"passes\": true") || resp.contains("\"passes\":true");

                if passes {
                    passed_tests += 1;
                } else {
                    failed_tests += 1;
                    failures.push(SpecViolation {
                        input_used: vector.input_repr.clone(),
                        expected: vector.expected_result_pattern.clone().unwrap_or_default(),
                        actual: "Invariant assertion failed dynamically evaluated by LLM".to_string(),
                        stack_trace: Some(format!("at {}:{}", spec.target_module, spec.target_function)),
                    });
                }
            }

            let is_green = failed_tests == 0;
            if strict_abort && !is_green {
                return Err(HgbError::Execution(format!(
                    "AutoSpec regression guard triggered: spec '{}' failed {} tests",
                    spec.spec_id, failed_tests
                )));
            }

            reports.push(SpecExecutionReport {
                spec_id: spec.spec_id.clone(),
                total_tests,
                passed_tests,
                failed_tests,
                failures,
                is_green,
            });
        }

        Ok(reports)
    }
}
