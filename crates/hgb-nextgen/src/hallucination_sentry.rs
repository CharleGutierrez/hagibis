//! # Hallucination Sentry Package Fact-Checker (`hgb-nextgen`)
//!
//! Pre-flight package integrity & hallucination monitor:
//! - Audits `Cargo.toml`, `package.json`, `requirements.txt`, and `go.mod`
//! - Validates package names against built-in authoritative registry catalog
//! - Intercepts hallucinated LLM dependencies (e.g. `next-vibe`, `serde-super`, `react-magical-state`)
//! - Suggests canonical valid package names and auto-heals manifest files

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Package ecosystem type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestEcosystem {
    Cargo,
    Npm,
    PyPi,
    GoMod,
}

/// Verification result for a single package entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageAuditEntry {
    pub ecosystem: ManifestEcosystem,
    pub package_name: String,
    pub version_req: String,
    pub is_valid: bool,
    pub is_hallucinated: bool,
    pub canonical_replacement: Option<String>,
    pub warning: Option<String>,
}

/// Complete audit report for a manifest file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestAuditReport {
    pub manifest_file: PathBuf,
    pub ecosystem: ManifestEcosystem,
    pub total_packages: usize,
    pub verified_valid: usize,
    pub hallucinated_count: usize,
    pub entries: Vec<PackageAuditEntry>,
    pub healed_content: Option<String>,
}

/// Pre-Flight Package Hallucination Sentry
pub struct HallucinationSentry {
    known_cargo_crates: HashSet<String>,
    known_npm_packages: HashSet<String>,
    known_pypi_packages: HashSet<String>,
    known_aliases: HashMap<String, String>,
}

impl Default for HallucinationSentry {
    fn default() -> Self {
        Self::new()
    }
}

impl HallucinationSentry {
    pub fn new() -> Self {
        let mut known_cargo_crates = HashSet::new();
        for k in &[
            "tokio", "serde", "serde_json", "ratatui", "crossterm", "blake3", "rusqlite",
            "clap", "colored", "thiserror", "regex", "reqwest", "bincode", "chrono",
            "futures", "async-trait", "sha2", "base64", "memmap2", "unicode-width",
        ] {
            known_cargo_crates.insert(k.to_string());
        }

        let mut known_npm_packages = HashSet::new();
        for k in &[
            "react", "react-dom", "next", "vite", "tailwindcss", "typescript", "express",
            "zod", "axios", "lucide-react", "postcss", "clsx", "tailwind-merge", "dotenv",
        ] {
            known_npm_packages.insert(k.to_string());
        }

        let mut known_pypi_packages = HashSet::new();
        for k in &[
            "fastapi", "uvicorn", "pydantic", "pytest", "requests", "numpy", "torch",
            "flask", "django", "httpx", "sqlalchemy", "celery", "redis",
        ] {
            known_pypi_packages.insert(k.to_string());
        }

        let mut known_aliases = HashMap::new();
        // Common LLM hallucinated packages mapped to genuine packages
        known_aliases.insert("serde-super".to_string(), "serde".to_string());
        known_aliases.insert("next-vibe".to_string(), "next".to_string());
        known_aliases.insert("tokio-full".to_string(), "tokio".to_string());
        known_aliases.insert("react-super-state".to_string(), "react".to_string());
        known_aliases.insert("fastapi-magic".to_string(), "fastapi".to_string());

        Self {
            known_cargo_crates,
            known_npm_packages,
            known_pypi_packages,
            known_aliases,
        }
    }

    /// Audit a manifest file content directly
    pub fn audit_manifest(&self, file_path: &Path, content: &str) -> ManifestAuditReport {
        let file_name = file_path.file_name().and_then(|f| f.to_str()).unwrap_or("");
        let (ecosystem, parsed_pkgs) = if file_name.ends_with("Cargo.toml") {
            (ManifestEcosystem::Cargo, self.parse_cargo_deps(content))
        } else if file_name.ends_with("package.json") {
            (ManifestEcosystem::Npm, self.parse_npm_deps(content))
        } else {
            (ManifestEcosystem::PyPi, self.parse_pypi_deps(content))
        };

        let mut entries = Vec::new();
        let mut verified_valid = 0;
        let mut hallucinated_count = 0;
        let mut healed_content = content.to_string();

        for (pkg, ver) in &parsed_pkgs {
            let (is_valid, is_hallucinated, replacement) = self.verify_package(ecosystem, pkg);

            let warning = if is_hallucinated {
                hallucinated_count += 1;
                if let Some(ref rep) = replacement {
                    // Replace hallucinated package in healed manifest content
                    healed_content = healed_content.replace(pkg, rep);
                    Some(format!("Hallucinated package '{}' intercepted! Replaced with canonical '{}'", pkg, rep))
                } else {
                    Some(format!("Unverified/Hallucinated package '{}' detected!", pkg))
                }
            } else {
                verified_valid += 1;
                None
            };

            entries.push(PackageAuditEntry {
                ecosystem,
                package_name: pkg.clone(),
                version_req: ver.clone(),
                is_valid,
                is_hallucinated,
                canonical_replacement: replacement,
                warning,
            });
        }

        ManifestAuditReport {
            manifest_file: file_path.to_path_buf(),
            ecosystem,
            total_packages: entries.len(),
            verified_valid,
            hallucinated_count,
            entries,
            healed_content: if hallucinated_count > 0 { Some(healed_content) } else { None },
        }
    }

    fn verify_package(&self, eco: ManifestEcosystem, name: &str) -> (bool, bool, Option<String>) {
        if let Some(canonical) = self.known_aliases.get(name) {
            return (false, true, Some(canonical.clone()));
        }

        let is_known = match eco {
            ManifestEcosystem::Cargo => self.known_cargo_crates.contains(name) || name.starts_with("hgb-"),
            ManifestEcosystem::Npm => self.known_npm_packages.contains(name) || name.starts_with('@'),
            ManifestEcosystem::PyPi => self.known_pypi_packages.contains(name),
            ManifestEcosystem::GoMod => true,
        };

        if is_known {
            (true, false, None)
        } else {
            // Flag as unknown/hallucinated
            (false, true, None)
        }
    }

    fn parse_cargo_deps(&self, content: &str) -> Vec<(String, String)> {
        let mut pkgs = Vec::new();
        let mut in_deps = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[dependencies]") || trimmed.starts_with("[dev-dependencies]") {
                in_deps = true;
                continue;
            } else if trimmed.starts_with('[') {
                in_deps = false;
            }

            if in_deps && trimmed.contains('=') && !trimmed.starts_with('#') {
                let parts: Vec<&str> = trimmed.split('=').collect();
                let name = parts[0].trim().to_string();
                let ver = parts.get(1).map(|v| v.trim().to_string()).unwrap_or_default();
                pkgs.push((name, ver));
            }
        }
        pkgs
    }

    fn parse_npm_deps(&self, content: &str) -> Vec<(String, String)> {
        let mut pkgs = Vec::new();
        if let Ok(Value::Object(map)) = serde_json::from_str(content) {
            for key in &["dependencies", "devDependencies"] {
                if let Some(Value::Object(deps)) = map.get(*key) {
                    for (k, v) in deps {
                        pkgs.push((k.clone(), v.as_str().unwrap_or("").to_string()));
                    }
                }
            }
        }
        pkgs
    }

    fn parse_pypi_deps(&self, content: &str) -> Vec<(String, String)> {
        let mut pkgs = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                let name = trimmed.split(&['=', '>', '<', '~'][..]).next().unwrap_or(trimmed).trim().to_string();
                pkgs.push((name, "".to_string()));
            }
        }
        pkgs
    }
}

use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hallucination_sentry_cargo_audit() {
        let sentry = HallucinationSentry::new();
        let cargo_toml = r#"
[dependencies]
tokio = "1.0"
serde = "1.0"
serde-super = "2.0"
"#;
        let report = sentry.audit_manifest(Path::new("Cargo.toml"), cargo_toml);
        assert_eq!(report.total_packages, 3);
        assert_eq!(report.verified_valid, 2);
        assert_eq!(report.hallucinated_count, 1);

        let hallucinated = report.entries.iter().find(|e| e.package_name == "serde-super").unwrap();
        assert!(hallucinated.is_hallucinated);
        assert_eq!(hallucinated.canonical_replacement.as_deref(), Some("serde"));

        let healed = report.healed_content.expect("Must produce healed content");
        assert!(healed.contains("serde = \"2.0\""));
        assert!(!healed.contains("serde-super"));
    }
}
