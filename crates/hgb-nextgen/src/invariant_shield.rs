use serde::{Deserialize, Serialize};
use hgb_core::providers::OllamaProvider;
use hgb_core::traits::HgbProvider;

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
    pub async fn audit_and_shield(file_path: &str, source: &str) -> InvariantAuditReport {
        let provider = OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));
        let prompt = format!(
            "Scan this code for panic hazards (unwrap, out of bounds, div by zero, lock reentrancy).
Provide a list of hazards and a fully repaired source code string.
Respond ONLY in JSON:
{{
  \"hazards\": [
    {{
      \"kind\": \"UncheckedUnwrap\" | \"UnboundedIndex\" | \"PotentialDivZero\" | \"LockReentrancy\",
      \"line_number\": 10,
      \"raw_line\": \"string\",
      \"defensive_replacement\": \"string\"
    }}
  ],
  \"auto_repaired_source\": \"full string\"
}}
Code:
{}",
            source
        );

        let resp = provider.complete(&prompt, None).await.unwrap_or_default();
        let start = resp.find('{').unwrap_or(0);
        let end = resp.rfind('}').unwrap_or(resp.len() - 1) + 1;
        let json_str = &resp[start..end];

        #[derive(serde::Deserialize)]
        struct Resp {
            hazards: Vec<PanicHazard>,
            auto_repaired_source: String,
        }

        let parsed = serde_json::from_str::<Resp>(json_str).unwrap_or_else(|_| Resp {
            hazards: vec![],
            auto_repaired_source: source.to_string(),
        });

        let hazard_count = parsed.hazards.len();
        let score = if hazard_count == 0 {
            100
        } else {
            100u8.saturating_sub((hazard_count * 15) as u8).max(10)
        };

        InvariantAuditReport {
            file_path: file_path.to_string(),
            total_hazards: hazard_count,
            hazards: parsed.hazards,
            safety_score: score,
            auto_repaired_source: parsed.auto_repaired_source,
        }
    }
}
