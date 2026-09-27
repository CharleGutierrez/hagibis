//! # ShadowWorkspace - Ephemeral Pre-Flight Validation & Auto-Repair Engine
//!
//! Elevates Cursor's Shadow Workspace concept. Creates an isolated scratch
//! environment to compile, typecheck, and validate code diffs before they
//! ever touch the developer's working tree.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Diagnostic error or warning emitted during pre-flight check
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowDiagnostic {
    pub file_path: String,
    pub line_number: usize,
    pub column: usize,
    pub message: String,
    pub severity: String,
}

/// Result of pre-flight validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightResult {
    pub is_valid: bool,
    pub compiler_output: String,
    pub diagnostics: Vec<ShadowDiagnostic>,
    pub diff_stats: String,
    pub repaired_content: Option<String>,
}

/// Project build runner detected in workspace
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildRunner {
    Cargo,
    TypeScript,
    Node,
    Python,
    Go,
    Generic,
}

pub struct ShadowWorkspace {
    pub workspace_root: PathBuf,
    pub shadow_root: PathBuf,
}

impl ShadowWorkspace {
    pub fn new(workspace_root: impl AsRef<Path>, shadow_dir_name: Option<&str>) -> Self {
        let ws = workspace_root.as_ref().to_path_buf();
        let shadow_name = shadow_dir_name.unwrap_or(".hgb_shadow");
        let shadow = ws.join(".hgb").join(shadow_name);
        Self {
            workspace_root: ws,
            shadow_root: shadow,
        }
    }

    /// Initialize the shadow workspace directory
    pub fn init(&self) -> Result<()> {
        if !self.shadow_root.exists() {
            fs::create_dir_all(&self.shadow_root).map_err(|e| HgbError::Io(e))?;
        }
        Ok(())
    }

    /// Clean up shadow directory
    pub fn cleanup(&self) -> Result<()> {
        if self.shadow_root.exists() {
            let _ = fs::remove_dir_all(&self.shadow_root);
        }
        Ok(())
    }

    /// Detect the dominant build runner in workspace
    pub fn detect_runner(&self) -> BuildRunner {
        if self.workspace_root.join("Cargo.toml").exists() {
            BuildRunner::Cargo
        } else if self.workspace_root.join("tsconfig.json").exists() {
            BuildRunner::TypeScript
        } else if self.workspace_root.join("package.json").exists() {
            BuildRunner::Node
        } else if self.workspace_root.join("pyproject.toml").exists()
            || self.workspace_root.join("requirements.txt").exists()
        {
            BuildRunner::Python
        } else if self.workspace_root.join("go.mod").exists() {
            BuildRunner::Go
        } else {
            BuildRunner::Generic
        }
    }

    /// Stage a modified candidate file into the shadow workspace
    pub fn stage_file(&self, relative_path: &Path, content: &str) -> Result<PathBuf> {
        self.init()?;
        let target_path = self.shadow_root.join(relative_path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|e| HgbError::Io(e))?;
        }
        fs::write(&target_path, content).map_err(|e| HgbError::Io(e))?;
        Ok(target_path)
    }

    /// Run preflight validation against the staged code
    pub fn validate_file(
        &self,
        relative_path: &Path,
        candidate_content: &str,
    ) -> Result<PreflightResult> {
        let shadow_file = self.stage_file(relative_path, candidate_content)?;

        // Compute diff statistics compared to original working tree
        let orig_path = self.workspace_root.join(relative_path);
        let orig_content = if orig_path.exists() {
            fs::read_to_string(&orig_path).unwrap_or_default()
        } else {
            String::new()
        };
        let diff_stats = Self::compute_diff_stats(&orig_content, candidate_content);

        let _runner = self.detect_runner();
        let ext = relative_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let mut is_valid = true;
        let mut compiler_output = String::new();
        let mut diagnostics = Vec::new();
        let mut repaired_content = None;

        // Perform fast local syntax / compile verification
        match ext {
            "py" => {
                let out = Command::new("python3")
                    .args(&["-m", "py_compile", shadow_file.to_str().unwrap_or("")])
                    .output();
                if let Ok(res) = out {
                    if !res.status.success() {
                        is_valid = false;
                        compiler_output = String::from_utf8_lossy(&res.stderr).to_string();
                        diagnostics = Self::parse_python_errors(&compiler_output);
                    } else {
                        compiler_output = "Python syntax check passed clean.".to_string();
                    }
                }
            }
            "js" | "mjs" => {
                let out = Command::new("node")
                    .args(&["-c", shadow_file.to_str().unwrap_or("")])
                    .output();
                if let Ok(res) = out {
                    if !res.status.success() {
                        is_valid = false;
                        compiler_output = String::from_utf8_lossy(&res.stderr).to_string();
                        diagnostics = Self::parse_node_errors(&compiler_output);
                    } else {
                        compiler_output = "Node syntax check passed clean.".to_string();
                    }
                }
            }
            "rs" => {
                let out = Command::new("rustc")
                    .args(&[
                        "--crate-type",
                        "lib",
                        "--emit=metadata",
                        "--out-dir",
                        self.shadow_root.to_str().unwrap_or("/tmp"),
                        shadow_file.to_str().unwrap_or(""),
                    ])
                    .output();
                if let Ok(res) = out {
                    if !res.status.success() {
                        is_valid = false;
                        compiler_output = String::from_utf8_lossy(&res.stderr).to_string();
                        diagnostics = Self::parse_rustc_errors(&compiler_output);
                    } else {
                        compiler_output = "Rustc metadata compilation passed clean.".to_string();
                    }
                } else {
                    // Fallback to internal syntax balance checks
                    if let Err(e) = Self::verify_bracket_balance(candidate_content) {
                        is_valid = false;
                        compiler_output = format!("Syntax structure error: {}", e);
                        diagnostics.push(ShadowDiagnostic {
                            file_path: relative_path.to_string_lossy().to_string(),
                            line_number: 1,
                            column: 1,
                            message: e,
                            severity: "error".to_string(),
                        });
                    }
                }
            }
            _ => {
                // Generic syntax balance check
                if let Err(e) = Self::verify_bracket_balance(candidate_content) {
                    is_valid = false;
                    compiler_output = format!("Generic syntax error: {}", e);
                    diagnostics.push(ShadowDiagnostic {
                        file_path: relative_path.to_string_lossy().to_string(),
                        line_number: 1,
                        column: 1,
                        message: e,
                        severity: "error".to_string(),
                    });
                } else {
                    compiler_output = "Generic structural syntax passed.".to_string();
                }
            }
        }

        // If invalid, attempt speculative auto-repair
        if !is_valid {
            if let Some(repaired) = Self::attempt_speculative_repair(candidate_content, &diagnostics) {
                repaired_content = Some(repaired);
            }
        }

        Ok(PreflightResult {
            is_valid,
            compiler_output,
            diagnostics,
            diff_stats,
            repaired_content,
        })
    }

    /// Attempt speculative auto-repairs for common hallucinations
    pub fn attempt_speculative_repair(
        content: &str,
        diagnostics: &[ShadowDiagnostic],
    ) -> Option<String> {
        let mut fixed = content.to_string();
        let mut was_fixed = false;

        for diag in diagnostics {
            if diag.message.contains("unclosed") || diag.message.contains("mismatched bracket") {
                // Check if closing brace or parenthesis was omitted at EOF
                let open_curlies = content.chars().filter(|&c| c == '{').count();
                let close_curlies = content.chars().filter(|&c| c == '}').count();
                if open_curlies > close_curlies {
                    for _ in 0..(open_curlies - close_curlies) {
                        fixed.push_str("\n}\n");
                    }
                    was_fixed = true;
                }
            }
        }

        if was_fixed {
            Some(fixed)
        } else {
            None
        }
    }

    fn verify_bracket_balance(code: &str) -> std::result::Result<(), String> {
        let mut stack = Vec::new();
        for (line_idx, line) in code.lines().enumerate() {
            // Skip comments
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("#") {
                continue;
            }

            for c in line.chars() {
                match c {
                    '{' | '(' | '[' => stack.push((c, line_idx + 1)),
                    '}' => {
                        if stack.pop().map(|(o, _)| o) != Some('{') {
                            return Err(format!("Mismatched closing brace '}}' at line {}", line_idx + 1));
                        }
                    }
                    ')' => {
                        if stack.pop().map(|(o, _)| o) != Some('(') {
                            return Err(format!("Mismatched closing parenthesis ')' at line {}", line_idx + 1));
                        }
                    }
                    ']' => {
                        if stack.pop().map(|(o, _)| o) != Some('[') {
                            return Err(format!("Mismatched closing bracket ']' at line {}", line_idx + 1));
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some((unclosed, line)) = stack.pop() {
            return Err(format!("Unclosed '{}' opened at line {}", unclosed, line));
        }

        Ok(())
    }

    fn parse_python_errors(stderr: &str) -> Vec<ShadowDiagnostic> {
        let mut diags = Vec::new();
        for line in stderr.lines() {
            if line.contains("SyntaxError:") {
                diags.push(ShadowDiagnostic {
                    file_path: "python_script".to_string(),
                    line_number: 1,
                    column: 1,
                    message: line.trim().to_string(),
                    severity: "error".to_string(),
                });
            }
        }
        diags
    }

    fn parse_node_errors(stderr: &str) -> Vec<ShadowDiagnostic> {
        let mut diags = Vec::new();
        for line in stderr.lines() {
            if line.contains("SyntaxError:") {
                diags.push(ShadowDiagnostic {
                    file_path: "node_script".to_string(),
                    line_number: 1,
                    column: 1,
                    message: line.trim().to_string(),
                    severity: "error".to_string(),
                });
            }
        }
        diags
    }

    fn parse_rustc_errors(stderr: &str) -> Vec<ShadowDiagnostic> {
        let mut diags = Vec::new();
        for line in stderr.lines() {
            if line.contains("error:") || line.contains("error[E") {
                diags.push(ShadowDiagnostic {
                    file_path: "rust_src".to_string(),
                    line_number: 1,
                    column: 1,
                    message: line.trim().to_string(),
                    severity: "error".to_string(),
                });
            }
        }
        diags
    }

    fn compute_diff_stats(orig: &str, patched: &str) -> String {
        let orig_lines: Vec<&str> = orig.lines().collect();
        let patched_lines: Vec<&str> = patched.lines().collect();

        let mut added = 0;
        let mut removed = 0;

        if orig_lines.len() < patched_lines.len() {
            added = patched_lines.len() - orig_lines.len();
        } else {
            removed = orig_lines.len() - patched_lines.len();
        }

        format!("+{} / -{} lines", added, removed)
    }
}
