use hgb_core::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum FuzzViolation {
    Panic(String),
    SecurityInjectionDetected(String),
    InvariantFailed(String),
}

pub struct AgenticFuzzEngine;

impl AgenticFuzzEngine {
    pub fn generate_mutations(base: &str) -> Vec<String> {
        vec![
            base.to_string(),
            "".to_string(),
            "0".to_string(),
            "-1".to_string(),
            "999999999999999999".to_string(),
            "NaN".to_string(),
            "' OR 1=1 --".to_string(),
            "<script>alert(1)</script>".to_string(),
            "\u{200B}\u{200C}admin".to_string(),
            "%s%s%s%s%n".to_string(),
            "../../../../etc/passwd".to_string(),
            "\0nullbyte".to_string(),
        ]
    }

    pub fn fuzz_target<F>(base_input: &str, mut test_fn: F) -> Vec<FuzzViolation>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let mut violations = Vec::new();
        let mutations = Self::generate_mutations(base_input);

        for input in mutations {
            if let Err(e) = test_fn(&input) {
                violations.push(FuzzViolation::InvariantFailed(format!("Input '{}' failed: {}", input, e)));
            }
        }
        violations
    }
}
