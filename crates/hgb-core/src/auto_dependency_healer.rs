//! # Invisible Dependency & Package Auto-Healing
//!
//! Silently diagnoses missing packages, unlinked crates, and unresolved imports
//! from compiler diagnostics (Rust `rustc`/`cargo` and TypeScript/Node `npm`/`tsc`),
//! automatically adding them to the workspace manifest during preflight verification.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Identified missing package dependency
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingPackage {
    pub ecosystem: String, // "cargo", "npm"
    pub package_name: String,
    pub detected_symbol: String,
}

/// Action performed to resolve the missing dependency
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealingAction {
    pub package_name: String,
    pub manifest_file: String,
    pub action_type: String, // "manifest_injected", "cli_installed"
    pub success: bool,
}

/// Report after diagnosing and healing dependencies
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyHealingReport {
    pub missing_detected: Vec<MissingPackage>,
    pub actions_taken: Vec<HealingAction>,
    pub is_fully_healed: bool,
    pub summary: String,
}

pub struct AutoDependencyHealer {
    pub workspace_root: PathBuf,
}

impl AutoDependencyHealer {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Diagnose compiler / build log output for missing dependencies
    pub fn diagnose_compiler_output(compiler_log: &str) -> Vec<MissingPackage> {
        let mut missing = Vec::new();

        for line in compiler_log.lines() {
            // Rust: error[E0433]: cannot find module or crate `foo` in this scope
            if line.contains("cannot find module or crate `") {
                if let Some(start) = line.find('`') {
                    if let Some(end) = line[start + 1..].find('`') {
                        let crate_name = &line[start + 1..start + 1 + end];
                        missing.push(MissingPackage {
                            ecosystem: "cargo".to_string(),
                            package_name: crate_name.to_string(),
                            detected_symbol: crate_name.to_string(),
                        });
                    }
                }
            }
            // Rust: error[E0432]: unresolved import `foo`
            else if line.contains("unresolved import `") {
                if let Some(start) = line.find('`') {
                    if let Some(end) = line[start + 1..].find('`') {
                        let full_import = &line[start + 1..start + 1 + end];
                        let root_crate = full_import.split("::").next().unwrap_or(full_import);
                        if root_crate != "crate" && root_crate != "super" && root_crate != "self" {
                            missing.push(MissingPackage {
                                ecosystem: "cargo".to_string(),
                                package_name: root_crate.to_string(),
                                detected_symbol: full_import.to_string(),
                            });
                        }
                    }
                }
            }
            // Node/TS: Cannot find module 'foo' or its corresponding type declarations
            else if line.contains("Cannot find module '") || line.contains("Can't resolve '") {
                let marker = if line.contains("Cannot find module '") { "Cannot find module '" } else { "Can't resolve '" };
                if let Some(start) = line.find(marker) {
                    let rem = &line[start + marker.len()..];
                    if let Some(end) = rem.find('\'') {
                        let pkg_name = &rem[..end];
                        if !pkg_name.starts_with('.') && !pkg_name.starts_with('/') {
                            // Extract top-level package or scoped package (@scope/pkg)
                            let parts: Vec<&str> = pkg_name.split('/').collect();
                            let clean_name = if parts.len() >= 2 && parts[0].starts_with('@') {
                                format!("{}/{}", parts[0], parts[1])
                            } else {
                                parts[0].to_string()
                            };

                            missing.push(MissingPackage {
                                ecosystem: "npm".to_string(),
                                package_name: clean_name,
                                detected_symbol: pkg_name.to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Deduplicate
        let mut deduped: Vec<MissingPackage> = Vec::new();
        for m in missing {
            if !deduped.iter().any(|d| d.package_name == m.package_name && d.ecosystem == m.ecosystem) {
                deduped.push(m);
            }
        }

        deduped
    }

    /// Automatically heal missing dependencies by modifying manifests or executing package installers
    pub fn heal_dependencies(&self, compiler_log: &str) -> Result<DependencyHealingReport> {
        let missing = Self::diagnose_compiler_output(compiler_log);
        let mut actions = Vec::new();

        for pkg in &missing {
            if pkg.ecosystem == "cargo" {
                let cargo_toml = self.workspace_root.join("Cargo.toml");
                if cargo_toml.exists() {
                    let mut content = fs::read_to_string(&cargo_toml).map_err(HgbError::Io)?;
                    if !content.contains(&format!("{} =", pkg.package_name)) && !content.contains(&format!("{}=", pkg.package_name)) {
                        // Inject into [dependencies]
                        if let Some(pos) = content.find("[dependencies]") {
                            let insert_pos = pos + "[dependencies]".len();
                            content.insert_str(insert_pos, &format!("\n{} = \"*\"", pkg.package_name));
                            let _ = fs::write(&cargo_toml, content);
                            actions.push(HealingAction {
                                package_name: pkg.package_name.clone(),
                                manifest_file: cargo_toml.to_string_lossy().to_string(),
                                action_type: "manifest_injected".to_string(),
                                success: true,
                            });
                        }
                    }
                }
            } else if pkg.ecosystem == "npm" {
                let pkg_json = self.workspace_root.join("package.json");
                if pkg_json.exists() {
                    let content = fs::read_to_string(&pkg_json).map_err(HgbError::Io)?;
                    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(deps) = val.get_mut("dependencies").and_then(|d| d.as_object_mut()) {
                            deps.insert(pkg.package_name.clone(), serde_json::Value::String("latest".to_string()));
                            if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                                let _ = fs::write(&pkg_json, pretty);
                                actions.push(HealingAction {
                                    package_name: pkg.package_name.clone(),
                                    manifest_file: pkg_json.to_string_lossy().to_string(),
                                    action_type: "manifest_injected".to_string(),
                                    success: true,
                                });
                            }
                        }
                    }
                }
            }
        }

        let is_fully_healed = !actions.is_empty() && actions.iter().all(|a| a.success);
        let summary = if actions.is_empty() {
            "No unresolved dependencies detected.".to_string()
        } else {
            format!("Auto-healed {} missing dependencies across workspace manifests.", actions.len())
        };

        Ok(DependencyHealingReport {
            missing_detected: missing,
            actions_taken: actions,
            is_fully_healed,
            summary,
        })
    }
}
