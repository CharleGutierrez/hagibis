use crate::error::{HgbError, Result};
use blake3::Hasher;

/// Lightweight Zero-Ambient-Authority Security Gate
pub struct AgentShieldLight;

impl AgentShieldLight {
    /// Inspect command or script for forbidden dangerous patterns
    pub fn audit_command(cmd: &str) -> Result<()> {
        let dangerous_patterns = [
            "rm -rf /",
            ":(){ :|:& };:",
            "mkfs.",
            "dd if=/dev/zero of=/dev/sd",
            "chmod -R 777 /",
        ];

        for pattern in &dangerous_patterns {
            if cmd.contains(pattern) {
                return Err(HgbError::Security(format!(
                    "Command blocked by AgentShieldLight: matched dangerous pattern '{}'",
                    pattern
                )));
            }
        }
        Ok(())
    }

    /// Audit target file path against sensitive credentials and system files
    pub fn audit_path(path: &str) -> Result<()> {
        let p = path.replace('\\', "/");
        let sensitive = [
            "/etc/shadow",
            "/etc/passwd",
            "/etc/sudoers",
            ".ssh",
            "id_rsa",
            "id_ed25519",
            ".env",
        ];

        for s in &sensitive {
            if p == *s || p.ends_with(&format!("/{}", s)) || p.contains(&format!("{}/", s)) || p.contains(s) {
                return Err(HgbError::Security(format!(
                    "Access to sensitive or prohibited credentials path '{}' blocked by AgentShieldLight",
                    path
                )));
            }
        }
        Ok(())
    }

    /// Audit payload for prompt injections
    pub fn audit_payload(content: &str) -> Result<()> {
        let lower = content.to_lowercase();
        if lower.contains("<system_override>")
            || lower.contains("ignore all previous instructions")
            || lower.contains("ignore previous instructions")
            || lower.contains("ignore all instructions")
            || lower.contains("bypass all security filters")
            || lower.contains("new system directive:")
        {
            return Err(HgbError::Security(
                "Prompt injection attempt detected and blocked by AgentShieldLight".to_string(),
            ));
        }
        Ok(())
    }

    /// Scan dynamic tool arguments against path and payload security rules
    pub fn scan_tool_call(_tool: &str, args: &serde_json::Value) -> Result<()> {
        let path_keys = ["path", "AbsolutePath", "TargetFile", "search_directory", "search_path"];
        for key in &path_keys {
            if let Some(val) = args.get(*key).and_then(|v| v.as_str()) {
                Self::audit_path(val)?;
            }
        }

        let content_keys = ["content", "CodeContent", "ReplacementContent", "target_content"];
        for key in &content_keys {
            if let Some(val) = args.get(*key).and_then(|v| v.as_str()) {
                Self::audit_payload(val)?;
            }
        }

        Ok(())
    }

    /// Compute cryptographic fingerprint of payload
    pub fn digest(data: &[u8]) -> String {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finalize().to_hex().to_string()
    }
}
