pub mod server;
pub mod ambient_service;
pub mod proxy_server;
pub mod remote_bot;
pub mod mcp_sse_server;
pub use server::{DaemonState, HagibisDaemon};
pub use ambient_service::{AmbientDaemonService, DreamJournal, DreamJournalEntry};
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
    GovernorWatchdog, GovernorWatchdogStatus,
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





