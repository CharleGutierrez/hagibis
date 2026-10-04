use blake3::Hasher;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use crate::error::{HgbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVarUsage {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
    pub ecosystem: String,
    pub code_snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EnvVarStatus {
    Configured { value_obscured: String },
    MissingInEnv,
    PlaceholderValue { value: String },
    UnusedInCode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvReconciliationItem {
    pub name: String,
    pub status: EnvVarStatus,
    pub used_in_files: Vec<String>,
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretLeakFinding {
    pub secret_type: String,
    pub sample_redacted: String,
    pub entropy: f64,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvAuditReport {
    pub workspace_root: PathBuf,
    pub env_file_found: bool,
    pub env_file_path: Option<PathBuf>,
    pub variables: Vec<EnvReconciliationItem>,
    pub secret_leaks: Vec<SecretLeakFinding>,
    pub total_detected: usize,
    pub missing_count: usize,
    pub placeholder_count: usize,
    pub is_clean: bool,
}

pub struct EnvSentinel;

impl EnvSentinel {
    /// Calculate Shannon entropy in bits per character for a string token
    pub fn calculate_shannon_entropy(data: &str) -> f64 {
        if data.is_empty() {
            return 0.0;
        }
        let mut frequencies = HashMap::new();
        for ch in data.chars() {
            *frequencies.entry(ch).or_insert(0usize) += 1;
        }
        let total_chars = data.chars().count() as f64;
        let mut entropy = 0.0;
        for &count in frequencies.values() {
            let p = count as f64 / total_chars;
            entropy -= p * p.log2();
        }
        entropy
    }

    /// Obscure a secret value (e.g. `sk-proj-abc...1234`)
    pub fn obscure_value(val: &str) -> String {
        if val.len() <= 6 {
            return "******".to_string();
        }
        let prefix = &val[..std::cmp::min(4, val.len())];
        let suffix = &val[val.len() - std::cmp::min(4, val.len())..];
        format!("{}...{}", prefix, suffix)
    }

    /// Check if a value is a placeholder placeholder
    pub fn is_placeholder(val: &str) -> bool {
        let trimmed = val.trim().trim_matches('"').trim_matches('\'').to_lowercase();
        let placeholders = [
            "your_api_key_here",
            "your-api-key",
            "your_key",
            "todo",
            "changeme",
            "change_me",
            "replace_me",
            "xxx",
            "xxxx",
            "foo",
            "bar",
            "baz",
            "test",
            "secret",
            "123456",
            "<your_token>",
            "<api_key>",
        ];
        if trimmed.is_empty() {
            return true;
        }
        for p in &placeholders {
            if trimmed == *p || trimmed.contains(p) {
                return true;
            }
        }
        if trimmed.starts_with('<') && trimmed.ends_with('>') {
            return true;
        }
        false
    }

    /// Scan arbitrary code content for environment variable references across ecosystems
    pub fn scan_content(content: &str, path: &Path) -> Vec<EnvVarUsage> {
        let mut usages = Vec::new();
        let lines: Vec<&str> = content.lines().collect();

        // 1. JavaScript / TypeScript patterns
        let js_patterns = [
            r"process\.env\.([A-Z0-9_]{2,})",
            r#"process\.env\[['"]([A-Z0-9_]{2,})['"]\]"#,
            r"import\.meta\.env\.([A-Z0-9_]{2,})",
        ];

        // 2. Rust patterns
        let rust_patterns = [
            r#"std::env::var\(\s*["']([A-Z0-9_]{2,})["']\s*\)"#,
            r#"std::env::var_os\(\s*["']([A-Z0-9_]{2,})["']\s*\)"#,
            r#"env!\(\s*["']([A-Z0-9_]{2,})["']\s*\)"#,
            r#"option_env!\(\s*["']([A-Z0-9_]{2,})["']\s*\)"#,
        ];

        // 3. Python patterns
        let py_patterns = [
            r#"os\.environ\[["']([A-Z0-9_]{2,})["']\]"#,
            r#"os\.environ\.get\(["']([A-Z0-9_]{2,})["']"#,
            r#"os\.getenv\(["']([A-Z0-9_]{2,})["']"#,
        ];

        // 4. Go patterns
        let go_patterns = [
            r#"os\.Getenv\(["']([A-Z0-9_]{2,})["']"#,
            r#"os\.LookupEnv\(["']([A-Z0-9_]{2,})["']"#,
        ];

        let mut compiled = Vec::new();
        for p in &js_patterns {
            if let Ok(re) = Regex::new(p) {
                compiled.push(("javascript", re));
            }
        }
        for p in &rust_patterns {
            if let Ok(re) = Regex::new(p) {
                compiled.push(("rust", re));
            }
        }
        for p in &py_patterns {
            if let Ok(re) = Regex::new(p) {
                compiled.push(("python", re));
            }
        }
        for p in &go_patterns {
            if let Ok(re) = Regex::new(p) {
                compiled.push(("go", re));
            }
        }

        for (line_idx, line) in lines.iter().enumerate() {
            let line_num = line_idx + 1;
            for (eco, re) in &compiled {
                for cap in re.captures_iter(line) {
                    if let Some(m) = cap.get(1) {
                        usages.push(EnvVarUsage {
                            name: m.as_str().to_string(),
                            file: path.to_path_buf(),
                            line: line_num,
                            ecosystem: eco.to_string(),
                            code_snippet: line.trim().to_string(),
                        });
                    }
                }
            }
        }

        usages
    }

    /// Parse key-value pairs from a `.env` file
    pub fn parse_env_file(path: &Path) -> Result<HashMap<String, String>> {
        if !path.exists() {
            return Err(HgbError::NotFound(format!(".env file not found: {}", path.display())));
        }
        let content = fs::read_to_string(path)?;
        let mut map = HashMap::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                let key = k.trim().to_string();
                let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if !key.is_empty() {
                    map.insert(key, val);
                }
            }
        }
        Ok(map)
    }

    /// Audit an entire workspace by reconciling source code env usages with .env definitions
    pub fn audit_workspace(root: &Path, env_file_override: Option<&str>) -> Result<EnvAuditReport> {
        let env_path = if let Some(e) = env_file_override {
            root.join(e)
        } else {
            root.join(".env")
        };

        let env_file_found = env_path.exists();
        let env_vars = if env_file_found {
            Self::parse_env_file(&env_path).unwrap_or_default()
        } else {
            HashMap::new()
        };

        // Scan code files recursively (excluding target, node_modules, .git)
        let mut code_usages = Vec::new();
        let mut secret_leaks = Vec::new();

        Self::walk_and_scan(root, &mut code_usages, &mut secret_leaks)?;

        let mut usage_by_name: HashMap<String, Vec<EnvVarUsage>> = HashMap::new();
        for u in code_usages {
            usage_by_name.entry(u.name.clone()).or_default().push(u);
        }

        let mut all_keys: HashSet<String> = HashSet::new();
        for k in env_vars.keys() {
            all_keys.insert(k.clone());
        }
        for k in usage_by_name.keys() {
            all_keys.insert(k.clone());
        }

        let mut items = Vec::new();
        let mut missing_count = 0;
        let mut placeholder_count = 0;

        for key in all_keys {
            let in_env = env_vars.get(&key);
            let in_code = usage_by_name.get(&key);

            let used_files: Vec<String> = in_code
                .map(|list| list.iter().map(|u| format!("{}:{}", u.file.display(), u.line)).collect())
                .unwrap_or_default();

            match (in_env, in_code) {
                (Some(val), Some(_)) => {
                    if Self::is_placeholder(val) {
                        placeholder_count += 1;
                        items.push(EnvReconciliationItem {
                            name: key.clone(),
                            status: EnvVarStatus::PlaceholderValue { value: val.clone() },
                            used_in_files: used_files,
                            recommendation: format!("Update placeholder value '{}' in .env with valid credential", val),
                        });
                    } else {
                        items.push(EnvReconciliationItem {
                            name: key.clone(),
                            status: EnvVarStatus::Configured {
                                value_obscured: Self::obscure_value(val),
                            },
                            used_in_files: used_files,
                            recommendation: "Configured and active".to_string(),
                        });
                    }
                }
                (None, Some(_)) => {
                    missing_count += 1;
                    items.push(EnvReconciliationItem {
                        name: key.clone(),
                        status: EnvVarStatus::MissingInEnv,
                        used_in_files: used_files,
                        recommendation: "Add variable definition to .env".to_string(),
                    });
                }
                (Some(val), None) => {
                    items.push(EnvReconciliationItem {
                        name: key.clone(),
                        status: EnvVarStatus::UnusedInCode,
                        used_in_files: vec![],
                        recommendation: format!("Defined in .env as '{}' but unused across scanned source files", Self::obscure_value(val)),
                    });
                }
                (None, None) => {}
            }
        }

        items.sort_by(|a, b| a.name.cmp(&b.name));
        let total_detected = items.len();
        let is_clean = missing_count == 0 && placeholder_count == 0 && secret_leaks.is_empty();

        Ok(EnvAuditReport {
            workspace_root: root.to_path_buf(),
            env_file_found,
            env_file_path: if env_file_found { Some(env_path) } else { None },
            variables: items,
            secret_leaks,
            total_detected,
            missing_count,
            placeholder_count,
            is_clean,
        })
    }

    fn walk_and_scan(
        dir: &Path,
        usages: &mut Vec<EnvVarUsage>,
        leaks: &mut Vec<SecretLeakFinding>,
    ) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" || name == "build" {
                continue;
            }
            if p.is_dir() {
                Self::walk_and_scan(&p, usages, leaks)?;
            } else if p.is_file() {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ["rs", "ts", "js", "tsx", "jsx", "py", "go"].contains(&ext) {
                    if let Ok(content) = fs::read_to_string(&p) {
                        usages.extend(Self::scan_content(&content, &p));
                        let (_, found_leaks) = Self::shred_secrets(&content);
                        leaks.extend(found_leaks);
                    }
                }
            }
        }
        Ok(())
    }

    /// Generate clean, git-safe `.env.example` content from audit report
    pub fn generate_env_example(report: &EnvAuditReport) -> String {
        let mut out = String::new();
        out.push_str("# Auto-generated by Hagibis EnvSentinel (100% Safe Template)\n");
        out.push_str("# Fill in required values and copy to .env\n\n");

        for item in &report.variables {
            let name = &item.name;
            let val = match name.as_str() {
                "DATABASE_URL" => "postgres://user:password@localhost:5432/mydb".to_string(),
                n if n.ends_with("_PORT") || n == "PORT" => "3000".to_string(),
                n if n.ends_with("_URL") || n.ends_with("_URI") => "https://api.example.com".to_string(),
                "NODE_ENV" | "ENVIRONMENT" => "development".to_string(),
                n if n.ends_with("_KEY") || n.ends_with("_SECRET") || n.ends_with("_TOKEN") => {
                    format!("<YOUR_{}_HERE>", n)
                }
                n => format!("<YOUR_{}>", n),
            };
            out.push_str(&format!("{}={}\n", name, val));
        }

        out
    }

    /// Shred secrets from content, replacing tokens with cryptographic blake3 hashes
    pub fn shred_secrets(input: &str) -> (String, Vec<SecretLeakFinding>) {
        let mut findings = Vec::new();
        let mut output = input.to_string();

        let patterns = [
            ("Google API Key", r"AIzaSy[a-zA-Z0-9_-]{32,39}"),
            ("OpenAI Project Key", r"sk-proj-[a-zA-Z0-9_-]{48,}"),
            ("OpenAI Key", r"sk-[a-zA-Z0-9]{32,}"),
            ("GitHub PAT", r"ghp_[a-zA-Z0-9]{36}"),
            ("GitHub Token", r"github_pat_[a-zA-Z0-9_]{82}"),
            ("AWS Access Key ID", r"AKIA[0-9A-Z]{16}"),
        ];

        for (desc, pattern) in &patterns {
            if let Ok(re) = Regex::new(pattern) {
                for mat in re.find_iter(input) {
                    let matched_str = mat.as_str();
                    let entropy = Self::calculate_shannon_entropy(matched_str);
                    let mut hasher = Hasher::new();
                    hasher.update(matched_str.as_bytes());
                    let hash_prefix = &hasher.finalize().to_hex()[..8];

                    let redacted = format!("[REDACTED_SECRET:{}]", hash_prefix);
                    output = output.replace(matched_str, &redacted);

                    findings.push(SecretLeakFinding {
                        secret_type: desc.to_string(),
                        sample_redacted: format!("{}...{}", &matched_str[..4], &matched_str[matched_str.len() - 4..]),
                        entropy,
                        line: 1,
                    });
                }
            }
        }

        (output, findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_polyglot_scanner() {
        let code = r#"
            const gemini = process.env.GEMINI_API_KEY;
            const db = process.env["DATABASE_URL"];
            const rustKey = std::env::var("AWS_SECRET_ACCESS_KEY");
            const pyKey = os.environ["OPENAI_API_KEY"];
            const goKey = os.Getenv("GITHUB_TOKEN");
        "#;
        let usages = EnvSentinel::scan_content(code, Path::new("index.ts"));
        let names: Vec<String> = usages.into_iter().map(|u| u.name).collect();
        assert!(names.contains(&"GEMINI_API_KEY".to_string()));
        assert!(names.contains(&"DATABASE_URL".to_string()));
        assert!(names.contains(&"AWS_SECRET_ACCESS_KEY".to_string()));
        assert!(names.contains(&"OPENAI_API_KEY".to_string()));
        assert!(names.contains(&"GITHUB_TOKEN".to_string()));
    }

    #[test]
    fn test_secret_shredder() {
        let payload = "const key = 'AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q';";
        let (shredded, leaks) = EnvSentinel::shred_secrets(payload);
        assert_eq!(leaks.len(), 1);
        assert!(shredded.contains("[REDACTED_SECRET:"));
        assert!(!shredded.contains("AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q"));
    }

    #[test]
    fn test_shannon_entropy() {
        let random_secret = "4f8a9b2c3d1e0f7a6b5c4d3e2f1a0b9c";
        let entropy = EnvSentinel::calculate_shannon_entropy(random_secret);
        assert!(entropy > 3.0);
    }
}
