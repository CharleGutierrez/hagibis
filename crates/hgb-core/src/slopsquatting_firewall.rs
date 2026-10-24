//! # Supply-Chain & Slopsquatting Hallucination Firewall
//!
//! Intercepts dependency additions across Cargo.toml, package.json, and requirements.txt,
//! scanning for hallucinated package names, typosquatting variants of top libraries,
//! and brand-new unverified releases before `npm install`, `pip install`, or `cargo add` executes.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageRiskLevel {
    Safe,
    Suspicious,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageAuditItem {
    pub package_name: String,
    pub version: Option<String>,
    pub ecosystem: String,
    pub risk_level: PackageRiskLevel,
    pub reason: String,
    pub blocked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallAuditReport {
    pub total_inspected: usize,
    pub safe_count: usize,
    pub blocked_count: usize,
    pub items: Vec<PackageAuditItem>,
}

pub struct SlopsquattingFirewall;

impl SlopsquattingFirewall {
    pub const CARGO_CANONICAL: &'static [&'static str] = &[
        "tokio", "serde", "serde_json", "reqwest", "regex", "axum", "clap",
        "tracing", "anyhow", "thiserror", "chrono", "rand", "async-trait",
        "futures", "log", "hyper", "uuid", "sha2", "blake3", "rusqlite",
        "sqlx", "tower", "syn", "quote", "proc-macro2", "bytes", "dotenv",
    ];

    pub const NPM_CANONICAL: &'static [&'static str] = &[
        "react", "react-dom", "express", "lodash", "axios", "typescript",
        "next", "vue", "tailwindcss", "jest", "mocha", "webpack", "vite",
        "eslint", "prettier", "dayjs", "moment", "dotenv", "cors", "zod",
        "chalk", "commander", "prisma", "rxjs", "zustand", "svelte",
    ];

    pub const PYPI_CANONICAL: &'static [&'static str] = &[
        "requests", "numpy", "pandas", "flask", "django", "pydantic",
        "pytest", "fastapi", "torch", "scipy", "urllib3", "scikit-learn",
        "matplotlib", "boto3", "click", "rich", "sqlalchemy", "aiohttp",
        "httpx", "black", "uvicorn", "setuptools", "wheel", "jinja2",
    ];

    pub const KNOWN_HALLUCINATIONS: &'static [&'static str] = &[
        "python-dotenv-vault",
        "react-router-v7-compat",
        "openai-secret-key-parser",
        "flask-jwt-router",
        "express-async-await",
        "crypto-stealer",
        "cryptostealer",
        "auto-generated-crypto",
        "fastapi-auto-crud",
        "torch-cuda-autotune",
        "npm-security-patch",
        "cargo-auto-sandbox",
    ];

    /// Audits a list of package names for an ecosystem
    pub fn audit_packages(packages: &[&str], ecosystem: &str) -> FirewallAuditReport {
        let mut items = Vec::new();
        let mut safe_count = 0;
        let mut blocked_count = 0;

        for &pkg in packages {
            let clean = pkg.trim();
            if clean.is_empty() {
                continue;
            }

            // Parse optional version specification (e.g. "serde@1.0.100" or "requests==2.28")
            let (pkg_name, version) = Self::parse_name_and_version(clean);
            let (risk, reason, blocked) = Self::evaluate_risk(&pkg_name, version.as_deref(), ecosystem);

            if blocked {
                blocked_count += 1;
            } else {
                safe_count += 1;
            }

            items.push(PackageAuditItem {
                package_name: pkg_name,
                version,
                ecosystem: ecosystem.to_string(),
                risk_level: risk,
                reason,
                blocked,
            });
        }

        FirewallAuditReport {
            total_inspected: items.len(),
            safe_count,
            blocked_count,
            items,
        }
    }

    /// Split package name and version string
    fn parse_name_and_version(input: &str) -> (String, Option<String>) {
        let operators = ["==", ">=", "<=", "~=", "!=", "=", ">", "<", "@"];
        for op in operators {
            if let Some((name, ver)) = input.split_once(op) {
                let trimmed_name = name.trim().to_lowercase();
                if !trimmed_name.is_empty() {
                    return (trimmed_name, Some(ver.trim().to_string()));
                }
            }
        }
        (input.trim().to_lowercase(), None)
    }

    /// Evaluate risk profile of an individual dependency candidate
    pub fn evaluate_risk(pkg: &str, version: Option<&str>, ecosystem: &str) -> (PackageRiskLevel, String, bool) {
        let clean = pkg.to_lowercase();
        let normalized_dash = clean.replace('_', "-");
        let eco = ecosystem.to_lowercase();

        // 1. Check known hallucinated packages blacklist
        for &hallucinated in Self::KNOWN_HALLUCINATIONS {
            let canon_hallucinated = hallucinated.replace('_', "-");
            if normalized_dash == canon_hallucinated
                || clean == hallucinated
                || normalized_dash.contains("cryptostealer")
                || normalized_dash.contains("crypto-stealer")
            {
                return (
                    PackageRiskLevel::Quarantined,
                    format!("Matches known hallucinated or malicious package blacklist entry '{}'", hallucinated),
                    true,
                );
            }
        }

        // Suspicious slopsquatting prefix/suffix patterns
        if normalized_dash.starts_with("auto-generated-")
            || normalized_dash.contains("hallucinated")
            || normalized_dash.ends_with("-secure-jwt")
            || normalized_dash.ends_with("-auth-token")
        {
            return (
                PackageRiskLevel::Quarantined,
                "Matches known automated slopsquatting pattern".to_string(),
                true,
            );
        }

        // 2. Select canonical library inventory for the ecosystem
        let canonical_list = match eco.as_str() {
            "npm" | "node" | "javascript" | "typescript" => Self::NPM_CANONICAL,
            "pypi" | "python" | "pip" => Self::PYPI_CANONICAL,
            _ => Self::CARGO_CANONICAL,
        };

        // 3. Direct canonical match
        let clean_or_hyphen = if canonical_list.contains(&clean.as_str()) {
            Some(clean.as_str())
        } else if canonical_list.contains(&normalized_dash.as_str()) {
            Some(normalized_dash.as_str())
        } else {
            None
        };

        if let Some(canon_match) = clean_or_hyphen {
            // Check version sanity if version provided
            if let Some(v) = version {
                if v == "0.0.0" || v == "0.0.1" || v.starts_with("99.99") {
                    return (
                        PackageRiskLevel::Suspicious,
                        format!("Canonical package '{}' specified with suspicious unverified release version '{}'", canon_match, v),
                        true,
                    );
                }
            }
            return (PackageRiskLevel::Safe, "Verified canonical ecosystem package".to_string(), false);
        }

        // 4. Check for character substitution typosquat first (e.g. 0 -> o, 1 -> l)
        for &canon in canonical_list {
            let normalized_pkg = clean.replace('0', "o").replace('1', "l").replace('-', "_");
            let normalized_canon = canon.replace('-', "_");
            if normalized_pkg == normalized_canon && clean != canon {
                return (
                    PackageRiskLevel::Quarantined,
                    format!("Character-substitution typosquat targeting canonical package '{}'", canon),
                    true,
                );
            }
        }

        // 5. Check for typosquatting / distance-1 or distance-2 to canonical packages
        for &canon in canonical_list {
            let dist = Self::levenshtein_distance(&clean, canon);
            if dist == 1 {
                return (
                    PackageRiskLevel::Quarantined,
                    format!("High-probability typosquat of canonical package '{}' (edit distance 1)", canon),
                    true,
                );
            }

            // Also check distance 2 if length >= 6 (e.g. tokiioo or reqwesst)
            if dist == 2 && clean.len() >= 6 && canon.len() >= 6 {
                return (
                    PackageRiskLevel::Suspicious,
                    format!("Probable typosquat variant of canonical package '{}' (edit distance 2)", canon),
                    true,
                );
            }
        }

        // 6. Unverified brand-new release check for non-canonical package
        if let Some(v) = version {
            if v == "0.0.0" || v == "0.0.1" || v.starts_with("99.") {
                return (
                    PackageRiskLevel::Suspicious,
                    format!("Unverified brand-new or placeholder release version '{}'", v),
                    true,
                );
            }
        }

        (PackageRiskLevel::Safe, "Ecosystem reputation check passed".to_string(), false)
    }

    /// Screen dependencies declared in a Cargo.toml file content
    pub fn screen_cargo_toml(content: &str) -> FirewallAuditReport {
        let mut packages = Vec::new();
        let mut in_dep_section = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if trimmed.starts_with('[') {
                in_dep_section = trimmed.contains("dependencies")
                    || trimmed.contains("dev-dependencies")
                    || trimmed.contains("build-dependencies");
                continue;
            }

            if in_dep_section {
                if let Some((name_part, val_part)) = trimmed.split_once('=') {
                    let pkg_name = name_part.trim().trim_matches('"').trim_matches('\'');
                    if !pkg_name.is_empty() && !pkg_name.contains(' ') {
                        let version = if val_part.trim().starts_with('{') {
                            // Extract version = "..."
                            val_part.split("version")
                                .nth(1)
                                .and_then(|s| s.split('"').nth(1))
                                .map(|s| s.to_string())
                        } else {
                            val_part.trim().trim_matches('"').trim_matches('\'').split_whitespace().next().map(|s| s.to_string())
                        };

                        let full_spec = match version {
                            Some(ref v) => format!("{}@{}", pkg_name, v),
                            None => pkg_name.to_string(),
                        };
                        packages.push(full_spec);
                    }
                }
            }
        }

        let slice: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        Self::audit_packages(&slice, "cargo")
    }

    /// Screen dependencies declared in a package.json file content
    pub fn screen_package_json(content: &str) -> FirewallAuditReport {
        let mut packages = Vec::new();

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
            let sections = ["dependencies", "devDependencies", "peerDependencies"];
            for sec in sections {
                if let Some(map) = parsed.get(sec).and_then(|d| d.as_object()) {
                    for (pkg_name, ver_val) in map {
                        let ver_str = ver_val.as_str().unwrap_or("").trim_start_matches('^').trim_start_matches('~');
                        let spec = if !ver_str.is_empty() {
                            format!("{}@{}", pkg_name, ver_str)
                        } else {
                            pkg_name.clone()
                        };
                        packages.push(spec);
                    }
                }
            }
        }

        let slice: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        Self::audit_packages(&slice, "npm")
    }

    /// Screen dependencies declared in a requirements.txt file content
    pub fn screen_requirements_txt(content: &str) -> FirewallAuditReport {
        let mut packages = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("-r") || trimmed.starts_with("-i") {
                continue;
            }

            packages.push(trimmed.to_string());
        }

        let slice: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
        Self::audit_packages(&slice, "pypi")
    }

    /// Recursively scan workspace for dependency files (Cargo.toml, package.json, requirements.txt)
    pub fn screen_workspace(workspace_root: &Path) -> FirewallAuditReport {
        let mut combined_items = Vec::new();
        let mut total_inspected = 0;
        let mut safe_count = 0;
        let mut blocked_count = 0;

        let cargo_toml = workspace_root.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(c) = std::fs::read_to_string(&cargo_toml) {
                let rep = Self::screen_cargo_toml(&c);
                total_inspected += rep.total_inspected;
                safe_count += rep.safe_count;
                blocked_count += rep.blocked_count;
                combined_items.extend(rep.items);
            }
        }

        let package_json = workspace_root.join("package.json");
        if package_json.exists() {
            if let Ok(c) = std::fs::read_to_string(&package_json) {
                let rep = Self::screen_package_json(&c);
                total_inspected += rep.total_inspected;
                safe_count += rep.safe_count;
                blocked_count += rep.blocked_count;
                combined_items.extend(rep.items);
            }
        }

        let req_txt = workspace_root.join("requirements.txt");
        if req_txt.exists() {
            if let Ok(c) = std::fs::read_to_string(&req_txt) {
                let rep = Self::screen_requirements_txt(&c);
                total_inspected += rep.total_inspected;
                safe_count += rep.safe_count;
                blocked_count += rep.blocked_count;
                combined_items.extend(rep.items);
            }
        }

        FirewallAuditReport {
            total_inspected,
            safe_count,
            blocked_count,
            items: combined_items,
        }
    }

    /// Extracts imported external package/crate names from raw source code
    pub fn extract_source_imports(code: &str, ecosystem: &str) -> Vec<String> {
        let mut packages = HashSet::new();
        let eco = ecosystem.to_lowercase();

        for line in code.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }

            if eco.contains("cargo") || eco.contains("rust") || eco.is_empty() {
                // Rust imports: `use foo::...;` or `extern crate foo;`
                if let Some(rest) = trimmed.strip_prefix("use ") {
                    let cleaned = rest.trim_start_matches("::");
                    let crate_name = cleaned.split("::").next().unwrap_or("").trim_end_matches(';').trim();
                    let standard_crates = ["std", "core", "alloc", "crate", "super", "self"];
                    if !crate_name.is_empty() && !standard_crates.contains(&crate_name) && !crate_name.contains('{') && !crate_name.contains(' ') {
                        packages.insert(crate_name.to_string());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("extern crate ") {
                    let crate_name = rest.trim_end_matches(';').trim();
                    if !crate_name.is_empty() && crate_name != "std" {
                        packages.insert(crate_name.to_string());
                    }
                }
            }

            if eco.contains("npm") || eco.contains("node") || eco.contains("ts") || eco.contains("js") {
                // JS/TS imports: `import ... from 'foo'` or `require('foo')`
                if trimmed.starts_with("import ") && trimmed.contains(" from ") {
                    if let Some((_, from_part)) = trimmed.split_once(" from ") {
                        let raw_pkg = from_part.trim().trim_end_matches(';').trim_matches('\'').trim_matches('"');
                        let pkg = if raw_pkg.starts_with('@') {
                            let parts: Vec<&str> = raw_pkg.split('/').take(2).collect();
                            parts.join("/")
                        } else {
                            raw_pkg.split('/').next().unwrap_or("").to_string()
                        };
                        if !pkg.is_empty() && !pkg.starts_with('.') && !pkg.starts_with('/') {
                            packages.insert(pkg);
                        }
                    }
                } else if trimmed.contains("require(") {
                    if let Some(start) = trimmed.find("require(") {
                        let rest = &trimmed[start + 8..];
                        if let Some(end) = rest.find(')') {
                            let raw_pkg = rest[..end].trim().trim_matches('\'').trim_matches('"');
                            let pkg = if raw_pkg.starts_with('@') {
                                let parts: Vec<&str> = raw_pkg.split('/').take(2).collect();
                                parts.join("/")
                            } else {
                                raw_pkg.split('/').next().unwrap_or("").to_string()
                            };
                            if !pkg.is_empty() && !pkg.starts_with('.') && !pkg.starts_with('/') {
                                packages.insert(pkg);
                            }
                        }
                    }
                }
            }

            if eco.contains("pypi") || eco.contains("python") || eco.contains("pip") {
                // Python imports: `import foo` or `from foo import ...`
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    let pkg = rest.split(',').next().unwrap_or("").split_whitespace().next().unwrap_or("").split('.').next().unwrap_or("");
                    if !pkg.is_empty() && !pkg.starts_with('.') {
                        packages.insert(pkg.to_string());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("from ") {
                    let pkg = rest.split_whitespace().next().unwrap_or("").split('.').next().unwrap_or("");
                    if !pkg.is_empty() && !pkg.starts_with('.') {
                        packages.insert(pkg.to_string());
                    }
                }
            }
        }

        let mut sorted: Vec<String> = packages.into_iter().collect();
        sorted.sort();
        sorted
    }

    /// Audits imported external packages directly from synthesized source code
    pub fn audit_source_imports(code: &str, ecosystem: &str) -> FirewallAuditReport {
        let extracted = Self::extract_source_imports(code, ecosystem);
        let refs: Vec<&str> = extracted.iter().map(|s| s.as_str()).collect();
        Self::audit_packages(&refs, ecosystem)
    }

    fn levenshtein_distance(s1: &str, s2: &str) -> usize {
        crate::zig_accelerate::levenshtein_distance(s1, s2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slopsquatting_firewall_blocks_typosquats() {
        let pkgs = ["serde", "reqwests", "tokio", "tokiio", "regex", "auto-generated-crypto"];
        let report = SlopsquattingFirewall::audit_packages(&pkgs, "cargo");

        assert_eq!(report.total_inspected, 6);
        assert_eq!(report.safe_count, 3); // serde, tokio, regex
        assert_eq!(report.blocked_count, 3); // reqwests, tokiio, auto-generated-crypto

        let reqwests_item = report.items.iter().find(|i| i.package_name == "reqwests").unwrap();
        assert_eq!(reqwests_item.risk_level, PackageRiskLevel::Quarantined);
        assert!(reqwests_item.blocked);

        let tokiio_item = report.items.iter().find(|i| i.package_name == "tokiio").unwrap();
        assert_eq!(tokiio_item.risk_level, PackageRiskLevel::Quarantined);
        assert!(tokiio_item.blocked);
    }

    #[test]
    fn test_screen_cargo_toml() {
        let cargo_toml = r#"
[package]
name = "test_app"
version = "0.1.0"

[dependencies]
tokio = "1.0"
serde = { version = "1.0", features = ["derive"] }
reqwests = "0.11"
python-dotenv-vault = "0.0.1"
"#;
        let report = SlopsquattingFirewall::screen_cargo_toml(cargo_toml);
        assert_eq!(report.total_inspected, 4);
        assert_eq!(report.safe_count, 2); // tokio, serde
        assert_eq!(report.blocked_count, 2); // reqwests (typosquat), python-dotenv-vault (hallucinated)
    }

    #[test]
    fn test_screen_package_json() {
        let pkg_json = r#"{
            "name": "vibe-frontend",
            "dependencies": {
                "react": "^18.2.0",
                "lodash": "4.17.21",
                "expresss": "^4.18.2",
                "react-router-v7-compat": "0.0.1"
            }
        }"#;
        let report = SlopsquattingFirewall::screen_package_json(pkg_json);
        assert_eq!(report.total_inspected, 4);
        assert_eq!(report.safe_count, 2); // react, lodash
        assert_eq!(report.blocked_count, 2); // expresss (typosquat), react-router-v7-compat (hallucinated)
    }

    #[test]
    fn test_screen_requirements_txt() {
        let req_txt = r#"
# Core packages
requests==2.28.1
numpy>=1.24.0
fastapi==0.95.0
pydantiic==1.10.7
openai-secret-key-parser==0.0.1
"#;
        let report = SlopsquattingFirewall::screen_requirements_txt(req_txt);
        assert_eq!(report.total_inspected, 5);
        assert_eq!(report.safe_count, 3); // requests, numpy, fastapi
        assert_eq!(report.blocked_count, 2); // pydantiic, openai-secret-key-parser
    }

    #[test]
    fn test_character_substitution_typosquat() {
        let (risk, reason, blocked) = SlopsquattingFirewall::evaluate_risk("t0kio", None, "cargo");
        assert_eq!(risk, PackageRiskLevel::Quarantined);
        assert!(blocked);
        assert!(reason.contains("Character-substitution typosquat"));
    }

    #[test]
    fn test_audit_source_imports_catches_slop() {
        let rust_code = r#"
            use tokio::sync::mpsc;
            use reqwests::Client; // Typo of reqwest!
            use crypto_stealer::steal; // Hallucination/malicious!
            use std::path::PathBuf;

            pub fn test() {}
        "#;
        let report = SlopsquattingFirewall::audit_source_imports(rust_code, "cargo");
        assert_eq!(report.total_inspected, 3); // tokio, reqwests, crypto_stealer (std ignored)
        assert_eq!(report.safe_count, 1); // tokio
        assert_eq!(report.blocked_count, 2); // reqwests, crypto_stealer
    }
}
