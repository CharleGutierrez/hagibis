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

    /// Compute cryptographic fingerprint of payload
    pub fn digest(data: &[u8]) -> String {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finalize().to_hex().to_string()
    }
}
