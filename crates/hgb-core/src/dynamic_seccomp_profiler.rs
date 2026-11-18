// ============================================================================
// HAGIBIS CORE: DYNAMIC SECCOMP PROFILER FOR FIRECRACKER MICROVM
// ============================================================================
//! Dynamic per-task Seccomp BPF profile generator for Firecracker microVMs.
//! Rather than relying on a static broad profile, this engine analyzes the exact
//! syscall requirements of the running agent task (Compute, Wasm, Edge Worker, etc.)
//! and dynamically synthesizes a strict default-deny Seccomp filter while
//! unconditionally scrubbing all lethal/dangerous host syscall attack vectors.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// Unconditionally blocked lethal and dangerous syscalls that must never be permitted
/// under any task profile, preventing microVM and host kernel escapes.
pub const LETHAL_DANGEROUS_SYSCALLS: &[(&str, &str)] = &[
    ("ptrace", "Process tracing, memory tampering, registers hijacking"),
    ("kexec_load", "Host kernel replacement and kexec reboot attack"),
    ("kexec_file_load", "Host kernel replacement via file descriptor"),
    ("clone3", "Modern process namespace creation bypass"),
    ("process_vm_readv", "Direct cross-process memory extraction"),
    ("process_vm_writev", "Direct cross-process code and memory injection"),
    ("userfaultfd", "Kernel heap exploitation and page fault race manipulation"),
    ("reboot", "Host system reboot and denial of service"),
    ("sys_chroot", "Chroot jailbreak and root filesystem manipulation"),
    ("bpf", "Host eBPF program injection and kernel verifier bypass"),
    ("mount", "Filesystem mount manipulation on host"),
    ("umount2", "Host filesystem unmounting"),
    ("pivot_root", "Root filesystem reassignment"),
    ("setns", "Host namespace joining and escape"),
    ("unshare", "Unprivileged namespace creation and user namespace exploit"),
    ("iopl", "Direct x86 I/O port privilege level elevation"),
    ("ioperm", "Arbitrary x86 I/O port permission modification"),
    ("init_module", "Loading arbitrary malicious host kernel modules"),
    ("finit_module", "Loading kernel module from file descriptor"),
    ("delete_module", "Unloading security-critical kernel modules"),
    ("perf_event_open", "Kernel performance counter side-channel extraction"),
    ("open_by_handle_at", "Filesystem inode handle jailbreak bypassing directory trees"),
    ("name_to_handle_at", "Obtaining file handles for jailbreak attacks"),
    ("keyctl", "Accessing host kernel keyring credentials"),
    ("add_key", "Injecting keys into host kernel keyring"),
    ("request_key", "Requesting keys from host kernel keyring"),
    ("acct", "BSD process accounting tampering"),
    ("swapon", "Enabling malicious swap device"),
    ("swapoff", "Disabling host swap memory"),
];

/// Checks if a given syscall name is classified as lethal/dangerous.
pub fn is_lethal_syscall(name: &str) -> bool {
    LETHAL_DANGEROUS_SYSCALLS.iter().any(|(s, _)| *s == name)
}

/// Agent Task Profiles categorizing the exact syscall needs of an autonomous workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentTaskProfile {
    /// Pure compute workload: tensor operations, mathematical proof checking, deterministic code execution.
    /// Strictly no networking, no file creation outside stdout/stderr, minimal memory management.
    ComputeReadOnly,
    /// WASI 0.2 / Wasmtime sandboxed component execution: memory mapping, pipes, futex synchronization.
    SandboxedWasmRunner,
    /// Edge gateway worker: bound TCP/UDP sockets and non-blocking polling, without filesystem write authority.
    NetworkEdgeWorker,
    /// Custom explicit syscall allowlist, scrubbed against lethal attack vectors.
    CustomSyscallSet(Vec<String>),
}

/// Default action when an unauthorized syscall is attempted by the microVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeccompDefaultAction {
    Errno(u32),
    KillProcess,
    KillThread,
    Trap,
}

impl SeccompDefaultAction {
    pub fn errno_eperm() -> Self {
        Self::Errno(1)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Errno(_) => "errno",
            Self::KillProcess => "kill_process",
            Self::KillThread => "kill_thread",
            Self::Trap => "trap",
        }
    }

    pub fn errno_val(&self) -> Option<u32> {
        match self {
            Self::Errno(val) => Some(*val),
            _ => None,
        }
    }
}

/// Serialized rule entry for an authorized syscall in Firecracker format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeccompSyscallRule {
    pub name: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errno: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub comment: Vec<String>,
}

/// Complete Firecracker-compatible Seccomp Profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicFirecrackerSeccompProfile {
    pub metadata: DynamicSeccompMetadata,
    pub default_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_errno: Option<u32>,
    pub allowed_syscalls: Vec<SeccompSyscallRule>,
    pub blocked_dangerous_syscalls: Vec<BlockedSyscallEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicSeccompMetadata {
    pub profile_name: String,
    pub version: String,
    pub security_level: String,
    pub task_profile: String,
    pub generated_at_epoch_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedSyscallEntry {
    pub name: String,
    pub threat: String,
    pub action: String,
}

/// Dynamic Seccomp Profiler Engine
pub struct DynamicSeccompProfiler;

impl DynamicSeccompProfiler {
    /// Returns the minimal required syscall list for the specified task profile.
    pub fn base_syscalls_for_profile(profile: &AgentTaskProfile) -> Vec<&'static str> {
        match profile {
            AgentTaskProfile::ComputeReadOnly => vec![
                "read", "write", "fstat", "lseek", "mmap", "mprotect", "munmap", "brk",
                "rt_sigaction", "rt_sigprocmask", "rt_sigreturn", "ioctl", "getpid",
                "gettid", "exit", "exit_group", "futex", "nanosleep", "clock_gettime",
                "gettimeofday", "sched_yield", "sigaltstack",
            ],
            AgentTaskProfile::SandboxedWasmRunner => vec![
                "read", "write", "fstat", "newfstatat", "lseek", "mmap", "mprotect", "munmap",
                "madvise", "brk", "rt_sigaction", "rt_sigprocmask", "rt_sigreturn", "ioctl",
                "pipe2", "eventfd2", "epoll_create1", "epoll_ctl", "epoll_wait", "epoll_pwait",
                "getpid", "gettid", "exit", "exit_group", "futex", "nanosleep", "clock_gettime",
                "clock_getres", "sched_yield", "sched_getaffinity", "sigaltstack", "rseq",
            ],
            AgentTaskProfile::NetworkEdgeWorker => vec![
                "read", "write", "fstat", "newfstatat", "close", "mmap", "mprotect", "munmap",
                "brk", "rt_sigaction", "rt_sigprocmask", "rt_sigreturn", "ioctl", "socket",
                "connect", "bind", "listen", "accept4", "getsockopt", "setsockopt", "getsockname",
                "getpeername", "sendto", "recvfrom", "sendmsg", "recvmsg", "shutdown",
                "epoll_create1", "epoll_ctl", "epoll_wait", "epoll_pwait", "poll", "ppoll",
                "getpid", "gettid", "exit", "exit_group", "futex", "nanosleep", "clock_gettime",
            ],
            AgentTaskProfile::CustomSyscallSet(_) => Vec::new(),
        }
    }

    /// Synthesizes a DynamicFirecrackerSeccompProfile tailored strictly to the task profile.
    pub fn generate_profile(
        profile: &AgentTaskProfile,
        default_action: SeccompDefaultAction,
    ) -> DynamicFirecrackerSeccompProfile {
        let mut allowed_set = HashSet::new();

        match profile {
            AgentTaskProfile::CustomSyscallSet(custom) => {
                for s in custom {
                    allowed_set.insert(s.clone());
                }
            }
            _ => {
                for s in Self::base_syscalls_for_profile(profile) {
                    allowed_set.insert(s.to_string());
                }
            }
        }

        // Unconditionally scrub all lethal dangerous syscalls
        for (lethal, _) in LETHAL_DANGEROUS_SYSCALLS {
            allowed_set.remove(*lethal);
        }

        let mut allowed_rules: Vec<SeccompSyscallRule> = allowed_set
            .into_iter()
            .map(|name| SeccompSyscallRule {
                name,
                action: "allow".to_string(),
                errno: None,
                comment: Vec::new(),
            })
            .collect();
        allowed_rules.sort_by(|a, b| a.name.cmp(&b.name));

        let blocked_dangerous = LETHAL_DANGEROUS_SYSCALLS
            .iter()
            .map(|(name, threat)| BlockedSyscallEntry {
                name: name.to_string(),
                threat: threat.to_string(),
                action: default_action.as_str().to_string(),
            })
            .collect();

        let profile_tag = match profile {
            AgentTaskProfile::ComputeReadOnly => "ComputeReadOnly",
            AgentTaskProfile::SandboxedWasmRunner => "SandboxedWasmRunner",
            AgentTaskProfile::NetworkEdgeWorker => "NetworkEdgeWorker",
            AgentTaskProfile::CustomSyscallSet(_) => "CustomSyscallSet",
        };

        let now_sec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        DynamicFirecrackerSeccompProfile {
            metadata: DynamicSeccompMetadata {
                profile_name: format!("hgb_dynamic_seccomp_{}", profile_tag.to_lowercase()),
                version: "2.1.0".to_string(),
                security_level: "DYNAMIC_TASK_ISOLATED".to_string(),
                task_profile: profile_tag.to_string(),
                generated_at_epoch_sec: now_sec,
            },
            default_action: default_action.as_str().to_string(),
            default_errno: default_action.errno_val(),
            allowed_syscalls: allowed_rules,
            blocked_dangerous_syscalls: blocked_dangerous,
        }
    }

    /// Emits the synthesized profile as a JSON file ready for Firecracker's `--seccomp-filter` flag.
    pub fn emit_profile_json(
        profile: &AgentTaskProfile,
        default_action: SeccompDefaultAction,
        output_path: &Path,
    ) -> io::Result<PathBuf> {
        let compiled = Self::generate_profile(profile, default_action);
        let json_str = serde_json::to_string_pretty(&compiled)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output_path, json_str)?;
        Ok(output_path.to_path_buf())
    }

    /// Validates an existing Seccomp profile JSON against lethal syscall leaks.
    pub fn validate_profile_safety(json_str: &str) -> Result<bool, String> {
        let parsed: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| format!("Invalid JSON: {}", e))?;

        if let Some(allowed) = parsed.get("allowed_syscalls").and_then(|v| v.as_array()) {
            for item in allowed {
                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    if is_lethal_syscall(name) {
                        return Err(format!(
                            "Security Violation: Lethal syscall '{}' found in allowlist!",
                            name
                        ));
                    }
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_compute_read_only_profile_generation() {
        let profile = DynamicSeccompProfiler::generate_profile(
            &AgentTaskProfile::ComputeReadOnly,
            SeccompDefaultAction::errno_eperm(),
        );

        assert_eq!(profile.default_action, "errno");
        assert_eq!(profile.default_errno, Some(1));
        assert!(profile.allowed_syscalls.iter().any(|r| r.name == "read"));
        assert!(profile.allowed_syscalls.iter().any(|r| r.name == "write"));

        // Must not contain lethal syscalls
        for (lethal, _) in LETHAL_DANGEROUS_SYSCALLS {
            assert!(!profile.allowed_syscalls.iter().any(|r| r.name == *lethal));
        }
    }

    #[test]
    fn test_lethal_syscall_scrubbing_on_custom_set() {
        let malicious_set = vec![
            "read".to_string(),
            "write".to_string(),
            "ptrace".to_string(),
            "kexec_load".to_string(),
            "reboot".to_string(),
            "futex".to_string(),
        ];

        let profile = DynamicSeccompProfiler::generate_profile(
            &AgentTaskProfile::CustomSyscallSet(malicious_set),
            SeccompDefaultAction::KillProcess,
        );

        assert_eq!(profile.default_action, "kill_process");
        assert!(profile.allowed_syscalls.iter().any(|r| r.name == "read"));
        assert!(profile.allowed_syscalls.iter().any(|r| r.name == "write"));
        assert!(profile.allowed_syscalls.iter().any(|r| r.name == "futex"));

        // All malicious entries must be eliminated
        assert!(!profile.allowed_syscalls.iter().any(|r| r.name == "ptrace"));
        assert!(!profile.allowed_syscalls.iter().any(|r| r.name == "kexec_load"));
        assert!(!profile.allowed_syscalls.iter().any(|r| r.name == "reboot"));
    }

    #[test]
    fn test_emit_and_validate_json_profile() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("dynamic_profile.json");

        let emitted = DynamicSeccompProfiler::emit_profile_json(
            &AgentTaskProfile::SandboxedWasmRunner,
            SeccompDefaultAction::Trap,
            &path,
        )
        .expect("emit json");

        assert!(emitted.exists());
        let content = fs::read_to_string(&emitted).expect("read");
        let valid = DynamicSeccompProfiler::validate_profile_safety(&content);
        assert!(valid.is_ok());
        assert_eq!(valid.unwrap(), true);
    }
}
