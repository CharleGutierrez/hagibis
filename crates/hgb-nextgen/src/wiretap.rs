//! # Zero-Friction Wiretap & Contract Healer (`hgb-nextgen`)
//!
//! Autonomous API contract verification & dual-sided drift healer:
//! - Inspects live JSON payloads against expected endpoint schemas
//! - Detects contract drift:
//!   1. Missing required fields
//!   2. Type mismatches (e.g. integer expected, string received)
//!   3. Naming convention mismatches (`snake_case` vs `camelCase`)
//! - Synthesizes automated dual-sided `ContractPatch` (Frontend TS & Backend Rust DTO)

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Classification of detected API contract drift
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftKind {
    MissingField,
    TypeMismatch,
    NamingConventionMismatch,
}

/// A detected deviation from the contract specification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractDrift {
    pub kind: DriftKind,
    pub field_name: String,
    pub expected_spec: String,
    pub actual_found: String,
    pub suggested_remedy: String,
}

/// Automated dual-sided patch resolving contract drift
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractPatch {
    pub endpoint: String,
    pub drifts_found: usize,
    pub frontend_ts_adapter: String,
    pub backend_rust_dto: String,
    pub diff_summary: String,
}

/// Contract specification for an expected API schema field
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedField {
    pub name: String,
    pub expected_type: String, // "string", "number", "boolean", "array", "object"
    pub required: bool,
}

/// Zero-Friction Wiretap & Contract Healing Engine
pub struct WiretapEngine;

impl Default for WiretapEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl WiretapEngine {
    pub fn new() -> Self {
        Self
    }

    /// Compare live JSON payload against expected field schema
    pub fn inspect_payload(
        &self,
        _endpoint: &str,
        expected_schema: &[ExpectedField],
        live_json: &Value,
    ) -> Vec<ContractDrift> {
        let mut drifts = Vec::new();
        let obj = match live_json.as_object() {
            Some(o) => o,
            None => {
                drifts.push(ContractDrift {
                    kind: DriftKind::TypeMismatch,
                    field_name: "root".to_string(),
                    expected_spec: "object".to_string(),
                    actual_found: format!("{:?}", live_json),
                    suggested_remedy: "Wrap payload in top-level JSON object".to_string(),
                });
                return drifts;
            }
        };

        for field in expected_schema {
            // Check direct match
            if let Some(val) = obj.get(&field.name) {
                // Verify type
                let actual_type = match val {
                    Value::String(_) => "string",
                    Value::Number(_) => "number",
                    Value::Bool(_) => "boolean",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                    Value::Null => "null",
                };

                if actual_type != field.expected_type && (actual_type != "null" || field.required) {
                    drifts.push(ContractDrift {
                        kind: DriftKind::TypeMismatch,
                        field_name: field.name.clone(),
                        expected_spec: field.expected_type.clone(),
                        actual_found: actual_type.to_string(),
                        suggested_remedy: format!("Cast '{}' to expected type {}", field.name, field.expected_type),
                    });
                }
            } else {
                // Check if snake_case vs camelCase naming mismatch exists
                let snake = to_snake_case(&field.name);
                let camel = to_camel_case(&field.name);

                if obj.contains_key(&snake) && field.name != snake {
                    drifts.push(ContractDrift {
                        kind: DriftKind::NamingConventionMismatch,
                        field_name: field.name.clone(),
                        expected_spec: format!("camelCase ('{}')", field.name),
                        actual_found: format!("snake_case ('{}')", snake),
                        suggested_remedy: format!("Add #[serde(rename = \"{}\")] or standardize casing", field.name),
                    });
                } else if obj.contains_key(&camel) && field.name != camel {
                    drifts.push(ContractDrift {
                        kind: DriftKind::NamingConventionMismatch,
                        field_name: field.name.clone(),
                        expected_spec: format!("snake_case ('{}')", field.name),
                        actual_found: format!("camelCase ('{}')", camel),
                        suggested_remedy: format!("Add #[serde(rename = \"{}\")] or standardize casing", field.name),
                    });
                } else if field.required {
                    drifts.push(ContractDrift {
                        kind: DriftKind::MissingField,
                        field_name: field.name.clone(),
                        expected_spec: format!("required {}", field.expected_type),
                        actual_found: "missing".to_string(),
                        suggested_remedy: format!("Populate missing required key '{}'", field.name),
                    });
                }
            }
        }

        drifts
    }

    /// Synthesize dual-sided ContractPatch healing both Frontend & Backend
    pub fn heal_contract(&self, endpoint: &str, drifts: &[ContractDrift]) -> ContractPatch {
        let mut fe_props = Vec::new();
        let mut be_fields = Vec::new();

        for d in drifts {
            match d.kind {
                DriftKind::NamingConventionMismatch => {
                    be_fields.push(format!("    #[serde(alias = \"{}\", rename = \"{}\")]\n    pub {}: Value,", d.actual_found.split('\'').nth(1).unwrap_or(&d.field_name), d.field_name, to_snake_case(&d.field_name)));
                    fe_props.push(format!("  /** Auto-mapped from {} */\n  {}: any;", d.actual_found, d.field_name));
                }
                DriftKind::TypeMismatch => {
                    be_fields.push(format!("    // Healed type drift: expected {}\n    pub {}: Value,", d.expected_spec, to_snake_case(&d.field_name)));
                    fe_props.push(format!("  {}: string | number;", d.field_name));
                }
                DriftKind::MissingField => {
                    be_fields.push(format!("    #[serde(default)]\n    pub {}: Option<Value>,", to_snake_case(&d.field_name)));
                    fe_props.push(format!("  {}?: any;", d.field_name));
                }
            }
        }

        let fe_ts = format!(
            "// Wiretap Contract Healer (Frontend Adapter for {})\nexport interface Healed{}Response {{\n{}\n}}\n",
            endpoint,
            endpoint.replace('/', "_").replace('-', "_"),
            fe_props.join("\n")
        );

        let be_rust = format!(
            "// Wiretap Contract Healer (Backend DTO for {})\n#[derive(Debug, Serialize, Deserialize)]\npub struct Healed{}Dto {{\n{}\n}}\n",
            endpoint,
            endpoint.replace('/', "_").replace('-', "_"),
            be_fields.join("\n")
        );

        let diff_summary = format!("Healed {} contract drift(s) across Frontend TS and Backend Rust DTO", drifts.len());

        ContractPatch {
            endpoint: endpoint.to_string(),
            drifts_found: drifts.len(),
            frontend_ts_adapter: fe_ts,
            backend_rust_dto: be_rust,
            diff_summary,
        }
    }
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn to_camel_case(s: &str) -> String {
    let mut out = String::new();
    let mut capitalize = false;
    for ch in s.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            out.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_wiretap_drift_detection_and_dual_healing() {
        let wiretap = WiretapEngine::new();
        let expected = vec![
            ExpectedField {
                name: "userId".to_string(), // expects camelCase
                expected_type: "number".to_string(),
                required: true,
            },
            ExpectedField {
                name: "authToken".to_string(),
                expected_type: "string".to_string(),
                required: true,
            },
        ];

        // Payload has snake_case `user_id` and string instead of number
        let live_payload = json!({
            "user_id": "420",
            // missing authToken
        });

        let drifts = wiretap.inspect_payload("/api/v1/session", &expected, &live_payload);
        assert_eq!(drifts.len(), 2);

        // Drift 1: Naming mismatch (camelCase vs snake_case)
        assert!(drifts.iter().any(|d| d.kind == DriftKind::NamingConventionMismatch));
        // Drift 2: Missing authToken
        assert!(drifts.iter().any(|d| d.kind == DriftKind::MissingField));

        // Generate dual-sided patch
        let patch = wiretap.heal_contract("/api/v1/session", &drifts);
        assert!(patch.frontend_ts_adapter.contains("Healed_api_v1_sessionResponse"));
        assert!(patch.backend_rust_dto.contains("#[serde(alias"));
        assert_eq!(patch.drifts_found, 2);
    }
}
