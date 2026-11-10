pub mod server;
pub mod ambient_service;
pub mod proxy_server;
pub mod remote_bot;
pub mod mcp_sse_server;
pub mod event_driven_pipeline_engine;
pub use server::{DaemonState, HagibisDaemon};
pub use ambient_service::{AmbientDaemonService, DreamJournal, DreamJournalEntry};
pub use event_driven_pipeline_engine::{
    evaluate_condition, DeclarativePipeline, EventDrivenPipelineEngine,
    PipelineExecutionReport, PipelineStep, PipelineTrigger, StepExecutionReport,
};
pub use mcp_sse_server::{
    McpSseServer, McpSseServerStatus, McpSseState, DEFAULT_MCP_SSE_PORT,
};
pub use proxy_server::{
    build_proxy_router, start_proxy_server, AnthropicBlock, AnthropicContent,
    AnthropicIncomingMessage, AnthropicMessagesRequest, AnthropicResponseMessage,
    ModelItem, ModelListResponse, OpenAiChatRequest, OpenAiChatResponse,
    ProxyServerConfig, ProxyState,
};
pub use remote_bot::{
    BotCommand, IncomingBotMessage, OutgoingBotResponse, RemoteBotConfig,
    RemoteMobileGateway,
};

use colored::Colorize;
use std::path::Path;

pub fn get_memory_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        // 1. Primary: Real dynamic /proc/self/statm parsing with dynamic system page size
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(rss_pages_str) = statm.split_whitespace().nth(1) {
                if let Ok(pages) = rss_pages_str.parse::<f64>() {
                    let page_size_bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                    let page_size = if page_size_bytes > 0 {
                        page_size_bytes as f64
                    } else {
                        4096.0
                    };
                    let mb = (pages * page_size) / (1024.0 * 1024.0);
                    if mb > 0.0 {
                        return mb;
                    }
                }
            }
        }

        // 2. Secondary Linux fallback: /proc/self/status VmRSS
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if let Some(kb_str) = parts.first() {
                        if let Ok(kb) = kb_str.parse::<f64>() {
                            let mb = kb / 1024.0;
                            if mb > 0.0 {
                                return mb;
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Unix getrusage dynamic fallback
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            let u = unsafe { usage.assume_init() };
            #[cfg(target_os = "macos")]
            let mb = (u.ru_maxrss as f64) / (1024.0 * 1024.0);
            #[cfg(not(target_os = "macos"))]
            let mb = (u.ru_maxrss as f64) / 1024.0;
            if mb > 0.0 {
                return mb;
            }
        }
    }

    0.0
}

/// Compute cryptographic Blake3 hash of the live workspace manifest
pub fn compute_workspace_manifest_hash(workspace_root: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut files = Vec::new();
    collect_manifest_files(workspace_root, Path::new(""), &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel_path, size, mtime) in files {
        hasher.update(rel_path.as_bytes());
        hasher.update(&size.to_le_bytes());
        hasher.update(&mtime.to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn collect_manifest_files(dir: &Path, rel_prefix: &Path, files: &mut Vec<(String, u64, u64)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name == ".git"
                || file_name == ".hgb"
                || file_name == "target"
                || file_name == "node_modules"
                || file_name == ".cache"
                || file_name.ends_with(".tmp")
            {
                continue;
            }
            if let Ok(ft) = entry.file_type() {
                if ft.is_symlink() && path.is_dir() {
                    continue;
                }
            }
            let rel = rel_prefix.join(&file_name);
            if path.is_dir() {
                collect_manifest_files(&path, &rel, files);
            } else if path.is_file() {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                files.push((rel.to_string_lossy().to_string(), size, mtime));
            }
        }
    }
}

/// Cached completion record from an LLM provider
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderCacheEntry {
    pub prompt_hash: String,
    pub model: String,
    pub response: String,
    pub manifest_hash: String,
    pub created_at_epoch_sec: u64,
}

/// In-Process Shared Provider Cache with Manifest-Hash Invalidation
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SharedProviderCache {
    pub current_manifest_hash: String,
    pub entries: std::collections::HashMap<String, ProviderCacheEntry>,
    pub max_entries: usize,
    pub total_hits: u64,
    pub total_invalidations: u64,
}

impl SharedProviderCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            current_manifest_hash: String::new(),
            entries: std::collections::HashMap::new(),
            max_entries: max_entries.max(1),
            total_hits: 0,
            total_invalidations: 0,
        }
    }

    /// Invalidate cache if workspace manifest hash has changed
    pub fn invalidate_if_changed(&mut self, workspace_root: &Path) -> bool {
        let live_hash = compute_workspace_manifest_hash(workspace_root);
        if live_hash != self.current_manifest_hash {
            let was_empty = self.current_manifest_hash.is_empty();
            self.current_manifest_hash = live_hash;
            if !was_empty && !self.entries.is_empty() {
                self.entries.clear();
                self.total_invalidations += 1;
                return true;
            }
        }
        false
    }

    pub fn lookup(&mut self, model: &str, prompt: &str, current_manifest_hash: &str) -> Option<String> {
        if self.current_manifest_hash != current_manifest_hash {
            self.entries.clear();
            self.current_manifest_hash = current_manifest_hash.to_string();
            self.total_invalidations += 1;
            return None;
        }

        let key = format!("{}:{}", model, prompt);
        let hash = blake3::hash(key.as_bytes()).to_hex().to_string();
        if let Some(entry) = self.entries.get(&hash) {
            if entry.manifest_hash == current_manifest_hash {
                self.total_hits += 1;
                return Some(entry.response.clone());
            }
        }
        None
    }

    pub fn store(&mut self, model: &str, prompt: &str, response: &str, manifest_hash: &str) {
        if self.entries.len() >= self.max_entries {
            self.entries.clear();
        }
        let key = format!("{}:{}", model, prompt);
        let hash = blake3::hash(key.as_bytes()).to_hex().to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.entries.insert(
            hash.clone(),
            ProviderCacheEntry {
                prompt_hash: hash,
                model: model.to_string(),
                response: response.to_string(),
                manifest_hash: manifest_hash.to_string(),
                created_at_epoch_sec: now,
            },
        );
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_invalidations += 1;
    }
}

pub fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    let mut in_csi = false;

    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            in_csi = false;
        } else if in_escape {
            if c == '[' {
                in_csi = true;
            } else if in_csi {
                if (c >= '@' && c <= '~') || c.is_ascii_alphabetic() {
                    in_escape = false;
                    in_csi = false;
                }
            } else {
                in_escape = false;
            }
        } else {
            width += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    width
}

pub fn render_daemon_card(socket_path: &Path) -> String {
    let width: usize = 78;
    let inner_width = width - 2;

    let mut out = String::new();
    let top_border = format!("╭{}╮", "─".repeat(inner_width));
    let divider = format!("├{}┤", "─".repeat(inner_width));
    let bottom_border = format!("╰{}╯", "─".repeat(inner_width));

    let header_colored = format!(
        "{} {} {}",
        "▲".cyan().bold(),
        "HAGIBIS MICROKERNEL DAEMON (hgbd)".white().bold(),
        "─ Resident Engine v0.1.0".dimmed()
    );

    let socket_str = socket_path.display().to_string();
    let socket_colored = format!("[🔌 {}]", socket_str).cyan().bold().to_string();

    let provider_status = hgb_core::GeminiProvider::credential_status();
    let provider_colored = format!("[{}]", provider_status).magenta().bold().to_string();

    let memory_rss = get_memory_rss_mb();
    let memory_colored = format!("[RAM: {:.1} MB • Zero-Copy Swarm Cache: Nominal]", memory_rss)
        .yellow()
        .bold()
        .to_string();

    let ready_colored = format!("[{} {}]", "●".green().bold(), "ONLINE / READY".green().bold());

    let pad_line = |content: &str| -> String {
        let vis = visible_width(content);
        let pad = inner_width.saturating_sub(vis + 2);
        format!("│ {}{} │\n", content, " ".repeat(pad))
    };

    out.push_str(&format!("{}\n", top_border.cyan()));
    out.push_str(&pad_line(&header_colored));
    out.push_str(&format!("{}\n", divider.cyan()));
    out.push_str(&pad_line(&format!("Socket:    {}", socket_colored)));
    out.push_str(&pad_line(&format!("Provider:  {}", provider_colored)));
    out.push_str(&pad_line(&format!("Memory:    {}", memory_colored)));
    out.push_str(&pad_line(&format!("State:     {}", ready_colored)));
    out.push_str(&format!("{}\n", bottom_border.cyan()));

    out
}

pub fn print_daemon_card(socket_path: &Path) {
    print!("{}", render_daemon_card(socket_path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_card_rendering() {
        let path = Path::new("/run/user/1000/hgb.sock");
        let rendered = render_daemon_card(path);
        assert!(rendered.contains("HAGIBIS MICROKERNEL DAEMON (hgbd)"));
        assert!(rendered.contains("ONLINE / READY"));
        assert!(rendered.contains("hgb.sock"));
        assert!(rendered.contains("RAM:"));
    }

    #[test]
    fn test_dynamic_memory_rss_telemetry() {
        let rss_mb = get_memory_rss_mb();
        #[cfg(target_os = "linux")]
        {
            assert!(rss_mb > 0.0, "Real dynamic RSS on Linux must be > 0.0 MB, got {}", rss_mb);
        }
    }

    #[test]
    fn test_shared_provider_cache_manifest_invalidation() {
        let temp_dir = std::env::temp_dir().join(format!("cache_inv_test_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&temp_dir);

        let test_file = temp_dir.join("code.rs");
        std::fs::write(&test_file, b"fn alpha() {}\n").unwrap();

        let initial_hash = compute_workspace_manifest_hash(&temp_dir);
        assert!(!initial_hash.is_empty());

        let mut cache = SharedProviderCache::new(50);
        cache.current_manifest_hash = initial_hash.clone();
        cache.store("gemini-2.5-flash", "What is alpha?", "Alpha is a function", &initial_hash);

        // Verify cache hit
        let hit = cache.lookup("gemini-2.5-flash", "What is alpha?", &initial_hash);
        assert_eq!(hit.as_deref(), Some("Alpha is a function"));

        // Mutate workspace file
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&test_file, b"fn alpha() { println!(\"mutated\"); }\n").unwrap();

        let new_hash = compute_workspace_manifest_hash(&temp_dir);
        assert_ne!(initial_hash, new_hash, "Manifest hash must change when workspace files mutate");

        // Lookup with new manifest hash must invalidate and return None (miss)
        let miss = cache.lookup("gemini-2.5-flash", "What is alpha?", &new_hash);
        assert_eq!(miss, None, "Expected cache miss after workspace manifest changed");
        assert_eq!(cache.total_invalidations, 1);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

// ===========================================================================
// ADDITIONS FOR crates/hgb-daemon/src/lib.rs
// ===========================================================================

pub mod p2p_tunnel;

pub use p2p_tunnel::{
    build_tunnel_router, P2pAuthToken, P2pClientMetrics, P2pSessionManager,
    P2pTunnelServer, TerminalQrRasterizer, TunnelState,
};

// Superpower 3: Unified MCP Mesh Network
pub mod mcp_mesh_network;
pub use mcp_mesh_network::{
    McpMeshNetwork, MeshMutationVector, MeshPeer, MeshStatusReport, MeshSyncReport, MeshTool,
};

// Pillar 3: Self-Healing Autonomous Daemon Loops
pub mod self_healing_daemon;
pub use self_healing_daemon::{
    DiagnosticPackage, ExecutionFailureEvent, FailureSource, HealingAttemptReport,
    SelfHealingDaemonLoop, SelfHealingDaemonStatus,
};

// Superpower 1: Zero-Overhead IPC via Shared Memory & Unix Domain Sockets
pub mod shm_grpc_bridge;
pub use shm_grpc_bridge::{
    ShmBridgeStatus, ShmGrpcBridge, DEFAULT_SHM_SOCKET_PATH,
};

// Superpower 4: Hardware-Aware Dynamic CPU Governor Integration
pub mod governor_watchdog;
pub use governor_watchdog::{
    CorrectiveGovernorParams, GovernorWatchdog, GovernorWatchdogStatus,
    ProcessSample, TelemetryOptimizationRecord, TriggerReason,
};

// Autonomous Expansion Pillar 3: Proactive Autonomous Feedback Loops (Vibe-Coding Watcher)
pub mod vibe_coding_watcher;
pub use vibe_coding_watcher::{
    DpoDistillationPair, VibeCodingWatcher, VibeWatcherCycleRecord, VibeWatcherStatusReport,
};

// Strategic Upgrade 2: Distributed MicroVM Swarms & Agent Mesh via Firecracker & WASM
pub mod distributed_agent_mesh;
pub use distributed_agent_mesh::{
    AgentMeshStatusReport, DistributedAgentMesh, MeshTask, MeshTaskStatus, MeshWorkerInfo,
    MeshWorkerTier, SwarmExecutionResult,
};

// Strategic Upgrade 4: Universal Model Context Protocol (MCP) Hot-Reloading
pub mod mcp_hot_reloader;
pub use mcp_hot_reloader::{
    HotLoadedTool, McpHotReloader, McpHotReloaderReport, McpPluginManifest,
};

// Strategic Upgrade 5: Zero-Copy IPC Daemon Communication
pub mod zero_copy_ipc;
pub use zero_copy_ipc::{
    ZeroCopyFrameHeader, ZeroCopyIpcBenchReport, ZeroCopyIpcStatusReport, ZeroCopyIpcTransport,
    DEFAULT_SHM_FILE_PATH, DEFAULT_SHM_RING_CAPACITY, ZERO_COPY_MAGIC,
};

// Strategic Upgrade 2: Native Multi-Modal MCP Streaming & Dynamic Tool Hot-Swapping
pub mod dynamic_tool_hotswap;
pub use dynamic_tool_hotswap::{
    DynamicCompilationResult, DynamicHotSwapperReport, DynamicSourceLanguage,
    DynamicToolHotSwapper, DynamicToolRequest, DynamicToolSummary,
};

// Upgrade 1: Resident Daemon Shared-Memory Heartbeat Watchdog
pub mod heartbeat_watchdog;
pub use heartbeat_watchdog::{
    check_daemon_health, check_daemon_health_at, resolve_heartbeat_path, DaemonLivenessStatus,
    HeartbeatHeader, HeartbeatStatusSnapshot, HeartbeatWatchdog, DEFAULT_HEARTBEAT_PATH,
    HEARTBEAT_HEADER_SIZE, HEARTBEAT_MAGIC, HEARTBEAT_VERSION,
};

// Pillar 4: Autonomous Self-Healing Agent Loops (Autonomous Watchdog, Snapshotting, and Self-Correction)
pub mod autonomous_watchdog;
pub use autonomous_watchdog::{
    AgentContextSnapshot, AgentGenerationParams, AutonomousWatchdog, FailurePattern,
    RecoveryStory, WatchdogStatusReport,
};

// Pillar 2: Distributed MicroVM Mesh & WASM Swarm Federation Service
pub mod swarm_service;
pub use swarm_service::{
    SwarmDaemonConfig, SwarmDaemonService, SwarmServiceStatusSnapshot,
};

// Vector 2: Universal WASM Plugin Hot-Reloading Service
pub mod wasm_hot_reload_service;
pub use wasm_hot_reload_service::WasmHotReloadService;

// Pillar 4: Autonomous Agent Loop Expansion (Masterclass Chapter 8)
pub mod autonomous_daemon_loop;
pub use autonomous_daemon_loop::{
    AutonomousDaemonLoopService, AutonomousDaemonLoopStatusReport, AutonomousDaemonRequest,
};

// Enhancement 4: Dynamic MCP Plugin Registry
pub mod dynamic_mcp_registry;
pub use dynamic_mcp_registry::{
    DynamicMcpRegistry, DynamicMcpRegistryReport, DynamicMcpTool, ToolBenchmarkMetrics,
};

// Pillar 2: Autonomous Self-Profiling & PGO Feedback Loops
pub mod pgo_feedback_loop;
pub use pgo_feedback_loop::{
    ConcurrencyRegulationReport, HardwareGovernorSynergy, HardwareProfileTelemetry,
    LatencyPercentiles, PgoFeedbackLoop, PgoFeedbackStatusReport, PgoHotspotSnapshot,
    PgoTriggerReport, TaskExecutionTelemetry, TaskFailureDiagnosticSnapshot,
    TokenStreamStats, ToolCallStats, DEFAULT_HOTSPOT_TOOL_CALL_THRESHOLD,
    DEFAULT_LATENCY_SLA_P99_THRESHOLD_MS, DEFAULT_PGO_COOLDOWN_SECS,
    TASK_ERROR_RATE_THROTTLE_THRESHOLD_PCT, THERMAL_CRITICAL_TEMP_C,
    THERMAL_RECOVERY_TEMP_C, THERMAL_THROTTLE_TEMP_C,
    BATTERY_DISCHARGE_CRITICAL_PCT, BURST_MGLRU_MIN_TTL_MS, IDLE_MGLRU_MIN_TTL_MS,
};

// Pillar 3: Distributed Firecracker MicroVM Swarms
pub mod microvm_work_stealing_queue;
pub use microvm_work_stealing_queue::{
    MicrovmJob, MicrovmJobResult, MicrovmQueueStatus, MicrovmWorkStealingQueue,
    WorkStealingQueue, DEFAULT_MAX_SPINUP_TARGET_MS,
};

// Pillar II: Hardware-Software Co-Design & Dynamic Workload Shedding
pub mod workload_shedder;
pub use workload_shedder::{
    DynamicWorkloadShedder, HeavyWorkloadKind, BackgroundJobKind, BackgroundJobState,
    ShedderState, HardwareTelemetrySnapshot, WorkloadShedderStatus, ShedderEvaluationReport,
    WorkloadGuard, THERMAL_PEAK_THRESHOLD_C, THERMAL_RECOVERY_THRESHOLD_C,
    BATTERY_CRITICAL_THRESHOLD_PCT, THROTTLED_CONCURRENCY_WORKERS,
};

// Pillar 4: Native Fully Autonomous Self-Healing Daemon Engine (Masterclass Chapter 8)
pub mod native_self_healing_daemon;
pub use native_self_healing_daemon::{
    AutonomousConsensusCertificate, CandidatePatch, DiagnosticLocation, HealingIncidentReport,
    IncidentKind, NativeSelfHealingDaemon, RamCowOverlaySandbox, SelfHealingLogStore,
    VerificationResult, DEFAULT_BENCHMARK_REGRESSION_THRESHOLD_PCT, DEFAULT_MAX_RETRIES,
};

// Pillar 1: Direct IPC & FFI Bridge with Dynatune & Kernel-Optimizer
pub mod dynatune_ipc_governor;
pub use dynatune_ipc_governor::{
    DynatuneGovernorStatus, DynatuneIpcConfig, DynatuneIpcGovernor, DynatuneIpcMessage,
    DynatuneIpcResponse, DynatuneWorkloadLease, GovernorMode, HeavyWorkloadType,
    TelemetryOverride, BATTERY_CRITICAL_DISCHARGE_PCT,
    CLAMPED_CONCURRENCY_LIMIT, DEFAULT_DYNATUNE_SOCK, DEFAULT_IDLE_GRACE_PERIOD_SECS,
    DEFAULT_NOMINAL_CONCURRENCY_LIMIT, SIBLING_KERNEL_CTL_BIN,
    SYSTEM_KERNEL_CTL_BIN, THERMAL_CLAMP_THRESHOLD_C,
};

// Pillar 2: Event-Driven Actor Model (Masterclass Chapter 8)
pub mod agent_actor_mesh;
pub use agent_actor_mesh::{
    ActorHandle, ActorHealthStatus, ActorMessage, ActorStatusSummary,
    ActorSupervisor, AgentActor, DeadLetterRecord, SupervisorEvent,
    DEFAULT_ACTOR_MAILBOX_CAPACITY, DEFAULT_HEARTBEAT_TIMEOUT_MS,
};

// Pillar 2: Autonomous Evaluation (Eval-Driven Development)
pub mod eval_driven_gate;
pub use eval_driven_gate::{
    DeterministicAssertion, EvalDrivenGate, EvalMetrics, EvalSpec, EvalVerdict,
    SpeculativeRamOverlay,
};

// Pillar 3: Dynamic Thermal/Power Awareness IPC Bridge
pub mod hardware_telemetry_bridge;
pub use hardware_telemetry_bridge::{
    DynatuneIpcCommand, HardwareGovernor, HardwareProfile,
    HardwareTelemetrySnapshot as BridgeHardwareTelemetrySnapshot,
};

// Pillar 1: Close the Loop Between Dynatune, Kernel-Optimizer, and Hagibis Daemon
pub mod hardware_aware_ai_controller;
pub use hardware_aware_ai_controller::{
    HardwareAwareAiController, GenerationPacer, WorkloadThrottler,
    HardwareTelemetry, PacingEvaluation, GovernorMode as HwGovernorMode,
    ThermalZoneTelemetry, MemoryPressurePsi, PsiRecord,
    BATTERY_CRITICAL_DISCHARGE_PCT as HW_BATTERY_CRITICAL_DISCHARGE_PCT,
    BOOST_CONCURRENCY_LIMIT as HW_BOOST_CONCURRENCY_LIMIT,
    NOMINAL_CONCURRENCY_LIMIT as HW_NOMINAL_CONCURRENCY_LIMIT,
    THROTTLED_CONCURRENCY_LIMIT as HW_THROTTLED_CONCURRENCY_LIMIT,
    THERMAL_CLAMP_THRESHOLD_C as HW_THERMAL_CLAMP_THRESHOLD_C,
    THERMAL_COOL_BOOST_THRESHOLD_C as HW_THERMAL_COOL_BOOST_THRESHOLD_C,
    THERMAL_RECOVERY_THRESHOLD_C as HW_THERMAL_RECOVERY_THRESHOLD_C,
    THROTTLED_TOKEN_PACING_DELAY_MS,
};

// Pillar 2: Multi-Tenant WebAssembly MCP Tool Integration
pub mod multitenant_mcp_tool;
pub use multitenant_mcp_tool::{
    MultitenantMcpRequest, MultitenantMcpResponse, MultitenantMcpTool, MultitenantMcpToolConfig,
    MULTITENANT_WASM_TOOL_NAME,
};

// Pillar 4: Native Desktop Cockpit Launcher & Visual Monitor
pub mod desktop_cockpit_launcher;
pub use desktop_cockpit_launcher::{
    build_desktop_cockpit_router, CockpitUnifiedMetrics, CpuGovernorTelemetry,
    DesktopCockpitLauncher, ExecutionRuntimeMetrics, MasterclassBookmark,
    McpAggregatedMetrics, McpToolCallTelemetry, SetGovernorPayload, SidecarHealthReport,
    SidecarStatus, DEFAULT_DAEMON_SOCKET, MASTERCLASS_PDF_NAME,
};

// Pillar 1: Distributed "Swarm" MicroVMs via Firecracker & WASM
pub mod distributed_swarm_orchestrator;
pub use distributed_swarm_orchestrator::{
    DistributedSwarmOrchestrator, NodeHealthState, NodeLocation,
    SwarmClusterTelemetry, SwarmDispatchResult, SwarmDispatchTask,
    SwarmMcpTools, SwarmNodeProfile, TaskPriority, WorkerRuntimeKind,
    SWARM_DISPATCH_TOOL_NAME, SWARM_TELEMETRY_TOOL_NAME,
};

// Pillar 3: Advanced Closed-Loop Autonomous Reflection Runner
pub mod closed_loop_self_healing_runner;
pub use closed_loop_self_healing_runner::{
    AutonomousSelfHealRequest, AutonomousSelfHealResponse, ClosedLoopSelfHealingRunner,
    AUTONOMOUS_SELF_HEAL_TOOL_NAME,
};

// Pillar 4: Unified Neural-Hardware Feedback Loop with dynatune & kernel-optimizer
pub mod unified_neural_hardware_loop;
pub use unified_neural_hardware_loop::{
    AiWorkloadKind, HardwareBoostGuard, HardwareLoopPaths, NeuralGovernorProfile,
    SafetyClampStatus, UnifiedHardwareTelemetry, UnifiedNeuralHardwareLoop,
    BATTERY_CRITICAL_DISCHARGE_PCT as NEURAL_BATTERY_CRITICAL_DISCHARGE_PCT,
    BATTERY_RECOVERY_DISCHARGE_PCT as NEURAL_BATTERY_RECOVERY_DISCHARGE_PCT,
    BURST_MGLRU_MIN_TTL_MS as NEURAL_BURST_MGLRU_MIN_TTL_MS,
    DEFAULT_DYNATUNE_SOCKET, DEFAULT_IDLE_GRACE_PERIOD_SECS as NEURAL_DEFAULT_IDLE_GRACE_PERIOD_SECS,
    IDLE_MGLRU_MIN_TTL_MS as NEURAL_IDLE_MGLRU_MIN_TTL_MS,
    NEURAL_HARDWARE_BOOST_TOOL_NAME,
    THERMAL_CLAMP_THRESHOLD_C as NEURAL_THERMAL_CLAMP_THRESHOLD_C,
    THERMAL_RECOVERY_THRESHOLD_C as NEURAL_THERMAL_RECOVERY_THRESHOLD_C,
};

// Power-Up 4: Hardware-Aware Dynamic Acceleration (dynatune & kernel-optimizer)
pub mod hardware_dynamic_ffi_bridge;
pub use hardware_dynamic_ffi_bridge::{
    AccelerationWorkload, BatteryTelemetry, HardwareAccelerationGuard,
    HardwareBridgeConfig, HardwareBridgeTelemetrySnapshot, HardwareDynamicFfiBridge,
    HardwareGovernorProfile, HardwareTelemetryOverride, MemoryPressureRecord,
    MemoryTelemetry, SafetyClampState, ThermalTelemetry, ThermalZoneMetric,
    BATTERY_CRITICAL_DISCHARGE_PCT as FFI_BATTERY_CRITICAL_DISCHARGE_PCT,
    BATTERY_RECOVERY_DISCHARGE_PCT as FFI_BATTERY_RECOVERY_DISCHARGE_PCT,
    BURST_MGLRU_MIN_TTL_MS as FFI_BURST_MGLRU_MIN_TTL_MS,
    CLAMPED_CONCURRENCY_LIMIT as FFI_CLAMPED_CONCURRENCY_LIMIT,
    DEFAULT_BOOST_CONCURRENCY_LIMIT as FFI_DEFAULT_BOOST_CONCURRENCY_LIMIT,
    DEFAULT_DYNATUNE_SOCK as FFI_DEFAULT_DYNATUNE_SOCK,
    DEFAULT_IDLE_GRACE_PERIOD_SECS as FFI_DEFAULT_IDLE_GRACE_PERIOD_SECS,
    DEFAULT_NOMINAL_CONCURRENCY_LIMIT as FFI_DEFAULT_NOMINAL_CONCURRENCY_LIMIT,
    IDLE_MGLRU_MIN_TTL_MS as FFI_IDLE_MGLRU_MIN_TTL_MS,
    THERMAL_CLAMP_THRESHOLD_C as FFI_THERMAL_CLAMP_THRESHOLD_C,
    THERMAL_RECOVERY_THRESHOLD_C as FFI_THERMAL_RECOVERY_THRESHOLD_C,
};

// Power-Up 3: Autonomous MCP Self-Healing & Profiling Loop
pub mod autonomous_mcp_self_healing_loop;
pub use autonomous_mcp_self_healing_loop::{
    AutoPatchBottleneckReport, AutoPatchBottleneckRequest, AutonomousMcpSelfHealingLoop,
    DiagnosticLanguage, ParsedDiagnostic, PgoHardwareTelemetry, PgoTunePassReport,
    PgoTunePassRequest, SelfHealingDiagnoseReport, SelfHealingDiagnoseRequest,
};

// Architectural Power-Up I: Close the Loop with Native Linux Kernel Integration (`dynatune` & `kernel-optimizer`)
pub mod kernel_dynatune_ipc_loop;
pub use kernel_dynatune_ipc_loop::{
    BatterySnapshot, BurstWorkload, CpuTuningState, DynatuneKernelPaths,
    KernelDynatuneConfig, KernelDynatuneLoop, KernelIpcCommand, KernelIpcResponse,
    KernelLoopStatus, KernelProfile, MglruStatus, ThermalSnapshot, ThermalZone,
    WorkloadBurstGuard,
};

// Pillar 2: Distributed MicroVM Orchestration (firecracker-microvm/firecracker)
pub mod firecracker_microvm_runner;
pub use firecracker_microvm_runner::{
    FirecrackerExecutionReport, FirecrackerMicroVmRunner, ManagedJailInfo,
    ManagedMicrovmJail, MicrovmJailStatus, ProvisionJailSpec,
    DEFAULT_RUNTIME_SOCKET_DIR, FALLBACK_SHM_SOCKET_DIR,
};

// Strategic Upgrade: Distributed MicroVM Hot-Plugging (Dynamic WASM Injection)
pub mod microvm_hotplug_manager;
pub use microvm_hotplug_manager::{
    HotPlugConfig, HotPlugExecutionReport, HotPlugReport, HotPluggedModuleEntry,
    MicroVmHotPlugManager,
};

// Strategic Upgrade: Real-Time Autonomous Self-Healing Control Loops (Dynatune & OTel Governor)
pub mod autonomous_hardware_healer_loop;
pub use autonomous_hardware_healer_loop::{
    AutonomousHardwareHealerLoop, AutonomousHealDecision, AutonomicHardwareTelemetrySnapshot,
    HealerHealthState, ThreadSheddingReport, WorkloadMigrationReport,
    MEMORY_PRESSURE_CRITICAL_PERCENT, THERMAL_CRITICAL_THRESHOLD_CELSIUS,
    THERMAL_WARNING_THRESHOLD_CELSIUS,
};

// Strategic Power-Up: Distributed MicroVM Mesh via Raft Consensus
pub mod distributed_microvm_mesh;
pub use distributed_microvm_mesh::{
    ClusterNodeDescriptor, DistributedMicrovmMesh, MeshExecutionReport,
    MeshIsolationKind, MeshTaskSpec, MeshWorkloadKind,
    DEFAULT_NODE_HEARTBEAT_TIMEOUT_MS, DEFAULT_TASK_LEASE_TIMEOUT_MS,
};

// Strategic Power-Up: Active Reinforcement Learning & Self-Compilation Loop (PGO)
pub mod reinforcement_learning_pgo_loop;
pub use reinforcement_learning_pgo_loop::{
    LocalRewardLedger, LocalRewardModel, OptimizationActionCategory,
    OptimizationEpisode, PgoRecompilationReport, ReinforcementLearningPgoLoop,
    TelemetryDeltaMetrics, DEFAULT_PGO_TRIGGER_REWARD_THRESHOLD,
};

// Strategic Upgrade 1: Autonomous Self-Compilation & PGO Hot-Swap Daemon
pub mod autonomous_pgo_hotswap_daemon;
pub use autonomous_pgo_hotswap_daemon::{
    AutonomousPgoHotswapDaemon, PgoHotswapReport, SubsystemHotspotMetric,
    WorkloadHotspotProfiler, ZeroDowntimeBinarySwapper,
    DEFAULT_HEALTH_CHECK_TIMEOUT_MS, DEFAULT_PGO_COOLDOWN_SECONDS,
    DEFAULT_PGO_PROFILE_TRIGGER_INVOCATIONS,
};

// Strategic Upgrade 2: Distributed MicroVM Swarm Intelligence (Firecracker + Wasm Mesh)
pub mod distributed_swarm_intelligence;
pub use distributed_swarm_intelligence::{
    DistributedSwarmCoordinator, EphemeralWasmMicrokernelCompiler,
    SwarmExecutionClusterReport, SwarmPodDescriptor, SwarmPodExecutionKind,
    SwarmPodState, SwarmShardResult, SwarmTaskShard,
    DEFAULT_MAX_POD_CAPACITY, DEFAULT_SWARM_HEARTBEAT_TIMEOUT_MS,
};

// Strategic Power-Up 1: Distributed Swarm MicroVM & Wasm Execution Fabric
pub mod distributed_swarm_fabric;
pub use distributed_swarm_fabric::{
    DistributedSwarmFabric, MeshPeerNode, NodeLoadMetrics, NodeMeshCapabilities,
    OffloadExecutionReport, OffloadTaskKind, OffloadTaskSpec, HEARTBEAT_TIMEOUT_MS,
};

// Strategic Power-Up 3: Natural Language Dynatune Agent
pub mod natural_language_dynatune_agent;
pub use natural_language_dynatune_agent::{
    DynatuneExecutionReport, DynatuneOptimizationIntent, KernelTuningActionPlan,
    NaturalLanguageDynatuneAgent,
};

// Strategic Power-Up 4: Zero-Copy Shared Memory Daemon Transport
pub mod zero_copy_shm_server;
pub use zero_copy_shm_server::{
    ZeroCopyIpcBenchmarkMetrics, ZeroCopyShmServer,
};

// Strategic Power-Up Directive 4: Self-Hosting Autonomic PGO Rewriter & Hot-Swap Daemon
pub mod self_healing_pgo_rewriter;
pub use self_healing_pgo_rewriter::{
    PgoRecompileExecutionReport, PgoRecompileReason, SelfHealingPgoRewriter,
    DEFAULT_PGO_HOTSPOT_INVOCATIONS_THRESHOLD, DEFAULT_PGO_MIN_COOLDOWN_MS,
    DEFAULT_PGO_REGRESSION_THRESHOLD_PCT,
};

// Strategic Power-Up Pillar 1: Distributed Raft Agent Mesh Service
pub mod distributed_raft_agent_service;
pub use distributed_raft_agent_service::{
    DistributedRaftAgentService, DistributedRaftServiceConfig,
};

// Strategic Power-Up Pillar 3: Intent-Driven Autonomous Self-Evolving Code Loops
pub mod autonomous_evolutionary_loop;
pub use autonomous_evolutionary_loop::{
    AutonomousEvolutionaryLoop, EvolutionaryCycleReceipt, EvolutionaryCycleStatus,
    EvolutionaryTriggerKind,
};

// Strategic Power-Up Directive 3: Native Agentic Self-Compilation & Hot-Patching
pub mod agentic_self_compiler;
pub use agentic_self_compiler::{
    AgenticSelfCompiler, DynamicSkillDescriptor, NovelTaskSpecification,
    SelfCompilationReceipt, SkillSourceLanguage,
};
















