//! # Shell Mind-Reader & Autonomous Terminal Rescue Engine
//!
//! Analyzes failed shell executions (exit codes, stderr, stdout) and synthesizes
//! verified, safe corrective actions with AgentShieldLight validation.

use crate::error::Result;
use crate::security::AgentShieldLight;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Categorized failure mode detected from terminal execution output
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureCategory {
    PortConflict { port: u16, conflicting_process: Option<String> },
    MissingDependency { package: String, manager: String },
    GitStateIssue { reason: String },
    PermissionDenied { target_path: String },
    RustCompilationError { error_code: Option<String> },
    CommandNotFound { command: String },
    GenericExecutionFailure,
}

impl std::fmt::Display for FailureCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PortConflict { port, .. } => write!(f, "PORT_CONFLICT (:{})", port),
            Self::MissingDependency { package, manager } => write!(f, "MISSING_DEPENDENCY ({} via {})", package, manager),
            Self::GitStateIssue { reason } => write!(f, "GIT_STATE_ISSUE ({})", reason),
            Self::PermissionDenied { target_path } => write!(f, "PERMISSION_DENIED ({})", target_path),
            Self::RustCompilationError { error_code } => write!(f, "RUSTC_ERROR ({:?})", error_code),
            Self::CommandNotFound { command } => write!(f, "COMMAND_NOT_FOUND ({})", command),
            Self::GenericExecutionFailure => write!(f, "GENERIC_FAILURE"),
        }
    }
}

/// A verified, actionable corrective command suggested by the rescue engine
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectiveAction {
    pub description: String,
    pub command: String,
    pub is_destructive: bool,
    pub explanation: String,
}

/// Diagnostic report containing root cause and ranked corrective actions
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RescueReport {
    pub failed_command: String,
    pub exit_code: i32,
    pub category: FailureCategory,
    pub explanation: String,
    pub suggested_fixes: Vec<CorrectiveAction>,
}

/// The Terminal Rescue Engine
pub struct TerminalRescue;

impl TerminalRescue {
    /// Diagnose a failed shell command from its exit code and output streams
    pub fn diagnose(failed_cmd: &str, exit_code: i32, stderr: &str, stdout: &str) -> RescueReport {
        let combined = format!("{}\n{}", stdout, stderr);
        let trimmed_cmd = failed_cmd.trim();

        // 1. Port Conflict Detection (EADDRINUSE / Address already in use)
        let port_re = Regex::new(r"(?:EADDRINUSE|address already in use|port\s+(?:is\s+already\s+in\s+use|already\s+allocated)?).*?[:\s](\d{2,5})").unwrap();
        if let Some(cap) = port_re.captures(&combined) {
            let port_str = cap.get(1).map(|m| m.as_str()).unwrap_or("3000");
            let port = port_str.parse::<u16>().unwrap_or(3000);

            let fixes = vec![
                CorrectiveAction {
                    description: format!("Free port {} by terminating conflicting process", port),
                    command: format!("fuser -k {}/tcp", port),
                    is_destructive: false,
                    explanation: format!("Kills any lingering process holding port {} to permit clean restart.", port),
                },
                CorrectiveAction {
                    description: format!("Alternative: find PID and terminate via lsof (port {})", port),
                    command: format!("kill -9 $(lsof -t -i:{})", port),
                    is_destructive: false,
                    explanation: "Locates the process ID with lsof and sends SIGKILL.".to_string(),
                },
            ];

            return RescueReport {
                failed_command: trimmed_cmd.to_string(),
                exit_code,
                category: FailureCategory::PortConflict { port, conflicting_process: None },
                explanation: format!("Port {} is already bound by another process or lingering dev server.", port),
                suggested_fixes: fixes,
            };
        }

        // 2. Rust Compilation Errors
        if combined.contains("error[E") || combined.contains("could not compile `") {
            let err_code_re = Regex::new(r"error\[(E\d{4})\]").unwrap();
            let code = err_code_re.captures(&combined).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());

            let fixes = vec![
                CorrectiveAction {
                    description: "Launch Hagibis Self-Healing Loop".to_string(),
                    command: "hgb heal".to_string(),
                    is_destructive: false,
                    explanation: "Queries the AI provider to inspect diagnostics and surgically patch the compiler errors.".to_string(),
                },
                CorrectiveAction {
                    description: "Run compiler check with JSON diagnostics".to_string(),
                    command: "cargo check --message-format=json".to_string(),
                    is_destructive: false,
                    explanation: "Outputs structured compiler diagnostics for detailed analysis.".to_string(),
                },
            ];

            return RescueReport {
                failed_command: trimmed_cmd.to_string(),
                exit_code,
                category: FailureCategory::RustCompilationError { error_code: code.clone() },
                explanation: format!("Rust compiler error detected (code: {:?}). Type/borrow errors prevent build completion.", code),
                suggested_fixes: fixes,
            };
        }

        // 3. Missing Dependencies (Node.js / Python / Rust)
        if combined.contains("Cannot find module") || combined.contains("ModuleNotFoundError") || combined.contains("No module named") {
            let mod_re = Regex::new(r#"(?:Cannot find module ['"]([^'"]+)['"]|No module named ['"]([^'"]+)['"])"#).unwrap();
            let pkg = mod_re.captures(&combined)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .map(|m| m.as_str())
                .unwrap_or("unknown");

            let is_python = combined.contains("ModuleNotFoundError") || combined.contains("No module named");
            let manager = if is_python { "pip" } else { "npm" };
            let cmd = if is_python {
                format!("pip install {}", pkg)
            } else {
                format!("npm install {}", pkg)
            };

            let fixes = vec![
                CorrectiveAction {
                    description: format!("Install missing package '{}' via {}", pkg, manager),
                    command: cmd,
                    is_destructive: false,
                    explanation: format!("Installs {} into active environment.", pkg),
                },
            ];

            return RescueReport {
                failed_command: trimmed_cmd.to_string(),
                exit_code,
                category: FailureCategory::MissingDependency { package: pkg.to_string(), manager: manager.to_string() },
                explanation: format!("Required dependency '{}' is missing from the runtime environment.", pkg),
                suggested_fixes: fixes,
            };
        }

        // 4. Command Not Found
        if combined.contains("command not found") || combined.contains("not recognized as an internal or external command") {
            let cmd_re = Regex::new(r#"([a-zA-Z0-9_-]+):\s*(?:command\s+not\s+found|not\s+found)"#).unwrap();
            let missing_bin = cmd_re.captures(&combined)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str())
                .unwrap_or_else(|| trimmed_cmd.split_whitespace().next().unwrap_or(""));

            return RescueReport {
                failed_command: trimmed_cmd.to_string(),
                exit_code,
                category: FailureCategory::CommandNotFound { command: missing_bin.to_string() },
                explanation: format!("Binary '{}' is not installed or not in PATH.", missing_bin),
                suggested_fixes: vec![
                    CorrectiveAction {
                        description: format!("Check if '{}' is on PATH or install via package manager", missing_bin),
                        command: format!("which {} || echo 'Not installed'", missing_bin),
                        is_destructive: false,
                        explanation: "Verifies whether the command exists in system PATH.".to_string(),
                    },
                ],
            };
        }

        // 5. Git State Conflicts
        if combined.contains("merge conflict") || combined.contains("CONFLICT (content)") || combined.contains("you have divergent branches") {
            return RescueReport {
                failed_command: trimmed_cmd.to_string(),
                exit_code,
                category: FailureCategory::GitStateIssue { reason: "Merge/Rebase Conflict".to_string() },
                explanation: "Git working tree has unmerged conflicts or diverged upstream history.".to_string(),
                suggested_fixes: vec![
                    CorrectiveAction {
                        description: "Inspect active git conflicts with Hagibis Diff HUD".to_string(),
                        command: "git status -s".to_string(),
                        is_destructive: false,
                        explanation: "Lists conflicting files requiring manual resolution or AI patch.".to_string(),
                    },
                    CorrectiveAction {
                        description: "Abort current merge and restore clean tree".to_string(),
                        command: "git merge --abort".to_string(),
                        is_destructive: true,
                        explanation: "Aborts the broken merge and restores working tree to pre-merge commit.".to_string(),
                    },
                ],
            };
        }

        // 6. Permission Denied
        if combined.contains("Permission denied") || combined.contains("EACCES") {
            return RescueReport {
                failed_command: trimmed_cmd.to_string(),
                exit_code,
                category: FailureCategory::PermissionDenied { target_path: "target".to_string() },
                explanation: "File permission or write access denied on target path.".to_string(),
                suggested_fixes: vec![
                    CorrectiveAction {
                        description: "Check file permissions and ownership".to_string(),
                        command: format!("ls -la {}", trimmed_cmd.split_whitespace().last().unwrap_or(".")),
                        is_destructive: false,
                        explanation: "Audits current user permissions against the target path.".to_string(),
                    },
                ],
            };
        }

        // 7. Generic Fallback
        RescueReport {
            failed_command: trimmed_cmd.to_string(),
            exit_code,
            category: FailureCategory::GenericExecutionFailure,
            explanation: format!("Command exited with status code {}. Review stdout/stderr output.", exit_code),
            suggested_fixes: vec![
                CorrectiveAction {
                    description: "Execute with verbose diagnostics".to_string(),
                    command: format!("RUST_BACKTRACE=1 {}", trimmed_cmd),
                    is_destructive: false,
                    explanation: "Re-runs the command with backtraces enabled.".to_string(),
                },
            ],
        }
    }

    /// Audit a proposed corrective action against AgentShieldLight safety policies
    pub fn verify_action(action: &CorrectiveAction) -> Result<()> {
        AgentShieldLight::audit_command(&action.command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnose_port_conflict() {
        let stderr = "Error: listen EADDRINUSE: address already in use :::3000\n    at Server.setupListenHandle";
        let report = TerminalRescue::diagnose("npm run dev", 1, stderr, "");

        assert!(matches!(report.category, FailureCategory::PortConflict { port: 3000, .. }));
        assert!(!report.suggested_fixes.is_empty());
        assert!(report.suggested_fixes[0].command.contains("3000"));

        // Verify that the suggested corrective action passes AgentShieldLight
        assert!(TerminalRescue::verify_action(&report.suggested_fixes[0]).is_ok());
    }

    #[test]
    fn test_diagnose_rust_compiler_error() {
        let stderr = "error[E0308]: mismatched types\n --> src/main.rs:10:5\nexpected u32, found &str";
        let report = TerminalRescue::diagnose("cargo build", 101, stderr, "");

        assert!(matches!(report.category, FailureCategory::RustCompilationError { error_code: Some(_) }));
        assert!(report.suggested_fixes.iter().any(|f| f.command == "hgb heal"));
    }

    #[test]
    fn test_diagnose_missing_python_module() {
        let stderr = "Traceback (most recent call last):\n  File 'app.py', line 1\nModuleNotFoundError: No module named 'fastapi'";
        let report = TerminalRescue::diagnose("python app.py", 1, stderr, "");

        assert!(matches!(report.category, FailureCategory::MissingDependency { ref package, ref manager } if package == "fastapi" && manager == "pip"));
        assert!(report.suggested_fixes[0].command.contains("pip install fastapi"));
    }
}
