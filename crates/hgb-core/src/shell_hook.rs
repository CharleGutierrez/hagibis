//! # Shell Companion & Crash Interceptor
//!
//! Generates shell hooks for bash, zsh, fish, logs shell crash states into `.hgb/crashes/`,
//! and provides systems-grade root-cause diagnosis, fixes, and confidence ratings.

use std::collections::HashMap;
use std::path::PathBuf;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::error::{HgbError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupportedShell {
    Bash,
    Zsh,
    Fish,
}

impl SupportedShell {
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }
}

pub struct ShellHookGenerator;

impl ShellHookGenerator {
    pub fn generate(shell: SupportedShell, binary_name: &str) -> String {
        match shell {
            SupportedShell::Bash => format!(
                r#"# Hagibis ({bin}) Shell Integration for Bash
__hgb_preexec() {{
    __HGB_LAST_CMD="$BASH_COMMAND"
}}
trap '__hgb_preexec' DEBUG

__hgb_prompt_hook() {{
    local EXIT_CODE=$?
    if [ $EXIT_CODE -ne 0 ] && [ -n "$__HGB_LAST_CMD" ]; then
        if command -v {bin} >/dev/null 2>&1; then
            {bin} crash-record --cmd "$__HGB_LAST_CMD" --code "$EXIT_CODE" --pwd "$PWD" >/dev/null 2>&1 &
        fi
    fi
}}
PROMPT_COMMAND="__hgb_prompt_hook; $PROMPT_COMMAND"

hgb_fix() {{
    {bin} fix "$@"
}}
"#,
                bin = binary_name
            ),
            SupportedShell::Zsh => format!(
                r#"# Hagibis ({bin}) Shell Integration for Zsh
autoload -Uz add-zsh-hook

__hgb_zsh_preexec() {{
    __HGB_LAST_CMD="$1"
}}
add-zsh-hook preexec __hgb_zsh_preexec

__hgb_zsh_precmd() {{
    local EXIT_CODE=$?
    if [[ $EXIT_CODE -ne 0 && -n "$__HGB_LAST_CMD" ]]; then
        if command -v {bin} >/dev/null 2>&1; then
            {bin} crash-record --cmd "$__HGB_LAST_CMD" --code "$EXIT_CODE" --pwd "$PWD" >/dev/null 2>&1 &
        fi
    fi
}}
add-zsh-hook precmd __hgb_zsh_precmd

hgb_fix() {{
    {bin} fix "$@"
}}
"#,
                bin = binary_name
            ),
            SupportedShell::Fish => format!(
                r#"# Hagibis ({bin}) Shell Integration for Fish
function __hgb_postexec --on-event fish_postexec
    set -l EXIT_CODE $status
    if test $EXIT_CODE -ne 0
        if type -q {bin}
            {bin} crash-record --cmd "$argv" --code "$EXIT_CODE" --pwd "$PWD" >/dev/null 2>&1 &
        end
    end
end

function hgb_fix
    {bin} fix $argv
end
"#,
                bin = binary_name
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrashRecord {
    pub crash_id: String,
    pub command: String,
    pub exit_code: i32,
    pub cwd: PathBuf,
    pub stderr_snippet: String,
    pub stdout_snippet: String,
    pub timestamp: String,
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashCategory {
    MissingDependency,
    SyntaxError,
    PermissionDenied,
    PortConflict,
    CommandNotFound,
    TestFailure,
    BuildError,
    GitIssue,
    OutOfMemory,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrashDiagnosis {
    pub crash_id: String,
    pub category: CrashCategory,
    pub root_cause: String,
    pub suggested_command_fix: String,
    pub alternative_fixes: Vec<String>,
    pub confidence: f32,
    pub explanation: String,
}

pub struct CrashInterceptor {
    crashes_dir: PathBuf,
}

impl CrashInterceptor {
    pub fn new<P: Into<PathBuf>>(workspace_root: P) -> Self {
        let root = workspace_root.into();
        Self {
            crashes_dir: root.join(".hgb").join("crashes"),
        }
    }

    pub fn record_crash(&self, mut record: CrashRecord) -> Result<String> {
        if !self.crashes_dir.exists() {
            std::fs::create_dir_all(&self.crashes_dir)?;
        }

        if record.crash_id.is_empty() {
            record.crash_id = blake3::hash(format!("{}:{}:{}", record.command, record.timestamp, record.exit_code).as_bytes())
                .to_hex()
                .to_string();
        }

        let content = serde_json::to_string_pretty(&record)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let id_file = self.crashes_dir.join(format!("{}.json", record.crash_id));
        std::fs::write(&id_file, &content)?;

        let latest_file = self.crashes_dir.join("latest.json");
        std::fs::write(&latest_file, &content)?;

        Ok(record.crash_id)
    }

    pub fn load_latest_crash(&self) -> Result<Option<CrashRecord>> {
        let latest = self.crashes_dir.join("latest.json");
        if !latest.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&latest)?;
        let record: CrashRecord = serde_json::from_str(&data)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        Ok(Some(record))
    }

    pub fn diagnose_and_fix(&self, record: &CrashRecord) -> CrashDiagnosis {
        let output = format!("{}\n{}", record.stdout_snippet, record.stderr_snippet);

        // 1. Command not found (exit code 127)
        if record.exit_code == 127 || output.contains("command not found") || output.contains("not found:") {
            let cmd_bin = record.command.split_whitespace().next().unwrap_or("tool");
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::CommandNotFound,
                root_cause: format!("The executable '{}' is not installed or not in PATH.", cmd_bin),
                suggested_command_fix: format!("sudo apt-get install -y {} || cargo install {} || npm install -g {}", cmd_bin, cmd_bin, cmd_bin),
                alternative_fixes: vec!["export PATH=\"$PATH:/usr/local/bin:~/.cargo/bin:~/.npm/bin\"".into()],
                confidence: 0.95,
                explanation: format!("Command exited with status code 127 indicating binary '{}' was not resolved.", cmd_bin),
            };
        }

        // 2. Permission denied (exit code 126)
        if record.exit_code == 126 || output.contains("Permission denied") {
            let cmd_bin = record.command.split_whitespace().next().unwrap_or(&record.command);
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::PermissionDenied,
                root_cause: format!("Insufficient execution permissions on '{}'.", cmd_bin),
                suggested_command_fix: format!("chmod +x {}", cmd_bin),
                alternative_fixes: vec![format!("sudo {}", record.command)],
                confidence: 0.92,
                explanation: "The OS denied execution access. Making the target executable or running with elevated permissions resolves this.".into(),
            };
        }

        // 3. Port conflict / EADDRINUSE
        if output.contains("EADDRINUSE") || output.contains("Address already in use") || output.contains("port is already allocated") {
            let re = Regex::new(r"(?:port\s+|:)(\d{2,5})").unwrap();
            let port = re.captures(&output).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("8080");
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::PortConflict,
                root_cause: format!("TCP Port :{} is already bound by another process.", port),
                suggested_command_fix: format!("lsof -ti:{} | xargs kill -9", port),
                alternative_fixes: vec![format!("fuser -k {}/tcp", port)],
                confidence: 0.96,
                explanation: format!("The requested network port {} has an active socket. Killing the zombie process frees the port immediately.", port),
            };
        }

        // 4. Rust compilation / missing crate
        if output.contains("error[E0432]") || output.contains("error[E0433]") || output.contains("unresolved import") {
            let re = Regex::new(r"unresolved import `([^`]+)`").unwrap();
            let missing = re.captures(&output).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("crate_name");
            let base_pkg = missing.split("::").next().unwrap_or(missing);
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::MissingDependency,
                root_cause: format!("Unresolved Rust crate or module dependency '{}'.", missing),
                suggested_command_fix: format!("cargo add {}", base_pkg),
                alternative_fixes: vec!["cargo update".into(), "cargo check".into()],
                confidence: 0.94,
                explanation: format!("The rustc compiler cannot find module '{}'. Adding it to Cargo.toml satisfies the dependency.", missing),
            };
        }

        // 5. Python ModuleNotFoundError
        if output.contains("ModuleNotFoundError: No module named") || output.contains("ImportError: No module named") {
            let re = Regex::new(r"No module named '([^']+)'").unwrap();
            let module = re.captures(&output).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("package");
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::MissingDependency,
                root_cause: format!("Python package '{}' is missing in virtual environment.", module),
                suggested_command_fix: format!("pip install {}", module),
                alternative_fixes: vec![format!("uv pip install {}", module), "poetry add {}".into()],
                confidence: 0.95,
                explanation: format!("Python failed importing '{}'. Installing via pip restores the environment.", module),
            };
        }

        // 6. Node / npm missing module
        if output.contains("Cannot find module") {
            let re = Regex::new(r"Cannot find module '([^']+)'").unwrap();
            let module = re.captures(&output).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("pkg");
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::MissingDependency,
                root_cause: format!("Node.js module '{}' is not present in node_modules.", module),
                suggested_command_fix: format!("npm install {}", module),
                alternative_fixes: vec![format!("pnpm add {}", module), format!("yarn add {}", module)],
                confidence: 0.95,
                explanation: format!("Node module '{}' was imported but not found in node_modules.", module),
            };
        }

        // 7. Cargo test failure
        if output.contains("FAILED") && (output.contains("test result:") || output.contains("panicked at")) {
            let re = Regex::new(r"test ([a-zA-Z0-9_:]+) \.\.\. FAILED").unwrap();
            let failing_test = re.captures(&output).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("");
            let cmd_fix = if !failing_test.is_empty() {
                format!("RUST_BACKTRACE=1 cargo test {} -- --nocapture", failing_test)
            } else {
                "RUST_BACKTRACE=1 cargo test -- --nocapture".to_string()
            };
            return CrashDiagnosis {
                crash_id: record.crash_id.clone(),
                category: CrashCategory::TestFailure,
                root_cause: format!("Assertion or panic failure in test '{}'.", failing_test),
                suggested_command_fix: cmd_fix,
                alternative_fixes: vec!["cargo check --tests".into()],
                confidence: 0.91,
                explanation: "Test runner encountered assertion failure. Re-running with backtrace reveals the exact failing line.".into(),
            };
        }

        // Generic fallback
        CrashDiagnosis {
            crash_id: record.crash_id.clone(),
            category: CrashCategory::Unknown,
            root_cause: format!("Process exited with status code {}.", record.exit_code),
            suggested_command_fix: record.command.clone(),
            alternative_fixes: vec![],
            confidence: 0.60,
            explanation: "Review stdout/stderr log snippets to identify environment or parameter mismatches.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_script_generation() {
        let bash_script = ShellHookGenerator::generate(SupportedShell::Bash, "hgb");
        assert!(bash_script.contains("__hgb_preexec"));
        assert!(bash_script.contains("hgb crash-record"));

        let zsh_script = ShellHookGenerator::generate(SupportedShell::Zsh, "hgb");
        assert!(zsh_script.contains("__hgb_zsh_precmd"));

        let fish_script = ShellHookGenerator::generate(SupportedShell::Fish, "hgb");
        assert!(fish_script.contains("fish_postexec"));
    }

    #[test]
    fn test_crash_record_and_diagnose() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_shell_hook_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let interceptor = CrashInterceptor::new(&temp_dir);

        let record = CrashRecord {
            crash_id: String::new(),
            command: "python app.py".into(),
            exit_code: 1,
            cwd: temp_dir.clone(),
            stderr_snippet: "ModuleNotFoundError: No module named 'fastapi'".into(),
            stdout_snippet: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            environment: HashMap::new(),
        };

        let id = interceptor.record_crash(record.clone()).unwrap();
        assert!(!id.is_empty());

        let loaded = interceptor.load_latest_crash().unwrap().expect("Loaded latest crash");
        assert_eq!(loaded.command, "python app.py");

        let diagnosis = interceptor.diagnose_and_fix(&loaded);
        assert_eq!(diagnosis.category, CrashCategory::MissingDependency);
        assert_eq!(diagnosis.suggested_command_fix, "pip install fastapi");
        assert!(diagnosis.confidence >= 0.9);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
