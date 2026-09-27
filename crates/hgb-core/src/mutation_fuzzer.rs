//! # MutationFuzzer - Anti-Placebo Test Integrity & Mutation Fuzzing
//!
//! Elevates Qodo's test integrity verification. Injects deliberate semantic
//! mutations into production logic to verify whether the AI-generated test suite
//! actually catches regressions or is merely a "placebo" test suite.

use serde::{Deserialize, Serialize};

/// Type of mutation operator injected into production AST/tokens
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MutationOperator {
    InvertEquality,
    InvertComparison,
    InvertBoolean,
    InvertArithmetic,
    InvertLogical,
    InvertReturn,
}

impl MutationOperator {
    pub fn name(&self) -> &'static str {
        match self {
            Self::InvertEquality => "InvertEquality (== <=> !=)",
            Self::InvertComparison => "InvertComparison (> <=> <=, < <=> >=)",
            Self::InvertBoolean => "InvertBoolean (true <=> false)",
            Self::InvertArithmetic => "InvertArithmetic (+ <=> -, * <=> /)",
            Self::InvertLogical => "InvertLogical (&& <=> ||)",
            Self::InvertReturn => "InvertReturn (Ok <=> Err / true <=> false)",
        }
    }
}

/// Candidate mutant to be applied
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutantCandidate {
    pub id: String,
    pub operator: MutationOperator,
    pub line_number: usize,
    pub original_snippet: String,
    pub mutated_snippet: String,
    pub description: String,
}

/// Status of a mutant after running the test suite
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutantStatus {
    /// Test suite failed as expected -> Excellent, the test caught the bug!
    Killed,
    /// Test suite passed despite code being broken -> Placebo / Missing assertion!
    Survived,
    /// Mutant failed to compile
    CompileError,
}

/// Complete report of test suite integrity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationReport {
    pub total_mutants: usize,
    pub killed_mutants: usize,
    pub survived_mutants: usize,
    pub compile_errors: usize,
    pub mutation_score_pct: f64,
    pub integrity_grade: String,
    pub survived_details: Vec<MutantCandidate>,
    pub recommendations: Vec<String>,
}

pub struct MutationFuzzer;

impl MutationFuzzer {
    /// Discover valid mutation candidate points in source code
    pub fn scan_mutants(source: &str) -> Vec<MutantCandidate> {
        let mut candidates = Vec::new();
        let mut id_counter = 1;

        for (idx, line) in source.lines().enumerate() {
            let line_no = idx + 1;
            let trimmed = line.trim();

            // Skip comments and imports
            if trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("#")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("use ")
            {
                continue;
            }

            // 1. Invert Equality: == <-> !=
            if line.contains(" == ") {
                let mutated = line.replacen(" == ", " != ", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-eq-{}", id_counter),
                    operator: MutationOperator::InvertEquality,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Replaced '==' with '!=' equality check".to_string(),
                });
                id_counter += 1;
            } else if line.contains(" != ") {
                let mutated = line.replacen(" != ", " == ", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-eq-{}", id_counter),
                    operator: MutationOperator::InvertEquality,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Replaced '!=' with '==' equality check".to_string(),
                });
                id_counter += 1;
            }

            // 2. Invert Comparison: > <-> <=, < <-> >=
            if line.contains(" > ") {
                let mutated = line.replacen(" > ", " <= ", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-cmp-{}", id_counter),
                    operator: MutationOperator::InvertComparison,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Inverted relational operator '>' to '<='".to_string(),
                });
                id_counter += 1;
            } else if line.contains(" < ") {
                let mutated = line.replacen(" < ", " >= ", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-cmp-{}", id_counter),
                    operator: MutationOperator::InvertComparison,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Inverted relational operator '<' to '>='".to_string(),
                });
                id_counter += 1;
            }

            // 3. Invert Boolean: true <-> false
            if line.contains("true") {
                let mutated = line.replacen("true", "false", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-bool-{}", id_counter),
                    operator: MutationOperator::InvertBoolean,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Swapped literal 'true' with 'false'".to_string(),
                });
                id_counter += 1;
            } else if line.contains("false") {
                let mutated = line.replacen("false", "true", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-bool-{}", id_counter),
                    operator: MutationOperator::InvertBoolean,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Swapped literal 'false' with 'true'".to_string(),
                });
                id_counter += 1;
            }

            // 4. Invert Logical: && <-> ||
            if line.contains(" && ") {
                let mutated = line.replacen(" && ", " || ", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-log-{}", id_counter),
                    operator: MutationOperator::InvertLogical,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Swapped logical AND '&&' with OR '||'".to_string(),
                });
                id_counter += 1;
            } else if line.contains(" || ") {
                let mutated = line.replacen(" || ", " && ", 1);
                candidates.push(MutantCandidate {
                    id: format!("mut-log-{}", id_counter),
                    operator: MutationOperator::InvertLogical,
                    line_number: line_no,
                    original_snippet: line.to_string(),
                    mutated_snippet: mutated,
                    description: "Swapped logical OR '||' with AND '&&'".to_string(),
                });
                id_counter += 1;
            }
        }

        candidates
    }

    /// Apply a single mutant cleanly to the source code
    pub fn apply_mutant(source: &str, mutant: &MutantCandidate) -> String {
        let mut lines: Vec<String> = source.lines().map(|s| s.to_string()).collect();
        if mutant.line_number > 0 && mutant.line_number <= lines.len() {
            lines[mutant.line_number - 1] = mutant.mutated_snippet.clone();
        }
        lines.join("\n")
    }

    /// Compute mutation score and generate diagnosis report
    pub fn generate_report(mutant_outcomes: &[(MutantCandidate, MutantStatus)]) -> MutationReport {
        let total_mutants = mutant_outcomes.len();
        let mut killed = 0;
        let mut survived = 0;
        let mut compile_errors = 0;
        let mut survived_details = Vec::new();

        for (mutant, status) in mutant_outcomes {
            match status {
                MutantStatus::Killed => killed += 1,
                MutantStatus::Survived => {
                    survived += 1;
                    survived_details.push(mutant.clone());
                }
                MutantStatus::CompileError => compile_errors += 1,
            }
        }

        let evaluated = killed + survived;
        let mutation_score_pct = if evaluated > 0 {
            (killed as f64 / evaluated as f64) * 100.0
        } else {
            100.0
        };

        let integrity_grade = if mutation_score_pct >= 90.0 {
            "A+ (Diamond Solid Test Suite)".to_string()
        } else if mutation_score_pct >= 75.0 {
            "B (Reliable, minor edge case omissions)".to_string()
        } else if mutation_score_pct >= 50.0 {
            "C (Vulnerable, significant placebo tests)".to_string()
        } else {
            "F (Critical: High Placebo Danger, tests assert trivialities)".to_string()
        };

        let mut recommendations = Vec::new();
        if survived > 0 {
            recommendations.push(format!(
                "Identified {} survived mutants. Test suite failed to catch deliberate logic mutations.",
                survived
            ));
            for s in survived_details.iter().take(5) {
                recommendations.push(format!(
                    "Line {}: Add explicit assertion for '{}' [Operator: {}]",
                    s.line_number, s.description, s.operator.name()
                ));
            }
        } else {
            recommendations.push("100% of injected mutants killed! Test suite verified immune to placebo assertions.".to_string());
        }

        MutationReport {
            total_mutants,
            killed_mutants: killed,
            survived_mutants: survived,
            compile_errors,
            mutation_score_pct,
            integrity_grade,
            survived_details,
            recommendations,
        }
    }
}
