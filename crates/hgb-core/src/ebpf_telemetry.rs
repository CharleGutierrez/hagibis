//! # Native eBPF Kernel Telemetry & Anomaly Models (`hgb-core`)
//!
//! Tier 1 Primitive models for eBPF kernel interception, system call telemetry,
//! socket metrics, memory violations, and autonomous remediation contracts.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Target execution domain monitored by the eBPF probes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MonitoredTarget {
    WasmSandbox { sandbox_id: String, instance_id: u64 },
    MicroVm { vm_id: String, vcpu_id: u32 },
    NativeAgentTool { pid: u32, tgid: u32, tool_name: String },
}

impl MonitoredTarget {
    pub fn identifier(&self) -> &str {
        match self {
            Self::WasmSandbox { sandbox_id, .. } => sandbox_id.as_str(),
            Self::MicroVm { vm_id, .. } => vm_id.as_str(),
            Self::NativeAgentTool { tool_name, .. } => tool_name.as_str(),
        }
    }
}

/// Raw register dump captured during page faults or SIGSEGV traps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct RegisterDump {
    pub rip: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rflags: u64,
}

/// Syscall interception telemetry emitted from `sys_enter`/`sys_exit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyscallTelemetry {
    pub target: MonitoredTarget,
    pub syscall_nr: u32,
    pub args: [u64; 6],
    pub return_code: i64,
    pub latency_ns: u64,
    pub timestamp_mono_ns: u64,
}

/// Network socket status and event telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketTelemetry {
    pub target: MonitoredTarget,
    pub fd: u32,
    pub socket_family: u16, // AF_INET (2), AF_INET6 (10)
    pub local_endpoint: String,
    pub remote_endpoint: String,
    pub state_flags: u32,
    pub elapsed_ns: u64,
    pub error_code: Option<i32>,
}

/// File system I/O telemetry intercepted via kernel VFS tracepoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIoTelemetry {
    pub target: MonitoredTarget,
    pub fd: i32,
    pub path: String,
    pub open_flags: u32,
    pub error_code: i32,
    pub bytes_transferred: i64,
    pub timestamp_mono_ns: u64,
}

/// Memory violation and crash telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryViolationTelemetry {
    pub target: MonitoredTarget,
    pub fault_address: u64,
    pub instruction_pointer: u64,
    pub signal: i32, // 11 = SIGSEGV, 7 = SIGBUS
    pub access_type: MemoryAccessType,
    pub registers: RegisterDump,
    pub wasm_linear_memory_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryAccessType {
    Read,
    Write,
    Execute,
    OutOfBoundsWasmHeap,
}

/// Classified anomalies discovered through eBPF telemetry stream analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SentinelAnomaly {
    SegmentationFault {
        violation: MemoryViolationTelemetry,
        is_wasm_bounds_exhaustion: bool,
    },
    InfiniteSyscallLoop {
        target: MonitoredTarget,
        cycle_pattern: Vec<u32>,
        period: usize,
        repetitions: u32,
        window_duration_ns: u64,
    },
    SpinlockStarvation {
        target: MonitoredTarget,
        cpu_burn_ns: u64,
        syscall_count: u32,
        zero_io_progress: bool,
    },
    FileIoViolation {
        telemetry: FileIoTelemetry,
        policy_reason: String,
    },
    SocketTimeout {
        telemetry: SocketTelemetry,
        threshold_ns: u64,
    },
}

/// Hot-patch fallback or replacement strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchStrategy {
    RedirectToSafeStub { stub_identifier: String },
    BypassOperationWithDefaultResponse { response_bytes: Vec<u8> },
    VirtualizeFilesystemPath { virtual_mapping: String },
}

/// Directives emitted by the autonomic remediation engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemediationDirective {
    RestartWasmWithAdjustedQuota {
        sandbox_id: String,
        additional_memory_pages: u32,
        additional_fuel: u64,
        reason: String,
    },
    ApplyHotPatch {
        target: MonitoredTarget,
        faulty_symbol_or_path: String,
        strategy: PatchStrategy,
    },
    QuarantineAndIsolate {
        target: MonitoredTarget,
        freeze_execution: bool,
        memory_dump_path: Option<String>,
        reason: String,
    },
    ThrottleCpuGovernor {
        pid: u32,
        quota_us: u32,
        period_us: u32,
    },
}

/// Proof of remediation execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationReceipt {
    pub receipt_id: String,
    pub target: MonitoredTarget,
    pub directive: RemediationDirective,
    pub generation_id: u64,
    pub executed_at_mono_ns: u64,
    pub success: bool,
    pub details: String,
}

impl fmt::Display for RemediationReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Receipt[{}] target={} gen={} success={}",
            self.receipt_id,
            self.target.identifier(),
            self.generation_id,
            self.success
        )
    }
}
