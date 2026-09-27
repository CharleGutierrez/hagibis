//! # ShellPanicHook - Warp-Style Interactive Shell Panic & 1-Key Auto-Repair
//!
//! Elevates Warp Terminal's interactive panic interceptor. Captures nonzero shell
//! exits, diagnoses the underlying failure, and synthesizes instantaneous 1-key
//! auto-repair commands directly in the developer's shell session.

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Classification of shell failure root cause
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellFailureCategory {
    MissingCommand,
    PortConflict,
    PermissionDenied,
    CompilationError,
    GitRemoteDivergence,
    MissingDependency,
    DiskOrMemoryExhaustion,
    GenericFailure,
}

impl ShellFailureCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::MissingCommand => "Command Not Found",
            Self::PortConflict => "Port In Use (EADDRINUSE)",
            Self::PermissionDenied => "Permission Denied (EACCES)",
            Self::CompilationError => "Compilation Failure",
            Self::GitRemoteDivergence => "Git Non-Fast-Forward Divergence",
            Self::MissingDependency => "Missing Package / Module",
            Self::DiskOrMemoryExhaustion => "Resource Exhaustion",
            Self::GenericFailure => "Non-Zero Exit Code",
        }
    }
}

/// Incident captured from the user's terminal
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellIncident {
    pub command: String,
    pub exit_code: i32,
    pub stderr: String,
    pub working_dir: String,
}

/// Actionable diagnostic and synthesized 1-key auto-repair
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellPanicDiagnosis {
    pub category: ShellFailureCategory,
    pub root_cause: String,
    pub confidence_score: u8,
    pub suggested_fix_command: String,
    pub explanation: String,
}

pub struct ShellPanicHook;

impl ShellPanicHook {
    /// Diagnose a shell incident and formulate a 1-key repair command
    pub fn diagnose(incident: &ShellIncident) -> ShellPanicDiagnosis {
        let err_lower = incident.stderr.to_lowercase();
        let cmd = incident.command.trim();

        // 1. Port conflict: EADDRINUSE or "address already in use"
        if err_lower.contains("eaddrinuse") || err_lower.contains("address already in use") {
            let port_re = Regex::new(r"(?::|port\s+)(\d{2,5})").unwrap();
            let port = port_re
                .captures(&incident.stderr)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "3000".to_string());

            return ShellPanicDiagnosis {
                category: ShellFailureCategory::PortConflict,
                root_cause: format!("Port {} is already bound by another background process.", port),
                confidence_score: 95,
                suggested_fix_command: format!("fuser -k {}/tcp || lsof -ti :{} | xargs -r kill -9", port, port),
                explanation: format!("Terminate the stale process holding port {} and rerun '{}'.", port, cmd),
            };
        }

        // 2. Command not found: "command not found" / "not recognized"
        if err_lower.contains("command not found") || err_lower.contains("no such file or directory") {
            let first_word = cmd.split_whitespace().next().unwrap_or(cmd);
            return ShellPanicDiagnosis {
                category: ShellFailureCategory::MissingCommand,
                root_cause: format!("Executable '{}' is not installed or not in PATH.", first_word),
                confidence_score: 90,
                suggested_fix_command: format!("which {} || echo 'Missing tool {}'", first_word, first_word),
                explanation: format!("Verify whether '{}' is installed or add its directory to your system PATH.", first_word),
            };
        }

        // 3. Permission denied: EACCES / "permission denied"
        if err_lower.contains("permission denied") || err_lower.contains("eacces") {
            return ShellPanicDiagnosis {
                category: ShellFailureCategory::PermissionDenied,
                root_cause: "Missing execute or write permissions on target file or directory.".to_string(),
                confidence_score: 88,
                suggested_fix_command: format!("chmod +x {} 2>/dev/null || sudo {}", cmd.split_whitespace().last().unwrap_or(cmd), cmd),
                explanation: "Grant executable permissions or elevate user privileges.".to_string(),
            };
        }

        // 4. Git remote divergence: "[rejected]" / "non-fast-forward" / "fetch first"
        if (cmd.starts_with("git push") || cmd.starts_with("git pull"))
            && (err_lower.contains("non-fast-forward") || err_lower.contains("fetch first") || err_lower.contains("rejected"))
        {
            return ShellPanicDiagnosis {
                category: ShellFailureCategory::GitRemoteDivergence,
                root_cause: "Remote branch contains commits that do not exist locally.".to_string(),
                confidence_score: 96,
                suggested_fix_command: "git pull --rebase origin $(git branch --show-current)".to_string(),
                explanation: "Rebase local commits on top of remote changes to maintain a clean linear history.".to_string(),
            };
        }

        // 5. Rust/Cargo compilation error
        if cmd.starts_with("cargo") && (err_lower.contains("error[e") || err_lower.contains("could not compile")) {
            return ShellPanicDiagnosis {
                category: ShellFailureCategory::CompilationError,
                root_cause: "Compiler syntax or type resolution error in crate.".to_string(),
                confidence_score: 92,
                suggested_fix_command: "hgb preflight src/lib.rs || cargo check --message-format=short".to_string(),
                explanation: "Run Hagibis preflight shadow check to automatically repair bracket or syntax errors.".to_string(),
            };
        }

        // 6. Missing Node/Python dependency
        if err_lower.contains("cannot find module") || err_lower.contains("modulenotfounderror") {
            let mod_re = Regex::new(r#"['"]([a-zA-Z0-9_\-]+)['"]"#).unwrap();
            let missing_pkg = mod_re
                .captures(&incident.stderr)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "dependency".to_string());

            let fix = if cmd.starts_with("python") || cmd.starts_with("pytest") {
                format!("pip install {}", missing_pkg)
            } else {
                format!("npm install --save {}", missing_pkg)
            };

            return ShellPanicDiagnosis {
                category: ShellFailureCategory::MissingDependency,
                root_cause: format!("Required dependency package '{}' is missing.", missing_pkg),
                confidence_score: 91,
                suggested_fix_command: fix,
                explanation: format!("Install package '{}' and re-run.", missing_pkg),
            };
        }

        // Fallback: Generic non-zero exit
        ShellPanicDiagnosis {
            category: ShellFailureCategory::GenericFailure,
            root_cause: format!("Command exited with status code {}.", incident.exit_code),
            confidence_score: 50,
            suggested_fix_command: format!("hgb squeeze --file <(echo '{}')", incident.stderr.replace('\'', "\\'")),
            explanation: "Examine squeezed stderr diagnostics for root cause analysis.".to_string(),
        }
    }

    /// Generate portable companion hook script for Bash, Zsh, and Fish
    pub fn generate_shell_hook(shell: &str) -> String {
        match shell {
            "zsh" => r#"
# Hagibis Warp-Style Shell Panic Hook (Zsh)
hgb_precmd() {
    local exit_code=$?
    if [ $exit_code -ne 0 ] && [ -n "$HGB_LAST_CMD" ]; then
        hgb shell-panic --code $exit_code --cmd "$HGB_LAST_CMD" 2>/dev/null
    fi
}
hgb_preexec() {
    export HGB_LAST_CMD="$1"
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd hgb_precmd
add-zsh-hook preexec hgb_preexec
"#.trim().to_string(),

            "fish" => r#"
# Hagibis Warp-Style Shell Panic Hook (Fish)
function __hgb_on_exit --on-event fish_postexec
    set -l exit_code $status
    if test $exit_code -ne 0
        hgb shell-panic --code $exit_code --cmd "$argv" 2>/dev/null
    end
end
"#.trim().to_string(),

            _ => r#"
# Hagibis Warp-Style Shell Panic Hook (Bash)
hgb_trap_exit() {
    local exit_code=$?
    if [ $exit_code -ne 0 ] && [ -n "$BASH_COMMAND" ]; then
        hgb shell-panic --code $exit_code --cmd "$BASH_COMMAND" 2>/dev/null
    fi
}
trap 'hgb_trap_exit' ERR
"#.trim().to_string(),
        }
    }
}
