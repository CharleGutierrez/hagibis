//! # Verification Gate & Golden Invariant Guard
//!
//! Systems-grade autonomous verification pipeline enforcing golden invariants,
//! synthesizing smoke-tests, computing blake3 integrity certificates, and executing
//! a multi-iteration self-healing loop for vibe-coding operations.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use serde::{Deserialize, Serialize};
use crate::error::{HgbError, Result};

/// Overall status outcome of a verification run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    Passed,
    Failed,
    Healed,
    Rejected,
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passed => write!(f, "PASSED"),
            Self::Failed => write!(f, "FAILED"),
            Self::Healed => write!(f, "HEALED"),
            Self::Rejected => write!(f, "REJECTED"),
        }
    }
}

/// Category of verification check
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationCheckKind {
    BaselineSyntax,
    StaticLint,
    UnitTests,
    SmokeTest,
    GoldenInvariant,
    Custom,
}

/// An invariant definition that must hold true for code changes to be accepted
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoldenInvariant {
    pub name: String,
    pub description: String,
    pub check_command: String,
    pub expected_exit_code: i32,
    pub critical: bool,
    pub kind: VerificationCheckKind,
}

impl GoldenInvariant {
    pub fn new(name: impl Into<String>, check_command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            check_command: check_command.into(),
            expected_exit_code: 0,
            critical: true,
            kind: VerificationCheckKind::GoldenInvariant,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn with_kind(mut self, kind: VerificationCheckKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn non_critical(mut self) -> Self {
        self.critical = false;
        self
    }
}

/// Result of a single verification step
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationStepResult {
    pub name: String,
    pub kind: VerificationCheckKind,
    pub passed: bool,
    pub critical: bool,
    pub command: String,
    pub exit_code: i32,
    pub output: String,
    pub duration_ms: u64,
    pub error_message: Option<String>,
}

/// Tamper-evident verification certificate signed by Blake3 hash
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationCertificate {
    pub certificate_id: String,
    pub workspace_root: PathBuf,
    pub status: VerificationStatus,
    pub baseline_hash: String,
    pub integrity_hash: String,
    pub timestamp: String,
    pub iterations_run: usize,
    pub steps: Vec<VerificationStepResult>,
    pub healed_patches: Vec<String>,
    pub summary: String,
}

impl VerificationCertificate {
    pub fn compute_integrity_hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.certificate_id.as_bytes());
        hasher.update(format!("{}", self.status).as_bytes());
        hasher.update(self.workspace_root.to_string_lossy().as_bytes());
        hasher.update(self.baseline_hash.as_bytes());
        hasher.update(self.timestamp.as_bytes());
        hasher.update(&self.iterations_run.to_le_bytes());

        for step in &self.steps {
            hasher.update(step.name.as_bytes());
            hasher.update(&[step.passed as u8, step.critical as u8]);
            hasher.update(&step.exit_code.to_le_bytes());
            hasher.update(step.command.as_bytes());
        }

        for patch in &self.healed_patches {
            hasher.update(patch.as_bytes());
        }

        hasher.finalize().to_hex().to_string()
    }

    pub fn verify_integrity(&self) -> bool {
        self.integrity_hash == self.compute_integrity_hash()
    }

    pub fn save_to_dir<P: AsRef<Path>>(&self, dir: P) -> Result<PathBuf> {
        let dir = dir.as_ref();
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
        }
        let cert_file = dir.join(format!("cert_{}.json", self.certificate_id));
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        std::fs::write(&cert_file, content)?;
        Ok(cert_file)
    }
}

pub struct VerificationGate {
    workspace_root: PathBuf,
    invariants: Vec<GoldenInvariant>,
    max_heal_iterations: usize,
}

impl VerificationGate {
    pub fn new<P: Into<PathBuf>>(workspace_root: P) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            invariants: Vec::new(),
            max_heal_iterations: 3,
        }
    }

    pub fn with_invariant(mut self, invariant: GoldenInvariant) -> Self {
        self.invariants.push(invariant);
        self
    }

    pub fn with_invariants(mut self, invariants: Vec<GoldenInvariant>) -> Self {
        self.invariants.extend(invariants);
        self
    }

    pub fn with_max_heal_iterations(mut self, max: usize) -> Self {
        self.max_heal_iterations = max;
        self
    }

    pub fn auto_detect_invariants(&mut self) {
        if self.workspace_root.join("Cargo.toml").exists() {
            self.invariants.push(
                GoldenInvariant::new("cargo_check", "cargo check --workspace")
                    .with_description("Ensures clean compilation across Cargo workspace")
                    .with_kind(VerificationCheckKind::BaselineSyntax),
            );
            self.invariants.push(
                GoldenInvariant::new("cargo_test", "cargo test --workspace -- --nocapture")
                    .with_description("Executes all test suites across the workspace")
                    .with_kind(VerificationCheckKind::UnitTests),
            );
        }

        if self.workspace_root.join("package.json").exists() {
            if let Ok(pkg_str) = std::fs::read_to_string(self.workspace_root.join("package.json")) {
                if pkg_str.contains("\"test\"") {
                    self.invariants.push(
                        GoldenInvariant::new("npm_test", "npm test")
                            .with_description("Runs npm test suite")
                            .with_kind(VerificationCheckKind::UnitTests),
                    );
                }
            }
        }

        if self.workspace_root.join("pyproject.toml").exists()
            || self.workspace_root.join("pytest.ini").exists()
            || self.workspace_root.join("setup.py").exists()
        {
            self.invariants.push(
                GoldenInvariant::new("pytest", "pytest")
                    .with_description("Executes pytest test suites")
                    .with_kind(VerificationCheckKind::UnitTests),
            );
        }

        if self.workspace_root.join("go.mod").exists() {
            self.invariants.push(
                GoldenInvariant::new("go_test", "go test ./...")
                    .with_description("Executes all Go package tests")
                    .with_kind(VerificationCheckKind::UnitTests),
            );
        }
    }

    pub fn synthesize_smoke_test(&self) -> Option<GoldenInvariant> {
        if self.workspace_root.join("Cargo.toml").exists() {
            Some(
                GoldenInvariant::new("smoke_cargo_check", "cargo check")
                    .with_description("Synthesized smoke test: cargo fast check")
                    .with_kind(VerificationCheckKind::SmokeTest),
            )
        } else if self.workspace_root.join("package.json").exists() {
            Some(
                GoldenInvariant::new("smoke_node_syntax", "node -c index.js")
                    .with_description("Synthesized smoke test: node syntax check")
                    .with_kind(VerificationCheckKind::SmokeTest)
                    .non_critical(),
            )
        } else {
            None
        }
    }

    pub fn calculate_workspace_fingerprint(&self) -> Result<String> {
        let mut hasher = blake3::Hasher::new();
        let mut files = Vec::new();
        Self::collect_source_files(&self.workspace_root, &self.workspace_root, &mut files)?;
        files.sort();

        for rel in files {
            let full = self.workspace_root.join(&rel);
            if let Ok(bytes) = std::fs::read(&full) {
                hasher.update(rel.as_bytes());
                hasher.update(&bytes);
            }
        }

        Ok(hasher.finalize().to_hex().to_string())
    }

    fn collect_source_files(root: &Path, current: &Path, list: &mut Vec<String>) -> Result<()> {
        if !current.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if name == ".git"
                || name == ".hgb"
                || name == "target"
                || name == "node_modules"
                || name == "__pycache__"
            {
                continue;
            }

            if path.is_dir() {
                Self::collect_source_files(root, &path, list)?;
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if ["rs", "ts", "js", "py", "go", "json", "toml", "yaml", "yml", "c", "cpp", "h"].contains(&ext) {
                        if let Ok(rel) = path.strip_prefix(root) {
                            list.push(rel.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn execute_step(&self, inv: &GoldenInvariant) -> VerificationStepResult {
        let start = Instant::now();

        let output = Command::new("sh")
            .arg("-c")
            .arg(&inv.check_command)
            .current_dir(&self.workspace_root)
            .output();

        let duration_ms = start.elapsed().as_millis() as u64;

        match output {
            Ok(out) => {
                let code = out.status.code().unwrap_or(-1);
                let passed = code == inv.expected_exit_code;
                let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }

                let error_message = if !passed {
                    Some(format!("Command exited with status code {}", code))
                } else {
                    None
                };

                VerificationStepResult {
                    name: inv.name.clone(),
                    kind: inv.kind,
                    passed,
                    critical: inv.critical,
                    command: inv.check_command.clone(),
                    exit_code: code,
                    output: combined,
                    duration_ms,
                    error_message,
                }
            }
            Err(e) => VerificationStepResult {
                name: inv.name.clone(),
                kind: inv.kind,
                passed: false,
                critical: inv.critical,
                command: inv.check_command.clone(),
                exit_code: -1,
                output: String::new(),
                duration_ms,
                error_message: Some(format!("Failed to spawn command: {}", e)),
            },
        }
    }

    pub fn verify_and_heal<H>(&self, mut healer: Option<H>) -> Result<VerificationCertificate>
    where
        H: FnMut(&[VerificationStepResult], usize) -> Result<Option<String>>,
    {
        let baseline_hash = self.calculate_workspace_fingerprint().unwrap_or_else(|_| "unknown".into());
        let now = chrono::Utc::now().to_rfc3339();

        let mut check_list = self.invariants.clone();
        if check_list.is_empty() {
            if let Some(smoke) = self.synthesize_smoke_test() {
                check_list.push(smoke);
            }
        }

        let mut steps = Vec::new();
        for inv in &check_list {
            let res = self.execute_step(inv);
            steps.push(res);
        }

        let mut healed_patches = Vec::new();
        let mut iterations_run = 0;

        let has_critical_failures = |st: &[VerificationStepResult]| -> bool {
            st.iter().any(|s| !s.passed && s.critical)
        };

        if has_critical_failures(&steps) {
            if let Some(ref mut heal_fn) = healer {
                for iter in 1..=self.max_heal_iterations {
                    iterations_run = iter;
                    let failures: Vec<VerificationStepResult> = steps
                        .iter()
                        .filter(|s| !s.passed)
                        .cloned()
                        .collect();

                    if failures.is_empty() {
                        break;
                    }

                    match heal_fn(&failures, iter) {
                        Ok(Some(patch_desc)) => {
                            healed_patches.push(patch_desc);
                            let mut re_eval = Vec::new();
                            for inv in &check_list {
                                re_eval.push(self.execute_step(inv));
                            }
                            steps = re_eval;

                            if !has_critical_failures(&steps) {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }
        }

        let final_status = if !has_critical_failures(&steps) {
            if !healed_patches.is_empty() {
                VerificationStatus::Healed
            } else {
                VerificationStatus::Passed
            }
        } else {
            VerificationStatus::Failed
        };

        let cert_id = blake3::hash(format!("{}:{}:{}", baseline_hash, now, final_status).as_bytes())
            .to_hex()
            .to_string();

        let summary = format!(
            "Verification status: {} ({} total steps, {} passed, {} failed, {} healing iterations)",
            final_status,
            steps.len(),
            steps.iter().filter(|s| s.passed).count(),
            steps.iter().filter(|s| !s.passed).count(),
            iterations_run
        );

        let mut cert = VerificationCertificate {
            certificate_id: cert_id,
            workspace_root: self.workspace_root.clone(),
            status: final_status,
            baseline_hash,
            integrity_hash: String::new(),
            timestamp: now,
            iterations_run,
            steps,
            healed_patches,
            summary,
        };

        cert.integrity_hash = cert.compute_integrity_hash();

        let certs_dir = self.workspace_root.join(".hgb").join("certificates");
        let _ = cert.save_to_dir(&certs_dir);

        Ok(cert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verification_gate_success() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_vg_test_succ_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut gate = VerificationGate::new(&temp_dir);
        gate = gate.with_invariant(GoldenInvariant::new("echo_test", "echo 'hello world'"));

        let cert = gate.verify_and_heal::<fn(&[VerificationStepResult], usize) -> Result<Option<String>>>(None).unwrap();
        assert_eq!(cert.status, VerificationStatus::Passed);
        assert_eq!(cert.steps.len(), 1);
        assert!(cert.steps[0].passed);
        assert!(cert.verify_integrity());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_verification_gate_healing() {
        let temp_dir = std::env::temp_dir().join(format!("hgb_vg_test_heal_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let flag_file = temp_dir.join("healed.flag");
        let flag_path_str = flag_file.to_string_lossy().to_string();

        let mut gate = VerificationGate::new(&temp_dir);
        let check_cmd = format!("test -f {}", flag_path_str);
        gate = gate.with_invariant(GoldenInvariant::new("flag_check", check_cmd));

        let flag_clone = flag_file.clone();
        let cert = gate.verify_and_heal(Some(move |_failures: &[VerificationStepResult], iter: usize| {
            if iter == 1 {
                std::fs::write(&flag_clone, "ok").unwrap();
                Ok(Some("Created healed.flag".to_string()))
            } else {
                Ok(None)
            }
        })).unwrap();

        assert_eq!(cert.status, VerificationStatus::Healed);
        assert_eq!(cert.healed_patches.len(), 1);
        assert!(cert.verify_integrity());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
