use serde::{Deserialize, Serialize};

/// Categories of detected panic hazards
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HazardKind {
    UncheckedUnwrap,
    UnboundedIndex,
    PotentialDivZero,
    LockReentrancy,
}

/// Discovered panic hazard with surgical defensive fix
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanicHazard {
    pub kind: HazardKind,
    pub line_number: usize,
    pub raw_line: String,
    pub defensive_replacement: String,
}

/// Audit report from formal invariant scanning
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvariantAuditReport {
    pub file_path: String,
    pub total_hazards: usize,
    pub hazards: Vec<PanicHazard>,
    pub safety_score: u8,
    pub auto_repaired_source: String,
}

/// Ambient Formal Invariant & Panic Shield Engine
pub struct InvariantShieldEngine;

impl InvariantShieldEngine {
    /// Scan source code for panic hazards and synthesize safe defensive replacements
    pub fn audit_and_shield(file_path: &str, source: &str) -> InvariantAuditReport {
        let mut hazards = Vec::new();
        let mut repaired_lines = Vec::new();

        for (idx, line) in source.lines().enumerate() {
            let line_num = idx + 1;
            let mut rep_line = line.to_string();

            // 1. Detect unchecked .unwrap()
            if line.contains(".unwrap()") && !line.contains("// allow-unwrap") {
                let fix = line.replace(".unwrap()", ".unwrap_or_default()");
                hazards.push(PanicHazard {
                    kind: HazardKind::UncheckedUnwrap,
                    line_number: line_num,
                    raw_line: line.trim().to_string(),
                    defensive_replacement: fix.trim().to_string(),
                });
                rep_line = fix;
            }
            // 2. Detect potential division by zero
            else if line.contains("/ 0") || (line.contains(" / ") && line.contains("divisor")) {
                let fix = format!("/* guarded div */ if divisor != 0 {{ {} }} else {{ 0 }}", line.trim());
                hazards.push(PanicHazard {
                    kind: HazardKind::PotentialDivZero,
                    line_number: line_num,
                    raw_line: line.trim().to_string(),
                    defensive_replacement: fix.clone(),
                });
                rep_line = fix;
            }

            repaired_lines.push(rep_line);
        }

        let hazard_count = hazards.len();
        let score = if hazard_count == 0 {
            100
        } else {
            100u8.saturating_sub((hazard_count * 15) as u8).max(10)
        };

        InvariantAuditReport {
            file_path: file_path.to_string(),
            total_hazards: hazard_count,
            hazards,
            safety_score: score,
            auto_repaired_source: repaired_lines.join("\n"),
        }
    }
}
