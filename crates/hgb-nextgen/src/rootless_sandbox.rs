use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use hgb_core::crud::{AgyCrud, CommandOptions};
use hgb_core::error::Result;
use hgb_core::security::AgentShieldLight;

/// Configuration options for the ephemeral rootless sandbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub isolated_tmp: bool,
    pub block_sensitive_dirs: Vec<String>,
    pub network_isolated: bool,
    pub timeout_ms: u64,
    pub allowed_workspace: PathBuf,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            isolated_tmp: true,
            block_sensitive_dirs: vec![
                ".ssh".to_string(),
                ".aws".to_string(),
                ".gnupg".to_string(),
                "/etc/shadow".to_string(),
                "/etc/sudoers".to_string(),
                ".bash_history".to_string(),
            ],
            network_isolated: false,
            timeout_ms: 10000,
            allowed_workspace: PathBuf::from("."),
        }
    }
}

/// Detailed audit report of sandboxed execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxExecutionReport {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub security_passed: bool,
    pub blocked_violations: Vec<String>,
    pub duration_ms: u64,
    pub ephemeral_jail_dir: Option<PathBuf>,
}

/// Rootless Ephemeral Sandbox Engine
pub struct RootlessSandboxEngine;

impl RootlessSandboxEngine {
    /// Execute a command in a sanitized rootless jail with secret isolation
    pub async fn execute_sandboxed(
        cmd: &str,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> Result<SandboxExecutionReport> {
        let t0 = Instant::now();

        // 1. Static Catastrophic and Secret Traversal Check
        let mut violations = Vec::new();

        // Use AgentShieldLight for catastrophic command validation
        if let Err(e) = AgentShieldLight::audit_command(cmd) {
            violations.push(format!("AgentShield: {}", e));
        }

        // Check for sensitive directory inspection attempts
        for sensitive in &config.block_sensitive_dirs {
            if cmd.contains(sensitive) {
                violations.push(format!("Access to sensitive target '{}' is prohibited in rootless sandbox", sensitive));
            }
        }

        if !violations.is_empty() {
            return Ok(SandboxExecutionReport {
                exit_code: 126,
                stdout: String::new(),
                stderr: format!("Sandbox policy violation:\n{}", violations.join("\n")),
                timed_out: false,
                security_passed: false,
                blocked_violations: violations,
                duration_ms: t0.elapsed().as_millis() as u64,
                ephemeral_jail_dir: None,
            });
        }

        // 2. Setup ephemeral isolated tmp/jail
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let jail_dir = std::env::temp_dir().join(format!("hgb_jail_{}_{}", std::process::id(), nanos));
        let _ = fs::create_dir_all(&jail_dir);

        if hgb_core::zig_accelerate::sandbox_check_support() {
            let dir_str = workspace.to_string_lossy().to_string();
            let _ = hgb_core::zig_accelerate::sandbox_apply_landlock(&dir_str);
        }

        // 3. Configure sanitized environment
        let mut envs = HashMap::new();
        envs.insert("TMPDIR".to_string(), jail_dir.to_string_lossy().to_string());
        envs.insert("HOME".to_string(), jail_dir.to_string_lossy().to_string());
        envs.insert("HGB_SANDBOXED".to_string(), "1".to_string());

        let cmd_opts = CommandOptions {
            cwd: Some(workspace.to_path_buf()),
            timeout_ms: Some(config.timeout_ms),
            env: envs,
            max_output_bytes: Some(64 * 1024),
            wait_ms_before_async: None,
        };

        // 4. Run through AgyCrud
        let res = AgyCrud::run_command(cmd, Some(workspace), cmd_opts).await?;
        let elapsed = t0.elapsed().as_millis() as u64;

        // Cleanup jail dir safely
        let _ = fs::remove_dir_all(&jail_dir);

        Ok(SandboxExecutionReport {
            exit_code: res.exit_code,
            stdout: res.stdout,
            stderr: res.stderr,
            timed_out: res.timed_out,
            security_passed: true,
            blocked_violations: Vec::new(),
            duration_ms: elapsed,
            ephemeral_jail_dir: Some(jail_dir),
        })
    }
}
