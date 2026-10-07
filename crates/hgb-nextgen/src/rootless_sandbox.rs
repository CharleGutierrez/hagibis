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

        // 3. Configure sanitized environment
        let mut envs = HashMap::new();
        envs.insert("TMPDIR".to_string(), jail_dir.to_string_lossy().to_string());
        envs.insert("HOME".to_string(), jail_dir.to_string_lossy().to_string());
        envs.insert("HGB_SANDBOXED".to_string(), "1".to_string());
        envs.insert("HGB_JAIL_DIR".to_string(), jail_dir.to_string_lossy().to_string());

        let cmd_opts = CommandOptions {
            cwd: Some(workspace.to_path_buf()),
            timeout_ms: Some(config.timeout_ms),
            env: envs,
            max_output_bytes: Some(64 * 1024),
            wait_ms_before_async: None,
        };

        // 4. Run through AgyCrud (child pre_exec hook applies hardware Landlock LSM jail)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_rootless_sandbox_landlock_jail_confinement() {
        let temp_dir = std::env::temp_dir();
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let ws_dir = temp_dir.join(format!("hgb_test_ws_{}", test_id));
        let outside_dir = temp_dir.join(format!("hgb_test_outside_{}", test_id));
        fs::create_dir_all(&ws_dir).expect("create ws");
        fs::create_dir_all(&outside_dir).expect("create outside dir");

        let allowed_file = ws_dir.join("allowed.txt");
        let forbidden_file = outside_dir.join("secret_data.txt");
        fs::write(&allowed_file, "ALLOWED_PAYLOAD").expect("write allowed");
        fs::write(&forbidden_file, "SECRET_FORBIDDEN_PAYLOAD").expect("write forbidden");

        let config = SandboxConfig {
            allowed_workspace: ws_dir.clone(),
            ..Default::default()
        };

        // 1. Reading allowed file in workspace must SUCCEED
        let cmd_allowed = format!("cat {}", allowed_file.display());
        let res_allowed = RootlessSandboxEngine::execute_sandboxed(&cmd_allowed, &ws_dir, &config)
            .await
            .expect("exec allowed");
        assert_eq!(res_allowed.exit_code, 0, "Reading allowed file must succeed");
        assert!(res_allowed.stdout.contains("ALLOWED_PAYLOAD"));

        // 2. Reading forbidden file outside workspace must be BLOCKED by Landlock LSM
        let cmd_forbidden = format!("cat {}", forbidden_file.display());
        let res_forbidden = RootlessSandboxEngine::execute_sandboxed(&cmd_forbidden, &ws_dir, &config)
            .await
            .expect("exec forbidden");

        if hgb_core::zig_accelerate::sandbox_check_support() {
            assert_ne!(res_forbidden.exit_code, 0, "Reading outside forbidden file must fail under Landlock");
            assert!(
                res_forbidden.stderr.contains("Permission denied"),
                "Stderr should report 'Permission denied' from kernel Landlock LSM, got: {}",
                res_forbidden.stderr
            );
        }

        // 3. Parent process remains UNCONFINED and can freely read both files
        let parent_read = fs::read_to_string(&forbidden_file).expect("parent read forbidden");
        assert_eq!(parent_read, "SECRET_FORBIDDEN_PAYLOAD");

        // Cleanup
        let _ = fs::remove_dir_all(&ws_dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }
}
