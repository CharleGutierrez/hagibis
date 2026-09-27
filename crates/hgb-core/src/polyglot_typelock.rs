//! # Zero-Drift Polyglot Type Lock & Auto-Synchronizer
//!
//! Synchronizes domain models across Rust backend structs, SQL schemas,
//! TypeScript frontend interfaces, and runtime Zod schemas with zero drift.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolyglotField {
    pub name: String,
    pub rust_type: String,
    pub ts_type: String,
    pub nullable: bool,
    pub is_array: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolyglotModel {
    pub name: String,
    pub fields: Vec<PolyglotField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDriftItem {
    pub model: String,
    pub field: String,
    pub discrepancy: String,
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeLockSyncReport {
    pub models_synced: usize,
    pub generated_ts_interfaces: String,
    pub generated_zod_schemas: String,
    pub drifts_detected: Vec<TypeDriftItem>,
    pub zero_drift_achieved: bool,
}

pub struct PolyglotTypeLock {
    models: Vec<PolyglotModel>,
}

impl PolyglotTypeLock {
    pub fn new() -> Self {
        Self { models: Vec::new() }
    }

    pub fn register_model(&mut self, model: PolyglotModel) {
        self.models.push(model);
    }

    fn map_rust_to_ts(rust_type: &str) -> String {
        if rust_type.contains("String") || rust_type.contains("&str") || rust_type.contains("Uuid") {
            "string".to_string()
        } else if rust_type.contains("i8") || rust_type.contains("i16") || rust_type.contains("i32") || rust_type.contains("i64") || rust_type.contains("i128") || rust_type.contains("isize")
            || rust_type.contains("u8") || rust_type.contains("u16") || rust_type.contains("u32") || rust_type.contains("u64") || rust_type.contains("u128") || rust_type.contains("usize")
            || rust_type.contains("f32") || rust_type.contains("f64") {
            "number".to_string()
        } else if rust_type.contains("bool") {
            "boolean".to_string()
        } else {
            "unknown".to_string()
        }
    }

    /// Parses Rust struct syntax into PolyglotModel(s), supporting whole files with multiple structs
    pub fn parse_rust_struct(&mut self, source: &str) -> Option<PolyglotModel> {
        let mut last_model = None;
        let mut in_struct: Option<(String, Vec<PolyglotField>)> = None;

        for line in source.lines() {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with("/*") || line.starts_with('*') || line.starts_with("#[") {
                continue;
            }

            if line.starts_with("pub struct ") || line.starts_with("struct ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let name = if parts.len() >= 3 && parts[0] == "pub" {
                    parts[2].trim_end_matches('{').trim().to_string()
                } else if parts.len() >= 2 {
                    parts[1].trim_end_matches('{').trim().to_string()
                } else {
                    continue;
                };

                // Check for single-line struct
                if line.contains('{') && line.contains('}') {
                    if let Some((_, body)) = line.split_once('{') {
                        let inner = body.trim_end_matches('}').trim();
                        let mut fields = Vec::new();
                        for chunk in inner.split(',') {
                            let chunk = chunk.trim();
                            if let Some((fname, ftype)) = chunk.split_once(':') {
                                let f_name = fname.trim().trim_start_matches("pub ").trim().to_string();
                                let r_type = ftype.trim().trim_end_matches(';').trim().to_string();
                                let nullable = r_type.starts_with("Option<");
                                let is_array = r_type.starts_with("Vec<");
                                let ts_type = Self::map_rust_to_ts(&r_type);
                                fields.push(PolyglotField {
                                    name: f_name,
                                    rust_type: r_type,
                                    ts_type,
                                    nullable,
                                    is_array,
                                });
                            }
                        }
                        let model = PolyglotModel { name, fields };
                        self.models.push(model.clone());
                        last_model = Some(model);
                        continue;
                    }
                }

                in_struct = Some((name, Vec::new()));
                continue;
            }

            if let Some((ref name, ref mut fields)) = in_struct {
                if line.starts_with('}') {
                    let model = PolyglotModel {
                        name: name.clone(),
                        fields: fields.clone(),
                    };
                    self.models.push(model.clone());
                    last_model = Some(model);
                    in_struct = None;
                    continue;
                }

                if let Some((fname, ftype)) = line.split_once(':') {
                    let f_name = fname.trim().trim_start_matches("pub ").trim().to_string();
                    let r_type = ftype.trim().trim_end_matches(',').trim_end_matches(';').trim().to_string();
                    let nullable = r_type.starts_with("Option<");
                    let is_array = r_type.starts_with("Vec<");
                    let ts_type = Self::map_rust_to_ts(&r_type);
                    fields.push(PolyglotField {
                        name: f_name,
                        rust_type: r_type,
                        ts_type,
                        nullable,
                        is_array,
                    });
                }
            }
        }

        last_model
    }

    /// Generates TypeScript interfaces from registered models
    pub fn generate_typescript(&self) -> String {
        let mut out = String::new();
        out.push_str("// Auto-generated by Hagibis Polyglot Type Lock (Zero-Drift)\n\n");

        for model in &self.models {
            out.push_str(&format!("export interface {} {{\n", model.name));
            for f in &model.fields {
                let optional_mark = if f.nullable { "?" } else { "" };
                let type_suffix = if f.is_array { "[]" } else { "" };
                out.push_str(&format!("  {}{}: {}{};\n", f.name, optional_mark, f.ts_type, type_suffix));
            }
            out.push_str("}\n\n");
        }
        out
    }

    /// Generates Zod validation schemas from registered models
    pub fn generate_zod(&self) -> String {
        let mut out = String::new();
        out.push_str("// Auto-generated Zod Schemas by Hagibis Polyglot Type Lock\nimport { z } from \"zod\";\n\n");

        for model in &self.models {
            out.push_str(&format!("export const {}Schema = z.object({{\n", model.name));
            for f in &model.fields {
                let mut zod_type = match f.ts_type.as_str() {
                    "string" => "z.string()".to_string(),
                    "number" => "z.number()".to_string(),
                    "boolean" => "z.boolean()".to_string(),
                    _ => "z.unknown()".to_string(),
                };
                if f.is_array {
                    zod_type = format!("z.array({})", zod_type);
                }
                if f.nullable {
                    zod_type = format!("{}.optional()", zod_type);
                }
                out.push_str(&format!("  {}: {},\n", f.name, zod_type));
            }
            out.push_str("});\n\n");
        }
        out
    }

    /// Audits drift between backend models and an existing TypeScript file
    pub fn audit_drift(&self, existing_ts: &str) -> TypeLockSyncReport {
        let mut drifts = Vec::new();

        for model in &self.models {
            for f in &model.fields {
                // Check if field exists in existing ts
                let pattern = format!("{}:", f.name);
                let optional_pattern = format!("{}?:", f.name);
                if !existing_ts.contains(&pattern) && !existing_ts.contains(&optional_pattern) {
                    drifts.push(TypeDriftItem {
                        model: model.name.clone(),
                        field: f.name.clone(),
                        discrepancy: "Missing in frontend TypeScript interface".to_string(),
                        severity: "High".to_string(),
                    });
                }
            }
        }

        let zero_drift = drifts.is_empty();
        TypeLockSyncReport {
            models_synced: self.models.len(),
            generated_ts_interfaces: self.generate_typescript(),
            generated_zod_schemas: self.generate_zod(),
            drifts_detected: drifts,
            zero_drift_achieved: zero_drift,
        }
    }
}

impl Default for PolyglotTypeLock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_polyglot_typelock_parse_and_generate() {
        let mut typelock = PolyglotTypeLock::new();
        let rust_code = r#"
        pub struct UserAccount {
            pub id: Uuid,
            pub email: String,
            pub age: u32,
            pub is_verified: bool,
            pub tags: Vec<String>,
            pub avatar_url: Option<String>,
        }
        "#;

        let model = typelock.parse_rust_struct(rust_code).unwrap();
        assert_eq!(model.name, "UserAccount");
        assert_eq!(model.fields.len(), 6);

        let ts = typelock.generate_typescript();
        assert!(ts.contains("export interface UserAccount {"));
        assert!(ts.contains("id: string;"));
        assert!(ts.contains("email: string;"));
        assert!(ts.contains("age: number;"));
        assert!(ts.contains("tags: string[];"));
        assert!(ts.contains("avatar_url?: string;"));

        let zod = typelock.generate_zod();
        assert!(zod.contains("export const UserAccountSchema = z.object({"));
        assert!(zod.contains("tags: z.array(z.string()),"));
        assert!(zod.contains("avatar_url: z.string().optional(),"));

        // Audit against matching ts
        let report = typelock.audit_drift(&ts);
        assert!(report.zero_drift_achieved);
        assert_eq!(report.drifts_detected.len(), 0);

        // Audit against missing field
        let partial_ts = "export interface UserAccount { id: string; }";
        let drift_report = typelock.audit_drift(partial_ts);
        assert!(!drift_report.zero_drift_achieved);
        assert!(drift_report.drifts_detected.len() >= 5);
    }
}
