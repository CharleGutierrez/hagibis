//! # BaaS Auto-Graduation Engine ("Mock-to-Real")
//!
//! Elevates in-memory Zero-Mock prototypes into production cloud backends.
//! Automatically synthesizes production-grade SQL DDL, Row-Level Security (RLS) policies,
//! Firestore rules, and client SDK configuration for Supabase, Firebase, and Neon.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Supported Backend-as-a-Service graduation targets
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BaasTarget {
    Supabase,
    Firebase,
    Neon,
}

impl BaasTarget {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Supabase => "Supabase (PostgreSQL + RLS)",
            Self::Firebase => "Firebase (Cloud Firestore + Rules)",
            Self::Neon => "Neon (Serverless Postgres)",
        }
    }
}

/// Inferred column definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferredColumn {
    pub name: String,
    pub sql_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
}

/// Complete graduation blueprint report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaasGraduationReport {
    pub target: BaasTarget,
    pub resource_name: String,
    pub table_ddl: String,
    pub security_rules: String,
    pub client_sdk_snippet: String,
    pub env_template: String,
    pub columns_inferred: Vec<InferredColumn>,
}

pub struct BaasGraduateEngine;

impl BaasGraduateEngine {
    /// Infer schema columns from JSON seed objects
    pub fn infer_columns(sample_json: &Value) -> Vec<InferredColumn> {
        let mut cols = Vec::new();

        if let Value::Object(map) = sample_json {
            for (key, val) in map {
                let is_pk = key == "id" || key == "_id";
                let (sql_type, nullable) = match val {
                    Value::Null => ("TEXT", true),
                    Value::Bool(_) => ("BOOLEAN NOT NULL DEFAULT FALSE", false),
                    Value::Number(n) => {
                        if n.is_i64() {
                            if is_pk { ("BIGSERIAL PRIMARY KEY", false) } else { ("BIGINT NOT NULL DEFAULT 0", false) }
                        } else {
                            ("DOUBLE PRECISION NOT NULL DEFAULT 0.0", false)
                        }
                    }
                    Value::String(s) => {
                        if is_pk {
                            ("UUID PRIMARY KEY DEFAULT gen_random_uuid()", false)
                        } else if s.contains('@') && key.contains("email") {
                            ("VARCHAR(255) NOT NULL UNIQUE", false)
                        } else if s.len() > 255 {
                            ("TEXT", true)
                        } else {
                            ("VARCHAR(255) NOT NULL", false)
                        }
                    }
                    Value::Array(_) => ("JSONB NOT NULL DEFAULT '[]'::jsonb", false),
                    Value::Object(_) => ("JSONB NOT NULL DEFAULT '{}'::jsonb", false),
                };

                cols.push(InferredColumn {
                    name: key.clone(),
                    sql_type: sql_type.to_string(),
                    is_nullable: nullable,
                    is_primary_key: is_pk,
                });
            }
        }

        // Ensure primary key exists if none was inferred
        if !cols.iter().any(|c| c.is_primary_key) {
            cols.insert(
                0,
                InferredColumn {
                    name: "id".to_string(),
                    sql_type: "UUID PRIMARY KEY DEFAULT gen_random_uuid()".to_string(),
                    is_nullable: false,
                    is_primary_key: true,
                },
            );
        }

        cols
    }

    /// Synthesize production graduation report for the selected BaaS provider
    pub fn graduate(resource_name: &str, sample_json: &Value, target: BaasTarget) -> Result<BaasGraduationReport> {
        let table_name = resource_name.trim().trim_start_matches('/').to_lowercase();
        let cols = Self::infer_columns(sample_json);

        match target {
            BaasTarget::Supabase => {
                let mut ddl = format!("-- 🪽 Hagibis Auto-Graduated Supabase Migration\n");
                ddl.push_str(&format!("CREATE TABLE IF NOT EXISTS public.{} (\n", table_name));
                for (idx, c) in cols.iter().enumerate() {
                    let comma = if idx + 1 < cols.len() { "," } else { "" };
                    ddl.push_str(&format!("    \"{}\" {}{}\n", c.name, c.sql_type, comma));
                }
                ddl.push_str(");\n\n");
                ddl.push_str(&format!("-- Enable Row-Level Security\nALTER TABLE public.{} ENABLE ROW LEVEL SECURITY;\n\n", table_name));
                ddl.push_str(&format!(
                    "CREATE POLICY \"Allow authenticated users full access to {}\" ON public.{}\n    FOR ALL TO authenticated USING (true) WITH CHECK (true);\n",
                    table_name, table_name
                ));

                let client_snippet = format!(
                    r#"import {{ createClient }} from '@supabase/supabase-js';

const supabaseUrl = process.env.NEXT_PUBLIC_SUPABASE_URL!;
const supabaseAnonKey = process.env.NEXT_PUBLIC_SUPABASE_ANON_KEY!;

export const supabase = createClient(supabaseUrl, supabaseAnonKey);

// Fetch all {} records
export async function get{}() {{
    const {{ data, error }} = await supabase.from('{}').select('*');
    if (error) throw error;
    return data;
}}
"#,
                    table_name,
                    capitalize(&table_name),
                    table_name
                );

                let env_template = "NEXT_PUBLIC_SUPABASE_URL=https://your-project.supabase.co\nNEXT_PUBLIC_SUPABASE_ANON_KEY=eyJhbGciOi...".to_string();

                Ok(BaasGraduationReport {
                    target,
                    resource_name: table_name,
                    table_ddl: ddl,
                    security_rules: "Row Level Security Active (Authenticated Users Policy)".to_string(),
                    client_sdk_snippet: client_snippet,
                    env_template,
                    columns_inferred: cols,
                })
            }
            BaasTarget::Firebase => {
                let firestore_rules = format!(
                    r#"rules_version = '2';
service cloud.firestore {{
  match /databases/{{database}}/documents {{
    match /{}/{{docId}} {{
      allow read, write: if request.auth != null;
    }}
  }}
}}"#,
                    table_name
                );

                let client_snippet = format!(
                    r#"import {{ initializeApp }} from 'firebase/app';
import {{ getFirestore, collection, getDocs }} from 'firebase/firestore';

const firebaseConfig = {{
  apiKey: process.env.NEXT_PUBLIC_FIREBASE_API_KEY,
  projectId: process.env.NEXT_PUBLIC_FIREBASE_PROJECT_ID,
}};

const app = initializeApp(firebaseConfig);
export const db = getFirestore(app);

export async function fetch{}() {{
    const snap = await getDocs(collection(db, '{}'));
    return snap.docs.map(doc => ({{ id: doc.id, ...doc.data() }}));
}}
"#,
                    capitalize(&table_name),
                    table_name
                );

                let env_template = "NEXT_PUBLIC_FIREBASE_API_KEY=AIzaSy...\nNEXT_PUBLIC_FIREBASE_PROJECT_ID=my-vibe-project".to_string();

                Ok(BaasGraduationReport {
                    target,
                    resource_name: table_name,
                    table_ddl: "-- No DDL required for Cloud Firestore schema-less collection".to_string(),
                    security_rules: firestore_rules,
                    client_sdk_snippet: client_snippet,
                    env_template,
                    columns_inferred: cols,
                })
            }
            BaasTarget::Neon => {
                let mut ddl = format!("-- 🪽 Hagibis Auto-Graduated Neon Serverless Postgres Schema\n");
                ddl.push_str(&format!("CREATE TABLE IF NOT EXISTS {} (\n", table_name));
                for (idx, c) in cols.iter().enumerate() {
                    let comma = if idx + 1 < cols.len() { "," } else { "" };
                    ddl.push_str(&format!("    {} {}{}\n", c.name, c.sql_type, comma));
                }
                ddl.push_str(");\n\n");
                ddl.push_str(&format!("CREATE INDEX IF NOT EXISTS idx_{}_created ON {} (id);\n", table_name, table_name));

                let client_snippet = format!(
                    r#"import {{ Pool }} from '@neondatabase/serverless';

const pool = new Pool({{ connectionString: process.env.DATABASE_URL }});

export async function get{}() {{
    const {{ rows }} = await pool.query('SELECT * FROM {} ORDER BY id DESC');
    return rows;
}}
"#,
                    capitalize(&table_name),
                    table_name
                );

                let env_template = "DATABASE_URL=postgres://user:password@ep-sparkling-base.us-east-2.aws.neon.tech/neondb?sslmode=require".to_string();

                Ok(BaasGraduationReport {
                    target,
                    resource_name: table_name,
                    table_ddl: ddl,
                    security_rules: "Connection String SSL Pooling".to_string(),
                    client_sdk_snippet: client_snippet,
                    env_template,
                    columns_inferred: cols,
                })
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
