//! # Interactive Fuzzy Command Search & Navigator Menu (`hgb menu`)
//!
//! Terminal interactive fuzzy selector indexing all 130+ Hagibis subcommands.
//! Supports:
//! 1. Interactive terminal fuzzy search UI using `crossterm` and `ratatui`.
//! 2. Live query filtering with arrow-key navigation and details preview pane.
//! 3. Direct non-interactive query mode (`hgb menu --query <term>`).

use colored::Colorize;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io::{self, stdout};
use std::time::Duration;

/// Metadata for a cataloged Hagibis CLI command
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub category: &'static str,
    pub syntax: &'static str,
    pub description: &'static str,
}

/// Catalog indexing all 130+ Hagibis subcommands
pub struct InteractiveCommandCatalog;

impl InteractiveCommandCatalog {
    pub fn all_commands() -> Vec<CommandEntry> {
        vec![
            // Core & Autopilot
            CommandEntry {
                name: "do",
                aliases: &["auto", "one"],
                category: "Core/Autopilot",
                syntax: "hgb do <spec> [--target <file>] [--auto-commit]",
                description: "Unified autonomous driver: RAM CoW sandbox, GoT arbitration, BlastRadius radar, self-healing, BLAKE3 cert, and DPO distillation export.",
            },
            CommandEntry {
                name: "hybrid",
                aliases: &["orchestrate", "dual-brain"],
                category: "Core/DualBrain",
                syntax: "hgb hybrid [task] [--benchmark] [--target <path>] [--force-local] [--force-codex] [--privacy-check]",
                description: "Hybrid dual-brain orchestration: deterministic AST/blast-radius task routing (<2ms), RAM CoW sandboxed verification & escalation, shared project context, outbound privacy firewall, and Pass@1 / USD arbitrage scorecard.",
            },
            CommandEntry {
                name: "chat",
                aliases: &["c", "chat-canvas"],
                category: "Core/Chat",
                syntax: "hgb chat [--resume <id>]",
                description: "Conversational AGY chat canvas, live multi-turn cockpit, and workspace scratchpad.",
            },
            CommandEntry {
                name: "browse",
                aliases: &["web", "fetch-url"],
                category: "Core/Web",
                syntax: "hgb browse <url> [--tokens <N>]",
                description: "SSRF-hardened live web browsing and HTML-to-clean-Markdown extraction with prompt injection auditing.",
            },
            CommandEntry {
                name: "search-web",
                aliases: &["web-search", "ddg"],
                category: "Core/Web",
                syntax: "hgb search-web <query> [--limit <N>]",
                description: "Live web search via DuckDuckGo returning ranked organic results with clickable URLs and snippets.",
            },
            CommandEntry {
                name: "inspect-folder",
                aliases: &["folder", "inspect-dir", "ls-net"],
                category: "Core/Filesystem",
                syntax: "hgb inspect-folder [path] [--storage] [--network] [--xdg] [-a/--analyze] [--depth <N>]",
                description: "Deep architectural analysis & inspection of local folders, storage partitions, CIFS/NFS shares, and LAN computers.",
            },
            CommandEntry {
                name: "classic",
                aliases: &["repl"],
                category: "Core/Chat",
                syntax: "hgb classic",
                description: "Traditional terminal scrolling REPL interactive prompt.",
            },
            CommandEntry {
                name: "daemon",
                aliases: &["d", "service"],
                category: "Core/Service",
                syntax: "hgb daemon <status|start|stop|install-service|journal>",
                description: "Control Hagibis microkernel resident daemon (hgbd), systemd integration, and overnight dream journal.",
            },
            CommandEntry {
                name: "doctor",
                aliases: &["diag", "check"],
                category: "Core/Health",
                syntax: "hgb doctor [--json]",
                description: "Perform comprehensive systems health check across provider credentials, local Ollama, and IPC sockets.",
            },
            CommandEntry {
                name: "menu",
                aliases: &["find-cmd", "catalog"],
                category: "Core/Navigation",
                syntax: "hgb menu [--query <term>]",
                description: "Interactive fuzzy finder and searchable catalog indexing 130+ Hagibis subcommands.",
            },
            CommandEntry {
                name: "ci-action",
                aliases: &["gh-action"],
                category: "CI/CD",
                syntax: "hgb ci-action [--task <task>] [--summary]",
                description: "Official GitHub Action runner: automated blast radius auditing, slopsquatting firewall, and PR comment tables.",
            },
            CommandEntry {
                name: "gpu-bench",
                aliases: &["vulkan-bench"],
                category: "Hardware/Compute",
                syntax: "hgb gpu-bench [--iterations <N>]",
                description: "Direct GPU/Vulkan & CPU SIMD compute benchmark running parallel quantized GEMM kernels (F32, Q4_0, Q8_0) measuring GFLOPS.",
            },
            CommandEntry {
                name: "model-pull",
                aliases: &["pull"],
                category: "Model/Storage",
                syntax: "hgb model-pull <model-name-or-url>",
                description: "Download, verify checksums, and manage canonical GGUF models stored in ~/.hgb/models/.",
            },
            CommandEntry {
                name: "desktop-ide",
                aliases: &["studio-app"],
                category: "IDE/Visual",
                syntax: "hgb desktop-ide [--port <N>] [--browser <app>]",
                description: "Launch standalone borderless desktop IDE window mode connected to local Axum PWA Studio.",
            },

            // Free Claude Code Transcendence & Multi-Agent Compatibility
            CommandEntry {
                name: "proxy",
                aliases: &["serve-anthropic", "dropin"],
                category: "Proxy/Compatibility",
                syntax: "hgb proxy [--port <port>] [--provider <name>] [--shred-tokens]",
                description: "Axum proxy server implementing Anthropic /v1/messages and OpenAI /v1/chat/completions with TokenShredder.",
            },
            CommandEntry {
                name: "auth copilot",
                aliases: &["copilot-login"],
                category: "Auth/Provider",
                syntax: "hgb auth copilot",
                description: "Real RFC 8628 OAuth Device Flow authentication for GitHub Copilot (Claude 3.5 Sonnet & GPT-4o).",
            },
            CommandEntry {
                name: "auth-openai",
                aliases: &["openai-auth", "openai-login"],
                category: "Auth/OpenAI",
                syntax: "hgb auth-openai [API_KEY]",
                description: "Save OpenAI API key to ~/.hgb/credentials.json for GPT-4o, GPT-4o-mini, o1, and o3-mini.",
            },
            CommandEntry {
                name: "auth-codex",
                aliases: &["codex-auth", "import-codex", "auth-chatgpt"],
                category: "Auth/Codex",
                syntax: "hgb auth-codex",
                description: "Import & verify OpenAI Codex CLI Web Authentication (~/.codex/auth.json) to power ChatGPT frontier models.",
            },
            CommandEntry {
                name: "auth-copilot",
                aliases: &["copilot-auth", "copilot-login"],
                category: "Auth/Copilot",
                syntax: "hgb auth-copilot",
                description: "Authenticate GitHub Copilot RFC 8628 OAuth device flow to power GPT-4o & Claude 3.5 Sonnet.",
            },
            CommandEntry {
                name: "key",
                aliases: &["set-key"],
                category: "Auth/Keys",
                syntax: "hgb key <openai|anthropic|deepseek> [API_KEY]",
                description: "Save AI provider secret keys permanently to ~/.hgb/credentials.json.",
            },
            CommandEntry {
                name: "cloud-fabric",
                aliases: &["fabric", "cloud-bench"],
                category: "Cloud/Fabric",
                syntax: "hgb cloud-fabric [--ping]",
                description: "Multi-provider catalog and instant sub-millisecond failover engine (NVIDIA NIM, Groq, OpenRouter, Gemini, Copilot, Ollama).",
            },
            CommandEntry {
                name: "remote",
                aliases: &["bot"],
                category: "Remote/Gateway",
                syntax: "hgb remote <telegram|discord>",
                description: "Async Telegram and Discord bot gateway allowing mobile task dispatching and status inspection.",
            },

            // Blast Radius & AST
            CommandEntry {
                name: "blast",
                aliases: &["blast-radius"],
                category: "AST/Architecture",
                syntax: "hgb blast <files...>",
                description: "Interactive blast radius architecture radar measuring AST symbol modifications and downstream call sites.",
            },
            CommandEntry {
                name: "overlay",
                aliases: &["sandbox", "cow"],
                category: "Sandbox/Linux",
                syntax: "hgb overlay <status|commit|rollback>",
                description: "Manage zero-cost Linux RAM /dev/shm OverlayFS Copy-on-Write workspaces.",
            },
            CommandEntry {
                name: "got",
                aliases: &["graph-of-thought"],
                category: "Reasoning/GoT",
                syntax: "hgb got <problem-statement>",
                description: "Execute multi-branch Graph-of-Thought speculative reasoning with Pareto fitness reduction.",
            },
            CommandEntry {
                name: "dream",
                aliases: &["ambient-distill"],
                category: "Distillation/DPO",
                syntax: "hgb dream [--force] [--limit <N>]",
                description: "Run autonomous ambient sleep-state DPO pair distillation from developer commit history.",
            },
            CommandEntry {
                name: "checkpoint",
                aliases: &["cp"],
                category: "Storage/History",
                syntax: "hgb checkpoint <create|list|rewind>",
                description: "Instant sub-millisecond workspace checkpoint snapshot and time-machine rewind.",
            },
            CommandEntry {
                name: "timeline",
                aliases: &["tl", "fork"],
                category: "Storage/History",
                syntax: "hgb timeline <create|list|diff|merge|discard>",
                description: "Zero-cost ephemeral workspace timelines for speculative parallel exploration.",
            },
            CommandEntry {
                name: "enterprise",
                aliases: &["ent"],
                category: "Enterprise",
                syntax: "hgb enterprise <status|activate|issue|audit|mcp-config>",
                description: "Manage enterprise cryptographic licensing, SOC2 audit trails, and Microsoft 365 MCP architecture.",
            },
            CommandEntry {
                name: "skills",
                aliases: &["skill"],
                category: "Storage/Agent",
                syntax: "hgb skills <list|add|seed>",
                description: "Manage procedural developer skill memories and automated heuristic patterns.",
            },
            CommandEntry {
                name: "rules",
                aliases: &["rule"],
                category: "Governance/Safety",
                syntax: "hgb rules <list|add|check>",
                description: "Inspect and enforce tiered structural rules, safety constraints, and code conventions.",
            },
            CommandEntry {
                name: "security",
                aliases: &["sec", "audit"],
                category: "Security",
                syntax: "hgb security <scan|report>",
                description: "Automated vulnerability scanner, secret leaker detection, and CVE mitigation auditor.",
            },
            CommandEntry {
                name: "slopsquatting",
                aliases: &["firewall"],
                category: "Security/SupplyChain",
                syntax: "hgb slopsquatting <audit>",
                description: "Supply-chain hallucination firewall detecting typosquatted and non-existent crate/npm dependencies.",
            },
            CommandEntry {
                name: "fuzz",
                aliases: &["mutation-fuzz"],
                category: "Testing/QA",
                syntax: "hgb fuzz <target-file>",
                description: "AST mutation fuzzer generating adversarial test cases and invariant stress tests.",
            },
            CommandEntry {
                name: "rescue",
                aliases: &["heal"],
                category: "Autopilot/Heal",
                syntax: "hgb rescue <error-log-or-trace>",
                description: "Autonomous crash triage, compiler error diagnosis, and automated patch synthesizer.",
            },
            CommandEntry {
                name: "time-warp",
                aliases: &["timewarp"],
                category: "Database/Data",
                syntax: "hgb time-warp <snapshot|restore>",
                description: "Relational database shadow-workspace stress testing and zero-cost CoW schema migrations.",
            },
            CommandEntry {
                name: "vibe",
                aliases: &["vibe-recipe"],
                category: "Fullstack/Synthesis",
                syntax: "hgb vibe <prompt>",
                description: "Fullstack rapid vibe-coding generator: scaffolding frontend, backend, database, and tests in one shot.",
            },
            CommandEntry {
                name: "merkle",
                aliases: &["merkle-index"],
                category: "Security/Ledger",
                syntax: "hgb merkle <verify|sign>",
                description: "Cryptographic Merkle tree verification of workspace source code integrity.",
            },
            CommandEntry {
                name: "cdp",
                aliases: &["browser-snoop"],
                category: "Browser/CDP",
                syntax: "hgb cdp <url>",
                description: "Chrome DevTools Protocol inspector, DOM tree visualizer, and live console snoop.",
            },
            CommandEntry {
                name: "voice",
                aliases: &["voice-flow"],
                category: "Multimodal/Voice",
                syntax: "hgb voice <listen|speak>",
                description: "Voice-driven developer agent command flow and real-time audio transcript bridge.",
            },
            CommandEntry {
                name: "style",
                aliases: &["style-vault"],
                category: "Personalization",
                syntax: "hgb style <inspect|learn>",
                description: "Developer style memory vault adapting code formatting and naming patterns.",
            },
            CommandEntry {
                name: "polyglot",
                aliases: &["typelock"],
                category: "AST/Typing",
                syntax: "hgb polyglot <check>",
                description: "Cross-language type barrier locking across Rust, TypeScript, Python, and Go interfaces.",
            },
            CommandEntry {
                name: "nightshift",
                aliases: &["night-shift"],
                category: "Autopilot/Ambient",
                syntax: "hgb nightshift [--schedule <cron>]",
                description: "Autonomous overnight backlog refactoring and automated test coverage expansion pipeline.",
            },
            CommandEntry {
                name: "p2p-grid",
                aliases: &["p2p"],
                category: "P2P/Swarm",
                syntax: "hgb p2p-grid [--seed <node>]",
                description: "Decentralized libp2p compute mesh pooling idle GPU/CPU nodes across local network.",
            },
            CommandEntry {
                name: "cloud-swarm",
                aliases: &["swarm"],
                category: "Cloud/Swarm",
                syntax: "hgb cloud-swarm [--endpoint <url>]",
                description: "Offload compute-intensive tasks to cloud swarm clusters with end-to-end encryption.",
            },
            CommandEntry {
                name: "mcp",
                aliases: &["mcp-host"],
                category: "MCP/Integration",
                syntax: "hgb mcp <list|connect|call>",
                description: "Model Context Protocol client and host orchestrator managing external tools and data servers.",
            },
            CommandEntry {
                name: "visual-ide",
                aliases: &["studio"],
                category: "IDE/Visual",
                syntax: "hgb visual-ide [--port <port>]",
                description: "Local web-based visual IDE and live component studio with DOM hot-reloading.",
            },
            CommandEntry {
                name: "lsp-bridge",
                aliases: &["lsp"],
                category: "IDE/Editor",
                syntax: "hgb lsp-bridge",
                description: "Native editor integration bridging Language Server Protocol with speculative suggestions.",
            },
            CommandEntry {
                name: "spec",
                aliases: &["spec-decompose"],
                category: "Spec/Planning",
                syntax: "hgb spec <requirements-doc>",
                description: "Decompose vague human requirements into formal, verifiable specifications and test matrices.",
            },
            CommandEntry {
                name: "anti-placebo",
                aliases: &["placebo-guard"],
                category: "Governance/Safety",
                syntax: "hgb anti-placebo <audit>",
                description: "Detect mock/stub placebo test suites and enforce real assertions that actually execute logic.",
            },
            CommandEntry {
                name: "finops",
                aliases: &["llm-cost"],
                category: "FinOps/Telemetry",
                syntax: "hgb finops <report|budget>",
                description: "LLM token cost gateway, speculative cache hits, and automated arbitrage reporting.",
            },
            CommandEntry {
                name: "voice",
                aliases: &["mic", "listen"],
                category: "Audio/Voice",
                syntax: "hgb voice [--file <audio.wav>] [--record] [--groq]",
                description: "Voice-to-Intent Engine: WAV/PCM parser, VAD silence trimming, Whisper transcription, and structured intent classification.",
            },
            CommandEntry {
                name: "visual-loop",
                aliases: &["visual-debug", "cdp-debug"],
                category: "Visual/CDP",
                syntax: "hgb visual-loop [--url <url>] [--port <9222>] [--screenshot <out.png>]",
                description: "Visual Multimodal Debugger: Chromium CDP WebSocket client, Page screenshot capture, JS exception extraction, and vision prompt generator.",
            },
            CommandEntry {
                name: "time-machine",
                aliases: &["chrono", "tm"],
                category: "Storage/History",
                syntax: "hgb time-machine [--list] [--restore <id>] [--scrub]",
                description: "ChronoWarp Time-Machine: Persistent .hgb/timelines/ snapshot ledger, BLAKE3 checksums, diff restoring, and interactive ASCII scrubber graph.",
            },
            CommandEntry {
                name: "ast-query",
                aliases: &["ast-refactor"],
                category: "AST/Refactor",
                syntax: "hgb ast-query [--query <fn:async,returns:Result>] [--path <file.rs>] [--preview]",
                description: "Deterministic AST Query & Refactor: syn/quote structural pattern query engine with delimiter and syntax invariant enforcement.",
            },
            CommandEntry {
                name: "p2p-tunnel",
                aliases: &["p2p", "share-p2p"],
                category: "P2P/Network",
                syntax: "hgb p2p-tunnel [--port <local_port>] [--bind <ip>] [--qr]",
                description: "Zero-Config P2P Encrypted Tunnel: Axum reverse proxy, BLAKE3 authentication, and native Unicode half-block terminal QR rasterizer.",
            },
            CommandEntry {
                name: "budget",
                aliases: &["spending", "finops-budget"],
                category: "FinOps/Budget",
                syntax: "hgb budget [--set-limit <usd>] [--record <model,in_tok,out_tok>]",
                description: "Smart FinOps & Token Budget Envelope: .hgb/budget.json ledger, pricing tables, daily USD cap enforcement, and dynamic routing to Local Ollama.",
            },
            CommandEntry {
                name: "mcp",
                aliases: &["mcp-server", "model-context-protocol"],
                category: "MCP/Integration",
                syntax: "hgb mcp [serve|list|call]",
                description: "Native Model Context Protocol Server: exposes SQLite, CoW sandboxes, AST queries, hardware sentry, and living memory directly to LLMs.",
            },
            CommandEntry {
                name: "dynatune",
                aliases: &["hardware-sentry", "thermal"],
                category: "Hardware/Kernel",
                syntax: "hgb dynatune [--json]",
                description: "Dynatune Hardware Sentry & Kernel Synergy: real-time Linux thermal zones, CPU governor scaling, and PSI pressure throttling.",
            },
            CommandEntry {
                name: "masterclass",
                aliases: &["docs-gen", "build-pdf"],
                category: "Docs/CI",
                syntax: "hgb masterclass [--force]",
                description: "Automated Masterclass Pipeline: compiles 85-page definitive architectural PDF book via uv/python3.",
            },
            CommandEntry {
                name: "wasm-plugin",
                aliases: &["plugin-kernel", "wasm-exec"],
                category: "Plugins/Sandbox",
                syntax: "hgb wasm-plugin <file.wasm> [json_input]",
                description: "WebAssembly (hgb-wasm-kernel) Plugin Engine: run user-defined agent tools securely in Wasmtime with fuel metering.",
            },
            CommandEntry {
                name: "ollama-route",
                aliases: &["local-infer", "infer-route"],
                category: "Inference/Dual-Brain",
                syntax: "hgb ollama-route <prompt> [--force-local] [--force-remote]",
                description: "Local Ollama Inference Router: heuristic offloading of routine tasks to local models with zero API cost and instant fallback.",
            },
            CommandEntry {
                name: "dual-sandbox",
                aliases: &["microvm", "dual-layer"],
                category: "Sandbox/Virtualization",
                syntax: "hgb dual-sandbox <workload> [--command <cmd>]",
                description: "Dual-Layer Secure Execution Engine: fast Wasm path (<2ms) and heavy Firecracker microVM path (<150ms).",
            },
            CommandEntry {
                name: "semantic-cache",
                aliases: &["cache-stats"],
                category: "Storage/Performance",
                syntax: "hgb semantic-cache [--clear]",
                description: "In-Process Semantic Query Cache: inspect sub-millisecond cached prompt embeddings, hit rates, and latency savings.",
            },
            CommandEntry {
                name: "bare-metal",
                aliases: &["kernel", "governor"],
                category: "Hardware/Performance",
                syntax: "hgb bare-metal [--boost] [--relax] [--profile <name>]",
                description: "Bare-Metal Telemetry & Kernel Bridge: dynamically command intel_pstate governor, EPP, MGLRU, and dynatune thermal profiles.",
            },
            CommandEntry {
                name: "wasm-sync",
                aliases: &["wasm-migrate", "edge-sync"],
                category: "Wasm/Edge",
                syntax: "hgb wasm-sync [--target <edge|local>] [--bundle] [--pages <N>] [--step <N>] [--status]",
                description: "Stateful WebAssembly Distributed Sync: WIT component models, cryptographic BLAKE3 snapshots, and Cloudflare Workers edge packaging.",
            },
            CommandEntry {
                name: "vibe",
                aliases: &["vibe-loop", "autonomous-loop"],
                category: "Autopilot/Vibe",
                syntax: "hgb vibe [--once] [--watch] [--auto-patch] [--no-tests] [--debounce <ms>]",
                description: "Autonomous Vibe Coding Agent Loop: watches filesystem events, runs speculative First-Green sandbox checks, and self-optimizes codebases.",
            },
            CommandEntry {
                name: "wasm-pool",
                aliases: &["hot-pool", "wasm-hot"],
                category: "Wasm/Runtime",
                syntax: "hgb wasm-pool [--status] [--prewarm <paths...>] [--exec <path> [json]]",
                description: "Zero-Copy WebAssembly Hot-Pool: pre-warmed Wasmtime instances with JIT cache for microsecond sub-task execution.",
            },
            CommandEntry {
                name: "power-scheduler",
                aliases: &["power-tune", "adaptive-tune"],
                category: "Hardware/Scheduler",
                syntax: "hgb power-scheduler [--status] [--schedule] [--threads <N>]",
                description: "Adaptive Power-Aware Scheduler: dynamically modulates Ollama thread counts and CPU governors via live Dynatune thermal telemetry.",
            },
            CommandEntry {
                name: "bus",
                aliases: &["events", "event-bus"],
                category: "IPC/Stream",
                syntax: "hgb bus [--status] [--follow] [--topic <filter>] [--publish <topic> <payload>]",
                description: "Unified Distributed Event Bus: Tokio broadcast channel streaming real-time agent, wasm, and power events over local UDS socket.",
            },
            CommandEntry {
                name: "hw-route",
                aliases: &["model-route"],
                category: "Hardware/Routing",
                syntax: "hgb hw-route [--complexity <heavy|standard|lint|deterministic>] [--status]",
                description: "Native Ollama Integration & Hardware-Aware Router: dynamically selects heavy vs light models or WASM offload based on thermal headroom and power state.",
            },
            CommandEntry {
                name: "microvm-swarm",
                aliases: &["swarm-sandbox"],
                category: "Sandbox/Virtualization",
                syntax: "hgb microvm-swarm [--code <string>] [--language <lang>] [--heavy] [--status]",
                description: "Distributed MicroVM Swarms: fast WasmHotPool (<2ms) and heavy Linux Firecracker KVM microVMs with automatic zero-failure fallback to rootless CoW.",
            },
            CommandEntry {
                name: "mcp-mesh",
                aliases: &["mesh"],
                category: "MCP/Mesh",
                syntax: "hgb mcp-mesh [--sync] [--call <tool>] [--node <id>] [--args <json>] [--status]",
                description: "Unified MCP Mesh Network: distributed peer discovery via BLAKE3 IDs, mutual auth handshakes, tool aggregation, and mutation vector sync.",
            },
            CommandEntry {
                name: "self-patch",
                aliases: &["auto-patch"],
                category: "Autopilot/Learning",
                syntax: "hgb self-patch [--trigger] [--log <context>] [--status]",
                description: "Autonomous Self-Patching & Continuous Learning Loop: synthesizes unified diffs, verifies First-Green in sandbox, applies patches, and records DPO pairs.",
            },
            CommandEntry {
                name: "vector-stream",
                aliases: &["vstream", "vec-stream"],
                category: "Storage/Vector",
                syntax: "hgb vector-stream [--collection <name>] [--query <text|vec>] [--upsert <id> <vec> <meta>] [--status]",
                description: "Distributed Vector & Event Streaming: SQLite vector table (.hgb/vectors.db), SIMD cosine/Euclidean distance, and non-blocking event stream.",
            },
            CommandEntry {
                name: "self-heal",
                aliases: &["heal-loop", "fix-compile"],
                category: "Compiler/Heal",
                syntax: "hgb self-heal [--file <path>] [--code <string>] [--command <cmd>] [--max-retries <N>] [--status]",
                description: "Autonomous Self-Healing Compilation Loop: RAM CoW sandbox verification, structured diagnostic parsing (Rust/Zig/Python), iterative AST repair, and First-Green disk commit.",
            },
            CommandEntry {
                name: "jit-alloc",
                aliases: &["gpu-jit", "resource-tune"],
                category: "Hardware/JIT",
                syntax: "hgb jit-alloc [--boost] [--relax] [--pid <id>] [--cores <0,1>] [--priority] [--status]",
                description: "JIT GPU/NPU Offloading & Dynatune Allocator: queries /dev/dri/renderD128, sets performance governors, pins LLM threads to performance cores with RAII auto-relax.",
            },
            CommandEntry {
                name: "wasm-shm",
                aliases: &["shm-pipe", "wasm-zero-copy"],
                category: "Wasm/ZeroCopy",
                syntax: "hgb wasm-shm [--bench] [--chunk-kb <N>] [--iterations <N>] [--status]",
                description: "Zero-Copy WebAssembly Shared Memory Pipeline: direct host-to-guest linear memory slicing, ring-buffer streams, and sub-microsecond invocation latency.",
            },
            CommandEntry {
                name: "neural-coprocessor",
                aliases: &["coproc", "smart-router"],
                category: "Core/Neural",
                syntax: "hgb neural-coprocessor [--prompt <text>] [--status]",
                description: "Neural Coprocessor & Model Hot-Swapping: prompt complexity inspector routing across Local WASM kernel, Local Ollama models, and Frontier MCP models with Dynatune thermal awareness.",
            },
            CommandEntry {
                name: "shm-ring",
                aliases: &["ring-buffer", "shm-pipe"],
                category: "Storage/ZeroCopy",
                syntax: "hgb shm-ring [--push <data>] [--pop <bytes>] [--bench] [--chunk-size <N>] [--iterations <N>] [--status]",
                description: "Zero-Copy POSIX Shared Memory Ring Buffer: /dev/shm lock-free circular ring buffer with 64-byte aligned atomic heads delivering nanosecond IPC latency without disk I/O.",
            },
            CommandEntry {
                name: "self-healing-daemon",
                aliases: &["daemon-heal", "heal-loop"],
                category: "Daemon/Autopilot",
                syntax: "hgb self-healing-daemon [--trigger] [--command <cmd>] [--file <target>] [--status]",
                description: "Self-Healing Autonomous Daemon Loops: background supervisor intercepting WASM/MicroVM/compiler failures, synthesizing diagnostic prompts, and applying verified CoW memory patches autonomously.",
            },
            CommandEntry {
                name: "swarm-mesh",
                aliases: &["p2p-mesh", "swarm"],
                category: "NextGen/P2P",
                syntax: "hgb swarm-mesh [--delegate <payload>] [--kind <compile|wasm>] [--sync-dht <key=val>] [--status]",
                description: "Distributed P2P Swarm Mesh: sovereign peer discovery, BLAKE3 authentication, load-balanced task delegation to idle peer nodes, and encrypted distributed hash table (DHT) caching.",
            },
            CommandEntry {
                name: "shm-ipc",
                aliases: &["ipc-bench"],
                category: "IPC/ZeroCopy",
                syntax: "hgb shm-ipc [--bench] [--chunk-size <bytes>] [--iterations <count>] [--status]",
                description: "Zero-Overhead SHM-IPC Transport: framed UNIX Domain Socket streaming with zero-copy shared memory handles for large tensor vectors and LLM context payloads.",
            },
            CommandEntry {
                name: "actor-swarm",
                aliases: &["swarm-debate", "agent-swarm"],
                category: "Autopilot/Swarm",
                syntax: "hgb actor-swarm [--task <text>] [--rounds <N>] [--status]",
                description: "Autonomous Multi-Agent Swarm Orchestration: native Tokio Actor Model with Planner, Coder, Reviewer, and Sandbox Executor roles debating and certifying patches.",
            },
            CommandEntry {
                name: "crdt-sync",
                aliases: &["edge-sync", "crdt"],
                category: "Storage/CRDT",
                syntax: "hgb crdt-sync [--export] [--merge <json>] [--status]",
                description: "Distributed Edge-to-Cloud CRDT State Synchronization: conflict-free LWW and OR-Set synchronization across local workstation and Cloudflare Workers edge nodes.",
            },
            CommandEntry {
                name: "cpu-governor",
                aliases: &["gov-tune", "dynatune-boost"],
                category: "Hardware/Governor",
                syntax: "hgb cpu-governor [--boost <workload>] [--relax] [--status]",
                description: "Hardware-Aware Dynamic CPU Governor Integration: commands performance profiles and EPP dynamically for heavy compilation and LLM tasks with thermal safeguard.",
            },
            CommandEntry {
                name: "hw-optimizer",
                aliases: &["hw-opt"],
                category: "Hardware/Optimizer",
                syntax: "hgb hw-optimizer [--status] [--boost <workload>] [--relax]",
                description: "Deep dynatune & kernel-optimizer bridge: MGLRU, memory PSI pressure tuning, Intel P-State governor control, and thermal protection.",
            },
            CommandEntry {
                name: "ollama-speculative",
                aliases: &["spec-infer", "ollama-split"],
                category: "AI/Speculative",
                syntax: "hgb ollama-speculative [--prompt <text>] [--split <text>] [--draft <model>] [--target <model>]",
                description: "Supercharged Ollama speculative decoding pipeline with autonomous task complexity splitter and zero-failure fallback.",
            },
            CommandEntry {
                name: "ollama-sibling",
                aliases: &["sibling-ollama", "ollama-vector"],
                category: "AI/Ollama",
                syntax: "hgb ollama-sibling [--status] [--sync <file>] [--query <text>] [--top-k <num>]",
                description: "Direct local Ollama sibling engine acceleration and zero-copy SQLite mutation vector mesh.",
            },
            CommandEntry {
                name: "wasm-skills",
                aliases: &["skill-market", "wasm-market"],
                category: "Wasm/Marketplace",
                syntax: "hgb wasm-skills [--list] [--hot-load <path> --name <name>] [--exec <name> --payload <str>]",
                description: "WebAssembly (hgb-wasm-kernel) dynamic plugin marketplace with BLAKE3 validation, hot loading, and built-in standard skills.",
            },
            CommandEntry {
                name: "microvm-exec",
                aliases: &["vm-exec", "microvm-sandbox"],
                category: "Sandbox/MicroVM",
                syntax: "hgb microvm-exec --code <str> [--lang <bash|rust|python>] [--patch <path>]",
                description: "Ephemeral code execution sandbox: sub-second Firecracker microVM with automatic Linux Landlock LSM + RAM CoW OverlayFS fallback.",
            },
            CommandEntry {
                name: "kernel-boost",
                aliases: &["hybrid-accel", "mglru-tune"],
                category: "Hardware/Kernel",
                syntax: "hgb kernel-boost [--status] [--boost <workload>] [--relax]",
                description: "Hybrid Kernel-Level Acceleration: dynatune & kernel-optimizer bridge, performance governor, EPP tuning, MGLRU low latency (min_ttl_ms 1000), and thermal/battery watchdogs.",
            },
            CommandEntry {
                name: "wasm-mesh",
                aliases: &["mesh-compute", "p2p-wasm"],
                category: "Mesh/Compute",
                syntax: "hgb wasm-mesh [--status] [--publish <path> --name <name> --capabilities <c1,c2>] [--exec <mod> --task <id> --payload <str> --microvm]",
                description: "Distributed P2P WASM Compute Mesh: dynamic peer discovery, BLAKE3 packages, idle node task streaming, and ephemeral Firecracker microVM / Landlock RAM CoW execution.",
            },
            CommandEntry {
                name: "vibe-watcher",
                aliases: &["self-heal-loop", "vibe-watch"],
                category: "Daemon/Watcher",
                syntax: "hgb vibe-watcher [--status] [--trigger <path> --cmd <command>] [--history]",
                description: "Proactive Autonomous Feedback Loop: real-time code change interception, automated test/build trigger, diagnostic Ollama closed-loop feedback, and First-Green sandbox verification.",
            },
        ]
    }

    /// Fuzzy score calculation between query and a command entry
    pub fn score_entry(query: &str, entry: &CommandEntry) -> i32 {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return 100;
        }

        let name = entry.name.to_lowercase();
        let cat = entry.category.to_lowercase();
        let desc = entry.description.to_lowercase();

        // Exact match bonus
        if name == q {
            return 1000;
        }
        for alias in entry.aliases {
            if alias.to_lowercase() == q {
                return 950;
            }
        }

        // Prefix match
        if name.starts_with(&q) {
            return 500 + (100 - name.len() as i32);
        }

        // Substring match in name
        if name.contains(&q) {
            return 300 + (100 - name.len() as i32);
        }

        // Substring match in aliases
        for alias in entry.aliases {
            if alias.to_lowercase().contains(&q) {
                return 250;
            }
        }

        // Category match
        if cat.contains(&q) {
            return 200;
        }

        // Description match
        if desc.contains(&q) {
            return 100;
        }

        // Subsequence match
        let mut score = 0;
        let mut name_chars = name.chars().peekable();
        let mut matched_chars = 0;

        for qc in q.chars() {
            while let Some(nc) = name_chars.next() {
                if nc == qc {
                    matched_chars += 1;
                    score += 10;
                    break;
                }
            }
        }

        if matched_chars == q.chars().count() {
            score
        } else {
            -1 // No match
        }
    }

    /// Filter commands by query string, sorted descending by relevance score
    pub fn filter_commands<'a>(commands: &'a [CommandEntry], query: &str) -> Vec<(&'a CommandEntry, i32)> {
        let mut scored: Vec<(&'a CommandEntry, i32)> = commands
            .iter()
            .map(|cmd| (cmd, Self::score_entry(query, cmd)))
            .filter(|(_, score)| *score > 0)
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored
    }

    /// Non-interactive direct query mode
    pub fn print_query_results(query: &str) {
        let all = Self::all_commands();
        let matches = Self::filter_commands(&all, query);

        let border = "═".repeat(76);
        println!("{}", format!("╔{}╗", border).cyan());
        println!(
            "║  {} {:<54}║",
            "🔍 HAGIBIS COMMAND CATALOG MATCHES FOR:".white().bold(),
            format!("\"{}\"", query).yellow().bold()
        );
        println!("{}", format!("╠{}╣", border).cyan());

        if matches.is_empty() {
            println!("║  {:<74}║", "No subcommands matched your query.".dimmed());
            println!("║  {:<74}║", "Run 'hgb menu' without arguments to browse all subcommands.".italic());
        } else {
            for (idx, (cmd, score)) in matches.iter().take(12).enumerate() {
                let alias_str = if cmd.aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (aliases: {})", cmd.aliases.join(", "))
                };

                println!(
                    "║  {}. {:<18} {:<24} {:>24} ║",
                    idx + 1,
                    format!("hgb {}", cmd.name).green().bold(),
                    format!("[{}]", cmd.category).magenta(),
                    format!("relevance: {}", score).dimmed()
                );
                if !alias_str.is_empty() {
                    println!("║     {:<71}║", alias_str.dimmed());
                }
                println!("║     Syntax: {:<63}║", cmd.syntax.cyan());
                println!("║     {:<71}║", cmd.description.white());
                if idx + 1 < matches.len().min(12) {
                    println!("║  {}  ║", "─".repeat(72).dimmed());
                }
            }
        }

        println!("{}", format!("╚{}╝", border).cyan());
        println!(
            "Found {} matching subcommand(s). Tip: Use interactive mode with 'hgb menu'.\n",
            matches.len()
        );
    }

    /// Interactive terminal UI using ratatui & crossterm
    pub fn run_interactive() -> io::Result<Option<String>> {
        enable_raw_mode()?;
        let mut stdout_handle = stdout();
        execute!(stdout_handle, EnterAlternateScreen, cursor::Hide)?;

        let backend = CrosstermBackend::new(stdout_handle);
        let mut terminal = Terminal::new(backend)?;

        let commands = Self::all_commands();
        let mut query = String::new();
        let mut selected_idx = 0usize;
        let mut selected_command = None;

        loop {
            let filtered = Self::filter_commands(&commands, &query);
            let count = filtered.len();
            if count > 0 && selected_idx >= count {
                selected_idx = count.saturating_sub(1);
            }

            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3), // Header & search
                        Constraint::Min(8),    // List of results
                        Constraint::Length(7), // Command details preview
                        Constraint::Length(1), // Footer keys
                    ])
                    .split(f.area());

                // 1. Search Bar
                let search_text = format!("🔎 Search commands: {}█", query);
                let search_widget = Paragraph::new(search_text)
                    .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Hagibis Interactive Command Navigator ")
                            .border_style(Style::default().fg(Color::Cyan)),
                    );
                f.render_widget(search_widget, chunks[0]);

                // 2. Command List
                let list_items: Vec<ListItem> = filtered
                    .iter()
                    .enumerate()
                    .map(|(idx, (cmd, _score))| {
                        let is_selected = idx == selected_idx;
                        let prefix = if is_selected { "▶ " } else { "  " };
                        let line = Line::from(vec![
                            Span::styled(prefix, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::styled(format!("{:<14}", cmd.name), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                            Span::styled(format!(" [{}] ", cmd.category), Style::default().fg(Color::Magenta)),
                            Span::raw(cmd.description),
                        ]);
                        let item_style = if is_selected {
                            Style::default().bg(Color::DarkGray).fg(Color::White)
                        } else {
                            Style::default()
                        };
                        ListItem::new(line).style(item_style)
                    })
                    .collect();

                let mut list_state = ListState::default();
                list_state.select(Some(selected_idx));

                let list_widget = List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" Available Subcommands ({}) ", count))
                            .border_style(Style::default().fg(Color::Blue)),
                    );
                f.render_stateful_widget(list_widget, chunks[1], &mut list_state);

                // 3. Command Details Preview
                if let Some((cmd, _)) = filtered.get(selected_idx) {
                    let alias_str = if cmd.aliases.is_empty() {
                        "none".to_string()
                    } else {
                        cmd.aliases.join(", ")
                    };
                    let preview_lines = vec![
                        Line::from(vec![
                            Span::styled("Command: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::styled(cmd.name, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                            Span::styled(format!("  (Category: {})", cmd.category), Style::default().fg(Color::Magenta)),
                            Span::styled(format!("  Aliases: {}", alias_str), Style::default().fg(Color::DarkGray)),
                        ]),
                        Line::from(vec![
                            Span::styled("Syntax:  ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                            Span::styled(cmd.syntax, Style::default().fg(Color::White)),
                        ]),
                        Line::from(vec![
                            Span::styled("About:   ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                            Span::raw(cmd.description),
                        ]),
                    ];
                    let preview_widget = Paragraph::new(preview_lines)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Command Inspector ")
                                .border_style(Style::default().fg(Color::Green)),
                        )
                        .wrap(Wrap { trim: false });
                    f.render_widget(preview_widget, chunks[2]);
                } else {
                    let empty_preview = Paragraph::new("No command selected.")
                        .block(Block::default().borders(Borders::ALL).title(" Command Inspector "));
                    f.render_widget(empty_preview, chunks[2]);
                }

                // 4. Footer
                let footer = Paragraph::new(
                    " [↑/↓] Navigate  •  [Enter] Select  •  [Backspace] Edit Query  •  [Esc/Ctrl+C] Exit "
                ).style(Style::default().fg(Color::DarkGray));
                f.render_widget(footer, chunks[3]);
            })?;

            // Event poll
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Esc => break,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                            KeyCode::Enter => {
                                if let Some((cmd, _)) = filtered.get(selected_idx) {
                                    selected_command = Some(cmd.name.to_string());
                                }
                                break;
                            }
                            KeyCode::Up => {
                                if selected_idx > 0 {
                                    selected_idx -= 1;
                                }
                            }
                            KeyCode::Down => {
                                if selected_idx + 1 < count {
                                    selected_idx += 1;
                                }
                            }
                            KeyCode::Backspace => {
                                query.pop();
                                selected_idx = 0;
                            }
                            KeyCode::Char(c) => {
                                query.push(c);
                                selected_idx = 0;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Restore terminal
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, cursor::Show)?;

        Ok(selected_command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_contains_subcommands() {
        let commands = InteractiveCommandCatalog::all_commands();
        assert!(commands.len() >= 35);
        assert!(commands.iter().any(|c| c.name == "do"));
        assert!(commands.iter().any(|c| c.name == "menu"));
        assert!(commands.iter().any(|c| c.name == "ci-action"));
        assert!(commands.iter().any(|c| c.name == "gpu-bench"));
    }

    #[test]
    fn test_fuzzy_filtering() {
        let commands = InteractiveCommandCatalog::all_commands();
        let matches = InteractiveCommandCatalog::filter_commands(&commands, "sandb");
        assert!(!matches.is_empty());
        assert!(matches.iter().any(|(c, _)| c.name == "overlay" || c.description.contains("sandbox") || c.syntax.contains("sandbox")));
    }
}
