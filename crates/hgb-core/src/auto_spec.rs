use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantType {
    BoundaryNumeric,
    BoundaryString,
    RoundTripIdentity,
    Idempotence,
    NonNegativeOutput,
}

impl std::fmt::Display for InvariantType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BoundaryNumeric => write!(f, "BoundaryNumeric"),
            Self::BoundaryString => write!(f, "BoundaryString"),
            Self::RoundTripIdentity => write!(f, "RoundTripIdentity"),
            Self::Idempotence => write!(f, "Idempotence"),
            Self::NonNegativeOutput => write!(f, "NonNegativeOutput"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestVector {
    pub input_repr: String,
    pub expected_result_pattern: Option<String>,
    pub should_panic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenSpec {
    pub spec_id: String,
    pub target_function: String,
    pub target_module: String,
    pub implementation_blake3: String,
    pub invariant_rules: Vec<InvariantType>,
    pub golden_vectors: Vec<TestVector>,
    pub created_at_rfc3339: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecViolation {
    pub input_used: String,
    pub expected: String,
    pub actual: String,
    pub stack_trace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecExecutionReport {
    pub spec_id: String,
    pub total_tests: usize,
    pub passed_tests: usize,
    pub failed_tests: usize,
    pub failures: Vec<SpecViolation>,
    pub is_green: bool,
}
